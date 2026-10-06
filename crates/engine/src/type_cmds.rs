//! `type.*` commands: create and edit type (text) layers.
//!
//! Text offsets in parameters and results are **character** indices (Unicode scalar values),
//! so JSON clients never split a UTF-8 sequence. Sizes, leading, indents and spacing are in
//! points (converted with the document resolution); tracking is in 1/1000 em; positions and
//! boxes are document pixels. Every edit re-renders the layer with `photocraft-text` and
//! regenerates its PSD `TySh` data, so the layer saves as editable text.

use std::sync::Arc;

use photocraft_color::Color;
use photocraft_doc::text::{
    AntiAlias, Caps, CharStyle, FontFeature, FontVariation, Kerning, ParagraphRun, ParagraphStyle, TextAlign, TextDirection, TextRun, TextShape,
};
use photocraft_doc::{Affine, Document, Layer, LayerContent, LayerId, TextLayer};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn layer_id(s: &Session, p: &Value) -> Result<LayerId> {
    match p.get("layer").and_then(Value::as_u64) {
        Some(id) => Ok(LayerId(id)),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into())),
    }
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn f32p(p: &Value, k: &str) -> Option<f32> {
    p.get(k).and_then(Value::as_f64).map(|v| v as f32)
}

fn color(v: &Value) -> Option<Color> {
    match v {
        Value::Array(a) if a.len() >= 3 => {
            let c: Vec<f32> = a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
            Some(Color::rgba(c[0], c[1], c[2], c.get(3).copied().unwrap_or(1.0)))
        }
        Value::String(s) => {
            let s = s.trim_start_matches('#');
            let h = |i: usize| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map(|v| f32::from(v) / 255.0);
            match s.len() {
                6 => Some(Color::rgb(h(0)?, h(2)?, h(4)?)),
                8 => Some(Color::rgba(h(0)?, h(2)?, h(4)?, h(6)?)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Normalises line breaks to `\n`.
fn norm_text(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn byte_at(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

fn char_at(text: &str, bi: usize) -> usize {
    text[..bi.min(text.len())].chars().count()
}

/// `range: [start, end]` in characters → byte range (whole text when absent).
fn range_param(text: &str, p: &Value) -> (usize, usize) {
    match p.get("range").and_then(Value::as_array) {
        Some(r) if r.len() == 2 => {
            let a = r[0].as_u64().unwrap_or(0) as usize;
            let b = r[1].as_u64().unwrap_or(a as u64) as usize;
            let (a, b) = (a.min(b), a.max(b));
            (byte_at(text, a), byte_at(text, b))
        }
        _ => (0, text.len()),
    }
}

/// Photoshop's manual kerning range (1/1000 em).
pub const KERN_MIN: f64 = -1000.0;
pub const KERN_MAX: f64 = 10_000.0;

/// Validates the `kerning` key of character params: a number in [`KERN_MIN`]..=[`KERN_MAX`] or
/// `"metrics"`, `"optical"`, `"off"` (`"none"`, `"0"`).
pub fn check_kerning(p: &Value) -> std::result::Result<(), String> {
    match p.get("kerning") {
        None => Ok(()),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(k) if (KERN_MIN..=KERN_MAX).contains(&k) => Ok(()),
            _ => Err(format!("kerning must be between {KERN_MIN} and {KERN_MAX} (1/1000 em)")),
        },
        Some(Value::String(v)) if matches!(v.to_ascii_lowercase().as_str(), "metrics" | "optical" | "off" | "none" | "0") => Ok(()),
        Some(_) => Err("kerning must be a number (1/1000 em) or \"metrics\", \"optical\" or \"off\"".into()),
    }
}

/// Applies character-style keys from JSON. Returns true if any key was present.
pub fn apply_char_props(s: &mut CharStyle, p: &Value) -> bool {
    let mut any = false;
    let mut hit = |b: bool| any |= b;
    if let Some(v) = p.get("font").or_else(|| p.get("family")).and_then(Value::as_str) {
        s.font_family = v.to_string();
        s.postscript_name = None;
        hit(true);
    }
    if let Some(v) = p.get("postscriptName").and_then(Value::as_str) {
        s.postscript_name = Some(v.to_string());
        hit(true);
    }
    if let Some(v) = p.get("fontStyle").and_then(Value::as_str) {
        s.font_style = v.to_string();
        let l = v.to_lowercase();
        s.italic = l.contains("italic") || l.contains("oblique");
        let g = photocraft_text::fonts::guess_from_postscript(&format!("X-{}", v.replace(' ', "")));
        s.weight = g.weight;
        hit(true);
    }
    if let Some(v) = p.get("weight").and_then(Value::as_f64) {
        s.weight = v.clamp(1.0, 1000.0) as u16;
        hit(true);
    }
    if let Some(v) = p.get("italic").and_then(Value::as_bool) {
        s.italic = v;
        hit(true);
    }
    if let Some(v) = f32p(p, "size") {
        s.size_pt = v.max(0.01);
        hit(true);
    }
    if let Some(c) = p.get("color").and_then(color) {
        s.color = c;
        hit(true);
    }
    if let Some(v) = f32p(p, "tracking") {
        s.tracking = v;
        hit(true);
    }
    match p.get("leading") {
        Some(Value::Null) => {
            s.leading_pt = None;
            hit(true);
        }
        Some(Value::String(a)) if a == "auto" => {
            s.leading_pt = None;
            hit(true);
        }
        Some(v) if v.is_number() => {
            s.leading_pt = v.as_f64().map(|x| x as f32);
            hit(true);
        }
        _ => {}
    }
    for (k, f) in [("baselineShift", &mut s.baseline_shift_pt), ("horizontalScale", &mut s.horizontal_scale), ("verticalScale", &mut s.vertical_scale)] {
        if let Some(v) = f32p(p, k) {
            *f = if k == "baselineShift" { v } else { (v / 100.0).max(0.01) };
            hit(true);
        }
    }
    for (k, f) in [
        ("underline", &mut s.underline),
        ("strikethrough", &mut s.strikethrough),
        ("fauxBold", &mut s.faux_bold),
        ("fauxItalic", &mut s.faux_italic),
        ("ligatures", &mut s.ligatures),
        ("discretionaryLigatures", &mut s.discretionary_ligatures),
    ] {
        if let Some(v) = p.get(k).and_then(Value::as_bool) {
            *f = v;
            hit(true);
        }
    }
    match p.get("kerning") {
        // A number is Photoshop's manual kerning: no automatic pair kerning plus the value.
        Some(v) if v.is_number() => {
            if let Some(k) = v.as_f64().filter(|k| k.is_finite()) {
                s.kerning = Kerning::Off;
                s.kern = k.clamp(KERN_MIN, KERN_MAX) as f32;
                hit(true);
            }
        }
        Some(Value::String(v)) => {
            s.kerning = match v.to_ascii_lowercase().as_str() {
                "optical" => Kerning::Optical,
                "none" | "off" | "0" => Kerning::Off,
                _ => Kerning::Metrics,
            };
            s.kern = 0.0;
            hit(true);
        }
        _ => {}
    }
    if let Some(v) = p.get("caps").and_then(Value::as_str) {
        s.caps = match v {
            "small" | "smallCaps" => Caps::SmallCaps,
            "all" | "allCaps" => Caps::AllCaps,
            _ => Caps::Normal,
        };
        hit(true);
    }
    if let Some(Value::Object(m)) = p.get("features") {
        for (tag, v) in m {
            let value = v.as_u64().map(|x| x as u16).or_else(|| v.as_bool().map(u16::from)).unwrap_or(1);
            s.features.retain(|f| &f.tag != tag);
            s.features.push(FontFeature { tag: tag.clone(), value });
        }
        hit(true);
    }
    if let Some(Value::Object(m)) = p.get("variations") {
        for (axis, v) in m {
            s.variations.retain(|f| &f.axis != axis);
            if let Some(x) = v.as_f64() {
                s.variations.push(FontVariation { axis: axis.clone(), value: x as f32 });
            }
        }
        hit(true);
    }
    if let Some(v) = p.get("language") {
        s.language = v.as_str().map(str::to_string);
        hit(true);
    }
    any
}

/// Applies paragraph-style keys from JSON. Returns true if any key was present.
pub fn apply_para_props(s: &mut ParagraphStyle, p: &Value) -> bool {
    let mut any = false;
    if let Some(v) = p.get("align").and_then(Value::as_str) {
        s.align = match v {
            "center" => TextAlign::Center,
            "right" => TextAlign::Right,
            "justify" | "justifyLeft" => TextAlign::JustifyLeft,
            "justifyCenter" => TextAlign::JustifyCenter,
            "justifyRight" => TextAlign::JustifyRight,
            "justifyAll" => TextAlign::JustifyAll,
            _ => TextAlign::Left,
        };
        any = true;
    }
    for (k, f) in [
        ("firstLineIndent", &mut s.first_line_indent_pt),
        ("startIndent", &mut s.start_indent_pt),
        ("endIndent", &mut s.end_indent_pt),
        ("spaceBefore", &mut s.space_before_pt),
        ("spaceAfter", &mut s.space_after_pt),
        ("autoLeading", &mut s.auto_leading),
    ] {
        if let Some(v) = f32p(p, k) {
            *f = if k == "autoLeading" { v / 100.0 } else { v };
            any = true;
        }
    }
    if let Some(v) = p.get("direction").and_then(Value::as_str) {
        s.direction = match v {
            "ltr" => TextDirection::Ltr,
            "rtl" => TextDirection::Rtl,
            _ => TextDirection::Auto,
        };
        any = true;
    }
    if let Some(v) = p.get("hyphenate").and_then(Value::as_bool) {
        s.hyphenate = v;
        any = true;
    }
    any
}

fn push_merge<S: PartialEq>(out: &mut Vec<(usize, S)>, len: usize, st: S) {
    if len == 0 {
        return;
    }
    match out.last_mut() {
        Some(last) if last.1 == st => last.0 += len,
        _ => out.push((len, st)),
    }
}

/// Applies `f` to the character style of bytes `a..b` (splitting runs at the edges).
pub fn style_range(t: &mut TextLayer, a: usize, b: usize, f: &dyn Fn(&mut CharStyle)) {
    let runs = t.char_runs();
    if t.text.is_empty() {
        let mut st = runs.into_iter().next().map(|r| r.style).unwrap_or_default();
        f(&mut st);
        t.runs = vec![TextRun { len: 0, style: st }];
        t.sync_summary();
        return;
    }
    let mut out: Vec<(usize, CharStyle)> = Vec::new();
    let mut at = 0;
    for r in runs {
        let (rs, re) = (at, at + r.len);
        at = re;
        let (ca, cb) = (a.clamp(rs, re), b.clamp(rs, re));
        push_merge(&mut out, ca - rs, r.style.clone());
        let mut st = r.style.clone();
        f(&mut st);
        push_merge(&mut out, cb - ca, st);
        push_merge(&mut out, re - cb, r.style);
    }
    t.runs = out.into_iter().map(|(len, style)| TextRun { len, style }).collect();
    t.sync_summary();
}

/// Applies `f` to every paragraph touching bytes `a..b` (paragraph runs follow paragraphs).
pub fn style_paragraphs(t: &mut TextLayer, a: usize, b: usize, f: &dyn Fn(&mut ParagraphStyle)) {
    let old = t.paragraph_runs();
    let starts: Vec<usize> = old
        .iter()
        .scan(0, |acc, r| {
            let s = *acc;
            *acc += r.len;
            Some(s)
        })
        .collect();
    let style_at = |off: usize| old[starts.iter().rposition(|&s| s <= off).unwrap_or(0)].style.clone();
    let mut out: Vec<(usize, ParagraphStyle)> = Vec::new();
    for pr in photocraft_text::layout::split_paragraphs(&t.text) {
        let mut st = style_at(pr.start);
        let touches = (pr.start < b && pr.end > a) || (a == b && a >= pr.start && (a < pr.end || pr.end == t.text.len()));
        if touches {
            f(&mut st);
        }
        push_merge(&mut out, pr.len(), st.clone());
        if out.is_empty() {
            out.push((0, st));
        }
    }
    t.paragraphs = out.into_iter().map(|(len, style)| ParagraphRun { len, style }).collect();
}

/// Replaces bytes `a..b` with `new`, keeping styles: inserted text takes the style of the
/// character before it (or after it at the start).
pub fn replace_text(t: &mut TextLayer, a: usize, b: usize, new: &str) {
    fn adjust<S: Clone + PartialEq>(runs: Vec<(usize, S)>, a: usize, b: usize, ins: usize) -> Vec<(usize, S)> {
        let mut out: Vec<(usize, S)> = Vec::new();
        let mut at = 0;
        let host = runs
            .iter()
            .scan(0, |acc, r| {
                let s = *acc;
                *acc += r.0;
                Some((s, s + r.0))
            })
            .position(|(s, e)| if a == 0 { e > 0 || s == 0 } else { a > s && a <= e })
            .unwrap_or(0);
        for (i, (len, st)) in runs.into_iter().enumerate() {
            let (rs, re) = (at, at + len);
            at = re;
            let keep = (a.clamp(rs, re) - rs) + (re - b.clamp(rs, re));
            let add = if i == host { ins } else { 0 };
            push_merge(&mut out, keep + add, st);
        }
        out
    }
    let (a, b) = (a.min(t.text.len()), b.min(t.text.len()).max(a.min(t.text.len())));
    let new = norm_text(new);
    let cr: Vec<(usize, CharStyle)> = t.char_runs().into_iter().map(|r| (r.len, r.style)).collect();
    let pr: Vec<(usize, ParagraphStyle)> = t.paragraph_runs().into_iter().map(|r| (r.len, r.style)).collect();
    let first_c = cr.first().map(|r| r.1.clone()).unwrap_or_default();
    let first_p = pr.first().map(|r| r.1.clone()).unwrap_or_default();
    let mut cr = adjust(cr, a, b, new.len());
    let mut pr = adjust(pr, a, b, new.len());
    t.text.replace_range(a..b, &new);
    if cr.is_empty() {
        cr.push((t.text.len(), first_c));
    }
    if pr.is_empty() {
        pr.push((t.text.len(), first_p));
    }
    t.runs = cr.into_iter().map(|(len, style)| TextRun { len, style }).collect();
    t.paragraphs = pr.into_iter().map(|(len, style)| ParagraphRun { len, style }).collect();
    t.sync_summary();
}

/// Re-renders the layer's pixels and regenerates its PSD `TySh` data.
pub fn refresh(doc: &Document, t: &mut TextLayer) {
    let dpi = doc.resolution_dpi;
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    let (layout, r) = eng.render(t, dpi, doc.pixel_format());
    t.cache = Some(r.surface);
    t.psd_raw = Some(Arc::new(photocraft_text::psd::build_tysh(t, dpi, layout.bounds())));
}

fn with_text_layer<R>(s: &mut Session, p: &Value, label: &str, f: impl FnOnce(&mut TextLayer, &Document) -> Result<R>) -> Result<R> {
    let id = layer_id(s, p)?;
    let (r, damage) = s.edit(label, |doc, _| {
        let snapshot = doc.clone();
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let LayerContent::Text(t) = &mut l.content else {
            return Err(EngineError::Other(format!("layer {} is a {} layer, not a type layer", id.0, l.content.kind_name())));
        };
        let before = t.cache.as_ref().map(|c| c.tile_bounds());
        let r = f(t, &snapshot)?;
        refresh(&snapshot, t);
        // Only this layer's pixels changed: the canvas recomposites their old and new area
        // instead of the whole document (#124). Unknown old pixels mean a full refresh.
        let damage = before.zip(t.cache.as_ref().map(|c| c.tile_bounds())).map(|(a, b)| a.union(&b));
        Ok((r, damage))
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = damage;
    }
    Ok(r)
}

fn layer_name(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    let name: String = first.chars().take(40).collect();
    if name.is_empty() { "Type Layer".into() } else { name }
}

fn info(s: &Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Text(t) = &l.content else {
        return Err(EngineError::Other(format!("layer {} is not a type layer", id.0)));
    };
    let text = &t.text;
    let mut at = 0;
    let runs: Vec<Value> = t
        .char_runs()
        .into_iter()
        .map(|r| {
            let v = json!({ "start": char_at(text, at), "end": char_at(text, at + r.len), "style": r.style });
            at += r.len;
            v
        })
        .collect();
    let mut at = 0;
    let paragraphs: Vec<Value> = t
        .paragraph_runs()
        .into_iter()
        .map(|r| {
            let v = json!({ "start": char_at(text, at), "end": char_at(text, at + r.len), "style": r.style });
            at += r.len;
            v
        })
        .collect();
    let layout = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner()).layout(t, d.doc.resolution_dpi);
    let lines: Vec<Value> = layout
        .lines
        .iter()
        .map(|ln| json!({ "start": char_at(text, ln.range.start), "end": char_at(text, ln.range.end), "baseline": ln.baseline, "x0": ln.x0, "x1": ln.x1, "ascent": ln.ascent, "descent": ln.descent }))
        .collect();
    let bounds = t.cache.as_ref().map(|c| c.content_bounds()).map(|r| json!([r.x0, r.y0, r.x1, r.y1]));
    Ok(json!({
        "layer": id.0,
        "text": text,
        "shape": t.shape,
        "transform": t.transform.m,
        "antialias": t.antialias,
        "orientation": t.orientation,
        "warp": t.warp,
        "runs": runs,
        "paragraphs": paragraphs,
        "lines": lines,
        "bounds": bounds,
        "pxPerPt": layout.px_per_pt,
    }))
}

const CHAR_PARAMS: &str = r##""font":str,"fontStyle":str,"weight":100..900,"italic":bool,"size":pt,"color":"#rrggbb"|[r,g,b,a],"tracking":1/1000em,"leading":pt|"auto","baselineShift":pt,"horizontalScale":%,"verticalScale":%,"underline":bool,"strikethrough":bool,"fauxBold":bool,"fauxItalic":bool,"kerning":1/1000em (manual, after each character)|"metrics"|"optical"|"off","caps":"normal|small|all","ligatures":bool,"discretionaryLigatures":bool,"features":{"ss01":1},"variations":{"wght":650},"language":str"##;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "type.create",
            label: "New Type Layer",
            menu: &["Layer", "New"],
            shortcut: None,
            params: r##"{"x":px,"y":px (baseline anchor of point text),"text":str,"box":[x,y,w,h]? (paragraph text),"name":str?,"align":"left|center|right|justify…"?, …character keys: "font","size":pt=12,"color",…}"##,
            enabled: has_doc,
            journal: true,
            run: |s, p| {
                let text = norm_text(p.get("text").and_then(Value::as_str).unwrap_or(""));
                // Type › Save Default Type Styles sets the starting styles; the colour is always
                // the foreground colour, as in Photoshop.
                let (mut style, mut para) = s.type_defaults.clone().unwrap_or_else(|| (CharStyle { font_family: photocraft_text::fonts::DEFAULT_FAMILY.into(), ..Default::default() }, ParagraphStyle::default()));
                let fg = s.tools.foreground;
                style.color = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
                apply_char_props(&mut style, p);
                apply_para_props(&mut para, p);
                let (shape, transform) = match p.get("box").and_then(Value::as_array) {
                    Some(b) if b.len() == 4 => {
                        let v: Vec<f32> = b.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
                        if v[2] <= 0.0 || v[3] <= 0.0 {
                            return Err(bad("type.create", "box width and height must be positive"));
                        }
                        (TextShape::Box { x: 0.0, y: 0.0, width: v[2], height: v[3] }, Affine::translate(f64::from(v[0]), f64::from(v[1])))
                    }
                    _ => (TextShape::Point, Affine::translate(f32p(p, "x").unwrap_or(0.0).into(), f32p(p, "y").unwrap_or(0.0).into())),
                };
                let mut t = TextLayer {
                    runs: vec![TextRun { len: text.len(), style }],
                    paragraphs: vec![ParagraphRun { len: text.len(), style: para }],
                    text,
                    shape,
                    transform,
                    ..Default::default()
                };
                t.sync_summary();
                let name = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| layer_name(&t.text));
                let id = s.edit("New Type Layer", |doc, active| {
                    refresh(doc, &mut t);
                    let id = doc.insert_above(*active, Layer::new(name, LayerContent::Text(t)));
                    *active = Some(id);
                    Ok(id)
                })?;
                let bounds = s.active().and_then(|d| d.doc.layer(id)).and_then(|l| l.surface()).map(|c| c.content_bounds());
                Ok(json!({ "layer": id.0, "bounds": bounds.map(|r| [r.x0, r.y0, r.x1, r.y1]) }))
            },
        },
        CommandSpec {
            id: "type.edit",
            label: "Edit Type",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"text":str? (replace all, styles kept),"replace":{"start":char,"end":char,"text":str}?,"runs":[{"start":char,"end":char,…character keys}]?,"box":[x,y,w,h]? (to paragraph text),"point":[x,y]? (to point text),"move":[dx,dy]?,"transform":[a,b,c,d,e,f]?,"antialias":"none|sharp|crisp|strong|smooth"?,"name":str?,"kerning":1/1000em|"metrics"|"optical"|"off"? (with "range":[startChar,endChar]?, default all),"kernPair":{"at":caretChar,"by":1/1000em}? (Photoshop Alt+←/→: the pair before the caret becomes manual, its current kerning + by)}"##,
            enabled: has_doc,
            journal: true,
            run: |s, p| {
                let id = layer_id(s, p)?;
                let name = p.get("name").and_then(Value::as_str).map(str::to_string);
                check_kerning(p).map_err(|m| bad("type.edit", m))?;
                if let Some(Value::Array(runs)) = p.get("runs") {
                    for r in runs {
                        check_kerning(r).map_err(|m| bad("type.edit", m))?;
                    }
                }
                let kern_pair = match p.get("kernPair") {
                    None => None,
                    Some(k) => {
                        let at = k.get("at").and_then(Value::as_u64).ok_or_else(|| bad("type.edit", "kernPair.at must be a caret position (character index)"))?;
                        let by = k
                            .get("by")
                            .and_then(Value::as_f64)
                            .filter(|v| v.is_finite() && v.abs() <= KERN_MAX)
                            .ok_or_else(|| bad("type.edit", "kernPair.by must be a number of 1/1000 em"))?;
                        Some((at as usize, by))
                    }
                };
                let label = if kern_pair.is_some() { "Kerning" } else { "Edit Type" };
                with_text_layer(s, p, label, |t, doc| {
                    if let Some((at, by)) = kern_pair {
                        // Photoshop's Alt+←/→: the pair before the caret becomes manually kerned,
                        // starting from what it shows now (its metrics/optical or manual value).
                        let n = t.text.chars().count();
                        if at == 0 || at >= n {
                            return Err(bad("type.edit", "kernPair needs a caret between two characters"));
                        }
                        let a = byte_at(&t.text, at - 1);
                        let b = byte_at(&t.text, at);
                        let now = {
                            let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
                            eng.pair_kerning(t, doc.resolution_dpi, a)
                        }
                        .ok_or_else(|| bad("type.edit", "no kerning pair at the caret (line break or line end)"))?;
                        let value = (f64::from(now.round()) + by).clamp(KERN_MIN, KERN_MAX) as f32;
                        style_range(t, a, b, &|st| {
                            st.kerning = Kerning::Off;
                            st.kern = value;
                        });
                    }
                    if p.get("kerning").is_some() {
                        let (a, b) = range_param(&t.text, p);
                        style_range(t, a, b, &|st| {
                            apply_char_props(st, &json!({ "kerning": p.get("kerning") }));
                        });
                    }
                    if let Some(v) = p.get("text").and_then(Value::as_str) {
                        let n = t.text.len();
                        replace_text(t, 0, n, v);
                    }
                    if let Some(r) = p.get("replace") {
                        let a = byte_at(&t.text, r.get("start").and_then(Value::as_u64).unwrap_or(0) as usize);
                        let b = byte_at(&t.text, r.get("end").and_then(Value::as_u64).map_or(usize::MAX, |x| x as usize));
                        let new = r.get("text").and_then(Value::as_str).unwrap_or("");
                        replace_text(t, a, b.max(a), new);
                    }
                    if let Some(Value::Array(runs)) = p.get("runs") {
                        for r in runs {
                            let a = byte_at(&t.text, r.get("start").and_then(Value::as_u64).unwrap_or(0) as usize);
                            let b = byte_at(&t.text, r.get("end").and_then(Value::as_u64).map_or(usize::MAX, |x| x as usize));
                            style_range(t, a, b, &|st| {
                                apply_char_props(st, r);
                            });
                        }
                    }
                    let lin = t.transform.m;
                    if let Some(b) = p.get("box").and_then(Value::as_array).filter(|b| b.len() == 4) {
                        let v: Vec<f64> = b.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
                        t.shape = TextShape::Box { x: 0.0, y: 0.0, width: v[2].max(1.0) as f32, height: v[3].max(1.0) as f32 };
                        t.transform = Affine { m: [lin[0], lin[1], lin[2], lin[3], v[0], v[1]] };
                    }
                    if let Some(pt) = p.get("point").and_then(Value::as_array).filter(|b| b.len() == 2) {
                        t.shape = TextShape::Point;
                        t.transform = Affine { m: [lin[0], lin[1], lin[2], lin[3], pt[0].as_f64().unwrap_or(0.0), pt[1].as_f64().unwrap_or(0.0)] };
                    }
                    if let Some(d) = p.get("move").and_then(Value::as_array).filter(|b| b.len() == 2) {
                        t.transform.m[4] += d[0].as_f64().unwrap_or(0.0);
                        t.transform.m[5] += d[1].as_f64().unwrap_or(0.0);
                    }
                    if let Some(m) = p.get("transform").and_then(Value::as_array).filter(|b| b.len() == 6) {
                        for (i, v) in m.iter().enumerate() {
                            t.transform.m[i] = v.as_f64().unwrap_or(t.transform.m[i]);
                        }
                    }
                    if let Some(a) = p.get("antialias").and_then(Value::as_str) {
                        t.antialias = match a {
                            "none" => AntiAlias::None,
                            "sharp" => AntiAlias::Sharp,
                            "crisp" => AntiAlias::Crisp,
                            "strong" => AntiAlias::Strong,
                            _ => AntiAlias::Smooth,
                        };
                    }
                    Ok(())
                })?;
                if let Some(n) = name {
                    s.edit("Rename Layer", |doc, _| {
                        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.name = n;
                        Ok(())
                    })?;
                }
                info(s, &json!({ "layer": id.0 }))
            },
        },
        CommandSpec {
            id: "type.setStyle",
            label: "Set Type Style",
            menu: &[],
            shortcut: None,
            params: Box::leak(
                format!(r##"{{"layer":id?,"range":[startChar,endChar]? (default all), {CHAR_PARAMS}, paragraph keys: "align":"left|center|right|justify|justifyCenter|justifyRight|justifyAll","firstLineIndent":pt,"startIndent":pt,"endIndent":pt,"spaceBefore":pt,"spaceAfter":pt,"autoLeading":%,"direction":"auto|ltr|rtl","hyphenate":bool}}"##)
                    .into_boxed_str(),
            ),
            enabled: has_doc,
            journal: true,
            run: |s, p| {
                let id = layer_id(s, p)?;
                check_kerning(p).map_err(|m| bad("type.setStyle", m))?;
                with_text_layer(s, p, "Set Type Style", |t, _| {
                    let (a, b) = range_param(&t.text, p);
                    let mut probe = CharStyle::default();
                    if apply_char_props(&mut probe, p) {
                        style_range(t, a, b, &|st| {
                            apply_char_props(st, p);
                        });
                    }
                    let mut pprobe = ParagraphStyle::default();
                    if apply_para_props(&mut pprobe, p) {
                        style_paragraphs(t, a, b, &|st| {
                            apply_para_props(st, p);
                        });
                    }
                    Ok(())
                })?;
                info(s, &json!({ "layer": id.0 }))
            },
        },
        CommandSpec {
            id: "type.rasterize",
            label: "Rasterize Type",
            menu: &["Layer", "Rasterize"],
            shortcut: None,
            params: r##"{"layer":id?}"##,
            enabled: has_doc,
            journal: true,
            run: |s, p| {
                let id = layer_id(s, p)?;
                s.edit("Rasterize Type", |doc, _| {
                    let snapshot = doc.clone();
                    let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                    let LayerContent::Text(t) = &mut l.content else {
                        return Err(EngineError::Other(format!("layer {} is not a type layer", id.0)));
                    };
                    if t.cache.is_none() {
                        refresh(&snapshot, t);
                    }
                    let surface = t.cache.take().unwrap_or_else(|| photocraft_raster::Surface::new(snapshot.pixel_format()));
                    let surface = if surface.format() == snapshot.pixel_format() { surface } else { surface.convert(snapshot.pixel_format()) };
                    l.content = LayerContent::Raster(surface);
                    l.psd_blocks.retain(|(k, _)| k != b"TySh");
                    Ok(())
                })?;
                Ok(json!({ "layer": id.0 }))
            },
        },
        CommandSpec {
            id: "type.info",
            label: "Type Layer Info",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?} → text, runs/paragraphs (char offsets + styles), shape, transform, laid-out lines (text space px), bounds"##,
            enabled: has_doc,
            journal: false,
            run: |s, p| info(s, p),
        },
        CommandSpec {
            id: "type.fonts",
            label: "List Fonts",
            menu: &[],
            shortcut: None,
            params: r##"{"family":str? (faces of one family)}"##,
            enabled: |_| Ok(()),
            journal: false,
            run: |_, p| {
                let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
                match p.get("family").and_then(Value::as_str) {
                    Some(f) => {
                        let faces: Vec<Value> = eng.fonts.faces(f).into_iter().map(|x| json!({ "family": x.family, "weight": x.weight, "italic": x.italic, "axes": x.axes })).collect();
                        Ok(json!({ "faces": faces }))
                    }
                    None => Ok(json!({ "families": eng.fonts.families() })),
                }
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100, "background": "transparent"})).unwrap();
        s
    }

    fn text_layer(s: &Session, id: u64) -> TextLayer {
        match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
            LayerContent::Text(t) => t.clone(),
            other => panic!("{}", other.kind_name()),
        }
    }

    #[test]
    fn create_renders_and_is_undoable() {
        let mut s = session();
        let r = s.execute("type.create", json!({"x": 10, "y": 50, "text": "Hello", "size": 24, "color": "#ff0000"})).unwrap();
        let id = r["layer"].as_u64().unwrap();
        let b = r["bounds"].as_array().unwrap();
        assert!(b[0].as_i64().unwrap() >= 9 && b[3].as_i64().unwrap() <= 56, "{b:?}");
        let t = text_layer(&s, id);
        assert_eq!(t.text, "Hello");
        assert_eq!(t.runs[0].style.size_pt, 24.0);
        assert!(t.cache.is_some() && t.psd_raw.is_some());
        let l = s.active().unwrap().doc.layer(LayerId(id)).unwrap();
        assert_eq!(l.name, "Hello");
        // Red pixels exist.
        let c = t.cache.as_ref().unwrap();
        let r = c.content_bounds();
        assert!(c.read_region(r).as_chunks::<4>().0.iter().any(|p| p[3] > 0.9 && p[0] > 0.9 && p[1] < 0.1));
        // The TySh we generated parses back to the same model.
        let back = photocraft_text::psd::text_layer_from_tysh(t.psd_raw.as_ref().unwrap(), 72.0).unwrap();
        assert_eq!(back.text, "Hello");
        let strip = |mut r: Vec<TextRun>| {
            for x in &mut r {
                x.style.postscript_name = None;
            }
            r
        };
        assert_eq!(strip(back.char_runs()), strip(t.char_runs()));
        assert_eq!(back.char_runs()[0].style.postscript_name.as_deref(), Some("Inter-Regular"));
        assert!(s.undo());
        assert!(s.active().unwrap().doc.layer(LayerId(id)).is_none());
    }

    #[test]
    fn edit_replace_keeps_styles() {
        let mut s = session();
        let id = s.execute("type.create", json!({"x": 0, "y": 40, "text": "Hello world"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("type.setStyle", json!({"layer": id, "range": [6, 11], "fauxBold": true, "color": [0, 0, 1]})).unwrap();
        let t = text_layer(&s, id);
        assert_eq!(t.runs.len(), 2);
        assert_eq!(t.runs[0].len, 6);
        assert!(t.runs[1].style.faux_bold);
        // Insert inside the bold word: inherits bold.
        s.execute("type.edit", json!({"layer": id, "replace": {"start": 8, "end": 8, "text": "XX"}})).unwrap();
        let t = text_layer(&s, id);
        assert_eq!(t.text, "Hello woXXrld");
        assert_eq!(t.runs[1].len, 7);
        // Delete across the style boundary.
        s.execute("type.edit", json!({"layer": id, "replace": {"start": 3, "end": 8, "text": ""}})).unwrap();
        let t = text_layer(&s, id);
        assert_eq!(t.text, "HelXXrld");
        assert_eq!(t.runs.iter().map(|r| r.len).collect::<Vec<_>>(), vec![3, 5]);
        // Whole-text replace with a multi-byte string.
        let info = s.execute("type.edit", json!({"layer": id, "text": "Größe ✓\nzwei"})).unwrap();
        assert_eq!(info["lines"].as_array().unwrap().len(), 2);
        let t = text_layer(&s, id);
        assert_eq!(t.runs.iter().map(|r| r.len).sum::<usize>(), t.text.len());
    }

    #[test]
    fn set_style_paragraph_and_box() {
        let mut s = session();
        let id = s.execute("type.create", json!({"box": [10, 10, 80, 80], "text": "one two three four five six seven", "size": 12})).unwrap()["layer"]
            .as_u64()
            .unwrap();
        let info = s.execute("type.setStyle", json!({"layer": id, "align": "center", "leading": 20})).unwrap();
        let lines = info["lines"].as_array().unwrap();
        assert!(lines.len() >= 3);
        let b0 = lines[0]["baseline"].as_f64().unwrap();
        let b1 = lines[1]["baseline"].as_f64().unwrap();
        assert!((b1 - b0 - 20.0).abs() < 1e-3, "explicit leading {b0} {b1}");
        assert_eq!(info["paragraphs"][0]["style"]["align"], "Center");
        let t = text_layer(&s, id);
        assert!(matches!(t.shape, TextShape::Box { width, .. } if width == 80.0));
        // Pixels stay inside the box horizontally.
        let r = t.cache.as_ref().unwrap().content_bounds();
        assert!(r.x0 >= 9 && r.x1 <= 91, "{r:?}");
        // Convert to point text and move.
        s.execute("type.edit", json!({"layer": id, "point": [5, 30]})).unwrap();
        s.execute("type.edit", json!({"layer": id, "move": [10, 0]})).unwrap();
        let t = text_layer(&s, id);
        assert_eq!(t.shape, TextShape::Point);
        assert_eq!(t.transform.m[4], 15.0);
    }

    #[test]
    fn rasterize_and_fonts() {
        let mut s = session();
        let id = s.execute("type.create", json!({"x": 5, "y": 30, "text": "Raster"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("type.rasterize", json!({"layer": id})).unwrap();
        let l = s.active().unwrap().doc.layer(LayerId(id)).unwrap();
        assert!(matches!(l.content, LayerContent::Raster(_)));
        assert!(l.surface().unwrap().content_bounds().width() > 10);
        assert!(s.execute("type.edit", json!({"layer": id, "text": "x"})).is_err());
        let fonts = s.execute("type.fonts", json!({})).unwrap();
        assert!(fonts["families"].as_array().unwrap().iter().any(|f| f == "Inter"));
        let faces = s.execute("type.fonts", json!({"family": "Inter"})).unwrap();
        assert!(!faces["faces"].as_array().unwrap().is_empty());
    }

    /// #124: a type edit damages only the layer's old and new pixels (the canvas recomposites
    /// that, not the document), and a drag's coalesced steps are one history step.
    #[test]
    fn type_edits_damage_only_the_layer_and_coalesce() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 3000, "height": 2000})).unwrap();
        let id = s.execute("type.create", json!({"x": 100, "y": 100, "text": "Hi", "size": 20})).unwrap()["layer"].as_u64().unwrap();
        let steps = s.active().unwrap().history.entries().len();
        for k in 0..5 {
            s.execute("type.setStyle", json!({"layer": id, "size": 30 + k * 10, "coalesce": "drag"})).unwrap();
            let d = s.active().unwrap().last_damage.expect("damage rect");
            let r = text_layer(&s, id).cache.unwrap().content_bounds();
            assert!(d.intersect(&r) == r && d.width() <= 512 && d.height() <= 512, "{d:?} {r:?}");
        }
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 1);
        assert!(s.undo());
        assert_eq!(text_layer(&s, id).runs[0].style.size_pt, 20.0);
    }

    #[test]
    fn style_range_on_empty_text() {
        let mut t = TextLayer::default();
        style_range(&mut t, 0, 0, &|s| s.size_pt = 40.0);
        assert_eq!(t.runs[0].style.size_pt, 40.0);
        replace_text(&mut t, 0, 0, "abc");
        assert_eq!(t.runs.len(), 1);
        assert_eq!(t.runs[0].len, 3);
        assert_eq!(t.runs[0].style.size_pt, 40.0);
    }

    fn kerning_of(s: &Session, id: u64) -> Vec<(Kerning, f32)> {
        let t = text_layer(s, id);
        let mut out = Vec::new();
        let mut at = 0;
        for r in t.char_runs() {
            out.extend(std::iter::repeat_n((r.style.kerning, r.style.kern), t.text[at..at + r.len].chars().count()));
            at += r.len;
        }
        out
    }

    /// #206: `type.edit {"kerning"}` over a range takes a number (manual, 1/1000 em) or a mode.
    #[test]
    fn edit_sets_kerning_over_a_range() {
        let mut s = session();
        let id = s.execute("type.create", json!({"x": 5, "y": 50, "text": "AVAT", "size": 30})).unwrap()["layer"].as_u64().unwrap();
        s.execute("type.edit", json!({"layer": id, "range": [0, 1], "kerning": 100})).unwrap();
        s.execute("type.edit", json!({"layer": id, "range": [2, 4], "kerning": "optical"})).unwrap();
        assert_eq!(kerning_of(&s, id), vec![(Kerning::Off, 100.0), (Kerning::Metrics, 0.0), (Kerning::Optical, 0.0), (Kerning::Optical, 0.0)]);
        s.execute("type.edit", json!({"layer": id, "kerning": "metrics"})).unwrap();
        assert!(kerning_of(&s, id).iter().all(|k| *k == (Kerning::Metrics, 0.0)));
        // Runs and setStyle take the same values; type.info reports them.
        s.execute("type.edit", json!({"layer": id, "runs": [{"start": 1, "end": 2, "kerning": -50}]})).unwrap();
        s.execute("type.setStyle", json!({"layer": id, "range": [3, 4], "kerning": "off"})).unwrap();
        assert_eq!(kerning_of(&s, id)[1], (Kerning::Off, -50.0));
        assert_eq!(kerning_of(&s, id)[3], (Kerning::Off, 0.0));
        let info = s.execute("type.info", json!({"layer": id})).unwrap();
        assert_eq!(info["runs"][1]["style"]["kern"], json!(-50.0));
    }

    /// Bad kerning params are errors, never panics, and leave the layer alone.
    #[test]
    fn kerning_params_fail_gracefully() {
        let mut s = session();
        let id = s.execute("type.create", json!({"x": 5, "y": 50, "text": "AV\nT", "size": 30})).unwrap()["layer"].as_u64().unwrap();
        let before = text_layer(&s, id);
        for p in [
            json!({"kerning": "tight"}),
            json!({"kerning": 1e9}),
            json!({"kerning": -5000}),
            json!({"kerning": true}),
            json!({"kerning": [1]}),
            json!({"runs": [{"start": 0, "end": 1, "kerning": "x"}]}),
            json!({"kernPair": 3}),
            json!({"kernPair": {"by": 20}}),
            json!({"kernPair": {"at": 1}}),
            json!({"kernPair": {"at": 1, "by": "20"}}),
            json!({"kernPair": {"at": 1, "by": 1e12}}),
            json!({"kernPair": {"at": 0, "by": 20}}),
            json!({"kernPair": {"at": 4, "by": 20}}),
            json!({"kernPair": {"at": 99, "by": 20}}),
            json!({"kernPair": {"at": 2, "by": 20}}),
            json!({"kernPair": {"at": 3, "by": 20}}),
            json!({"kernPair": {"at": -1, "by": 20}}),
        ] {
            let mut p = p;
            p["layer"] = json!(id);
            assert!(s.execute("type.edit", p.clone()).is_err(), "{p}");
        }
        assert!(s.execute("type.setStyle", json!({"layer": id, "kerning": "tight"})).is_err());
        assert_eq!(text_layer(&s, id).runs, before.runs);
    }

    /// Cost of one Alt+←/→ press (`kernPair`: two measuring layouts + the edit's re-render) on
    /// a headline and on a 2000-character paragraph. Release timings for the dev log:
    /// `cargo test --release -p photocraft-engine kern_pair_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn kern_pair_cost() {
        let para = "The quick brown fox jumps over the lazy dog. ".repeat(45);
        for (name, text, size, bx, kerning) in [
            ("headline", "AVATAR Wave", 72, false, "metrics"),
            ("paragraph", para.as_str(), 12, true, "metrics"),
            ("optical paragraph", para.as_str(), 12, true, "optical"),
        ] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 2000, "height": 1500})).unwrap();
            let mut p = json!({"x": 20, "y": 100, "text": text, "size": size, "font": "Inter", "kerning": kerning});
            if bx {
                p["box"] = json!([20, 20, 900, 1400]);
            }
            let id = s.execute("type.create", p).unwrap()["layer"].as_u64().unwrap();
            let n = 20;
            let t0 = std::time::Instant::now();
            for i in 0..n {
                s.execute("type.edit", json!({"layer": id, "kernPair": {"at": 1 + i % 3, "by": 20}})).unwrap();
            }
            let per = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
            let t1 = std::time::Instant::now();
            for i in 0..n {
                s.execute("type.setStyle", json!({"layer": id, "range": [1 + i % 3, 2 + i % 3], "tracking": i})).unwrap();
            }
            let base = t1.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
            eprintln!("{name} ({} chars): kernPair {per:.2} ms/press; a plain style edit {base:.2} ms", text.chars().count());
        }
    }

    /// Photoshop's Alt+←/→: the pair before the caret becomes manual, starting from the kerning
    /// it shows (the font's pair kerning for "AV"), ±20 per press, one history step each.
    #[test]
    fn kern_pair_steps_from_the_shown_kerning() {
        let mut s = session();
        let id = s.execute("type.create", json!({"x": 5, "y": 50, "text": "AVA", "size": 40, "font": "Inter"})).unwrap()["layer"].as_u64().unwrap();
        let metric = {
            let t = text_layer(&s, id);
            photocraft_text::shared().lock().unwrap().pair_kerning(&t, 72.0, 0).unwrap()
        };
        assert!(metric < -10.0, "Inter kerns AV: {metric}");
        let steps = s.active().unwrap().history.entries().len();
        s.execute("type.edit", json!({"layer": id, "kernPair": {"at": 1, "by": 20}})).unwrap();
        let k0 = kerning_of(&s, id)[0];
        assert_eq!(k0, (Kerning::Off, metric.round() + 20.0));
        s.execute("type.edit", json!({"layer": id, "kernPair": {"at": 1, "by": 100}})).unwrap();
        assert_eq!(kerning_of(&s, id)[0].1, metric.round() + 120.0);
        s.execute("type.edit", json!({"layer": id, "kernPair": {"at": 1, "by": -20}})).unwrap();
        assert_eq!(kerning_of(&s, id)[0].1, metric.round() + 100.0);
        // Other characters are untouched; each press is its own undoable step.
        assert_eq!(&kerning_of(&s, id)[1..], &[(Kerning::Metrics, 0.0), (Kerning::Metrics, 0.0)]);
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 3);
        assert!(s.undo());
        assert_eq!(kerning_of(&s, id)[0].1, metric.round() + 120.0);
        assert!(s.undo() && s.undo());
        assert_eq!(kerning_of(&s, id)[0], (Kerning::Metrics, 0.0));
    }
}

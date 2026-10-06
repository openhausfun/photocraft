//! PSD type layers: the `TySh` block (transform + text descriptor with `EngineData` + warp) ⇄ the
//! typed [`TextLayer`] model.
//!
//! Units: EngineData sizes are in text-space units, which the `TySh` transform maps to document
//! pixels; Photoshop shows them as points at the document resolution, so
//! `size_pt = FontSize × 72 / dpi` (and likewise leading, tracking-independent lengths, indents).
//! Box bounds stay in text-space pixels.
//!
//! Writing starts from the layer's original `TySh` (when present) and patches only the text, runs,
//! paragraphs, fonts, shape and transform, so Photoshop-only settings survive an edit.

use photocraft_color::{Color, ColorMode};
use photocraft_doc::TextLayer;
use photocraft_doc::text::{AntiAlias, Caps, CharStyle, Kerning, Orientation, ParagraphRun, ParagraphStyle, TextAlign, TextRun, TextShape, TextWarp};
use photocraft_geom::Affine;
use photocraft_psd::descriptor::{Descriptor, Id, UnicodeString, Value as D, VersionedDescriptor};

use crate::engine_data::{self as ed, Value as E};
use crate::fonts::guess_from_postscript;

/// Parsed `TySh` contents.
#[derive(Clone, Debug, PartialEq)]
pub struct TySh {
    pub transform: Affine,
    pub text: Descriptor,
    pub warp: Option<Descriptor>,
    /// The four trailing i32 values (left, top, right, bottom).
    pub bounds: [i32; 4],
}

/// Parses a `TySh` block.
pub fn parse_tysh(data: &[u8]) -> Option<TySh> {
    let f = |at: usize| data.get(at..at + 8).map(|b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])));
    let mut m = [0.0; 6];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f(2 + i * 8)?;
    }
    let (text, used) = VersionedDescriptor::parse_prefix(data.get(52..)?).ok()?;
    let rest = data.get(52 + used..).unwrap_or(&[]);
    let (warp, wused) = match rest.get(2..).and_then(|r| VersionedDescriptor::parse_prefix(r).ok()) {
        Some((w, n)) => (Some(w.descriptor), n + 2),
        None => (None, 0),
    };
    let tail = rest.get(wused..).unwrap_or(&[]);
    let mut bounds = [0i32; 4];
    for (i, b) in bounds.iter_mut().enumerate() {
        if let Some(x) = tail.get(i * 4..i * 4 + 4) {
            *b = i32::from_be_bytes(x.try_into().unwrap_or([0; 4]));
        }
    }
    Some(TySh { transform: Affine { m }, text: text.descriptor, warp, bounds })
}

/// Serializes a `TySh` block.
pub fn write_tysh(t: &TySh) -> Vec<u8> {
    let mut v = 1u16.to_be_bytes().to_vec();
    for x in t.transform.m {
        v.extend_from_slice(&x.to_be_bytes());
    }
    v.extend_from_slice(&50u16.to_be_bytes());
    v.extend(VersionedDescriptor::new(t.text.clone()).to_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend(VersionedDescriptor::new(t.warp.clone().unwrap_or_else(default_warp)).to_bytes());
    for b in t.bounds {
        v.extend_from_slice(&b.to_be_bytes());
    }
    v
}

fn default_warp() -> Descriptor {
    Descriptor::new("warp")
        .with("warpStyle", enumv("warpStyle", "warpNone"))
        .with("warpValue", D::Double(0.0))
        .with("warpPerspective", D::Double(0.0))
        .with("warpPerspectiveOther", D::Double(0.0))
        .with("warpRotate", enumv("Ornt", "Hrzn"))
}

fn enumv(t: &str, v: &str) -> D {
    D::Enumerated { type_id: Id::new(t), value: Id::new(v) }
}

fn enum_value(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key) {
        Some(D::Enumerated { value, .. }) => Some(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        _ => None,
    }
}

fn dnum(v: Option<&D>) -> Option<f64> {
    match v {
        Some(D::Double(x)) => Some(*x),
        Some(D::UnitFloat { value, .. }) => Some(*value),
        Some(D::Integer(i)) => Some(f64::from(*i)),
        _ => None,
    }
}

/// The EngineData of a text descriptor.
pub fn engine_data(text: &Descriptor) -> Option<E> {
    match text.get("EngineData") {
        Some(D::RawData(b)) => ed::parse(b).ok(),
        _ => None,
    }
}

/// Parses the document's `Txt2` block (Photoshop's text engine data for all type layers).
pub fn parse_txt2(data: &[u8]) -> Option<E> {
    // Txt2 is a bare sequence of `/key value` pairs (no enclosing `<< >>`).
    let mut wrapped = Vec::with_capacity(data.len() + 4);
    wrapped.extend_from_slice(b"<<");
    wrapped.extend_from_slice(data);
    wrapped.extend_from_slice(b">>");
    ed::parse(&wrapped).ok().filter(|v| matches!(v, E::Dict(_)))
}

/// Applies the automatic kerning modes that Photoshop keeps only in `Txt2` (EngineData writes
/// Optical and "0" alike as `AutoKerning true`) to `layer`, read from `tysh`. Only when the
/// layer's `TextIndex` entry still holds the layer's text (an edit elsewhere leaves `Txt2`
/// stale).
///
/// Observed layout: `Txt2 /1` is the list of text objects; an object's `/0` is its text model
/// with `/0` the text (`\r` breaks, trailing `\r`) and `/6 /0` its style runs, each
/// `<< /0 << /0 << … /6 <<style>> >> >> /1 utf16-length >>`; style key `/11` is the auto-kern
/// mode of the pair after each character: 0 manual, 1 metrics, 2 optical.
pub fn apply_txt2(layer: &mut TextLayer, tysh: &[u8], txt2: &E) {
    let Some(index) = parse_tysh(tysh).and_then(|t| match t.text.get("TextIndex") {
        Some(D::Integer(i)) => usize::try_from(*i).ok(),
        _ => None,
    }) else {
        return;
    };
    let Some(model) = txt2.path(&["1", "1"]).and_then(E::as_array).and_then(|a| a.get(index)).and_then(|o| o.get("0")) else {
        return;
    };
    let Some(text) = model.get("0").and_then(E::as_str) else {
        return;
    };
    if text.strip_suffix('\r').unwrap_or(text).replace('\r', "\n") != layer.text {
        return;
    }
    let Some(style_runs) = model.path(&["6", "0"]).and_then(E::as_array) else {
        return;
    };
    let modes: Vec<(f64, Option<i64>)> =
        style_runs.iter().map(|r| (r.get("1").and_then(E::as_f64).unwrap_or(0.0), r.path(&["0", "0", "6", "11"]).and_then(E::as_i64))).collect();
    let lens = utf16_to_byte_lengths(text, &modes.iter().map(|m| m.0).collect::<Vec<_>>());
    // Auto-kern mode per byte segment, then split the model runs at segment boundaries.
    let mut segs: Vec<(usize, Option<i64>)> = lens.into_iter().zip(modes.iter().map(|m| m.1)).collect();
    segs.retain(|s| s.0 > 0);
    let runs = layer.char_runs();
    let mut out: Vec<TextRun> = Vec::new();
    let (mut si, mut seg_end) = (0usize, segs.first().map_or(usize::MAX, |s| s.0));
    let mut at = 0usize;
    for r in runs {
        let end = at + r.len;
        while at < end {
            while at >= seg_end && si + 1 < segs.len() {
                si += 1;
                seg_end = seg_end.saturating_add(segs.get(si).map_or(0, |s| s.0));
            }
            let piece_end = if at < seg_end { end.min(seg_end) } else { end };
            let mut style = r.style.clone();
            // Txt2 has the exact mode (EngineData's reading also marks neighbours of manual
            // kerns manual, like Photoshop's legacy reader).
            match segs.get(si).and_then(|s| s.1).filter(|_| at < seg_end) {
                Some(0) => style.kerning = Kerning::Off,
                Some(1) => style.kerning = Kerning::Metrics,
                Some(2) => style.kerning = Kerning::Optical,
                _ => {}
            }
            let len = piece_end - at;
            match out.last_mut() {
                Some(last) if last.style == style => last.len += len,
                _ => out.push(TextRun { len, style }),
            }
            at = piece_end;
        }
    }
    layer.runs = out;
    layer.sync_summary();
}

/// Builds a [`TextLayer`] (model, text and transform; no cache, no `psd_raw`) from `TySh` data.
pub fn text_layer_from_tysh(data: &[u8], dpi: f32) -> Option<TextLayer> {
    let t = parse_tysh(data)?;
    let mut layer = TextLayer { transform: t.transform, ..Default::default() };
    let txt = match t.text.get("Txt ") {
        Some(D::Text(s)) => Some(s.to_string_lossy().trim_end_matches('\0').to_string()),
        _ => None,
    };
    layer.orientation = if enum_value(&t.text, "Ornt").as_deref() == Some("Vrtc") { Orientation::Vertical } else { Orientation::Horizontal };
    layer.antialias = match enum_value(&t.text, "AntA").as_deref() {
        Some("Anno" | "antiAliasNone") => AntiAlias::None,
        Some("antiAliasSharp" | "AnSh") => AntiAlias::Sharp,
        Some("AnCr" | "antiAliasCrisp") => AntiAlias::Crisp,
        Some("AnSt" | "antiAliasStrong") => AntiAlias::Strong,
        Some("antiAliasPlatformGray") => AntiAlias::Windows,
        Some("antiAliasPlatformLCD") => AntiAlias::WindowsLcd,
        _ => AntiAlias::Smooth,
    };
    if let Some(w) = &t.warp {
        let style = enum_value(w, "warpStyle").unwrap_or_else(|| "warpNone".into());
        if style != "warpNone" {
            layer.warp = Some(TextWarp {
                style,
                value: dnum(w.get("warpValue")).unwrap_or(0.0) as f32,
                horizontal_distortion: dnum(w.get("warpPerspective")).unwrap_or(0.0) as f32,
                vertical_distortion: dnum(w.get("warpPerspectiveOther")).unwrap_or(0.0) as f32,
                horizontal: enum_value(w, "warpRotate").as_deref() != Some("Vrtc"),
            });
        }
    }
    let k = 72.0 / if dpi > 0.0 { dpi } else { 72.0 };
    if let Some(e) = engine_data(&t.text) {
        // Without an `Ornt` key, the engine data's writing direction says it (2 = vertical).
        if enum_value(&t.text, "Ornt").is_none() && e.path(&["EngineDict", "Rendered", "Shapes", "WritingDirection"]).and_then(E::as_f64) == Some(2.0) {
            layer.orientation = Orientation::Vertical;
        }
        apply_engine_data(&mut layer, &e, txt.as_deref(), k);
    } else {
        layer.text = txt.unwrap_or_default().replace('\r', "\n");
    }
    layer.sync_summary();
    Some(layer)
}

pub(crate) fn arr_f(v: Option<&E>) -> Vec<f64> {
    v.and_then(E::as_array).map(|a| a.iter().filter_map(E::as_f64).collect()).unwrap_or_default()
}

/// Converts UTF-16 run lengths over `text` into UTF-8 byte lengths.
pub(crate) fn utf16_to_byte_lengths(text: &str, lens: &[f64]) -> Vec<usize> {
    let mut out = Vec::with_capacity(lens.len());
    let mut chars = text.chars().peekable();
    for &l in lens {
        let mut units = l.max(0.0) as usize;
        let mut bytes = 0;
        while units > 0 {
            match chars.next() {
                Some(c) => {
                    units = units.saturating_sub(c.len_utf16());
                    bytes += c.len_utf8();
                }
                None => break,
            }
        }
        out.push(bytes);
    }
    out
}

fn apply_engine_data(layer: &mut TextLayer, e: &E, txt: Option<&str>, k: f32) {
    let ed_text = e.path(&["EngineDict", "Editor", "Text"]).and_then(E::as_str).unwrap_or("").to_string();
    // Photoshop always ends the engine text with a paragraph break; the descriptor text doesn't.
    let text = txt.map(str::to_string).unwrap_or_else(|| ed_text.strip_suffix('\r').unwrap_or(&ed_text).to_string());
    layer.text = text.replace('\r', "\n");
    let res = e.get("ResourceDict").or_else(|| e.get("DocumentResources"));
    let fonts: Vec<String> = res
        .and_then(|r| r.get("FontSet"))
        .and_then(E::as_array)
        .map(|a| a.iter().map(|f| f.get("Name").and_then(E::as_str).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let default_sheet = res.and_then(|r| r.get("TheNormalStyleSheet")).and_then(E::as_i64).unwrap_or(0) as usize;
    let base_style =
        res.and_then(|r| r.get("StyleSheetSet")).and_then(E::as_array).and_then(|a| a.get(default_sheet).or(a.first())).and_then(|s| s.get("StyleSheetData"));
    let default_para_sheet = res.and_then(|r| r.get("TheNormalParagraphSheet")).and_then(E::as_i64).unwrap_or(0) as usize;
    let base_para = res
        .and_then(|r| r.get("ParagraphSheetSet"))
        .and_then(E::as_array)
        .and_then(|a| a.get(default_para_sheet).or(a.first()))
        .and_then(|s| s.get("Properties"));

    // Character runs.
    let srun = e.path(&["EngineDict", "StyleRun"]);
    let sdata: Vec<&E> = srun
        .and_then(|r| r.get("RunArray"))
        .and_then(E::as_array)
        .map(|a| a.iter().filter_map(|x| x.path(&["StyleSheet", "StyleSheetData"])).collect())
        .unwrap_or_default();
    let slens = utf16_to_byte_lengths(&ed_text, &arr_f(srun.and_then(|r| r.get("RunLengthArray"))));
    let incoming: Vec<(usize, bool, f32)> = sdata
        .iter()
        .zip(&slens)
        .map(|(d, len)| {
            let auto = lookup(d, base_style, "AutoKerning").and_then(E::as_bool) != Some(false);
            let kern = lookup(d, base_style, "Kerning").and_then(E::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0) as f32;
            (*len, auto, kern)
        })
        .collect();
    layer.runs = sdata.iter().zip(slens).map(|(d, len)| TextRun { len, style: char_style(base_style, d, &fonts, k) }).collect();
    layer.runs = kerning_from_pairs(&layer.text, layer.char_runs(), &incoming);

    // Paragraph runs.
    let prun = e.path(&["EngineDict", "ParagraphRun"]);
    let pdata: Vec<&E> = prun
        .and_then(|r| r.get("RunArray"))
        .and_then(E::as_array)
        .map(|a| a.iter().filter_map(|x| x.path(&["ParagraphSheet", "Properties"])).collect())
        .unwrap_or_default();
    let plens = utf16_to_byte_lengths(&ed_text, &arr_f(prun.and_then(|r| r.get("RunLengthArray"))));
    layer.paragraphs = pdata.iter().zip(plens).map(|(d, len)| ParagraphRun { len, style: para_style(base_para, d, k) }).collect();

    // Shape.
    let shape = e.path(&["EngineDict", "Rendered", "Shapes", "Children"]).and_then(E::as_array).and_then(|a| a.first());
    if let Some(sh) = shape
        && sh.get("ShapeType").and_then(E::as_i64) == Some(1)
    {
        let b = arr_f(sh.path(&["Cookie", "Photoshop", "BoxBounds"]));
        if b.len() == 4 {
            layer.shape = TextShape::Box { x: b[0] as f32, y: b[1] as f32, width: (b[2] - b[0]) as f32, height: (b[3] - b[1]) as f32 };
        }
    }
}

fn lookup<'a>(run: &'a E, base: Option<&'a E>, key: &str) -> Option<&'a E> {
    run.get(key).or_else(|| base.and_then(|b| b.get(key)))
}

pub(crate) fn char_style(base: Option<&E>, d: &E, fonts: &[String], k: f32) -> CharStyle {
    let g = |key: &str| lookup(d, base, key);
    let num = |key: &str| g(key).and_then(E::as_f64);
    let flag = |key: &str| g(key).and_then(E::as_bool);
    let mut s = CharStyle::default();
    if let Some(ps) = g("Font").and_then(E::as_i64).and_then(|i| fonts.get(i as usize)) {
        let r = guess_from_postscript(ps);
        s.font_family = r.family;
        s.weight = r.weight;
        s.italic = r.italic;
        s.postscript_name = Some(ps.clone());
    }
    if let Some(v) = num("FontSize") {
        s.size_pt = v as f32 * k;
    }
    if flag("AutoLeading") == Some(false)
        && let Some(v) = num("Leading")
    {
        s.leading_pt = Some(v as f32 * k);
    }
    s.tracking = num("Tracking").unwrap_or(0.0) as f32;
    s.baseline_shift_pt = num("BaselineShift").unwrap_or(0.0) as f32 * k;
    s.horizontal_scale = num("HorizontalScale").unwrap_or(1.0) as f32;
    s.vertical_scale = num("VerticalScale").unwrap_or(1.0) as f32;
    s.faux_bold = flag("FauxBold").unwrap_or(false);
    s.faux_italic = flag("FauxItalic").unwrap_or(false);
    s.underline = flag("Underline").unwrap_or(false);
    s.strikethrough = flag("Strikethrough").unwrap_or(false);
    s.ligatures = flag("Ligatures").unwrap_or(true);
    s.discretionary_ligatures = flag("DLigatures").unwrap_or(false);
    for (key, tag) in OPENTYPE_KEYS {
        if flag(key) == Some(true) {
            s.features.push(photocraft_doc::text::FontFeature { tag: tag.to_string(), value: 1 });
        }
    }
    s.kerning = if flag("AutoKerning") == Some(false) { Kerning::Off } else { Kerning::Metrics };
    s.kern = num("Kerning").filter(|v| v.is_finite()).unwrap_or(0.0) as f32;
    s.caps = match g("FontCaps").and_then(E::as_i64) {
        Some(1) => Caps::SmallCaps,
        Some(2) => Caps::AllCaps,
        _ => Caps::Normal,
    };
    if let Some(c) = g("FillColor") {
        let v = arr_f(c.get("Values"));
        let a = v.first().copied().unwrap_or(1.0) as f32;
        s.color = match v.len() {
            5 => Color { mode: ColorMode::Cmyk, c: [v[1] as f32, v[2] as f32, v[3] as f32, v[4] as f32], alpha: a },
            2 => Color { mode: ColorMode::Grayscale, c: [v[1] as f32, 0.0, 0.0, 0.0], alpha: a },
            4 => Color::rgba(v[1] as f32, v[2] as f32, v[3] as f32, a),
            _ => Color::BLACK,
        };
    }
    s
}

pub(crate) fn para_style(base: Option<&E>, d: &E, k: f32) -> ParagraphStyle {
    let num = |key: &str| lookup(d, base, key).and_then(E::as_f64);
    ParagraphStyle {
        align: match num("Justification").map(|v| v as i64) {
            Some(1) => TextAlign::Right,
            Some(2) => TextAlign::Center,
            Some(3) => TextAlign::JustifyLeft,
            Some(4) => TextAlign::JustifyRight,
            Some(5) => TextAlign::JustifyCenter,
            Some(6) => TextAlign::JustifyAll,
            _ => TextAlign::Left,
        },
        first_line_indent_pt: num("FirstLineIndent").unwrap_or(0.0) as f32 * k,
        start_indent_pt: num("StartIndent").unwrap_or(0.0) as f32 * k,
        end_indent_pt: num("EndIndent").unwrap_or(0.0) as f32 * k,
        space_before_pt: num("SpaceBefore").unwrap_or(0.0) as f32 * k,
        space_after_pt: num("SpaceAfter").unwrap_or(0.0) as f32 * k,
        auto_leading: num("AutoLeading").unwrap_or(1.2) as f32,
        hyphenate: lookup(d, base, "AutoHyphenate").and_then(E::as_bool).unwrap_or(false),
        ..Default::default()
    }
}

// ---------- writing ----------

fn real(v: f32) -> E {
    E::Real(f64::from(v))
}

/// Manual kerning as Photoshop stores it: a whole number of 1/1000 em (its UI range is ±1000;
/// anything non-finite writes 0).
pub(crate) fn kern_units(v: f32) -> i64 {
    if v.is_finite() { v.round().clamp(-10_000.0, 10_000.0) as i64 } else { 0 }
}

// Kerning in EngineData, as observed in Photoshop 2026 (writing, and reading files without
// `Txt2`): a manual kern k (1/1000 em) after character c is written on character c + 1 as
// `AutoKerning false` + `Kerning k`; everything else is `true` + 0. Photoshop's reader takes
// `Kerning k` on character j as the kern after j - 1 (making j - 1 manual), and
// `AutoKerning false` on j as "character j - 2 is manually kerned" (no automatic kerning). The
// automatic mode itself (Metrics or Optical) isn't in EngineData: Photoshop keeps it in the
// document's `Txt2` (see [`apply_txt2`]). We write and read the same way, so Photoshop reads
// our kerning as we do and Photoshop's own fields round-trip unchanged. (The mode of the last
// character has no slot and reads back as Metrics.)

fn manual(s: &CharStyle) -> bool {
    s.kerning == Kerning::Off || kern_units(s.kern) != 0
}

/// EngineData style runs for `ed_text` (the layer text with `\r` breaks and the trailing `\r`):
/// (model run index, `(AutoKerning, Kerning)`, UTF-16 length), split where those change.
fn pair_runs(ed_text: &str, runs: &[TextRun]) -> Vec<(usize, (bool, i64), usize)> {
    // Model run of each character (the trailing `\r` takes the last run).
    let mut owner: Vec<(usize, usize)> = Vec::with_capacity(ed_text.len());
    let (mut ri, mut run_end) = (0usize, runs.first().map_or(0, |r| r.len));
    for (b, ch) in ed_text.char_indices() {
        while b >= run_end && ri + 1 < runs.len() {
            ri += 1;
            run_end = run_end.saturating_add(runs.get(ri).map_or(0, |r| r.len));
        }
        owner.push((ri, ch.len_utf16()));
    }
    let style = |i: usize| owner.get(i).and_then(|o| runs.get(o.0)).map(|r| &r.style);
    let mut out: Vec<(usize, (bool, i64), usize)> = Vec::new();
    for (j, &(ri, n)) in owner.iter().enumerate() {
        let k = j.checked_sub(1).and_then(style).map_or(0, |s| kern_units(s.kern));
        // `false` marks character j - 2 manual: written when it is and its own kern (on j - 1)
        // doesn't say so already; harmless (and Photoshop's own form) next to a kern on j.
        let off = match j.checked_sub(2) {
            Some(p) => style(p).is_some_and(|s| manual(s) && (kern_units(s.kern) == 0 || k != 0)),
            None => k != 0,
        };
        let fields = (!off, k);
        match out.last_mut() {
            Some(last) if last.0 == ri && last.1 == fields => last.2 += n,
            _ => out.push((ri, fields, n)),
        }
    }
    out
}

/// Reads EngineData's kerning fields back into the model (see the notes above). `incoming` is
/// per EngineData style run: (UTF-8 length in the engine text, `AutoKerning`, `Kerning`).
fn kerning_from_pairs(text: &str, runs: Vec<TextRun>, incoming: &[(usize, bool, f32)]) -> Vec<TextRun> {
    if text.is_empty() || runs.is_empty() {
        return runs;
    }
    // Byte offset of every character, plus the trailing break.
    let starts: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
    let n = starts.len().saturating_sub(1);
    let mut kern = vec![0.0f32; n];
    let mut off = vec![false; n];
    let (mut si, mut seg_end) = (0usize, incoming.first().map_or(0, |s| s.0));
    for (j, &b) in starts.iter().enumerate() {
        while b >= seg_end && si < incoming.len() {
            si += 1;
            seg_end = seg_end.saturating_add(incoming.get(si).map_or(0, |s| s.0));
        }
        let Some(&(_, auto, k)) = incoming.get(si) else { break };
        if k != 0.0
            && let Some(p) = j.checked_sub(1)
            && let (Some(kp), Some(op)) = (kern.get_mut(p), off.get_mut(p))
        {
            *kp = k;
            *op = true;
        }
        if !auto && let Some(op) = j.checked_sub(2).and_then(|p| off.get_mut(p)) {
            *op = true;
        }
    }
    let (mut ri, mut run_end) = (0usize, runs.first().map_or(0, |r| r.len));
    let mut out: Vec<TextRun> = Vec::new();
    for (c, w) in starts.windows(2).enumerate() {
        let (b, len) = (w[0], w[1] - w[0]);
        while b >= run_end && ri + 1 < runs.len() {
            ri += 1;
            run_end = run_end.saturating_add(runs.get(ri).map_or(0, |r| r.len));
        }
        let Some(r) = runs.get(ri) else { break };
        let k = kern.get(c).copied().unwrap_or(0.0);
        let kerning = if off.get(c).copied().unwrap_or(false) { Kerning::Off } else { Kerning::Metrics };
        match out.last_mut() {
            Some(last) if last.style.kern == k && last.style.kerning == kerning && same_but_kerning(&last.style, &r.style) => last.len += len,
            _ => {
                let mut style = r.style.clone();
                style.kerning = kerning;
                style.kern = k;
                out.push(TextRun { len, style });
            }
        }
    }
    out
}

/// Whether two styles differ at most in their kerning.
fn same_but_kerning(a: &CharStyle, b: &CharStyle) -> bool {
    CharStyle { kerning: b.kerning, kern: b.kern, ..a.clone() } == *b
}

/// PostScript name for a style (the PSD `FontSet` stores these).
pub(crate) fn postscript_for(s: &CharStyle) -> String {
    if let Some(ps) = &s.postscript_name {
        return ps.clone();
    }
    let fam = if s.font_family.is_empty() { crate::fonts::DEFAULT_FAMILY } else { &s.font_family };
    let w = match s.weight {
        0..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    };
    let style = match (w, s.italic) {
        ("Regular", true) => "Italic".to_string(),
        (w, true) => format!("{w}Italic"),
        (w, false) => w.to_string(),
    };
    format!("{}-{style}", fam.replace(' ', ""))
}

/// EngineData style keys for the Type › OpenType toggles and the OpenType feature each one turns
/// on (the standard and discretionary ligature keys map to dedicated [`CharStyle`] fields).
pub const OPENTYPE_KEYS: [(&str, &str); 8] = [
    ("ContextualLigatures", "calt"),
    ("Swash", "swsh"),
    ("OldStyle", "onum"),
    ("StylisticAlternates", "salt"),
    ("Titling", "titl"),
    ("Ornaments", "ornm"),
    ("Ordinals", "ordn"),
    ("Fractions", "frac"),
];

pub(crate) fn style_sheet_data(s: &CharStyle, font: usize, k: f32) -> E {
    let c = &s.color;
    let values = match c.mode {
        ColorMode::Cmyk => vec![real(c.alpha), real(c.c[0]), real(c.c[1]), real(c.c[2]), real(c.c[3])],
        ColorMode::Grayscale => vec![real(c.alpha), real(c.c[0])],
        _ => {
            let [r, g, b] = c.to_rgb();
            vec![real(c.alpha), real(r), real(g), real(b)]
        }
    };
    let color_type = match c.mode {
        ColorMode::Cmyk => 2,
        ColorMode::Grayscale => 0,
        _ => 1,
    };
    let mut opentype: Vec<(String, E)> = OPENTYPE_KEYS
        .iter()
        .filter(|(_, tag)| s.features.iter().any(|f| f.tag == *tag && f.value > 0))
        .map(|(key, _)| (key.to_string(), E::Bool(true)))
        .collect();
    let mut dict = vec![
        ("Font".into(), E::Int(font as i64)),
        ("FontSize".into(), real(s.size_pt / k)),
        ("FauxBold".into(), E::Bool(s.faux_bold)),
        ("FauxItalic".into(), E::Bool(s.faux_italic)),
        ("AutoLeading".into(), E::Bool(s.leading_pt.is_none())),
        ("Leading".into(), real(s.leading_pt.unwrap_or(0.0) / k)),
        ("HorizontalScale".into(), real(s.horizontal_scale)),
        ("VerticalScale".into(), real(s.vertical_scale)),
        ("Tracking".into(), E::Int(s.tracking.round() as i64)),
        ("AutoKerning".into(), E::Bool(s.kerning != Kerning::Off)),
        ("Kerning".into(), E::Int(kern_units(s.kern))),
        ("BaselineShift".into(), real(s.baseline_shift_pt / k)),
        (
            "FontCaps".into(),
            E::Int(match s.caps {
                Caps::Normal => 0,
                Caps::SmallCaps => 1,
                Caps::AllCaps => 2,
            }),
        ),
        ("Underline".into(), E::Bool(s.underline)),
        ("Strikethrough".into(), E::Bool(s.strikethrough)),
        ("Ligatures".into(), E::Bool(s.ligatures)),
        ("DLigatures".into(), E::Bool(s.discretionary_ligatures)),
        ("FillColor".into(), E::Dict(vec![("Type".into(), E::Int(color_type)), ("Values".into(), E::Array(values))])),
    ];
    dict.append(&mut opentype);
    E::Dict(dict)
}

pub(crate) fn paragraph_properties(p: &ParagraphStyle, k: f32) -> E {
    let j = match p.align {
        TextAlign::Left => 0,
        TextAlign::Right => 1,
        TextAlign::Center => 2,
        TextAlign::JustifyLeft => 3,
        TextAlign::JustifyRight => 4,
        TextAlign::JustifyCenter => 5,
        TextAlign::JustifyAll => 6,
    };
    E::Dict(vec![
        ("Justification".into(), E::Int(j)),
        ("FirstLineIndent".into(), real(p.first_line_indent_pt / k)),
        ("StartIndent".into(), real(p.start_indent_pt / k)),
        ("EndIndent".into(), real(p.end_indent_pt / k)),
        ("SpaceBefore".into(), real(p.space_before_pt / k)),
        ("SpaceAfter".into(), real(p.space_after_pt / k)),
        ("AutoHyphenate".into(), E::Bool(p.hyphenate)),
        ("AutoLeading".into(), real(p.auto_leading)),
        ("LeadingType".into(), E::Int(0)),
    ])
}

fn utf16_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

pub(crate) fn font_entry(name: &str) -> E {
    E::Dict(vec![("Name".into(), E::String(name.into())), ("Script".into(), E::Int(0)), ("FontType".into(), E::Int(1)), ("Synthetic".into(), E::Int(0))])
}

/// Updates (or creates) an EngineData tree for `layer`.
pub fn build_engine_data(layer: &TextLayer, template: Option<E>, dpi: f32) -> E {
    let k = 72.0 / if dpi > 0.0 { dpi } else { 72.0 };
    let fresh = || E::Dict(vec![("EngineDict".into(), E::dict()), ("ResourceDict".into(), E::dict())]);
    // The template comes from the PSD: it may not be a dictionary or may lack `EngineDict`.
    let mut e = match template {
        Some(t @ E::Dict(_)) => t,
        _ => fresh(),
    };
    if !matches!(e.get("EngineDict"), Some(E::Dict(_))) {
        e.set("EngineDict", E::dict());
    }
    let runs = layer.char_runs();
    let paras = layer.paragraph_runs();
    let text = layer.text.replace('\n', "\r");
    let ed_text = format!("{text}\r");

    // Fonts: keep the template's font list and append new PostScript names.
    let mut fonts: Vec<String> = e
        .path(&["ResourceDict", "FontSet"])
        .and_then(E::as_array)
        .map(|a| a.iter().map(|f| f.get("Name").and_then(E::as_str).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let mut font_index = |ps: String| match fonts.iter().position(|f| *f == ps) {
        Some(i) => i,
        None => {
            fonts.push(ps);
            fonts.len() - 1
        }
    };
    let mut sruns = Vec::new();
    let mut slens = Vec::new();
    for (ri, pair, n) in pair_runs(&ed_text, &runs) {
        let Some(r) = runs.get(ri) else { continue };
        let fi = font_index(postscript_for(&r.style));
        let mut data = style_sheet_data(&r.style, fi, k);
        data.set("AutoKerning", E::Bool(pair.0));
        data.set("Kerning", E::Int(pair.1));
        sruns.push(E::Dict(vec![("StyleSheet".into(), E::Dict(vec![("StyleSheetData".into(), data)]))]));
        slens.push(E::Int(n as i64));
    }
    let mut pruns = Vec::new();
    let mut plens = Vec::new();
    let mut at = 0usize;
    for (i, p) in paras.iter().enumerate() {
        let piece = &text[at..at + p.len];
        at += p.len;
        let mut n = utf16_len(piece);
        if i + 1 == paras.len() {
            n += 1;
        }
        pruns.push(E::Dict(vec![(
            "ParagraphSheet".into(),
            E::Dict(vec![("DefaultStyleSheet".into(), E::Int(0)), ("Properties".into(), paragraph_properties(&p.style, k))]),
        )]));
        plens.push(E::Int(n));
    }
    let fontset = E::Array(fonts.iter().map(|f| font_entry(f)).collect());

    let Some(dict) = e.get_mut("EngineDict") else {
        return fresh();
    };
    let mut editor = dict.get("Editor").cloned().unwrap_or_else(E::dict);
    editor.set("Text", E::String(ed_text));
    dict.set("Editor", editor);
    let mut pr = dict.get("ParagraphRun").cloned().unwrap_or_else(E::dict);
    pr.set("RunArray", E::Array(pruns));
    pr.set("RunLengthArray", E::Array(plens));
    if pr.get("IsJoinable").is_none() {
        pr.set("IsJoinable", E::Int(1));
    }
    dict.set("ParagraphRun", pr);
    let mut sr = dict.get("StyleRun").cloned().unwrap_or_else(E::dict);
    sr.set("RunArray", E::Array(sruns));
    sr.set("RunLengthArray", E::Array(slens));
    if sr.get("IsJoinable").is_none() {
        sr.set("IsJoinable", E::Int(2));
    }
    dict.set("StyleRun", sr);
    dict.set(
        "AntiAlias",
        E::Int(match layer.antialias {
            AntiAlias::None => 0,
            AntiAlias::Sharp => 1,
            AntiAlias::Crisp => 2,
            AntiAlias::Strong => 3,
            AntiAlias::Smooth => 4,
            // EngineData has no platform modes; the `AntA` descriptor carries the exact value.
            AntiAlias::Windows | AntiAlias::WindowsLcd => 1,
        }),
    );
    if dict.get("UseFractionalGlyphWidths").is_none() {
        dict.set("UseFractionalGlyphWidths", E::Bool(true));
    }
    let (shape_type, box_bounds) = match layer.shape {
        TextShape::Point => (0, None),
        TextShape::Box { x, y, width, height } => (1, Some([x, y, x + width, y + height])),
    };
    let mut photoshop = E::Dict(vec![("ShapeType".into(), E::Int(shape_type))]);
    match box_bounds {
        None => photoshop.set("PointBase", E::Array(vec![real(0.0), real(0.0)])),
        Some(b) => photoshop.set("BoxBounds", E::Array(b.iter().map(|v| real(*v)).collect())),
    }
    photoshop.set(
        "Base",
        E::Dict(vec![
            ("ShapeType".into(), E::Int(shape_type)),
            ("TransformPoint0".into(), E::Array(vec![real(1.0), real(0.0)])),
            ("TransformPoint1".into(), E::Array(vec![real(0.0), real(1.0)])),
            ("TransformPoint2".into(), E::Array(vec![real(0.0), real(0.0)])),
        ]),
    );
    // EngineData writing direction and procession, as Photoshop writes vertical type: 2 and 1
    // (horizontal: 0 and 0). The descriptor's `Ornt` agrees.
    let writing = if layer.orientation == Orientation::Vertical { 2 } else { 0 };
    let child = E::Dict(vec![
        ("ShapeType".into(), E::Int(shape_type)),
        ("Procession".into(), E::Int(i64::from(writing == 2))),
        ("Lines".into(), E::Dict(vec![("WritingDirection".into(), E::Int(writing)), ("Children".into(), E::Array(vec![]))])),
        ("Cookie".into(), E::Dict(vec![("Photoshop".into(), photoshop)])),
    ]);
    dict.set(
        "Rendered",
        E::Dict(vec![
            ("Version".into(), E::Int(1)),
            ("Shapes".into(), E::Dict(vec![("WritingDirection".into(), E::Int(writing)), ("Children".into(), E::Array(vec![child]))])),
        ]),
    );

    for key in ["ResourceDict", "DocumentResources"] {
        if key == "DocumentResources" && e.get(key).is_none() {
            let rd = e.get("ResourceDict").cloned().unwrap_or_else(E::dict);
            e.set(key, rd);
            continue;
        }
        let Some(rd) = e.get_mut(key) else { continue };
        rd.set("FontSet", fontset.clone());
        if rd.get("StyleSheetSet").is_none() {
            let base = runs.first().map(|r| r.style.clone()).unwrap_or_default();
            rd.set(
                "StyleSheetSet",
                E::Array(vec![E::Dict(vec![("Name".into(), E::String("Normal RGB".into())), ("StyleSheetData".into(), style_sheet_data(&base, 0, k))])]),
            );
        }
        if rd.get("ParagraphSheetSet").is_none() {
            rd.set(
                "ParagraphSheetSet",
                E::Array(vec![E::Dict(vec![
                    ("Name".into(), E::String("Normal RGB".into())),
                    ("DefaultStyleSheet".into(), E::Int(0)),
                    ("Properties".into(), paragraph_properties(&ParagraphStyle::default(), k)),
                ])]),
            );
        }
        for (k2, v) in [("TheNormalStyleSheet", 0), ("TheNormalParagraphSheet", 0)] {
            if rd.get(k2).is_none() {
                rd.set(k2, E::Int(v));
            }
        }
    }
    e
}

/// Builds the `TySh` block for `layer`, patching its current `psd_raw` when present.
/// `ink` is the layer's rendered bounds in text space (for `bounds`/`boundingBox`).
pub fn build_tysh(layer: &TextLayer, dpi: f32, ink: Option<[f32; 4]>) -> Vec<u8> {
    let old = layer.psd_raw.as_deref().and_then(|d| parse_tysh(d));
    let template = old.as_ref().and_then(|t| engine_data(&t.text));
    let e = build_engine_data(layer, template, dpi);
    let mut text = old.as_ref().map(|t| t.text.clone()).unwrap_or_else(|| Descriptor::new("TxLr"));
    let set = |d: &mut Descriptor, key: &str, v: D| match d.items.iter_mut().find(|(k, _)| k.is(key)) {
        Some(e) => e.1 = v,
        None => d.items.push((Id::new(key), v)),
    };
    set(&mut text, "Txt ", D::Text(UnicodeString::new_nul(&layer.text.replace('\n', "\r"))));
    if text.get("textGridding").is_none() {
        set(&mut text, "textGridding", enumv("textGridding", "None"));
    }
    set(&mut text, "Ornt", enumv("Ornt", if layer.orientation == Orientation::Vertical { "Vrtc" } else { "Hrzn" }));
    set(
        &mut text,
        "AntA",
        enumv(
            "Annt",
            match layer.antialias {
                AntiAlias::None => "Anno",
                AntiAlias::Sharp => "antiAliasSharp",
                AntiAlias::Crisp => "AnCr",
                AntiAlias::Strong => "AnSt",
                AntiAlias::Smooth => "AnSm",
                AntiAlias::Windows => "antiAliasPlatformGray",
                AntiAlias::WindowsLcd => "antiAliasPlatformLCD",
            },
        ),
    );
    let [l, t, r, b] = match (layer.shape, ink) {
        (TextShape::Box { x, y, width, height }, _) => [x, y, x + width, y + height],
        (_, Some(i)) => i,
        _ => [0.0; 4],
    };
    let rect = |cls: &str| {
        D::Descriptor(
            Descriptor::new(cls)
                .with("Left", D::UnitFloat { unit: *b"#Pnt", value: f64::from(l) })
                .with("Top ", D::UnitFloat { unit: *b"#Pnt", value: f64::from(t) })
                .with("Rght", D::UnitFloat { unit: *b"#Pnt", value: f64::from(r) })
                .with("Btom", D::UnitFloat { unit: *b"#Pnt", value: f64::from(b) }),
        )
    };
    set(&mut text, "bounds", rect("bounds"));
    set(&mut text, "boundingBox", rect("boundingBox"));
    if text.get("TextIndex").is_none() {
        set(&mut text, "TextIndex", D::Integer(0));
    }
    set(&mut text, "EngineData", D::RawData(ed::write(&e)));
    let warp = match (&layer.warp, old.as_ref().and_then(|o| o.warp.clone())) {
        (Some(w), _) => Some(
            Descriptor::new("warp")
                .with("warpStyle", enumv("warpStyle", &w.style))
                .with("warpValue", D::Double(f64::from(w.value)))
                .with("warpPerspective", D::Double(f64::from(w.horizontal_distortion)))
                .with("warpPerspectiveOther", D::Double(f64::from(w.vertical_distortion)))
                .with("warpRotate", enumv("Ornt", if w.horizontal { "Hrzn" } else { "Vrtc" })),
        ),
        (None, Some(old)) if enum_value(&old, "warpStyle").as_deref() == Some("warpNone") => Some(old),
        _ => None,
    };
    let bounds = [l.floor() as i32, t.floor() as i32, r.ceil() as i32, b.ceil() as i32];
    write_tysh(&TySh { transform: layer.transform, text, warp, bounds })
}

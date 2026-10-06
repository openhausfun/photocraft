//! Artboards: Layer › New › Artboard (from scratch, a group or the selected layers), artboard
//! properties (board rect, background, preset), View › Clear Selected Artboard Guides, and
//! File › Export › Artboards to Files / to PDF.
//!
//! An artboard is a top-level group with [`photocraft_doc::Artboard`] set; the compositor clips
//! its children to the board and paints its background. Moving the group (Move tool,
//! `layer.translate`) moves the board with its contents. The canvas grows to the right and
//! bottom to fit new boards, like Photoshop's auto-expanding artboard canvas.

use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Artboard, ArtboardBackground, Document, Layer, LayerContent, LayerId, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, color_param, int};
use crate::file_cmds::{encode, f64_param, join, native_doc, sanitize, save_doc, stem, str_param, write_file};
use crate::{EngineError, Result, Session};

/// Artboard size presets (name, width, height) in pixels, Photoshop's common devices and pages.
pub const PRESETS: &[(&str, u32, u32)] = &[
    ("iPhone 14", 390, 844),
    ("iPhone 14 Pro Max", 430, 932),
    ("Android 1080p", 360, 640),
    ("iPad Pro 12.9", 1024, 1366),
    ("Web 1280", 1280, 800),
    ("Web 1366", 1366, 768),
    ("Web 1920", 1920, 1080),
    ("Instagram Post", 1080, 1080),
    ("Letter", 612, 792),
    ("A4", 595, 842),
];

/// Gap Photoshop leaves between a new artboard and the rightmost existing one.
const GAP: i32 = 100;

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn has_artboards(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if d.doc.has_artboards() { Ok(()) } else { Err("the document has no artboards".into()) }
}

fn export_artboards(s: &Session) -> std::result::Result<(), String> {
    native_doc(s)?;
    has_artboards(s)
}

/// The active layer's artboard (the layer itself or its top-level ancestor).
fn active_artboard(s: &Session) -> Option<LayerId> {
    let d = s.active()?;
    d.doc.artboard_of(d.active_layer?)
}

fn has_active_artboard(s: &Session) -> std::result::Result<(), String> {
    active_artboard(s).map(|_| ()).ok_or_else(|| "select an artboard (or a layer inside one)".into())
}

fn has_plain_group(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    let l = d.doc.layer(id).ok_or("no active layer")?;
    if !l.is_group() || l.artboard().is_some() {
        return Err("select a layer group".into());
    }
    if !d.doc.layers.iter().any(|t| t.id == id) {
        return Err("artboards must be top-level: select a group that is not nested".into());
    }
    Ok(())
}

fn has_free_layers(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let sel = d.selected_layers();
    if sel.is_empty() {
        return Err("select one or more layers".into());
    }
    if sel.iter().any(|id| d.doc.artboard_of(*id).is_some()) {
        return Err("the selection is already in an artboard".into());
    }
    Ok(())
}

fn preset_size(name: &str) -> Option<(u32, u32)> {
    PRESETS.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name)).map(|(_, w, h)| (*w, *h))
}

fn background_param(p: &Value, cmd: &str) -> Result<Option<ArtboardBackground>> {
    let custom = p.get("color").map(|_| {
        let c = color_param(p, "color", [1.0; 4]);
        ArtboardBackground::Custom(Color::rgba(c[0], c[1], c[2], c[3]))
    });
    Ok(match p.get("background").and_then(Value::as_str) {
        None => custom,
        Some("white") => Some(ArtboardBackground::White),
        Some("black") => Some(ArtboardBackground::Black),
        Some("transparent") => Some(ArtboardBackground::Transparent),
        Some("custom" | "other") => Some(custom.unwrap_or(ArtboardBackground::White)),
        Some(o) => return Err(bad(cmd, format!("unknown background `{o}` (white|black|transparent|custom)"))),
    })
}

/// `rect` as `[x, y, w, h]` or `x`/`y`/`width`/`height` keys over `base`.
fn rect_param(p: &Value, base: Rect, cmd: &str) -> Result<Rect> {
    if let Some(Value::Array(a)) = p.get("rect") {
        let v: Vec<i32> = a.iter().filter_map(|x| x.as_f64().map(|f| f.round() as i32)).collect();
        if v.len() != 4 || v[2] <= 0 || v[3] <= 0 {
            return Err(bad(cmd, "\"rect\" must be [x, y, width, height] with a positive size"));
        }
        return Ok(Rect::from_xywh(v[0], v[1], v[2] as u32, v[3] as u32));
    }
    let x = int(p, "x").map_or(base.x0, |v| v as i32);
    let y = int(p, "y").map_or(base.y0, |v| v as i32);
    let w = int(p, "width").unwrap_or(i64::from(base.width()));
    let h = int(p, "height").unwrap_or(i64::from(base.height()));
    if w <= 0 || h <= 0 {
        return Err(bad(cmd, "the artboard size must be positive"));
    }
    Ok(Rect::from_xywh(x, y, w as u32, h as u32))
}

/// Grow the canvas (right/bottom) so every artboard fits.
fn fit_canvas(doc: &mut Document) {
    let (mut w, mut h) = (doc.size.width as i64, doc.size.height as i64);
    for (_, _, a) in doc.artboards() {
        w = w.max(i64::from(a.rect.x1));
        h = h.max(i64::from(a.rect.y1));
    }
    doc.size = Size::new(w.clamp(1, 300_000) as u32, h.clamp(1, 300_000) as u32);
}

fn next_artboard_name(doc: &Document) -> String {
    let names: Vec<&str> = doc.artboards().into_iter().map(|b| b.1).collect();
    // `names.len() + 1` candidates always include a free one.
    (1..=names.len() + 1).map(|n| format!("Artboard {n}")).find(|n| !names.contains(&n.as_str())).unwrap_or_else(|| "Artboard".into())
}

fn set_artboard(l: &mut Layer, a: Artboard) {
    if let LayerContent::Group(g) = &mut l.content {
        g.artboard = Some(a);
        g.expanded = true;
    }
    // Artboards composite like pass-through groups.
    l.clipped = false;
}

fn new_artboard(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "layer.new.artboard";
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let preset = p.get("preset").and_then(Value::as_str).unwrap_or("").to_string();
    let (pw, ph) = match preset.as_str() {
        "" => (d.doc.size.width, d.doc.size.height),
        name => preset_size(name).ok_or_else(|| bad(cmd, format!("unknown preset `{name}`")))?,
    };
    // Default placement: right of the rightmost artboard, else the canvas origin.
    let right = d.doc.artboards().iter().map(|b| b.2.rect.x1).max();
    let top = d.doc.artboards().iter().map(|b| b.2.rect.y0).min().unwrap_or(0);
    let base = match right {
        Some(x) => Rect::from_xywh(x + GAP, top, pw, ph),
        None => Rect::from_xywh(0, 0, pw, ph),
    };
    // A preset decides the size (dialogs send the canvas size alongside it).
    let rect = if preset.is_empty() || p.get("rect").is_some() {
        rect_param(p, base, cmd)?
    } else {
        rect_param(&json!({"x": p.get("x"), "y": p.get("y")}), base, cmd)?
    };
    let background = background_param(p, cmd)?.unwrap_or_default();
    let name = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).map(str::to_string);
    let id = s.edit("New Artboard", |doc, active| {
        let mut g = Layer::group(name.unwrap_or_else(|| next_artboard_name(doc)), vec![]);
        set_artboard(&mut g, Artboard { rect, background, preset });
        let id = doc.insert_above(None, g);
        fit_canvas(doc);
        *active = Some(id);
        Ok(id)
    })?;
    crate::layer_multi_cmds::set_selection(s, vec![id], Some(id), Some(id))?;
    Ok(json!({"layer": id.0, "rect": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

fn artboard_from_group(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = p.get("layer").and_then(Value::as_u64).map(LayerId).or(d.active_layer).ok_or(EngineError::Other("no active layer".into()))?;
    let canvas = d.doc.bounds();
    let background = background_param(p, "layer.new.artboardFromGroup")?.unwrap_or_default();
    let rect = s.edit("Artboard from Group", |doc, active| {
        if !doc.layers.iter().any(|t| t.id == id && t.is_group() && t.artboard().is_none()) {
            return Err(EngineError::Other("artboards are made from a top-level layer group".into()));
        }
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let rect = crate::layer_multi_cmds::layer_bounds(l).unwrap_or(canvas);
        set_artboard(l, Artboard { rect, background, preset: String::new() });
        fit_canvas(doc);
        *active = Some(id);
        Ok(rect)
    })?;
    Ok(json!({"layer": id.0, "rect": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

fn artboard_from_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = crate::layer_multi_cmds::selected(s);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let background = background_param(p, "layer.new.artboardFromLayers")?.unwrap_or_default();
    let (gid, rect) = s.edit("Artboard from Layers", |doc, active| {
        let ids = crate::layer_multi_cmds::top_level(doc, &sel);
        if ids.is_empty() || ids.iter().any(|id| doc.artboard_of(*id).is_some()) {
            return Err(EngineError::Other("select layers that are not in an artboard".into()));
        }
        let mut children = Vec::with_capacity(ids.len());
        for id in &ids {
            children.push(doc.remove(*id).ok_or(EngineError::NoLayer(*id))?);
        }
        let rect = children.iter().filter_map(crate::layer_multi_cmds::layer_bounds).reduce(|a, b| a.union(&b)).unwrap_or(canvas);
        let mut g = Layer::group(name.unwrap_or_else(|| next_artboard_name(doc)), children);
        set_artboard(&mut g, Artboard { rect, background, preset: String::new() });
        let gid = doc.insert_above(None, g);
        fit_canvas(doc);
        *active = Some(gid);
        Ok((gid, rect))
    })?;
    crate::layer_multi_cmds::set_selection(s, vec![gid], Some(gid), Some(gid))?;
    Ok(json!({"layer": gid.0, "rect": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

/// Properties panel / Artboard tool: board position and size, background, preset, name.
/// Changing X/Y moves the board with its contents; changing W/H resizes it from the top left.
fn set_props(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "layer.artboard.set";
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = match p.get("layer").and_then(Value::as_u64) {
        Some(v) => LayerId(v),
        None => active_artboard(s).ok_or_else(|| bad(cmd, "no artboard selected"))?,
    };
    let old = d.doc.layer(id).and_then(Layer::artboard).cloned().ok_or_else(|| bad(cmd, "the layer is not an artboard"))?;
    let preset = p.get("preset").and_then(Value::as_str).map(str::to_string);
    let mut base = old.rect;
    if let Some(name) = preset.as_deref().filter(|n| !n.is_empty()) {
        let (w, h) = preset_size(name).ok_or_else(|| bad(cmd, format!("unknown preset `{name}`")))?;
        base = Rect::from_xywh(base.x0, base.y0, w, h);
    }
    let rect = rect_param(p, base, cmd)?;
    let background = background_param(p, cmd)?;
    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let move_contents = p.get("moveContents").and_then(Value::as_bool).unwrap_or(true);
    s.edit("Edit Artboard", |doc, _| {
        let (dx, dy) = (rect.x0 - old.rect.x0, rect.y0 - old.rect.y0);
        if move_contents && (dx, dy) != (0, 0) {
            let snapshot = doc.clone();
            let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            crate::commands::translate_layer(&snapshot, l, dx, dy);
            crate::vector_cmds::translate_vectors(&snapshot, l, f64::from(dx), f64::from(dy));
        }
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if let Some(n) = name {
            l.name = n;
        }
        let a = l.artboard_mut().ok_or(EngineError::NoLayer(id))?;
        a.rect = rect;
        if let Some(b) = background {
            a.background = b;
        }
        match preset {
            Some(pr) => a.preset = pr,
            // A hand-edited size no longer matches the preset.
            None if rect.width() != old.rect.width() || rect.height() != old.rect.height() => a.preset.clear(),
            None => {}
        }
        fit_canvas(doc);
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "rect": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

fn clear_artboard_guides(s: &mut Session) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = active_artboard(s).ok_or(EngineError::Other("no artboard selected".into()))?;
    let r = d.doc.layer(id).and_then(Layer::artboard).map(|a| a.rect).ok_or(EngineError::NoLayer(id))?;
    let inside_x = |x: &f32| f64::from(*x) > f64::from(r.x0) && f64::from(*x) < f64::from(r.x1);
    let inside_y = |y: &f32| f64::from(*y) > f64::from(r.y0) && f64::from(*y) < f64::from(r.y1);
    let n = d.doc.guides.vertical.iter().filter(|x| inside_x(x)).count() + d.doc.guides.horizontal.iter().filter(|y| inside_y(y)).count();
    if n == 0 {
        return Ok(json!({"cleared": 0}));
    }
    s.edit("Clear Selected Artboard Guides", |doc, _| {
        doc.guides.vertical.retain(|x| !inside_x(x));
        doc.guides.horizontal.retain(|y| !inside_y(y));
        Ok(())
    })?;
    Ok(json!({"cleared": n}))
}

/// One artboard as its own document: the board's size, its group moved to the origin.
pub fn artboard_document(doc: &Document, id: LayerId) -> Option<Document> {
    let l = doc.layer(id)?;
    let r = l.artboard()?.rect;
    let mut group = l.clone();
    group.visible = true;
    crate::commands::translate_layer(doc, &mut group, -r.x0, -r.y0);
    crate::vector_cmds::translate_vectors(doc, &mut group, f64::from(-r.x0), f64::from(-r.y0));
    let mut one = doc.clone();
    one.name = l.name.clone();
    one.size = Size::new(r.width(), r.height());
    one.layers = vec![group];
    one.channels.clear();
    one.selection = None;
    one.quick_mask = None;
    one.layer_comps.clear();
    one.last_applied_comp = None;
    one.last_document_state = None;
    let shift = |v: &[f32], o: i32, len: u32| v.iter().map(|g| g - o as f32).filter(|g| *g > 0.0 && *g < len as f32).collect::<Vec<_>>();
    one.guides.vertical = shift(&doc.guides.vertical, r.x0, r.width());
    one.guides.horizontal = shift(&doc.guides.horizontal, r.y0, r.height());
    Some(one)
}

/// The artboards an export covers: `"artboards": [ids]`, else all, top of the Layers panel first.
fn chosen_artboards(doc: &Document, p: &Value) -> Vec<LayerId> {
    let all: Vec<LayerId> = doc.artboards().into_iter().rev().map(|b| b.0).collect();
    match p.get("artboards") {
        Some(Value::Array(a)) => {
            let want: Vec<u64> = a.iter().filter_map(Value::as_u64).collect();
            all.into_iter().filter(|id| want.contains(&id.0)).collect()
        }
        _ => all,
    }
}

/// File › Export › Artboards to Files: `<prefix>_<artboard name>.<format>` per board.
fn artboards_to_files(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.artboardsToFiles";
    let dir = str_param(p, "dir", cmd).or_else(|_| str_param(p, "output", cmd))?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or("png").trim_start_matches('.').to_ascii_lowercase();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let prefix = p.get("prefix").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| stem(&d.doc.name));
    let doc = d.doc.clone();
    let mut files = Vec::new();
    for id in chosen_artboards(&doc, p) {
        let Some(one) = artboard_document(&doc, id) else { continue };
        let name = if prefix.is_empty() { sanitize(&one.name) } else { format!("{}_{}", sanitize(&prefix), sanitize(&one.name)) };
        let path = join(&dir, &format!("{name}.{format}"));
        save_doc(&one, &path, f64_param(p, "quality"))?;
        files.push(path);
    }
    if files.is_empty() {
        return Err(bad(cmd, "no matching artboards"));
    }
    Ok(json!({"files": files}))
}

/// A minimal PDF with one page per JPEG image: (width px, height px, dpi, JPEG bytes).
/// Pages are sized so the image prints at its resolution.
pub fn raster_pdf(pages: &[(u32, u32, f32, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    // Objects: 1 catalog, 2 page tree, then per page: page, contents, image.
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).as_bytes());
    for (i, (w, h, dpi, jpeg)) in pages.iter().enumerate() {
        let k = 72.0 / f64::from(dpi.max(1.0));
        let (pw, ph) = (f64::from(*w) * k, f64::from(*h) * k);
        let (contents, image) = (4 + i * 3, 5 + i * 3);
        obj(
            &mut out,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.3} {ph:.3}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        let stream = format!("q {pw:.3} 0 0 {ph:.3} 0 0 cm /Im0 Do Q");
        obj(&mut out, format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()).as_bytes());
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        img.extend_from_slice(jpeg);
        img.extend_from_slice(b"\nendstream");
        obj(&mut out, &img);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    out
}

/// File › Export › Artboards to PDF: one page per board (flattened over white, JPEG-encoded).
fn artboards_to_pdf(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.artboardsToPdf";
    let path = str_param(p, "path", cmd)?.to_string();
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let quality = f64_param(p, "quality").or(Some(10.0));
    let mut pages = Vec::new();
    for id in chosen_artboards(&doc, p) {
        let Some(one) = artboard_document(&doc, id) else { continue };
        let buf = photocraft_compose::flatten(&one).over_background([1.0, 1.0, 1.0]);
        let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
        let mut flat = Document::new(&one.name, one.size, ColorMode::Rgb, SampleType::U8);
        let mut surf = Surface::new(fmt);
        let vals: Vec<f32> = buf.px.iter().flat_map(|px| photocraft_raster::from_rgba(&fmt, *px)).collect();
        surf.write_region(one.bounds(), &vals);
        flat.layers.push(Layer::new("Background", LayerContent::Raster(surf)));
        let (jpeg, _) = encode(&flat, "page.jpg", quality)?;
        pages.push((one.size.width, one.size.height, doc.resolution_dpi, jpeg));
    }
    if pages.is_empty() {
        return Err(bad(cmd, "no matching artboards"));
    }
    write_file(&path, &raster_pdf(&pages))?;
    Ok(json!({"path": path, "pages": pages.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: None, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    vec![
        spec!(
            "layer.new.artboard",
            "Artboard…",
            &["Layer", "New"],
            r##"{"rect":[x,y,w,h]? | "x","y","width","height"? (default: canvas size, right of the last board),"preset":"iPhone 14|Web 1920|A4|…"?,"name":str?,"background":"white|black|transparent|custom"="white","color":[r,g,b]|"#rrggbb"? (custom)} → {layer, rect}"##,
            has_doc,
            new_artboard
        ),
        spec!(
            "layer.new.artboardFromGroup",
            "Artboard from Group…",
            &["Layer", "New"],
            r##"{"layer":id? (a top-level group; default active),"background":…?} → {layer, rect}"##,
            has_plain_group,
            artboard_from_group
        ),
        spec!(
            "layer.new.artboardFromLayers",
            "Artboard from Layers…",
            &["Layer", "New"],
            r##"{"name":str?,"background":…?} (the selected layers) → {layer, rect}"##,
            has_free_layers,
            artboard_from_layers
        ),
        spec!(
            "layer.artboard.set",
            "Edit Artboard",
            &[],
            r##"{"layer":id? (default: the active artboard),"x","y","width","height"?|"rect":[x,y,w,h]?,"preset":str?,"background":"white|black|transparent|custom"?,"color":…?,"name":str?,"moveContents":bool=true} → {layer, rect}"##,
            has_active_artboard,
            set_props
        ),
        spec!(
            "view.clearSelectedArtboardGuides",
            "Clear Selected Artboard Guides",
            &["View"],
            "{} (guides inside the active artboard)",
            has_active_artboard,
            |s, _| clear_artboard_guides(s)
        ),
        spec!(
            "file.export.artboardsToFiles",
            "Artboards to Files…",
            &["File", "Export"],
            r##"{"dir":folder,"format":"png|jpg|psd|tiff|…"="png","prefix":str=document name ("" = none),"artboards":[id]? (default all),"quality":0..12?} → {files}"##,
            export_artboards,
            artboards_to_files
        ),
        spec!(
            "file.export.artboardsToPdf",
            "Artboards to PDF…",
            &["File", "Export"],
            r##"{"path":str (.pdf),"artboards":[id]?,"quality":0..12=10} → {path, pages} (one raster page per board)"##,
            export_artboards,
            artboards_to_pdf
        ),
    ]
}

#[cfg(test)]
#[path = "artboard_cmds/tests.rs"]
mod tests;

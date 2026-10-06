//! Image menu: Image Size, Canvas Size, Crop, Trim, Mode (colour model and
//! bit depth) conversions, Duplicate.

use photocraft_algo::resample::{Resample, crop_surface, resize_surface, translate_surface};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Effect, Effects, FxPaint, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

/// Applies `f` to every pixel surface of a layer tree: content (raster or
/// cached text/shape/smart pixels), fill caches, and (if `masks`) masks.
pub(crate) fn for_each_surface(layers: &mut [Layer], masks: bool, f: &mut dyn FnMut(&mut Surface, bool)) {
    for l in layers {
        if masks && let Some(m) = &mut l.mask {
            f(&mut m.surface, true);
        }
        if let Some(fc) = &mut l.fill_cache {
            f(&mut fc.surface, false);
        }
        match &mut l.content {
            LayerContent::Raster(s) => f(s, false),
            LayerContent::Text(t) => {
                if let Some(c) = &mut t.cache {
                    f(c, false)
                }
            }
            LayerContent::Shape(sh) => {
                if let Some(c) = &mut sh.cache {
                    f(c, false)
                }
            }
            LayerContent::Smart(sm) => {
                if let Some(c) = &mut sm.cache {
                    f(c, false)
                }
            }
            LayerContent::Group(g) => for_each_surface(&mut g.children, masks, f),
            _ => {}
        }
    }
}

fn for_each_layer(layers: &mut [Layer], f: &mut dyn FnMut(&mut Layer)) {
    for l in layers {
        f(l);
        if let Some(ch) = l.children_mut() {
            for_each_layer(ch, f);
        }
    }
}

pub(crate) fn scale_effects(fx: &mut Effects, k: f32) {
    for e in &mut fx.items {
        match e {
            Effect::DropShadow(s) | Effect::InnerShadow(s) => {
                s.distance *= k;
                s.size *= k;
            }
            Effect::OuterGlow(g) | Effect::InnerGlow(g) => g.size *= k,
            Effect::Stroke(s) => s.size *= k,
            Effect::Satin(s) => {
                s.distance *= k;
                s.size *= k;
            }
            Effect::BevelEmboss(b) => {
                b.size *= k;
                b.soften *= k;
            }
            Effect::PatternOverlay { scale, .. } => *scale *= k,
            Effect::GradientOverlay { .. } | Effect::ColorOverlay { .. } => {}
        }
        if let Effect::Stroke(s) = e
            && let FxPaint::Pattern { scale, .. } = &mut s.paint
        {
            *scale *= k;
        }
    }
    // Effects changed: the preserved PSD block no longer applies.
    if k != 1.0 {
        fx.psd_raw = None;
    }
}

fn parse_resample(s: &str) -> Option<Resample> {
    Some(match s {
        "none" => return None,
        "nearest" | "nearestNeighbor" => Resample::Nearest,
        "bilinear" => Resample::Bilinear,
        "lanczos" => Resample::Lanczos,
        "preserveDetails" => Resample::PreserveDetails,
        _ => Resample::Bicubic,
    })
}

/// Image → Image Size.
fn image_size(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (ow, oh) = (d.doc.size.width, d.doc.size.height);
    let resample = parse_resample(p.get("resample").and_then(Value::as_str).unwrap_or("bicubic"));
    let pw = p.get("width").and_then(Value::as_f64);
    let ph = p.get("height").and_then(Value::as_f64);
    // Missing dimension keeps the aspect ratio.
    let (nw, nh) = match (pw, ph) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * oh as f64 / ow.max(1) as f64),
        (None, Some(h)) => (h * ow as f64 / oh.max(1) as f64, h),
        (None, None) => (ow as f64, oh as f64),
    };
    let (nw, nh) = (nw.round().clamp(1.0, 300_000.0) as u32, nh.round().clamp(1.0, 300_000.0) as u32);
    let dpi = p.get("resolution").and_then(Value::as_f64).map(|v| v as f32);
    s.edit("Image Size", |doc, _| {
        if let Some(r) = dpi {
            doc.resolution_dpi = r.clamp(1.0, 10_000.0);
        }
        let Some(filter) = resample else { return Ok(()) };
        let (sx, sy) = (nw as f64 / ow.max(1) as f64, nh as f64 / oh.max(1) as f64);
        if (sx - 1.0).abs() < 1e-12 && (sy - 1.0).abs() < 1e-12 {
            return Ok(());
        }
        for_each_surface(&mut doc.layers, true, &mut |surf, is_mask| {
            *surf = resize_surface(surf, sx, sy, if is_mask { Resample::Bilinear } else { filter });
        });
        let k = ((sx + sy) / 2.0) as f32;
        for_each_layer(&mut doc.layers, &mut |l| scale_effects(&mut l.effects, k));
        for ch in doc.channels.iter_mut().chain(doc.quick_mask.as_mut()) {
            ch.surface = resize_surface(&ch.surface, sx, sy, Resample::Bilinear);
        }
        if let Some(sel) = &doc.selection {
            doc.selection = Some(resize_surface(sel, sx, sy, Resample::Bilinear));
        }
        // Vector geometry, guides and marks scale with the pixels; vectors re-render sharp.
        crate::canvas_geom::transform_geometry(doc, &photocraft_geom::Affine { m: [sx, 0.0, 0.0, sy, 0.0, 0.0] });
        doc.size = Size::new(nw, nh);
        crate::canvas_geom::refresh(doc, crate::canvas_geom::Refresh::All);
        Ok(())
    })?;
    Ok(json!({ "width": nw, "height": nh }))
}

/// Moves every surface, channel, selection and guide by `(dx, dy)`.
fn translate_doc(doc: &mut Document, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    for_each_surface(&mut doc.layers, true, &mut |surf, _| *surf = translate_surface(surf, dx, dy));
    for ch in doc.channels.iter_mut().chain(doc.quick_mask.as_mut()) {
        ch.surface = translate_surface(&ch.surface, dx, dy);
    }
    if let Some(sel) = &doc.selection {
        doc.selection = Some(translate_surface(sel, dx, dy));
    }
    // Type, shapes, smart objects, vector masks, paths, guides, slices, notes… (caches above
    // are already translated exactly, so nothing needs re-rendering for the move itself).
    crate::canvas_geom::transform_geometry(doc, &photocraft_geom::Affine::translate(f64::from(dx), f64::from(dy)));
}

/// Crops the document to `r` (in current document coordinates).
fn crop_doc(doc: &mut Document, r: Rect, delete_pixels: bool) {
    if delete_pixels {
        for_each_surface(&mut doc.layers, false, &mut |surf, _| *surf = crop_surface(surf, r));
    }
    translate_doc(doc, -r.x0, -r.y0);
    doc.size = Size::new(r.width(), r.height());
    crate::canvas_geom::refresh(doc, crate::canvas_geom::Refresh::Shapes);
}

fn anchor_factors(a: &str) -> (f64, f64) {
    let x = if a.contains("Left") || a == "left" {
        0.0
    } else if a.contains("Right") || a == "right" {
        1.0
    } else {
        0.5
    };
    let y = if a.starts_with("top") {
        0.0
    } else if a.starts_with("bottom") {
        1.0
    } else {
        0.5
    };
    (x, y)
}

fn extension_color(s: &Session, p: &Value) -> [f32; 4] {
    match p.get("extensionColor").and_then(Value::as_str).unwrap_or("background") {
        "foreground" => s.tools.foreground,
        "white" => [1.0; 4],
        "black" => [0.0, 0.0, 0.0, 1.0],
        "transparent" => [0.0; 4],
        hex if hex.starts_with('#') => {
            let h = hex.trim_start_matches('#');
            let c = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map_or(0.0, |v| f32::from(v) / 255.0);
            [c(0), c(2), c(4), 1.0]
        }
        _ => s.tools.background,
    }
}

/// Image → Canvas Size.
fn canvas_size(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (ow, oh) = (d.doc.size.width as f64, d.doc.size.height as f64);
    let relative = p.get("relative").and_then(Value::as_bool).unwrap_or(false);
    let w = p.get("width").and_then(Value::as_f64).unwrap_or(if relative { 0.0 } else { ow });
    let h = p.get("height").and_then(Value::as_f64).unwrap_or(if relative { 0.0 } else { oh });
    let (nw, nh) = if relative { (ow + w, oh + h) } else { (w, h) };
    let (nw, nh) = (nw.round().clamp(1.0, 300_000.0) as u32, nh.round().clamp(1.0, 300_000.0) as u32);
    let (ax, ay) = anchor_factors(p.get("anchor").and_then(Value::as_str).unwrap_or("center"));
    let dx = ((nw as f64 - ow) * ax).round() as i32;
    let dy = ((nh as f64 - oh) * ay).round() as i32;
    let ext = extension_color(s, p);
    s.edit("Canvas Size", |doc, _| {
        translate_doc(doc, dx, dy);
        doc.size = Size::new(nw, nh);
        crate::canvas_geom::refresh(doc, crate::canvas_geom::Refresh::Shapes);
        let canvas = doc.bounds();
        let old = Rect::from_xywh(dx, dy, ow as u32, oh as u32);
        // The locked Background layer is extended with the extension colour.
        if ext[3] > 0.0
            && let Some(bg) = doc.layers.first_mut()
            && bg.name == "Background"
            && bg.locks.transparency
            && let LayerContent::Raster(surf) = &mut bg.content
        {
            let fmt = surf.format();
            let px = photocraft_raster::from_rgba(&fmt, ext);
            let keep = surf.content_bounds().intersect(&old);
            let saved = surf.to_interleaved(keep);
            surf.fill_rect(canvas, &px);
            if !keep.is_empty() {
                surf.write_interleaved(keep, &saved);
            }
        }
        Ok(())
    })?;
    Ok(json!({ "width": nw, "height": nh, "offset": [dx, dy] }))
}

/// Image → Crop (to the selection bounds).
fn crop(s: &mut Session, p: &Value) -> Result<Value> {
    let delete = p.get("deleteCroppedPixels").and_then(Value::as_bool).unwrap_or(true);
    let explicit = match (crate::commands::int(p, "x"), crate::commands::int(p, "y"), crate::commands::int(p, "width"), crate::commands::int(p, "height")) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 0 && h > 0 => Some(Rect::new(x as i32, y as i32, (x + w) as i32, (y + h) as i32)),
        _ => None,
    };
    // An explicit rectangle (the Crop tool) may extend past the canvas; a selection crop is clamped.
    let r = explicit
        .or_else(|| s.active().and_then(|d| d.doc.selection.as_ref().map(|sel| sel.content_bounds().intersect(&d.doc.bounds()))))
        .unwrap_or(Rect::EMPTY);
    if r.is_empty() {
        return Err(EngineError::Other("nothing to crop: pass x/y/width/height or make a selection".into()));
    }
    s.edit("Crop", |doc, _| {
        crop_doc(doc, r, delete);
        doc.selection = None;
        Ok(())
    })?;
    Ok(json!({ "x": r.x0, "y": r.y0, "width": r.width(), "height": r.height() }))
}

/// Image → Trim.
fn trim(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let buf = photocraft_compose::flatten(&d.doc);
    let based = p.get("basedOn").and_then(Value::as_str).unwrap_or("transparent");
    let w = canvas.width() as usize;
    let reference = match based {
        "topLeft" => Some(buf.px[0]),
        "bottomRight" => buf.px.last().copied(),
        _ => None,
    };
    let differs = |q: [f32; 4]| match reference {
        None => q[3] > 0.0,
        Some(r) => (0..4).any(|c| (q[c] * q[3] - r[c] * r[3]).abs() > 0.5 / 255.0 || (c == 3 && (q[3] - r[3]).abs() > 0.5 / 255.0)),
    };
    let mut b = Rect::EMPTY;
    for (i, q) in buf.px.iter().enumerate() {
        if differs(*q) {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            b = if b.is_empty() { Rect::new(x, y, x + 1, y + 1) } else { b.union(&Rect::new(x, y, x + 1, y + 1)) };
        }
    }
    if b.is_empty() {
        return Err(EngineError::Other("nothing to trim to".into()));
    }
    let side = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(true);
    let r = Rect::new(
        if side("left") { b.x0 } else { canvas.x0 },
        if side("top") { b.y0 } else { canvas.y0 },
        if side("right") { b.x1 } else { canvas.x1 },
        if side("bottom") { b.y1 } else { canvas.y1 },
    );
    s.edit("Trim", |doc, _| {
        crop_doc(doc, r, true);
        Ok(())
    })?;
    Ok(json!({ "x": r.x0, "y": r.y0, "width": r.width(), "height": r.height() }))
}

/// Image → Mode → colour model, through the CMS (see [`crate::color_cmds::convert_mode`]).
fn convert_mode(s: &mut Session, mode: ColorMode, p: &Value) -> Result<Value> {
    crate::color_cmds::convert_mode(s, mode, p)
}

/// Image → Mode → 8/16/32 Bits/Channel.
fn convert_depth(s: &mut Session, depth: SampleType) -> Result<Value> {
    let current = s.active().ok_or(EngineError::NoDocument)?.doc.depth;
    if current == depth {
        // History::record clears redo even when an edit closure leaves the document unchanged.
        return Ok(Value::Null);
    }
    s.edit("Bit Depth", |doc, _| {
        for_each_surface(&mut doc.layers, true, &mut |surf, _| {
            let f = surf.format().with_sample(depth);
            *surf = surf.convert(f);
        });
        for ch in doc.channels.iter_mut().chain(doc.quick_mask.as_mut()) {
            let f = ch.surface.format().with_sample(depth);
            ch.surface = ch.surface.convert(f);
        }
        doc.depth = depth;
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Image → Duplicate.
fn duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let mut doc = (*d.doc).clone();
    doc.id = photocraft_doc::DocId::fresh();
    doc.name = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("{} copy", d.doc.name));
    if p.get("mergedOnly").and_then(Value::as_bool).unwrap_or(false) {
        let buf = photocraft_compose::flatten(&doc);
        let fmt = doc.pixel_format();
        let mut surf = Surface::new(fmt);
        let vals: Vec<f32> = buf.px.iter().flat_map(|q| photocraft_raster::from_rgba(&fmt, *q)).collect();
        surf.write_region(doc.bounds(), &vals);
        surf.prune();
        doc.layers = vec![Layer::new("Background", LayerContent::Raster(surf))];
    }
    for_each_layer(&mut doc.layers, &mut |l| l.id = photocraft_doc::LayerId::fresh());
    let name = doc.name.clone();
    let index = s.add_document(doc, None);
    Ok(json!({ "document": index, "name": name }))
}

macro_rules! spec {
    ($id:literal, $label:literal, [$($m:literal),*], $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

/// Image menu command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "image.imageSize",
            "Image Size…",
            ["Image"],
            r##"{"width":px,"height":px,"resolution":ppi,"resample":"bicubic|bilinear|nearest|lanczos|preserveDetails|none"="bicubic"}"##,
            has_doc,
            image_size
        ),
        spec!(
            "image.canvasSize",
            "Canvas Size…",
            ["Image"],
            r##"{"width":px,"height":px,"relative":bool=false,"anchor":"topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight"="center","extensionColor":"background|foreground|white|black|transparent|#rrggbb"="background"}"##,
            has_doc,
            canvas_size
        ),
        spec!("image.crop", "Crop", ["Image"], r##"{"x":px,"y":px,"width":px,"height":px,"deleteCroppedPixels":bool=true}"##, has_doc, crop),
        spec!(
            "image.trim",
            "Trim…",
            ["Image"],
            r##"{"basedOn":"transparent|topLeft|bottomRight"="transparent","top":bool=true,"bottom":bool=true,"left":bool=true,"right":bool=true}"##,
            has_doc,
            trim
        ),
        spec!(
            "image.mode.rgb",
            "RGB Color",
            ["Image", "Mode"],
            r##"{"profile":"<builtin id>|working|/path/to/profile.icc"=working,"intent":"perceptual|relative|saturation|absolute"="relative","bpc":bool=true}"##,
            has_doc,
            |s, p| convert_mode(s, ColorMode::Rgb, p)
        ),
        spec!(
            "image.mode.grayscale",
            "Grayscale",
            ["Image", "Mode"],
            r##"{"profile":"<builtin id>|working|/path/to/profile.icc"=working,"intent":"perceptual|relative|saturation|absolute"="relative","bpc":bool=true}"##,
            has_doc,
            |s, p| convert_mode(s, ColorMode::Grayscale, p)
        ),
        spec!(
            "image.mode.cmyk",
            "CMYK Color",
            ["Image", "Mode"],
            r##"{"profile":"<builtin id>|working|/path/to/profile.icc"=working,"intent":"perceptual|relative|saturation|absolute"="relative","bpc":bool=true}"##,
            has_doc,
            |s, p| convert_mode(s, ColorMode::Cmyk, p)
        ),
        spec!(
            "image.mode.lab",
            "Lab Color",
            ["Image", "Mode"],
            r##"{"profile":"<builtin id>|working|/path/to/profile.icc"=working,"intent":"perceptual|relative|saturation|absolute"="relative","bpc":bool=true}"##,
            has_doc,
            |s, p| convert_mode(s, ColorMode::Lab, p)
        ),
        spec!("image.mode.bits8", "8 Bits/Channel", ["Image", "Mode"], "{}", has_doc, |s, _| convert_depth(s, SampleType::U8)),
        spec!("image.mode.bits16", "16 Bits/Channel", ["Image", "Mode"], "{}", has_doc, |s, _| convert_depth(s, SampleType::U16)),
        spec!("image.mode.bits32", "32 Bits/Channel", ["Image", "Mode"], "{}", has_doc, |s, _| convert_depth(s, SampleType::F32)),
        spec!("image.duplicate", "Duplicate…", ["Image"], r##"{"name":str,"mergedOnly":bool=false}"##, has_doc, duplicate),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 20})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("paint", |doc, active| {
            let l = doc.layer_mut(active.unwrap()).unwrap();
            l.surface_mut().unwrap().fill_rect(Rect::new(10, 5, 20, 15), &[1.0, 0.0, 0.0, 1.0]);
            l.mask = Some(photocraft_doc::LayerMask::reveal_all());
            l.effects.items.push(Effect::default_drop_shadow());
            Ok(())
        })
        .unwrap();
        s
    }

    fn doc(s: &Session) -> &Document {
        &s.active().unwrap().doc
    }

    #[test]
    fn image_size_scales_everything() {
        for resample in ["bicubic", "bilinear", "nearest", "lanczos", "preserveDetails"] {
            let mut s = session();
            s.execute("image.imageSize", json!({"width": 80, "resample": resample})).unwrap();
            let d = doc(&s);
            assert_eq!(d.size, Size::new(80, 40), "{resample}");
            let l = &d.layers[1];
            let b = l.surface().unwrap().content_bounds();
            assert!((b.x0 - 20).abs() <= 1 && (b.x1 - 40).abs() <= 1, "{resample} {b:?}");
            let Effect::DropShadow(sh) = &l.effects.items[0] else { panic!() };
            assert_eq!(sh.distance, 10.0);
            assert_eq!(l.mask.as_ref().unwrap().surface.default_pixel(), vec![1.0]);
        }
    }

    #[test]
    fn resolution_only_change() {
        let mut s = session();
        s.execute("image.imageSize", json!({"resolution": 300, "resample": "none", "width": 999})).unwrap();
        assert_eq!(doc(&s).size, Size::new(40, 20));
        assert_eq!(doc(&s).resolution_dpi, 300.0);
    }

    #[test]
    fn canvas_size_anchors_and_extends_background() {
        let mut s = session();
        s.execute("image.canvasSize", json!({"width": 60, "height": 30, "anchor": "topLeft", "extensionColor": "#00ff00"})).unwrap();
        let d = doc(&s);
        assert_eq!(d.size, Size::new(60, 30));
        assert_eq!(d.layers[1].surface().unwrap().content_bounds(), Rect::new(10, 5, 20, 15));
        let bg = d.layers[0].surface().unwrap();
        assert_eq!(bg.pixel(50, 25), vec![0.0, 1.0, 0.0, 1.0]);
        assert_eq!(bg.pixel(5, 5), vec![1.0, 1.0, 1.0, 1.0]);
        let mut s = session();
        s.execute("image.canvasSize", json!({"width": 20, "height": 10, "relative": true, "anchor": "center"})).unwrap();
        assert_eq!(doc(&s).layers[1].surface().unwrap().content_bounds(), Rect::new(20, 10, 30, 20));
    }

    #[test]
    fn crop_to_selection_and_trim() {
        let mut s = session();
        assert!(s.execute("image.crop", json!({})).is_err(), "no selection, no rect");
        s.execute("select.rect", json!({"x": 5, "y": 2, "width": 20, "height": 10})).unwrap();
        s.execute("image.crop", json!({})).unwrap();
        let d = doc(&s);
        assert_eq!(d.size, Size::new(20, 10));
        assert!(d.selection.is_none());
        assert_eq!(d.layers[1].surface().unwrap().content_bounds(), Rect::new(5, 3, 15, 10));
        // Trim transparent: hide the background, trim to the red square.
        let mut s = session();
        s.execute("layer.setProps", json!({"layer": doc(&s).layers[0].id.0, "visible": false})).unwrap();
        s.execute("image.trim", json!({"basedOn": "transparent"})).unwrap();
        // The red square (10×10) plus its drop shadow (distance 5, size 5).
        let size = doc(&s).size;
        assert!(size.width > 10 && size.width < 30 && size.height > 10 && size.height < 25, "{size:?}");
    }

    #[test]
    fn crop_to_explicit_rect_can_extend_canvas() {
        let mut s = session();
        s.execute("image.crop", json!({"x": 10, "y": 5, "width": 10, "height": 10})).unwrap();
        assert_eq!(doc(&s).size, Size::new(10, 10));
        assert_eq!(doc(&s).layers[1].surface().unwrap().pixel(0, 0), vec![1.0, 0.0, 0.0, 1.0]);
        let mut s = session();
        s.execute("image.crop", json!({"x": -10, "y": 0, "width": 60, "height": 20})).unwrap();
        assert_eq!(doc(&s).size, Size::new(60, 20));
        assert_eq!(doc(&s).layers[1].surface().unwrap().pixel(20, 5), vec![1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn trim_based_on_top_left_colour() {
        let mut s = session();
        s.edit("no fx", |doc, _| {
            doc.layers[1].effects.items.clear();
            Ok(())
        })
        .unwrap();
        s.execute("image.trim", json!({"basedOn": "topLeft"})).unwrap();
        assert_eq!(doc(&s).size, Size::new(10, 10));
    }

    #[test]
    fn mode_and_depth_conversions() {
        let mut s = session();
        s.execute("image.mode.grayscale", json!({})).unwrap();
        assert_eq!(doc(&s).mode, ColorMode::Grayscale);
        assert_eq!(doc(&s).layers[1].surface().unwrap().format().mode, ColorMode::Grayscale);
        s.execute("image.mode.bits16", json!({})).unwrap();
        assert_eq!(doc(&s).depth, SampleType::U16);
        assert_eq!(doc(&s).layers[1].surface().unwrap().format().sample, SampleType::U16);
        assert_eq!(doc(&s).layers[1].mask.as_ref().unwrap().surface.format().sample, SampleType::U16);
        s.execute("image.mode.cmyk", json!({})).unwrap();
        s.execute("image.mode.bits32", json!({})).unwrap();
        s.execute("image.mode.lab", json!({})).unwrap();
        s.execute("image.mode.rgb", json!({})).unwrap();
        s.execute("image.mode.bits8", json!({})).unwrap();
        let d = doc(&s);
        assert_eq!((d.mode, d.depth), (ColorMode::Rgb, SampleType::U8));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(doc(&s).depth, SampleType::F32);
        let revision = s.active().unwrap().revision;
        s.execute("image.mode.bits32", json!({})).unwrap();
        assert_eq!(s.active().unwrap().revision, revision, "an unchanged bit depth must preserve the history branch");
        assert!(s.redo(), "selecting the current depth must not discard the undone conversion");
        assert_eq!(doc(&s).depth, SampleType::U8);
    }

    #[test]
    fn duplicate_document() {
        let mut s = session();
        let r = s.execute("image.duplicate", json!({"mergedOnly": true})).unwrap();
        assert_eq!(r["document"], json!(1));
        assert_eq!(s.documents().len(), 2);
        assert_eq!(doc(&s).layers.len(), 1);
        assert!(doc(&s).name.ends_with("copy"));
    }
}

//! Smart selection commands: Quick Selection, Object Selection, Select Subject, Select and Mask
//! (Refine Edge) and Focus Area. The algorithms live in `photocraft_algo::segment` and
//! `photocraft_algo::matting`; these commands pick the pixels to analyse (active layer or
//! composite), run them, and store the result as the selection (or a mask / new layer).

use photocraft_algo::matting::{self, RefineParams};
use photocraft_algo::segment::{Sampler, SurfaceSampler, focus, grabcut, quick, subject};
use photocraft_algo::selection::{Region, SelectionMode, combine_region};
use photocraft_doc::{Document, Layer, LayerContent, LayerMask};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}
fn has_selection(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

fn f(p: &Value, k: &str, d: f32) -> f32 {
    p.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn b(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn mode(p: &Value, default: &str) -> SelectionMode {
    let m = p.get("mode").and_then(Value::as_str).unwrap_or(default);
    SelectionMode::parse(if m == "new" { "replace" } else { m })
}
fn bad(cmd: &str, msg: &str) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// Composite of all visible layers.
struct CompositeSampler<'a>(&'a Document);

impl Sampler for CompositeSampler<'_> {
    fn rgba(&self, r: Rect) -> Vec<[f32; 4]> {
        photocraft_compose::render(self.0, r).px
    }
}

/// Runs `f` with a sampler over the active layer (or the composite with `all_layers`, or when the
/// active layer has no pixels).
fn with_sampler<R>(s: &Session, all_layers: bool, f: impl FnOnce(&dyn Sampler, &Document) -> R) -> Result<R> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let layer = d.active_layer.and_then(|id| doc.layer(id)).and_then(|l| l.surface());
    Ok(match (all_layers, layer) {
        (false, Some(surf)) => f(&SurfaceSampler(surf), &doc),
        _ => f(&CompositeSampler(&doc), &doc),
    })
}

/// Stores `region` combined with the current selection by `m` as one history step.
fn apply(s: &mut Session, label: &str, region: Option<Region>, m: SelectionMode) -> Result<Value> {
    if region.is_none() && matches!(m, SelectionMode::Add | SelectionMode::Subtract) {
        // Nothing found: leave the selection (and history) alone.
        let selected = s.active().ok_or(EngineError::NoDocument)?.doc.selection.is_some();
        return Ok(json!({ "selected": selected, "changed": false }));
    }
    let bbox = region.as_ref().map(|r| r.bbox);
    let selected = s.edit(label, |doc, _| {
        doc.selection = combine_region(doc.selection.as_ref(), region.as_ref(), m);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected, "changed": true, "bounds": bbox.map(|r| [r.x0, r.y0, r.width() as i32, r.height() as i32]) }))
}

fn points(p: &Value) -> Vec<(f32, f32)> {
    p.get("points")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|q| Some((q.get(0)?.as_f64()? as f32, q.get(1)?.as_f64()? as f32))).collect())
        .unwrap_or_default()
}

/// Auto-Enhance for Quick Selection: a light edge refinement that flows the edge towards image
/// edges and removes blockiness.
const ENHANCE: RefineParams = RefineParams { radius: 4.0, smart_radius: true, smooth: 15.0, feather: 0.0, contrast: 25.0, shift_edge: 0.0 };

fn quick_selection(s: &mut Session, p: &Value) -> Result<Value> {
    let pts = points(p);
    if pts.is_empty() {
        return Err(bad("select.quick", "needs at least one point"));
    }
    let size = f(p, "size", 30.0).clamp(1.0, 5000.0);
    let m = match p.get("mode").and_then(Value::as_str).unwrap_or("add") {
        "subtract" => SelectionMode::Subtract,
        "replace" | "new" => SelectionMode::Replace,
        _ => SelectionMode::Add,
    };
    let enhance = b(p, "enhanceEdge", false);
    let region = with_sampler(s, b(p, "sampleAllLayers", false), |smp, doc| {
        let canvas = doc.bounds();
        let r = quick::quick_select(smp, canvas, &pts, size, quick::WORK_PX)?;
        if enhance { matting::refine_mask(smp, &matting::region_reader(&r), r.bbox, canvas, &ENHANCE) } else { Some(r) }
    })?;
    apply(s, "Quick Selection", region, m)
}

fn rect_param(p: &Value) -> Option<Rect> {
    let a = p.get("rect")?.as_array()?;
    let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
    if v.len() < 4 {
        return None;
    }
    // Negative sizes (dragging up/left) are normalised.
    let (x0, x1) = (v[0].min(v[0] + v[2]), v[0].max(v[0] + v[2]));
    let (y0, y1) = (v[1].min(v[1] + v[3]), v[1].max(v[1] + v[3]));
    Some(Rect::new(x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32))
}

fn object_selection(s: &mut Session, p: &Value) -> Result<Value> {
    let rect = rect_param(p).ok_or_else(|| bad("select.object", "needs \"rect\": [x, y, w, h]"))?;
    let region = with_sampler(s, b(p, "sampleAllLayers", false), |smp, doc| grabcut::object_select(smp, doc.bounds(), rect, 160_000))?;
    apply(s, "Object Selection", region, mode(p, "replace"))
}

fn select_subject(s: &mut Session, p: &Value) -> Result<Value> {
    let region = with_sampler(s, b(p, "sampleAllLayers", true), |smp, doc| subject::select_subject(smp, doc.bounds()))?;
    apply(s, "Select Subject", region, mode(p, "replace"))
}

fn focus_area(s: &mut Session, p: &Value) -> Result<Value> {
    let (range, noise) = (f(p, "range", 0.5).clamp(0.0, 1.0), f(p, "noise", 0.0).clamp(0.0, 1.0));
    let region = with_sampler(s, b(p, "sampleAllLayers", true), |smp, doc| focus::focus_area(smp, doc.bounds(), range, noise))?;
    apply(s, "Focus Area", region, mode(p, "replace"))
}

fn refine_params(p: &Value) -> RefineParams {
    RefineParams {
        radius: f(p, "radius", 0.0).clamp(0.0, 250.0),
        smart_radius: b(p, "smartRadius", false),
        smooth: f(p, "smooth", 0.0).clamp(0.0, 100.0),
        feather: f(p, "feather", 0.0).clamp(0.0, 1000.0),
        contrast: f(p, "contrast", 0.0).clamp(0.0, 100.0),
        shift_edge: f(p, "shiftEdge", 0.0).clamp(-100.0, 100.0),
    }
}

/// Select and Mask: refines the selection edge, then outputs to the selection, a layer mask on
/// the active layer, or a new layer (with or without a mask). Decontaminating colours needs a
/// new layer, so it upgrades `selection` / `layerMask` output to `newLayerWithMask` (as
/// Photoshop does). New-layer outputs hide the source layer.
fn refine_edge(s: &mut Session, p: &Value) -> Result<Value> {
    let params = refine_params(p);
    let decontaminate = b(p, "decontaminate", false);
    let amount = f(p, "amount", 100.0).clamp(0.0, 100.0);
    let mut output = p.get("output").and_then(Value::as_str).unwrap_or("selection").to_string();
    if !matches!(output.as_str(), "selection" | "layerMask" | "newLayer" | "newLayerWithMask") {
        return Err(bad("select.refineEdge", "output must be selection|layerMask|newLayer|newLayerWithMask"));
    }
    if decontaminate && matches!(output.as_str(), "selection" | "layerMask") {
        output = "newLayerWithMask".into();
    }
    let region = with_sampler(s, b(p, "sampleAllLayers", false), |smp, doc| {
        let sel = doc.selection.as_ref()?;
        matting::refine_mask(smp, &matting::surface_reader(sel), sel.content_bounds(), doc.bounds(), &params)
    })?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let active = d.active_layer.filter(|id| d.doc.layer(*id).is_some());
    match output.as_str() {
        "selection" => {
            let selected = s.edit("Select and Mask", |doc, _| {
                doc.selection = region.as_ref().map(matting::region_surface);
                Ok(doc.selection.is_some())
            })?;
            Ok(json!({ "output": output, "selected": selected }))
        }
        "layerMask" => {
            let id = active.ok_or_else(|| EngineError::Other("no active layer".into()))?;
            s.edit("Select and Mask", |doc, _| {
                let surface =
                    region.as_ref().map(matting::region_surface).unwrap_or_else(|| photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8));
                doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.mask = Some(LayerMask { surface, ..LayerMask::reveal_all() });
                doc.selection = None;
                Ok(())
            })?;
            Ok(json!({ "output": output, "layer": id.0, "selected": false }))
        }
        _ => {
            let id = active.ok_or_else(|| EngineError::Other("no active layer".into()))?;
            let src_layer = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let src = src_layer
                .surface()
                .ok_or_else(|| EngineError::Other(format!("the active layer is a {} layer without pixels", src_layer.content.kind_name())))?;
            let name = format!("{} copy", src_layer.name);
            let empty = Region { bbox: Rect::EMPTY, mask: Vec::new() };
            let reg = region.as_ref().unwrap_or(&empty);
            let pixels = if decontaminate { matting::decontaminate(src, reg, params.radius, amount) } else { src.clone() };
            let with_mask = output == "newLayerWithMask";
            let (content, mask) = if with_mask {
                (pixels, Some(LayerMask { surface: matting::region_surface(reg), ..LayerMask::reveal_all() }))
            } else {
                (matting::masked_copy(&pixels, reg), None)
            };
            let new_id = s.edit("Select and Mask", |doc, act| {
                let mut layer = Layer::new(name, LayerContent::Raster(content));
                layer.mask = mask;
                let nid = doc.insert_above(Some(id), layer);
                if let Some(l) = doc.layer_mut(id) {
                    l.visible = false;
                }
                doc.selection = None;
                *act = Some(nid);
                Ok(nid)
            })?;
            Ok(json!({ "output": output, "layer": new_id.0, "selected": false }))
        }
    }
}

macro_rules! spec {
    ($id:literal, $label:literal, [$($m:literal),*], $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

/// Smart selection command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "select.quick",
            "Quick Selection",
            [],
            r##"{"points":[[x,y],…],"size":px=30,"mode":"add|subtract|replace"="add","sampleAllLayers":bool=false,"enhanceEdge":bool=false}"##,
            has_doc,
            quick_selection
        ),
        spec!(
            "select.object",
            "Object Selection",
            [],
            r##"{"rect":[x,y,w,h],"mode":"replace|add|subtract|intersect"="replace","sampleAllLayers":bool=false}"##,
            has_doc,
            object_selection
        ),
        spec!(
            "select.subject",
            "Subject",
            ["Select"],
            r##"{"sampleAllLayers":bool=true,"mode":"replace|add|subtract|intersect"="replace"}"##,
            has_doc,
            select_subject
        ),
        spec!(
            "select.refineEdge",
            "Refine Edge",
            [],
            r##"{"radius":px=0,"smartRadius":bool=false,"smooth":0..100=0,"feather":px=0,"contrast":0..100=0,"shiftEdge":-100..100=0,"decontaminate":bool=false,"amount":0..100=100,"output":"selection|layerMask|newLayer|newLayerWithMask"="selection","sampleAllLayers":bool=false}"##,
            has_selection,
            refine_edge
        ),
        spec!(
            "select.focusArea",
            "Focus Area…",
            ["Select"],
            r##"{"range":0..1=0.5,"noise":0..1=0,"sampleAllLayers":bool=true,"mode":"replace|add|subtract|intersect"="replace"}"##,
            has_doc,
            focus_area
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_algo::segment::Rng;

    /// A document whose background shows `px(x, y)` (RGB, 0–1), at the given depth.
    fn session_with(w: u32, h: u32, depth: u32, px: impl Fn(u32, u32) -> [f32; 3]) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": w, "height": h, "depth": depth})).unwrap();
        s.edit("paint", |doc, _| {
            let bg = doc.layers[0].surface_mut().unwrap();
            let n = bg.channels();
            let mut data = Vec::with_capacity((w * h) as usize * n);
            for y in 0..h {
                for x in 0..w {
                    let c = px(x, y);
                    data.extend_from_slice(&c[..n.min(3)]);
                    if n == 4 {
                        data.push(1.0);
                    }
                }
            }
            bg.write_region(Rect::new(0, 0, w as i32, h as i32), &data);
            Ok(())
        })
        .unwrap();
        s
    }

    fn noise_field(w: u32, h: u32, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        (0..w * h).map(|_| rng.normal()).collect()
    }

    /// Warm textured left part (x < 60), cool right part.
    fn two_regions(depth: u32) -> Session {
        let nz = noise_field(140, 100, 1);
        session_with(140, 100, depth, move |x, y| {
            let n = 0.04 * nz[(y * 140 + x) as usize];
            if x < 60 { [0.8 + n, 0.5 + n, 0.3] } else { [0.2, 0.35 + n, 0.6 + n] }.map(|v| v.clamp(0.0, 1.0))
        })
    }

    /// Textured disc (centre (100, 80), radius 45) on a noisy background.
    fn disc(depth: u32) -> (Session, impl Fn(i32, i32) -> bool) {
        let nz = noise_field(200, 160, 2);
        let inside = |x: i32, y: i32| {
            let (dx, dy) = (x as f32 + 0.5 - 100.0, y as f32 + 0.5 - 80.0);
            dx * dx + dy * dy <= 45.0 * 45.0
        };
        let s = session_with(200, 160, depth, move |x, y| {
            let n = 0.05 * nz[(y * 200 + x) as usize];
            if inside(x as i32, y as i32) {
                let t = ((x + y) as f32 * 0.35).sin() * 0.5 + 0.5;
                [0.85 + 0.05 * t + n, 0.2 + 0.3 * t + n, 0.15 + n]
            } else {
                [0.25 + n, 0.45 + n, 0.7 + n]
            }
            .map(|v| v.clamp(0.0, 1.0))
        });
        (s, inside)
    }

    fn cov(s: &Session, x: i32, y: i32) -> f32 {
        s.active().unwrap().doc.selection.as_ref().map_or(0.0, |m| m.sample_channel(x, y, 0))
    }

    fn sel_iou(s: &Session, w: i32, h: i32, truth: impl Fn(i32, i32) -> bool) -> f32 {
        let (mut i, mut u) = (0, 0);
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (cov(s, x, y) >= 0.5, truth(x, y));
                i += (a && b) as i32;
                u += (a || b) as i32;
            }
        }
        i as f32 / u.max(1) as f32
    }

    #[test]
    fn quick_selection_add_subtract_undo() {
        for depth in [8, 16] {
            let mut s = two_regions(depth);
            let r = s.execute("select.quick", json!({"points": [[20, 50], [35, 50]], "size": 8})).unwrap();
            assert_eq!(r["selected"], true);
            for y in (0..100).step_by(7) {
                for x in (0..140).step_by(3) {
                    let c = cov(&s, x, y);
                    if x < 57 {
                        assert!(c >= 0.5, "depth {depth}: ({x},{y}) not selected");
                    } else if x > 62 {
                        assert!(c < 0.5, "depth {depth}: leaked to ({x},{y})");
                    }
                }
            }
            // A second stroke on the right adds it.
            s.execute("select.quick", json!({"points": [[100, 50]], "size": 8})).unwrap();
            assert!(cov(&s, 130, 90) >= 0.5 && cov(&s, 10, 10) >= 0.5);
            // Subtract the left region again.
            s.execute("select.quick", json!({"points": [[20, 50]], "size": 8, "mode": "subtract"})).unwrap();
            assert!(cov(&s, 10, 10) < 0.5 && cov(&s, 130, 90) >= 0.5);
            s.execute("edit.undo", json!({})).unwrap();
            assert!(cov(&s, 10, 10) >= 0.5);
            assert!(s.execute("select.quick", json!({"points": []})).is_err());
        }
    }

    #[test]
    fn quick_selection_subtract_from_select_all_and_enhance() {
        let mut s = two_regions(8);
        s.execute("select.all", json!({})).unwrap();
        s.execute("select.quick", json!({"points": [[100, 30], [110, 70]], "size": 10, "mode": "subtract"})).unwrap();
        assert!(cov(&s, 10, 50) >= 0.99);
        assert!(cov(&s, 120, 20) < 0.01 && cov(&s, 70, 90) < 0.01);
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("select.quick", json!({"points": [[20, 50]], "size": 8, "enhanceEdge": true})).unwrap();
        assert!(cov(&s, 10, 10) >= 0.5 && cov(&s, 100, 50) < 0.5);
    }

    #[test]
    fn object_selection_disc() {
        for depth in [8, 16, 32] {
            let (mut s, inside) = disc(depth);
            s.execute("select.object", json!({"rect": [40, 20, 120, 120]})).unwrap();
            let v = sel_iou(&s, 200, 160, &inside);
            assert!(v > 0.95, "depth {depth}: IoU {v}");
        }
        let (mut s, _) = disc(8);
        assert!(s.execute("select.object", json!({"rect": [1, 2]})).is_err());
        // Negative width/height (drag up-left) work too.
        s.execute("select.object", json!({"rect": [160, 140, -120, -120]})).unwrap();
        assert!(cov(&s, 100, 80) >= 0.5);
    }

    #[test]
    fn select_subject_centred_object() {
        let (mut s, inside) = disc(8);
        assert!(s.is_enabled("select.subject"));
        s.execute("select.subject", json!({})).unwrap();
        let v = sel_iou(&s, 200, 160, inside);
        assert!(v > 0.9, "IoU {v}");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none());
    }

    /// A vertical Gaussian-blurred edge at x = 40 (warm left, cool right) with the left half selected.
    fn blurred_edge_selected() -> Session {
        let mut s = session_with(80, 40, 8, |x, _| {
            let t = 1.0 / (1.0 + ((x as f32 + 0.5 - 40.0) / 1.8).exp());
            [0.9 * t + 0.1 * (1.0 - t), 0.8 * t + 0.2 * (1.0 - t), 0.2 * t + 0.6 * (1.0 - t)]
        });
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 40, "height": 40})).unwrap();
        s
    }

    fn soft_count(s: &photocraft_raster::Surface, y: i32) -> usize {
        (0..80)
            .filter(|x| {
                let v = s.sample_channel(*x, y, 0);
                v > 0.05 && v < 0.95
            })
            .count()
    }

    #[test]
    fn refine_edge_outputs_and_undo() {
        // Selection output: the hard edge becomes a soft matte following the blur.
        let mut s = blurred_edge_selected();
        assert!(s.is_enabled("select.refineEdge"));
        s.execute("select.refineEdge", json!({"radius": 8})).unwrap();
        let sel = s.active().unwrap().doc.selection.clone().unwrap();
        assert!(soft_count(&sel, 20) >= 4, "selection edge not soft");
        assert!(sel.sample_channel(5, 20, 0) > 0.99 && sel.sample_channel(75, 20, 0) < 0.01);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(soft_count(s.active().unwrap().doc.selection.as_ref().unwrap(), 20), 0);

        // Layer mask output.
        let layers_before = s.active().unwrap().doc.layer_count();
        let r = s.execute("select.refineEdge", json!({"radius": 8, "output": "layerMask"})).unwrap();
        let d = s.active().unwrap();
        let l = d.doc.layer(photocraft_doc::LayerId(r["layer"].as_u64().unwrap())).unwrap();
        let m = l.mask.as_ref().unwrap();
        assert!(soft_count(&m.surface, 20) >= 4);
        assert!(m.value(5, 20) > 0.99 && m.value(75, 20) < 0.01);
        assert!(d.doc.selection.is_none());
        assert_eq!(d.doc.layer_count(), layers_before);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layers[0].mask.is_none());
        assert!(s.active().unwrap().doc.selection.is_some());

        // New layer: pixels with the refined alpha; the source is hidden.
        let r = s.execute("select.refineEdge", json!({"radius": 8, "feather": 1, "output": "newLayer"})).unwrap();
        let d = s.active().unwrap();
        let nid = photocraft_doc::LayerId(r["layer"].as_u64().unwrap());
        assert_eq!(d.active_layer, Some(nid));
        assert_eq!(d.doc.layer_count(), layers_before + 1);
        let nl = d.doc.layer(nid).unwrap();
        let surf = nl.surface().unwrap();
        assert!(surf.format().alpha);
        let a = |x: i32| surf.pixel(x, 20)[3];
        assert!(a(5) > 0.99 && a(75) == 0.0 && a(40) > 0.05 && a(40) < 0.95);
        assert!(!d.doc.layers[0].visible);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.layer_count(), layers_before);
        assert!(s.active().unwrap().doc.layers[0].visible);

        // New layer with mask + decontamination (selection output upgrades to it).
        let r = s.execute("select.refineEdge", json!({"radius": 8, "decontaminate": true, "amount": 100})).unwrap();
        assert_eq!(r["output"], "newLayerWithMask");
        let d = s.active().unwrap();
        let nl = d.doc.layer(photocraft_doc::LayerId(r["layer"].as_u64().unwrap())).unwrap();
        assert!(nl.mask.is_some());
        let px = nl.surface().unwrap().pixel(41, 20);
        let orig = d.doc.layers[0].surface().unwrap().pixel(41, 20);
        // The fringe colour moved towards the warm foreground (more red, less blue).
        assert!(px[0] > orig[0] + 0.05 && px[2] < orig[2] - 0.05, "{px:?} vs {orig:?}");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.execute("select.refineEdge", json!({"output": "bogus"})).is_err());
        s.execute("select.deselect", json!({})).unwrap();
        assert!(!s.is_enabled("select.refineEdge"));
    }

    #[test]
    fn refine_edge_shift_and_contrast() {
        let mut s = blurred_edge_selected();
        s.execute("select.refineEdge", json!({"shiftEdge": 100})).unwrap();
        assert!(cov(&s, 42, 20) > 0.99);
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("select.refineEdge", json!({"feather": 8, "contrast": 100})).unwrap();
        assert!(cov(&s, 38, 20) == 1.0 && cov(&s, 42, 20) == 0.0);
    }

    #[test]
    fn focus_area_command() {
        let nz = noise_field(160, 100, 3);
        let mut blurred = vec![0.0f32; nz.len()];
        for y in 0..100i32 {
            for x in 0..160i32 {
                let mut a = 0.0;
                for dy in -4..=4 {
                    for dx in -4..=4 {
                        a += nz[((y + dy).clamp(0, 99) * 160 + (x + dx).clamp(0, 159)) as usize];
                    }
                }
                blurred[(y * 160 + x) as usize] = a / 81.0;
            }
        }
        let mut s = session_with(160, 100, 8, move |x, y| {
            let i = (y * 160 + x) as usize;
            let v = 0.5 + 0.15 * if x < 80 { nz[i] } else { blurred[i] * 3.0 };
            [v, v, v].map(|c| c.clamp(0.0, 1.0))
        });
        s.execute("select.focusArea", json!({})).unwrap();
        assert!(cov(&s, 30, 50) >= 0.5 && cov(&s, 130, 50) < 0.5);
    }
}

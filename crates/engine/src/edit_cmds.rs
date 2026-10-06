//! Everyday Photoshop commands: clipboard (Cut/Copy/Copy Merged/Paste/Paste in Place), Layer via
//! Copy/Cut, Merge Visible, Auto Tone/Contrast/Color, Toggle Last State and Transform Again.

use photocraft_color::PixelFormat;
use photocraft_doc::adjust::LevelsChannel;
use photocraft_doc::{Adjustment, Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Pixels on the session clipboard, with where they came from (for Paste in Place).
#[derive(Clone, Debug)]
pub struct Clip {
    pub surface: Surface,
    pub bounds: Rect,
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn has_pixels(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let l = d.active_layer.and_then(|id| d.doc.layer(id)).ok_or("no active layer")?;
    if l.surface().is_some() && matches!(l.content, LayerContent::Raster(_)) { Ok(()) } else { Err("the active layer has no pixels".into()) }
}

fn has_clip(s: &Session) -> std::result::Result<(), String> {
    has_doc(s)?;
    s.clipboard.as_ref().map(|_| ()).ok_or_else(|| "the clipboard is empty".into())
}

fn active_id(s: &Session) -> Result<LayerId> {
    s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into()))
}

/// The pixels of `src` inside the selection (or all of them), as a straight-alpha surface.
fn lift(src: &Surface, sel: Option<&Surface>, canvas: Rect) -> Clip {
    let fmt = src.format();
    let with_alpha = PixelFormat::new(fmt.mode, fmt.sample, true);
    let area = match sel {
        Some(m) => m.content_bounds().intersect(&canvas),
        None => src.content_bounds().intersect(&canvas),
    };
    let mut out = Surface::new(with_alpha);
    if area.is_empty() {
        return Clip { surface: out, bounds: Rect::EMPTY };
    }
    let conv = src.convert(with_alpha);
    let mut px = conv.read_region(area);
    let n = with_alpha.channels();
    if let Some(m) = sel {
        let w = area.width() as usize;
        for (i, p) in px.chunks_exact_mut(n).enumerate() {
            p[n - 1] *= m.sample_channel(area.x0 + (i % w) as i32, area.y0 + (i / w) as i32, 0);
        }
    }
    out.write_region(area, &px);
    out.prune();
    let bounds = out.content_bounds();
    Clip { surface: out, bounds }
}

/// Merged composite of the visible document as a surface in the document's format.
fn merged_surface(doc: &Document) -> Surface {
    let fmt = doc.pixel_format();
    photocraft_compose::flatten_to_surface(doc, PixelFormat::new(fmt.mode, fmt.sample, true), None)
}

fn copy(s: &mut Session, merged: bool) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let clip = if merged {
        lift(&merged_surface(&d.doc), d.doc.selection.as_ref(), canvas)
    } else {
        let id = d.active_layer.ok_or(EngineError::Other("no active layer".into()))?;
        let surf = d.doc.layer(id).and_then(Layer::surface).ok_or(EngineError::Other("the active layer has no pixels".into()))?;
        lift(surf, d.doc.selection.as_ref(), canvas)
    };
    if clip.bounds.is_empty() {
        return Err(EngineError::Other("Could not copy: the selected area is empty".into()));
    }
    let b = clip.bounds;
    s.clipboard = Some(clip);
    Ok(json!({"bounds": [b.x0, b.y0, b.width(), b.height()]}))
}

fn clear_selected(doc: &mut Document, id: LayerId, background: [f32; 4]) -> Result<()> {
    let sel = doc.selection.clone();
    let canvas = doc.bounds();
    let area = sel.as_ref().map_or(canvas, |m| m.content_bounds().intersect(&canvas));
    clear_area(doc, id, area, sel.as_ref(), background)
}

/// Cut / Clear on a layer: makes the selected pixels transparent. The Background can't hold
/// transparency, so there the area is filled with the background colour instead (Photoshop).
pub(crate) fn clear_area(doc: &mut Document, id: LayerId, area: Rect, sel: Option<&Surface>, background: [f32; 4]) -> Result<()> {
    let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
    let bg = crate::extra_cmds::is_background(l);
    let surf = l.surface_mut().ok_or(EngineError::Other("the active layer has no pixels".into()))?;
    if bg {
        crate::pixels::fill_surface(surf, area, background, sel, true);
    } else {
        crate::pixels::clear_surface(surf, area, sel);
    }
    surf.prune();
    Ok(())
}

/// Paste as a new layer. `in_place` keeps the original position; otherwise the pixels are centred
/// on `center` (the view centre from the UI) or the canvas, unless they already overlap the canvas.
fn paste(s: &mut Session, p: &Value, in_place: bool) -> Result<Value> {
    let clip = s.clipboard.clone().ok_or(EngineError::Other("the clipboard is empty".into()))?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let fmt = d.doc.pixel_format();
    let (dx, dy) = if in_place || (clip.bounds.intersect(&canvas) == clip.bounds && p.get("center").is_none()) {
        (0, 0)
    } else {
        let c = p.get("center").and_then(Value::as_array).filter(|a| a.len() >= 2).map(|a| (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)));
        let (cx, cy) = c.unwrap_or(((canvas.x0 + canvas.x1) as f64 / 2.0, (canvas.y0 + canvas.y1) as f64 / 2.0));
        let b = clip.bounds;
        ((cx - (b.x0 + b.x1) as f64 / 2.0).round() as i32, (cy - (b.y0 + b.y1) as f64 / 2.0).round() as i32)
    };
    let moved = if dx == 0 && dy == 0 { clip.surface.clone() } else { photocraft_algo::resample::translate_surface(&clip.surface, dx, dy) };
    let target = PixelFormat::new(fmt.mode, fmt.sample, true);
    let surf = if moved.format() == target { moved } else { moved.convert(target) };
    let id = s.edit("Paste", |doc, active| {
        let mut l = Layer::raster(doc.next_layer_name("Layer"), target);
        *crate::pixels_mut(&mut l)? = surf;
        let id = doc.insert_above(*active, l);
        *active = Some(id);
        doc.selection = None;
        Ok(id)
    })?;
    Ok(json!({"layer": id.0, "offset": [dx, dy]}))
}

fn layer_via(s: &mut Session, cut: bool) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = active_id(s)?;
    let has_sel = d.doc.selection.is_some();
    if !has_sel && !cut {
        // No selection: Layer via Copy duplicates the whole layer (Photoshop).
        let nid = s.edit("Layer Via Copy", |doc, active| {
            let src = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let mut dup = src.duplicate();
            dup.name = format!("{} copy", src.name);
            dup.locks = Default::default();
            let nid = doc.insert_above(Some(id), dup);
            *active = Some(nid);
            Ok(nid)
        })?;
        return Ok(json!({"layer": nid.0}));
    }
    let surf = d.doc.layer(id).and_then(Layer::surface).ok_or(EngineError::Other("the active layer has no pixels".into()))?;
    let clip = lift(surf, d.doc.selection.as_ref(), d.doc.bounds());
    if clip.bounds.is_empty() {
        return Err(EngineError::Other("Could not complete the command: the selected area is empty".into()));
    }
    let bg = s.tools.background;
    let label = if cut { "Layer Via Cut" } else { "Layer Via Copy" };
    let nid = s.edit(label, |doc, active| {
        if cut {
            clear_selected(doc, id, bg)?;
        }
        let mut l = Layer::raster(doc.next_layer_name("Layer"), clip.surface.format());
        *crate::pixels_mut(&mut l)? = clip.surface;
        let nid = doc.insert_above(Some(id), l);
        *active = Some(nid);
        Ok(nid)
    })?;
    Ok(json!({"layer": nid.0}))
}

fn merge_visible(s: &mut Session) -> Result<Value> {
    s.edit("Merge Visible", |doc, active| {
        let visible: Vec<usize> = doc.layers.iter().enumerate().filter(|(_, l)| l.visible).map(|(i, _)| i).collect();
        if visible.len() < 2 && !visible.iter().any(|i| doc.layers[*i].is_group()) {
            return Err(EngineError::Other("there are not enough visible layers to merge".into()));
        }
        let lowest = visible[0];
        let base = doc.layers[lowest].clone();
        let is_background = base.name == "Background" && base.locks.transparency;
        // Composite of visible layers only (hidden ones stay where they are).
        let mut solo = doc.clone();
        solo.layers.retain(|l| l.visible);
        let buf = photocraft_compose::flatten(&solo);
        let buf = if is_background { buf.over_background([1.0, 1.0, 1.0]) } else { buf };
        let fmt = doc.pixel_format();
        let fmt = if is_background { fmt } else { PixelFormat::new(fmt.mode, fmt.sample, true) };
        let data: Vec<f32> = buf.px.iter().flat_map(|p| photocraft_raster::from_rgba(&fmt, *p)).collect();
        let mut merged = Layer::raster(base.name.clone(), fmt);
        merged.locks = base.locks;
        let surf = crate::pixels_mut(&mut merged)?;
        surf.write_region(doc.bounds(), &data);
        surf.prune();
        let mid = merged.id;
        let mut out = Vec::with_capacity(doc.layers.len());
        for (i, l) in doc.layers.drain(..).enumerate() {
            if i == lowest {
                out.push(merged.clone());
            } else if !l.visible {
                out.push(l);
            }
        }
        doc.layers = out;
        *active = Some(mid);
        Ok(())
    })?;
    Ok(Value::Null)
}

/// Histogram of a pixel layer (8-bit bins per channel: R, G, B) inside the selection.
fn histograms(surf: &Surface, area: Rect) -> [[u64; 256]; 3] {
    let mut h = [[0u64; 256]; 3];
    let step = ((area.width() as u64 * area.height() as u64) / 4_000_000).max(1) as usize; // sample big images
    let mut row = vec![[0u8; 4]; area.width() as usize];
    for y in (area.y0..area.y1).step_by(step.isqrt().max(1)) {
        surf.read_rgba8_into(Rect::new(area.x0, y, area.x1, y + 1), &mut row);
        for p in row.iter().step_by(step.isqrt().max(1)) {
            if p[3] == 0 {
                continue;
            }
            for c in 0..3 {
                h[c][p[c] as usize] += 1;
            }
        }
    }
    h
}

/// Black and white points with `clip` (fraction) clipped at each end.
fn clip_points(h: &[u64; 256], clip: f64) -> (f32, f32) {
    let total: u64 = h.iter().sum();
    if total == 0 {
        return (0.0, 1.0);
    }
    let lim = (total as f64 * clip) as u64;
    let (mut acc, mut lo) = (0u64, 0usize);
    while lo < 254 && acc + h[lo] <= lim {
        acc += h[lo];
        lo += 1;
    }
    let (mut acc, mut hi) = (0u64, 255usize);
    while hi > lo + 1 && acc + h[hi] <= lim {
        acc += h[hi];
        hi -= 1;
    }
    (lo as f32 / 255.0, hi as f32 / 255.0)
}

fn median(h: &[u64; 256]) -> f32 {
    let total: u64 = h.iter().sum();
    let mut acc = 0;
    for (i, v) in h.iter().enumerate() {
        acc += v;
        if acc * 2 >= total {
            return i as f32 / 255.0;
        }
    }
    0.5
}

/// Image › Auto Tone / Auto Contrast / Auto Color (0.1% clipping, Photoshop's defaults).
fn auto_adjust(s: &mut Session, kind: &str) -> Result<Value> {
    let id = active_id(s)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let surf = d.doc.layer(id).and_then(Layer::surface).ok_or(EngineError::Other("the active layer has no pixels".into()))?;
    let area = d.doc.selection.as_ref().map_or(d.doc.bounds(), |m| m.content_bounds()).intersect(&d.doc.bounds());
    let h = histograms(surf, area);
    let clip = 0.001;
    let mut per = [LevelsChannel::default(), LevelsChannel::default(), LevelsChannel::default()];
    match kind {
        "contrast" => {
            // Same points for all channels (preserves colour), from the combined histogram.
            let mut all = [0u64; 256];
            for c in &h {
                for (a, v) in all.iter_mut().zip(c) {
                    *a += v;
                }
            }
            let (lo, hi) = clip_points(&all, clip);
            for p in &mut per {
                (p.in_black, p.in_white) = (lo, hi);
            }
        }
        _ => {
            for (c, p) in per.iter_mut().enumerate() {
                (p.in_black, p.in_white) = clip_points(&h[c], clip);
            }
            if kind == "color" {
                // Neutralise midtones: move each channel's median onto the average median.
                let meds: Vec<f32> = (0..3).map(|c| median(&h[c])).collect();
                let target = meds.iter().sum::<f32>() / 3.0;
                for (c, p) in per.iter_mut().enumerate() {
                    let t = ((meds[c] - p.in_black) / (p.in_white - p.in_black).max(1e-3)).clamp(0.02, 0.98);
                    let want = ((target - p.in_black) / (p.in_white - p.in_black).max(1e-3)).clamp(0.02, 0.98);
                    // out = t^(1/g) = want  →  g = ln t / ln want
                    p.gamma = (t.ln() / want.ln()).clamp(0.1, 9.99);
                }
            }
        }
    }
    let adj = Adjustment::Levels { master: LevelsChannel::default(), per_channel: per, space: Default::default(), black: LevelsChannel::default() };
    let label = match kind {
        "contrast" => "Auto Contrast",
        "color" => "Auto Color",
        _ => "Auto Tone",
    };
    s.edit(label, |doc, _| {
        let sel = doc.selection.clone();
        let mode = doc.mode;
        let surf = doc.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        crate::pixels::adjust_surface(surf, &adj, sel.as_ref(), mode);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn toggle_last_state(s: &mut Session) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let redo = st.history.can_redo();
    Ok(json!(if redo { s.redo() } else { s.undo() }))
}

/// Edit › Transform › Again: replay the last `edit.transform` on the active layer.
fn transform_again(s: &mut Session) -> Result<Value> {
    let (_, last) =
        s.journal.iter().rev().find(|(id, _)| id == "edit.transform").cloned().ok_or(EngineError::Other("there is no transform to repeat".into()))?;
    let (rect, quad) = (last.get("rect").cloned(), last.get("quad").cloned());
    let mut p = json!({"interpolation": last.get("interpolation").cloned().unwrap_or(json!("bicubic"))});
    match (rect, quad, last.get("matrix").cloned()) {
        (Some(r), Some(q), _) => {
            let v = |x: &Value| -> Vec<f64> { x.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default() };
            let r = v(&r);
            let q: Vec<Vec<f64>> = q.as_array().map(|a| a.iter().map(v).collect()).unwrap_or_default();
            if r.len() != 4 || q.len() != 4 || q.iter().any(|row| row.len() < 2) {
                return Err(EngineError::Other("the last transform can't be repeated".into()));
            }
            let h = photocraft_algo::transform::Homography::rect_to_quad(
                [r[0], r[1], r[2], r[3]],
                [[q[0][0], q[0][1]], [q[1][0], q[1][1]], [q[2][0], q[2][1]], [q[3][0], q[3][1]]],
            )
            .ok_or(EngineError::Other("the last transform can't be repeated".into()))?;
            let d = s.active().ok_or(EngineError::NoDocument)?;
            let l = d.doc.layer(active_id(s)?).ok_or(EngineError::Other("no active layer".into()))?;
            let b = crate::transform_cmds::transform_bounds(&d.doc, l);
            if b.is_empty() {
                return Err(EngineError::Other("nothing to transform".into()));
            }
            let nr = [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64];
            let corners = [(nr[0], nr[1]), (nr[2], nr[1]), (nr[2], nr[3]), (nr[0], nr[3])].map(|(x, y)| {
                let (a, b) = h.apply(x, y);
                [a, b]
            });
            p["rect"] = json!(nr);
            p["quad"] = json!(corners);
        }
        (_, _, Some(m)) => p["matrix"] = m,
        _ => return Err(EngineError::Other("the last transform can't be repeated".into())),
    }
    s.execute("edit.transform", p)
}

/// View › New Guide / guide moves (undoable, like Photoshop's "New Guide"/"Move Guide" states).
fn guide_cmd(s: &mut Session, p: &Value, op: &str) -> Result<Value> {
    let vertical = p.get("orientation").and_then(Value::as_str).unwrap_or("horizontal") == "vertical";
    let pos = p.get("position").and_then(Value::as_f64).map(|v| v as f32);
    let index = p.get("index").and_then(Value::as_u64).map(|v| v as usize);
    let label = match op {
        "new" => "New Guide",
        "move" => "Move Guide",
        "delete" => "Delete Guide",
        _ => "Clear Guides",
    };
    s.edit(label, |doc, _| {
        let g = if vertical { &mut doc.guides.vertical } else { &mut doc.guides.horizontal };
        match op {
            "new" => g.push(pos.ok_or(EngineError::Other("missing `position`".into()))?),
            "move" => {
                let i = index.filter(|i| *i < g.len()).ok_or(EngineError::Other("no such guide".into()))?;
                g[i] = pos.ok_or(EngineError::Other("missing `position`".into()))?;
            }
            "delete" => {
                let i = index.filter(|i| *i < g.len()).ok_or(EngineError::Other("no such guide".into()))?;
                g.remove(i);
            }
            _ => {
                doc.guides.horizontal.clear();
                doc.guides.vertical.clear();
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    vec![
        spec!("edit.cut", "Cut", &["Edit"], Some("Cmd+X"), "{}", has_pixels, |s, _| {
            let r = copy(s, false)?;
            let id = active_id(s)?;
            let bg = s.tools.background;
            s.edit("Cut Pixels", |doc, _| clear_selected(doc, id, bg))?;
            Ok(r)
        }),
        spec!("edit.copy", "Copy", &["Edit"], Some("Cmd+C"), "{}", has_pixels, |s, _| copy(s, false)),
        spec!("edit.copyMerged", "Copy Merged", &["Edit"], Some("Cmd+Shift+C"), "{}", has_doc, |s, _| copy(s, true)),
        spec!(
            "edit.paste",
            "Paste",
            &["Edit"],
            Some("Cmd+V"),
            r##"{"center":[x,y]? (view centre; default keeps the position when it overlaps the canvas)}"##,
            has_clip,
            |s, p| paste(s, p, false)
        ),
        spec!("edit.pasteSpecial.pasteInPlace", "Paste in Place", &["Edit", "Paste Special"], Some("Cmd+Shift+V"), "{}", has_clip, |s, p| paste(s, p, true)),
        spec!("layer.new.layerViaCopy", "Layer via Copy", &["Layer", "New"], Some("Cmd+J"), "{}", has_doc, |s, _| layer_via(s, false)),
        spec!("layer.new.layerViaCut", "Layer via Cut", &["Layer", "New"], Some("Cmd+Shift+J"), "{}", has_pixels, |s, _| layer_via(s, true)),
        spec!("layer.mergeVisible", "Merge Visible", &["Layer"], Some("Cmd+Shift+E"), "{}", has_doc, |s, _| merge_visible(s)),
        spec!("image.autoTone", "Auto Tone", &["Image"], Some("Cmd+Shift+L"), "{}", has_pixels, |s, _| auto_adjust(s, "tone")),
        spec!("image.autoContrast", "Auto Contrast", &["Image"], Some("Cmd+Alt+Shift+L"), "{}", has_pixels, |s, _| auto_adjust(s, "contrast")),
        spec!("image.autoColor", "Auto Color", &["Image"], Some("Cmd+Shift+B"), "{}", has_pixels, |s, _| auto_adjust(s, "color")),
        spec!("edit.toggleLastState", "Toggle Last State", &["Edit"], Some("Cmd+Alt+Z"), "{}", has_doc, |s, _| toggle_last_state(s)),
        spec!("edit.transform.again", "Again", &["Edit", "Transform"], Some("Cmd+Shift+T"), "{}", has_doc, |s, _| transform_again(s)),
        spec!("view.newGuide", "New Guide…", &["View"], None, r##"{"orientation":"horizontal|vertical","position":px}"##, has_doc, |s, p| guide_cmd(
            s, p, "new"
        )),
        spec!("view.moveGuide", "Move Guide", &[], None, r##"{"orientation":"horizontal|vertical","index":n,"position":px}"##, has_doc, |s, p| guide_cmd(
            s, p, "move"
        )),
        spec!("view.deleteGuide", "Delete Guide", &[], None, r##"{"orientation":"horizontal|vertical","index":n}"##, has_doc, |s, p| guide_cmd(s, p, "delete")),
        spec!("view.clearGuides", "Clear Guides", &["View"], None, "{}", has_doc, |s, p| guide_cmd(s, p, "clear")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("paint", |doc, a| {
            doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(10, 10, 50, 30), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
        s
    }

    fn active_bounds(s: &Session) -> Rect {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds()
    }

    #[test]
    fn copy_paste_in_place_and_centred() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        s.execute("edit.pasteSpecial.pasteInPlace", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 30, 30));
        assert!(s.active().unwrap().doc.selection.is_none());
        s.execute("edit.paste", json!({"center": [70, 70]})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(60, 60, 80, 80));
        assert_eq!(s.active().unwrap().doc.layers.len(), 4);
    }

    #[test]
    fn paste_with_malformed_center_does_not_panic() {
        // A short/empty `center` array must not panic (Rule 9) — it falls back to the canvas centre.
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        for center in [json!([5]), json!([]), json!("nope"), json!([1, 2, 3])] {
            // Fresh clipboard each time is unnecessary; copy persists. Must return Ok, never panic.
            assert!(s.execute("edit.paste", json!({ "center": center })).is_ok(), "center={center}");
        }
    }

    #[test]
    fn copy_paste_between_documents() {
        // Copy in one tab, paste into another: the clipboard is shared across documents.
        let mut s = session(); // doc A (100x100): red rect at (10,10)-(50,30)
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 40, "height": 20})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        // Open a second document (a new tab), which becomes active.
        s.execute("file.new", json!({"width": 200, "height": 200})).unwrap();
        assert_eq!(s.active_index().unwrap(), 1, "the new document is the active tab");
        let before = s.active().unwrap().doc.layers.len();
        s.execute("edit.paste", json!({})).unwrap();
        assert_eq!(s.active_index().unwrap(), 1, "paste stays in the second tab");
        let d = &s.active().unwrap().doc;
        assert_eq!(d.layers.len(), before + 1, "pasted layer added to the second document");
        let pasted = d.layers.last().unwrap().surface().unwrap();
        let cb = pasted.content_bounds();
        let (cx, cy) = (cb.x0 + cb.width() as i32 / 2, cb.y0 + cb.height() as i32 / 2);
        let mut px = [[0.0f32; 4]; 1];
        pasted.read_rgba_into(Rect::new(cx, cy, cx + 1, cy + 1), &mut px);
        assert!(px[0][0] > 0.9 && px[0][1] < 0.1 && px[0][2] < 0.1, "pasted content is red: {:?}", px[0]);
    }

    #[test]
    fn paste_between_different_color_modes() {
        // Copy from an RGB document, paste into a Grayscale one: the clipboard converts to the
        // target document's format (a realistic between-tabs case that must not panic).
        let mut s = session(); // RGB, red rect
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 40, "height": 20})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        s.execute("file.new", json!({"width": 120, "height": 120, "mode": "gray"})).unwrap();
        s.execute("edit.paste", json!({})).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!(d.mode, photocraft_doc::ColorMode::Grayscale);
        let pasted = d.layers.last().unwrap().surface().unwrap();
        assert_eq!(pasted.format().mode, photocraft_color::ColorMode::Grayscale, "pasted layer is grayscale");
        assert!(!pasted.content_bounds().is_empty(), "pasted content exists");
    }

    #[test]
    fn transform_scale_grows_content() {
        let mut s = session(); // red rect (10,10)-(50,30): 40x20
        s.execute("edit.transform", json!({"matrix": [2, 0, 0, 2, 0, 0]})).unwrap();
        let d = &s.active().unwrap().doc;
        let cb = d.layers.last().unwrap().surface().unwrap().content_bounds();
        // Scaled ~2x about the origin (≈ 80x40 at ≈ (20,20)); allow ~1px bilinear edge bleed.
        assert!((cb.x0 - 20).abs() <= 2 && (cb.y0 - 20).abs() <= 2, "origin ~ (20,20): {cb:?}");
        assert!((cb.width() as i32 - 80).abs() <= 3 && (cb.height() as i32 - 40).abs() <= 3, "size ~ 80x40: {cb:?}");
    }

    #[test]
    fn cut_and_clear_on_the_background_fill_with_the_background_colour() {
        // The Background keeps its transparency lock: cutting fills with the background colour.
        let fresh = || {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
            s.tools.background = [1.0, 0.0, 0.0, 1.0];
            s.execute("select.rect", json!({"x": 5, "y": 5, "width": 10, "height": 10})).unwrap();
            s
        };
        let px = |s: &Session, i: usize, x: i32, y: i32| s.active().unwrap().doc.layers[i].surface().unwrap().pixel(x, y);
        for cmd in ["edit.cut", "edit.clear", "layer.new.layerViaCut"] {
            let mut s = fresh();
            s.execute(cmd, json!({})).unwrap();
            let d = &s.active().unwrap().doc;
            assert!(crate::extra_cmds::is_background(&d.layers[0]), "{cmd}: still the Background");
            assert_eq!(px(&s, 0, 8, 8), vec![1.0, 0.0, 0.0, 1.0], "{cmd}: cut area is the background colour");
            assert_eq!(px(&s, 0, 30, 20), vec![1.0, 1.0, 1.0, 1.0], "{cmd}: outside untouched");
        }
        // Layer via Cut still lifts the original pixels onto the new layer.
        let mut s = fresh();
        s.execute("layer.new.layerViaCut", json!({})).unwrap();
        assert_eq!(px(&s, 1, 8, 8), vec![1.0, 1.0, 1.0, 1.0]);
        // Once it's a normal layer, cutting makes transparency again.
        let mut s = fresh();
        s.execute("layer.new.layerFromBackground", json!({})).unwrap();
        s.execute("edit.clear", json!({})).unwrap();
        assert_eq!(px(&s, 0, 8, 8)[3], 0.0);
    }

    #[test]
    fn cut_clears_and_layer_via_copy_cut() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 20})).unwrap();
        s.execute("edit.cut", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(20, 10, 50, 30));
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 20})).unwrap();
        s.execute("layer.new.layerViaCut", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 20, 30));
        let d = &s.active().unwrap().doc;
        assert_eq!(d.layers[1].surface().unwrap().content_bounds(), Rect::new(20, 10, 50, 30));
        // Without a selection ⌘J duplicates the layer.
        let mut s = session();
        s.execute("layer.new.layerViaCopy", json!({})).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!(d.layers.len(), 3);
        assert!(d.layers[2].name.ends_with("copy"));
    }

    #[test]
    fn merge_visible_keeps_hidden_layers() {
        let mut s = session();
        s.execute("layer.new.layer", json!({})).unwrap();
        let hidden = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.setProps", json!({"layer": hidden.0, "visible": false})).unwrap();
        s.execute("layer.mergeVisible", json!({})).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!(d.layers.len(), 2);
        assert_eq!(d.layers[0].name, "Background");
        assert_eq!(d.layers[1].id, hidden);
        assert_eq!(d.layers[0].surface().unwrap().pixel(20, 20)[..3], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn auto_tone_stretches_a_low_contrast_layer() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.edit("grey ramp", |doc, _| {
            let surf = doc.layers[0].surface_mut().unwrap();
            for x in 0..64 {
                let v = 0.3 + 0.4 * x as f32 / 63.0;
                surf.fill_rect(Rect::new(x, 0, x + 1, 64), &[v, v * 0.9, v * 0.8, 1.0]);
            }
            Ok(())
        })
        .unwrap();
        for cmd in ["image.autoTone", "image.autoContrast", "image.autoColor"] {
            s.execute(cmd, json!({})).unwrap();
            let d = &s.active().unwrap().doc;
            let (a, b) = (d.layers[0].surface().unwrap().pixel(0, 0), d.layers[0].surface().unwrap().pixel(63, 0));
            // Auto Contrast keeps colour (shared points): only the extreme channels reach 0 and 1.
            let (lo, hi) = if cmd == "image.autoContrast" { (a[2], b[0]) } else { (a[0], b[0]) };
            assert!(lo < 0.05 && hi > 0.95, "{cmd}: {a:?} {b:?}");
            s.undo();
        }
    }

    #[test]
    fn toggle_last_state_and_transform_again() {
        let mut s = session();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 10, 0]})).unwrap();
        s.execute("edit.transform.again", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(30, 10, 70, 30));
        s.execute("edit.toggleLastState", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(20, 10, 60, 30));
        s.execute("edit.toggleLastState", json!({})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(30, 10, 70, 30));
    }

    #[test]
    fn guides_are_undoable() {
        let mut s = session();
        s.execute("view.newGuide", json!({"orientation": "vertical", "position": 40})).unwrap();
        s.execute("view.newGuide", json!({"orientation": "horizontal", "position": 12.5})).unwrap();
        s.execute("view.moveGuide", json!({"orientation": "vertical", "index": 0, "position": 60})).unwrap();
        let g = &s.active().unwrap().doc.guides;
        assert_eq!((g.vertical.clone(), g.horizontal.clone()), (vec![60.0], vec![12.5]));
        s.execute("view.deleteGuide", json!({"orientation": "horizontal", "index": 0})).unwrap();
        s.undo();
        assert_eq!(s.active().unwrap().doc.guides.horizontal, vec![12.5]);
        s.execute("view.clearGuides", json!({})).unwrap();
        assert!(s.active().unwrap().doc.guides.vertical.is_empty());
        assert!(s.execute("view.moveGuide", json!({"orientation": "vertical", "index": 3, "position": 1})).is_err());
    }
}

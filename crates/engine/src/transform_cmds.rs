//! Edit › Free Transform / Transform: projective transforms of layers (and the selection).

use photocraft_algo::transform::{Homography, Interp, warp_surface};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_geom::{Affine, Rect};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    s.active().and_then(|d| d.active_layer).map(|_| ()).ok_or_else(|| "no active layer".into())
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: "edit.transform".into(), msg: msg.into() }
}

/// Document-space bounds a transform of `layer` starts from (what Free Transform frames).
/// Content scans are cached per tile: snapping asks for every layer's bounds per Move drag.
pub fn transform_bounds(doc: &Document, layer: &Layer) -> Rect {
    let content = match &layer.content {
        LayerContent::Group(g) => g.children.iter().map(|l| transform_bounds(doc, l)).fold(Rect::EMPTY, |a, b| a.union(&b)),
        _ => layer.surface().map_or(Rect::EMPTY, photocraft_compose::bounds::content_bounds),
    };
    let content =
        if content.is_empty() { layer.mask.as_ref().map_or(Rect::EMPTY, |m| photocraft_compose::bounds::content_bounds(&m.surface)) } else { content };
    match &doc.selection {
        Some(sel) if !layer.is_group() => content.intersect(&sel.content_bounds()),
        _ => content,
    }
}

/// Warp a surface whose pixels outside the content read as `default` (masks, selections):
/// warp the content with alpha, then flatten back onto the default value.
pub(crate) fn warp_gray(s: &Surface, h: &Homography, interp: Interp) -> Surface {
    let default = s.default_pixel().first().copied().unwrap_or(0.0);
    let fmt = s.format();
    let src = s.content_bounds();
    let mut out = Surface::with_default(fmt, &[default]);
    if src.is_empty() {
        return out;
    }
    // Content-only copy (default 0 + alpha) so the "outside" is transparent during the warp.
    let with_alpha = PixelFormat::new(fmt.mode, fmt.sample, true);
    let mut tmp = Surface::new(with_alpha);
    let v = s.read_region(src);
    tmp.write_region(src, &v.iter().flat_map(|g| [*g, 1.0]).collect::<Vec<f32>>());
    let w = warp_surface(&tmp, src, h, interp);
    // Clear the old content region, then composite the warped content over it. These are written as
    // two sparse regions rather than one dense `src ∪ warped` block: a transform that moves the
    // content far away (e.g. a huge translation) would otherwise allocate a buffer spanning both and
    // hang/OOM. `Surface` is tile-sparse, so distant regions cost only their own tiles.
    let old: Vec<f32> = vec![default; src.width() as usize * src.height() as usize];
    out.write_region(src, &old);
    let b = w.content_bounds();
    if !b.is_empty() {
        let px = w.read_region(b);
        let flat: Vec<f32> = px.as_chunks::<2>().0.iter().map(|p| p[0] * p[1] + default * (1.0 - p[1])).collect();
        out.write_region(b, &flat);
    }
    out.prune();
    out
}

/// Split a single-channel surface whose outside reads as its default (a mask, a channel) by a
/// selection: (the selected values as grey + alpha = selection, the surface with the selected
/// values replaced by the default). `None` for surfaces that aren't a single grey channel.
pub fn split_gray_selected(s: &Surface, sel: &Surface) -> Option<(Surface, Surface)> {
    let fmt = s.format();
    if fmt.alpha || fmt.channels() != 1 {
        return None;
    }
    let default = s.default_pixel().first().copied().unwrap_or(0.0);
    let mut rest = s.clone();
    let mut lifted = Surface::new(PixelFormat::new(fmt.mode, fmt.sample, true));
    // Everything selected moves, including default-valued parts outside the painted content, so
    // the moved region stays aligned with the moved pixels.
    let area = sel.content_bounds();
    if area.is_empty() {
        return Some((lifted, rest));
    }
    let v = s.read_region(area);
    let w = area.width() as usize;
    let mut lp = Vec::with_capacity(v.len() * 2);
    let mut rp = Vec::with_capacity(v.len());
    for (i, g) in v.iter().enumerate() {
        let (x, y) = (area.x0 + (i % w) as i32, area.y0 + (i / w) as i32);
        let k = sel.sample_channel(x, y, 0);
        lp.extend([*g, k]);
        rp.push(g * (1.0 - k) + default * k);
    }
    lifted.write_region(area, &lp);
    lifted.prune();
    rest.write_region(area, &rp);
    rest.prune();
    Some((lifted, rest))
}

/// [`warp_gray`] limited to a selection: only the selected values move (the vacated area reads as
/// the default), as the selected pixels of the layer do.
pub(crate) fn warp_gray_selected(s: &Surface, sel: &Surface, h: &Homography, interp: Interp) -> Surface {
    let Some((lifted, mut out)) = split_gray_selected(s, sel) else { return warp_gray(s, h, interp) };
    let src = lifted.content_bounds();
    if src.is_empty() {
        return out;
    }
    let w = warp_surface(&lifted, src, h, interp);
    let b = w.content_bounds();
    if !b.is_empty() {
        let moved = w.read_region(b);
        let under = out.read_region(b);
        let flat: Vec<f32> = moved.as_chunks::<2>().0.iter().zip(&under).map(|(p, u)| p[0] * p[1] + u * (1.0 - p[1])).collect();
        out.write_region(b, &flat);
    }
    out.prune();
    out
}

/// The surface Free Transform moves by itself when the params target one (`"target"`): an
/// unlinked layer mask, an alpha channel or the Quick Mask. `None` means the layer, with its
/// linked masks (a targeted *linked* mask moves together with its layer, as in Photoshop).
pub fn lone_target<'a>(doc: &'a Document, layer: Option<LayerId>, p: &Value) -> Result<Option<&'a Surface>> {
    use crate::channel_cmds::Target;
    match crate::channel_cmds::target_of(p) {
        Target::Pixels => Ok(None),
        Target::Mask => {
            let id = layer.ok_or_else(|| EngineError::Other("no active layer".into()))?;
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let m = l.mask.as_ref().ok_or_else(|| EngineError::Other("layer has no mask".into()))?;
            Ok((!m.linked).then_some(&m.surface))
        }
        Target::Alpha(i) => doc
            .channels
            .get(i)
            .map(|c| Some(&c.surface))
            .ok_or_else(|| EngineError::Other(format!("no alpha channel {i} (document has {})", doc.channels.len()))),
        Target::QuickMask => doc.quick_mask.as_ref().map(|c| Some(&c.surface)).ok_or_else(|| EngineError::Other("not in Quick Mask mode".into())),
    }
}

/// [`lone_target`], mutably.
pub fn lone_target_mut<'a>(doc: &'a mut Document, layer: Option<LayerId>, p: &Value) -> Result<Option<&'a mut Surface>> {
    if lone_target(doc, layer, p)?.is_none() {
        return Ok(None);
    }
    crate::channel_cmds::target_surface(doc, layer, p).map(|(s, _)| Some(s))
}

/// The frame Free Transform starts from on a lone target: its painted content (the whole canvas
/// when nothing is painted), within the selection.
pub fn target_bounds(doc: &Document, surf: &Surface) -> Rect {
    let content = surf.content_bounds();
    let content = if content.is_empty() { doc.bounds() } else { content };
    match &doc.selection {
        Some(sel) => content.intersect(&sel.content_bounds()),
        None => content,
    }
}

pub(crate) fn transform_layer(doc_sel: Option<&Surface>, l: &mut Layer, h: &Homography, affine: Option<Affine>, interp: Interp) -> Result<()> {
    // Photoshop turns the Background into a normal layer before transforming it.
    if l.locks.position && l.name == "Background" {
        l.locks.position = false;
        l.locks.transparency = false;
        l.name = "Layer 0".into();
    }
    if l.locks.position || l.locks.all {
        return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
    }
    // With a selection only the selected pixels move, and so only the same region of a linked
    // mask (#205). Groups, type, shapes and smart objects move whole.
    let mask_sel = doc_sel.filter(|_| !matches!(l.content, LayerContent::Group(_) | LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_)));
    match &mut l.content {
        LayerContent::Group(g) => {
            for c in g.children.iter_mut() {
                transform_layer(None, c, h, affine, interp)?;
            }
        }
        LayerContent::Text(t) => {
            let Some(a) = affine else {
                return Err(EngineError::Other("Distort and Perspective need rasterized type (Type › Rasterize Type Layer)".into()));
            };
            t.transform = a.mul(&t.transform);
        }
        LayerContent::Shape(sh) => {
            let Some(a) = affine else {
                return Err(EngineError::Other("Distort and Perspective need a rasterized shape (Layer › Rasterize › Shape)".into()));
            };
            crate::vector_cmds::transform_shape(sh, &a);
        }
        LayerContent::Smart(sm) => {
            // Smart objects keep the transform and re-render from their source afterwards
            // (`refresh_text`), so repeated transforms don't degrade the pixels.
            let Some(a) = affine else {
                return Err(EngineError::Other("Distort and Perspective on smart objects aren't supported yet".into()));
            };
            sm.transform = crate::smart_cmds::snap_affine(a.mul(&sm.transform));
            // Fallback appearance for sources that can't be re-rendered.
            if let Some(c) = &mut sm.cache {
                let src = c.content_bounds();
                *c = warp_surface(c, src, h, interp);
            }
            if let Some(m) = &mut sm.filter_mask {
                m.surface = warp_gray(&m.surface, h, interp);
            }
        }
        _ => {
            if let Some(surf) = l.surface_mut() {
                let src = surf.content_bounds();
                *surf = match doc_sel {
                    // Only the selected pixels move: lift them, clear them, warp and paste back.
                    Some(sel) => {
                        let (lifted, mut rest) = split_selected(surf, sel);
                        let moved = warp_surface(&lifted, src, h, interp);
                        composite_over(&mut rest, &moved);
                        rest.prune();
                        rest
                    }
                    None => warp_surface(surf, src, h, interp),
                };
            }
        }
    }
    if let Some(m) = l.mask.as_mut()
        && m.linked
    {
        m.surface = match mask_sel {
            Some(sel) => warp_gray_selected(&m.surface, sel, h, interp),
            None => warp_gray(&m.surface, h, interp),
        };
    }
    if let Some(vm) = l.vector_mask.as_mut()
        && vm.linked
        && let Some(a) = affine
    {
        vm.path = vm.path.transform(&a);
    }
    Ok(())
}

/// Split a layer surface by a selection: (selected pixels, everything else), both with alpha.
pub fn split_selected(surf: &Surface, sel: &Surface) -> (Surface, Surface) {
    let fmt = surf.format();
    let with_alpha = PixelFormat::new(fmt.mode, fmt.sample, true);
    let mut lifted = Surface::new(with_alpha);
    let mut rest = surf.convert(with_alpha);
    let src = surf.content_bounds();
    if src.is_empty() {
        return (lifted, rest);
    }
    let n = with_alpha.channels();
    let px = rest.read_region(src);
    let mut lp = px.clone();
    let mut rp = px;
    let w = src.width() as usize;
    for (i, (l, r)) in lp.chunks_exact_mut(n).zip(rp.chunks_exact_mut(n)).enumerate() {
        let (x, y) = (src.x0 + (i % w) as i32, src.y0 + (i / w) as i32);
        let k = sel.sample_channel(x, y, 0);
        l[n - 1] *= k;
        r[n - 1] *= 1.0 - k;
    }
    lifted.write_region(src, &lp);
    lifted.prune();
    rest.write_region(src, &rp);
    (lifted, rest)
}

pub(crate) fn refresh_text(doc: &Document, l: &mut Layer) {
    match &mut l.content {
        LayerContent::Text(t) => crate::type_cmds::refresh(doc, t),
        LayerContent::Shape(sh) => crate::vector_cmds::refresh_shape(doc, sh),
        LayerContent::Smart(_) => {
            // Unavailable sources keep the warped cache.
            let _ = crate::smart_cmds::refresh_layer(doc, l);
        }
        LayerContent::Group(g) => g.children.iter_mut().for_each(|c| refresh_text(doc, c)),
        _ => {}
    }
}

/// Normal-blend `top` over `dst` (both straight alpha, same format).
pub(crate) fn composite_over(dst: &mut Surface, top: &Surface) {
    let b = top.content_bounds();
    if b.is_empty() {
        return;
    }
    let n = dst.format().channels();
    let a = n - 1;
    let t = top.read_region(b);
    let mut d = dst.read_region(b);
    for (dp, tp) in d.chunks_exact_mut(n).zip(t.chunks_exact(n)) {
        let (ta, da) = (tp[a], dp[a]);
        let oa = ta + da * (1.0 - ta);
        if oa > 0.0 {
            for c in 0..a {
                dp[c] = (tp[c] * ta + dp[c] * da * (1.0 - ta)) / oa;
            }
        }
        dp[a] = oa;
    }
    dst.write_region(b, &d);
}

fn quad_param(p: &Value) -> Option<[[f64; 2]; 4]> {
    let a = p.get("quad")?.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let mut q = [[0.0; 2]; 4];
    for (i, v) in a.iter().enumerate() {
        let c = v.as_array()?;
        q[i] = [c.first()?.as_f64()?, c.get(1)?.as_f64()?];
    }
    Some(q)
}

fn transform(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let id = match p.get("layer").and_then(Value::as_u64) {
        Some(v) => Some(LayerId(v)),
        None => st.active_layer,
    };
    let lone = lone_target(&st.doc, id, p)?;
    let id = match (id, lone.is_some() && crate::channel_cmds::is_channel_target(p)) {
        (_, true) => None,
        (Some(id), false) => Some(id),
        (None, false) => return Err(EngineError::Other("no active layer".into())),
    };
    let rect = match p.get("rect").and_then(Value::as_array) {
        Some(r) if r.len() == 4 => {
            let v: Vec<f64> = r.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
            [v[0], v[1], v[2], v[3]]
        }
        _ => {
            let b = match (lone, id) {
                (Some(surf), _) => target_bounds(&st.doc, surf),
                (None, Some(id)) => transform_bounds(&st.doc, st.doc.layer(id).ok_or(EngineError::NoLayer(id))?),
                (None, None) => Rect::EMPTY,
            };
            if b.is_empty() {
                return Err(EngineError::Other("nothing to transform".into()));
            }
            [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64]
        }
    };
    let h = if let Some(q) = quad_param(p) {
        Homography::rect_to_quad(rect, q).ok_or_else(|| bad("degenerate quad"))?
    } else if let Some(m) = p.get("matrix").and_then(Value::as_array) {
        let v: Vec<f64> = m.iter().filter_map(Value::as_f64).collect();
        if v.len() != 6 {
            return Err(bad("matrix must be [a, b, c, d, e, f]"));
        }
        // Affine [a b c d e f] maps (x, y) → (a·x + c·y + e, b·x + d·y + f).
        Homography([v[0], v[2], v[4], v[1], v[3], v[5], 0.0, 0.0, 1.0])
    } else {
        return Err(bad("pass `quad` (where the rect's corners go) or `matrix`"));
    };
    if h.inverse().is_none() {
        return Err(bad("the transform collapses the layer"));
    }
    let m = h.0;
    let affine =
        (m[6].abs() < 1e-12 && m[7].abs() < 1e-12).then(|| Affine { m: [m[0] / m[8], m[3] / m[8], m[1] / m[8], m[4] / m[8], m[2] / m[8], m[5] / m[8]] });
    let interp = Interp::parse(p.get("interpolation").and_then(Value::as_str).unwrap_or("bicubic"));
    let lone = lone.is_some();
    s.edit("Free Transform", |doc, _| {
        let sel = doc.selection.clone();
        if lone {
            // A targeted unlinked mask, alpha channel or Quick Mask transforms by itself.
            let (surf, _) = crate::channel_cmds::target_surface(doc, id, p)?;
            *surf = match &sel {
                Some(sel) => warp_gray_selected(surf, sel, &h, interp),
                None => warp_gray(surf, &h, interp),
            };
            if let Some(sel) = &doc.selection {
                doc.selection = Some(warp_gray(sel, &h, Interp::Bilinear)).filter(|s| !s.content_bounds().is_empty());
            }
            return Ok(());
        }
        let id = id.ok_or_else(|| EngineError::Other("no active layer".into()))?;
        let is_group = doc.layer(id).is_some_and(Layer::is_group);
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        transform_layer(if is_group { None } else { sel.as_ref() }, l, &h, affine, interp)?;
        // Type layers re-render from their new transform.
        let snapshot = doc.clone();
        if let Some(l) = doc.layer_mut(id) {
            refresh_text(&snapshot, l);
        }
        // The selection outline moves with the pixels.
        if let Some(sel) = &doc.selection {
            doc.selection = Some(warp_gray(sel, &h, Interp::Bilinear)).filter(|s| !s.content_bounds().is_empty());
        }
        Ok(())
    })?;
    Ok(json!({"rect": rect}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "edit.transform",
        label: "Free Transform",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id?,"rect":[x0,y0,x1,y1]? (source frame; default = layer content ∩ selection),"quad":[[x,y]×4]? (where the frame's corners go, clockwise from top-left),"matrix":[a,b,c,d,e,f]? (affine alternative),"interpolation":"bicubic|bilinear|nearest"="bicubic","target":"pixels"|"mask"|"quickMask"|{"channel":i}=Channels panel target (an unlinked mask, an alpha channel or the Quick Mask transforms alone; a linked mask moves with its layer)}"##,
        enabled: has_layer,
        journal: true,
        run: transform,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("paint", |doc, active| {
            let l = doc.layer_mut(active.unwrap()).unwrap();
            l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 30, 20), &[1.0, 0.0, 0.0, 1.0]);
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
    fn scale_via_quad_and_undo() {
        let mut s = session();
        s.execute("edit.transform", json!({"quad": [[10, 10], [50, 10], [50, 30], [10, 30]]})).unwrap();
        let b = active_bounds(&s);
        assert!(b.width().abs_diff(40) <= 2 && b.height().abs_diff(20) <= 2, "{b:?}");
        s.undo();
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 30, 20));
    }

    #[test]
    fn affine_matrix_and_perspective() {
        let mut s = session();
        // Translate by (40, 50).
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50]})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(50, 60, 70, 70));
        // Perspective: top edge narrower than the bottom.
        s.execute("edit.transform", json!({"quad": [[55, 60], [65, 60], [75, 70], [45, 70]]})).unwrap();
        let b = active_bounds(&s);
        assert!(b.x0 <= 46 && b.x1 >= 74, "{b:?}");
        assert!(s.execute("edit.transform", json!({"quad": [[0, 0], [0, 0], [0, 0], [0, 0]]})).is_err());
    }

    #[test]
    fn background_becomes_layer_0() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
        s.execute("edit.transform", json!({"quad": [[0, 0], [20, 0], [20, 20], [0, 20]]})).unwrap();
        let st = s.active().unwrap();
        let l = &st.doc.layers[0];
        assert_eq!(l.name, "Layer 0");
        assert!(l.surface().unwrap().format().alpha);
        assert_eq!(l.surface().unwrap().pixel(30, 30)[3], 0.0, "revealed area is transparent");
    }

    #[test]
    fn with_selection_only_selected_pixels_move() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 0, 50]})).unwrap();
        let st = s.active().unwrap();
        let surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        assert_eq!(surf.pixel(15, 15)[3], 0.0, "lifted area is cleared");
        assert_eq!(surf.pixel(25, 15)[3], 1.0, "unselected part stays");
        assert_eq!(surf.pixel(15, 65)[3], 1.0, "moved pixels land");
        let sel = st.doc.selection.as_ref().unwrap().content_bounds();
        assert_eq!((sel.y0, sel.y1), (60, 70), "selection moves too");
    }

    #[test]
    fn type_layers_transform_as_vectors() {
        let mut s = session();
        let r = s.execute("type.create", json!({"x": 10, "y": 50, "text": "Hi", "size": 20})).unwrap();
        let id = r["layer"].as_u64().unwrap();
        let w0 = active_bounds(&s).width();
        s.execute("edit.transform", json!({"layer": id, "matrix": [2, 0, 0, 2, -10, -50]})).unwrap();
        let w1 = active_bounds(&s).width();
        assert!(w1 as f32 > w0 as f32 * 1.8, "{w0} → {w1}");
        let st = s.active().unwrap();
        let LayerContent::Text(t) = &st.doc.layer(LayerId(id)).unwrap().content else { panic!() };
        assert!((t.transform.m[0] - 2.0).abs() < 1e-9);
        // Perspective on type is refused, like Photoshop.
        assert!(s.execute("edit.transform", json!({"layer": id, "quad": [[0, 0], [10, 0], [12, 10], [-2, 10]]})).is_err());
    }

    #[test]
    fn shapes_move_and_transform_as_vectors() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 200})).unwrap();
        s.execute("shape.create", json!({"kind": "rect", "rect": [10, 10, 40, 20], "fill": "#ff0000"})).unwrap();
        s.execute("layer.translate", json!({"dx": 30, "dy": 5})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(40, 15, 80, 35));
        s.execute("edit.transform", json!({"matrix": [2, 0, 0, 2, -40, -15]})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(40, 15, 120, 55));
        assert!(s.execute("edit.transform", json!({"quad": [[40, 15], [120, 15], [130, 55], [30, 55]]})).is_err());
    }

    #[test]
    fn type_moves_with_the_move_command() {
        let mut s = session();
        s.execute("type.create", json!({"x": 10, "y": 50, "text": "Hi", "size": 20})).unwrap();
        let b0 = active_bounds(&s);
        s.execute("layer.translate", json!({"dx": 20, "dy": 10})).unwrap();
        let b1 = active_bounds(&s);
        assert_eq!((b1.x0 - b0.x0, b1.y0 - b0.y0), (20, 10));
    }

    // ---- Layer masks and targets (#205) ----

    /// `session()` plus a reveal-all mask with `hide` painted black (linked unless `linked` is false).
    fn masked(hide: &[Rect], linked: bool) -> Session {
        let mut s = session();
        s.execute("layer.layerMask.revealAll", json!({})).unwrap();
        s.edit("mask", |doc, active| {
            let m = doc.layer_mut(active.unwrap()).unwrap().mask.as_mut().unwrap();
            for r in hide {
                m.surface.fill_rect(*r, &[0.0]);
            }
            m.linked = linked;
            Ok(())
        })
        .unwrap();
        s
    }

    fn mask_at(s: &Session, x: i32, y: i32) -> f32 {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().mask.as_ref().unwrap().surface.sample_channel(x, y, 0)
    }

    fn alpha_at(s: &Session, x: i32, y: i32) -> f32 {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)[3]
    }

    #[test]
    fn linked_raster_mask_moves_with_the_pixels() {
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], true);
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50]})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(50, 60, 70, 70));
        assert!(mask_at(&s, 55, 65) < 0.01, "the hidden half moved with the pixels");
        assert!(mask_at(&s, 65, 65) > 0.99);
        assert!(mask_at(&s, 15, 15) > 0.99, "nothing hidden left behind");
    }

    #[test]
    fn linked_mask_with_a_selection_moves_only_the_selected_region() {
        // The selection covers the left half of the red rect; the mask hides its left quarter
        // (selected) and its right quarter (not selected).
        let mut s = masked(&[Rect::new(10, 10, 15, 20), Rect::new(25, 10, 30, 20)], true);
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 0, 50]})).unwrap();
        // Moved pixels and their mask line up.
        assert_eq!(alpha_at(&s, 12, 65), 1.0);
        assert!(mask_at(&s, 12, 65) < 0.01, "selected hidden quarter moved");
        assert!(mask_at(&s, 17, 65) > 0.99, "selected visible quarter moved");
        // The vacated area reads as revealed (default), with transparent pixels.
        assert_eq!(alpha_at(&s, 12, 15), 0.0);
        assert!(mask_at(&s, 12, 15) > 0.99);
        // The unselected part of the mask stays, aligned with the unselected pixels.
        assert_eq!(alpha_at(&s, 27, 15), 1.0);
        assert!(mask_at(&s, 27, 15) < 0.01, "unselected mask stays put");
        assert!(mask_at(&s, 27, 65) > 0.99, "and isn't copied along");
        // Every pixel's mask value is where it was relative to the pixel.
        for (x, y) in [(11, 61), (14, 69), (15, 61), (19, 69)] {
            let want = if x < 15 { 0.0 } else { 1.0 };
            assert!((mask_at(&s, x, y) - want).abs() < 0.01, "({x},{y})");
        }
    }

    #[test]
    fn unlinked_mask_stays_put() {
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], false);
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50]})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(50, 60, 70, 70));
        assert!(mask_at(&s, 15, 15) < 0.01);
        assert!(mask_at(&s, 55, 65) > 0.99);
        // With a selection too.
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], false);
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 0, 50]})).unwrap();
        assert!(mask_at(&s, 15, 15) < 0.01 && mask_at(&s, 15, 65) > 0.99);
    }

    #[test]
    fn targeted_unlinked_mask_transforms_alone() {
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], false);
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50], "target": "mask"})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 30, 20), "pixels untouched");
        assert!(mask_at(&s, 55, 65) < 0.01, "mask moved");
        assert!(mask_at(&s, 15, 15) > 0.99, "vacated area reads as the default");
        s.undo();
        assert!(mask_at(&s, 15, 15) < 0.01, "one undo step");
        // With a selection, only its part of the mask.
        s.execute("select.rect", json!({"x": 10, "y": 10, "width": 5, "height": 10})).unwrap();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 0, 50], "target": "mask"})).unwrap();
        assert!(mask_at(&s, 12, 65) < 0.01 && mask_at(&s, 12, 15) > 0.99);
        assert!(mask_at(&s, 17, 15) < 0.01, "unselected mask stays");
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 30, 20), "pixels untouched");
    }

    #[test]
    fn targeted_linked_mask_moves_with_its_layer() {
        // Photoshop: a linked mask can't move without its layer.
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], true);
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50], "target": "mask"})).unwrap();
        assert_eq!(active_bounds(&s), Rect::new(50, 60, 70, 70));
        assert!(mask_at(&s, 55, 65) < 0.01);
    }

    #[test]
    fn targeted_alpha_channel_transforms_alone() {
        let mut s = session();
        s.execute("channel.new", json!({})).unwrap();
        s.edit("paint channel", |doc, _| {
            doc.channels[0].surface.fill_rect(Rect::new(10, 10, 20, 20), &[1.0]);
            Ok(())
        })
        .unwrap();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 40, 50], "target": {"channel": 0}})).unwrap();
        let st = s.active().unwrap();
        let ch = &st.doc.channels[0].surface;
        assert!(ch.sample_channel(55, 65, 0) > 0.99 && ch.sample_channel(15, 15, 0) < 0.01);
        assert_eq!(active_bounds(&s), Rect::new(10, 10, 30, 20), "layer untouched");
    }

    #[test]
    fn transform_targets_fail_gracefully() {
        let mut s = session();
        let m = json!([1, 0, 0, 1, 5, 5]);
        assert!(s.execute("edit.transform", json!({"matrix": m, "target": "mask"})).is_err(), "no mask");
        assert!(s.execute("edit.transform", json!({"matrix": m, "target": {"channel": 9}})).is_err());
        assert!(s.execute("edit.transform", json!({"matrix": m, "target": "quickMask"})).is_err());
        // Unknown target shapes fall back to the pixels.
        s.execute("edit.transform", json!({"matrix": m, "target": 7})).unwrap();
        s.execute("edit.transform", json!({"matrix": m, "target": {"channel": "x"}})).unwrap();
        // Selections that miss the mask entirely transform nothing, without panicking.
        let mut s = masked(&[Rect::new(10, 10, 20, 20)], false);
        s.execute("select.rect", json!({"x": 80, "y": 80, "width": 5, "height": 5})).unwrap();
        let _ = s.execute("edit.transform", json!({"matrix": [1e9, 0, 0, 1e9, 0, 0], "target": "mask"}));
        let _ = s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 1e12, 0], "target": "mask"}));
    }

    #[test]
    fn split_gray_selected_rejects_non_gray() {
        let s = Surface::new(PixelFormat::RGBA8);
        let mut sel = Surface::new(PixelFormat::GRAY8);
        sel.fill_rect(Rect::new(0, 0, 4, 4), &[1.0]);
        assert!(split_gray_selected(&s, &sel).is_none());
    }
}

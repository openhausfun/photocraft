//! Edit › Auto-Align Layers and Auto-Blend Layers, on the selected pixel layers.
//!
//! Both share Photomerge's registration and blending ([`photocraft_algo::panorama`] via
//! `photo_cmds`). Auto-Align matches Harris / steered-BRIEF features between every pair of
//! layers, chains the verified pairs from the reference layer (maximum spanning tree) and
//! bundle-adjusts all of them, then warps each layer. Projections: Auto, Perspective
//! (homography), Cylindrical and Spherical (rigid motion on the cylinder / sphere, focal length
//! from the homographies or EXIF), Collage (similarity) and Reposition (translation). The
//! reference stays put in the planar projections.
//!
//! Auto-Blend gives each selected layer a mask so the composite shows, per pixel, the sharpest
//! layer (Stack Images, for focus stacking) or one layer per region with seams routed where the
//! layers agree (Panorama). With Seamless Tones and Colors it also adds a merged layer blended
//! with Laplacian pyramids so transitions are invisible.

use photocraft_algo::panorama::Layout;
use photocraft_algo::pyramid;
use photocraft_algo::transform::Interp;
use photocraft_doc::{Document, Layer, LayerContent, LayerId, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, to_rgba};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::photo_cmds::{plane_matrix, register, seam_blend, seam_order, warp_placed};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// The selected raster layers, bottom to top.
fn selected_pixel_layers(s: &Session) -> Vec<LayerId> {
    let Some(st) = s.active() else { return Vec::new() };
    st.selected_layers().into_iter().filter(|id| st.doc.layer(*id).is_some_and(|l| matches!(l.content, LayerContent::Raster(_)))).collect()
}

fn two_layers(s: &Session) -> std::result::Result<(), String> {
    s.active().ok_or("no document open")?;
    if selected_pixel_layers(s).len() < 2 { Err("select two or more pixel layers in the Layers panel".into()) } else { Ok(()) }
}

fn auto_align(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.autoAlignLayers";
    let ids = selected_pixel_layers(s);
    if ids.len() < 2 {
        return Err(bad(cmd, "select two or more pixel layers"));
    }
    let projection = p.get("projection").and_then(Value::as_str).unwrap_or("auto");
    let layout = Layout::parse(projection)
        .ok_or_else(|| bad(cmd, format!("unknown projection `{projection}` (auto|perspective|cylindrical|spherical|collage|reposition)")))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.clone();
    let reference = match p.get("reference").and_then(Value::as_u64) {
        Some(r) if ids.contains(&LayerId(r)) => LayerId(r),
        Some(r) => return Err(bad(cmd, format!("reference {r} is not a selected pixel layer"))),
        None => ids[0],
    };
    let ref_idx = ids.iter().position(|i| *i == reference).unwrap_or(0);
    let area = doc.bounds();
    let surfs: Vec<&Surface> = ids.iter().map(|id| doc.layer(*id).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
    let images: Vec<(&Surface, Rect)> = surfs.iter().map(|s| (*s, area)).collect();
    let focal35 = doc.metadata.exif.as_ref().and_then(|e| photocraft_algo::exif::read(e).focal_length_35mm);
    let geometric = p.get("geometricCorrection").and_then(Value::as_bool).unwrap_or(false);
    let al = register(&images, layout, Some(ref_idx), geometric, focal35)
        .ok_or_else(|| EngineError::Other("Auto-Align couldn't find enough matching detail between the layers".into()))?;
    let interp = Interp::parse(p.get("interpolation").and_then(Value::as_str).unwrap_or("bicubic"));
    let planar = matches!(al.layout, Layout::Perspective | Layout::Collage | Layout::Reposition);
    let mut inliers = vec![0usize; ids.len()];
    for &(a, b, n) in &al.pairs {
        inliers[a] += n;
        inliers[b] += n;
    }
    // Everything but the reference moves in planar layouts; every layer is reprojected otherwise.
    let moves: Vec<usize> = (0..ids.len()).filter(|&i| al.placements[i].is_some() && (i != ref_idx || !planar)).collect();
    let failed: Vec<u64> = (0..ids.len()).filter(|&i| al.placements[i].is_none()).map(|i| ids[i].0).collect();
    if moves.is_empty() {
        return Err(EngineError::Other("Auto-Align couldn't find enough matching detail between the layers".into()));
    }
    let warped: Vec<(LayerId, Surface, Option<Surface>)> = moves
        .iter()
        .map(|&i| {
            let pl = al.placements[i].as_ref().ok_or_else(|| EngineError::Other("Auto-Align lost a layer's placement".into()))?;
            let l = doc.layer(ids[i]).ok_or(EngineError::NoLayer(ids[i]))?;
            let surf = l.surface().ok_or(EngineError::NoLayer(ids[i]))?;
            let b = surf.content_bounds();
            let px = if b.is_empty() { surf.clone() } else { warp_placed(surf, b, (0.0, 0.0), pl, (0.0, 0.0), interp) };
            let mask = l.mask.as_ref().map(|m| {
                let mb = m.surface.content_bounds();
                if mb.is_empty() { m.surface.clone() } else { warp_placed(&m.surface, mb, (0.0, 0.0), pl, (0.0, 0.0), Interp::Bilinear) }
            });
            Ok((ids[i], px, mask))
        })
        .collect::<Result<_>>()?;
    s.edit("Auto-Align Layers", |doc, _| {
        for (id, px, mask) in &warped {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            if let Some(surf) = l.surface_mut() {
                *surf = px.clone();
            }
            if let (Some(m), Some(ms)) = (&mut l.mask, mask) {
                m.surface = ms.clone();
            }
        }
        Ok(())
    })?;
    let model = match al.layout {
        Layout::Reposition => "reposition",
        Layout::Collage => "collage",
        Layout::Perspective | Layout::Auto => "perspective",
        Layout::Cylindrical => "cylindrical",
        Layout::Spherical => "spherical",
    };
    let aligned: Vec<Value> = moves
        .iter()
        .filter_map(|&i| {
            let pl = al.placements[i].as_ref()?;
            Some(json!({"layer": ids[i].0, "model": model, "matches": inliers[i], "matrix": plane_matrix(pl, (0.0, 0.0)).map(|h| h.0.to_vec())}))
        })
        .collect();
    Ok(json!({"reference": reference.0, "layout": al.layout.name(), "focal": al.focal, "rms": al.rms, "aligned": aligned, "failed": failed}))
}

/// Native-channel pixels of a layer over `area` (transparent outside its content).
fn pixels(l: &Layer, area: Rect) -> Option<(Vec<f32>, photocraft_color::PixelFormat)> {
    let s = l.surface()?;
    Some((s.read_region(area), s.format()))
}

fn auto_blend(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.autoBlendLayers";
    let ids = selected_pixel_layers(s);
    if ids.len() < 2 {
        return Err(bad(cmd, "select two or more pixel layers"));
    }
    let method = p.get("method").and_then(Value::as_str).unwrap_or("panorama");
    if !matches!(method, "panorama" | "stack") {
        return Err(bad(cmd, "`method` must be panorama|stack"));
    }
    let seamless = p.get("seamlessTones").and_then(Value::as_bool).unwrap_or(true);
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc: std::sync::Arc<Document> = st.doc.clone();
    let mut area = Rect::EMPTY;
    for id in &ids {
        if let Some(b) = doc.layer(*id).and_then(|l| l.surface()).map(|s| s.content_bounds()) {
            area = area.union(&b);
        }
    }
    let area = area.intersect(&doc.bounds());
    if area.is_empty() {
        return Err(EngineError::Other("the selected layers are empty".into()));
    }
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut imgs: Vec<Vec<f32>> = Vec::new();
    let mut luma: Vec<Vec<f32>> = Vec::new();
    let mut alpha: Vec<Vec<f32>> = Vec::new();
    let mut fmt = None;
    for id in &ids {
        let l = doc.layer(*id).ok_or(EngineError::NoLayer(*id))?;
        let (px, f) = pixels(l, area).ok_or(EngineError::NoLayer(*id))?;
        let n = f.channels();
        let (mut lv, mut av) = (vec![0.0f32; w * h], vec![0.0f32; w * h]);
        for i in 0..w * h {
            let c = to_rgba(&f, &px[i * n..(i + 1) * n]);
            lv[i] = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            av[i] = c[3];
        }
        if fmt.is_some_and(|g: photocraft_color::PixelFormat| g != f) {
            return Err(EngineError::Other("the layers have different pixel formats".into()));
        }
        fmt = Some(f);
        imgs.push(px);
        luma.push(lv);
        alpha.push(av);
    }
    let fmt = fmt.ok_or(EngineError::NoDocument)?;
    let (weights, blended) = if method == "stack" {
        let weights = pyramid::stack_weights(w, h, &luma, &alpha);
        let blended = seamless.then(|| {
            let refs: Vec<&[f32]> = imgs.iter().map(Vec::as_slice).collect();
            pyramid::blend(w, h, fmt.channels(), &refs, &weights, pyramid::auto_levels(w, h))
        });
        (weights, blended)
    } else {
        // Panorama: seams routed through agreement, multi-band blended (photo_cmds).
        let surfs: Vec<Surface> =
            ids.iter().map(|id| doc.layer(*id).and_then(|l| l.surface()).cloned().ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
        let order = seam_order(&surfs, 0);
        let b = seam_blend(&surfs, area, &order, seamless, false, None);
        let weights: Vec<Vec<f32>> = b.layers.iter().map(|(_, m)| m.read_region(area)).collect();
        let blended = seamless.then(|| b.composite.read_region(area));
        (weights, blended)
    };
    let top = *ids.last().ok_or_else(|| EngineError::Other("Auto-Blend needs two or more layers".into()))?;
    let label = "Auto-Blend Layers";
    let new_layer = s.edit(label, |doc, active| {
        for (id, wt) in ids.iter().zip(&weights) {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            let mut mask = LayerMask::hide_all();
            mask.surface.write_region(area, wt);
            mask.surface.prune();
            l.mask = Some(mask);
        }
        let Some(px) = &blended else { return Ok(None) };
        let mut l = Layer::raster(doc.next_layer_name("Blended"), fmt);
        if let Some(surf) = l.surface_mut() {
            surf.write_region(area, px);
            surf.prune();
        }
        let nid = doc.insert_above(Some(top), l);
        *active = Some(nid);
        Ok(Some(nid))
    })?;
    Ok(json!({"method": method, "layers": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "blended": new_layer.map(|l| l.0)}))
}

macro_rules! spec {
    ($id:literal, $label:literal, $params:literal, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &["Edit"], shortcut: None, params: $params, enabled: two_layers, run: $run, journal: true }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "edit.autoAlignLayers",
            "Auto-Align Layers…",
            r##"{"projection":"auto|perspective|cylindrical|spherical|collage|reposition","reference":layer id?=bottom selected layer,"geometricCorrection":bool=false,"interpolation":"bicubic|bilinear|nearest"}"##,
            auto_align
        ),
        spec!("edit.autoBlendLayers", "Auto-Blend Layers…", r##"{"method":"panorama|stack","seamlessTones":bool=true}"##, auto_blend),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic texture written into a layer at offset (dx, dy).
    fn texture_layer(s: &mut Session, name: &str, dx: i32, dy: i32) -> LayerId {
        s.execute("layer.new.layer", json!({"name": name})).unwrap();
        s.edit("tex", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let mut seed = 99u64;
            let mut rnd = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (seed >> 33) as f64 / (1u64 << 31) as f64
            };
            surf.fill_rect(Rect::new(dx, dy, dx + 240, dy + 180), &[0.4, 0.4, 0.4, 1.0]);
            for _ in 0..120 {
                let (x, y) = ((rnd() * 220.0) as i32, (rnd() * 160.0) as i32);
                let (sw, sh) = (4 + (rnd() * 14.0) as i32, 4 + (rnd() * 10.0) as i32);
                let v = rnd() as f32;
                surf.fill_rect(Rect::new(dx + x, dy + y, dx + x + sw, dy + y + sh), &[v, 1.0 - v, v * 0.5, 1.0]);
            }
            Ok(())
        })
        .unwrap();
        s.active().unwrap().active_layer.unwrap()
    }

    fn select(s: &mut Session, ids: &[LayerId]) {
        s.execute("layer.select", json!({"layer": ids[0].0})).unwrap();
        for id in &ids[1..] {
            s.execute("layer.select", json!({"layer": id.0, "mode": "add"})).unwrap();
        }
    }

    #[test]
    fn auto_align_repositions_at_all_depths() {
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 300, "height": 240, "depth": depth, "background": "transparent"})).unwrap();
            let a = texture_layer(&mut s, "A", 20, 20);
            let b = texture_layer(&mut s, "B", 31, 13);
            assert!(!s.is_enabled("edit.autoAlignLayers"), "one layer selected");
            select(&mut s, &[a, b]);
            assert!(s.is_enabled("edit.autoAlignLayers"));
            let r = s.execute("edit.autoAlignLayers", json!({"projection": "reposition"})).unwrap();
            assert_eq!(r["reference"], a.0);
            let bb = s.active().unwrap().doc.layer(b).unwrap().surface().unwrap().content_bounds();
            assert!((bb.x0 - 20).abs() <= 1 && (bb.y0 - 20).abs() <= 1, "depth {depth}: {bb:?} {r}");
            s.undo();
            let bb = s.active().unwrap().doc.layer(b).unwrap().surface().unwrap().content_bounds();
            assert_eq!((bb.x0, bb.y0), (31, 13));
        }
    }

    #[test]
    fn auto_align_auto_projection_and_errors() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 300, "height": 240, "background": "transparent"})).unwrap();
        let a = texture_layer(&mut s, "A", 20, 20);
        let b = texture_layer(&mut s, "B", 26, 25);
        select(&mut s, &[a, b]);
        let r = s.execute("edit.autoAlignLayers", json!({})).unwrap();
        let m = r["aligned"][0]["matrix"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!((m[2] + 6.0).abs() < 1.0 && (m[5] + 5.0).abs() < 1.0, "{r}");
        assert!(s.execute("edit.autoAlignLayers", json!({"projection": "sideways"})).is_err());
        assert!(s.execute("edit.autoAlignLayers", json!({"reference": 9999})).is_err());
    }

    #[test]
    fn auto_blend_panorama_masks_and_seamless_layer() {
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 200, "height": 60, "depth": depth, "background": "transparent"})).unwrap();
            s.execute("layer.new.layer", json!({"name": "left"})).unwrap();
            s.edit("l", |doc, active| {
                doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 130, 60), &[0.2, 0.3, 0.8, 1.0]);
                Ok(())
            })
            .unwrap();
            let left = s.active().unwrap().active_layer.unwrap();
            s.execute("layer.new.layer", json!({"name": "right"})).unwrap();
            s.edit("r", |doc, active| {
                doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(70, 0, 200, 60), &[0.9, 0.6, 0.2, 1.0]);
                Ok(())
            })
            .unwrap();
            let right = s.active().unwrap().active_layer.unwrap();
            select(&mut s, &[left, right]);
            let r = s.execute("edit.autoBlendLayers", json!({"method": "panorama"})).unwrap();
            let doc = &s.active().unwrap().doc;
            let ml = doc.layer(left).unwrap().mask.as_ref().unwrap();
            let mr = doc.layer(right).unwrap().mask.as_ref().unwrap();
            assert_eq!((ml.value(10, 30), mr.value(10, 30)), (1.0, 0.0), "depth {depth}");
            assert_eq!((ml.value(190, 30), mr.value(190, 30)), (0.0, 1.0));
            let blended = LayerId(r["blended"].as_u64().unwrap());
            let px = doc.layer(blended).unwrap().surface().unwrap().pixel(5, 30);
            assert!((px[2] - 0.8).abs() < 0.02, "{px:?}");
            s.undo();
            assert!(s.active().unwrap().doc.layer(left).unwrap().mask.is_none());
        }
    }

    #[test]
    fn auto_blend_stack_without_seamless() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64, "background": "transparent"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "sharp"})).unwrap();
        s.edit("checker", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            for y in (0..64).step_by(2) {
                for x in (0..64).step_by(2) {
                    let v = if (x / 2 + y / 2) % 2 == 0 { 0.1 } else { 0.9 };
                    surf.fill_rect(Rect::new(x, y, x + 2, y + 2), &[v, v, v, 1.0]);
                }
            }
            Ok(())
        })
        .unwrap();
        let a = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.new.layer", json!({"name": "flat"})).unwrap();
        s.edit("flat", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 64), &[0.5, 0.5, 0.5, 1.0]);
            Ok(())
        })
        .unwrap();
        let flat = s.active().unwrap().active_layer.unwrap();
        select(&mut s, &[a, flat]);
        let r = s.execute("edit.autoBlendLayers", json!({"method": "stack", "seamlessTones": false})).unwrap();
        assert!(r["blended"].is_null());
        let doc = &s.active().unwrap().doc;
        // The detailed layer is sharper everywhere.
        let shown = (0..64).filter(|x| doc.layer(a).unwrap().mask.as_ref().unwrap().value(*x, 32) > 0.5).count();
        assert_eq!(shown, 64);
        assert_eq!(doc.layer(flat).unwrap().mask.as_ref().unwrap().value(10, 10), 0.0);
        assert!(s.execute("edit.autoBlendLayers", json!({"method": "sideways"})).is_err());
    }
}

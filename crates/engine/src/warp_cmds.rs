//! Edit › Transform › Warp, Layer › Smart Objects › Warp and split warps.
//!
//! A warp is a [`Warp`] (preset style + bend/distortions, or a custom bicubic Bezier mesh)
//! defined over a source box in document space. Pixel layers are resampled once
//! (`photocraft_algo::warp`); smart objects store the warp in their source space and re-render
//! from the source, so editing a smart object's warp never degrades it.
//!
//! The split commands edit a mesh: either one passed in (`"warp"`, returned edited — what the
//! Free Transform warp UI uses) or the active smart object's stored warp (a history step).

use photocraft_algo::transform::Interp;
use photocraft_algo::warp::{warp_mesh_gray, warp_mesh_surface};
use photocraft_doc::{Document, Layer, LayerContent, LayerId, Rect};
use photocraft_geom::Affine;
use photocraft_geom::warp::{BezierMesh, Warp, WarpStyle};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    s.active().and_then(|d| d.active_layer).map(|_| ()).ok_or_else(|| "no active layer".into())
}

fn active_smart(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document")?;
    match d.active_layer.and_then(|id| d.doc.layer(id)).map(|l| &l.content) {
        Some(LayerContent::Smart(_)) => Ok(()),
        _ => Err("the active layer is not a smart object".into()),
    }
}

fn rect_f(r: Rect) -> [f64; 4] {
    [f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1)]
}

fn affine_apply(t: &Affine, p: [f64; 2]) -> [f64; 2] {
    let [a, b, c, d, e, f] = t.m;
    [a * p[0] + c * p[1] + e, b * p[0] + d * p[1] + f]
}

/// The box a warp of `layer` starts from (Free Transform's frame).
/// For a warped smart object that is its (unwarped) source box, so re-warping replaces the warp
/// over the same frame.
pub fn warp_bounds(doc: &Document, layer: &Layer) -> Rect {
    if let LayerContent::Smart(sm) = &layer.content
        && let Some(w) = &sm.warp
    {
        let t = sm.transform;
        let c = [[w.bounds[0], w.bounds[1]], [w.bounds[2], w.bounds[1]], [w.bounds[2], w.bounds[3]], [w.bounds[0], w.bounds[3]]].map(|p| affine_apply(&t, p));
        let b = c.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
        return Rect::new(b[0].round() as i32, b[1].round() as i32, b[2].round() as i32, b[3].round() as i32);
    }
    crate::transform_cmds::transform_bounds(doc, layer)
}

/// A smart object's stored warp expressed in document space (for editing), if it has one.
pub fn smart_warp_doc_space(layer: &Layer) -> Option<Warp> {
    let LayerContent::Smart(sm) = &layer.content else { return None };
    let w = sm.warp.as_ref()?;
    let t = sm.transform;
    let mesh = w.to_mesh(1, 1).map_points(|p| affine_apply(&t, p));
    let corners = [[w.bounds[0], w.bounds[1]], [w.bounds[2], w.bounds[1]], [w.bounds[2], w.bounds[3]], [w.bounds[0], w.bounds[3]]].map(|p| affine_apply(&t, p));
    let b = corners.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
    Some(Warp::custom(mesh, b))
}

/// Parses the warp params: a full `"warp"` object, or `"style"` (+ `"bend"`, `"hDistort"`,
/// `"vDistort"`, `"vertical"`), `"mesh"` and `"grid"` over `rect`.
pub fn warp_from_params(cmd: &str, p: &Value, rect: [f64; 4]) -> Result<Warp> {
    if let Some(w) = p.get("warp") {
        let w: Warp = serde_json::from_value(w.clone()).map_err(|e| bad(cmd, format!("bad `warp`: {e}")))?;
        if w.mesh.as_ref().is_some_and(|m| !m.is_valid()) {
            return Err(bad(cmd, "malformed warp mesh"));
        }
        return Ok(w);
    }
    let num = |k: &str, d: f64| p.get(k).and_then(Value::as_f64).unwrap_or(d);
    let style = match p.get("style").and_then(Value::as_str) {
        Some(st) => WarpStyle::parse(st).ok_or_else(|| bad(cmd, format!("unknown warp style \"{st}\"")))?,
        None if p.get("mesh").is_some() || p.get("grid").is_some() => WarpStyle::Custom,
        None => return Err(bad(cmd, "pass `style` (arc, flag, …, custom), `mesh` or `warp`")),
    };
    let mut w = Warp::preset(style, num("bend", 50.0), rect);
    w.h_distort = num("hDistort", 0.0);
    w.v_distort = num("vDistort", 0.0);
    w.vertical = p.get("vertical").and_then(Value::as_bool).unwrap_or(false);
    if style == WarpStyle::Custom {
        let mesh = match p.get("mesh") {
            Some(m) => {
                let m: BezierMesh = serde_json::from_value(m.clone()).map_err(|e| bad(cmd, format!("bad `mesh`: {e}")))?;
                if !m.is_valid() {
                    return Err(bad(cmd, "malformed mesh (need (3·cols+1)×(3·rows+1) points and knots 0..1)"));
                }
                m
            }
            None => {
                let g = p.get("grid").and_then(Value::as_array);
                let at = |i: usize| g.and_then(|a| a.get(i)).and_then(Value::as_u64).unwrap_or(1) as usize;
                BezierMesh::identity(rect, at(0), at(1))
            }
        };
        w.mesh = Some(mesh);
    }
    Ok(w)
}

fn warp_layer(doc_sel: Option<&Surface>, l: &mut Layer, w: &Warp, rect: Rect, interp: Interp) -> Result<()> {
    if l.locks.position && l.name == "Background" {
        l.locks.position = false;
        l.locks.transparency = false;
        l.name = "Layer 0".into();
    }
    if l.locks.position || l.locks.all {
        return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
    }
    let map = |x: f64, y: f64| w.map(x, y);
    match &mut l.content {
        LayerContent::Group(g) => {
            for c in g.children.iter_mut() {
                warp_layer(None, c, w, rect, interp)?;
            }
        }
        LayerContent::Text(_) => return Err(EngineError::Other("Warp needs rasterized type (or use Type › Warp Text)".into())),
        LayerContent::Shape(_) => return Err(EngineError::Other("Warp needs a rasterized shape (Layer › Rasterize › Shape)".into())),
        LayerContent::Smart(sm) => {
            // Store in source space: undo the placement transform around the warp.
            let t = sm.transform;
            let inv = t.inverse().ok_or_else(|| EngineError::Other("the smart object's transform is degenerate".into()))?;
            let corners =
                [[w.bounds[0], w.bounds[1]], [w.bounds[2], w.bounds[1]], [w.bounds[2], w.bounds[3]], [w.bounds[0], w.bounds[3]]].map(|p| affine_apply(&inv, p));
            let sb = corners.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
            let src_warp = match w.style {
                WarpStyle::None => None,
                WarpStyle::Custom => Some(Warp::custom(w.to_mesh(1, 1).map_points(|p| affine_apply(&inv, p)), sb)),
                _ => Some(Warp { bounds: sb, mesh: None, ..w.clone() }),
            };
            sm.warp = src_warp.filter(|w| !w.is_identity());
            // Fallback appearance for sources that can't be re-rendered.
            if let Some(c) = &mut sm.cache {
                *c = warp_mesh_surface(c, rect, &map, interp);
            }
            if let Some(m) = &mut sm.filter_mask {
                m.surface = warp_mesh_gray(&m.surface, &map, interp);
            }
        }
        _ => {
            if let Some(surf) = l.surface_mut() {
                *surf = match doc_sel {
                    Some(sel) => {
                        let (lifted, mut rest) = crate::transform_cmds::split_selected(surf, sel);
                        let moved = warp_mesh_surface(&lifted, rect, &map, interp);
                        crate::transform_cmds::composite_over(&mut rest, &moved);
                        rest.prune();
                        rest
                    }
                    None => {
                        // Content outside the warp box stays where it is.
                        let fmt = surf.format();
                        let warped = warp_mesh_surface(surf, rect, &map, interp);
                        let mut rest = surf.convert(photocraft_color::PixelFormat::new(fmt.mode, fmt.sample, true));
                        crate::pixels::clear_surface(&mut rest, rect, None);
                        crate::transform_cmds::composite_over(&mut rest, &warped);
                        rest.prune();
                        rest
                    }
                };
            }
        }
    }
    if let Some(m) = l.mask.as_mut()
        && m.linked
    {
        m.surface = warp_mesh_gray(&m.surface, &map, interp);
    }
    Ok(())
}

fn target(s: &Session, cmd: &str, p: &Value) -> Result<(LayerId, Rect)> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let id = match p.get("layer").and_then(Value::as_u64) {
        Some(v) => LayerId(v),
        None => st.active_layer.ok_or(EngineError::Other("no active layer".into()))?,
    };
    let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let rect = match p.get("rect").and_then(Value::as_array) {
        Some(r) if r.len() == 4 => {
            let v: Vec<i32> = r.iter().map(|x| x.as_f64().unwrap_or(0.0).round() as i32).collect();
            Rect::new(v[0], v[1], v[2], v[3])
        }
        _ => warp_bounds(&st.doc, layer),
    };
    if rect.is_empty() {
        return Err(bad(cmd, "nothing to warp"));
    }
    Ok((id, rect))
}

fn apply(s: &mut Session, p: &Value, cmd: &str) -> Result<Value> {
    let (id, rect) = target(s, cmd, p)?;
    let w = warp_from_params(cmd, p, rect_f(rect))?;
    let is_smart = matches!(s.active().and_then(|d| d.doc.layer(id)).map(|l| &l.content), Some(LayerContent::Smart(_)));
    if w.is_identity() && !is_smart {
        return Ok(json!({"layer": id.0, "changed": false}));
    }
    let interp = Interp::parse(p.get("interpolation").and_then(Value::as_str).unwrap_or("bicubic"));
    s.edit("Warp", |doc, _| {
        let sel = doc.selection.clone();
        let is_group = doc.layer(id).is_some_and(Layer::is_group);
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        warp_layer(if is_group { None } else { sel.as_ref() }, l, &w, rect, interp)?;
        let snapshot = doc.clone();
        if let Some(l) = doc.layer_mut(id) {
            crate::transform_cmds::refresh_text(&snapshot, l);
        }
        if let Some(sel) = &doc.selection {
            doc.selection = Some(warp_mesh_gray(sel, &|x, y| w.map(x, y), Interp::Bilinear)).filter(|s| !s.content_bounds().is_empty());
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "changed": true, "rect": [rect.x0, rect.y0, rect.x1, rect.y1]}))
}

#[derive(Clone, Copy)]
pub enum Split {
    Crosswise,
    Horizontal,
    Vertical,
    Remove,
}

/// Applies a split (or removes the split nearest `at`) on `w`'s mesh (presets become custom).
pub fn split_warp(w: &Warp, at: Option<[f64; 2]>, how: Split) -> Option<Warp> {
    let mut mesh = w.to_mesh(1, 1);
    let (s, t) = match at {
        Some(p) => mesh.param_at(p),
        None => (0.5, 0.5),
    };
    // Split inside the patch: the middle of the patch containing (s, t) when no point is given.
    let mid = |k: &[f64], v: f64| {
        let i = k.windows(2).position(|w| v <= w[1]).unwrap_or(k.len() - 2);
        (k[i] + k[i + 1]) / 2.0
    };
    let (s, t) = if at.is_some() { (s, t) } else { (mid(&mesh.us, s), mid(&mesh.vs, t)) };
    let ok = match how {
        Split::Crosswise => {
            let a = mesh.split_u(s);
            let b = mesh.split_v(t);
            a || b
        }
        // A horizontal split line divides rows; a vertical one divides columns.
        Split::Horizontal => mesh.split_v(t),
        Split::Vertical => mesh.split_u(s),
        Split::Remove => {
            let near = |k: &[f64], v: f64| (1..k.len() - 1).min_by(|a, b| (k[*a] - v).abs().total_cmp(&(k[*b] - v).abs()));
            let (iu, iv) = (near(&mesh.us, s), near(&mesh.vs, t));
            let du = iu.map_or(f64::MAX, |i| (mesh.us[i] - s).abs());
            let dv = iv.map_or(f64::MAX, |i| (mesh.vs[i] - t).abs());
            match (iu, iv) {
                (Some(i), _) if du <= dv => mesh.remove_split_u(i),
                (_, Some(i)) => mesh.remove_split_v(i),
                (Some(i), None) => mesh.remove_split_u(i),
                _ => false,
            }
        }
    };
    ok.then(|| Warp::custom(mesh, w.bounds))
}

fn split(s: &mut Session, p: &Value, cmd: &str, how: Split) -> Result<Value> {
    let at = p.get("at").and_then(Value::as_array).and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]));
    if p.get("warp").is_some() || p.get("mesh").is_some() || p.get("style").is_some() {
        let rect = p.get("rect").and_then(Value::as_array).map(|r| {
            let v: Vec<f64> = r.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
            [v[0], v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(1.0), v.get(3).copied().unwrap_or(1.0)]
        });
        let rect = rect.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let w = warp_from_params(cmd, p, rect)?;
        let out = split_warp(&w, at, how).ok_or_else(|| bad(cmd, "nothing to split or remove there"))?;
        return Ok(json!({"warp": out}));
    }
    // The active smart object's stored warp.
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let id = st.active_layer.ok_or(EngineError::Other("no active layer".into()))?;
    let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Smart(sm) = &layer.content else {
        return Err(bad(cmd, "pass the warp being edited (`warp`), or select a warped smart object"));
    };
    let w = sm.warp.clone().unwrap_or_else(|| {
        let b = layer.surface().map_or(Rect::EMPTY, Surface::content_bounds);
        let inv = sm.transform.inverse().unwrap_or(Affine::IDENTITY);
        let c = [affine_apply(&inv, [f64::from(b.x0), f64::from(b.y0)]), affine_apply(&inv, [f64::from(b.x1), f64::from(b.y1)])];
        Warp::none([c[0][0].min(c[1][0]), c[0][1].min(c[1][1]), c[0][0].max(c[1][0]), c[0][1].max(c[1][1])])
    });
    let t = sm.transform;
    let at_src = at.and_then(|a| t.inverse().map(|inv| affine_apply(&inv, a)));
    let out = split_warp(&w, at_src, how).ok_or_else(|| bad(cmd, "nothing to split or remove there"))?;
    let label = if matches!(how, Split::Remove) { "Remove Warp Split" } else { "Split Warp" };
    s.edit(label, |doc, _| {
        if let Some(Layer { content: LayerContent::Smart(sm), .. }) = doc.layer_mut(id) {
            sm.warp = Some(out.clone());
        }
        crate::smart_cmds::refresh(doc, id).map(|_| ())
    })?;
    Ok(json!({"layer": id.0, "warp": out}))
}

pub fn specs() -> Vec<CommandSpec> {
    const P: &str = r##"{"layer":id?,"rect":[x0,y0,x1,y1]? (warp box; default = layer content ∩ selection),"style":"custom|none|arc|arcLower|arcUpper|arch|bulge|shellLower|shellUpper|flag|wave|fish|rise|fisheye|inflate|squeeze|twist","bend":%=50,"hDistort":%,"vDistort":%,"vertical":bool,"mesh":{"us":[0,…,1],"vs":[0,…,1],"points":[[x,y]…]} ((3c+1)×(3r+1) control points, row-major, document px),"grid":[cols,rows]?,"warp":{full warp object}?,"interpolation":"bicubic|bilinear|nearest"}"##;
    const S: &str = r##"{"warp":{…}? (the warp being edited; returned split),"rect":[x0,y0,x1,y1]?,"at":[x,y]? (document point; default = middle of the patch)} — without `warp`, edits the active smart object's warp"##;
    vec![
        CommandSpec {
            id: "edit.transform.warp",
            label: "Warp",
            menu: &["Edit", "Transform"],
            shortcut: None,
            params: P,
            enabled: has_layer,
            journal: true,
            run: |s, p| apply(s, p, "edit.transform.warp"),
        },
        CommandSpec {
            id: "layer.smartObjects.warp",
            label: "Warp",
            menu: &["Layer", "Smart Objects"],
            shortcut: None,
            params: P,
            enabled: active_smart,
            journal: true,
            run: |s, p| apply(s, p, "layer.smartObjects.warp"),
        },
        CommandSpec {
            id: "edit.transform.splitWarpCrosswise",
            label: "Split Warp Crosswise",
            menu: &["Edit", "Transform"],
            shortcut: None,
            params: S,
            enabled: has_layer,
            journal: true,
            run: |s, p| split(s, p, "edit.transform.splitWarpCrosswise", Split::Crosswise),
        },
        CommandSpec {
            id: "edit.transform.splitWarpHorizontally",
            label: "Split Warp Horizontally",
            menu: &["Edit", "Transform"],
            shortcut: None,
            params: S,
            enabled: has_layer,
            journal: true,
            run: |s, p| split(s, p, "edit.transform.splitWarpHorizontally", Split::Horizontal),
        },
        CommandSpec {
            id: "edit.transform.splitWarpVertically",
            label: "Split Warp Vertically",
            menu: &["Edit", "Transform"],
            shortcut: None,
            params: S,
            enabled: has_layer,
            journal: true,
            run: |s, p| split(s, p, "edit.transform.splitWarpVertically", Split::Vertical),
        },
        CommandSpec {
            id: "edit.transform.removeWarpSplit",
            label: "Remove Warp Split",
            menu: &["Edit", "Transform"],
            shortcut: None,
            params: S,
            enabled: has_layer,
            journal: true,
            run: |s, p| split(s, p, "edit.transform.removeWarpSplit", Split::Remove),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(depth: u64) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 80, "height": 60, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("paint", |doc, active| {
            let l = doc.layer_mut(active.unwrap()).unwrap();
            let surf = l.surface_mut().unwrap();
            for y in 10..40 {
                for x in 10..60 {
                    let v = ((x * 3 + y * 5) % 11) as f32 / 10.0;
                    surf.write_pixel(x, y, &[v, 1.0 - v, 0.5, 1.0]);
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn active_surface(s: &Session) -> Surface {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().clone()
    }

    #[test]
    fn identity_and_bend_zero_are_no_ops_at_every_depth() {
        for depth in [8u64, 16, 32] {
            let mut s = session(depth);
            let before = active_surface(&s);
            let r = s.execute("edit.transform.warp", json!({"style": "custom"})).unwrap();
            assert_eq!(r["changed"], false);
            s.execute("edit.transform.warp", json!({"style": "flag", "bend": 0})).unwrap();
            // A custom identity mesh passed explicitly (forces a resample) stays within 1/255.
            let m = BezierMesh::identity([10.0, 10.0, 60.0, 40.0], 2, 2);
            let mut w = Warp::custom(m, [10.0, 10.0, 60.0, 40.0]);
            w.mesh.as_mut().unwrap().points[5][0] += 1e-12; // not exactly identity
            s.execute("edit.transform.warp", json!({"warp": w})).unwrap();
            let after = active_surface(&s);
            let r = Rect::new(10, 10, 60, 40);
            assert_eq!(after.content_bounds(), r, "{depth}");
            let worst = before.read_region(r).iter().zip(after.read_region(r)).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst <= 1.0 / 255.0, "{depth}: {worst}");
        }
    }

    #[test]
    fn arc_warps_pixels_and_undoes() {
        let mut s = session(8);
        let before = active_surface(&s);
        s.execute("edit.transform.warp", json!({"style": "arc", "bend": 50})).unwrap();
        let after = active_surface(&s);
        assert!(after.content_bounds().y1 > 42, "{:?}", after.content_bounds());
        assert!(after.pixel(11, 11)[3] < 0.5, "top-left corner pulls in");
        s.undo();
        assert_eq!(active_surface(&s), before);
        assert!(s.execute("edit.transform.warp", json!({"style": "spiral"})).is_err());
        assert!(s.execute("edit.transform.warp", json!({})).is_err());
        assert!(s.execute("edit.transform.warp", json!({"mesh": {"us": [0, 1], "vs": [0, 1], "points": [[0, 0]]}})).is_err());
    }

    #[test]
    fn custom_mesh_moves_a_control_point() {
        let mut s = session(16);
        let mut m = BezierMesh::identity([10.0, 10.0, 60.0, 40.0], 1, 1);
        m.points[15] = [70.0, 50.0]; // bottom-right corner
        s.execute("edit.transform.warp", json!({"mesh": m})).unwrap();
        let b = active_surface(&s).content_bounds();
        assert!(b.x1 >= 69 && b.y1 >= 49, "{b:?}");
    }

    #[test]
    fn split_commands_edit_meshes_exactly() {
        let mut s = session(8);
        let w = Warp::preset(WarpStyle::Bulge, 30.0, [10.0, 10.0, 60.0, 40.0]);
        let r = s.execute("edit.transform.splitWarpCrosswise", json!({"warp": w, "at": [30.0, 20.0]})).unwrap();
        let split: Warp = serde_json::from_value(r["warp"].clone()).unwrap();
        let m = split.mesh.as_ref().unwrap();
        assert_eq!((m.nx(), m.ny()), (7, 7));
        let r = s.execute("edit.transform.splitWarpVertically", json!({"warp": split})).unwrap();
        let v: Warp = serde_json::from_value(r["warp"].clone()).unwrap();
        assert_eq!(v.mesh.as_ref().unwrap().us.len(), 4);
        let r = s.execute("edit.transform.removeWarpSplit", json!({"warp": v, "at": [30.0, 20.0]})).unwrap();
        let back: Warp = serde_json::from_value(r["warp"].clone()).unwrap();
        assert!(back.mesh.as_ref().unwrap().us.len() + back.mesh.as_ref().unwrap().vs.len() < 7);
        // Splitting never changes the shape.
        for (x, y) in [(15.0, 15.0), (40.0, 33.0)] {
            let (a, b) = (w.map(x, y), split.map(x, y));
            assert!((a.0 - b.0).abs() < 0.6 && (a.1 - b.1).abs() < 0.6, "{a:?} {b:?}");
        }
    }

    #[test]
    fn smart_object_warp_is_lossless() {
        for depth in [8u64, 16, 32] {
            let mut s = session(depth);
            s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
            let original = active_surface(&s);
            s.execute("layer.smartObjects.warp", json!({"style": "wave", "bend": 60})).unwrap();
            let warped = active_surface(&s);
            assert_ne!(warped, original);
            let st = s.active().unwrap();
            let LayerContent::Smart(sm) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!() };
            assert!(sm.warp.is_some());
            // Removing the warp re-renders the source exactly (no accumulated resampling).
            s.execute("layer.smartObjects.warp", json!({"style": "none"})).unwrap();
            assert_eq!(active_surface(&s), original, "{depth}");
            // Warping twice equals warping once with the second warp.
            s.execute("layer.smartObjects.warp", json!({"style": "arc", "bend": 20})).unwrap();
            s.execute("layer.smartObjects.warp", json!({"style": "wave", "bend": 60})).unwrap();
            assert_eq!(active_surface(&s), warped, "{depth}");
            // Split on the stored warp keeps the appearance (within resampling of the mesh fit).
            s.execute("edit.transform.splitWarpCrosswise", json!({})).unwrap();
            let st = s.active().unwrap();
            let LayerContent::Smart(sm) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!() };
            assert_eq!(sm.warp.as_ref().unwrap().style, WarpStyle::Custom);
        }
    }
}

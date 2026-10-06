use serde_json::json;

use super::*;

fn session(w: u32, h: u32, depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "white", "depth": depth})).unwrap();
    s
}

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

fn shape(s: &Session, id: u64) -> ShapeLayer {
    match &doc(s).layer(LayerId(id)).unwrap().content {
        LayerContent::Shape(sh) => sh.clone(),
        other => panic!("not a shape: {}", other.kind_name()),
    }
}

fn alpha_sum(sh: &ShapeLayer) -> f64 {
    let c = sh.cache.as_ref().unwrap();
    let r = c.content_bounds();
    let fmt = c.format();
    c.read_region(r).chunks_exact(fmt.channels()).map(|p| f64::from(photocraft_raster::to_rgba(&fmt, p)[3])).sum()
}

fn selection_mask(s: &Session) -> Vec<f32> {
    let d = doc(s);
    sel::mask_from_surface(d.selection.as_ref(), d.bounds())
}

#[test]
fn shape_create_every_kind_and_undo() {
    for depth in [8, 16, 32] {
        let mut s = session(200, 150, depth);
        let cases = [
            (json!({"kind": "rect", "rect": [10, 10, 50, 40]}), 2000.0),
            (json!({"kind": "roundedRect", "rect": [10, 10, 50, 40], "radii": 10}), 2000.0 - (4.0 - std::f64::consts::PI) * 100.0),
            (json!({"kind": "ellipse", "rect": [20, 20, 100, 60]}), std::f64::consts::PI * 50.0 * 30.0),
            (json!({"kind": "polygon", "rect": [0, 0, 100, 100], "sides": 6}), 1.5 * 3f64.sqrt() * 2500.0),
            (json!({"kind": "star", "rect": [0, 0, 100, 100], "sides": 5, "starRatio": 0.5}), 0.0),
            (json!({"kind": "line", "from": [10, 100], "to": [110, 100], "weight": 4}), 400.0),
            (json!({"kind": "path", "path": {"subpaths": [{"closed": true, "knots": [[0, 0], [60, 0], [0, 60]]}]}}), 1800.0),
        ];
        let before = doc(&s).layer_count();
        for (i, (p, area)) in cases.iter().enumerate() {
            let r = s.execute("shape.create", p.clone()).unwrap();
            let id = r["layer"].as_u64().unwrap();
            let sh = shape(&s, id);
            assert_eq!(sh.cache.as_ref().unwrap().format(), doc(&s).pixel_format());
            let a = alpha_sum(&sh);
            if *area > 0.0 {
                assert!((a - area).abs() / area < 0.01, "case {i} depth {depth}: {a} vs {area}");
            } else {
                assert!(a > 1000.0);
            }
            assert_eq!(r["kind"], p["kind"], "{r}");
            assert_eq!(s.active().unwrap().active_layer, Some(LayerId(id)));
        }
        assert_eq!(doc(&s).layer_count(), before + cases.len());
        for _ in 0..cases.len() {
            assert!(s.undo());
        }
        assert_eq!(doc(&s).layer_count(), before);
        assert!(s.redo());
        assert_eq!(doc(&s).layer_count(), before + 1);
    }
}

#[test]
fn shape_create_fill_stroke_and_composite() {
    let mut s = session(100, 100, 8);
    let r = s
        .execute(
            "shape.create",
            json!({"kind": "rect", "rect": [20, 20, 60, 60], "fill": "#ff0000", "stroke": {"width": 4, "color": "#0000ff", "align": "outside"}}),
        )
        .unwrap();
    assert_eq!(r["fill"], "#ff0000");
    assert_eq!(r["stroke"]["align"], "outside");
    assert_eq!(r["bounds"], json!([16, 16, 68, 68]));
    let f = photocraft_compose::flatten(doc(&s));
    let px = |x: i32, y: i32| f.get(x, y);
    assert!(px(50, 50)[0] > 0.99 && px(50, 50)[2] < 0.01);
    assert!(px(18, 50)[2] > 0.99 && px(18, 50)[0] < 0.01);
    assert!(px(10, 10)[1] > 0.99);
    // No fill: only the stroke.
    let r = s.execute("shape.create", json!({"kind": "ellipse", "rect": [5, 5, 40, 40], "fill": null, "stroke": {"width": 2}})).unwrap();
    let sh = shape(&s, r["layer"].as_u64().unwrap());
    assert!(sh.fill.is_none());
    assert!((alpha_sum(&sh) - std::f64::consts::PI * 40.0 * 2.0).abs() < 3.0, "{}", alpha_sum(&sh));
    assert!(s.execute("shape.create", json!({"kind": "rect"})).is_err());
    assert!(s.execute("shape.create", json!({"kind": "blob", "rect": [0, 0, 1, 1]})).is_err());
}

#[test]
fn shape_edit_live_params_and_undo() {
    let mut s = session(200, 200, 8);
    let id = s.execute("shape.create", json!({"kind": "rect", "rect": [10, 10, 50, 50]})).unwrap()["layer"].as_u64().unwrap();
    let r = s.execute("shape.edit", json!({"layer": id, "rect": [10, 10, 100, 50]})).unwrap();
    assert_eq!(r["bounds"], json!([10, 10, 100, 50]));
    assert!((alpha_sum(&shape(&s, id)) - 5000.0).abs() < 1.0);
    // Radii turn it into a rounded rectangle.
    let r = s.execute("shape.edit", json!({"layer": id, "radii": [20, 0, 20, 0]})).unwrap();
    assert_eq!(r["kind"], "roundedRect");
    assert_eq!(r["live"]["radii"], json!([20.0, 0.0, 20.0, 0.0]));
    // Move keeps the live shape; a rotation drops it.
    let r = s.execute("shape.edit", json!({"layer": id, "move": [5, 7]})).unwrap();
    assert_eq!(r["live"]["rect"], json!([15.0, 17.0, 100.0, 50.0]));
    let r = s.execute("shape.edit", json!({"layer": id, "transform": [0.0, 1.0, -1.0, 0.0, 150.0, 0.0]})).unwrap();
    assert_eq!(r["kind"], "path");
    // Fill/stroke edits merge.
    s.execute("shape.edit", json!({"layer": id, "stroke": {"width": 3}})).unwrap();
    let r = s
        .execute(
            "shape.edit",
            json!({"layer": id, "stroke": {"cap": "round", "dashes": [2, 1]}, "fill": {"gradient": {"stops": [[0, "#000000"], [1, "#ffffff"]], "angle": 0}}}),
        )
        .unwrap();
    assert_eq!(r["stroke"]["width"], 3.0);
    assert_eq!(r["stroke"]["cap"], "round");
    assert!(r["fill"]["gradient"].is_object());
    s.execute("shape.edit", json!({"layer": id, "stroke": null, "name": "Box"})).unwrap();
    assert!(shape(&s, id).stroke.is_none());
    assert_eq!(doc(&s).layer(LayerId(id)).unwrap().name, "Box");
    // Undo walks back one edit at a time.
    s.undo();
    assert!(shape(&s, id).stroke.is_some());
    // Editing a non-shape layer fails.
    let bg = doc(&s).layers[0].id.0;
    assert!(s.execute("shape.edit", json!({"layer": bg, "fill": "#000000"})).is_err());
}

#[test]
fn shape_add_to_with_path_ops() {
    let mut s = session(100, 100, 8);
    let id = s.execute("shape.create", json!({"kind": "rect", "rect": [0, 0, 60, 60]})).unwrap()["layer"].as_u64().unwrap();
    let r = s.execute("shape.create", json!({"kind": "rect", "rect": [30, 30, 60, 60], "addTo": id, "op": "subtract"})).unwrap();
    assert_eq!(r["path"]["subpaths"][1]["op"], "subtract");
    assert!((alpha_sum(&shape(&s, id)) - (3600.0 - 900.0)).abs() < 1.0);
    s.execute("layer.combineShapes.intersectShapeAreas", json!({"layer": id})).unwrap();
    assert!((alpha_sum(&shape(&s, id)) - 900.0).abs() < 1.0);
    s.execute("layer.combineShapes.excludeOverlappingShapes", json!({"layer": id})).unwrap();
    assert!((alpha_sum(&shape(&s, id)) - 5400.0).abs() < 1.0);
    // Merge bakes the xor into plain outlines with the same area.
    let r = s.execute("layer.combineShapes.mergeShapeComponents", json!({"layer": id})).unwrap();
    let merged = shape(&s, id);
    assert!(merged.path.subpaths.len() >= 2, "{r}");
    assert!((alpha_sum(&merged) - 5400.0).abs() < 20.0, "{}", alpha_sum(&merged));
}

#[test]
fn shape_rasterize_keeps_pixels() {
    let mut s = session(64, 64, 16);
    let id = s.execute("shape.create", json!({"kind": "ellipse", "rect": [4, 4, 40, 30]})).unwrap()["layer"].as_u64().unwrap();
    let before = shape(&s, id).cache.unwrap();
    let flat = photocraft_compose::flatten(doc(&s));
    s.execute("layer.rasterize.shape", json!({"layer": id})).unwrap();
    let l = doc(&s).layer(LayerId(id)).unwrap();
    assert!(matches!(l.content, LayerContent::Raster(_)));
    assert_eq!(l.surface().unwrap(), &before);
    assert_eq!(photocraft_compose::flatten(doc(&s)).px, flat.px);
    assert!(s.execute("shape.rasterize", json!({"layer": id})).is_err());
}

#[test]
fn paths_set_rename_delete_list() {
    let mut s = session(100, 100, 8);
    let tri = json!({"subpaths": [{"knots": [[10, 10], [90, 10], [50, 90]]}]});
    s.execute("path.set", json!({"path": tri})).unwrap();
    assert!(doc(&s).work_path.is_some());
    // Saving the work path (rename) moves it into the saved paths.
    s.execute("path.rename", json!({"to": "Triangle"})).unwrap();
    assert!(doc(&s).work_path.is_none());
    assert_eq!(doc(&s).paths[0].name, "Triangle");
    s.execute("path.set", json!({"name": "Triangle", "op": "subtract", "path": {"subpaths": [{"knots": [[40, 20], [60, 20], [60, 40], [40, 40]]}]}})).unwrap();
    assert_eq!(doc(&s).paths[0].path.subpaths.len(), 2);
    assert_eq!(doc(&s).paths[0].path.subpaths[1].op, PathOp::Subtract);
    let l = s.execute("path.list", json!({})).unwrap();
    assert_eq!(l["paths"][0]["knots"], 7);
    let info = s.execute("path.info", json!({"name": "Triangle"})).unwrap();
    assert_eq!(info["path"]["subpaths"][1]["op"], "subtract");
    s.execute("path.rename", json!({"name": "Triangle", "to": "Tri"})).unwrap();
    assert!(s.execute("path.delete", json!({"name": "Triangle"})).is_err());
    s.execute("path.delete", json!({"name": "Tri"})).unwrap();
    assert!(doc(&s).paths.is_empty());
    s.undo();
    assert_eq!(doc(&s).paths[0].name, "Tri");
    assert!(s.execute("path.set", json!({"path": {"subpaths": [{"knots": [[0, 0], [1]]}]}})).is_err());
}

#[test]
fn path_to_selection_matches_analytic_shape() {
    let mut s = session(200, 200, 8);
    // Circle of radius 60 at (100, 100) as a work path.
    let c = vector::shapes::ellipse(40.0, 40.0, 120.0, 120.0);
    s.execute("path.set", json!({"path": path_json(&c)})).unwrap();
    s.execute("path.toSelection", json!({})).unwrap();
    let m = selection_mask(&s);
    let area: f64 = m.iter().map(|v| f64::from(*v)).sum();
    let want = std::f64::consts::PI * 3600.0;
    assert!((area - want).abs() / want < 0.005, "{area} vs {want}");
    // Pixel-level: centres well inside/outside are exact; edge pixels match the analytic coverage.
    let at = |x: usize, y: usize| m[y * 200 + x];
    assert_eq!(at(100, 100), 1.0);
    assert_eq!(at(10, 10), 0.0);
    for (x, y) in [(160usize, 100usize), (100, 40), (142, 142)] {
        let d = ((x as f64 + 0.5 - 100.0).powi(2) + (y as f64 + 0.5 - 100.0).powi(2)).sqrt();
        let analytic = (60.0 - d + 0.5).clamp(0.0, 1.0);
        assert!((f64::from(at(x, y)) - analytic).abs() < 0.2, "({x},{y}): {} vs {analytic}", at(x, y));
    }
    // Modes: subtract a square, then intersect.
    s.execute("path.set", json!({"name": "Sq", "path": {"subpaths": [{"knots": [[100, 0], [200, 0], [200, 200], [100, 200]]}]}})).unwrap();
    s.execute("path.toSelection", json!({"name": "Sq", "mode": "subtract"})).unwrap();
    let half: f64 = selection_mask(&s).iter().map(|v| f64::from(*v)).sum();
    assert!((half - want / 2.0).abs() / want < 0.005);
    // Non-anti-aliased selections are binary; feather softens.
    s.execute("path.toSelection", json!({"antiAlias": false})).unwrap();
    assert!(selection_mask(&s).iter().all(|v| *v == 0.0 || *v == 1.0));
    s.execute("path.toSelection", json!({"feather": 8})).unwrap();
    let m = selection_mask(&s);
    assert!(m[100 * 200 + 160] > 0.2 && m[100 * 200 + 160] < 0.8);
}

#[test]
fn selection_to_work_path_roundtrip_iou() {
    let mut s = session(240, 180, 8);
    // An anti-aliased donut plus a rectangle, from a path, as the selection.
    let mut p = vector::shapes::ellipse(20.0, 20.0, 140.0, 140.0);
    p.subpaths.push(vector::shapes::ellipse(60.0, 60.0, 60.0, 60.0).subpaths.remove(0).with_op(PathOp::Subtract));
    p.subpaths.push(vector::shapes::rect(170.0, 30.0, 50.0, 120.0).subpaths.remove(0));
    s.execute("path.set", json!({"name": "src", "path": path_json(&p)})).unwrap();
    s.execute("path.toSelection", json!({"name": "src"})).unwrap();
    let before = selection_mask(&s);
    for tol in [1.0, 2.0] {
        let r = s.execute("select.toWorkPath", json!({"tolerance": tol})).unwrap();
        assert_eq!(r["subpaths"], 3);
        s.execute("path.toSelection", json!({})).unwrap();
        let after = selection_mask(&s);
        let (mut i, mut u) = (0.0f64, 0.0f64);
        for (a, b) in before.iter().zip(&after) {
            i += f64::from(a.min(*b));
            u += f64::from(a.max(*b));
        }
        assert!(i / u > 0.98, "tolerance {tol}: IoU {}", i / u);
        // Restore the original selection for the next tolerance.
        s.execute("path.toSelection", json!({"name": "src"})).unwrap();
    }
    // Selection → shape layer.
    let r = s.execute("select.convertToShape", json!({"fill": "#00ff00"})).unwrap();
    assert_eq!(r["fill"], "#00ff00");
    assert_eq!(r["path"]["subpaths"].as_array().unwrap().len(), 3);
}

#[test]
fn fill_and_stroke_path_on_pixel_layer() {
    let mut s = session(100, 100, 16);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("path.set", json!({"path": {"subpaths": [{"knots": [[10, 10], [60, 10], [60, 60], [10, 60]]}]}})).unwrap();
    s.execute("path.fill", json!({"color": "#ff0000", "opacity": 50})).unwrap();
    let d = doc(&s);
    let l = d.layer(s.active().unwrap().active_layer.unwrap()).unwrap();
    let px = l.surface().unwrap().rgba(30, 30);
    assert!((px[0] - 1.0).abs() < 1e-3 && (px[3] - 0.5).abs() < 0.01, "{px:?}");
    assert_eq!(l.surface().unwrap().rgba(80, 80)[3], 0.0);
    // Stroke with a hard 4 px pencil along the square outline.
    s.execute("path.stroke", json!({"tool": "pencil", "size": 4, "color": "#0000ff", "opacity": 100})).unwrap();
    let d = doc(&s);
    let surf = d.layer(s.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().clone();
    assert!(surf.rgba(10, 35)[2] > 0.9);
    assert!(surf.rgba(60, 35)[2] > 0.9);
    assert!(surf.rgba(35, 35)[2] < 0.1);
    assert!(s.execute("path.stroke", json!({"tool": "airbrush"})).is_err());
    // Filling needs a pixel layer.
    s.execute("shape.create", json!({"kind": "rect", "rect": [0, 0, 5, 5]})).unwrap();
    assert!(s.execute("path.fill", json!({})).is_err());
}

#[test]
fn vector_mask_commands_and_compositing() {
    let mut s = session(100, 100, 8);
    s.execute("layer.new.layer", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap().0;
    // Fill the layer black via a full-canvas path fill (works regardless of edit.fill).
    s.execute("path.set", json!({"name": "all", "path": {"subpaths": [{"knots": [[0, 0], [100, 0], [100, 100], [0, 100]]}]}})).unwrap();
    s.execute("path.fill", json!({"name": "all", "color": "#000000"})).unwrap();
    // Reveal All: nothing hidden.
    s.execute("layer.vectorMask.revealAll", json!({})).unwrap();
    let f = photocraft_compose::flatten(doc(&s));
    assert!(f.get(80, 80)[0] < 0.01);
    // Current path → only the left half shows the black layer.
    s.execute("path.set", json!({"path": {"subpaths": [{"knots": [[0, 0], [50, 0], [50, 100], [0, 100]]}]}})).unwrap();
    s.execute("layer.vectorMask.currentPath", json!({})).unwrap();
    let f = photocraft_compose::flatten(doc(&s));
    assert!(f.get(20, 50)[0] < 0.01 && f.get(80, 50)[0] > 0.99);
    // Disable → all black again; toggling re-enables.
    s.execute("layer.vectorMask.enabled", json!({})).unwrap();
    assert!(photocraft_compose::flatten(doc(&s)).get(80, 50)[0] < 0.01);
    s.execute("layer.vectorMask.enabled", json!({})).unwrap();
    // Density 50%: the hidden half shows half grey.
    let r = s.execute("layer.vectorMask.edit", json!({"density": 50})).unwrap();
    assert_eq!(r["density"], 50.0);
    assert!((photocraft_compose::flatten(doc(&s)).get(80, 50)[0] - 0.5).abs() < 0.01);
    s.execute("layer.vectorMask.edit", json!({"density": 100})).unwrap();
    // Hide All.
    s.execute("layer.vectorMask.hideAll", json!({})).unwrap();
    assert!(photocraft_compose::flatten(doc(&s)).get(20, 50)[0] > 0.99);
    // Rasterize into the pixel mask: same composite, no vector mask left.
    s.execute("layer.vectorMask.currentPath", json!({})).unwrap();
    let before = photocraft_compose::flatten(doc(&s));
    s.execute("layer.rasterize.vectorMask", json!({})).unwrap();
    let l = doc(&s).layer(LayerId(id)).unwrap();
    assert!(l.vector_mask.is_none() && l.mask.is_some());
    assert_eq!(photocraft_compose::flatten(doc(&s)).px, before.px);
    // Delete / undo.
    s.undo();
    s.execute("layer.vectorMask.delete", json!({})).unwrap();
    assert!(doc(&s).layer(LayerId(id)).unwrap().vector_mask.is_none());
    assert!(s.execute("layer.vectorMask.delete", json!({})).is_err());
    s.undo();
    assert!(doc(&s).layer(LayerId(id)).unwrap().vector_mask.is_some());
    // Shape layers cannot take a separate vector mask.
    s.execute("shape.create", json!({"kind": "rect", "rect": [0, 0, 10, 10]})).unwrap();
    assert!(s.execute("layer.vectorMask.revealAll", json!({})).is_err());
}

#[test]
fn translate_vectors_moves_shapes_and_masks() {
    let mut s = session(100, 100, 8);
    let id = s.execute("shape.create", json!({"kind": "rect", "rect": [10, 10, 20, 20]})).unwrap()["layer"].as_u64().unwrap();
    s.edit("Move", |d, _| {
        let snap = d.clone();
        let l = d.layer_mut(LayerId(id)).unwrap();
        translate_vectors(&snap, l, 5.0, 0.0);
        Ok(())
    })
    .unwrap();
    let sh = shape(&s, id);
    assert_eq!(sh.cache.unwrap().content_bounds(), Rect::new(15, 10, 35, 30));
    assert_eq!(sh.live, Some(LiveShape::Rect { rect: [15.0, 10.0, 20.0, 20.0], radii: [0.0; 4] }));
}

#[test]
fn all_vector_commands_are_registered() {
    for id in [
        "shape.create",
        "shape.edit",
        "shape.info",
        "shape.rasterize",
        "path.set",
        "path.delete",
        "path.rename",
        "path.list",
        "path.info",
        "path.toSelection",
        "select.toWorkPath",
        "path.fill",
        "path.stroke",
        "layer.vectorMask.add",
        "layer.vectorMask.fromPath",
        "layer.vectorMask.delete",
        "layer.vectorMask.edit",
        "layer.rasterize.vectorMask",
    ] {
        assert!(crate::commands::find(id).is_some(), "{id}");
    }
    let mut ids: Vec<&str> = crate::command_specs().iter().map(|c| c.id).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate command ids");
}

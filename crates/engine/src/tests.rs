use super::*;
use serde_json::json;

fn session_with_doc() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    s
}

fn px(s: &mut Session, x: i32, y: i32) -> Vec<f32> {
    serde_json::from_value(s.execute("document.pixel", json!({"x": x, "y": y})).unwrap()).unwrap()
}

#[test]
fn command_ids_are_unique_and_documented() {
    let mut seen = std::collections::HashSet::new();
    for c in command_specs() {
        assert!(seen.insert(c.id), "duplicate command id {}", c.id);
        assert!(!c.label.is_empty());
        assert!(c.id.contains('.'), "{} should be namespaced", c.id);
    }
    assert!(command_specs().len() > 60, "{}", command_specs().len());
}

#[test]
fn unknown_and_disabled_commands_error() {
    let mut s = Session::new();
    assert!(matches!(s.execute("nope.nothing", json!({})), Err(EngineError::UnknownCommand(_))));
    assert!(matches!(s.execute("layer.new.layer", json!({})), Err(EngineError::Disabled(..))));
    assert!(!s.is_enabled("edit.undo"));
}

#[test]
fn file_new_takes_whole_floats_and_survives_odd_sizes() {
    // #254: the New dialog sent `512.0`; it must not fall back to 1920 x 1080.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 512.0, "height": 511.6})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!((d.size.width, d.size.height), (512, 512));
    s.execute("file.new", json!({"width": -5.0, "height": "x"})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!((d.size.width, d.size.height), (1, 1080));
}

#[test]
fn file_new_variants() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 10, "height": 5, "background": "transparent"})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers[0].name, "Layer 1");
    s.execute("file.new", json!({"width": 10, "height": 5, "mode": "cmyk", "depth": 16})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!(d.mode, photocraft_color::ColorMode::Cmyk);
    assert_eq!(d.depth, photocraft_color::SampleType::U16);
    s.execute("file.new", json!({"background": "#ff0000", "width": 4, "height": 4})).unwrap();
    assert_eq!(px(&mut s, 1, 1), vec![1.0, 0.0, 0.0, 1.0]);
    assert_eq!(s.documents().len(), 3);
    s.execute("file.close", json!({})).unwrap();
    assert_eq!(s.documents().len(), 2);
}

#[test]
fn layer_lifecycle_with_undo() {
    let mut s = session_with_doc();
    let r = s.execute("layer.new.layer", json!({})).unwrap();
    let id = r["layer"].as_u64().unwrap();
    assert_eq!(s.active().unwrap().active_layer, Some(LayerId(id)));
    s.execute("layer.setProps", json!({"name": "Ink", "opacity": 0.5, "blend": "multiply"})).unwrap();
    let doc = &s.active().unwrap().doc;
    let l = doc.layer(LayerId(id)).unwrap();
    assert_eq!((l.name.as_str(), l.opacity, l.blend), ("Ink", 0.5, photocraft_color::BlendMode::Multiply));
    s.execute("layer.duplicate", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer_count(), 3);
    s.execute("layer.delete", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer_count(), 2);
    for _ in 0..4 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(s.active().unwrap().doc.layer_count(), 1);
    assert!(!s.is_enabled("edit.undo"));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer_count(), 2);
}

#[test]
fn bad_blend_mode_is_a_param_error() {
    let mut s = session_with_doc();
    let e = s.execute("layer.setProps", json!({"blend": "sparkle"})).unwrap_err();
    assert!(matches!(e, EngineError::BadParams { .. }), "{e}");
}

#[test]
fn adjustment_layers_change_composite() {
    let mut s = session_with_doc();
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    assert_eq!(px(&mut s, 3, 3), vec![0.0, 0.0, 0.0, 1.0]);
    let doc = s.execute("document.inspect", json!({})).unwrap();
    assert_eq!(doc["layers"][0]["kind"], "Adjustment");
    assert_eq!(doc["layers"][0]["name"], "Invert 1");
    s.execute("layer.setProps", json!({"visible": false})).unwrap();
    assert_eq!(px(&mut s, 3, 3), vec![1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn every_adjustment_command_runs() {
    let mut s = session_with_doc();
    let ids: Vec<&str> =
        command_specs().iter().map(|c| c.id).filter(|id| id.starts_with("layer.newAdjustmentLayer.") || id.starts_with("image.adjustments.")).collect();
    assert!(ids.len() >= 28);
    for id in ids {
        // re-select the background for destructive ones
        let bg = s.active().unwrap().doc.layers[0].id;
        s.select_layer(bg).unwrap();
        s.execute(id, json!({})).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
}

#[test]
fn threshold_and_hue_params() {
    let mut s = session_with_doc();
    s.execute("edit.fill", json!({"color": "#404040"})).unwrap();
    s.execute("layer.newAdjustmentLayer.threshold", json!({"level": 128})).unwrap();
    assert_eq!(px(&mut s, 0, 0), vec![0.0, 0.0, 0.0, 1.0]);
    s.execute("layer.setAdjustment", json!({"level": 10})).unwrap();
    assert_eq!(px(&mut s, 0, 0), vec![1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn paint_stroke_and_selection() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 32, "height": 48})).unwrap();
    let r = s.execute("paint.stroke", json!({"points": [[4, 20], [60, 20]], "size": 6, "color": "#0000ff"})).unwrap();
    assert!(r["damage"][2].as_i64().unwrap() > 0);
    assert_eq!(px(&mut s, 10, 20), vec![0.0, 0.0, 1.0, 1.0]);
    assert_eq!(px(&mut s, 50, 20), vec![1.0, 1.0, 1.0, 1.0], "outside selection stays white");
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("paint.stroke", json!({"points": [[4, 30], [60, 30]], "size": 6, "erase": false})).unwrap();
    assert_eq!(px(&mut s, 50, 30), vec![0.0, 0.0, 0.0, 1.0], "foreground is black");
}

#[test]
fn marquee_dragged_past_the_canvas_stops_at_its_edge() {
    let mut s = session_with_doc(); // 64 × 48
    let bounds = |s: &mut Session| s.execute("document.inspect", json!({})).unwrap()["selectionBounds"].clone();
    s.execute("select.rect", json!({"x": -20, "y": 10, "width": 200, "height": 100})).unwrap();
    assert_eq!(bounds(&mut s), json!([0, 10, 64, 38]));
    s.execute("select.rect", json!({"x": -30, "y": -30, "width": 200, "height": 200, "ellipse": true})).unwrap();
    assert_eq!(bounds(&mut s), json!([0, 0, 64, 48]));
    // Entirely off the canvas: nothing is selected.
    s.execute("select.rect", json!({"x": 100, "y": 100, "width": 20, "height": 20})).unwrap();
    assert_eq!(bounds(&mut s), Value::Null);
}

#[test]
fn selection_modes() {
    let mut s = session_with_doc();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 0, "width": 10, "height": 10, "mode": "add"})).unwrap();
    let b = s.execute("document.inspect", json!({})).unwrap()["selectionBounds"].clone();
    assert_eq!(b, json!([0, 0, 30, 10]));
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 10, "mode": "subtract"})).unwrap();
    assert_eq!(s.execute("document.inspect", json!({})).unwrap()["selectionBounds"], json!([20, 0, 10, 10]));
    s.execute("select.inverse", json!({})).unwrap();
    s.execute("select.all", json!({})).unwrap();
    assert_eq!(s.execute("document.inspect", json!({})).unwrap()["selectionBounds"], json!([0, 0, 64, 48]));
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 20, "height": 20, "ellipse": true})).unwrap();
}

#[test]
fn fill_clear_and_masks() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
    assert_eq!(px(&mut s, 5, 5), vec![0.0, 1.0, 0.0, 1.0]);
    s.execute("layer.layerMask.hideAll", json!({})).unwrap();
    assert_eq!(px(&mut s, 5, 5), vec![1.0, 1.0, 1.0, 1.0]);
    s.execute("layer.layerMask.delete", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(px(&mut s, 5, 5), vec![1.0, 1.0, 1.0, 1.0]);
    assert_eq!(px(&mut s, 20, 20), vec![0.0, 1.0, 0.0, 1.0]);
}

#[test]
fn clipping_and_merge_and_flatten() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 48})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("edit.fill", json!({"color": "#0000ff"})).unwrap();
    s.execute("layer.createClippingMask", json!({})).unwrap();
    assert_eq!(px(&mut s, 5, 5), vec![0.0, 0.0, 1.0, 1.0]);
    assert_eq!(px(&mut s, 30, 5), vec![1.0, 1.0, 1.0, 1.0]);
    s.execute("layer.mergeDown", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer_count(), 2);
    assert_eq!(px(&mut s, 5, 5), vec![0.0, 0.0, 1.0, 1.0]);
    assert_eq!(px(&mut s, 30, 5), vec![1.0, 1.0, 1.0, 1.0]);
    s.execute("layer.flattenImage", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer_count(), 1);
    assert_eq!(px(&mut s, 5, 5), vec![0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn arrange_and_group() {
    let mut s = session_with_doc();
    let a = s.execute("layer.new.layer", json!({"name": "A"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.new.layer", json!({"name": "B"})).unwrap();
    s.execute("layer.select", json!({"layer": a})).unwrap();
    s.execute("layer.arrange.bringToFront", json!({})).unwrap();
    let names: Vec<String> = s.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect();
    assert_eq!(names, ["Background", "B", "A"]);
    assert!(s.execute("layer.arrange.bringForward", json!({})).is_err());
    s.execute("layer.groupLayers", json!({})).unwrap();
    let doc = s.execute("document.inspect", json!({})).unwrap();
    assert_eq!(doc["layers"][0]["kind"], "Group");
    assert_eq!(doc["layers"][0]["children"][0]["name"], "A");
}

#[test]
fn canvas_flips_and_rotation() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 10, "height": 4})).unwrap();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 2, "height": 1})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("image.imageRotation.flipCanvasHorizontal", json!({})).unwrap();
    assert_eq!(px(&mut s, 9, 0), vec![1.0, 0.0, 0.0, 1.0]);
    assert_eq!(px(&mut s, 0, 0), vec![1.0, 1.0, 1.0, 1.0]);
    s.execute("image.imageRotation.flipCanvasVertical", json!({})).unwrap();
    assert_eq!(px(&mut s, 9, 3), vec![1.0, 0.0, 0.0, 1.0]);
    s.execute("image.imageRotation.90cw", json!({})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!((d.size.width, d.size.height), (4, 10));
    assert_eq!(px(&mut s, 0, 9), vec![1.0, 0.0, 0.0, 1.0]);
    s.execute("image.imageRotation.90ccw", json!({})).unwrap();
    s.execute("image.imageRotation.180", json!({})).unwrap();
    assert_eq!(px(&mut s, 0, 0), vec![1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn image_rotation_rotates_every_layer_not_just_the_active_one() {
    // Issue #1: Image Rotation must rotate the whole canvas, not only the selected layer.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 10, "background": "white"})).unwrap();
    // A second (lower) layer with a red dot at the top-left; keep a third layer active.
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("dot", |doc, a| {
        doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(2, 2, 4, 4), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    let dotted = s.active().unwrap().active_layer.unwrap();
    s.execute("layer.new.layer", json!({})).unwrap(); // a different, empty active layer
    assert_ne!(s.active().unwrap().active_layer.unwrap(), dotted, "active layer is not the dotted one");

    s.execute("image.imageRotation.180", json!({})).unwrap();

    // The non-active layer's content rotated too: (2,2)..(4,4) in 20x10 → (16,6)..(18,8).
    let st = s.active().unwrap();
    let surf = st.doc.layer(dotted).unwrap().surface().unwrap();
    assert_eq!(surf.rgba(17, 6), [1.0, 0.0, 0.0, 1.0], "dot moved to the opposite corner");
    assert_eq!(surf.rgba(3, 3)[3], 0.0, "original spot is now empty");
    // The Background layer rotated as well (still fully opaque white everywhere).
    assert_eq!(st.doc.layers[0].surface().unwrap().rgba(1, 1), [1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn fill_layers_and_colors() {
    let mut s = session_with_doc();
    s.execute("tools.setColors", json!({"foreground": "#112233"})).unwrap();
    s.execute("tools.swapColors", json!({})).unwrap();
    assert_eq!(s.tools.foreground, [1.0, 1.0, 1.0, 1.0]);
    s.execute("tools.defaultColors", json!({})).unwrap();
    s.execute("layer.newFillLayer.solidColor", json!({"color": "#ff00ff"})).unwrap();
    assert_eq!(px(&mut s, 1, 1), vec![1.0, 0.0, 1.0, 1.0]);
    s.execute("layer.newFillLayer.gradient", json!({"from": "#000000", "to": "#ffffff", "angle": 0})).unwrap();
    let l = px(&mut s, 0, 10)[0];
    let r = px(&mut s, 63, 10)[0];
    assert!(l < r);
}

#[test]
fn journal_records_mutations_only() {
    let mut s = session_with_doc();
    s.execute("document.inspect", json!({})).unwrap();
    s.execute("layer.new.layer", json!({"name": "x"})).unwrap();
    let ids: Vec<&str> = s.journal.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["file.new", "layer.new.layer"]);
    // replaying the journal reproduces the document
    let mut replay = Session::new();
    for (id, p) in s.journal.clone() {
        replay.execute(&id, p).unwrap();
    }
    assert_eq!(replay.active().unwrap().doc.layer_count(), s.active().unwrap().doc.layer_count());
}

#[test]
fn command_list_reports_enablement() {
    let mut s = Session::new();
    let list = s.execute("command.list", json!({})).unwrap();
    let find = |id: &str| list.as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!(find("file.new")["enabled"], true);
    assert_eq!(find("layer.new.layer")["enabled"], false);
}

#[test]
fn translate_moves_pixels_and_respects_locks() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.translate", json!({"dx": 10, "dy": 5})).unwrap();
    assert_eq!(px(&mut s, 11, 6), vec![1.0, 0.0, 0.0, 1.0]);
    assert_eq!(px(&mut s, 1, 1), vec![1.0, 1.0, 1.0, 1.0]);
    // Background is position-locked
    let bg = s.active().unwrap().doc.layers[0].id;
    s.select_layer(bg).unwrap();
    assert!(s.execute("layer.translate", json!({"dx": 1, "dy": 0})).is_err());
}

#[test]
fn damage_is_reported_for_strokes_only() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    assert_eq!(s.active().unwrap().last_damage, None);
    s.execute("paint.stroke", json!({"points": [[10, 10], [20, 10]], "size": 4})).unwrap();
    let d = s.active().unwrap().last_damage.unwrap();
    assert!(d.contains(15, 10) && d.width() < 30);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().last_damage, None);
}

#[test]
fn integer_params_accept_json_floats() {
    // UIs send coordinates as floats (e.g. 12.0); every integer parameter must accept them.
    let mut s = session_with_doc();
    s.execute("select.rect", json!({"x": 1.0, "y": 2.0, "width": 10.0, "height": 5.4})).unwrap();
    assert_eq!(s.execute("document.inspect", json!({})).unwrap()["selectionBounds"], json!([1, 2, 10, 5]));
    let px: Vec<f32> = serde_json::from_value(s.execute("document.pixel", json!({"x": 3.0, "y": 3.0})).unwrap()).unwrap();
    assert_eq!(px, vec![1.0, 1.0, 1.0, 1.0]);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("layer.translate", json!({"dx": 2.0, "dy": -1.0})).unwrap();
}

#[test]
fn move_to_reorders_and_nests() {
    let mut s = session_with_doc();
    let a = s.execute("layer.new.layer", json!({"name": "A"})).unwrap()["layer"].as_u64().unwrap();
    let b = s.execute("layer.new.layer", json!({"name": "B"})).unwrap()["layer"].as_u64().unwrap();
    let g = s.execute("layer.new.group", json!({"name": "G"})).unwrap()["layer"].as_u64().unwrap();
    let names = |s: &Session| s.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    // bottom->top: Background, A, B, G ; move A above B
    s.execute("layer.moveTo", json!({"layer": a, "target": b, "position": "above"})).unwrap();
    assert_eq!(names(&s), ["Background", "B", "A", "G"]);
    s.execute("layer.moveTo", json!({"layer": a, "target": g, "position": "into"})).unwrap();
    assert_eq!(names(&s), ["Background", "B", "G"]);
    assert_eq!(s.execute("document.inspect", json!({})).unwrap()["layers"][0]["children"][0]["name"], "A");
    // groups can't go into themselves
    assert!(s.execute("layer.moveTo", json!({"layer": g, "target": a, "position": "into"})).is_err());
    // one undo step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(names(&s), ["Background", "B", "A", "G"]);
}

#[test]
fn selecting_a_layer_does_not_dirty_the_document() {
    let mut s = session_with_doc();
    let bg = s.active().unwrap().doc.layers[0].id;
    assert!(!s.active().unwrap().is_dirty());
    s.execute("layer.select", json!({"layer": bg.0})).unwrap();
    assert!(!s.active().unwrap().is_dirty());
    s.execute("layer.new.layer", json!({})).unwrap();
    assert!(s.active().unwrap().is_dirty());
}

#[test]
fn coalesced_edits_share_one_history_step() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let steps = |s: &Session| s.active().unwrap().history.entries().len();
    let base = steps(&s);
    for o in [10, 20, 30] {
        s.execute("layer.setProps", json!({"opacity": o as f64 / 100.0, "coalesce": "drag-1"})).unwrap();
    }
    assert_eq!(steps(&s), base + 1, "three coalesced edits = one step");
    // A different key starts a new step; an uncoalesced edit breaks the chain.
    s.execute("layer.setProps", json!({"opacity": 0.4, "coalesce": "drag-2"})).unwrap();
    s.execute("layer.setProps", json!({"opacity": 0.5})).unwrap();
    s.execute("layer.setProps", json!({"opacity": 0.6, "coalesce": "drag-2"})).unwrap();
    assert_eq!(steps(&s), base + 4);
    // Undo returns to the state before the whole coalesced run.
    s.undo();
    s.undo();
    s.undo();
    s.undo();
    let st = s.active().unwrap();
    let l = st.doc.layer(st.active_layer.unwrap()).unwrap().opacity;
    assert!((l - 1.0).abs() < 1e-6, "{l}");
}

#[test]
fn type_edit_rerenders_cache_to_new_text() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 800, "height": 400})).unwrap();
    let r = s.execute("type.create", json!({"x": 50, "y": 200, "text": "Lorem Ipsum", "size": 48, "coalesce": "k"})).unwrap();
    let id = r["layer"].as_u64().unwrap();
    let width = |s: &Session| {
        let st = s.active().unwrap();
        st.doc.layer(photocraft_doc::LayerId(id)).unwrap().surface().unwrap().content_bounds().width()
    };
    let w0 = width(&s);
    s.execute("type.edit", json!({"layer": id, "replace": {"start": 0, "end": 11, "text": "Photocraft"}, "coalesce": "k"})).unwrap();
    let w1 = width(&s);
    assert!(w0 > 150 && w1 > 150, "cache widths {w0} → {w1}");
}

#[test]
fn levels_and_curves_params_cover_output_and_channels() {
    use photocraft_doc::Adjustment;
    let a = crate::commands::adjustment_from_params("levels", &json!({"inBlack": 10, "outWhite": 200, "green": {"gamma": 1.5}}));
    let Adjustment::Levels { master, per_channel, .. } = a else { panic!() };
    assert!((master.in_black - 10.0 / 255.0).abs() < 1e-6 && (master.out_white - 200.0 / 255.0).abs() < 1e-6);
    assert_eq!(per_channel[1].gamma, 1.5);
    assert_eq!(per_channel[0].gamma, 1.0);
    let c = crate::commands::adjustment_from_params("curves", &json!({"points": [[0, 0], [128, 160], [255, 255]], "blue": [[0, 20], [255, 235]]}));
    let Adjustment::Curves { master, per_channel, .. } = c else { panic!() };
    assert_eq!(master.len(), 3);
    assert!((per_channel[2][0].output - 20.0 / 255.0).abs() < 1e-6);
    assert_eq!(per_channel[0].len(), 2);
}

#[test]
fn painting_can_target_the_layer_mask() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    s.edit("mask", |doc, _| {
        doc.layer_mut(id).unwrap().mask = Some(photocraft_doc::LayerMask::reveal_all());
        Ok(())
    })
    .unwrap();
    // Adjustment layers are paintable through their mask only.
    assert!(s.execute("paint.stroke", json!({"points": [[20, 20]], "size": 10, "hardness": 1.0, "color": "#000000"})).is_err());
    s.execute("paint.stroke", json!({"points": [[20, 20]], "size": 10, "hardness": 1.0, "color": "#000000", "target": "mask"})).unwrap();
    let m = |s: &Session, x, y| s.active().unwrap().doc.layer(id).unwrap().mask.as_ref().unwrap().surface.pixel(x, y)[0];
    assert!(m(&s, 20, 20) < 0.01, "painted black");
    assert!(m(&s, 2, 2) > 0.99, "rest still revealed");
    // Eraser on a mask paints the background colour (white).
    s.execute("paint.stroke", json!({"points": [[20, 20]], "size": 10, "hardness": 1.0, "erase": true, "target": "mask"})).unwrap();
    assert!(m(&s, 20, 20) > 0.99);
    // Gradient into the mask.
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [40, 0], "colors": ["#000000", "#ffffff"], "target": "mask"})).unwrap();
    assert!(m(&s, 2, 20) < 0.1 && m(&s, 38, 20) > 0.9);
}

#[test]
fn layer_locks_are_set_and_enforced() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("layer.setProps", json!({"locks": {"pixels": true}})).unwrap();
    assert!(s.execute("paint.stroke", json!({"points": [[5, 5]], "size": 4})).is_err(), "pixel lock blocks painting");
    s.execute("layer.setProps", json!({"locks": {"pixels": false, "position": true}})).unwrap();
    s.execute("paint.stroke", json!({"points": [[5, 5]], "size": 4})).unwrap();
    assert!(s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 3, 0]})).is_err(), "position lock blocks transforms");
    assert!(s.execute("layer.setProps", json!({"locks": {"bogus": true}})).is_err());
    let st = s.active().unwrap();
    let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
    assert!(l.locks.position && !l.locks.pixels);
}

#[test]
fn moving_a_layer_moves_its_effects_reference_point() {
    let mut s = session_with_doc();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("ref", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().effects.reference = Some((-201.0, 9.0));
        Ok(())
    })
    .unwrap();
    s.execute("layer.translate", json!({"dx": 10, "dy": 5})).unwrap();
    let d = s.active().unwrap();
    let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
    assert_eq!(l.effects.reference, Some((-191.0, 14.0)));
}

#[test]
fn marquee_feather_and_anti_aliased_ellipse() {
    let sel_at = |s: &Session, x: i32, y: i32| {
        let mut v = [0.0f32];
        if let Some(m) = s.active().unwrap().doc.selection.as_ref() {
            m.read_pixel(x, y, &mut v);
        }
        v[0]
    };
    // Hard rectangle: fully in or out.
    let mut s = session_with_doc();
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20})).unwrap();
    assert_eq!(sel_at(&s, 10, 20), 1.0);
    assert_eq!(sel_at(&s, 9, 20), 0.0);
    // Feather softens only the new shape's edge: partial coverage across the boundary.
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 20, "height": 20, "feather": 3})).unwrap();
    let (inside, edge, outside) = (sel_at(&s, 20, 20), sel_at(&s, 10, 20), sel_at(&s, 8, 20));
    assert!(inside > 0.95 && edge > 0.2 && edge < 0.8 && outside > 0.0 && outside < edge, "{inside} {edge} {outside}");
    // Anti-aliased ellipse edges have partial coverage; aliased ones don't.
    let partial = |s: &Session| (0..48).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| (0.01..0.99).contains(&sel_at(s, x, y))).count();
    s.execute("select.rect", json!({"x": 5, "y": 5, "width": 40, "height": 30, "ellipse": true})).unwrap();
    assert!(partial(&s) > 20);
    s.execute("select.rect", json!({"x": 5, "y": 5, "width": 40, "height": 30, "ellipse": true, "antiAlias": false})).unwrap();
    assert_eq!(partial(&s), 0);
}

#[test]
fn file_new_resolution_and_background_color() {
    let mut s = Session::new();
    s.tools.background = [1.0, 0.0, 0.0, 1.0];
    s.execute("file.new", json!({"width": 8, "height": 8, "resolution": 300, "background": "backgroundColor"})).unwrap();
    assert_eq!(s.active().unwrap().doc.resolution_dpi, 300.0);
    let p = px(&mut s, 2, 2);
    assert!(p[0] > 0.99 && p[1] < 0.01, "{p:?}");
}

#[test]
fn duplicating_the_background_unlocks_the_copy() {
    let mut s = session_with_doc();
    s.execute("layer.duplicate", json!({})).unwrap();
    let st = s.active().unwrap();
    let copy = st.doc.layer(st.active_layer.unwrap()).unwrap();
    assert_eq!(copy.name, "Background copy");
    assert_eq!(copy.locks, photocraft_doc::Locks::default());
    assert!(st.doc.layers[0].locks.transparency);
}

#[test]
fn advanced_blending_channels() {
    let mut s = session_with_doc();
    let r = s.execute("layer.new.layer", json!({})).unwrap();
    let id = r["layer"].as_u64().unwrap();
    s.execute("layer.setProps", json!({"channels": [true, true, false]})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(LayerId(id)).unwrap().excluded_channels, 0b100);
    let ins = crate::inspect::layer(s.active().unwrap().doc.layer(LayerId(id)).unwrap());
    assert_eq!(ins["channels"], json!([true, true, false, true]));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(LayerId(id)).unwrap().excluded_channels, 0);
}

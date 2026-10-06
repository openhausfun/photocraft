use super::*;
use photocraft_doc::vector::Subpath;

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": depth, "background": "white"})).unwrap();
    s
}

fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
    let st = s.active().unwrap();
    st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
}

fn active(s: &Session) -> &Layer {
    let st = s.active().unwrap();
    st.doc.layer(st.active_layer.unwrap()).unwrap()
}

#[test]
fn fade_blends_last_step_at_all_depths() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        assert!(!s.is_enabled("edit.fade"), "nothing to fade yet");
        s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        assert!(s.is_enabled("edit.fade"));
        let undo_steps = s.active().unwrap().history.past_len();
        s.execute("edit.fade", json!({"opacity": 25})).unwrap();
        let v = px(&s, 5, 5);
        assert!((v[0] - 0.75).abs() < 0.01, "depth {depth}: {v:?}");
        // One history step, labelled after the faded command, and Fade can't repeat.
        assert_eq!(s.active().unwrap().history.past_len(), undo_steps + 1);
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Fade Fill"));
        assert!(!s.is_enabled("edit.fade"));
        s.undo();
        assert_eq!(px(&s, 5, 5)[0], 0.0);
    }
}

#[test]
fn fade_with_blend_mode_and_invalidation() {
    let mut s = session(8);
    s.execute("edit.fill", json!({"color": "#808080"})).unwrap();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    // Difference of grey and inverted grey at 100%: |0.502 − 0.498| ≈ 0.
    s.execute("edit.fade", json!({"opacity": 100, "mode": "difference"})).unwrap();
    assert!(px(&s, 1, 1)[0] < 0.02);
    // Any other edit in between disables Fade.
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    assert!(!s.is_enabled("edit.fade"));
    assert!(s.execute("edit.fade", json!({})).is_err());
    // Queries don't.
    s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
    s.execute("document.inspect", json!({})).unwrap();
    assert!(s.is_enabled("edit.fade"));
    assert!(s.execute("edit.fade", json!({"mode": "nonsense"})).is_err());
}

#[test]
fn purge_commands() {
    let mut s = session(8);
    assert!(!s.is_enabled("edit.purge.undo"));
    assert!(s.is_enabled("edit.purge.videoCache"));
    s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    let r = s.execute("edit.purge.undo", json!({})).unwrap();
    assert_eq!(r["purged"][0], "undo");
    assert!(r["bytes"].as_u64().unwrap() > 0);
    // One step left to undo (the black fill is gone from history).
    assert!(s.undo());
    assert!(!s.undo());
    s.execute("select.all", json!({})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.is_enabled("edit.purge.clipboard"));
    s.execute("edit.purge.clipboard", json!({})).unwrap();
    assert!(s.clipboard.is_none());
    assert!(!s.is_enabled("edit.purge.clipboard"));
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
    s.execute("edit.purge.histories", json!({})).unwrap();
    assert!(s.documents().iter().all(|d| !d.history.can_undo()));
    assert!(!s.is_enabled("edit.purge.histories"));
    assert_eq!(s.execute("edit.purge.videoCache", json!({})).unwrap()["bytes"], 0);
    let _ = s.execute("edit.purge.all", json!({}));
}

/// White canvas with vertical black stripes, a red blob to remove, and a selection around it.
fn blob_session(depth: u32) -> Session {
    let mut s = session(depth);
    s.edit("stripes", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        for x in (0..64).step_by(8) {
            surf.fill_rect(Rect::new(x, 0, x + 3, 48), &[0.0, 0.0, 0.0, 1.0]);
        }
        surf.fill_rect(Rect::new(28, 18, 36, 28), &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 26, "y": 16, "width": 12, "height": 14})).unwrap();
    s
}

#[test]
fn content_aware_fill_removes_object_at_all_depths() {
    for depth in [8, 16, 32] {
        let mut s = blob_session(depth);
        assert!(s.is_enabled("edit.contentAwareFill"));
        let r = s.execute("edit.contentAwareFill", json!({"colorAdaptation": "none"})).unwrap();
        assert!(r["filled"].as_u64().unwrap() > 100);
        for (x, y) in [(30, 20), (33, 25)] {
            let v = px(&s, x, y);
            assert!(!(v[0] > 0.9 && v[1] < 0.1), "depth {depth}: red left at ({x},{y}): {v:?}");
        }
        // Outside the selection untouched.
        assert_eq!(px(&s, 1, 1)[0], 0.0);
        assert_eq!(px(&s, 4, 1)[0], 1.0);
        s.undo();
        assert_eq!(px(&s, 30, 20)[1], 0.0, "undo restores the blob");
    }
}

#[test]
fn content_aware_fill_outputs_and_sampling() {
    let mut s = blob_session(8);
    let base = s.active().unwrap().active_layer.unwrap();
    let r = s.execute("edit.contentAwareFill", json!({"output": "new", "sampling": "rectangular", "margin": 12})).unwrap();
    let nid = LayerId(r["layer"].as_u64().unwrap());
    assert_ne!(nid, base);
    // The new layer holds only the fill.
    let l = s.active().unwrap().doc.layer(nid).unwrap();
    let b = l.surface().unwrap().content_bounds();
    assert!(Rect::new(26, 16, 38, 30).contains_rect(&b), "{b:?}");
    s.undo();
    let r = s.execute("edit.contentAwareFill", json!({"output": "duplicate", "sampling": "custom", "area": [0, 0, 20, 48], "mirror": true})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap().name, "Background copy");
    assert!(s.execute("edit.contentAwareFill", json!({"sampling": "custom"})).is_err());
    assert!(s.execute("edit.contentAwareFill", json!({"output": "elsewhere"})).is_err());
    s.execute("select.deselect", json!({})).unwrap();
    assert!(!s.is_enabled("edit.contentAwareFill"));
}

#[test]
fn content_aware_scale_keeps_subject() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("paint", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            surf.fill_rect(Rect::new(0, 0, 64, 48), &[0.5, 0.5, 0.5, 1.0]);
            for x in 20..26 {
                let c = if x % 2 == 0 { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] };
                surf.fill_rect(Rect::new(x, 0, x + 1, 48), &c);
            }
            Ok(())
        })
        .unwrap();
        let r = s.execute("edit.contentAwareScale", json!({"width": 40})).unwrap();
        assert_eq!(r["to"], json!([40, 48]));
        let surf = active(&s).surface().unwrap();
        assert_eq!(surf.content_bounds(), Rect::new(0, 0, 40, 48), "depth {depth}");
        let striped = (0..40).filter(|x| {
            let p = surf.pixel(*x, 10);
            p[1] < 0.05 && (p[0] > 0.95 || p[2] > 0.95)
        });
        assert_eq!(striped.count(), 6, "depth {depth}: the striped subject survives");
        s.undo();
        assert_eq!(active(&s).surface().unwrap().content_bounds().width(), 64);
    }
}

#[test]
fn content_aware_scale_params() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 32, 32), &[0.8, 0.6, 0.5, 1.0]);
        Ok(())
    })
    .unwrap();
    let r = s.execute("edit.contentAwareScale", json!({"scaleX": 150, "scaleY": 50, "amount": 50, "protectSkinTones": true})).unwrap();
    assert_eq!(r["to"], json!([48, 16]));
    assert!(s.execute("edit.contentAwareScale", json!({"width": 0})).is_err());
    assert!(s.execute("edit.contentAwareScale", json!({"protect": "no such channel"})).is_err());
}

fn square_path(x: f64, y: f64, w: f64) -> Path {
    Path::new(vec![Subpath::polygon(&[(x, y), (x + w, y), (x + w, y + w), (x, y + w)])])
}

#[test]
fn define_custom_shape_and_preset_manager() {
    let mut s = session(8);
    assert!(!s.is_enabled("edit.defineCustomShape"));
    s.edit("path", |doc, _| {
        doc.work_path = Some(square_path(4.0, 4.0, 10.0));
        Ok(())
    })
    .unwrap();
    s.execute("edit.defineCustomShape", json!({"name": "Box"})).unwrap();
    s.execute("edit.defineCustomShape", json!({})).unwrap();
    assert_eq!(custom_shapes(&s).len(), 2);
    assert_eq!(custom_shapes(&s)[1].name, "Shape 2");
    let list = s.execute("edit.presets.presetManager", json!({})).unwrap();
    assert_eq!(list["customShapes"], json!(["Box", "Shape 2"]));
    assert!(list["brushes"].as_array().unwrap().len() > 3);
    s.execute("edit.presets.presetManager", json!({"action": "rename", "kind": "customShapes", "name": "Shape 2", "newName": "Star"})).unwrap();
    s.execute("edit.presets.presetManager", json!({"action": "move", "kind": "customShapes", "index": 1, "to": 0})).unwrap();
    assert_eq!(custom_shapes(&s)[0].name, "Star");
    s.execute("edit.presets.presetManager", json!({"action": "delete", "kind": "customShapes", "name": "Box"})).unwrap();
    assert_eq!(custom_shapes(&s).len(), 1);
    assert!(s.execute("edit.presets.presetManager", json!({"action": "delete", "kind": "gradients", "index": 0})).is_err());
    // The pattern library (Edit › Define Pattern) is managed here too.
    let n = list["patterns"].as_array().unwrap().len();
    assert!(n > 0);
    s.execute("edit.presets.presetManager", json!({"action": "rename", "kind": "patterns", "index": 0, "newName": "First"})).unwrap();
    s.execute("edit.presets.presetManager", json!({"action": "delete", "kind": "patterns", "name": "First"})).unwrap();
    assert_eq!(s.patterns.items.len(), n - 1);
    // Export → import into a fresh session.
    s.execute("brush.presets.save", json!({"name": "Mine"})).unwrap();
    let out = s.execute("edit.presets.exportImportPresets", json!({"action": "export"})).unwrap();
    assert_eq!(out["brushes"], 1, "built-ins are not exported by default");
    let mut t = Session::new();
    let r = t.execute("edit.presets.exportImportPresets", json!({"action": "import", "data": out["data"].to_string()})).unwrap();
    assert_eq!((r["brushes"].as_u64(), r["customShapes"].as_u64()), (Some(1), Some(1)));
    assert!(t.tools.presets.iter().any(|b| b.name == "Mine"));
    assert_eq!(custom_shapes(&t)[0].path, custom_shapes(&s)[0].path);
    assert!(t.execute("edit.presets.exportImportPresets", json!({"action": "import", "data": {"format": "other"}})).is_err());
}

#[test]
fn define_brush_preset_wraps_define_from_selection() {
    let mut s = session(8);
    assert!(!s.is_enabled("edit.defineBrushPreset"));
    s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
    s.execute("select.rect", json!({"x": 2, "y": 2, "width": 10, "height": 10})).unwrap();
    s.execute("edit.defineBrushPreset", json!({})).unwrap();
    assert!(s.tools.presets.iter().any(|b| b.name == "Sampled Brush 1"));
}

#[test]
fn find_matches_cases() {
    assert_eq!(find_matches("Cat cat CAT", "cat", false, false), vec![(0, 3), (4, 7), (8, 11)]);
    assert_eq!(find_matches("Cat cat CAT", "cat", true, false), vec![(4, 7)]);
    assert_eq!(find_matches("cat catalog bobcat", "cat", false, true), vec![(0, 3)]);
    assert_eq!(find_matches("Straße STRASSE", "straße", false, false), vec![(0, 7)]);
    assert!(find_matches("abc", "", false, false).is_empty());
}

#[test]
fn find_and_replace_across_type_layers() {
    let mut s = session(8);
    assert!(!s.is_enabled("edit.findAndReplaceText"));
    s.execute("type.create", json!({"text": "Hello world", "x": 4, "y": 20, "size": 12})).unwrap();
    s.execute("type.create", json!({"text": "world peace, World", "x": 4, "y": 40, "size": 12})).unwrap();
    let r = s.execute("edit.findAndReplaceText", json!({"find": "world", "replace": "planet"})).unwrap();
    assert_eq!((r["count"].as_u64(), r["layers"].as_u64()), (Some(3), Some(2)));
    let texts: Vec<String> = s
        .active()
        .unwrap()
        .doc
        .walk()
        .into_iter()
        .filter_map(|(_, _, l)| match &l.content {
            LayerContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"Hello planet".to_string()), "{texts:?}");
    assert!(texts.contains(&"planet peace, planet".to_string()), "{texts:?}");
    s.undo();
    // Step through matches: case-sensitive finds only lower-case "world".
    let f1 = s.execute("edit.findAndReplaceText", json!({"find": "world", "action": "find", "caseSensitive": true})).unwrap();
    assert_eq!(f1["found"]["text"], "world");
    let f2 = s.execute("edit.findAndReplaceText", json!({"find": "world", "action": "changeFind", "replace": "Earth", "caseSensitive": true})).unwrap();
    assert!(f2["changed"].is_object());
    assert_eq!(f2["found"]["text"], "world");
    assert_ne!(f2["found"]["layer"], f2["changed"]["layer"]);
    assert!(s.execute("edit.findAndReplaceText", json!({"find": ""})).is_err());
}

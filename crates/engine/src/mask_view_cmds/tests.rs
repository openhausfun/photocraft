use super::*;
use photocraft_geom::Rect;

/// 40×20 document: Background, then "Masked" (red, mask hiding the left half) on top.
fn session() -> (Session, u64, u64) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 20})).unwrap();
    let bg = s.active().unwrap().doc.layers[0].id.0;
    let masked = s.execute("layer.new.layer", json!({"name": "Masked"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("edit.fill", json!({"contents": "color", "color": "#ff0000"})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 0, "width": 20, "height": 20})).unwrap();
    s.execute("layer.layerMask.revealSelection", json!({})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    let st = s.active_mut().unwrap();
    st.saved_revision = st.revision;
    (s, masked, bg)
}

/// The view as `document.inspect` reports it.
fn view(s: &Session) -> Value {
    crate::inspect::document(s.active().unwrap())["layerMaskView"].clone()
}

#[test]
fn modes_toggle_and_show_in_inspect_without_history() {
    let (mut s, masked, _) = session();
    let steps = s.active().unwrap().history.entries().len();
    assert_eq!(view(&s), Value::Null);
    assert_eq!(s.execute(ID, json!({})).unwrap()["mode"], "gray", "no mode: toggle the grayscale view");
    assert_eq!(view(&s), json!({"layer": masked, "mode": "gray"}));
    assert_eq!(s.execute(ID, json!({"mode": "toggleGray"})).unwrap()["mode"], "off", "⌥-click again returns");
    assert_eq!(s.execute(ID, json!({"layer": masked, "mode": "overlay"})).unwrap()["mode"], "overlay");
    assert_eq!(s.active().unwrap().channel_view.layer_mask.unwrap().mode, MaskViewMode::Overlay);
    let ch = crate::channel_cmds::channels_json(s.active().unwrap());
    assert_eq!(ch["layerMaskView"]["mode"], "overlay", "the Channels state carries it too");
    assert_eq!(s.execute(ID, json!({"mode": "toggleGray"})).unwrap()["mode"], "gray", "overlay → gray");
    assert_eq!(s.execute(ID, json!({"mode": "off"})).unwrap()["mode"], "off");
    let st = s.active().unwrap();
    assert_eq!(st.history.entries().len(), steps, "view changes are not history steps");
    assert!(!st.is_dirty(), "a clean document stays clean");
    assert_eq!(st.last_damage, Some(Rect::EMPTY), "no recomposite");
}

#[test]
fn a_mask_view_never_alters_pixels() {
    let (mut s, masked, _) = session();
    let before = s.active().unwrap().doc.clone();
    let px = |s: &Session| photocraft_compose::render(&s.active().unwrap().doc, Rect::new(0, 0, 40, 20)).px;
    let composite = px(&s);
    for mode in ["gray", "overlay", "toggleOverlay", "toggleGray", "off"] {
        s.execute(ID, json!({"layer": masked, "mode": mode})).unwrap();
        assert!(std::sync::Arc::ptr_eq(&before, &s.active().unwrap().doc), "{mode}: the document is untouched");
        assert_eq!(px(&s), composite, "{mode}: same composite");
    }
}

#[test]
fn painting_in_mask_view_paints_the_mask() {
    let (mut s, masked, _) = session();
    s.execute(ID, json!({"mode": "gray"})).unwrap();
    let id = photocraft_doc::LayerId(masked);
    let pixels_before = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().clone();
    s.execute("paint.stroke", json!({"points": [[30, 10]], "size": 6, "hardness": 1.0, "color": "#000000"})).unwrap();
    let l = s.active().unwrap().doc.layer(id).unwrap().clone();
    assert!(l.mask.as_ref().unwrap().value(30, 10) < 0.01, "the stroke went into the mask");
    let mut a = Vec::new();
    let mut b = Vec::new();
    pixels_before.read_region_into(Rect::new(0, 0, 40, 20), &mut a);
    l.surface().unwrap().read_region_into(Rect::new(0, 0, 40, 20), &mut b);
    assert_eq!(a, b, "the layer's pixels are untouched");
    // An explicit target still wins.
    s.execute("paint.stroke", json!({"points": [[5, 5]], "size": 4, "color": "#00ff00", "target": "pixels"})).unwrap();
    let l = s.active().unwrap().doc.layer(id).unwrap();
    assert!(l.surface().unwrap().sample_channel(5, 5, 1) > 0.9);
}

#[test]
fn the_view_ends_with_its_layer_or_mask() {
    let (mut s, masked, bg) = session();
    s.execute(ID, json!({"mode": "gray"})).unwrap();
    s.execute("layer.select", json!({"layer": bg})).unwrap();
    assert_eq!(view(&s), Value::Null, "selecting another layer leaves mask view");
    assert!(s.active().unwrap().channel_view.layer_mask.is_none());
    // ⌥-clicking a mask of an unselected layer selects it.
    s.execute(ID, json!({"layer": masked, "mode": "overlay"})).unwrap();
    assert_eq!(s.active().unwrap().active_layer, Some(photocraft_doc::LayerId(masked)));
    s.execute("layer.layerMask.delete", json!({})).unwrap();
    assert!(s.active().unwrap().channel_view.layer_mask.is_none(), "deleting the mask ends the view");
    s.undo();
    assert!(s.active().unwrap().channel_view.layer_mask.is_none(), "undo doesn't bring a stale view back");
}

#[test]
fn bad_params_and_no_mask_fail_gracefully() {
    let (mut s, masked, bg) = session();
    for p in [
        json!({"layer": bg, "mode": "gray"}),
        json!({"layer": bg}),
        json!({"layer": 999_999, "mode": "gray"}),
        json!({"layer": "x"}),
        json!({"layer": -1}),
        json!({"mode": 3}),
        json!({"mode": "sideways"}),
        json!({"layer": masked, "mode": ["gray"]}),
    ] {
        assert!(s.execute(ID, p.clone()).is_err(), "{p} should fail");
    }
    assert_eq!(view(&s), Value::Null, "failures change nothing");
    // "off" is always fine, even for a layer without a mask, and leaves another view alone.
    s.execute(ID, json!({"layer": masked, "mode": "gray"})).unwrap();
    assert_eq!(s.execute(ID, json!({"layer": bg, "mode": "off"})).unwrap()["mode"], "off");
    assert_eq!(view(&s)["mode"], "gray");
    // No document.
    let mut empty = Session::new();
    assert!(empty.execute(ID, json!({"mode": "gray"})).is_err());
}

#[test]
fn load_selection_from_a_vector_mask() {
    let (mut s, masked, _) = session();
    let path = json!({"subpaths": [{"knots": [[4, 4], [16, 4], [16, 16], [4, 16]]}]});
    s.execute("layer.vectorMask.add", json!({"layer": masked, "path": path})).unwrap();
    s.execute("layer.vectorMask.enabled", json!({"enabled": false})).unwrap();
    s.execute("select.loadSelection", json!({"channel": "vectorMask", "layer": masked})).unwrap();
    let b = s.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
    assert_eq!((b.x0, b.y0, b.width(), b.height()), (4, 4, 12, 12), "the path's shape, even when disabled");
    // ⌘⌥: subtract, ⌘⇧⌥: intersect.
    s.execute("select.all", json!({})).unwrap();
    s.execute("select.loadSelection", json!({"channel": "vectorMask", "operation": "subtract"})).unwrap();
    let sel = s.active().unwrap().doc.selection.clone().unwrap();
    assert!(sel.sample_channel(10, 10, 0) < 0.01 && sel.sample_channel(30, 10, 0) > 0.99);
    s.execute("select.loadSelection", json!({"channel": "vectorMask", "operation": "intersect"})).unwrap();
    assert!(s.active().unwrap().doc.selection.is_none(), "nothing left");
    // No vector mask: an error, not a crash.
    s.execute("layer.vectorMask.delete", json!({})).unwrap();
    assert!(s.execute("select.loadSelection", json!({"channel": "vectorMask"})).is_err());
}

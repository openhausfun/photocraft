use super::*;

/// 80×60 document, a new layer with a patterned 50×30 block at (10, 10).
fn session(depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 80, "height": 60, "depth": depth})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let l = doc.layer_mut(active.unwrap()).unwrap();
        let surf = l.surface_mut().unwrap();
        for y in 10..40 {
            for x in 10..60 {
                let v = ((x / 3 + y / 3) % 2) as f32;
                surf.write_pixel(x, y, &[v, 1.0 - v, (x as f32) / 80.0, 1.0]);
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

fn worst(a: &Surface, b: &Surface, r: Rect) -> f32 {
    a.read_region(r).iter().zip(b.read_region(r)).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn smart(s: &Session) -> photocraft_doc::SmartObject {
    let st = s.active().unwrap();
    match &st.doc.layer(st.active_layer.unwrap()).unwrap().content {
        LayerContent::Smart(sm) => sm.clone(),
        _ => panic!("not smart"),
    }
}

#[test]
fn liquify_identity_stroke_displacement_reconstruct_and_undo() {
    for depth in [8u64, 16, 32] {
        let mut s = session(depth);
        let before = active_surface(&s);
        let r = s.execute(LIQUIFY, json!({})).unwrap();
        assert_eq!(r["changed"], false);
        let r = s.execute(LIQUIFY, json!({"strokes": [{"tool": "forwardWarp", "size": 30, "pressure": 0, "points": [[30, 25], [45, 25]]}]})).unwrap();
        assert_eq!(r["changed"], false, "zero pressure is a no-op");
        let hist = s.active().unwrap().history.past_len();
        let stroke = json!({"tool": "forwardWarp", "size": 30, "points": [[30, 25], [42, 25]]});
        let r = s.execute(LIQUIFY, json!({"strokes": [stroke]})).unwrap();
        assert_eq!(r["changed"], true);
        assert!(r["maxDisplacement"].as_f64().unwrap() > 4.0);
        assert_eq!(s.active().unwrap().history.past_len(), hist + 1, "one history step");
        let after = active_surface(&s);
        // The pattern moved right: the output at (42, 25) shows what was a few px to the left.
        let d = (0..12).map(|k| worst_px(&after.pixel(42, 25), &before.pixel(42 - k, 25))).enumerate().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0;
        assert!(d >= 3, "{depth}: displaced by {d}");
        assert_ne!(after, before);
        // Reconstruct All in the same stroke list restores the original.
        let mut s2 = session(depth);
        s2.execute(LIQUIFY, json!({"strokes": [stroke, {"tool": "reconstructAll", "amount": 100}]})).unwrap();
        assert!(worst(&active_surface(&s2), &before, Rect::new(0, 0, 80, 60)) <= 1.0 / 255.0);
        s.undo();
        assert_eq!(active_surface(&s), before);
        assert!(s.execute(LIQUIFY, json!({"strokes": [{"tool": "melt", "points": [[1, 1]]}]})).is_err());
        assert!(s.execute(LIQUIFY, json!({"strokes": [{"points": [[1]]}]})).is_err());
        assert!(s.execute(LIQUIFY, json!({"strokes": [], "meshSize": 0})).is_err());
    }
}

fn worst_px(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn liquify_respects_the_selection() {
    let mut s = session(8);
    let before = active_surface(&s);
    s.execute("select.all", json!({})).ok();
    s.edit("sel", |doc, _| {
        let mut sel = Surface::new(PixelFormat::GRAY8);
        sel.fill_rect(Rect::new(0, 0, 35, 60), &[1.0]);
        doc.selection = Some(sel);
        Ok(())
    })
    .unwrap();
    s.execute(LIQUIFY, json!({"strokes": [{"tool": "bloat", "size": 60, "points": [[35, 25], [35, 25], [35, 25], [35, 25]]}]})).unwrap();
    let after = active_surface(&s);
    assert_eq!(after.read_region(Rect::new(36, 0, 80, 60)), before.read_region(Rect::new(36, 0, 80, 60)), "unselected pixels stay");
    assert_ne!(after.read_region(Rect::new(10, 10, 35, 40)), before.read_region(Rect::new(10, 10, 35, 40)));
}

#[test]
fn puppet_identity_translation_and_undo() {
    for depth in [8u64, 16, 32] {
        let mut s = session(depth);
        let before = active_surface(&s);
        assert_eq!(s.execute(PUPPET, json!({})).unwrap()["changed"], false);
        assert_eq!(s.execute(PUPPET, json!({"pins": [{"src": [20, 20], "dst": [20, 20]}]})).unwrap()["changed"], false);
        s.execute(PUPPET, json!({"pins": [{"src": [30, 25], "dst": [36, 29]}], "mode": "rigid", "interpolation": "bilinear"})).unwrap();
        let after = active_surface(&s);
        assert_eq!(after.content_bounds(), Rect::new(16, 14, 66, 44), "{depth}");
        assert!(
            worst(&after.convert(before.format()), &photocraft_algo::resample::translate_surface(&before, 6, 4), Rect::new(16, 14, 66, 44)) <= 1.0 / 255.0,
            "{depth}"
        );
        s.undo();
        assert_eq!(active_surface(&s), before);
        assert!(s.execute(PUPPET, json!({"pins": [{"src": [1, 1]}]})).is_err());
        assert!(s.execute(PUPPET, json!({"pins": [], "mode": "wobbly"})).is_err());
        assert!(s.execute(PUPPET, json!({"pins": [], "density": "lots"})).is_err());
    }
}

#[test]
fn puppet_two_pins_bend_the_block() {
    let mut s = session(8);
    // Hold the left end, lift the right end: the right side rises, the left stays.
    s.execute(PUPPET, json!({"pins": [{"src": [15, 25], "dst": [15, 25]}, {"src": [55, 25], "dst": [55, 10]}], "mode": "normal", "density": "more"})).unwrap();
    let after = active_surface(&s);
    assert!(after.pixel(12, 25)[3] > 0.5, "the held end stays");
    assert!(after.content_bounds().y0 < 6, "{:?}", after.content_bounds());
}

#[test]
fn perspective_identity_known_homography_and_mask() {
    for depth in [8u64, 16, 32] {
        let mut s = session(depth);
        let before = active_surface(&s);
        let q = [[10.0, 10.0], [60.0, 10.0], [60.0, 40.0], [10.0, 40.0]];
        let r = s.execute(PERSPECTIVE, json!({"planes": [{"src": q, "dst": q}]})).unwrap();
        assert_eq!(r["changed"], false);
        // Shift the whole quad by (5, 3): a pure translation of the layer.
        let d = q.map(|p| [p[0] + 5.0, p[1] + 3.0]);
        s.execute(PERSPECTIVE, json!({"planes": [{"src": q, "dst": d}], "interpolation": "bilinear"})).unwrap();
        let after = active_surface(&s);
        assert_eq!(after.content_bounds(), Rect::new(15, 13, 65, 43));
        assert!(
            worst(&after.convert(before.format()), &photocraft_algo::resample::translate_surface(&before, 5, 3), Rect::new(15, 13, 65, 43)) <= 1.0 / 255.0,
            "{depth}"
        );
        s.undo();
        assert_eq!(active_surface(&s), before);
    }
    // A keystone: corners land where asked.
    let mut s = session(8);
    let q = [[10.0, 10.0], [60.0, 10.0], [60.0, 40.0], [10.0, 40.0]];
    let d = [[20.0, 10.0], [50.0, 10.0], [70.0, 50.0], [0.0, 50.0]];
    s.execute("layer.layerMask.revealAll", json!({})).ok();
    s.execute(PERSPECTIVE, json!({"planes": [{"src": q, "dst": d}]})).unwrap();
    let after = active_surface(&s);
    let b = after.content_bounds();
    assert!(b.x0 <= 1 && b.x1 >= 69 && b.y1 >= 49 && b.y0 >= 9, "{b:?}");
    assert!(after.pixel(15, 12)[3] < 0.1, "the top narrowed");
    assert!(s.execute(PERSPECTIVE, json!({})).is_err());
    assert!(s.execute(PERSPECTIVE, json!({"planes": [{"src": [[0, 0], [0, 0], [0, 0], [0, 0]], "dst": [[0, 0], [0, 0], [0, 0], [0, 0]]}]})).is_err());
    assert!(s.execute(PERSPECTIVE, json!({"planes": [{"src": q, "dst": d}], "straighten": "sideways"})).is_err());
}

#[test]
fn smart_objects_store_distortions_as_smart_filters_losslessly() {
    for depth in [8u64, 16, 32] {
        let mut s = session(depth);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        let original = active_surface(&s);
        assert!(s.is_enabled(PUPPET_SMART) && s.is_enabled(PERSPECTIVE_SMART));
        s.execute(LIQUIFY, json!({"strokes": [{"tool": "twirlCw", "size": 40, "points": [[35, 25], [35, 25], [35, 25]]}]})).unwrap();
        let liq = active_surface(&s);
        assert_ne!(liq, original);
        s.execute(PUPPET_SMART, json!({"pins": [{"src": [30, 25], "dst": [34, 25]}], "mode": "rigid"})).unwrap();
        let q = [[10.0, 10.0], [60.0, 10.0], [60.0, 40.0], [10.0, 40.0]];
        s.execute(PERSPECTIVE_SMART, json!({"planes": [{"src": q, "dst": [[12.0, 10.0], [58.0, 10.0], [60.0, 40.0], [10.0, 40.0]]}]})).unwrap();
        let sm = smart(&s);
        let ids: Vec<&str> = sm.smart_filters.iter().map(|f| f.command.as_str()).collect();
        assert_eq!(ids, [LIQUIFY, PUPPET_SMART, PERSPECTIVE_SMART]);
        // Deleting the top two filters re-renders exactly the liquified state; deleting all
        // gives back the original pixels (nothing accumulated).
        s.execute("layer.smartFilter.delete", json!({})).unwrap();
        s.execute("layer.smartFilter.delete", json!({})).unwrap();
        assert_eq!(active_surface(&s), liq, "{depth}");
        s.execute("layer.smartFilter.delete", json!({})).unwrap();
        assert_eq!(active_surface(&s), original, "{depth}");
        s.undo();
        assert_eq!(active_surface(&s), liq);
    }
}

#[test]
fn enabled_states() {
    let mut s = session(8);
    assert!(s.is_enabled(LIQUIFY) && s.is_enabled(PUPPET) && s.is_enabled(PERSPECTIVE));
    assert!(!s.is_enabled(PUPPET_SMART) && !s.is_enabled(PERSPECTIVE_SMART));
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    assert!(!s.is_enabled(PUPPET) && !s.is_enabled(PERSPECTIVE));
    let mut empty = Session::new();
    assert!(!empty.is_enabled(LIQUIFY));
    assert!(empty.execute(PUPPET, json!({"pins": []})).is_err());
}

#[test]
fn background_layer_is_floated_by_geometric_warps() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 30, "depth": 8})).unwrap();
    s.execute(PUPPET, json!({"pins": [{"src": [20, 15], "dst": [25, 15]}], "mode": "rigid"})).unwrap();
    let st = s.active().unwrap();
    let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
    assert_eq!(l.name, "Layer 0");
    assert!(l.surface().unwrap().format().alpha);
    assert!(l.surface().unwrap().pixel(2, 15)[3] < 0.01, "exposed area is transparent");
}

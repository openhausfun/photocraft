use super::*;

fn session(w: u32, h: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "transparent"})).unwrap();
    s
}

fn surface(s: &Session) -> Surface {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().clone()
}

fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
    surface(s).rgba(x, y)
}

fn same_pixels(a: &Surface, b: &Surface, r: Rect) -> bool {
    (r.y0..r.y1).all(|y| (r.x0..r.x1).all(|x| a.pixel(x, y) == b.pixel(x, y)))
}

#[test]
fn stroke_backwards_compatible() {
    let mut s = session(80, 40);
    let r = s.execute("paint.stroke", json!({"points": [[4, 20], [60, 20]], "size": 6, "color": "#0000ff"})).unwrap();
    assert_eq!(r["damage"].as_array().unwrap().len(), 4);
    assert_eq!(rgba(&s, 30, 20), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(rgba(&s, 30, 30)[3], 0.0);
    // [x, y, pressure] with the default pressure-size brush: light pressure → thinner line.
    s.execute("paint.stroke", json!({"points": [[4, 5, 0.2], [60, 5, 0.2]], "size": 10})).unwrap();
    assert!(rgba(&s, 30, 5)[3] > 0.9 && rgba(&s, 30, 8)[3] < 0.01);
}

#[test]
fn stroke_accepts_full_points_brush_and_preset() {
    let mut s = session(200, 100);
    // 7-tuples plus a brush object with tilt-driven angle on a flat tip.
    let brush = json!({"size": 30, "roundness": 0.2, "pressureSize": false, "spacing": 0.1,
        "shapeDynamics": {"enabled": true, "angle": {"control": "penTilt"}}});
    s.execute("paint.stroke", json!({"points": [[50, 50, 1, 0, -45, 0, 0], [52, 50, 1, 0, -45, 0, 16]], "brush": brush})).unwrap();
    // Tilt up = 90°: the flat tip stands vertically.
    assert!(rgba(&s, 50, 40)[3] > 0.9 && rgba(&s, 50, 60)[3] > 0.9 && rgba(&s, 60, 50)[3] < 0.01);
    // Object points.
    s.execute("paint.stroke", json!({"points": [{"x": 150, "y": 20}, {"x": 180, "y": 20, "pressure": 1.0}], "size": 4, "brush": {"pressureSize": false}}))
        .unwrap();
    assert!(rgba(&s, 165, 20)[3] > 0.9);
    // Presets by name.
    s.execute("paint.stroke", json!({"points": [[20, 80], [120, 80]], "preset": "Hard Round"})).unwrap();
    assert!(rgba(&s, 70, 80)[3] > 0.99);
    assert!(s.execute("paint.stroke", json!({"points": [[1, 1]], "preset": "Nope"})).is_err());
    assert!(s.execute("paint.stroke", json!({"points": [[1, 1]], "brush": {"size": "big"}})).is_err());
}

#[test]
fn replay_is_deterministic() {
    let params = json!({"points": [[10, 50], [60, 30, 0.7], [120, 60, 0.9], [180, 40]], "preset": "Spatter", "color": "#ff8800"});
    let run = || {
        let mut s = session(200, 100);
        s.execute("paint.stroke", params.clone()).unwrap();
        s.execute("tools.setBrush", json!({"preset": "Confetti"})).unwrap();
        s.execute("paint.stroke", json!({"points": [[10, 80], [190, 80]]})).unwrap();
        s
    };
    let (a, b) = (run(), run());
    let r = Rect::new(0, 0, 200, 100);
    assert!(same_pixels(&surface(&a), &surface(&b), r));
    // Replaying the journal into a fresh session reproduces the pixels.
    let mut c = session(200, 100);
    for (id, p) in a.journal.iter().skip(1) {
        c.execute(id, p.clone()).unwrap();
    }
    assert!(same_pixels(&surface(&a), &surface(&c), r));
    // A different explicit seed changes the scatter.
    let mut d = session(200, 100);
    let mut p2 = params.clone();
    p2["seed"] = json!(12345);
    d.execute("paint.stroke", p2).unwrap();
    let mut e = session(200, 100);
    e.execute("paint.stroke", params).unwrap();
    assert!(!same_pixels(&surface(&d), &surface(&e), r));
}

#[test]
fn pencil_is_aliased_and_auto_erases() {
    let mut s = session(60, 30);
    s.execute("paint.pencil", json!({"points": [[5, 10.3], [50, 10.3]], "size": 5, "color": "#000000"})).unwrap();
    let surf = surface(&s);
    for y in 0..30 {
        for x in 0..60 {
            let a = surf.rgba(x, y)[3];
            assert!(a == 0.0 || a == 1.0);
        }
    }
    s.execute("tools.setColors", json!({"foreground": "#000000", "background": "#ffffff"})).unwrap();
    // Starting on a foreground pixel: paints background.
    s.execute("paint.pencil", json!({"points": [[20, 10], [20, 25]], "size": 3, "autoErase": true})).unwrap();
    assert_eq!(rgba(&s, 20, 22), [1.0, 1.0, 1.0, 1.0]);
    // Starting elsewhere: paints foreground.
    s.execute("paint.pencil", json!({"points": [[40, 25], [40, 28]], "size": 3, "autoErase": true})).unwrap();
    assert_eq!(rgba(&s, 40, 26), [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn mixer_brush_reservoir_and_cleaning() {
    let mut s = session(200, 30);
    s.execute("tools.setColors", json!({"foreground": "#ff0000"})).unwrap();
    s.execute(
        "paint.mixerBrush",
        json!({"points": [[5, 15], [195, 15]], "size": 8, "wet": 0, "load": 0, "mix": 0, "brush": {"pressureSize": false, "spacing": 0.25}}),
    )
    .unwrap();
    assert!(rgba(&s, 10, 15)[3] > 0.95 && rgba(&s, 190, 15)[3] < 0.5, "low load dries out");
    assert!(s.tools.mixer.pickup.is_none(), "cleaned after stroke");
    // Wet, not cleaned: the pickup persists into the session.
    s.execute("paint.mixerBrush", json!({"points": [[5, 5], [100, 5]], "size": 8, "wet": 100, "mix": 80, "cleanAfterStroke": false})).unwrap();
    assert!(s.tools.mixer.pickup.is_some());
    // Sample All Layers path runs.
    s.execute("paint.mixerBrush", json!({"points": [[5, 25], [50, 25]], "size": 6, "sampleAllLayers": true})).unwrap();
}

#[test]
fn color_replacement_modes() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 60, "height": 40, "background": "#33993f"})).unwrap();
    s.execute("tools.setColors", json!({"foreground": "#ff0000"})).unwrap();
    let before = rgba(&s, 30, 20);
    s.execute("paint.colorReplacement", json!({"points": [[10, 20], [50, 20]], "size": 20, "mode": "color", "tolerance": 20})).unwrap();
    let c = rgba(&s, 30, 20);
    let lum = |c: [f32; 4]| photocraft_color::blend::lum([c[0], c[1], c[2]]);
    assert!(c[0] > c[1], "{c:?}");
    assert!((lum(c) - lum(before)).abs() < 0.02);
    s.execute("paint.colorReplacement", json!({"points": [[10, 5], [50, 5]], "size": 6, "mode": "luminosity", "sampling": "once", "limits": "discontiguous"}))
        .unwrap();
    assert!((lum(rgba(&s, 30, 5)) - 0.3).abs() < 0.02);
    assert!(s.execute("paint.colorReplacement", json!({"points": [[1, 1]], "mode": "bogus"})).is_err());
}

#[test]
fn presets_save_load_delete_round_trip() {
    let mut s = session(10, 10);
    let n0 = s.execute("brush.presets.list", json!({})).unwrap()["presets"].as_array().unwrap().len();
    assert!(n0 >= 12);
    s.execute("brush.presets.save", json!({"name": "Mine", "brush": {"size": 77, "scattering": {"enabled": true, "count": 3}}})).unwrap();
    let list = s.execute("brush.presets.list", json!({"full": true})).unwrap();
    let mine = list["presets"].as_array().unwrap().iter().find(|p| p["name"] == "Mine").unwrap().clone();
    assert_eq!(mine["brush"]["size"], 77.0);
    assert_eq!(mine["brush"]["scattering"]["count"], 3);
    // Round trip: select it, read it back.
    let b = s.execute("tools.setBrush", json!({"preset": "mine"})).unwrap();
    assert_eq!(b["size"], 77.0);
    let back: BrushSettings = serde_json::from_value(s.execute("brush.get", json!({})).unwrap()).unwrap();
    assert_eq!(back, s.tools.brush);
    // Overwrite, then delete.
    s.execute("brush.presets.save", json!({"name": "Mine"})).unwrap();
    assert_eq!(s.tools.presets.len(), n0 + 1);
    s.execute("brush.presets.delete", json!({"name": "Mine"})).unwrap();
    assert_eq!(s.tools.presets.len(), n0);
    assert!(s.execute("brush.presets.delete", json!({"name": "Mine"})).is_err());
    assert!(s.execute("brush.presets.save", json!({})).is_err());
}

#[test]
fn set_brush_merges_fields() {
    let mut s = session(10, 10);
    s.execute("tools.setBrush", json!({"size": 55, "shapeDynamics": {"enabled": true, "size": {"jitter": 0.5}}})).unwrap();
    s.execute("tools.setBrush", json!({"shapeDynamics": {"angle": {"jitter": 0.25}}})).unwrap();
    let b = &s.tools.brush;
    assert_eq!(b.size, 55.0);
    assert!(b.shape_dynamics.enabled && b.shape_dynamics.size.jitter == 0.5 && b.shape_dynamics.angle.jitter == 0.25);
    s.execute("tools.setBrush", json!({"reset": true})).unwrap();
    assert_eq!(s.tools.brush, BrushSettings::default());
    // Protect texture carries the pattern across textured presets.
    s.execute("tools.setBrush", json!({"preset": "Chalk", "protectTexture": true})).unwrap();
    let chalk_pattern = s.tools.brush.texture.pattern.clone();
    s.execute("tools.setBrush", json!({"preset": "Canvas Texture"})).unwrap();
    assert_eq!(s.tools.brush.texture.pattern, chalk_pattern);
}

#[test]
fn set_brush_keeps_untouched_bitmaps_and_replaces_touched_ones() {
    let tile = |w, h, k: u32| GrayTile::from_fn(w, h, move |x, y| ((x * k + y) % 5) as f32 / 4.0);
    let base = BrushSettings {
        tip: TipShape::Sampled(tile(900, 700, 3)),
        dual_brush: photocraft_paint::DualBrush { tip: TipShape::Sampled(tile(30, 20, 2)), ..Default::default() },
        texture: photocraft_paint::Texture { pattern: photocraft_paint::Pattern::Tile(tile(64, 64, 1)), ..Default::default() },
        ..Default::default()
    };
    let cmd = "tools.setBrush";
    // Untouched bitmaps survive any patch that doesn't name them.
    for patch in [json!({"size": 12}), json!({"dualBrush": {"size": 9}}), json!({"texture": {"depth": 0.5}}), json!({})] {
        let b = merge_brush(&base, &patch, cmd).unwrap();
        assert_eq!((&b.tip, &b.dual_brush.tip, &b.texture.pattern), (&base.tip, &base.dual_brush.tip, &base.texture.pattern), "{patch}");
    }
    let b = merge_brush(&base, &json!({"size": 12, "dualBrush": {"size": 9}}), cmd).unwrap();
    assert_eq!((b.size, b.dual_brush.size), (12.0, 9.0));
    // Named ones are replaced.
    let b = merge_brush(
        &base,
        &json!({"tip": "round", "dualBrush": {"tip": "round"}, "texture": {"pattern": {"procedural": {"style": "dots", "size": 32, "seed": 2}}}}),
        cmd,
    )
    .unwrap();
    assert_eq!((&b.tip, &b.dual_brush.tip), (&TipShape::Round, &TipShape::Round));
    assert!(matches!(b.texture.pattern, photocraft_paint::Pattern::Procedural { size: 32, .. }));
    // A section replaced by a non-object is an error, not a silently kept bitmap.
    assert!(merge_brush(&base, &json!({"dualBrush": 3}), cmd).is_err());
    assert!(merge_brush(&base, &json!(null), cmd).is_err());
    // Speed: a panel edit on a big imported tip doesn't base64 the tip.
    let big = BrushSettings { tip: TipShape::Sampled(tile(2500, 2500, 7)), ..Default::default() };
    let t0 = std::time::Instant::now();
    for i in 0..10 {
        merge_brush(&big, &json!({"size": 10 + i}), cmd).unwrap();
    }
    let ms = t0.elapsed().as_secs_f64() * 100.0;
    eprintln!("merge_brush on a 2500 px tip: {ms:.2} ms per edit");
}

#[test]
fn define_brush_from_selection() {
    let mut s = session(40, 40);
    assert!(s.execute("brush.defineFromSelection", json!({"name": "Blob"})).is_err(), "needs a selection");
    // A black 6×4 block inside a larger selection.
    s.edit("setup", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(10, 12, 16, 16), &[0.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 5, "y": 5, "width": 20, "height": 20})).unwrap();
    let r = s.execute("brush.defineFromSelection", json!({"name": "Blob"})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(6), Some(4)));
    assert_eq!(s.tools.brush.size, 6.0);
    assert!(matches!(&s.tools.brush.tip, TipShape::Sampled(t) if t.width == 6 && t.height == 4 && t.get(0, 0) == 1.0));
    assert!(s.tools.presets.iter().any(|p| p.name == "Blob"));
    // Paint with it.
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("paint.stroke", json!({"points": [[30, 32]], "color": "#ff0000"})).unwrap();
    assert!(rgba(&s, 30, 32)[3] > 0.9);
}

#[test]
fn works_on_cmyk_and_16_bit() {
    for (mode, depth) in [("cmyk", 8), ("rgb", 16), ("grayscale", 32)] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 60, "height": 30, "mode": mode, "depth": depth})).unwrap();
        s.execute("paint.stroke", json!({"points": [[5, 15], [55, 15]], "preset": "Chalk", "color": "#000000"})).unwrap();
        s.execute("paint.pencil", json!({"points": [[5, 5], [55, 5]], "size": 2, "color": "#000000"})).unwrap();
        assert!(rgba(&s, 30, 5)[0] < 0.2, "{mode}/{depth}: {:?}", rgba(&s, 30, 5));
    }
}

#[test]
fn brush_blend_mode_multiply() {
    // Painting with a blend mode (options-bar "Mode"): blue × yellow = black (Multiply).
    let fill = |s: &mut Session| {
        s.edit("bg", |doc, a| {
            doc.layer_mut(a.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 40, 40), &[1.0, 1.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    };
    let mut s = session(40, 40);
    fill(&mut s);
    s.execute("paint.stroke", json!({"points": [[5, 20], [35, 20]], "size": 12, "color": "#0000ff", "mode": "multiply", "brush": {"hardness": 1.0}})).unwrap();
    let c = rgba(&s, 20, 20);
    assert!(c[0] < 0.1 && c[1] < 0.1 && c[2] < 0.1, "multiply blue×yellow ≈ black: {c:?}");
    // Normal mode paints opaque blue at the same spot.
    let mut s2 = session(40, 40);
    fill(&mut s2);
    s2.execute("paint.stroke", json!({"points": [[5, 20], [35, 20]], "size": 12, "color": "#0000ff", "brush": {"hardness": 1.0}})).unwrap();
    let n = rgba(&s2, 20, 20);
    assert!(n[2] > 0.9 && n[0] < 0.1, "normal paints blue: {n:?}");
}

#[test]
fn eraser_on_background_layer_paints_the_background_colour() {
    // Issue #15: erasing the Background layer (locked transparency) must paint the background
    // colour, not silently do nothing (opacity can't be removed there).
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 20, "background": "white"})).unwrap();
    s.execute("tools.setColors", json!({"background": "#ff0000"})).unwrap();
    s.execute("paint.stroke", json!({"points": [[2, 10], [18, 10]], "size": 8, "erase": true, "brush": {"hardness": 1.0}})).unwrap();
    let st = s.active().unwrap();
    let c = st.doc.layers[0].surface().unwrap().rgba(10, 10);
    assert!(c[0] > 0.9 && c[1] < 0.1 && c[2] < 0.1 && c[3] > 0.99, "erased to opaque background red: {c:?}");
    // A corner the stroke didn't touch stays white.
    let w = st.doc.layers[0].surface().unwrap().rgba(10, 2);
    assert!(w[0] > 0.9 && w[1] > 0.9 && w[2] > 0.9, "untouched stays white: {w:?}");
}

#[test]
fn live_stroke_matches_the_committed_stroke() {
    // The canvas previews strokes with `LiveStroke`; it must show what `paint.stroke` commits
    // (soft edge, scatter seed), not a stand-in.
    let mut s = session(200, 100);
    let pts = [[10.0, 50.0, 1.0], [60.0, 30.0, 0.7], [120.0, 60.0, 0.9], [180.0, 40.0, 1.0]];
    let p = json!({"points": [pts[0]], "preset": "Spatter", "brush": {"hardness": 0.0}, "smoothing": 0.0, "target": "pixels"});
    let mut live = LiveStroke::begin(&s, &p).unwrap();
    for c in pts[1..].chunks(2) {
        let sp: Vec<StrokePoint> = c.iter().map(|q| StrokePoint::new(q[0], q[1], q[2] as f32)).collect();
        assert!(!live.push(&sp).unwrap().is_empty());
    }
    let mut commit = p.clone();
    commit["points"] = json!(pts);
    commit["seed"] = json!(live.seed);
    s.execute("paint.stroke", commit).unwrap();
    let shown = live.doc.layer(s.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().clone();
    assert!(same_pixels(&shown, &surface(&s), Rect::new(0, 0, 200, 100)));
    assert!((0..100).any(|y| (0..200).any(|x| (0.05..0.95).contains(&shown.rgba(x, y)[3]))), "soft edge previewed");
    // Bad input is an error, not a panic.
    assert!(LiveStroke::begin(&s, &json!({"points": []})).is_err());
    assert!(LiveStroke::begin(&s, &json!({"points": [[1, 1]], "target": {"channel": 9}})).is_err());
    assert!(LiveStroke::begin(&Session::new(), &json!({"points": [[1, 1]]})).is_err());
}

#[test]
fn live_pencil_matches_the_committed_pencil_and_auto_erases() {
    let mut s = session(80, 40);
    s.execute("tools.setColors", json!({"foreground": "#000000", "background": "#ffffff"})).unwrap();
    // A soft, low-flow session brush: the Pencil is still hard, aliased and full flow.
    s.execute("tools.setBrush", json!({"brush": {"size": 3, "hardness": 0.0, "flow": 0.3}})).unwrap();
    let pts = [[5.0, 5.0, 1.0], [30.2, 17.7, 0.6], [70.0, 33.0, 1.0]];
    for (auto, start) in [(false, pts[0]), (true, [30.4, 17.2, 1.0])] {
        let p = json!({"points": [start], "autoErase": auto, "smoothing": 0.0});
        let mut live = LiveStroke::begin_with(&s, "paint.pencil", &p).unwrap();
        let rest: Vec<StrokePoint> = pts.iter().map(|q| StrokePoint::new(q[0], q[1], q[2] as f32)).collect();
        live.push(&rest).unwrap();
        let mut commit = p.clone();
        let mut all = vec![start];
        all.extend(pts);
        commit["points"] = json!(all);
        commit["seed"] = json!(live.seed);
        s.execute("paint.pencil", commit).unwrap();
        let shown = live.doc.layer(s.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().clone();
        assert!(same_pixels(&shown, &surface(&s), Rect::new(0, 0, 80, 40)), "auto erase {auto}");
        for y in 0..40 {
            for x in 0..80 {
                let a = shown.rgba(x, y)[3];
                assert!(a == 0.0 || a == 1.0, "partial alpha {a} at ({x},{y})");
            }
        }
    }
    // The second stroke started on the first one's black: Auto Erase painted white.
    assert_eq!(rgba(&s, 70, 33), [1.0, 1.0, 1.0, 1.0]);
    // Only the Brush and the Pencil stroke live.
    assert!(LiveStroke::begin_with(&s, "paint.mixerBrush", &json!({"points": [[1, 1]]})).is_err());
    assert!(LiveStroke::begin_with(&s, "paint.pencil", &json!({"points": "none"})).is_err());
}

/// Pencil latency on a 24 MP document: the time to start a live stroke, per pointer move, and
/// to commit. `cargo test -p photocraft-engine --release --lib pencil_latency -- --ignored --nocapture`
#[test]
#[ignore]
fn pencil_latency_24mp() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 6000, "height": 4000, "background": "white"})).unwrap();
    for size in [1.0f64, 9.0, 40.0] {
        let p = json!({"points": [[100.0, 100.0]], "size": size, "smoothing": 0.0});
        let t = std::time::Instant::now();
        let mut live = LiveStroke::begin_with(&s, "paint.pencil", &p).unwrap();
        let begin = t.elapsed().as_secs_f64() * 1000.0;
        let mut steps = Vec::new();
        let mut all = vec![[100.0, 100.0]];
        for i in 1..=300 {
            let q = [100.0 + i as f64 * 15.0, 100.0 + (i as f64 * 0.07).sin() * 900.0 + 1000.0];
            all.push(q);
            let t = std::time::Instant::now();
            live.push(&[StrokePoint::new(q[0], q[1], 1.0)]).unwrap();
            steps.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let mut commit = p.clone();
        commit["points"] = json!(all);
        commit["seed"] = json!(live.seed);
        let t = std::time::Instant::now();
        s.execute("paint.pencil", commit).unwrap();
        let done = t.elapsed().as_secs_f64() * 1000.0;
        steps.sort_by(f64::total_cmp);
        println!(
            "pencil {size:>4} px: begin {begin:.2} ms, per move p50 {:.3} ms p95 {:.3} ms max {:.3} ms, commit {done:.1} ms",
            steps[steps.len() / 2],
            steps[steps.len() * 95 / 100],
            steps[steps.len() - 1]
        );
    }
}

#[test]
fn live_stroke_equals_the_commit_at_every_zoom_with_smoothing() {
    // #189: the preview while dragging must be the committed stroke (within 1/255), with a soft
    // brush on an opaque Background with content, at any zoom (the smoothing string scales with
    // it), with smoothing on, pressure, 8 and 16 bits, and points fed one pointer move at a time.
    let pts: Vec<[f64; 3]> = (0..40)
        .map(|i| {
            let t = f64::from(i) / 39.0;
            [40.0 + 120.0 * t, 60.0 + 30.0 * (t * 7.0).sin(), 0.3 + 0.7 * t]
        })
        .collect();
    for depth in [8, 16] {
        for zoom in [0.25, 1.0, 4.0, 16.0] {
            for (amount, pulled) in [(0.1, false), (0.5, false), (0.3, true)] {
                let mut s = Session::new();
                s.execute("file.new", json!({"width": 200, "height": 120, "depth": depth, "background": "white"})).unwrap();
                s.execute("edit.fill", json!({"color": "#1a2550"})).unwrap();
                s.execute("paint.stroke", json!({"points": [[20, 20], [180, 100]], "size": 8, "hardness": 1.0, "color": "#ffffff"})).unwrap();
                s.tools.foreground = [0.0, 0.0, 0.0, 1.0];
                let brush = json!({"size": 40, "hardness": 0.0, "pressureSize": true, "smoothing": {"amount": amount, "pulledString": pulled}});
                let p = json!({"points": [pts[0]], "brush": brush, "zoom": zoom, "target": "pixels"});
                let mut live = LiveStroke::begin(&s, &p).unwrap();
                for q in &pts[1..] {
                    live.push(&[StrokePoint::new(q[0], q[1], q[2] as f32)]).unwrap();
                }
                let mut commit = p.clone();
                commit["points"] = json!(pts);
                commit["seed"] = json!(live.seed);
                s.execute("paint.stroke", commit).unwrap();
                let shown = live.doc.layer(s.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().clone();
                let done = surface(&s);
                let worst = (0..120).flat_map(|y| (0..200).map(move |x| (x, y))).map(|(x, y)| {
                    let (a, b) = (shown.rgba(x, y), done.rgba(x, y));
                    (0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0f32, f32::max)
                });
                let worst = worst.fold(0.0f32, f32::max);
                let label = format!("depth {depth}, zoom {zoom}, smoothing {amount} (pulled string {pulled})");
                assert!(worst <= 1.0 / 255.0, "{label}: preview differs from the commit by {worst}");
                // Not two blank canvases: the soft stroke is there, edge included.
                assert!(done.rgba(40, 60)[0] < 0.1 && (0.05..0.9).contains(&done.rgba(40, 60 - 14)[2]), "{label}: soft stroke");
            }
        }
    }
}

#[test]
fn new_brush_controls_round_trip_through_set_brush() {
    let mut s = session(40, 40);
    let patch = json!({
        "spacingEnabled": false,
        "shapeDynamics": {"tiltScale": 1.5, "brushProjection": true},
        "transfer": {"wetness": {"jitter": 0.3, "control": "penPressure"}, "mix": {"jitter": 0.6, "minimum": 0.2}},
        "locks": {"shapeDynamics": true, "texture": true, "noise": true},
        "mixer": {"wet": 0.8, "load": 0.25, "mix": 0.4, "flow": 0.9, "sampleAllLayers": true},
    });
    s.execute("tools.setBrush", json!({ "brush": patch })).unwrap();
    let b = &s.tools.brush;
    assert!(!b.spacing_enabled && b.shape_dynamics.brush_projection && b.shape_dynamics.tilt_scale == 1.5);
    assert_eq!(b.transfer.wetness.control, photocraft_paint::Control::PenPressure);
    assert_eq!((b.transfer.mix.jitter, b.transfer.mix.minimum), (0.6, 0.2));
    assert!(b.locks.shape_dynamics && b.locks.texture && b.locks.noise && !b.locks.scattering);
    assert_eq!((b.mixer.wet, b.mixer.load, b.mixer.mix, b.mixer.flow, b.mixer.sample_all_layers), (0.8, 0.25, 0.4, 0.9, true));
    // brush.get reports them, and a fresh session gets the same brush from the reported JSON.
    let got = s.execute("brush.get", json!({})).unwrap();
    for (path, want) in [
        ("/spacingEnabled", json!(false)),
        ("/shapeDynamics/tiltScale", json!(1.5)),
        ("/shapeDynamics/brushProjection", json!(true)),
        ("/transfer/wetness/control", json!("penPressure")),
        ("/locks/texture", json!(true)),
        ("/mixer/sampleAllLayers", json!(true)),
        ("/mixer/load", json!(0.25)),
    ] {
        assert_eq!(got.pointer(path), Some(&want), "{path}");
    }
    let mut t = session(40, 40);
    t.execute("tools.setBrush", json!({ "brush": got })).unwrap();
    assert_eq!(t.tools.brush, s.tools.brush);
    // Locked sections survive picking a preset through the command.
    s.execute("tools.setBrush", json!({"preset": "Chalk"})).unwrap();
    assert!(s.tools.brush.shape_dynamics.brush_projection && s.tools.brush.locks.texture);
    // Wrong types are errors and leave the brush alone.
    let before = s.tools.brush.clone();
    for bad in
        [json!({"spacingEnabled": 3}), json!({"locks": {"texture": "yes"}}), json!({"mixer": {"wet": "wet"}}), json!({"shapeDynamics": {"tiltScale": []}})]
    {
        assert!(s.execute("tools.setBrush", json!({ "brush": bad })).is_err());
    }
    assert_eq!(s.tools.brush, before);
}

#[test]
fn mixer_brush_uses_the_brush_mixer_settings() {
    let mut s = session(200, 30);
    s.execute("tools.setColors", json!({"foreground": "#ff0000"})).unwrap();
    // No wet/load params: the brush's Mixer settings (a dry brush with 0 % Load) apply.
    s.execute("tools.setBrush", json!({"brush": {"pressureSize": false, "spacing": 0.25, "mixer": {"wet": 0, "load": 0, "mix": 0}}})).unwrap();
    s.execute("paint.mixerBrush", json!({"points": [[5, 15], [195, 15]], "size": 8})).unwrap();
    assert!(rgba(&s, 10, 15)[3] > 0.95 && rgba(&s, 190, 15)[3] < 0.5, "the brush's 0 % Load dries out");
    // A param still overrides the brush.
    s.execute("paint.mixerBrush", json!({"points": [[5, 5], [195, 5]], "size": 8, "load": 100})).unwrap();
    assert!(rgba(&s, 190, 5)[3] > 0.95);
}

#[test]
fn coalesced_set_brush_calls_journal_once_per_gesture() {
    let mut s = session(20, 20);
    let n = s.journal.len();
    for k in 1..=10 {
        s.execute("tools.setBrush", json!({"brush": {"opacity": k as f32 / 20.0}, "coalesce": "bar:1"})).unwrap();
        s.execute("brush.get", json!({})).unwrap();
    }
    s.execute("tools.setBrush", json!({"brush": {"shapeDynamics": {"tiltScale": 0.5}}, "coalesce": "bar:1"})).unwrap();
    assert_eq!(s.journal.len(), n + 1, "one gesture, one entry");
    let (id, p) = s.journal.last().unwrap();
    assert_eq!(id, "tools.setBrush");
    assert_eq!(p["brush"], json!({"opacity": 0.5, "shapeDynamics": {"tiltScale": 0.5}}));
    // A new gesture is a new entry; so is an uncoalesced call.
    s.execute("tools.setBrush", json!({"brush": {"flow": 0.5}, "coalesce": "bar:2"})).unwrap();
    s.execute("tools.setBrush", json!({"brush": {"flow": 0.4}, "coalesce": "bar:2"})).unwrap();
    s.execute("tools.setBrush", json!({"brush": {"size": 33}})).unwrap();
    assert_eq!(s.journal.len(), n + 3);
    // Another command in between ends the gesture.
    s.execute("tools.setBrush", json!({"brush": {"size": 34}, "coalesce": "bar:3"})).unwrap();
    s.execute("paint.stroke", json!({"points": [[2, 2], [10, 10]]})).unwrap();
    s.execute("tools.setBrush", json!({"brush": {"size": 35}, "coalesce": "bar:3"})).unwrap();
    assert_eq!(s.journal.len(), n + 6);
    // Replay gives the same brush.
    let mut t = session(20, 20);
    for (id, p) in s.journal.iter().skip(1) {
        t.execute(id, p.clone()).unwrap();
    }
    assert_eq!(t.tools.brush, s.tools.brush);
}

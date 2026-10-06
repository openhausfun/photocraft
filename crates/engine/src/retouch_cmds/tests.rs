use super::*;
use photocraft_color::ColorMode;

const DEPTHS: [u64; 3] = [8, 16, 32];

fn session(w: u32, h: u32, depth: u64, mode: &str) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": depth, "mode": mode})).unwrap();
    s
}

/// Straight RGBA of the active layer at a pixel.
fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
}

fn paint_layer(s: &mut Session, f: impl Fn(i32, i32) -> [f32; 4]) {
    s.edit("setup", |doc, active| {
        let b = doc.bounds();
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let mut data = Vec::new();
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                data.extend(photocraft_raster::from_rgba(&fmt, f(x, y)));
            }
        }
        surf.write_region(b, &data);
        Ok(())
    })
    .unwrap();
}

fn tol(depth: u64) -> f32 {
    if depth == 8 { 1.0 / 255.0 + 1e-4 } else { 1e-3 }
}

/// A smooth colour gradient plus deterministic fine noise.
fn texture(x: i32, y: i32) -> [f32; 4] {
    let n = ((x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) as u32 % 1000) as f32 / 1000.0 * 0.06 - 0.03;
    [0.3 + 0.004 * x as f32 + n, 0.5 + 0.003 * y as f32 + n, 0.45 + n, 1.0]
}

#[test]
fn clone_stamp_copies_exactly_at_all_depths() {
    for depth in DEPTHS {
        let mut s = session(96, 64, depth, "rgb");
        paint_layer(&mut s, |x, y| [x as f32 / 96.0, y as f32 / 64.0, ((x * 7 + y * 3) % 11) as f32 / 11.0, 1.0]);
        let r = s.execute("paint.cloneStamp", json!({"points": [[60, 30], [80, 30]], "source": [20, 20], "size": 10, "hardness": 100})).unwrap();
        assert_eq!(r["offset"], json!([-40, -10]));
        assert!(r["nextSource"].is_null(), "aligned keeps the offset");
        for x in [60, 70, 80] {
            let (got, want) = (rgba(&s, x, 30), rgba(&s, x - 40, 20));
            for c in 0..4 {
                assert!((got[c] - want[c]).abs() <= tol(depth), "depth {depth} x {x}: {got:?} vs {want:?}");
            }
        }
        // Outside the brush untouched.
        let far = rgba(&s, 70, 50);
        assert!((far[0] - 70.0 / 96.0).abs() <= tol(depth) && (far[1] - 50.0 / 64.0).abs() <= tol(depth));
        // One undo step.
        assert!(s.undo());
        assert!((rgba(&s, 70, 30)[1] - 30.0 / 64.0).abs() <= tol(depth));
    }
}

#[test]
fn clone_stamp_samples_pre_stroke_state_and_non_aligned_returns_source() {
    let mut s = session(80, 20, 16, "rgb");
    paint_layer(&mut s, |x, _| [x as f32 / 80.0, 0.0, 0.0, 1.0]);
    // Source 4 px to the right of the stroke start, stroke runs right: without pre-stroke sampling the
    // stroke would re-clone its own output and smear.
    let r = s.execute("paint.cloneStamp", json!({"points": [[10, 10], [50, 10]], "source": [14, 10], "size": 6, "hardness": 100, "aligned": false})).unwrap();
    assert_eq!(r["nextSource"], json!([14.0, 10.0]));
    for x in [20, 30, 45] {
        assert!((rgba(&s, x, 10)[0] - (x + 4) as f32 / 80.0).abs() < 1e-3, "x {x}");
    }
    assert!(s.execute("paint.cloneStamp", json!({"points": [[1, 1]]})).is_err(), "source is required");
    // An explicit offset works too.
    s.execute("paint.cloneStamp", json!({"points": [[70, 10]], "offset": [-60, 0], "size": 4, "hardness": 100})).unwrap();
    assert!((rgba(&s, 70, 10)[0] - 14.0 / 80.0).abs() < 1e-3, "samples the current state (x=10 was cloned from x=14)");
}

#[test]
fn clone_stamp_sample_all_layers_onto_empty_layer_and_opacity() {
    let mut s = session(64, 32, 8, "rgb");
    paint_layer(&mut s, |x, _| if x < 32 { [1.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] });
    s.execute("layer.new.layer", json!({})).unwrap();
    // Current layer is empty: sampling "current" clones transparency (a no-op) …
    s.execute("paint.cloneStamp", json!({"points": [[48, 16]], "source": [16, 16], "size": 8, "hardness": 100})).unwrap();
    assert_eq!(rgba(&s, 48, 16)[3], 0.0);
    // … sampling all layers clones the visible red.
    s.execute("paint.cloneStamp", json!({"points": [[48, 16]], "source": [16, 16], "size": 8, "hardness": 100, "sampleLayer": "all"})).unwrap();
    let p = rgba(&s, 48, 16);
    assert!(p[0] > 0.99 && p[1] < 0.01 && p[3] > 0.99, "{p:?}");
    // currentAndBelow on the bottom layer ignores layers above.
    s.execute("paint.cloneStamp", json!({"points": [[40, 5]], "source": [8, 5], "size": 4, "hardness": 100, "opacity": 50, "sampleLayer": "currentAndBelow"}))
        .unwrap();
    let p = rgba(&s, 40, 5);
    assert!((p[3] - 0.5).abs() < 0.02, "opacity 50 → half alpha {p:?}");
    assert!(s.execute("paint.cloneStamp", json!({"points": [[1, 1]], "source": [0, 0], "sampleLayer": "nope"})).is_err());
}

#[test]
fn clone_stamp_respects_selection_and_cmyk() {
    let mut s = session(64, 32, 8, "cmyk");
    paint_layer(&mut s, |x, _| if x < 32 { [0.0, 0.0, 1.0, 1.0] } else { [1.0, 1.0, 0.0, 1.0] });
    s.execute("select.rect", json!({"x": 40, "y": 0, "width": 8, "height": 32})).unwrap();
    s.execute("paint.cloneStamp", json!({"points": [[36, 16], [56, 16]], "offset": [-30, 0], "size": 8, "hardness": 100})).unwrap();
    // CMYK pixels display through the CMYK profile, so sRGB blue/yellow come back as their
    // (less saturated) in-gamut CMYK equivalents.
    let (inside, outside) = (rgba(&s, 44, 16), rgba(&s, 52, 16));
    assert!(inside[2] > inside[0] + 0.2, "inside the selection: cloned blue {inside:?}");
    assert!(outside[2] < 0.3 && outside[0] > 0.8, "outside the selection: untouched yellow {outside:?}");
}

#[test]
fn healing_brush_takes_destination_colour_and_source_texture() {
    for depth in [8u64, 32] {
        let mut s = session(128, 64, depth, "rgb");
        // Left: dark blue with strong vertical stripes (the texture); right: flat light orange.
        paint_layer(&mut s, |x, _| {
            if x < 64 {
                let t = if (x / 3) % 2 == 0 { 0.08 } else { -0.08 };
                [0.1 + t + 0.1, 0.15 + t + 0.1, 0.5 + t, 1.0]
            } else {
                [0.9, 0.6, 0.3, 1.0]
            }
        });
        s.execute("paint.healingBrush", json!({"points": [[96, 32]], "source": [32, 32], "size": 30, "hardness": 100})).unwrap();
        // Inside the healed disc: mean ≈ destination colour, variance ≈ source texture.
        let pts: Vec<(i32, i32)> = (-8..=8).flat_map(|dy| (-8..=8).map(move |dx| (96 + dx, 32 + dy))).collect();
        let vals: Vec<[f32; 4]> = pts.iter().map(|&(x, y)| rgba(&s, x, y)).collect();
        let mean: Vec<f32> = (0..3).map(|c| vals.iter().map(|v| v[c]).sum::<f32>() / vals.len() as f32).collect();
        for (c, want) in [0.9f32, 0.6, 0.3].iter().enumerate() {
            assert!((mean[c] - want).abs() < 0.06, "depth {depth} channel {c}: mean {} want {want}", mean[c]);
        }
        let var = vals.iter().map(|v| (v[1] - mean[1]).powi(2)).sum::<f32>() / vals.len() as f32;
        assert!(var > 0.003, "texture carried: variance {var}");
        // Horizontal gradient pattern matches the source stripes (period 6).
        let (a, b) = (rgba(&s, 96, 32)[1] - rgba(&s, 95, 32)[1], rgba(&s, 32, 32)[1] - rgba(&s, 31, 32)[1]);
        assert!((a - b).abs() < 0.03, "gradient {a} vs source {b}");
        // Far away untouched.
        assert!((rgba(&s, 125, 3)[0] - 0.9).abs() <= tol(depth));
    }
}

#[test]
fn spot_healing_removes_blemish_all_types() {
    for (depth, kind) in [(8u64, "contentAware"), (16, "contentAware"), (32, "contentAware"), (8, "proximityMatch"), (16, "createTexture")] {
        let mut s = session(160, 120, depth, "rgb");
        paint_layer(&mut s, |x, y| {
            let d = ((x - 80) as f32).hypot((y - 60) as f32);
            if d < 6.0 { [0.05, 0.03, 0.02, 1.0] } else { texture(x, y) }
        });
        let err = |s: &Session| {
            let mut e = 0.0f32;
            let mut n = 0;
            for y in 52..=68 {
                for x in 72..=88 {
                    let (p, t) = (rgba(s, x, y), texture(x, y));
                    e += (0..3).map(|c| (p[c] - t[c]).powi(2)).sum::<f32>();
                    n += 3;
                }
            }
            (e / n as f32).sqrt()
        };
        let before = err(&s);
        s.execute("paint.spotHealing", json!({"points": [[80, 60]], "size": 20, "hardness": 100, "type": kind})).unwrap();
        let after = err(&s);
        assert!(after < 0.05 && after < before * 0.2, "{kind} depth {depth}: rmse {before} → {after}");
        // Far away untouched.
        let (p, t) = (rgba(&s, 10, 10), texture(10, 10));
        assert!((p[0] - t[0]).abs() <= tol(depth) + 1e-3);
    }
}

#[test]
fn dodge_brightens_midtones_more_than_shadows_and_burn_darkens() {
    for depth in DEPTHS {
        let mut s = session(80, 20, depth, "rgb");
        paint_layer(&mut s, |x, _| if x < 40 { [0.5, 0.5, 0.5, 1.0] } else { [0.08, 0.08, 0.08, 1.0] });
        s.execute("paint.dodge", json!({"points": [[5, 10], [75, 10]], "size": 10, "hardness": 100})).unwrap();
        let (mid, shadow) = (rgba(&s, 20, 10)[0] - 0.5, rgba(&s, 60, 10)[0] - 0.08);
        assert!(mid > 0.05, "depth {depth}: midtone +{mid}");
        assert!(mid > 2.0 * shadow.max(0.0), "depth {depth}: midtone +{mid} shadow +{shadow}");
        s.execute("paint.burn", json!({"points": [[5, 15], [35, 15]], "size": 6, "hardness": 100, "exposure": 80})).unwrap();
        assert!(rgba(&s, 20, 15)[0] < 0.45);
        // Shadows range dodges the dark side more than the grey.
        s.execute("paint.dodge", json!({"points": [[5, 3], [75, 3]], "size": 4, "hardness": 100, "range": "shadows"})).unwrap();
        assert!(rgba(&s, 60, 3)[0] - 0.08 > rgba(&s, 20, 3)[0] - 0.5);
    }
    // Grayscale document too.
    let mut s = session(20, 20, 8, "gray");
    s.execute("paint.burn", json!({"points": [[10, 10]], "size": 6, "hardness": 100, "range": "highlights"})).unwrap();
    assert!(rgba(&s, 10, 10)[0] < 0.9);
    assert_eq!(s.active().unwrap().doc.mode, ColorMode::Grayscale);
}

#[test]
fn sponge_reduces_and_increases_saturation() {
    for depth in DEPTHS {
        let mut s = session(40, 20, depth, "rgb");
        paint_layer(&mut s, |_, _| [0.8, 0.4, 0.3, 1.0]);
        let sat = |p: [f32; 4]| photocraft_algo::retouch::saturation([p[0], p[1], p[2]]);
        let s0 = sat(rgba(&s, 10, 10));
        s.execute("paint.sponge", json!({"points": [[2, 10], [38, 10]], "size": 8, "hardness": 100})).unwrap();
        let s1 = sat(rgba(&s, 10, 10));
        assert!(s1 < s0 * 0.8, "depth {depth}: {s0} → {s1}");
        s.execute("paint.sponge", json!({"points": [[2, 3], [38, 3]], "size": 4, "hardness": 100, "mode": "saturate", "vibrance": false})).unwrap();
        assert!(sat(rgba(&s, 10, 3)) > s0);
    }
}

#[test]
fn blur_reduces_local_variance_and_sharpen_increases_it() {
    for depth in [8u64, 16] {
        let mut s = session(60, 30, depth, "rgb");
        paint_layer(&mut s, |x, y| if (x + y) % 2 == 0 { [0.3, 0.3, 0.3, 1.0] } else { [0.7, 0.7, 0.7, 1.0] });
        let var = |s: &Session, x0: i32| {
            let v: Vec<f32> = (10..20).flat_map(|y| (x0..x0 + 10).map(move |x| (x, y))).map(|(x, y)| rgba(s, x, y)[0]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32
        };
        let v0 = var(&s, 10);
        s.execute("paint.blur", json!({"points": [[5, 15], [25, 15]], "size": 20, "hardness": 100, "strength": 100})).unwrap();
        assert!(var(&s, 10) < v0 * 0.5, "depth {depth}: {v0} → {}", var(&s, 10));
        assert!((var(&s, 45) - v0).abs() < 1e-3, "outside untouched");
        // Sharpen a low-contrast checkerboard: contrast grows; Protect Detail caps it at the local range.
        let mut s = session(60, 30, depth, "rgb");
        paint_layer(&mut s, |x, y| if (x + y) % 2 == 0 { [0.45, 0.45, 0.45, 1.0] } else { [0.55, 0.55, 0.55, 1.0] });
        let v0 = var(&s, 10);
        s.execute("paint.sharpen", json!({"points": [[5, 15], [25, 15]], "size": 20, "hardness": 100, "strength": 100, "protectDetail": false})).unwrap();
        assert!(var(&s, 10) > v0 * 1.5, "depth {depth}: {v0} → {}", var(&s, 10));
        s.execute("paint.sharpen", json!({"points": [[35, 15], [55, 15]], "size": 20, "hardness": 100, "strength": 100})).unwrap();
        let hi = (10..20).flat_map(|y| (40..50).map(move |x| (x, y))).map(|(x, y)| rgba(&s, x, y)[0]).fold(0.0, f32::max);
        assert!(hi <= 0.55 + tol(depth), "protect detail: {hi}");
    }
}

#[test]
fn smudge_moves_colour_along_the_stroke() {
    for depth in DEPTHS {
        let mut s = session(100, 30, depth, "rgb");
        paint_layer(&mut s, |x, _| if x < 30 { [1.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] });
        s.execute("paint.smudge", json!({"points": [[20, 15], [70, 15]], "size": 12, "hardness": 100, "strength": 80})).unwrap();
        let g = |x| rgba(&s, x, 15)[1];
        assert!(g(36) < 0.6, "depth {depth}: red dragged right ({})", g(36));
        assert!(g(36) < g(65));
        assert!(rgba(&s, 36, 2)[1] > 0.99, "off-stroke untouched");
        s.execute("tools.setColors", json!({"foreground": "#0000ff"})).unwrap();
        s.execute("paint.smudge", json!({"points": [[80, 25], [95, 25]], "size": 6, "hardness": 100, "fingerPainting": true})).unwrap();
        assert!(rgba(&s, 81, 25)[0] < 0.7, "finger painting lays down the foreground colour");
    }
}

#[test]
fn history_brush_restores_earlier_pixels() {
    for depth in [8u64, 32] {
        let mut s = session(40, 20, depth, "rgb");
        s.execute("paint.stroke", json!({"points": [[0, 10], [40, 10]], "size": 12, "hardness": 1.0, "color": "#ff0000"})).unwrap();
        s.execute("paint.stroke", json!({"points": [[0, 10], [40, 10]], "size": 12, "hardness": 1.0, "color": "#00ff00"})).unwrap();
        assert!(rgba(&s, 20, 10)[1] > 0.99);
        // State 1 = after the red stroke.
        s.execute("paint.historyBrush", json!({"points": [[5, 10], [15, 10]], "size": 6, "hardness": 100, "state": 1})).unwrap();
        let p = rgba(&s, 10, 10);
        assert!(p[0] > 0.99 && p[1] < 0.01, "depth {depth}: {p:?}");
        // Default = the Open snapshot: white.
        s.execute("paint.historyBrush", json!({"points": [[25, 10], [35, 10]], "size": 6, "hardness": 100})).unwrap();
        assert!(rgba(&s, 30, 10).iter().all(|v| *v > 0.99));
        assert!(rgba(&s, 20, 10)[1] > 0.99, "between the dabs still green");
        assert!(s.execute("paint.historyBrush", json!({"points": [[5, 10]], "state": 99})).is_err());
    }
}

#[test]
fn history_brush_errors_when_layer_is_new() {
    let mut s = session(20, 20, 8, "rgb");
    s.execute("layer.new.layer", json!({})).unwrap();
    assert!(s.execute("paint.historyBrush", json!({"points": [[5, 5]], "state": 0})).is_err());
}

#[test]
fn retouch_commands_are_registered_and_need_a_pixel_layer() {
    let ids = [
        "paint.cloneStamp",
        "paint.healingBrush",
        "paint.spotHealing",
        "paint.dodge",
        "paint.burn",
        "paint.sponge",
        "paint.blur",
        "paint.sharpen",
        "paint.smudge",
        "paint.historyBrush",
    ];
    for id in ids {
        let spec = crate::commands::find(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(spec.params.contains("\"points\"") && spec.params.contains("spacing"), "{id}");
        assert!(!Session::new().is_enabled(id));
    }
    let mut s = session(10, 10, 8, "rgb");
    assert!(s.execute("paint.dodge", json!({"points": []})).is_err());
    assert!(s.execute("paint.dodge", json!({"points": [[1, 1]], "range": "bogus"})).is_err());
    assert!(s.execute("paint.spotHealing", json!({"points": [[1, 1]], "type": "bogus"})).is_err());
}

// ---------------------------------------------------------------------------------------------
// Paint target (#207): mask, alpha channel, Quick Mask
// ---------------------------------------------------------------------------------------------

const RETOUCH_IDS: [&str; 10] = [
    "paint.cloneStamp",
    "paint.healingBrush",
    "paint.spotHealing",
    "paint.dodge",
    "paint.burn",
    "paint.sponge",
    "paint.blur",
    "paint.sharpen",
    "paint.smudge",
    "paint.historyBrush",
];

/// Grey noise in 0.3..0.7 (no period along the clone offset used below).
fn gray_noise(x: i32, y: i32) -> f32 {
    0.3 + ((x * 7 + y * 13).rem_euclid(10)) as f32 * 0.04
}

fn texture_surface(surf: &mut Surface, b: Rect) {
    let mut data = Vec::new();
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            data.push(gray_noise(x, y));
        }
    }
    surf.write_region(b, &data);
}

fn active_layer(s: &Session) -> &photocraft_doc::Layer {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap()
}

fn read_all(surf: &Surface, b: Rect) -> Vec<f32> {
    surf.read_region(b)
}

/// A textured pixel layer with a textured mask and a textured alpha channel. Returns the history
/// index of the state before the mask and channel were textured (for the History Brush).
fn targeted_session() -> (Session, usize) {
    let mut s = session(60, 40, 8, "rgb");
    s.execute("layer.new.layer", json!({})).unwrap();
    paint_layer(&mut s, texture);
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.execute("channel.new", json!({"fill": "white"})).unwrap();
    let before = s.active().unwrap().history.past_len();
    s.edit("texture", |doc, active| {
        let b = doc.bounds();
        texture_surface(&mut doc.layer_mut(active.unwrap()).unwrap().mask.as_mut().unwrap().surface, b);
        texture_surface(&mut doc.channels[0].surface, b);
        Ok(())
    })
    .unwrap();
    (s, before)
}

fn retouch_params(id: &str, history_state: usize, target: Value) -> Value {
    let mut p = json!({"points": [[20, 20], [40, 20]], "size": 12, "hardness": 100, "strength": 100, "exposure": 100, "target": target});
    match id {
        "paint.cloneStamp" | "paint.healingBrush" => p["offset"] = json!([-13, 3]),
        "paint.historyBrush" => p["state"] = json!(history_state),
        "paint.sponge" => p["mode"] = json!("saturate"),
        _ => {}
    }
    p
}

/// Tools whose stroke must visibly change grey noise (Sponge has no colour to change on grey, and
/// healing a noise texture with itself may land on the same values).
fn changes_gray(id: &str) -> bool {
    !matches!(id, "paint.sponge" | "paint.healingBrush" | "paint.spotHealing")
}

#[test]
fn retouch_tools_paint_only_the_targeted_mask() {
    for id in RETOUCH_IDS {
        let (mut s, state) = targeted_session();
        let b = s.active().unwrap().doc.bounds();
        let px0 = read_all(active_layer(&s).surface().unwrap(), b);
        let mask0 = read_all(&active_layer(&s).mask.as_ref().unwrap().surface, b);
        let ch0 = read_all(&s.active().unwrap().doc.channels[0].surface, b);
        s.execute(id, retouch_params(id, state, json!("mask"))).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(read_all(active_layer(&s).surface().unwrap(), b), px0, "{id}: layer pixels untouched");
        assert_eq!(read_all(&s.active().unwrap().doc.channels[0].surface, b), ch0, "{id}: channel untouched");
        if changes_gray(id) {
            assert_ne!(read_all(&active_layer(&s).mask.as_ref().unwrap().surface, b), mask0, "{id}: the mask changed");
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(read_all(&active_layer(&s).mask.as_ref().unwrap().surface, b), mask0, "{id}: one undo step");
    }
}

#[test]
fn retouch_tools_paint_only_the_targeted_alpha_channel() {
    for id in RETOUCH_IDS {
        let (mut s, state) = targeted_session();
        let b = s.active().unwrap().doc.bounds();
        let px0 = read_all(active_layer(&s).surface().unwrap(), b);
        let mask0 = read_all(&active_layer(&s).mask.as_ref().unwrap().surface, b);
        let ch0 = read_all(&s.active().unwrap().doc.channels[0].surface, b);
        s.execute(id, retouch_params(id, state, json!({"channel": 0}))).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(read_all(active_layer(&s).surface().unwrap(), b), px0, "{id}: layer pixels untouched");
        assert_eq!(read_all(&active_layer(&s).mask.as_ref().unwrap().surface, b), mask0, "{id}: mask untouched");
        if changes_gray(id) {
            assert_ne!(read_all(&s.active().unwrap().doc.channels[0].surface, b), ch0, "{id}: the channel changed");
        }
    }
}

#[test]
fn retouch_tools_follow_the_channels_panel_target() {
    // No "target" param: the targeted alpha channel (Channels panel) is painted, like the Brush.
    for id in RETOUCH_IDS.into_iter().filter(|id| changes_gray(id)) {
        let (mut s, state) = targeted_session();
        let b = s.active().unwrap().doc.bounds();
        let px0 = read_all(active_layer(&s).surface().unwrap(), b);
        let ch0 = read_all(&s.active().unwrap().doc.channels[0].surface, b);
        s.active_mut().unwrap().channel_view.target = crate::channel_cmds::ChannelTarget::Alpha(0);
        let mut p = retouch_params(id, state, Value::Null);
        p.as_object_mut().unwrap().remove("target");
        assert!(s.is_enabled(id), "{id}");
        s.execute(id, p).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(read_all(active_layer(&s).surface().unwrap(), b), px0, "{id}: layer pixels untouched");
        assert_ne!(read_all(&s.active().unwrap().doc.channels[0].surface, b), ch0, "{id}: the channel changed");
    }
}

#[test]
fn retouch_tools_paint_the_quick_mask() {
    let (mut s, _) = targeted_session();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 30, "height": 40})).unwrap();
    s.execute("select.editInQuickMaskMode", json!({"on": true})).unwrap();
    let b = s.active().unwrap().doc.bounds();
    let px0 = read_all(active_layer(&s).surface().unwrap(), b);
    let q0 = read_all(&s.active().unwrap().doc.quick_mask.as_ref().unwrap().surface, b);
    s.execute("paint.smudge", json!({"points": [[20, 20], [45, 20]], "size": 12, "hardness": 100, "strength": 90})).unwrap();
    assert_eq!(read_all(active_layer(&s).surface().unwrap(), b), px0, "layer pixels untouched");
    assert_ne!(read_all(&s.active().unwrap().doc.quick_mask.as_ref().unwrap().surface, b), q0, "the Quick Mask changed");
}

#[test]
fn retouch_targets_fail_gracefully() {
    let mut s = session(30, 30, 8, "rgb");
    for id in RETOUCH_IDS {
        let p = |t: Value| json!({"points": [[5, 5], [10, 5]], "size": 6, "state": 0, "offset": [1, 1], "target": t});
        assert!(s.execute(id, p(json!("mask"))).is_err(), "{id}: no mask");
        assert!(s.execute(id, p(json!({"channel": 7}))).is_err(), "{id}: no channel 7");
        assert!(s.execute(id, p(json!("quickMask"))).is_err(), "{id}: not in Quick Mask mode");
        let _ = s.execute(id, p(json!(42)));
        let _ = s.execute(id, p(json!({"channel": "x"})));
        let _ = s.execute(id, json!({"points": [[5, 5]], "sampleAllLayers": "yes"}));
    }
}

// ---------------------------------------------------------------------------------------------
// Sample All Layers (#207)
// ---------------------------------------------------------------------------------------------

/// A Background with `bottom` and an empty layer above it (active).
fn two_layers(bottom: impl Fn(i32, i32) -> [f32; 4]) -> Session {
    let mut s = session(100, 30, 8, "rgb");
    paint_layer(&mut s, bottom);
    s.execute("layer.new.layer", json!({})).unwrap();
    s
}

#[test]
fn sample_all_layers_blur_and_sharpen_work_on_an_empty_layer() {
    let checker = |x: i32, y: i32| if (x + y) % 2 == 0 { [0.4, 0.4, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    for (id, extra) in [("paint.blur", json!({})), ("paint.sharpen", json!({"protectDetail": false}))] {
        let stroke = |all: bool| {
            let mut p = json!({"points": [[10, 15], [40, 15]], "size": 16, "hardness": 100, "strength": 100, "sampleAllLayers": all});
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            p
        };
        // Off: the empty layer has nothing to blur.
        let mut s = two_layers(checker);
        s.execute(id, stroke(false)).unwrap();
        assert_eq!(rgba(&s, 25, 15)[3], 0.0, "{id}: without Sample All Layers the layer stays empty");
        // On: the layer picks up the blurred / sharpened picture.
        let mut s = two_layers(checker);
        s.execute(id, stroke(true)).unwrap();
        let (a, b) = (rgba(&s, 25, 15), rgba(&s, 26, 15));
        assert!(a[3] > 0.99, "{id}: {a:?}");
        let contrast = (a[0] - b[0]).abs();
        if id == "paint.blur" {
            assert!(contrast < 0.1, "{id}: blurred ({contrast})");
        } else {
            assert!(contrast > 0.25, "{id}: sharpened ({contrast})");
        }
        assert_eq!(rgba(&s, 80, 15)[3], 0.0, "{id}: outside the stroke untouched");
    }
}

#[test]
fn sample_all_layers_smudge_drags_colour_from_below() {
    let edge = |x: i32, _: i32| if x < 30 { [1.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
    let stroke = |all: bool| json!({"points": [[20, 15], [70, 15]], "size": 12, "hardness": 100, "strength": 80, "sampleAllLayers": all});
    let mut s = two_layers(edge);
    s.execute("paint.smudge", stroke(false)).unwrap();
    assert_eq!(rgba(&s, 40, 15)[3], 0.0, "without Sample All Layers the layer stays empty");
    let mut s = two_layers(edge);
    s.execute("paint.smudge", stroke(true)).unwrap();
    let p = rgba(&s, 36, 15);
    assert!(p[3] > 0.99 && p[1] < 0.6, "red dragged onto the empty layer: {p:?}");
    // A mask target ignores Sample All Layers (nothing to sample from other layers).
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.execute("paint.smudge", json!({"points": [[20, 25], [70, 25]], "size": 6, "sampleAllLayers": true, "target": "mask"})).unwrap();
}

#[test]
fn smudge_across_a_transparent_edge_has_no_dark_fringe() {
    for depth in DEPTHS {
        let mut s = session(100, 30, depth, "rgb");
        s.execute("layer.new.layer", json!({})).unwrap();
        paint_layer(&mut s, |x, _| if x < 30 { [1.0, 0.8, 0.0, 1.0] } else { [0.0, 0.0, 0.0, 0.0] });
        s.execute("paint.smudge", json!({"points": [[20, 15], [70, 15]], "size": 12, "hardness": 50, "strength": 80})).unwrap();
        let mut n = 0;
        for x in 30..80 {
            let p = rgba(&s, x, 15);
            if p[3] > 0.02 {
                n += 1;
                assert!(p[0] > 0.97 && (p[1] - 0.8).abs() < 0.03, "depth {depth} x {x}: {p:?}");
            }
        }
        assert!(n > 5, "depth {depth}: colour dragged into the transparent area");
    }
}

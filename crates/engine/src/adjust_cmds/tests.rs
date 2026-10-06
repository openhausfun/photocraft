use super::*;

const DEPTHS: [u64; 3] = [8, 16, 32];
const MODES: [&str; 4] = ["rgb", "gray", "cmyk", "lab"];

fn session(depth: u64, mode: &str) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 48, "height": 32, "depth": depth, "mode": mode})).unwrap();
    paint(&mut s, |x, y| [0.1 + x as f32 / 60.0, 0.2 + y as f32 / 50.0, 0.6 - x as f32 / 120.0, 1.0]);
    s
}

fn paint(s: &mut Session, f: impl Fn(i32, i32) -> [f32; 4]) {
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

fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
}

fn changed(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)
}

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

#[test]
fn destructive_adjustments_all_depths_and_modes_one_undo_step() {
    let cases: [(&str, Value); 4] = [
        ("image.adjustments.shadowsHighlights", json!({"shadowAmount": 80, "blackClip": 0, "whiteClip": 0})),
        ("image.adjustments.replaceColor", json!({"color": "#4d66cc", "fuzziness": 200, "hue": 90, "saturation": 30})),
        ("image.adjustments.matchColor", json!({"neutralize": true, "intensity": 50})),
        ("image.adjustments.hdrToning", json!({"strength": 2, "exposure": 0.5})),
    ];
    for (id, params) in &cases {
        for depth in DEPTHS {
            for mode in MODES {
                let mut s = session(depth, mode);
                let before = rgba(&s, 3, 3);
                let steps = s.active().unwrap().history.past_len();
                s.execute(id, params.clone()).unwrap_or_else(|e| panic!("{id} {depth} {mode}: {e}"));
                let after = rgba(&s, 3, 3);
                // Gray documents can't show hue changes from Replace Color at a gray pixel.
                if !(mode == "gray" && (id.ends_with("replaceColor") || id.ends_with("matchColor"))) {
                    assert!(changed(before, after) > 1e-3, "{id} {depth} {mode}: {before:?} → {after:?}");
                }
                assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "{id}: one history step");
                s.execute("edit.undo", json!({})).unwrap();
                assert!(changed(before, rgba(&s, 3, 3)) < 1e-6, "{id} undo");
            }
        }
    }
}

#[test]
fn destructive_adjustments_respect_selection() {
    let mut s = session(8, "rgb");
    s.execute("select.all", json!({})).ok();
    s.edit("sel", |doc, _| {
        let mut sel = Surface::new(photocraft_color::PixelFormat::GRAY8);
        sel.write_region(Rect::new(0, 0, 10, 32), &vec![1.0; 320]);
        doc.selection = Some(sel);
        Ok(())
    })
    .unwrap();
    let outside = rgba(&s, 30, 5);
    let inside = rgba(&s, 5, 5);
    s.execute("image.adjustments.shadowsHighlights", json!({"shadowAmount": 100, "blackClip": 0, "whiteClip": 0})).unwrap();
    assert!(changed(inside, rgba(&s, 5, 5)) > 1e-3);
    assert_eq!(outside, rgba(&s, 30, 5));
}

#[test]
fn match_color_from_another_document() {
    let mut s = session(8, "rgb");
    s.execute("file.new", json!({"width": 16, "height": 16, "background": "#e05010"})).unwrap();
    // The target is document 0.
    s.execute("document.activate", json!({"document": 0})).unwrap();
    let before = rgba(&s, 10, 10);
    s.execute("image.adjustments.matchColor", json!({"source": 1})).unwrap();
    let after = rgba(&s, 10, 10);
    assert!(after[0] > before[0] && after[2] < before[2], "{before:?} → {after:?}");
    // Fade 100 is a no-op; a bad source index is an error.
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("image.adjustments.matchColor", json!({"source": 1, "fade": 100})).unwrap();
    assert!(changed(before, rgba(&s, 10, 10)) < 2.0 / 255.0);
    assert!(s.execute("image.adjustments.matchColor", json!({"source": 7})).is_err());
    // Source "None" with default options is a no-op.
    let now = rgba(&s, 10, 10);
    s.execute("image.adjustments.matchColor", json!({})).unwrap();
    assert!(changed(now, rgba(&s, 10, 10)) < 2.0 / 255.0);
}

#[test]
fn hdr_toning_flattens() {
    let mut s = session(16, "rgb");
    s.execute("layer.new.layer", json!({})).unwrap();
    assert_eq!(doc(&s).layers.len(), 2);
    s.execute("image.adjustments.hdrToning", json!({})).unwrap();
    assert_eq!(doc(&s).layers.len(), 1);
    assert_eq!(doc(&s).layers[0].name, "Background");
}

#[test]
fn disabled_without_pixels() {
    let mut s = session(8, "rgb");
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    for id in ["image.adjustments.shadowsHighlights", "image.adjustments.replaceColor", "image.adjustments.matchColor"] {
        assert!(!s.is_enabled(id), "{id}");
    }
    assert!(s.is_enabled("image.adjustments.hdrToning"));
    assert!(!Session::new().is_enabled("image.adjustments.hdrToning"));
}

#[test]
fn selective_color_layer_and_destructive() {
    for depth in DEPTHS {
        for mode in ["rgb", "cmyk", "lab"] {
            let mut s = session(depth, mode);
            paint(&mut s, |_, _| [0.9, 0.1, 0.1, 1.0]);
            let before = rgba(&s, 1, 1);
            // Reds: −100 % cyan (absolute) makes red redder; +100 % black darkens.
            s.execute("image.adjustments.selectiveColor", json!({"method": "absolute", "reds": [0, 0, 0, 60]})).unwrap();
            let after = rgba(&s, 1, 1);
            assert!(after[0] < before[0] - 0.05, "{depth} {mode}: {before:?} → {after:?}");
        }
    }
    let mut s = session(8, "rgb");
    let r = s.execute("layer.newAdjustmentLayer.selectiveColor", json!({"colors": "blues", "yellow": 40})).unwrap();
    let id = photocraft_doc::LayerId(r["layer"].as_u64().unwrap());
    let get = |s: &Session| match &doc(s).layer(id).unwrap().content {
        LayerContent::Adjustment(Adjustment::SelectiveColor { relative, adjustments }) => (*relative, *adjustments),
        other => panic!("{other:?}"),
    };
    assert_eq!(get(&s).1[4], [0.0, 0.0, 40.0, 0.0]);
    // Properties edits merge one range at a time.
    s.execute("layer.setAdjustment", json!({"layer": id.0, "colors": "neutrals", "black": -20, "method": "absolute"})).unwrap();
    let (rel, a) = get(&s);
    assert!(!rel);
    assert_eq!(a[4][2], 40.0);
    assert_eq!(a[7][3], -20.0);
    // No parameters: reset.
    s.execute("layer.setAdjustment", json!({"layer": id.0})).unwrap();
    assert_eq!(get(&s), (true, [[0.0; 4]; 9]));
}

#[test]
fn color_lookup_builtin_data_and_errors() {
    let mut s = session(8, "rgb");
    let r = s.execute("layer.newAdjustmentLayer.colorLookup", json!({"lut": "dayForNight"})).unwrap();
    let id = photocraft_doc::LayerId(r["layer"].as_u64().unwrap());
    let px = photocraft_compose::render(doc(&s), Rect::from_xywh(40, 30, 1, 1)).px[0];
    assert!(px[2] > px[0], "night is blue: {px:?}");
    // Switching interpolation keeps the table.
    s.execute("layer.setAdjustment", json!({"layer": id.0, "interpolation": "tetrahedral", "dither": true})).unwrap();
    match &doc(&s).layer(id).unwrap().content {
        LayerContent::Adjustment(Adjustment::ColorLookup { lut: Some(_), size: 33, tetrahedral: true, dither: true, name }) => {
            assert_eq!(name, "Day for Night")
        }
        other => panic!("{other:?}"),
    }
    // Embedded .cube text (an inverting LUT) applied destructively, at every depth.
    let mut cube = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{} {} {}\n", 1 - r, 1 - g, 1 - b));
            }
        }
    }
    for depth in DEPTHS {
        let mut s = session(depth, "rgb");
        let before = rgba(&s, 5, 5);
        s.execute("image.adjustments.colorLookup", json!({"data": cube, "fileName": "invert.cube"})).unwrap();
        let after = rgba(&s, 5, 5);
        for c in 0..3 {
            assert!((after[c] - (1.0 - before[c])).abs() < 2.0 / 255.0, "{depth}: {before:?} {after:?}");
        }
    }
    assert!(s.execute("image.adjustments.colorLookup", json!({"lut": "nope"})).is_err());
    assert!(s.execute("image.adjustments.colorLookup", json!({"data": "LUT_3D_SIZE 3\n0 0 0\n"})).is_err());
    assert!(s.execute("layer.newAdjustmentLayer.colorLookup", json!({"file": "/nonexistent/x.cube"})).is_err());
    let looks = s.execute("image.adjustments.colorLookup.list", json!({})).unwrap();
    assert_eq!(looks.as_array().unwrap().len(), photocraft_cms::lutfile::BUILTIN.len());
}

#[test]
fn new_kinds_round_trip_through_psd_and_pcraft() {
    let mut s = session(8, "rgb");
    s.execute("layer.newAdjustmentLayer.selectiveColor", json!({"reds": [10, -20, 30, -40], "method": "absolute"})).unwrap();
    s.execute("layer.newAdjustmentLayer.colorLookup", json!({"lut": "warm", "dither": true})).unwrap();
    let d = doc(&s).clone();
    let bytes = photocraft_io::export(&d, "x.psd", &Default::default()).unwrap().bytes;
    let back = photocraft_io::import("x.psd", &bytes).unwrap().document;
    let kinds: Vec<&str> = back
        .layers
        .iter()
        .filter_map(|l| match &l.content {
            LayerContent::Adjustment(a) => Some(crate::commands::adjustment_kind(a)),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["selectiveColor", "colorLookup"]);
    let pc = photocraft_format::save_to_bytes(&d, &Default::default()).unwrap();
    let again = photocraft_format::load_from_bytes(&pc).unwrap();
    for (a, b) in d.layers.iter().zip(&again.layers) {
        assert_eq!(a.content, b.content);
    }
}

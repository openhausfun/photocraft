use super::*;
use photocraft_geom::Rect;

fn session(w: u32, h: u32, depth: u64, mode: &str) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": depth, "mode": mode})).unwrap();
    paint(&mut s, |x, y| [x as f32 / w as f32, y as f32 / h as f32, 0.5, 1.0]);
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

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

fn colours(d: &Document) -> std::collections::HashSet<[u8; 3]> {
    let surf = d.layers[0].surface().unwrap();
    let mut set = std::collections::HashSet::new();
    for y in 0..d.size.height as i32 {
        for x in 0..d.size.width as i32 {
            let c = surf.rgba(x, y);
            if c[3] > 0.0 {
                set.insert([c[0], c[1], c[2]].map(|v| (v * 255.0).round() as u8));
            }
        }
    }
    set
}

#[test]
fn rotate_arbitrary_grows_canvas_and_keeps_background() {
    for depth in [8, 16, 32] {
        let mut s = session(40, 20, depth, "rgb");
        s.execute("layer.new.layer", json!({})).unwrap();
        paint(&mut s, |x, y| if (15..25).contains(&x) && (5..15).contains(&y) { [1.0, 0.0, 0.0, 1.0] } else { [0.0; 4] });
        s.execute("tools.setColors", json!({"background": "#00ff00"})).unwrap();
        let r = s.execute("image.rotation.arbitrary", json!({"angle": 90, "direction": "cw"})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(20), Some(40)));
        let d = doc(&s);
        assert_eq!(d.layers[0].name, "Background");
        // The red square stays centred.
        assert!(d.layers[1].surface().unwrap().rgba(10, 20)[0] > 0.9, "{depth}");
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("image.rotation.arbitrary", json!({"angle": 30, "direction": "ccw"})).unwrap();
        let d = doc(&s);
        assert_eq!(d.size, rotated_size(Size::new(40, 20), 30.0));
        assert!(d.size.width > 40 && d.size.height > 20);
        // Exposed corners take the background colour on the Background layer.
        let corner = d.layers[0].surface().unwrap().rgba(0, 0);
        assert!(corner[1] > 0.9 && corner[0] < 0.1, "{corner:?}");
    }
    let mut s = session(10, 10, 8, "rgb");
    assert!(s.execute("image.rotation.arbitrary", json!({"angle": 10, "direction": "up"})).is_err());
    let n = s.active().unwrap().history.past_len();
    s.execute("image.rotation.arbitrary", json!({"angle": 360})).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), n, "a full turn changes nothing");
}

#[test]
fn rotate_arbitrary_other_modes() {
    for mode in ["gray", "cmyk", "lab"] {
        let mut s = session(16, 12, 16, mode);
        s.execute("image.rotation.arbitrary", json!({"angle": 45})).unwrap();
        assert_eq!(doc(&s).size, rotated_size(Size::new(16, 12), 45.0), "{mode}");
    }
}

#[test]
fn indexed_color_palettes_and_dithers() {
    for palette in ["selective", "perceptual", "adaptive", "web", "uniform", "systemMac", "systemWindows"] {
        for dither in ["none", "diffusion", "pattern", "noise"] {
            let mut s = session(24, 16, 16, "rgb");
            let r = s.execute("image.mode.indexedColor", json!({"palette": palette, "colors": 16, "dither": dither, "forced": "none"})).unwrap();
            let d = doc(&s);
            assert_eq!((d.mode, d.depth), (ColorMode::Indexed, SampleType::U8), "{palette} {dither}");
            let table = d.color_table.as_ref().unwrap();
            assert_eq!(table.colors.len() as u64, r["colors"].as_u64().unwrap());
            let used = colours(d);
            assert!(used.iter().all(|c| table.colors.contains(c)), "{palette} {dither}: pixels off the table");
            if !matches!(palette, "web" | "systemMac" | "systemWindows") {
                assert!(table.colors.len() <= 16, "{palette}: {}", table.colors.len());
            }
        }
    }
}

#[test]
fn indexed_exact_transparency_and_sources() {
    // Two colours + transparency: exact palette with a transparent entry.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8, "background": "transparent"})).unwrap();
    paint(&mut s, |x, _| {
        if x < 3 {
            [1.0, 0.0, 0.0, 1.0]
        } else if x < 6 {
            [0.0, 0.0, 1.0, 1.0]
        } else {
            [0.0; 4]
        }
    });
    s.execute("image.mode.indexedColor", json!({"palette": "exact", "forced": "none"})).unwrap();
    let d = doc(&s);
    let t = d.color_table.as_ref().unwrap();
    assert_eq!(t.colors.len(), 3);
    assert_eq!(t.transparent, Some(2));
    assert_eq!(d.layers[0].name, "Index");
    assert_eq!(d.layers[0].surface().unwrap().rgba(7, 0)[3], 0.0);
    // Exact fails on a gradient.
    let mut s = session(64, 64, 8, "rgb");
    assert!(s.execute("image.mode.indexedColor", json!({"palette": "exact"})).is_err());
    // Gray, CMYK and Lab sources flatten through the composite.
    for mode in ["gray", "cmyk", "lab"] {
        let mut s = session(12, 12, 16, mode);
        s.execute("image.mode.indexedColor", json!({"colors": 8})).unwrap();
        assert_eq!(doc(&s).mode, ColorMode::Indexed, "{mode}");
    }
    // Back to RGB drops the table.
    s.execute("image.mode.indexedColor", json!({"colors": 8})).unwrap();
    s.execute("image.mode.rgb", json!({})).unwrap();
    assert!(doc(&s).color_table.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(doc(&s).color_table.is_some());
}

#[test]
fn color_table_presets_and_entries() {
    let mut s = session(16, 16, 8, "rgb");
    assert!(!s.is_enabled("image.mode.colorTable"));
    s.execute("image.mode.indexedColor", json!({"palette": "uniform", "colors": 8, "dither": "none", "forced": "none"})).unwrap();
    let before = doc(&s).color_table.clone().unwrap();
    // No changes: a report, no history step.
    let n = s.active().unwrap().history.past_len();
    let r = s.execute("image.mode.colorTable", json!({})).unwrap();
    assert_eq!(r["colors"].as_array().unwrap().len(), before.colors.len());
    assert_eq!(s.active().unwrap().history.past_len(), n);
    // Replace entry 0: pixels of index 0 change colour.
    let px0 = doc(&s).layers[0].surface().unwrap().rgba(0, 0);
    let i0 = before.nearest([px0[0], px0[1], px0[2]]);
    s.execute("image.mode.colorTable", json!({"entries": {i0.to_string(): "#ff00ff"}})).unwrap();
    assert_eq!(doc(&s).layers[0].surface().unwrap().rgba(0, 0), [1.0, 0.0, 1.0, 1.0]);
    s.execute("image.mode.colorTable", json!({"table": "blackBody"})).unwrap();
    assert_eq!(doc(&s).color_table.as_ref().unwrap().colors.len(), 256);
    s.execute("image.mode.colorTable", json!({"transparent": 0})).unwrap();
    assert!(s.execute("image.mode.colorTable", json!({"table": "nope"})).is_err());
    assert!(s.execute("image.mode.colorTable", json!({"entries": {"999": "#000000"}})).is_err());
}

#[test]
fn bitmap_methods_need_grayscale() {
    let mut s = session(32, 32, 16, "rgb");
    assert!(!s.is_enabled("image.mode.bitmap"));
    s.execute("image.mode.grayscale", json!({})).unwrap();
    for method in ["threshold", "pattern", "diffusion", "halftone"] {
        s.execute("image.mode.bitmap", json!({"method": method, "frequency": 12, "shape": "diamond"})).unwrap();
        let d = doc(&s);
        assert_eq!((d.mode, d.depth), (ColorMode::Bitmap, SampleType::U8), "{method}");
        let surf = d.layers[0].surface().unwrap();
        for y in 0..32 {
            for x in 0..32 {
                let v = surf.pixel(x, y)[0];
                assert!(v == 0.0 || v == 1.0, "{method}: {v}");
            }
        }
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert!(s.execute("image.mode.bitmap", json!({"method": "nope"})).is_err());
}

#[test]
fn duotone_inks_and_display() {
    let mut s = session(16, 8, 8, "gray");
    paint(&mut s, |x, _| [x as f32 / 15.0; 4].map(|v| v.min(1.0)));
    assert!(s.execute("image.mode.duotone", json!({"type": "quintone"})).is_err());
    let r = s
        .execute(
            "image.mode.duotone",
            json!({"type": "duotone", "inks": [{"name": "Black"}, {"name": "Orange", "color": "#ff8000", "curve": [[0, 0], [100, 80]]}]}),
        )
        .unwrap();
    assert_eq!(r["inks"], json!(["Black", "Orange"]));
    let d = doc(&s);
    assert_eq!(d.mode, ColorMode::Duotone);
    let inks = &d.duotone.as_ref().unwrap().inks;
    assert_eq!(inks[1].curve.last().unwrap().output, 0.8);
    // Display: warm midtones (red > blue) where the gray image is neutral.
    let shown = display_document(d).unwrap();
    let px = photocraft_compose::render(&shown, Rect::from_xywh(8, 4, 1, 1)).px[0];
    assert!(px[0] > px[2] + 0.05, "{px:?}");
    assert!(display_document(&Document::new("x", Size::new(1, 1), ColorMode::Grayscale, SampleType::U8)).is_none());
    // Tritone defaults and leaving the mode.
    s.execute("image.mode.duotone", json!({"type": "tritone"})).unwrap();
    assert_eq!(doc(&s).duotone.as_ref().unwrap().inks.len(), 3);
    s.execute("image.mode.grayscale", json!({})).unwrap();
    assert!(doc(&s).duotone.is_none());
    assert_eq!(doc(&s).mode, ColorMode::Grayscale);
    assert!(!session(4, 4, 8, "rgb").is_enabled("image.mode.duotone"));
}

#[test]
fn modes_round_trip_through_pcraft() {
    let mut s = session(12, 12, 8, "rgb");
    s.execute("image.mode.indexedColor", json!({"colors": 6})).unwrap();
    let bytes = photocraft_format::save_to_bytes(doc(&s), &Default::default()).unwrap();
    let back = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(back.color_table, doc(&s).color_table);
    assert_eq!(back.mode, ColorMode::Indexed);
}

use super::*;

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 300, "height": 120, "depth": depth, "background": "white"})).unwrap();
    s
}

fn text_layer(s: &mut Session, text: &str) -> LayerId {
    let r = s.execute("type.create", json!({"x": 20, "y": 70, "text": text, "size": 36, "color": "#000000"})).unwrap();
    LayerId(r["layer"].as_u64().unwrap())
}

fn text(s: &Session, id: LayerId) -> &TextLayer {
    match &s.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Text(t) => t,
        c => panic!("not text: {}", c.kind_name()),
    }
}

fn ink(s: &Session, id: LayerId) -> photocraft_geom::Rect {
    s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().content_bounds()
}

#[test]
fn anti_alias_and_orientation_are_one_step_each() {
    let mut s = session(8);
    assert!(!s.is_enabled("type.antiAlias.crisp"));
    let id = text_layer(&mut s, "Aa");
    for (cmd, aa) in [
        ("type.antiAlias.crisp", AntiAlias::Crisp),
        ("type.antiAlias.windows", AntiAlias::Windows),
        ("type.antiAlias.windowsLcd", AntiAlias::WindowsLcd),
        ("type.antiAlias.none", AntiAlias::None),
    ] {
        s.execute(cmd, json!({})).unwrap();
        assert_eq!(text(&s, id).antialias, aa);
    }
    // No anti-aliasing: only fully covered or empty pixels.
    let surf = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().clone();
    let r = surf.content_bounds();
    let n = surf.channels();
    assert!(surf.read_region(r).chunks_exact(n).all(|p| p[n - 1] == 0.0 || p[n - 1] == 1.0));
    s.execute("type.orientation.vertical", json!({})).unwrap();
    assert_eq!(text(&s, id).orientation, Orientation::Vertical);
    assert!(s.undo());
    assert_eq!(text(&s, id).orientation, Orientation::Horizontal);
    assert!(s.undo());
    assert_eq!(text(&s, id).antialias, AntiAlias::WindowsLcd);
}

#[test]
fn point_and_paragraph_conversion_keep_the_text_in_place() {
    for align in ["left", "center", "right"] {
        let mut s = session(8);
        let r = s.execute("type.create", json!({"x": 150, "y": 70, "text": "Hello world", "size": 30, "align": align})).unwrap();
        let id = LayerId(r["layer"].as_u64().unwrap());
        let before = ink(&s, id);
        assert!(!s.is_enabled("type.convertToPointText"));
        s.execute("type.convertToParagraphText", json!({})).unwrap();
        assert!(matches!(text(&s, id).shape, TextShape::Box { .. }));
        let after = ink(&s, id);
        assert!((after.x0 - before.x0).abs() <= 2 && (after.y0 - before.y0).abs() <= 2, "{align}: {before:?} → {after:?}");
        let lines = |s: &Session| {
            let st = s.active().unwrap();
            layout(&st.doc, text(s, id)).lines.len()
        };
        assert_eq!(lines(&s), 1, "{align}: no new wraps");
        s.execute("type.convertToPointText", json!({})).unwrap();
        assert!(matches!(text(&s, id).shape, TextShape::Point));
        let back = ink(&s, id);
        assert!((back.x0 - before.x0).abs() <= 2 && (back.y0 - before.y0).abs() <= 2, "{align}: {before:?} → {back:?}");
    }
}

/// Issue #199: vertical type is laid out top to bottom, columns right to left, on the canvas.
#[test]
fn vertical_orientation_renders_columns_inside_the_canvas() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 1000, "height": 600, "background": "#fff8e7", "name": "flyer"})).unwrap();
    // The issue's repro (its font may be missing here: fallback fonts then cover the text).
    let r = s.execute("type.create", json!({"x": 900, "y": 30, "text": "縦書きテスト", "size": 40, "font": "Noto Sans CJK JP", "name": "vertical"})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let flat = ink(&s, id);
    assert!(flat.width() > flat.height(), "horizontal first: {flat:?}");
    let v = s.execute("type.orientation.vertical", json!({})).unwrap();
    assert_eq!(v, json!({"orientation": "vertical"}));
    let b = ink(&s, id);
    assert!(b.height() > b.width(), "{b:?}");
    assert!(b.x0 >= 0 && b.x1 <= 1000 && b.y0 >= 0 && b.y1 <= 600, "inside the canvas: {b:?}");
    // Centred on the anchor's x, running down from its y.
    assert!(b.x0 < 900 && b.x1 > 900 && b.y0 >= 25, "{b:?}");
    // Latin text: a second column of a two-line layer sits left of the first.
    let r = s.execute("type.create", json!({"x": 500, "y": 30, "text": "ABC\nDEF", "size": 40})).unwrap();
    let two = LayerId(r["layer"].as_u64().unwrap());
    s.execute("type.orientation.vertical", json!({"layer": two.0})).unwrap();
    let st = s.active().unwrap();
    let l = layout(&st.doc, text(&s, two));
    assert!(l.vertical && l.lines.len() == 2);
    let [x0, y0, x1, y1] = l.bounds().unwrap();
    assert!(x1 > 0.0 && x0 < -40.0 && y0 >= -1e-3 && y1 > 60.0, "{:?}", l.bounds());
    // Bad layer ids fail cleanly.
    assert!(s.execute("type.orientation.vertical", json!({"layer": 9999})).is_err());
}

#[test]
fn vertical_point_and_paragraph_conversion_keep_the_columns_in_place() {
    for align in ["left", "center", "right"] {
        let mut s = session(8);
        s.execute("image.canvasSize", json!({"width": 300, "height": 400})).ok();
        let r = s.execute("type.create", json!({"x": 150, "y": 100, "text": "Hello world", "size": 30, "align": align})).unwrap();
        let id = LayerId(r["layer"].as_u64().unwrap());
        s.execute("type.orientation.vertical", json!({})).unwrap();
        let st = s.active().unwrap();
        let before = layout(&st.doc, text(&s, id)).bounds().unwrap();
        let before_tf = text(&s, id).transform;
        let map = |t: &photocraft_geom::Affine, b: [f32; 4]| {
            let p = t.apply(photocraft_geom::Point::new(f64::from(b[0]), f64::from(b[1])));
            (p.x, p.y)
        };
        let doc_before = map(&before_tf, before);
        s.execute("type.convertToParagraphText", json!({})).unwrap();
        let TextShape::Box { height, .. } = text(&s, id).shape else { panic!("box expected") };
        let st = s.active().unwrap();
        let l = layout(&st.doc, text(&s, id));
        assert_eq!(l.lines.len(), 1, "{align}: no new wraps");
        assert!(height > 0.0);
        // The column's first glyph stays where it was (glyph positions in document space).
        let g_after = l.glyphs[0];
        let tf = text(&s, id).transform;
        let p_after = tf.apply(photocraft_geom::Point::new(f64::from(g_after.x), f64::from(g_after.y)));
        s.execute("type.convertToPointText", json!({})).unwrap();
        let st = s.active().unwrap();
        let back = layout(&st.doc, text(&s, id));
        let g_back = back.glyphs[0];
        let p_back = text(&s, id).transform.apply(photocraft_geom::Point::new(f64::from(g_back.x), f64::from(g_back.y)));
        assert!((p_after.x - p_back.x).abs() < 1.5 && (p_after.y - p_back.y).abs() < 1.5, "{align}: {p_after:?} → {p_back:?}");
        let back_doc = map(&text(&s, id).transform, back.bounds().unwrap());
        assert!((back_doc.0 - doc_before.0).abs() < 1.5 && (back_doc.1 - doc_before.1).abs() < 1.5, "{align}: {doc_before:?} → {back_doc:?}");
    }
}

#[test]
fn paragraph_to_point_hardens_soft_wraps() {
    let mut s = session(8);
    let r = s.execute("type.create", json!({"box": [10, 10, 120, 100], "text": "one two three four five six", "size": 18})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let st = s.active().unwrap();
    let soft = layout(&st.doc, text(&s, id)).lines.len();
    assert!(soft >= 2);
    let r = s.execute("type.convertToPointText", json!({})).unwrap();
    assert_eq!(r["inserted"], soft - 1);
    let t = text(&s, id);
    assert_eq!(t.text.matches('\n').count(), soft - 1);
    assert_eq!(t.text.replace('\n', ""), "one two three four five six");
    let st = s.active().unwrap();
    assert_eq!(layout(&st.doc, text(&s, id)).lines.len(), soft);
}

#[test]
fn work_path_and_shape_follow_the_glyphs() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        let id = text_layer(&mut s, "oil");
        let before = ink(&s, id);
        s.execute("type.createWorkPath", json!({})).unwrap();
        let wp = s.active().unwrap().doc.work_path.clone().unwrap();
        // 'o' has an outer contour and a counter (subtracted), plus 'i' (stem + dot) and 'l'.
        assert!(wp.subpaths.len() >= 5, "{}", wp.subpaths.len());
        assert!(wp.subpaths.iter().any(|sp| sp.op == PathOp::Subtract));
        let (x0, y0, x1, y1) = wp.control_bounds().unwrap();
        assert!((x0 - before.x0 as f64).abs() < 3.0 && (x1 - before.x1 as f64).abs() < 3.0, "{x0},{x1} vs {before:?}");
        assert!((y0 - before.y0 as f64).abs() < 3.0 && (y1 - before.y1 as f64).abs() < 3.0);
        // Convert to Shape renders the same pixels (the counter of the 'o' stays open).
        let comp_before = photocraft_compose::flatten(&s.active().unwrap().doc);
        s.execute("type.convertToShape", json!({})).unwrap();
        let l = s.active().unwrap().doc.layer(id).unwrap();
        assert!(matches!(l.content, LayerContent::Shape(_)));
        assert!(!l.psd_blocks.iter().any(|(k, _)| k == b"TySh"));
        let comp_after = photocraft_compose::flatten(&s.active().unwrap().doc);
        let diff: f32 = comp_before.px.iter().zip(&comp_after.px).map(|(a, b)| (a[0] - b[0]).abs()).sum::<f32>() / comp_before.px.len() as f32;
        assert!(diff < 0.01, "depth {depth}: mean diff {diff}");
        // The middle of the 'o' counter is background (white).
        assert!(s.undo());
        assert!(matches!(s.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Text(_)));
    }
}

#[test]
fn warp_text_changes_pixels_and_round_trips() {
    let mut s = session(8);
    let id = text_layer(&mut s, "WARP ME");
    let flat = ink(&s, id);
    s.execute("type.warpText", json!({"style": "arc", "bend": 60})).unwrap();
    let w = text(&s, id).warp.clone().unwrap();
    assert_eq!(w.style, "warpArc");
    assert_eq!(w.value, 60.0);
    let arced = ink(&s, id);
    assert!(arced.height() > flat.height() + 8, "{flat:?} → {arced:?}");
    // The regenerated TySh carries the warp (PSD round trip).
    let raw = text(&s, id).psd_raw.clone().unwrap();
    let back = photocraft_text::psd::text_layer_from_tysh(&raw, 72.0).unwrap();
    assert_eq!(back.warp, Some(w));
    assert!(s.execute("type.warpText", json!({"style": "spiral"})).is_err());
    s.execute("type.warpText", json!({"style": "none"})).unwrap();
    assert!(text(&s, id).warp.is_none());
    assert_eq!(ink(&s, id), flat);
    for (_, style) in photocraft_text::warp::STYLES {
        s.execute("type.warpText", json!({"style": style, "bend": -40, "horizontalDistortion": 10})).unwrap();
        assert!(!ink(&s, id).is_empty(), "{style}");
    }
}

#[test]
fn opentype_toggles_store_features() {
    let mut s = session(8);
    let id = text_layer(&mut s, "office 1/2");
    let st = |s: &Session| text(s, id).char_runs()[0].style.clone();
    assert!(st(&s).ligatures);
    s.execute("type.openType.standardLigatures", json!({})).unwrap();
    assert!(!st(&s).ligatures);
    s.execute("type.openType.fractions", json!({})).unwrap();
    s.execute("type.openType.discretionaryLigatures", json!({"on": true})).unwrap();
    s.execute("type.openType.swash", json!({"range": [0, 3]})).unwrap();
    assert!(feature_on(&st(&s), "frac") && feature_on(&st(&s), "dlig") && feature_on(&st(&s), "swsh"));
    let runs = text(&s, id).char_runs();
    assert!(!feature_on(&runs.last().unwrap().style, "swsh"), "range respected");
    s.execute("type.openType.fractions", json!({})).unwrap();
    assert!(!feature_on(&st(&s), "frac"));
    // PSD round trip of the toggles.
    let raw = text(&s, id).psd_raw.clone().unwrap();
    let back = photocraft_text::psd::text_layer_from_tysh(&raw, 72.0).unwrap();
    assert!(feature_on(&back.char_runs()[0].style, "swsh") && !back.char_runs()[0].style.ligatures);
}

#[test]
fn lorem_update_fonts_and_default_styles() {
    let mut s = session(16);
    // No type layer: a new paragraph box.
    let r = s.execute("type.pasteLoremIpsum", json!({})).unwrap();
    let box_id = LayerId(r["layer"].as_u64().unwrap());
    assert!(matches!(text(&s, box_id).shape, TextShape::Box { .. }));
    let id = text_layer(&mut s, "Hi ");
    let r = s.execute("type.pasteLoremIpsum", json!({})).unwrap();
    assert!(text(&s, id).text.starts_with("Hi Lorem ipsum"));
    assert_eq!(r["caret"], text(&s, id).text.chars().count());
    assert_eq!(s.execute("type.updateAllTextLayers", json!({})).unwrap()["updated"], 2);
    // A missing font is reported and replaced.
    s.execute("type.setStyle", json!({"font": "No Such Font Family"})).unwrap();
    assert_eq!(s.execute("type.resolveMissingFonts", json!({})).unwrap()["missing"], json!(["No Such Font Family"]));
    s.execute("type.resolveMissingFonts", json!({"map": {"No Such Font Family": "JetBrains Mono"}})).unwrap();
    assert_eq!(text(&s, id).char_runs()[0].style.font_family, "JetBrains Mono");
    s.execute("type.setStyle", json!({"font": "Gone Sans"})).unwrap();
    assert_eq!(s.execute("type.replaceAllMissingFonts", json!({})).unwrap()["replaced"], 1);
    assert_eq!(text(&s, id).char_runs()[0].style.font_family, photocraft_text::fonts::DEFAULT_FAMILY);
    // Default type styles: save from one layer, new layers and Load use them.
    assert!(!s.is_enabled("type.loadDefaultTypeStyles"));
    s.execute("type.setStyle", json!({"size": 50, "font": "JetBrains Mono"})).unwrap();
    s.execute("type.saveDefaultTypeStyles", json!({})).unwrap();
    s.select_layer(box_id).unwrap();
    s.execute("type.loadDefaultTypeStyles", json!({})).unwrap();
    assert_eq!(text(&s, box_id).char_runs()[0].style.size_pt, 50.0);
    let r = s.execute("type.create", json!({"x": 5, "y": 5, "text": "new"})).unwrap();
    let st = text(&s, LayerId(r["layer"].as_u64().unwrap())).char_runs()[0].style.clone();
    assert_eq!((st.font_family.as_str(), st.size_pt), ("JetBrains Mono", 50.0));
}

#[test]
fn rasterize_type_layer() {
    let mut s = session(8);
    let id = text_layer(&mut s, "R");
    s.execute("type.rasterizeTypeLayer", json!({})).unwrap();
    assert!(matches!(s.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Raster(_)));
    assert!(!s.is_enabled("type.rasterizeTypeLayer"));
}

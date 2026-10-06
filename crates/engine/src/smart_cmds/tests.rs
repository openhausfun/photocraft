use super::*;

const DEPTHS: [u64; 3] = [8, 16, 32];
const W: i32 = 64;
const H: i32 = 48;

fn session(depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "depth": depth})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s
}

/// Paints a textured block on the active layer, partly outside the canvas (top-left), so
/// conversion has to keep off-canvas pixels and crop/translate correctly.
fn paint(s: &mut Session) {
    s.edit("setup", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let r = Rect::new(-6, 5, 40, 38);
        let mut data = Vec::new();
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let v = ((x * 7 + y * 13) % 23) as f32 / 22.0;
                let a = if (x + y) % 5 == 0 { 0.5 } else { 1.0 };
                data.extend(photocraft_raster::from_rgba(&fmt, [v, 1.0 - v, ((x * y) % 7) as f32 / 6.0, a]));
            }
        }
        surf.write_region(r, &data);
        Ok(())
    })
    .unwrap();
}

fn flat(s: &Session) -> Vec<[f32; 4]> {
    photocraft_compose::flatten(&s.active().unwrap().doc).px
}

fn max_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max)
}

fn mean_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    let sum: f32 = a.iter().zip(b).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum();
    sum / (a.len() * 4) as f32
}

fn active_smart(s: &Session) -> SmartObject {
    let d = s.active().unwrap();
    match &d.doc.layer(d.active_layer.unwrap()).unwrap().content {
        LayerContent::Smart(sm) => sm.clone(),
        other => panic!("not smart: {}", other.kind_name()),
    }
}

fn convert(s: &mut Session) -> u64 {
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap()
}

#[test]
fn conversion_is_pixel_identical_at_every_depth() {
    for depth in DEPTHS {
        let mut s = session(depth);
        paint(&mut s);
        let id = s.active().unwrap().active_layer.unwrap();
        s.edit("opacity", |doc, _| {
            doc.layer_mut(id).unwrap().opacity = 0.6;
            Ok(())
        })
        .unwrap();
        let before = flat(&s);
        convert(&mut s);
        assert_eq!(max_diff(&flat(&s), &before), 0.0, "depth {depth}");
        let d = s.active().unwrap();
        let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
        assert!((l.opacity - 0.6).abs() < 1e-6, "opacity stays on the smart layer");
        let sm = active_smart(&s);
        let SmartSource::Embedded { file_name, bytes } = &sm.source else { panic!() };
        assert!(file_name.ends_with(".pcraft"));
        // The nested document is cropped to the content, including the off-canvas part.
        let inner = decode_source(file_name, bytes).unwrap();
        assert_eq!((inner.size.width, inner.size.height), (46, 33));
        assert_eq!(inner.depth, d.doc.depth);
        assert_eq!(sm.transform, Affine::translate(-6.0, 5.0));
        // Re-rendering from the source reproduces the conversion cache exactly.
        let rendered = render(&d.doc, &sm).unwrap().unwrap();
        let b = Rect::new(-6, 5, 40, 38);
        assert_eq!(rendered.read_region(b), sm.cache.as_ref().unwrap().read_region(b), "depth {depth}");
        s.undo();
        assert_eq!(flat(&s), before);
    }
}

#[test]
fn masked_layer_and_group_convert_within_rounding() {
    let mut s = session(8);
    paint(&mut s);
    s.edit("mask", |doc, active| {
        let l = doc.layer_mut(active.unwrap()).unwrap();
        let mut m = LayerMask::reveal_all();
        m.surface.fill_rect(Rect::new(0, 0, 20, 20), &[0.3]);
        l.mask = Some(m);
        Ok(())
    })
    .unwrap();
    let before = flat(&s);
    convert(&mut s);
    assert!(max_diff(&flat(&s), &before) <= 1.0 / 255.0 + 1e-6);
    // A group converts too (its children move into the nested document).
    s.execute("layer.new.layer", json!({})).unwrap();
    paint(&mut s);
    s.execute("layer.groupLayers", json!({})).unwrap();
    let before = flat(&s);
    convert(&mut s);
    assert!(max_diff(&flat(&s), &before) <= 1.0 / 255.0 + 1e-6);
}

#[test]
fn empty_layer_cannot_be_converted() {
    let mut s = session(8);
    assert!(s.execute("layer.smartObjects.convertToSmartObject", json!({})).is_err());
}

#[test]
fn smart_filters_are_re_editable() {
    for depth in [8, 16] {
        let mut s = session(depth);
        paint(&mut s);
        let original = flat(&s);
        s.execute("filter.convertForSmartFilters", json!({})).unwrap();
        assert!(!s.is_enabled("filter.convertForSmartFilters"), "already smart");
        s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
        let r2 = flat(&s);
        assert!(max_diff(&r2, &original) > 0.05);
        let sm = active_smart(&s);
        assert_eq!(sm.smart_filters.len(), 1);
        let SmartSource::Embedded { bytes, .. } = &sm.source else { panic!() };
        // Change the radius: the result comes from the source, not from the blurred pixels.
        s.execute("layer.smartFilter.setParams", json!({"index": 0, "params": {"radius": 6}})).unwrap();
        let r6 = flat(&s);
        assert!(max_diff(&r6, &r2) > 0.01);
        s.execute("layer.smartFilter.setParams", json!({"params": {"radius": 2}})).unwrap();
        assert!(max_diff(&flat(&s), &r2) < 1e-6, "same params, same pixels");
        // Hiding the filter shows the original exactly.
        s.execute("layer.smartFilter.setVisible", json!({"index": 0})).unwrap();
        assert_eq!(flat(&s), original);
        assert!(!active_smart(&s).smart_filters[0].visible);
        s.undo();
        assert!(max_diff(&flat(&s), &r2) < 1e-6);
        s.redo();
        assert_eq!(flat(&s), original);
        // All of that decoded the contents at most once (the conversion seeded the cache).
        assert!(source_decode_count(bytes, s.active().unwrap().doc.pixel_format()) <= 1);
    }
}

#[test]
fn filter_stack_order_blend_disable_and_clear() {
    let mut s = session(8);
    paint(&mut s);
    let original = flat(&s);
    convert(&mut s);
    s.execute("filter.other.offset", json!({"horizontal": 5, "undefinedAreas": "transparent"})).unwrap();
    s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    let ab = flat(&s);
    s.execute("layer.smartFilter.move", json!({"index": 1, "to": 0})).unwrap();
    assert_eq!(active_smart(&s).smart_filters[0].command, "filter.blur.gaussianBlur");
    assert!(max_diff(&flat(&s), &ab) > 1e-3, "order matters");
    s.undo();
    assert_eq!(flat(&s), ab);
    // 0 % opacity on both filters = original.
    s.execute("layer.smartFilter.blendingOptions", json!({"index": 0, "opacity": 0})).unwrap();
    s.execute("layer.smartFilter.blendingOptions", json!({"index": 1, "opacity": 0, "blend": "multiply"})).unwrap();
    assert!(max_diff(&flat(&s), &original) < 1e-6);
    assert_eq!(active_smart(&s).smart_filters[1].blend, BlendMode::Multiply);
    assert!(s.execute("layer.smartFilter.blendingOptions", json!({"blend": "nope"})).is_err());
    s.undo();
    s.undo();
    s.execute("layer.smartFilter.disableSmartFilters", json!({})).unwrap();
    assert_eq!(flat(&s), original);
    assert!(!active_smart(&s).filters_enabled);
    s.execute("layer.smartFilter.disableSmartFilters", json!({})).unwrap();
    assert_eq!(flat(&s), ab);
    s.execute("layer.smartFilter.delete", json!({"index": 0})).unwrap();
    assert_eq!(active_smart(&s).smart_filters.len(), 1);
    assert!(s.execute("layer.smartFilter.delete", json!({"index": 5})).is_err());
    s.execute("layer.smartFilter.clearSmartFilters", json!({})).unwrap();
    assert_eq!(flat(&s), original);
    assert!(!s.is_enabled("layer.smartFilter.clearSmartFilters"));
}

#[test]
fn selection_becomes_the_filter_mask() {
    let mut s = session(8);
    paint(&mut s);
    let original = flat(&s);
    convert(&mut s);
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 16, "height": 48})).unwrap();
    s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    assert!(active_smart(&s).filter_mask.is_some());
    let px = flat(&s);
    let at = |v: &Vec<[f32; 4]>, x: i32, y: i32| v[(y * W + x) as usize];
    assert_eq!(at(&px, 30, 20), at(&original, 30, 20), "outside the mask: unfiltered");
    assert_ne!(at(&px, 6, 20), at(&original, 6, 20), "inside the mask: filtered");
    s.execute("layer.smartFilter.disableFilterMask", json!({})).unwrap();
    assert_ne!(at(&flat(&s), 30, 20), at(&original, 30, 20));
    s.execute("layer.smartFilter.deleteFilterMask", json!({})).unwrap();
    assert!(active_smart(&s).filter_mask.is_none());
    assert!(!s.is_enabled("layer.smartFilter.deleteFilterMask"));
}

/// Scales `id` to 10 % around the origin and back, returning the composite.
fn shrink_and_grow(s: &mut Session, id: u64) -> Vec<[f32; 4]> {
    s.execute("edit.transform", json!({"layer": id, "matrix": [0.1, 0, 0, 0.1, 0, 0]})).unwrap();
    s.execute("edit.transform", json!({"layer": id, "matrix": [10, 0, 0, 10, 0, 0]})).unwrap();
    flat(s)
}

#[test]
fn transforms_re_render_from_source_losslessly() {
    let mut raster = session(8);
    paint(&mut raster);
    let original = flat(&raster);
    let rid = raster.active().unwrap().active_layer.unwrap().0;
    let raster_result = shrink_and_grow(&mut raster, rid);

    let mut smart = session(8);
    paint(&mut smart);
    let sid = convert(&mut smart);
    // Scaled down, the smart object really is small...
    smart.execute("edit.transform", json!({"layer": sid, "matrix": [0.1, 0, 0, 0.1, 0, 0]})).unwrap();
    let small = active_smart(&smart).cache.unwrap().content_bounds();
    assert!(small.width() <= 7, "{small:?}");
    // ...and scaled back up it is the original again.
    smart.execute("edit.transform", json!({"layer": sid, "matrix": [10, 0, 0, 10, 0, 0]})).unwrap();
    let smart_result = flat(&smart);
    let (es, er) = (mean_diff(&smart_result, &original), mean_diff(&raster_result, &original));
    assert!(es < 1e-6, "smart error {es}");
    assert!(er > 0.02, "raster error {er}");
    // A non-integer move re-renders with bicubic; moving back is exact again.
    smart.execute("edit.transform", json!({"layer": sid, "matrix": [1, 0, 0, 1, 0.5, 0.25]})).unwrap();
    assert!(max_diff(&flat(&smart), &original) > 0.01);
    smart.execute("edit.transform", json!({"layer": sid, "matrix": [1, 0, 0, 1, -0.5, -0.25]})).unwrap();
    assert!(max_diff(&flat(&smart), &original) < 1e-6);
    // Whole-pixel moves (Move tool) shift without re-rendering.
    smart.execute("layer.translate", json!({"dx": 3, "dy": -2})).unwrap();
    assert_eq!(active_smart(&smart).transform, Affine::translate(-3.0, 3.0));
    // Perspective is refused (for now) rather than silently rasterizing.
    assert!(smart.execute("edit.transform", json!({"layer": sid, "quad": [[0, 0], [10, 0], [12, 10], [-2, 10]]})).is_err());
}

#[test]
fn filters_follow_transforms() {
    let mut s = session(16);
    paint(&mut s);
    let id = convert(&mut s);
    s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
    let blurred = flat(&s);
    s.execute("edit.transform", json!({"layer": id, "matrix": [2, 0, 0, 2, 0, 0]})).unwrap();
    assert_eq!(active_smart(&s).smart_filters.len(), 1);
    s.execute("edit.transform", json!({"layer": id, "matrix": [0.5, 0, 0, 0.5, 0, 0]})).unwrap();
    assert!(max_diff(&flat(&s), &blurred) < 1e-6, "filter re-applied to the re-rendered source");
}

#[test]
fn edit_contents_updates_the_parent() {
    let mut s = session(8);
    paint(&mut s);
    let id = convert(&mut s);
    let before = flat(&s);
    let r = s.execute("layer.smartObjects.editContents", json!({})).unwrap();
    let child = r["document"].as_u64().unwrap() as usize;
    assert_eq!(s.active_index(), Some(child));
    assert!(s.is_enabled("layer.smartObjects.saveContents"));
    // Paint the nested document's layer solid red.
    s.edit("paint", |doc, _| {
        let l = &mut doc.layers[0];
        let b = Rect::new(0, 0, 46, 33);
        l.surface_mut().unwrap().fill_rect(b, &[1.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    assert!(!s.active().unwrap().is_dirty());
    s.set_active(0);
    let after = flat(&s);
    assert_eq!(after[(20 * W + 10) as usize], [1.0, 0.0, 0.0, 1.0]);
    assert_ne!(after, before);
    s.undo();
    assert_eq!(flat(&s), before, "the update is one undoable step in the parent");
    s.redo();
    // Closing an edited contents document also commits it.
    s.set_active(child);
    s.edit("paint2", |doc, _| {
        doc.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 46, 33), &[0.0, 0.0, 1.0, 1.0]);
        Ok(())
    })
    .unwrap();
    s.execute("file.close", json!({})).unwrap();
    assert!(s.smart_links.is_empty());
    s.set_active(0);
    assert_eq!(flat(&s)[(20 * W + 10) as usize], [0.0, 0.0, 1.0, 1.0]);
    let d = s.active().unwrap();
    assert!(matches!(d.doc.layer(LayerId(id)).unwrap().content, LayerContent::Smart(_)));
}

#[test]
fn rasterize_and_via_copy() {
    let mut s = session(8);
    paint(&mut s);
    convert(&mut s);
    s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
    let blurred = flat(&s);
    // Via copy: an independent smart object above.
    let copy = s.execute("layer.smartObjects.newSmartObjectViaCopy", json!({})).unwrap()["layer"].as_u64().unwrap();
    assert_eq!(s.active().unwrap().active_layer, Some(LayerId(copy)));
    s.execute("layer.smartFilter.setParams", json!({"params": {"radius": 8}})).unwrap();
    let d = s.active().unwrap();
    let n_smart = d.doc.walk().iter().filter(|(_, _, l)| matches!(l.content, LayerContent::Smart(_))).count();
    assert_eq!(n_smart, 2);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("layer.smartObjects.rasterize", json!({})).unwrap();
    let d = s.active().unwrap();
    assert!(matches!(d.doc.layer(d.active_layer.unwrap()).unwrap().content, LayerContent::Raster(_)));
    assert!(max_diff(&flat(&s), &blurred) <= 1.0 / 255.0 + 1e-6);
    assert!(!s.is_enabled("layer.smartObjects.rasterize"));
}

#[test]
fn pcraft_round_trip_keeps_live_smart_objects() {
    for depth in [8, 16] {
        let mut s = session(depth);
        paint(&mut s);
        convert(&mut s);
        s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 30, "height": 30})).unwrap();
        s.execute("filter.noise.addNoise", json!({"amount": 20, "seed": 3})).unwrap();
        s.execute("layer.smartFilter.blendingOptions", json!({"opacity": 0.5, "blend": "screen"})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
        let back = photocraft_format::load_from_bytes(&bytes).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        let (a, b) = (smart(&doc, id).unwrap(), smart(&back, id).unwrap());
        assert_eq!(a, b, "depth {depth}");
        assert_eq!(b.smart_filters.len(), 2);
        assert!(b.filter_mask.is_some());
        // Still live after loading: re-editing works and matches a fresh render.
        let mut s2 = Session::new();
        s2.add_document(back, None);
        s2.select_layer(id).unwrap();
        s2.execute("layer.smartFilter.setVisible", json!({"index": 1, "visible": false})).unwrap();
        s.execute("layer.smartFilter.setVisible", json!({"index": 1, "visible": false})).unwrap();
        assert_eq!(flat(&s2), flat(&s));
    }
}

#[test]
fn psd_placed_layer_renders_from_lnk2_data() {
    // A PNG embedded the way Photoshop stores placed files: a global lnk2 block keyed by uuid.
    let mut inner = Document::with_background(
        "in",
        Size::new(10, 8),
        photocraft_color::ColorMode::Rgb,
        photocraft_color::SampleType::U8,
        photocraft_color::Color::rgba(0.0, 1.0, 0.0, 1.0),
    );
    inner.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 5, 8), &[1.0, 0.0, 0.0, 1.0]);
    let png = photocraft_io::export(&inner, "png", &Default::default()).unwrap().bytes;
    let lnk2 = photocraft_io::linked::encode_linked_file(&photocraft_io::linked::LinkedFile { uuid: "uuid-1".into(), file_name: "in.png".into(), bytes: png });
    let mut s = session(8);
    s.edit("place", |doc, active| {
        doc.metadata.psd_global_blocks.push((*b"8BIM", *b"lnk2", Arc::new(lnk2)));
        // Photoshop's rendering (deliberately different, to see the re-render happen).
        let mut cache = Surface::new(doc.pixel_format());
        cache.fill_rect(Rect::new(20, 10, 40, 26), &[0.0, 0.0, 1.0, 1.0]);
        let mut sm = SmartObject::new(SmartSource::Linked { path: "uuid-1".into() }, Affine { m: [2.0, 0.0, 0.0, 2.0, 20.0, 10.0] }, Some(cache));
        sm.psd_raw = Some(Arc::new(vec![1, 2, 3]));
        let mut l = Layer::new("placed", LayerContent::Smart(sm));
        l.psd_blocks.push((*b"SoLd", Arc::new(vec![1, 2, 3])));
        *active = Some(doc.insert_above(*active, l));
        Ok(())
    })
    .unwrap();
    // Nothing re-renders until something changes (PSD round trips stay byte-stable).
    assert_eq!(active_smart(&s).cache.unwrap().rgba(25, 12), [0.0, 0.0, 1.0, 1.0]);
    s.execute("filter.other.offset", json!({"horizontal": 0})).unwrap();
    let c = active_smart(&s).cache.unwrap();
    assert_eq!(c.rgba(23, 15), [1.0, 0.0, 0.0, 1.0], "left half red, from the source");
    assert_eq!(c.rgba(37, 15), [0.0, 1.0, 0.0, 1.0]);
    assert!(active_smart(&s).psd_raw.is_some(), "filters alone keep the placed-layer block");
    // Embedding resolves the lnk2 data.
    assert!(s.is_enabled("layer.smartObjects.convertToEmbedded"));
    s.execute("layer.smartObjects.convertToEmbedded", json!({})).unwrap();
    assert!(matches!(active_smart(&s).source, SmartSource::Embedded { ref file_name, .. } if file_name == "in.png"));
}

#[test]
fn export_replace_and_linked_contents() {
    let dir = std::env::temp_dir().join(format!("pcraft-smart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = session(8);
    paint(&mut s);
    convert(&mut s);
    let out = dir.join("contents.pcraft");
    s.execute("layer.smartObjects.exportContents", json!({"path": out.to_str().unwrap()})).unwrap();
    assert!(photocraft_format::is_pcraft(&std::fs::read(&out).unwrap()));
    // Replace with a flat PNG.
    let png_doc = Document::with_background(
        "p",
        Size::new(4, 4),
        photocraft_color::ColorMode::Rgb,
        photocraft_color::SampleType::U8,
        photocraft_color::Color::rgba(1.0, 1.0, 0.0, 1.0),
    );
    let png = dir.join("yellow.png");
    std::fs::write(&png, photocraft_io::export(&png_doc, "png", &Default::default()).unwrap().bytes).unwrap();
    s.execute("layer.smartObjects.replaceContents", json!({"path": png.to_str().unwrap()})).unwrap();
    let sm = active_smart(&s);
    assert_eq!(sm.cache.as_ref().unwrap().content_bounds(), Rect::new(-6, 5, -2, 9), "transform kept");
    assert!(s.execute("layer.smartObjects.replaceContents", json!({})).is_err());
    // Linked: write out, edit the file, update.
    let linked = dir.join("linked.png");
    s.execute("layer.smartObjects.convertToLinked", json!({"path": linked.to_str().unwrap()})).unwrap();
    assert!(matches!(active_smart(&s).source, SmartSource::Linked { .. }));
    let mut red = png_doc.clone();
    let rb = red.bounds();
    red.layers[0].surface_mut().unwrap().fill_rect(rb, &[1.0, 0.0, 0.0, 1.0]);
    std::fs::write(&linked, photocraft_io::export(&red, "png", &Default::default()).unwrap().bytes).unwrap();
    s.execute("layer.smartObjects.updateAllModifiedContent", json!({})).unwrap();
    assert_eq!(active_smart(&s).cache.unwrap().rgba(-5, 6), [1.0, 0.0, 0.0, 1.0]);
    s.execute("layer.smartObjects.convertToEmbedded", json!({})).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    // Missing linked file: re-rendering fails cleanly and changes nothing.
    s.execute("layer.smartObjects.relinkToFile", json!({"path": dir.join("gone.png").to_str().unwrap()})).unwrap_err();
    assert!(matches!(active_smart(&s).source, SmartSource::Embedded { .. }));
}

#[test]
fn commands_are_registered_with_catalog_ids() {
    for id in [
        "layer.smartObjects.convertToSmartObject",
        "filter.convertForSmartFilters",
        "layer.smartObjects.newSmartObjectViaCopy",
        "layer.smartObjects.rasterize",
        "layer.smartObjects.editContents",
        "layer.smartObjects.replaceContents",
        "layer.smartObjects.exportContents",
        "layer.smartObjects.updateModifiedContent",
        "layer.smartObjects.updateAllModifiedContent",
        "layer.smartObjects.convertToEmbedded",
        "layer.smartObjects.convertToLinked",
        "layer.smartObjects.relinkToFile",
        "layer.smartFilter.disableSmartFilters",
        "layer.smartFilter.clearSmartFilters",
        "layer.smartFilter.blendingOptions",
        "layer.smartFilter.deleteFilterMask",
        "layer.smartFilter.disableFilterMask",
    ] {
        let spec = crate::commands::find(id).unwrap_or_else(|| panic!("{id}"));
        assert!(!spec.menu.is_empty(), "{id}");
    }
    let s = Session::new();
    assert!(!s.is_enabled("layer.smartObjects.convertToSmartObject"));
    assert!(!s.is_enabled("layer.smartObjects.editContents"));
}

#[test]
fn smart_filter_blur_repeats_the_canvas_edge_like_a_layer_filter() {
    for depth in [8, 32] {
        let fill_canvas = |s: &mut Session| {
            s.edit("setup", |doc, active| {
                let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
                let fmt = surf.format();
                let r = Rect::new(0, 0, W, H);
                let mut data = Vec::new();
                for y in r.y0..r.y1 {
                    for x in r.x0..r.x1 {
                        let v = if (x / 8 + y / 8) % 2 == 0 { 0.2 } else { 0.9 };
                        data.extend(photocraft_raster::from_rgba(&fmt, [v, 0.5, 1.0 - v, 1.0]));
                    }
                }
                surf.write_region(r, &data);
                Ok(())
            })
            .unwrap();
        };
        let mut layer = session(depth);
        fill_canvas(&mut layer);
        layer.execute("filter.blur.gaussianBlur", json!({"radius": 4})).unwrap();
        let mut smart = session(depth);
        fill_canvas(&mut smart);
        convert(&mut smart);
        smart.execute("filter.blur.gaussianBlur", json!({"radius": 4})).unwrap();
        let (a, b) = (flat(&layer), flat(&smart));
        // The corner stays opaque (no transparency fades in from outside the canvas).
        assert!((b[0][3] - 1.0).abs() < 1e-3, "corner alpha {}", b[0][3]);
        assert!(max_diff(&a, &b) < 2.0 / 255.0, "smart re-render matches the layer filter ({depth}-bit)");
    }
}

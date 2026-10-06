//! Smart objects and smart filters made in PhotoCraft survive a PSD save and open (#215): the
//! layer comes back as a live smart object (not pixels) with the same nested document (embedded
//! in `lnk2` as a PSB), transform, filter list and filter mask, and re-renders like before.
//!
//! Set `PHOTOCRAFT_SMART_PSD_OUT=<dir>` to keep the exported files (for opening in Photoshop).

use photocraft_doc::{Document, LayerContent, SmartObject, SmartSource};
use photocraft_engine::Session;
use photocraft_engine::smart_cmds::{decode_source, refresh_layer, source_bytes};
use photocraft_geom::Rect;
use serde_json::json;

const W: i32 = 96;
const H: i32 = 64;

fn session(depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "depth": depth})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let r = Rect::new(8, 6, 80, 58);
        let mut data = Vec::new();
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let v = ((x * 7 + y * 13) % 23) as f32 / 22.0;
                data.extend(photocraft_raster::from_rgba(&fmt, [v, 1.0 - v, ((x * y) % 7) as f32 / 6.0, 1.0]));
            }
        }
        surf.write_region(r, &data);
        Ok(())
    })
    .unwrap();
    s
}

fn smart(doc: &Document) -> (u64, SmartObject) {
    doc.walk()
        .into_iter()
        .find_map(|(_, _, l)| match &l.content {
            LayerContent::Smart(sm) => Some((l.id.0, sm.clone())),
            _ => None,
        })
        .expect("a smart object")
}

fn max_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).flat_map(|(p, q)| (0..4).map(move |c| (p[c] * p[3].max(0.0) - q[c] * q[3].max(0.0)).abs())).fold(0.0, f32::max)
}

/// Every block with inner structure re-parses strictly (`TaggedBlock::check_structure`).
fn structure_errors(bytes: &[u8]) -> Vec<String> {
    let f = photocraft_psd::PsdFile::from_bytes(bytes).unwrap();
    let mut errs = Vec::new();
    for b in &f.global_blocks {
        if let Err(e) = b.check_structure() {
            errs.push(format!("global {}: {e}", b.key_str()));
        }
    }
    for l in f.layers() {
        for b in &l.blocks {
            if let Err(e) = b.check_structure() {
                errs.push(format!("layer {}: {e}", b.key_str()));
            }
        }
    }
    errs
}

fn keep(name: &str, bytes: &[u8]) {
    if let Ok(dir) = std::env::var("PHOTOCRAFT_SMART_PSD_OUT") {
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

#[test]
fn smart_objects_and_filters_survive_psd() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 40, "height": 64})).unwrap();
        s.execute("filter.noise.addNoise", json!({"amount": 20, "seed": 3, "distribution": "gaussian"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.smartFilter.blendingOptions", json!({"opacity": 0.5, "blend": "screen"})).unwrap();
        s.execute("filter.stylize.emboss", json!({"angle": 120, "height": 2, "amount": 80})).unwrap();
        s.execute("layer.smartFilter.setVisible", json!({"index": 2, "visible": false})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap().0;
        s.execute("edit.transform", json!({"layer": id, "matrix": [0.75, 0, 0, 0.75, 10, 4]})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let (_, before) = smart(&doc);
        let SmartSource::Embedded { bytes: pcraft, .. } = &before.source else { panic!("embedded") };
        let nested = decode_source("x.pcraft", pcraft).unwrap();

        let out = photocraft_io::export(&doc, "psd", &Default::default()).unwrap();
        assert!(!out.warnings.iter().any(|w| w.contains("pixels")), "{:?}", out.warnings);
        assert_eq!(structure_errors(&out.bytes), Vec::<String>::new());
        keep(&format!("smart-{depth}.psd"), &out.bytes);

        let back = photocraft_io::import("x.psd", &out.bytes).unwrap().document;
        let (bid, after) = smart(&back);
        assert!(matches!(after.source, SmartSource::Linked { .. }), "depth {depth}: source in lnk2");
        // Same filters, blending and settings (numbers may come back as floats, defaults spelled out).
        assert_eq!(after.smart_filters.len(), before.smart_filters.len());
        for (a, b) in after.smart_filters.iter().zip(&before.smart_filters) {
            assert_eq!((&a.command, a.blend, a.opacity, a.visible), (&b.command, b.blend, b.opacity, b.visible), "depth {depth}");
            for (k, v) in b.params.as_object().unwrap() {
                let w = &a.params[k];
                assert!(w == v || w.as_f64().zip(v.as_f64()).is_some_and(|(x, y)| x == y), "{}: {k} {v} came back as {w}", b.command);
            }
        }
        assert_eq!(after.filters_enabled, before.filters_enabled);
        // The filter mask covers the same pixels (PSD extends it with white past its bounds).
        let (ma, mb) = (after.filter_mask.as_ref().unwrap(), before.filter_mask.as_ref().unwrap());
        assert_eq!((ma.enabled, ma.linked), (mb.enabled, mb.linked));
        let area = doc.bounds().union(&before.cache.as_ref().unwrap().content_bounds());
        let (mut va, mut vb) = (Vec::new(), Vec::new());
        ma.values_into(area, &mut va);
        mb.values_into(area, &mut vb);
        assert_eq!(va, vb, "depth {depth}: filter mask");
        for (a, b) in after.transform.m.iter().zip(before.transform.m) {
            assert!((a - b).abs() < 1e-9, "depth {depth}: transform {:?} vs {:?}", after.transform, before.transform);
        }
        // The nested document came back (as the embedded PSB), layer for layer.
        let (name, src) = source_bytes(&back.metadata, &after.source).unwrap();
        assert!(name.ends_with(".psb"), "{name}");
        let inner = decode_source(&name, &src).unwrap();
        assert_eq!((inner.size, inner.layers.len(), inner.depth), (nested.size, nested.layers.len(), nested.depth));
        assert!(max_diff(&photocraft_compose::flatten(&inner).px, &photocraft_compose::flatten(&nested).px) <= 1.0 / 255.0 + 1e-6);
        // And it re-renders from that source like the original did.
        let mut l = back.layer(photocraft_doc::LayerId(bid)).unwrap().clone();
        assert!(refresh_layer(&back, &mut l).unwrap());
        let mut rerendered = back.clone();
        *rerendered.layer_mut(photocraft_doc::LayerId(bid)).unwrap() = l;
        let d = max_diff(&photocraft_compose::flatten(&rerendered).px, &photocraft_compose::flatten(&doc).px);
        assert!(d <= 2.0 / 255.0, "depth {depth}: re-render differs by {d}");

        // Saving the re-imported file again keeps the same embedded file (no growth).
        let again = photocraft_io::export(&back, "psd", &Default::default()).unwrap();
        assert_eq!(structure_errors(&again.bytes), Vec::<String>::new());
        assert!(again.bytes.len() <= out.bytes.len() + 64, "{} vs {}", again.bytes.len(), out.bytes.len());
    }
}

/// Every colour model: the smart object, its filters and the filter cache's unfiltered pixels
/// (in the document's model, CMYK inverted and 16-bit Lab scaled as in layer channels) survive.
#[test]
fn smart_filters_survive_psd_in_every_colour_model() {
    for (mode, depth) in [("cmyk", 8), ("cmyk", 16), ("grayscale", 8), ("lab", 8), ("lab", 16)] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 24, "mode": mode, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 4, "y": 4, "width": 20, "height": 12})).unwrap();
        s.execute("edit.fill", json!({"color": "#c83c28"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 1.5})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let out = photocraft_io::export(&doc, "psd", &Default::default()).unwrap();
        assert!(!out.warnings.iter().any(|w| w.contains("smart")), "{mode} {depth}: {:?}", out.warnings);
        assert_eq!(structure_errors(&out.bytes), Vec::<String>::new());
        let back = photocraft_io::import("x.psd", &out.bytes).unwrap().document;
        let ((_, a), (_, b)) = (smart(&doc), smart(&back));
        assert_eq!(b.smart_filters.len(), 1, "{mode} {depth}");
        assert_eq!(b.smart_filters[0].command, a.smart_filters[0].command);
        // The cache's unfiltered planes decode back to the unfiltered rendering.
        let f = photocraft_psd::PsdFile::from_bytes(&out.bytes).unwrap();
        let fx = f.global_blocks.iter().find(|g| &g.key == b"FEid").map(|g| photocraft_psd::filter_effects::FilterEffects::parse(&g.data).unwrap()).unwrap();
        let item = &fx.items[0];
        let (w, h) = item.rect.size().unwrap();
        let bits = u16::try_from(item.depth).unwrap();
        let first = item.slots[0].as_ref().unwrap().decode(w, h, bits).unwrap();
        let mut unfiltered = a.clone();
        unfiltered.smart_filters.clear();
        let px = photocraft_engine::smart_cmds::render(&doc, &unfiltered).unwrap().unwrap();
        let (x, y) = (10 - item.rect.left, 8 - item.rect.top);
        let at = (y as usize * w + x as usize) * usize::from(bits / 8);
        let stored = if bits == 8 { f32::from(first[at]) / 255.0 } else { f32::from(u16::from_be_bytes([first[at], first[at + 1]])) / 65535.0 };
        let v = px.sample_channel(10, 8, 0);
        let expect = if mode == "cmyk" { 1.0 - v } else { v };
        assert!((stored - expect).abs() < 0.01, "{mode} {depth}: slot 0 holds {stored}, expected {expect}");
    }
}

/// Writes sample files for checking in Photoshop (`PHOTOCRAFT_SMART_PSD_OUT`): per depth, a
/// plain layer, a smart object without filters, and one with filters.
#[test]
#[ignore = "writes files for a manual Photoshop check"]
fn photoshop_samples() {
    for (mode, depth) in [("rgb", 8), ("rgb", 16), ("rgb", 32), ("cmyk", 8), ("lab", 16), ("grayscale", 8)] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": W, "height": H, "mode": mode, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 10, "y": 8, "width": 50, "height": 40})).unwrap();
        s.execute("edit.fill", json!({"color": "#c83c28"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        let tag = format!("{mode}{depth}");
        keep(&format!("plain-{tag}.psd"), &photocraft_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        keep(&format!("nofilter-{tag}.psd"), &photocraft_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
        s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
        keep(&format!("gauss-{tag}.psd"), &photocraft_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
    }
}

/// PSD export of a 24 MP smart object versus the same layer as pixels (release:
/// `cargo test --release -p photocraft-engine --test smart_psd -- --ignored --nocapture perf`).
#[test]
#[ignore = "benchmark"]
fn perf_24mp_smart_object_export() {
    let (w, h) = (6000, 4000);
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let r = Rect::new(0, 0, w, h);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                // Photo-like: smooth gradients plus a little texture (RLE can't collapse it).
                let v = (x as f32 / w as f32) * 0.7 + (((x * 31 + y * 17) % 29) as f32 / 29.0) * 0.1;
                data.extend(photocraft_raster::from_rgba(&fmt, [v, (y as f32 / h as f32) * 0.8, 1.0 - v, 1.0]));
            }
        }
        surf.write_region(r, &data);
        Ok(())
    })
    .unwrap();
    let time = |doc: &Document| {
        let t = std::time::Instant::now();
        let out = photocraft_io::export(doc, "psd", &Default::default()).unwrap();
        (t.elapsed().as_secs_f64(), out.bytes.len(), out.warnings)
    };
    let (tf, sf, _) = time(&s.active().unwrap().doc);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let (ts, ss, ws) = time(&s.active().unwrap().doc);
    assert!(ws.is_empty(), "{ws:?}");
    s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    let (tg, sg, wg) = time(&s.active().unwrap().doc);
    assert!(wg.is_empty(), "{wg:?}");
    let mb = |b: usize| b as f64 / 1e6;
    eprintln!(
        "24 MP PSD export: pixels {tf:.2} s {:.1} MB | smart object {ts:.2} s {:.1} MB | + Gaussian Blur smart filter {tg:.2} s {:.1} MB",
        mb(sf),
        mb(ss),
        mb(sg)
    );
    // The embedded file is about one more copy of the layer; the filter cache one more again.
    assert!(ss < sf * 5 / 2, "smart object {ss} vs pixels {sf}");
    assert!(sg < sf * 4, "with filters {sg} vs pixels {sf}");
}

#[test]
fn edited_psd_smart_objects_drop_their_old_file() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let first = photocraft_io::export(&doc, "psd", &Default::default()).unwrap().bytes;
    // Re-open, edit the contents (a new embedded file), save: one embedded file, the new one.
    let mut s2 = Session::new();
    s2.add_document(photocraft_io::import("a.psd", &first).unwrap().document, None);
    let (id, _) = smart(&s2.active().unwrap().doc);
    s2.select_layer(photocraft_doc::LayerId(id)).unwrap();
    s2.execute("layer.smartObjects.editContents", json!({})).unwrap();
    s2.execute("image.adjustments.invert", json!({})).unwrap();
    s2.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    let parent = s2.documents().iter().position(|d| d.doc.layer(photocraft_doc::LayerId(id)).is_some()).unwrap();
    let doc2 = s2.documents()[parent].doc.clone();
    let second = photocraft_io::export(&doc2, "psd", &Default::default()).unwrap().bytes;
    let f = photocraft_psd::PsdFile::from_bytes(&second).unwrap();
    let uuids: Vec<String> = f.global_blocks.iter().filter(|b| &b.key == b"lnk2").flat_map(|b| photocraft_io::linked::block_uuids(&b.data)).collect();
    let back = photocraft_io::import("b.psd", &second).unwrap().document;
    let (_, sm) = smart(&back);
    let SmartSource::Linked { path } = &sm.source else { panic!() };
    assert_eq!(uuids, vec![path.clone()]);
}

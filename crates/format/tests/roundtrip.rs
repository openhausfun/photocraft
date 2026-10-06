//! doc → .pcraft → doc equality, incremental saves, ids, previews.

mod common;
use common::*;
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerId};
use photocraft_format::*;
use photocraft_raster::Rgba8Image;

const MODES: [ColorMode; 4] = [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Cmyk, ColorMode::Lab];

fn check_zip(mode: ColorMode, depth: SampleType) {
    let doc = rich_doc(mode, depth);
    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    assert!(is_pcraft(&bytes));
    let back = load_from_bytes(&bytes).unwrap();
    assert_eq!(back, doc, "{mode:?} {depth:?}");
}

macro_rules! zip_cases {
    ($($name:ident: $m:ident, $d:ident;)*) => {$(
        #[test]
        fn $name() {
            check_zip(ColorMode::$m, SampleType::$d);
        }
    )*};
}

zip_cases! {
    rgb_u8: Rgb, U8; rgb_u16: Rgb, U16; rgb_f32: Rgb, F32;
    gray_u8: Grayscale, U8; gray_u16: Grayscale, U16; gray_f32: Grayscale, F32;
    cmyk_u8: Cmyk, U8; cmyk_u16: Cmyk, U16; cmyk_f32: Cmyk, F32;
    lab_u8: Lab, U8; lab_u16: Lab, U16; lab_f32: Lab, F32;
}

#[test]
fn directory_bundle_roundtrip_all_modes() {
    for mode in MODES {
        for depth in SampleType::ALL {
            let doc = rich_doc(mode, depth);
            let dir = temp_dir("dir");
            PcraftWriter::new().save_dir(&doc, &dir, &SaveOptions::default()).unwrap();
            assert!(dir.join("manifest.json").is_file());
            assert_eq!(load_path(&dir).unwrap(), doc, "{mode:?} {depth:?}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn save_path_zip_file() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let dir = temp_dir("file");
    let path = dir.join("a.pcraft");
    PcraftWriter::new().save_path(&doc, &path, &SaveOptions::default()).unwrap();
    assert!(path.is_file());
    assert_eq!(load_path(&path).unwrap(), doc);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn empty_document_roundtrips() {
    let doc = Document::new("empty", photocraft_doc::Size::new(1, 1), ColorMode::Rgb, SampleType::U8);
    assert_eq!(load_from_bytes(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap(), doc);
}

#[test]
fn zip_incremental_only_new_tiles() {
    let mut doc = rich_doc(ColorMode::Rgb, SampleType::U16);
    let mut w = PcraftWriter::new();
    let (_, s1) = w.save_zip(&doc, &SaveOptions::default()).unwrap();
    assert!(s1.tiles_total > 5);
    assert_eq!(s1.tiles_written, s1.tiles_total);
    let (_, s2) = w.save_zip(&doc, &SaveOptions::default()).unwrap();
    assert_eq!(s2.tiles_written, 0);
    assert_eq!(s2.tiles_reused, s2.tiles_total);
    assert_eq!(s2.blobs_written, 0);
    // Touch one pixel of one layer: exactly one tile is new.
    let id = doc.layers[1].id;
    doc.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(3, 3, &[0.5, 0.5, 0.5, 1.0]);
    let (bytes, s3) = w.save_zip(&doc, &SaveOptions::default()).unwrap();
    assert_eq!(s3.tiles_written, 1);
    assert_eq!(load_from_bytes(&bytes).unwrap(), doc);
}

#[test]
fn directory_incremental_and_gc() {
    let mut doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let dir = temp_dir("gc");
    let mut w = PcraftWriter::new();
    let s1 = w.save_dir(&doc, &dir, &SaveOptions::default()).unwrap();
    assert_eq!(s1.tiles_written, s1.tiles_total);
    // A fresh writer still skips files already on disk.
    let s2 = PcraftWriter::new().save_dir(&doc, &dir, &SaveOptions::default()).unwrap();
    assert_eq!(s2.tiles_written, 0);
    assert_eq!(s2.objects_removed, 0);
    // Delete a layer: its unique tiles get garbage-collected.
    let paint = doc.layers[1].id;
    doc.remove(paint);
    let s3 = w.save_dir(&doc, &dir, &SaveOptions::default()).unwrap();
    assert!(s3.objects_removed > 0, "{s3:?}");
    assert_eq!(load_path(&dir).unwrap(), doc);
    let files = std::fs::read_dir(dir.join("tiles")).unwrap().count();
    assert_eq!(files, s3.tiles_total);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identical_tiles_are_stored_once() {
    let mut doc = Document::new("d", photocraft_doc::Size::new(10, 10), ColorMode::Rgb, SampleType::U8);
    let mut a = Layer::raster("a", doc.pixel_format());
    scribble(a.surface_mut().unwrap(), 5, false);
    let b = a.duplicate();
    doc.layers.push(a);
    doc.layers.push(b);
    let (_, s) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    let per_layer = doc.layers[0].surface().unwrap().tile_count();
    assert_eq!(s.tiles_total, per_layer);
}

#[test]
fn ids_preserved_and_counter_advanced() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    // Simulate a file written with much larger ids.
    let mut m = read_manifest(&bytes).unwrap();
    m.document.layers[0].id = LayerId::fresh().0 + 5000;
    let big = m.document.layers[0].id;
    let text = serde_json::to_vec(&m).unwrap();
    let mut z = zip::ZipWriter::new();
    z.add("manifest.json", &text).unwrap();
    let r = zip::ZipReader::new(&bytes).unwrap();
    for e in r.entries.iter().filter(|e| e.name != "manifest.json") {
        z.add(&e.name, &r.read(e, usize::MAX).unwrap()).unwrap();
    }
    let back = load_from_bytes(&z.finish().unwrap()).unwrap();
    assert_eq!(back.layers[0].id.0, big);
    assert!(LayerId::fresh().0 > big, "fresh ids must not collide with loaded ids");
}

#[test]
fn fresh_ids_option() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    let back = load_from_bytes_with(&bytes, &LoadOptions { preserve_ids: false, ..Default::default() }).unwrap();
    assert_ne!(back.id, doc.id);
    assert_ne!(back.layers[0].id, doc.layers[0].id);
    assert_eq!(back.layers.len(), doc.layers.len());
    assert_eq!(back.layers[1].surface(), doc.layers[1].surface());
}

#[test]
fn previews_embedded_and_readable() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let mut thumb = Rgba8Image::new(4, 3);
    thumb.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = i as u8);
    let opts = SaveOptions { thumbnail: Some(thumb.clone()), composite: Some(thumb.clone()) };
    let bytes = save_to_bytes(&doc, &opts).unwrap();
    let png = read_thumbnail(&bytes).unwrap().unwrap();
    let img = photocraft_codecs::decode(&png).unwrap();
    assert_eq!(img.dimensions(), (4, 3));
    assert_eq!(img.to_rgba8(), thumb.pixels);
    let m = read_manifest(&bytes).unwrap();
    assert_eq!(m.composite.as_deref(), Some("composite/preview.png"));
    assert_eq!(m.format_version, FORMAT_VERSION);
    assert!(read_thumbnail(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap().is_none());
}

#[test]
fn manifest_is_human_readable_json() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    let r = zip::ZipReader::new(&bytes).unwrap();
    assert_eq!(r.entries[0].name, "manifest.json");
    let text = String::from_utf8(r.read_by_name("manifest.json", usize::MAX).unwrap()).unwrap();
    assert!(text.contains("\"format_version\": 1"));
    assert!(text.contains("\"Hue/Sat\""));
    assert!(text.contains("filter.blur.gaussian"));
    assert!(r.entries.iter().any(|e| e.name.starts_with("tiles/") && e.name.ends_with(".zst")));
    assert!(r.entries.iter().any(|e| e.name.starts_with("blobs/")));
}

#[test]
fn save_is_deterministic() {
    let doc = rich_doc(ColorMode::Cmyk, SampleType::U16);
    let a = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    let b = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn not_pcraft() {
    assert!(!is_pcraft(b""));
    assert!(!is_pcraft(b"8BPS...."));
    let mut z = zip::ZipWriter::new();
    z.add("other.txt", b"x").unwrap();
    assert!(!is_pcraft(&z.finish().unwrap()));
}

#[test]
fn vector_fields_roundtrip_and_default_when_absent() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let back = load_from_bytes(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap();
    assert_eq!(back.paths, doc.paths);
    assert_eq!(back.work_path, doc.work_path);
    assert_eq!(back.clipping_path, doc.clipping_path);
    let vm = |d: &Document| d.walk().into_iter().find_map(|(_, _, l)| l.vector_mask.clone());
    assert!(vm(&back).is_some());
    assert_eq!(vm(&back), vm(&doc));
    // Manifests written before the vector fields existed still load (serde defaults).
    let m = read_manifest(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap();
    let mut v = serde_json::to_value(&m.document).unwrap();
    for k in ["paths", "work_path", "clipping_path"] {
        v.as_object_mut().unwrap().remove(k);
    }
    fn strip(layers: &mut serde_json::Value) {
        for l in layers.as_array_mut().unwrap() {
            l.as_object_mut().unwrap().remove("vector_mask");
            let c = l["content"].as_object_mut().unwrap();
            for k in ["path", "stroke", "live"] {
                c.remove(k);
            }
            if let Some(ch) = c.get_mut("children") {
                strip(ch);
            }
        }
    }
    strip(&mut v["layers"]);
    let old: photocraft_format::manifest::DocM = serde_json::from_value(v).unwrap();
    assert!(old.paths.is_empty() && old.work_path.is_none());
    assert!(old.layers.iter().all(|l| l.vector_mask.is_none()));
}

/// Type kerning (#206): the auto mode and manual kerning survive .pcraft; manifests written
/// before `kern` existed load with no manual kerning.
#[test]
fn text_kerning_roundtrips_and_defaults() {
    use photocraft_doc::LayerContent;
    use photocraft_doc::text::Kerning;
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    let back = load_from_bytes(&bytes).unwrap();
    let runs = |d: &Document| {
        d.walk()
            .into_iter()
            .find_map(|(_, _, l)| match &l.content {
                LayerContent::Text(t) => Some(t.runs.iter().map(|r| (r.len, r.style.kerning, r.style.kern)).collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(runs(&back), vec![(1, Kerning::Off, 120.0), (5, Kerning::Metrics, 0.0), (9, Kerning::Optical, 0.0)]);
    assert_eq!(runs(&back), runs(&doc));
    let m = read_manifest(&bytes).unwrap();
    let mut v = serde_json::to_value(&m.document).unwrap();
    fn strip(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                o.remove("kern");
                o.values_mut().for_each(strip);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    assert!(!v.to_string().contains("\"kern\""));
    let old: photocraft_format::manifest::DocM = serde_json::from_value(v).unwrap();
    let s = serde_json::to_string(&old).unwrap();
    assert!(s.contains("\"kern\":0.0"), "kern defaults to 0");
}

#[test]
fn channel_restrictions_and_bevel_elements_roundtrip() {
    use photocraft_doc::{Bevel, BevelContour, BevelTexture, Contour, Effect};
    let mut doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    doc.layers[0].excluded_channels = 0b101;
    let mut b = Bevel {
        enabled: true,
        style: photocraft_doc::BevelStyle::Emboss,
        technique: photocraft_doc::BevelTechnique::ChiselHard,
        depth: 1.2,
        up: false,
        size: 9.0,
        soften: 1.0,
        angle: 30.0,
        altitude: 40.0,
        use_global_light: false,
        gloss_contour: Contour::Linear,
        highlight: photocraft_doc::FxCommon::new(photocraft_color::BlendMode::Screen, 0.7),
        highlight_color: photocraft_color::Color::WHITE,
        shadow: photocraft_doc::FxCommon::new(photocraft_color::BlendMode::Multiply, 0.6),
        shadow_color: photocraft_color::Color::BLACK,
        contour: None,
        texture: None,
    };
    b.contour = Some(BevelContour { contour: Contour::Linear, range: 0.3, anti_alias: true });
    b.texture = Some(BevelTexture { name: "p".into(), id: "i".into(), scale: 0.5, depth: -1.5, invert: true, link: false, phase: (1.0, 2.0) });
    doc.layers[0].effects.items.push(Effect::BevelEmboss(Bevel { ..b }));
    let back = load_from_bytes(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap();
    assert_eq!(back, doc);
    assert_eq!(back.layers[0].excluded_channels, 0b101);
}

#[test]
fn blend_if_roundtrips() {
    use photocraft_doc::{BlendIf, BlendRange};
    let mut doc = rich_doc(ColorMode::Rgb, SampleType::U16);
    let mut bi = BlendIf::default();
    bi.set(0, [BlendRange { black: [20, 60], white: [255, 255] }, BlendRange::FULL]);
    bi.set(3, [BlendRange::FULL, BlendRange { black: [0, 0], white: [180, 220] }]);
    doc.layers[0].blend_if = bi.clone();
    let back = load_from_bytes(&save_to_bytes(&doc, &SaveOptions::default()).unwrap()).unwrap();
    assert_eq!(back, doc);
    assert_eq!(back.layers[0].blend_if, bi);
    // Layers without Blend If keep the default (the field is omitted from the manifest).
    assert!(back.layers.iter().skip(1).all(|l| l.blend_if.is_default()));
}

#[test]
fn video_layer_frames_survive_roundtrip() {
    use photocraft_doc::{Timeline, VideoData, VideoSource};
    use photocraft_geom::Rect;
    let mut doc = Document::new("Vid", photocraft_geom::Size { width: 8, height: 6 }, ColorMode::Rgb, SampleType::U8);
    doc.timeline = Some(Timeline::new(3, 24.0));
    let fmt = doc.pixel_format();
    let mut l = Layer::raster("Clip", fmt);
    let mut frames = Vec::new();
    for i in 0..3u8 {
        let mut s = photocraft_raster::Surface::new(fmt);
        let v = i as f32 / 2.0;
        s.fill_rect(Rect::new(0, 0, 8, 6), &photocraft_raster::from_rgba(&fmt, [v, 0.0, 1.0 - v, 1.0]));
        frames.push(s);
    }
    let mut vd = VideoData::new(frames, 24.0);
    vd.source = VideoSource::File { path: "/tmp/seq".into() };
    vd.show_altered = false;
    l.video = Some(vd);
    doc.layers.push(l);

    let bytes = save_to_bytes(&doc, &SaveOptions::default()).unwrap();
    let back = load_from_bytes(&bytes).unwrap();
    let v = back.layers.last().unwrap().video.as_ref().expect("video survived");
    assert_eq!(v.frames.len(), 3);
    assert_eq!(v.fps, 24.0);
    assert!(!v.show_altered);
    assert_eq!(v.source, VideoSource::File { path: "/tmp/seq".into() });
    assert_eq!(back.timeline.as_ref().unwrap().duration, 3);
    // A middle frame's pixel survived.
    let mut px = [[0.0f32; 4]; 1];
    v.frames[2].read_rgba_into(Rect::from_xywh(1, 1, 1, 1), &mut px);
    assert!(px[0][0] > 0.9 && px[0][2] < 0.1, "frame 2 is reddish: {:?}", px[0]);
}

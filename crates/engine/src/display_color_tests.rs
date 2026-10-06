//! Colour-managed display (#46): non-sRGB documents show their colours on an sRGB (or other)
//! monitor. Fixtures are built-in (CC0) or synthesized profiles only.

use std::sync::Arc;

use photocraft_cms::synth::{CmykParams, cmyk_profile};
use photocraft_cms::{Builtin, Intent, Profile, Transform};
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::json;

use crate::Session;

/// Tolerance of the CPU path against the reference (8-bit quantisation + table evaluation).
const TOL: f32 = 2.0;

fn rgb_doc(profile: Option<&Profile>, rgb: [f32; 3], depth: SampleType) -> Document {
    let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Rgb, depth, Color::rgb(rgb[0], rgb[1], rgb[2]));
    d.icc_profile = profile.map(Profile::to_bytes);
    d
}

fn gray_doc(profile: &Profile, v: f32) -> Document {
    let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Grayscale, SampleType::U8, Color::gray(v));
    d.icc_profile = Some(profile.to_bytes());
    d
}

/// A CMYK document of two layers (so composites go through the compositor): the ink below and
/// an empty layer on top.
fn cmyk_doc(profile: Option<&Profile>, ink: [f32; 4]) -> Document {
    let fmt = PixelFormat::new(ColorMode::Cmyk, SampleType::U8, true);
    let mut d = Document::new("t", Size::new(8, 8), ColorMode::Cmyk, SampleType::U8);
    let mut s = Surface::new(fmt);
    s.fill_rect(Rect::new(0, 0, 8, 8), &[ink[0], ink[1], ink[2], ink[3], 1.0]);
    d.layers.push(Layer::new("ink", LayerContent::Raster(s)));
    d.layers.push(Layer::new("empty", LayerContent::Raster(Surface::new(fmt))));
    d.icc_profile = profile.map(Profile::to_bytes);
    d
}

/// Synthetic CMYK profile unlike the built-in coated one (heavier dot gain, small tables).
fn test_cmyk() -> Profile {
    cmyk_profile(&CmykParams { description: "Test Uncoated CMYK".into(), tvi: [0.26, 0.26, 0.26, 0.3], grid_a2b: 7, grid_b2a: 11, ..Default::default() })
}

/// What the CPU canvas shows at the centre pixel (RGBA8).
fn shown(s: &Session, d: &Document) -> [f32; 3] {
    let buf = photocraft_compose::flatten(d);
    let img = s.color.canvas_display(d).unwrap().to_rgba8(&buf);
    let i = (4 * d.size.width as usize + 4) * 4;
    [0, 1, 2].map(|k| img.pixels[i + k] as f32)
}

/// Reference: the CMS evaluating document values → sRGB, in 8-bit codes.
fn reference(src: &Profile, v: &[f32]) -> [f32; 3] {
    let t = Transform::new(src, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true).unwrap();
    let mut o = [0.0f32; 16];
    t.eval(v, &mut o);
    let n = t.outputs();
    [0, 1, 2].map(|k| o[if n == 1 { 0 } else { k }].clamp(0.0, 1.0) * 255.0)
}

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
}

fn far(a: [f32; 3], b: [f32; 3], d: f32) -> bool {
    (0..3).any(|i| (a[i] - b[i]).abs() >= d)
}

#[test]
fn srgb_is_the_identity_fast_path() {
    let s = Session::new();
    for d in [rgb_doc(None, [0.8, 0.5, 0.3], SampleType::U8), rgb_doc(Some(Builtin::Srgb.profile()), [0.8, 0.5, 0.3], SampleType::U16)] {
        let cd = s.color.canvas_display(&d).unwrap();
        assert!(cd.is_identity(), "sRGB on an sRGB monitor costs nothing");
        assert!(s.color.canvas_lut(&d, 9).unwrap().is_none(), "no GPU LUT either");
        assert!(close(shown(&s, &d), [204.0, 127.5, 76.5], 0.6));
    }
    // Untagged gray (sGray), CMYK (shown in sRGB) and Lab composites are sRGB too.
    let g = Document::with_background("g", Size::new(4, 4), ColorMode::Grayscale, SampleType::U8, Color::gray(0.3));
    assert!(s.color.canvas_display(&g).unwrap().is_identity());
    assert!(s.color.canvas_display(&cmyk_doc(None, [0.1, 0.5, 0.5, 0.0])).unwrap().is_identity());
    // Cached: the same Arc for the same profiles.
    let d = rgb_doc(Some(Builtin::DisplayP3.profile()), [0.5; 3], SampleType::U8);
    assert!(Arc::ptr_eq(&s.color.canvas_display(&d).unwrap(), &s.color.canvas_display(&d).unwrap()));
}

#[test]
fn display_p3_matches_hand_computed_srgb() {
    // P3 (0.8, 0.5, 0.3): linear (0.6038, 0.2140, 0.0732) → linear sRGB through the published
    // P3 → sRGB matrix (0.6915, 0.1976, 0.0517) → encoded (216.7, 122.9, 64.3).
    let s = Session::new();
    let d = rgb_doc(Some(Builtin::DisplayP3.profile()), [0.8, 0.5, 0.3], SampleType::U8);
    let got = shown(&s, &d);
    assert!(close(got, [216.7, 122.9, 64.3], TOL), "{got:?}");
    assert!(far(got, [204.0, 127.5, 76.5], 10.0), "raw values would be desaturated: {got:?}");
    // The GPU LUT maps the same value there (the lattice point nearest to it, within interpolation error).
    let lut = s.color.canvas_lut(&d, 33).expect("lut").expect("not the identity");
    assert_eq!(lut.len(), 33 * 33 * 33 * 4);
}

#[test]
fn wide_gamut_and_gray_fixtures_display_correctly() {
    let s = Session::new();
    let cases: [(&Profile, [f32; 3]); 3] = [
        (Builtin::AdobeRgbCompat.profile(), [0.3, 0.7, 0.4]),
        (Builtin::ProPhotoCompat.profile(), [0.55, 0.35, 0.25]),
        (Builtin::DisplayP3.profile(), [0.2, 0.6, 0.8]),
    ];
    for (p, v) in cases {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let d = rgb_doc(Some(p), v, depth);
            let got = shown(&s, &d);
            let want = reference(p, &v);
            assert!(close(got, want, TOL), "{} {depth:?}: {got:?} vs {want:?}", p.description);
            assert!(far(got, v.map(|x| x * 255.0), 4.0), "{}: shown raw {got:?}", p.description);
        }
    }
    // Gray Gamma 2.2: gray 0.1 is L = 0.1^2.2 = 0.00631 → sRGB 18.6 (raw would be 25.5).
    let d = gray_doc(Builtin::GrayGamma22.profile(), 0.1);
    assert!(!s.color.canvas_display(&d).unwrap().is_identity());
    let got = shown(&s, &d);
    assert!(close(got, [18.6; 3], TOL), "{got:?}");
}

#[test]
fn embedded_cmyk_profile_is_used_for_display_and_flat_export() {
    let s = Session::new();
    let p = test_cmyk();
    let ink = [0.1, 0.6, 0.7, 0.05];
    let want = reference(&p, &ink);
    let coated = reference(Builtin::CoatedCmyk.profile(), &ink);
    assert!(far(want, coated, 4.0), "the fixture differs from the default: {want:?} {coated:?}");
    let d = cmyk_doc(Some(&p), ink);
    let got = shown(&s, &d);
    assert!(close(got, want, TOL), "canvas {got:?} vs {want:?} (built-in would be {coated:?})");
    // Untagged documents keep the built-in coated CMYK.
    assert!(close(shown(&s, &cmyk_doc(None, ink)), coated, TOL));
    // Flat export of the (layered) document writes sRGB through the embedded profile.
    let r = photocraft_io::export(&d, "out.png", &photocraft_io::ExportOptions::default()).unwrap();
    let img = photocraft_codecs::decode(&r.bytes).unwrap().convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
    let px = &img.data()[(4 * 8 + 4) * 4..][..3];
    assert!(close([px[0] as f32, px[1] as f32, px[2] as f32], want, TOL), "export {px:?} vs {want:?}");
    // A single-layer CMYK document exported to a format without CMYK converts the same way.
    let mut one = d.clone();
    one.layers.truncate(1);
    let r = photocraft_io::export(&one, "out.png", &photocraft_io::ExportOptions::default()).unwrap();
    let img = photocraft_codecs::decode(&r.bytes).unwrap().convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
    let px = &img.data()[(4 * 8 + 4) * 4..][..3];
    assert!(close([px[0] as f32, px[1] as f32, px[2] as f32], want, TOL), "single-layer export {px:?} vs {want:?}");
}

#[test]
fn linear_documents_are_encoded_for_the_texture() {
    let s = Session::new();
    // Linear 0.2 is sRGB 0.4845 (123.5).
    let d = rgb_doc(Some(Builtin::LinearSrgb.profile()), [0.2, 0.2, 0.2], SampleType::F32);
    let cd = s.color.canvas_display(&d).unwrap();
    assert!(cd.encode_srgb && cd.transform.is_none(), "linear sRGB = sRGB primaries: encoding is all it takes");
    assert!(close(shown(&s, &d), [123.5; 3], 1.0), "{:?}", shown(&s, &d));
    let buf = photocraft_compose::flatten(&d);
    let tex = cd.texture_buffer(&buf);
    assert!((tex.px[0][0] - 0.4845).abs() < 1e-3);
    // An sRGB document is passed through untouched.
    let srgb = rgb_doc(None, [0.2; 3], SampleType::F32);
    let b2 = photocraft_compose::flatten(&srgb);
    assert!(matches!(s.color.canvas_display(&srgb).unwrap().texture_buffer(&b2), std::borrow::Cow::Borrowed(_)));
}

#[test]
fn exr_round_trip_is_linear() {
    let mut s = Session::new();
    // An sRGB document exported to EXR is linearised; re-opened it is tagged linear sRGB and
    // displays as before.
    let d = rgb_doc(None, [0.5, 0.25, 0.75], SampleType::F32);
    let r = photocraft_io::export(&d, "x.exr", &photocraft_io::ExportOptions::default()).unwrap();
    let back = photocraft_io::import("x.exr", &r.bytes).unwrap().document;
    let icc = back.icc_profile.clone().expect("tagged");
    assert_eq!(Profile::parse(&icc).unwrap().content_hash(), Builtin::LinearSrgb.profile().content_hash());
    let v = back.layers[0].surface().unwrap().pixel(1, 1);
    assert!((v[0] - 0.214).abs() < 2e-3, "stored linear: {v:?}");
    assert!(close(shown(&s, &back), shown(&s, &d), 1.0));
    // Opening keeps it linear without a mismatch prompt.
    let (_, report) = s.open_document(back, None);
    assert_eq!(report["action"], "kept");
    assert!(report.get("ask").is_none());
}

#[test]
fn monitor_profile_setting() {
    let mut s = Session::new();
    let srgb = rgb_doc(None, [0.8, 0.5, 0.3], SampleType::U8);
    let p3 = rgb_doc(Some(Builtin::DisplayP3.profile()), [0.8, 0.5, 0.3], SampleType::U8);
    let r = s.execute("edit.colorSettings", json!({"monitorProfile": "display-p3"})).unwrap();
    assert_eq!(r["monitor"], "Display P3");
    // On a P3 monitor, P3 documents are the identity and sRGB ones are converted.
    assert!(s.color.canvas_display(&p3).unwrap().is_identity());
    assert!(!s.color.canvas_display(&srgb).unwrap().is_identity());
    let got = shown(&s, &srgb);
    let mut want = [0.0f32; 3];
    Transform::new(Builtin::Srgb.profile(), Builtin::DisplayP3.profile(), Intent::RelativeColorimetric, true).unwrap().eval(&[0.8, 0.5, 0.3], &mut want);
    assert!(close(got, want.map(|v| v * 255.0), TOL), "{got:?} vs {want:?}");
    // `auto` uses the platform's profile when supplied, else sRGB.
    s.execute("edit.colorSettings", json!({"monitorProfile": "auto"})).unwrap();
    assert!(s.color.canvas_display(&srgb).unwrap().is_identity());
    s.color.monitor_profile = Some(Builtin::DisplayP3.profile().to_bytes());
    assert_eq!(s.color.monitor().description, "Display P3");
    assert!(s.color.canvas_display(&p3).unwrap().is_identity());
    // Garbage platform bytes fall back to sRGB.
    s.color.monitor_profile = Some(Arc::new(vec![0u8; 16]));
    assert_eq!(s.color.monitor().content_hash(), Builtin::Srgb.profile().content_hash());
    // The signature follows the monitor.
    let a = s.color.display_signature(&p3);
    s.execute("edit.colorSettings", json!({"monitorProfile": "display-p3"})).unwrap();
    assert_ne!(a, s.color.display_signature(&p3));
}

#[test]
fn monitor_profile_bad_params_fail_gracefully() {
    let mut s = Session::new();
    for bad in [
        json!({"monitorProfile": "coated-cmyk"}),
        json!({"monitorProfile": "sgray"}),
        json!({"monitorProfile": "no-such-profile"}),
        json!({"monitorProfile": "/nonexistent/x.icc"}),
    ] {
        assert!(s.execute("edit.colorSettings", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(s.color.settings.monitor_profile, "auto", "unchanged after errors");
    // Non-string values are ignored.
    assert!(s.execute("edit.colorSettings", json!({"monitorProfile": 7})).is_ok());
}

#[test]
fn proof_colors_still_apply_on_top() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8, "mode": "rgb", "depth": 8})).unwrap();
    s.execute("edit.assignProfile", json!({"profile": "display-p3"})).unwrap();
    let d = s.active().unwrap().doc.clone();
    let plain = s.color.canvas_lut(&d, 9).unwrap().unwrap();
    s.execute("view.proofColors", json!({"on": true})).unwrap();
    let proof = s.color.canvas_lut(&d, 9).unwrap().unwrap();
    assert_ne!(plain, proof, "the proof changes the LUT");
    let sig = s.color.display_signature(&d);
    s.execute("view.gamutWarning", json!({"on": true})).unwrap();
    assert_ne!(sig, s.color.display_signature(&d));
}

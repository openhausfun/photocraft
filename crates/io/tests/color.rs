//! ICC profiles through import/export: byte-exact PSD round trips with real profiles, PNG/JPEG
//! embedding and extraction, colour-managed CMYK → RGB for formats without CMYK.

mod common;

use std::sync::Arc;

use common::*;
use photocraft_cms::{Builtin, ColorSpace, Profile};
use photocraft_color::{ColorMode, SampleType};
use photocraft_io::*;

fn single(mode: ColorMode, depth: SampleType, alpha: bool) -> photocraft_doc::Document {
    let mut d = photocraft_doc::Document::new("s", photocraft_geom::Size::new(9, 6), mode, depth);
    let fmt = d.pixel_format();
    d.layers.push(raster("Background", fmt, d.bounds(), 3, alpha));
    d
}

fn real_profiles() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = Builtin::ALL.iter().map(|b| b.profile().to_bytes().to_vec()).collect();
    for p in ["/System/Library/ColorSync/Profiles/Generic CMYK Profile.icc", "/System/Library/ColorSync/Profiles/AdobeRGB1998.icc"] {
        if let Ok(b) = std::fs::read(p) {
            v.push(b);
        }
    }
    v
}

#[test]
fn psd_icc_roundtrip_byte_exact() {
    for icc in real_profiles() {
        let p = Profile::parse(&icc).unwrap();
        let mode = match p.color_space {
            ColorSpace::Cmyk => ColorMode::Cmyk,
            ColorSpace::Gray => ColorMode::Grayscale,
            ColorSpace::Lab => ColorMode::Lab,
            _ => ColorMode::Rgb,
        };
        let mut d = gen_doc(mode, SampleType::U8, Features::PIXELS);
        d.icc_profile = Some(Arc::new(icc.clone()));
        let r = export(&d, "x.psd", &ExportOptions::default()).unwrap();
        let back = import("x.psd", &r.bytes).unwrap().document;
        assert_eq!(back.icc_profile.as_deref(), Some(&icc), "{}", p.description);
    }
}

#[test]
fn png_and_jpeg_embed_and_extract_icc() {
    for b in [Builtin::Srgb, Builtin::DisplayP3, Builtin::AdobeRgbCompat, Builtin::ProPhotoCompat] {
        let icc = b.profile().to_bytes().to_vec();
        for (name, alpha) in [("x.png", true), ("x.jpg", false)] {
            let mut d = single(ColorMode::Rgb, SampleType::U8, alpha);
            d.icc_profile = Some(Arc::new(icc.clone()));
            let r = export(&d, name, &ExportOptions::default()).unwrap();
            let back = import(name, &r.bytes).unwrap().document;
            assert_eq!(back.icc_profile.as_deref(), Some(&icc), "{b:?} {name}");
            assert_eq!(Profile::parse(back.icc_profile.as_ref().unwrap()).unwrap().description, b.description());
        }
    }
    // Gray PNG with a gray profile.
    let icc = Builtin::GrayGamma22.profile().to_bytes().to_vec();
    let mut d = single(ColorMode::Grayscale, SampleType::U16, false);
    d.icc_profile = Some(Arc::new(icc.clone()));
    let r = export(&d, "g.png", &ExportOptions::default()).unwrap();
    assert_eq!(import("g.png", &r.bytes).unwrap().document.icc_profile.as_deref(), Some(&icc));
}

#[test]
fn cmyk_to_png_is_colour_managed_and_tagged_srgb() {
    // Native single-layer CMYK document written to PNG: converted through the CMYK profile.
    let mut d = single(ColorMode::Cmyk, SampleType::U8, false);
    d.icc_profile = Some(Builtin::CoatedCmyk.profile().to_bytes());
    let r = export(&d, "c.png", &ExportOptions::default()).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("sRGB")), "{:?}", r.warnings);
    let back = import("c.png", &r.bytes).unwrap().document;
    assert_eq!(back.mode, ColorMode::Rgb);
    let icc = back.icc_profile.expect("tagged");
    assert_eq!(Profile::parse(&icc).unwrap().description, Builtin::Srgb.profile().description);
    // Lab documents never write Lab numbers as RGB.
    let d = single(ColorMode::Lab, SampleType::U8, false);
    let r = export(&d, "l.png", &ExportOptions::default()).unwrap();
    let back = import("l.png", &r.bytes).unwrap().document;
    assert_eq!(Profile::parse(back.icc_profile.as_ref().unwrap()).unwrap().color_space, ColorSpace::Rgb);
}

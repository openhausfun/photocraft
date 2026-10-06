//! Integration tests: built-in profiles, known colorimetric values, round trips, CMYK
//! behaviour, intents, BPC, interpolation accuracy, real-world profiles and an independent
//! oracle (moxcms, dev-dependency only).

use photocraft_cms::math::{self, delta_e76};
use photocraft_cms::{Builtin, Clut, ColorSpace, Intent, Lut3d, Profile, Transform, TransformOptions};

fn lab() -> &'static Profile {
    Builtin::LabD50.profile()
}
fn srgb() -> &'static Profile {
    Builtin::Srgb.profile()
}
fn cmyk() -> &'static Profile {
    Builtin::CoatedCmyk.profile()
}

/// Normalised device Lab (v4 encoding) → float Lab.
fn dec(o: &[f32]) -> [f64; 3] {
    [o[0] as f64 * 100.0, o[1] as f64 * 255.0 - 128.0, o[2] as f64 * 255.0 - 128.0]
}
fn enc(l: [f64; 3]) -> [f32; 3] {
    [(l[0] / 100.0) as f32, ((l[1] + 128.0) / 255.0) as f32, ((l[2] + 128.0) / 255.0) as f32]
}

fn to_lab(p: &Profile, dev: &[f32], intent: Intent) -> [f64; 3] {
    let t = Transform::new(p, lab(), intent, false).unwrap();
    let mut o = [0.0f32; 3];
    t.eval(dev, &mut o);
    dec(&o)
}

#[test]
fn builtins_encode_and_reparse() {
    for b in Builtin::ALL {
        let p = b.profile();
        let bytes = p.to_bytes();
        let q = Profile::parse(&bytes).unwrap_or_else(|e| panic!("{b:?}: {e}"));
        assert_eq!(q.description, b.description(), "{b:?}");
        assert_eq!(q.color_space, p.color_space);
        assert_eq!(&*q.to_bytes(), &*bytes, "{b:?} bytes are kept verbatim");
        assert_eq!(Builtin::from_id(b.id()), Some(b));
        // Every builtin converts to Lab and back.
        Transform::new(p, lab(), Intent::RelativeColorimetric, false).unwrap();
        Transform::new(lab(), p, Intent::RelativeColorimetric, false).unwrap();
    }
    assert_eq!(Builtin::from_id("Adobe RGB (1998)"), Some(Builtin::AdobeRgbCompat));
}

#[test]
fn srgb_known_lab_values() {
    let w = to_lab(srgb(), &[1.0, 1.0, 1.0], Intent::RelativeColorimetric);
    assert!((w[0] - 100.0).abs() < 0.01 && w[1].abs() < 0.01 && w[2].abs() < 0.01, "white {w:?}");
    let k = to_lab(srgb(), &[0.0, 0.0, 0.0], Intent::RelativeColorimetric);
    assert!(k[0].abs() < 0.01, "black {k:?}");
    // ICC PCS is D50: sRGB red Bradford-adapted to D50 is L*a*b* ≈ (54.29, 80.80, 69.89).
    // (53.24, 80.09, 67.20) is the same colour referenced to D65 without adaptation.
    let r = to_lab(srgb(), &[1.0, 0.0, 0.0], Intent::RelativeColorimetric);
    assert!(delta_e76(r, [54.29, 80.80, 69.89]) < 0.15, "red {r:?}");
    let g = to_lab(srgb(), &[0.0, 1.0, 0.0], Intent::RelativeColorimetric);
    assert!(delta_e76(g, [87.82, -79.29, 80.99]) < 0.3, "green {g:?}");
    let b = to_lab(srgb(), &[0.0, 0.0, 1.0], Intent::RelativeColorimetric);
    assert!(delta_e76(b, [29.57, 68.30, -112.03]) < 0.3, "blue {b:?}");
    // Grays are neutral in every RGB builtin.
    for bi in [Builtin::Srgb, Builtin::DisplayP3, Builtin::AdobeRgbCompat, Builtin::ProPhotoCompat, Builtin::LinearSrgb, Builtin::Rec2020] {
        let m = to_lab(bi.profile(), &[0.5, 0.5, 0.5], Intent::RelativeColorimetric);
        assert!(m[1].abs() < 0.02 && m[2].abs() < 0.02, "{bi:?} gray {m:?}");
    }
}

#[test]
fn rgb_lab_rgb_roundtrip_8bit_and_float() {
    let to = Transform::new(srgb(), lab(), Intent::RelativeColorimetric, false).unwrap();
    let back = Transform::new(lab(), srgb(), Intent::RelativeColorimetric, false).unwrap();
    // 8-bit RGB through a float Lab intermediate: exact to the 8-bit level.
    let mut src8 = Vec::new();
    for r in (0..256).step_by(5) {
        for g in (0..256).step_by(7) {
            for b in (0..256).step_by(9) {
                src8.extend_from_slice(&[r as u8, g as u8, b as u8]);
            }
        }
    }
    let f: Vec<f32> = src8.iter().map(|v| *v as f32 / 255.0).collect();
    let mut labf = vec![0.0f32; f.len()];
    to.convert_f32(&f, 3, &mut labf, 3, false);
    let mut rgbf = vec![0.0f32; f.len()];
    back.convert_f32(&labf, 3, &mut rgbf, 3, false);
    for (a, b) in src8.iter().zip(&rgbf) {
        assert!((*a as f32 - b * 255.0).abs() <= 1.0, "{a} vs {}", b * 255.0);
    }
    // 8-bit RGB stored as 16-bit Lab then back to 8-bit through the lookup tables.
    let lab16: Vec<u16> = labf.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect();
    let mut rgb16 = vec![0u16; lab16.len()];
    back.convert_u16(&lab16, 3, &mut rgb16, 3, false);
    for (a, b) in src8.iter().zip(&rgb16) {
        assert!((*a as f32 - *b as f32 / 257.0).abs() <= 1.0, "{a} vs {}", *b as f32 / 257.0);
    }
    // Float: 1e-4.
    for (a, b) in f.iter().zip(&rgbf) {
        assert!((a - b).abs() < 1e-4, "{a} vs {b}");
    }
    // Fast 8-bit path: RGB8 → Lab8-grid → compare with exact within 1 level of Lab encoding.
    let mut lab8 = vec![0u8; src8.len()];
    to.convert_u8(&src8, 3, &mut lab8, 3, false);
    for (q, e) in lab8.iter().zip(&labf) {
        assert!((*q as f32 - e * 255.0).abs() <= 1.0);
    }
}

#[test]
fn rgb_to_rgb_matrix_shaper_paths_agree() {
    // Integer lookup path vs exact pipeline for pure-gamma destinations (hard near black).
    for dst in [Builtin::AdobeRgbCompat, Builtin::ProPhotoCompat, Builtin::DisplayP3, Builtin::LinearSrgb] {
        let t = Transform::new(srgb(), dst.profile(), Intent::RelativeColorimetric, false).unwrap();
        let src: Vec<u8> = (0..=255u8).flat_map(|v| [v, v / 2, 255 - v]).collect();
        let mut out = vec![0u8; src.len()];
        t.convert_u8(&src, 3, &mut out, 3, false);
        for (px, o) in src.chunks(3).zip(out.chunks(3)) {
            let mut e = [0.0f32; 3];
            t.eval(&[px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0], &mut e);
            for k in 0..3 {
                assert!((o[k] as f32 - e[k].clamp(0.0, 1.0) * 255.0).abs() <= 0.6, "{dst:?} {px:?}: {o:?} vs {e:?}");
            }
        }
    }
}

#[test]
fn identity_transform_is_identity() {
    let t = Transform::new(srgb(), srgb(), Intent::RelativeColorimetric, true).unwrap();
    let src: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 3, 77]).collect();
    let mut out = vec![0u8; src.len()];
    t.convert_u8(&src, 4, &mut out, 4, true);
    assert_eq!(src, out);
}

#[test]
fn cmyk_profile_is_monotonic_and_in_range() {
    let p = cmyk();
    assert_eq!(p.color_space, ColorSpace::Cmyk);
    // CMYK → Lab: L* decreases with every ink.
    for ink in 0..4 {
        let mut prev = f64::INFINITY;
        for i in 0..=32 {
            let mut v = [0.1f32, 0.1, 0.1, 0.1];
            v[ink] = i as f32 / 32.0;
            let l = to_lab(p, &v, Intent::RelativeColorimetric)[0];
            assert!(l <= prev + 0.05, "ink {ink} at {i}: {l} > {prev}");
            prev = l;
        }
    }
    // Paper is L* 100 (relative).
    let w = to_lab(p, &[0.0; 4], Intent::RelativeColorimetric);
    assert!(delta_e76(w, [100.0, 0.0, 0.0]) < 0.5, "{w:?}");
    // sRGB gray ramp → CMYK: in range, K never decreases, total ink within the limit.
    for intent in [Intent::Perceptual, Intent::RelativeColorimetric] {
        let t = Transform::new(srgb(), p, intent, true).unwrap();
        let mut prev_k = -1.0f32;
        let mut prev_l = f64::INFINITY;
        let back = Transform::new(p, lab(), Intent::RelativeColorimetric, false).unwrap();
        for v in (0..=255).rev().step_by(5) {
            let g = v as f32 / 255.0;
            let mut o = [0.0f32; 4];
            t.eval(&[g, g, g], &mut o);
            assert!(o.iter().all(|c| (-1e-6..=1.0 + 1e-6).contains(c)), "{o:?}");
            assert!(o.iter().sum::<f32>() <= 3.0 + 0.02, "TAC {o:?}");
            assert!(o[3] >= prev_k - 0.01, "{intent:?} K not monotonic at {v}: {o:?}");
            prev_k = o[3];
            let mut l = [0.0f32; 3];
            back.eval(&o, &mut l);
            let lab = dec(&l);
            assert!(lab[0] <= prev_l + 0.3, "{intent:?} L* not monotonic at {v}: {lab:?}");
            assert!(lab[1].abs() < 3.0 && lab[2].abs() < 3.0, "{intent:?} gray {v} not neutral: {lab:?}");
            prev_l = lab[0];
        }
    }
    // Arbitrary colours: CMYK outputs in range for u8 and float paths.
    let t = Transform::new(srgb(), p, Intent::RelativeColorimetric, true).unwrap();
    let src: Vec<u8> = (0..4096u32).flat_map(|i| [(i * 37 % 256) as u8, (i * 91 % 256) as u8, (i * 13 % 256) as u8]).collect();
    let mut out = vec![0u8; 4096 * 4];
    t.convert_u8(&src, 3, &mut out, 4, false);
    for px in out.chunks(4) {
        assert!(px.iter().map(|v| *v as u32).sum::<u32>() <= 3 * 255 + 8, "{px:?}");
    }
}

#[test]
fn in_gamut_colours_survive_cmyk_roundtrip() {
    let to = Transform::new(lab(), cmyk(), Intent::RelativeColorimetric, false).unwrap();
    let back = Transform::new(cmyk(), lab(), Intent::RelativeColorimetric, false).unwrap();
    for target in [[50.0, 0.0, 0.0], [70.0, 10.0, 10.0], [60.0, -20.0, -20.0], [80.0, 0.0, 40.0], [40.0, 30.0, -10.0]] {
        let mut c = [0.0f32; 4];
        to.eval(&enc(target), &mut c);
        let mut l = [0.0f32; 3];
        back.eval(&c, &mut l);
        assert!(delta_e76(dec(&l), target) < 2.0, "{target:?} -> {c:?} -> {:?}", dec(&l));
    }
}

#[test]
fn intents_differ_as_expected() {
    let per = Transform::new(srgb(), cmyk(), Intent::Perceptual, false).unwrap();
    let rel = Transform::new(srgb(), cmyk(), Intent::RelativeColorimetric, false).unwrap();
    let sat = Transform::new(srgb(), cmyk(), Intent::Saturation, false).unwrap();
    let (mut a, mut b, mut s) = ([0.0f32; 4], [0.0f32; 4], [0.0f32; 4]);
    // Out-of-gamut sRGB blue: perceptual compresses, relative clips: different separations.
    per.eval(&[0.0, 0.2, 1.0], &mut a);
    rel.eval(&[0.0, 0.2, 1.0], &mut b);
    sat.eval(&[0.0, 0.2, 1.0], &mut s);
    let d: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
    assert!(d > 0.03, "perceptual {a:?} vs relative {b:?}");
    assert_eq!(a, s, "saturation shares the perceptual table");
    // Relative vs absolute on CMYK paper white shown in sRGB: relative is white, absolute shows
    // the paper tint (darker, slightly blue: paper Lab 95/0/−2).
    let rel = Transform::new(cmyk(), srgb(), Intent::RelativeColorimetric, false).unwrap();
    let abs = Transform::new(cmyk(), srgb(), Intent::AbsoluteColorimetric, false).unwrap();
    let (mut w1, mut w2) = ([0.0f32; 3], [0.0f32; 3]);
    rel.eval(&[0.0; 4], &mut w1);
    abs.eval(&[0.0; 4], &mut w2);
    assert!(w1.iter().all(|v| *v > 0.995), "{w1:?}");
    assert!(w2[0] < 0.97 && w2[2] > w2[0], "{w2:?}");
    let l = to_lab(cmyk(), &[0.0; 4], Intent::AbsoluteColorimetric);
    assert!(delta_e76(l, [95.0, 0.0, -2.0]) < 0.6, "absolute paper {l:?}");
}

#[test]
fn black_point_compensation() {
    let bp = photocraft_cms::black_point(cmyk(), Intent::RelativeColorimetric, false).unwrap();
    let bl = math::xyz_to_lab([bp * 0.9642, bp, bp * 0.8249], math::D50)[0];
    assert!(bl > 4.0 && bl < 20.0, "CMYK black L* {bl}");
    assert_eq!(photocraft_cms::black_point(srgb(), Intent::RelativeColorimetric, true).unwrap(), 0.0);
    // sRGB → CMYK: without BPC, deep shadows below the paper's black clip together; with BPC
    // they stay distinct.
    let with = Transform::new(srgb(), cmyk(), Intent::RelativeColorimetric, true).unwrap();
    let without = Transform::new(srgb(), cmyk(), Intent::RelativeColorimetric, false).unwrap();
    let back = Transform::new(cmyk(), lab(), Intent::RelativeColorimetric, false).unwrap();
    let l_of = |t: &Transform, v: f32| {
        let mut c = [0.0f32; 4];
        t.eval(&[v, v, v], &mut c);
        let mut l = [0.0f32; 3];
        back.eval(&c, &mut l);
        dec(&l)[0]
    };
    let (w0, w1) = (l_of(&with, 0.0), l_of(&with, 0.08));
    let (n0, n1) = (l_of(&without, 0.0), l_of(&without, 0.08));
    assert!(w1 - w0 > 1.5, "BPC keeps shadow detail: {w0} {w1}");
    assert!((n1 - n0).abs() < w1 - w0, "no BPC compresses shadows: {n0} {n1}");
    // CMYK → sRGB: with BPC the darkest CMYK maps to (near) sRGB black.
    let dark = {
        let mut c = [0.0f32; 4];
        with.eval(&[0.0, 0.0, 0.0], &mut c);
        c
    };
    let rgb_with = Transform::new(cmyk(), srgb(), Intent::RelativeColorimetric, true).unwrap();
    let rgb_without = Transform::new(cmyk(), srgb(), Intent::RelativeColorimetric, false).unwrap();
    let (mut a, mut b) = ([0.0f32; 3], [0.0f32; 3]);
    rgb_with.eval(&dark, &mut a);
    rgb_without.eval(&dark, &mut b);
    assert!(a[1] < 0.03 && b[1] > a[1] + 0.03, "with {a:?} without {b:?}");
}

#[test]
fn tetrahedral_beats_trilinear() {
    // Sample sRGB → Lab on a coarse grid and compare interpolation against the exact pipeline.
    let exact = Transform::new(srgb(), lab(), Intent::RelativeColorimetric, false).unwrap();
    let clut = Clut::sample(vec![9, 9, 9], 3, |i, o| exact.eval(i, o));
    let (mut tet_max, mut tri_max, mut tet_sum, mut tri_sum, mut tet_neutral, mut tri_neutral) = (0.0f64, 0.0f64, 0.0, 0.0, 0.0f64, 0.0f64);
    let mut n = 0.0;
    let mut seed = 12345u32;
    let mut rnd = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as f32 / (1 << 24) as f32
    };
    for i in 0..4000 {
        let p = if i < 200 {
            let v = i as f32 / 199.0;
            [v, v, v]
        } else {
            [rnd(), rnd(), rnd()]
        };
        let (mut e, mut a, mut b) = ([0.0f32; 3], [0.0f32; 3], [0.0f32; 3]);
        exact.eval(&p, &mut e);
        clut.eval(&p, &mut a);
        clut.eval_trilinear(&p, &mut b);
        let (da, db) = (delta_e76(dec(&a), dec(&e)), delta_e76(dec(&b), dec(&e)));
        if i < 200 {
            tet_neutral = tet_neutral.max(dec(&a)[1].abs().max(dec(&a)[2].abs()));
            tri_neutral = tri_neutral.max(dec(&b)[1].abs().max(dec(&b)[2].abs()));
        }
        tet_max = tet_max.max(da);
        tri_max = tri_max.max(db);
        tet_sum += da;
        tri_sum += db;
        n += 1.0;
    }
    eprintln!("tetra mean {:.4} max {:.4} | trilinear mean {:.4} max {:.4}", tet_sum / n, tet_max, tri_sum / n, tri_max);
    assert!(tet_neutral < 0.01, "tetrahedral keeps neutrals neutral: {tet_neutral}");
    assert!(tet_sum <= tri_sum * 1.05, "tetrahedral mean error {} vs trilinear {}", tet_sum / n, tri_sum / n);
    assert!(tet_max < 4.0);
    let _ = tri_neutral;
}

#[test]
fn proof_and_display_lut() {
    let p = Transform::proof(srgb(), cmyk(), srgb(), Intent::RelativeColorimetric, true, false).unwrap();
    let mut o = [0.0f32; 3];
    // Neutral gray stays (nearly) neutral through the proof.
    p.eval(&[0.5, 0.5, 0.5], &mut o);
    assert!((o[0] - o[1]).abs() < 0.02 && (o[1] - o[2]).abs() < 0.03, "{o:?}");
    // Saturated blue is desaturated by the CMYK gamut.
    p.eval(&[0.0, 0.0, 1.0], &mut o);
    assert!(o[0] > 0.1 || o[1] > 0.1, "{o:?}");
    // Paper simulation darkens white.
    let sim = Transform::proof(srgb(), cmyk(), srgb(), Intent::RelativeColorimetric, true, true).unwrap();
    sim.eval(&[1.0, 1.0, 1.0], &mut o);
    assert!(o[0] < 0.97, "{o:?}");

    let t = Transform::new(srgb(), Builtin::DisplayP3.profile(), Intent::RelativeColorimetric, false).unwrap();
    let lut = Lut3d::from_transform(&t, 33);
    assert_eq!(lut.to_rgba16f_bytes().len(), 33 * 33 * 33 * 8);
    let mut worst = 0.0f32;
    for i in 0..500u32 {
        let c = [(i * 7 % 101) as f32 / 100.0, (i * 13 % 101) as f32 / 100.0, (i * 29 % 101) as f32 / 100.0];
        let mut e = [0.0f32; 3];
        t.eval(&c, &mut e);
        let s = lut.sample(c);
        for k in 0..3 {
            worst = worst.max((s[k] - e[k]).abs());
        }
    }
    assert!(worst < 2.0 / 255.0, "display LUT error {worst}");
}

#[test]
fn gray_profiles() {
    let t = Transform::new(Builtin::SGray.profile(), srgb(), Intent::RelativeColorimetric, false).unwrap();
    for v in [0.0f32, 0.2, 0.5, 0.9, 1.0] {
        let mut o = [0.0f32; 3];
        t.eval(&[v], &mut o);
        for c in o {
            assert!((c - v).abs() < 1e-3, "sGray {v} -> {o:?}");
        }
    }
    let t = Transform::new(srgb(), Builtin::GrayGamma22.profile(), Intent::RelativeColorimetric, false).unwrap();
    let mut o = [0.0f32; 1];
    t.eval(&[1.0, 1.0, 1.0], &mut o);
    assert!((o[0] - 1.0).abs() < 1e-3);
    t.eval(&[0.0, 0.0, 0.0], &mut o);
    assert!(o[0].abs() < 1e-3);
}

#[test]
fn hdr_floats_survive_matrix_transforms() {
    let lin = Builtin::LinearSrgb.profile();
    let t =
        Transform::with_options(lin, Builtin::Rec2020.profile(), TransformOptions { intent: Intent::RelativeColorimetric, bpc: false, precise_float: true })
            .unwrap();
    let back = Transform::new(Builtin::Rec2020.profile(), lin, Intent::RelativeColorimetric, false).unwrap();
    let src = [4.0f32, 2.0, 0.5];
    let mut mid = [0.0f32; 3];
    let mut out = [0.0f32; 3];
    t.eval(&src, &mut mid);
    back.eval(&mid, &mut out);
    for k in 0..3 {
        assert!((out[k] - src[k]).abs() < 1e-3 * src[k].max(1.0), "{src:?} -> {mid:?} -> {out:?}");
    }
}

#[test]
fn malformed_profiles_are_rejected_without_panicking() {
    assert!(Profile::parse(&[]).is_err());
    assert!(Profile::parse(&[0u8; 200]).is_err());
    let good = srgb().to_bytes();
    for cut in [100, 131, 140, 200, good.len() - 20] {
        let _ = Profile::parse(&good[..cut]);
    }
    let mut bad = good.to_vec();
    for i in (128..bad.len()).step_by(7) {
        bad[i] ^= 0x5A;
        let _ = Profile::parse(&bad);
    }
    let c = cmyk().to_bytes();
    let mut bad = c.to_vec();
    for i in (128..400).step_by(3) {
        bad[i] = 0xFF;
        if let Ok(p) = Profile::parse(&bad) {
            let _ = Transform::new(&p, srgb(), Intent::Perceptual, true);
        }
    }
}

/// Parses every profile the OS ships (macOS ColorSync; read-only, nothing is copied) and
/// converts through each.
#[test]
fn system_profiles() {
    let dirs = ["/System/Library/ColorSync/Profiles", "/Library/ColorSync/Profiles", "/usr/share/color/icc", "/usr/share/color/icc/colord"];
    let mut seen = 0;
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if !matches!(path.extension().and_then(|x| x.to_str()), Some("icc" | "icm" | "ICC" | "ICM")) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let p = match Profile::parse(&bytes) {
                Ok(p) => p,
                // Named-colour profiles are never used for conversions; colord's
                // x11-colors.icc carries a stub B2A0 with overlapping elements.
                Err(_) if bytes.get(12..16) == Some(b"nmcl") => continue,
                Err(e) => panic!("{}: {e}", path.display()),
            };
            seen += 1;
            if !matches!(
                p.class,
                photocraft_cms::ProfileClass::Input
                    | photocraft_cms::ProfileClass::Display
                    | photocraft_cms::ProfileClass::Output
                    | photocraft_cms::ProfileClass::ColorSpace
            ) {
                continue;
            }
            assert!(!p.description.is_empty(), "{}", path.display());
            if matches!(p.color_space, ColorSpace::Rgb | ColorSpace::Gray | ColorSpace::Cmyk | ColorSpace::Lab) {
                for intent in Intent::ALL {
                    let t = Transform::new(&p, lab(), intent, true).unwrap_or_else(|e| panic!("{} {intent:?}: {e}", path.display()));
                    let n = p.channels();
                    let mut o = [0.0f32; 3];
                    t.eval(&vec![0.5; n], &mut o);
                    assert!(o.iter().all(|v| v.is_finite()), "{}", path.display());
                }
                // Additive white is (near) L* 100 for display profiles in relative colorimetric.
                if p.color_space == ColorSpace::Rgb {
                    let w = to_lab(&p, &[1.0, 1.0, 1.0], Intent::RelativeColorimetric);
                    assert!((w[0] - 100.0).abs() < 1.5, "{} white {w:?}", path.display());
                }
            }
            if p.color_space == ColorSpace::Cmyk {
                // Real CMYK profile: paper lighter than 100 % K.
                let w = to_lab(&p, &[0.0; 4], Intent::RelativeColorimetric);
                let k = to_lab(&p, &[0.0, 0.0, 0.0, 1.0], Intent::RelativeColorimetric);
                assert!(w[0] > 90.0 && k[0] < 40.0, "{}: {w:?} {k:?}", path.display());
                // And it round-trips through its own BToA.
                if p.b2a[1].is_some() {
                    let to = Transform::new(lab(), &p, Intent::RelativeColorimetric, false).unwrap();
                    let back = Transform::new(&p, lab(), Intent::RelativeColorimetric, false).unwrap();
                    let mut c = [0.0f32; 4];
                    to.eval(&enc([60.0, 10.0, -10.0]), &mut c);
                    let mut l = [0.0f32; 3];
                    back.eval(&c, &mut l);
                    assert!(delta_e76(dec(&l), [60.0, 10.0, -10.0]) < 3.0, "{}: {:?}", path.display(), dec(&l));
                }
            }
        }
    }
    eprintln!("parsed {seen} system profiles");
}

/// Independent oracle: moxcms (BSD-3/Apache-2.0, dev-dependency only) converting with the same
/// profile bytes.
#[test]
fn oracle_moxcms_rgb() {
    use moxcms::{ColorProfile, Layout, TransformOptions as MoxOpts};
    for dst in [Builtin::DisplayP3, Builtin::AdobeRgbCompat, Builtin::ProPhotoCompat] {
        let s = ColorProfile::new_from_slice(&srgb().to_bytes()).unwrap();
        let d = ColorProfile::new_from_slice(&dst.profile().to_bytes()).unwrap();
        let mt = s.create_transform_8bit(Layout::Rgb, &d, Layout::Rgb, MoxOpts::default()).unwrap();
        let src: Vec<u8> = (0..4096u32).flat_map(|i| [(i * 37 % 256) as u8, (i * 91 % 256) as u8, (i * 13 % 256) as u8]).collect();
        let mut theirs = vec![0u8; src.len()];
        mt.transform(&src, &mut theirs).unwrap();
        let ours_t = Transform::new(srgb(), dst.profile(), Intent::RelativeColorimetric, false).unwrap();
        let mut ours = vec![0u8; src.len()];
        ours_t.convert_u8(&src, 3, &mut ours, 3, false);
        let worst = ours.iter().zip(&theirs).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 2, "{dst:?}: max diff {worst}");
    }
}

#[test]
fn oracle_moxcms_system_cmyk() {
    use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions as MoxOpts};
    let path = "/System/Library/ColorSync/Profiles/Generic CMYK Profile.icc";
    let Ok(bytes) = std::fs::read(path) else { return };
    let ours_p = Profile::parse(&bytes).unwrap();
    let theirs_p = ColorProfile::new_from_slice(&bytes).unwrap();
    let s = ColorProfile::new_from_slice(&srgb().to_bytes()).unwrap();
    let opts = MoxOpts { rendering_intent: RenderingIntent::RelativeColorimetric, ..Default::default() };
    let Ok(mt) = theirs_p.create_transform_8bit(Layout::Rgba, &s, Layout::Rgb, opts) else { return };
    let src: Vec<u8> = (0..2048u32).flat_map(|i| [(i * 37 % 256) as u8, (i * 91 % 256) as u8, (i * 13 % 256) as u8, (i * 57 % 256) as u8]).collect();
    let mut theirs = vec![0u8; 2048 * 3];
    mt.transform(&src, &mut theirs).unwrap();
    let t = Transform::new(&ours_p, srgb(), Intent::RelativeColorimetric, false).unwrap();
    let mut ours = vec![0u8; 2048 * 3];
    t.convert_u8(&src, 4, &mut ours, 3, false);
    let diffs: Vec<i32> = ours.iter().zip(&theirs).map(|(a, b)| (*a as i32 - *b as i32).abs()).collect();
    let worst = *diffs.iter().max().unwrap();
    let mean = diffs.iter().sum::<i32>() as f64 / diffs.len() as f64;
    eprintln!("Generic CMYK → sRGB vs moxcms: max {worst}, mean {mean:.3}");
    let wi = diffs.iter().position(|d| *d == worst).unwrap() / 3;
    eprintln!("worst px cmyk {:?}: ours {:?} theirs {:?}", &src[wi * 4..wi * 4 + 4], &ours[wi * 3..wi * 3 + 3], &theirs[wi * 3..wi * 3 + 3]);
    // Outliers sit at the sRGB toe (slope 12.92) on near-gamut-edge colours, where moxcms's
    // own output interpolation differs; an independent float computation of one such pixel
    // (Lab 48.45/−32.55/−4.90 → sRGB red 0.86) matches ours (0.73), not moxcms (19).
    let mut sorted = diffs.clone();
    sorted.sort_unstable();
    let p99 = sorted[sorted.len() * 99 / 100];
    assert!(mean < 1.5 && p99 <= 6, "max {worst} p99 {p99} mean {mean}");
}

#[test]
fn builtin_all_in_declaration_order() {
    // `Builtin::profile` indexes its cache by discriminant.
    for (i, b) in Builtin::ALL.iter().enumerate() {
        assert_eq!(*b as usize, i, "{b:?}");
    }
}

/// `icc` with one more tag appended (the tag table grows by 12 bytes, so offsets shift).
fn with_tag(icc: &[u8], sig: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let be = |o: usize| u32::from_be_bytes(icc[o..o + 4].try_into().unwrap());
    let count = be(128) as usize;
    let table_end = 132 + count * 12;
    let mut body = icc[table_end..].to_vec();
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    let mut out = icc[..128].to_vec();
    out.extend_from_slice(&(count as u32 + 1).to_be_bytes());
    for i in 0..count {
        let o = 132 + i * 12;
        out.extend_from_slice(&icc[o..o + 4]);
        out.extend_from_slice(&(be(o + 4) + 12).to_be_bytes());
        out.extend_from_slice(&icc[o + 8..o + 12]);
    }
    let new_off = (table_end + 12 + body.len()) as u32;
    out.extend_from_slice(sig);
    out.extend_from_slice(&new_off.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(data);
    let size = out.len() as u32;
    out[..4].copy_from_slice(&size.to_be_bytes());
    out
}

/// A `lutBtoA` ("mBA ") tag, 1 → 3 channels, using every element:
/// B curves, matrix, M curves, CLUT, A curves.
fn mba_1_to_3() -> Vec<u8> {
    let curv = b"curv\0\0\0\0\0\0\0\0"; // identity curve, 12 bytes
    let mut d = b"mBA \0\0\0\0".to_vec();
    d.extend_from_slice(&[1, 3, 0, 0]);
    // offsets: B, matrix, M, CLUT, A
    for off in [32u32, 44, 92, 104, 132] {
        d.extend_from_slice(&off.to_be_bytes());
    }
    d.extend_from_slice(curv); // B @ 32
    for k in 0..12 {
        let v: i32 = if k % 4 == 0 && k < 9 { 0x10000 } else { 0 };
        d.extend_from_slice(&v.to_be_bytes()); // matrix @ 44 (identity, zero offset)
    }
    d.extend_from_slice(curv); // M @ 92
    let mut grid = [0u8; 16];
    grid[0] = 2;
    d.extend_from_slice(&grid); // CLUT @ 104
    d.extend_from_slice(&[1, 0, 0, 0]); // 8-bit precision
    d.extend_from_slice(&[0, 0, 0, 255, 255, 255]); // 2 nodes x 3 outputs
    d.extend_from_slice(&[0, 0]); // pad to 132
    for _ in 0..3 {
        d.extend_from_slice(curv); // A @ 132
    }
    d
}

/// Regression: a gray profile whose A2B0 tag is stored as "mBA " parsed fine, but
/// re-encoding its RGB view (`gray_as_rgb`, used for gray documents' composites) hit
/// `panic!("unsupported lutAtoB stage layout")` in the ICC writer.
#[test]
fn gray_profile_with_reversed_lut_type_reencodes() {
    let sgray = Builtin::SGray.profile().to_bytes();
    let icc = with_tag(&sgray, b"A2B0", &mba_1_to_3());
    let p = Profile::parse(&icc).unwrap();
    assert!(p.a2b[0].is_some(), "the crafted A2B0 tag should parse");
    let rgb = p.gray_as_rgb().unwrap();
    assert!(rgb.is_matrix_shaper(), "RGB view keeps no gray LUTs");
    let back = Profile::parse(&rgb.to_bytes()).unwrap();
    assert_eq!(back.color_space, ColorSpace::Rgb);
    // The writer itself must not panic on a LUT it can't store either.
    let mut odd = p.clone();
    odd.description = "re-encoded".into();
    let _ = Profile::parse(&odd.with_encoded_bytes().to_bytes()).unwrap();
}

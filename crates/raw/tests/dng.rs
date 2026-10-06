//! Synthetic DNG files: every storage layout decodes to the exact sensor
//! samples, and development reproduces known colours.

use photocraft_raw::testgen::{DngSpec, DngStorage, mosaic, scene};
use photocraft_raw::*;

fn sensor(bytes: &[u8]) -> Sensor {
    decode(bytes, &Limits::default()).unwrap()
}

/// Deterministic sensor-like samples below `2^bits`.
fn noise(n: usize, bits: u16) -> Vec<u16> {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let max = (1u32 << bits) - 1;
    (0..n)
        .map(|i| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((i as u32 * 13 + (s >> 40) as u32 % 200) % (max + 1)) as u16
        })
        .collect()
}

#[test]
fn identifies_dng() {
    let b = DngSpec::cfa(4, 4, vec![0; 16]).build();
    assert_eq!(identify(&b), Some(RawFormat::Dng));
    assert!(is_raw(&b));
    assert!(!is_raw(b"II*\0\x08\0\0\0\0\0"));
    assert!(!is_raw(b"\x89PNG\r\n\x1a\n"));
}

#[test]
fn storage_layouts_decode_exactly() {
    let (w, h) = (37, 23); // odd sizes exercise partial strips and edge tiles
    let cases: Vec<(&str, u16, DngStorage, bool)> = vec![
        ("16-bit strips", 16, DngStorage::Strips { rows: 5 }, false),
        ("16-bit big-endian", 16, DngStorage::Strips { rows: 23 }, true),
        ("12-bit packed", 12, DngStorage::Strips { rows: 4 }, false),
        ("10-bit packed big-endian", 10, DngStorage::Strips { rows: 7 }, true),
        ("8-bit", 8, DngStorage::Strips { rows: 9 }, false),
        ("LJ92 tiles", 14, DngStorage::Lj92Tiles { width: 16, height: 8 }, false),
        ("LJ92 tiles big-endian", 16, DngStorage::Lj92Tiles { width: 32, height: 32 }, true),
        ("LJ92 strips", 12, DngStorage::Lj92Strips { rows: 6 }, false),
    ];
    for (name, bits, storage, be) in cases {
        let data = noise(w * h, bits);
        let mut spec = DngSpec::cfa(w, h, data.clone());
        spec.bits = bits;
        spec.storage = storage;
        spec.big_endian = be;
        spec.white = (1 << bits) - 1;
        let s = sensor(&spec.build());
        assert_eq!((s.width, s.height, s.samples), (w, h, 1), "{name}");
        assert_eq!(s.data, data, "{name}");
        assert_eq!(s.format, RawFormat::Dng);
    }
}

#[test]
fn linear_raw_decodes_and_develops() {
    let (w, h) = (20, 10);
    let data = noise(w * h * 3, 16);
    let mut spec = DngSpec::cfa(w, h, data.clone());
    spec.samples = 3;
    spec.storage = DngStorage::Lj92Tiles { width: 16, height: 16 };
    let s = sensor(&spec.build());
    assert_eq!((s.samples, s.cfa.is_none()), (3, true));
    assert_eq!(s.data, data);
    let d = develop_sensor(&s, &DevelopOptions::default()).unwrap();
    assert_eq!((d.width, d.height, d.rgb.len()), (20, 10, 600));
    assert!(d.info.cfa.is_none());
}

#[test]
fn metadata_is_read() {
    let (w, h) = (40, 30);
    let mut spec = DngSpec::cfa(w, h, vec![1000; w * h]);
    spec.bits = 14;
    spec.white = 15000;
    spec.black = vec![100, 101, 102, 103];
    spec.black_repeat = (2, 2);
    spec.active_area = Some([2, 4, 28, 36]); // top, left, bottom, right
    spec.default_crop = Some(([2, 1], [20, 16]));
    spec.cfa = [1, 0, 2, 1]; // GRBG at the active-area origin
    spec.as_shot_neutral = Some([0.5, 1.0, 0.8]);
    spec.baseline_exposure = Some(0.5);
    spec.orientation = 6;
    spec.linearization = Some((0..4096u32).map(|v| (v * 3).min(65535) as u16).collect());
    let s = sensor(&spec.build());
    assert_eq!(s.white, [15000.0; 3]);
    assert_eq!(s.black.values, vec![100.0, 101.0, 102.0, 103.0]);
    assert_eq!(s.active, Rect::new(4, 2, 32, 26));
    assert_eq!(s.crop, Rect::new(6, 3, 20, 16));
    // Pattern anchored at the active area origin (4, 2): GRBG there.
    assert_eq!(s.cfa.as_ref().unwrap().phase(4, 2), [1, 0, 2, 1]);
    assert_eq!(s.color.as_shot_neutral, Some([0.5, 1.0, 0.8]));
    assert!((s.baseline_exposure - 0.5).abs() < 1e-6);
    assert_eq!(s.orientation, 6);
    assert_eq!(s.linearization.as_ref().map(Vec::len), Some(4096));
    let d = develop_sensor(&s, &DevelopOptions::default()).unwrap();
    // Orientation 6 swaps the cropped 20×16 to 16×20.
    assert_eq!((d.width, d.height), (16, 20));
    assert_eq!(d.info.wb_multipliers, [2.0, 1.0, 1.25]);
    let d = develop_sensor(&s, &DevelopOptions { orient: false, ..Default::default() }).unwrap();
    assert_eq!((d.width, d.height), (20, 16));
}

/// Linear sRGB (D65) → XYZ, from the sRGB primaries (IEC 61966-2-1).
const SRGB_TO_XYZ: [[f64; 3]; 3] = [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]];

fn invert(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
        [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
        [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ]
}

fn ap(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// The DNG model computed independently: camera XYZ of `camera` under an
/// illuminant whose camera neutral is `neutral`, Bradford-adapted from that
/// white (normalized to Y = 1) to D50, then XYZ D50 → linear ProPhoto.
fn reference_prophoto(camera: [f64; 3], neutral: [f64; 3]) -> [f64; 3] {
    const BRADFORD: [[f64; 3]; 3] = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
    const XYZ_D50_TO_PROPHOTO: [[f64; 3]; 3] = [[1.3459433, -0.2556075, -0.0511118], [-0.5445989, 1.5081673, 0.0205351], [0.0, 0.0, 1.2118128]];
    let white = ap(&SRGB_TO_XYZ, neutral);
    let white = white.map(|v| v / white[1]);
    let xyz = ap(&SRGB_TO_XYZ, camera).map(|v| v / ap(&SRGB_TO_XYZ, neutral)[1]);
    let (s, d) = (ap(&BRADFORD, white), ap(&BRADFORD, D50));
    let cone = ap(&BRADFORD, xyz);
    let adapted = ap(&invert(BRADFORD), [cone[0] * d[0] / s[0], cone[1] * d[1] / s[1], cone[2] * d[2] / s[2]]);
    ap(&XYZ_D50_TO_PROPHOTO, adapted)
}

#[test]
fn colour_matrix_development_matches_reference() {
    // A camera whose native RGB is linear sRGB: ColorMatrix = XYZ → sRGB at D65.
    // The scene is captured with per-channel gains (the "white balance" the camera
    // must undo), recorded as AsShotNeutral.
    let (w, h) = (48, 32);
    let gains = [0.5f32, 1.0, 0.7];
    let patches: [[f32; 3]; 4] = [[0.18, 0.18, 0.18], [0.40, 0.10, 0.05], [0.05, 0.30, 0.10], [0.10, 0.12, 0.45]];
    let rgb: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let p = patches[((i % w) / (w / 2)) + 2 * ((i / w) / (h / 2))];
            [p[0] * gains[0], p[1] * gains[1], p[2] * gains[2]]
        })
        .collect();
    let mut spec = DngSpec::cfa(w, h, mosaic(&rgb, w, [0, 1, 1, 2], 256, 16383));
    spec.bits = 14;
    spec.black = vec![256];
    spec.white = 16383;
    let cm = invert(SRGB_TO_XYZ);
    spec.color_matrix1 = Some((21, [cm[0][0], cm[0][1], cm[0][2], cm[1][0], cm[1][1], cm[1][2], cm[2][0], cm[2][1], cm[2][2]]));
    spec.as_shot_neutral = Some(gains.map(f64::from));
    for storage in [DngStorage::Strips { rows: 32 }, DngStorage::Lj92Tiles { width: 16, height: 16 }] {
        spec.storage = storage;
        for method in [Demosaic::Bilinear, Demosaic::Mhc, Demosaic::Ahd] {
            let d = develop(&spec.build(), &DevelopOptions { demosaic: method, ..Default::default() }).unwrap();
            for (pi, p) in patches.iter().enumerate() {
                // Centre of each patch.
                let (cx, cy) = ((pi % 2) * (w / 2) + w / 4, (pi / 2) * (h / 2) + h / 4);
                let o = &d.rgb[(cy * w + cx) * 3..(cy * w + cx) * 3 + 3];
                let g = gains.map(f64::from);
                let want = reference_prophoto([0, 1, 2].map(|c| f64::from(p[c]) * g[c]), g);
                for c in 0..3 {
                    let got = (f64::from(o[c]) / 65535.0).powf(1.8);
                    assert!((got - want[c]).abs() < 0.006, "{storage:?} {method:?} patch {pi} c {c}: got {got:.4} want {:.4}", want[c]);
                }
            }
        }
    }
}

#[test]
fn forward_matrix_path_maps_neutral_to_white() {
    let (w, h) = (16, 16);
    let rgb = vec![[0.3f32, 0.6, 0.45]; w * h]; // a grey under a green-ish light
    let mut spec = DngSpec::cfa(w, h, mosaic(&rgb, w, [0, 1, 1, 2], 0, 65535));
    let cm = invert(SRGB_TO_XYZ);
    let flat = |m: [[f64; 3]; 3]| [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]];
    spec.color_matrix1 = Some((17, flat(cm)));
    spec.color_matrix2 = Some((21, flat(cm)));
    // ForwardMatrix: balanced camera → XYZ D50 (sRGB adapted to D50; white → D50).
    let fm = [[0.4360747, 0.3850649, 0.1430804], [0.2225045, 0.7168786, 0.0606169], [0.0139322, 0.0971045, 0.7141733]];
    spec.forward_matrix1 = Some(flat(fm));
    spec.forward_matrix2 = Some(flat(fm));
    spec.as_shot_neutral = Some([0.5, 1.0, 0.75]);
    let d = develop(&spec.build(), &DevelopOptions::default()).unwrap();
    let o = &d.rgb[(8 * w + 8) * 3..(8 * w + 8) * 3 + 3];
    // Balanced grey 0.6 → ProPhoto (0.6, 0.6, 0.6).
    for (c, v) in o.iter().enumerate() {
        let got = (f64::from(*v) / 65535.0).powf(1.8);
        assert!((got - 0.6).abs() < 0.004, "c {c}: {got}");
    }
}

#[test]
fn highlights_clip_to_white_and_exposure_applies() {
    let (w, h) = (8, 8);
    let mut spec = DngSpec::cfa(w, h, vec![65535; w * h]);
    spec.as_shot_neutral = Some([0.5, 1.0, 0.6]);
    let d = develop(&spec.build(), &DevelopOptions::default()).unwrap();
    assert!(d.rgb.iter().all(|&v| v >= 65500), "clipped sensor must render white");
    let mut spec = DngSpec::cfa(w, h, vec![16384; w * h]); // 0.25 linear grey
    spec.as_shot_neutral = Some([1.0, 1.0, 1.0]);
    let b = spec.build();
    let base = develop(&b, &DevelopOptions::default()).unwrap().rgb[27];
    let up = develop(&b, &DevelopOptions { exposure: 1.0, ..Default::default() }).unwrap().rgb[27];
    let lin = |v: u16| (f64::from(v) / 65535.0).powf(1.8);
    assert!((lin(up) / lin(base) - 2.0).abs() < 0.02, "{} vs {}", lin(up), lin(base));
}

#[test]
fn gain_map_opcode_is_applied() {
    // Uniform 0.25 grey; a gain map that doubles everything over the whole image.
    let (w, h) = (16, 12);
    let mut spec = DngSpec::cfa(w, h, vec![16384; w * h]);
    spec.as_shot_neutral = Some([1.0; 3]);
    let lin = |v: u16| (f64::from(v) / 65535.0).powf(1.8);
    let base = develop(&spec.build(), &DevelopOptions::default()).unwrap().rgb[(6 * w + 8) * 3 + 1];
    spec.opcode_list2 = Some(photocraft_raw::testgen::gain_map_opcode_list([0, 0, h as u32, w as u32], 1, [[2.0, 2.0], [2.0, 2.0]]));
    let d = develop(&spec.build(), &DevelopOptions::default()).unwrap();
    assert!(!d.warnings.iter().any(|w| w.contains("Opcode")), "{:?}", d.warnings);
    let gained = d.rgb[(6 * w + 8) * 3 + 1];
    assert!((lin(gained) / lin(base) - 2.0).abs() < 0.02, "{} vs {}", lin(gained), lin(base));
}

#[test]
fn white_balance_options() {
    let (w, h) = (16, 16);
    let rgb = vec![[0.2f32, 0.4, 0.3]; w * h];
    let spec = DngSpec::cfa(w, h, mosaic(&rgb, w, [0, 1, 1, 2], 0, 65535));
    let b = spec.build();
    // No AsShotNeutral: grey world → neutral output.
    let d = develop(&b, &DevelopOptions::default()).unwrap();
    assert!(d.warnings.iter().any(|w| w.contains("grey world")));
    let o = &d.rgb[(8 * w + 8) * 3..(8 * w + 8) * 3 + 3];
    assert!((i32::from(o[0]) - i32::from(o[1])).abs() < 300 && (i32::from(o[2]) - i32::from(o[1])).abs() < 300, "{o:?}");
    let d = develop(&b, &DevelopOptions { white_balance: WhiteBalance::Multipliers([2.0, 1.0, 4.0 / 3.0]), ..Default::default() }).unwrap();
    assert_eq!(d.info.wb_multipliers.map(|v| (v * 1000.0).round()), [2000.0, 1000.0, 1333.0]);
    assert!(develop(&b, &DevelopOptions { white_balance: WhiteBalance::Multipliers([0.0, 1.0, 1.0]), ..Default::default() }).is_err());
}

#[test]
fn smooth_scene_develops_with_every_method() {
    let (w, h) = (64, 48);
    let rgb = scene(w, h);
    let spec = DngSpec::cfa(w, h, mosaic(&rgb, w, [2, 1, 1, 0], 0, 65535));
    let mut spec = spec;
    spec.cfa = [2, 1, 1, 0];
    spec.as_shot_neutral = Some([1.0; 3]);
    let b = spec.build();
    let outs: Vec<Vec<u16>> = [Demosaic::Bilinear, Demosaic::Mhc, Demosaic::Ahd]
        .iter()
        .map(|&m| develop(&b, &DevelopOptions { demosaic: m, ..Default::default() }).unwrap().rgb)
        .collect();
    // Interior pixels agree closely across methods on a smooth scene.
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            for c in 0..3 {
                let i = (y * w + x) * 3 + c;
                let (a, b, d) = (i32::from(outs[0][i]), i32::from(outs[1][i]), i32::from(outs[2][i]));
                assert!((a - b).abs() < 800 && (a - d).abs() < 800, "({x},{y}) c{c}: {a} {b} {d}");
            }
        }
    }
}

#[test]
fn unsupported_variants_say_so() {
    let mut spec = DngSpec::cfa(8, 8, vec![0; 64]);
    spec.cfa = [0, 1, 2, 1]; // not Bayer
    assert!(matches!(decode(&spec.build(), &Limits::default()), Err(RawError::Unsupported(_))));
}

#[test]
fn limits_are_enforced_before_allocating() {
    let spec = DngSpec::cfa(64, 64, vec![0; 64 * 64]);
    let b = spec.build();
    let tiny = Limits { max_width: 32, ..Limits::default() };
    assert!(matches!(decode(&b, &tiny), Err(RawError::LimitExceeded(_))));
    let tiny = Limits { max_alloc: 1000, ..Limits::default() };
    assert!(matches!(decode(&b, &tiny), Err(RawError::LimitExceeded(_))));
}

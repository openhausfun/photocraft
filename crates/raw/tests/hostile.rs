//! Truncated and corrupted raw files must fail cleanly, never panic, and
//! never allocate beyond the limits.

use photocraft_raw::testgen::{Cr2Spec, DngSpec, DngStorage, mosaic, orf, rw2, scene, sony_craw, tiff_ep};
use photocraft_raw::*;

fn samples() -> Vec<Vec<u8>> {
    let (w, h) = (24, 12);
    let cfa = mosaic(&scene(w, h), w, [0, 1, 1, 2], 64, 4000);
    let mut out = Vec::new();
    let mut d = DngSpec::cfa(w, h, cfa.clone());
    d.bits = 12;
    d.white = 4000;
    d.black = vec![64];
    d.as_shot_neutral = Some([0.5, 1.0, 0.7]);
    d.color_matrix1 = Some((21, [3.2, -1.5, -0.5, -1.0, 1.9, 0.04, 0.06, -0.2, 1.06]));
    out.push(d.build());
    d.storage = DngStorage::Lj92Tiles { width: 16, height: 8 };
    d.active_area = Some([1, 2, 11, 22]);
    d.default_crop = Some(([1, 1], [16, 8]));
    out.push(d.build());
    d.storage = DngStorage::Strips { rows: 5 };
    d.big_endian = true;
    out.push(d.build());
    let mut data = vec![600u16; 40 * 10];
    for (i, v) in data.iter_mut().enumerate() {
        *v = 600 + (i as u16 * 7) % 3000;
    }
    out.push(
        Cr2Spec {
            width: 40,
            height: 10,
            data,
            precision: 14,
            components: 2,
            slices: vec![16, 16, 8],
            borders: Some([8, 2, 39, 9]),
            wb_rggb: Some([2000, 1024, 1024, 1500]),
            orientation: 8,
            model_id: None,
        }
        .build(),
    );
    out.push(tiff_ep("NIKON", w, h, &cfa, [0, 1, 1, 2], 12, vec![]));
    let codes = mosaic(&scene(64, 6), 64, [0, 1, 1, 2], 256, 1900);
    out.push(sony_craw(64, 6, &codes, [8000, 10400, 12900, 14100]));
    out.push(rw2(30, 8, &mosaic(&scene(30, 8), 30, [0, 1, 1, 2], 128, 4095), 12));
    out.push(orf(w, h, &cfa));
    out
}

fn exercise(b: &[u8]) {
    let _ = identify(b);
    let _ = embedded_preview(b);
    let opts = DevelopOptions { limits: Limits { max_alloc: 64 << 20, ..Limits::default() }, ..Default::default() };
    if let Ok(s) = decode(b, &opts.limits) {
        for m in [Demosaic::Bilinear, Demosaic::Ahd] {
            let _ = develop_sensor(&s, &DevelopOptions { demosaic: m, ..opts.clone() });
        }
    }
}

#[test]
fn originals_develop() {
    for (i, b) in samples().iter().enumerate() {
        let d = develop(b, &DevelopOptions::default());
        assert!(d.is_ok(), "sample {i}: {:?}", d.err());
    }
}

#[test]
fn every_truncation_fails_cleanly() {
    for b in samples() {
        for n in (0..b.len()).step_by(3) {
            exercise(&b[..n]);
        }
    }
}

#[test]
fn random_corruption_never_panics() {
    let mut s = 0xDEAD_BEEF_u64;
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s >> 33
    };
    for b in samples() {
        for _ in 0..400 {
            let mut c = b.clone();
            for _ in 0..1 + next() % 6 {
                let at = next() as usize % c.len();
                c[at] = match next() % 4 {
                    0 => 0xFF,
                    1 => 0,
                    _ => next() as u8,
                };
            }
            exercise(&c);
        }
    }
}

#[test]
fn absurd_dimensions_are_rejected_without_allocating() {
    let mut d = DngSpec::cfa(4, 4, vec![0; 16]);
    d.storage = DngStorage::Strips { rows: 4 };
    let mut b = d.build();
    // Patch the raw IFD's ImageWidth / ImageLength (LONG, inline) to 200000 × 200000.
    let t = 256u16.to_le_bytes();
    let mut patched = 0;
    for i in 0..b.len().saturating_sub(12) {
        if b[i..i + 2] == t && b[i + 2..i + 4] == 4u16.to_le_bytes() && b[i + 8..i + 12] == 4u32.to_le_bytes() {
            b[i + 8..i + 12].copy_from_slice(&200_000u32.to_le_bytes());
            b[i + 20..i + 24].copy_from_slice(&200_000u32.to_le_bytes());
            patched += 1;
        }
    }
    assert!(patched > 0);
    assert!(matches!(decode(&b, &Limits::default()), Err(RawError::LimitExceeded(_))));
}

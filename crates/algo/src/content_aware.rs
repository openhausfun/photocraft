//! Edit › Content-Aware Fill: patch-based completion ([`crate::inpaint::complete`]) with a
//! restricted sampling area, geometric adaptation and colour adaptation.
//!
//! * **Sampling area**: pixels outside the allowed `source` mask may not be copied from. The
//!   completion only copies fully-known patches, so excluded pixels are handed to it as
//!   unknown and restored afterwards; only the real hole keeps the synthesised values.
//! * **Rotation / scale / mirror adaptation**: the sampling window is extended with rotated,
//!   rescaled and mirrored copies of itself, laid out side by side (separated by unknown gaps
//!   wider than a patch), so the patch search can pick transformed source patches, as in
//!   the generalised PatchMatch of C. Barnes, E. Shechtman, D. B. Goldman, A. Finkelstein,
//!   *The Generalized PatchMatch Correspondence Algorithm*, ECCV 2010 (which searches over
//!   rotations and scales; we enumerate a small set instead).
//! * **Colour adaptation**: the low frequencies of the fill are pulled towards the smooth
//!   membrane interpolation of the hole boundary (P. Pérez, M. Gangnet, A. Blake, *Poisson
//!   Image Editing*, SIGGRAPH 2003 uses the same membrane for seamless cloning), which corrects
//!   gradual brightness and colour changes across the hole while keeping the copied texture.
//!
//! Buffers are interleaved `w × h × ch` normalised floats (any model/depth); deterministic.

use crate::inpaint::{CompleteParams, complete_with};
use crate::poisson::membrane_fill_with;

/// Options for [`fill`].
#[derive(Clone, Debug, PartialEq)]
pub struct FillOptions {
    /// Colour adaptation strength `0..=1` (Photoshop: none 0, default ≈0.35, high, very high).
    pub color_adaptation: f32,
    /// Extra source rotations in degrees (Photoshop's rotation adaptation levels).
    pub rotations: Vec<f32>,
    /// Add downscaled and upscaled source copies.
    pub scale: bool,
    /// Add a horizontally mirrored source copy.
    pub mirror: bool,
    pub seed: u64,
}

impl Default for FillOptions {
    fn default() -> Self {
        Self { color_adaptation: 0.35, rotations: Vec::new(), scale: false, mirror: false, seed: 1 }
    }
}

/// Rotation set for Photoshop's adaptation levels: none, low, medium, high, full.
pub fn rotation_level(level: &str) -> Vec<f32> {
    match level {
        "low" => vec![-10.0, 10.0],
        "medium" => vec![-20.0, 20.0],
        "high" => vec![-35.0, 35.0],
        "full" => vec![90.0, 180.0, 270.0],
        _ => Vec::new(),
    }
}

/// Colour adaptation strength for Photoshop's levels: none, default, high, veryHigh.
pub fn color_level(level: &str) -> f32 {
    match level {
        "none" => 0.0,
        "high" => 0.65,
        "veryHigh" => 0.9,
        _ => 0.35,
    }
}

/// One source variant: pixels and which of them are usable.
struct Variant {
    w: usize,
    h: usize,
    img: Vec<f32>,
    ok: Vec<bool>,
}

/// A source buffer and which of its pixels may be sampled.
#[derive(Clone, Copy)]
struct Src<'a> {
    w: usize,
    h: usize,
    ch: usize,
    img: &'a [f32],
    ok: &'a [bool],
}

/// Sample the source bilinearly at (x, y); false when any tap is unusable.
fn sample(src: Src, x: f32, y: f32, out: &mut [f32]) -> bool {
    let Src { w, h, ch, img, ok } = src;
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return false;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    for (xx, yy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        if !ok[yy * w + xx] {
            return false;
        }
    }
    for (c, o) in out.iter_mut().enumerate().take(ch) {
        let p = |xx: usize, yy: usize| img[(yy * w + xx) * ch + c];
        let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
        let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
        *o = top + (bot - top) * ty;
    }
    true
}

/// Transformed copy of the source: rotation (degrees) and scale about the centre, optional mirror.
fn variant(src: Src, deg: f32, scale: f32, mirror: bool) -> Variant {
    let Src { w, h, ch, .. } = src;
    let (s, c) = (-deg.to_radians()).sin_cos();
    // Output size: the rotated, scaled bounding box.
    let (fw, fh) = (w as f32 * scale, h as f32 * scale);
    let ow = ((fw * c.abs() + fh * s.abs()).round() as usize).max(1);
    let oh = ((fw * s.abs() + fh * c.abs()).round() as usize).max(1);
    let (icx, icy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let (ocx, ocy) = ((ow as f32 - 1.0) / 2.0, (oh as f32 - 1.0) / 2.0);
    let mut out = Variant { w: ow, h: oh, img: vec![0.0; ow * oh * ch], ok: vec![false; ow * oh] };
    let mut px = vec![0.0f32; ch];
    for y in 0..oh {
        for x in 0..ow {
            let (dx, dy) = ((x as f32 - ocx) / scale, (y as f32 - ocy) / scale);
            let mut sx = icx + dx * c - dy * s;
            let sy = icy + dx * s + dy * c;
            if mirror {
                sx = (w as f32 - 1.0) - sx;
            }
            if sample(src, sx, sy, &mut px) {
                let i = y * ow + x;
                out.ok[i] = true;
                out.img[i * ch..(i + 1) * ch].copy_from_slice(&px);
            }
        }
    }
    out
}

/// Separable box blur of radius `r` (edge-clamped).
fn box_blur(w: usize, h: usize, ch: usize, v: &[f32], r: usize) -> Vec<f32> {
    if r == 0 {
        return v.to_vec();
    }
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut dst = vec![0.0f32; src.len()];
        let (n, m) = if horizontal { (h, w) } else { (w, h) };
        let idx = |line: usize, k: usize| if horizontal { line * w + k } else { k * w + line };
        let norm = 1.0 / (2 * r + 1) as f32;
        for line in 0..n {
            for c in 0..ch {
                let at = |k: i64| src[idx(line, k.clamp(0, m as i64 - 1) as usize) * ch + c];
                let mut acc: f32 = (-(r as i64)..=r as i64).map(at).sum();
                for k in 0..m {
                    dst[idx(line, k) * ch + c] = acc * norm;
                    acc += at(k as i64 + r as i64 + 1) - at(k as i64 - r as i64);
                }
            }
        }
        dst
    };
    pass(&pass(v, true), false)
}

/// Fill `hole` in `img`, copying only from pixels where `source` is true (and not in the hole).
/// Returns the full buffer (unchanged outside the hole). Falls back to the membrane fill when
/// nothing can be sampled.
pub fn fill(w: usize, h: usize, ch: usize, img: &[f32], hole: &[bool], source: &[bool], opts: &FillOptions) -> Vec<f32> {
    // Never cancelled; the fallback (the input unchanged) is unreachable.
    fill_with(w, h, ch, img, hole, source, opts, &photocraft_raster::Interrupt::NONE).unwrap_or_else(|_| img.to_vec())
}

/// [`fill`] that can be cancelled (see [`crate::inpaint::complete_with`]) and reports progress.
#[allow(clippy::too_many_arguments)]
pub fn fill_with(
    w: usize,
    h: usize,
    ch: usize,
    img: &[f32],
    hole: &[bool],
    source: &[bool],
    opts: &FillOptions,
    ctl: &photocraft_raster::Interrupt,
) -> Result<Vec<f32>, photocraft_raster::Cancelled> {
    assert_eq!(img.len(), w * h * ch);
    assert_eq!(hole.len(), w * h);
    assert_eq!(source.len(), w * h);
    if !hole.iter().any(|h| *h) {
        return Ok(img.to_vec());
    }
    let params = CompleteParams { seed: opts.seed, ..Default::default() };
    let gap = 2 * params.patch_radius + 2;
    let ok: Vec<bool> = hole.iter().zip(source).map(|(h, s)| !h && *s).collect();
    let src = Src { w, h, ch, img, ok: &ok };
    // Variants: the identity (which holds the hole) first, then the adaptations.
    let mut extra: Vec<Variant> = Vec::new();
    if opts.mirror {
        extra.push(variant(src, 0.0, 1.0, true));
    }
    for &deg in &opts.rotations {
        extra.push(variant(src, deg, 1.0, false));
    }
    if opts.scale {
        extra.push(variant(src, 0.0, 0.8, false));
        extra.push(variant(src, 0.0, 1.25, false));
    }
    // Mosaic: identity at the left, variants in a column to its right.
    let col_w = extra.iter().map(|v| v.w).max().unwrap_or(0);
    let col_h: usize = extra.iter().map(|v| v.h + gap).sum();
    let mw = if extra.is_empty() { w } else { w + gap + col_w };
    let mh = h.max(col_h);
    let mut mimg = vec![0.0f32; mw * mh * ch];
    let mut unknown = vec![true; mw * mh];
    for y in 0..h {
        for x in 0..w {
            let (s, d) = (y * w + x, y * mw + x);
            mimg[d * ch..(d + 1) * ch].copy_from_slice(&img[s * ch..(s + 1) * ch]);
            unknown[d] = !ok[s];
        }
    }
    let mut oy = 0;
    for v in &extra {
        for y in 0..v.h {
            for x in 0..v.w {
                let (s, d) = (y * v.w + x, (oy + y) * mw + w + gap + x);
                mimg[d * ch..(d + 1) * ch].copy_from_slice(&v.img[s * ch..(s + 1) * ch]);
                unknown[d] = !v.ok[s];
            }
        }
        oy += v.h + gap;
    }
    ctl.check()?;
    // The completion is nearly all the work: it reports 0–90 %.
    let cancel = || ctl.cancelled();
    let progress = |f: f32| ctl.progress(f * 0.9);
    let filled = complete_with(mw, mh, ch, &mimg, &unknown, &params, &photocraft_raster::Interrupt::new(&cancel, &progress))?;
    let mut out = img.to_vec();
    match filled {
        Some(f) => {
            for y in 0..h {
                for x in 0..w {
                    if hole[y * w + x] {
                        let (s, d) = ((y * mw + x) * ch, (y * w + x) * ch);
                        out[d..d + ch].copy_from_slice(&f[s..s + ch]);
                    }
                }
            }
        }
        None => out = membrane_fill_with(w, h, ch, img, hole, ctl)?,
    }
    ctl.check()?;
    if opts.color_adaptation > 0.0 {
        adapt_colors(w, h, ch, img, hole, &mut out, opts.color_adaptation, ctl)?;
    }
    ctl.progress(1.0);
    Ok(out)
}

/// Pull the fill's low frequencies towards the membrane interpolation of the hole boundary.
#[allow(clippy::too_many_arguments)]
fn adapt_colors(
    w: usize,
    h: usize,
    ch: usize,
    img: &[f32],
    hole: &[bool],
    out: &mut [f32],
    k: f32,
    ctl: &photocraft_raster::Interrupt,
) -> Result<(), photocraft_raster::Cancelled> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    let r = ((x1 - x0).max(y1 - y0) / 6).clamp(1, 32);
    let membrane = membrane_fill_with(w, h, ch, img, hole, ctl)?;
    ctl.progress(0.97);
    ctl.check()?;
    let low = box_blur(w, h, ch, out, r);
    for (i, &hl) in hole.iter().enumerate() {
        if hl {
            for c in 0..ch {
                let j = i * ch + c;
                out[j] = (out[j] + k * (membrane[j] - low[j])).clamp(0.0, 1.0);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vertical stripes (period 6) in grey, with a hole in the middle.
    fn stripes(w: usize, h: usize) -> (Vec<f32>, Vec<bool>) {
        let img: Vec<f32> = (0..w * h).map(|i| if (i % w) % 6 < 3 { 0.2 } else { 0.8 }).collect();
        let hole: Vec<bool> = (0..w * h).map(|i| (i % w).abs_diff(w / 2) < 4 && (i / w).abs_diff(h / 2) < 4).collect();
        (img, hole)
    }

    #[test]
    fn cancelled_fill_stops_and_progress_reaches_one() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let (w, h) = (60, 40);
        let (img, hole) = stripes(w, h);
        let source = vec![true; w * h];
        let yes = || true;
        let cancelled = photocraft_raster::Interrupt::cancel_only(&yes);
        assert!(fill_with(w, h, 1, &img, &hole, &source, &FillOptions::default(), &cancelled).is_err());
        let last = AtomicU32::new(0);
        let no = || false;
        let progress = |f: f32| {
            assert!(f >= f32::from_bits(last.load(Ordering::Relaxed)), "progress went backwards");
            last.store(f.to_bits(), Ordering::Relaxed);
        };
        let out = fill_with(w, h, 1, &img, &hole, &source, &FillOptions::default(), &photocraft_raster::Interrupt::new(&no, &progress)).unwrap();
        assert_eq!(f32::from_bits(last.load(Ordering::Relaxed)), 1.0);
        assert_eq!(out, fill(w, h, 1, &img, &hole, &source, &FillOptions::default()), "same result as the plain fill");
    }

    #[test]
    fn fills_only_the_hole_with_known_texture() {
        let (w, h) = (48, 32);
        let (img, hole) = stripes(w, h);
        let src = vec![true; w * h];
        let out = fill(w, h, 1, &img, &hole, &src, &FillOptions { color_adaptation: 0.0, ..Default::default() });
        for i in 0..w * h {
            if !hole[i] {
                assert_eq!(out[i], img[i]);
            }
        }
        // The hole is filled with stripe values, not a flat average.
        let vals: Vec<f32> = (0..w * h).filter(|i| hole[*i]).map(|i| out[i]).collect();
        assert!(vals.iter().any(|v| *v < 0.35) && vals.iter().any(|v| *v > 0.65), "{vals:?}");
    }

    #[test]
    fn sampling_area_restricts_sources() {
        // Left half dark, right half bright; sampling only the right half fills bright.
        let (w, h) = (40, 20);
        let img: Vec<f32> = (0..w * h).map(|i| if i % w < 20 { 0.1 } else { 0.9 }).collect();
        let hole: Vec<bool> = (0..w * h).map(|i| (16..24).contains(&(i % w)) && (6..14).contains(&(i / w))).collect();
        let right: Vec<bool> = (0..w * h).map(|i| i % w >= 24).collect();
        let out = fill(w, h, 1, &img, &hole, &right, &FillOptions { color_adaptation: 0.0, ..Default::default() });
        let mean: f32 = (0..w * h).filter(|i| hole[*i]).map(|i| out[i]).sum::<f32>() / 64.0;
        assert!(mean > 0.8, "mean {mean}");
    }

    #[test]
    fn adaptations_run_and_stay_deterministic() {
        let (w, h) = (32, 24);
        let (img, hole) = stripes(w, h);
        let rgb: Vec<f32> = img.iter().flat_map(|v| [*v, *v * 0.5, 1.0 - *v]).collect();
        let src = vec![true; w * h];
        let opts = FillOptions { color_adaptation: 0.65, rotations: rotation_level("low"), scale: true, mirror: true, seed: 7 };
        let a = fill(w, h, 3, &rgb, &hole, &src, &opts);
        let b = fill(w, h, 3, &rgb, &hole, &src, &opts);
        assert_eq!(a, b);
        assert!(a.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    }

    #[test]
    fn color_adaptation_follows_a_gradient() {
        // A horizontal brightness ramp with a hole: adaptation keeps the fill near the ramp.
        let (w, h) = (40, 20);
        let img: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 / w as f32).collect();
        let hole: Vec<bool> = (0..w * h).map(|i| (14..26).contains(&(i % w)) && (5..15).contains(&(i / w))).collect();
        let src = vec![true; w * h];
        let out = fill(w, h, 1, &img, &hole, &src, &FillOptions { color_adaptation: 0.9, ..Default::default() });
        let err: f32 = (0..w * h).filter(|i| hole[*i]).map(|i| (out[i] - img[i]).abs()).sum::<f32>() / 120.0;
        assert!(err < 0.12, "mean error {err}");
    }

    #[test]
    fn variant_geometry() {
        let img = vec![0.5f32; 10 * 6];
        let ok = vec![true; 60];
        let src = Src { w: 10, h: 6, ch: 1, img: &img, ok: &ok };
        let v = variant(src, 90.0, 1.0, false);
        assert_eq!((v.w, v.h), (6, 10));
        let v = variant(src, 0.0, 0.8, true);
        assert_eq!((v.w, v.h), (8, 5));
        assert!(v.ok.iter().filter(|o| **o).count() > 20);
    }
}

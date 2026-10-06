//! Destructive tone and colour adjustments that need more than a per-pixel formula:
//! Shadows/Highlights, Replace Color, Match Color and HDR Toning (Image › Adjustments).
//!
//! All work on straight (non-premultiplied) RGBA buffers in 0..=1, row-major `w × h`, so callers
//! convert any colour model / bit depth to RGBA first (as `Image › Adjustments` does for the
//! per-pixel adjustments). Formulas are documented approximations of Photoshop's behaviour,
//! derived from its manual and observation, not from its implementation.

use photocraft_color::convert::{lab_to_srgb, srgb_to_lab};
use serde::{Deserialize, Serialize};

use crate::fxutil::gauss_blur_n;
use crate::other2::{hsl_to_rgb, rgb_to_hsl};
use crate::photo_util::{par_map, par_rows};

fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Blurred luminance (Gaussian, `radius` px ≈ 2σ like Photoshop's radius fields).
fn blurred_luma(px: &[[f32; 4]], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let mut l: Vec<f32> = px.iter().map(|p| luma([p[0], p[1], p[2]])).collect();
    gauss_blur_n(&mut l, w, h, 1, radius.max(0.0) / 2.0);
    l
}

/// Image › Adjustments › Shadows/Highlights parameters (Photoshop's dialog, percentages as 0..=100).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShadowsHighlights {
    pub shadow_amount: f32,
    pub shadow_tone: f32,
    pub shadow_radius: f32,
    pub highlight_amount: f32,
    pub highlight_tone: f32,
    pub highlight_radius: f32,
    /// Colour (saturation) correction in -100..=100, applied where tones were changed.
    pub color: f32,
    /// Midtone contrast in -100..=100.
    pub midtone: f32,
    /// Percent of pixels clipped to black / white (0..=50).
    pub black_clip: f32,
    pub white_clip: f32,
}

impl Default for ShadowsHighlights {
    fn default() -> Self {
        ShadowsHighlights {
            shadow_amount: 35.0,
            shadow_tone: 50.0,
            shadow_radius: 30.0,
            highlight_amount: 0.0,
            highlight_tone: 50.0,
            highlight_radius: 30.0,
            color: 20.0,
            midtone: 0.0,
            black_clip: 0.01,
            white_clip: 0.01,
        }
    }
}

/// Local tone mapping: a blurred-luminance mask selects shadows (dark neighbourhoods, up to the
/// tonal width) and highlights; shadows are lifted and highlights pulled down by a gain that
/// preserves hue, then saturation is corrected where the gain moved tones, midtone contrast is
/// applied, and the result is stretched by the clip percentages.
pub fn shadows_highlights(px: &mut [[f32; 4]], w: usize, h: usize, p: &ShadowsHighlights) {
    if px.is_empty() {
        return;
    }
    let ls = blurred_luma(px, w, h, p.shadow_radius);
    let lh = if (p.highlight_radius - p.shadow_radius).abs() < 1e-3 { ls.clone() } else { blurred_luma(px, w, h, p.highlight_radius) };
    let (sa, ha) = (p.shadow_amount / 100.0, p.highlight_amount / 100.0);
    let (st, ht) = ((p.shadow_tone / 100.0).max(0.01), (p.highlight_tone / 100.0).max(0.01));
    let color = p.color / 100.0;
    let mid = p.midtone / 100.0;
    par_rows(px, w, 1, |y, pxrow| {
        for (x, q) in pxrow.iter_mut().enumerate() {
            let i = y * w + x;
            if q[3] <= 0.0 {
                continue;
            }
            let c = [q[0], q[1], q[2]];
            // Mask weights: 1 in deep shadows (resp. bright highlights), fading out at the tonal width.
            let ms = 1.0 - smoothstep(0.0, st, ls[i]);
            let mh = smoothstep(1.0 - ht, 1.0, lh[i]);
            let mut out = c;
            if sa > 0.0 && ms > 0.0 {
                // Lift: a gamma-like curve on the pixel, stronger for darker neighbourhoods.
                let g = 1.0 / (1.0 + 2.0 * sa * ms);
                out = out.map(|v| v.max(0.0).powf(g));
            }
            if ha > 0.0 && mh > 0.0 {
                let g = 1.0 / (1.0 + 2.0 * ha * mh);
                out = out.map(|v| 1.0 - (1.0 - v.min(1.0)).powf(g));
            }
            if mid != 0.0 {
                let k = 1.0 + mid * (1.0 - ms - mh).clamp(0.0, 1.0);
                out = out.map(|v| (v - 0.5) * k + 0.5);
            }
            let changed = (ms * sa + mh * ha).min(1.0);
            if color != 0.0 && changed > 0.0 {
                let l1 = luma(out);
                let s = 1.0 + color * changed;
                out = out.map(|v| l1 + (v - l1) * s);
            }
            q[0] = out[0].clamp(0.0, 1.0);
            q[1] = out[1].clamp(0.0, 1.0);
            q[2] = out[2].clamp(0.0, 1.0);
        }
    });
    clip_stretch(px, p.black_clip, p.white_clip);
}

/// Stretches levels so `black`% of pixels clip to 0 and `white`% to 1 (luminance histogram).
fn clip_stretch(px: &mut [[f32; 4]], black: f32, white: f32) {
    if black <= 0.0 && white <= 0.0 {
        return;
    }
    let mut hist = [0u32; 1024];
    let mut n = 0u32;
    for q in px.iter().filter(|q| q[3] > 0.0) {
        hist[((luma([q[0], q[1], q[2]]).clamp(0.0, 1.0)) * 1023.0).round() as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return;
    }
    let find = |pct: f32, rev: bool| -> f32 {
        let target = (pct.clamp(0.0, 50.0) / 100.0 * n as f32) as u32;
        let mut acc = 0;
        for k in 0..1024 {
            let i = if rev { 1023 - k } else { k };
            acc += hist[i];
            if acc > target {
                return i as f32 / 1023.0;
            }
        }
        if rev { 0.0 } else { 1.0 }
    };
    let lo = find(black, false);
    let hi = find(white, true);
    if hi - lo < 1e-3 {
        return;
    }
    for q in px.iter_mut().filter(|q| q[3] > 0.0) {
        for v in &mut q[..3] {
            *v = ((*v - lo) / (hi - lo)).clamp(0.0, 1.0);
        }
    }
}

/// Image › Adjustments › Replace Color: Color Range-style selection (fuzziness in 0..=200 levels)
/// around `color`, then a hue (±180°), saturation and lightness (±100) shift weighted by it.
/// Returns the selection mask (what the dialog previews).
pub fn replace_color(px: &mut [[f32; 4]], color: [f32; 3], fuzziness: f32, hue: f32, saturation: f32, lightness: f32) -> Vec<f32> {
    let mask = crate::selection::color_range(px, color, fuzziness);
    for (q, &k) in px.iter_mut().zip(&mask) {
        if k <= 0.0 {
            continue;
        }
        let c = [q[0], q[1], q[2]];
        let [hh, ss, ll] = rgb_to_hsl(c);
        let s = if saturation >= 0.0 { ss + (1.0 - ss) * saturation / 100.0 } else { ss * (1.0 + saturation / 100.0) };
        let l = if lightness >= 0.0 { ll + (1.0 - ll) * lightness / 100.0 } else { ll * (1.0 + lightness / 100.0) };
        let o = hsl_to_rgb([(hh + hue / 360.0).rem_euclid(1.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0)]);
        for i in 0..3 {
            q[i] = (c[i] + (o[i] - c[i]) * k).clamp(0.0, 1.0);
        }
    }
    mask
}

/// Lab mean and standard deviation of the opaque pixels, `[L, a, b]` each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabStats {
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// Statistics for Match Color (pixels weighted by alpha × optional mask).
pub fn lab_stats(px: &[[f32; 4]], mask: Option<&[f32]>) -> Option<LabStats> {
    let mut sum = [0.0f64; 3];
    let mut sq = [0.0f64; 3];
    let mut wsum = 0.0f64;
    for (i, q) in px.iter().enumerate() {
        let w = f64::from(q[3] * mask.map_or(1.0, |m| m.get(i).copied().unwrap_or(0.0)));
        if w <= 0.0 {
            continue;
        }
        let lab = srgb_to_lab([q[0], q[1], q[2]]);
        for k in 0..3 {
            sum[k] += w * f64::from(lab[k]);
            sq[k] += w * f64::from(lab[k]) * f64::from(lab[k]);
        }
        wsum += w;
    }
    if wsum <= 0.0 {
        return None;
    }
    let mean: [f32; 3] = std::array::from_fn(|k| (sum[k] / wsum) as f32);
    let std = std::array::from_fn(|k| ((sq[k] / wsum - (sum[k] / wsum).powi(2)).max(0.0).sqrt()) as f32);
    Some(LabStats { mean, std })
}

/// Image › Adjustments › Match Color options (Photoshop ranges).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchColor {
    /// 1..=200 (100 = matched luminance).
    pub luminance: f32,
    /// Color intensity 1..=200 (100 = matched chroma; 1 ≈ gray).
    pub intensity: f32,
    /// 0..=100: blend back toward the original.
    pub fade: f32,
    /// Remove the colour cast (target a/b means → 0).
    pub neutralize: bool,
}

impl Default for MatchColor {
    fn default() -> Self {
        MatchColor { luminance: 100.0, intensity: 100.0, fade: 0.0, neutralize: false }
    }
}

/// Reinhard-style colour transfer in Lab: shift and scale each channel of `px` (statistics
/// `target`) toward `source`'s statistics; with no source only Neutralize / intensity / luminance
/// act on the image's own statistics.
pub fn match_color(px: &mut [[f32; 4]], target: &LabStats, source: Option<&LabStats>, o: &MatchColor) {
    let src = source.copied().unwrap_or(*target);
    let lum = o.luminance / 100.0;
    let inten = o.intensity / 100.0;
    let fade = (o.fade / 100.0).clamp(0.0, 1.0);
    let scale = |k: usize| {
        if target.std[k] > 1e-4 { src.std[k] / target.std[k] } else { 1.0 }
    };
    let mut dst_mean = src.mean;
    if o.neutralize {
        dst_mean[1] = 0.0;
        dst_mean[2] = 0.0;
    }
    for q in px.iter_mut().filter(|q| q[3] > 0.0) {
        let c = [q[0], q[1], q[2]];
        let lab = srgb_to_lab(c);
        let l = ((lab[0] - target.mean[0]) * scale(0) + dst_mean[0]) * lum;
        let a = ((lab[1] - target.mean[1]) * scale(1) + dst_mean[1]) * inten;
        let b = ((lab[2] - target.mean[2]) * scale(2) + dst_mean[2]) * inten;
        let o = lab_to_srgb([l.clamp(0.0, 100.0), a, b]);
        for i in 0..3 {
            q[i] = (o[i] + (c[i] - o[i]) * fade).clamp(0.0, 1.0);
        }
    }
}

/// Image › Adjustments › HDR Toning ("Local Adaptation" method) parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HdrToning {
    /// Edge glow radius in px (1..=500).
    pub radius: f32,
    /// Edge glow strength 0.1..=4.
    pub strength: f32,
    /// 0.1..=2 (1 = neutral; lower = more contrast).
    pub gamma: f32,
    /// -5..=5 stops.
    pub exposure: f32,
    /// -100..=300 %.
    pub detail: f32,
    pub shadow: f32,
    pub highlight: f32,
    /// -100..=100.
    pub vibrance: f32,
    pub saturation: f32,
    /// Toning curve on luminance, (input, output) in 0..=1; empty = identity.
    pub curve: Vec<(f32, f32)>,
}

impl Default for HdrToning {
    fn default() -> Self {
        HdrToning {
            radius: 30.0,
            strength: 0.5,
            gamma: 1.0,
            exposure: 0.0,
            detail: 30.0,
            shadow: 0.0,
            highlight: 0.0,
            vibrance: 0.0,
            saturation: 20.0,
            curve: Vec::new(),
        }
    }
}

fn curve_eval(pts: &[(f32, f32)], x: f32) -> f32 {
    if pts.len() < 2 {
        return x;
    }
    if x <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        if x <= w[1].0 {
            let span = (w[1].0 - w[0].0).max(1e-6);
            return w[0].1 + (w[1].1 - w[0].1) * (x - w[0].0) / span;
        }
    }
    pts[pts.len() - 1].1
}

/// Local adaptation tone mapping (Durand–Dorsey style base/detail split in log luminance): the
/// blurred log luminance (radius) is the base layer, compressed toward its mean by `strength`;
/// detail is boosted; then exposure, gamma, shadow/highlight lift, the toning curve, and
/// vibrance/saturation. Like Photoshop the result is a flattened, low-dynamic-range image.
pub fn hdr_toning(px: &mut [[f32; 4]], w: usize, h: usize, p: &HdrToning) {
    if px.is_empty() {
        return;
    }
    const EPS: f32 = 1e-4;
    let logl: Vec<f32> = par_map(px.len(), |i| {
        let q = px[i];
        (luma([q[0], q[1], q[2]]).max(0.0) + EPS).ln()
    });
    let mut base = logl.clone();
    gauss_blur_n(&mut base, w, h, 1, p.radius.max(1.0) / 2.0);
    let n = px.iter().filter(|q| q[3] > 0.0).count().max(1) as f32;
    let mean = base.iter().zip(px.iter()).filter(|(_, q)| q[3] > 0.0).map(|(b, _)| *b).sum::<f32>() / n;
    let compress = 1.0 / (1.0 + p.strength.max(0.0));
    let detail = 1.0 + p.detail / 100.0;
    let exposure = 2f32.powf(p.exposure);
    let gamma = p.gamma.clamp(0.1, 4.0);
    let mut sorted = p.curve.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Per-pixel tone map: embarrassingly parallel (base/logl read by index).
    par_rows(px, w, 1, |y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let i = y * w + x;
            if q[3] <= 0.0 {
                continue;
            }
            let c = [q[0], q[1], q[2]];
            let l0 = luma(c).max(0.0) + EPS;
            let new_log = mean + (base[i] - mean) * compress + (logl[i] - base[i]) * detail;
            let mut l1 = (new_log.exp() * exposure).max(0.0).powf(gamma);
            l1 += p.shadow / 100.0 * (1.0 - l1).powi(3) * 0.5 - p.highlight / 100.0 * l1.powi(3) * 0.5;
            l1 = curve_eval(&sorted, l1.clamp(0.0, 1.0));
            let ratio = l1 / l0;
            let mut out = c.map(|v| v * ratio);
            // Saturation then vibrance (less saturated colours move more), around the new luminance.
            let ll = luma(out);
            let sat_now = {
                let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
                if mx > 0.0 { (mx - mn) / mx } else { 0.0 }
            };
            let s = 1.0 + p.saturation / 100.0 + p.vibrance / 100.0 * (1.0 - sat_now);
            out = out.map(|v| ll + (v - ll) * s.max(0.0));
            q[0] = out[0].clamp(0.0, 1.0);
            q[1] = out[1].clamp(0.0, 1.0);
            q[2] = out[2].clamp(0.0, 1.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Vec<[f32; 4]> {
        (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 / (w - 1) as f32;
                [x, x * 0.8, x * 0.6 + 0.1, 1.0]
            })
            .collect()
    }

    #[test]
    fn shadows_lift_dark_pixels_more_than_bright() {
        let mut px = ramp(32, 8);
        let orig = px.clone();
        shadows_highlights(&mut px, 32, 8, &ShadowsHighlights { black_clip: 0.0, white_clip: 0.0, ..Default::default() });
        let gain = |i: usize| luma([px[i][0], px[i][1], px[i][2]]) - luma([orig[i][0], orig[i][1], orig[i][2]]);
        assert!(gain(3) > 0.02, "{}", gain(3));
        assert!(gain(3) > gain(30), "{} {}", gain(3), gain(30));
        // Zero amounts and no clipping: identity.
        let mut px2 = orig.clone();
        let id = ShadowsHighlights { shadow_amount: 0.0, highlight_amount: 0.0, color: 0.0, black_clip: 0.0, white_clip: 0.0, ..Default::default() };
        shadows_highlights(&mut px2, 32, 8, &id);
        assert_eq!(px2, orig);
    }

    #[test]
    fn highlights_darken_bright_pixels() {
        let mut px = ramp(32, 8);
        let orig = px.clone();
        let p = ShadowsHighlights { shadow_amount: 0.0, highlight_amount: 80.0, black_clip: 0.0, white_clip: 0.0, ..Default::default() };
        shadows_highlights(&mut px, 32, 8, &p);
        assert!(px[30][0] < orig[30][0]);
        assert!((px[1][0] - orig[1][0]).abs() < 1e-6);
    }

    #[test]
    fn replace_color_only_touches_matching_pixels() {
        let mut px = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        let m = replace_color(&mut px, [1.0, 0.0, 0.0], 40.0, 120.0, 0.0, 0.0);
        assert_eq!(m[1], 0.0);
        assert!(px[0][1] > 0.9 && px[0][0] < 0.1, "{:?}", px[0]);
        assert_eq!(px[1], [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn match_color_moves_statistics_toward_source() {
        let src: Vec<[f32; 4]> = (0..64).map(|i| [0.8, 0.4 + (i % 8) as f32 * 0.02, 0.2, 1.0]).collect();
        let mut tgt: Vec<[f32; 4]> = (0..64).map(|i| [0.2, 0.3, 0.6 + (i % 8) as f32 * 0.03, 1.0]).collect();
        let s = lab_stats(&src, None).unwrap();
        let t = lab_stats(&tgt, None).unwrap();
        match_color(&mut tgt, &t, Some(&s), &MatchColor::default());
        let after = lab_stats(&tgt, None).unwrap();
        for k in 0..3 {
            assert!((after.mean[k] - s.mean[k]).abs() < (t.mean[k] - s.mean[k]).abs() * 0.2 + 1.0, "{k}: {after:?} vs {s:?}");
        }
        // Neutralize with no source: the mean colour goes gray.
        let mut tint: Vec<[f32; 4]> = (0..16).map(|i| [0.7, 0.5, 0.3 + i as f32 * 0.01, 1.0]).collect();
        let t = lab_stats(&tint, None).unwrap();
        match_color(&mut tint, &t, None, &MatchColor { neutralize: true, ..Default::default() });
        let n = lab_stats(&tint, None).unwrap();
        assert!(n.mean[1].abs() < 2.0 && n.mean[2].abs() < 2.0, "{n:?}");
    }

    #[test]
    fn hdr_toning_compresses_range_and_keeps_bounds() {
        let mut px = ramp(40, 10);
        hdr_toning(&mut px, 40, 10, &HdrToning { strength: 2.0, detail: 0.0, saturation: 0.0, ..Default::default() });
        let l0 = luma([px[2][0], px[2][1], px[2][2]]);
        let l1 = luma([px[38][0], px[38][1], px[38][2]]);
        assert!(l1 > l0);
        assert!(px.iter().all(|q| q[..3].iter().all(|v| (0.0..=1.0).contains(v))));
        // A toning curve that inverts luminance flips the order.
        let mut px = ramp(40, 10);
        hdr_toning(&mut px, 40, 10, &HdrToning { strength: 0.0, detail: 0.0, saturation: 0.0, curve: vec![(0.0, 1.0), (1.0, 0.0)], ..Default::default() });
        assert!(luma([px[2][0], px[2][1], px[2][2]]) > luma([px[38][0], px[38][1], px[38][2]]));
    }
}

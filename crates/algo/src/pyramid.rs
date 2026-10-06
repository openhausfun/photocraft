//! Multiresolution blending for Edit › Auto-Blend Layers.
//!
//! P. J. Burt, E. H. Adelson, *A Multiresolution Spline With Application to Image Mosaics*,
//! ACM TOG 1983: each image is split into a Laplacian pyramid (band-pass levels plus a low-pass
//! residual), the blend weights into a Gaussian pyramid, every level is blended with its own
//! smoothed weights and the result collapsed, so seams are invisible at every scale.
//!
//! Weights come from [`stack_weights`] (focus stacking: per pixel, the sharpest image wins, by
//! the smoothed magnitude of the luminance Laplacian) or [`panorama_weights`] (each pixel goes to
//! the image whose opaque area it is deepest inside). Buffers are interleaved `w × h × ch`
//! normalised floats of any colour model and depth; deterministic.

/// One pyramid level.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

use crate::photo_util::par_rows;

const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// 5-tap binomial filter + decimation by two (Burt & Adelson's REDUCE).
pub fn reduce(l: &Level, ch: usize) -> Level {
    let (w, h) = (l.w.div_ceil(2), l.h.div_ceil(2));
    // Horizontal pass on even columns, then vertical on even rows.
    let mut tmp = vec![0.0f32; w * l.h * ch];
    par_rows(&mut tmp, w, ch, |y, trow| {
        for x in 0..w {
            for (k, wt) in K.iter().enumerate() {
                let sx = (2 * x as i64 + k as i64 - 2).clamp(0, l.w as i64 - 1) as usize;
                for c in 0..ch {
                    trow[x * ch + c] += wt * l.px[(y * l.w + sx) * ch + c];
                }
            }
        }
    });
    let mut px = vec![0.0f32; w * h * ch];
    par_rows(&mut px, w, ch, |y, prow| {
        for (k, wt) in K.iter().enumerate() {
            let sy = (2 * y as i64 + k as i64 - 2).clamp(0, l.h as i64 - 1) as usize;
            for x in 0..w {
                for c in 0..ch {
                    prow[x * ch + c] += wt * tmp[(sy * w + x) * ch + c];
                }
            }
        }
    });
    Level { w, h, px }
}

/// Upsample `l` to `w × h` (EXPAND: zero insertion + the same filter, ×4 gain).
pub fn expand(l: &Level, ch: usize, w: usize, h: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * l.h * ch];
    par_rows(&mut tmp, w, ch, |y, trow| {
        for x in 0..w {
            for (k, wt) in K.iter().enumerate() {
                let t = x as i64 + k as i64 - 2;
                if t.rem_euclid(2) != 0 {
                    continue;
                }
                let sx = (t / 2).clamp(0, l.w as i64 - 1) as usize;
                for c in 0..ch {
                    trow[x * ch + c] += 2.0 * wt * l.px[(y * l.w + sx) * ch + c];
                }
            }
        }
    });
    let mut out = vec![0.0f32; w * h * ch];
    par_rows(&mut out, w, ch, |y, orow| {
        for (k, wt) in K.iter().enumerate() {
            let t = y as i64 + k as i64 - 2;
            if t.rem_euclid(2) != 0 {
                continue;
            }
            let sy = (t / 2).clamp(0, l.h as i64 - 1) as usize;
            for x in 0..w {
                for c in 0..ch {
                    orow[x * ch + c] += 2.0 * wt * tmp[(sy * w + x) * ch + c];
                }
            }
        }
    });
    out
}

/// Gaussian pyramid with `levels` levels (level 0 = the input).
pub fn gaussian(w: usize, h: usize, ch: usize, px: &[f32], levels: usize) -> Vec<Level> {
    let mut out = vec![Level { w, h, px: px.to_vec() }];
    while out.len() < levels.max(1) {
        let Some(last) = out.last() else { break };
        if last.w <= 1 && last.h <= 1 {
            break;
        }
        let next = reduce(last, ch);
        out.push(next);
    }
    out
}

/// Laplacian pyramid: band-pass levels, the last level is the low-pass residual.
pub fn laplacian(w: usize, h: usize, ch: usize, px: &[f32], levels: usize) -> Vec<Level> {
    let g = gaussian(w, h, ch, px, levels);
    let mut out = Vec::with_capacity(g.len());
    for i in 0..g.len() {
        if i + 1 == g.len() {
            out.push(g[i].clone());
        } else {
            let up = expand(&g[i + 1], ch, g[i].w, g[i].h);
            out.push(Level { w: g[i].w, h: g[i].h, px: g[i].px.iter().zip(&up).map(|(a, b)| a - b).collect() });
        }
    }
    out
}

/// Rebuild an image from its Laplacian pyramid.
pub fn collapse(pyr: &[Level], ch: usize) -> Vec<f32> {
    let mut cur = pyr.last().cloned().unwrap_or(Level { w: 0, h: 0, px: Vec::new() });
    for l in pyr.iter().rev().skip(1) {
        let up = expand(&cur, ch, l.w, l.h);
        cur = Level { w: l.w, h: l.h, px: l.px.iter().zip(&up).map(|(a, b)| a + b).collect() };
    }
    cur.px
}

/// Number of levels so the coarsest level is a few pixels across.
pub fn auto_levels(w: usize, h: usize) -> usize {
    let mut n = 1;
    let mut s = w.min(h);
    while s > 8 && n < 10 {
        s /= 2;
        n += 1;
    }
    n
}

/// Blend `images` with per-pixel `weights` (any non-negative values; normalised per level).
pub fn blend(w: usize, h: usize, ch: usize, images: &[&[f32]], weights: &[Vec<f32>], levels: usize) -> Vec<f32> {
    assert_eq!(images.len(), weights.len());
    let levels = levels.max(1);
    let mut acc: Option<Vec<Level>> = None;
    let mut wsum: Option<Vec<Level>> = None;
    for (img, wt) in images.iter().zip(weights) {
        let lp = laplacian(w, h, ch, img, levels);
        let gp = gaussian(w, h, 1, wt, levels);
        let a = acc.get_or_insert_with(|| lp.iter().map(|l| Level { w: l.w, h: l.h, px: vec![0.0; l.px.len()] }).collect());
        let s = wsum.get_or_insert_with(|| gp.iter().map(|l| Level { w: l.w, h: l.h, px: vec![0.0; l.px.len()] }).collect());
        for (li, (l, g)) in lp.iter().zip(&gp).enumerate() {
            for i in 0..l.w * l.h {
                let k = g.px[i];
                s[li].px[i] += k;
                for c in 0..ch {
                    a[li].px[i * ch + c] += k * l.px[i * ch + c];
                }
            }
        }
    }
    let (Some(mut a), Some(s)) = (acc, wsum) else { return vec![0.0; w * h * ch] };
    for (l, sl) in a.iter_mut().zip(&s) {
        for i in 0..l.w * l.h {
            let k = sl.px[i];
            for c in 0..ch {
                l.px[i * ch + c] = if k > 1e-6 { l.px[i * ch + c] / k } else { 0.0 };
            }
        }
    }
    collapse(&a, ch).into_iter().map(|v| v.clamp(0.0, 1.0)).collect()
}

fn box_blur1(w: usize, h: usize, v: &[f32], r: usize) -> Vec<f32> {
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut dst = vec![0.0f32; src.len()];
        let (n, m) = if horizontal { (h, w) } else { (w, h) };
        for line in 0..n {
            let at = |k: i64| {
                let k = k.clamp(0, m as i64 - 1) as usize;
                if horizontal { src[line * w + k] } else { src[k * w + line] }
            };
            let mut s: f32 = (-(r as i64)..=r as i64).map(at).sum();
            for k in 0..m {
                let i = if horizontal { line * w + k } else { k * w + line };
                dst[i] = s / (2 * r + 1) as f32;
                s += at(k as i64 + r as i64 + 1) - at(k as i64 - r as i64);
            }
        }
        dst
    };
    pass(&pass(v, true), false)
}

/// One-hot winner maps: weight 1 for the image with the largest score at each pixel.
fn winner_take_all(scores: &[Vec<f32>], n: usize) -> Vec<Vec<f32>> {
    let mut out = vec![vec![0.0f32; n]; scores.len()];
    for i in 0..n {
        let mut best = (0usize, f32::MIN);
        for (k, s) in scores.iter().enumerate() {
            if s[i] > best.1 {
                best = (k, s[i]);
            }
        }
        if best.1 > f32::MIN {
            out[best.0][i] = 1.0;
        }
    }
    out
}

/// Focus-stack weights: per pixel, the image with the most local detail (smoothed |∇²L|) wins.
/// `luma[k]` is image k's luminance, `alpha[k]` its coverage.
pub fn stack_weights(w: usize, h: usize, luma: &[Vec<f32>], alpha: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let radius = (w.min(h) / 64).clamp(2, 12);
    let scores: Vec<Vec<f32>> = luma
        .iter()
        .zip(alpha)
        .map(|(l, a)| {
            let mut lap = vec![0.0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let p = |xx: usize, yy: usize| l[yy * w + xx];
                    let (xl, xr, yu, yd) = (x.saturating_sub(1), (x + 1).min(w - 1), y.saturating_sub(1), (y + 1).min(h - 1));
                    lap[y * w + x] = (p(xl, y) + p(xr, y) + p(x, yu) + p(x, yd) - 4.0 * p(x, y)).abs();
                }
            }
            box_blur1(w, h, &lap, radius).iter().zip(a).map(|(s, a)| if *a > 0.5 { *s } else { -1.0 }).collect()
        })
        .collect();
    winner_take_all(&scores, w * h)
}

/// Panorama weights: each pixel goes to the image whose opaque area it lies deepest inside
/// (chamfer distance to that image's transparent pixels), so seams run through overlaps.
pub fn panorama_weights(w: usize, h: usize, alpha: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let scores: Vec<Vec<f32>> = alpha
        .iter()
        .map(|a| {
            // Two-pass 3-4 chamfer distance transform from transparent pixels / the border.
            let inf = 1.0e9f32;
            let mut d: Vec<f32> = a.iter().map(|v| if *v > 0.5 { inf } else { 0.0 }).collect();
            let at = |d: &[f32], x: i64, y: i64| if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0.0 } else { d[y as usize * w + x as usize] };
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    let i = y as usize * w + x as usize;
                    let m = d[i].min(at(&d, x - 1, y) + 3.0).min(at(&d, x, y - 1) + 3.0).min(at(&d, x - 1, y - 1) + 4.0).min(at(&d, x + 1, y - 1) + 4.0);
                    d[i] = m;
                }
            }
            for y in (0..h as i64).rev() {
                for x in (0..w as i64).rev() {
                    let i = y as usize * w + x as usize;
                    let m = d[i].min(at(&d, x + 1, y) + 3.0).min(at(&d, x, y + 1) + 3.0).min(at(&d, x + 1, y + 1) + 4.0).min(at(&d, x - 1, y + 1) + 4.0);
                    d[i] = m;
                }
            }
            d.iter().map(|v| if *v <= 0.0 { -1.0 } else { *v }).collect()
        })
        .collect();
    winner_take_all(&scores, w * h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laplacian_round_trip_is_exact() {
        let (w, h) = (37, 23);
        let img: Vec<f32> = (0..w * h * 2).map(|i| ((i * 37 % 101) as f32) / 101.0).collect();
        let pyr = laplacian(w, h, 2, &img, 5);
        assert_eq!(pyr.len(), 5);
        let back = collapse(&pyr, 2);
        assert!(img.iter().zip(&back).all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn blend_with_one_hot_weights_reproduces_each_side() {
        let (w, h) = (64, 32);
        let a = vec![0.2f32; w * h];
        let b = vec![0.8f32; w * h];
        let wa: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { 1.0 } else { 0.0 }).collect();
        let wb: Vec<f32> = wa.iter().map(|v| 1.0 - v).collect();
        let out = blend(w, h, 1, &[&a, &b], &[wa, wb], auto_levels(w, h));
        assert!((out[5 * w + 2] - 0.2).abs() < 1e-3);
        assert!((out[5 * w + 61] - 0.8).abs() < 1e-3);
        // The seam is a smooth ramp, not a step.
        let mid = out[5 * w + 31];
        assert!(mid > 0.3 && mid < 0.7, "{mid}");
    }

    #[test]
    fn stack_picks_the_sharp_image() {
        let (w, h) = (48, 48);
        // Image 0 is sharp on the left (checker), image 1 on the right.
        let checker = |x: usize, y: usize| if (x / 2 + y / 2).is_multiple_of(2) { 0.1 } else { 0.9 };
        let i0: Vec<f32> = (0..w * h).map(|i| if i % w < 24 { checker(i % w, i / w) } else { 0.5 }).collect();
        let i1: Vec<f32> = (0..w * h).map(|i| if i % w >= 24 { checker(i % w, i / w) } else { 0.5 }).collect();
        let ones = vec![1.0f32; w * h];
        let wts = stack_weights(w, h, &[i0, i1], &[ones.clone(), ones]);
        assert_eq!(wts[0][20 * w + 4], 1.0);
        assert_eq!(wts[1][20 * w + 44], 1.0);
    }

    #[test]
    fn panorama_splits_the_overlap() {
        let (w, h) = (60, 20);
        let a: Vec<f32> = (0..w * h).map(|i| if i % w < 40 { 1.0 } else { 0.0 }).collect();
        let b: Vec<f32> = (0..w * h).map(|i| if i % w >= 20 { 1.0 } else { 0.0 }).collect();
        let wts = panorama_weights(w, h, &[a, b]);
        assert_eq!(wts[0][10 * w + 5], 1.0);
        assert_eq!(wts[1][10 * w + 55], 1.0);
        // The seam falls inside the overlap (20..40), near its middle.
        let seam = (0..w).find(|x| wts[1][10 * w + x] == 1.0).unwrap();
        assert!((25..=35).contains(&seam), "{seam}");
    }
}

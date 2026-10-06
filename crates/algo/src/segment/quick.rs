//! Quick Selection brush.
//!
//! A brush stroke seeds a graph cut in a working window around the stroke:
//!
//! 1. Stroked pixels are hard foreground; a GMM of their colours is the foreground model.
//! 2. A geodesic distance from the stroke over a barrier map (the colour gradient of a lightly
//!    smoothed image above the texture level found under the stroke; raster-scan geodesic
//!    transform, Toivanen 1996; Criminisi, Sharp & Blake, "GeoS: Geodesic Image Segmentation",
//!    ECCV 2008) separates what is reachable without crossing an edge from what lies behind
//!    edges. Pixels behind edges are
//!    sampled for the background GMM (mixed with a uniform outlier density, so a window without
//!    any edge simply selects everything similar), and the distance also enters the data term.
//! 3. A contrast-sensitive min cut (Boykov & Jolly 2001) snaps the result to image edges; only
//!    components touching the stroke are kept.
//!
//! This follows the spirit of "Paint Selection" (Liu, Sun & Shum, SIGGRAPH 2009) and of
//! geodesic matting (Bai & Sapiro, ICCV 2007). Large windows run at a reduced working resolution
//! and the boundary is re-cut at full resolution.

use photocraft_geom::Rect;

use super::gmm::Gmm;
use super::{FREE, HARD_FG, Region, RgbImage, Sampler, contrast_beta, grid_cut, keep_seeded, subsample};

/// Covariance ridge for the (often tiny) stroke sample: ~6 levels of standard deviation.
const QREG: f32 = 6e-4;
/// Geodesic distance (colour units above noise) that counts as "behind an edge".
const EDGE: f32 = 0.06;
/// Neighbour colour steps below this never count as edges (smooth shading, fine noise).
const MIN_STEP: f32 = 0.03;
/// Data-term weight of the geodesic distance (nats per `EDGE`).
const KAPPA: f32 = 2.0;
const GAMMA: f32 = 50.0;

/// Default working-resolution budget (pixels) for a stroke.
pub const WORK_PX: usize = 160_000;

/// Grows a selection from a brush stroke (`points` in document pixels, brush diameter `size`).
/// Returns the new region (to be added to or subtracted from the current selection), or `None`
/// when the stroke misses the canvas.
pub fn quick_select(sampler: &dyn Sampler, canvas: Rect, points: &[(f32, f32)], size: f32, work_px: usize) -> Option<Region> {
    if points.is_empty() {
        return None;
    }
    let size = size.max(1.0);
    let r = size / 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in points {
        x0 = x0.min(x - r);
        y0 = y0.min(y - r);
        x1 = x1.max(x + r);
        y1 = y1.max(y + r);
    }
    let bbox = Rect::new(x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32 + 1, y1.ceil() as i32 + 1).intersect(&canvas);
    if bbox.is_empty() {
        return None;
    }
    let margin = (size * 6.0).max(512.0) as i32;
    let window = bbox.inflate(margin).intersect(&canvas);
    let step = super::scale_for(window, work_px);
    let img = sampler.rgb_scaled(window, step);
    let (lw, lh) = (img.w, img.h);
    let st = step as f32;
    let pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| ((x - window.x0 as f32) / st, (y - window.y0 as f32) / st)).collect();
    let seeds = stroke_mask(&pts, (r / st).max(0.75), lw, lh);
    if !seeds.iter().any(|s| *s) {
        return None;
    }
    let (fg, bg, low) = segment(&img, &seeds);
    if !low.iter().any(|v| *v) {
        return None;
    }
    super::finish_region(sampler, window, step, &low, lw, lh, Some((&fg, &bg)))
}

/// The working-resolution segmentation: (foreground model, background model, labels).
pub fn segment(img: &RgbImage, seeds: &[bool]) -> (Gmm, Gmm, Vec<bool>) {
    let (lw, lh) = (img.w, img.h);
    let geo = geodesic(img, seeds);
    let fs: Vec<[f32; 3]> = img.px.iter().zip(seeds).filter(|(_, s)| **s).map(|(p, _)| *p).collect();
    let bs: Vec<[f32; 3]> = img.px.iter().zip(&geo).filter(|(_, d)| **d > EDGE).map(|(p, _)| *p).collect();
    let fs = subsample(&fs, 20_000);
    let bs = subsample(&bs, 20_000);
    let fg = Gmm::fit(&fs, (fs.len() / 40).clamp(1, 4), QREG).unwrap_or_else(broad_model);
    let bg = if bs.len() >= 30 { Gmm::fit(&bs, 6, QREG) } else { None };
    let eval = |(p, d): (&[f32; 3], &f32)| -> (f32, f32) {
        let cf = fg.neg_log(*p) + KAPPA * (d / EDGE).min(4.0);
        let cb = match &bg {
            Some(b) => -((0.85 * b.log_prob(*p).exp() + 0.15).ln() as f32),
            None => 0.0,
        };
        (cf, cb)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let costs: Vec<(f32, f32)> = {
        use rayon::prelude::*;
        img.px.par_iter().zip(geo.par_iter()).map(eval).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let costs: Vec<(f32, f32)> = img.px.iter().zip(geo.iter()).map(eval).collect();
    let (cf, cb): (Vec<f32>, Vec<f32>) = costs.into_iter().unzip();
    let fixed: Vec<u8> = seeds.iter().map(|s| if *s { HARD_FG } else { FREE }).collect();
    let cut = grid_cut(img, &cf, &cb, &fixed, GAMMA, contrast_beta(img));
    let low = keep_seeded(&cut, seeds, lw, lh);
    (fg, bg.unwrap_or_else(broad_model), low)
}

/// A near-uniform density over the RGB cube.
pub fn broad_model() -> Gmm {
    let corners: Vec<[f32; 3]> = (0..8).map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect();
    // Eight fixed, distinct samples: the fit cannot fail.
    #[allow(clippy::expect_used)]
    Gmm::fit(&corners, 1, 0.0).expect("non-degenerate")
}

/// Pixels within `radius` (working pixels) of the polyline `pts`, plus each point's own pixel.
pub fn stroke_mask(pts: &[(f32, f32)], radius: f32, w: usize, h: usize) -> Vec<bool> {
    let mut m = vec![false; w * h];
    let segs: Vec<((f32, f32), (f32, f32))> = if pts.len() == 1 { vec![(pts[0], pts[0])] } else { pts.windows(2).map(|p| (p[0], p[1])).collect() };
    for (a, b) in segs {
        let x0 = ((a.0.min(b.0) - radius).floor().max(0.0)) as usize;
        let y0 = ((a.1.min(b.1) - radius).floor().max(0.0)) as usize;
        let x1 = ((a.0.max(b.0) + radius).ceil().max(0.0) as usize).min(w);
        let y1 = ((a.1.max(b.1) + radius).ceil().max(0.0) as usize).min(h);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (qx, qy) = (a.0 + t * dx - px, a.1 + t * dy - py);
                if qx * qx + qy * qy <= radius * radius {
                    m[y * w + x] = true;
                }
            }
        }
    }
    for &(x, y) in pts {
        if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
            m[y as usize * w + x as usize] = true;
        }
    }
    m
}

fn blur3(img: &RgbImage) -> RgbImage {
    let (w, h) = (img.w, img.h);
    let mut tmp = img.clone();
    for y in 0..h {
        for x in 0..w {
            let (l, r) = (img.at(x.saturating_sub(1), y), img.at((x + 1).min(w - 1), y));
            let c = img.at(x, y);
            tmp.px[y * w + x] = [0, 1, 2].map(|k| (l[k] + c[k] + r[k]) / 3.0);
        }
    }
    let mut out = tmp.clone();
    for y in 0..h {
        for x in 0..w {
            let (u, d) = (tmp.at(x, y.saturating_sub(1)), tmp.at(x, (y + 1).min(h - 1)));
            let c = tmp.at(x, y);
            out.px[y * w + x] = [0, 1, 2].map(|k| (u[k] + c[k] + d[k]) / 3.0);
        }
    }
    out
}

/// Geodesic distance from `seeds` over a barrier map: each pixel costs its colour gradient
/// magnitude (smoothed image) above the texture level of the stroke (twice the median gradient
/// under it), plus a tiny spatial term. Raster-scan approximation (two forward/backward sweeps,
/// 8-neighbours).
pub fn geodesic(img: &RgbImage, seeds: &[bool]) -> Vec<f32> {
    let (w, h) = (img.w, img.h);
    let sm = blur3(&blur3(img));
    // Texture level of what the user painted: median local gradient over the stroke (grown by
    // two pixels). Gradual colour changes and texture at that level cost nothing.
    let mut near = vec![false; w * h];
    for (i, _) in seeds.iter().enumerate().filter(|(_, s)| **s) {
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(2)..(y + 3).min(h) {
            near[yy * w + x.saturating_sub(2)..yy * w + (x + 3).min(w)].fill(true);
        }
    }
    // Colour gradient magnitude (central differences) of the smoothed image.
    let mut g = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (l, r) = (sm.at(x.saturating_sub(1), y), sm.at((x + 1).min(w - 1), y));
            let (u, dn) = (sm.at(x, y.saturating_sub(1)), sm.at(x, (y + 1).min(h - 1)));
            g[y * w + x] = ((super::d2(l, r) + super::d2(u, dn)) * 0.25).sqrt();
        }
    }
    let mut diffs: Vec<f32> = (0..w * h).filter(|i| near[*i]).map(|i| g[i]).collect();
    let tau = if diffs.is_empty() {
        0.0
    } else {
        let mid = diffs.len() / 2;
        *diffs.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1
    };
    let thr = (2.0 * tau).max(MIN_STEP);
    // Crossing pixels whose gradient exceeds the texture level costs the excess: a barrier map,
    // so an edge costs about its contrast whatever direction the path crosses it.
    let e: Vec<f32> = g.iter().map(|v| (v - thr).max(0.0)).collect();
    let cost = |a: usize, b: usize, len: f32| len * (0.5 * (e[a] + e[b]) + 1e-6);
    let mut d: Vec<f32> = seeds.iter().map(|s| if *s { 0.0 } else { f32::INFINITY }).collect();
    let sq = std::f32::consts::SQRT_2;
    for _ in 0..2 {
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let mut v = d[i];
                if x > 0 {
                    v = v.min(d[i - 1] + cost(i, i - 1, 1.0));
                }
                if y > 0 {
                    v = v.min(d[i - w] + cost(i, i - w, 1.0));
                    if x > 0 {
                        v = v.min(d[i - w - 1] + cost(i, i - w - 1, sq));
                    }
                    if x + 1 < w {
                        v = v.min(d[i - w + 1] + cost(i, i - w + 1, sq));
                    }
                }
                d[i] = v;
            }
        }
        for y in (0..h).rev() {
            for x in (0..w).rev() {
                let i = y * w + x;
                let mut v = d[i];
                if x + 1 < w {
                    v = v.min(d[i + 1] + cost(i, i + 1, 1.0));
                }
                if y + 1 < h {
                    v = v.min(d[i + w] + cost(i, i + w, 1.0));
                    if x + 1 < w {
                        v = v.min(d[i + w + 1] + cost(i, i + w + 1, sq));
                    }
                    if x > 0 {
                        v = v.min(d[i + w - 1] + cost(i, i + w - 1, sq));
                    }
                }
                d[i] = v;
            }
        }
    }
    d
}

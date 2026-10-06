//! GrabCut and rectangle Object Selection.
//!
//! Implemented from C. Rother, V. Kolmogorov and A. Blake, "GrabCut: Interactive Foreground
//! Extraction using Iterated Graph Cuts", SIGGRAPH 2004: a trimap (definite / probable
//! foreground and background), 5-component full-covariance GMMs per side, and iterated
//! "assign components → learn GMMs → min cut" steps with the contrast-sensitive smoothness term
//! `γ e^{−β‖z_m − z_n‖²} / dist(m, n)` (γ = 50, β from the mean squared neighbour difference).

use photocraft_geom::Rect;

use super::gmm::{DEFAULT_REG, Gmm};
use super::{FREE, HARD_BG, HARD_FG, Region, RgbImage, Sampler, contrast_beta, data_costs, grid_cut, subsample};

/// Definite background.
pub const BG: u8 = 0;
/// Definite foreground.
pub const FG: u8 = 1;
/// Probable background.
pub const PR_BG: u8 = 2;
/// Probable foreground.
pub const PR_FG: u8 = 3;

/// GrabCut's smoothness weight.
pub const GAMMA: f32 = 50.0;
/// Components per GMM.
pub const K: usize = 5;
const MAX_SAMPLES: usize = 40_000;

fn is_fg(t: u8) -> bool {
    t == FG || t == PR_FG
}

/// Runs `iters` GrabCut iterations, updating the probable labels of `trimap` in place. Returns
/// the final (foreground, background) models, or `None` if either side has no pixels.
pub fn grabcut(img: &RgbImage, trimap: &mut [u8], iters: usize) -> Option<(Gmm, Gmm)> {
    let beta = contrast_beta(img);
    let fixed: Vec<u8> = trimap
        .iter()
        .map(|t| match *t {
            FG => HARD_FG,
            BG => HARD_BG,
            _ => FREE,
        })
        .collect();
    let mut models: Option<(Gmm, Gmm)> = None;
    for _ in 0..iters.max(1) {
        let (mut fs, mut bs) = (Vec::new(), Vec::new());
        for (p, t) in img.px.iter().zip(trimap.iter()) {
            if is_fg(*t) { fs.push(*p) } else { bs.push(*p) }
        }
        let (fs, bs) = (subsample(&fs, MAX_SAMPLES), subsample(&bs, MAX_SAMPLES));
        let (fg, bg) = match &models {
            None => (Gmm::fit(&fs, K, DEFAULT_REG)?, Gmm::fit(&bs, K, DEFAULT_REG)?),
            Some((f, b)) => (f.refit(&fs).unwrap_or_else(|| f.clone()), b.refit(&bs).unwrap_or_else(|| b.clone())),
        };
        let (cf, cb) = data_costs(img, &fg, &bg);
        let cut = grid_cut(img, &cf, &cb, &fixed, GAMMA, beta);
        let mut changed = 0usize;
        for (t, fgnd) in trimap.iter_mut().zip(&cut) {
            if *t == PR_BG || *t == PR_FG {
                let nt = if *fgnd { PR_FG } else { PR_BG };
                changed += (nt != *t) as usize;
                *t = nt;
            }
        }
        models = Some((fg, bg));
        if changed * 2000 < trimap.len() {
            break;
        }
    }
    models
}

/// Object Selection (rectangle mode): GrabCut initialised from `rect` (outside = background,
/// inside = probable foreground). Runs on a working copy of about `work_px` pixels, then re-cuts
/// the boundary at full resolution. `canvas` bounds the analysis.
pub fn object_select(sampler: &dyn Sampler, canvas: Rect, rect: Rect, work_px: usize) -> Option<Region> {
    let rect = rect.intersect(&canvas);
    if rect.width() < 2 || rect.height() < 2 {
        return None;
    }
    let margin = ((rect.width().max(rect.height()) as f32 * 0.15).ceil() as i32).max(8);
    let window = rect.inflate(margin).intersect(&canvas);
    let step = super::scale_for(window, work_px);
    let img = sampler.rgb_scaled(window, step);
    let (lw, lh) = (img.w, img.h);
    let mut trimap = vec![BG; lw * lh];
    // Working pixel (x, y) covers doc pixels [x0 + x·step, …); its centre decides inside/outside.
    let inside = |x: usize, y: usize| {
        let (cx, cy) = (window.x0 + (x * step + step / 2) as i32, window.y0 + (y * step + step / 2) as i32);
        rect.contains(cx, cy)
    };
    let mut any_bg = false;
    for y in 0..lh {
        for x in 0..lw {
            if inside(x, y) {
                trimap[y * lw + x] = PR_FG;
            } else {
                any_bg = true;
            }
        }
    }
    if !any_bg {
        // The rectangle covers the whole canvas: use a thin outer ring as background.
        let ring = (lw.min(lh) / 50).max(1);
        for y in 0..lh {
            for x in 0..lw {
                if x < ring || y < ring || x + ring >= lw || y + ring >= lh {
                    trimap[y * lw + x] = BG;
                }
            }
        }
    }
    let (fg, bg) = grabcut(&img, &mut trimap, 8)?;
    let low: Vec<bool> = trimap.iter().map(|t| is_fg(*t)).collect();
    let low = super::clean_mask(&low, lw, lh, 0.1, 0.002);
    super::finish_region(sampler, window, step, &low, lw, lh, Some((&fg, &bg)))
}

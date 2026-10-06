//! Add Noise, Median, Dust & Scratches.

use photocraft_geom::Rect;

use crate::image::Image;
use crate::{Ctx, Distribution};

/// Deterministic hash → `[0, 1)` from document coordinates, so results do not
/// depend on tiling.
#[inline]
pub(crate) fn hash01(x: i32, y: i32, c: u32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ c.wrapping_mul(0xcb1a_b31f) ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Amount 100 % spans ±50 % of the range (uniform); Gaussian uses the same
/// amount as ~2σ.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add(src: &Image, out: Rect, ctx: &Ctx, amount: f32, dist: Distribution, mono: bool, seed: u32) -> Vec<f32> {
    let n = src.ch;
    let cc = if ctx.alpha { n - 1 } else { n };
    let a = amount.max(0.0) / 100.0 * 0.5;
    let mut res = src.crop(out);
    let w = out.width() as usize;
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        if ctx.alpha && px[n - 1] <= 0.0 {
            continue;
        }
        let (x, y) = (out.x0 + (i % w) as i32, out.y0 + (i / w) as i32);
        for (c, pv) in px.iter_mut().enumerate().take(cc) {
            let ch = if mono { 0 } else { c as u32 };
            let v = match dist {
                Distribution::Uniform => (hash01(x, y, ch, seed) - 0.5) * 2.0 * a,
                Distribution::Gaussian => {
                    let u1 = hash01(x, y, ch * 2 + 101, seed).max(1e-7);
                    let u2 = hash01(x, y, ch * 2 + 102, seed);
                    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos() * a * 0.5
                }
            };
            // Integer storage clamps on write; float surfaces keep the value.
            *pv += v;
        }
    }
    res
}

/// Median over a disc of `radius`; with `threshold`, a pixel is replaced only
/// when it differs from the median by more than `threshold` levels.
pub(crate) fn median(src: &Image, out: Rect, radius: f32, threshold: Option<f32>) -> Vec<f32> {
    let n = src.ch;
    let r = radius.max(0.0).round() as i32;
    if r == 0 {
        return src.crop(out);
    }
    let offs: Vec<(i32, i32)> = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).filter(|(dx, dy)| dx * dx + dy * dy <= r * r + r).collect();
    let t = threshold.map(|t| t / 255.0);
    let mut vals = Vec::with_capacity(offs.len());
    let mut res = Vec::with_capacity(out.width() as usize * out.height() as usize * n);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            for c in 0..n {
                vals.clear();
                vals.extend(offs.iter().map(|(dx, dy)| src.get(x + dx, y + dy, c)));
                let mid = vals.len() / 2;
                let (_, m, _) = vals.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
                let m = *m;
                let o = src.get(x, y, c);
                res.push(match t {
                    Some(t) if (o - m).abs() <= t => o,
                    _ => m,
                });
            }
        }
    }
    res
}

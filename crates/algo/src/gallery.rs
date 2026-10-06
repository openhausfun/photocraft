//! Blur Gallery: Tilt-Shift, Iris, Field (spatially varying Gaussian blur),
//! Spin (rotational blur inside ellipses) and Path (motion along paths).
//!
//! Spatially varying blur interpolates between a stack of Gaussian levels
//! (σ = 0 and a geometric series up to the maximum), each computed with a
//! radius-independent box cascade, so cost grows with the number of levels,
//! not the blur size.

use photocraft_geom::Rect;

use crate::fxutil::{MAXC, add_sample_premul, gauss_blur_n, premul_window, smoothstep, unpremul_px, xy};
use crate::image::Image;
use crate::{BlurPath, Ctx, FieldPin, IrisPin, SpinPin};

/// Gaussian σ for a gallery blur amount in px.
fn sigma_of(blur: f32) -> f32 {
    blur.clamp(0.0, 500.0) * 0.6
}

/// Pixels read beyond an output tile for a maximum blur amount.
pub(crate) fn reach(blur: f32) -> f32 {
    3.0 * sigma_of(blur) + 2.0
}

const LEVELS: usize = 6;

/// Blurs `src` over `out` with a per-pixel σ (`sigma(x, y)`, ≤ `smax`).
fn variable_blur(src: &Image, out: Rect, ctx: &Ctx, smax: f32, sigma: impl Fn(f32, f32) -> f32) -> Vec<f32> {
    let n = src.ch;
    if smax < 0.3 {
        return src.crop(out);
    }
    let win = src.rect;
    let (ww, wh) = (win.width() as usize, win.height() as usize);
    let (ow, oh) = (out.width() as usize, out.height() as usize);
    // Level σs: 0, then smax / 2^(LEVELS-1) … smax.
    let lv: Vec<f32> = std::iter::once(0.0).chain((0..LEVELS).map(|k| smax / 2f32.powi((LEVELS - 1 - k) as i32))).collect();
    let sig: Vec<f32> = (0..ow * oh)
        .map(|i| {
            let (x, y) = xy(out, i);
            sigma(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, smax)
        })
        .collect();
    // Hat-function weight of level k for σ s.
    let weight = |k: usize, s: f32| -> f32 {
        let c = lv[k];
        if k > 0 && s >= lv[k - 1] && s <= c {
            (s - lv[k - 1]) / (c - lv[k - 1]).max(1e-6)
        } else if k + 1 < lv.len() && s >= c && s <= lv[k + 1] {
            (lv[k + 1] - s) / (lv[k + 1] - c).max(1e-6)
        } else if k + 1 == lv.len() && s >= c {
            1.0
        } else {
            0.0
        }
    };
    let p = premul_window(src, win, ctx.alpha);
    let mut acc = vec![0.0f32; ow * oh * n];
    let mut buf = Vec::new();
    for (k, &lvk) in lv.iter().enumerate() {
        if !sig.iter().any(|&s| weight(k, s) > 0.0) {
            continue;
        }
        let level: &[f32] = if k == 0 {
            &p
        } else {
            buf.clear();
            buf.extend_from_slice(&p);
            gauss_blur_n(&mut buf, ww, wh, n, lvk);
            &buf
        };
        for (i, &s) in sig.iter().enumerate() {
            let wk = weight(k, s);
            if wk <= 0.0 {
                continue;
            }
            let (x, y) = xy(out, i);
            let o = (((y - win.y0) as usize) * ww + (x - win.x0) as usize) * n;
            for c in 0..n {
                acc[i * n + c] += level[o + c] * wk;
            }
        }
    }
    for px in acc.chunks_exact_mut(n) {
        unpremul_px(px, ctx.alpha);
    }
    acc
}

fn short_side(b: Rect) -> f32 {
    (b.width().min(b.height()) as f32).max(1.0)
}

/// Tilt-Shift: a sharp band through the centre at `angle`, blurring with
/// distance beyond `focus` over `transition` (both fractions of the shorter side).
#[allow(clippy::too_many_arguments)]
pub(crate) fn tilt_shift(src: &Image, out: Rect, ctx: &Ctx, blur: f32, centre: (f32, f32), angle: f32, focus: f32, transition: f32) -> Vec<f32> {
    let b = ctx.bounds;
    let ss = short_side(b);
    let (cx, cy) = (b.x0 as f32 + centre.0 * b.width() as f32, b.y0 as f32 + centre.1 * b.height() as f32);
    // Band normal (the band runs along `angle`).
    let (s, c) = angle.to_radians().sin_cos();
    let (nx, ny) = (s, c);
    let smax = sigma_of(blur);
    let (f0, f1) = (focus.max(0.0), focus.max(0.0) + transition.max(1e-3));
    variable_blur(src, out, ctx, smax, |x, y| {
        let d = ((x - cx) * nx + (y - cy) * ny).abs() / ss;
        smax * smoothstep(f0, f1, d)
    })
}

/// Normalized (super)elliptical distance of a point from an iris/spin pin.
#[allow(clippy::too_many_arguments)]
fn pin_distance(x: f32, y: f32, b: Rect, px: f32, py: f32, rx: f32, ry: f32, angle: f32, m: f32) -> (f32, f32, f32) {
    let ss = short_side(b);
    let (cx, cy) = (b.x0 as f32 + px * b.width() as f32, b.y0 as f32 + py * b.height() as f32);
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
    let (u, v) = (u / (rx.max(1e-3) * ss), v / (ry.max(1e-3) * ss));
    let e = if (m - 2.0).abs() < 1e-3 { (u * u + v * v).sqrt() } else { (u.abs().powf(m) + v.abs().powf(m)).powf(1.0 / m) };
    (e, u, v)
}

/// Iris Blur: everything outside each pin's ellipse is blurred; the sharp
/// core is `feather` of the ellipse. Sharpness from any pin wins.
pub(crate) fn iris(src: &Image, out: Rect, ctx: &Ctx, pins: &[IrisPin]) -> Vec<f32> {
    let b = ctx.bounds;
    let smax = pins.iter().map(|p| sigma_of(p.blur)).fold(0.0, f32::max);
    if pins.is_empty() {
        return src.crop(out);
    }
    variable_blur(src, out, ctx, smax, |x, y| {
        pins.iter()
            .map(|p| {
                let m = 2.0 + p.roundness.clamp(0.0, 100.0) / 100.0 * 6.0;
                let (e, _, _) = pin_distance(x, y, b, p.x, p.y, p.radius_x, p.radius_y, p.angle, m);
                sigma_of(p.blur) * smoothstep(p.feather.clamp(0.0, 0.999), 1.0, e)
            })
            .fold(f32::MAX, f32::min)
    })
}

/// Field Blur: blur amounts at pins, interpolated by inverse squared distance.
pub(crate) fn field(src: &Image, out: Rect, ctx: &Ctx, pins: &[FieldPin]) -> Vec<f32> {
    let b = ctx.bounds;
    if pins.is_empty() {
        return src.crop(out);
    }
    let smax = pins.iter().map(|p| sigma_of(p.blur)).fold(0.0, f32::max);
    let pts: Vec<(f32, f32, f32)> =
        pins.iter().map(|p| (b.x0 as f32 + p.x * b.width() as f32, b.y0 as f32 + p.y * b.height() as f32, sigma_of(p.blur))).collect();
    variable_blur(src, out, ctx, smax, |x, y| {
        let (mut ws, mut s) = (0.0f32, 0.0f32);
        for &(px, py, sg) in &pts {
            let w = 1.0 / ((x - px).powi(2) + (y - py).powi(2) + 1.0);
            ws += w;
            s += w * sg;
        }
        s / ws
    })
}

/// Spin Blur: content inside each ellipse is blurred along arcs about its
/// centre (`blur_angle` degrees of rotation), feathered at the rim.
pub(crate) fn spin(src: &Image, out: Rect, ctx: &Ctx, pins: &[SpinPin]) -> Vec<f32> {
    let n = src.ch;
    let b = ctx.bounds;
    let ss = short_side(b);
    let mut res = src.crop(out);
    let clip = b.intersect(&src.rect);
    if clip.is_empty() {
        return res;
    }
    let mut acc = [0.0f32; MAXC];
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let (x, y) = xy(out, i);
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        for p in pins {
            let (e, u, v) = pin_distance(fx, fy, b, p.x, p.y, p.radius_x, p.radius_y, p.angle, 2.0);
            if e >= 1.0 {
                continue;
            }
            let theta = p.blur_angle.clamp(0.0, 360.0).to_radians();
            let rpx = e * p.radius_x.max(p.radius_y) * ss;
            // ~2 px between samples along the arc is smooth enough for a blur and halves the cost.
            let m = ((theta * rpx / 2.0).ceil() as usize).clamp(2, 64);
            let (cx, cy) = (b.x0 as f32 + p.x * b.width() as f32, b.y0 as f32 + p.y * b.height() as f32);
            let (sa, ca) = p.angle.to_radians().sin_cos();
            acc[..n].fill(0.0);
            for j in 0..m {
                let phi = theta * (j as f32 / (m - 1) as f32 - 0.5);
                let (sp, cp) = phi.sin_cos();
                // Rotate in the ellipse's normalized space, then map back.
                let (ru, rv) = (u * cp - v * sp, u * sp + v * cp);
                let (lu, lv) = (ru * p.radius_x * ss, rv * p.radius_y * ss);
                let (sx, sy) = (cx + lu * ca - lv * sa, cy + lu * sa + lv * ca);
                add_sample_premul(src, sx, sy, clip, ctx.alpha, &mut acc[..n]);
            }
            for v in acc.iter_mut().take(n) {
                *v /= m as f32;
            }
            unpremul_px(&mut acc[..n], ctx.alpha);
            let k = 1.0 - smoothstep(0.85, 1.0, e);
            for c in 0..n {
                px[c] += (acc[c] - px[c]) * k;
            }
            break;
        }
    }
    res
}

/// Path Blur: motion blur whose direction follows the nearest paths
/// (inverse-distance weighted tangents) and whose length is the path speed,
/// optionally tapering toward each path's end.
pub(crate) fn path(src: &Image, out: Rect, ctx: &Ctx, paths: &[BlurPath]) -> Vec<f32> {
    let n = src.ch;
    let b = ctx.bounds;
    let (bw, bh) = (b.width() as f32, b.height() as f32);
    // Segments in document space: (ax, ay, bx, by, path index, t0, t1 along path).
    let mut segs = Vec::new();
    for (pi, p) in paths.iter().enumerate() {
        let pts: Vec<(f32, f32)> = p.points.iter().map(|q| (b.x0 as f32 + q[0] * bw, b.y0 as f32 + q[1] * bh)).collect();
        let total: f32 = pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum::<f32>().max(1e-3);
        let mut run = 0.0;
        for w in pts.windows(2) {
            let l = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            segs.push((w[0].0, w[0].1, w[1].0, w[1].1, pi, run / total, (run + l) / total));
            run += l;
        }
    }
    let mut res = src.crop(out);
    let clip = b.intersect(&src.rect);
    if segs.is_empty() || clip.is_empty() {
        return res;
    }
    let mut acc = [0.0f32; MAXC];
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let (x, y) = xy(out, i);
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let (mut dx, mut dy, mut len, mut ws) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for &(ax, ay, bx, by, pi, t0, t1) in &segs {
            let (sx, sy) = (bx - ax, by - ay);
            let l2 = (sx * sx + sy * sy).max(1e-6);
            let t = (((fx - ax) * sx + (fy - ay) * sy) / l2).clamp(0.0, 1.0);
            let d2 = (fx - ax - sx * t).powi(2) + (fy - ay - sy * t).powi(2);
            let w = 1.0 / (d2 + 1.0);
            let l = l2.sqrt();
            let p = &paths[pi];
            let along = t0 + (t1 - t0) * t;
            dx += w * sx / l;
            dy += w * sy / l;
            len += w * p.speed.abs() * (1.0 - p.taper.clamp(0.0, 100.0) / 100.0 * along);
            ws += w;
        }
        let m = (dx * dx + dy * dy).sqrt();
        let len = len / ws;
        if m < 1e-6 || len < 0.5 {
            continue;
        }
        let (ux, uy) = (dx / m, dy / m);
        // Samples ~1.5 px apart (bilinear fills the gaps), capped for very long blurs.
        let steps = ((len / 1.5).ceil() as usize + 1).clamp(2, 64);
        acc[..n].fill(0.0);
        for j in 0..steps {
            let o = len * (j as f32 / (steps - 1) as f32 - 0.5);
            add_sample_premul(src, fx + ux * o, fy + uy * o, clip, ctx.alpha, &mut acc[..n]);
        }
        for v in acc.iter_mut().take(n) {
            *v /= steps as f32;
        }
        unpremul_px(&mut acc[..n], ctx.alpha);
        px.copy_from_slice(&acc[..n]);
    }
    res
}

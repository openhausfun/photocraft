//! High-quality separable resampling of surfaces (Image Size).

use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

use crate::image::{premultiply, unpremultiply};

/// Resampling method (Photoshop's Image Size options).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Resample {
    Nearest,
    Bilinear,
    #[default]
    Bicubic,
    Lanczos,
    /// Lanczos followed by a light sharpening when enlarging.
    PreserveDetails,
}

impl Resample {
    fn support(self) -> f64 {
        match self {
            Resample::Nearest => 0.5,
            Resample::Bilinear => 1.0,
            Resample::Bicubic => 2.0,
            Resample::Lanczos | Resample::PreserveDetails => 3.0,
        }
    }
    fn weight(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Resample::Nearest => {
                if x <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Resample::Bilinear => (1.0 - x).max(0.0),
            Resample::Bicubic => {
                // Keys cubic, a = -0.5 (Catmull-Rom).
                let a = -0.5;
                if x < 1.0 {
                    (a + 2.0) * x * x * x - (a + 3.0) * x * x + 1.0
                } else if x < 2.0 {
                    a * x * x * x - 5.0 * a * x * x + 8.0 * a * x - 4.0 * a
                } else {
                    0.0
                }
            }
            Resample::Lanczos | Resample::PreserveDetails => {
                if x < 1e-9 {
                    1.0
                } else if x < 3.0 {
                    let px = std::f64::consts::PI * x;
                    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
                } else {
                    0.0
                }
            }
        }
    }
}

/// Per-output-sample source window and normalized weights.
struct Taps {
    start: i32,
    weights: Vec<f32>,
}

/// Taps for mapping output coordinate `o` (in `[dst0, dst1)`) to the source
/// via `u = (o + 0.5) / scale - 0.5`.
fn taps(dst0: i32, dst1: i32, scale: f64, filter: Resample) -> Vec<Taps> {
    let stretch = if scale < 1.0 { 1.0 / scale } else { 1.0 };
    let support = filter.support() * stretch;
    (dst0..dst1)
        .map(|o| {
            let u = (o as f64 + 0.5) / scale - 0.5;
            if filter == Resample::Nearest && scale >= 1.0 {
                return Taps { start: (u + 0.5).floor() as i32, weights: vec![1.0] };
            }
            let lo = (u - support).ceil() as i32;
            let hi = (u + support).floor() as i32;
            let mut w: Vec<f64> = (lo..=hi).map(|i| filter.weight((i as f64 - u) / stretch)).collect();
            let s: f64 = w.iter().sum();
            if s.abs() > 1e-12 {
                for v in &mut w {
                    *v /= s;
                }
            }
            Taps { start: lo, weights: w.into_iter().map(|v| v as f32).collect() }
        })
        .collect()
}

/// Output rectangle for `r` scaled by `(sx, sy)` about the origin.
pub fn scaled_rect(r: Rect, sx: f64, sy: f64) -> Rect {
    if r.is_empty() {
        return Rect::EMPTY;
    }
    Rect::new((r.x0 as f64 * sx).floor() as i32, (r.y0 as f64 * sy).floor() as i32, (r.x1 as f64 * sx).ceil() as i32, (r.y1 as f64 * sy).ceil() as i32)
}

/// Resizes a surface by `(sx, sy)` about the document origin. The surface's
/// default pixel is kept (masks keep their "reveal all" background).
pub fn resize_surface(s: &Surface, sx: f64, sy: f64, filter: Resample) -> Surface {
    let fmt = s.format();
    let mut out = Surface::with_default(fmt, &s.default_pixel());
    let src = s.content_bounds();
    let dst = scaled_rect(src, sx, sy);
    if dst.is_empty() {
        return out;
    }
    let n = fmt.channels();
    let alpha = fmt.alpha;
    let hx = taps(dst.x0, dst.x1, sx, filter);
    let vy = taps(dst.y0, dst.y1, sy, filter);
    let x_lo = hx.iter().map(|t| t.start).min().unwrap_or(0);
    let x_hi = hx.iter().map(|t| t.start + t.weights.len() as i32).max().unwrap_or(0);
    let dw = dst.width() as usize;
    const BAND: usize = 64;
    let bands: Vec<(usize, usize)> = (0..vy.len()).step_by(BAND).map(|b| (b, (b + BAND).min(vy.len()))).collect();
    let run = |&(b0, b1): &(usize, usize)| -> (Rect, Vec<f32>) {
        let y_lo = vy[b0..b1].iter().map(|t| t.start).min().unwrap_or(0);
        let y_hi = vy[b0..b1].iter().map(|t| t.start + t.weights.len() as i32).max().unwrap_or(0);
        let read = Rect::new(x_lo, y_lo, x_hi, y_hi);
        let mut rows = s.read_region(read);
        premultiply(&mut rows, n, alpha);
        let rw = read.width() as usize;
        // Horizontal pass: every source row in the band → dw samples.
        let nrows = read.height() as usize;
        let mut tmp = vec![0.0f32; nrows * dw * n];
        for r in 0..nrows {
            let row = &rows[r * rw * n..(r + 1) * rw * n];
            for (ox, t) in hx.iter().enumerate() {
                let d = &mut tmp[(r * dw + ox) * n..(r * dw + ox + 1) * n];
                for (k, w) in t.weights.iter().enumerate() {
                    let sx_ = (t.start + k as i32 - x_lo) as usize;
                    let sp = &row[sx_ * n..(sx_ + 1) * n];
                    for c in 0..n {
                        d[c] += sp[c] * w;
                    }
                }
            }
        }
        // Vertical pass.
        let mut res = vec![0.0f32; (b1 - b0) * dw * n];
        for (oy, t) in vy[b0..b1].iter().enumerate() {
            let d = &mut res[oy * dw * n..(oy + 1) * dw * n];
            for (k, w) in t.weights.iter().enumerate() {
                let r = (t.start + k as i32 - y_lo) as usize;
                let sp = &tmp[r * dw * n..(r + 1) * dw * n];
                for (dv, sv) in d.iter_mut().zip(sp) {
                    *dv += sv * w;
                }
            }
        }
        if alpha {
            for px in res.chunks_exact_mut(n) {
                px[n - 1] = px[n - 1].clamp(0.0, 1.0);
            }
        }
        unpremultiply(&mut res, n, alpha);
        (Rect::new(dst.x0, dst.y0 + b0 as i32, dst.x1, dst.y0 + b1 as i32), res)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let parts: Vec<(Rect, Vec<f32>)> = {
        use rayon::prelude::*;
        bands.par_iter().map(run).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let parts: Vec<(Rect, Vec<f32>)> = bands.iter().map(run).collect();
    for (r, d) in parts {
        out.write_region(r, &d);
    }
    if filter == Resample::PreserveDetails && (sx > 1.0 || sy > 1.0) {
        let p = crate::FilterParams::UnsharpMask { amount: 30.0, radius: 0.6 * sx.max(sy) as f32, threshold: 0.0 };
        let area = crate::output_area(&p, out.content_bounds(), dst, None);
        out = crate::apply(&out, &p, area, dst, None);
    }
    out.prune();
    out
}

/// Moves a surface by whole pixels (default pixel kept).
pub fn translate_surface(s: &Surface, dx: i32, dy: i32) -> Surface {
    let mut out = Surface::with_default(s.format(), &s.default_pixel());
    let r = s.content_bounds();
    if !r.is_empty() {
        out.write_interleaved(r.translate(dx, dy), &s.to_interleaved(r));
    }
    out
}

/// Keeps only the pixels inside `keep` (others become the default pixel).
pub fn crop_surface(s: &Surface, keep: Rect) -> Surface {
    let mut out = Surface::with_default(s.format(), &s.default_pixel());
    let r = s.content_bounds().intersect(&keep);
    if !r.is_empty() {
        out.write_interleaved(r, &s.to_interleaved(r));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, PixelFormat, SampleType};

    fn ramp(sample: SampleType) -> Surface {
        let mut s = Surface::new(PixelFormat::new(ColorMode::Rgb, sample, true));
        let r = Rect::new(0, 0, 64, 32);
        let v: Vec<f32> = (0..32).flat_map(|_| (0..64).flat_map(|x| [x as f32 / 63.0, 0.5, 1.0 - x as f32 / 63.0, 1.0])).collect();
        s.write_region(r, &v);
        s
    }

    #[test]
    fn identity_scale_is_lossless() {
        for f in [Resample::Nearest, Resample::Bilinear, Resample::Bicubic, Resample::Lanczos] {
            let s = ramp(SampleType::U16);
            let o = resize_surface(&s, 1.0, 1.0, f);
            let r = Rect::new(0, 0, 64, 32);
            let (a, b) = (s.read_region(r), o.read_region(r));
            let d = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
            assert!(d < 1e-4, "{f:?} {d}");
        }
    }

    #[test]
    fn half_and_double_sizes() {
        for f in [Resample::Nearest, Resample::Bilinear, Resample::Bicubic, Resample::Lanczos, Resample::PreserveDetails] {
            let s = ramp(SampleType::F32);
            let half = resize_surface(&s, 0.5, 0.5, f);
            assert_eq!(half.content_bounds(), Rect::new(0, 0, 32, 16), "{f:?}");
            let p = half.pixel(16, 8);
            assert!((p[1] - 0.5).abs() < 0.02 && (p[3] - 1.0).abs() < 1e-3, "{f:?} {p:?}");
            let dbl = resize_surface(&s, 2.0, 2.0, f);
            assert_eq!(dbl.content_bounds(), Rect::new(0, 0, 128, 64), "{f:?}");
            // Mid-gray channel and the ramp's monotonicity are preserved.
            let row: Vec<f32> = (8..120).map(|x| dbl.pixel(x, 30)[0]).collect();
            assert!(row.windows(2).all(|w| w[1] >= w[0] - 0.02), "{f:?}");
        }
    }

    #[test]
    fn downscale_averages_checkerboard() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) % 2) as f32;
                s.write_pixel(x, y, &[v, v, v, 1.0]);
            }
        }
        let o = resize_surface(&s, 0.25, 0.25, Resample::Bicubic);
        let p = o.pixel(8, 8);
        assert!((p[0] - 0.5).abs() < 0.05, "{p:?}");
    }

    #[test]
    fn transparent_edges_do_not_darken() {
        let mut s = Surface::new(PixelFormat::RGBA32F);
        s.fill_rect(Rect::new(10, 10, 20, 20), &[1.0, 0.0, 0.0, 1.0]);
        let o = resize_surface(&s, 1.7, 1.7, Resample::Lanczos);
        let edge = o.pixel(17, 25);
        assert!(edge[3] > 0.0 && (edge[0] - 1.0).abs() < 0.05 && edge[1].abs() < 0.05, "{edge:?}");
    }

    #[test]
    fn masks_keep_default_pixel() {
        let fmt = PixelFormat::GRAY8;
        let mut m = Surface::with_default(fmt, &[1.0]);
        m.fill_rect(Rect::new(0, 0, 10, 10), &[0.0]);
        let o = resize_surface(&m, 2.0, 2.0, Resample::Bilinear);
        assert_eq!(o.default_pixel(), vec![1.0]);
        assert!(o.pixel(5, 5)[0] < 0.01);
    }

    #[test]
    fn translate_and_crop() {
        let s = ramp(SampleType::U8);
        let t = translate_surface(&s, 5, -3);
        assert_eq!(t.pixel(15, 7), s.pixel(10, 10));
        let c = crop_surface(&s, Rect::new(0, 0, 10, 10));
        assert_eq!(c.content_bounds(), Rect::new(0, 0, 10, 10));
    }
}

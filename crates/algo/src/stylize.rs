//! Mosaic, Emboss, Find Edges, Solarize/Invert (per pixel), Desaturate.

use photocraft_color::PixelFormat;
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba, to_rgba};

use crate::Ctx;
use crate::image::Image;

fn fmt(ctx: &Ctx) -> PixelFormat {
    PixelFormat::new(ctx.mode, photocraft_color::SampleType::F32, ctx.alpha)
}

fn luma(ctx: &Ctx, px: &[f32]) -> f32 {
    let c = to_rgba(&fmt(ctx), px);
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// Replaces the colour with gray `g` in the pixel's own model (alpha kept).
fn set_gray(ctx: &Ctx, px: &mut [f32], g: f32) {
    let a = if ctx.alpha { px[px.len() - 1] } else { 1.0 };
    let v = from_rgba(&fmt(ctx), [g, g, g, a]);
    px.copy_from_slice(&v[..px.len()]);
}

/// Colour channels through `f` (alpha untouched).
pub(crate) fn per_pixel(src: &Image, out: Rect, ctx: &Ctx, f: impl Fn(f32) -> f32) -> Vec<f32> {
    let n = src.ch;
    let cc = if ctx.alpha { n - 1 } else { n };
    let mut res = src.crop(out);
    for px in res.chunks_exact_mut(n) {
        for v in px.iter_mut().take(cc) {
            *v = f(*v);
        }
    }
    res
}

pub(crate) fn desaturate(src: &Image, out: Rect, ctx: &Ctx) -> Vec<f32> {
    let n = src.ch;
    let mut res = src.crop(out);
    for px in res.chunks_exact_mut(n) {
        let g = luma(ctx, px);
        set_gray(ctx, px, g);
    }
    res
}

/// Mosaic cells anchored at the bounds' origin; each cell becomes its
/// (premultiplied) average over the part inside the bounds.
pub(crate) fn mosaic(src: &Image, out: Rect, ctx: &Ctx, cell: f32) -> Vec<f32> {
    let n = src.ch;
    let cs = cell.max(1.0).round() as i32;
    let b = ctx.bounds;
    let mut res = Vec::with_capacity(out.width() as usize * out.height() as usize * n);
    let mut cache: Option<(i32, i32, Vec<f32>)> = None;
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            let (cx, cy) = ((x - b.x0).div_euclid(cs), (y - b.y0).div_euclid(cs));
            if cache.as_ref().is_none_or(|(a, bb, _)| (*a, *bb) != (cx, cy)) {
                let r = Rect::new(b.x0 + cx * cs, b.y0 + cy * cs, b.x0 + (cx + 1) * cs, b.y0 + (cy + 1) * cs).intersect(&b);
                let mut acc = vec![0.0f32; n];
                let mut count = 0.0;
                for yy in r.y0..r.y1 {
                    for xx in r.x0..r.x1 {
                        let a = if ctx.alpha { src.get(xx, yy, n - 1) } else { 1.0 };
                        for (c, v) in acc.iter_mut().enumerate() {
                            let s = src.get(xx, yy, c);
                            *v += if ctx.alpha && c < n - 1 { s * a } else { s };
                        }
                        count += 1.0;
                    }
                }
                if count > 0.0 {
                    for v in acc.iter_mut() {
                        *v /= count;
                    }
                }
                if ctx.alpha {
                    let a = acc[n - 1];
                    for v in acc.iter_mut().take(n - 1) {
                        *v = if a > 1e-7 { *v / a } else { 0.0 };
                    }
                }
                cache = Some((cx, cy, acc));
            }
            res.extend_from_slice(&cache.as_ref().map(|c| c.2.clone()).unwrap_or_default());
        }
    }
    res
}

/// Emboss: gray relief from luminance differences along the light angle.
pub(crate) fn emboss(src: &Image, out: Rect, ctx: &Ctx, angle: f32, height: f32, amount: f32) -> Vec<f32> {
    let n = src.ch;
    let (s, c) = angle.to_radians().sin_cos();
    let h = height.max(1.0);
    let (dx, dy) = ((c * h).round() as i32, (-s * h).round() as i32);
    let k = amount / 100.0;
    let mut res = src.crop(out);
    let mut tmp = vec![0.0f32; n];
    let lum_at = |x: i32, y: i32, tmp: &mut Vec<f32>| {
        for (ci, t) in tmp.iter_mut().enumerate() {
            *t = src.get(x, y, ci);
        }
        luma(ctx, tmp)
    };
    let w = out.width() as usize;
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let (x, y) = (out.x0 + (i % w) as i32, out.y0 + (i / w) as i32);
        let g = 0.5 + k * (lum_at(x + dx, y + dy, &mut tmp) - lum_at(x - dx, y - dy, &mut tmp));
        set_gray(ctx, px, g.clamp(0.0, 1.0));
    }
    res
}

/// Find Edges: Sobel magnitude per colour channel, dark edges on white.
pub(crate) fn find_edges(src: &Image, out: Rect, ctx: &Ctx) -> Vec<f32> {
    let n = src.ch;
    let cc = if ctx.alpha { n - 1 } else { n };
    let mut res = src.crop(out);
    let w = out.width() as usize;
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let (x, y) = (out.x0 + (i % w) as i32, out.y0 + (i / w) as i32);
        for (c, v) in px.iter_mut().enumerate().take(cc) {
            let g = |dx: i32, dy: i32| src.get(x + dx, y + dy, c);
            let gx = (g(1, -1) + 2.0 * g(1, 0) + g(1, 1)) - (g(-1, -1) + 2.0 * g(-1, 0) + g(-1, 1));
            let gy = (g(-1, 1) + 2.0 * g(0, 1) + g(1, 1)) - (g(-1, -1) + 2.0 * g(0, -1) + g(1, -1));
            *v = (1.0 - (gx * gx + gy * gy).sqrt() / 4.0).clamp(0.0, 1.0);
        }
    }
    res
}

//! Free Transform: projective warps of surfaces.
//!
//! A transform is described by where the four corners of a source rectangle land (a quad). Scale,
//! rotate, skew and flips are affine special cases; Distort and Perspective use the full
//! homography. Warping is an inverse mapping with premultiplied-alpha sampling (nearest, bilinear
//! or Catmull-Rom bicubic), processed per destination tile (in parallel on native) so memory stays
//! bounded by the tile working set. Large reductions are pre-filtered with a proper resize first,
//! so shrinking a layer doesn't alias.

use photocraft_color::PixelFormat;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

use crate::resample::{Resample, resize_surface};

/// 3×3 projective matrix, row-major: `[x', y', w'] = H · [x, y, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    pub const IDENTITY: Homography = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        ((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w)
    }

    pub fn mul(&self, o: &Homography) -> Homography {
        let (a, b) = (&self.0, &o.0);
        let mut r = [0.0; 9];
        for i in 0..3 {
            for j in 0..3 {
                r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
            }
        }
        Homography(r)
    }

    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.0;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Homography(c.map(|v| v / det)))
    }

    /// Unit square → quad (corners in order: (0,0), (1,0), (1,1), (0,1)).
    fn square_to_quad(q: [[f64; 2]; 4]) -> Option<Homography> {
        let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = q;
        let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
            // Affine.
            return Some(Homography([x1 - x0, x3 - x0, x0, y1 - y0, y3 - y0, y0, 0.0, 0.0, 1.0]));
        }
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-12 {
            return None;
        }
        let g = (sx * dy2 - dx2 * sy) / den;
        let h = (dx1 * sy - sx * dy1) / den;
        Some(Homography([x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h, 1.0]))
    }

    /// The transform taking rectangle `r` (x0, y0, x1, y1) onto `quad` (corners clockwise from
    /// top-left). `None` for degenerate quads.
    pub fn rect_to_quad(r: [f64; 4], quad: [[f64; 2]; 4]) -> Option<Homography> {
        let (w, h) = (r[2] - r[0], r[3] - r[1]);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let to_unit = Homography([1.0 / w, 0.0, -r[0] / w, 0.0, 1.0 / h, -r[1] / h, 0.0, 0.0, 1.0]);
        Some(Self::square_to_quad(quad)?.mul(&to_unit))
    }

    /// Approximate linear scale of the mapping near `(x, y)` (sqrt of the Jacobian determinant).
    pub fn local_scale(&self, x: f64, y: f64) -> f64 {
        let e = 0.5;
        let (a, b) = (self.apply(x - e, y), self.apply(x + e, y));
        let (c, d) = (self.apply(x, y - e), self.apply(x, y + e));
        let (jx, jy) = ((b.0 - a.0, b.1 - a.1), (d.0 - c.0, d.1 - c.1));
        (jx.0 * jy.1 - jx.1 * jy.0).abs().sqrt()
    }
}

/// Resampling used by transforms (Photoshop's interpolation menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Interp {
    Nearest,
    Bilinear,
    #[default]
    Bicubic,
}

impl Interp {
    pub fn parse(s: &str) -> Self {
        match s {
            "nearest" | "nearestNeighbor" => Interp::Nearest,
            "bilinear" => Interp::Bilinear,
            _ => Interp::Bicubic,
        }
    }
}

fn catmull_rom(t: f64) -> [f64; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2]
}

/// Warp `src` (its content inside `src_rect`) by `h` (source → destination document space).
/// The result has the same format as `src`, with alpha added if `src` had none; pixels outside
/// the warped quad are transparent.
pub fn warp_surface(src: &Surface, src_rect: Rect, h: &Homography, interp: Interp) -> Surface {
    let mut fmt = src.format();
    let converted;
    let mut src = if fmt.alpha {
        src
    } else {
        fmt = PixelFormat::new(fmt.mode, fmt.sample, true);
        converted = src.convert(fmt);
        &converted
    };
    let mut out = Surface::new(fmt);
    if src_rect.is_empty() {
        return out;
    }
    let mut h = *h;
    let mut src_rect = src_rect;
    // Pre-reduce for large downscales (bicubic alone aliases below ~50%).
    let (cx, cy) = ((src_rect.x0 + src_rect.x1) as f64 / 2.0, (src_rect.y0 + src_rect.y1) as f64 / 2.0);
    let scale = h.local_scale(cx, cy);
    let reduced;
    if interp != Interp::Nearest && scale < 0.5 && scale > 0.0 {
        let f = 2f64.powf(scale.log2().ceil()).min(1.0); // remaining scale lands in [0.5, 1)
        reduced = resize_surface(src, f, f, Resample::Bicubic);
        src = &reduced;
        h = h.mul(&Homography([1.0 / f, 0.0, 0.0, 0.0, 1.0 / f, 0.0, 0.0, 0.0, 1.0]));
        src_rect = crate::resample::scaled_rect(src_rect, f, f);
    }
    let Some(inv) = h.inverse() else { return out };
    // Destination bounds: the warped corners (plus a pixel for filter support).
    let corners = [(src_rect.x0, src_rect.y0), (src_rect.x1, src_rect.y0), (src_rect.x1, src_rect.y1), (src_rect.x0, src_rect.y1)]
        .map(|(x, y)| h.apply(x as f64, y as f64));
    if corners.iter().any(|c| !c.0.is_finite() || !c.1.is_finite()) {
        return out;
    }
    let lim = 1 << 20;
    let bx0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor().max(-(lim as f64)) as i32 - 1;
    let by0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor().max(-(lim as f64)) as i32 - 1;
    let bx1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil().min(lim as f64) as i32 + 1;
    let by1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil().min(lim as f64) as i32 + 1;
    let dst = Rect::new(bx0, by0, bx1, by1);
    let n = fmt.channels();
    let a = n - 1;
    let tiles: Vec<Rect> = dst.tiles().map(|tc| tc.rect().intersect(&dst)).filter(|r| !r.is_empty()).collect();
    let src_ref: &Surface = src;
    let work = |t: &Rect| -> Option<(Rect, Vec<f32>)> {
        // Source footprint of this tile (inverse-mapped corners), padded for the filter.
        let tc = [(t.x0, t.y0), (t.x1, t.y0), (t.x1, t.y1), (t.x0, t.y1)].map(|(x, y)| inv.apply(x as f64, y as f64));
        let fx0 = tc.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor() as i32 - 3;
        let fy0 = tc.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor() as i32 - 3;
        let fx1 = tc.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil() as i32 + 3;
        let fy1 = tc.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil() as i32 + 3;
        let foot = Rect::new(fx0, fy0, fx1, fy1).intersect(&src_rect);
        if foot.is_empty() || !src_ref.has_tiles_in(foot) {
            return None;
        }
        // Premultiplied source window.
        let mut px = src_ref.read_region(foot);
        for p in px.chunks_exact_mut(n) {
            let al = p[a];
            for v in &mut p[..a] {
                *v *= al;
            }
        }
        let fw = foot.width() as usize;
        let at = |x: i32, y: i32, c: usize| -> f64 {
            if x < foot.x0 || y < foot.y0 || x >= foot.x1 || y >= foot.y1 {
                0.0
            } else {
                px[((y - foot.y0) as usize * fw + (x - foot.x0) as usize) * n + c] as f64
            }
        };
        let w = t.width() as usize;
        let mut outp = vec![0.0f32; w * t.height() as usize * n];
        let mut any = false;
        let mut acc = [0.0f64; 8];
        for y in t.y0..t.y1 {
            for x in t.x0..t.x1 {
                let (u, v) = inv.apply(x as f64 + 0.5, y as f64 + 0.5);
                let (u, v) = (u - 0.5, v - 0.5);
                if u < src_rect.x0 as f64 - 1.0 || v < src_rect.y0 as f64 - 1.0 || u > src_rect.x1 as f64 || v > src_rect.y1 as f64 {
                    continue;
                }
                acc[..n].fill(0.0);
                match interp {
                    Interp::Nearest => {
                        let (ix, iy) = ((u + 0.5).floor() as i32, (v + 0.5).floor() as i32);
                        for (c, s) in acc.iter_mut().enumerate().take(n) {
                            *s = at(ix, iy, c);
                        }
                    }
                    Interp::Bilinear => {
                        let (ix, iy) = (u.floor() as i32, v.floor() as i32);
                        let (fx, fy) = (u - ix as f64, v - iy as f64);
                        for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                            for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                                for (c, s) in acc.iter_mut().enumerate().take(n) {
                                    *s += at(ix + dx, iy + dy, c) * wx * wy;
                                }
                            }
                        }
                    }
                    Interp::Bicubic => {
                        let (ix, iy) = (u.floor() as i32, v.floor() as i32);
                        let (wx, wy) = (catmull_rom(u - ix as f64), catmull_rom(v - iy as f64));
                        for (j, wyj) in wy.iter().enumerate() {
                            for (i, wxi) in wx.iter().enumerate() {
                                let k = wxi * wyj;
                                for (c, s) in acc.iter_mut().enumerate().take(n) {
                                    *s += at(ix - 1 + i as i32, iy - 1 + j as i32, c) * k;
                                }
                            }
                        }
                    }
                }
                let al = acc[a].clamp(0.0, 1.0);
                if al <= 0.0 {
                    continue;
                }
                any = true;
                let o = ((y - t.y0) as usize * w + (x - t.x0) as usize) * n;
                for c in 0..a {
                    outp[o + c] = (acc[c] / al).clamp(0.0, 1.0) as f32;
                }
                outp[o + a] = al as f32;
            }
        }
        any.then_some((*t, outp))
    };
    #[cfg(not(target_arch = "wasm32"))]
    let done: Vec<(Rect, Vec<f32>)> = {
        use rayon::prelude::*;
        tiles.par_iter().filter_map(work).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let done: Vec<(Rect, Vec<f32>)> = tiles.iter().filter_map(work).collect();
    for (r, v) in done {
        out.write_region(r, &v);
    }
    out.prune();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba() -> Surface {
        Surface::new(PixelFormat::RGBA8)
    }

    #[test]
    fn homography_maps_rect_corners_and_inverts() {
        let r = [10.0, 20.0, 110.0, 70.0];
        let q = [[0.0, 0.0], [200.0, 10.0], [180.0, 150.0], [-20.0, 120.0]];
        let h = Homography::rect_to_quad(r, q).unwrap();
        for (p, e) in [(10.0, 20.0), (110.0, 20.0), (110.0, 70.0), (10.0, 70.0)].iter().zip(q) {
            let (x, y) = h.apply(p.0, p.1);
            assert!((x - e[0]).abs() < 1e-9 && (y - e[1]).abs() < 1e-9, "{p:?} → {x},{y} vs {e:?}");
        }
        let inv = h.inverse().unwrap();
        let (x, y) = inv.apply(90.0, 60.0);
        let (bx, by) = h.apply(x, y);
        assert!((bx - 90.0).abs() < 1e-9 && (by - 60.0).abs() < 1e-9);
        assert!(Homography::rect_to_quad(r, [[0.0, 0.0]; 4]).is_none() || Homography::rect_to_quad(r, [[0.0, 0.0]; 4]).unwrap().inverse().is_none());
    }

    #[test]
    fn identity_and_translation_are_exact() {
        let mut s = rgba();
        s.fill_rect(Rect::new(4, 4, 20, 12), &[1.0, 0.5, 0.0, 1.0]);
        let r = s.content_bounds();
        for interp in [Interp::Nearest, Interp::Bilinear, Interp::Bicubic] {
            let h = Homography([1.0, 0.0, 7.0, 0.0, 1.0, -3.0, 0.0, 0.0, 1.0]);
            let o = warp_surface(&s, r, &h, interp);
            assert_eq!(o.content_bounds(), Rect::new(11, 1, 27, 9), "{interp:?}");
            assert_eq!(o.pixel(15, 5), vec![1.0, 128.0 / 255.0, 0.0, 1.0], "{interp:?}");
        }
    }

    #[test]
    fn scale_doubles_size_and_rotation_keeps_area() {
        let mut s = rgba();
        s.fill_rect(Rect::new(0, 0, 40, 20), &[0.2, 0.4, 0.6, 1.0]);
        let r = s.content_bounds();
        let up = warp_surface(
            &s,
            r,
            &Homography::rect_to_quad([0.0, 0.0, 40.0, 20.0], [[0.0, 0.0], [80.0, 0.0], [80.0, 40.0], [0.0, 40.0]]).unwrap(),
            Interp::Bicubic,
        );
        let b = up.content_bounds();
        assert!(b.width() >= 80 && b.width() <= 82 && b.height() >= 40 && b.height() <= 42, "{b:?}");
        // 90° rotation about (20, 10): a 40×20 box becomes 20×40.
        let q = [[30.0, -10.0], [30.0, 30.0], [10.0, 30.0], [10.0, -10.0]];
        let rot = warp_surface(&s, r, &Homography::rect_to_quad([0.0, 0.0, 40.0, 20.0], q).unwrap(), Interp::Bilinear);
        let b = rot.content_bounds();
        assert!(b.width().abs_diff(20) <= 2 && b.height().abs_diff(40) <= 2, "{b:?}");
        let p = rot.pixel(20, 10);
        assert!((p[0] - 0.2).abs() < 0.01 && p[3] > 0.99, "{p:?}");
    }

    #[test]
    fn large_downscale_is_prefiltered() {
        // A 1px checkerboard shrunk 8× must come out ~50% grey, not aliased black/white.
        let mut s = rgba();
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) % 2) as f32;
                s.fill_rect(Rect::new(x, y, x + 1, y + 1), &[v, v, v, 1.0]);
            }
        }
        let h = Homography::rect_to_quad([0.0, 0.0, 64.0, 64.0], [[0.0, 0.0], [8.0, 0.0], [8.0, 8.0], [0.0, 8.0]]).unwrap();
        let o = warp_surface(&s, s.content_bounds(), &h, Interp::Bicubic);
        let p = o.pixel(4, 4);
        assert!((p[0] - 0.5).abs() < 0.1, "{p:?}");
    }

    #[test]
    fn surfaces_without_alpha_gain_it() {
        let mut s = Surface::new(PixelFormat::new(photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8, false));
        s.fill_rect(Rect::new(0, 0, 10, 10), &[1.0, 0.0, 0.0]);
        let h = Homography([1.0, 0.0, 5.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let o = warp_surface(&s, Rect::new(0, 0, 10, 10), &h, Interp::Nearest);
        assert!(o.format().alpha);
        assert_eq!(o.pixel(2, 2)[3], 0.0);
        assert_eq!(o.pixel(7, 2), vec![1.0, 0.0, 0.0, 1.0]);
    }
}

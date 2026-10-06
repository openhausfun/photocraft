//! Selection outlines: boundary segments of a coverage mask (threshold 0.5), merged into long runs,
//! drawn as animated marching ants.
//!
//! The outline is traced at display resolution: `outline_scaled` samples every `step`-th pixel, so
//! a 36 MP selection at 12% zoom costs a ~0.5 MP scan instead of reading the whole mask.

use photocraft_geom::Rect;
use photocraft_raster::Surface;

/// A boundary segment in document pixel coordinates (axis-aligned).
pub type Segment = ([i32; 2], [i32; 2]);

/// Extract the boundary of `mask` (> 0.5 = selected) as merged horizontal and vertical segments.
pub fn outline(mask: &Surface, bounds: Rect) -> Vec<Segment> {
    outline_scaled(mask, bounds, 1)
}

/// Like [`outline`], sampling one pixel per `step`×`step` block (segments stay in document space).
pub fn outline_scaled(mask: &Surface, bounds: Rect, step: u32) -> Vec<Segment> {
    if bounds.is_empty() {
        return Vec::new();
    }
    let step = step.max(1) as i32;
    // Grid of block samples, padded by one cell so edges at the bounds are detected.
    let gx0 = bounds.x0.div_euclid(step) - 1;
    let gy0 = bounds.y0.div_euclid(step) - 1;
    let gx1 = (bounds.x1 + step - 1).div_euclid(step) + 1;
    let gy1 = (bounds.y1 + step - 1).div_euclid(step) + 1;
    let (w, h) = ((gx1 - gx0) as usize, (gy1 - gy0) as usize);
    let mut grid = vec![false; w * h];
    let half = step / 2;
    let sample_row = |gy: usize, row: &mut [bool]| {
        let y = (gy0 + gy as i32) * step + half;
        if y < bounds.y0 || y >= bounds.y1 {
            return;
        }
        let mut vals = vec![0.0f32; row.len()];
        mask.sample_row_strided(y, gx0 * step + half, step, 0, &mut vals);
        for (gx, (cell, v)) in row.iter_mut().zip(vals).enumerate() {
            let x = (gx0 + gx as i32) * step + half;
            *cell = x >= bounds.x0 && x < bounds.x1 && v > 0.5;
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        grid.par_chunks_mut(w).enumerate().for_each(|(gy, row)| sample_row(gy, row));
    }
    #[cfg(target_arch = "wasm32")]
    grid.chunks_mut(w).enumerate().for_each(|(gy, row)| sample_row(gy, row));
    let inside = |x: usize, y: usize| grid[y * w + x];
    // Grid → document coordinates, clamped so coarse cells never draw outside the selection bounds.
    let r = Rect::new(gx0 * step, gy0 * step, gx1 * step, gy1 * step);
    let (cx, cy) = (|v: i32| v.clamp(bounds.x0, bounds.x1), |v: i32| v.clamp(bounds.y0, bounds.y1));
    let mut segs = Vec::new();
    // Horizontal edges between rows y-1 and y, merged along x.
    for y in 1..h {
        let mut run: Option<usize> = None;
        for x in 0..=w {
            let edge = x < w && inside(x, y) != inside(x, y - 1);
            match (edge, run) {
                (true, None) => run = Some(x),
                (false, Some(x0)) => {
                    let yy = cy(r.y0 + y as i32 * step);
                    segs.push(([cx(r.x0 + x0 as i32 * step), yy], [cx(r.x0 + x as i32 * step), yy]));
                    run = None;
                }
                _ => {}
            }
        }
    }
    // Vertical edges between columns x-1 and x, merged along y.
    for x in 1..w {
        let mut run: Option<usize> = None;
        for y in 0..=h {
            let edge = y < h && inside(x, y) != inside(x - 1, y);
            match (edge, run) {
                (true, None) => run = Some(y),
                (false, Some(y0)) => {
                    let xx = cx(r.x0 + x as i32 * step);
                    segs.push(([xx, cy(r.y0 + y0 as i32 * step)], [xx, cy(r.y0 + y as i32 * step)]));
                    run = None;
                }
                _ => {}
            }
        }
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;

    #[test]
    fn rectangle_has_four_edges() {
        let mut m = Surface::new(PixelFormat::GRAY8);
        m.fill_rect(Rect::new(2, 3, 10, 7), &[1.0]);
        let s = outline(&m, m.content_bounds());
        assert_eq!(s.len(), 4, "{s:?}");
        assert!(s.contains(&([2, 3], [10, 3])));
        assert!(s.contains(&([2, 7], [10, 7])));
        assert!(s.contains(&([2, 3], [2, 7])));
        assert!(s.contains(&([10, 3], [10, 7])));
    }

    #[test]
    fn hole_adds_inner_edges() {
        let mut m = Surface::new(PixelFormat::GRAY8);
        m.fill_rect(Rect::new(0, 0, 10, 10), &[1.0]);
        m.fill_rect(Rect::new(4, 4, 6, 6), &[0.0]);
        assert_eq!(outline(&m, m.content_bounds()).len(), 8);
    }

    #[test]
    fn scaled_outline_approximates_full_resolution() {
        let mut m = Surface::new(PixelFormat::GRAY8);
        m.fill_rect(Rect::new(8, 8, 72, 40), &[1.0]);
        let s = outline_scaled(&m, m.content_bounds(), 4);
        assert_eq!(s.len(), 4, "{s:?}");
        assert!(s.contains(&([8, 8], [72, 8])));
        assert!(s.contains(&([72, 8], [72, 40])));
        // Unaligned bounds are clamped, never drawn outside the selection.
        let mut m = Surface::new(PixelFormat::GRAY8);
        m.fill_rect(Rect::new(3, 5, 30, 21), &[1.0]);
        for seg in outline_scaled(&m, m.content_bounds(), 8) {
            for p in [seg.0, seg.1] {
                assert!(p[0] >= 3 && p[0] <= 30 && p[1] >= 5 && p[1] <= 21, "{seg:?}");
            }
        }
    }

    #[test]
    fn empty_mask_has_no_outline() {
        let m = Surface::new(PixelFormat::GRAY8);
        assert!(outline(&m, m.content_bounds()).is_empty());
    }
}

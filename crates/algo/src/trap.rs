//! Image › Trap (prepress). Trapping spreads each ink slightly into neighbouring inked areas so
//! that, if the press misregisters the plates, the colours still overlap at an edge instead of
//! leaving a white gap. We spread every ink by `width` px (grayscale dilation) but only *under*
//! already-inked pixels, so internal colour edges get a trap band while the object-vs-paper edge is
//! left alone. Clean-room approximation of Photoshop's Trap, which works on a flattened CMYK image.
//!
//! Operates on an interleaved buffer of `ch` channels per pixel (CMYK = inks 0..4, plus alpha).

use crate::photo_util::par_rows;

/// Separable grayscale dilation (max filter) of one interleaved channel, radius `r` px.
fn dilate_channel(src: &[f32], w: usize, h: usize, ch: usize, c: usize, r: usize) -> Vec<f32> {
    let get = |buf: &[f32], x: usize, y: usize| buf[(y * w + x) * ch + c];
    // Horizontal pass.
    let mut tmp = vec![0.0f32; w * h];
    par_rows(&mut tmp, w, 1, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            let mut m = 0.0f32;
            for xx in lo..=hi {
                m = m.max(get(src, xx, y));
            }
            *o = m;
        }
    });
    // Vertical pass over tmp.
    let mut out = vec![0.0f32; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        for (x, o) in row.iter_mut().enumerate() {
            let mut m = 0.0f32;
            for yy in lo..=hi {
                m = m.max(tmp[yy * w + x]);
            }
            *o = m;
        }
    });
    out
}

/// Trap an interleaved CMYK(+alpha) buffer in place. `width` is the trap in px (>=1). Inks 0..4 are
/// spread; any alpha channel (`ch > 4`) is untouched. A pixel keeps its original ink where it has
/// no ink at all (paper), so only internal colour edges gain a trap band.
pub fn trap(px: &mut [f32], w: usize, h: usize, ch: usize, width: usize) {
    if width == 0 || w == 0 || h == 0 || ch < 4 {
        return;
    }
    let inks = 4.min(ch);
    let dil: Vec<Vec<f32>> = (0..inks).map(|c| dilate_channel(px, w, h, ch, c, width)).collect();
    par_rows(px, w, ch, |y, row| {
        for (x, pixel) in row.chunks_exact_mut(ch).enumerate() {
            let i = y * w + x;
            // Printed where any ink is present.
            let inked = (0..inks).any(|c| pixel[c] > 1e-4);
            if !inked {
                continue;
            }
            for c in 0..inks {
                pixel[c] = pixel[c].max(dil[c][i]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two solid ink blocks meeting at x=4: cyan (C) on the left, magenta (M) on the right.
    fn two_blocks() -> (Vec<f32>, usize, usize) {
        let (w, h, ch) = (8usize, 4usize, 4usize);
        let mut px = vec![0.0f32; w * h * ch];
        for y in 0..h {
            for x in 0..w {
                let p = &mut px[(y * w + x) * ch..][..ch];
                if x < 4 {
                    p[0] = 1.0; // cyan
                } else {
                    p[1] = 1.0; // magenta
                }
            }
        }
        (px, w, h)
    }

    #[test]
    fn traps_the_internal_colour_edge() {
        let (mut px, w, h) = two_blocks();
        let ch = 4;
        trap(&mut px, w, h, ch, 1);
        // At the seam, cyan and magenta now overlap (both inks > 0) in a 1px band on each side.
        let at = |x: usize, y: usize, c: usize| px[(y * w + x) * ch + c];
        assert!(at(3, 0, 1) > 0.5, "magenta spread left into the cyan edge");
        assert!(at(4, 0, 0) > 0.5, "cyan spread right into the magenta edge");
        // Deep inside each block is unchanged (single ink).
        assert_eq!(at(0, 0, 1), 0.0, "far cyan has no magenta");
        assert_eq!(at(7, 0, 0), 0.0, "far magenta has no cyan");
    }

    #[test]
    fn leaves_paper_untouched() {
        let (w, h, ch) = (6usize, 3usize, 4usize);
        let mut px = vec![0.0f32; w * h * ch];
        // One cyan pixel in the middle, rest paper.
        px[(w + 3) * ch] = 1.0;
        let before = px.clone();
        trap(&mut px, w, h, ch, 1);
        // Paper pixels (no ink) stay paper — no ink bleeds into white.
        for i in 0..w * h {
            if before[i * ch] == 0.0 {
                assert_eq!(px[i * ch], 0.0, "paper pixel {i} gained cyan");
            }
        }
    }

    #[test]
    fn width_zero_is_identity() {
        let (mut px, w, h) = two_blocks();
        let orig = px.clone();
        trap(&mut px, w, h, 4, 0);
        assert_eq!(px, orig);
    }
}

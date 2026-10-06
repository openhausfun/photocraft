//! Content-aware scaling by seam carving (Edit › Content-Aware Scale).
//!
//! S. Avidan, A. Shamir, *Seam Carving for Content-Aware Image Resizing*, SIGGRAPH 2007:
//! a seam is an 8-connected path of one pixel per row (vertical seam) found by dynamic
//! programming over a gradient-magnitude energy map. Shrinking removes the lowest-energy seams one
//! at a time; enlarging finds the `k` lowest seams on a shrinking copy and duplicates them
//! (averaging each with its right neighbour) so the same seam is not stretched repeatedly.
//! Heights are handled by transposing.
//!
//! A protect map (`0..=1`, e.g. an alpha channel or detected skin tones) adds a large energy so
//! seams avoid protected pixels. Buffers are interleaved `w × h × ch` normalised floats of any
//! colour model and depth (alpha is carried like any other channel). Deterministic.

use photocraft_raster::{Cancelled, Interrupt};

/// Energy added per unit of protection: far above any gradient magnitude.
const PROTECT_ENERGY: f32 = 1.0e4;

/// Gradient-magnitude energy (the paper's e1, `|∂x| + |∂y|` summed over channels). Forward and
/// backward differences are both counted so one-pixel lines (whose central difference is zero)
/// carry energy too.
pub fn energy(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    let mut e = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            let at = |xx: usize, yy: usize, c: usize| img[(yy * w + xx) * ch + c];
            let mut s = 0.0;
            for c in 0..ch {
                let v = at(x, y, c);
                s += (at(xr, y, c) - v).abs() + (v - at(xl, y, c)).abs();
                s += (at(x, yd, c) - v).abs() + (v - at(x, yu, c)).abs();
            }
            e[y * w + x] = s * 0.5;
        }
    }
    e
}

/// The lowest-energy vertical seam: one x per row.
pub fn find_seam(w: usize, h: usize, energy: &[f32]) -> Vec<usize> {
    let mut cost = energy[..w].to_vec();
    let mut from = vec![0u8; w * h]; // 0 = up-left, 1 = up, 2 = up-right
    let mut next = vec![0.0f32; w];
    for y in 1..h {
        for x in 0..w {
            let mut best = (cost[x], 1u8);
            if x > 0 && cost[x - 1] < best.0 {
                best = (cost[x - 1], 0);
            }
            if x + 1 < w && cost[x + 1] < best.0 {
                best = (cost[x + 1], 2);
            }
            next[x] = best.0 + energy[y * w + x];
            from[y * w + x] = best.1;
        }
        std::mem::swap(&mut cost, &mut next);
    }
    let mut x = (0..w).min_by(|a, b| cost[*a].total_cmp(&cost[*b])).unwrap_or(0);
    let mut seam = vec![0usize; h];
    for y in (0..h).rev() {
        seam[y] = x;
        if y > 0 {
            x = match from[y * w + x] {
                0 => x - 1,
                2 => x + 1,
                _ => x,
            };
        }
    }
    seam
}

fn remove_seam(w: usize, h: usize, ch: usize, img: &[f32], seam: &[usize]) -> Vec<f32> {
    let mut out = Vec::with_capacity((w - 1) * h * ch);
    for (y, &sx) in seam.iter().enumerate().take(h) {
        let row = &img[y * w * ch..(y + 1) * w * ch];
        out.extend_from_slice(&row[..sx * ch]);
        out.extend_from_slice(&row[(sx + 1) * ch..]);
    }
    out
}

fn remove_seam_1(w: usize, h: usize, v: &[f32], seam: &[usize]) -> Vec<f32> {
    remove_seam(w, h, 1, v, seam)
}

fn transpose(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; img.len()];
    for y in 0..h {
        for x in 0..w {
            let (s, d) = ((y * w + x) * ch, (x * h + y) * ch);
            out[d..d + ch].copy_from_slice(&img[s..s + ch]);
        }
    }
    out
}

/// Change the width of `img` to `new_w` by removing or duplicating vertical seams.
pub fn carve_width(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize) -> Vec<f32> {
    // Never cancelled; the fallback is unreachable.
    carve_width_with(w, h, ch, img, protect, new_w, &Interrupt::NONE).unwrap_or_else(|_| img.to_vec())
}

/// [`carve_width`] that checks `ctl` before every seam and reports progress per seam.
pub fn carve_width_with(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, ctl: &Interrupt) -> Result<Vec<f32>, Cancelled> {
    let new_w = new_w.max(1);
    if new_w == w || w == 0 || h == 0 {
        return Ok(img.to_vec());
    }
    let seams = new_w.abs_diff(w).max(1) as f32;
    let protect_e = |e: &mut [f32], p: Option<&Vec<f32>>| {
        if let Some(p) = p {
            for (e, p) in e.iter_mut().zip(p) {
                *e += p.clamp(0.0, 1.0) * PROTECT_ENERGY;
            }
        }
    };
    if new_w < w {
        let (mut cur, mut cw) = (img.to_vec(), w);
        let mut prot = protect.map(<[f32]>::to_vec);
        while cw > new_w {
            ctl.check()?;
            ctl.progress((w - cw) as f32 / seams);
            let mut e = energy(cw, h, ch, &cur);
            protect_e(&mut e, prot.as_ref());
            let seam = find_seam(cw, h, &e);
            cur = remove_seam(cw, h, ch, &cur, &seam);
            prot = prot.map(|p| remove_seam_1(cw, h, &p, &seam));
            cw -= 1;
        }
        return Ok(cur);
    }
    // Enlarge in rounds of at most half the current width, each duplicating the k lowest seams.
    let (mut cur, mut cw) = (img.to_vec(), w);
    let mut prot = protect.map(<[f32]>::to_vec);
    while cw < new_w {
        let k = (new_w - cw).min((cw / 2).max(1));
        // Find k seams on a shrinking copy, mapping each back to original columns.
        let mut idx: Vec<Vec<usize>> = (0..h).map(|_| (0..cw).collect()).collect();
        let (mut tmp, mut tw) = (cur.clone(), cw);
        let mut tprot = prot.clone();
        let mut dup: Vec<Vec<usize>> = vec![Vec::with_capacity(k); h];
        for _ in 0..k {
            if tw <= 1 {
                break;
            }
            ctl.check()?;
            ctl.progress((cw + (cw - tw)).saturating_sub(w) as f32 / seams);
            let mut e = energy(tw, h, ch, &tmp);
            protect_e(&mut e, tprot.as_ref());
            let seam = find_seam(tw, h, &e);
            for (y, &sx) in seam.iter().enumerate() {
                dup[y].push(idx[y].remove(sx));
            }
            tmp = remove_seam(tw, h, ch, &tmp, &seam);
            tprot = tprot.map(|p| remove_seam_1(tw, h, &p, &seam));
            tw -= 1;
        }
        let added = dup[0].len();
        if added == 0 {
            break;
        }
        let nw = cw + added;
        let mut out = Vec::with_capacity(nw * h * ch);
        let mut nprot = prot.as_ref().map(|_| Vec::with_capacity(nw * h));
        for (y, d) in dup.iter_mut().enumerate() {
            d.sort_unstable();
            let row = &cur[y * cw * ch..(y + 1) * cw * ch];
            let mut di = 0;
            for x in 0..cw {
                out.extend_from_slice(&row[x * ch..(x + 1) * ch]);
                if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                    np.push(p[y * cw + x]);
                }
                while di < d.len() && d[di] == x {
                    let xr = (x + 1).min(cw - 1);
                    for c in 0..ch {
                        out.push((row[x * ch + c] + row[xr * ch + c]) * 0.5);
                    }
                    if let (Some(np), Some(p)) = (nprot.as_mut(), prot.as_ref()) {
                        np.push(p[y * cw + x]);
                    }
                    di += 1;
                }
            }
        }
        cur = out;
        prot = nprot;
        cw = nw;
    }
    Ok(cur)
}

/// Content-aware resize of `img` to `new_w × new_h` (width first, then height by transposing).
pub fn carve(w: usize, h: usize, ch: usize, img: &[f32], protect: Option<&[f32]>, new_w: usize, new_h: usize) -> Vec<f32> {
    // Never cancelled; the fallback is unreachable.
    carve_with(w, h, ch, img, protect, new_w, new_h, &Interrupt::NONE).unwrap_or_else(|_| img.to_vec())
}

/// [`carve`] that checks `ctl` before every seam (a cancel takes effect within one seam) and
/// reports progress over both passes.
#[allow(clippy::too_many_arguments)]
pub fn carve_with(
    w: usize,
    h: usize,
    ch: usize,
    img: &[f32],
    protect: Option<&[f32]>,
    new_w: usize,
    new_h: usize,
    ctl: &Interrupt,
) -> Result<Vec<f32>, Cancelled> {
    let (new_w, new_h) = (new_w.max(1), new_h.max(1));
    // Split progress between the passes by their seam counts.
    let (sw, sh) = (new_w.abs_diff(w) as f32, new_h.abs_diff(h) as f32);
    let split = if sw + sh > 0.0 { sw / (sw + sh) } else { 1.0 };
    let cancel = || ctl.cancelled();
    let first = |f: f32| ctl.progress(f * split);
    let wide = carve_width_with(w, h, ch, img, protect, new_w, &Interrupt::new(&cancel, &first))?;
    if new_h == h {
        return Ok(wide);
    }
    // Protection follows the width change by plain resampling (it only steers seams).
    let prot_t = protect.map(|p| transpose(new_w, h, 1, &resize_bilinear(w, h, 1, p, new_w, h)));
    let t = transpose(new_w, h, ch, &wide);
    let second = |f: f32| ctl.progress(split + f * (1.0 - split));
    let tall = carve_width_with(h, new_w, ch, &t, prot_t.as_deref(), new_h, &Interrupt::new(&cancel, &second))?;
    Ok(transpose(new_h, new_w, ch, &tall))
}

/// Plain bilinear resize (the non-content-aware part of the Amount blend).
pub fn resize_bilinear(w: usize, h: usize, ch: usize, img: &[f32], nw: usize, nh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; nw * nh * ch];
    if w == 0 || h == 0 {
        return out;
    }
    for y in 0..nh {
        let fy = ((y as f32 + 0.5) * h as f32 / nh as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..nw {
            let fx = ((x as f32 + 0.5) * w as f32 / nw as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..ch {
                let p = |xx: usize, yy: usize| img[(yy * w + xx) * ch + c];
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
                let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
                out[(y * nw + x) * ch + c] = top + (bot - top) * ty;
            }
        }
    }
    out
}

/// Skin-tone protection map (1 = skin) for RGB(A) buffers: the explicit RGB rule of
/// J. Kovač, P. Peer, F. Solina, *Human Skin Colour Clustering for Face Detection*, EUROCON 2003.
pub fn skin_mask(w: usize, h: usize, ch: usize, img: &[f32]) -> Vec<f32> {
    let mut m = vec![0.0f32; w * h];
    if ch < 3 {
        return m;
    }
    for (i, v) in m.iter_mut().enumerate() {
        let p = &img[i * ch..i * ch + 3];
        let (r, g, b) = (p[0] * 255.0, p[1] * 255.0, p[2] * 255.0);
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if r > 95.0 && g > 40.0 && b > 20.0 && mx - mn > 15.0 && (r - g).abs() > 15.0 && r > g && r > b {
            *v = 1.0;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_carve_stops() {
        let (w, h) = (40, 20);
        let img = vec![0.5f32; w * h * 3];
        let yes = || true;
        assert_eq!(carve_with(w, h, 3, &img, None, 30, 15, &Interrupt::cancel_only(&yes)), Err(Cancelled));
        assert_eq!(carve_with(w, h, 3, &img, None, 30, 15, &Interrupt::NONE).unwrap(), carve(w, h, 3, &img, None, 30, 15));
    }

    /// A flat grey image with a salient vertical bar at x = 10..14 (red/blue columns).
    fn bar(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0.5f32; w * h * 3];
        for y in 0..h {
            for x in 10..14 {
                let c = if x % 2 == 0 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
                v[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        v
    }

    /// Columns that are pure red or pure blue top to bottom.
    fn red_columns(w: usize, h: usize, img: &[f32]) -> usize {
        let pure = |p: &[f32]| p[1] < 0.05 && ((p[0] > 0.95 && p[2] < 0.05) || (p[2] > 0.95 && p[0] < 0.05));
        (0..w).filter(|x| (0..h).all(|y| pure(&img[(y * w + x) * 3..(y * w + x) * 3 + 3]))).count()
    }

    #[test]
    fn seam_follows_low_energy() {
        let (w, h) = (5, 4);
        let mut e = vec![9.0f32; w * h];
        for y in 0..h {
            e[y * w + 2] = 0.0;
        }
        assert_eq!(find_seam(w, h, &e), vec![2; 4]);
    }

    #[test]
    fn shrinking_keeps_the_salient_bar() {
        let (w, h) = (40, 12);
        let img = bar(w, h);
        let out = carve(w, h, 3, &img, None, 24, h);
        assert_eq!(out.len(), 24 * h * 3);
        assert_eq!(red_columns(24, h, &out), 4, "the bar survives intact");
        // Plain resampling squashes it instead.
        let plain = resize_bilinear(w, h, 3, &img, 24, h);
        assert!(red_columns(24, h, &plain) < 4);
    }

    #[test]
    fn enlarging_duplicates_flat_areas() {
        let (w, h) = (30, 8);
        let img = bar(w, h);
        let out = carve(w, h, 3, &img, None, 50, h);
        assert_eq!(out.len(), 50 * h * 3);
        assert_eq!(red_columns(50, h, &out), 4);
    }

    #[test]
    fn height_and_protection() {
        let (w, h) = (20, 30);
        // Horizontal bar: carve height.
        let mut img = vec![0.2f32; w * h];
        for x in 0..w {
            img[12 * w + x] = 1.0;
        }
        let out = carve(w, h, 1, &img, None, w, 18);
        assert_eq!(out.len(), w * 18);
        assert_eq!((0..18).filter(|y| (0..w).all(|x| out[y * w + x] > 0.9)).count(), 1);
        // Protecting a flat region keeps it: the seams go elsewhere.
        let flat = vec![0.5f32; 16 * 4];
        let mut protect = vec![0.0f32; 16 * 4];
        for y in 0..4 {
            protect[y * 16 + 3] = 1.0;
        }
        let mut marked = flat.clone();
        for y in 0..4 {
            marked[y * 16 + 3] = 0.51;
        }
        let out = carve(16, 4, 1, &marked, Some(&protect), 8, 4);
        assert!((0..4).all(|y| out[y * 8..y * 8 + 8].iter().any(|v| (*v - 0.51).abs() < 1e-6)));
    }

    #[test]
    fn skin_rule() {
        let px = [0.86f32, 0.62, 0.5, 0.2, 0.4, 0.8];
        assert_eq!(skin_mask(2, 1, 3, &px), vec![1.0, 0.0]);
    }
}

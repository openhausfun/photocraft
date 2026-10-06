//! Selection algorithms on coverage masks (`w × h` row-major `f32` in
//! `0..=1` over an area): magic wand / flood regions, colour range,
//! modify (expand, contract, border, smooth, feather), anti-aliased lasso,
//! and combining with an existing selection.

use photocraft_color::PixelFormat;
use photocraft_geom::{Rect, TILE_SIZE, TileCoord};
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

use crate::photo_util::par_rows;

/// How a new selection combines with the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SelectionMode {
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl SelectionMode {
    /// Parses `replace|add|subtract|intersect` (unknown → replace).
    pub fn parse(s: &str) -> Self {
        match s {
            "add" => SelectionMode::Add,
            "subtract" => SelectionMode::Subtract,
            "intersect" => SelectionMode::Intersect,
            _ => SelectionMode::Replace,
        }
    }
}

/// Reads a selection surface over `area` (no selection → all zero).
pub fn mask_from_surface(s: Option<&Surface>, area: Rect) -> Vec<f32> {
    let n = area.width() as usize * area.height() as usize;
    match s {
        None => vec![0.0; n],
        // Selections are GRAY8: copy tile bytes through a table, tile rows in parallel.
        Some(s) if s.format() == PixelFormat::GRAY8 && !area.is_empty() => {
            let w = area.width() as usize;
            let lut: [f32; 256] = std::array::from_fn(|i| i as f32 / 255.0);
            let dp = s.default_pixel().first().copied().unwrap_or(0.0);
            let mut out = vec![0.0f32; n];
            par_rows(&mut out, w, 1, |yy, row| {
                let y = area.y0 + yy as i32;
                let mut x = area.x0;
                while x < area.x1 {
                    let tc = TileCoord::containing(x, y);
                    let x1 = (tc.rect().x1).min(area.x1);
                    let dst = row.get_mut((x - area.x0) as usize..(x1 - area.x0) as usize).unwrap_or_default();
                    let src = s.tile(tc).and_then(|t| {
                        let base = (y - tc.ty * TILE_SIZE) as usize * TILE_SIZE as usize + (x - tc.tx * TILE_SIZE) as usize;
                        t.bytes().get(base..base + dst.len())
                    });
                    match src {
                        Some(src) => dst.iter_mut().zip(src).for_each(|(d, b)| *d = lut[*b as usize]),
                        None => dst.fill(dp),
                    }
                    x = x1;
                }
            });
            out
        }
        Some(s) => {
            let mut v = Vec::new();
            s.read_region_into(area, &mut v);
            let ch = s.channels();
            if ch > 1 { v.chunks_exact(ch).map(|p| p[0]).collect() } else { v }
        }
    }
}

/// 8-bit selection value of a coverage (clamped and rounded, as GRAY8 surfaces store it; NaN → 0).
#[inline]
fn coverage8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Builds a GRAY8 selection surface from 8-bit coverage over `area`, allocating only the tiles
/// that hold something (as [`Surface::prune`] would leave them).
fn surface_from_coverage8(bytes: &[u8], area: Rect) -> Surface {
    let mut s = Surface::new(PixelFormat::GRAY8);
    let w = area.width() as usize;
    let ts = TILE_SIZE as usize;
    for tc in area.tiles() {
        let tr = tc.rect().intersect(&area);
        let (lx, span) = ((tr.x0 - area.x0) as usize, tr.width() as usize);
        let row = |y: i32| bytes.get((y - area.y0) as usize * w + lx..(y - area.y0) as usize * w + lx + span);
        if !(tr.y0..tr.y1).any(|y| row(y).is_some_and(|r| r.iter().any(|b| *b != 0))) {
            continue;
        }
        let data = s.tile_mut(tc).bytes_mut();
        for y in tr.y0..tr.y1 {
            let o = (y - tc.ty * TILE_SIZE) as usize * ts + (tr.x0 - tc.tx * TILE_SIZE) as usize;
            if let (Some(src), Some(dst)) = (row(y), data.get_mut(o..o + span)) {
                dst.copy_from_slice(src);
            }
        }
    }
    s
}

/// 8-bit coverage of `m` (missing samples are 0), computed in parallel.
fn to_coverage8(m: &[f32], area: Rect, f: impl Fn(usize, f32) -> f32 + Sync + Send) -> Vec<u8> {
    let w = area.width() as usize;
    let mut bytes = vec![0u8; w * area.height() as usize];
    par_rows(&mut bytes, w, 1, |y, row| {
        for (x, b) in row.iter_mut().enumerate() {
            let i = y * w + x;
            *b = coverage8(f(i, m.get(i).copied().unwrap_or(0.0)));
        }
    });
    bytes
}

/// Builds a GRAY8 selection surface from a mask over `area`.
pub fn mask_to_surface(m: &[f32], area: Rect) -> Surface {
    if area.is_empty() {
        return Surface::new(PixelFormat::GRAY8);
    }
    surface_from_coverage8(&to_coverage8(m, area, |_, v| v), area)
}

/// Combines `new` with `old` by `mode`. Returns `None` when nothing is selected.
pub fn combine(old: Option<&Surface>, new: &[f32], area: Rect, mode: SelectionMode) -> Option<Surface> {
    if area.is_empty() {
        return None;
    }
    // Quantize to the 8-bit grid selections are stored on, so combining a mask with itself is
    // exact. Below half an 8-bit step is nothing.
    let q = |v: f32| f32::from(coverage8(v)) / 255.0;
    let bytes = match mode {
        SelectionMode::Replace => to_coverage8(new, area, |_, v| v),
        _ => {
            let prev = mask_from_surface(old, area);
            let at = |i: usize| prev.get(i).copied().unwrap_or(0.0);
            match mode {
                SelectionMode::Add => to_coverage8(new, area, |i, v| at(i).max(q(v))),
                SelectionMode::Subtract => to_coverage8(new, area, |i, v| (at(i) - q(v)).max(0.0)),
                _ => to_coverage8(new, area, |i, v| at(i).min(q(v))),
            }
        }
    };
    if bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(surface_from_coverage8(&bytes, area))
}

fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    (0..4).all(|c| (a[c] - b[c]).abs() * 255.0 <= tol + 1e-3)
}

/// Pixels similar to the seed pixel (per-channel difference ≤ `tolerance`
/// levels, alpha included). `contiguous` limits to the 4-connected region
/// around the seed. `px` covers `area`.
pub fn magic_wand(px: &[[f32; 4]], area: Rect, seed: (i32, i32), tolerance: f32, contiguous: bool, anti_alias: bool) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut m = vec![0.0f32; w * h];
    if !area.contains(seed.0, seed.1) {
        return m;
    }
    let si = (seed.1 - area.y0) as usize * w + (seed.0 - area.x0) as usize;
    let target = px[si];
    if contiguous {
        let mut stack = vec![si];
        m[si] = 1.0;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if m[j] == 0.0 && close(px[j], target, tolerance) {
                    m[j] = 1.0;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
    } else {
        for (v, p) in m.iter_mut().zip(px) {
            if close(*p, target, tolerance) {
                *v = 1.0;
            }
        }
    }
    if anti_alias {
        antialias(&mut m, w, h);
    }
    m
}

/// Straight RGBA8 copy of `area` of a surface, read in parallel bands on native.
pub fn rgba8_image(s: &Surface, area: Rect) -> Vec<[u8; 4]> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut img = vec![[0u8; 4]; w * h];
    if w == 0 || h == 0 {
        return img;
    }
    let band = 256usize;
    let read = |bi: usize, chunk: &mut [[u8; 4]]| {
        let y0 = area.y0 + (bi * band) as i32;
        s.read_rgba8_into(Rect::new(area.x0, y0, area.x1, y0 + (chunk.len() / w) as i32), chunk);
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        img.par_chunks_mut(w * band).enumerate().for_each(|(bi, c)| read(bi, c));
    }
    #[cfg(target_arch = "wasm32")]
    img.chunks_mut(w * band).enumerate().for_each(|(bi, c)| read(bi, c));
    img
}

/// A selected region: 8-bit coverage over `bbox` (row-major), the bounding box of the pixels
/// touched (including the anti-aliased fringe).
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub bbox: Rect,
    pub mask: Vec<u8>,
}

impl Region {
    /// Coverage (0–1) at document pixel `(x, y)`; zero outside the box.
    pub fn at(&self, x: i32, y: i32) -> f32 {
        if !self.bbox.contains(x, y) {
            return 0.0;
        }
        self.mask[(y - self.bbox.y0) as usize * self.bbox.width() as usize + (x - self.bbox.x0) as usize] as f32 / 255.0
    }
}

/// Magic wand on 8-bit pixels: a scanline flood fill (or a global match) that only allocates a
/// byte mark per pixel, then anti-aliases just the edges inside the touched bounding box.
/// Matches [`magic_wand`] for 8-bit sources. `None` if the seed is outside `area`.
pub fn wand_region(img: &[[u8; 4]], area: Rect, seed: (i32, i32), tolerance: f32, contiguous: bool, anti_alias: bool) -> Option<Region> {
    if !area.contains(seed.0, seed.1) {
        return None;
    }
    let (w, h) = (area.width() as usize, area.height() as usize);
    let target = img[(seed.1 - area.y0) as usize * w + (seed.0 - area.x0) as usize];
    let tol = (tolerance + 1e-3).floor().max(0.0) as i32;
    let similar = |p: [u8; 4]| (0..4).all(|c| (p[c] as i32 - target[c] as i32).abs() <= tol);
    let mut marks = vec![0u8; w * h];
    let (mut bx0, mut by0, mut bx1, mut by1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    if contiguous {
        // Span fill: fill a run, then scan the rows above and below it for new seeds.
        let mut stack = vec![((seed.0 - area.x0) as usize, (seed.1 - area.y0) as usize)];
        while let Some((x, y)) = stack.pop() {
            let row = y * w;
            if marks[row + x] != 0 {
                continue;
            }
            let (mut l, mut r) = (x, x);
            while l > 0 && marks[row + l - 1] == 0 && similar(img[row + l - 1]) {
                l -= 1;
            }
            while r + 1 < w && marks[row + r + 1] == 0 && similar(img[row + r + 1]) {
                r += 1;
            }
            marks[row + l..=row + r].fill(255);
            (bx0, bx1, by0, by1) = (bx0.min(l), bx1.max(r), by0.min(y), by1.max(y));
            for ny in [y.wrapping_sub(1), y + 1] {
                if ny >= h {
                    continue;
                }
                let nrow = ny * w;
                let mut in_run = false;
                for nx in l..=r {
                    let ok = marks[nrow + nx] == 0 && similar(img[nrow + nx]);
                    if ok && !in_run {
                        stack.push((nx, ny));
                    }
                    in_run = ok;
                }
            }
        }
    } else {
        let scan = |y: usize, row: &mut [u8]| -> Option<(usize, usize)> {
            let mut span: Option<(usize, usize)> = None;
            for (x, m) in row.iter_mut().enumerate() {
                if similar(img[y * w + x]) {
                    *m = 255;
                    span = Some(span.map_or((x, x), |(a, _)| (a, x)));
                }
            }
            span
        };
        #[cfg(not(target_arch = "wasm32"))]
        let spans: Vec<Option<(usize, usize)>> = {
            use rayon::prelude::*;
            marks.par_chunks_mut(w).enumerate().map(|(y, row)| scan(y, row)).collect()
        };
        #[cfg(target_arch = "wasm32")]
        let spans: Vec<Option<(usize, usize)>> = marks.chunks_mut(w).enumerate().map(|(y, row)| scan(y, row)).collect();
        for (y, sp) in spans.iter().enumerate() {
            if let Some((a, b)) = sp {
                (bx0, bx1, by0, by1) = (bx0.min(*a), bx1.max(*b), by0.min(y), by1.max(y));
            }
        }
    }
    if bx0 == usize::MAX {
        return None;
    }
    // One pixel of fringe for anti-aliasing, clamped to the area.
    let pad = anti_alias as usize;
    let (x0, y0) = (bx0.saturating_sub(pad), by0.saturating_sub(pad));
    let (x1, y1) = ((bx1 + 1 + pad).min(w), (by1 + 1 + pad).min(h));
    let bw = x1 - x0;
    let at = |x: usize, y: usize| marks[y * w + x];
    let mut mask = vec![0u8; bw * (y1 - y0)];
    let fill_row = |yy: usize, out: &mut [u8]| {
        let y = y0 + yy;
        for (xx, o) in out.iter_mut().enumerate() {
            let x = x0 + xx;
            let v = at(x, y);
            *o = v;
            if !anti_alias {
                continue;
            }
            // Same rule as `antialias`: edge pixels blend with their 3×3 neighbourhood; neighbours
            // outside the area count as the pixel itself.
            let mut sum = 0u32;
            let mut edge = false;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    let n = if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 { v } else { at(nx as usize, ny as usize) };
                    edge |= n != v;
                    sum += n as u32;
                }
            }
            if edge {
                let avg = sum as f32 / (9.0 * 255.0);
                let f = if v > 127 { 0.5 + 0.5 * avg } else { 0.5 * avg };
                *o = (f * 255.0).round() as u8;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        mask.par_chunks_mut(bw).enumerate().for_each(|(yy, row)| fill_row(yy, row));
    }
    #[cfg(target_arch = "wasm32")]
    mask.chunks_mut(bw).enumerate().for_each(|(yy, row)| fill_row(yy, row));
    let bbox = Rect::new(area.x0 + x0 as i32, area.y0 + y0 as i32, area.x0 + x1 as i32, area.y0 + y1 as i32);
    Some(Region { bbox, mask })
}

/// Combines a [`Region`] with the current selection, touching only the region's box (outside it
/// the new mask is zero). Returns `None` when nothing remains selected.
pub fn combine_region(old: Option<&Surface>, new: Option<&Region>, mode: SelectionMode) -> Option<Surface> {
    let empty = Region { bbox: Rect::EMPTY, mask: Vec::new() };
    let new = new.unwrap_or(&empty);
    let b = new.bbox;
    let prev_u8 = |s: &Surface| -> Vec<u8> {
        let mut v = Vec::new();
        s.read_region_into(b, &mut v);
        let ch = s.channels();
        v.chunks_exact(ch).map(|p| (p[0].clamp(0.0, 1.0) * 255.0).round() as u8).collect()
    };
    let from_bytes = |bytes: &[u8]| -> Option<Surface> {
        if b.is_empty() || bytes.iter().all(|v| *v == 0) {
            return None;
        }
        let mut s = Surface::from_interleaved(PixelFormat::GRAY8, b, bytes);
        s.prune();
        Some(s)
    };
    let nonempty = |s: Surface| -> Option<Surface> { (!s.content_bounds().is_empty()).then_some(s) };
    match (mode, old) {
        (SelectionMode::Replace, _) | (SelectionMode::Add, None) => from_bytes(&new.mask),
        (SelectionMode::Subtract | SelectionMode::Intersect, None) => None,
        (SelectionMode::Intersect, Some(o)) => {
            let p = prev_u8(o);
            from_bytes(&p.iter().zip(&new.mask).map(|(a, b)| *a.min(b)).collect::<Vec<_>>())
        }
        (SelectionMode::Add | SelectionMode::Subtract, Some(o)) => {
            if b.is_empty() {
                return nonempty(o.clone());
            }
            let p = prev_u8(o);
            let out: Vec<f32> =
                p.iter().zip(&new.mask).map(|(a, n)| if mode == SelectionMode::Add { *a.max(n) } else { a.saturating_sub(*n) } as f32 / 255.0).collect();
            let mut s = o.clone();
            s.write_region(b, &out);
            s.prune();
            nonempty(s)
        }
    }
}

/// Softens hard mask edges: edge pixels become the 3×3 average.
fn antialias(m: &mut [f32], w: usize, h: usize) {
    let src = m.to_vec();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut sum = 0.0;
            let mut cnt = 0.0;
            let mut edge = false;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    let v = if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 { src[i] } else { src[yy as usize * w + xx as usize] };
                    edge |= v != src[i];
                    sum += v;
                    cnt += 1.0;
                }
            }
            if edge {
                // Keep selected pixels mostly selected, add a soft fringe.
                m[i] = if src[i] > 0.5 { 0.5 + 0.5 * sum / cnt } else { 0.5 * sum / cnt };
            }
        }
    }
}

/// Colour Range: coverage falls off linearly with the per-channel colour
/// distance; `fuzziness` in levels (0–200).
pub fn color_range(px: &[[f32; 4]], color: [f32; 3], fuzziness: f32) -> Vec<f32> {
    let f = fuzziness.max(0.0) + 1.0;
    px.iter()
        .map(|p| {
            if p[3] <= 0.0 {
                return 0.0;
            }
            let d = (0..3).map(|c| (p[c] - color[c]).abs() * 255.0).fold(0.0f32, f32::max);
            (1.0 - d / f).clamp(0.0, 1.0)
        })
        .collect()
}

/// One Color Range sample (Sampled Colors): an eyedropper colour, and where it was picked
/// (needed for Localized Color Clusters; area-local pixel coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeSample {
    pub color: [f32; 3],
    pub at: Option<(f32, f32)>,
}

/// Color Range › Sampled Colors with several samples (the dialog's "Add to Sample" eyedropper):
/// each pixel takes its best match over the samples, each judged like [`color_range`]. With
/// `localized` (Localized Color Clusters) a sample also fades with distance from where it was
/// picked, reaching 0 at `localized` pixels, so only nearby matching colours are selected;
/// samples without a position aren't localized. `w` is the row width of `px`.
pub fn color_range_samples(px: &[[f32; 4]], w: usize, samples: &[RangeSample], fuzziness: f32, localized: Option<f32>) -> Vec<f32> {
    let f = fuzziness.max(0.0) + 1.0;
    px.iter()
        .enumerate()
        .map(|(i, &[r, g, b, a])| {
            if a <= 0.0 {
                return 0.0;
            }
            let (x, y) = ((i % w.max(1)) as f32, (i / w.max(1)) as f32);
            samples.iter().fold(0.0f32, |best, s| {
                let d = [r, g, b].iter().zip(&s.color).map(|(v, c)| (v - c).abs() * 255.0).fold(0.0f32, f32::max);
                let mut k = (1.0 - d / f).clamp(0.0, 1.0);
                if let (Some(radius), Some((sx, sy))) = (localized, s.at) {
                    let dist = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt();
                    k *= (1.0 - dist / radius.max(1.0)).clamp(0.0, 1.0);
                }
                best.max(k)
            })
        })
        .collect()
}

/// Hue (degrees, 0..360) and chroma (max − min) of an RGB colour.
fn hue_chroma([r, g, b]: [f32; 3]) -> (f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    if d <= 0.0 {
        return (0.0, 0.0);
    }
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, d)
}

/// Color Range › Select: Reds, Yellows, Greens, Cyans, Blues or Magentas (`center` = 0, 60,
/// 120, 180, 240, 300°). A pixel belongs to a family by how close its hue is (full at the
/// family's hue, none 60° away, so an orange is half red, half yellow) times its chroma, so
/// neutrals are never selected and muted colours only partly.
pub fn hue_range(px: &[[f32; 4]], center: f32) -> Vec<f32> {
    px.iter()
        .map(|&[r, g, b, a]| {
            if a <= 0.0 {
                return 0.0;
            }
            let (h, chroma) = hue_chroma([r, g, b].map(|v| v.clamp(0.0, 1.0)));
            let dh = (h - center).rem_euclid(360.0);
            let dh = dh.min(360.0 - dh);
            ((1.0 - dh / 60.0).clamp(0.0, 1.0) * chroma).clamp(0.0, 1.0)
        })
        .collect()
}

/// Color Range › Select: Highlights, Midtones or Shadows. Pixels whose luminance (Rec. 601
/// weights, 0..255) lies in `lo..=hi` are fully selected; outside, selection falls off linearly
/// to 0 over `falloff` levels (the dialog's Fuzziness).
pub fn tone_range(px: &[[f32; 4]], lo: f32, hi: f32, falloff: f32) -> Vec<f32> {
    let ramp = falloff.max(1e-3);
    px.iter()
        .map(|&[r, g, b, a]| {
            if a <= 0.0 {
                return 0.0;
            }
            let l = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 1.0) * 255.0;
            let out = if l < lo {
                lo - l
            } else if l > hi {
                l - hi
            } else {
                0.0
            };
            (1.0 - out / ramp).clamp(0.0, 1.0)
        })
        .collect()
}

/// 1D squared distance transform (Felzenszwalb & Huttenlocher).
fn dt1(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let d = q as f32 - v[k] as f32;
        *o = d * d + f[v[k]];
    }
}

/// Euclidean distance to the nearest `true` pixel.
pub fn edt(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let mut g: Vec<f32> = inside.iter().map(|&b| if b { 0.0 } else { 1e20 }).collect();
    let n = w.max(h).max(1);
    let (mut f, mut o, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0f32; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        dt1(&f[..h], &mut o[..h], &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = o[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        dt1(&f[..w], &mut o[..w], &mut v, &mut z);
        for x in 0..w {
            g[y * w + x] = o[x].sqrt();
        }
    }
    g
}

/// Grows the selection by `r` pixels.
pub fn expand(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let inside: Vec<bool> = m.iter().map(|v| *v >= 0.5).collect();
    let d = edt(&inside, w, h);
    // A pixel whose centre is `d` away from the nearest selected centre is
    // covered when d ≤ r (fractional radii give partial coverage).
    m.iter().zip(d).map(|(v, d)| v.max((r + 1.0 - d).clamp(0.0, 1.0))).collect()
}

/// Shrinks the selection by `r` pixels.
pub fn contract(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let inv: Vec<f32> = m.iter().map(|v| 1.0 - v).collect();
    expand(&inv, w, h, r).into_iter().map(|v| 1.0 - v).collect()
}

/// A band of width `r` straddling the selection edge.
pub fn border(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let outer = expand(m, w, h, r / 2.0);
    let inner = contract(m, w, h, r / 2.0);
    outer.iter().zip(inner).map(|(o, i)| (o - i).max(0.0)).collect()
}

/// Smooth: removes specks and rounds corners (box average of radius `round(r)`, edges
/// replicated, then re-thresholded). Parallel and independent of the radius (#211).
pub fn smooth(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    crate::selection_blur::smooth(m, w, h, r)
}

/// Feather: Gaussian blur of the mask with sigma = radius / 2 (kernel truncated at 3σ, zero
/// beyond the canvas). Parallel, confined to the selection's bounds, and independent of the
/// radius (#211).
pub fn feather(m: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    crate::selection_blur::feather(m, w, h, radius)
}

/// Anti-aliased polygon (even-odd) coverage over `area`: exact horizontal
/// coverage, 4 sub-rows per pixel row (or 1 when not anti-aliased).
pub fn polygon(points: &[(f32, f32)], area: Rect, anti_alias: bool) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut m = vec![0.0f32; w * h];
    if points.len() < 3 {
        return m;
    }
    let sub = if anti_alias { 4 } else { 1 };
    let mut xs: Vec<f32> = Vec::new();
    for py in 0..h {
        for s in 0..sub {
            let y = area.y0 as f32 + py as f32 + (s as f32 + 0.5) / sub as f32;
            xs.clear();
            for i in 0..points.len() {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                if (a.1 <= y && b.1 > y) || (b.1 <= y && a.1 > y) {
                    xs.push(a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            for span in xs.as_chunks::<2>().0 {
                let (x0, x1) = ((span[0] - area.x0 as f32).max(0.0), (span[1] - area.x0 as f32).min(w as f32));
                if x1 <= x0 {
                    continue;
                }
                let (c0, c1) = (x0.floor() as usize, (x1.ceil() as usize).min(w));
                for cx in c0..c1 {
                    let cov = if anti_alias {
                        ((cx + 1) as f32).min(x1) - (cx as f32).max(x0)
                    } else {
                        let c = cx as f32 + 0.5;
                        if c >= x0 && c < x1 { 1.0 } else { 0.0 }
                    };
                    m[py * w + cx] += cov.max(0.0) / sub as f32;
                }
            }
        }
    }
    for v in &mut m {
        *v = v.min(1.0);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 4]) -> Vec<[f32; 4]> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect()
    }

    #[test]
    fn wand_contiguous_vs_global() {
        // Two separate red squares on white.
        let px = img(20, 10, |x, _| if (2..6).contains(&x) || (12..16).contains(&x) { [1.0, 0.0, 0.0, 1.0] } else { [1.0; 4] });
        let a = Rect::new(0, 0, 20, 10);
        let c = magic_wand(&px, a, (3, 3), 10.0, true, false);
        assert_eq!(c.iter().filter(|v| **v > 0.0).count(), 40);
        let g = magic_wand(&px, a, (3, 3), 10.0, false, false);
        assert_eq!(g.iter().filter(|v| **v > 0.0).count(), 80);
        let aa = magic_wand(&px, a, (3, 3), 10.0, true, true);
        assert!(aa[3 * 20 + 6] > 0.0 && aa[3 * 20 + 6] < 1.0);
        assert!(magic_wand(&px, a, (50, 3), 10.0, true, false).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn wand_tolerance() {
        let px = img(10, 1, |x, _| [x as f32 * 10.0 / 255.0, 0.0, 0.0, 1.0]);
        let m = magic_wand(&px, Rect::new(0, 0, 10, 1), (0, 0), 25.0, true, false);
        assert_eq!(m.iter().filter(|v| **v > 0.0).count(), 3);
    }

    #[test]
    fn color_range_falloff() {
        let px = vec![[1.0, 0.0, 0.0, 1.0], [0.9, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 0.0]];
        let m = color_range(&px, [1.0, 0.0, 0.0], 40.0);
        assert_eq!(m[0], 1.0);
        assert!(m[1] > 0.3 && m[1] < 1.0);
        assert_eq!(m[2], 0.0);
        assert_eq!(m[3], 0.0);
    }

    #[test]
    fn color_range_several_samples_and_localized() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        // 4 × 1: red, blue, green, red.
        let px = vec![red, blue, green, red];
        let s = |c: [f32; 4], at: Option<(f32, f32)>| RangeSample { color: [c[0], c[1], c[2]], at };
        let m = color_range_samples(&px, 4, &[s(red, None), s(blue, None)], 10.0, None);
        assert_eq!(m, vec![1.0, 1.0, 0.0, 1.0]);
        // One sample is exactly the single-colour Color Range.
        assert_eq!(color_range_samples(&px, 4, &[s(red, None)], 40.0, None), color_range(&px, [1.0, 0.0, 0.0], 40.0));
        // Localized at x = 0 with a 2 px range: the far red pixel (x = 3) drops out, x = 1 would be half.
        let m = color_range_samples(&px, 4, &[s(red, Some((0.0, 0.0)))], 10.0, Some(2.0));
        assert_eq!(m, vec![1.0, 0.0, 0.0, 0.0]);
        let reds = vec![red; 4];
        let m = color_range_samples(&reds, 4, &[s(red, Some((0.0, 0.0)))], 10.0, Some(2.0));
        assert_eq!(m, vec![1.0, 0.5, 0.0, 0.0]);
        // No samples: nothing selected.
        assert!(color_range_samples(&px, 4, &[], 10.0, None).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn hue_families() {
        let px = vec![
            [1.0, 0.0, 0.0, 1.0], // red
            [1.0, 0.5, 0.0, 1.0], // orange: between red and yellow
            [0.5, 0.5, 0.5, 1.0], // grey
            [1.0, 0.5, 0.5, 1.0], // pink: a muted red
            [0.0, 0.0, 1.0, 1.0], // blue
            [1.0, 0.0, 0.0, 0.0], // transparent
        ];
        let reds = hue_range(&px, 0.0);
        assert_eq!(reds[0], 1.0);
        assert!((reds[1] - 0.5).abs() < 1e-5);
        assert_eq!(reds[2], 0.0);
        assert!((reds[3] - 0.5).abs() < 1e-5);
        assert_eq!(reds[4], 0.0);
        assert_eq!(reds[5], 0.0);
        let yellows = hue_range(&px, 60.0);
        assert!((yellows[1] - 0.5).abs() < 1e-5);
        assert_eq!(hue_range(&px, 240.0)[4], 1.0);
        // Magentas wrap around 360°.
        assert_eq!(hue_range(&[[1.0, 0.0, 1.0, 1.0]], 300.0)[0], 1.0);
        assert!((hue_range(&[[1.0, 0.0, 0.5, 1.0]], 300.0)[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn tonal_ranges() {
        let g = |v: f32| [v / 255.0, v / 255.0, v / 255.0, 1.0];
        let px = vec![g(0.0), g(60.0), g(75.0), g(128.0), g(200.0), g(255.0)];
        let near = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        // Shadows up to 65, fading over 20 levels.
        let sh = tone_range(&px, 0.0, 65.0, 20.0);
        assert!(near(&sh, &[1.0, 1.0, 0.5, 0.0, 0.0, 0.0]), "{sh:?}");
        // Highlights from 190.
        let h = tone_range(&px, 190.0, 255.0, 20.0);
        assert!(near(&h, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0]), "{h:?}");
        // Midtones 105..150.
        let m = tone_range(&px, 105.0, 150.0, 20.0);
        assert!(near(&m, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]), "{m:?}");
    }

    fn square(w: usize, h: usize) -> Vec<f32> {
        (0..w * h).map(|i| if (5..15).contains(&(i % w)) && (5..15).contains(&(i / w)) { 1.0 } else { 0.0 }).collect()
    }

    #[test]
    fn expand_contract_border() {
        let m = square(20, 20);
        let e = expand(&m, 20, 20, 2.0);
        assert_eq!(e[10 * 20 + 3], 1.0);
        assert_eq!(e[10 * 20 + 2], 0.0);
        let c = contract(&m, 20, 20, 2.0);
        assert_eq!(c[10 * 20 + 6], 0.0);
        assert_eq!(c[10 * 20 + 7], 1.0);
        let b = border(&m, 20, 20, 4.0);
        assert_eq!(b[10 * 20 + 10], 0.0);
        assert!(b[10 * 20 + 5] > 0.5 && b[10 * 20 + 4] > 0.5);
    }

    #[test]
    fn smooth_removes_specks_and_feather_softens() {
        let mut m = square(20, 20);
        m[2 * 20 + 2] = 1.0;
        let s = smooth(&m, 20, 20, 1.0);
        assert_eq!(s[2 * 20 + 2], 0.0);
        assert_eq!(s[10 * 20 + 10], 1.0);
        let f = feather(&square(20, 20), 20, 20, 4.0);
        let edge = f[10 * 20 + 5];
        assert!(edge > 0.3 && edge < 0.8, "{edge}");
        let sum: f32 = f.iter().sum();
        assert!((sum - 100.0).abs() < 1.0);
    }

    #[test]
    fn polygon_area_and_aa() {
        let a = Rect::new(0, 0, 20, 20);
        let sq = polygon(&[(2.0, 2.0), (12.0, 2.0), (12.0, 12.0), (2.0, 12.0)], a, true);
        let area: f32 = sq.iter().sum();
        assert!((area - 100.0).abs() < 1e-3, "{area}");
        let tri = polygon(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)], a, true);
        let area: f32 = tri.iter().sum();
        assert!((area - 50.0).abs() < 1.0, "{area}");
        assert!(tri.iter().any(|v| *v > 0.0 && *v < 1.0));
        let hard = polygon(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)], a, false);
        assert!(hard.iter().all(|v| *v == 0.0 || *v == 1.0));
        assert!(polygon(&[(0.0, 0.0), (1.0, 1.0)], a, true).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn combine_modes() {
        let a = Rect::new(0, 0, 4, 1);
        let old = mask_to_surface(&[1.0, 1.0, 0.0, 0.0], a);
        let new = [0.0, 1.0, 1.0, 0.0];
        let v = |s: Option<Surface>| mask_from_surface(s.as_ref(), a);
        assert_eq!(v(combine(Some(&old), &new, a, SelectionMode::Replace)), vec![0.0, 1.0, 1.0, 0.0]);
        assert_eq!(v(combine(Some(&old), &new, a, SelectionMode::Add)), vec![1.0, 1.0, 1.0, 0.0]);
        assert_eq!(v(combine(Some(&old), &new, a, SelectionMode::Subtract)), vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(v(combine(Some(&old), &new, a, SelectionMode::Intersect)), vec![0.0, 1.0, 0.0, 0.0]);
        assert!(combine(Some(&old), &[1.0; 4], a, SelectionMode::Subtract).is_none());
        assert_eq!(SelectionMode::parse("add"), SelectionMode::Add);
    }

    /// The previous (per-sample) conversions, kept as the oracle for the tiled fast paths.
    fn old_mask_to_surface(m: &[f32], area: Rect) -> Surface {
        let mut s = Surface::new(PixelFormat::GRAY8);
        if !area.is_empty() {
            s.write_region(area, m);
        }
        s.prune();
        s
    }

    fn old_combine(old: Option<&Surface>, new: &[f32], area: Rect, mode: SelectionMode) -> Option<Surface> {
        let mut prev = Vec::new();
        match old {
            Some(s) => s.read_region_into(area, &mut prev),
            None => prev = vec![0.0; new.len()],
        }
        let q: Vec<f32> = new.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0).collect();
        let out: Vec<f32> = match mode {
            SelectionMode::Replace => q,
            SelectionMode::Add => prev.iter().zip(&q).map(|(a, b)| a.max(*b)).collect(),
            SelectionMode::Subtract => prev.iter().zip(&q).map(|(a, b)| (a - b).max(0.0)).collect(),
            SelectionMode::Intersect => prev.iter().zip(&q).map(|(a, b)| a.min(*b)).collect(),
        };
        let out: Vec<f32> = out.into_iter().map(|v| if v < 0.5 / 255.0 { 0.0 } else { v }).collect();
        if out.iter().all(|v| *v <= 0.0) {
            return None;
        }
        Some(old_mask_to_surface(&out, area))
    }

    #[test]
    fn tiled_conversions_match_per_sample_ones() {
        let mut rng = crate::photo_util::Rng(7);
        let mut u = move || (rng.next() >> 11) as f32 / (1u64 << 53) as f32;
        // Off-grid areas spanning several tiles, including negative origins.
        for area in [Rect::new(0, 0, 300, 20), Rect::new(-37, 250, 530, 263), Rect::new(255, -3, 258, 600), Rect::new(5, 5, 6, 6)] {
            let n = area.width() as usize * area.height() as usize;
            let old_m: Vec<f32> = (0..n).map(|i| if i % 97 < 40 { u() } else { 0.0 }).collect();
            let old = old_mask_to_surface(&old_m, area);
            let fast = mask_to_surface(&old_m, area);
            assert_eq!(fast, old, "{area:?}");
            let mut slow_read = Vec::new();
            old.read_region_into(area, &mut slow_read);
            assert_eq!(mask_from_surface(Some(&old), area), slow_read);
            for kind in 0..3 {
                let new: Vec<f32> = (0..n)
                    .map(|i| match kind {
                        0 => u() * 1.2 - 0.1,
                        1 => {
                            if i % 131 < 50 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                        _ => 0.001,
                    })
                    .collect();
                for mode in [SelectionMode::Replace, SelectionMode::Add, SelectionMode::Subtract, SelectionMode::Intersect] {
                    for prev in [None, Some(&old)] {
                        assert_eq!(combine(prev, &new, area, mode), old_combine(prev, &new, area, mode), "{area:?} {kind} {mode:?}");
                    }
                }
            }
        }
        // A short mask buffer reads as zeros instead of panicking.
        let a = Rect::new(0, 0, 10, 10);
        assert!(combine(None, &[1.0; 5], a, SelectionMode::Replace).is_some());
        assert_eq!(mask_to_surface(&[], a).tile_count(), 0);
        assert!(combine(None, &[], Rect::EMPTY, SelectionMode::Replace).is_none());
    }

    fn blobs(w: i32, h: i32) -> Vec<[u8; 4]> {
        // Deterministic blotchy image with gradients and hard edges.
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let v = ((x * 7 + y * 3) % 23 + if (x / 5 + y / 4) % 3 == 0 { 120 } else { 0 }) as u8;
                [v, v / 2, 255 - v, if x % 11 == 0 { 128 } else { 255 }]
            })
            .collect()
    }

    #[test]
    fn wand_region_matches_reference_magic_wand() {
        let (w, h) = (37, 29);
        let area = Rect::new(3, -2, 3 + w, -2 + h);
        let img = blobs(w, h);
        let f: Vec<[f32; 4]> = img.iter().map(|p| p.map(|v| v as f32 / 255.0)).collect();
        for (seed, tol, contiguous, aa) in [
            ((10, 5), 10.0, true, true),
            ((10, 5), 10.0, true, false),
            ((20, 20), 40.0, false, true),
            ((4, -2), 0.0, true, true),
            ((30, 10), 255.0, true, true),
        ] {
            let reference = magic_wand(&f, area, seed, tol, contiguous, aa);
            let r = wand_region(&img, area, seed, tol, contiguous, aa).unwrap();
            for y in area.y0..area.y1 {
                for x in area.x0..area.x1 {
                    let want = (reference[((y - area.y0) * w + x - area.x0) as usize] * 255.0).round();
                    assert!((r.at(x, y) * 255.0 - want).abs() <= 1.0, "{seed:?} {tol} {contiguous} {aa} at {x},{y}: {} vs {want}", r.at(x, y) * 255.0);
                }
            }
        }
        assert!(wand_region(&img, area, (0, 0), 10.0, true, true).is_none());
    }

    #[test]
    fn combine_region_matches_combine() {
        let area = Rect::new(0, 0, 40, 30);
        let mut old = Surface::new(PixelFormat::GRAY8);
        old.fill_rect(Rect::new(5, 5, 25, 20), &[1.0]);
        old.fill_rect(Rect::new(8, 8, 10, 10), &[0.4]);
        let img = blobs(40, 30);
        let region = wand_region(&img, area, (12, 12), 30.0, true, true).unwrap();
        let full: Vec<f32> = (0..area.height()).flat_map(|y| (0..area.width()).map(move |x| (x, y))).map(|(x, y)| region.at(x as i32, y as i32)).collect();
        for mode in [SelectionMode::Replace, SelectionMode::Add, SelectionMode::Subtract, SelectionMode::Intersect] {
            let a = combine(Some(&old), &full, area, mode);
            let b = combine_region(Some(&old), Some(&region), mode);
            assert_eq!(a.is_some(), b.is_some(), "{mode:?}");
            if let (Some(a), Some(b)) = (a, b) {
                for y in 0..30 {
                    for x in 0..40 {
                        assert!((a.pixel(x, y)[0] - b.pixel(x, y)[0]).abs() < 1.5 / 255.0, "{mode:?} {x},{y}");
                    }
                }
            }
        }
        assert!(combine_region(None, None, SelectionMode::Subtract).is_none());
    }
}

//! Minimum, Maximum, Offset.

use photocraft_geom::Rect;

use crate::image::{Edge, Image};
use crate::{Ctx, Preserve, UndefinedAreas};

/// Minimum or Maximum with the dialog's Preserve option: a square (`min_max`) or a disc
/// (`min_max_round`).
pub(crate) fn min_max_preserve(src: &Image, out: Rect, radius: f32, max: bool, preserve: Preserve) -> Vec<f32> {
    match preserve {
        Preserve::Squareness => min_max(src, out, radius, max),
        Preserve::Roundness => min_max_round(src, out, radius, max),
    }
}

/// Minimum or Maximum over a disc (Preserve: Roundness): every offset with
/// `dx² + dy² ≤ radius²`, so corners and straight edges move by the same distance. Each row of
/// the disc is a horizontal window, taken with the van Herk / Gil-Werman running extreme
/// (constant work per pixel whatever the window), so the cost is O(radius) per pixel.
pub(crate) fn min_max_round(src: &Image, out: Rect, radius: f32, max: bool) -> Vec<f32> {
    let n = src.ch;
    let r = radius.max(0.0);
    let ri = r.floor() as i32;
    if ri == 0 {
        return src.crop(out);
    }
    let w = out.width() as usize;
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let mut res = vec![if max { f32::MIN } else { f32::MAX }; w * out.height() as usize * n];
    let (mut row, mut ext) = (Vec::new(), Vec::new());
    for dy in -ri..=ri {
        // Half-width of the disc on this row (the epsilon keeps exact squares, e.g. r 5, dy 3 → 4).
        let hw = ((r * r - (dy * dy) as f32).max(0.0).sqrt() + 1e-4).floor() as i32;
        for y in out.y0..out.y1 {
            let o = (y - out.y0) as usize * w * n;
            for c in 0..n {
                row.clear();
                row.extend((out.x0 - hw..out.x1 + hw).map(|x| src.get(x, y + dy, c)));
                running_extreme(&row, (2 * hw + 1) as usize, max, &mut ext);
                for (x, v) in ext.iter().enumerate() {
                    let i = o + x * n + c;
                    res[i] = pick(res[i], *v);
                }
            }
        }
    }
    res
}

/// `out[i]` = extreme of `v[i..i + k]` for every full window (van Herk / Gil-Werman: prefix
/// extremes within blocks of `k` plus suffix extremes within blocks, one lookup of each).
pub(crate) fn running_extreme(v: &[f32], k: usize, max: bool, out: &mut Vec<f32>) {
    out.clear();
    let n = v.len();
    if k <= 1 {
        out.extend_from_slice(v);
        return;
    }
    if n < k {
        return;
    }
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let mut pre = v.to_vec();
    let mut suf = v.to_vec();
    for i in 1..n {
        if i % k != 0 {
            pre[i] = pick(pre[i - 1], v[i]);
        }
    }
    for i in (0..n - 1).rev() {
        if (i + 1) % k != 0 {
            suf[i] = pick(suf[i + 1], v[i]);
        }
    }
    out.extend((0..=n - k).map(|i| pick(suf[i], pre[i + k - 1])));
}

/// Minimum (spreads dark/transparent areas) or Maximum over a square.
pub(crate) fn min_max(src: &Image, out: Rect, radius: f32, max: bool) -> Vec<f32> {
    let n = src.ch;
    let r = radius.max(0.0).round() as i32;
    if r == 0 {
        return src.crop(out);
    }
    // Separable: rows then columns.
    let win = Rect::new(out.x0, out.y0 - r, out.x1, out.y1 + r);
    let ww = win.width() as usize;
    let mut tmp = vec![0.0f32; ww * win.height() as usize * n];
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            for c in 0..n {
                let mut m = if max { f32::MIN } else { f32::MAX };
                for xx in x - r..=x + r {
                    let v = src.get(xx, y, c);
                    m = if max { m.max(v) } else { m.min(v) };
                }
                tmp[((y - win.y0) as usize * ww + (x - win.x0) as usize) * n + c] = m;
            }
        }
    }
    let mut res = Vec::with_capacity(out.width() as usize * out.height() as usize * n);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            for c in 0..n {
                let mut m = if max { f32::MIN } else { f32::MAX };
                for yy in y - r..=y + r {
                    let v = tmp[((yy - win.y0) as usize * ww + (x - win.x0) as usize) * n + c];
                    m = if max { m.max(v) } else { m.min(v) };
                }
                res.push(m);
            }
        }
    }
    res
}

pub(crate) fn edge_of(u: UndefinedAreas) -> Edge {
    match u {
        UndefinedAreas::Wrap => Edge::Wrap,
        UndefinedAreas::Repeat => Edge::Repeat,
        UndefinedAreas::Transparent => Edge::Transparent,
    }
}

/// Shifts the bounds' content by whole pixels.
pub(crate) fn offset(src: &Image, out: Rect, ctx: &Ctx, dx: i32, dy: i32, undefined: UndefinedAreas) -> Vec<f32> {
    let n = src.ch;
    let edge = edge_of(undefined);
    let mut res = Vec::with_capacity(out.width() as usize * out.height() as usize * n);
    for y in out.y0..out.y1 {
        for x in out.x0..out.x1 {
            for c in 0..n {
                res.push(src.get_edge(x - dx, y - dy, c, edge, ctx.bounds));
            }
        }
    }
    res
}

//! `Surface::content_bounds` with the per-tile scan cached by copy-on-write tile identity, so
//! layer bounds cost a hash lookup per tile instead of a pixel scan per render tile (the CPU
//! compositor asks for an effect layer's bounds in every tile; the GPU planner every frame).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_raster::{Surface, Tile};

struct Entry {
    tile: Weak<Tile>,
    /// Content bounds inside the tile (tile-local), for one default pixel.
    default: Box<[u8]>,
    bounds: Option<(u16, u16, u16, u16)>,
}

fn cache() -> &'static Mutex<HashMap<usize, Entry>> {
    static C: std::sync::OnceLock<Mutex<HashMap<usize, Entry>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Pixels of `t` that differ from `dp` (tile-local half-open bounds).
fn scan(t: &Tile, dp: &[u8]) -> Option<(u16, u16, u16, u16)> {
    let bpp = dp.len();
    let ts = TILE_SIZE as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (y, row) in t.bytes().chunks_exact(ts * bpp).enumerate() {
        let Some(first) = row.chunks_exact(bpp).position(|px| px != dp) else { continue };
        let last = ts - 1 - row.chunks_exact(bpp).rev().position(|px| px != dp).unwrap_or(0);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x0 != usize::MAX).then_some((x0 as u16, y0 as u16, x1 as u16, y1 as u16))
}

/// `Surface::content_bounds`, cached per tile.
pub fn content_bounds(s: &Surface) -> Rect {
    if s.tile_count() == 0 {
        return Rect::EMPTY;
    }
    let fmt = s.format();
    let mut dp = vec![0u8; fmt.bytes_per_pixel()];
    photocraft_raster::encode_pixel(&fmt, &s.default_pixel(), &mut dp);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 1 << 16 {
        c.retain(|_, e| e.tile.strong_count() > 0);
    }
    let mut out = Rect::EMPTY;
    for (coord, t) in s.tiles() {
        let key = Arc::as_ptr(t) as usize;
        let hit = c.get(&key).filter(|e| e.tile.upgrade().is_some_and(|u| Arc::ptr_eq(&u, t)) && *e.default == *dp).map(|e| e.bounds);
        let b = match hit {
            Some(b) => b,
            None => {
                let b = scan(t, &dp);
                c.insert(key, Entry { tile: Arc::downgrade(t), default: dp.clone().into_boxed_slice(), bounds: b });
                b
            }
        };
        if let Some((x0, y0, x1, y1)) = b {
            let o = coord.rect();
            let r = Rect::new(o.x0 + x0 as i32, o.y0 + y0 as i32, o.x0 + x1 as i32, o.y0 + y1 as i32);
            out = if out.is_empty() { r } else { out.union(&r) };
        }
    }
    out
}

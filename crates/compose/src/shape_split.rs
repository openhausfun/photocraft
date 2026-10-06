//! A stroked shape layer split into its fill and its vector stroke (rasterised over the canvas),
//! for shapes with clipped layers: Photoshop draws the stroke above the clipped layers. Cached
//! per shape state, so tiles (and the GPU's textures) reuse one rasterisation.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use photocraft_color::PixelFormat;
use photocraft_doc::vector::ShapeLayer;
use photocraft_geom::Rect;
use photocraft_raster::Surface;

/// (fill, stroke), last use, and the shape's pixels the stroke was fitted to (kept alive so
/// their tile addresses, part of the key, can't be reused).
type Entry = ((Surface, Surface), u64, Option<Surface>);

fn cache() -> &'static Mutex<(HashMap<u64, Entry>, u64)> {
    static C: OnceLock<Mutex<(HashMap<u64, Entry>, u64)>> = OnceLock::new();
    C.get_or_init(|| Mutex::new((HashMap::new(), 0)))
}

const CAPACITY: usize = 32;

/// (fill only, stroke only) of a stroked shape, rendered over `canvas` (RGBA8, as the CPU
/// compositor's split always was). `None` without a stroke. Where the shape's pixels
/// (Photoshop's rendering, `sh.cache`) reach past our fill, the part the fill doesn't explain is
/// stroke: the stroke follows Photoshop's coverage (psd-tools double-stroke-effects).
pub fn split(sh: &ShapeLayer, canvas: Rect) -> Option<(Surface, Surface)> {
    sh.stroke.as_ref()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (canvas.x0, canvas.y0, canvas.x1, canvas.y1).hash(&mut h);
    format!("{:?}{:?}{:?}{:?}", sh.path, sh.fill, sh.stroke, sh.live).hash(&mut h);
    if let Some(c) = &sh.cache {
        format!("{:?}{:?}", c.format(), c.default_pixel()).hash(&mut h);
        for (k, t) in c.tiles() {
            (k.tx, k.ty, std::sync::Arc::as_ptr(t) as usize).hash(&mut h);
        }
    }
    let key = h.finish();
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.1 += 1;
        let tick = c.1;
        if let Some(e) = c.0.get_mut(&key) {
            e.1 = tick;
            return Some(e.0.clone());
        }
    }
    let fmt = PixelFormat::RGBA8;
    let fill_only = ShapeLayer { stroke: None, cache: None, ..sh.clone() };
    let stroke_only = ShapeLayer { fill: None, cache: None, ..sh.clone() };
    let mut parts = (photocraft_vector::render_shape(&fill_only, fmt, canvas), photocraft_vector::render_shape(&stroke_only, fmt, canvas));
    if let Some(cache) = &sh.cache {
        fit_stroke(&parts.0, &mut parts.1, cache, canvas);
    }
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.0.len() >= CAPACITY
        && let Some(old) = c.0.iter().min_by_key(|e| (e.1).1).map(|e| *e.0)
    {
        c.0.remove(&old);
    }
    let tick = c.1;
    c.0.insert(key, (parts.clone(), tick, sh.cache.clone()));
    Some(parts)
}

/// Fits the stroke part to the shape's own pixels: where they reach past the fill, the stroke
/// covers what the fill doesn't (`(pixels - fill) / (1 - fill)`), in the pixels' colour where we
/// drew no stroke.
fn fit_stroke(fill: &Surface, stroke: &mut Surface, cache: &Surface, canvas: Rect) {
    // Where either has pixels (the stroke may reach past the shape's pixels, and vice versa).
    let (cb, sb) = (crate::bounds::content_bounds(cache), crate::bounds::content_bounds(stroke));
    let r = if cb.is_empty() {
        sb
    } else if sb.is_empty() {
        cb
    } else {
        cb.union(&sb)
    }
    .intersect(&canvas);
    if r.is_empty() {
        return;
    }
    let n = r.width() as usize * r.height() as usize;
    let (mut f, mut s, mut c) = (vec![[0.0f32; 4]; n], vec![[0.0f32; 4]; n], vec![[0.0f32; 4]; n]);
    fill.read_rgba_into(r, &mut f);
    stroke.read_rgba_into(r, &mut s);
    cache.read_rgba_into(r, &mut c);
    let mut out = Vec::with_capacity(n * 4);
    for ((fp, sp), cp) in f.iter().zip(&s).zip(&c) {
        let mut p = *sp;
        if fp[3] < 1.0 - 1e-3 {
            let a = ((cp[3] - fp[3]) / (1.0 - fp[3])).clamp(0.0, 1.0);
            p = if sp[3] <= 0.0 { [cp[0], cp[1], cp[2], a] } else { [sp[0], sp[1], sp[2], a] };
        }
        out.extend_from_slice(&p);
    }
    stroke.write_region(r, &out);
}

//! A layer's effective mask (pixel mask × vector mask, each feathered) as one surface, for
//! backends that sample masks from textures (the GPU compositor) and for feathered masks on the
//! CPU. Rasterising a vector mask costs a path coverage pass and feathering a blur, so results
//! are cached per mask state; unchanged masks return the same tiles (cheap to clone, and the
//! GPU sees no change).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::Layer;
use photocraft_geom::Rect;
use photocraft_raster::Surface;

const FORMAT: PixelFormat = PixelFormat { mode: ColorMode::Grayscale, sample: SampleType::F32, alpha: false };

struct Cache {
    map: HashMap<u64, (Surface, u64)>,
    tick: u64,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { map: HashMap::new(), tick: 0 }))
}

/// Entries kept (least recently used dropped first).
const CAPACITY: usize = 64;

fn key(layer: &Layer, canvas: Rect) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layer.id.0.hash(&mut h);
    (canvas.x0, canvas.y0, canvas.x1, canvas.y1).hash(&mut h);
    format!("{:?}", layer.vector_mask).hash(&mut h);
    if let Some(m) = &layer.mask {
        (m.enabled, m.density.to_bits(), m.feather.to_bits()).hash(&mut h);
        format!("{:?}", m.surface.default_pixel()).hash(&mut h);
        for (c, t) in m.surface.tiles() {
            (c.tx, c.ty, Arc::as_ptr(t) as usize).hash(&mut h);
        }
    }
    h.finish()
}

/// Gaussian sigma of a mask feather of `px` pixels (Properties › Feather). Fitted on the
/// psd-tools corpus (layer_mask_data.psd, mask-parameters-no-real-channel.psd: sigma = feather
/// beats 0.33, 0.5, 0.7, 1.3 and 1.6 times it).
pub fn feather_sigma(px: f32) -> f32 {
    if px.is_finite() { (px * FEATHER_SIGMA).clamp(0.0, MAX_FEATHER_SIGMA) } else { 0.0 }
}

const FEATHER_SIGMA: f32 = 1.0;
/// Photoshop's feather tops out at 1000 px.
const MAX_FEATHER_SIGMA: f32 = 1000.0 * FEATHER_SIGMA;

/// Whether `layer` has an enabled mask with a feather (rendered through [`combined_mask`]).
pub fn has_feather(layer: &Layer) -> bool {
    layer.mask.as_ref().is_some_and(|m| m.enabled && feather_sigma(m.feather) > 0.0)
        || layer.vector_mask.as_ref().is_some_and(|v| v.enabled && feather_sigma(v.feather) > 0.0)
}

/// Approximate Gaussian blur of a `w`×`h` plane: three box passes per axis (edges clamp, which
/// is exact here since the planes extend into constant mask regions).
fn gaussian(v: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 || w == 0 || h == 0 {
        return;
    }
    // Box widths whose three-pass variance matches sigma² (Wells 1986 / Kovesi).
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let r = (((ideal.floor() as usize) | 1).max(1) - 1) / 2;
    if r == 0 {
        return;
    }
    let mut line = Vec::new();
    let mut pass = |v: &mut [f32], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        for k in 0..count {
            for _ in 0..3 {
                line.clear();
                line.extend((0..len).map(|i| v[at(k, i)]));
                let get = |i: isize| line[i.clamp(0, len as isize - 1) as usize];
                let mut acc: f32 = (-(r as isize)..=r as isize).map(get).sum();
                let n = (2 * r + 1) as f32;
                for i in 0..len {
                    v[at(k, i)] = acc / n;
                    acc += get(i as isize + r as isize + 1) - get(i as isize - r as isize);
                }
            }
        }
    };
    pass(v, w, h, &|row, i| row * w + i);
    pass(v, h, w, &|col, i| i * w + col);
}

/// The layer's mask values (pixel mask with density × vector mask, each blurred by its feather,
/// exactly as the CPU compositor applies them) as a single-channel f32 surface over `canvas`, or
/// `None` when the layer has no enabled vector mask and no feathered pixel mask (use the pixel
/// mask directly). Values outside the computed area are the surface's default.
pub fn combined_mask(layer: &Layer, canvas: Rect) -> Option<Surface> {
    let vm = layer.vector_mask.as_ref().filter(|v| v.enabled);
    let pixel = layer.mask.as_ref().filter(|m| m.enabled);
    let (sv, sp) = (vm.map_or(0.0, |v| feather_sigma(v.feather)), pixel.map_or(0.0, |m| feather_sigma(m.feather)));
    if vm.is_none() && sp <= 0.0 {
        return None;
    }
    let k = key(layer, canvas);
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.tick += 1;
        let tick = c.tick;
        if let Some(e) = c.map.get_mut(&k) {
            e.1 = tick;
            return Some(e.0.clone());
        }
    }
    // Far outside the path the vector mask is constant.
    let far = Rect::from_xywh(canvas.x0 - 1_000_000, canvas.y0 - 1_000_000, 1, 1);
    let v_out = vm.map_or(1.0, |vm| photocraft_vector::vector_mask_values(vm, far)[0]);
    let p_def = pixel.map_or(1.0, |m| {
        let d = m.surface.default_pixel().first().copied().unwrap_or(1.0);
        1.0 - m.density * (1.0 - d)
    });
    let grow = |r: Rect, sigma: f32| {
        let m = (sigma * 3.0).ceil() as i32 + 2;
        if r.is_empty() { r } else { Rect::new(r.x0.saturating_sub(m), r.y0.saturating_sub(m), r.x1.saturating_add(m), r.y1.saturating_add(m)) }
    };
    // Where the product can differ from `p_def × v_out`: the path's bounds, plus the pixel
    // mask's painted tiles when the vector mask lets them through outside the path, each grown
    // by its feather.
    let mut area = match vm.and_then(|vm| vm.path.control_bounds()) {
        Some((x0, y0, x1, y1)) => grow(Rect::new(x0.floor() as i32 - 2, y0.floor() as i32 - 2, x1.ceil() as i32 + 2, y1.ceil() as i32 + 2), sv),
        None => Rect::EMPTY,
    };
    if let Some(m) = pixel
        && v_out > 0.0
    {
        let b = grow(m.surface.content_bounds(), sp);
        area = if area.is_empty() {
            b
        } else if b.is_empty() {
            area
        } else {
            area.union(&b)
        };
    }
    let area = area.intersect(&canvas);
    let mut s = Surface::with_default(FORMAT, &[p_def * v_out]);
    if !area.is_empty() {
        let (w, h) = (area.width() as usize, area.height() as usize);
        let mut v = match vm {
            Some(vm) => photocraft_vector::vector_mask_values(vm, area),
            None => vec![1.0; w * h],
        };
        gaussian(&mut v, w, h, sv);
        if let Some(m) = pixel {
            let mut pm = Vec::new();
            m.values_into(area, &mut pm);
            gaussian(&mut pm, w, h, sp);
            for (a, b) in v.iter_mut().zip(pm) {
                *a *= b;
            }
        }
        s.write_region(area, &v);
    }
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.map.len() >= CAPACITY
        && let Some(old) = c.map.iter().min_by_key(|e| e.1.1).map(|e| *e.0)
    {
        c.map.remove(&old);
    }
    let tick = c.tick;
    c.map.insert(k, (s.clone(), tick));
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::vector::{Knot, Path, Subpath, VectorMask};

    #[test]
    fn matches_the_cpu_mask_and_is_cached() {
        let mut l = Layer::raster("l", PixelFormat::RGBA8);
        let pts = [(5.0, 5.0), (30.0, 6.0), (20.0, 28.0)];
        let knots = pts.iter().map(|&(x, y)| Knot::corner(x, y)).collect();
        let mut path = Path::default();
        path.subpaths.push(Subpath { knots, closed: true, ..Default::default() });
        let mut vm = VectorMask::new(path);
        vm.density = 0.9;
        l.vector_mask = Some(vm);
        let canvas = Rect::new(0, 0, 40, 32);
        let s = combined_mask(&l, canvas).unwrap();
        let want = photocraft_vector::vector_mask_values(l.vector_mask.as_ref().unwrap(), canvas);
        let mut got = Vec::new();
        s.read_region_into(canvas, &mut got);
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() < 1e-6);
        }
        let again = combined_mask(&l, canvas).unwrap();
        assert!(s.tiles().zip(again.tiles()).all(|(a, b)| Arc::ptr_eq(a.1, b.1)));
        l.vector_mask = None;
        assert!(combined_mask(&l, canvas).is_none());
    }
}

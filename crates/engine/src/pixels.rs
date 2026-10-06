//! Destructive pixel operations on layers (Image → Adjustments, flips, fills).

use photocraft_compose::{Buffer, adjust};
use photocraft_doc::{Adjustment, Document, Layer, LayerContent};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba, to_rgba};

/// Apply an adjustment destructively to a surface, weighted by an optional selection.
/// Applies `adj` to a surface (any colour model and depth, via straight RGBA) through the
/// selection; `mode` is the document's, for the tone transfer (e.g. Exposure in Grayscale).
pub fn adjust_surface(s: &mut Surface, adj: &Adjustment, selection: Option<&Surface>, mode: photocraft_color::ColorMode) {
    let r = s.content_bounds();
    if r.is_empty() {
        return;
    }
    let fmt = s.format();
    let n = fmt.channels();
    let raw = s.read_region(r);
    let mut buf = Buffer { rect: r, px: raw.chunks_exact(n).map(|p| to_rgba(&fmt, p)).collect() };
    let orig = buf.clone();
    adjust::apply_with(adj, &mut buf, adjust::Transfer::for_mode(mode));
    let w = r.width() as usize;
    let mut out = Vec::with_capacity(raw.len());
    for (i, (a, o)) in buf.px.iter().zip(&orig.px).enumerate() {
        let k = selection.map_or(1.0, |sel| sel.sample_channel(r.x0 + (i % w) as i32, r.y0 + (i / w) as i32, 0));
        let mixed: [f32; 4] = std::array::from_fn(|c| o[c] + (a[c] - o[c]) * k);
        let mut enc = [0.0f32; 8];
        let m = photocraft_raster::from_rgba_into(&fmt, mixed, &mut enc);
        out.extend_from_slice(&enc[..m]);
    }
    s.write_region(r, &out);
}

/// Fill (respecting selection coverage) with a straight RGBA colour.
pub fn fill_surface(s: &mut Surface, area: Rect, color: [f32; 4], selection: Option<&Surface>, lock_transparency: bool) {
    let fmt = s.format();
    let n = fmt.channels();
    let mut region = s.read_region(area);
    let w = area.width() as usize;
    for (i, px) in region.chunks_exact_mut(n).enumerate() {
        let x = area.x0 + (i % w) as i32;
        let y = area.y0 + (i / w) as i32;
        let k = selection.map_or(1.0, |sel| sel.sample_channel(x, y, 0));
        if k <= 0.0 {
            continue;
        }
        let d = to_rgba(&fmt, px);
        let sa = color[3] * k;
        let oa = sa + d[3] * (1.0 - sa);
        let mut o = [0.0f32; 4];
        if oa > 0.0 {
            for c in 0..3 {
                o[c] = (color[c] * sa + d[c] * d[3] * (1.0 - sa)) / oa;
            }
        }
        o[3] = if lock_transparency { d[3] } else { oa };
        let mut enc = [0.0f32; 8];
        photocraft_raster::from_rgba_into(&fmt, o, &mut enc);
        px.copy_from_slice(&enc[..n]);
    }
    s.write_region(area, &region);
}

/// Clear pixels (make transparent) within the selection.
pub fn clear_surface(s: &mut Surface, area: Rect, selection: Option<&Surface>) {
    let fmt = s.format();
    let n = fmt.channels();
    if !fmt.alpha {
        return;
    }
    let mut region = s.read_region(area);
    let w = area.width() as usize;
    for (i, px) in region.chunks_exact_mut(n).enumerate() {
        let k = selection.map_or(1.0, |sel| sel.sample_channel(area.x0 + (i % w) as i32, area.y0 + (i / w) as i32, 0));
        px[n - 1] *= 1.0 - k;
        // Fully cleared pixels become the empty pixel, so bounds and tile pruning see them as gone.
        if px[n - 1] <= 0.0 {
            px.fill(0.0);
        }
    }
    s.write_region(area, &region);
    s.prune();
}

/// Remap a surface through a pixel-coordinate mapping (used for flips/rotations).
pub fn remap_surface(s: &Surface, map: impl Fn(i32, i32) -> (i32, i32)) -> Surface {
    let r = s.content_bounds();
    let mut out = Surface::with_default(s.format(), &s.default_pixel());
    let n = s.channels();
    let raw = s.read_region(r);
    let w = r.width() as usize;
    for (i, px) in raw.chunks_exact(n).enumerate() {
        let (x, y) = (r.x0 + (i % w) as i32, r.y0 + (i / w) as i32);
        let (nx, ny) = map(x, y);
        out.write_pixel(nx, ny, px);
    }
    out
}

/// Merge `upper` onto `lower` producing a raster layer (Layer → Merge Down).
pub fn merge_down(doc_bounds: Rect, lower: &Layer, upper: &Layer, format: photocraft_color::PixelFormat) -> Layer {
    let area = [lower, upper].iter().map(|l| l.surface().map(|s| s.content_bounds()).unwrap_or(doc_bounds)).fold(Rect::EMPTY, |a, b| a.union(&b));
    let stack = vec![lower.clone(), upper.clone()];
    let mut tmp = Document::new("merge", doc_bounds.size(), format.mode, format.sample);
    tmp.layers = stack;
    let buf = photocraft_compose::render(&tmp, area);
    let mut s = Surface::new(format);
    let data: Vec<f32> = buf.px.iter().flat_map(|p| from_rgba(&format, *p)).collect();
    if !area.is_empty() {
        s.write_region(area, &data);
        s.prune();
    }
    let mut merged = Layer::new(lower.name.clone(), LayerContent::Raster(s));
    merged.id = lower.id;
    merged.blend = lower.blend;
    merged.opacity = 1.0;
    merged
}

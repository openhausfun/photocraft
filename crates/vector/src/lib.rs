//! # photocraft-vector
//!
//! Rasterization of vector data from `photocraft-doc`: paths (with Photoshop path operations),
//! shape layers (fill + stroke), vector masks, and the inverse direction (tracing a coverage
//! mask back into a path for "Make Work Path").
//!
//! * [`flatten`]: cubic Bézier flattening with a distance tolerance.
//! * [`raster`]: exact area-coverage scanline rasterizer (non-zero / even-odd per component,
//!   boolean combination of components, inversion).
//! * [`stroke`]: polygon stroking with miter/round/bevel joins, butt/round/square caps, dashes.
//! * [`shapes`]: live shapes (rectangles with corner radii, ellipses, polygons, stars, lines).
//! * [`trace`]: contour tracing + curve fitting (selection → path).
//!
//! Output is either raw coverage (`f32` per pixel) or sparse tiled [`Surface`]s in any
//! [`PixelFormat`] (only tiles that differ from the default are allocated). Pure Rust, no
//! platform dependencies; builds for `wasm32` (single-threaded there, rayon elsewhere).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod flatten;
pub mod raster;
pub mod shapes;
pub mod stroke;
pub mod trace;

use photocraft_color::{ColorMode, PixelFormat};
use photocraft_doc::{Fill, FillRule, GradientStyle, Path, PathOp, ShapeLayer, ShapeStroke, StrokeAlign, VectorMask};
use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_raster::Surface;

pub use flatten::{Polyline, flatten_path, flatten_subpath};
pub use raster::Rasterizer;
pub use stroke::{StrokeStyle, stroke_polygons};

/// Default flattening tolerance in pixels (curve-to-chord distance).
pub const DEFAULT_TOLERANCE: f64 = 0.01;

/// Compiles the fill area of `path`: one component per subpath (filled with the path's rule)
/// folded with the subpath operations; open subpaths are closed implicitly.
pub fn fill_rasterizer(path: &Path, tol: f64) -> Rasterizer {
    let mut r = Rasterizer::new(path.inverted);
    for c in path.components() {
        let polys: Vec<Vec<(f64, f64)>> = path.subpaths[c.clone()].iter().map(|s| flatten_subpath(s, tol).pts).collect();
        r.add_component(&polys, path.effective_op(c.start), path.fill_rule);
    }
    r
}

/// Compiles the stroke outline of `path` (all subpaths, one non-zero component).
pub fn stroke_rasterizer(path: &Path, style: &StrokeStyle, tol: f64) -> Rasterizer {
    // Stroke outlines tolerate a coarser flattening than fills (the offset hides chord error).
    let lines = flatten_path(path, (tol * 4.0).min(style.width.max(0.01) * 0.1).max(1e-3));
    let polys = stroke_polygons(&lines, style, tol);
    let mut r = Rasterizer::new(false);
    r.add_component(&polys, PathOp::Combine, FillRule::NonZero);
    r
}

/// Stroke geometry for a shape stroke (dash lengths are multiples of the width in the model).
pub fn stroke_style(s: &ShapeStroke) -> StrokeStyle {
    let w = f64::from(s.width.max(0.0));
    StrokeStyle {
        width: w,
        cap: s.cap,
        join: s.join,
        miter_limit: f64::from(s.miter_limit.max(1.0)),
        dashes: s.dashes.iter().map(|d| f64::from(*d) * w).collect(),
        dash_offset: f64::from(s.dash_offset) * w,
    }
}

/// Fill coverage of `path` over `rect` (row-major, `0..=1`).
pub fn path_coverage(path: &Path, rect: Rect) -> Vec<f32> {
    fill_rasterizer(path, DEFAULT_TOLERANCE).render(rect)
}

enum MaskGeometry {
    Constant(f32),
    Path(Rasterizer),
}

/// A vector mask's immutable geometry and density, reusable across render rectangles.
pub struct CompiledVectorMask {
    geometry: MaskGeometry,
    density: Option<f32>,
}

impl CompiledVectorMask {
    /// Compiles only enabled, nonempty paths using the default flattening tolerance.
    pub fn new(m: &VectorMask) -> Self {
        let geometry = if !m.enabled {
            MaskGeometry::Constant(1.0)
        } else if m.path.is_empty() {
            // An empty Photoshop vector mask reveals all, unlike an empty fill path.
            MaskGeometry::Constant(if m.path.inverted { 0.0 } else { 1.0 })
        } else {
            MaskGeometry::Path(fill_rasterizer(&m.path, DEFAULT_TOLERANCE))
        };
        Self { geometry, density: (m.enabled && m.density < 1.0).then(|| m.density.clamp(0.0, 1.0)) }
    }

    /// Effective mask values over `rect`, with invocation-local rasterization state.
    pub fn render(&self, rect: Rect) -> Vec<f32> {
        let mut v = match &self.geometry {
            MaskGeometry::Constant(value) => vec![*value; rect.width() as usize * rect.height() as usize],
            MaskGeometry::Path(rasterizer) => rasterizer.render(rect),
        };
        if let Some(d) = self.density {
            for x in &mut v {
                *x = 1.0 - d * (1.0 - *x);
            }
        }
        v
    }
}

/// Effective vector-mask values over `rect` (density applied; all ones when disabled). Like
/// Photoshop, a vector mask without any subpath reveals everything (hides everything when
/// inverted): "Add Vector Mask" starts from an empty, revealing mask.
pub fn vector_mask_values(m: &VectorMask, rect: Rect) -> Vec<f32> {
    CompiledVectorMask::new(m).render(rect)
}

/// Tile-aligned bands (rows of tiles) covering `r`.
fn tile_bands(r: Rect) -> Vec<Rect> {
    let mut out = Vec::new();
    if r.is_empty() {
        return out;
    }
    let mut y = r.y0.div_euclid(TILE_SIZE) * TILE_SIZE;
    while y < r.y1 {
        let band = Rect::new(r.x0, y.max(r.y0), r.x1, (y + TILE_SIZE).min(r.y1));
        out.push(band);
        y += TILE_SIZE;
    }
    out
}

/// Splits a band buffer (`channels` floats per pixel) into tile-aligned pieces, skipping
/// pieces where `keep` is false for every pixel.
fn band_tiles(band: Rect, data: &[f32], channels: usize, keep: impl Fn(&[f32]) -> bool) -> Vec<(Rect, Vec<f32>)> {
    let w = band.width() as usize;
    let mut out = Vec::new();
    let mut x = band.x0.div_euclid(TILE_SIZE) * TILE_SIZE;
    while x < band.x1 {
        let tr = Rect::new(x.max(band.x0), band.y0, (x + TILE_SIZE).min(band.x1), band.y1);
        let tw = tr.width() as usize;
        let mut buf = Vec::with_capacity(tw * tr.height() as usize * channels);
        let mut any = false;
        for row in 0..tr.height() as usize {
            let o = (row * w + (tr.x0 - band.x0) as usize) * channels;
            let slice = &data[o..o + tw * channels];
            if !any && slice.chunks_exact(channels).any(&keep) {
                any = true;
            }
            buf.extend_from_slice(slice);
        }
        if any {
            out.push((tr, buf));
        }
        x += TILE_SIZE;
    }
    out
}

/// Runs `f` over the bands of `r` (in parallel groups on native targets) and writes the
/// resulting pieces into `surface`.
fn render_bands(surface: &mut Surface, r: Rect, f: &(dyn Fn(Rect) -> Vec<(Rect, Vec<f32>)> + Sync)) {
    let bands = tile_bands(r);
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let group = rayon::current_num_threads().max(1) * 2;
        for chunk in bands.chunks(group) {
            let parts: Vec<Vec<(Rect, Vec<f32>)>> = chunk.par_iter().map(|b| f(*b)).collect();
            for (tr, v) in parts.into_iter().flatten() {
                surface.write_region(tr, &v);
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    for b in bands {
        for (tr, v) in f(b) {
            surface.write_region(tr, &v);
        }
    }
}

/// Renders a rasterizer's coverage into a single-channel surface of `format` (grayscale
/// model, no alpha, e.g. a selection or mask) over `clip`. Outside `clip` the surface reads 0,
/// or 1 for inverted paths whose coverage is written everywhere inside `clip`.
pub fn coverage_surface(r: &Rasterizer, format: PixelFormat, clip: Rect) -> Surface {
    let fmt = PixelFormat::new(ColorMode::Grayscale, format.sample, false);
    let mut s = Surface::new(fmt);
    let area = if r.is_inverted() { clip } else { r.pixel_bounds().map_or(Rect::EMPTY, |b| b.intersect(&clip)) };
    render_bands(&mut s, area, &|band| {
        let cov = r.render(band);
        band_tiles(band, &cov, 1, |p| p[0] > 0.0)
    });
    s
}

/// Colour source for fills and strokes.
#[derive(Clone, Debug)]
pub enum Paint {
    None,
    /// Straight RGBA.
    Solid([f32; 4]),
    Gradient {
        stops: Vec<(f32, [f32; 4])>,
        style: GradientStyle,
        angle: f32,
        scale: f32,
        reverse: bool,
        bounds: (f64, f64, f64, f64),
    },
}

impl Paint {
    /// Paint for a model fill, with gradients laid out over `bounds` (`x0, y0, x1, y1`).
    /// Patterns are not rendered yet (transparent), matching the compositor.
    pub fn from_fill(f: &Fill, bounds: (f64, f64, f64, f64)) -> Paint {
        let rgba = |c: &photocraft_color::Color| {
            let v = c.to_rgb();
            [v[0], v[1], v[2], c.alpha]
        };
        match f {
            Fill::Solid(c) => Paint::Solid(rgba(c)),
            Fill::Gradient { stops, angle, scale, style, reverse, .. } => Paint::Gradient {
                stops: stops.iter().map(|(p, c)| (*p, rgba(c))).collect(),
                style: *style,
                angle: *angle,
                scale: *scale,
                reverse: *reverse,
                bounds,
            },
            Fill::Pattern { .. } => Paint::None,
        }
    }

    /// Straight RGBA at pixel centre `(x, y)`.
    pub fn at(&self, x: f64, y: f64) -> [f32; 4] {
        match self {
            Paint::None => [0.0; 4],
            Paint::Solid(c) => *c,
            Paint::Gradient { stops, style, angle, scale, reverse, bounds } => {
                let t = gradient_t(*style, *angle, *scale, *reverse, *bounds, x, y);
                sample_stops(stops, t)
            }
        }
    }
}

/// Gradient parameter at `(x, y)`: the gradient spans the bounds' extent along its angle
/// (degrees, counter-clockwise from 3 o'clock), centred on the bounds.
fn gradient_t(style: GradientStyle, angle: f32, scale: f32, reverse: bool, b: (f64, f64, f64, f64), x: f64, y: f64) -> f32 {
    let (w, h) = ((b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0));
    let (cx, cy) = (b.0 + w / 2.0, b.1 + h / 2.0);
    let a = f64::from(angle).to_radians();
    let (s, c) = a.sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    let along = dx * c - dy * s;
    let across = dx * s + dy * c;
    let len = ((c * w).powi(2) + (s * h).powi(2)).sqrt().max(1.0) * f64::from(scale.max(1e-3));
    let t = match style {
        GradientStyle::Linear => along / len + 0.5,
        GradientStyle::Reflected => (along / (len / 2.0)).abs(),
        GradientStyle::Radial => dx.hypot(dy) / (len / 2.0),
        GradientStyle::Diamond => (along.abs() + across.abs()) / (len / 2.0),
        GradientStyle::Angle => ((a - (-dy).atan2(dx)) / std::f64::consts::TAU).rem_euclid(1.0),
    };
    let t = t.clamp(0.0, 1.0) as f32;
    if reverse { 1.0 - t } else { t }
}

fn sample_stops(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    match stops {
        [] => [0.0; 4],
        [s] => s.1,
        _ => {
            if t <= stops[0].0 {
                return stops[0].1;
            }
            for w in stops.windows(2) {
                if t <= w[1].0 {
                    let k = if w[1].0 > w[0].0 { (t - w[0].0) / (w[1].0 - w[0].0) } else { 0.0 };
                    return std::array::from_fn(|i| w[0].1[i] + (w[1].1[i] - w[0].1[i]) * k);
                }
            }
            stops[stops.len() - 1].1
        }
    }
}

/// Everything needed to render a shape layer, compiled once.
pub struct CompiledShape {
    fill: Option<(Rasterizer, Paint)>,
    stroke: Option<(Rasterizer, Paint, f32)>,
    /// Fill area used to clip inside/outside strokes.
    align: Option<(Rasterizer, bool)>,
    bounds: Option<Rect>,
}

impl CompiledShape {
    pub fn new(shape: &ShapeLayer, tol: f64) -> Self {
        let pb = shape.path.control_bounds().unwrap_or((0.0, 0.0, 0.0, 0.0));
        let area = fill_rasterizer(&shape.path, tol);
        let mut bounds = None;
        let fill = shape.fill.as_ref().map(|f| {
            bounds = area.pixel_bounds();
            (area.clone(), Paint::from_fill(f, pb))
        });
        let all_closed = shape.path.subpaths.iter().all(|s| s.closed);
        let mut align = None;
        let stroke = shape.stroke.as_ref().filter(|s| s.width > 0.0 && s.opacity > 0.0).map(|s| {
            let mut st = stroke_style(s);
            if all_closed && s.align != StrokeAlign::Center {
                // Inside/outside strokes: a centred stroke twice as wide, clipped by the fill area.
                st.width *= 2.0;
                align = Some((area.clone(), s.align == StrokeAlign::Inside));
            }
            let r = stroke_rasterizer(&shape.path, &st, tol);
            let sb = r.pixel_bounds();
            bounds = match (bounds, sb) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) => a.or(b),
            };
            (r, Paint::from_fill(&s.paint, pb), s.opacity.clamp(0.0, 1.0))
        });
        if shape.path.inverted && shape.fill.is_some() {
            bounds = Some(Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4));
        }
        CompiledShape { fill, stroke, align, bounds }
    }

    /// Pixel bounds of the rendering (`None` = nothing visible).
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    /// Straight RGBA for every pixel of `rect`.
    pub fn render_rgba(&self, rect: Rect) -> Vec<[f32; 4]> {
        let n = rect.width() as usize * rect.height() as usize;
        let mut out = vec![[0.0f32; 4]; n];
        let w = rect.width() as usize;
        if let Some((r, paint)) = &self.fill {
            let cov = r.render(rect);
            for (i, (o, c)) in out.iter_mut().zip(&cov).enumerate() {
                if *c > 0.0 {
                    let (x, y) = ((rect.x0 + (i % w) as i32) as f64 + 0.5, (rect.y0 + (i / w) as i32) as f64 + 0.5);
                    let mut p = paint.at(x, y);
                    p[3] *= c;
                    *o = p;
                }
            }
        }
        if let Some((r, paint, opacity)) = &self.stroke {
            let mut cov = r.render(rect);
            if let Some((area, inside)) = &self.align {
                let a = area.render(rect);
                for (c, f) in cov.iter_mut().zip(&a) {
                    *c *= if *inside { *f } else { 1.0 - *f };
                }
            }
            for (i, (o, c)) in out.iter_mut().zip(&cov).enumerate() {
                if *c <= 0.0 {
                    continue;
                }
                let (x, y) = ((rect.x0 + (i % w) as i32) as f64 + 0.5, (rect.y0 + (i / w) as i32) as f64 + 0.5);
                let s = paint.at(x, y);
                let sa = s[3] * c * opacity;
                let da = o[3];
                let a = sa + da * (1.0 - sa);
                if a > 0.0 {
                    for k in 0..3 {
                        o[k] = (s[k] * sa + o[k] * da * (1.0 - sa)) / a;
                    }
                }
                o[3] = a;
            }
        }
        out
    }

    /// Renders into a new surface of `format` over `clip` (typically the canvas).
    pub fn render(&self, format: PixelFormat, clip: Rect) -> Surface {
        let mut s = Surface::new(format);
        let Some(b) = self.bounds else { return s };
        let area = b.intersect(&clip);
        let ch = format.channels();
        render_bands(&mut s, area, &|band| {
            let rgba = self.render_rgba(band);
            let mut vals = vec![0.0f32; rgba.len() * ch];
            for (p, o) in rgba.iter().zip(vals.chunks_exact_mut(ch)) {
                if p[3] > 0.0 {
                    photocraft_raster::from_rgba_into(&format, *p, o);
                }
            }
            let alpha = format.alpha;
            band_tiles(band, &vals, ch, |px| if alpha { px[ch - 1] > 0.0 } else { px.iter().any(|v| *v != 0.0) })
        });
        s
    }
}

/// Renders a shape layer's appearance (fill, then stroke over it) in `format`, clipped to `clip`.
pub fn render_shape(shape: &ShapeLayer, format: PixelFormat, clip: Rect) -> Surface {
    CompiledShape::new(shape, DEFAULT_TOLERANCE).render(format, clip)
}

#[cfg(test)]
mod tests;

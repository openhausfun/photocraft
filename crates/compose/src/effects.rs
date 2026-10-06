//! CPU reference renderer for layer effects (layer styles).
//!
//! Render order, bottom to top (Photoshop's layer-style stack read upwards):
//! drop shadows, outer glows, outer bevel, then the layer itself (content at
//! fill opacity) with pattern/gradient/colour overlays, satin, inner glows,
//! inner shadows, strokes and the inner bevel on top. Exterior effects blend
//! straight into the backdrop with their own modes; the layer and its
//! interior effects blend with the layer's mode. Emboss and pillow emboss then
//! shade the composited result (inside and outside halves). Layer opacity
//! applies to the whole stack; fill opacity only to the layer's own pixels.
//!
//! Interior effects are painted relative to the layer's shape and then take the shape's alpha
//! (an overlay recolours a half-transparent edge without adding coverage). The outside parts
//! of strokes blend onto the backdrop with their own modes, an upper stroke covering the
//! lower ones. Drop shadows are knocked out only through see-through fill.
//!
//! Shapes come from the layer's alpha (after its mask). Strokes and spread / choke measure
//! distances with a 5 × 5 chamfer metric seeded at sub-pixel edge offsets (as Photoshop does);
//! precise glows and chiselled bevels use an exact Euclidean distance transform. Soft falloffs
//! are two box blurs (a tent).

use photocraft_color::blend::BlendMode;
use photocraft_doc::Pattern;
use photocraft_doc::{
    Bevel, BevelStyle, BevelTechnique, Contour, Effect, FxPaint, GlobalLight, Glow, GlowSource, GlowTechnique, Gradient, GradientStyle, Layer, Shadow,
    StrokePosition,
};
use photocraft_geom::Rect;

use crate::pattern::{PREPARED_PATTERN_BYTES, Placement, PreparedPatterns};
use crate::{Buffer, psblend};

/// `true` if the layer has at least one enabled effect and the master switch is on.
pub fn has_effects(layer: &Layer) -> bool {
    layer.effects.enabled && layer.effects.items.iter().any(Effect::enabled)
}

/// Pixels an effect stack can reach beyond the layer's shape.
pub fn margin(layer: &Layer) -> i32 {
    let mut m = 0.0f32;
    for e in layer.effects.items.iter().filter(|e| e.enabled()) {
        let r = match e {
            Effect::DropShadow(s) | Effect::InnerShadow(s) => s.distance + s.size,
            Effect::OuterGlow(g) | Effect::InnerGlow(g) => g.size,
            Effect::Stroke(s) => s.size,
            Effect::Satin(s) => s.distance + s.size,
            Effect::BevelEmboss(b) => b.size + b.soften,
            _ => 0.0,
        };
        if r.is_finite() {
            m = m.max(r);
        }
    }
    // Bounded so malformed values can't request huge buffers; effects that
    // reach further are clipped at this distance.
    m.clamp(0.0, MAX_REACH).ceil() as i32 + 2
}

/// Maximum distance (px) an effect is rendered from the layer's shape.
pub const MAX_REACH: f32 = 512.0;

/// A single-channel map over a rectangle.
#[derive(Clone)]
struct Map {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Map {
    fn new(w: usize, h: usize, fill: f32) -> Self {
        Map { w, h, v: vec![fill; w * h] }
    }
    fn get(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 { 0.0 } else { self.v[y as usize * self.w + x as usize] }
    }
    /// Shifted copy (bilinear), outside reads `outside`.
    fn shifted(&self, dx: f32, dy: f32, outside: f32) -> Map {
        let mut out = Map::new(self.w, self.h, 0.0);
        let (fx, fy) = (dx.floor(), dy.floor());
        let (ax, ay) = (dx - fx, dy - fy);
        let s = |x: i64, y: i64| if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 { outside } else { self.v[y as usize * self.w + x as usize] };
        for y in 0..self.h as i64 {
            for x in 0..self.w as i64 {
                let (sx, sy) = (x - fx as i64, y - fy as i64);
                let v00 = s(sx, sy);
                let v10 = s(sx - 1, sy);
                let v01 = s(sx, sy - 1);
                let v11 = s(sx - 1, sy - 1);
                let top = v00 * (1.0 - ax) + v10 * ax;
                let bot = v01 * (1.0 - ax) + v11 * ax;
                out.v[y as usize * self.w + x as usize] = top * (1.0 - ay) + bot * ay;
            }
        }
        out
    }
    fn map(mut self, f: impl Fn(f32) -> f32) -> Self {
        for v in &mut self.v {
            *v = f(*v);
        }
        self
    }
}

/// 1D squared distance transform (Felzenszwalb & Huttenlocher).
fn dt1(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32], near: &mut [usize]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0 and parabola dominates: replace.
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
        let p = v[k];
        let d = q as f32 - p as f32;
        *o = d * d + f[p];
        near[q] = p;
    }
}

/// Exact Euclidean distance from every pixel to the nearest pixel where
/// `inside` is true (0 on those pixels).
#[cfg(test)]
fn edt(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    edt_nearest(inside, w, h).0
}

/// [`edt`] plus the index of the nearest `inside` pixel.
fn edt_nearest(inside: &[bool], w: usize, h: usize) -> (Vec<f32>, Vec<usize>) {
    const INF: f32 = 1e20;
    let mut g: Vec<f32> = inside.iter().map(|&b| if b { 0.0 } else { INF }).collect();
    let mut near_row = vec![0usize; w * h];
    let mut nearest = vec![0usize; w * h];
    let n = w.max(h);
    let (mut f, mut o, mut v, mut z, mut nr) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0f32; n + 1], vec![0usize; n]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        dt1(&f[..h], &mut o[..h], &mut v, &mut z, &mut nr[..h]);
        for y in 0..h {
            g[y * w + x] = o[y];
            near_row[y * w + x] = nr[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        dt1(&f[..w], &mut o[..w], &mut v, &mut z, &mut nr[..w]);
        for x in 0..w {
            g[y * w + x] = o[x].sqrt();
            let px = nr[x];
            nearest[y * w + x] = near_row[y * w + px] * w + px;
        }
    }
    (g, nearest)
}

/// Chamfer distance transform with a 5 × 5 mask of exact Euclidean step lengths (1, √2, √5),
/// plus the index of the nearest `inside` pixel (0 on inside pixels). Photoshop's effect
/// distances follow this metric: its stroke corners measure 3.236 = √5 + 1 at offset (1, 3)
/// and 3.650 = √5 + √2 at (2, 3), where the exact distances are 3.162 and 3.606.
#[cfg(test)]
fn chamfer_nearest(inside: &[bool], w: usize, h: usize) -> (Vec<f32>, Vec<usize>) {
    chamfer_from(inside.iter().map(|&b| if b { 0.0 } else { CHAMFER_INF }).collect(), w, h)
}

const CHAMFER_INF: f32 = 1e20;

/// [`chamfer_nearest`] from per-pixel start values (`CHAMFER_INF` = not a seed): the result is
/// the minimum over seeds of start value + path length, so partly covered seeds can start at
/// their sub-pixel edge offset.
fn chamfer_from(mut d: Vec<f32>, w: usize, h: usize) -> (Vec<f32>, Vec<usize>) {
    const S2: f32 = std::f32::consts::SQRT_2;
    const S5: f32 = 2.236_068;
    // Forward-pass neighbours (already visited in raster order); the backward pass mirrors them.
    const FWD: [(i64, i64, f32); 8] = [(-1, 0, 1.0), (-1, -1, S2), (0, -1, 1.0), (1, -1, S2), (-1, -2, S5), (1, -2, S5), (-2, -1, S5), (2, -1, S5)];
    let mut near: Vec<usize> = (0..w * h).collect();
    let (wi, hi) = (w as i64, h as i64);
    let relax = |x: i64, y: i64, sign: i64, d: &mut Vec<f32>, near: &mut Vec<usize>| {
        let i = (y * wi + x) as usize;
        let mut best = d[i];
        let mut bn = near[i];
        for &(ox, oy, wt) in &FWD {
            let (nx, ny) = (x + ox * sign, y + oy * sign);
            if nx < 0 || ny < 0 || nx >= wi || ny >= hi {
                continue;
            }
            let j = (ny * wi + nx) as usize;
            let c = d[j] + wt;
            if c < best {
                best = c;
                bn = near[j];
            }
        }
        d[i] = best;
        near[i] = bn;
    };
    for y in 0..hi {
        for x in 0..wi {
            relax(x, y, 1, &mut d, &mut near);
        }
    }
    for y in (0..hi).rev() {
        for x in (0..wi).rev() {
            relax(x, y, -1, &mut d, &mut near);
        }
    }
    (d, near)
}

/// Distance from each pixel centre outside the shape to the shape's edge.
/// Pixels with non-zero alpha are inside; the edge inside the nearest such
/// pixel is placed by its alpha (coverage), as Photoshop does. Inside pixels
/// get ≤ 0.
fn dist_outside(s: &Map) -> Vec<f32> {
    dist_outside_by(s, Metric::Euclidean)
}

/// Distance metric for effect distance fields.
#[derive(Clone, Copy, PartialEq)]
enum Metric {
    /// Exact Euclidean (precise glows, bevels).
    Euclidean,
    /// 5 × 5 chamfer (strokes, spread / choke; see [`chamfer_nearest`]).
    Chamfer,
}

fn dist_outside_by(s: &Map, m: Metric) -> Vec<f32> {
    if m == Metric::Chamfer {
        // Seeds start at their sub-pixel edge offset (1 − coverage).
        let start = s.v.iter().map(|&a| if a > INSIDE_EPS { 1.0 - a.min(1.0) } else { CHAMFER_INF }).collect();
        let (d, _) = chamfer_from(start, s.w, s.h);
        return d.iter().zip(&s.v).map(|(d, &a)| if a > INSIDE_EPS { -0.5 } else { d - 0.5 }).collect();
    }
    let inside: Vec<bool> = s.v.iter().map(|&a| a > INSIDE_EPS).collect();
    let (d, near) = edt_nearest(&inside, s.w, s.h);
    (0..d.len()).map(|i| if inside[i] { -0.5 } else { d[i] - 0.5 + (1.0 - s.v[near[i]].min(1.0)) }).collect()
}

/// Distance from each pixel centre inside the shape to the shape's edge
/// (≤ 0 outside).
fn dist_inside(s: &Map) -> Vec<f32> {
    dist_inside_by(s, Metric::Euclidean)
}

fn dist_inside_by(s: &Map, m: Metric) -> Vec<f32> {
    if m == Metric::Chamfer {
        // Partly covered edge pixels (alpha below their neighbourhood's) are seeds starting at
        // their coverage, mirroring `dist_outside`: a 25 % edge column puts the edge 0.25 px in.
        let cov = local_coverage(s);
        let start =
            s.v.iter()
                .zip(&cov.v)
                .map(|(&a, &c)| {
                    if a <= INSIDE_EPS {
                        0.0
                    } else if c < 1.0 - INSIDE_EPS {
                        c
                    } else {
                        CHAMFER_INF
                    }
                })
                .collect();
        let (d, _) = chamfer_from(start, s.w, s.h);
        return d.iter().zip(&s.v).map(|(d, &a)| if a <= INSIDE_EPS { -0.5 } else { d - 0.5 }).collect();
    }
    let outside: Vec<bool> = s.v.iter().map(|&a| a <= INSIDE_EPS).collect();
    let (d, _) = edt_nearest(&outside, s.w, s.h);
    d.iter().zip(&outside).map(|(d, o)| if *o { -0.5 } else { d - 0.5 }).collect()
}

/// Alpha relative to the 3×3 neighbourhood maximum: separates edge coverage
/// from the fill's own opacity on shape layers.
fn local_coverage(s: &Map) -> Map {
    let mut out = s.clone();
    for y in 0..s.h as i64 {
        for x in 0..s.w as i64 {
            let a = s.get(x, y);
            if a <= INSIDE_EPS {
                continue;
            }
            let mut mx = a;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    mx = mx.max(s.get(x + dx, y + dy));
                }
            }
            out.v[y as usize * s.w + x as usize] = (a / mx).min(1.0);
        }
    }
    out
}

/// Alpha above which a pixel belongs to the layer's shape (half an 8-bit step).
const INSIDE_EPS: f32 = 0.5 / 255.0;

/// The soft falloff of an effect of `size` pixels: two box passes of width `size` (a tent),
/// as Photoshop does. Fitted on psd-tools layer_effects.psd against the earlier Gaussian
/// (sigma = 0.4 × size): drop shadow coverage error 0.0064 → 0.0038, satin 0.014 → 0.002.
fn blur(m: &mut Map, size: f32) {
    if size.is_finite() {
        tent(m, size);
    }
}

/// Grows the shape by `r` pixels (anti-aliased), keeping the original soft edge. Distances use
/// the 5 × 5 chamfer metric, as for strokes: Photoshop's spread of a group drop shadow around an
/// ellipse (ag-psd group-drop-shadows) is round at 0° and 45° and falls short in between, peaking
/// near 15° where the chamfer metric overestimates most (5/255 lighter with an exact Euclidean
/// dilation, within 2/255 with the chamfer).
fn dilate(s: &Map, r: f32) -> Map {
    if r <= 0.0 {
        return s.clone();
    }
    let d = dist_outside_by(s, Metric::Chamfer);
    let mut out = s.clone();
    for (o, d) in out.v.iter_mut().zip(d) {
        // Partly covered pixels (distance sentinel < 0) grow by `r` from their own coverage, so a
        // 2 % spread doesn't make every anti-aliased edge pixel opaque (ag-psd drop shadow).
        *o = if d < 0.0 { (*o + r).min(1.0) } else { o.max((r + 0.5 - d).clamp(0.0, 1.0)) };
    }
    out
}

fn contour_lut(c: &Contour) -> Option<Vec<f32>> {
    match c {
        Contour::Linear => None,
        Contour::Custom { points, .. } => Some(crate::adjust::curve_lut(points)),
    }
}

/// Interpolated lookup in a 0..=1 table (as the GPU's `lut()`).
fn lut_at(l: &[f32], v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * (l.len() - 1) as f32;
    let i = x.floor() as usize;
    let j = (i + 1).min(l.len() - 1);
    l[i] + (l[j] - l[i]) * (x - i as f32)
}

/// A glow's coverage transfer: its contour applied over the Range (Photoshop maps the glow's
/// coverage `v` through `contour(min(v / range, 1))`, so the default 50 % doubles a blurred
/// edge's 0.5 to full strength). `None` = identity.
pub fn glow_lut(g: &Glow) -> Option<Vec<f32>> {
    ranged_lut(&g.contour, g.range)
}

/// A contour applied over a Range (`contour(min(v / range, 1))`); `None` = identity.
pub fn ranged_lut(contour: &Contour, range: f32) -> Option<Vec<f32>> {
    let base = contour_lut(contour);
    let range = if range.is_finite() { range.clamp(0.01, 1.0) } else { 1.0 };
    if range >= 0.999 {
        return base;
    }
    let n = 4096;
    Some(
        (0..n)
            .map(|k| {
                let v = (k as f32 / (n - 1) as f32 / range).min(1.0);
                base.as_ref().map_or(v, |l| lut_at(l, v))
            })
            .collect(),
    )
}

fn apply_lut(m: Map, l: Option<Vec<f32>>) -> Map {
    match l {
        None => m,
        Some(l) => m.map(|v| lut_at(&l, v)),
    }
}

fn apply_contour(m: Map, c: &Contour) -> Map {
    match contour_lut(c) {
        None => m,
        Some(l) => m.map(|v| {
            let x = v.clamp(0.0, 1.0) * (l.len() - 1) as f32;
            let i = x.floor() as usize;
            let j = (i + 1).min(l.len() - 1);
            l[i] + (l[j] - l[i]) * (x - i as f32)
        }),
    }
}

fn rgb(c: &photocraft_color::Color) -> [f32; 3] {
    c.to_rgb()
}

/// Offset of an effect lit from `angle` at `distance` (shadow falls away from
/// the light), rounded to whole pixels as Photoshop does.
fn offset(angle: f32, distance: f32) -> (f32, f32) {
    let a = angle.to_radians();
    ((-a.cos() * distance).round(), (a.sin() * distance).round())
}

/// The lengths a 100 % gradient at `angle` spans in a `w` × `h` frame: the chord (Linear and
/// Reflected) and the ellipse-norm length (Radial, Angle, Diamond; its half is the radius).
pub fn gradient_units(angle: f32, w: f32, h: f32) -> (f32, f32) {
    let (s, c) = angle.to_radians().sin_cos();
    // Gradient length: the bounds' extent along the angle as an ellipse
    // norm (a unit gradient scaled to the bounds); fitted on psd-tools
    // gradient-styles.psd (cached Photoshop renderings of every style).
    let len = ((c * w).powi(2) + (s * h).powi(2)).sqrt().max(1.0);
    // Linear / Reflected span the chord of the bounds through their centre along the angle:
    // min(w / |cos|, h / |sin|). A 45° gradient on a 29 px square runs 41 px (psd-tools
    // shape-fx2), an 87° one on a 600 × 60 text line 60 px (layer_effects); axis-aligned angles
    // span the width / height.
    let chord = (w / c.abs().max(1e-6)).min(h / s.abs().max(1e-6)).max(1.0);
    (chord, len)
}

/// Gradient parameter `t` in `0..=1` for pixel centre `(x, y)` inside `bounds`.
#[allow(clippy::too_many_arguments)]
pub fn gradient_t(style: GradientStyle, angle: f32, scale: f32, reverse: bool, offset: (f32, f32), bounds: Rect, x: f32, y: f32) -> f32 {
    let w = bounds.width().max(1) as f32;
    let h = bounds.height().max(1) as f32;
    let cx = bounds.x0 as f32 + w / 2.0 + offset.0 * w;
    let cy = bounds.y0 as f32 + h / 2.0 + offset.1 * h;
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    // Distance along the gradient direction (y axis points down).
    let along = dx * c - dy * s;
    let across = dx * s + dy * c;
    let (chord, len) = gradient_units(angle, w, h);
    let (chord, len) = (chord * scale.max(1e-3), len * scale.max(1e-3));
    // Linear / Reflected sample the pixel's top-left corner, half a pixel before its centre
    // (with the whole-pixel end points of `fill_layout`: layer_effects' overlay 5.8 → 1.8/255,
    // shape-fx2 2.8 → 1.1/255, gradient-fill.psd 1.9 → 0.5/255).
    let corner = 0.5 * (c - s);
    let mut t = match style {
        GradientStyle::Linear => (along - corner) / chord + 0.5,
        GradientStyle::Reflected => ((along - corner) / (chord / 2.0)).abs(),
        GradientStyle::Radial => (dx * dx + dy * dy).sqrt() / (len / 2.0),
        GradientStyle::Diamond => (along.abs() + across.abs()) / (len / 2.0),
        // Clockwise sweep starting at the gradient angle.
        GradientStyle::Angle => ((angle.to_radians() - (-dy).atan2(dx)) / std::f32::consts::TAU).rem_euclid(1.0),
    };
    t = t.clamp(0.0, 1.0);
    if reverse { 1.0 - t } else { t }
}

/// Samples colour and opacity stops at `t`.
pub fn sample_gradient(g: &Gradient, t: f32) -> [f32; 4] {
    PreparedGradient::new(g).sample(t)
}

// Prepared only for one paint call: CMYK conversions must use that call's active profile.
struct PreparedGradient {
    color: Vec<(f32, [f32; 3])>,
    opacity: Vec<(f32, [f32; 3])>,
}

impl PreparedGradient {
    fn new(g: &Gradient) -> Self {
        Self { color: g.stops.iter().map(|(p, c)| (*p, rgb(c))).collect(), opacity: g.opacity_stops.iter().map(|(p, a)| (*p, [*a; 3])).collect() }
    }

    fn sample(&self, t: f32) -> [f32; 4] {
        let color = sample_stops(&self.color, t);
        let alpha = if self.opacity.is_empty() { 1.0 } else { sample_stops(&self.opacity, t)[0] };
        [color[0], color[1], color[2], alpha]
    }
}

fn sample_stops(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    match stops {
        [] => [t; 3],
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

/// Composite `color × coverage` into `dst` (both over the same rectangle).
fn paint(dst: &mut Buffer, m: &Map, color: impl Fn(usize) -> [f32; 4], blend: BlendMode, opacity: f32) {
    for (i, p) in dst.px.iter_mut().enumerate() {
        let a = m.v[i] * opacity;
        if a <= 0.0 {
            continue;
        }
        let c = color(i);
        *p = psblend::composite(blend, *p, [c[0], c[1], c[2], c[3] * a], 1.0);
    }
}

fn paint_color(dst: &mut Buffer, m: &Map, c: [f32; 3], blend: BlendMode, opacity: f32) {
    paint(dst, m, |_| [c[0], c[1], c[2], 1.0], blend, opacity);
}

/// Spread / choke of a soft effect: (dilation radius, blur width). The dilation is whole pixels
/// (`size × spread` rounded: a 2 % spread of a 5 px shadow doesn't grow it at all), the blur
/// keeps `size × (1 − spread)` (ag-psd effects 10.6 → 3.8 % of pixels off).
pub fn spread_split(size: f32, spread: f32) -> (f32, f32) {
    ((size * spread).round(), size * (1.0 - spread))
}

fn shadow_map(shape: &Map, s: &Shadow, light: &GlobalLight, inner: bool) -> Map {
    let angle = if s.use_global_light { light.angle } else { s.angle };
    let (dx, dy) = offset(angle, s.distance);
    let src = if inner { shape.clone().map(|a| 1.0 - a) } else { shape.clone() };
    let mut m = src.shifted(dx, dy, if inner { 1.0 } else { 0.0 });
    let (r, bw) = spread_split(s.size, s.spread);
    m = dilate(&m, r);
    blur(&mut m, bw);
    let mut m = apply_contour(m, &s.contour);
    if inner {
        for (v, a) in m.v.iter_mut().zip(&shape.v) {
            *v *= a;
        }
    }
    m
}

fn glow_map(shape: &Map, g: &Glow, inner: bool) -> Map {
    let src = if inner {
        match g.source {
            GlowSource::Edge => shape.clone().map(|a| 1.0 - a),
            GlowSource::Center => shape.clone(),
        }
    } else {
        shape.clone()
    };
    let mut m = match g.technique {
        GlowTechnique::Precise => {
            let d = if inner && g.source == GlowSource::Edge { dist_inside(shape) } else { dist_outside(&src) };
            let solid = g.size * g.spread;
            let soft = (g.size - solid).max(1e-3);
            let mut m = Map::new(shape.w, shape.h, 0.0);
            for (o, d) in m.v.iter_mut().zip(d) {
                *o = if d <= solid { 1.0 } else { (1.0 - (d - solid) / soft).clamp(0.0, 1.0) };
            }
            if inner && g.source == GlowSource::Center {
                m = m.map(|v| 1.0 - v);
            }
            m
        }
        GlowTechnique::Softer => {
            let (r, bw) = spread_split(g.size, g.spread);
            let mut m = dilate(&src, r);
            blur(&mut m, bw);
            if inner && g.source == GlowSource::Center {
                // Brightest in the middle: coverage from the distance to the edge.
                let mut e = dilate(&shape.clone().map(|a| 1.0 - a), r);
                blur(&mut e, bw);
                m = e.map(|v| 1.0 - v);
            }
            m
        }
    };
    m = apply_lut(m, glow_lut(g));
    if inner {
        for (v, a) in m.v.iter_mut().zip(&shape.v) {
            *v *= a;
        }
    }
    m
}

/// Paints a pattern (looked up in `patterns`) through coverage `m`; missing patterns paint nothing.
#[allow(clippy::too_many_arguments)]
fn paint_pattern(
    dst: &mut Buffer,
    m: &Map,
    patterns: &PreparedPatterns<'_>,
    name: &str,
    id: &str,
    place: Placement,
    big: Rect,
    blend: BlendMode,
    opacity: f32,
) {
    let Some(tile) = patterns.get(id, name) else { return };
    let w = big.width() as usize;
    paint(
        dst,
        m,
        |i| {
            let (x, y) = (f64::from(big.x0 + (i % w) as i32) + 0.5, f64::from(big.y0 + (i / w) as i32) + 0.5);
            let (u, v) = place.map(x, y);
            tile.sample(u, v)
        },
        blend,
        opacity,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_fx(
    dst: &mut Buffer,
    m: &Map,
    p: &FxPaint,
    shape_bounds: Rect,
    anchor: (f64, f64),
    big: Rect,
    blend: BlendMode,
    opacity: f32,
    patterns: &PreparedPatterns<'_>,
) {
    match p {
        FxPaint::Color(c) => paint_color(dst, m, rgb(c), blend, opacity),
        FxPaint::Gradient(g) => {
            let prepared = PreparedGradient::new(g);
            let (angle, scale, offset) = crate::fill_layout::gradient_layout(g.style, g.angle, g.scale, g.offset, shape_bounds);
            let w = big.width() as usize;
            paint(
                dst,
                m,
                |i| {
                    let (x, y) = ((big.x0 + (i % w) as i32) as f32 + 0.5, (big.y0 + (i / w) as i32) as f32 + 0.5);
                    prepared.sample(gradient_t(g.style, angle, scale, g.reverse, offset, shape_bounds, x, y))
                },
                blend,
                opacity,
            )
        }
        FxPaint::Pattern { name, id, scale } => {
            paint_pattern(dst, m, patterns, name, id, Placement::anchored(anchor, true, (0.0, 0.0), *scale, 0.0), big, blend, opacity)
        }
    }
}

/// Normalised weights of a centred box of (fractional) width `w`: tap `i` gets the overlap of
/// `[i - 0.5, i + 0.5]` with `[-w/2, w/2]`.
fn box_weights(w: f32) -> Vec<f32> {
    let w = w.max(1.0);
    let half = w / 2.0;
    let r = (half - 0.5).ceil().max(0.0) as i64;
    let v: Vec<f32> = (-r..=r).map(|i| ((i as f32 + 0.5).min(half) - (i as f32 - 0.5).max(-half)).max(0.0)).collect();
    let sum: f32 = v.iter().sum();
    v.iter().map(|x| x / sum).collect()
}

/// The 1D kernel of two box passes of width `w` (a tent of half-width ≈ `w`): `(radius, weights)`.
/// Photoshop's bevel height maps are this blur of the shape (fitted on the corpus: a smooth
/// inner bevel of size 5 matches within 0.7 % mean, size 41 within 2.6 %).
pub fn tent_kernel(w: f32) -> (i32, Vec<f32>) {
    let b = box_weights(w);
    let n = b.len() * 2 - 1;
    let mut t = vec![0.0f32; n];
    for (i, x) in b.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            t[i + j] += x * y;
        }
    }
    ((n as i32 - 1) / 2, t)
}

/// Box geometry for a (fractional) width: (`r`, end-tap weight `f`, 1 / width) — the
/// [`box_weights`] taps are `r - 1` full ones each side of the centre plus the two end taps at `f`.
fn box_geom(bw: f32) -> (i64, f64, f64) {
    let half = bw.max(1.0) / 2.0;
    let r = (half - 0.5).ceil().max(0.0) as i64;
    let f = f64::from((half - (r as f32 - 0.5)).clamp(0.0, 1.0));
    (r, f, 1.0 / f64::from(bw.max(1.0)))
}

/// One box pass over `src` (zero outside it) evaluated at `x0 .. x0 + dst.len()`, as a running
/// sum (O(1) per sample whatever the width).
fn box_line(src: &[f64], x0: i64, dst: &mut [f64], bw: f32) {
    let n = src.len() as i64;
    let (r, f, norm) = box_geom(bw);
    let at = |i: i64| if i >= 0 && i < n { src[i as usize] } else { 0.0 };
    if r == 0 {
        for (k, d) in dst.iter_mut().enumerate() {
            *d = at(x0 + k as i64);
        }
        return;
    }
    let mut inner: f64 = (x0 - (r - 1)..=x0 + (r - 1)).map(at).sum();
    for (k, d) in dst.iter_mut().enumerate() {
        let x = x0 + k as i64;
        *d = (inner + f * (at(x - r) + at(x + r))) * norm;
        inner += at(x + r) - at(x - r + 1);
    }
}

/// Two box passes along a line, exactly the convolution with [`tent_kernel`] (zero outside): the
/// first pass is evaluated `r` samples beyond each end so the second sees what lies there.
fn tent_line(src: &[f32], dst: &mut [f32], bw: f32) {
    let (r, _, _) = box_geom(bw);
    let s: Vec<f64> = src.iter().map(|v| f64::from(*v)).collect();
    let mut mid = vec![0.0f64; src.len() + 2 * r as usize];
    box_line(&s, -r, &mut mid, bw);
    let mut out = vec![0.0f64; src.len()];
    // `mid[k]` holds position k - r: output position p reads index p + r.
    box_line(&mid, r, &mut out, bw);
    for (d, v) in dst.iter_mut().zip(out) {
        *d = v as f32;
    }
}

/// Rows of a `w`-wide row-major buffer, each through `f(src_row, dst_row)`, in parallel on
/// scoped OS threads. Not rayon: maps can be built from inside a rayon tile, where a waiting
/// worker steals other tiles. Map builds no longer block on each other (#276), but the blur
/// keeps out of the tile pool.
fn rows_par(src: &[f32], dst: &mut [f32], w: usize, f: impl Fn(&[f32], &mut [f32]) + Sync) {
    if w == 0 {
        return;
    }
    let threads =
        if cfg!(target_arch = "wasm32") || src.len() < 1 << 16 { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16) };
    if threads == 1 {
        dst.chunks_mut(w).zip(src.chunks(w)).for_each(|(d, s)| f(s, d));
        return;
    }
    let rows = (src.len() / w).div_ceil(threads);
    std::thread::scope(|sc| {
        for (d, s) in dst.chunks_mut(rows * w).zip(src.chunks(rows * w)) {
            let f = &f;
            sc.spawn(move || d.chunks_mut(w).zip(s.chunks(w)).for_each(|(d, s)| f(s, d)));
        }
    });
}

fn transpose_map(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; v.len()];
    const B: usize = 32;
    for by in (0..h).step_by(B) {
        for bx in (0..w).step_by(B) {
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    out[x * h + y] = v[y * w + x];
                }
            }
        }
    }
    out
}

/// Two box passes of width `w` along each axis (the tent [`tent_kernel`] describes, which the GPU
/// convolves directly).
fn tent(m: &mut Map, w: f32) {
    if tent_kernel(w).0 == 0 || m.v.is_empty() {
        return;
    }
    tent_fast(m, w);
}

/// [`tent`] as a direct convolution with [`tent_kernel`] (the GPU's way; tests compare).
#[cfg(test)]
fn tent_direct(m: &mut Map, w: f32) {
    {
        let (r, k) = tent_kernel(w);
        let (mw, mh) = (m.w as i64, m.h as i64);
        let mut tmp = vec![0.0f32; m.v.len()];
        for y in 0..mh {
            for x in 0..mw {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let xx = x + i as i64 - r as i64;
                    if xx >= 0 && xx < mw {
                        acc += m.v[(y * mw + xx) as usize] * kv;
                    }
                }
                tmp[(y * mw + x) as usize] = acc;
            }
        }
        for y in 0..mh {
            for x in 0..mw {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let yy = y + i as i64 - r as i64;
                    if yy >= 0 && yy < mh {
                        acc += tmp[(yy * mw + x) as usize] * kv;
                    }
                }
                m.v[(y * mw + x) as usize] = acc;
            }
        }
    }
}

fn tent_fast(m: &mut Map, w: f32) {
    let (mw, mh) = (m.w, m.h);
    let twice = |s: &[f32], d: &mut [f32]| tent_line(s, d, w);
    let mut a = vec![0.0f32; m.v.len()];
    rows_par(&m.v, &mut a, mw, twice);
    let t = transpose_map(&a, mw, mh);
    let mut b = vec![0.0f32; t.len()];
    rows_par(&t, &mut b, mh, twice);
    m.v = transpose_map(&b, mh, mw);
}

/// Where a bevel's maps paint: inside the shape (with the layer), outside it (onto the
/// backdrop), or both (emboss styles: maps 0–1 inside, 2–3 outside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BevelPaint {
    Inner,
    Outer,
    Both,
}

/// Height below which an outer bevel pixel counts as off the bevel (beyond the blur's reach):
/// above float noise of the running-sum blur (~3e-7), far below anything that shades.
pub const BEVEL_H_EPS: f32 = 1e-5;

/// Bevel geometry shared by the CPU maps and the GPU programs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BevelGeom {
    pub paint: BevelPaint,
    /// Box width of the smooth height blur (size; half the size, rounded up, for emboss styles).
    pub width: f32,
    /// Height-map gradient scale: depth × width (smooth) or depth × size (chisel), signed by
    /// direction.
    pub depth: f32,
    /// Pillow emboss: the outside slopes face the other way.
    pub pillow: bool,
    /// Chisel Soft: extra tent width over the chiselled height map.
    pub chisel_soft: f32,
}

pub fn bevel_geom(b: &Bevel) -> BevelGeom {
    let size = b.size.max(1.0);
    let emboss = matches!(b.style, BevelStyle::Emboss | BevelStyle::PillowEmboss);
    let paint = match b.style {
        BevelStyle::OuterBevel => BevelPaint::Outer,
        BevelStyle::Emboss | BevelStyle::PillowEmboss => BevelPaint::Both,
        BevelStyle::InnerBevel | BevelStyle::StrokeEmboss => BevelPaint::Inner,
    };
    // Emboss styles straddle the edge with half the size, rounded up to whole pixels: a 41 px
    // emboss blurs over 21 (psd-tools layer_effects: 20.5 left narrow letter parts up to
    // 5.7/255 dark, 21 is within 2.2).
    let width = if emboss { (size / 2.0).ceil() } else { size };
    let smooth = b.technique == BevelTechnique::Smooth;
    let sign = if b.up { 1.0 } else { -1.0 };
    BevelGeom {
        paint,
        width,
        depth: b.depth * if smooth { width } else { size } * sign,
        pillow: b.style == BevelStyle::PillowEmboss,
        chisel_soft: if b.technique == BevelTechnique::ChiselSoft { (size / 4.0).max(1.0) } else { 0.0 },
    }
}

/// Bevel height map in `0..=1`. Smooth: the shape blurred by two box passes of the bevel
/// width. Chisel: linear ramps of the exact distance to the edge (size wide; emboss styles
/// straddle the edge), slightly blurred for Chisel Soft. Soften blurs the result.
fn bevel_height(shape: &Map, b: &Bevel, g: &BevelGeom, tex: &TextureCtx, patterns: &PreparedPatterns<'_>) -> Map {
    let size = b.size.max(1.0);
    let mut h = if b.technique == BevelTechnique::Smooth {
        let mut h = shape.clone();
        tent(&mut h, g.width);
        h
    } else {
        let (din, dout) = (dist_inside(shape), dist_outside(shape));
        let mut h = Map::new(shape.w, shape.h, 0.0);
        for i in 0..h.v.len() {
            h.v[i] = bevel_chisel_h(g.paint, size, din[i], dout[i]);
        }
        if g.chisel_soft > 0.0 {
            tent(&mut h, g.chisel_soft);
        }
        h
    };
    // Contour element: the height profile through the contour over its range.
    if let Some(c) = &b.contour {
        h = apply_lut(h, Some(ranged_lut(&c.contour, c.range).unwrap_or_else(|| (0..4096).map(|k| k as f32 / 4095.0).collect())));
    }
    // Texture element: the pattern's luminance as extra height, scaled so a full-contrast step
    // at 100 % depth slopes like the bevel at its depth.
    if let Some(t) = &b.texture
        && let Some(tile) = patterns.get(&t.id, &t.name)
    {
        let unit = if b.depth.abs() > 1e-6 { (g.depth / b.depth).abs().max(1e-3) } else { g.width.max(1.0) };
        let place = Placement::anchored(tex.anchor, t.link, t.phase, t.scale, 0.0);
        let k = t.depth / unit;
        let (x0, y0) = (tex.rect.x0, tex.rect.y0);
        for y in 0..h.h {
            for x in 0..h.w {
                let (u, v) = place.map(f64::from(x0 + x as i32) + 0.5, f64::from(y0 + y as i32) + 0.5);
                let p = tile.sample(u, v);
                let l = (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) * p[3];
                h.v[y * h.w + x] += k * if t.invert { 1.0 - l } else { l };
            }
        }
    }
    if b.soften >= 1.0 {
        tent(&mut h, b.soften);
    }
    h
}

/// Where a bevel texture tiles: the maps' rectangle, the document's patterns and the linked
/// origin (the effects reference point, else the layer's top-left).
pub struct TextureCtx<'a> {
    pub rect: Rect,
    pub patterns: &'a [Pattern],
    pub anchor: (f64, f64),
}

/// Chiselled height from the inside / outside distances (keep in sync with `fs_mbevelh`).
pub fn bevel_chisel_h(paint: BevelPaint, size: f32, din: f32, dout: f32) -> f32 {
    match paint {
        BevelPaint::Outer => 1.0 - (dout / size).clamp(0.0, 1.0),
        BevelPaint::Both => {
            if din > 0.0 {
                0.5 + 0.5 * (din / (size / 2.0)).clamp(0.0, 1.0)
            } else {
                0.5 - 0.5 * (dout / (size / 2.0)).clamp(0.0, 1.0)
            }
        }
        BevelPaint::Inner => (din / size).clamp(0.0, 1.0),
    }
}

/// Bevel highlight and shadow maps: `[hi, sh]` (inner or outer) or `[hi, sh, hi_out, sh_out]`
/// (emboss styles).
fn bevel_maps(shape: &Map, b: &Bevel, light: &GlobalLight, tex: &TextureCtx, patterns: &PreparedPatterns<'_>) -> (Vec<Map>, BevelPaint) {
    let g = bevel_geom(b);
    let hmap = bevel_height(shape, b, &g, tex, patterns);
    let (angle, altitude) = if b.use_global_light { (light.angle, light.altitude) } else { (b.angle, b.altitude) };
    let (sa, ca) = angle.to_radians().sin_cos();
    let (se, ce) = altitude.to_radians().sin_cos();
    let light_v = [ca * ce, -sa * ce, se];
    // Region: 0 = inside (× the shape's alpha), 1 = under the layer's edge and outside.
    // A pixel is on the bevel where its height is raised. Emboss styles also shade the last pixel
    // past the blur's reach (a raised 4-neighbour: it still slopes; psd-tools layer_effects'
    // emboss shadow runs one row past it), outer bevels don't (Photoshop oracle
    // bevel-outer-smooth).
    let reach_past = g.paint == BevelPaint::Both;
    let on_bevel = |x: i64, y: i64| {
        hmap.get(x, y) > BEVEL_H_EPS || (reach_past && [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| hmap.get(x + dx, y + dy) > BEVEL_H_EPS))
    };
    let shade_into = |depth: f32, region_kind: u8| -> (Map, Map) {
        let (mut hi, mut sh) = (Map::new(shape.w, shape.h, 0.0), Map::new(shape.w, shape.h, 0.0));
        for y in 0..shape.h as i64 {
            for x in 0..shape.w as i64 {
                let i = y as usize * shape.w + x as usize;
                let gx = (hmap.get(x + 1, y) - hmap.get(x - 1, y)) * 0.5 * depth;
                let gy = (hmap.get(x, y + 1) - hmap.get(x, y - 1)) * 0.5 * depth;
                let n = [-gx, -gy, 1.0];
                let len = (n[0] * n[0] + n[1] * n[1] + 1.0).sqrt();
                let shade = (n[0] * light_v[0] + n[1] * light_v[1] + n[2] * light_v[2]) / len;
                let region = if region_kind == 0 { shape.v[i] } else { f32::from(shape.v[i] < 1.0 - INSIDE_EPS && on_bevel(x, y)) };
                let k = shade - se;
                if k > 0.0 {
                    hi.v[i] = (k / (1.0 - se).max(1e-3)).clamp(0.0, 1.0) * region;
                } else {
                    sh.v[i] = (-k / se.max(1e-3)).clamp(0.0, 1.0) * region;
                }
            }
        }
        (apply_contour(hi, &b.gloss_contour), apply_contour(sh, &b.gloss_contour))
    };
    let maps = match g.paint {
        BevelPaint::Inner | BevelPaint::Outer => {
            let (hi, sh) = shade_into(g.depth, u8::from(g.paint == BevelPaint::Outer));
            vec![hi, sh]
        }
        BevelPaint::Both => {
            // One map per colour over the whole bevel: the inside half where the shape is (× its
            // alpha), the outside half (pillow: facing the other way) for the rest of each pixel.
            // A 1 % edge pixel of a pillow emboss takes the outside shading (Photoshop oracle
            // bevel-pillow-smooth), a 75 % one of an emboss mostly the inside (layer_effects).
            let (hi, sh) = shade_into(g.depth, 0);
            let (ho, so) = shade_into(if g.pillow { -g.depth } else { g.depth }, 1);
            let mix = |a: Map, b: Map| -> Map {
                let v = a.v.iter().zip(&b.v).zip(&shape.v).map(|((x, y), s)| if *s > INSIDE_EPS { *x } else { 0.0 } + y * (1.0 - s.min(1.0))).collect();
                Map { w: a.w, h: a.h, v }
            };
            vec![mix(hi, ho), mix(sh, so)]
        }
    };
    (maps, g.paint)
}

/// Effect maps derived from a layer's shape (its alpha), computed once over the layer's whole
/// region and cropped per render tile. They depend only on the shape, the effect settings and the
/// global light, never on the backdrop, so they can be cached across tiles and edits.
pub struct FxMaps {
    rect: Rect,
    shape: Map,
    /// Per enabled effect (in `items` order): the maps it paints through.
    per: Vec<Vec<Map>>,
    bevel_paint: Vec<BevelPaint>,
    din: Option<Vec<f32>>,
    dout: Option<Vec<f32>>,
    /// Shape layers: distance outside the vector outline (from local coverage).
    vdout: Option<Vec<f32>>,
    /// `shape` joins the alpha with a filled shape's outline (`crate::effect_outline`).
    outline: bool,
    /// Frames of gradient strokes by outside width (bits): see [`stroke_frame`].
    frames: Vec<(u32, Rect)>,
}

impl FxMaps {
    /// The frame of a gradient stroke `st` of this layer (see [`stroke_frame`]), if built.
    pub fn stroke_frame(&self, st: &photocraft_doc::StrokeFx) -> Option<Rect> {
        self.frames.iter().find(|f| f.0 == stroke_widths(st).1.to_bits()).map(|f| f.1)
    }
}

impl FxMaps {
    /// Approximate heap size (for cache budgeting).
    pub fn bytes(&self) -> usize {
        let n = self.shape.v.len();
        let maps: usize = self.per.iter().map(Vec::len).sum();
        (1 + maps + usize::from(self.din.is_some()) + usize::from(self.dout.is_some()) + usize::from(self.vdout.is_some())) * n * 4
    }

    fn crop_vec(&self, v: &[f32], to: Rect, fill: f32) -> Vec<f32> {
        let (w, h) = (to.width() as usize, to.height() as usize);
        let mut out = vec![fill; w * h];
        let r = self.rect;
        let inter = r.intersect(&to);
        if inter.is_empty() {
            return out;
        }
        let sw = r.width() as usize;
        for y in inter.y0..inter.y1 {
            let s0 = (y - r.y0) as usize * sw + (inter.x0 - r.x0) as usize;
            let d0 = (y - to.y0) as usize * w + (inter.x0 - to.x0) as usize;
            let n = inter.width() as usize;
            out[d0..d0 + n].copy_from_slice(&v[s0..s0 + n]);
        }
        out
    }

    fn crop(&self, m: &Map, to: Rect, fill: f32) -> Map {
        Map { w: to.width() as usize, h: to.height() as usize, v: self.crop_vec(&m.v, to, fill) }
    }
}

fn satin_map(shape: &Map, s: &photocraft_doc::effects::Satin) -> Map {
    let (dx, dy) = offset(s.angle, s.distance);
    let mut a = shape.shifted(dx, dy, 0.0);
    let mut b = shape.shifted(-dx, -dy, 0.0);
    // Two box passes of the satin size (fitted: 0.2 % mean error on psd-tools layer_effects).
    tent(&mut a, s.size);
    tent(&mut b, s.size);
    let mut m = Map { w: shape.w, h: shape.h, v: a.v.iter().zip(&b.v).map(|(x, y)| (x - y).abs()).collect() };
    if s.invert {
        m = m.map(|v| 1.0 - v);
    }
    let mut m = apply_contour(m, &s.contour);
    for (v, a) in m.v.iter_mut().zip(&shape.v) {
        *v *= a;
    }
    m
}

/// Build every effect map for `layer` from its alpha `shape` over `rect`.
pub fn build_maps(layer: &Layer, shape: Vec<f32>, rect: Rect, light: &GlobalLight, tex: &TextureCtx) -> FxMaps {
    let prepared = PreparedPatterns::new(tex.patterns, PREPARED_PATTERN_BYTES);
    build_maps_prepared(layer, shape, rect, light, tex, &prepared)
}

pub(crate) fn build_maps_prepared(
    layer: &Layer,
    shape: Vec<f32>,
    rect: Rect,
    light: &GlobalLight,
    tex: &TextureCtx,
    patterns: &PreparedPatterns<'_>,
) -> FxMaps {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let shape = Map { w, h, v: shape };
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let mut bevel_paint = vec![BevelPaint::Inner; items.len()];
    let per: Vec<Vec<Map>> = items
        .iter()
        .enumerate()
        .map(|(i, e)| match e {
            Effect::DropShadow(s) => vec![shadow_map(&shape, s, light, false)],
            Effect::InnerShadow(s) => vec![shadow_map(&shape, s, light, true)],
            Effect::OuterGlow(g) => vec![glow_map(&shape, g, false)],
            Effect::InnerGlow(g) => vec![glow_map(&shape, g, true)],
            Effect::Satin(s) => vec![satin_map(&shape, s)],
            Effect::BevelEmboss(b) => {
                let (maps, paint) = bevel_maps(&shape, b, light, tex, patterns);
                bevel_paint[i] = paint;
                maps
            }
            _ => Vec::new(),
        })
        .collect();
    let has_stroke = items.iter().any(|e| matches!(e, Effect::Stroke(_)));
    let vector_shape = matches!(layer.content, photocraft_doc::LayerContent::Shape(_));
    let (din, dout) = if has_stroke { (Some(dist_inside_by(&shape, Metric::Chamfer)), Some(dist_outside_by(&shape, Metric::Chamfer))) } else { (None, None) };
    // Without an outline (unfilled shapes), estimate the vector outline from local coverage.
    let outline = crate::effect_outline(layer).is_some();
    let vdout = (has_stroke && vector_shape && !outline).then(|| dist_outside_by(&local_coverage(&shape), Metric::Chamfer));
    let mut frames: Vec<(u32, Rect)> = Vec::new();
    for e in &items {
        if let Effect::Stroke(st) = e
            && matches!(st.paint, FxPaint::Gradient(_))
        {
            let out_w = stroke_widths(st).1;
            if !frames.iter().any(|f| f.0 == out_w.to_bits()) {
                frames.push((out_w.to_bits(), stroke_frame(&shape, out_w, rect)));
            }
        }
    }
    FxMaps { rect, shape, per, bevel_paint, din, dout, vdout, outline, frames }
}

/// Far outside any shape (distance fill for cropped distance fields).
const FAR: f32 = 1.0e9;

/// Composites `content` (the layer's own pixels over `big`, alpha already
/// masked, clipped layers applied) plus its effects into `backdrop`.
pub fn composite_with_effects(layer: &Layer, content: &Buffer, backdrop: &mut Buffer, maps: &FxMaps, layer_bounds: Rect, patterns: &[Pattern]) {
    let prepared = PreparedPatterns::new(patterns, PREPARED_PATTERN_BYTES);
    composite_with_effects_prepared(layer, content, backdrop, maps, layer_bounds, &prepared, None);
}

pub(crate) fn composite_with_effects_prepared(
    layer: &Layer,
    content: &Buffer,
    backdrop: &mut Buffer,
    maps: &FxMaps,
    layer_bounds: Rect,
    patterns: &PreparedPatterns<'_>,
    vstroke: Option<VectorStroke<'_>>,
) {
    let big = content.rect;
    let (w, h) = (big.width() as usize, big.height() as usize);
    // The effect shape: the content's alpha (with a split-off vector stroke, `content` is the
    // unmasked fill and the mask applies to fill ∪ stroke), joined with a filled shape's outline.
    let kmask = |i: usize| vstroke.as_ref().and_then(|v| v.mask).and_then(|m| m.get(i)).copied().unwrap_or(1.0);
    let union = |i: usize, a: f32| vstroke.as_ref().and_then(|v| v.stroke.px.get(i)).map_or(a, |s| kmask(i) * (a + s[3] * (1.0 - a)));
    let shape = if maps.outline {
        let o = maps.crop(&maps.shape, big, 0.0);
        Map { w, h, v: o.v.iter().zip(&content.px).enumerate().map(|(i, (o, p))| o.max(union(i, p[3]))).collect() }
    } else {
        Map { w, h, v: content.px.iter().enumerate().map(|(i, p)| union(i, p[3])).collect() }
    };
    let relative = maps.outline || vstroke.is_some();
    let fx = |i: usize, k: usize| maps.crop(&maps.per[i][k], big, 0.0);
    // Layer bounds (gradients aligned with the layer use the whole layer,
    // independent of the render rect).
    let sb = layer_bounds;
    // Linked patterns tile from the effects reference point (else the layer's top-left).
    let anchor = layer.effects.reference.unwrap_or((f64::from(sb.x0), f64::from(sb.y0)));
    let rect = backdrop.rect;
    let before = backdrop.clone();
    // Exterior effects are painted on a backdrop copy extended to `big`.
    let mut work = Buffer::transparent(big);
    for y in rect.y0.max(big.y0)..rect.y1.min(big.y1) {
        for x in rect.x0.max(big.x0)..rect.x1.min(big.x1) {
            work.px[((y - big.y0) as usize) * w + (x - big.x0) as usize] = before.get(x, y);
        }
    }
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let vector_shape = matches!(layer.content, photocraft_doc::LayerContent::Shape(_));
    // Multiple instances: the first listed is on top, so paint in reverse.
    let rev = || items.iter().copied().enumerate().rev();

    for (i, e) in rev() {
        if let Effect::DropShadow(s) = e {
            let mut m = fx(i, 0);
            if s.knocks_out {
                // The layer hides the shadow beneath it only where its fill is see-through: at
                // 100 % fill the layer covers it anyway (and anti-aliased edges are not
                // attenuated twice), at 0 % the shape shows the bare backdrop.
                let see_through = 1.0 - layer.fill_opacity.clamp(0.0, 1.0);
                for (v, a) in m.v.iter_mut().zip(&shape.v) {
                    *v *= 1.0 - a * see_through;
                }
            }
            paint_color(&mut work, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::OuterGlow(g) = e {
            let m = fx(i, 0);
            paint_fx(&mut work, &m, &g.paint, sb, anchor, big, g.common.blend, g.common.opacity, patterns);
        }
    }

    // The layer: content at fill opacity, then interior effects. Interior effects are painted
    // relative to the layer's shape (alpha = coverage within the shape), then the shape's alpha
    // applies: a colour overlay at 100 % replaces the colour of a half-transparent edge pixel and
    // keeps its alpha, as in Photoshop.
    let fill = layer.fill_opacity;
    let inside = |a: f32| a > INSIDE_EPS;
    // Within an outline (or a split-off vector stroke) the content's own transparency (a fading
    // gradient fill) acts like fill opacity: the effects still cover the whole shape.
    let lay_alpha = |i: usize, p: &[f32; 4], a: f32| {
        if !inside(a) {
            0.0
        } else if relative {
            fill * (kmask(i) * p[3] / a).min(1.0)
        } else {
            fill
        }
    };
    let mut lay = Buffer { rect: big, px: content.px.iter().zip(&shape.v).enumerate().map(|(i, (p, a))| [p[0], p[1], p[2], lay_alpha(i, p, *a)]).collect() };
    let rel = |m: Map| -> Map {
        let v = m.v.iter().zip(&shape.v).map(|(m, a)| if inside(*a) { (m / a).min(1.0) } else { 0.0 }).collect();
        Map { w: m.w, h: m.h, v }
    };
    let full = Map { w, h, v: shape.v.iter().map(|a| if inside(*a) { 1.0 } else { 0.0 }).collect() };
    for (_, e) in rev() {
        if let Effect::PatternOverlay { common, name, id, scale, angle, link, phase } = e {
            paint_pattern(&mut lay, &full, patterns, name, id, Placement::anchored(anchor, *link, *phase, *scale, *angle), big, common.blend, common.opacity);
        }
    }
    for (_, e) in rev() {
        if let Effect::GradientOverlay { common, gradient, .. } = e {
            paint_fx(&mut lay, &full, &FxPaint::Gradient(gradient.clone()), sb, anchor, big, common.blend, common.opacity, patterns)
        }
    }
    for (_, e) in rev() {
        if let Effect::ColorOverlay { common, color } = e {
            paint_color(&mut lay, &full, rgb(color), common.blend, common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::Satin(s) = e {
            let m = rel(fx(i, 0));
            paint_color(&mut lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::InnerGlow(g) = e {
            let m = rel(fx(i, 0));
            paint_fx(&mut lay, &m, &g.paint, sb, anchor, big, g.common.blend, g.common.opacity, patterns);
        }
    }
    for (i, e) in rev() {
        if let Effect::InnerShadow(s) = e {
            let m = rel(fx(i, 0));
            paint_color(&mut lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    // A stroked shape's vector stroke: Photoshop draws it above the fill's clipped layers and
    // interior effects, below the stroke effect (psd-tools stroke-composite).
    if let Some(vs) = &vstroke {
        for (i, ((p, s), a)) in lay.px.iter_mut().zip(&vs.stroke.px).zip(&shape.v).enumerate() {
            if s[3] > 0.0 && inside(*a) {
                *p = psblend::composite(BlendMode::Normal, *p, [s[0], s[1], s[2], fill * (kmask(i) * s[3] / a).min(1.0)], 1.0);
            }
        }
    }
    // Strokes. Inside parts are painted over the layer (bottom instance first); outside parts
    // are slid beneath it, so the first listed (top) instance is processed first.
    let strokes: Vec<&photocraft_doc::StrokeFx> = items.iter().filter_map(|e| if let Effect::Stroke(s) = e { Some(s) } else { None }).collect();
    let (din, dout) = match (&maps.din, &maps.dout) {
        (Some(a), Some(b)) if !strokes.is_empty() => (maps.crop_vec(a, big, 0.0), maps.crop_vec(b, big, FAR)),
        _ => (Vec::new(), Vec::new()),
    };
    let widths = stroke_widths;
    // A gradient stroke aligned with the layer spans the stroke's own extent.
    let frame = |st: &photocraft_doc::StrokeFx| maps.stroke_frame(st).unwrap_or(sb);
    for st in strokes.iter().rev().copied() {
        let (in_w, _) = widths(st);
        if in_w <= 0.0 {
            continue;
        }
        let mut m = Map::new(w, h, 0.0);
        for ((mv, a), dv) in m.v.iter_mut().zip(&shape.v).zip(&din) {
            *mv = if inside(*a) { (in_w + 0.5 - dv).clamp(0.0, 1.0) } else { 0.0 };
        }
        paint_fx(&mut lay, &m, &st.paint, frame(st), anchor, big, st.common.blend, st.common.opacity, patterns);
    }
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && maps.bevel_paint[i] == BevelPaint::Inner
        {
            let (hi, sh) = (rel(fx(i, 0)), rel(fx(i, 1)));
            paint_color(&mut lay, &hi, rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut lay, &sh, rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }
    // The shape's own alpha.
    for (p, a) in lay.px.iter_mut().zip(&shape.v) {
        p[3] *= a.min(1.0);
    }
    let vdout = maps.vdout.as_ref().filter(|_| vector_shape && !strokes.is_empty()).map(|v| maps.crop_vec(v, big, FAR));
    // Outside parts lie beneath the layer and blend onto the backdrop with their own mode. A
    // higher stroke covers the ones below it (Photoshop: a Multiply stroke listed above a wider
    // Normal stroke multiplies the backdrop, not the lower stroke). Each stroke's share is its
    // coverage × opacity not yet taken by strokes above; for Normal strokes this equals stacking
    // them top over bottom.
    if strokes.iter().any(|st| widths(st).1 > 0.0) {
        let base = work.clone();
        let mut acc = vec![[0.0f32; 4]; w * h]; // premultiplied Σ share·blended
        let mut cover = vec![0.0f32; w * h];
        for st in strokes.iter().copied() {
            let (_, out_w) = widths(st);
            if out_w <= 0.0 {
                continue;
            }
            // Shape layers: Photoshop strokes the vector outline (estimated from local coverage
            // for unfilled shapes); the stroke never shows through the shape's pixels. Along a
            // filled shape's outline it covers the part of each edge pixel outside the path.
            let d = vdout.as_ref().unwrap_or(&dout);
            let mut share = vec![0.0f32; w * h];
            for (i, sh) in share.iter_mut().enumerate() {
                let band = (out_w + 0.5 - d[i]).clamp(0.0, 1.0);
                let k = if maps.outline {
                    band * outline_share(shape.v[i], lay.px[i][3])
                } else if inside(shape.v[i]) {
                    if vector_shape { 0.0 } else { 1.0 }
                } else {
                    band
                };
                *sh = k * st.common.opacity * (1.0 - cover[i]);
                cover[i] += *sh;
            }
            // The stroke blended over the backdrop at full coverage.
            let mut blended = base.clone();
            let ones = Map { w, h, v: share.iter().map(|&s| if s > 0.0 { 1.0 } else { 0.0 }).collect() };
            paint_fx(&mut blended, &ones, &st.paint, frame(st), anchor, big, st.common.blend, 1.0, patterns);
            for ((a, b), s) in acc.iter_mut().zip(&blended.px).zip(&share) {
                if *s > 0.0 {
                    for c in 0..3 {
                        a[c] += s * b[c] * b[3];
                    }
                    a[3] += s * b[3];
                }
            }
        }
        for ((p, a), c) in work.px.iter_mut().zip(&acc).zip(&cover) {
            if *c <= 0.0 {
                continue;
            }
            let k = 1.0 - c.min(1.0);
            let alpha = p[3] * k + a[3];
            if alpha > 0.0 {
                *p = [(p[0] * p[3] * k + a[0]) / alpha, (p[1] * p[3] * k + a[1]) / alpha, (p[2] * p[3] * k + a[2]) / alpha, alpha.min(1.0)];
            }
        }
    }
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && maps.bevel_paint[i] == BevelPaint::Outer
        {
            let (hi, sh) = (fx(i, 0), fx(i, 1));
            paint_color(&mut work, &hi, rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut work, &sh, rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }

    // Layer (with interior effects) onto the exterior result, in the layer's mode.
    let mode = if layer.blend == BlendMode::PassThrough { BlendMode::Normal } else { layer.blend };
    let gamma = crate::text_gamma(layer);
    for (wp, lp) in work.px.iter_mut().zip(&lay.px) {
        if lp[3] > 0.0 {
            *wp = psblend::composite_gamma(mode, *wp, *lp, 1.0, gamma);
        }
    }
    // Emboss styles shade the composited layer (their maps cover inside and outside). Painting
    // them into the layer before its (text-gamma) composite left a type layer's lit top edges up
    // to 6/255 dark (psd-tools layer_effects Emboss: 7.8 → 5.7/255).
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && maps.bevel_paint[i] == BevelPaint::Both
        {
            paint_color(&mut work, &fx(i, 0), rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut work, &fx(i, 1), rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }
    // Layer opacity applies to the whole stack.
    let op = layer.opacity;
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let i = ((y - rect.y0) as usize) * rect.width() as usize + (x - rect.x0) as usize;
            let a = before.px[i];
            let b = if x >= big.x0 && x < big.x1 && y >= big.y0 && y < big.y1 { work.px[((y - big.y0) as usize) * w + (x - big.x0) as usize] } else { a };
            backdrop.px[i] = mix_premul(a, b, op);
        }
    }
}

/// Coverage of an outside stroke beneath a filled shape's edge pixel, for a pixel the outline
/// covers `cov` of and the layer (with its interior effects) ends at alpha `l`. Photoshop treats
/// the stroke and the shape as disjoint areas: the stroke fills the part outside the path and
/// the layer adds its own alpha (psd-tools stroke-effects: a 58 % covered edge pixel of an
/// opaque fill ends opaque, one of a faded fill at 42 % + its alpha). Composited beneath the
/// layer, `share + l (1 - share) = (1 - cov) + l`.
pub fn outline_share(cov: f32, l: f32) -> f32 {
    let outside = (1.0 - cov.clamp(0.0, 1.0)).max(0.0);
    let rest = 1.0 - l.clamp(0.0, 1.0);
    if rest <= 1e-6 { 1.0 } else { (outside / rest).min(1.0) }
}

/// (inside width, outside width) of a stroke.
pub fn stroke_widths(st: &photocraft_doc::StrokeFx) -> (f32, f32) {
    match st.position {
        StrokePosition::Outside => (0.0, st.size),
        StrokePosition::Inside => (st.size, 0.0),
        StrokePosition::Center => (st.size / 2.0, st.size / 2.0),
    }
}

/// The frame a gradient stroke aligned with the layer is laid out in: the pixel bounds of the
/// effect shape (over `rect`) grown by the stroke's outside width less one pixel. Photoshop
/// spans the gradient over the stroke's extent, not the layer's: a 4 px outside stroke around a
/// 22 px square runs its 90° gradient over 28 px (psd-tools stroke-effects), a 3 px one around
/// 36 px of type over 40 px (psd-tools effect-stroke-gradient).
fn stroke_frame(shape: &Map, out_w: f32, rect: Rect) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..shape.h {
        for x in 0..shape.w {
            if shape.v[y * shape.w + x] > INSIDE_EPS {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 == usize::MAX {
        return rect;
    }
    let grow = if out_w.is_finite() { (out_w.min(MAX_REACH).ceil() as i32 - 1).max(0) } else { 0 };
    Rect::new(rect.x0 + x0 as i32, rect.y0 + y0 as i32, rect.x0 + x1 as i32, rect.y0 + y1 as i32).inflate(grow)
}

/// A stroked shape's vector stroke, composited apart from its fill (which the compositor then
/// passes unmasked): the stroke's pixels over the render rect and the layer's mask values.
pub(crate) struct VectorStroke<'a> {
    pub stroke: &'a Buffer,
    pub mask: Option<&'a [f32]>,
}

/// Premultiplied linear interpolation between two straight-alpha pixels.
fn mix_premul(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    let alpha = a[3] + (b[3] - a[3]) * k;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0; 4];
    for c in 0..3 {
        out[c] = (a[c] * a[3] + (b[c] * b[3] - a[c] * a[3]) * k) / alpha;
    }
    out[3] = alpha;
    out
}

/// A distance field of an effect shape, as the map builders above compute it (for the GPU
/// compositor, which builds the rest of the maps itself; keep in sync with `shadow_map`,
/// `glow_map`, `bevel_maps` and `build_maps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// `dist_outside` of the shape: precise outer and centre glows, bevels.
    Outside,
    /// `dist_inside` of the shape: precise edge inner glows, bevels.
    Inside,
    /// Chamfer distance outside the shape: outside strokes, and the spread of drop shadows and
    /// softer outer glows ([`dilate`]).
    StrokeOutside,
    /// Chamfer distance outside `1 - alpha`: the choke of inner shadows and softer inner glows.
    ChokeInside,
    /// Stroke distance inside the shape.
    StrokeInside,
    /// Stroke distance outside a shape layer's outline (from its local coverage).
    StrokeOutsideVector,
}

/// The `kind` distance field of the shape `alpha` (row-major `w × h`).
pub fn distance_field(kind: FieldKind, alpha: Vec<f32>, w: usize, h: usize) -> Vec<f32> {
    let s = Map { w, h, v: alpha };
    match kind {
        FieldKind::Outside => dist_outside(&s),
        FieldKind::Inside => dist_inside(&s),
        FieldKind::StrokeOutside => dist_outside_by(&s, Metric::Chamfer),
        FieldKind::ChokeInside => dist_outside_by(&s.map(|a| 1.0 - a), Metric::Chamfer),
        FieldKind::StrokeInside => dist_inside_by(&s, Metric::Chamfer),
        FieldKind::StrokeOutsideVector => dist_outside_by(&local_coverage(&s), Metric::Chamfer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_is_exact() {
        let (w, h) = (7, 5);
        let mut inside = vec![false; w * h];
        inside[2 * w + 3] = true;
        let d = edt(&inside, w, h);
        assert_eq!(d[2 * w + 3], 0.0);
        assert!((d[2 * w + 6] - 3.0).abs() < 1e-6);
        assert!((d[0] - (9.0f32 + 4.0).sqrt()).abs() < 1e-5);
        assert!((d[4 * w + 5] - (4.0f32 + 4.0).sqrt()).abs() < 1e-5);
    }

    #[test]
    fn edt_two_seeds() {
        let (w, h) = (10, 1);
        let mut inside = vec![false; w];
        inside[0] = true;
        inside[9] = true;
        let d = edt(&inside, w, h);
        assert_eq!(d, vec![0.0, 1.0, 2.0, 3.0, 4.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn blur_preserves_mass() {
        let mut m = Map::new(21, 21, 0.0);
        m.v[10 * 21 + 10] = 1.0;
        blur(&mut m, 6.0);
        let sum: f32 = m.v.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{sum}");
        assert!(m.v[10 * 21 + 10] < 1.0);
    }

    #[test]
    fn gradient_geometry() {
        let b = Rect::new(0, 0, 100, 100);
        let t = |s, x, y| gradient_t(s, 0.0, 1.0, false, (0.0, 0.0), b, x, y);
        assert!(t(GradientStyle::Linear, 0.0, 50.0) < 0.01);
        // Linear / reflected sample pixel corners: the centre is reached half a pixel later.
        assert!((t(GradientStyle::Linear, 50.5, 50.0) - 0.5).abs() < 1e-6);
        assert!(t(GradientStyle::Linear, 100.0, 50.0) > 0.99);
        assert!(t(GradientStyle::Reflected, 50.5, 50.0) < 1e-6);
        assert!((t(GradientStyle::Reflected, 0.0, 50.0) - 1.0).abs() < 1e-6);
        assert!(t(GradientStyle::Radial, 50.0, 50.0) < 1e-6);
        assert!((t(GradientStyle::Radial, 50.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((t(GradientStyle::Diamond, 75.0, 75.0) - 1.0).abs() < 1e-6);
        let ang = t(GradientStyle::Angle, 50.0, 0.0);
        assert!((ang - 0.75).abs() < 1e-3, "{ang}");
        let rev = gradient_t(GradientStyle::Linear, 0.0, 1.0, true, (0.0, 0.0), b, 0.0, 50.0);
        assert!(rev > 0.99);
    }

    #[test]
    fn effect_gradients_snap_end_points_and_sample_corners() {
        // 87° over 600 × 60 at (99, 422): end points (397, 482) and (400, 422), so the overlay runs
        // along (3, -60) from its midpoint (398.5, 452), sampled at pixel corners.
        let frame = Rect::new(99, 422, 699, 482);
        let (a, sc, o) = crate::fill_layout::gradient_layout(GradientStyle::Linear, 87.0, 1.0, (0.0, 0.0), frame);
        assert!((a - (60f32).atan2(3.0).to_degrees()).abs() < 1e-3, "{a}");
        let t = |x: f32, y: f32| gradient_t(GradientStyle::Linear, a, sc, false, o, frame, x, y);
        assert!((t(399.0, 452.5) - 0.5).abs() < 1e-4, "{}", t(399.0, 452.5));
        assert!(t(400.5, 422.5) > 0.99 && t(397.5, 482.5) < 0.01);
    }

    #[test]
    fn chamfer_uses_euclidean_step_lengths() {
        let (w, h) = (9, 9);
        let mut inside = vec![false; w * h];
        inside[0] = true;
        let (d, near) = chamfer_nearest(&inside, w, h);
        let at = |x: usize, y: usize| d[y * w + x];
        assert!((at(3, 0) - 3.0).abs() < 1e-5);
        assert!((at(2, 2) - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-5);
        assert!((at(1, 2) - 5f32.sqrt()).abs() < 1e-5);
        assert!((at(1, 3) - (5f32.sqrt() + 1.0)).abs() < 1e-5, "knight + straight, not √10");
        assert!((at(2, 3) - (5f32.sqrt() + std::f32::consts::SQRT_2)).abs() < 1e-5);
        assert!(near.iter().all(|&n| n == 0));
    }

    #[test]
    fn spread_dilates_with_the_chamfer_metric() {
        // One opaque pixel grown by 3 px: at offset (1, 3) the chamfer distance is √5 + 1 = 3.236
        // (Euclidean 3.162), so the anti-aliased rim keeps 3 + 0.5 − (3.236 − 0.5) = 0.764.
        let mut m = Map::new(9, 9, 0.0);
        m.v[4 * 9 + 4] = 1.0;
        let d = dilate(&m, 3.0);
        assert!((d.v[7 * 9 + 5] - 0.764).abs() < 1e-3, "{}", d.v[7 * 9 + 5]);
        // (2, 3): √5 + √2 = 3.650 (Euclidean 3.606) leaves 0.350; axis steps are exact.
        assert!((d.v[7 * 9 + 6] - (4.0 - 5f32.sqrt() - std::f32::consts::SQRT_2)).abs() < 1e-4, "{}", d.v[7 * 9 + 6]);
        assert_eq!(d.v[4 * 9 + 7], 1.0);
    }

    #[test]
    fn running_sum_tent_matches_the_kernel() {
        let (w, h) = (37usize, 23usize);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 101) as f32 / 100.0).collect();
        for width in [1.0f32, 2.0, 3.5, 5.0, 8.25, 41.0] {
            let mut m = Map { w, h, v: src.clone() };
            tent(&mut m, width);
            let mut want = Map { w, h, v: src.clone() };
            tent_direct(&mut want, width);
            for (a, b) in m.v.iter().zip(&want.v) {
                assert!((a - b).abs() < 1e-5, "width {width}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn tent_kernel_is_two_boxes() {
        let (r, k) = tent_kernel(5.0);
        assert_eq!(r, 4);
        let sum: f32 = k.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!((k[4] - 5.0 / 25.0).abs() < 1e-6 && (k[0] - 1.0 / 25.0).abs() < 1e-6);
        // Fractional widths taper the end taps; width 1 is the identity.
        let (r, k) = tent_kernel(4.0);
        assert_eq!(r, 4);
        assert!(k[0] > 0.0 && k[0] < 1.0 / 25.0);
        assert_eq!(tent_kernel(1.0), (0, vec![1.0]));
    }

    fn no_tex() -> TextureCtx<'static> {
        TextureCtx { rect: Rect::new(0, 0, 1, 1), patterns: &[], anchor: (0.0, 0.0) }
    }

    fn square(n: usize, x0: usize, x1: usize) -> Map {
        let mut m = Map::new(n, n, 0.0);
        for y in x0..x1 {
            for x in x0..x1 {
                m.v[y * n + x] = 1.0;
            }
        }
        m
    }

    fn bevel_of(style: BevelStyle, technique: BevelTechnique) -> Bevel {
        Bevel {
            enabled: true,
            style,
            technique,
            depth: 1.0,
            up: true,
            size: 5.0,
            soften: 0.0,
            angle: 90.0,
            altitude: 30.0,
            use_global_light: false,
            gloss_contour: Contour::Linear,
            highlight: photocraft_doc::FxCommon::new(BlendMode::Screen, 0.75),
            highlight_color: photocraft_color::Color::WHITE,
            shadow: photocraft_doc::FxCommon::new(BlendMode::Multiply, 0.75),
            shadow_color: photocraft_color::Color::BLACK,
            contour: None,
            texture: None,
        }
    }

    #[test]
    fn smooth_inner_bevel_lights_the_top_edge_inside_only() {
        let shape = square(40, 10, 30);
        let (maps, paint) = bevel_maps(
            &shape,
            &bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth),
            &GlobalLight::default(),
            &no_tex(),
            &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES),
        );
        assert_eq!(paint, BevelPaint::Inner);
        let at = |m: &Map, x: usize, y: usize| m.v[y * 40 + x];
        // Light from the top: highlight along the top edge, shadow along the bottom, flat middle.
        assert!(at(&maps[0], 20, 10) > 0.5 && at(&maps[1], 20, 10) == 0.0);
        assert!(at(&maps[1], 20, 29) > 0.5);
        assert!(at(&maps[0], 20, 20) < 1e-3 && at(&maps[1], 20, 20) < 1e-3);
        // Nothing outside the shape; the ramp reaches about `size` inside.
        assert_eq!(at(&maps[0], 20, 8), 0.0);
        assert!(at(&maps[0], 20, 15) < 0.05);
    }

    #[test]
    fn emboss_paints_both_sides_and_pillow_flips_the_outside() {
        let shape = square(40, 10, 30);
        let l = GlobalLight::default();
        let (e, paint) =
            bevel_maps(&shape, &bevel_of(BevelStyle::Emboss, BevelTechnique::Smooth), &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        assert_eq!((paint, e.len()), (BevelPaint::Both, 2));
        let at = |m: &Map, x: usize, y: usize| m.v[y * 40 + x];
        // Emboss: the slope across the top edge faces the light on both sides.
        assert!(at(&e[0], 20, 10) > 0.3 && at(&e[0], 20, 9) > 0.3);
        let (p, _) =
            bevel_maps(&shape, &bevel_of(BevelStyle::PillowEmboss, BevelTechnique::Smooth), &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        assert!(at(&p[0], 20, 10) > 0.3 && at(&p[1], 20, 9) > 0.3, "pillow: outside top edge in shadow");
    }

    #[test]
    fn emboss_width_rounds_up_and_shades_past_the_blur() {
        let mut b = bevel_of(BevelStyle::Emboss, BevelTechnique::Smooth);
        b.size = 41.0;
        let g = bevel_geom(&b);
        assert_eq!((g.width, g.depth), (21.0, 21.0));
        b.size = 5.0;
        assert_eq!(bevel_geom(&b).width, 3.0);
        // The first row past the height blur's reach still slopes (its upper neighbour is raised),
        // so it shades; the row after it does not.
        let shape = square(60, 20, 40);
        b.size = 9.0;
        b.angle = 90.0;
        let (m, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let reach = tent_kernel(bevel_geom(&b).width).0 as usize;
        let row = 39 + reach + 1; // last shape row + reach + 1: height 0, neighbour above raised
        assert!(m[1].v[row * 60 + 30] > 0.0, "{}", m[1].v[row * 60 + 30]);
        assert_eq!(m[1].v[(row + 1) * 60 + 30], 0.0);
    }

    #[test]
    fn chisel_hard_has_flat_facets() {
        let shape = square(48, 10, 38);
        let (m, _) = bevel_maps(
            &shape,
            &bevel_of(BevelStyle::InnerBevel, BevelTechnique::ChiselHard),
            &GlobalLight::default(),
            &no_tex(),
            &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES),
        );
        let at = |x: usize, y: usize| m[0].v[y * 48 + x];
        // A constant slope: equal highlight along the top facet.
        assert!(at(24, 12) > 0.1 && (at(24, 12) - at(24, 13)).abs() < 1e-3);
    }

    #[test]
    fn bevel_contour_and_texture_shape_the_height() {
        let shape = square(40, 6, 34);
        let l = GlobalLight::default();
        let plain = bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth);
        let (a, _) = bevel_maps(&shape, &plain, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        // A contour reshapes the profile (here a ramp folded at its middle).
        let mut c = plain.clone();
        let fold = vec![
            photocraft_doc::adjust::CurvePoint { input: 0.0, output: 0.0 },
            photocraft_doc::adjust::CurvePoint { input: 0.5, output: 1.0 },
            photocraft_doc::adjust::CurvePoint { input: 1.0, output: 0.0 },
        ];
        c.contour = Some(photocraft_doc::BevelContour { contour: Contour::Custom { name: "fold".into(), points: fold }, range: 1.0, anti_alias: false });
        let (b, _) = bevel_maps(&shape, &c, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let diff: f32 = a[1].v.iter().zip(&b[1].v).map(|(x, y)| (x - y).abs()).sum();
        assert!(diff > 5.0, "{diff}");
        // A striped texture lights the flat middle.
        let mut sf = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
        sf.fill_rect(Rect::new(0, 0, 4, 2), &[0.0, 0.0, 0.0, 1.0]);
        sf.fill_rect(Rect::new(0, 2, 4, 4), &[1.0, 1.0, 1.0, 1.0]);
        let pats = [Pattern::new("stripes", sf, 4, 4)];
        let mut t = plain.clone();
        t.texture = Some(photocraft_doc::BevelTexture {
            name: "stripes".into(),
            id: String::new(),
            scale: 1.0,
            depth: 1.0,
            invert: false,
            link: true,
            phase: (0.0, 0.0),
        });
        let tex = TextureCtx { rect: Rect::new(0, 0, 40, 40), patterns: &pats, anchor: (0.0, 0.0) };
        let (m, _) = bevel_maps(&shape, &t, &l, &tex, &PreparedPatterns::new(&pats, PREPARED_PATTERN_BYTES));
        let mid: f32 = (16..24).map(|y| m[0].v[y * 40 + 20] + m[1].v[y * 40 + 20]).sum();
        let flat: f32 = (16..24).map(|y| a[0].v[y * 40 + 20] + a[1].v[y * 40 + 20]).sum();
        assert!(mid > flat + 0.5, "{mid} vs {flat}");
    }

    #[test]
    fn glow_range_stretches_the_contour() {
        let mut g = Glow {
            common: photocraft_doc::FxCommon::new(BlendMode::Screen, 1.0),
            paint: FxPaint::Color(photocraft_color::Color::WHITE),
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size: 10.0,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise: 0.0,
            source: GlowSource::Edge,
        };
        let l = glow_lut(&g).unwrap();
        assert!((lut_at(&l, 0.25) - 0.5).abs() < 1e-3 && (lut_at(&l, 0.75) - 1.0).abs() < 1e-6);
        g.range = 1.0;
        assert!(glow_lut(&g).is_none());
    }

    #[test]
    fn offsets_follow_light() {
        let (dx, dy) = offset(120.0, 10.0);
        assert_eq!((dx, dy), (5.0, 9.0));
    }
}

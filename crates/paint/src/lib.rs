//! Brush engine: Photoshop-grade brushes (computed and sampled tips, Shape Dynamics, Scattering,
//! Texture, Dual Brush, Color Dynamics, Transfer, Brush Pose, Noise, Wet Edges, Build-up,
//! Smoothing) plus the dab machinery shared by the retouching tools.
//!
//! Strokes are data ([`Stroke`]) so they can be recorded, replayed by automation, and tested
//! deterministically: every jitter is a hash of the brush seed and the dab index ([`rng`]).
//! Dabs composite with "build-up up to stroke opacity" semantics: within one stroke, coverage
//! accumulates with flow towards each dab's opacity ceiling, and the stroke composites once at the
//! stroke opacity (Photoshop's Opacity versus Flow).
//!
//! Module map: [`brush`] (the serde model), [`dynamics`] (points → dabs), [`render`] (dabs →
//! stroke buffer → pixels), [`presets`] (built-ins), [`mixer`] (Mixer Brush), [`replace`] (Color
//! Replacement), [`retouch`] (sequential/accumulating helpers for the retouch tools).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use photocraft_geom::{Point, Rect};
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

pub mod bg_erase;
pub mod brush;
pub mod dynamics;
pub mod mixer;
pub mod presets;
pub mod procedural;
pub mod render;
pub mod replace;
pub mod retouch;
pub mod rng;
pub mod tile;

pub use brush::{
    BrushPreset, BrushSettings, ColorDynamics, Control, DualBrush, Dynamic, MaskMode, Pattern, PatternStyle, Pose, Scattering, SectionLocks, ShapeDynamics,
    Smoothing, Texture, TipShape, Transfer,
};
pub use mixer::MixerSettings;
pub use render::{BrushContext, StrokeRenderer, grid_center, grid_square, render_stroke};
pub use tile::GrayTile;

/// One input sample. Missing stylus data defaults to "mouse": full pressure, no tilt/rotation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StrokePoint {
    pub x: f64,
    pub y: f64,
    /// 0..1, 1 for mouse.
    pub pressure: f32,
    /// Degrees -90..90 (W3C Pointer Events `tiltX`/`tiltY`).
    pub tilt_x: f32,
    pub tilt_y: f32,
    /// Barrel rotation in degrees 0..360.
    pub rotation: f32,
    /// Airbrush stylus wheel 0..1.
    pub wheel: f32,
    /// Timestamp in milliseconds (airbrush build-up and smoothing catch-up).
    pub time: f64,
}

impl Default for StrokePoint {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, wheel: 1.0, time: 0.0 }
    }
}

impl StrokePoint {
    pub fn new(x: f64, y: f64, pressure: f32) -> Self {
        Self { x, y, pressure, ..Default::default() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub brush: BrushSettings,
    pub points: Vec<StrokePoint>,
}

/// A dab placed along the stroke (after dynamics).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub center: Point,
    pub radius: f32,
    /// Flow: per-dab build-up amount.
    pub alpha: f32,
    /// Radians, counter-clockwise.
    pub angle: f32,
    pub roundness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Opacity ceiling this dab builds up to (Transfer › Opacity).
    pub opacity: f32,
    /// Straight RGBA paint colour (Color Dynamics).
    pub color: [f32; 4],
    /// Texture depth for per-tip texturing.
    pub depth: f32,
    /// Position in the stroke (0 = first).
    pub index: u64,
    /// Brush Projection: direction (radians, counter-clockwise, y up) along which the tip is
    /// foreshortened, and the factor (1 = no projection).
    pub proj_angle: f32,
    pub proj_scale: f32,
    /// Mixer Brush multipliers of Wet and Mix (Transfer › Wetness/Mix Jitter), 1 = as set.
    pub wet: f32,
    pub mix: f32,
}

impl Dab {
    /// A plain round dab.
    pub fn round(center: Point, radius: f32, alpha: f32) -> Self {
        Self {
            center,
            radius,
            alpha,
            angle: 0.0,
            roundness: 1.0,
            flip_x: false,
            flip_y: false,
            opacity: 1.0,
            color: [0.0, 0.0, 0.0, 1.0],
            depth: 1.0,
            index: 0,
            proj_angle: 0.0,
            proj_scale: 1.0,
            wet: 1.0,
            mix: 1.0,
        }
    }
}

/// All primary dabs of a stroke (smoothing, pose, spacing, build-up and dynamics applied).
pub fn dabs(stroke: &Stroke) -> Vec<Dab> {
    let mut g = dynamics::DabGenerator::new(&stroke.brush, 1.0);
    let (mut out, mut dual) = (Vec::new(), Vec::new());
    g.push(&stroke.points, &mut out, &mut dual);
    g.finish(&mut out, &mut dual);
    out
}

/// Coverage of a round dab at distance `d` from its centre (anti-aliased edge, hardness falloff).
#[inline]
pub fn dab_coverage(d: f32, radius: f32, hardness: f32) -> f32 {
    if d >= radius + 0.5 {
        return 0.0;
    }
    let edge = ((radius + 0.5 - d).clamp(0.0, 1.0)).min(1.0);
    let h = hardness.clamp(0.0, 1.0);
    let inner = radius * h;
    let falloff = if d <= inner || radius - inner < 1e-3 {
        1.0
    } else {
        let t = ((d - inner) / (radius - inner)).clamp(0.0, 1.0);
        // smoothstep falloff
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    edge * falloff
}

/// Rasterize a stroke onto `target`, optionally limited by a selection (grayscale coverage surface).
/// Returns the damaged rectangle.
pub fn apply_stroke(target: &mut Surface, stroke: &Stroke, selection: Option<&Surface>, lock_transparency: bool) -> Rect {
    render_stroke(target, &stroke.brush, &stroke.points, selection, lock_transparency, 1.0)
}

/// Exponential moving-average smoothing of input points (Photoshop's "Smoothing" %, simplified).
pub fn smooth(points: &[StrokePoint], amount: f32) -> Vec<StrokePoint> {
    let a = (1.0 - amount.clamp(0.0, 0.95)) as f64;
    let mut out = Vec::with_capacity(points.len());
    let mut cur: Option<StrokePoint> = None;
    for p in points {
        let n = match cur {
            None => *p,
            Some(c) => StrokePoint { x: c.x + (p.x - c.x) * a, y: c.y + (p.y - c.y) * a, ..*p },
        };
        out.push(n);
        cur = Some(n);
    }
    if let (Some(last), Some(end)) = (out.last_mut(), points.last()) {
        *last = *end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;

    fn stroke(points: &[(f64, f64)], size: f32) -> Stroke {
        Stroke {
            brush: BrushSettings { size, pressure_size: false, ..Default::default() },
            points: points.iter().map(|&(x, y)| StrokePoint::new(x, y, 1.0)).collect(),
        }
    }

    #[test]
    fn dab_spacing() {
        let s = stroke(&[(0.0, 0.0), (100.0, 0.0)], 10.0);
        let d = dabs(&s);
        // spacing 0.1 * 10px = 1px → ~101 dabs
        assert!((95..=105).contains(&d.len()), "{}", d.len());
        let s = Stroke { brush: BrushSettings { spacing: 1.0, pressure_size: false, size: 10.0, ..Default::default() }, ..s };
        assert_eq!(dabs(&s).len(), 11);
    }

    #[test]
    fn coverage_profile() {
        assert_eq!(dab_coverage(0.0, 10.0, 1.0), 1.0);
        assert_eq!(dab_coverage(20.0, 10.0, 1.0), 0.0);
        let soft_mid = dab_coverage(5.0, 10.0, 0.0);
        assert!(soft_mid > 0.2 && soft_mid < 0.8);
        assert!(dab_coverage(9.9, 10.0, 1.0) > 0.5);
    }

    #[test]
    fn hard_brush_paints_solid_line() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        let dmg = apply_stroke(&mut s, &stroke(&[(10.0, 10.0), (90.0, 10.0)], 8.0), None, false);
        assert!(dmg.contains(50, 10));
        assert_eq!(s.pixel(50, 10), vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.pixel(50, 30)[3], 0.0);
    }

    #[test]
    fn opacity_caps_overlap() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        let mut st = stroke(&[(10.0, 10.0), (60.0, 10.0), (10.0, 10.0)], 10.0);
        st.brush.opacity = 0.5;
        apply_stroke(&mut s, &st, None, false);
        let a = s.pixel(30, 10)[3];
        assert!((a - 0.5).abs() < 0.02, "{a}");
    }

    #[test]
    fn selection_limits_paint() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        let mut sel = Surface::new(PixelFormat::GRAY8);
        sel.fill_rect(Rect::new(0, 0, 50, 100), &[1.0]);
        apply_stroke(&mut s, &stroke(&[(10.0, 10.0), (90.0, 10.0)], 8.0), Some(&sel), false);
        assert_eq!(s.pixel(20, 10)[3], 1.0);
        assert_eq!(s.pixel(70, 10)[3], 0.0);
    }

    #[test]
    fn eraser_removes_alpha() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 100, 20), &[1.0, 0.0, 0.0, 1.0]);
        let mut st = stroke(&[(10.0, 10.0), (90.0, 10.0)], 8.0);
        st.brush.erase = true;
        apply_stroke(&mut s, &st, None, false);
        assert_eq!(s.pixel(50, 10)[3], 0.0);
        assert_eq!(s.pixel(50, 1)[3], 1.0);
    }

    #[test]
    fn lock_transparency_preserves_alpha() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 50, 20), &[1.0, 1.0, 1.0, 1.0]);
        apply_stroke(&mut s, &stroke(&[(10.0, 10.0), (90.0, 10.0)], 8.0), None, true);
        assert_eq!(s.pixel(20, 10), vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.pixel(70, 10)[3], 0.0);
    }

    #[test]
    fn pressure_changes_size() {
        let mut st = stroke(&[(0.0, 0.0), (100.0, 0.0)], 20.0);
        st.brush.pressure_size = true;
        st.points[0].pressure = 0.1;
        st.points[1].pressure = 1.0;
        let d = dabs(&st);
        assert!(d.first().unwrap().radius < d.last().unwrap().radius);
    }

    #[test]
    fn smoothing_keeps_endpoints() {
        let pts: Vec<StrokePoint> = (0..20).map(|i| StrokePoint::new(i as f64, if i % 2 == 0 { 0.0 } else { 10.0 }, 1.0)).collect();
        let s = smooth(&pts, 0.8);
        assert_eq!(s.first(), pts.first());
        assert_eq!(s.last(), pts.last());
        let jitter: f64 = s[1..19].iter().map(|p| (p.y - 5.0).abs()).sum();
        let orig: f64 = pts[1..19].iter().map(|p| (p.y - 5.0).abs()).sum();
        assert!(jitter < orig);
    }

    #[test]
    fn works_at_all_depths() {
        for f in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAYA8, PixelFormat::CMYKA8] {
            let mut s = Surface::new(f);
            let mut st = stroke(&[(5.0, 5.0), (40.0, 5.0)], 6.0);
            st.brush.color = [1.0, 1.0, 1.0, 1.0];
            apply_stroke(&mut s, &st, None, false);
            let rgba = s.rgba(20, 5);
            assert!(rgba[3] > 0.99 && rgba[0] > 0.98, "{f:?} {rgba:?}");
        }
    }
}

#[cfg(test)]
mod brush_tests;

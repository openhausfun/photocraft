//! Brush-engine tests: tips, spacing, dynamics, masks, smoothing, determinism, formats.

use photocraft_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photocraft_geom::{Point, Rect};
use photocraft_raster::Surface;

use crate::brush::*;
use crate::dynamics::{Smoother, smooth_points};
use crate::render::{BrushContext, StrokeRenderer, mask_combine, render_stroke};
use crate::tile::GrayTile;
use crate::{Dab, Stroke, StrokePoint, dabs};

fn brush() -> BrushSettings {
    BrushSettings { pressure_size: false, spacing: 0.25, ..Default::default() }
}

fn line(x0: f64, x1: f64, y: f64) -> Vec<StrokePoint> {
    vec![StrokePoint::new(x0, y, 1.0), StrokePoint::new(x1, y, 1.0)]
}

fn dabs_of(b: &BrushSettings, pts: &[StrokePoint]) -> Vec<Dab> {
    dabs(&Stroke { brush: b.clone(), points: pts.to_vec() })
}

fn raster(b: &BrushSettings, d: &Dab) -> (Rect, Vec<f32>) {
    let ctx = BrushContext::new(b);
    let r = ctx.dab_rect(d, false);
    let mut v = Vec::new();
    ctx.rasterize(d, false, r, &mut v);
    (r, v)
}

fn at(r: &(Rect, Vec<f32>), x: i32, y: i32) -> f32 {
    r.1[((y - r.0.y0) * r.0.width() as i32 + (x - r.0.x0)) as usize]
}

fn paint(b: &BrushSettings, pts: &[StrokePoint], w: i32, h: i32) -> Surface {
    let mut s = Surface::new(PixelFormat::RGBA32F);
    let _ = w + h;
    render_stroke(&mut s, b, pts, None, false, 1.0);
    s
}

fn alpha_sum(s: &Surface, r: Rect) -> f32 {
    let mut t = 0.0;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            t += s.rgba(x, y)[3];
        }
    }
    t
}

// ---------- tips ----------

#[test]
fn round_tip_roundness_and_angle() {
    let b = brush();
    let mut d = Dab::round(Point::new(50.0, 50.0), 20.0, 1.0);
    d.roundness = 0.25;
    let r = raster(&b, &d);
    // Wide along x, thin along y.
    assert!(at(&r, 50 + 15, 50) > 0.99);
    assert!(at(&r, 50, 50 + 8) < 0.01);
    assert!(at(&r, 50, 50 + 3) > 0.99);
    // Rotated 90° counter-clockwise: now tall.
    d.angle = std::f32::consts::FRAC_PI_2;
    let r = raster(&b, &d);
    assert!(at(&r, 50, 50 - 15) > 0.99);
    assert!(at(&r, 50 + 8, 50) < 0.01);
    // 45°: the long axis runs up-right (y-down image space: x+, y−).
    d.angle = std::f32::consts::FRAC_PI_4;
    let r = raster(&b, &d);
    assert!(at(&r, 60, 40) > 0.99, "{}", at(&r, 60, 40));
    assert!(at(&r, 60, 60) < 0.01);
}

#[test]
fn hardness_profile_is_monotonic() {
    for h in [0.0f32, 0.5, 1.0] {
        let b = BrushSettings { hardness: h, ..brush() };
        let r = raster(&b, &Dab::round(Point::new(50.0, 50.0), 20.0, 1.0));
        let prof: Vec<f32> = (0..22).map(|i| at(&r, 50 + i, 50)).collect();
        assert!(prof.windows(2).all(|w| w[1] <= w[0] + 1e-6), "h={h} {prof:?}");
        assert!(prof[0] > 0.99);
        assert_eq!(prof[21], 0.0);
        let mid = at(&r, 60, 50);
        if h == 1.0 {
            assert!(mid > 0.99);
        } else if h == 0.0 {
            assert!(mid > 0.2 && mid < 0.8, "{mid}");
        }
    }
}

#[test]
fn sampled_tip_is_scaled_and_oriented() {
    // Tip: left half painted.
    let tip = GrayTile::from_fn(16, 16, |x, _| if x < 8 { 1.0 } else { 0.0 });
    let b = BrushSettings { tip: TipShape::Sampled(tip), ..brush() };
    let d = Dab::round(Point::new(50.0, 50.0), 20.0, 1.0);
    let r = raster(&b, &d);
    assert!(at(&r, 40, 50) > 0.99 && at(&r, 60, 50) < 0.01);
    // Flip X mirrors it.
    let r = raster(&b, &Dab { flip_x: true, ..d });
    assert!(at(&r, 40, 50) < 0.01 && at(&r, 60, 50) > 0.99);
    // Rotated 90° CCW: the painted (left) half moves to the bottom.
    let r = raster(&b, &Dab { angle: std::f32::consts::FRAC_PI_2, ..d });
    assert!(at(&r, 50, 60) > 0.99 && at(&r, 50, 40) < 0.01);
    // Heavily downscaled uses mips: still sensible coverage.
    let r = raster(&b, &Dab::round(Point::new(50.0, 50.0), 2.0, 1.0));
    assert!(r.1.iter().sum::<f32>() > 1.0);
}

#[test]
fn pencil_is_aliased() {
    let b = BrushSettings { aliased: true, hardness: 0.0, size: 9.0, ..brush() };
    let s = paint(&b, &line(10.0, 50.0, 20.5), 64, 40);
    for y in 10..32 {
        for x in 0..64 {
            let a = s.rgba(x, y)[3];
            assert!(a == 0.0 || a == 1.0, "({x},{y}) = {a}");
        }
    }
    assert_eq!(s.rgba(30, 20)[3], 1.0);
    // Anti-aliased brush has partial pixels.
    let s = paint(&BrushSettings { aliased: false, ..b }, &line(10.0, 50.0, 20.3), 64, 40);
    assert!((10..32).any(|y| (0..64).any(|x| (0.01..0.99).contains(&s.rgba(x, y)[3]))));
}

#[test]
fn pencil_dabs_sit_on_the_pixel_grid() {
    use crate::render::{grid_center, grid_square};
    // Odd sizes centre on the pixel holding the point, even ones on the nearest corner.
    assert_eq!(grid_center(10.0, 5.0, 1.0), (10.5, 5.5));
    assert_eq!(grid_center(10.9, 5.2, 3.0), (10.5, 5.5));
    assert_eq!(grid_center(10.4, 5.6, 2.0), (10.0, 6.0));
    assert_eq!(grid_square(10.2, 5.7, 1.0), [10.0, 5.0, 11.0, 6.0]);
    assert_eq!(grid_square(10.2, 5.7, 4.0), [8.0, 4.0, 12.0, 8.0]);
    assert_eq!(grid_square(f64::NAN, 0.0, f32::NAN).len(), 4);
    // A 1 px pencil paints exactly the pixel under each point, even on a pixel corner, with
    // no partial alpha.
    let b = BrushSettings { aliased: true, hardness: 1.0, size: 1.0, ..brush() };
    let s = paint(&b, &[StrokePoint::new(7.0, 4.0, 1.0)], 16, 16);
    for y in 0..16 {
        for x in 0..16 {
            assert_eq!(s.rgba(x, y)[3], if (x, y) == (7, 4) { 1.0 } else { 0.0 }, "({x},{y})");
        }
    }
    // A horizontal 1 px line is one pixel thick and unbroken.
    let s = paint(&b, &line(2.0, 20.0, 9.3), 24, 16);
    for x in 2..20 {
        assert_eq!(s.rgba(x, 9)[3], 1.0, "x {x}");
        assert_eq!(s.rgba(x, 8)[3] + s.rgba(x, 10)[3], 0.0, "x {x}");
    }
    // A 4 px dab stays inside its cursor square.
    let b = BrushSettings { size: 4.0, ..b };
    let s = paint(&b, &[StrokePoint::new(10.2, 5.7, 1.0)], 24, 16);
    let [x0, y0, x1, y1] = grid_square(10.2, 5.7, 4.0);
    for y in 0..16 {
        for x in 0..24 {
            let inside = (x as f64) >= x0 && (x as f64) < x1 && (y as f64) >= y0 && (y as f64) < y1;
            assert!(inside || s.rgba(x, y)[3] == 0.0, "({x},{y}) outside the square is painted");
        }
    }
    assert_eq!(s.rgba(9, 5)[3], 1.0);
}

// ---------- spacing ----------

#[test]
fn spacing_sets_dab_count() {
    for (spacing, len, expect) in [(0.25f32, 100.0, 21usize), (1.0, 100.0, 6), (0.5, 200.0, 21)] {
        let b = BrushSettings { spacing, size: 20.0, ..brush() };
        assert_eq!(dabs_of(&b, &line(0.0, len, 0.0)).len(), expect, "spacing {spacing}");
    }
    // Spacing carries across segments: a polyline gives the same count as the straight line.
    let b = BrushSettings { spacing: 0.5, size: 20.0, ..brush() };
    let pts: Vec<StrokePoint> = (0..=20).map(|i| StrokePoint::new(i as f64 * 10.0, 0.0, 1.0)).collect();
    assert_eq!(dabs_of(&b, &pts).len(), 21);
    let d = dabs_of(&b, &pts);
    assert!(d.windows(2).all(|w| ((w[1].center.x - w[0].center.x) - 10.0).abs() < 1e-6));
}

// ---------- dynamics ----------

#[test]
fn pressure_controls_size_with_minimum() {
    let b = BrushSettings {
        size: 40.0,
        shape_dynamics: ShapeDynamics {
            enabled: true,
            size: Dynamic { control: Control::PenPressure, minimum: 0.25, ..Default::default() },
            ..Default::default()
        },
        ..brush()
    };
    let pts = vec![StrokePoint::new(0.0, 0.0, 0.0), StrokePoint::new(200.0, 0.0, 1.0)];
    let d = dabs_of(&b, &pts);
    assert!((d[0].radius - 5.0).abs() < 1e-3, "min diameter 25 % of 40 → r 5, got {}", d[0].radius);
    assert!((d.last().unwrap().radius - 20.0).abs() < 0.5);
    assert!(d.windows(2).all(|w| w[1].radius >= w[0].radius - 1e-4));
}

#[test]
fn fade_ramps_over_steps() {
    let b = BrushSettings {
        size: 20.0,
        spacing: 0.5,
        shape_dynamics: ShapeDynamics {
            enabled: true,
            size: Dynamic { control: Control::Fade, fade_steps: 10, minimum: 0.0, ..Default::default() },
            ..Default::default()
        },
        ..brush()
    };
    let d = dabs_of(&b, &line(0.0, 300.0, 0.0));
    assert!((d[0].radius - 10.0).abs() < 1e-3);
    assert!(d[5].radius < d[0].radius * 0.6, "{}", d[5].radius);
    assert!(d.iter().skip(10).all(|x| x.radius <= 0.5 + 1e-6));
    // Fade on opacity (Transfer).
    let b = BrushSettings {
        transfer: Transfer { enabled: true, opacity: Dynamic { control: Control::Fade, fade_steps: 4, ..Default::default() }, ..Default::default() },
        ..brush()
    };
    let d = dabs_of(&b, &line(0.0, 100.0, 0.0));
    assert_eq!(d[0].opacity, 1.0);
    assert_eq!(d[4].opacity, 0.0);
}

#[test]
fn tilt_direction_and_rotation_drive_angle() {
    let mk = |c: Control| BrushSettings { shape_dynamics: ShapeDynamics { enabled: true, angle: Dynamic::controlled(c), ..Default::default() }, ..brush() };
    let mut p = StrokePoint::new(0.0, 0.0, 1.0);
    p.tilt_x = 0.0;
    p.tilt_y = -45.0; // pen leaning "up" the screen
    let pts = vec![p, StrokePoint { x: 50.0, ..p }];
    let d = dabs_of(&mk(Control::PenTilt), &pts);
    assert!((d[0].angle.to_degrees() - 90.0).abs() < 1e-3, "{}", d[0].angle.to_degrees());
    // Direction: travelling down the screen = -90° (counter-clockwise positive).
    let pts = vec![StrokePoint::new(0.0, 0.0, 1.0), StrokePoint::new(0.0, 50.0, 1.0)];
    let d = dabs_of(&mk(Control::Direction), &pts);
    assert!(d.iter().all(|x| (x.angle.to_degrees() + 90.0).abs() < 1e-3));
    // Rotation.
    let mut q = StrokePoint::new(0.0, 0.0, 1.0);
    q.rotation = 30.0;
    let d = dabs_of(&mk(Control::Rotation), &[q, StrokePoint { x: 20.0, ..q }]);
    assert!((d[0].angle.to_degrees() - 30.0).abs() < 1e-3);
    // Pen tilt on size: upright pen full size, flat pen minimum.
    let b = BrushSettings {
        shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic::controlled(Control::PenTilt), ..Default::default() },
        size: 20.0,
        ..brush()
    };
    let mut flat = StrokePoint::new(0.0, 0.0, 1.0);
    flat.tilt_x = 90.0;
    let d = dabs_of(&b, &[flat]);
    assert!(d[0].radius <= 0.5 + 1e-6);
}

#[test]
fn jitter_is_deterministic_per_seed() {
    let b = BrushSettings {
        size: 30.0,
        seed: 42,
        shape_dynamics: ShapeDynamics {
            enabled: true,
            size: Dynamic::jitter(1.0),
            angle: Dynamic::jitter(1.0),
            roundness: Dynamic { jitter: 1.0, minimum: 0.2, ..Default::default() },
            flip_x_jitter: true,
            flip_y_jitter: true,
            ..Default::default()
        },
        scattering: Scattering { enabled: true, scatter: Dynamic::jitter(2.0), both_axes: true, count: 3, count_jitter: Dynamic::jitter(1.0) },
        ..brush()
    };
    let pts = line(0.0, 300.0, 50.0);
    let a = dabs_of(&b, &pts);
    assert_eq!(a, dabs_of(&b, &pts));
    let c = dabs_of(&BrushSettings { seed: 43, ..b.clone() }, &pts);
    assert_ne!(a, c);
    // Jitter actually varies things, within their ranges.
    let radii: Vec<f32> = a.iter().map(|d| d.radius).collect();
    assert!(radii.iter().any(|&r| r < 10.0) && radii.iter().all(|&r| r <= 15.0 + 1e-4));
    assert!(a.iter().all(|d| d.roundness >= 0.2 - 1e-6 && d.roundness <= 1.0));
    assert!(a.iter().any(|d| d.flip_x) && a.iter().any(|d| !d.flip_x));
    // Chunked feeding gives identical dabs.
    let mut r = StrokeRenderer::new(&b, None, 1.0).record_dabs();
    let many: Vec<StrokePoint> = (0..=30).map(|i| StrokePoint::new(i as f64 * 10.0, 50.0, 1.0)).collect();
    for ch in many.chunks(4) {
        r.push(ch);
    }
    r.finish();
    assert_eq!(r.dabs(), dabs_of(&b, &many).as_slice());
}

#[test]
fn scattering_stays_in_bounds() {
    for both in [false, true] {
        let b = BrushSettings {
            size: 20.0,
            scattering: Scattering { enabled: true, scatter: Dynamic::jitter(2.0), both_axes: both, count: 4, ..Default::default() },
            ..brush()
        };
        let d = dabs_of(&b, &line(0.0, 400.0, 100.0));
        // count 4 per spacing step (5 px) → 81 steps.
        assert_eq!(d.len(), 81 * 4);
        let max_off = 2.0 * 10.0;
        assert!(d.iter().all(|x| (x.center.y - 100.0).abs() <= max_off + 1e-6));
        assert!(d.iter().any(|x| (x.center.y - 100.0).abs() > max_off * 0.5), "scatter too small");
        if !both {
            // Perpendicular only: x stays on the spacing grid.
            assert!(d.iter().all(|x| (x.center.x / 5.0 - (x.center.x / 5.0).round()).abs() < 1e-6));
        } else {
            assert!(d.iter().any(|x| (x.center.x / 5.0 - (x.center.x / 5.0).round()).abs() > 0.1));
        }
    }
}

#[test]
fn color_dynamics_ranges() {
    let base = BrushSettings { color: [1.0, 0.0, 0.0, 1.0], background: [0.0, 0.0, 1.0, 1.0], seed: 5, ..brush() };
    // FG/BG jitter mixes between the two colours only.
    let b = BrushSettings { color_dynamics: ColorDynamics { enabled: true, fg_bg: Dynamic::jitter(1.0), ..Default::default() }, ..base.clone() };
    let d = dabs_of(&b, &line(0.0, 200.0, 0.0));
    assert!(d.iter().all(|x| x.color[1].abs() < 1e-5 && (x.color[0] + x.color[2] - 1.0).abs() < 1e-3 || x.color[0].max(x.color[2]) >= 0.999));
    assert!(d.iter().any(|x| x.color[2] > 0.5) && d.iter().any(|x| x.color[0] > 0.5));
    // Hue jitter keeps saturation/brightness of a pure colour.
    let b = BrushSettings { color_dynamics: ColorDynamics { enabled: true, hue_jitter: 1.0, ..Default::default() }, ..base.clone() };
    let d = dabs_of(&b, &line(0.0, 200.0, 0.0));
    for x in &d {
        let mx = x.color[0].max(x.color[1]).max(x.color[2]);
        let mn = x.color[0].min(x.color[1]).min(x.color[2]);
        assert!((mx - 1.0).abs() < 1e-4 && mn.abs() < 1e-4, "{:?}", x.color);
    }
    assert!(d.iter().any(|x| x.color[1] > 0.5 || x.color[2] > 0.5));
    // Purity -1 desaturates completely.
    let b = BrushSettings { color_dynamics: ColorDynamics { enabled: true, purity: -1.0, ..Default::default() }, ..base.clone() };
    let d = dabs_of(&b, &line(0.0, 20.0, 0.0));
    assert!(d.iter().all(|x| (x.color[0] - x.color[1]).abs() < 1e-5 && (x.color[1] - x.color[2]).abs() < 1e-5));
    // Per-stroke: one colour for all dabs.
    let b = BrushSettings { color_dynamics: ColorDynamics { enabled: true, per_tip: false, hue_jitter: 1.0, ..Default::default() }, ..base.clone() };
    let d = dabs_of(&b, &line(0.0, 200.0, 0.0));
    assert!(d.windows(2).all(|w| w[0].color == w[1].color));
    // Per-tip colours land in the pixels.
    let b =
        BrushSettings { size: 10.0, spacing: 3.0, color_dynamics: ColorDynamics { enabled: true, fg_bg: Dynamic::jitter(1.0), ..Default::default() }, ..base };
    let s = paint(&b, &line(10.0, 310.0, 20.0), 330, 40);
    let colours: Vec<[f32; 4]> = (0..11).map(|i| s.rgba(10 + i * 30, 20)).collect();
    assert!(colours.iter().any(|c| c[2] > 0.6) && colours.iter().any(|c| c[0] > 0.6), "{colours:?}");
}

#[test]
fn transfer_opacity_caps_and_flow_builds() {
    // Pressure on opacity caps coverage.
    let b =
        BrushSettings { transfer: Transfer { enabled: true, opacity: Dynamic::controlled(Control::PenPressure), ..Default::default() }, size: 10.0, ..brush() };
    let pts = vec![StrokePoint::new(10.0, 10.0, 0.4), StrokePoint::new(90.0, 10.0, 0.4)];
    let s = paint(&b, &pts, 100, 20);
    assert!((s.rgba(50, 10)[3] - 0.4).abs() < 0.01, "{}", s.rgba(50, 10)[3]);
    // Low flow builds up with overlap but never past the opacity.
    let b = BrushSettings { flow: 0.1, opacity: 0.8, size: 10.0, ..brush() };
    let one = paint(&b, &[StrokePoint::new(50.0, 10.0, 1.0)], 100, 20).rgba(50, 10)[3];
    let many = paint(&b, &line(40.0, 60.0, 10.0), 100, 20).rgba(50, 10)[3];
    assert!((one - 0.08).abs() < 0.01, "{one}");
    assert!(many > one * 3.0 && many <= 0.8 + 1e-4, "{many}");
    // Flow jitter varies the dab alpha deterministically.
    let b = BrushSettings { transfer: Transfer { enabled: true, flow: Dynamic::jitter(1.0), ..Default::default() }, ..brush() };
    let d = dabs_of(&b, &line(0.0, 100.0, 0.0));
    assert!(d.iter().any(|x| x.alpha < 0.5) && d.iter().all(|x| x.alpha <= 1.0));
}

#[test]
fn build_up_accumulates_over_time() {
    let b = BrushSettings { build_up: true, build_up_rate: 20.0, flow: 0.05, size: 20.0, hardness: 1.0, ..brush() };
    let hold = |ms: f64| {
        let mut a = StrokePoint::new(50.0, 50.0, 1.0);
        let mut z = a;
        a.time = 0.0;
        z.time = ms;
        let s = paint(&b, &[a, z], 100, 100);
        (dabs_of(&b, &[a, z]).len(), s.rgba(50, 50)[3])
    };
    let (n0, a0) = hold(0.0);
    let (n1, a1) = hold(500.0);
    let (n2, a2) = hold(2000.0);
    assert_eq!((n0, n1, n2), (1, 11, 41));
    assert!(a0 < a1 && a1 < a2, "{a0} {a1} {a2}");
    // Without build-up, lingering adds nothing.
    let nb = BrushSettings { build_up: false, ..b };
    let mut a = StrokePoint::new(50.0, 50.0, 1.0);
    let z = StrokePoint { time: 2000.0, ..a };
    a.time = 0.0;
    assert_eq!(dabs_of(&nb, &[a, z]).len(), 1);
}

#[test]
fn texture_modulates_coverage() {
    let tile = GrayTile::from_fn(8, 8, |x, _| if x < 4 { 1.0 } else { 0.0 });
    let tex = |mode: MaskMode, depth: f32, each_tip: bool| BrushSettings {
        size: 40.0,
        texture: Texture { enabled: true, pattern: Pattern::Tile(tile.clone()), mode, depth, each_tip, ..Default::default() },
        ..brush()
    };
    for each_tip in [false, true] {
        let s = paint(&tex(MaskMode::Multiply, 1.0, each_tip), &line(20.0, 80.0, 30.0), 100, 60);
        assert!(s.rgba(46, 30)[3] < 0.1, "dark texel blocks paint");
        assert!(s.rgba(41, 30)[3] > 0.9, "light texel passes");
        let half = paint(&tex(MaskMode::Multiply, 0.5, each_tip), &line(20.0, 80.0, 30.0), 100, 60);
        let v = half.rgba(46, 30)[3];
        if each_tip {
            // Per-tip texturing builds up across overlapping dabs.
            assert!(v > 0.5 && v < 1.0, "{v}");
        } else {
            assert!((v - 0.5).abs() < 0.02, "{v}");
        }
    }
    // Brightness and invert.
    let mut b = tex(MaskMode::Multiply, 1.0, false);
    b.texture.invert = true;
    let s = paint(&b, &line(20.0, 80.0, 30.0), 100, 60);
    assert!(s.rgba(46, 30)[3] > 0.9 && s.rgba(41, 30)[3] < 0.1);
    b.texture.invert = false;
    b.texture.brightness = 1.0;
    let s = paint(&b, &line(20.0, 80.0, 30.0), 100, 60);
    assert!(s.rgba(46, 30)[3] > 0.9);
    // All modes keep zero coverage at zero and stay in range.
    for m in [
        MaskMode::Multiply,
        MaskMode::Subtract,
        MaskMode::Darken,
        MaskMode::Overlay,
        MaskMode::ColorDodge,
        MaskMode::ColorBurn,
        MaskMode::LinearBurn,
        MaskMode::HardMix,
        MaskMode::LinearHeight,
        MaskMode::Height,
    ] {
        for t in [0.0, 0.3, 1.0] {
            assert_eq!(mask_combine(m, 0.0, t, 1.0), 0.0, "{m:?}");
            let v = mask_combine(m, 0.7, t, 0.8);
            assert!((0.0..=1.0).contains(&v), "{m:?}");
        }
        // Full-white texture never reduces multiply-like masks below the brush.
        assert!(mask_combine(m, 0.6, 1.0, 1.0) >= 0.6 - 1e-5 || matches!(m, MaskMode::Overlay), "{m:?}");
    }
    // Depth jitter per tip is deterministic and within [min depth, depth].
    let mut b = tex(MaskMode::Multiply, 0.8, true);
    b.texture.depth_jitter = Dynamic { jitter: 1.0, minimum: 0.25, ..Default::default() };
    let d = dabs_of(&b, &line(0.0, 200.0, 0.0));
    assert!(d.iter().all(|x| x.depth <= 0.8 + 1e-6 && x.depth >= 0.8 * 0.25 - 1e-6));
    assert!(d.iter().any(|x| x.depth < 0.6));
}

#[test]
fn dual_brush_intersects() {
    // Dual tip: small dabs far apart → the stroke survives only where they land.
    let b = BrushSettings { size: 30.0, dual_brush: DualBrush { enabled: true, size: 6.0, spacing: 5.0, hardness: 1.0, ..Default::default() }, ..brush() };
    let s = paint(&b, &line(20.0, 200.0, 30.0), 220, 60);
    // Dual dabs every 30 px starting at x = 20.
    assert!(s.rgba(20, 30)[3] > 0.9 && s.rgba(50, 30)[3] > 0.9);
    assert!(s.rgba(35, 30)[3] < 0.01, "between dual dabs: {}", s.rgba(35, 30)[3]);
    // Outside the primary (y beyond its radius) nothing, even if dual covers.
    assert_eq!(s.rgba(20, 50)[3], 0.0);
    // Without dual, the stroke is continuous.
    let s = paint(&BrushSettings { dual_brush: DualBrush::default(), ..b }, &line(20.0, 200.0, 30.0), 220, 60);
    assert!(s.rgba(35, 30)[3] > 0.9);
}

#[test]
fn wet_edges_and_noise() {
    let b = BrushSettings { size: 40.0, hardness: 0.0, wet_edges: true, ..brush() };
    let s = paint(&b, &line(20.0, 120.0, 50.0), 140, 100);
    let centre = s.rgba(70, 50)[3];
    assert!(centre < 0.6, "wet interior is lighter: {centre}");
    // Overlap doesn't build up past a single dab.
    let single = paint(&b, &[StrokePoint::new(70.0, 50.0, 1.0)], 140, 100).rgba(70, 50)[3];
    assert!((centre - single).abs() < 0.05, "{centre} vs {single}");
    // Rim is darker than the interior for a hard tip.
    let hb = BrushSettings { hardness: 1.0, ..b };
    let d = paint(&hb, &[StrokePoint::new(50.0, 50.0, 1.0)], 100, 100);
    assert!(d.rgba(50 + 18, 50)[3] > d.rgba(50, 50)[3] + 0.2);
    // Noise: soft tip coverage becomes grainy (more pixels near 0/1).
    let soft = BrushSettings { size: 40.0, hardness: 0.0, ..brush() };
    let a = paint(&soft, &[StrokePoint::new(50.0, 50.0, 1.0)], 100, 100);
    let n = paint(&BrushSettings { noise: true, ..soft }, &[StrokePoint::new(50.0, 50.0, 1.0)], 100, 100);
    let extreme = |s: &Surface| {
        (30..70)
            .flat_map(|y| (30..70).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let v = s.rgba(x, y)[3];
                !(0.15..=0.85).contains(&v)
            })
            .count()
    };
    assert!(extreme(&n) > extreme(&a) + 100, "{} vs {}", extreme(&n), extreme(&a));
}

#[test]
fn smoothing_modes() {
    let zig: Vec<StrokePoint> = (0..40).map(|i| StrokePoint::new(i as f64 * 5.0, if i % 2 == 0 { 0.0 } else { 6.0 }, 1.0)).collect();
    let dev = |pts: &[StrokePoint]| pts.iter().map(|p| (p.y - 3.0).abs()).sum::<f64>() / pts.len() as f64;
    // Exponential smoothing reduces the zig-zag.
    let cfg = Smoothing { amount: 0.8, ..Default::default() };
    let sm = smooth_points(&cfg, 1.0, &zig);
    assert!(dev(&sm[5..sm.len() - 1]) < dev(&zig) * 0.5);
    // Catch-up on end finishes at the last input point; without it, the stroke ends short.
    assert_eq!(sm.last().map(|p| (p.x, p.y)), zig.last().map(|p| (p.x, p.y)));
    let no = smooth_points(&Smoothing { catch_up_on_end: false, ..cfg.clone() }, 1.0, &zig);
    assert!(no.last().unwrap().x < zig.last().unwrap().x - 1.0);
    // Pulled string: jitter smaller than the string moves nothing; the brush trails by the string.
    let ps = Smoothing { amount: 0.2, pulled_string: true, catch_up_on_end: false, ..Default::default() };
    let jitter: Vec<StrokePoint> = (0..20).map(|i| StrokePoint::new(if i % 2 == 0 { 0.0 } else { 5.0 }, 0.0, 1.0)).collect();
    assert_eq!(smooth_points(&ps, 1.0, &jitter).len(), 1, "within the 20 px string: no motion");
    let far = smooth_points(&ps, 1.0, &[StrokePoint::new(0.0, 0.0, 1.0), StrokePoint::new(100.0, 0.0, 1.0)]);
    assert!((far.last().unwrap().x - 80.0).abs() < 1e-4);
    // Adjust for zoom: at 200 % zoom the string is half as long in image pixels.
    let far = smooth_points(&ps, 2.0, &[StrokePoint::new(0.0, 0.0, 1.0), StrokePoint::new(100.0, 0.0, 1.0)]);
    assert!((far.last().unwrap().x - 90.0).abs() < 1e-4);
    // Stroke catch-up: a pause (timestamps) lets the brush converge on the pointer.
    let mut a = StrokePoint::new(0.0, 0.0, 1.0);
    a.time = 0.0;
    let b = StrokePoint { x: 100.0, time: 16.0, ..a };
    let c = StrokePoint { time: 500.0, ..b };
    let mut s = Smoother::new(&Smoothing { amount: 0.8, catch_up: true, catch_up_on_end: false, ..Default::default() }, 1.0);
    let mut out = Vec::new();
    for p in [a, b, c] {
        s.push(p, &mut out);
    }
    assert!(out.last().unwrap().x > 99.0, "{}", out.last().unwrap().x);
    let mut s = Smoother::new(&Smoothing { amount: 0.8, catch_up: false, catch_up_on_end: false, ..Default::default() }, 1.0);
    let mut out = Vec::new();
    for p in [a, b, c] {
        s.push(p, &mut out);
    }
    assert!(out.last().unwrap().x < 70.0, "{}", out.last().unwrap().x);
}

#[test]
fn pose_overrides_stylus_values() {
    let b = BrushSettings {
        size: 40.0,
        pose: Pose { enabled: true, pressure: 0.5, override_pressure: true, ..Default::default() },
        shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic::controlled(Control::PenPressure), ..Default::default() },
        ..brush()
    };
    let d = dabs_of(&b, &line(0.0, 50.0, 0.0));
    assert!(d.iter().all(|x| (x.radius - 10.0).abs() < 1e-4));
}

#[test]
fn chunked_rendering_matches_one_shot() {
    let b = BrushSettings {
        size: 24.0,
        seed: 9,
        shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic::jitter(0.5), angle: Dynamic::jitter(1.0), ..Default::default() },
        scattering: Scattering { enabled: true, scatter: Dynamic::jitter(1.0), both_axes: true, count: 2, ..Default::default() },
        texture: Texture { enabled: true, depth: 0.5, ..Default::default() },
        color_dynamics: ColorDynamics { enabled: true, hue_jitter: 0.5, ..Default::default() },
        color: [1.0, 0.0, 0.0, 1.0],
        ..brush()
    };
    let pts: Vec<StrokePoint> = (0..60).map(|i| StrokePoint::new(20.0 + i as f64 * 4.0, 60.0 + (i as f64 * 0.3).sin() * 30.0, 1.0)).collect();
    let mut one = Surface::new(PixelFormat::RGBA8);
    let d1 = render_stroke(&mut one, &b, &pts, None, false, 1.0);
    let mut inc = Surface::new(PixelFormat::RGBA8);
    let pre = inc.clone();
    let mut r = StrokeRenderer::new(&b, Some(inc.format()), 1.0);
    for ch in pts.chunks(7) {
        r.push(ch);
        r.composite(&pre, &mut inc, None, false, false);
    }
    r.finish();
    r.composite(&pre, &mut inc, None, false, false);
    assert_eq!(d1, r.bounds());
    for y in d1.y0..d1.y1 {
        for x in d1.x0..d1.x1 {
            assert_eq!(one.pixel(x, y), inc.pixel(x, y), "({x},{y})");
        }
    }
}

#[test]
fn paints_every_depth_and_model() {
    let b = BrushSettings { size: 8.0, color: [1.0, 0.0, 0.0, 1.0], ..brush() };
    for mode in [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Cmyk] {
        for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
            for alpha in [true, false] {
                let fmt = PixelFormat::new(mode, sample, alpha);
                let mut s = Surface::with_default(fmt, &photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 1.0, 1.0]));
                let fancy = BrushSettings {
                    shape_dynamics: ShapeDynamics { enabled: true, size: Dynamic::jitter(0.3), ..Default::default() },
                    color_dynamics: ColorDynamics { enabled: true, brightness_jitter: 0.1, ..Default::default() },
                    texture: Texture { enabled: true, depth: 0.2, ..Default::default() },
                    ..b.clone()
                };
                for br in [&b, &fancy] {
                    render_stroke(&mut s, br, &line(5.0, 40.0, 5.0), None, false, 1.0);
                }
                let c = s.rgba(20, 5);
                let expect = photocraft_raster::to_rgba(&fmt, &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
                assert!((c[0] - expect[0]).abs() < 0.15 && (c[1] - expect[1]).abs() < 0.12, "{fmt:?} {c:?} vs {expect:?}");
                assert_eq!(s.pixel(20, 30), s.pixel(5000, 5000), "{fmt:?} untouched");
            }
        }
    }
}

#[test]
fn blend_mode_and_dissolve() {
    let mut s = Surface::new(PixelFormat::RGBA32F);
    s.fill_rect(Rect::new(0, 0, 100, 20), &[0.5, 0.5, 0.5, 1.0]);
    let b = BrushSettings { size: 10.0, color: [0.5, 0.5, 0.5, 1.0], mode: BlendMode::Multiply, ..brush() };
    render_stroke(&mut s, &b, &line(10.0, 90.0, 10.0), None, false, 1.0);
    assert!((s.rgba(50, 10)[0] - 0.25).abs() < 1e-4);
    let mut s = Surface::new(PixelFormat::RGBA32F);
    let b = BrushSettings { size: 30.0, opacity: 0.5, mode: BlendMode::Dissolve, ..brush() };
    render_stroke(&mut s, &b, &line(20.0, 80.0, 20.0), None, false, 1.0);
    let vals: Vec<f32> = (30..70).map(|x| s.rgba(x, 20)[3]).collect();
    assert!(vals.iter().all(|&v| v == 0.0 || v == 1.0) && vals.contains(&0.0) && vals.contains(&1.0));
}

#[test]
fn presets_render_and_round_trip() {
    let presets = crate::presets::builtin();
    assert!(presets.len() >= 12);
    for p in &presets {
        let j = serde_json::to_string(p).unwrap();
        let back: BrushPreset = serde_json::from_str(&j).unwrap();
        assert_eq!(&back, p, "{}", p.name);
        let mut s = Surface::new(PixelFormat::RGBA8);
        let pts: Vec<StrokePoint> = (0..20).map(|i| StrokePoint { time: i as f64 * 16.0, ..StrokePoint::new(40.0 + i as f64 * 8.0, 60.0, 0.8) }).collect();
        let dmg = render_stroke(&mut s, &p.brush, &pts, None, false, 1.0);
        assert!(!dmg.is_empty(), "{}", p.name);
        assert!(alpha_sum(&s, dmg) > 10.0, "{} painted nothing", p.name);
    }
    // Partial JSON fills defaults.
    let b: BrushSettings = serde_json::from_str(r#"{"size": 50, "scattering": {"enabled": true, "scatter": {"jitter": 2}}, "pressure_size": false}"#).unwrap();
    assert_eq!(b.size, 50.0);
    assert!(b.scattering.enabled && b.scattering.count == 1 && !b.pressure_size);
    assert_eq!(b.hardness, 1.0);
}

#[test]
fn protect_texture_keeps_pattern() {
    let presets = crate::presets::builtin();
    let chalk = crate::presets::find(&presets, "chalk").unwrap().brush.clone();
    let canvas = crate::presets::find(&presets, "Canvas Texture").unwrap().brush.clone();
    let cur = BrushSettings { protect_texture: true, ..chalk.clone() };
    let next = canvas.clone().with_protected_texture(&cur);
    assert_eq!(next.texture.pattern, chalk.texture.pattern);
    assert_eq!(next.texture.mode, canvas.texture.mode);
    assert_eq!(canvas.clone().with_protected_texture(&chalk).texture.pattern, canvas.texture.pattern);
}

#[test]
fn tail_preview_shows_the_stroke_as_finishing_it_would() {
    // Live previews draw the smoothing catch-up tail before the stroke ends: compositing the
    // tail preview over the incremental composite gives exactly the finished stroke.
    let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U16, true);
    let b = BrushSettings { size: 9.0, smoothing: Smoothing { amount: 0.7, ..Default::default() }, ..brush() };
    let pts: Vec<StrokePoint> = (0..12).map(|i| StrokePoint::new(5.0 + i as f64 * 9.0, 20.0 + (i % 3) as f64 * 7.0, 1.0)).collect();
    let pre = Surface::new(fmt);
    let mut live = pre.clone();
    let mut r = StrokeRenderer::new(&b, Some(fmt), 1.0);
    r.push(&pts);
    r.composite(&pre, &mut live, None, false, false);
    let mut tail = r.tail_preview().expect("the brush lags: there is a tail");
    tail.composite(&pre, &mut live, None, false, false);
    let mut done = pre.clone();
    render_stroke(&mut done, &b, &pts, None, false, 1.0);
    assert!((0..60).all(|y| (0..120).all(|x| live.rgba(x, y) == done.rgba(x, y))));
    // Without smoothing (and past the first dab) finishing adds nothing.
    let mut r = StrokeRenderer::new(&brush(), Some(fmt), 1.0);
    r.push(&pts);
    assert!(r.tail_preview().is_none());
}

// ---------- Tilt Scale, Brush Projection, speed spacing, locks, mixer transfer ----------

/// Coverage extent (pixels above half coverage) of a rasterised dab along x and y through its centre.
fn extents(b: &BrushSettings, d: &Dab) -> (usize, usize) {
    let r = raster(b, d);
    let (cx, cy) = (d.center.x.floor() as i32, d.center.y.floor() as i32);
    let w = (r.0.x0..r.0.x1).filter(|&x| at(&r, x, cy) > 0.5).count();
    let h = (r.0.y0..r.0.y1).filter(|&y| at(&r, cx, y) > 0.5).count();
    (w, h)
}

fn pen(x: f64, y: f64, tilt_x: f32, tilt_y: f32, rotation: f32) -> StrokePoint {
    StrokePoint { tilt_x, tilt_y, rotation, ..StrokePoint::new(x, y, 1.0) }
}

#[test]
fn tilt_scale_squashes_the_tip_height_with_pen_tilt() {
    // Size control on Pen Tilt with a 100 % minimum, so only Tilt Scale changes the shape.
    let mk = |scale: f32| BrushSettings {
        size: 40.0,
        shape_dynamics: ShapeDynamics {
            enabled: true,
            size: Dynamic { control: Control::PenTilt, minimum: 1.0, ..Default::default() },
            tilt_scale: scale,
            ..Default::default()
        },
        ..brush()
    };
    let upright = [pen(50.0, 50.0, 0.0, 0.0, 0.0)];
    let tilted = [pen(50.0, 50.0, 45.0, 0.0, 0.0)];
    // 0 % (the default) and an upright pen leave the tip round.
    let d = dabs_of(&mk(0.0), &tilted)[0];
    assert_eq!(d.roundness, 1.0);
    let d = dabs_of(&mk(2.0), &upright)[0];
    assert_eq!(d.roundness, 1.0);
    // 100 % at 45° tilt: height × (1 − 0.5 × 0.5) = 0.75; 200 %: 0.5.
    let d = dabs_of(&mk(1.0), &tilted)[0];
    assert!((d.roundness - 0.75).abs() < 1e-4, "{}", d.roundness);
    let d2 = dabs_of(&mk(2.0), &tilted)[0];
    assert!((d2.roundness - 0.5).abs() < 1e-4, "{}", d2.roundness);
    let (w, h) = extents(&mk(2.0), &d2);
    assert!(w >= 38 && (h as i32 - 20).abs() <= 2, "{w}×{h}");
    // Only with the size control on Pen Tilt (Photoshop greys the slider otherwise).
    let mut b = mk(2.0);
    b.shape_dynamics.size.control = Control::PenPressure;
    assert_eq!(dabs_of(&b, &tilted)[0].roundness, 1.0);
    // Rendered strokes differ.
    let a = paint(&mk(0.0), &[pen(20.0, 40.0, 60.0, 0.0, 0.0), pen(120.0, 40.0, 60.0, 0.0, 0.0)], 0, 0);
    let c = paint(&mk(2.0), &[pen(20.0, 40.0, 60.0, 0.0, 0.0), pen(120.0, 40.0, 60.0, 0.0, 0.0)], 0, 0);
    let area = Rect::new(0, 0, 160, 80);
    assert!(alpha_sum(&c, area) < alpha_sum(&a, area) * 0.75, "{} vs {}", alpha_sum(&c, area), alpha_sum(&a, area));
}

#[test]
fn brush_projection_foreshortens_along_tilt_and_turns_with_rotation() {
    let mk = |on: bool| BrushSettings { size: 40.0, shape_dynamics: ShapeDynamics { enabled: true, brush_projection: on, ..Default::default() }, ..brush() };
    // Tilted 60° towards +x: the tip is foreshortened to cos 60° = ½ along x.
    let pts = [pen(50.0, 50.0, 60.0, 0.0, 0.0)];
    let d = dabs_of(&mk(true), &pts)[0];
    assert!((d.proj_scale - 0.5).abs() < 1e-4 && d.proj_angle.abs() < 1e-5, "{d:?}");
    let (w, h) = extents(&mk(true), &d);
    assert!((w as i32 - 20).abs() <= 2 && h >= 38, "{w}×{h}");
    // Tilted along y instead: foreshortened vertically.
    let d = dabs_of(&mk(true), &[pen(50.0, 50.0, 0.0, 60.0, 0.0)])[0];
    let (w, h) = extents(&mk(true), &d);
    assert!(w >= 38 && (h as i32 - 20).abs() <= 2, "{w}×{h}");
    // Off: the tilt doesn't touch the tip.
    let d = dabs_of(&mk(false), &pts)[0];
    assert_eq!((d.proj_scale, d.angle), (1.0, 0.0));
    assert_eq!(extents(&mk(false), &d), (40, 40));
    // Barrel rotation turns the tip (an elliptical one shows it).
    let mut b = mk(true);
    b.roundness = 0.5;
    let d = dabs_of(&b, &[pen(50.0, 50.0, 0.0, 0.0, 90.0)])[0];
    assert!((d.angle.to_degrees() - 90.0).abs() < 1e-3);
    let (w, h) = extents(&b, &d);
    assert!(w < h, "{w}×{h}");
    // Dual-brush dabs are never projected.
    let ctx = BrushContext::new(&mk(true));
    let dd = Dab { proj_scale: 0.3, ..Dab::round(Point::new(50.0, 50.0), 20.0, 1.0) };
    let (r, mut a, mut c) = (ctx.dab_rect(&dd, true), Vec::new(), Vec::new());
    ctx.rasterize(&dd, true, r, &mut a);
    ctx.rasterize(&Dab { proj_scale: 1.0, ..dd }, true, r, &mut c);
    assert_eq!(a, c);
}

#[test]
fn spacing_off_spaces_dabs_by_pointer_speed() {
    let mut b = BrushSettings { size: 20.0, spacing_enabled: false, ..brush() };
    let timed = |ms: f64| vec![StrokePoint { time: 0.0, ..StrokePoint::new(0.0, 10.0, 1.0) }, StrokePoint { time: ms, ..StrokePoint::new(100.0, 10.0, 1.0) }];
    // 100 px in 400 ms: a dab every 8 ms = every 2 px; in 40 ms: every 20 px.
    let slow = dabs_of(&b, &timed(400.0)).len();
    let fast = dabs_of(&b, &timed(40.0)).len();
    assert_eq!((slow, fast), (51, 6));
    // Never denser than half a pixel, however slow.
    assert!(dabs_of(&b, &timed(1.0e9)).len() <= 201);
    // No timestamps: one dab per input point.
    let pts: Vec<StrokePoint> = (0..7).map(|i| StrokePoint::new(f64::from(i) * 15.0, 10.0, 1.0)).collect();
    assert_eq!(dabs_of(&b, &pts).len(), 7);
    // Checked: fixed spacing (25 % of 20 px = 5 px) whatever the speed.
    b.spacing_enabled = true;
    assert_eq!(dabs_of(&b, &timed(400.0)).len(), dabs_of(&b, &timed(40.0)).len());
    // Chunked input gives the same dabs.
    b.spacing_enabled = false;
    let pts: Vec<StrokePoint> = (0..20).map(|i| StrokePoint { time: f64::from(i) * 13.0, ..StrokePoint::new(f64::from(i) * 7.0, 10.0, 1.0) }).collect();
    let one = dabs_of(&b, &pts);
    let mut g = crate::dynamics::DabGenerator::new(&b, 1.0);
    let (mut out, mut dual) = (Vec::new(), Vec::new());
    for c in pts.chunks(3) {
        g.push(c, &mut out, &mut dual);
    }
    g.finish(&mut out, &mut dual);
    assert_eq!(one, out);
}

#[test]
fn locked_sections_survive_picking_a_preset() {
    let presets = crate::presets::builtin();
    let chalk = crate::presets::find(&presets, "chalk").unwrap().brush.clone();
    let mut cur = BrushSettings {
        scattering: Scattering { enabled: true, count: 5, ..Default::default() },
        transfer: Transfer { enabled: true, wetness: Dynamic::jitter(0.4), ..Default::default() },
        noise: true,
        ..brush()
    };
    // Unlocked: the preset's sections win.
    let next = chalk.clone().picked_over(&cur);
    assert_eq!(next.scattering, chalk.scattering);
    assert_eq!(next.noise, chalk.noise);
    // Locked: the current sections stay, and so do the locks.
    cur.locks = SectionLocks { scattering: true, transfer: true, noise: true, ..Default::default() };
    let next = chalk.clone().picked_over(&cur);
    assert_eq!(next.scattering, cur.scattering);
    assert_eq!(next.transfer, cur.transfer);
    assert!(next.noise);
    assert_eq!(next.locks, cur.locks);
    // Unlocked sections still come from the preset.
    assert_eq!(next.texture, chalk.texture);
    assert_eq!(next.tip, chalk.tip);
}

#[test]
fn transfer_wetness_and_mix_vary_per_dab() {
    let b = BrushSettings {
        transfer: Transfer {
            enabled: true,
            wetness: Dynamic::controlled(Control::PenPressure),
            mix: Dynamic { jitter: 1.0, minimum: 0.25, ..Default::default() },
            ..Default::default()
        },
        ..brush()
    };
    let pts = [StrokePoint::new(0.0, 0.0, 0.3), StrokePoint::new(60.0, 0.0, 0.3)];
    let d = dabs_of(&b, &pts);
    assert!(d.iter().all(|x| (x.wet - 0.3).abs() < 1e-5));
    assert!(d.iter().all(|x| (0.25..=1.0).contains(&x.mix)) && d.iter().any(|x| x.mix < 0.9));
    // Transfer off: as set.
    let d = dabs_of(&brush(), &pts);
    assert!(d.iter().all(|x| x.wet == 1.0 && x.mix == 1.0));
    // New fields round-trip and old JSON loads with neutral values.
    let full = BrushSettings {
        spacing_enabled: false,
        shape_dynamics: ShapeDynamics { tilt_scale: 1.5, brush_projection: true, ..Default::default() },
        locks: SectionLocks { texture: true, ..Default::default() },
        mixer: crate::mixer::MixerSettings { wet: 0.9, sample_all_layers: true, ..Default::default() },
        ..b
    };
    let back: BrushSettings = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(back, full);
    let old: BrushSettings = serde_json::from_str(r#"{"size": 12, "shapeDynamics": {"enabled": true}, "transfer": {"enabled": true}}"#).unwrap();
    assert!(old.spacing_enabled && !old.shape_dynamics.brush_projection && old.shape_dynamics.tilt_scale == 0.0 && old.locks == SectionLocks::default());
    assert_eq!(old.mixer, crate::mixer::MixerSettings::default());
}

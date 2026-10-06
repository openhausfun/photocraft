//! Stroke effects on shape layers: the outline, the vector stroke's place, gradient frames.

use super::*;
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::vector::{Path, ShapeLayer, ShapeStroke, StrokeAlign, Subpath};
use photocraft_doc::{Effect, FxCommon, FxPaint, Gradient, GradientStyle, Layer, LayerContent, LayerMask, StrokeFx, StrokePosition};
use photocraft_geom::Size;

const E: f32 = 2.0 / 255.0;

fn close4(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= E)
}

fn doc(w: u32, h: u32) -> Document {
    Document::with_background("t", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE)
}

fn px(doc: &Document, x: i32, y: i32) -> [f32; 4] {
    render(doc, Rect::from_xywh(x, y, 1, 1)).px[0]
}

fn stroke(size: f32, position: StrokePosition, paint: FxPaint) -> Effect {
    Effect::Stroke(StrokeFx { common: FxCommon::new(BlendMode::Normal, 1.0), size, position, paint })
}

fn blue() -> FxPaint {
    FxPaint::Color(Color::rgb(0.0, 0.0, 1.0))
}

/// A shape layer over the square `x0..x1` with its Photoshop-style cached pixels.
fn shape(x0: f64, x1: f64, fill: Fill, stroke: Option<ShapeStroke>) -> Layer {
    let path = Path::new(vec![Subpath::polygon(&[(x0, x0), (x1, x0), (x1, x1), (x0, x1)])]);
    let mut sh = ShapeLayer { path, fill: Some(fill), stroke, live: None, cache: None, psd_raw: None };
    sh.cache = Some(photocraft_vector::render_shape(&sh, PixelFormat::RGBA8, Rect::new(0, 0, 40, 40)));
    Layer::new("shape", LayerContent::Shape(sh))
}

#[test]
fn outline_share_treats_stroke_and_shape_as_disjoint_areas() {
    // Outside the shape the stroke covers the pixel; inside it nothing.
    assert_eq!(effects::outline_share(0.0, 0.0), 1.0);
    assert_eq!(effects::outline_share(1.0, 0.3), 0.0);
    // Beneath an opaque edge pixel the result ends opaque: share + l (1 - share) = 1 - cov + l.
    let (cov, l) = (0.6, 0.6);
    let s = effects::outline_share(cov, l);
    assert!((s + l * (1.0 - s) - (1.0 - cov + l).min(1.0)).abs() < 1e-6);
    // A faded fill keeps the edge part of the stroke only.
    let s = effects::outline_share(0.6, 0.0);
    assert!((s - 0.4).abs() < 1e-6);
    assert_eq!(effects::outline_share(0.5, 1.0), 1.0);
    assert!(effects::outline_share(f32::NAN, 0.5).is_finite() || effects::outline_share(f32::NAN, 0.5).is_nan());
}

#[test]
fn filled_shape_stroke_follows_the_outline_where_the_fill_fades_out() {
    let mut d = doc(40, 40);
    let mut transparent = Color::rgb(1.0, 0.0, 0.0);
    transparent.alpha = 0.0;
    let fade = Fill::gradient(vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, transparent)], 0.0, 1.0, GradientStyle::Linear, false);
    let mut l = shape(10.0, 30.0, fade, None);
    l.effects.items = vec![stroke(3.0, StrokePosition::Outside, blue()), stroke(2.0, StrokePosition::Inside, FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)))];
    d.layers.push(l);
    // The opaque side and the faded side both carry the outside stroke along the path.
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 8, 20));
    assert!(close4(px(&d, 31, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 31, 20));
    // ... and the inside stroke, even where the fill is transparent.
    assert!(close4(px(&d, 28, 20), [0.0, 1.0, 0.0, 1.0]), "{:?}", px(&d, 28, 20));
    // Between them the faded fill shows the background.
    assert!(px(&d, 25, 20)[0] > 0.9 && px(&d, 25, 20)[1] > 0.5, "{:?}", px(&d, 25, 20));
}

#[test]
fn outside_stroke_fills_the_rest_of_a_shapes_edge_pixel() {
    // The right edge at x = 30.5 covers half of column 30: an opaque fill and the stroke beneath
    // it end opaque there (no seam of backdrop between them).
    let mut d = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    let mut l = shape(10.0, 30.5, Fill::Solid(Color::rgb(1.0, 0.0, 0.0)), None);
    l.effects.items = vec![stroke(3.0, StrokePosition::Outside, blue())];
    d.layers.clear();
    d.layers.push(l);
    let p = px(&d, 30, 20);
    assert!((p[3] - 1.0).abs() <= E, "{p:?}");
    assert!((p[0] - 0.5).abs() <= 0.02 && (p[2] - 0.5).abs() <= 0.02, "{p:?}");
}

#[test]
fn vector_stroke_stays_above_interior_effects_and_below_the_stroke_effect() {
    let mut d = doc(40, 40);
    let vs = ShapeStroke { width: 4.0, align: StrokeAlign::Inside, paint: Fill::Solid(Color::rgb(0.0, 1.0, 0.0)), ..ShapeStroke::default() };
    let mut l = shape(10.0, 30.0, Fill::Solid(Color::rgb(1.0, 0.0, 0.0)), Some(vs));
    l.effects.items = vec![
        Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::rgb(0.0, 0.0, 1.0) },
        stroke(1.0, StrokePosition::Inside, FxPaint::Color(Color::rgb(1.0, 1.0, 0.0))),
    ];
    d.layers.push(l);
    assert!(close4(px(&d, 10, 20), [1.0, 1.0, 0.0, 1.0]), "stroke effect on top: {:?}", px(&d, 10, 20));
    assert!(close4(px(&d, 12, 20), [0.0, 1.0, 0.0, 1.0]), "vector stroke over the overlay: {:?}", px(&d, 12, 20));
    assert!(close4(px(&d, 20, 20), [0.0, 0.0, 1.0, 1.0]), "overlay on the fill: {:?}", px(&d, 20, 20));
}

#[test]
fn a_stroked_shapes_mask_applies_to_fill_and_stroke_together() {
    let mut d = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    d.layers.clear();
    let vs = ShapeStroke { width: 4.0, align: StrokeAlign::Inside, paint: Fill::Solid(Color::rgb(0.0, 1.0, 0.0)), ..ShapeStroke::default() };
    let mut l = shape(10.0, 30.0, Fill::Solid(Color::rgb(1.0, 0.0, 0.0)), Some(vs));
    let mut m = LayerMask::reveal_all();
    m.surface = photocraft_raster::Surface::with_default(PixelFormat::GRAY8, &[0.5]);
    l.mask = Some(m);
    let mut clip = Layer::raster("clip", PixelFormat::RGBA8);
    clip.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 40, 40), &[0.0, 0.0, 1.0, 1.0]);
    clip.clipped = true;
    d.layers.push(l.clone());
    d.layers.push(clip.clone());
    // On the vector stroke: the stroke's colour at the mask's 50 %, not 75 %.
    let p = px(&d, 12, 20);
    assert!(close4(p, [0.0, 1.0, 0.0, 0.5]), "{p:?}");
    // With effects too.
    l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::rgb(1.0, 1.0, 1.0) }];
    d.layers = vec![l.clone()];
    let p = px(&d, 12, 20);
    assert!(close4(p, [0.0, 1.0, 0.0, 0.5]), "without clipped layers: {p:?}");
    d.layers = vec![l, clip];
    let p = px(&d, 12, 20);
    assert!(close4(p, [0.0, 1.0, 0.0, 0.5]), "{p:?}");
}

#[test]
fn gradient_stroke_spans_the_strokes_extent() {
    let mut d = doc(40, 40);
    let mut l = Layer::raster("sq", PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 30, 30), &[1.0, 0.0, 0.0, 1.0]);
    let g = Gradient { stops: vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], angle: 90.0, ..Gradient::default() };
    l.effects.items = vec![stroke(4.0, StrokePosition::Outside, FxPaint::Gradient(g))];
    d.layers.push(l);
    // The frame is the square grown by 3 px (7..33), laid out like every linear gradient
    // (fill_layout::gradient_layout); the layer's own bounds would give other values.
    let t_in = |frame: Rect, x: i32, y: i32| {
        let (a, s, o) = crate::fill_layout::gradient_layout(GradientStyle::Linear, 90.0, 1.0, (0.0, 0.0), frame);
        effects::gradient_t(GradientStyle::Linear, a, s, false, o, frame, x as f32 + 0.5, y as f32 + 0.5)
    };
    let (stroke_frame, layer_frame) = (Rect::new(7, 7, 33, 33), Rect::new(10, 10, 30, 30));
    for y in [7, 9, 31, 32] {
        let p = px(&d, 20, y);
        assert!((p[0] - t_in(stroke_frame, 20, y)).abs() <= 0.01, "row {y}: {p:?}");
    }
    assert!((t_in(stroke_frame, 20, 9) - t_in(layer_frame, 20, 9)).abs() > 0.05);
}

#[test]
fn several_strokes_stack_top_instance_first() {
    // Multiple instances: the first listed is on top; an outer wider stroke shows past it.
    let mut d = doc(60, 60);
    let mut l = Layer::raster("sq", PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(Rect::new(20, 20, 40, 40), &[1.0, 0.0, 0.0, 1.0]);
    l.effects.items = vec![
        stroke(2.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(0.0, 1.0, 0.0))),
        stroke(5.0, StrokePosition::Outside, blue()),
        stroke(2.0, StrokePosition::Inside, FxPaint::Color(Color::rgb(1.0, 1.0, 0.0))),
    ];
    d.layers.push(l);
    assert!(close4(px(&d, 19, 30), [0.0, 1.0, 0.0, 1.0]));
    assert!(close4(px(&d, 16, 30), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 20, 30), [1.0, 1.0, 0.0, 1.0]));
    assert!(close4(px(&d, 14, 30), [1.0; 4]));
}

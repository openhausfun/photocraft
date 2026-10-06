use super::*;
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::adjust::{CurvePoint, LevelsChannel};
use photocraft_doc::{Adjustment, Document, Fill, Layer, LayerContent, LayerMask};
use photocraft_geom::Size;

const E: f32 = 2.0 / 255.0;

fn close4(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= E)
}

fn doc_white(w: u32, h: u32) -> Document {
    Document::with_background("t", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE)
}

fn solid_layer(name: &str, rect: Rect, rgba: [f32; 4]) -> Layer {
    let mut l = Layer::raster(name, PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(rect, &rgba);
    l
}

fn px(doc: &Document, x: i32, y: i32) -> [f32; 4] {
    render(doc, Rect::from_xywh(x, y, 1, 1)).px[0]
}

#[test]
fn background_only() {
    let d = doc_white(8, 8);
    assert!(close4(px(&d, 3, 3), [1.0; 4]));
    // outside canvas the background layer has no pixels
    assert!(close4(px(&d, 20, 3), [0.0; 4]));
}

#[test]
fn normal_layer_over_background() {
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("red", Rect::new(0, 0, 4, 8), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 5, 1), [1.0; 4]));
}

#[test]
fn opacity_and_fill_multiply() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    l.opacity = 0.5;
    l.fill_opacity = 0.5;
    d.layers.push(l);
    let p = px(&d, 0, 0);
    assert!((p[0] - 0.75).abs() <= E, "{p:?}");
}

#[test]
fn hidden_layers_skipped() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    l.visible = false;
    d.layers.push(l);
    assert!(close4(px(&d, 0, 0), [1.0; 4]));
}

#[test]
fn every_blend_mode_matches_reference_on_opaque_pixels() {
    let backdrop = [0.6, 0.3, 0.2, 1.0];
    let source = [0.2, 0.7, 0.5, 1.0];
    for mode in BlendMode::LAYER_MODES {
        if mode == BlendMode::Dissolve {
            continue;
        }
        let mut d = Document::new("b", Size::new(2, 2), ColorMode::Rgb, SampleType::F32);
        let mut bottom = Layer::raster("b", PixelFormat::RGBA32F);
        bottom.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &backdrop);
        let mut top = Layer::raster("t", PixelFormat::RGBA32F);
        top.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &source);
        top.blend = mode;
        d.layers = vec![bottom, top];
        let got = px(&d, 0, 0);
        let want = blend::blend_rgb(mode, [0.6, 0.3, 0.2], [0.2, 0.7, 0.5]);
        for i in 0..3 {
            assert!((got[i] - want[i]).abs() < 1e-5, "{mode:?}: {got:?} vs {want:?}");
        }
        assert!((got[3] - 1.0).abs() < 1e-6);
    }
}

#[test]
fn layer_mask_hides_pixels() {
    let mut d = doc_white(4, 4);
    let mut l = solid_layer("k", Rect::new(0, 0, 4, 4), [0.0, 0.0, 0.0, 1.0]);
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 2, 4), &[0.0]);
    l.mask = Some(m);
    d.layers.push(l);
    assert!(close4(px(&d, 0, 0), [1.0; 4]));
    assert!(close4(px(&d, 3, 0), [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn disabled_mask_is_ignored() {
    let mut d = doc_white(2, 2);
    let mut l = solid_layer("k", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut m = LayerMask::hide_all();
    m.enabled = false;
    l.mask = Some(m);
    d.layers.push(l);
    // CMYK displays through the built-in CMYK profile: 100 % K alone is a dark neutral
    // (as in any real CMYK profile), not pure black.
    let p = px(&d, 0, 0);
    assert!(p[0] < 0.3 && (p[0] - p[1]).abs() < 0.05 && (p[1] - p[2]).abs() < 0.05 && p[3] == 1.0, "{p:?}");
}

#[test]
fn clipping_mask_restricts_to_base_alpha() {
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("base", Rect::new(0, 0, 4, 8), [0.0, 0.0, 1.0, 1.0]));
    let mut clip = solid_layer("clip", Rect::new(0, 0, 8, 8), [1.0, 0.0, 0.0, 1.0]);
    clip.clipped = true;
    d.layers.push(clip);
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]), "inside base: clipped layer visible");
    assert!(close4(px(&d, 6, 1), [1.0; 4]), "outside base: clipped layer hidden");
}

#[test]
fn hidden_base_hides_clipping_group() {
    let mut d = doc_white(4, 4);
    let mut base = solid_layer("base", Rect::new(0, 0, 4, 4), [0.0, 0.0, 1.0, 1.0]);
    base.visible = false;
    d.layers.push(base);
    let mut clip = solid_layer("clip", Rect::new(0, 0, 4, 4), [1.0, 0.0, 0.0, 1.0]);
    clip.clipped = true;
    d.layers.push(clip);
    assert!(close4(px(&d, 1, 1), [1.0; 4]));
}

#[test]
fn isolated_group_differs_from_pass_through() {
    // A Multiply layer inside a group: pass-through multiplies with the background,
    // an isolated (Normal) group multiplies only with its own (empty) contents.
    let make = |blend: BlendMode| {
        let mut d = doc_white(2, 2);
        d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.5, 0.5, 0.5, 1.0]);
        let mut m = solid_layer("m", Rect::new(0, 0, 2, 2), [0.5, 0.5, 0.5, 1.0]);
        m.blend = BlendMode::Multiply;
        let mut g = Layer::group("g", vec![m]);
        g.blend = blend;
        d.layers.push(g);
        px(&d, 0, 0)
    };
    let pass = make(BlendMode::PassThrough);
    let iso = make(BlendMode::Normal);
    assert!((pass[0] - 0.25).abs() <= E, "{pass:?}");
    assert!((iso[0] - 0.5).abs() <= E, "{iso:?}");
}

/// psd-tools passthrough_fill_adjustment.psd: below 100% fill a pass-through group is isolated,
/// so an adjustment inside it no longer changes the layers beneath.
#[test]
fn pass_through_group_below_full_fill_is_isolated() {
    let make = |fill: f32| {
        let mut d = doc_white(2, 2);
        let mut g = Layer::group("g", vec![Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert))]);
        g.blend = BlendMode::PassThrough;
        g.fill_opacity = fill;
        d.layers.push(g);
        px(&d, 0, 0)
    };
    assert!(close4(make(1.0), [0.0, 0.0, 0.0, 1.0]), "full fill: the invert reaches the background");
    assert!(close4(make(0.4), [1.0; 4]), "isolated: nothing beneath to invert");
}

/// psd-tools layer_mask_data.psd: a mask feather blurs the mask edge (CPU and the GPU's
/// combined mask share `masks::combined_mask`).
#[test]
fn mask_feather_blurs_the_edge() {
    let mut d = doc_white(64, 8);
    let mut l = solid_layer("k", Rect::new(0, 0, 64, 8), [0.0, 0.0, 0.0, 1.0]);
    let mut m = LayerMask::hide_all();
    m.surface.fill_rect(Rect::new(0, 0, 32, 8), &[1.0]);
    l.mask = Some(m);
    d.layers.push(l.clone());
    assert!(px(&d, 31, 4)[0] < 0.01 && px(&d, 32, 4)[0] > 0.99, "sharp without feather");
    if let Some(m) = &mut d.layers[1].mask {
        m.feather = 8.0;
    }
    let (a, b) = (px(&d, 31, 4)[0], px(&d, 32, 4)[0]);
    assert!(a > 0.3 && a < 0.5 && b > 0.5 && b < 0.7, "soft edge around 50%: {a} {b}");
    assert!(px(&d, 4, 4)[0] < 0.01 && px(&d, 60, 4)[0] > 0.99, "far from the edge unchanged");
}

#[test]
fn group_opacity_applies_once() {
    let mut d = doc_white(2, 2);
    let a = solid_layer("a", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let b = solid_layer("b", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut g = Layer::group("g", vec![a, b]);
    g.blend = BlendMode::Normal;
    g.opacity = 0.5;
    d.layers.push(g);
    assert!((px(&d, 0, 0)[0] - 0.5).abs() <= E);
}

#[test]
fn pass_through_group_opacity_mixes() {
    let mut d = doc_white(2, 2);
    let a = solid_layer("a", Rect::new(0, 0, 2, 2), [0.0, 0.0, 0.0, 1.0]);
    let mut g = Layer::group("g", vec![a]);
    g.opacity = 0.25;
    d.layers.push(g);
    assert!((px(&d, 0, 0)[0] - 0.75).abs() <= E);

    // Opaque backdrops hide straight-alpha interpolation errors.
    let mut transparent = Document::new("coverage", Size::new(2, 2), ColorMode::Rgb, SampleType::F32);
    let mut red = Layer::raster("HDR red", transparent.pixel_format());
    red.surface_mut().unwrap().fill_rect(transparent.bounds(), &[2.0, 0.0, 0.0, 1.0]);
    let mut group = Layer::group("half", vec![red]);
    group.opacity = 0.5;
    transparent.layers.push(group);
    let actual = px(&transparent, 0, 0);
    assert!(close4(actual, [2.0, 0.0, 0.0, 0.5]), "{actual:?}");
}

#[test]
fn invert_adjustment_layer() {
    let mut d = doc_white(2, 2);
    d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.2, 0.4, 0.6, 1.0]);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    assert!(close4(px(&d, 0, 0), [0.8, 0.6, 0.4, 1.0]));
}

#[test]
fn adjustment_opacity_and_mask() {
    let mut d = doc_white(4, 1);
    d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 1), &[0.0, 0.0, 0.0, 1.0]);
    let mut adj = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    adj.opacity = 0.5;
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(2, 0, 4, 1), &[0.0]);
    adj.mask = Some(m);
    d.layers.push(adj);
    assert!((px(&d, 0, 0)[0] - 0.5).abs() <= E);
    assert!(px(&d, 3, 0)[0].abs() <= E);
}

#[test]
fn clipped_adjustment_only_affects_base() {
    let mut d = doc_white(4, 1);
    d.layers.push(solid_layer("base", Rect::new(0, 0, 2, 1), [0.0, 0.0, 0.0, 1.0]));
    let mut adj = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    adj.clipped = true;
    d.layers.push(adj);
    assert!(close4(px(&d, 0, 0), [1.0; 4]), "base inverted to white");
    assert!(close4(px(&d, 3, 0), [1.0; 4]), "background untouched (white)");
}

#[test]
fn threshold_posterize_levels_curves() {
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.3, 0.3, 0.3, 1.0]);
    adjust::apply(&Adjustment::Threshold { level: 0.5 }, &mut buf);
    assert!(close4(buf.px[0], [0.0, 0.0, 0.0, 1.0]));

    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.3, 0.6, 0.9, 1.0]);
    adjust::apply(&Adjustment::Posterize { levels: 2 }, &mut buf);
    assert!(close4(buf.px[0], [0.0, 1.0, 1.0, 1.0]));

    let lv = LevelsChannel { in_black: 0.2, in_white: 0.8, ..Default::default() };
    assert!((adjust::levels(&lv, 0.5) - 0.5).abs() < 1e-6);
    assert!((adjust::levels(&lv, 0.2)).abs() < 1e-6);
    assert!((adjust::levels(&lv, 0.9) - 1.0).abs() < 1e-6);

    let identity = adjust::curve_lut(&[CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }]);
    for (i, v) in identity.iter().enumerate().step_by(97) {
        assert!((v - i as f32 / (identity.len() - 1) as f32).abs() < 1e-4);
    }
    let s = adjust::curve_lut(&[
        CurvePoint { input: 0.0, output: 0.0 },
        CurvePoint { input: 0.25, output: 0.15 },
        CurvePoint { input: 0.75, output: 0.85 },
        CurvePoint { input: 1.0, output: 1.0 },
    ]);
    assert!(s.windows(2).all(|w| w[1] >= w[0] - 1e-6), "monotone");
    assert!(s[s.len() / 4] < 0.25 && s[3 * s.len() / 4] > 0.75);
}

#[test]
fn hue_saturation_roundtrips_and_desaturates() {
    let c = [0.8, 0.3, 0.1];
    let (h, s, l) = adjust::rgb_to_hsl(c);
    let back = adjust::hsl_to_rgb(h, s, l);
    for i in 0..3 {
        assert!((back[i] - c[i]).abs() < 1e-5);
    }
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [0.8, 0.3, 0.1, 1.0]);
    adjust::apply(
        &Adjustment::HueSaturation { hue: 0.0, saturation: -100.0, lightness: 0.0, colorize: false, ranges: photocraft_doc::adjust::HueRange::defaults() },
        &mut buf,
    );
    let p = buf.px[0];
    assert!((p[0] - p[1]).abs() < 1e-5 && (p[1] - p[2]).abs() < 1e-5);
    let mut buf = Buffer::filled(Rect::new(0, 0, 1, 1), [1.0, 0.0, 0.0, 1.0]);
    adjust::apply(
        &Adjustment::HueSaturation { hue: 120.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: photocraft_doc::adjust::HueRange::defaults() },
        &mut buf,
    );
    assert!(close4(buf.px[0], [0.0, 1.0, 0.0, 1.0]));
}

#[test]
fn solid_and_gradient_fill_layers() {
    let mut d = doc_white(10, 1);
    d.layers.push(Layer::new("fill", LayerContent::Fill(Fill::Solid(Color::rgb(0.0, 0.0, 1.0)))));
    assert!(close4(px(&d, 5, 0), [0.0, 0.0, 1.0, 1.0]));

    let g = Fill::gradient(vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], 0.0, 1.0, photocraft_doc::GradientStyle::Linear, false);
    let patterns = pattern::PreparedPatterns::new(&[], pattern::PREPARED_PATTERN_BYTES);
    let buf = render_fill(&g, Rect::new(0, 0, 10, 1), Rect::new(0, 0, 10, 1), &patterns);
    // tile independence: a 1px render of the right edge equals the full render
    let one = render_fill(&g, Rect::new(9, 0, 10, 1), Rect::new(0, 0, 10, 1), &patterns);
    assert_eq!(one.px[0], buf.px[9]);
    assert!(buf.px[0][0] < buf.px[9][0], "left dark, right light");
}

#[test]
fn dissolve_coverage_matches_opacity() {
    let mut d = Document::new("d", Size::new(64, 64), ColorMode::Rgb, SampleType::U8);
    let mut l = solid_layer("k", Rect::new(0, 0, 64, 64), [0.0, 0.0, 0.0, 1.0]);
    l.blend = BlendMode::Dissolve;
    l.opacity = 0.3;
    d.layers.push(l);
    let b = flatten(&d);
    let covered = b.px.iter().filter(|p| p[3] > 0.5).count() as f32 / b.px.len() as f32;
    assert!((covered - 0.3).abs() < 0.05, "{covered}");
    // deterministic
    assert_eq!(flatten(&d), b);
}

#[test]
fn render_is_tile_independent() {
    // Rendering a sub-rect must equal the same region of a full render.
    let mut d = doc_white(300, 300);
    let mut l = solid_layer("a", Rect::new(20, 20, 280, 280), [0.1, 0.5, 0.9, 0.7]);
    l.blend = BlendMode::Overlay;
    d.layers.push(l);
    let full = flatten(&d);
    let sub = render(&d, Rect::new(250, 250, 270, 262));
    for y in 250..262 {
        for x in 250..270 {
            assert_eq!(full.get(x, y), sub.get(x, y));
        }
    }
}

#[test]
fn cmyk_document_renders_via_rgb() {
    let mut d = Document::new("c", Size::new(2, 2), ColorMode::Cmyk, SampleType::U8);
    let mut l = Layer::raster("k", d.pixel_format());
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.0, 0.0, 0.0, 1.0, 1.0]);
    d.layers.push(l);
    // CMYK displays through the built-in CMYK profile: 100 % K alone is a dark neutral
    // (as in any real CMYK profile), not pure black.
    let p = px(&d, 0, 0);
    assert!(p[0] < 0.3 && (p[0] - p[1]).abs() < 0.05 && (p[1] - p[2]).abs() < 0.05 && p[3] == 1.0, "{p:?}");
}

#[test]
fn sixteen_bit_and_float_layers_composite() {
    for fmt in [PixelFormat::RGBA16, PixelFormat::RGBA32F] {
        let mut d = Document::new("x", Size::new(2, 2), ColorMode::Rgb, fmt.sample);
        let mut l = Layer::raster("a", fmt);
        l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 2), &[0.25, 0.5, 0.75, 1.0]);
        d.layers.push(l);
        assert!(close4(px(&d, 1, 1), [0.25, 0.5, 0.75, 1.0]), "{fmt:?}");
    }
}

#[test]
fn thumbnail_dimensions() {
    let d = doc_white(400, 200);
    let t = thumbnail(&d, 100);
    assert_eq!((t.width, t.height), (100, 50));
    assert_eq!(&t.pixels[0..4], &[255, 255, 255, 255]);
    let small = doc_white(10, 5);
    let t = thumbnail(&small, 100);
    assert_eq!((t.width, t.height), (10, 5));
}

#[test]
fn buffer_over_background() {
    let b = Buffer::filled(Rect::new(0, 0, 1, 1), [0.0, 0.0, 0.0, 0.5]);
    let o = b.over_background([1.0, 1.0, 1.0]);
    assert!(close4(o.px[0], [0.5, 0.5, 0.5, 1.0]));
    assert_eq!(b.to_rgba8().pixels, vec![0, 0, 0, 128]);
}

// ---------- layer effects ----------

use photocraft_doc::{Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Gradient, GradientStyle, Satin, Shadow, StrokeFx, StrokePosition};

fn fx_doc(effects: Vec<Effect>) -> Document {
    let mut d = doc_white(40, 40);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.effects.items = effects;
    d.layers.push(l);
    d
}

fn stroke(size: f32, position: StrokePosition) -> Effect {
    Effect::Stroke(StrokeFx {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        size,
        position,
        paint: FxPaint::Color(Color::rgb(0.0, 0.0, 1.0)),
    })
}

#[test]
fn outside_stroke_width() {
    let d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 7, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 5, 20), [1.0; 4]), "{:?}", px(&d, 5, 20));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]), "interior untouched");
}

#[test]
fn inside_and_center_strokes() {
    let d = fx_doc(vec![stroke(2.0, StrokePosition::Inside)]);
    assert!(close4(px(&d, 10, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 11, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 13, 20), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 9, 20), [1.0; 4]));
    let d = fx_doc(vec![stroke(4.0, StrokePosition::Center)]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 11, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 13, 20), [1.0, 0.0, 0.0, 1.0]));
}

#[test]
fn master_switch_and_per_effect_enable() {
    let mut d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    d.layers[1].effects.enabled = false;
    assert!(close4(px(&d, 8, 20), [1.0; 4]));
    let mut d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    if let Effect::Stroke(s) = &mut d.layers[1].effects.items[0] {
        s.common.enabled = false;
    }
    assert!(close4(px(&d, 8, 20), [1.0; 4]));
}

#[test]
fn color_overlay_ignores_fill_opacity_but_not_opacity() {
    let mut d = fx_doc(vec![Effect::ColorOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), color: Color::rgb(0.0, 1.0, 0.0) }]);
    d.layers[1].fill_opacity = 0.0;
    assert!(close4(px(&d, 20, 20), [0.0, 1.0, 0.0, 1.0]), "overlay shows at fill 0");
    d.layers[1].opacity = 0.5;
    assert!(close4(px(&d, 20, 20), [0.5, 1.0, 0.5, 1.0]), "{:?}", px(&d, 20, 20));
}

fn shadow(distance: f32, angle: f32) -> Shadow {
    Shadow {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        color: Color::BLACK,
        angle,
        use_global_light: false,
        distance,
        spread: 1.0,
        size: 0.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: true,
    }
}

#[test]
fn drop_shadow_falls_away_from_light() {
    // Light from the top (90°): shadow below the square.
    let d = fx_doc(vec![Effect::DropShadow(shadow(5.0, 90.0))]);
    assert!(close4(px(&d, 20, 32), [0.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 20, 32));
    assert!(close4(px(&d, 20, 8), [1.0; 4]));
    // Light from the right (0°, 3 o'clock): shadow to the left.
    let d = fx_doc(vec![Effect::DropShadow(shadow(5.0, 0.0))]);
    assert!(close4(px(&d, 8, 20), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 32, 20), [1.0; 4]));
}

#[test]
fn drop_shadow_uses_global_light_and_knockout() {
    let mut d = fx_doc(vec![Effect::DropShadow(Shadow { use_global_light: true, ..shadow(5.0, 0.0) })]);
    d.global_light.angle = 90.0;
    assert!(close4(px(&d, 20, 32), [0.0, 0.0, 0.0, 1.0]));
    // Knock-out: with fill 0 the shadow does not show through the layer.
    d.layers[1].fill_opacity = 0.0;
    assert!(close4(px(&d, 20, 28), [1.0; 4]), "{:?}", px(&d, 20, 28));
    if let Effect::DropShadow(s) = &mut d.layers[1].effects.items[0] {
        s.knocks_out = false;
    }
    assert!(close4(px(&d, 20, 28), [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn soft_shadow_is_blurred_and_bounded() {
    let d = fx_doc(vec![Effect::DropShadow(Shadow { spread: 0.0, size: 6.0, ..shadow(0.0, 90.0) })]);
    let edge = px(&d, 30, 20); // just outside the right edge
    let far = px(&d, 39, 20);
    assert!(edge[0] < 0.8 && edge[0] > 0.2, "{edge:?}");
    assert!(far[0] > 0.99, "{far:?}");
}

#[test]
fn inner_shadow_only_inside() {
    let d = fx_doc(vec![Effect::InnerShadow(Shadow { knocks_out: false, ..shadow(4.0, 90.0) })]);
    // Top rows of the square are shadowed (light from the top pushes the
    // outside's shadow down into the shape).
    assert!(close4(px(&d, 20, 11), [0.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 20, 11));
    assert!(close4(px(&d, 20, 25), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 20, 5), [1.0; 4]));
}

fn glow(technique: GlowTechnique, source: GlowSource) -> Glow {
    Glow {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        paint: FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)),
        technique,
        spread: 0.0,
        size: 4.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source,
    }
}

#[test]
fn outer_and_inner_glow_regions() {
    let d = fx_doc(vec![Effect::OuterGlow(glow(GlowTechnique::Precise, GlowSource::Edge))]);
    let near = px(&d, 9, 20);
    assert!(near[1] > 0.7 && near[0] < 0.3, "{near:?}");
    assert!(close4(px(&d, 3, 20), [1.0; 4]));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]));
    let d = fx_doc(vec![Effect::InnerGlow(glow(GlowTechnique::Softer, GlowSource::Edge))]);
    let edge = px(&d, 10, 20);
    assert!(edge[1] > 0.3, "{edge:?}");
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 5, 20), [1.0; 4]));
}

#[test]
fn gradient_overlay_follows_angle_and_reverse() {
    let g = Gradient { stops: vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], angle: 0.0, ..Gradient::default() };
    let d = fx_doc(vec![Effect::GradientOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), gradient: g.clone(), dither: false }]);
    let (l, r) = (px(&d, 10, 20), px(&d, 29, 20));
    assert!(l[0] < 0.1 && r[0] > 0.9, "{l:?} {r:?}");
    let d = fx_doc(vec![Effect::GradientOverlay {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        gradient: Gradient { reverse: true, style: GradientStyle::Linear, ..g },
        dither: false,
    }]);
    assert!(px(&d, 10, 20)[0] > 0.9);
}

#[test]
fn satin_and_bevel_stay_inside_shape() {
    let satin = Effect::Satin(Satin {
        common: FxCommon::new(photocraft_color::BlendMode::Multiply, 1.0),
        color: Color::BLACK,
        angle: 19.0,
        distance: 4.0,
        size: 4.0,
        contour: photocraft_doc::Contour::Linear,
        anti_alias: true,
        invert: false,
    });
    let d = fx_doc(vec![
        satin,
        Effect::BevelEmboss(photocraft_doc::Bevel {
            enabled: true,
            style: photocraft_doc::BevelStyle::InnerBevel,
            technique: photocraft_doc::BevelTechnique::Smooth,
            depth: 1.0,
            up: true,
            size: 4.0,
            soften: 0.0,
            angle: 90.0,
            altitude: 30.0,
            use_global_light: false,
            gloss_contour: photocraft_doc::Contour::Linear,
            highlight: FxCommon::new(photocraft_color::BlendMode::Screen, 0.75),
            highlight_color: Color::WHITE,
            shadow: FxCommon::new(photocraft_color::BlendMode::Multiply, 0.75),
            shadow_color: Color::BLACK,
            contour: None,
            texture: None,
        }),
    ]);
    for (x, y) in [(5, 5), (35, 20), (20, 35)] {
        assert!(close4(px(&d, x, y), [1.0; 4]), "({x},{y}) {:?}", px(&d, x, y));
    }
    // Light from the top: the top bevel edge is brighter than the bottom one.
    let (top, bot) = (px(&d, 20, 11), px(&d, 20, 28));
    assert!(top[1] > bot[1], "{top:?} {bot:?}");
}

#[test]
fn clipped_layer_effects_are_clipped_to_base() {
    let mut d = doc_white(40, 40);
    d.layers.push(solid_layer("base", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]));
    let mut c = solid_layer("clip", Rect::new(20, 10, 40, 30), [0.0, 1.0, 0.0, 1.0]);
    c.clipped = true;
    c.effects.items.push(stroke(3.0, StrokePosition::Outside));
    d.layers.push(c);
    assert!(close4(px(&d, 18, 20), [0.0, 0.0, 1.0, 1.0]), "stroke inside base");
    assert!(close4(px(&d, 20, 8), [1.0; 4]), "stroke clipped outside base");
}

#[test]
fn group_effects_apply_to_group_shape() {
    let mut d = doc_white(40, 40);
    let mut g = Layer::group(
        "g",
        vec![solid_layer("a", Rect::new(10, 10, 20, 30), [1.0, 0.0, 0.0, 1.0]), solid_layer("b", Rect::new(20, 10, 30, 30), [1.0, 0.0, 0.0, 1.0])],
    );
    g.effects.items.push(stroke(2.0, StrokePosition::Outside));
    d.layers.push(g);
    assert!(close4(px(&d, 9, 20), [0.0, 0.0, 1.0, 1.0]));
    assert!(close4(px(&d, 20, 20), [1.0, 0.0, 0.0, 1.0]), "no stroke at the seam between children");
}

#[test]
fn effects_render_identically_in_tiles() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut d = fx_doc(vec![Effect::DropShadow(Shadow { spread: 0.0, size: 5.0, ..shadow(4.0, 120.0) }), stroke(2.0, StrokePosition::Outside)]);
        d.depth = depth;
        for layer in &mut d.layers {
            if let Some(surface) = layer.surface_mut() {
                *surface = surface.convert(photocraft_color::PixelFormat { sample: depth, ..surface.format() });
            }
        }
        let full = flatten(&d);
        // Compare every boundary pixel: a few sampled points miss application-halo regressions.
        for tile in [1, 7, 33] {
            assert_eq!(render_tiled(&d, d.bounds(), tile), full, "{depth:?}, tile {tile}");
        }
    }
}

#[test]
fn parallel_tiles_match_single_pass() {
    // A document exercising masks, clipping, groups, adjustments, fills and effects.
    let mut d = doc_white(97, 61);
    let mut a = solid_layer("a", Rect::new(5, 5, 70, 50), [1.0, 0.2, 0.1, 0.8]);
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 40, 61), &[0.25]);
    m.density = 0.8;
    a.mask = Some(m);
    a.effects.items.push(stroke(3.0, StrokePosition::Outside));
    d.layers.push(a);
    let mut clip = solid_layer("clip", Rect::new(30, 0, 97, 61), [0.0, 1.0, 0.0, 1.0]);
    clip.clipped = true;
    clip.blend = photocraft_color::BlendMode::Multiply;
    d.layers.push(clip);
    let mut g = Layer::group("g", vec![solid_layer("in", Rect::new(50, 20, 90, 60), [0.2, 0.3, 0.9, 1.0])]);
    g.opacity = 0.6;
    d.layers.push(g);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    let mut f = Layer::new("fill", LayerContent::Fill(Fill::Solid(Color::rgb(0.5, 0.5, 0.0))));
    f.opacity = 0.3;
    d.layers.push(f);
    let r = d.bounds();
    let whole = render_tiled(&d, r, 10_000);
    for tile in [7, 16, 33] {
        assert_eq!(render_tiled(&d, r, tile), whole, "tile {tile}");
    }
    assert_eq!(render(&d, r), whole);
}

#[test]
fn vector_mask_combines_with_pixel_mask() {
    use photocraft_doc::{Path, Subpath, VectorMask};
    let mut d = doc_white(8, 8);
    let mut l = solid_layer("k", Rect::new(0, 0, 8, 8), [0.0, 0.0, 0.0, 1.0]);
    // Vector mask reveals x in 0..4 (and half of column 4); the pixel mask hides rows 0..2.
    l.vector_mask = Some(VectorMask::new(Path::new(vec![Subpath::polygon(&[(0.0, 0.0), (4.5, 0.0), (4.5, 8.0), (0.0, 8.0)])])));
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 8, 2), &[0.0]);
    l.mask = Some(m);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 4), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 4), [1.0; 4]));
    assert!(close4(px(&d, 4, 4), [0.5, 0.5, 0.5, 1.0]));
    assert!(close4(px(&d, 1, 1), [1.0; 4]));
    // Tiled rendering gives the same result.
    let whole = render_tiled(&d, d.bounds(), 3);
    assert_eq!(whole.px, render_tiled(&d, d.bounds(), 256).px);
    // Density and disabling.
    let top = d.layers.len() - 1;
    let vm = d.layers[top].vector_mask.as_mut().unwrap();
    vm.density = 0.5;
    assert!(close4(px(&d, 6, 4), [0.5, 0.5, 0.5, 1.0]));
    d.layers[top].vector_mask.as_mut().unwrap().enabled = false;
    assert!(close4(px(&d, 6, 4), [0.0, 0.0, 0.0, 1.0]));
    // Without a pixel mask the vector mask alone applies.
    d.layers[top].mask = None;
    d.layers[top].vector_mask.as_mut().unwrap().enabled = true;
    d.layers[top].vector_mask.as_mut().unwrap().density = 1.0;
    assert!(close4(px(&d, 1, 1), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 1), [1.0; 4]));
}

#[test]
fn effect_maps_are_cached_and_invalidated_by_pixel_changes() {
    let mut doc = doc_white(64, 64);
    let mut l = solid_layer("fx", Rect::new(16, 16, 48, 48), [1.0, 0.0, 0.0, 1.0]);
    l.effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    doc.layers.push(l);
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES);
    let cx = Ctx::for_doc(&doc, &patterns);
    let a = effect_maps(&doc.layers[1], &cx);
    let b = effect_maps(&doc.layers[1], &cx);
    assert!(std::sync::Arc::ptr_eq(&a, &b), "second request hits the cache");
    let first = flatten(&doc);
    // Editing the layer's pixels changes its tiles, so the maps are rebuilt.
    doc.layers[1].surface_mut().unwrap().fill_rect(Rect::new(8, 8, 20, 20), &[0.0, 0.0, 1.0, 1.0]);
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES);
    let cx = Ctx::for_doc(&doc, &patterns);
    let c = effect_maps(&doc.layers[1], &cx);
    assert!(!std::sync::Arc::ptr_eq(&a, &c), "pixel edit invalidates");
    // Cached rendering equals a fresh build (tiled and full renders agree too).
    let again = flatten(&doc);
    let region = doc.bounds();
    let tiled = render_tiled(&doc, region, 16);
    for (p, q) in again.px.iter().zip(&tiled.px) {
        assert!(close4(*p, *q));
    }
    assert_ne!(first.px, again.px);

    // Tileless masks differ only in their defaults; reusing their effect maps leaves a ghost shadow.
    let mut masked = fx_doc(vec![Effect::DropShadow(shadow(5.0, 90.0))]);
    masked.layers[1].mask = Some(photocraft_doc::LayerMask::reveal_all());
    assert!(close4(px(&masked, 20, 32), [0.0, 0.0, 0.0, 1.0]));
    masked.layers[1].mask = Some(photocraft_doc::LayerMask::hide_all());
    let warm = flatten(&masked);
    let mut cold = masked.clone();
    cold.layers[1].id = Layer::raster("uncached", cold.pixel_format()).id;
    assert!(warm.px == flatten(&cold).px, "default-only mask changes must invalidate cached effects");
    assert!(close4(px(&masked, 20, 32), [1.0; 4]));
}

// ---------- PSD-fidelity effect semantics (fitted on Photoshop composites) ----------

#[test]
fn stroke_corners_follow_the_5x5_chamfer_metric() {
    // Square 10..30, outside stroke 3: pixel (9, 7) is offset (1, 3) from the corner pixel.
    // Photoshop measures √5 + 1 = 3.236 there (exact distance 3.162): coverage 3 + 1 − 3.236.
    let d = fx_doc(vec![stroke(3.0, StrokePosition::Outside)]);
    let a = 1.0 - px(&d, 9, 7)[0]; // blue stroke over white: red channel drops by coverage
    assert!((a - 0.764).abs() < 0.01, "{a}");
    // (2, 2) stays Euclidean (two diagonal steps).
    let a = 1.0 - px(&d, 8, 8)[0];
    assert!((a - 1.0).abs() < 0.01, "{a}");
}

#[test]
fn inside_stroke_respects_partial_edge_coverage() {
    // A 25 %-alpha edge column puts the edge 0.75 px further out: a 3 px inside stroke then
    // reaches 75 % into the fourth column, and the edge column takes the stroke colour at 25 %.
    let mut d = doc_white(40, 40);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 11, 30), &[1.0, 0.0, 0.0, 0.25]);
    l.effects.items = vec![stroke(3.0, StrokePosition::Inside)];
    d.layers.push(l);
    let edge = px(&d, 10, 20);
    assert!(close4(edge, [0.75, 0.75, 1.0, 1.0]), "edge = blue stroke at 25 % over white: {edge:?}");
    let p = px(&d, 13, 20);
    assert!((p[2] - 0.75).abs() < 0.02 && (p[0] - 0.25).abs() < 0.02, "{p:?}");
}

#[test]
fn emboss_shades_the_composited_layer() {
    // Emboss styles paint over the layer as composited (for type: mixed at the text gamma), so a
    // Normal white highlight `k` on an edge pixel keeps (1 − R) = (1 − k)(1 − C) whatever C is:
    // the ratio of type to raster results equals that of their plain composites.
    let emboss = Effect::BevelEmboss(photocraft_doc::Bevel {
        enabled: true,
        style: photocraft_doc::BevelStyle::Emboss,
        technique: photocraft_doc::BevelTechnique::Smooth,
        depth: 1.0,
        up: true,
        size: 6.0,
        soften: 0.0,
        angle: 90.0,
        altitude: 30.0,
        use_global_light: false,
        gloss_contour: photocraft_doc::Contour::Linear,
        highlight: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        highlight_color: Color::WHITE,
        shadow: FxCommon::new(photocraft_color::BlendMode::Multiply, 0.0),
        shadow_color: Color::BLACK,
        contour: None,
        texture: None,
    });
    let make = |text: bool, fx: bool| {
        let mut d = doc_white(40, 40);
        let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [0.2, 0.1, 0.6, 1.0]);
        l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 30, 11), &[0.2, 0.1, 0.6, 0.5]);
        if text {
            let t = photocraft_doc::TextLayer { cache: l.surface().cloned(), ..Default::default() };
            l = Layer::new("t", LayerContent::Text(t));
        }
        if fx {
            l.effects.items = vec![emboss.clone()];
        }
        d.layers.push(l);
        px(&d, 20, 10)
    };
    let (ct, cr, rt, rr) = (make(true, false), make(false, false), make(true, true), make(false, true));
    assert!(rr[1] > cr[1] + 0.05, "the top edge is lit: {rr:?} vs {cr:?}");
    assert!((ct[1] - cr[1]).abs() > 0.02, "text gamma changes the edge: {ct:?} {cr:?}");
    for c in 0..3 {
        let want = (1.0 - ct[c]) / (1.0 - cr[c]);
        assert!(((1.0 - rt[c]) / (1.0 - rr[c]) - want).abs() < 2e-3, "channel {c}: {rt:?} {rr:?} {ct:?} {cr:?}");
    }
}

#[test]
fn interior_effects_keep_the_layer_alpha() {
    // A colour overlay replaces a half-transparent pixel's colour without adding coverage.
    let mut d = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 0.5]);
    l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0), color: Color::rgb(0.0, 1.0, 0.0) }];
    d.layers.push(l);
    assert!(close4(px(&d, 20, 20), [0.0, 1.0, 0.0, 0.5]), "{:?}", px(&d, 20, 20));
}

#[test]
fn upper_stroke_blends_with_the_backdrop_not_the_lower_stroke() {
    // Multiply yellow (3 px) listed above a Normal blue (8 px): the inner ring multiplies the
    // white backdrop (stays yellow), the outer ring is blue.
    let yellow = Effect::Stroke(StrokeFx {
        common: FxCommon::new(photocraft_color::BlendMode::Multiply, 1.0),
        size: 3.0,
        position: StrokePosition::Outside,
        paint: FxPaint::Color(Color::rgb(1.0, 1.0, 0.0)),
    });
    let d = fx_doc(vec![yellow, stroke(8.0, StrokePosition::Outside)]);
    assert!(close4(px(&d, 8, 20), [1.0, 1.0, 0.0, 1.0]), "{:?}", px(&d, 8, 20));
    assert!(close4(px(&d, 4, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 4, 20));
}

#[test]
fn shadow_knockout_needs_see_through_fill() {
    // At 100 % fill an anti-aliased edge is not attenuated twice: the result equals no knockout.
    let mut base = Document::new("t", Size::new(40, 40), ColorMode::Rgb, SampleType::U8);
    base.layers.push(solid_layer("bg", Rect::new(0, 0, 40, 40), [1.0; 4]));
    let mut l = solid_layer("sq", Rect::new(10, 10, 30, 30), [1.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 11, 30), &[1.0, 0.0, 0.0, 0.5]);
    l.effects.items = vec![Effect::DropShadow(Shadow { distance: 0.0, spread: 1.0, size: 4.0, ..shadow(0.0, 0.0) })];
    base.layers.push(l);
    let on = px(&base, 10, 20);
    if let Effect::DropShadow(s) = &mut base.layers[1].effects.items[0] {
        s.knocks_out = false;
    }
    assert!(close4(on, px(&base, 10, 20)), "{on:?}");
}

#[test]
fn linked_pattern_overlay_anchors_at_the_effects_reference_point() {
    use photocraft_doc::pattern::Pattern;
    // 2 × 1 tile: red, blue. Anchored at x = 11 (reference point), x = 11 is red, 12 blue.
    let mut tile = photocraft_raster::Surface::new(PixelFormat::RGBA8);
    tile.fill_rect(Rect::new(0, 0, 1, 1), &[1.0, 0.0, 0.0, 1.0]);
    tile.fill_rect(Rect::new(1, 0, 2, 1), &[0.0, 0.0, 1.0, 1.0]);
    let pat = Pattern::new("rb", tile, 2, 1);
    let overlay = Effect::PatternOverlay {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
        name: "rb".into(),
        id: pat.id.clone(),
        scale: 1.0,
        angle: 0.0,
        link: true,
        phase: (0.0, 0.0),
    };
    let mut d = fx_doc(vec![overlay]);
    d.patterns.push(pat);
    // Without a reference point the layer's top-left (10) anchors the tiling.
    assert!(close4(px(&d, 10, 20), [1.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 10, 20));
    d.layers[1].effects.reference = Some((11.0, 0.0));
    assert!(close4(px(&d, 11, 20), [1.0, 0.0, 0.0, 1.0]), "{:?}", px(&d, 11, 20));
    assert!(close4(px(&d, 12, 20), [0.0, 0.0, 1.0, 1.0]), "{:?}", px(&d, 12, 20));
}

#[test]
fn channel_restrictions_keep_the_backdrop() {
    // Blue left out (Photoshop's Advanced Blending › Channels: R, G only).
    let mut d = doc_white(8, 8);
    let mut l = solid_layer("dark", Rect::new(0, 0, 8, 8), [0.2, 0.3, 0.4, 1.0]);
    l.excluded_channels = 0b100;
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [0.2, 0.3, 1.0, 1.0]));
    // Adjustment layers honour it too.
    let mut inv = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    inv.excluded_channels = 0b001;
    d.layers.push(inv);
    assert!(close4(px(&d, 1, 1), [0.2, 0.7, 0.0, 1.0]));
    // CMYK documents composite in display RGB: no exact equivalent, ignored.
    assert_eq!(channel_weights(&d.layers[1], ColorMode::Cmyk), None);
    assert_eq!(channel_weights(&d.layers[1], ColorMode::Rgb), Some([1.0, 1.0, 0.0]));
}

fn range(black: [u8; 2], white: [u8; 2]) -> photocraft_doc::BlendRange {
    photocraft_doc::BlendRange { black, white }
}

const FULL: photocraft_doc::BlendRange = photocraft_doc::BlendRange::FULL;

#[test]
fn blend_if_this_layer_hides_by_the_layers_own_value() {
    // Mid-grey backdrop; the layer is black on the left, white on the right.
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("grey", Rect::new(0, 0, 8, 8), [0.5, 0.5, 0.5, 1.0]));
    let mut l = solid_layer("bw", Rect::new(0, 0, 4, 8), [0.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(4, 0, 8, 8), &[1.0, 1.0, 1.0, 1.0]);
    // Gray › This Layer: black point at 50 hides the blacks.
    l.blend_if.set(0, [range([50, 50], [255, 255]), FULL]);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [0.5, 0.5, 0.5, 1.0]), "{:?}", px(&d, 1, 1));
    assert!(close4(px(&d, 6, 1), [1.0, 1.0, 1.0, 1.0]));
    // White point at 200 hides the whites as well.
    d.layers[2].blend_if.set(0, [range([50, 50], [200, 200]), FULL]);
    assert!(close4(px(&d, 6, 1), [0.5, 0.5, 0.5, 1.0]));
    // Back to the defaults: everything shows again.
    d.layers[2].blend_if.set(0, [FULL, FULL]);
    assert!(d.layers[2].blend_if.is_default());
    assert!(close4(px(&d, 1, 1), [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn blend_if_underlying_layer_hides_by_the_backdrop_value() {
    // Backdrop black on the left, white on the right; a red layer over all of it.
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("black", Rect::new(0, 0, 4, 8), [0.0, 0.0, 0.0, 1.0]));
    let mut l = solid_layer("red", Rect::new(0, 0, 8, 8), [1.0, 0.0, 0.0, 1.0]);
    // Gray › Underlying Layer: white point at 128 = only over the darks (sky-replacement style).
    l.blend_if.set(0, [FULL, range([0, 0], [128, 128])]);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 1), [1.0, 1.0, 1.0, 1.0]));
}

#[test]
fn blend_if_split_point_fades_like_opacity() {
    // A split black point (Alt-drag) at 50/150 shows a value-100 layer at half strength, which
    // is exactly the layer at 50% opacity, also over a semi-transparent backdrop.
    let v = 100.0 / 255.0;
    let mut d = doc_white(4, 4);
    d.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 4), &[0.0, 0.2, 1.0, 0.5]);
    let mut l = solid_layer("v", Rect::new(0, 0, 4, 4), [v, v, v, 1.0]);
    l.blend = BlendMode::Multiply;
    let mut half = l.clone();
    half.opacity = 0.5;
    l.blend_if.set(0, [range([50, 150], [255, 255]), FULL]);
    let mut d2 = d.clone();
    d.layers.push(l);
    d2.layers.push(half);
    let (a, b) = (px(&d, 1, 1), px(&d2, 1, 1));
    assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4), "{a:?} vs {b:?}");
}

#[test]
fn blend_if_per_channel_ranges() {
    // Blue › This Layer: hide pixels whose blue is above 100.
    let mut d = doc_white(8, 8);
    let mut l = solid_layer("c", Rect::new(0, 0, 4, 8), [1.0, 0.0, 0.0, 1.0]);
    l.surface_mut().unwrap().fill_rect(Rect::new(4, 0, 8, 8), &[0.0, 0.0, 1.0, 1.0]);
    l.blend_if.set(3, [range([0, 0], [100, 100]), FULL]);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [1.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 1), [1.0, 1.0, 1.0, 1.0]));
    // Red › Underlying Layer: the backdrop's red (255) is above 254 → hidden everywhere.
    d.layers[1].blend_if = Default::default();
    d.layers[1].blend_if.set(1, [FULL, range([0, 0], [254, 254])]);
    assert!(close4(px(&d, 1, 1), [1.0; 4]));
}

#[test]
fn blend_if_on_adjustment_and_clipped_layers() {
    let mut d = doc_white(8, 8);
    d.layers.push(solid_layer("dark", Rect::new(0, 0, 4, 8), [0.1, 0.1, 0.1, 1.0]));
    // Invert, but only where the backdrop is dark (Underlying white point at 128).
    let mut inv = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    inv.blend_if.set(0, [FULL, range([0, 0], [128, 128])]);
    d.layers.push(inv);
    assert!(close4(px(&d, 1, 1), [0.9, 0.9, 0.9, 1.0]));
    assert!(close4(px(&d, 6, 1), [1.0; 4]));
    // A clipped layer judges "underlying" by its clipping base.
    let mut d = doc_white(8, 8);
    let mut base = solid_layer("base", Rect::new(0, 0, 8, 8), [0.0, 0.0, 0.0, 1.0]);
    base.surface_mut().unwrap().fill_rect(Rect::new(4, 0, 8, 8), &[0.9, 0.9, 0.9, 1.0]);
    let mut clip = solid_layer("clip", Rect::new(0, 0, 8, 8), [0.0, 1.0, 0.0, 1.0]);
    clip.clipped = true;
    clip.blend_if.set(0, [FULL, range([128, 128], [255, 255])]);
    d.layers.push(base);
    d.layers.push(clip);
    assert!(close4(px(&d, 1, 1), [0.0, 0.0, 0.0, 1.0]));
    assert!(close4(px(&d, 6, 1), [0.0, 1.0, 0.0, 1.0]));
}

#[test]
fn blend_if_modes() {
    // Grayscale documents: the single channel's entry (or the gray entry) applies.
    let mut d = Document::with_background("g", Size::new(4, 4), ColorMode::Grayscale, SampleType::U8, Color::WHITE);
    let mut l = Layer::raster("k", d.pixel_format());
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 4, 4), &[0.0, 1.0]);
    l.blend_if.set(1, [range([10, 10], [255, 255]), FULL]);
    d.layers.push(l);
    assert!(close4(px(&d, 1, 1), [1.0; 4]), "{:?}", px(&d, 1, 1));
    // CMYK/Lab composite in display RGB: kept for round trip, not applied.
    assert!(blend_if_active(&d.layers[1], ColorMode::Rgb));
    assert!(blend_if_active(&d.layers[1], ColorMode::Grayscale));
    assert!(!blend_if_active(&d.layers[1], ColorMode::Cmyk));
    assert!(!blend_if_active(&d.layers[1], ColorMode::Lab));
    assert!(!blend_if_active(&d.layers[0], ColorMode::Rgb));
}

#[test]
fn effect_maps_built_inside_parallel_tiles_do_not_deadlock() {
    // A map large enough for the blur to go multi-threaded, built while rayon renders tiles that
    // all wait on the same map (a rayon-parallel blur deadlocked here).
    let mut d = doc_white(900, 700);
    let mut l = solid_layer("fx", Rect::new(100, 100, 800, 600), [0.2, 0.4, 0.9, 1.0]);
    l.effects.items = vec![photocraft_doc::Effect::default_drop_shadow()];
    if let photocraft_doc::Effect::DropShadow(s) = &mut l.effects.items[0] {
        s.size = 30.0;
    }
    d.layers.push(l);
    let out = render(&d, d.bounds());
    assert_eq!(out.px.len(), 900 * 700);
}

#[test]
fn adjustment_results_are_rounded_to_the_document_depth() {
    for (depth, q) in [(SampleType::U8, Some(255.0f32)), (SampleType::U16, Some(32768.0)), (SampleType::F32, None)] {
        let mut d = Document::with_background("q", Size::new(4, 4), ColorMode::Rgb, depth, Color::rgb(0.3, 0.6, 0.9));
        d.layers.push(Layer::new("lv", LayerContent::Adjustment(Adjustment::Exposure { exposure: 0.37, offset: 0.0, gamma: 1.0 })));
        let p = px(&d, 1, 1);
        match q {
            Some(q) => assert!(p.iter().all(|v| ((v * q).round() - v * q).abs() < 1e-3), "{depth:?} {p:?}"),
            None => assert!(p[..3].iter().any(|v| ((v * 255.0).round() - v * 255.0).abs() > 1e-3), "{p:?}"),
        }
    }
}

// Ground truth captured from Adobe Photoshop 2026 (27.10.0): a 0..255 ramp pushed through modern
// (non-legacy) Brightness/Contrast via `executeAction("BrgC", ... useLegacy=false)`. These pin our
// reverse-engineered curves (see crates/compose/src/adjust.rs modern_* and log/devlog.md).
#[test]
fn modern_contrast_matches_photoshop() {
    // (contrast, [out at x = 0,16,32,64,96,128,160,192,224,255])
    let cases: [(f32, [u8; 10]); 4] = [
        (50.0, [0, 11, 23, 52, 87, 128, 169, 204, 233, 255]),
        (-50.0, [0, 21, 41, 76, 105, 128, 151, 180, 215, 255]),
        (100.0, [0, 5, 14, 40, 78, 128, 178, 216, 242, 255]),
        (-25.0, [0, 19, 37, 70, 101, 128, 155, 186, 220, 255]),
    ];
    let xs = [0usize, 16, 32, 64, 96, 128, 160, 192, 224, 255];
    for (c, out) in cases {
        for (i, &x) in xs.iter().enumerate() {
            let got = adjust::modern_contrast(x as f32 / 255.0, c) * 255.0;
            let err = (got - out[i] as f32).abs();
            assert!(err <= 2.0, "contrast {c} x={x}: got {got:.1} want {} (err {err:.1})", out[i]);
        }
    }
}

#[test]
fn modern_brightness_matches_photoshop() {
    let cases: [(f32, [u8; 10]); 2] = [(50.0, [0, 22, 44, 88, 132, 171, 203, 228, 246, 255]), (-50.0, [0, 12, 23, 47, 70, 93, 118, 148, 186, 255])];
    let xs = [0usize, 16, 32, 64, 96, 128, 160, 192, 224, 255];
    for (b, out) in cases {
        for (i, &x) in xs.iter().enumerate() {
            let got = adjust::modern_brightness(x as f32 / 255.0, b) * 255.0;
            let err = (got - out[i] as f32).abs();
            // The brightness roll-off is a spline; our fit is close but approximate at the top.
            assert!(err <= 6.0, "brightness {b} x={x}: got {got:.1} want {} (err {err:.1})", out[i]);
        }
    }
    // Endpoints and the zero slider are exact.
    assert_eq!(adjust::modern_brightness(0.0, 120.0), 0.0);
    assert_eq!(adjust::modern_brightness(1.0, -120.0), 1.0);
    assert_eq!(adjust::modern_brightness(0.37, 0.0), 0.37);
    assert_eq!(adjust::modern_contrast(0.37, 0.0), 0.37);
}

// Levels ground truth from Adobe Photoshop 2026 (adjustLevels on a 0..255 ramp). Verifies our
// `levels()` matches the real app, including the gamma>1 soft shadow toe (initial slope 2^gamma).
#[test]
fn levels_matches_photoshop() {
    let ident = |g: f32| LevelsChannel { in_black: 0.0, in_white: 1.0, gamma: g, out_black: 0.0, out_white: 1.0 };
    // gamma 2.0 (lifts shadows): x -> Photoshop out. The deep shadow is the toe-bounded region.
    let g2: [(usize, u8); 8] = [(1, 4), (8, 30), (16, 55), (32, 90), (64, 128), (128, 181), (192, 221), (224, 239)];
    for (x, ps) in g2 {
        let got = (adjust::levels(&ident(2.0), x as f32 / 255.0) * 255.0).round();
        assert!((got - ps as f32).abs() <= 4.0, "levels gamma 2.0 x={x}: got {got} want {ps}");
    }
    // gamma 0.5 (darkens midtones) is exact: out = (x/255)^2.
    for x in [0usize, 32, 64, 128, 192, 255] {
        let got = (adjust::levels(&ident(0.5), x as f32 / 255.0) * 255.0).round();
        let want = ((x as f32 / 255.0).powi(2) * 255.0).round();
        assert_eq!(got, want, "levels gamma 0.5 x={x}");
    }
    // Endpoints pinned; identity is identity. (The gamma>1 toe's soft-min leaves white within a
    // few parts in 1e5 of 1.0 — invisible at any bit depth; exact at 8-bit.)
    assert_eq!(adjust::levels(&ident(2.5), 0.0), 0.0);
    assert_eq!((adjust::levels(&ident(2.5), 1.0) * 255.0).round(), 255.0);
    assert!((adjust::levels(&ident(2.5), 1.0) - 1.0).abs() < 5e-4);
    assert!((adjust::levels(&ident(1.0), 0.37) - 0.37).abs() < 1e-6);
}

// Photoshop (psd-tools levels_grayscale, 8-bit): input range 44..214, gamma 1.78. The stretch
// is rounded to whole levels before the gamma (45 → 1.5 → 2, 47 → 4.5 → 5), so the shadows
// step unevenly: 45, 46, 47, 48, 52, 69 → 7, 10, 17, 20, 37, 88.
#[test]
fn levels_work_on_whole_levels() {
    let ch = LevelsChannel { in_black: 44.0 / 255.0, in_white: 214.0 / 255.0, gamma: 1.78, out_black: 0.0, out_white: 1.0 };
    let adj = Adjustment::Levels {
        master: LevelsChannel::default(),
        per_channel: [ch.clone(), ch.clone(), ch.clone()],
        space: Default::default(),
        black: LevelsChannel::default(),
    };
    let luts = adjust::tone_luts_q(&adj, Some(255.0));
    let at = |v: u8| {
        let x = f32::from(v) / 255.0 * 4095.0;
        let (i, f) = (x.floor() as usize, x.fract());
        let y = luts[0][i] * (1.0 - f) + luts[0][(i + 1).min(4095)] * f;
        (y * 255.0).round()
    };
    for (v, ps) in [(44u8, 0.0f32), (45, 7.0), (46, 10.0), (47, 17.0), (48, 20.0), (52, 37.0), (69, 88.0), (100, 137.0), (214, 255.0)] {
        assert!((at(v) - ps).abs() <= 1.0, "levels {v}: got {} want {ps}", at(v));
    }
    // 16-bit and float stay smooth (no 8-bit staircase).
    for q in [Some(32768.0), None] {
        let a = adjust::levels_q(&ch, 0.2, q);
        let b = adjust::levels_q(&ch, 0.2 + 1.0 / 2048.0, q);
        assert!(b > a && b - a < 2.0 / 255.0, "{q:?}: {a} {b}");
    }
    // Hostile quantum values fall back to the continuous curve.
    for q in [Some(0.0), Some(f32::NAN), Some(-3.0)] {
        assert_eq!(adjust::levels_q(&ch, 0.4, q), adjust::levels(&ch, 0.4));
    }
}

// Exposure linearises RGB documents through a 2.2 power, not the sRGB curve: Photoshop lifts 76
// to 134 with an offset of 0.1738 (psd-tools adjustment_nested_composition_4).
#[test]
fn exposure_offset_uses_gamma_2_2_in_rgb() {
    let mut b = Buffer::filled(Rect::new(0, 0, 1, 1), [76.0 / 255.0, 76.0 / 255.0, 76.0 / 255.0, 1.0]);
    adjust::apply_with(&Adjustment::Exposure { exposure: 0.0, offset: 0.1738, gamma: 1.0 }, &mut b, adjust::Transfer::Srgb);
    assert_eq!((b.px[0][0] * 255.0).round(), 134.0);
    assert_eq!(adjust::Transfer::Gamma(1.732).for_exposure(), adjust::Transfer::Gamma(1.732));
}

// A 30° Reflected gradient fill on a 4 × 4 canvas renders as Photoshop's (its end point snaps to
// the corner: t = |x − y| / 4) at every depth.
#[test]
fn small_gradient_fill_matches_photoshop_at_all_depths() {
    let stops = vec![(0.0, Color::rgb(0.0, 0.0, 0.0)), (1.0, Color::rgb(1.0, 1.0, 1.0))];
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut d = Document::new("g", Size::new(4, 4), ColorMode::Rgb, depth);
        let fill = Fill::gradient(stops.clone(), 30.0, 1.0, photocraft_doc::GradientStyle::Reflected, false);
        d.layers.push(Layer::new("g", LayerContent::Fill(fill)));
        let out = flatten(&d);
        for y in 0..4usize {
            for x in 0..4usize {
                let want = (x as f32 - y as f32).abs() / 4.0;
                let got = out.px[y * 4 + x][0];
                assert!((got - want).abs() < 1.0 / 255.0, "{depth:?} ({x},{y}): {got} vs {want}");
            }
        }
    }
}

/// A tall document with soft content, a translucent region and an adjustment.
fn tall_doc(w: u32, h: u32) -> Document {
    let mut d = doc_white(w, h);
    let mut l = Layer::raster("grad", PixelFormat::RGBA8);
    let s = l.surface_mut().unwrap();
    for y in 0..h as i32 {
        s.fill_rect(Rect::new(0, y, w as i32 / 2, y + 1), &[y as f32 / h as f32, 0.3, 1.0 - y as f32 / h as f32, 1.0]);
    }
    s.fill_rect(Rect::new(w as i32 / 2, 0, w as i32, h as i32 / 3), &[0.9, 0.1, 0.1, 0.4]);
    d.layers.push(l);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    d
}

#[test]
fn render_tiled_in_place_matches_one_tile_with_many_bands() {
    // Enough bands for the band-parallel path, and few for the tile-parallel one.
    let d = tall_doc(37, 900);
    let whole = render_tiled(&d, d.bounds(), 10_000);
    for tile in [5, 64, 300] {
        assert_eq!(render_tiled(&d, d.bounds(), tile), whole, "tile {tile}");
    }
    let r = Rect::new(3, 17, 30, 811);
    assert_eq!(render_tiled(&d, r, 4).px, render_tiled(&d, r, 10_000).px);
    assert!(render_tiled(&d, Rect::new(5, 5, 5, 900), 4).px.is_empty());
}

#[test]
fn render_bands_cover_the_rect_in_order() {
    let d = tall_doc(20, 700);
    let r = Rect::new(2, 10, 18, 690);
    let mut rows = Vec::new();
    let mut px = Vec::new();
    render_bands(&d, r, 100, |b| -> Result<(), ()> {
        assert_eq!((b.rect.x0, b.rect.x1), (r.x0, r.x1));
        rows.push((b.rect.y0, b.rect.y1));
        px.extend(b.px);
        Ok(())
    })
    .unwrap();
    // 100 rows round up to one 256-row tile band.
    assert_eq!(rows, vec![(10, 266), (266, 522), (522, 690)]);
    assert_eq!(px, render(&d, r).px);
    // The sink's error stops the render.
    let mut n = 0;
    assert_eq!(
        render_bands(&d, r, 1, |_| {
            n += 1;
            Err("stop")
        }),
        Err("stop")
    );
    assert_eq!(n, 1);
    assert_eq!(band_rows_for(14_000, 0) % RENDER_TILE, 0);
    assert!(band_rows_for(1, 0) > 0 && band_rows_for(0, -5) > 0);
}

/// The previous thumbnail: flatten, then area-average (premultiplied).
fn reference_reduce(doc: &Document, w: usize, h: usize) -> Vec<[f32; 4]> {
    let full = flatten(doc);
    let (fw, fh) = (full.rect.width() as usize, full.rect.height() as usize);
    let mut out = Vec::new();
    for ty in 0..h {
        let (y0, y1) = (ty * fh / h, ((ty + 1) * fh / h).max(ty * fh / h + 1).min(fh));
        for tx in 0..w {
            let (x0, x1) = (tx * fw / w, ((tx + 1) * fw / w).max(tx * fw / w + 1).min(fw));
            let mut acc = [0.0f32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = full.px[y * fw + x];
                    for c in 0..3 {
                        acc[c] += p[c] * p[3];
                    }
                    acc[3] += p[3];
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as f32;
            out.push(if acc[3] > 0.0 { [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3] / n] } else { [0.0; 4] });
        }
    }
    out
}

#[test]
fn reduced_render_matches_the_full_composite_averaged() {
    let d = tall_doc(53, 1000);
    for (w, h, band) in [(10, 190, 0), (7, 33, 256), (53, 999, 512), (1, 1, 256)] {
        let got = render_reduced_in_bands(&d, w, h, None, band);
        assert_eq!((got.rect.width(), got.rect.height()), (w, h));
        assert_eq!(got.px, reference_reduce(&d, w as usize, h as usize), "{w}x{h} band {band}");
    }
    // Full size is the composite itself; an empty document reduces to transparency.
    assert_eq!(render_reduced(&d, 53, 1000).px, flatten(&d).px);
    let empty = Document::new("e", Size::new(0, 0), ColorMode::Rgb, SampleType::U8);
    assert_eq!(render_reduced(&empty, 4, 4).px, vec![[0.0; 4]]);
}

#[test]
fn reduced_damage_matches_the_whole_reduction() {
    // A reduced canvas texture updates only what a stroke touched: those pixels must equal the
    // whole reduction's, for uneven factors, damage on span edges and bands smaller than the area.
    let d = tall_doc(53, 1000);
    for (w, h, band) in [(10, 190, 0), (7, 33, 256), (53, 999, 64), (53, 1000, 0), (1, 1, 256)] {
        let all = render_reduced_in_bands(&d, w, h, None, band);
        for dmg in [Rect::new(0, 0, 1, 1), Rect::new(5, 17, 6, 18), Rect::new(12, 300, 40, 701), Rect::new(-9, 990, 80, 2000), Rect::new(0, 0, 53, 1000)] {
            let part = render_reduced_in_bands(&d, w, h, Some(dmg), band);
            let r = part.rect;
            assert!(!r.is_empty() && r.x0 >= 0 && r.y0 >= 0 && r.x1 as u32 <= w && r.y1 as u32 <= h, "{w}x{h} {dmg:?} -> {r:?}");
            for y in r.y0..r.y1 {
                for x in r.x0..r.x1 {
                    assert_eq!(part.get(x, y), all.get(x, y), "{w}x{h} band {band} {dmg:?} at {x},{y}");
                }
            }
            // Exactly the output pixels whose source area meets the damage.
            let span = |t: u32, f: u32, n: u32| (t * f / n, ((t + 1) * f / n).max(t * f / n + 1).min(f));
            let d = dmg.intersect(&Rect::new(0, 0, 53, 1000));
            for y in 0..h {
                for x in 0..w {
                    let ((x0, x1), (y0, y1)) = (span(x, 53, w), span(y, 1000, h));
                    let meets = !Rect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32).intersect(&d).is_empty();
                    assert_eq!(r.contains(x as i32, y as i32), meets, "{w}x{h} {dmg:?} at {x},{y}");
                }
            }
        }
        assert!(render_reduced_in_bands(&d, w, h, Some(Rect::new(60, 0, 70, 10)), band).rect.is_empty());
    }
}

#[test]
fn large_documents_thumbnail_from_a_proxy() {
    // 4200 × 4200 (17.6 MP) is above PROXY_THUMBNAIL_PIXELS: the proxy path must still give a
    // thumbnail of the right size whose colours match the exact reduction.
    let mut d = doc_white(4200, 4200);
    d.layers.push(solid_layer("red", Rect::new(0, 0, 2100, 4200), [1.0, 0.0, 0.0, 1.0]));
    let t = thumbnail(&d, 100);
    assert_eq!((t.width, t.height), (100, 100));
    let at = |x: usize, y: usize| t.pixels[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4].to_vec();
    assert_eq!(at(10, 50), vec![255, 0, 0, 255]);
    assert_eq!(at(90, 50), vec![255, 255, 255, 255]);
    // Effects don't scale with a proxy: such documents reduce the exact composite.
    assert!(proxy::proxy_faithful(&d));
    d.layers[1].effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    assert!(!proxy::proxy_faithful(&d));
}

/// Layers whose effect maps are built with nested Rayon work (a gradient fill's rows render in
/// parallel), each carrying a different effect.
#[cfg(not(target_arch = "wasm32"))]
fn nested_parallel_fx_doc() -> Document {
    use photocraft_doc::{Contour, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, StrokeFx, StrokePosition};
    let mut d = doc_white(512, 512);
    let g = |a: f32| Fill::gradient(vec![(0.0, Color::BLACK), (1.0, Color::rgb(0.2, 0.5, 0.9))], a, 1.0, photocraft_doc::GradientStyle::Linear, false);
    let mut shadow = Layer::new("shadow", LayerContent::Fill(g(0.0)));
    shadow.effects.items = vec![Effect::default_drop_shadow()];
    let mut stroke = Layer::new("stroke", LayerContent::Fill(g(45.0)));
    stroke.opacity = 0.6;
    stroke.effects.items = vec![Effect::Stroke(StrokeFx {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        size: 3.0,
        position: StrokePosition::Inside,
        paint: FxPaint::Color(Color::rgb(1.0, 0.0, 0.0)),
    })];
    let mut glow = Layer::new("glow", LayerContent::Fill(g(90.0)));
    glow.opacity = 0.5;
    glow.effects.items = vec![Effect::InnerGlow(Glow {
        common: FxCommon::new(BlendMode::Screen, 0.75),
        paint: FxPaint::Color(Color::rgb(1.0, 1.0, 0.7)),
        technique: GlowTechnique::Softer,
        spread: 0.0,
        size: 9.0,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    })];
    // Unaffected layers whose rows also render in parallel keep both workers stealing.
    let base = Layer::new("base", LayerContent::Fill(g(30.0)));
    let mut top = Layer::new("top", LayerContent::Fill(g(60.0)));
    top.opacity = 0.3;
    d.layers.extend([base, shadow, stroke, glow, top]);
    d
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn effect_maps_with_nested_rayon_work_do_not_deadlock() {
    // #276: a tile building a layer's effect maps inside a once-init ran Rayon work; waiting on
    // it, its worker stole another tile of the same layer, which then blocked on the init held
    // further up its own stack. Eight threads (two rarely steal across that wait) and 64 tiles make that stealing likely; the cache is
    // purged every round so the maps are rebuilt. A watchdog turns a hang into a failure.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(8).build() else { return };
        let d = nested_parallel_fx_doc();
        let first = pool.install(|| render_tiled(&d, d.bounds(), 64));
        for _ in 0..100 {
            purge_effect_cache();
            let out = pool.install(|| render_tiled(&d, d.bounds(), 64));
            assert!(out.px == first.px, "renders differ between rounds");
        }
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("effect-map rendering deadlocked (#276)"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("render thread panicked"),
    }
}

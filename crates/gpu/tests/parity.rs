//! GPU vs CPU parity: every blend mode, adjustment, group/clip/mask/fill combination must match
//! the reference compositor within 2/255 (premultiplied). Skips when no GPU adapter exists.

use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::adjust::{CurvePoint, HueRange, LevelsChannel};
use photocraft_doc::{Adjustment, Document, Fill, GradientStyle, Layer, LayerContent, LayerMask};
use photocraft_geom::{Rect, Size};
use photocraft_gpu::{Compositor, render_to_vec};

const TOL: f32 = 1.0 / 255.0;

/// Concurrent wgpu instances in one process segfault on some drivers (RADV), so the GPU tests
/// take this lock and hold it until their device is dropped.
static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    comp: Compositor,
    /// The same device with a simulated texture limit of [`PAGED_LIMIT`]: every check also runs
    /// through layer pages and per-cell effect maps.
    paged: Compositor,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// Simulated texture limit (pages and chunks of 256 px).
const PAGED_LIMIT: u32 = 256;

fn gpu() -> Option<Gpu> {
    let lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let instance = wgpu::Instance::default();
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skipping GPU parity tests: no adapter ({e})");
            return None;
        }
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    // Exact parity needs 32-bit float targets. Adapters without them (e.g. GL software
    // rasterizers) use the CPU compositor in the app, so there is nothing to compare there.
    if Compositor::preferred_acc_format(&adapter) != wgpu::TextureFormat::Rgba32Float {
        eprintln!("skipping GPU parity tests: adapter can't render Rgba32Float");
        return None;
    }
    let comp = match Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping GPU parity tests: {e}");
            return None;
        }
    };
    let mut paged = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).ok()?;
    paged.set_texture_limit(PAGED_LIMIT);
    Some(Gpu { device, queue, comp, paged, _lock: lock })
}

/// Any adapter and device, for the fallback-path tests (which don't need 32-bit float targets).
/// The returned guard holds [`GPU_LOCK`] until the device is dropped.
fn any_device() -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue, std::sync::MutexGuard<'static, ()>)> {
    let lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let adapter = pollster::block_on(wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some((adapter, device, queue, lock))
}

/// Deterministic pseudo-random values in 0..1.
fn rnd(seed: u32, i: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ i.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

fn noise_layer(name: &str, fmt: PixelFormat, rect: Rect, seed: u32, min_alpha: f32) -> Layer {
    let mut l = Layer::raster(name, fmt);
    let n = rect.width() as usize * rect.height() as usize;
    let ch = fmt.channels();
    let mut data = Vec::with_capacity(n * ch);
    for i in 0..n as u32 {
        for c in 0..ch {
            let v = rnd(seed + c as u32 * 7919, i);
            let is_alpha = fmt.alpha && c == ch - 1;
            data.push(if is_alpha { min_alpha + (1.0 - min_alpha) * v } else { v });
        }
    }
    l.surface_mut().unwrap().write_region(rect, &data);
    l
}

fn mask(rect: Rect, seed: u32, default: f32) -> LayerMask {
    let mut m = LayerMask::reveal_all();
    m.surface = photocraft_raster::Surface::with_default(PixelFormat::GRAY8, &[default]);
    let data: Vec<f32> = (0..rect.width() * rect.height()).map(|i| rnd(seed, i)).collect();
    m.surface.write_region(rect, &data);
    m.density = 0.8;
    m
}

fn base_doc(w: u32, h: u32) -> Document {
    let mut d = Document::new("t", Size::new(w, h), ColorMode::Rgb, SampleType::U8);
    d.layers.push(noise_layer("bg", PixelFormat::RGBA8, Rect::from_xywh(0, 0, w, h), 1, 0.3));
    d
}

/// Largest premultiplied difference between `cpu` and `gpu` pixels, and its index.
fn worst_diff(cpu: &[[f32; 4]], gpu: &[[f32; 4]]) -> (f32, usize) {
    let mut worst = (0.0f32, 0usize);
    for (i, (c, o)) in cpu.iter().zip(gpu).enumerate() {
        let pc = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
        let po = [o[0] * o[3], o[1] * o[3], o[2] * o[3], o[3]];
        for k in 0..4 {
            let d = (pc[k] - po[k]).abs();
            if d.is_nan() || d > worst.0 {
                worst = (if d.is_nan() { 9.0 } else { d }, i);
            }
        }
    }
    worst
}

/// GPU (whole textures, then pages) vs CPU over `rect`: Err with the worst pixel if over the
/// tolerance, else the stats of the unpaged render.
fn diff_rect(g: &mut Gpu, doc: &Document, rect: Rect, what: &str) -> Result<photocraft_gpu::Stats, String> {
    let cpu = photocraft_compose::render(doc, rect);
    let mut first = None;
    for (label, comp) in [("", &mut g.comp), (" (paged)", &mut g.paged)] {
        let (out, stats) = photocraft_gpu::render_to_vec_stats(comp, &g.device, &g.queue, doc, rect).map_err(|e| format!("{what}{label}: {e}"))?;
        let worst = worst_diff(&cpu.px, &out);
        let w = rect.width() as usize;
        let (x, y) = (rect.x0 + (worst.1 % w) as i32, rect.y0 + (worst.1 / w) as i32);
        if worst.0 > TOL {
            return Err(format!("{what}{label}: max diff {:.2}/255 at ({x},{y}): cpu {:?} gpu {:?}", worst.0 * 255.0, cpu.px[worst.1], out[worst.1]));
        }
        first.get_or_insert(stats);
    }
    first.ok_or_else(|| format!("{what}: no render"))
}

fn check(g: &mut Gpu, doc: &Document, what: &str) {
    diff_rect(g, doc, doc.bounds(), what).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn first_frame_renders() {
    // Regression: the first submission after device creation is dropped on some drivers
    // (RADV), which used to make the very first render come back all zeroes.
    let Some(mut g) = gpu() else { return };
    check(&mut g, &base_doc(64, 48), "first frame");
}

#[test]
fn blend_modes() {
    let Some(mut g) = gpu() else { return };
    for mode in BlendMode::LAYER_MODES {
        let mut d = base_doc(64, 48);
        let mut l = noise_layer("top", PixelFormat::RGBA8, Rect::new(4, 3, 60, 45), 2, 0.0);
        l.blend = mode;
        l.opacity = 0.8;
        l.fill_opacity = 0.9;
        d.layers.push(l);
        check(&mut g, &d, &format!("{mode:?}"));
        // Over an opaque backdrop too (the common case).
        d.layers[0] = noise_layer("bg", PixelFormat::RGBA8, Rect::from_xywh(0, 0, 64, 48), 5, 1.0);
        check(&mut g, &d, &format!("{mode:?} opaque"));
    }
}

fn hue_ranges() -> [HueRange; 6] {
    let mut r = HueRange::defaults();
    r[0] = HueRange { hue: 20.0, saturation: -50.0, lightness: 10.0, ..HueRange::neutral(0) };
    r[4] = HueRange { hue: -30.0, saturation: 40.0, lightness: -20.0, bounds: [180.0, 220.0, 260.0, 300.0] };
    r
}

fn adjustments() -> Vec<Adjustment> {
    let lc = |a: f32, b: f32, g: f32| LevelsChannel { in_black: a, in_white: b, gamma: g, out_black: 0.05, out_white: 0.95 };
    let pts = |v: &[(f32, f32)]| v.iter().map(|&(input, output)| CurvePoint { input, output }).collect::<Vec<_>>();
    vec![
        Adjustment::Invert,
        Adjustment::Threshold { level: 0.5 },
        Adjustment::Posterize { levels: 5 },
        Adjustment::BrightnessContrast { brightness: 30.0, contrast: 40.0, legacy: false },
        Adjustment::BrightnessContrast { brightness: -20.0, contrast: -30.0, legacy: true },
        Adjustment::Exposure { exposure: 0.7, offset: 0.02, gamma: 1.2 },
        Adjustment::Levels {
            master: lc(0.1, 0.9, 1.3),
            per_channel: [lc(0.0, 1.0, 0.8), LevelsChannel::default(), lc(0.2, 0.8, 1.0)],
            space: Default::default(),
            black: LevelsChannel::default(),
        },
        Adjustment::Curves {
            master: pts(&[(0.0, 0.1), (0.4, 0.6), (1.0, 0.9)]),
            per_channel: [pts(&[(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)]), pts(&[(0.0, 0.0), (1.0, 1.0)]), pts(&[(0.0, 0.2), (1.0, 1.0)])],
            space: Default::default(),
            black: Vec::new(),
        },
        Adjustment::HueSaturation { hue: 40.0, saturation: 30.0, lightness: -10.0, colorize: false, ranges: HueRange::defaults() },
        Adjustment::HueSaturation { hue: 200.0, saturation: 50.0, lightness: 20.0, colorize: true, ranges: HueRange::defaults() },
        Adjustment::HueSaturation { hue: -10.0, saturation: 10.0, lightness: 5.0, colorize: false, ranges: hue_ranges() },
        Adjustment::Vibrance { vibrance: 50.0, saturation: -20.0 },
        Adjustment::ChannelMixer { matrix: [[0.5, 0.3, 0.2, 0.0], [0.1, 0.8, 0.1, 0.05], [0.0, 0.2, 0.9, -0.05]], monochrome: false },
        Adjustment::ChannelMixer { matrix: [[0.4, 0.4, 0.2, 0.0], [0.0; 4], [0.0; 4]], monochrome: true },
        Adjustment::PhotoFilter { color: [0.9, 0.6, 0.2], density: 0.4, preserve_luminosity: true },
        Adjustment::PhotoFilter { color: [0.2, 0.6, 0.9], density: 0.3, preserve_luminosity: false },
        Adjustment::BlackWhite { weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0], tint: None },
        Adjustment::BlackWhite { weights: [70.0, 20.0, 50.0, 10.0, 90.0, 30.0], tint: Some([0.9, 0.7, 0.5]) },
        Adjustment::GradientMap { stops: vec![(0.0, [0.1, 0.0, 0.3]), (0.5, [0.9, 0.3, 0.1]), (1.0, [1.0, 1.0, 0.8])], reverse: false, dither: false },
        Adjustment::GradientMap { stops: vec![(0.0, [0.0, 0.0, 0.0]), (1.0, [1.0, 1.0, 1.0])], reverse: true, dither: true },
        Adjustment::ColorBalance { shadows: [20.0, -10.0, 5.0], midtones: [-15.0, 10.0, 30.0], highlights: [0.0, 5.0, -20.0], preserve_luminosity: true },
        Adjustment::ColorBalance { shadows: [10.0, 0.0, 0.0], midtones: [0.0, 0.0, 0.0], highlights: [0.0, 0.0, 10.0], preserve_luminosity: false },
        Adjustment::SelectiveColor { relative: true, adjustments: selective() },
        Adjustment::SelectiveColor { relative: false, adjustments: selective() },
        lookup(17, false, false),
        lookup(5, true, false),
        lookup(33, false, true),
    ]
}

fn selective() -> [[f32; 4]; 9] {
    std::array::from_fn(|r| std::array::from_fn(|k| ((r * 4 + k) as f32 * 37.0) % 200.0 - 100.0))
}

/// A warped (non-identity) 3D LUT so interpolation differences show.
fn lookup(n: usize, tetrahedral: bool, dither: bool) -> Adjustment {
    let mut t = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (r, g, b) = (r as f32 / (n - 1) as f32, g as f32 / (n - 1) as f32, b as f32 / (n - 1) as f32);
                t.extend([(r * r * 0.8 + b * 0.2).min(1.0), g.sqrt(), (1.0 - r) * 0.3 + b * 0.7]);
            }
        }
    }
    Adjustment::ColorLookup { name: "test".into(), lut: Some(std::sync::Arc::new(t)), size: n as u32, tetrahedral, dither }
}

#[test]
fn adjustment_layers() {
    let Some(mut g) = gpu() else { return };
    // 16-bit and float documents (adjustment results round to 1/32768 resp. not at all).
    for depth in [SampleType::U16, SampleType::F32] {
        let mut d = Document::new("t", Size::new(40, 30), ColorMode::Rgb, depth);
        let fmt = d.pixel_format();
        d.layers.push(noise_layer("bg", fmt, Rect::from_xywh(0, 0, 40, 30), 71, 0.5));
        d.layers.push(Layer::new("ex", LayerContent::Adjustment(Adjustment::Exposure { exposure: 0.4, offset: 0.01, gamma: 1.1 })));
        check(&mut g, &d, &format!("adjustment {depth:?}"));
        // Levels on whole levels of the document's depth (compose::adjust::levels_q).
        let lc = LevelsChannel { in_black: 0.17, in_white: 0.84, gamma: 1.78, out_black: 0.0, out_white: 1.0 };
        let lv = Adjustment::Levels { master: lc, per_channel: Default::default(), space: Default::default(), black: LevelsChannel::default() };
        d.layers.push(Layer::new("lv", LayerContent::Adjustment(lv)));
        check(&mut g, &d, &format!("levels {depth:?}"));
    }
    for adj in adjustments() {
        for (mode, opacity, masked) in [(BlendMode::Normal, 1.0, false), (BlendMode::Multiply, 0.7, true)] {
            let mut d = base_doc(48, 40);
            d.layers.push(noise_layer("mid", PixelFormat::RGBA8, Rect::new(8, 8, 40, 32), 3, 0.5));
            let mut a = Layer::new("adj", LayerContent::Adjustment(adj.clone()));
            a.blend = mode;
            a.opacity = opacity;
            if masked {
                a.mask = Some(mask(Rect::new(0, 0, 48, 20), 9, 0.3));
            }
            d.layers.push(a);
            check(&mut g, &d, &format!("{} {mode:?}", adj.label()));
        }
    }
}

#[test]
fn groups_masks_and_clipping() {
    let Some(mut g) = gpu() else { return };
    let child = |seed, mode| {
        let mut l = noise_layer("c", PixelFormat::RGBA8, Rect::new(5, 5, 50, 40), seed, 0.0);
        l.blend = mode;
        l
    };
    for isolated in [false, true] {
        let mut d = base_doc(56, 44);
        let mut grp = Layer::group("g", vec![child(11, BlendMode::Normal), child(12, BlendMode::Screen), child(13, BlendMode::Multiply)]);
        if isolated {
            grp.blend = BlendMode::Overlay;
        }
        grp.opacity = 0.7;
        grp.mask = Some(mask(Rect::new(10, 0, 40, 44), 21, 1.0));
        d.layers.push(grp);
        check(&mut g, &d, &format!("group isolated={isolated}"));
    }
    // Nested groups with an adjustment inside a pass-through group.
    let mut d = base_doc(56, 44);
    let inner = Layer::group("inner", vec![child(14, BlendMode::Normal), Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert))]);
    let mut outer = Layer::group("outer", vec![inner, child(15, BlendMode::Difference)]);
    outer.blend = BlendMode::Normal;
    d.layers.push(outer);
    check(&mut g, &d, "nested groups");

    // Clipping: raster + adjustment clipped to a base, a masked base, and a hidden base.
    for hidden in [false, true] {
        let mut d = base_doc(56, 44);
        let mut base = noise_layer("base", PixelFormat::RGBA8, Rect::new(10, 6, 46, 38), 16, 0.0);
        base.mask = Some(mask(Rect::new(0, 0, 56, 44), 22, 1.0));
        base.visible = !hidden;
        let mut c1 = child(17, BlendMode::Multiply);
        c1.clipped = true;
        c1.opacity = 0.6;
        let mut c2 = Layer::new(
            "hs",
            LayerContent::Adjustment(Adjustment::HueSaturation { hue: 90.0, saturation: 20.0, lightness: 0.0, colorize: false, ranges: HueRange::defaults() }),
        );
        c2.clipped = true;
        d.layers.extend([base, c1, c2]);
        check(&mut g, &d, &format!("clipping hidden={hidden}"));
    }
    // Clipped group base (isolated) with a clipped layer.
    let mut d = base_doc(56, 44);
    let mut gb = Layer::group("gb", vec![child(18, BlendMode::Normal)]);
    gb.blend = BlendMode::Normal;
    let mut c = child(19, BlendMode::Screen);
    c.clipped = true;
    d.layers.extend([gb, c]);
    check(&mut g, &d, "clipped to isolated group");
    // Layers clipped to a pass-through group (with opacity and a mask, children in modes).
    for (op, masked) in [(1.0, false), (0.6, true)] {
        let mut d = base_doc(56, 44);
        let mut pt = Layer::group("pt", vec![child(31, BlendMode::Normal), child(32, BlendMode::Multiply)]);
        pt.opacity = op;
        if masked {
            pt.mask = Some(mask(Rect::new(5, 0, 50, 44), 33, 1.0));
        }
        let mut c1 = child(34, BlendMode::Screen);
        c1.clipped = true;
        c1.opacity = 0.8;
        let mut c2 = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
        c2.clipped = true;
        d.layers.extend([pt, c1, c2]);
        check(&mut g, &d, &format!("clipped to pass-through op {op} masked {masked}"));
    }
}

#[test]
fn fills_and_dissolve() {
    let Some(mut g) = gpu() else { return };
    let mut d = base_doc(64, 48);
    let mut solid = Layer::new("solid", LayerContent::Fill(Fill::Solid(Color::rgb(0.2, 0.5, 0.8))));
    solid.mask = Some(mask(Rect::new(0, 0, 32, 48), 31, 0.0));
    solid.blend = BlendMode::HardLight;
    d.layers.push(solid);
    check(&mut g, &d, "solid fill");
    for style in [GradientStyle::Linear, GradientStyle::Radial, GradientStyle::Angle, GradientStyle::Reflected, GradientStyle::Diamond] {
        let mut d = base_doc(64, 48);
        let stops = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.6, Color::rgb(0.0, 1.0, 0.2)), (1.0, Color::rgb(0.1, 0.1, 0.9))];
        let mut l = Layer::new("grad", LayerContent::Fill(Fill::gradient(stops, 30.0, 0.8, style, style == GradientStyle::Radial)));
        l.opacity = 0.9;
        d.layers.push(l);
        check(&mut g, &d, &format!("gradient {style:?}"));
        // A live gradient (Gradient tool): canvas-aligned, offset, midpoints, opacity stops,
        // dither, masked by a selection.
        let mut d = base_doc(64, 48);
        let stops = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.6, Color::rgb(0.0, 1.0, 0.2)), (1.0, Color::rgb(0.1, 0.1, 0.9))];
        let fill = Fill::Gradient {
            stops,
            angle: -20.0,
            scale: 0.6,
            style,
            reverse: false,
            opacity_stops: vec![(0.0, 1.0), (1.0, 0.3)],
            midpoints: vec![0.3, 0.7],
            offset: (0.15, -0.1),
            dither: true,
            align: false,
        };
        let mut l = Layer::new("live", LayerContent::Fill(fill));
        l.mask = Some(mask(Rect::new(8, 4, 40, 40), 13, 0.0));
        d.layers.push(l);
        check(&mut g, &d, &format!("live gradient {style:?}"));
    }
    // Tiny frames, where the whole-pixel end points (compose::fill_layout) change the angle
    // and centre.
    for (w, h, style, angle) in [(4, 4, GradientStyle::Reflected, 30.0), (7, 5, GradientStyle::Linear, 30.0), (9, 4, GradientStyle::Linear, -60.0)] {
        let mut d = base_doc(w, h);
        let stops = vec![(0.0, Color::rgb(0.0, 0.0, 0.7)), (0.5, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(1.0, 1.0, 0.0))];
        d.layers.push(Layer::new("grad", LayerContent::Fill(Fill::gradient(stops, angle, 1.0, style, false))));
        check(&mut g, &d, &format!("small gradient {w}x{h} {style:?} {angle}"));
    }
    // An opaque, dithered gradient over everything (the GPU skips the layers it hides), with a
    // clipped layer and an adjustment above it.
    let mut d = base_doc(64, 48);
    d.layers.push(noise_layer("under", PixelFormat::RGBA8, Rect::new(5, 5, 50, 40), 17, 0.4));
    let stops = vec![(0.0, Color::rgb(0.9, 0.2, 0.1)), (1.0, Color::rgb(0.1, 0.3, 0.9))];
    let mut cover = Fill::gradient(stops, 70.0, 0.7, GradientStyle::Linear, false);
    if let Fill::Gradient { dither, offset, .. } = &mut cover {
        *dither = true;
        *offset = (0.1, -0.05);
    }
    d.layers.push(Layer::new("cover", LayerContent::Fill(cover)));
    let mut clip = noise_layer("clip", PixelFormat::RGBA8, Rect::new(10, 10, 30, 30), 23, 0.2);
    clip.clipped = true;
    d.layers.push(clip);
    d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
    check(&mut g, &d, "opaque gradient over everything");
    let mut d = base_doc(64, 48);
    let mut l = noise_layer("dis", PixelFormat::RGBA8, Rect::new(0, 0, 64, 48), 41, 0.2);
    l.blend = BlendMode::Dissolve;
    l.opacity = 0.6;
    d.layers.push(l);
    check(&mut g, &d, "dissolve");
}

#[test]
fn formats_offsets_and_chunks() {
    let Some(mut g) = gpu() else { return };
    // 16-bit and float layers, and a layer extending off-canvas at negative coordinates.
    let mut d = Document::new("t", Size::new(300, 70), ColorMode::Rgb, SampleType::U16);
    d.layers.push(noise_layer("bg16", PixelFormat::RGBA16, Rect::new(0, 0, 300, 70), 51, 1.0));
    let mut f = noise_layer("f32", PixelFormat::RGBA32F, Rect::new(-40, -10, 200, 60), 52, 0.0);
    f.blend = BlendMode::SoftLight;
    d.layers.push(f);
    check(&mut g, &d, "16-bit / float / offsets");

    // Grayscale and CMYK documents.
    let mut d = Document::new("g", Size::new(40, 30), ColorMode::Grayscale, SampleType::U8);
    d.layers.push(noise_layer("g", PixelFormat::GRAYA8, Rect::new(0, 0, 40, 30), 53, 0.5));
    check(&mut g, &d, "grayscale");
    let mut d = Document::new("c", Size::new(40, 30), ColorMode::Cmyk, SampleType::U8);
    d.layers.push(noise_layer("c", PixelFormat::CMYKA8, Rect::new(0, 0, 40, 30), 54, 1.0));
    check(&mut g, &d, "cmyk");

    // Wider than one chunk.
    let w = photocraft_gpu::CHUNK + 100;
    let mut d = base_doc(w, 24);
    let mut l = noise_layer("top", PixelFormat::RGBA8, Rect::new(0, 0, w as i32, 24), 55, 0.0);
    l.blend = BlendMode::Color;
    d.layers.push(l);
    check(&mut g, &d, "multi-chunk");
}

#[test]
fn incremental_updates_follow_the_document() {
    let Some(mut g) = gpu() else { return };
    let mut d = base_doc(600, 300);
    d.layers.push(noise_layer("top", PixelFormat::RGBA8, Rect::new(0, 0, 600, 300), 61, 0.0));
    check(&mut g, &d, "initial");
    // Paint into one tile of the top layer: only that tile uploads.
    let top = d.layers[1].surface_mut().unwrap();
    top.fill_rect(Rect::new(10, 10, 60, 60), &[1.0, 0.0, 0.0, 1.0]);
    let before = photocraft_gpu::render_to_vec(&mut g.comp, &g.device, &g.queue, &d, Rect::new(0, 0, 1, 1)).unwrap();
    assert_eq!(before.len(), 1);
    check(&mut g, &d, "after paint");
    // Remove all tiles of the top layer, hide nothing: the GPU must clear them.
    d.layers[1] = Layer::raster("empty", PixelFormat::RGBA8);
    check(&mut g, &d, "after clearing");
    // Visibility and opacity changes need no uploads.
    d.layers[0].opacity = 0.5;
    check(&mut g, &d, "opacity");
}

// ---- layer effects --------------------------------------------------------------------------

use photocraft_doc::adjust::CurvePoint as Cp;
use photocraft_doc::{
    Bevel, BevelStyle, BevelTechnique, Contour, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Gradient, Pattern, Satin, Shadow, StrokeFx,
    StrokePosition,
};

/// An anti-aliased blob (disc plus a soft-edged bar and a hole) with partially transparent parts,
/// so effects see real edge coverage, concavities and interior alpha.
/// Small layers composite only over their bounds and are copied back into the backdrop (#125):
/// every blend mode, clipped layers, isolated and pass-through groups (with adjustments, masks,
/// opacity), layers partly off the canvas and over several chunks must still match the CPU.
#[test]
fn small_layers_composite_over_their_bounds() {
    let Some(mut g) = gpu() else { return };
    let small = |seed: u32, x: i32, y: i32, mode: BlendMode| {
        let mut l = noise_layer("s", PixelFormat::RGBA8, Rect::from_xywh(x, y, 23, 17), seed, 0.0);
        l.blend = mode;
        l.opacity = 0.85;
        l
    };
    let mut d = base_doc(200, 150);
    for (i, mode) in BlendMode::LAYER_MODES.into_iter().enumerate() {
        let i = i as i32;
        d.layers.push(small(100 + i as u32, (i * 29) % 190 - 8, (i * 37) % 140 - 5, mode));
    }
    // A clipping base with a raster and an adjustment clipped to it.
    let base = small(200, 120, 90, BlendMode::Normal);
    let mut c1 = noise_layer("c1", PixelFormat::RGBA8, Rect::new(100, 80, 190, 140), 201, 0.0);
    c1.clipped = true;
    c1.blend = BlendMode::Multiply;
    let mut c2 = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    c2.clipped = true;
    d.layers.extend([base, c1, c2]);
    // An isolated group with a mask, an adjustment and a nested pass-through group.
    let inner = Layer::group("inner", vec![small(300, 40, 100, BlendMode::Screen), small(301, 55, 110, BlendMode::Normal)]);
    let mut iso = Layer::group(
        "iso",
        vec![
            small(302, 30, 95, BlendMode::Normal),
            Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)),
            inner,
            small(303, 70, 120, BlendMode::Overlay),
        ],
    );
    iso.blend = BlendMode::Normal;
    iso.opacity = 0.75;
    iso.mask = Some(mask(Rect::new(25, 90, 100, 145), 304, 1.0));
    d.layers.push(iso);
    // A pass-through group with opacity around small layers.
    let mut pt = Layer::group("pt", vec![small(305, 150, 10, BlendMode::Normal), small(306, 160, 20, BlendMode::Difference)]);
    pt.opacity = 0.6;
    d.layers.push(pt);
    check(&mut g, &d, "small layers");
    // The same over several compositor chunks.
    let mut big = base_doc(1500, 1200);
    for (i, mode) in BlendMode::LAYER_MODES.into_iter().enumerate() {
        let i = i as i32;
        big.layers.push(small(400 + i as u32, 1010 + (i % 4) * 5, 1010 + (i / 4) * 5, mode));
    }
    check(&mut g, &big, "small layers across chunks");
}

fn blob(name: &str, fmt: PixelFormat, cx: f32, cy: f32, r: f32, color: [f32; 3]) -> Layer {
    let mut l = Layer::raster(name, fmt);
    let rect = Rect::new((cx - r - 12.0) as i32, (cy - r - 4.0) as i32, (cx + r + 14.0) as i32, (cy + r + 4.0) as i32);
    let ch = fmt.channels();
    let mut data = Vec::with_capacity(rect.width() as usize * rect.height() as usize * ch);
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let (fx, fy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (fx * fx + fy * fy).sqrt();
            let disc = (r - d + 0.5).clamp(0.0, 1.0);
            let hole = ((d - r * 0.3) + 0.5).clamp(0.0, 1.0);
            let bar = ((r * 0.25 - fy.abs()) + 0.5).clamp(0.0, 1.0) * ((r + 10.0 - fx.abs()) * 0.7).clamp(0.0, 1.0);
            let a = (disc * hole).max(bar * 0.7);
            let rgba = [color[0] * (0.7 + 0.3 * (fx / r).abs()), color[1], color[2] * (0.8 + 0.2 * (fy / r)), a];
            let px = photocraft_raster::from_rgba(&fmt, rgba);
            data.extend_from_slice(&px);
        }
    }
    l.surface_mut().unwrap().write_region(rect, &data);
    l
}

fn fx_doc(w: u32, h: u32, depth: SampleType) -> Document {
    let mut d = Document::new("fx", Size::new(w, h), ColorMode::Rgb, depth);
    let fmt = d.pixel_format();
    d.layers.push(noise_layer("bg", fmt, Rect::from_xywh(0, 0, w, h), 7, 0.8));
    d
}

fn contour() -> Contour {
    Contour::Custom { name: "cove".into(), points: vec![Cp { input: 0.0, output: 0.1 }, Cp { input: 0.4, output: 0.8 }, Cp { input: 1.0, output: 0.6 }] }
}

fn shadow(blend: BlendMode, opacity: f32, angle: f32, distance: f32, size: f32, spread: f32) -> Shadow {
    Shadow {
        common: FxCommon::new(blend, opacity),
        color: Color::rgb(0.1, 0.05, 0.3),
        angle,
        use_global_light: false,
        distance,
        spread,
        size,
        contour: Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: true,
    }
}

fn glow(paint: FxPaint, technique: GlowTechnique, size: f32, spread: f32, source: GlowSource) -> Glow {
    Glow {
        common: FxCommon::new(BlendMode::Screen, 0.8),
        paint,
        technique,
        spread,
        size,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source,
    }
}

fn gradient() -> Gradient {
    Gradient {
        stops: vec![(0.0, Color::rgb(1.0, 0.2, 0.0)), (0.5, Color::rgb(0.1, 0.9, 0.3)), (1.0, Color::rgb(0.2, 0.1, 1.0))],
        opacity_stops: vec![(0.0, 1.0), (1.0, 0.4)],
        style: GradientStyle::Linear,
        angle: 30.0,
        scale: 0.9,
        reverse: false,
        align: true,
        offset: (0.1, -0.05),
    }
}

fn bevel(style: BevelStyle, up: bool, size: f32, soften: f32) -> Bevel {
    Bevel {
        enabled: true,
        style,
        technique: BevelTechnique::Smooth,
        depth: 1.3,
        up,
        size,
        soften,
        angle: 135.0,
        altitude: 35.0,
        use_global_light: true,
        gloss_contour: Contour::Linear,
        highlight: FxCommon::new(BlendMode::Screen, 0.8),
        highlight_color: Color::rgb(1.0, 1.0, 0.9),
        shadow: FxCommon::new(BlendMode::Multiply, 0.7),
        shadow_color: Color::rgb(0.1, 0.0, 0.2),
        contour: None,
        texture: None,
    }
}

fn stroke(size: f32, position: StrokePosition, paint: FxPaint) -> StrokeFx {
    StrokeFx { common: FxCommon::new(BlendMode::Normal, 0.9), size, position, paint }
}

fn checker_pattern() -> Pattern {
    let mut s = photocraft_raster::Surface::new(PixelFormat::RGBA8);
    s.fill_rect(Rect::new(0, 0, 6, 5), &[0.9, 0.2, 0.1, 1.0]);
    s.fill_rect(Rect::new(0, 0, 3, 3), &[0.1, 0.3, 0.9, 0.6]);
    s.fill_rect(Rect::new(3, 3, 6, 5), &[0.2, 0.8, 0.3, 1.0]);
    Pattern::new("checker", s, 6, 5)
}

/// Every effect kind with several option combinations, one effect stack per case.
fn effect_cases() -> Vec<(&'static str, Vec<Effect>)> {
    let pat = checker_pattern();
    let pat_paint = FxPaint::Pattern { name: pat.name.clone(), id: pat.id.clone(), scale: 1.0 };
    let mut ds_contour = shadow(BlendMode::Normal, 0.8, 200.0, 4.0, 9.0, 0.35);
    ds_contour.contour = contour();
    ds_contour.knocks_out = false;
    let mut ds_global = shadow(BlendMode::Multiply, 0.75, 0.0, 6.0, 7.0, 0.0);
    ds_global.use_global_light = true;
    let mut og_contour = glow(FxPaint::Color(Color::rgb(1.0, 0.9, 0.2)), GlowTechnique::Softer, 9.0, 0.0, GlowSource::Edge);
    og_contour.contour = contour();
    let mut satin_inv = Satin {
        common: FxCommon::new(BlendMode::Multiply, 0.6),
        color: Color::rgb(0.3, 0.0, 0.4),
        angle: 19.0,
        distance: 7.0,
        size: 8.0,
        contour: contour(),
        anti_alias: false,
        invert: true,
    };
    let satin = Satin {
        common: FxCommon::new(BlendMode::Overlay, 0.7),
        color: Color::rgb(0.9, 0.4, 0.1),
        angle: 60.0,
        distance: 5.0,
        size: 6.0,
        contour: Contour::Linear,
        anti_alias: false,
        invert: false,
    };
    satin_inv.common.enabled = true;
    let mut bevel_contour = bevel(BevelStyle::InnerBevel, true, 8.0, 3.0);
    bevel_contour.gloss_contour = contour();
    bevel_contour.use_global_light = false;
    vec![
        ("drop shadow", vec![Effect::DropShadow(shadow(BlendMode::Multiply, 0.75, 120.0, 6.0, 8.0, 0.0))]),
        ("drop shadow spread contour", vec![Effect::DropShadow(ds_contour)]),
        ("drop shadow hard", vec![Effect::DropShadow(shadow(BlendMode::Normal, 1.0, 45.0, 3.0, 0.0, 0.0))]),
        ("drop shadow global light", vec![Effect::DropShadow(ds_global)]),
        ("inner shadow", vec![Effect::InnerShadow(shadow(BlendMode::Multiply, 0.8, 120.0, 4.0, 6.0, 0.0))]),
        ("inner shadow choke", vec![Effect::InnerShadow(shadow(BlendMode::Normal, 0.9, 300.0, 3.0, 7.0, 0.4))]),
        ("outer glow softer", vec![Effect::OuterGlow(glow(FxPaint::Color(Color::rgb(1.0, 0.9, 0.2)), GlowTechnique::Softer, 10.0, 0.3, GlowSource::Edge))]),
        ("outer glow contour", vec![Effect::OuterGlow(og_contour)]),
        ("outer glow precise gradient", vec![Effect::OuterGlow(glow(FxPaint::Gradient(gradient()), GlowTechnique::Precise, 8.0, 0.25, GlowSource::Edge))]),
        ("inner glow softer edge", vec![Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(0.9, 1.0, 0.8)), GlowTechnique::Softer, 7.0, 0.2, GlowSource::Edge))]),
        (
            "inner glow softer center",
            vec![Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(0.9, 1.0, 0.8)), GlowTechnique::Softer, 6.0, 0.0, GlowSource::Center))],
        ),
        (
            "inner glow precise edge",
            vec![Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(0.2, 1.0, 0.8)), GlowTechnique::Precise, 6.0, 0.3, GlowSource::Edge))],
        ),
        ("inner glow precise center", vec![Effect::InnerGlow(glow(pat_paint.clone(), GlowTechnique::Precise, 5.0, 0.0, GlowSource::Center))]),
        ("bevel inner", vec![Effect::BevelEmboss(bevel(BevelStyle::InnerBevel, true, 7.0, 2.0))]),
        ("bevel outer", vec![Effect::BevelEmboss(bevel(BevelStyle::OuterBevel, true, 6.0, 0.0))]),
        ("bevel emboss down", vec![Effect::BevelEmboss(bevel(BevelStyle::Emboss, false, 8.0, 1.0))]),
        ("bevel pillow", vec![Effect::BevelEmboss(bevel(BevelStyle::PillowEmboss, true, 5.0, 4.0))]),
        (
            "bevel inner chisel hard",
            vec![Effect::BevelEmboss(Bevel { technique: BevelTechnique::ChiselHard, ..bevel(BevelStyle::InnerBevel, true, 9.0, 0.0) })],
        ),
        (
            "bevel outer chisel soft",
            vec![Effect::BevelEmboss(Bevel { technique: BevelTechnique::ChiselSoft, ..bevel(BevelStyle::OuterBevel, false, 7.5, 2.0) })],
        ),
        (
            "bevel pillow chisel hard",
            vec![Effect::BevelEmboss(Bevel { technique: BevelTechnique::ChiselHard, ..bevel(BevelStyle::PillowEmboss, false, 7.0, 0.0) })],
        ),
        ("bevel emboss smooth wide", vec![Effect::BevelEmboss(bevel(BevelStyle::Emboss, true, 21.0, 0.0))]),
        ("bevel stroke emboss", vec![Effect::BevelEmboss(bevel(BevelStyle::StrokeEmboss, true, 6.0, 0.0))]),
        (
            "bevel texture",
            vec![Effect::BevelEmboss(Bevel {
                texture: Some(photocraft_doc::BevelTexture {
                    name: "checker".into(),
                    id: String::new(),
                    scale: 1.4,
                    depth: -1.5,
                    invert: true,
                    link: true,
                    phase: (2.0, 1.0),
                }),
                ..bevel(BevelStyle::InnerBevel, true, 7.0, 1.0)
            })],
        ),
        (
            "bevel contour",
            vec![Effect::BevelEmboss(Bevel {
                contour: Some(photocraft_doc::BevelContour { contour: contour(), range: 0.6, anti_alias: false }),
                ..bevel(BevelStyle::Emboss, true, 9.0, 1.0)
            })],
        ),
        ("bevel contour own light", vec![Effect::BevelEmboss(bevel_contour)]),
        ("satin", vec![Effect::Satin(satin)]),
        ("satin inverted contour", vec![Effect::Satin(satin_inv)]),
        ("stroke outside colour", vec![Effect::Stroke(stroke(4.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(0.95, 0.85, 0.1))))]),
        ("stroke inside gradient", vec![Effect::Stroke(stroke(3.0, StrokePosition::Inside, FxPaint::Gradient(gradient())))]),
        ("stroke centre pattern", vec![Effect::Stroke(stroke(5.0, StrokePosition::Center, pat_paint.clone()))]),
        ("colour overlay", vec![Effect::ColorOverlay { common: FxCommon::new(BlendMode::Multiply, 0.7), color: Color::rgb(0.2, 0.7, 0.9) }]),
        (
            "gradient overlay",
            vec![Effect::GradientOverlay {
                common: FxCommon::new(BlendMode::Normal, 0.8),
                gradient: Gradient { style: GradientStyle::Radial, ..gradient() },
                dither: false,
            }],
        ),
        (
            "pattern overlay",
            vec![Effect::PatternOverlay {
                common: FxCommon::new(BlendMode::Normal, 0.9),
                name: pat.name.clone(),
                id: pat.id.clone(),
                scale: 1.0,
                angle: 0.0,
                link: true,
                phase: (2.0, 1.0),
            }],
        ),
        (
            "pattern overlay scaled rotated",
            vec![Effect::PatternOverlay {
                common: FxCommon::new(BlendMode::Screen, 0.8),
                name: pat.name.clone(),
                id: pat.id.clone(),
                scale: 1.7,
                angle: 30.0,
                link: false,
                phase: (0.0, 0.0),
            }],
        ),
        (
            "missing pattern",
            vec![
                Effect::PatternOverlay {
                    common: FxCommon::new(BlendMode::Normal, 1.0),
                    name: "nope".into(),
                    id: "nope".into(),
                    scale: 1.0,
                    angle: 0.0,
                    link: true,
                    phase: (0.0, 0.0),
                },
                Effect::Stroke(stroke(3.0, StrokePosition::Outside, FxPaint::Pattern { name: "nope".into(), id: "nope".into(), scale: 1.0 })),
                Effect::Stroke(stroke(6.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(0.0, 0.0, 0.0)))),
            ],
        ),
        (
            "multiple instances",
            vec![
                Effect::Stroke(stroke(2.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(1.0, 1.0, 1.0)))),
                Effect::Stroke(stroke(5.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(0.1, 0.1, 0.1)))),
                Effect::DropShadow(shadow(BlendMode::Multiply, 0.6, 90.0, 4.0, 5.0, 0.0)),
                Effect::DropShadow(shadow(BlendMode::Normal, 0.5, 270.0, 8.0, 3.0, 0.0)),
                Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 0.3), color: Color::rgb(1.0, 0.0, 0.0) },
            ],
        ),
        (
            "full stack",
            vec![
                Effect::DropShadow(shadow(BlendMode::Multiply, 0.75, 120.0, 5.0, 6.0, 0.1)),
                Effect::InnerShadow(shadow(BlendMode::Multiply, 0.5, 120.0, 3.0, 4.0, 0.0)),
                Effect::OuterGlow(glow(FxPaint::Color(Color::rgb(1.0, 1.0, 0.6)), GlowTechnique::Softer, 6.0, 0.0, GlowSource::Edge)),
                Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(1.0, 1.0, 0.6)), GlowTechnique::Softer, 4.0, 0.0, GlowSource::Edge)),
                Effect::BevelEmboss(bevel(BevelStyle::InnerBevel, true, 5.0, 1.0)),
                Effect::Satin(Satin {
                    common: FxCommon::new(BlendMode::Multiply, 0.4),
                    color: Color::rgb(0.0, 0.0, 0.0),
                    angle: 19.0,
                    distance: 4.0,
                    size: 5.0,
                    contour: Contour::Linear,
                    anti_alias: false,
                    invert: false,
                }),
                Effect::ColorOverlay { common: FxCommon::new(BlendMode::SoftLight, 0.5), color: Color::rgb(0.9, 0.3, 0.2) },
                Effect::GradientOverlay { common: FxCommon::new(BlendMode::Overlay, 0.4), gradient: gradient(), dither: false },
                Effect::Stroke(stroke(3.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(0.0, 0.0, 0.0)))),
            ],
        ),
    ]
}

/// GPU vs CPU over the whole document: Err with the worst pixel if over the tolerance.
fn fx_diff(g: &mut Gpu, doc: &Document, what: &str) -> Result<photocraft_gpu::Stats, String> {
    diff_rect(g, doc, doc.bounds(), what)
}

fn fx_check(g: &mut Gpu, doc: &Document, what: &str) -> photocraft_gpu::Stats {
    fx_diff(g, doc, what).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn layer_effects_every_kind() {
    let Some(mut g) = gpu() else { return };
    let mut failures = Vec::new();
    for depth in [SampleType::U8, SampleType::U16] {
        for (name, fx) in effect_cases() {
            let mut d = fx_doc(96, 80, depth);
            d.patterns.push(checker_pattern());
            let mut l = blob("fx", d.pixel_format(), 46.0, 40.0, 22.0, [0.9, 0.4, 0.2]);
            l.effects.items = fx;
            d.layers.push(l);
            if let Err(e) = fx_diff(&mut g, &d, &format!("{name} {depth:?}")) {
                failures.push(e);
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn layer_effects_opacity_fill_blend_and_off_canvas() {
    let Some(mut g) = gpu() else { return };
    let stack = effect_cases().into_iter().find(|(n, _)| *n == "full stack").unwrap().1;
    for (opacity, fill, blend) in [
        (0.7, 1.0, BlendMode::Normal),
        (1.0, 0.0, BlendMode::Normal),
        (0.6, 0.35, BlendMode::Multiply),
        (1.0, 0.5, BlendMode::Screen),
        (0.8, 1.0, BlendMode::Dissolve),
    ] {
        let mut d = fx_doc(96, 80, SampleType::U8);
        let mut l = blob("fx", d.pixel_format(), 46.0, 40.0, 22.0, [0.2, 0.6, 0.9]);
        l.effects.items = stack.clone();
        l.opacity = opacity;
        l.fill_opacity = fill;
        l.blend = blend;
        d.layers.push(l);
        fx_check(&mut g, &d, &format!("opacity {opacity} fill {fill} {blend:?}"));
    }
    // Shapes partly off the canvas (their effects reach back in) and a masked effect layer.
    let mut d = fx_doc(90, 70, SampleType::U8);
    let mut l = blob("edge", d.pixel_format(), 4.0, 66.0, 18.0, [0.9, 0.9, 0.2]);
    l.effects.items = stack.clone();
    l.mask = Some(mask(Rect::new(0, 30, 50, 70), 77, 1.0));
    d.layers.push(l);
    fx_check(&mut g, &d, "off canvas + mask");
    // Disabled master switch / disabled items are ignored.
    let mut d = fx_doc(64, 64, SampleType::U8);
    let mut l = blob("off", d.pixel_format(), 30.0, 30.0, 14.0, [0.9, 0.2, 0.2]);
    l.effects.items = stack;
    if let Effect::DropShadow(s) = &mut l.effects.items[0] {
        s.common.enabled = false;
    }
    d.layers.push(l.clone());
    fx_check(&mut g, &d, "disabled item");
    d.layers[1].effects.enabled = false;
    fx_check(&mut g, &d, "master switch off");
}

#[test]
fn layer_effects_on_groups_clipping_and_fills() {
    let Some(mut g) = gpu() else { return };
    let ds = Effect::DropShadow(shadow(BlendMode::Multiply, 0.8, 120.0, 5.0, 6.0, 0.0));
    let st = Effect::Stroke(stroke(3.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(1.0, 1.0, 1.0))));
    let bv = Effect::BevelEmboss(bevel(BevelStyle::InnerBevel, true, 5.0, 1.0));
    // Effects on an isolated group and on a pass-through group (rendered isolated).
    for blend in [BlendMode::Normal, BlendMode::PassThrough] {
        let mut d = fx_doc(100, 80, SampleType::U8);
        let a = blob("a", d.pixel_format(), 35.0, 35.0, 16.0, [0.9, 0.3, 0.2]);
        let mut b = blob("b", d.pixel_format(), 62.0, 45.0, 14.0, [0.2, 0.3, 0.9]);
        b.blend = BlendMode::Screen;
        b.effects.items = vec![Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(1.0, 1.0, 0.5)), GlowTechnique::Softer, 4.0, 0.0, GlowSource::Edge))];
        let mut grp = Layer::group("g", vec![a, b]);
        grp.blend = blend;
        grp.opacity = 0.85;
        grp.effects.items = vec![ds.clone(), st.clone()];
        d.layers.push(grp);
        fx_check(&mut g, &d, &format!("group {blend:?}"));
    }
    // An effect layer as a clipping base, and a clipped layer with effects.
    let mut d = fx_doc(100, 80, SampleType::U8);
    let mut base = blob("base", d.pixel_format(), 45.0, 40.0, 24.0, [0.3, 0.8, 0.4]);
    base.effects.items = vec![ds.clone(), bv.clone()];
    let mut c1 = noise_layer("c1", PixelFormat::RGBA8, Rect::new(10, 10, 60, 50), 91, 0.4);
    c1.clipped = true;
    c1.blend = BlendMode::Multiply;
    let mut c2 = blob("c2", d.pixel_format(), 60.0, 50.0, 12.0, [0.9, 0.9, 0.1]);
    c2.clipped = true;
    c2.effects.items = vec![st.clone(), ds.clone()];
    c2.opacity = 0.8;
    d.layers.extend([base, c1, c2]);
    fx_check(&mut g, &d, "clipping with effects");
    // A solid fill layer with a mask and effects; a gradient fill with a stroke.
    let mut d = fx_doc(90, 70, SampleType::U8);
    let mut solid = Layer::new("solid", LayerContent::Fill(Fill::Solid(Color::rgb(0.2, 0.5, 0.8))));
    let mut m = LayerMask::reveal_all();
    m.surface = photocraft_raster::Surface::with_default(PixelFormat::GRAY8, &[0.0]);
    m.surface.fill_rect(Rect::new(20, 15, 60, 50), &[1.0]);
    solid.mask = Some(m);
    solid.effects.items = vec![ds.clone(), st.clone(), bv];
    d.layers.push(solid);
    fx_check(&mut g, &d, "solid fill with effects");
}

#[test]
fn layer_effects_update_incrementally() {
    let Some(mut g) = gpu() else { return };
    let stack = effect_cases().into_iter().find(|(n, _)| *n == "full stack").unwrap().1;
    let mut d = fx_doc(700, 600, SampleType::U8);
    let mut l = blob("fx", d.pixel_format(), 300.0, 280.0, 200.0, [0.9, 0.4, 0.2]);
    l.effects.items = stack;
    d.layers.push(l);
    d.layers.push(noise_layer("plain", PixelFormat::RGBA8, Rect::new(500, 20, 650, 120), 5, 0.0));
    let first = fx_check(&mut g, &d, "initial");
    assert!(first.fx_programs > 0 && first.fx_shapes == 1);
    // An unrelated edit (another layer's pixels, the effect layer's opacity and an effect colour)
    // reuses every map.
    d.layers[2].surface_mut().unwrap().fill_rect(Rect::new(520, 30, 560, 60), &[0.0, 1.0, 0.0, 1.0]);
    d.layers[1].opacity = 0.8;
    if let Effect::ColorOverlay { color, .. } = &mut d.layers[1].effects.items[6] {
        *color = Color::rgb(0.1, 0.9, 0.9);
    }
    let s = fx_check(&mut g, &d, "unrelated edit");
    assert_eq!((s.fx_shapes, s.fx_programs), (0, 0), "{s:?}");
    // A dab on the effect layer recomputes a neighbourhood of it only.
    d.layers[1].surface_mut().unwrap().fill_rect(Rect::new(290, 270, 330, 300), &[0.1, 0.1, 0.9, 1.0]);
    let s = fx_check(&mut g, &d, "dab on the effect layer");
    assert_eq!(s.fx_shapes, 1);
    assert!(s.fx_pixels < first.fx_pixels / 2, "partial {} vs full {}", s.fx_pixels, first.fx_pixels);
    // Erasing to the layer's edge grows nothing; erasing all of it empties the effects.
    d.layers[1].surface_mut().unwrap().fill_rect(Rect::new(80, 60, 200, 200), &[0.0, 0.0, 0.0, 0.0]);
    fx_check(&mut g, &d, "erase part");
    // A setting change that keeps the effect reach rebuilds that effect only.
    if let Effect::DropShadow(s) = &mut d.layers[1].effects.items[0] {
        s.spread = 0.3;
    }
    let s = fx_check(&mut g, &d, "drop shadow spread");
    assert_eq!(s.fx_programs, 1, "{s:?}");
    // Moving the layer by whole pixels moves its maps with it: nothing is rebuilt.
    let moved = {
        let src = d.layers[1].surface().unwrap();
        let b = src.content_bounds();
        let mut dst = photocraft_raster::Surface::new(src.format());
        dst.write_region(Rect::new(b.x0 + 17, b.y0 - 9, b.x1 + 17, b.y1 - 9), &src.read_region(b));
        dst
    };
    *d.layers[1].surface_mut().unwrap() = moved;
    let s = fx_check(&mut g, &d, "moved");
    assert_eq!(s.fx_programs, 0, "{s:?}");
    // A change of shape with the move rebuilds.
    d.layers[1].surface_mut().unwrap().fill_rect(Rect::new(300, 300, 310, 310), &[1.0, 1.0, 1.0, 0.5]);
    fx_check(&mut g, &d, "moved and painted");
}

#[test]
fn layer_effects_on_shape_layers() {
    let Some(mut g) = gpu() else { return };
    use photocraft_doc::vector::{Path, ShapeLayer, Subpath};
    for alpha in [1.0, 0.55] {
        let mut d = fx_doc(90, 70, SampleType::U8);
        let path = Path::new(vec![Subpath::polygon(&[(14.3, 12.6), (70.2, 18.1), (60.7, 58.4), (24.9, 50.2)])]);
        let mut fill = Color::rgb(0.3, 0.6, 0.9);
        fill.alpha = alpha;
        let mut sh = ShapeLayer { path, fill: Some(Fill::Solid(fill)), stroke: None, live: None, cache: None, psd_raw: None };
        sh.cache = Some(photocraft_vector::render_shape(&sh, d.pixel_format(), d.bounds()));
        let mut l = Layer::new("shape", LayerContent::Shape(sh));
        l.effects.items = vec![
            Effect::Stroke(stroke(3.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(1.0, 1.0, 1.0)))),
            Effect::Stroke(stroke(4.0, StrokePosition::Center, FxPaint::Gradient(gradient()))),
            Effect::DropShadow(shadow(BlendMode::Multiply, 0.7, 120.0, 4.0, 5.0, 0.0)),
            Effect::BevelEmboss(bevel(BevelStyle::InnerBevel, true, 4.0, 1.0)),
        ];
        d.layers.push(l);
        fx_check(&mut g, &d, &format!("shape layer alpha {alpha}"));
    }
}

#[test]
fn stroke_effects_on_filled_and_stroked_shapes() {
    // compose::effect_outline (a fading gradient fill keeps its strokes along the path), several
    // stroke instances with gradient frames, and a vector stroke above the interior effects with
    // clipped layers and a mask.
    let Some(mut g) = gpu() else { return };
    use photocraft_doc::vector::{Path, ShapeLayer, ShapeStroke, StrokeAlign, Subpath};
    for (fill_kind, vector_stroke, masked) in [(0, false, false), (1, false, true), (0, true, false), (1, true, true)] {
        let mut d = fx_doc(90, 70, SampleType::U8);
        let path = Path::new(vec![Subpath::polygon(&[(14.3, 12.6), (70.2, 18.1), (60.7, 58.4), (24.9, 50.2)])]);
        let mut clear = Color::rgb(0.9, 0.3, 0.1);
        clear.alpha = 0.0;
        let fill = if fill_kind == 0 {
            Fill::Solid(Color::rgb(0.3, 0.6, 0.9))
        } else {
            Fill::gradient(vec![(0.0, Color::rgb(0.9, 0.3, 0.1)), (1.0, clear)], 20.0, 1.0, GradientStyle::Linear, false)
        };
        let stroke_v = vector_stroke.then(|| ShapeStroke {
            width: 3.0,
            align: StrokeAlign::Inside,
            paint: Fill::Solid(Color::rgb(0.1, 0.8, 0.2)),
            ..ShapeStroke::default()
        });
        let mut sh = ShapeLayer { path, fill: Some(fill), stroke: stroke_v, live: None, cache: None, psd_raw: None };
        sh.cache = Some(photocraft_vector::render_shape(&sh, d.pixel_format(), d.bounds()));
        let mut l = Layer::new("shape", LayerContent::Shape(sh));
        if masked {
            l.mask = Some(mask(Rect::new(0, 0, 90, 70), 7, 0.6));
        }
        l.effects.items = vec![
            Effect::Stroke(stroke(2.0, StrokePosition::Outside, FxPaint::Color(Color::rgb(1.0, 1.0, 1.0)))),
            Effect::Stroke(stroke(5.0, StrokePosition::Outside, FxPaint::Gradient(gradient()))),
            Effect::Stroke(stroke(3.0, StrokePosition::Inside, FxPaint::Gradient(gradient()))),
            Effect::ColorOverlay { common: photocraft_doc::FxCommon::new(BlendMode::Multiply, 0.7), color: Color::rgb(0.2, 0.2, 0.9) },
            Effect::DropShadow(shadow(BlendMode::Multiply, 0.7, 120.0, 4.0, 5.0, 0.0)),
        ];
        d.layers.push(l);
        let mut c = noise_layer("clip", PixelFormat::RGBA8, Rect::new(30, 0, 60, 70), 9, 0.5);
        c.clipped = true;
        d.layers.push(c);
        fx_check(&mut g, &d, &format!("fill {fill_kind} vector stroke {vector_stroke} masked {masked}"));
    }
}

#[test]
fn channel_restrictions() {
    let Some(mut g) = gpu() else { return };
    for mask_bits in [0b001u32, 0b010, 0b100, 0b101, 0b111] {
        // Raster layer in a blend mode, an adjustment and a clipped layer.
        let mut d = base_doc(64, 48);
        let mut l = noise_layer("top", PixelFormat::RGBA8, Rect::new(4, 3, 60, 45), 2, 0.0);
        l.blend = BlendMode::Multiply;
        l.excluded_channels = mask_bits;
        d.layers.push(l);
        let mut adj = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
        adj.excluded_channels = mask_bits.rotate_left(1) & 0b111;
        d.layers.push(adj);
        let mut c = noise_layer("clip", PixelFormat::RGBA8, Rect::new(10, 10, 50, 40), 3, 0.2);
        c.clipped = true;
        c.excluded_channels = mask_bits;
        d.layers.push(c);
        check(&mut g, &d, &format!("channels {mask_bits:03b}"));

        // An effect layer (exterior and interior effects).
        let mut d = fx_doc(96, 80, SampleType::U8);
        let mut l = blob("fx", d.pixel_format(), 46.0, 40.0, 22.0, [0.9, 0.4, 0.2]);
        l.effects.items = vec![
            Effect::DropShadow(shadow(BlendMode::Multiply, 0.7, 120.0, 6.0, 8.0, 0.1)),
            Effect::InnerGlow(glow(FxPaint::Color(Color::rgb(1.0, 1.0, 0.5)), GlowTechnique::Softer, 9.0, 0.0, GlowSource::Edge)),
        ];
        l.excluded_channels = mask_bits;
        d.layers.push(l);
        fx_check(&mut g, &d, &format!("fx channels {mask_bits:03b}"));
    }
    // Grayscale: the single channel left out keeps the backdrop.
    let mut d = Document::new("g", Size::new(32, 32), ColorMode::Grayscale, SampleType::U8);
    d.layers.push(noise_layer("bg", PixelFormat::GRAYA8, Rect::new(0, 0, 32, 32), 9, 1.0));
    let mut l = noise_layer("top", PixelFormat::GRAYA8, Rect::new(0, 0, 32, 32), 10, 1.0);
    l.excluded_channels = 1;
    d.layers.push(l);
    check(&mut g, &d, "gray channels");
    let flat = photocraft_compose::flatten(&d);
    let bg = photocraft_compose::render_layer(&d.layers[0], d.bounds());
    assert!(flat.px.iter().zip(&bg.px).all(|(a, b)| (a[0] - b[0]).abs() < 1e-6));
}

/// A type layer whose rendered pixels are `src`'s (blends with the text gamma).
fn as_text(src: Layer) -> Layer {
    let t = photocraft_doc::TextLayer { cache: src.surface().cloned(), ..Default::default() };
    let mut l = Layer::new(&src.name, LayerContent::Text(t));
    l.blend = src.blend;
    l.opacity = src.opacity;
    l.effects = src.effects;
    l
}

/// Held by tests that change the process-wide text gamma, and by tests with type layers that
/// run long enough to see such a change.
static TEXT_GAMMA: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn type_layers_blend_with_text_gamma() {
    let _gamma = TEXT_GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut g) = gpu() else { return };
    for mode in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen, BlendMode::Color] {
        let mut d = base_doc(64, 48);
        let mut l = as_text(noise_layer("text", PixelFormat::RGBA8, Rect::new(4, 3, 60, 45), 21, 0.0));
        l.blend = mode;
        l.opacity = 0.9;
        d.layers.push(l);
        // Clipped type layer.
        let mut c = as_text(noise_layer("clip", PixelFormat::RGBA8, Rect::new(8, 8, 50, 40), 22, 0.0));
        c.clipped = true;
        c.blend = mode;
        d.layers.push(c);
        check(&mut g, &d, &format!("text {mode:?}"));
        // Another gamma (Color Settings), and off.
        for gamma in [1.8, 1.0] {
            photocraft_compose::psblend::set_text_gamma(gamma);
            check(&mut g, &d, &format!("text {mode:?} gamma {gamma}"));
        }
        photocraft_compose::psblend::set_text_gamma(photocraft_compose::psblend::TEXT_GAMMA);
    }
    // With effects (the merge of the layer onto its exterior effects).
    let mut d = fx_doc(96, 80, SampleType::U8);
    let mut l = as_text(blob("fx", d.pixel_format(), 46.0, 40.0, 22.0, [0.2, 0.1, 0.6]));
    l.effects.items = vec![
        Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::rgb(0.0, 0.2, 0.6) },
        Effect::DropShadow(shadow(BlendMode::Multiply, 0.6, 90.0, 5.0, 6.0, 0.0)),
    ];
    d.layers.push(l);
    fx_check(&mut g, &d, "text fx");
    // Emboss styles shade the composited layer (merge, shade, then the opacity mix), also as a
    // clipped layer over an opaque base.
    let mut e = fx_doc(96, 80, SampleType::U8);
    let mut l = as_text(blob("emboss", e.pixel_format(), 46.0, 40.0, 20.0, [0.4, 0.1, 0.3]));
    l.effects.items = vec![
        Effect::BevelEmboss(bevel(BevelStyle::Emboss, true, 9.0, 0.0)),
        Effect::BevelEmboss(bevel(BevelStyle::PillowEmboss, false, 5.0, 1.0)),
        Effect::DropShadow(shadow(BlendMode::Multiply, 0.6, 90.0, 5.0, 6.0, 0.0)),
    ];
    l.opacity = 0.7;
    l.fill_opacity = 0.6;
    e.layers.push(l.clone());
    fx_check(&mut g, &e, "text emboss");
    let mut c = l;
    c.clipped = true;
    c.blend = BlendMode::Multiply;
    e.layers.push(c);
    fx_check(&mut g, &e, "clipped text emboss");
    // The gamma changes edge pixels against a linear mix.
    let flat = photocraft_compose::flatten(&d);
    let mut lin = d.clone();
    let raster = {
        let LayerContent::Text(t) = &lin.layers[1].content else { unreachable!() };
        let mut r = Layer::new("r", LayerContent::Raster(t.cache.clone().unwrap()));
        r.effects = lin.layers[1].effects.clone();
        r
    };
    lin.layers[1] = raster;
    let flat_lin = photocraft_compose::flatten(&lin);
    assert!(flat.px.iter().zip(&flat_lin.px).any(|(a, b)| (a[0] - b[0]).abs() > 0.02));
}

#[test]
fn blend_mode_extremes() {
    // Exact 0 / 1 channels on both sides (Color Burn / Dodge / Vivid Light / Hard Mix corners).
    let Some(mut g) = gpu() else { return };
    let vals = [0.0f32, 1.0, 0.5];
    for depth in [SampleType::U8, SampleType::U16] {
        for mode in BlendMode::LAYER_MODES {
            let mut d = Document::new("x", Size::new(9, 9), ColorMode::Rgb, depth);
            let fmt = d.pixel_format();
            let mut bg = Layer::raster("bg", fmt);
            let mut top = Layer::raster("top", fmt);
            for (i, &b) in vals.iter().enumerate() {
                for (j, &s) in vals.iter().enumerate() {
                    for k in 0..3 {
                        let (x, y) = ((i * 3 + k) as i32, j as i32 * 3);
                        let r = Rect::from_xywh(x, y, 1, 3);
                        bg.surface_mut().unwrap().fill_rect(r, &photocraft_raster::from_rgba(&fmt, [b, b, b, 1.0]));
                        top.surface_mut().unwrap().fill_rect(r, &photocraft_raster::from_rgba(&fmt, [s, s, s, [1.0, 0.6, 0.2][k]]));
                    }
                }
            }
            top.blend = mode;
            d.layers.push(bg);
            d.layers.push(top);
            check(&mut g, &d, &format!("extremes {mode:?} {depth:?}"));
        }
    }
}

#[test]
fn stroked_shapes_with_clipped_layers() {
    let Some(mut g) = gpu() else { return };
    use photocraft_doc::vector::{Path, ShapeLayer, ShapeStroke, Subpath};
    for (blend, masked) in [(BlendMode::Normal, false), (BlendMode::Multiply, true)] {
        let mut d = base_doc(80, 64);
        let path = Path::new(vec![Subpath::polygon(&[(10.3, 8.6), (66.2, 12.1), (58.7, 54.4), (16.9, 48.2)])]);
        let stroke = ShapeStroke { width: 5.0, paint: Fill::Solid(Color::rgb(0.9, 0.9, 0.1)), ..Default::default() };
        let mut sh = ShapeLayer { path, fill: Some(Fill::Solid(Color::rgb(0.2, 0.3, 0.8))), stroke: Some(stroke), live: None, cache: None, psd_raw: None };
        sh.cache = Some(photocraft_vector::render_shape(&sh, d.pixel_format(), d.bounds()));
        let mut l = Layer::new("shape", LayerContent::Shape(sh));
        l.blend = blend;
        l.opacity = 0.9;
        if masked {
            l.mask = Some(mask(Rect::new(0, 0, 80, 64), 41, 1.0));
        }
        let mut c = noise_layer("clip", PixelFormat::RGBA8, Rect::new(0, 0, 80, 64), 42, 0.5);
        c.clipped = true;
        c.blend = BlendMode::Screen;
        d.layers.extend([l, c]);
        let st = photocraft_gpu::render_to_vec_stats(&mut g.comp, &g.device, &g.queue, &d, d.bounds());
        assert!(st.is_ok(), "planned on the GPU");
        check(&mut g, &d, &format!("stroked shape + clipped {blend:?} masked {masked}"));
    }
}

#[test]
fn artboards() {
    let Some(mut g) = gpu() else { return };
    use photocraft_doc::{Artboard, ArtboardBackground};
    let backgrounds = [
        ArtboardBackground::White,
        ArtboardBackground::Transparent,
        ArtboardBackground::Custom({
            let mut c = Color::rgb(0.2, 0.7, 0.4);
            c.alpha = 0.5;
            c
        }),
    ];
    for bg in backgrounds {
        for blend in [BlendMode::PassThrough, BlendMode::Normal, BlendMode::Multiply] {
            let mut d = base_doc(72, 48);
            let mut child = noise_layer("c", PixelFormat::RGBA8, Rect::new(0, 0, 72, 48), 51, 0.0);
            child.blend = BlendMode::Screen;
            let mut fx = blob("fx", PixelFormat::RGBA8, 34.0, 24.0, 12.0, [0.8, 0.3, 0.2]);
            fx.effects.items = vec![Effect::DropShadow(shadow(BlendMode::Multiply, 0.8, 120.0, 6.0, 8.0, 0.0))];
            let mut ab = Layer::group("Artboard", vec![child, fx]);
            ab.blend = blend;
            if let LayerContent::Group(gr) = &mut ab.content {
                gr.artboard = Some(Artboard { rect: Rect::new(12, 6, 60, 40), background: bg, preset: String::new() });
            }
            d.layers.push(ab);
            // A second, partly off-canvas board.
            let mut ab2 = Layer::group("Artboard 2", vec![noise_layer("c2", PixelFormat::RGBA8, Rect::new(0, 0, 72, 48), 52, 0.3)]);
            if let LayerContent::Group(gr) = &mut ab2.content {
                gr.artboard = Some(Artboard { rect: Rect::new(50, 30, 90, 70), background: ArtboardBackground::Black, preset: String::new() });
            }
            d.layers.push(ab2);
            fx_check(&mut g, &d, &format!("artboard {bg:?} {blend:?}"));
        }
    }
}

#[test]
fn pattern_fill_layers() {
    let Some(mut g) = gpu() else { return };
    let pat = checker_pattern();
    for (link, scale, angle, masked) in [(true, 1.0, 0.0, false), (false, 0.7, 25.0, true), (true, 1.6, -40.0, false)] {
        let mut d = base_doc(70, 50);
        d.patterns.push(pat.clone());
        let fill = Fill::Pattern { name: pat.name.clone(), id: pat.id.clone(), scale, angle, link, phase: (3.0, -2.0) };
        let mut l = Layer::new("pat", LayerContent::Fill(fill));
        l.opacity = 0.85;
        l.blend = BlendMode::Overlay;
        if masked {
            l.mask = Some(mask(Rect::new(5, 5, 60, 45), 61, 0.0));
        }
        d.layers.push(l.clone());
        // With effects, and a missing pattern (transparent).
        let mut fx = l.clone();
        fx.id = photocraft_doc::LayerId::fresh();
        fx.effects.items = vec![Effect::DropShadow(shadow(BlendMode::Multiply, 0.6, 90.0, 3.0, 4.0, 0.0))];
        fx.mask = Some(mask(Rect::new(20, 10, 50, 40), 62, 0.0));
        d.layers.push(fx);
        let mut missing = l;
        missing.id = photocraft_doc::LayerId::fresh();
        missing.content = LayerContent::Fill(Fill::Pattern { name: "nope".into(), id: "nope".into(), scale: 1.0, angle: 0.0, link: true, phase: (0.0, 0.0) });
        d.layers.push(missing);
        fx_check(&mut g, &d, &format!("pattern fill link {link} scale {scale} angle {angle}"));
    }
}

#[test]
fn lab_documents_mix_in_lab() {
    let Some(mut g) = gpu() else { return };
    for depth in [SampleType::U8, SampleType::U16] {
        let mut d = Document::new("lab", Size::new(48, 40), ColorMode::Lab, depth);
        let fmt = d.pixel_format();
        d.layers.push(noise_layer("bg", fmt, Rect::new(0, 0, 48, 40), 81, 0.7));
        let mut top = noise_layer("top", fmt, Rect::new(4, 4, 44, 36), 82, 0.0);
        top.opacity = 0.8;
        d.layers.push(top);
        let mut m = noise_layer("mul", fmt, Rect::new(10, 2, 30, 38), 83, 0.3);
        m.blend = BlendMode::Multiply;
        d.layers.push(m);
        let mut fx = blob("fx", fmt, 24.0, 20.0, 12.0, [0.7, 0.3, 0.2]);
        fx.effects.items = vec![Effect::DropShadow(shadow(BlendMode::Normal, 0.6, 120.0, 4.0, 5.0, 0.0))];
        d.layers.push(fx);
        fx_check(&mut g, &d, &format!("lab {depth:?}"));
    }
    // The mix really is in Lab: a half-transparent edge differs from an sRGB mix.
    let mut d = Document::new("lab", Size::new(2, 1), ColorMode::Lab, SampleType::U8);
    let fmt = d.pixel_format();
    let mut a = Layer::raster("a", fmt);
    a.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 1), &photocraft_raster::from_rgba(&fmt, [0.0, 0.0, 1.0, 1.0]));
    let mut b = Layer::raster("b", fmt);
    b.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2, 1), &photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 0.0, 0.5]));
    d.layers = vec![a, b];
    let p = photocraft_compose::flatten(&d).px[0];
    assert!((p[0] - 0.5).abs() > 0.05 || (p[2] - 0.5).abs() > 0.05, "{p:?}");
}

#[test]
fn rgba16f_fallback_path_renders() {
    // Issue #7 / #4: on adapters that can't render Rgba32Float the compositor falls back to
    // Rgba16Float. Force that path here (even on a 32f-capable GPU) to prove it works end to end —
    // pipeline creation, the accumulation texture, and the half-float readback — within display
    // tolerance of the CPU reference. This is the path Intel-Vulkan / limited GPUs take.
    let Some((adapter, device, queue, _lock)) = any_device() else { return };
    let mut comp = match Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba16Float) {
        Ok(c) => c,
        Err(e) => return eprintln!("skipping: {e}"),
    };
    let mut d = base_doc(48, 32);
    let mut top = noise_layer("top", PixelFormat::RGBA8, Rect::from_xywh(0, 0, 48, 32), 7, 1.0);
    top.blend = BlendMode::Multiply;
    top.opacity = 0.8;
    d.layers.push(top);
    d.layers.push(Layer::new("adj", LayerContent::Adjustment(Adjustment::BrightnessContrast { brightness: 30.0, contrast: 40.0, legacy: false })));
    let cpu = photocraft_compose::flatten(&d);
    let out = render_to_vec(&mut comp, &device, &queue, &d, d.bounds()).expect("16f render");
    let mut worst = 0.0f32;
    for (c, o) in cpu.px.iter().zip(&out) {
        for k in 0..4 {
            worst = worst.max((c[k] * c[3] - o[k] * o[3]).abs());
        }
    }
    // Half-float display precision — looser than the exact 1/255 parity bar, but proves the path works.
    assert!(worst <= 3.0 / 255.0, "16f fallback max diff {:.2}/255", worst * 255.0);
    // preferred_acc_format returns a renderable format for this adapter.
    let f = Compositor::preferred_acc_format(&adapter);
    assert!(matches!(f, wgpu::TextureFormat::Rgba32Float | wgpu::TextureFormat::Rgba16Float));
}

#[test]
fn unbuildable_pipelines_are_an_error_not_a_panic() {
    // The app falls back to the CPU compositor on Err; a panic here would crash it (as FXC once
    // did on D3D12). A depth format can't be a colour target, so its pipelines fail to build.
    let Some((adapter, device, _, _lock)) = any_device() else { return };
    let e = Compositor::try_new_with_format(&device, wgpu::TextureFormat::Depth32Float).err().expect("depth target must fail");
    assert!(e.0.contains("pipelines"), "{e}");
    // The format the app picks builds (effect maps the adapter can't render are left out, not fatal).
    let f = Compositor::preferred_acc_format(&adapter);
    if adapter.get_texture_format_features(f).allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        assert!(Compositor::try_new_with_format(&device, f).is_ok());
    }
}

// ---- documents larger than the texture limit ------------------------------------------------

/// A document several pages wide under [`PAGED_LIMIT`]: pixel layers crossing page borders (one
/// masked, one off-canvas), a big effect layer whose region exceeds the limit (maps built per
/// page cell), a type layer with a shadow, an adjustment, a clipped group and a gradient fill.
fn big_doc() -> Document {
    let (w, h) = (1100, 900);
    let mut d = fx_doc(w, h, SampleType::U8);
    // Right over the background: adjustment results round to 8 bits, which would turn sub-LSB
    // float differences of blends beneath into whole steps.
    let mut adj = Layer::new("invert", LayerContent::Adjustment(Adjustment::Invert));
    adj.opacity = 0.6;
    adj.mask = Some(mask(Rect::new(240, 0, 800, 520), 83, 0.0));
    d.layers.push(adj);
    let mut top = noise_layer("top", PixelFormat::RGBA8, Rect::new(-30, 200, 700, 820), 81, 0.0);
    top.blend = BlendMode::Multiply;
    top.mask = Some(mask(Rect::new(100, 240, 600, 700), 82, 1.0));
    d.layers.push(top);
    let stack = effect_cases().into_iter().find(|(n, _)| *n == "full stack").unwrap().1;
    let mut big = blob("big", d.pixel_format(), 520.0, 450.0, 330.0, [0.9, 0.4, 0.2]);
    big.effects.items = stack;
    big.effects.items.push(Effect::OuterGlow(glow(FxPaint::Color(Color::rgb(0.2, 1.0, 0.6)), GlowTechnique::Softer, 40.0, 0.2, GlowSource::Edge)));
    d.layers.push(big);
    let mut text = as_text(blob("type", d.pixel_format(), 900.0, 150.0, 90.0, [0.1, 0.2, 0.9]));
    text.effects.items = vec![Effect::DropShadow(shadow(BlendMode::Multiply, 0.8, 135.0, 12.0, 20.0, 0.0))];
    d.layers.push(text);
    let a = noise_layer("ga", PixelFormat::RGBA8, Rect::new(200, 500, 1000, 880), 84, 0.6);
    let mut c = noise_layer("gc", PixelFormat::RGBA8, Rect::new(0, 600, 1100, 700), 85, 0.0);
    c.clipped = true;
    c.blend = BlendMode::Screen;
    let mut grp = Layer::group("g", vec![a, c]);
    grp.opacity = 0.9;
    d.layers.push(grp);
    let stops = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.6, Color::rgb(0.0, 1.0, 0.2)), (1.0, Color::rgb(0.1, 0.1, 0.9))];
    let mut grad = Layer::new("grad", LayerContent::Fill(Fill::gradient(stops, 30.0, 0.8, GradientStyle::Radial, false)));
    grad.opacity = 0.3;
    grad.blend = BlendMode::Overlay;
    d.layers.push(grad);
    d
}

#[test]
fn documents_larger_than_the_texture_limit() {
    let _gamma = TEXT_GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut g) = gpu() else { return };
    let mut d = big_doc();
    assert!(g.paged.supports(&d).is_ok());
    // Every pass sees page borders, and effect maps and blurs crossing them.
    check(&mut g, &d, "big document");
    let s = photocraft_gpu::render_to_vec_stats(&mut g.paged, &g.device, &g.queue, &d, d.bounds()).unwrap().1;
    assert!(s.cells >= 20 && s.chunks == s.cells, "{s:?}");
    // A viewport off the page grid covers only its cells.
    let view = Rect::new(300, 330, 790, 610);
    diff_rect(&mut g, &d, view, "viewport").unwrap_or_else(|e| panic!("{e}"));
    let s = photocraft_gpu::render_to_vec_stats(&mut g.paged, &g.device, &g.queue, &d, view).unwrap().1;
    assert_eq!(s.cells, 6, "{s:?}");
    // A dab across a page corner on the effect layer, and an edit elsewhere.
    d.layers[3].surface_mut().unwrap().fill_rect(Rect::new(500, 490, 530, 530), &[0.1, 0.1, 0.9, 1.0]);
    d.layers[2].surface_mut().unwrap().fill_rect(Rect::new(250, 250, 270, 270), &[0.0, 0.0, 0.0, 0.0]);
    check(&mut g, &d, "after dabs");
    let s = photocraft_gpu::render_to_vec_stats(&mut g.paged, &g.device, &g.queue, &d, d.bounds()).unwrap().1;
    assert_eq!((s.tiles_uploaded, s.fx_shapes), (0, 0), "nothing changed since the last render: {s:?}");
}

#[test]
fn pages_are_evicted_under_the_budget() {
    let _gamma = TEXT_GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut g) = gpu() else { return };
    let mut d = big_doc();
    check(&mut g, &d, "warm");
    let all = g.paged.resident_bytes();
    // A budget of one page: everything but the cell being drawn is evicted as the render goes.
    let page = u64::from(g.paged.page_size()).pow(2) * 4;
    g.paged.set_resident_budget(page);
    for round in 0..3 {
        let cpu = photocraft_compose::flatten(&d);
        let (out, s) = photocraft_gpu::render_to_vec_stats(&mut g.paged, &g.device, &g.queue, &d, d.bounds()).unwrap();
        let worst = worst_diff(&cpu.px, &out);
        assert!(worst.0 <= TOL, "round {round}: max diff {:.2}/255 at {}", worst.0 * 255.0, worst.1);
        assert!(s.evicted > 0, "round {round}: {s:?}");
        assert!(g.paged.resident_bytes() < all / 4, "{} of {all}", g.paged.resident_bytes());
        // Evicted pages come back with the edits made meanwhile.
        d.layers[2].surface_mut().unwrap().fill_rect(Rect::new(10 + round * 300, 300, 60 + round * 300, 340), &[0.9, 0.9, 0.1, 1.0]);
        d.layers[3].surface_mut().unwrap().fill_rect(Rect::new(200 + round * 200, 420, 240 + round * 200, 470), &[0.0, 0.0, 0.0, 0.0]);
    }
    check(&mut g, &d, "after eviction");
}

#[test]
fn the_focused_view_stays_resident_after_a_full_refresh() {
    let _gamma = TEXT_GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(g) = gpu() else { return };
    let d = big_doc();
    let mut comp = Compositor::try_new_with_format(&g.device, wgpu::TextureFormat::Rgba32Float).unwrap();
    comp.set_texture_limit(PAGED_LIMIT);
    let view = Rect::new(520, 300, 760, 500);
    photocraft_gpu::render_to_vec(&mut comp, &g.device, &g.queue, &d, view).unwrap();
    // A budget holding just the view's pages: a full refresh evicts as it goes, and draws the
    // view's cells last, so the next edit in the view uploads nothing.
    // The working set of a region is what rendering it makes resident.
    assert_eq!(comp.working_set(&d, view), comp.resident_bytes());
    assert!(comp.working_set(&d, d.bounds()) > 4 * comp.resident_bytes());
    comp.set_resident_budget(comp.resident_bytes());
    assert!(comp.fits_budget(&d, view) && !comp.fits_budget(&d, d.bounds()));
    comp.set_focus(Some(view));
    for _ in 0..2 {
        let s = photocraft_gpu::render_to_vec_stats(&mut comp, &g.device, &g.queue, &d, d.bounds()).unwrap().1;
        assert!(s.evicted > 0, "{s:?}");
        let (out, s) = photocraft_gpu::render_to_vec_stats(&mut comp, &g.device, &g.queue, &d, view).unwrap();
        assert_eq!(s.tiles_uploaded, 0, "{s:?}");
        let cpu = photocraft_compose::render(&d, view);
        assert!(worst_diff(&cpu.px, &out).0 <= TOL);
    }
}

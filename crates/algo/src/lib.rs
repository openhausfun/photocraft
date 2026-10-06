//! # photocraft-algo
//!
//! CPU image filters (Photoshop's Filter menu): blur, sharpen, noise,
//! pixelate, stylize and distort. Every filter is
//!
//! * **depth-agnostic**: works on normalized `f32` samples read from any
//!   [`Surface`] (8/16-bit integer or 32-bit float) in the surface's own
//!   colour model;
//! * **tile/region aware**: [`FilterParams::halo`] declares how far outside an
//!   output tile the filter reads, so results are identical whatever the
//!   tiling (distortions declare [`Halo::Bounds`] and read the whole
//!   reference area);
//! * **selection aware**: output is mixed with the original by the
//!   selection's coverage.
//!
//! [`apply`] runs a filter over an area, tile by tile (in parallel with
//! rayon on native targets; single-threaded on wasm).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod artistic;
mod artistic_fx;
mod blur;
mod blur2;
pub mod camera_raw;
pub mod content_aware;
mod denoise;
mod distort;
mod distort2;
pub mod erase;
pub mod exif;
pub mod features;
mod fxutil;
mod gallery;
pub mod hdr;
mod image;
pub mod inpaint;
pub mod lens;
pub mod liquify;
pub mod matting;
mod noise;
mod oil;
mod other;
mod other2;
pub mod paint;
pub mod panorama;
pub mod perspective;
mod photo_util;
mod pixelate;
pub mod poisson;
pub mod puppet;
pub mod pyramid;
pub mod quantize;
mod render;
pub mod render2;
pub mod resample;
pub mod retouch;
pub mod scancrop;
pub mod seam;
pub mod segment;
pub mod selection;
mod selection_blur;
mod sharpen;
pub mod stack;
mod stylize;
mod stylize2;
pub mod tone;
pub mod transform;
pub mod trap;
pub mod vanishing;
mod video;
pub mod warp;
pub mod wideangle;

pub use artistic::{GALLERY_CATEGORIES, GalleryEffect, GalleryFilter, GalleryParam, GalleryParamKind};
pub use image::{Edge, Image};
pub use params_ext::*;

mod params_ext;

use photocraft_color::ColorMode;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

/// How far outside an output tile a filter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halo {
    /// A margin of this many pixels.
    Radius(i32),
    /// The whole reference bounds (distortions, radial blur).
    Bounds,
}

/// Radial blur method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RadialMethod {
    #[default]
    Spin,
    Zoom,
}

/// Noise distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Distribution {
    #[default]
    Uniform,
    Gaussian,
}

/// How pixels uncovered by a displacement are filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum UndefinedAreas {
    #[default]
    Wrap,
    Repeat,
    Transparent,
}

/// Minimum / Maximum › Preserve: the shape the filter grows or shrinks by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Preserve {
    /// A square of `2·radius + 1` pixels (keeps corners square).
    #[default]
    Squareness,
    /// A disc of `radius` pixels (rounds corners off evenly).
    Roundness,
}

/// Spherize mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SpherizeMode {
    #[default]
    Normal,
    HorizontalOnly,
    VerticalOnly,
}

/// Wave shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WaveType {
    #[default]
    Sine,
    Triangle,
    Square,
}

/// Ripple size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RippleSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// Polar coordinates direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PolarMode {
    #[default]
    RectangularToPolar,
    PolarToRectangular,
}

/// A filter and its parameters, in Photoshop's dialog units (pixels,
/// percent, degrees, levels 0–255).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "filter", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FilterParams {
    /// Radius = Gaussian standard deviation in pixels (0.1–1000).
    GaussianBlur {
        radius: f32,
    },
    /// Box of `2·radius + 1` pixels.
    BoxBlur {
        radius: f32,
    },
    MotionBlur {
        angle: f32,
        distance: f32,
    },
    /// Amount 1–100; centre as a fraction of the bounds (0.5, 0.5 = centre).
    RadialBlur {
        amount: f32,
        method: RadialMethod,
        center_x: f32,
        center_y: f32,
    },
    /// Radius px, threshold in levels.
    SurfaceBlur {
        radius: f32,
        threshold: f32,
    },
    /// Amount %, radius px, threshold levels.
    UnsharpMask {
        amount: f32,
        radius: f32,
        threshold: f32,
    },
    /// Amount %, radius px, noise reduction % (basic: Gaussian unsharp).
    SmartSharpen {
        amount: f32,
        radius: f32,
        reduce_noise: f32,
    },
    HighPass {
        radius: f32,
    },
    /// Amount 0–400 %.
    AddNoise {
        amount: f32,
        distribution: Distribution,
        monochromatic: bool,
        seed: u32,
    },
    Median {
        radius: f32,
    },
    DustAndScratches {
        radius: f32,
        threshold: f32,
    },
    /// Radius px; `preserve` is the dialog's Preserve (older params default to Squareness).
    Minimum {
        radius: f32,
        #[serde(default)]
        preserve: Preserve,
    },
    Maximum {
        radius: f32,
        #[serde(default)]
        preserve: Preserve,
    },
    Offset {
        horizontal: i32,
        vertical: i32,
        undefined: UndefinedAreas,
    },
    Mosaic {
        cell_size: f32,
    },
    /// Angle degrees, height px, amount %.
    Emboss {
        angle: f32,
        height: f32,
        amount: f32,
    },
    FindEdges,
    Solarize,
    Invert,
    Desaturate,
    /// Angle −999…999 degrees.
    Twirl {
        angle: f32,
    },
    /// Amount −100…100 %.
    Pinch {
        amount: f32,
    },
    /// Amount −100…100 %.
    Spherize {
        amount: f32,
        mode: SpherizeMode,
    },
    Wave {
        generators: u32,
        wavelength_min: f32,
        wavelength_max: f32,
        amplitude_min: f32,
        amplitude_max: f32,
        wave_type: WaveType,
        undefined: UndefinedAreas,
        seed: u32,
    },
    /// Amount −999…999 %.
    Ripple {
        amount: f32,
        size: RippleSize,
    },
    PolarCoordinates {
        mode: PolarMode,
    },

    // ---- Pixelate ----
    /// Max dot radius px (4–127); screen angles per channel in degrees.
    ColorHalftone {
        max_radius: f32,
        angles: [f32; 4],
    },
    /// Cell size px (3–300).
    Crystallize {
        cell_size: f32,
        seed: u32,
    },
    Facet,
    Fragment,
    Mezzotint {
        kind: MezzotintType,
        seed: u32,
    },
    /// Cell size px (3–300); `background` is straight sRGB RGBA.
    Pointillize {
        cell_size: f32,
        seed: u32,
        background: [f32; 4],
    },

    // ---- Stylize ----
    Diffuse {
        mode: DiffuseMode,
        seed: u32,
    },
    /// Size px (2–255), depth (1–255).
    Extrude {
        kind: ExtrudeType,
        size: f32,
        depth: f32,
        level_based: bool,
        solid_front: bool,
        mask_incomplete: bool,
        seed: u32,
    },
    /// Photoshop's Oil Paint units: stylization 0.1–10, cleanliness 0–10, scale 0.1–10,
    /// bristle detail 0–10, lighting angle degrees, shine 0–10.
    OilPaint {
        stylization: f32,
        cleanliness: f32,
        scale: f32,
        bristle_detail: f32,
        lighting: bool,
        angle: f32,
        shine: f32,
    },
    /// Tiles along the shorter side (1–99), max offset % (1–99).
    Tiles {
        count: u32,
        max_offset: f32,
        fill: TileFill,
        foreground: [f32; 4],
        background: [f32; 4],
        seed: u32,
    },
    /// Level 0–255; `upper` traces the side above the level.
    TraceContour {
        level: f32,
        upper: bool,
    },
    Wind {
        method: WindMethod,
        from_right: bool,
        seed: u32,
    },

    // ---- Distort ----
    /// Scales in % (−999…999; 100 % = 128 px at full map contrast). The map is
    /// supplied by the caller (another document, layer or file).
    Displace {
        horizontal: f32,
        vertical: f32,
        stretch: bool,
        undefined: UndefinedAreas,
        #[serde(skip)]
        map: Option<std::sync::Arc<Image>>,
    },
    /// Curve points `[t, offset]`: t 0–1 top→bottom, offset −1…1 of half the width.
    Shear {
        points: Vec<[f32; 2]>,
        undefined: UndefinedAreas,
    },
    /// Amount −100…100, ridges 0–20.
    ZigZag {
        amount: f32,
        ridges: f32,
        style: ZigZagStyle,
    },

    // ---- Render ----
    /// Variance 1–64, strength 1–64; colours straight sRGB RGBA.
    Fibers {
        variance: f32,
        strength: f32,
        seed: u32,
        foreground: [f32; 4],
        background: [f32; 4],
    },
    /// Brightness 10–300 %, centre as a fraction of the bounds.
    LensFlare {
        brightness: f32,
        center_x: f32,
        center_y: f32,
        lens: LensType,
    },
    /// Gloss/metallic/exposure/ambience −100…100; bump height 0–100.
    LightingEffects {
        lights: Vec<Light>,
        gloss: f32,
        metallic: f32,
        exposure: f32,
        ambience: f32,
        texture: TextureChannel,
        height: f32,
        white_is_high: bool,
    },

    // ---- Noise ----
    /// Strength 0–10, the rest 0–100 %.
    ReduceNoise {
        strength: f32,
        preserve_details: f32,
        reduce_color_noise: f32,
        sharpen_details: f32,
        remove_jpeg_artifact: bool,
    },

    // ---- Blur ----
    /// Radius 0.1–100 px, threshold 0.1–100 levels.
    SmartBlur {
        radius: f32,
        threshold: f32,
        quality: BlurQuality,
        mode: SmartBlurMode,
    },
    /// Iris radius 0–100 px, `blades` 3–8, curvature 0–100, rotation degrees;
    /// focal distance 0–255; specular brightness 0–100 / threshold 0–255; noise 0–100.
    LensBlur {
        radius: f32,
        blades: u32,
        curvature: f32,
        rotation: f32,
        depth: DepthSource,
        focal_distance: f32,
        invert_depth: bool,
        brightness: f32,
        threshold: f32,
        noise: f32,
        distribution: Distribution,
        monochromatic: bool,
        seed: u32,
        /// Depth for [`DepthSource::LayerMask`] (channel 0, document coordinates), supplied by the caller.
        #[serde(skip)]
        depth_map: Option<std::sync::Arc<Image>>,
    },
    /// Radius 5–1000 px.
    ShapeBlur {
        radius: f32,
        shape: BlurShape,
    },

    // ---- Blur Gallery ----
    /// Centre fractions, angle degrees, focus/transition as fractions of the bounds' shorter side.
    TiltShift {
        blur: f32,
        center_x: f32,
        center_y: f32,
        angle: f32,
        focus: f32,
        transition: f32,
    },
    IrisBlur {
        pins: Vec<IrisPin>,
    },
    FieldBlur {
        pins: Vec<FieldPin>,
    },
    SpinBlur {
        pins: Vec<SpinPin>,
    },
    PathBlur {
        paths: Vec<BlurPath>,
    },

    // ---- Other ----
    /// 5×5 kernel (row-major, centre = index 12), divided by `scale`, plus `offset` levels.
    Custom {
        kernel: Vec<f32>,
        scale: f32,
        offset: f32,
    },
    HsbHsl {
        input: HsbModel,
        output: HsbModel,
    },

    // ---- Video ----
    /// Removes the even (or odd) lines and rebuilds them by interpolation (or duplication).
    DeInterlace {
        eliminate_even: bool,
        interpolate: bool,
    },
    NtscColors,
    /// Filter Gallery: a stack of gallery effects applied in order.
    FilterGallery {
        effects: Vec<GalleryEffect>,
    },
}

impl FilterParams {
    /// Pixels read outside an output tile.
    pub fn halo(&self) -> Halo {
        let g = |sigma: f32| Halo::Radius((sigma.max(0.0) * 3.0).ceil() as i32 + 1);
        match self {
            FilterParams::GaussianBlur { radius } | FilterParams::HighPass { radius } => g(*radius),
            FilterParams::UnsharpMask { radius, .. } | FilterParams::SmartSharpen { radius, .. } => g(*radius),
            FilterParams::BoxBlur { radius }
            | FilterParams::SurfaceBlur { radius, .. }
            | FilterParams::Median { radius }
            | FilterParams::DustAndScratches { radius, .. }
            | FilterParams::Minimum { radius, .. }
            | FilterParams::Maximum { radius, .. } => Halo::Radius(radius.max(0.0).ceil() as i32 + 1),
            FilterParams::MotionBlur { distance, .. } => Halo::Radius((distance.abs() / 2.0).ceil() as i32 + 2),
            FilterParams::Mosaic { cell_size } => Halo::Radius(cell_size.max(1.0).ceil() as i32 + 1),
            FilterParams::Emboss { height, .. } => Halo::Radius(height.abs().ceil() as i32 + 2),
            FilterParams::FindEdges => Halo::Radius(1),
            FilterParams::AddNoise { .. } | FilterParams::Solarize | FilterParams::Invert | FilterParams::Desaturate => Halo::Radius(0),
            FilterParams::RadialBlur { .. }
            | FilterParams::Offset { .. }
            | FilterParams::Twirl { .. }
            | FilterParams::Pinch { .. }
            | FilterParams::Spherize { .. }
            | FilterParams::Wave { .. }
            | FilterParams::Ripple { .. }
            | FilterParams::PolarCoordinates { .. } => Halo::Bounds,
            other => halo_ext(other),
        }
    }

    /// Whether the filter moves pixels around the reference bounds (its
    /// output area is the bounds, not the layer's content grown by the halo).
    pub fn is_global(&self) -> bool {
        self.halo() == Halo::Bounds
    }

    /// Human-readable name.
    pub fn label(&self) -> &'static str {
        match self {
            FilterParams::GaussianBlur { .. } => "Gaussian Blur",
            FilterParams::BoxBlur { .. } => "Box Blur",
            FilterParams::MotionBlur { .. } => "Motion Blur",
            FilterParams::RadialBlur { .. } => "Radial Blur",
            FilterParams::SurfaceBlur { .. } => "Surface Blur",
            FilterParams::UnsharpMask { .. } => "Unsharp Mask",
            FilterParams::SmartSharpen { .. } => "Smart Sharpen",
            FilterParams::HighPass { .. } => "High Pass",
            FilterParams::AddNoise { .. } => "Add Noise",
            FilterParams::Median { .. } => "Median",
            FilterParams::DustAndScratches { .. } => "Dust & Scratches",
            FilterParams::Minimum { .. } => "Minimum",
            FilterParams::Maximum { .. } => "Maximum",
            FilterParams::Offset { .. } => "Offset",
            FilterParams::Mosaic { .. } => "Mosaic",
            FilterParams::Emboss { .. } => "Emboss",
            FilterParams::FindEdges => "Find Edges",
            FilterParams::Solarize => "Solarize",
            FilterParams::Invert => "Invert",
            FilterParams::Desaturate => "Desaturate",
            FilterParams::Twirl { .. } => "Twirl",
            FilterParams::Pinch { .. } => "Pinch",
            FilterParams::Spherize { .. } => "Spherize",
            FilterParams::Wave { .. } => "Wave",
            FilterParams::Ripple { .. } => "Ripple",
            FilterParams::PolarCoordinates { .. } => "Polar Coordinates",
            FilterParams::ColorHalftone { .. } => "Color Halftone",
            FilterParams::Crystallize { .. } => "Crystallize",
            FilterParams::Facet => "Facet",
            FilterParams::Fragment => "Fragment",
            FilterParams::Mezzotint { .. } => "Mezzotint",
            FilterParams::Pointillize { .. } => "Pointillize",
            FilterParams::Diffuse { .. } => "Diffuse",
            FilterParams::Extrude { .. } => "Extrude",
            FilterParams::OilPaint { .. } => "Oil Paint",
            FilterParams::Tiles { .. } => "Tiles",
            FilterParams::TraceContour { .. } => "Trace Contour",
            FilterParams::Wind { .. } => "Wind",
            FilterParams::Displace { .. } => "Displace",
            FilterParams::Shear { .. } => "Shear",
            FilterParams::ZigZag { .. } => "ZigZag",
            FilterParams::Fibers { .. } => "Fibers",
            FilterParams::LensFlare { .. } => "Lens Flare",
            FilterParams::LightingEffects { .. } => "Lighting Effects",
            FilterParams::ReduceNoise { .. } => "Reduce Noise",
            FilterParams::SmartBlur { .. } => "Smart Blur",
            FilterParams::LensBlur { .. } => "Lens Blur",
            FilterParams::ShapeBlur { .. } => "Shape Blur",
            FilterParams::TiltShift { .. } => "Tilt-Shift",
            FilterParams::IrisBlur { .. } => "Iris Blur",
            FilterParams::FieldBlur { .. } => "Field Blur",
            FilterParams::SpinBlur { .. } => "Spin Blur",
            FilterParams::PathBlur { .. } => "Path Blur",
            FilterParams::Custom { .. } => "Custom",
            FilterParams::HsbHsl { .. } => "HSB/HSL",
            FilterParams::DeInterlace { .. } => "De-Interlace",
            FilterParams::NtscColors => "NTSC Colors",
            FilterParams::FilterGallery { effects } => match effects.as_slice() {
                [one] => one.filter.name(),
                _ => "Filter Gallery",
            },
        }
    }
}

/// Halos of the second filter batch.
fn halo_ext(p: &FilterParams) -> Halo {
    let r = |v: f32| Halo::Radius(v.max(0.0).ceil() as i32 + 1);
    match p {
        FilterParams::ColorHalftone { max_radius, .. } => r(max_radius * 3.0),
        FilterParams::Crystallize { cell_size, .. } => r(cell_size.max(1.0) * 3.0),
        FilterParams::Pointillize { cell_size, .. } => r(cell_size.max(1.0) * 2.0),
        FilterParams::Facet => r(3.0),
        FilterParams::Fragment => r(4.0),
        FilterParams::Mezzotint { .. } | FilterParams::HsbHsl { .. } | FilterParams::NtscColors => Halo::Radius(0),
        FilterParams::Diffuse { .. } => r(1.0),
        FilterParams::Extrude { size, depth, .. } => r(stylize2::extrude_reach(*depth) + size.clamp(2.0, 255.0) * 3.0 + 1.0),
        FilterParams::OilPaint { stylization, scale, .. } => r(oil::reach(*stylization, *scale)),
        FilterParams::Tiles { .. } => Halo::Bounds,
        FilterParams::TraceContour { .. } => r(1.0),
        FilterParams::Wind { method, .. } => r(stylize2::wind_reach(*method)),
        FilterParams::Displace { .. } | FilterParams::Shear { .. } | FilterParams::ZigZag { .. } | FilterParams::SpinBlur { .. } => Halo::Bounds,
        // Generators and per-pixel renders only need the bounds geometry (always in the context).
        FilterParams::Fibers { .. } | FilterParams::LensFlare { .. } => Halo::Radius(0),
        FilterParams::LightingEffects { .. } => r(1.0),
        FilterParams::ReduceNoise { .. } => r(denoise::reach()),
        FilterParams::SmartBlur { radius, .. } => r(*radius),
        FilterParams::LensBlur { radius, .. } => r(*radius),
        FilterParams::ShapeBlur { radius, .. } => r(*radius),
        FilterParams::TiltShift { blur, .. } => r(gallery::reach(*blur)),
        FilterParams::IrisBlur { pins } => r(gallery::reach(pins.iter().map(|p| p.blur).fold(0.0, f32::max))),
        FilterParams::FieldBlur { pins } => r(gallery::reach(pins.iter().map(|p| p.blur).fold(0.0, f32::max))),
        FilterParams::PathBlur { paths } => r(paths.iter().map(|p| p.speed.abs()).fold(0.0, f32::max) / 2.0 + 1.0),
        FilterParams::Custom { .. } => r(2.0),
        FilterParams::DeInterlace { .. } => r(1.0),
        FilterParams::FilterGallery { effects } => Halo::Radius(artistic_fx::reach(effects)),
        _ => Halo::Radius(0),
    }
}

/// Context a filter runs in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ctx {
    /// Reference area: distortions are centred on it and sample within it
    /// (Photoshop uses the selection bounds, else the canvas).
    pub bounds: Rect,
    /// Colour model of the samples (the last channel is alpha if `alpha`).
    pub mode: ColorMode,
    /// Whether the last channel is alpha.
    pub alpha: bool,
}

/// Runs the filter kernel for `out` from `src` (which covers `out` grown by
/// the halo, or the bounds for global filters). Returns interleaved samples.
pub fn kernel(params: &FilterParams, src: &Image, out: Rect, ctx: &Ctx) -> Vec<f32> {
    match params {
        FilterParams::GaussianBlur { radius } => blur::gaussian(src, out, ctx, *radius),
        FilterParams::BoxBlur { radius } => blur::boxed(src, out, ctx, *radius),
        FilterParams::MotionBlur { angle, distance } => blur::motion(src, out, ctx, *angle, *distance),
        FilterParams::RadialBlur { amount, method, center_x, center_y } => blur::radial(src, out, ctx, *amount, *method, (*center_x, *center_y)),
        FilterParams::SurfaceBlur { radius, threshold } => blur::surface(src, out, ctx, *radius, *threshold),
        FilterParams::UnsharpMask { amount, radius, threshold } => sharpen::unsharp(src, out, ctx, *amount, *radius, *threshold),
        FilterParams::SmartSharpen { amount, radius, reduce_noise } => sharpen::smart(src, out, ctx, *amount, *radius, *reduce_noise),
        FilterParams::HighPass { radius } => sharpen::high_pass(src, out, ctx, *radius),
        FilterParams::AddNoise { amount, distribution, monochromatic, seed } => noise::add(src, out, ctx, *amount, *distribution, *monochromatic, *seed),
        FilterParams::Median { radius } => noise::median(src, out, *radius, None),
        FilterParams::DustAndScratches { radius, threshold } => noise::median(src, out, *radius, Some(*threshold)),
        FilterParams::Minimum { radius, preserve } => other::min_max_preserve(src, out, *radius, false, *preserve),
        FilterParams::Maximum { radius, preserve } => other::min_max_preserve(src, out, *radius, true, *preserve),
        FilterParams::Offset { horizontal, vertical, undefined } => other::offset(src, out, ctx, *horizontal, *vertical, *undefined),
        FilterParams::Mosaic { cell_size } => stylize::mosaic(src, out, ctx, *cell_size),
        FilterParams::Emboss { angle, height, amount } => stylize::emboss(src, out, ctx, *angle, *height, *amount),
        FilterParams::FindEdges => stylize::find_edges(src, out, ctx),
        FilterParams::Solarize => stylize::per_pixel(src, out, ctx, |v| if v > 0.5 { 1.0 - v } else { v }),
        FilterParams::Invert => stylize::per_pixel(src, out, ctx, |v| 1.0 - v),
        FilterParams::Desaturate => stylize::desaturate(src, out, ctx),
        FilterParams::Twirl { angle } => distort::twirl(src, out, ctx, *angle),
        FilterParams::Pinch { amount } => distort::pinch(src, out, ctx, *amount),
        FilterParams::Spherize { amount, mode } => distort::spherize(src, out, ctx, *amount, *mode),
        FilterParams::Wave { generators, wavelength_min, wavelength_max, amplitude_min, amplitude_max, wave_type, undefined, seed } => distort::wave(
            src,
            out,
            ctx,
            distort::WaveSpec {
                generators: *generators,
                wavelength: (*wavelength_min, *wavelength_max),
                amplitude: (*amplitude_min, *amplitude_max),
                wave_type: *wave_type,
                undefined: *undefined,
                seed: *seed,
            },
        ),
        FilterParams::Ripple { amount, size } => distort::ripple(src, out, ctx, *amount, *size),
        FilterParams::PolarCoordinates { mode } => distort::polar(src, out, ctx, *mode),
        FilterParams::ColorHalftone { max_radius, angles } => pixelate::color_halftone(src, out, ctx, *max_radius, *angles),
        FilterParams::Crystallize { cell_size, seed } => pixelate::crystallize(src, out, ctx, *cell_size, *seed),
        FilterParams::Facet => pixelate::facet(src, out, ctx),
        FilterParams::Fragment => pixelate::fragment(src, out, ctx),
        FilterParams::Mezzotint { kind, seed } => pixelate::mezzotint(src, out, ctx, *kind, *seed),
        FilterParams::Pointillize { cell_size, seed, background } => pixelate::pointillize(src, out, ctx, *cell_size, *seed, *background),
        FilterParams::Diffuse { mode, seed } => stylize2::diffuse(src, out, ctx, *mode, *seed),
        FilterParams::Extrude { kind, size, depth, level_based, solid_front, mask_incomplete, seed } => stylize2::extrude(
            src,
            out,
            ctx,
            &stylize2::ExtrudeSpec {
                kind: *kind,
                size: *size,
                depth: *depth,
                level_based: *level_based,
                solid_front: *solid_front,
                mask_incomplete: *mask_incomplete,
                seed: *seed,
            },
        ),
        FilterParams::OilPaint { stylization, cleanliness, scale, bristle_detail, lighting, angle, shine } => oil::oil_paint(
            src,
            out,
            ctx,
            &oil::OilSpec {
                stylization: *stylization,
                cleanliness: *cleanliness,
                scale: *scale,
                bristle_detail: *bristle_detail,
                lighting: *lighting,
                angle: *angle,
                shine: *shine,
            },
        ),
        FilterParams::Tiles { count, max_offset, fill, foreground, background, seed } => {
            stylize2::tiles(src, out, ctx, *count, *max_offset, *fill, (*foreground, *background), *seed)
        }
        FilterParams::TraceContour { level, upper } => stylize2::trace_contour(src, out, ctx, *level, *upper),
        FilterParams::Wind { method, from_right, seed } => stylize2::wind(src, out, ctx, *method, *from_right, *seed),
        FilterParams::Displace { horizontal, vertical, stretch, undefined, map } => {
            distort2::displace(src, out, ctx, (*horizontal, *vertical), *stretch, *undefined, map.as_deref())
        }
        FilterParams::Shear { points, undefined } => distort2::shear(src, out, ctx, points, *undefined),
        FilterParams::ZigZag { amount, ridges, style } => distort2::zigzag(src, out, ctx, *amount, *ridges, *style),
        FilterParams::Fibers { variance, strength, seed, foreground, background } => {
            render::fibers(src, out, ctx, *variance, *strength, *seed, (*foreground, *background))
        }
        FilterParams::LensFlare { brightness, center_x, center_y, lens } => render::lens_flare(src, out, ctx, *brightness, (*center_x, *center_y), *lens),
        FilterParams::LightingEffects { lights, gloss, metallic, exposure, ambience, texture, height, white_is_high } => render::lighting(
            src,
            out,
            ctx,
            &render::LightingSpec {
                lights,
                gloss: *gloss,
                metallic: *metallic,
                exposure: *exposure,
                ambience: *ambience,
                texture: *texture,
                height: *height,
                white_is_high: *white_is_high,
            },
        ),
        FilterParams::ReduceNoise { strength, preserve_details, reduce_color_noise, sharpen_details, remove_jpeg_artifact } => denoise::reduce_noise(
            src,
            out,
            ctx,
            &denoise::DenoiseSpec {
                strength: *strength,
                preserve: *preserve_details,
                color: *reduce_color_noise,
                sharpen: *sharpen_details,
                jpeg: *remove_jpeg_artifact,
            },
        ),
        FilterParams::SmartBlur { radius, threshold, quality, mode } => blur2::smart_blur(src, out, ctx, *radius, *threshold, *quality, *mode),
        FilterParams::LensBlur {
            radius,
            blades,
            curvature,
            rotation,
            depth,
            focal_distance,
            invert_depth,
            brightness,
            threshold,
            noise,
            distribution,
            monochromatic,
            seed,
            depth_map,
        } => blur2::lens_blur(
            src,
            out,
            ctx,
            &blur2::LensSpec {
                radius: *radius,
                blades: *blades,
                curvature: *curvature,
                rotation: *rotation,
                depth: *depth,
                focal: *focal_distance,
                invert: *invert_depth,
                brightness: *brightness,
                threshold: *threshold,
                noise: *noise,
                distribution: *distribution,
                mono: *monochromatic,
                seed: *seed,
                depth_map: depth_map.as_deref(),
            },
        ),
        FilterParams::ShapeBlur { radius, shape } => blur2::shape_blur(src, out, ctx, *radius, *shape),
        FilterParams::TiltShift { blur, center_x, center_y, angle, focus, transition } => {
            gallery::tilt_shift(src, out, ctx, *blur, (*center_x, *center_y), *angle, *focus, *transition)
        }
        FilterParams::IrisBlur { pins } => gallery::iris(src, out, ctx, pins),
        FilterParams::FieldBlur { pins } => gallery::field(src, out, ctx, pins),
        FilterParams::SpinBlur { pins } => gallery::spin(src, out, ctx, pins),
        FilterParams::PathBlur { paths } => gallery::path(src, out, ctx, paths),
        FilterParams::Custom { kernel, scale, offset } => other2::custom(src, out, ctx, kernel, *scale, *offset),
        FilterParams::HsbHsl { input, output } => other2::hsb_hsl(src, out, ctx, *input, *output),
        FilterParams::DeInterlace { eliminate_even, interpolate } => video::deinterlace(src, out, ctx, *eliminate_even, *interpolate),
        FilterParams::NtscColors => video::ntsc(src, out, ctx),
        FilterParams::FilterGallery { effects } => artistic_fx::run(effects, src, out, ctx),
    }
}

/// Output tile size used by [`apply`].
pub const TILE: i32 = 256;

/// The area a filter writes for a layer whose pixels cover `content`:
/// global filters write the reference bounds; others the content grown by
/// the halo (blurs spread into transparent areas). Clipped to the
/// selection's bounds when there is one.
pub fn output_area(params: &FilterParams, content: Rect, bounds: Rect, selection_bounds: Option<Rect>) -> Rect {
    let mut area = match params.halo() {
        Halo::Bounds => bounds,
        Halo::Radius(r) => {
            if content.is_empty() {
                Rect::EMPTY
            } else {
                content.inflate(r)
            }
        }
    };
    if let Some(sb) = selection_bounds {
        area = area.intersect(&sb);
    }
    area
}

/// Applies a filter to `area` of `surface`, mixing with the original by the
/// selection coverage (channel 0 of `selection`). Returns the new surface.
pub fn apply(surface: &Surface, params: &FilterParams, area: Rect, bounds: Rect, selection: Option<&Surface>) -> Surface {
    apply_tiled(surface, params, area, bounds, selection, auto_tile(params), None)
}

/// [`apply`] for a layer in a document: neighbourhood filters repeat the edge pixels of `extent`
/// (the canvas plus any off-canvas pixels the layer has) instead of reading transparency beyond
/// it, and the output is clipped to `extent`, as Photoshop does at the canvas edge.
pub fn apply_in(surface: &Surface, params: &FilterParams, area: Rect, bounds: Rect, selection: Option<&Surface>, extent: Rect) -> Surface {
    apply_tiled(surface, params, area.intersect(&extent), bounds, selection, auto_tile(params), Some(extent))
}

/// [`apply_in`] that can be cancelled (checked before each tile) and reports progress per tile
/// group. `None` when `ctl` was cancelled; the input surface is never modified.
pub fn apply_in_with(
    surface: &Surface,
    params: &FilterParams,
    area: Rect,
    bounds: Rect,
    selection: Option<&Surface>,
    extent: Rect,
    ctl: &photocraft_raster::Interrupt,
) -> Option<Surface> {
    apply_tiled_with(surface, params, area.intersect(&extent), bounds, selection, auto_tile(params), Some(extent), ctl)
}

/// Results do not depend on the tiling, so wide-halo filters use bigger tiles to keep the
/// re-read margin (and its cost) below ~2× the tile area.
fn auto_tile(params: &FilterParams) -> i32 {
    match params.halo() {
        Halo::Radius(r) => TILE.max((2 * r + 63) / 64 * 64).min(2048),
        Halo::Bounds => TILE,
    }
}

/// [`apply`] / [`apply_in`] with an explicit tile size (tests use it to check tile independence).
pub fn apply_tiled(
    surface: &Surface,
    params: &FilterParams,
    area: Rect,
    bounds: Rect,
    selection: Option<&Surface>,
    tile: i32,
    extent: Option<Rect>,
) -> Surface {
    // Never cancelled, so always `Some`; the fallback (the input unchanged) is unreachable.
    apply_tiled_with(surface, params, area, bounds, selection, tile, extent, &photocraft_raster::Interrupt::NONE).unwrap_or_else(|| surface.clone())
}

/// [`apply_tiled`] with cancellation (checked before each tile, so a cancel takes effect within
/// one tile's work) and progress (after each group of tiles). `None` when cancelled.
#[allow(clippy::too_many_arguments)]
pub fn apply_tiled_with(
    surface: &Surface,
    params: &FilterParams,
    area: Rect,
    bounds: Rect,
    selection: Option<&Surface>,
    tile: i32,
    extent: Option<Rect>,
    ctl: &photocraft_raster::Interrupt,
) -> Option<Surface> {
    let mut out = surface.clone();
    if area.is_empty() {
        return Some(out);
    }
    let fmt = surface.format();
    let ctx = Ctx { bounds, mode: fmt.mode, alpha: fmt.alpha };
    let halo = params.halo();
    let shared = (halo == Halo::Bounds).then(|| Image::read(surface, bounds.union(&area)));
    let mut tiles = Vec::new();
    let mut y = area.y0;
    while y < area.y1 {
        let mut x = area.x0;
        while x < area.x1 {
            tiles.push(Rect::new(x, y, (x + tile).min(area.x1), (y + tile).min(area.y1)));
            x += tile;
        }
        y += tile;
    }
    // Tiles finished so far, for progress reported per tile (from any worker thread).
    let finished = std::sync::atomic::AtomicUsize::new(0);
    let total = tiles.len().max(1);
    let run = |t: &Rect| -> (Rect, Vec<f32>) {
        // Cancelled: skip the remaining tiles of the group (the result is discarded).
        if ctl.cancelled() {
            return (*t, Vec::new());
        }
        let tick = || {
            let n = finished.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // The last few percent are the writes after each group.
            ctl.progress(0.98 * n as f32 / total as f32);
        };
        let owned;
        let src = match (&shared, halo) {
            (Some(s), _) => s,
            (None, Halo::Radius(r)) => {
                owned = match extent {
                    Some(e) => Image::read_clamped(surface, t.inflate(r), e),
                    None => Image::read(surface, t.inflate(r)),
                };
                &owned
            }
            // `shared` is always read for global filters; read the same image if it weren't.
            (None, Halo::Bounds) => {
                owned = Image::read(surface, bounds.union(&area));
                &owned
            }
        };
        let mut data = kernel(params, src, *t, &ctx);
        if let Some(sel) = selection {
            let n = src.ch;
            let w = t.width() as usize;
            for (i, px) in data.chunks_exact_mut(n).enumerate() {
                let (xx, yy) = (t.x0 + (i % w) as i32, t.y0 + (i / w) as i32);
                let k = sel.pixel(xx, yy)[0].clamp(0.0, 1.0);
                if k >= 1.0 {
                    continue;
                }
                for (c, v) in px.iter_mut().enumerate() {
                    let o = src.get(xx, yy, c);
                    *v = o + (*v - o) * k;
                }
            }
        }
        tick();
        (*t, data)
    };
    // Tiles are filtered in groups of about RESULT_BUDGET bytes (at least one per core) and each
    // group is written before the next starts, so a huge layer never holds all its results as
    // floats at once.
    #[cfg(not(target_arch = "wasm32"))]
    let threads = rayon::current_num_threads();
    #[cfg(target_arch = "wasm32")]
    let threads = 1;
    let per_tile = (tile.max(1) as usize).pow(2) * fmt.channels() * std::mem::size_of::<f32>();
    let group = (RESULT_BUDGET / per_tile.max(1)).max(threads).max(1);
    for chunk in tiles.chunks(group) {
        if ctl.cancelled() {
            return None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let results: Vec<(Rect, Vec<f32>)> = {
            use rayon::prelude::*;
            chunk.par_iter().map(run).collect()
        };
        #[cfg(target_arch = "wasm32")]
        let results: Vec<(Rect, Vec<f32>)> = chunk.iter().map(run).collect();
        if ctl.cancelled() {
            return None;
        }
        for (t, data) in results {
            out.write_region(t, &data);
        }
    }
    out.prune();
    ctl.progress(1.0);
    Some(out)
}

/// Bytes of float filter results held at once by [`apply_tiled`].
const RESULT_BUDGET: usize = 256 << 20;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ext;

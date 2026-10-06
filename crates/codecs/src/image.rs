//! The crate's own flat raster image type.
//!
//! Samples are stored interleaved (`RGBARGBA…`), row-major, top-to-bottom,
//! in **native endianness**. Integer samples are unsigned and normalized to
//! `[0, max]`; float samples are nominally `[0, 1]` but may exceed it (HDR).

use crate::error::CodecError;
use half::f16;

/// Channel layout of an [`Image`]. Alpha, when present, is always the last
/// channel and is *straight* (not premultiplied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    Gray,
    GrayA,
    Rgb,
    Rgba,
    /// Ink amounts, `0 = no ink`, `max = full ink` (not Adobe-inverted).
    Cmyk,
    CmykA,
}

impl ChannelLayout {
    pub const ALL: [ChannelLayout; 6] =
        [ChannelLayout::Gray, ChannelLayout::GrayA, ChannelLayout::Rgb, ChannelLayout::Rgba, ChannelLayout::Cmyk, ChannelLayout::CmykA];

    /// Total number of channels including alpha.
    pub const fn channels(self) -> usize {
        match self {
            ChannelLayout::Gray => 1,
            ChannelLayout::GrayA => 2,
            ChannelLayout::Rgb => 3,
            ChannelLayout::Rgba | ChannelLayout::Cmyk => 4,
            ChannelLayout::CmykA => 5,
        }
    }

    /// Number of colour (non-alpha) channels.
    pub const fn color_channels(self) -> usize {
        self.channels() - self.has_alpha() as usize
    }

    pub const fn has_alpha(self) -> bool {
        matches!(self, ChannelLayout::GrayA | ChannelLayout::Rgba | ChannelLayout::CmykA)
    }

    pub const fn is_gray(self) -> bool {
        matches!(self, ChannelLayout::Gray | ChannelLayout::GrayA)
    }

    pub const fn is_rgb(self) -> bool {
        matches!(self, ChannelLayout::Rgb | ChannelLayout::Rgba)
    }

    pub const fn is_cmyk(self) -> bool {
        matches!(self, ChannelLayout::Cmyk | ChannelLayout::CmykA)
    }

    /// The same colour model with alpha added.
    pub const fn with_alpha(self) -> Self {
        match self {
            ChannelLayout::Gray | ChannelLayout::GrayA => ChannelLayout::GrayA,
            ChannelLayout::Rgb | ChannelLayout::Rgba => ChannelLayout::Rgba,
            ChannelLayout::Cmyk | ChannelLayout::CmykA => ChannelLayout::CmykA,
        }
    }

    /// The same colour model without alpha.
    pub const fn without_alpha(self) -> Self {
        match self {
            ChannelLayout::Gray | ChannelLayout::GrayA => ChannelLayout::Gray,
            ChannelLayout::Rgb | ChannelLayout::Rgba => ChannelLayout::Rgb,
            ChannelLayout::Cmyk | ChannelLayout::CmykA => ChannelLayout::Cmyk,
        }
    }

    /// `true` if both layouts share a colour model (ignoring alpha).
    pub const fn same_model(self, other: Self) -> bool {
        (self.is_gray() && other.is_gray()) || (self.is_rgb() && other.is_rgb()) || (self.is_cmyk() && other.is_cmyk())
    }
}

/// Storage type of one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SampleType {
    U8,
    U16,
    /// IEEE 754 half float, stored as its 16 bit pattern.
    F16,
    F32,
}

impl SampleType {
    pub const ALL: [SampleType; 4] = [SampleType::U8, SampleType::U16, SampleType::F16, SampleType::F32];

    pub const fn bytes(self) -> usize {
        match self {
            SampleType::U8 => 1,
            SampleType::U16 | SampleType::F16 => 2,
            SampleType::F32 => 4,
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, SampleType::F16 | SampleType::F32)
    }
}

/// Basic, format-independent metadata.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// Raw EXIF payload in TIFF structure (starting with `II*\0`/`MM\0*`),
    /// **without** the JPEG `Exif\0\0` prefix.
    pub exif: Option<Vec<u8>>,
    /// XMP packet as UTF-8.
    pub xmp: Option<String>,
    /// Horizontal and vertical resolution in dots per inch.
    pub dpi: Option<(f32, f32)>,
    /// Free-form key/value text (PNG tEXt/iTXt, TIFF ASCII tags).
    pub text: Vec<(String, String)>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none() && self.dpi.is_none() && self.text.is_empty()
    }
}

/// A single flat raster image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    layout: ChannelLayout,
    sample: SampleType,
    data: Vec<u8>,
    /// Embedded ICC profile (must describe `layout`'s colour model).
    pub icc: Option<Vec<u8>>,
    pub meta: Metadata,
}

fn byte_len(width: u32, height: u32, layout: ChannelLayout, sample: SampleType) -> Option<usize> {
    (width as usize).checked_mul(height as usize)?.checked_mul(layout.channels())?.checked_mul(sample.bytes())
}

impl Image {
    /// A zero-filled image, or an error if the byte size overflows `usize`.
    pub fn new(width: u32, height: u32, layout: ChannelLayout, sample: SampleType) -> Result<Self, CodecError> {
        let len = byte_len(width, height, layout, sample).ok_or_else(|| CodecError::InvalidImage("image size overflows usize".into()))?;
        Ok(Image { width, height, layout, sample, data: vec![0; len], icc: None, meta: Metadata::default() })
    }

    /// Wrap raw interleaved, native-endian bytes.
    pub fn from_raw(width: u32, height: u32, layout: ChannelLayout, sample: SampleType, data: Vec<u8>) -> Result<Self, CodecError> {
        let expected = byte_len(width, height, layout, sample).ok_or_else(|| CodecError::InvalidImage("image size overflows usize".into()))?;
        if data.len() != expected {
            return Err(CodecError::InvalidImage(format!("buffer has {} bytes, expected {expected} for {width}x{height} {layout:?} {sample:?}", data.len())));
        }
        Ok(Image { width, height, layout, sample, data, icc: None, meta: Metadata::default() })
    }

    pub fn from_u8(width: u32, height: u32, layout: ChannelLayout, data: Vec<u8>) -> Result<Self, CodecError> {
        Self::from_raw(width, height, layout, SampleType::U8, data)
    }

    pub fn from_u16(width: u32, height: u32, layout: ChannelLayout, data: &[u16]) -> Result<Self, CodecError> {
        let bytes = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
        Self::from_raw(width, height, layout, SampleType::U16, bytes)
    }

    pub fn from_f16(width: u32, height: u32, layout: ChannelLayout, data: &[f16]) -> Result<Self, CodecError> {
        let bytes = data.iter().flat_map(|v| v.to_bits().to_ne_bytes()).collect();
        Self::from_raw(width, height, layout, SampleType::F16, bytes)
    }

    pub fn from_f32(width: u32, height: u32, layout: ChannelLayout, data: &[f32]) -> Result<Self, CodecError> {
        let bytes = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
        Self::from_raw(width, height, layout, SampleType::F32, bytes)
    }

    /// Build an image from normalized f32 samples (`[0,1]` for integer
    /// targets; values are clamped and rounded when quantizing).
    pub fn from_normalized(width: u32, height: u32, layout: ChannelLayout, sample: SampleType, values: &[f32]) -> Result<Self, CodecError> {
        let data = quantize(values, sample);
        Self::from_raw(width, height, layout, sample, data)
    }

    pub fn with_icc(mut self, icc: Option<Vec<u8>>) -> Self {
        self.icc = icc;
        self
    }

    pub fn with_meta(mut self, meta: Metadata) -> Self {
        self.meta = meta;
        self
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn layout(&self) -> ChannelLayout {
        self.layout
    }
    pub fn sample_type(&self) -> SampleType {
        self.sample
    }
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }
    /// Number of samples (pixels × channels).
    pub fn sample_count(&self) -> usize {
        self.pixel_count() * self.layout.channels()
    }
    /// Raw interleaved native-endian bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// Borrow samples as `u8` (only when the sample type is `U8`).
    pub fn as_u8(&self) -> Option<&[u8]> {
        (self.sample == SampleType::U8).then_some(&self.data[..])
    }

    /// Copy samples out as `u16` (only when the sample type is `U16`).
    pub fn to_u16_samples(&self) -> Option<Vec<u16>> {
        (self.sample == SampleType::U16).then(|| self.data.as_chunks::<2>().0.iter().map(|c| u16::from_ne_bytes([c[0], c[1]])).collect())
    }

    /// Copy samples out as `f16` (only when the sample type is `F16`).
    pub fn to_f16_samples(&self) -> Option<Vec<f16>> {
        (self.sample == SampleType::F16).then(|| self.data.as_chunks::<2>().0.iter().map(|c| f16::from_bits(u16::from_ne_bytes([c[0], c[1]]))).collect())
    }

    /// Copy samples out as `f32` (only when the sample type is `F32`).
    pub fn to_f32_samples(&self) -> Option<Vec<f32>> {
        (self.sample == SampleType::F32).then(|| self.data.as_chunks::<4>().0.iter().map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    /// Sample `index` (in samples, not bytes) as a normalized f32.
    pub fn sample_normalized(&self, index: usize) -> f32 {
        read_normalized(&self.data, self.sample, index)
    }

    /// Normalized f32 of channel `c` at pixel `(x, y)`.
    pub fn get(&self, x: u32, y: u32, c: usize) -> f32 {
        let idx = (y as usize * self.width as usize + x as usize) * self.layout.channels() + c;
        self.sample_normalized(idx)
    }

    /// All samples as normalized f32 (integers mapped to `[0,1]`).
    pub fn to_normalized(&self) -> Vec<f32> {
        to_normalized(&self.data, self.sample)
    }

    /// `true` when the image has alpha and at least one pixel is not fully
    /// opaque.
    pub fn has_translucency(&self) -> bool {
        if !self.layout.has_alpha() {
            return false;
        }
        let ch = self.layout.channels();
        (0..self.pixel_count()).any(|p| self.sample_normalized(p * ch + ch - 1) < 1.0)
    }

    /// `true` when this is a float image with any sample outside `[0, 1]`
    /// (or NaN).
    pub fn has_out_of_range(&self) -> bool {
        if !self.sample.is_float() {
            return false;
        }
        (0..self.sample_count()).any(|i| {
            let v = self.sample_normalized(i);
            !(0.0..=1.0).contains(&v)
        })
    }

    /// Convert to another layout and/or sample type. The ICC profile is kept
    /// only if the colour model is unchanged (an RGB profile on CMYK data
    /// would be wrong). Metadata is kept.
    pub fn convert(&self, layout: ChannelLayout, sample: SampleType) -> Image {
        if layout == self.layout && sample == self.sample {
            return self.clone();
        }
        let data = if sample == self.sample && !self.layout.has_alpha() && layout == self.layout.with_alpha() {
            // Fast path (opening any opaque file): copy the samples, append an opaque alpha.
            let bps = sample.bytes();
            let src = self.layout.channels() * bps;
            let one: Vec<u8> = match sample {
                SampleType::U8 => vec![u8::MAX],
                SampleType::U16 => u16::MAX.to_ne_bytes().to_vec(),
                SampleType::F16 => f16::ONE.to_bits().to_ne_bytes().to_vec(),
                SampleType::F32 => 1.0f32.to_ne_bytes().to_vec(),
            };
            let mut out = Vec::with_capacity(self.data.len() / src * (src + bps));
            for px in self.data.chunks_exact(src) {
                out.extend_from_slice(px);
                out.extend_from_slice(&one);
            }
            out
        } else if layout == self.layout {
            quantize(&self.to_normalized(), sample)
        } else {
            let src = self.to_normalized();
            let values = convert_layout(&src, self.layout, layout);
            quantize(&values, sample)
        };
        Image {
            width: self.width,
            height: self.height,
            layout,
            sample,
            data,
            icc: if layout.same_model(self.layout) { self.icc.clone() } else { None },
            meta: self.meta.clone(),
        }
    }

    /// [`Image::convert`], borrowing `self` when it already has that layout and sample type
    /// (encoders use it so a full-size image isn't copied for nothing).
    pub fn converted(&self, layout: ChannelLayout, sample: SampleType) -> std::borrow::Cow<'_, Image> {
        if layout == self.layout && sample == self.sample { std::borrow::Cow::Borrowed(self) } else { std::borrow::Cow::Owned(self.convert(layout, sample)) }
    }

    /// Interleaved RGBA, 8 bits per sample.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.convert(ChannelLayout::Rgba, SampleType::U8).data
    }

    /// Interleaved RGBA, 16 bits per sample.
    pub fn to_rgba16(&self) -> Vec<u16> {
        self.convert(ChannelLayout::Rgba, SampleType::U16).to_u16_samples().unwrap_or_default()
    }

    /// Interleaved RGBA as f32 (integer inputs normalized to `[0,1]`).
    pub fn to_rgba_f32(&self) -> Vec<f32> {
        convert_layout(&self.to_normalized(), self.layout, ChannelLayout::Rgba)
    }
}

// ---------------------------------------------------------------------------
// Sample conversion helpers
// ---------------------------------------------------------------------------

#[inline]
pub(crate) fn read_normalized(data: &[u8], sample: SampleType, i: usize) -> f32 {
    match sample {
        SampleType::U8 => data[i] as f32 / 255.0,
        SampleType::U16 => u16::from_ne_bytes([data[2 * i], data[2 * i + 1]]) as f32 / 65535.0,
        SampleType::F16 => f16::from_bits(u16::from_ne_bytes([data[2 * i], data[2 * i + 1]])).to_f32(),
        SampleType::F32 => f32::from_ne_bytes([data[4 * i], data[4 * i + 1], data[4 * i + 2], data[4 * i + 3]]),
    }
}

pub(crate) fn to_normalized(data: &[u8], sample: SampleType) -> Vec<f32> {
    match sample {
        SampleType::U8 => data.iter().map(|&v| v as f32 / 255.0).collect(),
        SampleType::U16 => data.as_chunks::<2>().0.iter().map(|c| u16::from_ne_bytes([c[0], c[1]]) as f32 / 65535.0).collect(),
        SampleType::F16 => data.as_chunks::<2>().0.iter().map(|c| f16::from_bits(u16::from_ne_bytes([c[0], c[1]])).to_f32()).collect(),
        SampleType::F32 => data.as_chunks::<4>().0.iter().map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect(),
    }
}

#[inline]
fn clamp01(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

pub(crate) fn quantize(values: &[f32], sample: SampleType) -> Vec<u8> {
    match sample {
        SampleType::U8 => values.iter().map(|&v| (clamp01(v) * 255.0).round() as u8).collect(),
        SampleType::U16 => values.iter().flat_map(|&v| ((clamp01(v) * 65535.0).round() as u16).to_ne_bytes()).collect(),
        SampleType::F16 => values.iter().flat_map(|&v| f16::from_f32(v).to_bits().to_ne_bytes()).collect(),
        SampleType::F32 => values.iter().flat_map(|&v| v.to_ne_bytes()).collect(),
    }
}

/// Rec. 709 luma weights applied to encoded values.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
}

fn rgb_to_cmyk(r: f32, g: f32, b: f32) -> [f32; 4] {
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let d = 1.0 - k;
    [(1.0 - r - k) / d, (1.0 - g - k) / d, (1.0 - b - k) / d, k]
}

/// Convert normalized interleaved samples between layouts. Naive
/// (non-colour-managed) formulas; colour-managed conversion belongs in
/// `photocraft-color`.
pub(crate) fn convert_layout(src: &[f32], from: ChannelLayout, to: ChannelLayout) -> Vec<f32> {
    if from == to {
        return src.to_vec();
    }
    let fc = from.channels();
    let tc = to.channels();
    let n = src.len() / fc;
    let mut out = Vec::with_capacity(n * tc);
    for px in src.chunks_exact(fc) {
        let alpha = if from.has_alpha() { px[fc - 1] } else { 1.0 };
        let color = &px[..from.color_channels()];
        if from.same_model(to) {
            out.extend_from_slice(color);
        } else {
            let rgb = match from {
                ChannelLayout::Gray | ChannelLayout::GrayA => [color[0]; 3],
                ChannelLayout::Rgb | ChannelLayout::Rgba => [color[0], color[1], color[2]],
                ChannelLayout::Cmyk | ChannelLayout::CmykA => cmyk_to_rgb(color[0], color[1], color[2], color[3]),
            };
            match to {
                ChannelLayout::Gray | ChannelLayout::GrayA => {
                    out.push(rgb[0] * LUMA[0] + rgb[1] * LUMA[1] + rgb[2] * LUMA[2]);
                }
                ChannelLayout::Rgb | ChannelLayout::Rgba => out.extend_from_slice(&rgb),
                ChannelLayout::Cmyk | ChannelLayout::CmykA => out.extend_from_slice(&rgb_to_cmyk(rgb[0], rgb[1], rgb[2])),
            }
        }
        if to.has_alpha() {
            out.push(alpha);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn adding_alpha_fast_path_matches_the_general_conversion() {
        for sample in SampleType::ALL {
            for layout in [ChannelLayout::Gray, ChannelLayout::Rgb, ChannelLayout::Cmyk] {
                let n = 7 * 3 * layout.channels();
                let vals: Vec<f32> = (0..n).map(|i| (i as f32 * 0.137) % 1.0).collect();
                let img = super::Image::from_normalized(7, 3, layout, sample, &vals).unwrap();
                let fast = img.convert(layout.with_alpha(), sample);
                let slow = super::quantize(&super::convert_layout(&img.to_normalized(), layout, layout.with_alpha()), sample);
                assert_eq!(fast.data(), &slow[..], "{layout:?} {sample:?}");
            }
        }
    }

    use super::*;

    #[test]
    fn u8_u16_roundtrip_exact() {
        let v: Vec<u8> = (0..=255).collect();
        let img = Image::from_u8(256, 1, ChannelLayout::Gray, v.clone()).unwrap();
        let back = img.convert(ChannelLayout::Gray, SampleType::U16).convert(ChannelLayout::Gray, SampleType::U8);
        assert_eq!(back.data(), &v[..]);
    }

    #[test]
    fn u16_f32_roundtrip_exact() {
        let v: Vec<u16> = (0..=65535u32).step_by(7).map(|x| x as u16).collect();
        let img = Image::from_u16(v.len() as u32, 1, ChannelLayout::Gray, &v).unwrap();
        let back = img.convert(ChannelLayout::Gray, SampleType::F32).convert(ChannelLayout::Gray, SampleType::U16);
        assert_eq!(back.to_u16_samples().unwrap(), v);
    }

    #[test]
    fn icc_dropped_on_model_change() {
        let img = Image::new(2, 2, ChannelLayout::Cmyk, SampleType::U8).unwrap().with_icc(Some(vec![1, 2, 3]));
        assert!(img.convert(ChannelLayout::Rgb, SampleType::U8).icc.is_none());
        assert!(img.convert(ChannelLayout::CmykA, SampleType::U16).icc.is_some());
    }

    #[test]
    fn from_raw_rejects_bad_len() {
        assert!(Image::from_raw(2, 2, ChannelLayout::Rgb, SampleType::U8, vec![0; 11]).is_err());
    }

    #[test]
    fn rgba8_of_gray() {
        let img = Image::from_u8(1, 1, ChannelLayout::GrayA, vec![10, 20]).unwrap();
        assert_eq!(img.to_rgba8(), vec![10, 10, 10, 20]);
    }

    #[test]
    fn cmyk_rgb_basic() {
        let img = Image::from_u8(1, 1, ChannelLayout::Cmyk, vec![0, 0, 0, 255]).unwrap();
        assert_eq!(img.to_rgba8(), vec![0, 0, 0, 255]);
        let img = Image::from_u8(1, 1, ChannelLayout::Rgb, vec![255, 0, 0]).unwrap();
        let c = img.convert(ChannelLayout::Cmyk, SampleType::U8);
        assert_eq!(c.data(), &[0, 255, 255, 0]);
    }
}

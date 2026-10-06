//! Decode limits and encode options.

use crate::error::CodecError;
use crate::image::{ChannelLayout, SampleType};

/// Decompression-bomb guards. All checks happen on header values *before*
/// the pixel buffer is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_width: u32,
    pub max_height: u32,
    /// Maximum `width * height`.
    pub max_pixels: u64,
    /// Maximum bytes for the decoded pixel buffer (and passed on as the
    /// allocation budget to underlying decoders).
    pub max_alloc: u64,
}

impl Default for Limits {
    fn default() -> Self {
        // 2^30 pixels (e.g. 32768²) and, on 64-bit targets, 8 GiB: enough for the large
        // documents Photoshop users open (a 20000² RGBA 16-bit image is 3.2 GB).
        let max_alloc = if cfg!(target_pointer_width = "64") { 8 << 30 } else { 2 << 30 };
        Limits { max_width: 1 << 18, max_height: 1 << 18, max_pixels: 1 << 30, max_alloc }
    }
}

impl Limits {
    /// Effectively unlimited (still bounded by `usize` overflow checks).
    pub fn none() -> Self {
        Limits { max_width: u32::MAX, max_height: u32::MAX, max_pixels: u64::MAX, max_alloc: u64::MAX }
    }

    /// Validate declared dimensions for an output buffer of the given layout
    /// and sample type.
    pub fn check(&self, width: u32, height: u32, layout: ChannelLayout, sample: SampleType) -> Result<(), CodecError> {
        self.check_bytes(width, height, (layout.channels() * sample.bytes()) as u64)
    }

    pub(crate) fn check_bytes(&self, width: u32, height: u32, bytes_per_pixel: u64) -> Result<(), CodecError> {
        if width == 0 || height == 0 {
            return Err(CodecError::InvalidImage(format!("zero-sized image {width}x{height}")));
        }
        if width > self.max_width || height > self.max_height {
            return Err(CodecError::LimitExceeded(format!("dimensions {width}x{height} exceed max {}x{}", self.max_width, self.max_height)));
        }
        let pixels = width as u64 * height as u64;
        if pixels > self.max_pixels {
            return Err(CodecError::LimitExceeded(format!("{pixels} pixels exceed max {}", self.max_pixels)));
        }
        let bytes = pixels.saturating_mul(bytes_per_pixel);
        if bytes > self.max_alloc {
            return Err(CodecError::LimitExceeded(format!("{bytes} bytes exceed max_alloc {}", self.max_alloc)));
        }
        if usize::try_from(bytes).is_err() {
            return Err(CodecError::LimitExceeded(format!("{bytes} bytes do not fit in memory")));
        }
        Ok(())
    }

    pub(crate) fn alloc_usize(&self) -> usize {
        usize::try_from(self.max_alloc).unwrap_or(usize::MAX)
    }
}

/// Options for [`crate::decode_with`].
#[derive(Debug, Clone, Default)]
pub struct DecodeOptions {
    pub limits: Limits,
    /// Keep the pixels as stored instead of applying the EXIF / TIFF
    /// orientation (by default they are turned upright and the metadata's
    /// orientation is rewritten to 1).
    pub keep_orientation: bool,
}

/// PNG zlib effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngCompression {
    None,
    Fast,
    #[default]
    Default,
    Best,
}

/// TIFF strip compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TiffCompression {
    None,
    Lzw,
    #[default]
    Deflate,
    PackBits,
}

/// OpenEXR compression (lossless methods only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExrCompression {
    None,
    Rle,
    Zip1,
    #[default]
    Zip16,
    Piz,
}

/// Options for [`crate::encode`].
#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// JPEG quality 1..=100 (also AVIF quality when enabled).
    pub jpeg_quality: u8,
    /// JPEG 4:2:0 chroma subsampling (`false` = 4:4:4).
    pub jpeg_chroma_subsampling: bool,
    pub png_compression: PngCompression,
    /// Write Adam7-interlaced PNG.
    pub png_interlaced: bool,
    /// WebP: only lossless encoding is available in pure Rust; `false`
    /// yields [`CodecError::Unsupported`].
    pub webp_lossless: bool,
    pub tiff_compression: TiffCompression,
    pub exr_compression: ExrCompression,
    /// Embed the ICC profile when the format supports it.
    pub embed_icc: bool,
    /// Embed EXIF/XMP/DPI/text when the format supports it.
    pub embed_metadata: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            jpeg_quality: 90,
            jpeg_chroma_subsampling: true,
            png_compression: PngCompression::Default,
            png_interlaced: false,
            webp_lossless: true,
            tiff_compression: TiffCompression::Deflate,
            exr_compression: ExrCompression::Zip16,
            embed_icc: true,
            embed_metadata: true,
        }
    }
}

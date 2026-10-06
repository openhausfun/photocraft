//! Format identification and capability declarations.

use crate::image::{ChannelLayout, SampleType};

/// Supported (or known) raster file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    Jpeg,
    Tiff,
    WebP,
    Gif,
    Bmp,
    Tga,
    Ico,
    /// Netpbm family: PBM/PGM/PPM (P1–P6), PAM (P7) and PFM (`PF`/`Pf`).
    Pnm,
    Qoi,
    OpenExr,
    /// Radiance RGBE.
    Hdr,
    /// Encode-only behind the `avif` feature; see [`ASYMMETRIC_EXCEPTIONS`].
    Avif,
}

/// What a format can hold **and** what this crate reads/writes for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatCaps {
    /// Decoding is available in this build.
    pub read: bool,
    /// Encoding is available in this build.
    pub write: bool,
    /// Sample types stored without conversion.
    pub depths: &'static [SampleType],
    /// Layouts stored *and read back* without conversion.
    pub layouts: &'static [ChannelLayout],
    pub alpha: bool,
    pub icc: bool,
    pub exif: bool,
    pub xmp: bool,
    /// Physical resolution (DPI) is preserved.
    pub dpi: bool,
    /// Free-form key/value text is preserved.
    pub text: bool,
    /// The container can hold animation/multiple frames. This crate only
    /// decodes the first frame and encodes a single frame.
    pub animation: bool,
    /// Our encoder is lossy (quantization or lossy compression).
    pub lossy: bool,
}

/// Formats that are enabled but intentionally not symmetric, with the reason.
/// The test-suite asserts that every other enabled format is read+write.
pub const ASYMMETRIC_EXCEPTIONS: &[(Format, &str)] = &[(
    Format::Avif,
    "AVIF encode uses ravif (pure Rust) but decoding requires dav1d (C); read stays unsupported \
     until a pure-Rust AV1 decoder is viable. Only enabled with the non-default `avif` feature.",
)];

use ChannelLayout as L;
use SampleType as S;

const RGB_GRAY: &[ChannelLayout] = &[L::Gray, L::GrayA, L::Rgb, L::Rgba];
const ALL_LAYOUTS: &[ChannelLayout] = &[L::Gray, L::GrayA, L::Rgb, L::Rgba, L::Cmyk, L::CmykA];

impl Format {
    /// Every format known to the crate (enabled or not).
    pub const ALL: [Format; 13] = [
        Format::Png,
        Format::Jpeg,
        Format::Tiff,
        Format::WebP,
        Format::Gif,
        Format::Bmp,
        Format::Tga,
        Format::Ico,
        Format::Pnm,
        Format::Qoi,
        Format::OpenExr,
        Format::Hdr,
        Format::Avif,
    ];

    pub fn caps(self) -> FormatCaps {
        caps(self)
    }

    /// Readable or writable in this build.
    pub fn is_enabled(self) -> bool {
        let c = caps(self);
        c.read || c.write
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Tiff => "TIFF",
            Format::WebP => "WebP",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::Tga => "TGA",
            Format::Ico => "ICO",
            Format::Pnm => "Netpbm",
            Format::Qoi => "QOI",
            Format::OpenExr => "OpenEXR",
            Format::Hdr => "Radiance HDR",
            Format::Avif => "AVIF",
        }
    }

    /// Recognized file extensions (lowercase, without dot); the first one is
    /// the preferred extension for writing.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Png => &["png", "apng"],
            Format::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Format::Tiff => &["tif", "tiff"],
            Format::WebP => &["webp"],
            Format::Gif => &["gif"],
            Format::Bmp => &["bmp", "dib"],
            Format::Tga => &["tga", "icb", "vda", "vst"],
            Format::Ico => &["ico"],
            Format::Pnm => &["pnm", "pbm", "pgm", "ppm", "pam", "pfm"],
            Format::Qoi => &["qoi"],
            Format::OpenExr => &["exr"],
            Format::Hdr => &["hdr"],
            Format::Avif => &["avif"],
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Tiff => "image/tiff",
            Format::WebP => "image/webp",
            Format::Gif => "image/gif",
            Format::Bmp => "image/bmp",
            Format::Tga => "image/x-tga",
            Format::Ico => "image/x-icon",
            Format::Pnm => "image/x-portable-anymap",
            Format::Qoi => "image/qoi",
            Format::OpenExr => "image/x-exr",
            Format::Hdr => "image/vnd.radiance",
            Format::Avif => "image/avif",
        }
    }

    /// Maximum encodable dimensions, if the format has a hard limit.
    pub fn max_dimensions(self) -> Option<(u32, u32)> {
        match self {
            Format::Jpeg | Format::Gif => Some((65535, 65535)),
            Format::WebP => Some((16384, 16384)),
            Format::Ico => Some((256, 256)),
            _ => None,
        }
    }
}

/// Capability table. `read`/`write` reflect the current build's features.
pub fn caps(format: Format) -> FormatCaps {
    let base = FormatCaps {
        read: true,
        write: true,
        depths: &[S::U8],
        layouts: &[L::Rgb, L::Rgba],
        alpha: true,
        icc: false,
        exif: false,
        xmp: false,
        dpi: false,
        text: false,
        animation: false,
        lossy: false,
    };
    match format {
        Format::Png => {
            FormatCaps { depths: &[S::U8, S::U16], layouts: RGB_GRAY, icc: true, exif: true, xmp: true, dpi: true, text: true, animation: true, ..base }
        }
        Format::Jpeg => FormatCaps { layouts: &[L::Gray, L::Rgb, L::Cmyk], alpha: false, icc: true, exif: true, xmp: true, dpi: true, lossy: true, ..base },
        Format::Tiff => FormatCaps { depths: &[S::U8, S::U16, S::F32], layouts: ALL_LAYOUTS, icc: true, xmp: true, dpi: true, text: true, ..base },
        Format::WebP => FormatCaps { icc: true, exif: true, xmp: true, animation: true, ..base },
        Format::Gif => FormatCaps { layouts: &[L::Rgba], animation: true, lossy: true, ..base },
        Format::Bmp => base,
        Format::Tga => FormatCaps { layouts: RGB_GRAY, ..base },
        Format::Ico => FormatCaps { layouts: &[L::Rgba], ..base },
        Format::Pnm => FormatCaps { depths: &[S::U8, S::U16, S::F32], layouts: ALL_LAYOUTS, ..base },
        Format::Qoi => base,
        Format::OpenExr => FormatCaps { depths: &[S::F16, S::F32], layouts: RGB_GRAY, ..base },
        Format::Hdr => FormatCaps { depths: &[S::F32], layouts: &[L::Rgb], alpha: false, lossy: true, ..base },
        Format::Avif => FormatCaps { read: false, write: cfg!(feature = "avif"), lossy: true, ..base },
    }
}

/// Identify a format from its leading bytes (magic numbers). TGA has no
/// magic and is recognized last, by header plausibility.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    let b = bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Format::Jpeg);
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") || b.starts_with(b"II+\0") || b.starts_with(b"MM\0+") {
        return Some(Format::Tiff);
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some(Format::WebP);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(Format::Gif);
    }
    if b.starts_with(b"qoif") {
        return Some(Format::Qoi);
    }
    if b.starts_with(&[0x76, 0x2F, 0x31, 0x01]) {
        return Some(Format::OpenExr);
    }
    if b.starts_with(b"#?RADIANCE") || b.starts_with(b"#?RGBE") {
        return Some(Format::Hdr);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" && is_avif_ftyp(b) {
        return Some(Format::Avif);
    }
    if b.len() >= 14 && b.starts_with(b"BM") {
        return Some(Format::Bmp);
    }
    if b.len() >= 6 && b[0..4] == [0, 0, 1, 0] && (b[4] != 0 || b[5] != 0) {
        return Some(Format::Ico);
    }
    if b.len() >= 3 && b[0] == b'P' && matches!(b[1], b'1'..=b'7' | b'F' | b'f') && b[2].is_ascii_whitespace() {
        return Some(Format::Pnm);
    }
    if looks_like_tga(b) {
        return Some(Format::Tga);
    }
    None
}

fn is_avif_ftyp(b: &[u8]) -> bool {
    let size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let end = size.clamp(12, b.len().min(64));
    let brands = &b[8..end];
    brands.as_chunks::<4>().0.iter().enumerate().any(|(i, c)| i != 1 && (c == b"avif" || c == b"avis"))
}

fn looks_like_tga(b: &[u8]) -> bool {
    if b.len() < 18 {
        return false;
    }
    if b.len() >= 26 && &b[b.len() - 18..] == b"TRUEVISION-XFILE.\0" {
        return true;
    }
    let cmap_type = b[1];
    let img_type = b[2];
    let cmap_len = u16::from_le_bytes([b[5], b[6]]);
    let cmap_depth = b[7];
    let w = u16::from_le_bytes([b[12], b[13]]);
    let h = u16::from_le_bytes([b[14], b[15]]);
    let depth = b[16];
    let desc = b[17];
    let type_ok = matches!(img_type, 1 | 2 | 3 | 9 | 10 | 11);
    let cmap_ok = match cmap_type {
        0 => cmap_len == 0 && cmap_depth == 0 && !matches!(img_type, 1 | 9),
        1 => matches!(img_type, 1 | 9) && cmap_len > 0 && matches!(cmap_depth, 15 | 16 | 24 | 32),
        _ => false,
    };
    let depth_ok = match img_type {
        1 | 9 => depth == 8,
        3 | 11 => matches!(depth, 8 | 16),
        _ => matches!(depth, 15 | 16 | 24 | 32),
    };
    type_ok && cmap_ok && depth_ok && w > 0 && h > 0 && desc & 0xC0 == 0
}

/// Look up a format by file extension (case-insensitive, with or without a
/// leading dot, or a full file name/path).
pub fn from_extension(ext: &str) -> Option<Format> {
    let ext = ext.rsplit(['.', '/', '\\']).next().unwrap_or(ext).to_ascii_lowercase();
    Format::ALL.into_iter().find(|f| f.extensions().contains(&ext.as_str()))
}

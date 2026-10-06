//! `photocraft-raw`: a clean-room, pure-Rust camera raw decoder and developer.
//!
//! Implemented only from public specifications and papers: TIFF 6.0,
//! TIFF/EP (ISO 12234-2), the Adobe DNG Specification 1.7, ITU-T T.81
//! (lossless JPEG, process 14), the published structure of Canon's CR2
//! container, published descriptions of Sony's cRAW code, public maker-note
//! tag tables, observation of sample files, and the demosaicing papers cited
//! in [`Demosaic`]. No code from
//! dcraw, LibRaw, rawspeed, rawler, rawloader or darktable was used.
//!
//! * [`identify`] / [`is_raw`] recognise raw files from their bytes.
//! * [`decode`] reads the undeveloped [`Sensor`] data and metadata.
//! * [`develop`] turns it into 16-bit RGB in ProPhoto RGB (ROMM primaries,
//!   D50 white, gamma 1.8): linearization, black/white levels, white balance,
//!   highlight clipping, demosaicing, camera → XYZ → ProPhoto, exposure and
//!   orientation.
//! * [`embedded_preview`] finds the camera's full-size JPEG preview, for
//!   formats whose sensor data is not decoded yet.
//!
//! Decoded today: DNG (uncompressed and lossless-JPEG, strips and tiles, CFA
//! and LinearRaw), CR2 (lossless JPEG with Canon slices), uncompressed or
//! lossless-JPEG TIFF/EP raws (NEF, ARW, PEF… when not vendor-compressed),
//! Sony compressed ARW (cRAW), Panasonic RW2 (RawFormat 5) and uncompressed
//! Olympus ORF. Everything else reports [`RawError::Unsupported`].
//!
//! The crate is standalone (no workspace dependencies), does no I/O, builds for
//! `wasm32-unknown-unknown` and never panics on hostile input: sizes are
//! checked against [`Limits`] before allocating.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod color;
mod cr2;
mod demosaic;
mod develop;
mod dng;
mod error;
mod ljpeg;
mod opcodes;
mod orf;
mod par;
mod preview;
mod rw2;
mod sensor;
mod sony;
mod tiff;
mod tiffep;

#[cfg(feature = "testgen")]
pub mod testgen;

pub use crate::color::{Calibration, ColorInfo, Mat3};
pub use crate::demosaic::Demosaic;
pub use crate::develop::{DevelopOptions, Developed, RawInfo, WhiteBalance, develop_sensor};
pub use crate::error::RawError;
pub use crate::opcodes::GainMap;
pub use crate::preview::{Preview, embedded_preview};
pub use crate::sensor::{BlackLevels, Cfa, Rect, Sensor};

use crate::tiff::{Tiff, tag};

/// Name of the output colour space of [`develop`].
pub const OUTPUT_SPACE: &str = "ProPhoto RGB (ROMM primaries, D50, gamma 1.8)";

/// Camera raw container families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawFormat {
    Dng,
    /// Canon CR2.
    Cr2,
    /// Canon CR3 (ISO BMFF).
    Cr3,
    /// Nikon NEF / NRW.
    Nef,
    /// Sony ARW / SR2.
    Arw,
    /// Pentax PEF.
    Pef,
    /// Olympus ORF.
    Orf,
    /// Panasonic RW2.
    Rw2,
    /// Fujifilm RAF.
    Raf,
    /// Another TIFF/EP raw.
    TiffEp,
}

impl RawFormat {
    pub fn name(self) -> &'static str {
        match self {
            RawFormat::Dng => "DNG",
            RawFormat::Cr2 => "CR2",
            RawFormat::Cr3 => "CR3",
            RawFormat::Nef => "NEF",
            RawFormat::Arw => "ARW",
            RawFormat::Pef => "PEF",
            RawFormat::Orf => "ORF",
            RawFormat::Rw2 => "RW2",
            RawFormat::Raf => "RAF",
            RawFormat::TiffEp => "TIFF/EP raw",
        }
    }
}

/// File extensions of the raw formats this crate recognises.
pub const EXTENSIONS: &[&str] = &["dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "pef", "orf", "rw2", "raf"];

/// Decompression-bomb guards, checked on header values before allocating
/// (the same shape and defaults as `photocraft-codecs`' limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_width: u32,
    pub max_height: u32,
    /// Maximum `width * height`.
    pub max_pixels: u64,
    /// Maximum bytes of one buffer.
    pub max_alloc: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_width: 1 << 18, max_height: 1 << 18, max_pixels: 1 << 28, max_alloc: 2 << 30 }
    }
}

impl Limits {
    /// Checks a `width × height` buffer of `bytes_per_pixel` bytes per pixel.
    pub fn check(&self, width: u64, height: u64, bytes_per_pixel: u64) -> Result<(), RawError> {
        if width == 0 || height == 0 {
            return Err(RawError::Malformed(format!("zero-sized image {width}x{height}")));
        }
        if width > u64::from(self.max_width) || height > u64::from(self.max_height) {
            return Err(RawError::LimitExceeded(format!("dimensions {width}x{height} exceed {}x{}", self.max_width, self.max_height)));
        }
        let pixels = width.saturating_mul(height);
        if pixels > self.max_pixels {
            return Err(RawError::LimitExceeded(format!("{pixels} pixels exceed {}", self.max_pixels)));
        }
        let bytes = pixels.saturating_mul(bytes_per_pixel);
        if bytes > self.max_alloc || usize::try_from(bytes).is_err() {
            return Err(RawError::LimitExceeded(format!("{bytes} bytes exceed {}", self.max_alloc)));
        }
        Ok(())
    }
}

const PHOTOMETRIC_CFA: u32 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;

fn make_format(make: Option<&str>) -> RawFormat {
    let m = make.unwrap_or("").to_ascii_uppercase();
    if m.starts_with("NIKON") {
        RawFormat::Nef
    } else if m.starts_with("SONY") {
        RawFormat::Arw
    } else if m.starts_with("PENTAX") || m.starts_with("RICOH") {
        RawFormat::Pef
    } else if m.starts_with("OLYMPUS") || m.starts_with("OM DIGITAL") {
        RawFormat::Orf
    } else {
        RawFormat::TiffEp
    }
}

/// Recognises a camera raw file from its bytes (cheap: header and IFDs only).
pub fn identify(bytes: &[u8]) -> Option<RawFormat> {
    if bytes.get(4..12) == Some(b"ftypcrx ") {
        return Some(RawFormat::Cr3);
    }
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        return Some(RawFormat::Raf);
    }
    let t = Tiff::new(bytes)?;
    match bytes.get(0..4) {
        Some(b"IIRO" | b"IIRS" | b"MMOR") => return Some(RawFormat::Orf),
        Some(b"IIU\0") => return Some(RawFormat::Rw2),
        _ => {}
    }
    if bytes.get(8..11) == Some(b"CR\x02") {
        return Some(RawFormat::Cr2);
    }
    let ifds = t.all_ifds();
    if ifds.iter().any(|i| i.has(tag::DNG_VERSION)) {
        return Some(RawFormat::Dng);
    }
    if ifds.iter().any(|i| matches!(t.tag_uint(i, tag::PHOTOMETRIC), Some(PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW))) {
        let make = ifds.iter().find_map(|i| t.tag_ascii(i, tag::MAKE));
        return Some(make_format(make.as_deref()));
    }
    if tiffep::is_sony_non_cfa(&t, &ifds) {
        return Some(RawFormat::Arw);
    }
    None
}

/// `true` if `bytes` look like a camera raw file.
pub fn is_raw(bytes: &[u8]) -> bool {
    identify(bytes).is_some()
}

/// Reads the undeveloped sensor data and metadata.
pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Sensor, RawError> {
    let format = identify(bytes).ok_or(RawError::NotRaw)?;
    match format {
        RawFormat::Cr3 => Err(RawError::unsupported("Canon CR3 (ISO BMFF / CRX) is not decoded yet")),
        RawFormat::Raf => Err(RawError::unsupported("Fujifilm RAF is not decoded yet")),
        _ => {
            let t = Tiff::new(bytes).ok_or(RawError::NotRaw)?;
            match format {
                RawFormat::Dng => dng::decode(&t, limits),
                RawFormat::Cr2 => cr2::decode(&t, limits),
                RawFormat::Rw2 => rw2::decode(&t, limits),
                RawFormat::Orf => orf::decode(&t, limits),
                f => tiffep::decode(&t, f, limits),
            }
        }
    }
}

/// Decodes a lossless JPEG (T.81 process 14) stream into `(width, height,
/// components, samples)`; at most `max_samples` samples are allocated.
/// Exposed for fuzzing and tools.
#[doc(hidden)]
pub fn decode_lossless_jpeg(bytes: &[u8], max_samples: usize) -> Result<(usize, usize, usize, Vec<u16>), RawError> {
    ljpeg::decode(bytes, max_samples).map(|(f, v)| (f.width, f.height, f.components, v))
}

/// Lists the TIFF structure of a raw file (debugging aid).
#[doc(hidden)]
pub fn dump_structure(bytes: &[u8]) -> String {
    tiff::dump(bytes)
}

/// Decodes and develops a raw file.
pub fn develop(bytes: &[u8], opts: &DevelopOptions) -> Result<Developed, RawError> {
    let sensor = decode(bytes, &opts.limits)?;
    develop_sensor(&sensor, opts)
}

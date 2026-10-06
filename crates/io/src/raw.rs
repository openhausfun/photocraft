//! Camera raw files via `photocraft-raw`: developed to a 16-bit RGB document
//! in ProPhoto RGB (the embedded profile is the built-in ProPhoto-compatible
//! profile), or, for raw variants not decoded yet, the camera's embedded
//! JPEG preview with a warning.

use std::sync::Arc;

use photocraft_codecs::{self as codecs, ChannelLayout, Format, Image};
use photocraft_raw::{DevelopOptions, Limits, RawError};

use crate::flat::image_to_document;
use crate::{ImportResult, IoError};

/// `true` if `bytes` are a camera raw file `photocraft-raw` recognises.
pub fn is_raw(bytes: &[u8]) -> bool {
    photocraft_raw::is_raw(bytes)
}

/// The decode limits shared with the flat codecs.
fn limits() -> Limits {
    let l = codecs::Limits::default();
    Limits { max_width: l.max_width, max_height: l.max_height, max_pixels: l.max_pixels, max_alloc: l.max_alloc }
}

/// A raw file's embedded JPEG preview, turned upright. Its own EXIF orientation wins when it
/// has one; otherwise the raw's IFD0 orientation applies (TIFF-based raws record it there, and
/// their previews are usually stored as the sensor reads out). Developed raws are oriented by
/// `photocraft-raw` and carry no EXIF, so nothing is turned twice.
fn upright_preview(raw: &[u8], jpeg: &[u8]) -> Result<Image, IoError> {
    let img = codecs::decode_as_with(Format::Jpeg, jpeg, &codecs::DecodeOptions { keep_orientation: true, ..Default::default() })?;
    let own = img.meta.exif.as_deref().map_or(1, codecs::exif_orientation);
    let o = if own != 1 { own } else { codecs::exif_orientation(raw) };
    Ok(img.oriented(o)?)
}

/// Develops a raw file with the default settings.
pub fn import_raw(name: &str, bytes: &[u8]) -> Result<ImportResult, IoError> {
    import_raw_with(name, bytes, &DevelopOptions { limits: limits(), ..Default::default() })
}

/// Develops a raw file with explicit settings.
pub fn import_raw_with(name: &str, bytes: &[u8], opts: &DevelopOptions) -> Result<ImportResult, IoError> {
    let format = photocraft_raw::identify(bytes).map(|f| f.name()).unwrap_or("camera raw");
    match photocraft_raw::develop(bytes, opts) {
        Ok(dev) => {
            let img = Image::from_u16(dev.width, dev.height, ChannelLayout::Rgb, &dev.rgb)?;
            let mut r = image_to_document(name, &img)?;
            r.document.icc_profile = Some(Arc::new(photocraft_cms::Builtin::ProPhotoCompat.profile().to_bytes().to_vec()));
            // "Canon" + "Canon EOS 80D" reads as "Canon EOS 80D".
            let camera = match (dev.info.make.as_deref(), dev.info.model.as_deref()) {
                (Some(make), Some(model)) if model.to_ascii_lowercase().starts_with(&make.to_ascii_lowercase()) => model.to_string(),
                (make, model) => [make, model].into_iter().flatten().collect::<Vec<_>>().join(" "),
            };
            r.warnings.push(format!(
                "{format}{} developed with default settings ({} demosaic, as-shot white balance) into 16-bit {}",
                if camera.is_empty() { String::new() } else { format!(" from {camera}") },
                opts.demosaic.id(),
                photocraft_raw::OUTPUT_SPACE
            ));
            r.warnings.extend(dev.warnings);
            Ok(r)
        }
        Err(RawError::Unsupported(reason)) => match photocraft_raw::embedded_preview(bytes) {
            Some(p) => {
                let img = upright_preview(bytes, p.jpeg)?;
                let mut r = image_to_document(name, &img)?;
                r.warnings.insert(
                    0,
                    format!(
                        "{format}: {reason} is not supported yet; opened the camera's embedded {}x{} JPEG preview instead (8-bit, not the raw sensor data)",
                        p.width, p.height
                    ),
                );
                Ok(r)
            }
            None => Err(IoError::Raw(RawError::Unsupported(reason))),
        },
        Err(e) => Err(IoError::Raw(e)),
    }
}

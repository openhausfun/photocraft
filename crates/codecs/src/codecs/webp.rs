//! WebP via `image-webp`: decode lossy/lossless (first frame), encode
//! lossless only (no pure-Rust lossy encoder exists). ICC/EXIF/XMP both ways.

use std::io::Cursor;

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, Metadata, SampleType};
use crate::options::{EncodeOptions, Limits};

const F: Format = Format::WebP;

fn err(e: impl std::fmt::Display) -> CodecError {
    CodecError::malformed(F, e)
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, CodecError> {
    let mut dec = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
    dec.set_memory_limit(limits.alloc_usize());
    let (w, h) = dec.dimensions();
    let layout = if dec.has_alpha() { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    limits.check(w, h, layout, SampleType::U8)?;
    let size = dec.output_buffer_size().ok_or_else(|| err("image too large"))?;
    let mut buf = vec![0u8; size];
    dec.read_image(&mut buf).map_err(err)?;
    let icc = dec.icc_profile().ok().flatten();
    let exif = dec.exif_metadata().ok().flatten();
    let xmp = dec.xmp_metadata().ok().flatten().and_then(|b| String::from_utf8(b).ok());
    let mut img = Image::from_u8(w, h, layout, buf)?;
    img.icc = icc;
    img.meta = Metadata { exif, xmp, ..Default::default() };
    Ok(img)
}

pub(crate) fn encode(src: &Image, plan: Plan, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    if !opts.webp_lossless {
        return Err(CodecError::unsupported(F, "lossy WebP encoding needs libwebp (C); only lossless is available"));
    }
    let img = src.converted(plan.layout, plan.sample);
    let ct = match img.layout() {
        ChannelLayout::Gray => image_webp::ColorType::L8,
        ChannelLayout::GrayA => image_webp::ColorType::La8,
        ChannelLayout::Rgb => image_webp::ColorType::Rgb8,
        ChannelLayout::Rgba => image_webp::ColorType::Rgba8,
        l => return Err(CodecError::encode(F, format!("unsupported layout {l:?}"))),
    };
    let mut out = Vec::new();
    let mut enc = image_webp::WebPEncoder::new(&mut out);
    if opts.embed_icc
        && let Some(icc) = &img.icc
    {
        enc.set_icc_profile(icc.clone());
    }
    if opts.embed_metadata {
        if let Some(exif) = &img.meta.exif {
            // The pixels are written as they are shown: never let a viewer rotate them again.
            enc.set_exif_metadata(crate::orientation::upright_exif(exif).into_owned());
        }
        if let Some(xmp) = &img.meta.xmp {
            enc.set_xmp_metadata(crate::orientation::upright_xmp(xmp).as_bytes().to_vec());
        }
    }
    enc.encode(img.data(), img.width(), img.height(), ct).map_err(|e| CodecError::encode(F, e))?;
    Ok(out)
}

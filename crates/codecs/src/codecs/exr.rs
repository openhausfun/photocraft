//! OpenEXR via the pure-Rust `exr` crate: F16/F32, Y/YA/RGB/RGBA channels,
//! lossless compression. Reads the first valid layer at full resolution;
//! pixel data is taken from the data window.

use std::io::Cursor;

use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Layer, LayerAttributes, LineOrder, ReadChannels, ReadLayers, WritableImage, read,
};
use half::f16;

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, SampleType};
use crate::options::{EncodeOptions, ExrCompression, Limits};

const F: Format = Format::OpenExr;

fn err(e: impl std::fmt::Display) -> CodecError {
    CodecError::malformed(F, e)
}

/// Strip an optional `layer.` prefix from a channel name.
fn base_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, CodecError> {
    // Check declared sizes before any pixel allocation.
    let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false).map_err(err)?;
    for header in meta.headers.iter() {
        let size = header.layer_size;
        let (w, h) = (u32::try_from(size.0).unwrap_or(u32::MAX), u32::try_from(size.1).unwrap_or(u32::MAX));
        let bpp = header.channels.list.len().max(1) as u64 * 4;
        limits.check_bytes(w, h, bpp)?;
    }

    let image =
        read().no_deep_data().largest_resolution_level().all_channels().first_valid_layer().all_attributes().from_buffered(Cursor::new(bytes)).map_err(err)?;
    let layer = &image.layer_data;
    let (w, h) = (layer.size.0, layer.size.1);
    let channels = &layer.channel_data.list;
    let find = |n: &str| channels.iter().find(|c| base_name(c.name.to_string().as_str()) == n);
    let (color, layout): (Vec<_>, ChannelLayout) = if let (Some(r), Some(g), Some(b)) = (find("R"), find("G"), find("B")) {
        match find("A") {
            Some(a) => (vec![r, g, b, a], ChannelLayout::Rgba),
            None => (vec![r, g, b], ChannelLayout::Rgb),
        }
    } else if let Some(y) = find("Y").or_else(|| (channels.len() == 1).then(|| &channels[0])) {
        match find("A") {
            Some(a) => (vec![y, a], ChannelLayout::GrayA),
            None => (vec![y], ChannelLayout::Gray),
        }
    } else {
        return Err(CodecError::unsupported(F, "no R/G/B or Y channels"));
    };
    let all_f16 = color.iter().all(|c| matches!(c.sample_data, FlatSamples::F16(_)));
    let sample = if all_f16 { SampleType::F16 } else { SampleType::F32 };
    let npx = w * h;
    if color.iter().any(|c| c.sample_data.len() != npx) {
        return Err(err("channel sample count mismatch (subsampled channels are unsupported)"));
    }
    let nc = color.len();
    let (w32, h32) = (w as u32, h as u32);
    let img = if all_f16 {
        let mut out = vec![f16::ZERO; npx * nc];
        for (ci, c) in color.iter().enumerate() {
            if let FlatSamples::F16(v) = &c.sample_data {
                for (i, s) in v.iter().enumerate() {
                    out[i * nc + ci] = *s;
                }
            }
        }
        Image::from_f16(w32, h32, layout, &out)?
    } else {
        let mut out = vec![0f32; npx * nc];
        for (ci, c) in color.iter().enumerate() {
            for (i, s) in c.sample_data.values_as_f32().enumerate() {
                out[i * nc + ci] = s;
            }
        }
        Image::from_f32(w32, h32, layout, &out)?
    };
    debug_assert_eq!(img.sample_type(), sample);
    Ok(img)
}

pub(crate) fn encode(src: &Image, plan: Plan, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let img = src.converted(plan.layout, plan.sample);
    let (w, h) = (img.width() as usize, img.height() as usize);
    let layout = img.layout();
    let names: &[&str] = match layout {
        ChannelLayout::Gray => &["Y"],
        ChannelLayout::GrayA => &["Y", "A"],
        ChannelLayout::Rgb => &["R", "G", "B"],
        ChannelLayout::Rgba => &["R", "G", "B", "A"],
        l => return Err(CodecError::encode(F, format!("unsupported layout {l:?}"))),
    };
    let nc = names.len();
    let mut list: Vec<AnyChannel<FlatSamples>> = Vec::with_capacity(nc);
    match img.sample_type() {
        SampleType::F16 => {
            let s = img.to_f16_samples().unwrap_or_default();
            for (ci, n) in names.iter().enumerate() {
                let v: Vec<f16> = s.iter().skip(ci).step_by(nc).copied().collect();
                list.push(AnyChannel::new(*n, FlatSamples::F16(v)));
            }
        }
        SampleType::F32 => {
            let s = img.to_f32_samples().unwrap_or_default();
            for (ci, n) in names.iter().enumerate() {
                let v: Vec<f32> = s.iter().skip(ci).step_by(nc).copied().collect();
                list.push(AnyChannel::new(*n, FlatSamples::F32(v)));
            }
        }
        s => return Err(CodecError::encode(F, format!("unsupported sample {s:?}"))),
    }
    let compression = match opts.exr_compression {
        ExrCompression::None => Compression::Uncompressed,
        ExrCompression::Rle => Compression::RLE,
        ExrCompression::Zip1 => Compression::ZIP1,
        ExrCompression::Zip16 => Compression::ZIP16,
        ExrCompression::Piz => Compression::PIZ,
    };
    let encoding = Encoding { compression, blocks: Blocks::ScanLines, line_order: LineOrder::Increasing };
    let channels = AnyChannels::sort(list.into_iter().collect());
    let layer = Layer::new((w, h), LayerAttributes::default(), encoding, channels);
    let image = exr::image::Image::from_layer(layer);
    let mut cursor = Cursor::new(Vec::new());
    image.write().to_buffered(&mut cursor).map_err(|e| CodecError::encode(F, e))?;
    Ok(cursor.into_inner())
}

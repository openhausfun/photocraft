//! Multichannel documents <-> PSD colour mode 7.
//!
//! A Multichannel document has no layers: its image is a set of ink (spot) channels, stored in
//! [`Document::channels`] with ink density as the value (1 = solid ink, like every spot channel).
//! A PSD stores the same planes the way Photoshop shows them in the Channels panel: 0 (black) =
//! solid ink, max = paper. Channel names come from resources 1006 / 1045 and ink colours from
//! DisplayInfo (1077), exactly as for spot channels in the other modes.

use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{AlphaChannel, Document};
use photocraft_geom::Rect;
use photocraft_psd::file::LayerInfoPlacement;
use photocraft_psd::resources::{ImageResource, ResolutionInfo, ids, version_info_resource};
use photocraft_psd::{ColorMode as PsdMode, Compression, Header, ImageData, PsdFile, Version};
use photocraft_raster::Surface;

use crate::pixels::{encode_be, psd_depth};

/// Value of sample `i` of a big-endian plane.
fn decode(plane: &[u8], i: usize, s: SampleType) -> f32 {
    match s {
        SampleType::U8 => f32::from(plane[i]) / 255.0,
        SampleType::U16 => f32::from(u16::from_be_bytes([plane[2 * i], plane[2 * i + 1]])) / 65535.0,
        SampleType::F32 => f32::from_be_bytes([plane[4 * i], plane[4 * i + 1], plane[4 * i + 2], plane[4 * i + 3]]),
    }
}

/// Fills `doc` (already in Multichannel mode at the file depth) from the merged planes `all`.
pub(crate) fn import(file: &PsdFile, all: &[u8], doc: &mut Document, names: &[String], warnings: &mut Vec<String>) {
    let h = &file.header;
    let depth = doc.depth;
    let (w, hh) = (h.width as usize, h.height as usize);
    let n = w * hh;
    let plane = h.row_bytes() * hh;
    let canvas = Rect::new(0, 0, h.width as i32, h.height as i32);
    let fmt = PixelFormat::new(ColorMode::Grayscale, depth, false);
    for k in 0..usize::from(h.channels) {
        let Some(p) = all.get(k * plane..(k + 1) * plane) else {
            warnings.push(format!("multichannel: channel {} is missing", k + 1));
            break;
        };
        let vals: Vec<f32> = (0..n).map(|i| 1.0 - decode(p, i, depth)).collect();
        let mut s = Surface::new(fmt);
        s.write_region(canvas, &vals);
        s.prune();
        let name = names.get(k).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Channel {}", k + 1));
        // Without DisplayInfo the channels print in black, as Photoshop shows them.
        doc.channels.push(AlphaChannel { spot: Some((Color::BLACK, 0.0)), ..AlphaChannel::new(name, s) });
    }
    if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO) {
        crate::channel_map::apply_display_info(&r.data, true, &mut doc.channels);
    } else if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO_OLD) {
        crate::channel_map::apply_display_info(&r.data, false, &mut doc.channels);
    }
}

/// Writes a Multichannel document as a mode-7 PSD (no layer section; every channel a plane).
pub(crate) fn document_to_psd(doc: &Document, force_psb: bool) -> (PsdFile, Vec<String>) {
    let mut warnings = Vec::new();
    // Photoshop's Multichannel mode is 8 or 16 bits per channel.
    let sample = match doc.depth {
        SampleType::F32 => {
            warnings.push("32-bit Multichannel written as 16 bits/channel (Photoshop has no 32-bit Multichannel)".into());
            SampleType::U16
        }
        s => s,
    };
    if !doc.layers.is_empty() {
        warnings.push("Multichannel documents have no layers: the layers were not written".into());
    }
    let canvas = doc.bounds();
    let channels: Vec<&AlphaChannel> = doc.channels.iter().take(56).collect();
    if doc.channels.len() > 56 {
        warnings.push("only 56 channels fit in PSD; the rest were dropped".into());
    }
    let mut planes = Vec::new();
    if channels.is_empty() {
        // A PSD needs at least one channel: an empty (paper) plane.
        for _ in 0..canvas.width() as usize * canvas.height() as usize {
            encode_be(1.0, sample, &mut planes);
        }
    }
    for ch in &channels {
        for v in ch.surface.read_region(canvas).chunks_exact(ch.surface.channels()) {
            encode_be(1.0 - v[0], sample, &mut planes);
        }
    }
    let big = doc.size.width > 30_000 || doc.size.height > 30_000;
    let version = if force_psb || big { Version::Psb } else { Version::Psd };
    let header = Header::new(version, doc.size.width, doc.size.height, channels.len().max(1) as u16, psd_depth(sample), PsdMode::Multichannel);
    let image_data = ImageData::encode(Compression::Rle, &planes, &header)
        .or_else(|_| ImageData::encode(Compression::Raw, &planes, &header))
        .unwrap_or(ImageData { compression: Compression::Raw, data: planes });
    let mut resources = vec![ImageResource::new(ids::RESOLUTION_INFO, ResolutionInfo::from_dpi(f64::from(doc.resolution_dpi)).to_bytes())];
    if !channels.is_empty() {
        let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
        resources.push(ImageResource::new(1006, crate::psd_export::pascal_names_resource(&names)));
        resources.push(ImageResource::new(1045, crate::psd_export::unicode_names_resource(&names)));
        resources.push(ImageResource::new(crate::channel_map::DISPLAY_INFO, crate::channel_map::display_info(&channels)));
    }
    if let Some(x) = &doc.metadata.xmp {
        resources.push(ImageResource::new(ids::XMP, x.as_bytes().to_vec()));
    }
    resources.push(version_info_resource(true));
    let color_mode_data = photocraft_psd::hdr::color_mode_data_for_depth(header.depth);
    let file = PsdFile {
        header,
        color_mode_data,
        resources,
        layer_info: None,
        layer_info_placement: LayerInfoPlacement::Section,
        global_layer_mask: None,
        global_blocks: Vec::new(),
        layer_mask_trailing: Vec::new(),
        image_data,
    };
    (file, warnings)
}

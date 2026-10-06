//! Flat raster formats via `photocraft-codecs`.

use std::sync::Arc;

use photocraft_codecs::{self as codecs, ChannelLayout, Format, Image, SampleType as CSample};
use photocraft_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size, TILE_SIZE};
use photocraft_raster::Surface;

use crate::{ExportOptions, ExportResult, ImportResult, IoError};

/// Bytes per band when converting or exporting a band of rows at a time.
const BAND_BYTES: usize = 32 << 20;

/// Decodes a flat image into a single-layer document.
pub fn import_flat(name: &str, bytes: &[u8]) -> Result<ImportResult, IoError> {
    let img = codecs::decode(bytes)?;
    let mut r = image_to_document(name, &img)?;
    // OpenEXR and Radiance HDR hold linear, scene-referred values (Rec. 709 primaries unless
    // stated otherwise): tag them linear sRGB so they display and convert correctly.
    let d = &mut r.document;
    if d.icc_profile.is_none() && d.mode == ColorMode::Rgb && matches!(codecs::detect(bytes), Some(Format::OpenExr | Format::Hdr)) {
        d.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
    }
    Ok(r)
}

/// A decoded flat image as a single-layer document.
pub(crate) fn image_to_document(name: &str, img: &Image) -> Result<ImportResult, IoError> {
    let mut warnings = Vec::new();
    let (mode, target_layout) = match img.layout() {
        ChannelLayout::Gray | ChannelLayout::GrayA => (ColorMode::Grayscale, ChannelLayout::GrayA),
        ChannelLayout::Rgb | ChannelLayout::Rgba => (ColorMode::Rgb, ChannelLayout::Rgba),
        ChannelLayout::Cmyk | ChannelLayout::CmykA => (ColorMode::Cmyk, ChannelLayout::CmykA),
    };
    let (depth, csample) = match img.sample_type() {
        CSample::U8 => (SampleType::U8, CSample::U8),
        CSample::U16 => (SampleType::U16, CSample::U16),
        CSample::F16 => {
            warnings.push("16-bit float samples are stored as 32-bit float".to_string());
            (SampleType::F32, CSample::F32)
        }
        CSample::F32 => (SampleType::F32, CSample::F32),
    };
    let (w, h) = img.dimensions();
    let mut doc = Document::new(name, Size::new(w, h), mode, depth);
    let fmt = PixelFormat::new(mode, depth, true);
    let mut s = Surface::new(fmt);
    if img.layout() == target_layout && img.sample_type() == csample {
        s.write_interleaved(Rect::new(0, 0, w as i32, h as i32), img.data());
    } else {
        // Converted a band of rows at a time: no second full-size copy of the image.
        let row = img.data().len() / (h.max(1) as usize);
        let band = (BAND_BYTES / row.max(1)).max(1);
        for (i, rows) in img.data().chunks(row.max(1) * band).enumerate() {
            let n = (rows.len() / row.max(1)) as u32;
            let part = Image::from_raw(w, n, img.layout(), img.sample_type(), rows.to_vec())?.convert(target_layout, csample);
            let y0 = (i * band) as i32;
            s.write_interleaved(Rect::new(0, y0, w as i32, y0 + n as i32), part.data());
        }
    }
    s.prune();
    let mut bg = Layer::new("Background", LayerContent::Raster(s));
    if !img.layout().has_alpha() {
        bg.locks.transparency = true;
        bg.locks.position = true;
    }
    doc.layers.push(bg);
    doc.icc_profile = img.icc.clone().map(Arc::new);
    doc.metadata.exif = img.meta.exif.clone().map(Arc::new);
    doc.metadata.xmp = img.meta.xmp.clone();
    if let Some((x, _)) = img.meta.dpi {
        doc.resolution_dpi = x;
    }
    if !img.meta.text.is_empty() {
        warnings.push(format!("{} text metadata entries are not kept in the document", img.meta.text.len()));
    }
    Ok(ImportResult { document: doc, warnings })
}

/// `Some(surface)` when the document is exactly one visible, unmasked,
/// normal, fully opaque raster layer: its pixels can be written natively
/// (keeping CMYK / depth exactly) instead of going through the compositor.
fn single_layer(doc: &Document) -> Option<&Surface> {
    let [l] = &doc.layers[..] else { return None };
    let ok = l.visible
        && l.opacity >= 1.0
        && l.fill_opacity >= 1.0
        && l.mask.is_none()
        && l.vector_mask.as_ref().is_none_or(|mask| !mask.enabled)
        && l.effects.items.is_empty()
        && l.effects.psd_raw.is_none()
        && matches!(l.blend, BlendMode::Normal | BlendMode::PassThrough)
        && photocraft_compose::channel_weights(l, doc.mode).is_none()
        && !photocraft_compose::blend_if_active(l, doc.mode);
    match (&l.content, ok) {
        (LayerContent::Raster(s), true) if s.format() == doc.pixel_format() => Some(s),
        _ => None,
    }
}

fn layout_for(mode: ColorMode, alpha: bool) -> ChannelLayout {
    match (mode, alpha) {
        (ColorMode::Grayscale, false) => ChannelLayout::Gray,
        (ColorMode::Grayscale, true) => ChannelLayout::GrayA,
        (ColorMode::Cmyk, false) => ChannelLayout::Cmyk,
        (ColorMode::Cmyk, true) => ChannelLayout::CmykA,
        (_, false) => ChannelLayout::Rgb,
        (_, true) => ChannelLayout::Rgba,
    }
}

fn csample(s: SampleType) -> CSample {
    match s {
        SampleType::U8 => CSample::U8,
        SampleType::U16 => CSample::U16,
        SampleType::F32 => CSample::F32,
    }
}

/// Renders the document to a flat codec image (native pixels when possible).
pub fn document_to_image(doc: &Document, warnings: &mut Vec<String>) -> Result<Image, IoError> {
    let (w, h) = (doc.size.width, doc.size.height);
    let canvas = doc.bounds();
    let fmt = doc.pixel_format();
    let n = (w as usize) * (h as usize);
    // Lab has no flat-format layout here: Lab documents always go through the composite.
    let native = single_layer(doc).filter(|_| fmt.mode != ColorMode::Lab);
    let mut icc = doc.icc_profile.as_ref().map(|i| i.to_vec());
    let img = if let Some(s) = native {
        // Native path: keep model and depth. The surface's encoded samples are the codec's raw
        // native-endian samples, copied a band of rows at a time (alpha dropped when opaque).
        let ch = fmt.channels();
        let opaque = opaque_surface(s, canvas);
        let layout = layout_for(fmt.mode, !opaque);
        let bps = fmt.sample.bytes();
        let keep = if opaque { (ch - 1) * bps } else { ch * bps };
        let mut data = try_buffer(n, keep)?;
        let rows = (BAND_BYTES / (w as usize * ch * bps).max(1)).max(1) as i32;
        let mut y = 0;
        while y < h as i32 {
            let y1 = y.saturating_add(rows).min(h as i32);
            let band = s.to_interleaved(Rect::new(0, y, w as i32, y1));
            if opaque {
                for px in band.chunks_exact(ch * bps) {
                    data.extend_from_slice(&px[..keep]);
                }
            } else {
                data.extend_from_slice(&band);
            }
            y = y1;
        }
        Image::from_raw(w, h, layout, csample(fmt.sample), data)?
    } else {
        let count = doc.layer_count();
        warnings.push(format!("{count} layer(s) flattened; layers, masks and blend modes are not kept"));
        // The compositor works in RGB; write RGB/gray.
        let gray = fmt.mode == ColorMode::Grayscale;
        if fmt.mode == ColorMode::Cmyk || fmt.mode == ColorMode::Lab {
            // The compositor renders CMYK/Lab documents in sRGB (CMYK through the built-in
            // profile), so the file is tagged sRGB.
            warnings.push(format!("{:?} composite written as sRGB RGB (colour-managed conversion)", fmt.mode));
            icc = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes().to_vec());
        }
        let cs = csample(fmt.sample);
        let colors = if gray { 1 } else { 3 };
        // Rendered and quantised in bands (no full-size float composite), with alpha; the alpha
        // is dropped afterwards, in place, when every pixel turned out opaque.
        let mut data = try_buffer(n, (colors + 1) * cs.bytes())?;
        let mut opaque = true;
        let _ = photocraft_compose::render_bands(doc, canvas, 0, |band| -> Result<(), ()> {
            opaque &= band.px.iter().all(|p| p[3] >= 1.0);
            let parts = crate::pixels::par_map(crate::pixels::bands(band.px.len()), |range| {
                let mut out = Vec::with_capacity(range.len() * (colors + 1) * cs.bytes());
                let mut put = |v: f32| {
                    let v = if v.is_nan() { 0.0 } else { v };
                    match cs {
                        CSample::U8 => out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8),
                        CSample::U16 => out.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_ne_bytes()),
                        CSample::F16 | CSample::F32 => out.extend_from_slice(&v.to_ne_bytes()),
                    }
                };
                for p in &band.px[range] {
                    if gray {
                        put(photocraft_color::convert::rgb_to_gray([p[0], p[1], p[2]]));
                    } else {
                        put(p[0]);
                        put(p[1]);
                        put(p[2]);
                    }
                    put(p[3]);
                }
                out
            });
            for part in parts {
                data.extend_from_slice(&part);
            }
            Ok(())
        });
        if opaque {
            drop_alpha(&mut data, colors * cs.bytes(), cs.bytes());
        }
        let layout = layout_for(if gray { ColorMode::Grayscale } else { ColorMode::Rgb }, !opaque);
        Image::from_raw(w, h, layout, cs, data)?
    };
    let meta = codecs::Metadata {
        exif: doc.metadata.exif.as_ref().map(|e| e.to_vec()),
        xmp: doc.metadata.xmp.clone(),
        dpi: Some((doc.resolution_dpi, doc.resolution_dpi)),
        text: Vec::new(),
    };
    Ok(img.with_icc(icc).with_meta(meta))
}

/// An empty buffer with room for `pixels × bytes_per_pixel` bytes, or an error (not an abort)
/// when that much memory can't be had.
fn try_buffer(pixels: usize, bytes_per_pixel: usize) -> Result<Vec<u8>, IoError> {
    let len = pixels.checked_mul(bytes_per_pixel).ok_or_else(|| IoError::Unsupported("image too large".into()))?;
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| IoError::Unsupported(format!("not enough memory for a {} MB image", len >> 20)))?;
    Ok(v)
}

/// Remove the trailing `alpha` bytes of every `color + alpha`-byte pixel, in place.
fn drop_alpha(data: &mut Vec<u8>, color: usize, alpha: usize) {
    let stride = color + alpha;
    let n = data.len() / stride;
    for i in 0..n {
        data.copy_within(i * stride..i * stride + color, i * color);
    }
    data.truncate(n * color);
    data.shrink_to_fit();
}

/// Whether every pixel of `s` over `r` is fully opaque (read from the tiles, no copy).
fn opaque_surface(s: &Surface, r: Rect) -> bool {
    let fmt = s.format();
    let (ch, bps) = (fmt.channels(), fmt.sample.bytes());
    let a = ch - 1;
    let alpha_ok = |px: &[u8]| photocraft_color::read_sample(px, fmt.sample, a) >= 1.0;
    let mut default = vec![0u8; ch * bps];
    photocraft_raster::encode_pixel(&fmt, &s.default_pixel(), &mut default);
    r.tiles().all(|tc| {
        let tr = tc.rect().intersect(&r);
        match s.tile(tc) {
            None => alpha_ok(&default),
            Some(t) => (tr.y0..tr.y1).all(|y| {
                let row = ((y - tc.rect().y0) as usize * TILE_SIZE as usize + (tr.x0 - tc.rect().x0) as usize) * ch * bps;
                t.bytes()[row..row + tr.width() as usize * ch * bps].chunks_exact(ch * bps).all(alpha_ok)
            }),
        }
    })
}

/// Flattens and encodes as `format`.
pub fn export_flat(doc: &Document, format: Format, opts: &ExportOptions) -> Result<ExportResult, IoError> {
    if let Some(r) = export_mode_specific(doc, format, opts)? {
        return Ok(r);
    }
    let mut warnings = Vec::new();
    let mut img = document_to_image(doc, &mut warnings)?;
    if img.layout().is_cmyk() && !format.caps().layouts.iter().any(|l| l.is_cmyk()) {
        img = cmyk_image_to_srgb(&img)?;
        warnings.push(format!("CMYK converted to sRGB for {format:?} through the document's colour profile"));
    }
    if matches!(format, Format::OpenExr | Format::Hdr) {
        img = rgb_image_to_linear(img)?;
    }
    for w in codecs::fidelity_warnings_with(&img, format, &opts.encode) {
        if w.is_fatal() {
            return Err(IoError::Unsupported(w.to_string()));
        }
        warnings.push(w.to_string());
    }
    let bytes = codecs::encode(&img, format, &opts.encode)?;
    Ok(ExportResult { bytes, warnings })
}

/// Colour-managed CMYK → sRGB for formats that cannot store CMYK (the document's embedded
/// CMYK profile when it parses, else the built-in coated CMYK; relative colorimetric + BPC).
fn cmyk_image_to_srgb(img: &Image) -> Result<Image, IoError> {
    use photocraft_cms::{Builtin, ColorSpace, Intent, Profile, Transform};
    let src = img
        .icc
        .as_ref()
        .and_then(|b| Profile::parse(b).ok())
        .filter(|p| p.color_space == ColorSpace::Cmyk)
        .unwrap_or_else(|| Builtin::CoatedCmyk.profile().clone());
    let dst = Builtin::Srgb.profile();
    let t = Transform::new(&src, dst, Intent::RelativeColorimetric, true).map_err(|e| IoError::Unsupported(e.to_string()))?;
    let alpha = img.layout().has_alpha();
    let (ss, ds) = (if alpha { 5 } else { 4 }, if alpha { 4 } else { 3 });
    let vals = img.to_normalized();
    let mut out = vec![0.0f32; img.pixel_count() * ds];
    t.convert_f32(&vals, ss, &mut out, ds, true);
    let (w, h) = img.dimensions();
    let layout = if alpha { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    let sample = match img.sample_type() {
        CSample::F16 => CSample::F32,
        s => s,
    };
    Ok(Image::from_normalized(w, h, layout, sample, &out)?.with_icc(Some(dst.to_bytes().to_vec())).with_meta(img.meta.clone()))
}

/// OpenEXR and Radiance HDR store linear light (read back as linear sRGB, see [`import_flat`]):
/// RGB pixels in another profile (sRGB when untagged) are converted to linear sRGB, unclamped.
fn rgb_image_to_linear(img: Image) -> Result<Image, IoError> {
    use photocraft_cms::{Builtin, ColorSpace, Intent, Profile, Transform};
    if !matches!(img.layout(), ChannelLayout::Rgb | ChannelLayout::Rgba) {
        return Ok(img);
    }
    let linear = Builtin::LinearSrgb.profile();
    let src =
        img.icc.as_ref().and_then(|b| Profile::parse(b).ok()).filter(|p| p.color_space == ColorSpace::Rgb).unwrap_or_else(|| Builtin::Srgb.profile().clone());
    if src.content_hash() == linear.content_hash() {
        return Ok(img);
    }
    let t = Transform::new(&src, linear, Intent::RelativeColorimetric, false).map_err(|e| IoError::Unsupported(e.to_string()))?;
    let stride = img.layout().channels();
    let mut vals = img.to_normalized();
    t.apply(&mut vals, stride);
    let (w, h) = img.dimensions();
    Ok(Image::from_normalized(w, h, img.layout(), CSample::F32, &vals)?.with_icc(None).with_meta(img.meta.clone()))
}

/// Indexed Color → PNG-8 with its colour table; Duotone → the inks rendered as RGB.
fn export_mode_specific(doc: &Document, format: Format, opts: &ExportOptions) -> Result<Option<ExportResult>, IoError> {
    match doc.mode {
        ColorMode::Indexed if format == Format::Png => {
            let Some(table) = doc.color_table.as_ref().filter(|t| !t.colors.is_empty() && t.colors.len() <= 256) else { return Ok(None) };
            let mut idx = try_buffer(doc.size.area() as usize, 1)?;
            let _ = photocraft_compose::render_bands(doc, doc.bounds(), 0, |band| -> Result<(), ()> {
                idx.extend(band.px.iter().map(|p| match table.transparent {
                    Some(t) if p[3] < 0.5 => t,
                    _ => table.nearest([p[0], p[1], p[2]]) as u8,
                }));
                Ok(())
            });
            let bytes = codecs::encode_png_indexed(doc.size.width, doc.size.height, &idx, &table.colors, table.transparent)?;
            Ok(Some(ExportResult { bytes, warnings: vec![format!("written as an 8-bit palette PNG ({} colours)", table.colors.len())] }))
        }
        ColorMode::Duotone => {
            let Some(d) = doc.duotone.as_ref() else { return Ok(None) };
            let mut shown = doc.clone();
            shown.layers.push(photocraft_doc::Layer::new("Duotone", photocraft_doc::LayerContent::Adjustment(d.display_adjustment())));
            shown.mode = ColorMode::Rgb;
            shown.icc_profile = None;
            shown.duotone = None;
            let mut r = export_flat(&shown, format, opts)?;
            r.warnings.retain(|w| !w.contains("flattened"));
            r.warnings.push(format!("Duotone ({} inks) written as RGB", d.inks.len()));
            Ok(Some(r))
        }
        _ => Ok(None),
    }
}

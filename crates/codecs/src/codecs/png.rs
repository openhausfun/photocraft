//! PNG via the `png` crate directly: 8/16-bit, Adam7 read (and write),
//! iCCP, eXIf, tEXt/zTXt/iTXt (XMP in `XML:com.adobe.xmp`), pHYs.

use std::io::{Cursor, Write};

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, Metadata, SampleType};
use crate::options::{EncodeOptions, Limits, PngCompression};

const F: Format = Format::Png;
pub(crate) const XMP_KEYWORD: &str = "XML:com.adobe.xmp";
const METERS_PER_INCH: f32 = 0.0254;

fn err(e: impl std::fmt::Display) -> CodecError {
    CodecError::malformed(F, e)
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, CodecError> {
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: limits.alloc_usize() });
    decoder.set_transformations(png::Transformations::EXPAND);
    {
        let info = decoder.read_header_info().map_err(map_png_err)?;
        let (w, h) = info.size();
        // Worst case output: 4 channels x 16 bit.
        limits.check_bytes(w, h, 8)?;
    }
    let mut reader = decoder.read_info().map_err(map_png_err)?;
    let (color, depth) = reader.output_color_type();
    let layout = match color {
        png::ColorType::Grayscale => ChannelLayout::Gray,
        png::ColorType::GrayscaleAlpha => ChannelLayout::GrayA,
        png::ColorType::Rgb => ChannelLayout::Rgb,
        png::ColorType::Rgba => ChannelLayout::Rgba,
        png::ColorType::Indexed => return Err(err("palette was not expanded")),
    };
    let sample = match depth {
        png::BitDepth::Eight => SampleType::U8,
        png::BitDepth::Sixteen => SampleType::U16,
        d => return Err(err(format!("unexpected output bit depth {d:?}"))),
    };
    let size = reader.output_buffer_size().ok_or_else(|| err("image too large"))?;
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(map_png_err)?;
    buf.truncate(out.buffer_size());
    // Collect chunks after IDAT (text may live there). Errors here are
    // tolerated: pixel data is already complete.
    let _ = reader.finish();
    if sample == SampleType::U16 {
        for c in buf.as_chunks_mut::<2>().0 {
            let v = u16::from_be_bytes([c[0], c[1]]);
            c.copy_from_slice(&v.to_ne_bytes());
        }
    }
    let info = reader.info();
    let mut img = Image::from_raw(out.width, out.height, layout, sample, buf)?;
    img.icc = info.icc_profile.as_ref().map(|c| c.to_vec());
    let mut meta = Metadata { exif: info.exif_metadata.as_ref().map(|c| c.to_vec()), ..Default::default() };
    if let Some(d) = info.pixel_dims
        && d.unit == png::Unit::Meter
        && d.xppu > 0
        && d.yppu > 0
    {
        meta.dpi = Some((d.xppu as f32 * METERS_PER_INCH, d.yppu as f32 * METERS_PER_INCH));
    }
    for t in &info.uncompressed_latin1_text {
        meta.text.push((t.keyword.clone(), t.text.clone()));
    }
    for t in &info.compressed_latin1_text {
        if let Ok(s) = t.get_text() {
            meta.text.push((t.keyword.clone(), s));
        }
    }
    for t in &info.utf8_text {
        if let Ok(s) = t.get_text() {
            if t.keyword == XMP_KEYWORD {
                meta.xmp = Some(s);
            } else {
                meta.text.push((t.keyword.clone(), s));
            }
        }
    }
    img.meta = meta;
    Ok(img)
}

fn map_png_err(e: png::DecodingError) -> CodecError {
    match e {
        png::DecodingError::LimitsExceeded => CodecError::LimitExceeded("PNG decoder memory limit".into()),
        e => err(e),
    }
}

fn is_latin1_keyword(k: &str) -> bool {
    !k.is_empty() && k.len() <= 79 && k.chars().all(|c| (' '..='~').contains(&c) || ('\u{a1}'..='\u{ff}').contains(&c))
}

pub(crate) fn encode(src: &Image, plan: Plan, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let img = src.converted(plan.layout, plan.sample);
    let (w, h) = img.dimensions();
    let color = match img.layout() {
        ChannelLayout::Gray => png::ColorType::Grayscale,
        ChannelLayout::GrayA => png::ColorType::GrayscaleAlpha,
        ChannelLayout::Rgb => png::ColorType::Rgb,
        ChannelLayout::Rgba => png::ColorType::Rgba,
        l => return Err(CodecError::encode(F, format!("unsupported layout {l:?}"))),
    };
    let depth = match img.sample_type() {
        SampleType::U8 => png::BitDepth::Eight,
        SampleType::U16 => png::BitDepth::Sixteen,
        s => return Err(CodecError::encode(F, format!("unsupported sample {s:?}"))),
    };
    // Big-endian sample bytes.
    let mut data = std::borrow::Cow::Borrowed(img.data());
    if depth == png::BitDepth::Sixteen {
        for c in data.to_mut().as_chunks_mut::<2>().0 {
            let v = u16::from_ne_bytes([c[0], c[1]]);
            c.copy_from_slice(&v.to_be_bytes());
        }
    }

    let mut info = png::Info::with_size(w, h);
    info.color_type = color;
    info.bit_depth = depth;
    info.interlaced = opts.png_interlaced;
    if opts.embed_icc
        && let Some(icc) = &img.icc
    {
        info.icc_profile = Some(icc.clone().into());
    }
    if opts.embed_metadata {
        if let Some(exif) = &img.meta.exif {
            // The pixels are written as they are shown: never let a viewer rotate them again.
            info.exif_metadata = Some(crate::orientation::upright_exif(exif).into_owned().into());
        }
        if let Some((x, y)) = img.meta.dpi
            && x > 0.0
            && y > 0.0
        {
            info.pixel_dims =
                Some(png::PixelDimensions { xppu: (x / METERS_PER_INCH).round() as u32, yppu: (y / METERS_PER_INCH).round() as u32, unit: png::Unit::Meter });
        }
    }

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::with_info(&mut out, info).map_err(|e| CodecError::encode(F, e))?;
        encoder.set_compression(match opts.png_compression {
            PngCompression::None => png::Compression::NoCompression,
            PngCompression::Fast => png::Compression::Fast,
            PngCompression::Default => png::Compression::Balanced,
            PngCompression::Best => png::Compression::High,
        });
        if opts.embed_metadata {
            for (k, v) in &img.meta.text {
                let latin1_text = v.chars().all(|c| (c as u32) < 256);
                let res = if is_latin1_keyword(k) && latin1_text {
                    encoder.add_text_chunk(k.clone(), v.clone())
                } else if is_latin1_keyword(k) {
                    encoder.add_itxt_chunk(k.clone(), v.clone())
                } else {
                    continue;
                };
                res.map_err(|e| CodecError::encode(F, e))?;
            }
            if let Some(xmp) = &img.meta.xmp {
                encoder.add_itxt_chunk(XMP_KEYWORD.into(), crate::orientation::upright_xmp(xmp).into_owned()).map_err(|e| CodecError::encode(F, e))?;
            }
        }
        let mut writer = encoder.write_header().map_err(|e| CodecError::encode(F, e))?;
        if opts.png_interlaced {
            let bpp = img.layout().channels() * img.sample_type().bytes();
            let idat = adam7_idat(&data, w as usize, h as usize, bpp, opts.png_compression)?;
            writer.write_chunk(png::chunk::IDAT, &idat).map_err(|e| CodecError::encode(F, e))?;
        } else if let Some(idat) = parallel_idat(&data, w as usize, h as usize, img.layout().channels() * img.sample_type().bytes(), opts.png_compression) {
            writer.write_chunk(png::chunk::IDAT, &idat).map_err(|e| CodecError::encode(F, e))?;
        } else {
            writer.write_image_data(&data).map_err(|e| CodecError::encode(F, e))?;
        }
        writer.finish().map_err(|e| CodecError::encode(F, e))?;
    }
    Ok(out)
}

/// Build the zlib stream for an Adam7-interlaced image (filter type 0).
fn adam7_idat(data: &[u8], w: usize, h: usize, bpp: usize, level: PngCompression) -> Result<Vec<u8>, CodecError> {
    // (x0, y0, dx, dy) per pass.
    const PASSES: [(usize, usize, usize, usize); 7] = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
    let mut raw = Vec::with_capacity(data.len() + h * 7);
    for (x0, y0, dx, dy) in PASSES {
        if x0 >= w || y0 >= h {
            continue;
        }
        let mut y = y0;
        while y < h {
            raw.push(0u8);
            let mut x = x0;
            while x < w {
                let o = (y * w + x) * bpp;
                raw.extend_from_slice(&data[o..o + bpp]);
                x += dx;
            }
            y += dy;
        }
    }
    let lvl = match level {
        PngCompression::None => flate2::Compression::none(),
        PngCompression::Fast => flate2::Compression::fast(),
        PngCompression::Default => flate2::Compression::default(),
        PngCompression::Best => flate2::Compression::best(),
    };
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), lvl);
    z.write_all(&raw).map_err(|e| CodecError::encode(F, e))?;
    z.finish().map_err(|e| CodecError::encode(F, e))
}

/// Images at least this large (bytes) are filtered and deflated on all cores.
const PARALLEL_MIN_BYTES: usize = 4 << 20;

/// The zlib stream of a non-interlaced image, built in parallel (`None` below
/// [`PARALLEL_MIN_BYTES`], on wasm, or without compression): rows get the adaptive filter
/// (minimum sum of absolute differences over the five PNG filters, as libpng and the `png`
/// crate do), then bands of rows are deflated independently at the requested level, each
/// ending on a sync flush so the raw deflate streams concatenate into one (as `pigz` does).
/// A 36 MP RGB export drops from ~9 s to well under a second on 12 cores; the output is a few
/// percent larger (no dictionary across bands).
fn parallel_idat(data: &[u8], w: usize, h: usize, bpp: usize, level: PngCompression) -> Option<Vec<u8>> {
    if cfg!(target_arch = "wasm32") || data.len() < PARALLEL_MIN_BYTES || level == PngCompression::None || w == 0 || h == 0 {
        return None;
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 32);
    if threads < 2 {
        return None;
    }
    let stride = w * bpp;
    let band_rows = h.div_ceil(threads * 4).max(1);
    let bands: Vec<(usize, usize)> = (0..h).step_by(band_rows).map(|y| (y, (y + band_rows).min(h))).collect();
    let lvl = match level {
        PngCompression::Fast => flate2::Compression::fast(),
        PngCompression::Best => flate2::Compression::best(),
        _ => flate2::Compression::default(),
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    type Band = (usize, Vec<u8>, u32, usize);
    let mut parts: Vec<Band> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..threads.min(bands.len()))
            .map(|_| {
                sc.spawn(|| {
                    let mut out: Vec<Band> = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(y0, y1)) = bands.get(i) else { break };
                        let mut filtered = Vec::with_capacity((y1 - y0) * (stride + 1));
                        let mut scratch = vec![0u8; stride];
                        for y in y0..y1 {
                            let row = &data[y * stride..(y + 1) * stride];
                            let prev = (y > 0).then(|| &data[(y - 1) * stride..y * stride]);
                            filter_row(row, prev, bpp, &mut scratch, &mut filtered);
                        }
                        let adler = adler32(1, &filtered);
                        let mut c = flate2::Compress::new(lvl, false);
                        let mut z = Vec::with_capacity(filtered.len() / 2 + 64);
                        let last = y1 == h;
                        let flush = if last { flate2::FlushCompress::Finish } else { flate2::FlushCompress::Sync };
                        loop {
                            let consumed = c.total_in() as usize;
                            if z.capacity() - z.len() < 64 * 1024 {
                                z.reserve(z.capacity().max(64 * 1024));
                            }
                            match c.compress_vec(&filtered[consumed..], &mut z, flush) {
                                Ok(flate2::Status::StreamEnd) => break,
                                Ok(_) if !last && c.total_in() as usize == filtered.len() && z.capacity() > z.len() => break,
                                Ok(_) => {}
                                Err(_) => return Vec::new(),
                            }
                        }
                        out.push((i, z, adler, filtered.len()));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    if parts.len() != bands.len() {
        return None;
    }
    parts.sort_by_key(|p| p.0);
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.1.len()).sum::<usize>() + 6);
    // zlib header: deflate, 32 K window; FLEVEL only advertises the effort.
    out.extend_from_slice(match level {
        PngCompression::Fast => &[0x78, 0x01],
        PngCompression::Best => &[0x78, 0xDA],
        _ => &[0x78, 0x9C],
    });
    let mut adler = 1u32;
    for (_, z, a, len) in &parts {
        out.extend_from_slice(z);
        adler = adler32_combine(adler, *a, *len);
    }
    out.extend_from_slice(&adler.to_be_bytes());
    Some(out)
}

/// One scanline with the adaptive filter choice, appended to `out` (type byte + data).
fn filter_row(row: &[u8], prev: Option<&[u8]>, bpp: usize, scratch: &mut [u8], out: &mut Vec<u8>) {
    let n = row.len();
    let up = |i: usize| prev.map_or(0, |p| p[i]);
    let left = |i: usize| if i >= bpp { row[i - bpp] } else { 0 };
    let ul = |i: usize| if i >= bpp { prev.map_or(0, |p| p[i - bpp]) } else { 0 };
    let paeth = |a: u8, b: u8, c: u8| {
        let p = i16::from(a) + i16::from(b) - i16::from(c);
        let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    };
    let cost = |v: &[u8]| v.iter().map(|&b| u64::from((b as i8).unsigned_abs())).sum::<u64>();
    let mut best = (cost(row), 0u8);
    for kind in 1u8..=4 {
        for i in 0..n {
            let x = row[i];
            scratch[i] = match kind {
                1 => x.wrapping_sub(left(i)),
                2 => x.wrapping_sub(up(i)),
                3 => x.wrapping_sub(((u16::from(left(i)) + u16::from(up(i))) / 2) as u8),
                _ => x.wrapping_sub(paeth(left(i), up(i), ul(i))),
            };
        }
        let c = cost(&scratch[..n]);
        if c < best.0 {
            best = (c, kind);
        }
    }
    out.push(best.1);
    if best.1 == 0 {
        out.extend_from_slice(row);
        return;
    }
    for i in 0..n {
        let x = row[i];
        scratch[i] = match best.1 {
            1 => x.wrapping_sub(left(i)),
            2 => x.wrapping_sub(up(i)),
            3 => x.wrapping_sub(((u16::from(left(i)) + u16::from(up(i))) / 2) as u8),
            _ => x.wrapping_sub(paeth(left(i), up(i), ul(i))),
        };
    }
    out.extend_from_slice(&scratch[..n]);
}

const ADLER_MOD: u32 = 65_521;

fn adler32(start: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (start & 0xffff, start >> 16);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= ADLER_MOD;
        b %= ADLER_MOD;
    }
    (b << 16) | a
}

/// Adler-32 of A ‖ B from adler(A), adler(B) and |B| (zlib's `adler32_combine`).
fn adler32_combine(a1: u32, a2: u32, len2: usize) -> u32 {
    let rem = (len2 % ADLER_MOD as usize) as u64;
    let m = u64::from(ADLER_MOD);
    let (s1a, s2a) = (u64::from(a1 & 0xffff), u64::from(a1 >> 16));
    let (s1b, s2b) = (u64::from(a2 & 0xffff), u64::from(a2 >> 16));
    let s1 = (s1a + s1b + m - 1) % m;
    let s2 = (s2a + s2b + (rem * s1a) % m + m - rem) % m;
    ((s2 as u32) << 16) | s1 as u32
}

/// Writes an 8-bit palette PNG (PNG-8): `indices` row-major, `palette` RGB entries (≤ 256),
/// optional fully transparent entry (`tRNS`). Used for Indexed Color documents.
pub fn encode_indexed(width: u32, height: u32, indices: &[u8], palette: &[[u8; 3]], transparent: Option<u8>) -> Result<Vec<u8>, CodecError> {
    if width == 0 || height == 0 || indices.len() != width as usize * height as usize {
        return Err(CodecError::InvalidImage("index data does not match the size".into()));
    }
    if palette.is_empty() || palette.len() > 256 || indices.iter().any(|&i| usize::from(i) >= palette.len()) {
        return Err(CodecError::InvalidImage("palette must have 1..=256 entries covering every index".into()));
    }
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Indexed;
    info.bit_depth = png::BitDepth::Eight;
    info.palette = Some(palette.iter().flatten().copied().collect::<Vec<u8>>().into());
    if let Some(t) = transparent.filter(|&t| usize::from(t) < palette.len()) {
        let mut trns = vec![255u8; usize::from(t) + 1];
        trns[usize::from(t)] = 0;
        info.trns = Some(trns.into());
    }
    let mut out = Vec::new();
    {
        let encoder = png::Encoder::with_info(&mut out, info).map_err(|e| CodecError::encode(F, e))?;
        let mut writer = encoder.write_header().map_err(|e| CodecError::encode(F, e))?;
        writer.write_image_data(indices).map_err(|e| CodecError::encode(F, e))?;
        writer.finish().map_err(|e| CodecError::encode(F, e))?;
    }
    Ok(out)
}

#[cfg(test)]
mod indexed_tests {
    use super::*;

    #[test]
    fn png8_roundtrips_through_the_decoder() {
        let pal = [[255, 0, 0], [0, 0, 255], [0, 0, 0]];
        let idx = [0u8, 1, 2, 1, 0, 2];
        let bytes = encode_indexed(3, 2, &idx, &pal, Some(2)).unwrap();
        let img = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!(img.dimensions(), (3, 2));
        let px = img.convert(ChannelLayout::Rgba, SampleType::U8);
        assert_eq!(&px.data()[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(px.data()[11], 0, "transparent entry");
        assert!(encode_indexed(3, 2, &[9; 6], &pal, None).is_err());
        assert!(encode_indexed(3, 3, &idx, &pal, None).is_err());
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;

    #[test]
    fn adler_combine_matches_one_pass() {
        let a: Vec<u8> = (0..100_000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let b: Vec<u8> = (0..70_001u32).map(|i| (i * 31) as u8).collect();
        let whole: Vec<u8> = a.iter().chain(&b).copied().collect();
        assert_eq!(adler32_combine(adler32(1, &a), adler32(1, &b), b.len()), adler32(1, &whole));
    }

    #[test]
    fn large_images_roundtrip_through_the_parallel_encoder() {
        for (layout, sample, ch, bytes) in [(ChannelLayout::Rgb, SampleType::U8, 3usize, 1usize), (ChannelLayout::Rgba, SampleType::U16, 4, 2)] {
            let (w, h) = (1531u32, 977u32);
            let n = w as usize * h as usize * ch * bytes;
            let px: Vec<u8> = (0..n).map(|i| ((i / 3) as u32).wrapping_mul(2_654_435_761).rotate_left(i as u32 % 7) as u8 / 3 + (i % 97) as u8).collect();
            let img = Image::from_raw(w, h, layout, sample, px.clone()).unwrap();
            assert!(n >= PARALLEL_MIN_BYTES || sample == SampleType::U8);
            for level in [PngCompression::Fast, PngCompression::Default] {
                let opts = EncodeOptions { png_compression: level, ..Default::default() };
                let bytes = crate::encode(&img, Format::Png, &opts).unwrap();
                let back = decode(&bytes, &Limits::default()).unwrap();
                assert_eq!(back.dimensions(), (w, h));
                assert_eq!(back.convert(layout, sample).data(), img.data(), "{layout:?} {sample:?} {level:?}");
                // The oracle decoder agrees.
                let o = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
                assert_eq!((o.width(), o.height()), (w, h));
            }
        }
    }
}

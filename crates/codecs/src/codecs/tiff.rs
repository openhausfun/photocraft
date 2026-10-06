//! TIFF via the `tiff` crate: 8/16-bit integer and 32-bit float, gray/RGB/
//! CMYK with optional (unassociated) alpha, ICC (34675), XMP (700), DPI and
//! a few ASCII text tags. Only the first IFD is read.

use std::borrow::Cow;
use std::io::Cursor;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::encoder::colortype::{self, ColorType as TiffColorType};
use tiff::encoder::{Rational, TiffEncoder, TiffValue};
use tiff::tags::{PhotometricInterpretation, ResolutionUnit, SampleFormat, Tag, Type};

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, Metadata, SampleType};
use crate::options::{EncodeOptions, Limits, TiffCompression};

const F: Format = Format::Tiff;
const TAG_XMP: u16 = 700;

/// ASCII tags mapped to `Metadata::text` keys.
const TEXT_TAGS: &[(Tag, &str)] = &[
    (Tag::ImageDescription, "Description"),
    (Tag::Make, "Make"),
    (Tag::Model, "Model"),
    (Tag::Software, "Software"),
    (Tag::DateTime, "DateTime"),
    (Tag::Artist, "Artist"),
    (Tag::Copyright, "Copyright"),
];

fn err(e: impl std::fmt::Display) -> CodecError {
    CodecError::malformed(F, e)
}

fn map_err(e: tiff::TiffError) -> CodecError {
    match e {
        tiff::TiffError::LimitsExceeded => CodecError::LimitExceeded("TIFF decoder limit".into()),
        tiff::TiffError::UnsupportedError(u) => CodecError::unsupported(F, u.to_string()),
        e => err(e),
    }
}

fn value_bytes(v: tiff::decoder::ifd::Value) -> Option<Vec<u8>> {
    use tiff::decoder::ifd::Value;
    match v {
        Value::Byte(b) => Some(vec![b]),
        Value::Ascii(s) => Some(s.into_bytes()),
        Value::List(l) => l
            .into_iter()
            .map(|v| match v {
                Value::Byte(b) => Some(b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// TIFF/EP `PhotometricInterpretation` for colour filter array (sensor) data.
const PHOTOMETRIC_CFA: u32 = 32803;
/// DNG `PhotometricInterpretation` for demosaiced but undeveloped sensor data.
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_SUB_IFDS: u16 = 330;
const TAG_DNG_VERSION: u16 = 50706;

/// Recognizes TIFF-structured camera raw files (TIFF/EP, DNG, CR2 and the
/// TIFF-based NEF/ARW/PEF… layouts): a CR2 signature, a DNGVersion tag, or
/// a CFA / LinearRaw image in IFD0, its SubIFDs or the next few IFDs. Their
/// first IFD is often a reduced preview, so this runs before decoding.
/// Reads only bounds-checked header bytes; anything malformed is "not raw".
fn camera_raw(b: &[u8]) -> bool {
    let le = match b.get(0..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return false, // BigTIFF and others: no raw layouts to recognize
    };
    let u16_at = |o: usize| {
        let s = b.get(o..o.checked_add(2)?)?;
        Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    };
    let u32_at = |o: usize| {
        let s: [u8; 4] = b.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    };
    // CR2: "CR" and major version 2 right after the TIFF header.
    if b.get(8..11) == Some(b"CR\x02") {
        return true;
    }
    // Entry value (count 1) as SHORT or LONG; anything else is ignored.
    let value = |e: usize| match u16_at(e.saturating_add(2))? {
        3 => u16_at(e.saturating_add(8)).map(u32::from),
        4 | 13 => u32_at(e.saturating_add(8)),
        _ => None,
    };
    let mut queue: Vec<u32> = u32_at(4).into_iter().collect();
    let mut visited = 0;
    while let Some(ifd) = queue.pop() {
        // A handful of IFDs is enough for every real layout; also stops loops.
        visited += 1;
        if visited > 16 {
            break;
        }
        let ifd = ifd as usize;
        let Some(n) = u16_at(ifd) else { continue };
        for i in 0..usize::from(n) {
            let e = ifd.saturating_add(2 + 12 * i);
            let (Some(tag), Some(count)) = (u16_at(e), u32_at(e.saturating_add(4))) else {
                break;
            };
            match tag {
                TAG_DNG_VERSION => return true,
                TAG_PHOTOMETRIC if matches!(value(e), Some(PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW)) => {
                    return true;
                }
                TAG_SUB_IFDS if count == 1 => queue.extend(value(e)),
                TAG_SUB_IFDS => {
                    // More than one offset: they are stored at the entry's offset.
                    let at = u32_at(e.saturating_add(8)).unwrap_or(0) as usize;
                    for k in 0..count.min(8) as usize {
                        queue.extend(u32_at(at.saturating_add(4 * k)));
                    }
                }
                _ => {}
            }
        }
        if let Some(next) = u32_at(ifd.saturating_add(2 + 12 * usize::from(n))).filter(|&o| o != 0) {
            queue.push(next);
        }
    }
    false
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, CodecError> {
    if camera_raw(bytes) {
        return Err(CodecError::unsupported(F, "camera raw files (such as CR2, NEF, ARW or DNG) are not flat images; decode them with photocraft-raw"));
    }
    let tl = {
        let mut l = tiff::decoder::Limits::default();
        l.decoding_buffer_size = limits.alloc_usize();
        l.intermediate_buffer_size = limits.alloc_usize().min(1 << 30);
        l
    };
    let mut dec = Decoder::new(Cursor::new(bytes)).map_err(map_err)?.with_limits(tl);
    let (w, h) = dec.dimensions().map_err(map_err)?;
    let ct = dec.colortype().map_err(map_err)?;
    let (layout, bits) = match ct {
        tiff::ColorType::Gray(b) => (ChannelLayout::Gray, b),
        tiff::ColorType::GrayA(b) => (ChannelLayout::GrayA, b),
        tiff::ColorType::RGB(b) => (ChannelLayout::Rgb, b),
        tiff::ColorType::RGBA(b) => (ChannelLayout::Rgba, b),
        tiff::ColorType::CMYK(b) => (ChannelLayout::Cmyk, b),
        tiff::ColorType::CMYKA(b) => (ChannelLayout::CmykA, b),
        // The decoder reports gray+alpha as multiband (it ignores ExtraSamples).
        tiff::ColorType::Multiband { bit_depth, num_samples: 2 } => (ChannelLayout::GrayA, bit_depth),
        other => return Err(CodecError::unsupported(F, format!("colour type {other:?}"))),
    };
    let bytes_per_sample = u64::from(bits.max(8) / 8);
    limits.check_bytes(w, h, layout.channels() as u64 * bytes_per_sample.max(1))?;
    let white_is_zero = dec.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation).ok().flatten() == Some(PhotometricInterpretation::WhiteIsZero.to_u16());

    let result = dec.read_image().map_err(map_err)?;
    let n = w as usize * h as usize * layout.channels();
    let mut img = match result {
        DecodingResult::U8(v) if bits == 8 => Image::from_u8(w, h, layout, v)?,
        DecodingResult::U8(v) if bits < 8 && layout == ChannelLayout::Gray => Image::from_u8(w, h, layout, unpack_bits(&v, w as usize, h as usize, bits)?)?,
        DecodingResult::U16(v) => Image::from_u16(w, h, layout, &v)?,
        DecodingResult::U32(v) => {
            let v: Vec<u16> = v.iter().map(|&x| (x >> 16) as u16).collect();
            Image::from_u16(w, h, layout, &v)?
        }
        DecodingResult::F16(v) => Image::from_f16(w, h, layout, &v)?,
        DecodingResult::F32(v) => Image::from_f32(w, h, layout, &v)?,
        DecodingResult::F64(v) => {
            let v: Vec<f32> = v.iter().map(|&x| x as f32).collect();
            Image::from_f32(w, h, layout, &v)?
        }
        _ => {
            return Err(CodecError::unsupported(F, format!("sample format for {ct:?}")));
        }
    };
    if img.sample_count() != n {
        return Err(err("sample count mismatch"));
    }
    if white_is_zero && layout.is_gray() {
        let v: Vec<f32> = img
            .to_normalized()
            .chunks(layout.channels())
            .flat_map(|p| {
                let mut p = p.to_vec();
                p[0] = 1.0 - p[0];
                p
            })
            .collect();
        img = Image::from_normalized(w, h, layout, img.sample_type(), &v)?;
    }

    img.icc = dec.find_tag(Tag::IccProfile).ok().flatten().and_then(value_bytes);
    let mut meta = Metadata {
        xmp: dec
            .find_tag(Tag::Unknown(TAG_XMP))
            .ok()
            .flatten()
            .and_then(value_bytes)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim_end_matches('\0').to_owned()),
        ..Default::default()
    };
    let unit = dec.find_tag_unsigned::<u16>(Tag::ResolutionUnit).ok().flatten().unwrap_or(2);
    let xr = dec.find_tag(Tag::XResolution).ok().flatten().and_then(rational_f64);
    let yr = dec.find_tag(Tag::YResolution).ok().flatten().and_then(rational_f64);
    if let (Some(x), Some(y)) = (xr, yr)
        && x > 0.0
        && y > 0.0
    {
        meta.dpi = match unit {
            2 => Some((x as f32, y as f32)),
            3 => Some(((x * 2.54) as f32, (y * 2.54) as f32)),
            _ => None,
        };
    }
    for (tag, key) in TEXT_TAGS {
        if let Some(s) = dec.find_tag(*tag).ok().flatten().and_then(|v| v.into_string().ok()) {
            let s = s.trim_end_matches('\0').to_owned();
            if !s.is_empty() {
                meta.text.push(((*key).to_owned(), s));
            }
        }
    }
    img.meta = meta;
    Ok(img)
}

fn rational_f64(v: tiff::decoder::ifd::Value) -> Option<f64> {
    use tiff::decoder::ifd::Value;
    match v {
        Value::Rational(n, d) if d != 0 => Some(n as f64 / d as f64),
        Value::Float(f) => Some(f as f64),
        Value::Double(f) => Some(f),
        Value::Unsigned(u) => Some(u as f64),
        Value::Short(u) => Some(u as f64),
        Value::List(mut l) if l.len() == 1 => rational_f64(l.remove(0)),
        _ => None,
    }
}

fn unpack_bits(packed: &[u8], w: usize, h: usize, bits: u8) -> Result<Vec<u8>, CodecError> {
    let bits = bits as usize;
    if !matches!(bits, 1 | 2 | 4) {
        return Err(CodecError::unsupported(F, format!("{bits}-bit gray")));
    }
    let row_bytes = (w * bits).div_ceil(8);
    if packed.len() < row_bytes * h {
        return Err(err("short bilevel data"));
    }
    let max = (1u32 << bits) - 1;
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = &packed[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w {
            let bit = x * bits;
            let v = (row[bit / 8] >> (8 - bits - bit % 8)) as u32 & max;
            out.push((v * 255 / max) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Bytes written with TIFF type UNDEFINED (7), as required for ICC.
struct Undefined<'a>(&'a [u8]);

impl TiffValue for Undefined<'_> {
    const BYTE_LEN: u8 = 1;
    const FIELD_TYPE: Type = Type::UNDEFINED;
    fn count(&self) -> usize {
        self.0.len()
    }
    fn data(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.0)
    }
}

macro_rules! custom_colortype {
    ($name:ident, $inner:ty, $photo:expr, $bits:expr, $fmt:expr, int) => {
        struct $name;
        impl TiffColorType for $name {
            type Inner = $inner;
            const TIFF_VALUE: PhotometricInterpretation = $photo;
            const BITS_PER_SAMPLE: &'static [u16] = $bits;
            const SAMPLE_FORMAT: &'static [SampleFormat] = $fmt;
            fn horizontal_predict(row: &[Self::Inner], result: &mut Vec<Self::Inner>) {
                let n = Self::SAMPLE_FORMAT.len();
                let n = n.min(row.len());
                result.extend_from_slice(&row[..n]);
                result.extend(row.iter().zip(&row[n..]).map(|(p, c)| c.wrapping_sub(*p)));
            }
        }
    };
    ($name:ident, $inner:ty, $photo:expr, $bits:expr, $fmt:expr, float) => {
        struct $name;
        impl TiffColorType for $name {
            type Inner = $inner;
            const TIFF_VALUE: PhotometricInterpretation = $photo;
            const BITS_PER_SAMPLE: &'static [u16] = $bits;
            const SAMPLE_FORMAT: &'static [SampleFormat] = $fmt;
            fn horizontal_predict(row: &[Self::Inner], result: &mut Vec<Self::Inner>) {
                result.extend_from_slice(row);
            }
        }
    };
}

use PhotometricInterpretation as PI;
custom_colortype!(GrayA8, u8, PI::BlackIsZero, &[8, 8], &[SampleFormat::Uint; 2], int);
custom_colortype!(GrayA16, u16, PI::BlackIsZero, &[16, 16], &[SampleFormat::Uint; 2], int);
custom_colortype!(GrayA32F, f32, PI::BlackIsZero, &[32, 32], &[SampleFormat::IEEEFP; 2], float);
custom_colortype!(CmykA16, u16, PI::CMYK, &[16; 5], &[SampleFormat::Uint; 5], int);
custom_colortype!(CmykA32F, f32, PI::CMYK, &[32; 5], &[SampleFormat::IEEEFP; 5], float);

struct TagSet<'a> {
    icc: Option<&'a [u8]>,
    xmp: Option<&'a [u8]>,
    dpi: Option<(f32, f32)>,
    text: Vec<(Tag, &'a str)>,
    alpha: bool,
}

fn write_one<C>(enc: &mut TiffEncoder<&mut Cursor<Vec<u8>>>, w: u32, h: u32, data: &[C::Inner], tags: &TagSet<'_>) -> tiff::TiffResult<()>
where
    C: TiffColorType,
    [C::Inner]: TiffValue,
{
    let mut im = enc.new_image::<C>(w, h)?;
    if let Some((x, y)) = tags.dpi {
        im.resolution_unit(ResolutionUnit::Inch);
        im.x_resolution(Rational { n: (x * 1000.0).round() as u32, d: 1000 });
        im.y_resolution(Rational { n: (y * 1000.0).round() as u32, d: 1000 });
    }
    let d = im.encoder();
    if tags.alpha {
        // 2 = unassociated alpha
        d.write_tag(Tag::ExtraSamples, &[2u16][..])?;
    }
    if let Some(icc) = tags.icc {
        d.write_tag(Tag::IccProfile, Undefined(icc))?;
    }
    if let Some(xmp) = tags.xmp {
        d.write_tag(Tag::Unknown(TAG_XMP), xmp)?;
    }
    for (tag, s) in &tags.text {
        d.write_tag(*tag, *s)?;
    }
    im.write_data(data)
}

pub(crate) fn encode(src: &Image, plan: Plan, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let img = src.converted(plan.layout, plan.sample);
    let (w, h) = img.dimensions();
    let compression = match opts.tiff_compression {
        TiffCompression::None => tiff::encoder::Compression::Uncompressed,
        TiffCompression::Lzw => tiff::encoder::Compression::Lzw,
        TiffCompression::Deflate => tiff::encoder::Compression::Deflate(tiff::encoder::DeflateLevel::Balanced),
        TiffCompression::PackBits => tiff::encoder::Compression::Packbits,
    };
    let predictor = if !img.sample_type().is_float() && matches!(opts.tiff_compression, TiffCompression::Lzw | TiffCompression::Deflate) {
        tiff::encoder::Predictor::Horizontal
    } else {
        tiff::encoder::Predictor::None
    };
    let text: Vec<(Tag, &str)> = if opts.embed_metadata {
        img.meta
            .text
            .iter()
            .filter_map(|(k, v)| TEXT_TAGS.iter().find(|(_, key)| key.eq_ignore_ascii_case(k)).map(|(t, _)| (*t, v.as_str())))
            .filter(|(_, v)| v.is_ascii() && !v.contains('\0'))
            .collect()
    } else {
        Vec::new()
    };
    // TIFF writes no Orientation tag (= 1): keep the XMP from contradicting it.
    let xmp = if opts.embed_metadata { img.meta.xmp.as_deref().map(crate::orientation::upright_xmp) } else { None };
    let tags = TagSet {
        icc: if opts.embed_icc { img.icc.as_deref() } else { None },
        xmp: xmp.as_deref().map(str::as_bytes),
        dpi: if opts.embed_metadata { img.meta.dpi.filter(|d| d.0 > 0.0 && d.1 > 0.0) } else { None },
        text,
        alpha: img.layout().has_alpha(),
    };

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut enc = TiffEncoder::new(&mut cursor).map_err(|e| CodecError::encode(F, e))?.with_compression(compression).with_predictor(predictor);
        use ChannelLayout as L;
        use SampleType as S;
        let u16s = || img.to_u16_samples().unwrap_or_default();
        let f32s = || img.to_f32_samples().unwrap_or_default();
        let d8 = img.data();
        let res = match (img.layout(), img.sample_type()) {
            (L::Gray, S::U8) => write_one::<colortype::Gray8>(&mut enc, w, h, d8, &tags),
            (L::Gray, S::U16) => write_one::<colortype::Gray16>(&mut enc, w, h, &u16s(), &tags),
            (L::Gray, S::F32) => write_one::<colortype::Gray32Float>(&mut enc, w, h, &f32s(), &tags),
            (L::GrayA, S::U8) => write_one::<GrayA8>(&mut enc, w, h, d8, &tags),
            (L::GrayA, S::U16) => write_one::<GrayA16>(&mut enc, w, h, &u16s(), &tags),
            (L::GrayA, S::F32) => write_one::<GrayA32F>(&mut enc, w, h, &f32s(), &tags),
            (L::Rgb, S::U8) => write_one::<colortype::RGB8>(&mut enc, w, h, d8, &tags),
            (L::Rgb, S::U16) => write_one::<colortype::RGB16>(&mut enc, w, h, &u16s(), &tags),
            (L::Rgb, S::F32) => write_one::<colortype::RGB32Float>(&mut enc, w, h, &f32s(), &tags),
            (L::Rgba, S::U8) => write_one::<colortype::RGBA8>(&mut enc, w, h, d8, &tags),
            (L::Rgba, S::U16) => write_one::<colortype::RGBA16>(&mut enc, w, h, &u16s(), &tags),
            (L::Rgba, S::F32) => write_one::<colortype::RGBA32Float>(&mut enc, w, h, &f32s(), &tags),
            (L::Cmyk, S::U8) => write_one::<colortype::CMYK8>(&mut enc, w, h, d8, &tags),
            (L::Cmyk, S::U16) => write_one::<colortype::CMYK16>(&mut enc, w, h, &u16s(), &tags),
            (L::Cmyk, S::F32) => write_one::<colortype::CMYK32Float>(&mut enc, w, h, &f32s(), &tags),
            (L::CmykA, S::U8) => write_one::<colortype::CMYKA8>(&mut enc, w, h, d8, &tags),
            (L::CmykA, S::U16) => write_one::<CmykA16>(&mut enc, w, h, &u16s(), &tags),
            (L::CmykA, S::F32) => write_one::<CmykA32F>(&mut enc, w, h, &f32s(), &tags),
            (l, s) => return Err(CodecError::encode(F, format!("unsupported {l:?} {s:?}"))),
        };
        res.map_err(|e| CodecError::encode(F, e))?;
    }
    Ok(cursor.into_inner())
}

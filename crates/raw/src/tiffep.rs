//! TIFF/EP-structured raws (NEF, ARW, PEF and others) whose sensor data is
//! stored uncompressed or as lossless JPEG in a standard CFA IFD (TIFF/EP,
//! ISO 12234-2: PhotometricInterpretation 32803, CFARepeatPatternDim and
//! CFAPattern), plus Sony's compressed ARW (see [`crate::sony`]). Other
//! vendor-specific compressions are reported as unsupported.
//!
//! Black and white levels use DNG-style tags when the camera writes them,
//! then the vendor's publicly documented tags (Nikon maker note BlackLevel,
//! Sony raw-IFD BlackLevel), else 0 with a warning. The white level falls
//! back to the clipping point found in the data. The as-shot white balance
//! comes from the documented vendor tags (Nikon WB_RBLevels, Sony
//! WB_RGGBLevels); otherwise it is estimated.

use crate::cr2::clip_level;
use crate::error::{RawError, Result};
use crate::sensor::{BlackLevels, Cfa, JpegLayout, Rect, Sensor, read_plane};
use crate::sony::{self, SONY_RAW_FILE_TYPE};
use crate::tiff::{Ifd, Tiff, tag};
use crate::{Limits, RawFormat};

const PHOTOMETRIC_CFA: u32 = 32803;
const NIKON_WB_RB_LEVELS: u16 = 0x000C;
const NIKON_BLACK_LEVEL: u16 = 0x003D;
const SONY_BLACK_LEVEL: u16 = 0x7310;
const SONY_WB_RGGB_LEVELS: u16 = 0x7313;

fn raw_ifd(t: &Tiff, ifds: &[Ifd]) -> Option<Ifd> {
    ifds.iter()
        .filter(|i| t.tag_uint(i, tag::PHOTOMETRIC) == Some(PHOTOMETRIC_CFA))
        .max_by_key(|i| u64::from(t.tag_uint(i, tag::IMAGE_WIDTH).unwrap_or(0)) * u64::from(t.tag_uint(i, tag::IMAGE_LENGTH).unwrap_or(0)))
        .cloned()
}

/// `true` for Sony ARW files whose raw IFD is not CFA data (the reduced-size
/// lossless "M" / "S" YCbCr variants): recognised, but not decoded.
pub(crate) fn is_sony_non_cfa(t: &Tiff, ifds: &[Ifd]) -> bool {
    ifds.iter().any(|i| i.has(SONY_RAW_FILE_TYPE)) && ifds.iter().any(|i| t.tag_ascii(i, tag::MAKE).is_some_and(|m| m.to_ascii_uppercase().starts_with("SONY")))
}

/// CFA from the TIFF/EP tags of the raw IFD, or the EXIF CFAPattern.
pub(crate) fn cfa(t: &Tiff, raw: &Ifd, ifds: &[Ifd]) -> Option<Cfa> {
    let dim = t.tag_uints(raw, tag::CFA_REPEAT_PATTERN_DIM);
    let pat = t.tag_uints(raw, tag::CFA_PATTERN);
    let (rows, cols, colors) = match (dim.as_slice(), pat) {
        ([r, c], p) if p.len() == (*r as usize) * (*c as usize) && !p.is_empty() => (*r as usize, *c as usize, p),
        _ => {
            // EXIF CFAPattern: two 16-bit counts (columns, rows) then the colours.
            let e = ifds.iter().find_map(|i| i.get(tag::EXIF_CFA_PATTERN).copied())?;
            let b = t.raw(&e)?;
            let rd = |o: usize| b.get(o..o + 2).map(|s| if t.le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) });
            let (c, r) = (usize::from(rd(0)?), usize::from(rd(2)?));
            let p = b.get(4..4 + r.checked_mul(c)?)?;
            (r, c, p.iter().map(|&v| u32::from(v)).collect())
        }
    };
    if rows == 0 || cols == 0 || rows > 16 || cols > 16 {
        return None;
    }
    let colors = colors.into_iter().map(|c| u8::try_from(c).ok().filter(|c| *c <= 2)).collect::<Option<Vec<u8>>>()?;
    Some(Cfa { width: cols, height: rows, colors, origin_x: 0, origin_y: 0 })
}

/// Nikon maker note: "Nikon\0", version, then an embedded TIFF header whose
/// offsets are relative to itself.
fn nikon_maker_note<'a>(t: &Tiff<'a>, ifds: &[Ifd]) -> Option<(Tiff<'a>, Ifd)> {
    let e = ifds.iter().find_map(|i| i.get(tag::MAKER_NOTE).copied())?;
    let b = t.raw(&e)?;
    if !b.starts_with(b"Nikon\0") {
        return None;
    }
    let base = e.at.checked_add(10)?;
    let inner = Tiff::new(t.data.get(base..)?)?;
    let ifd = inner.ifd_at(inner.first_ifd, 0)?;
    Some((inner, ifd))
}

/// Four levels listed in R, G, G, B order, placed at their 2×2 CFA positions
/// (data parity).
pub(crate) fn rggb_by_position(cfa: &Cfa, v: &[f64]) -> Option<[f32; 4]> {
    let [r, g1, g2, b] = <[f64; 4]>::try_from(v).ok()?;
    if [r, g1, g2, b].iter().any(|x| !x.is_finite() || *x < 0.0) {
        return None;
    }
    let mut greens = [g1, g2].into_iter();
    let mut out = [0.0f32; 4];
    for (k, slot) in out.iter_mut().enumerate() {
        *slot = match cfa.color(k & 1, k >> 1) {
            0 => r,
            2 => b,
            _ => greens.next().unwrap_or(g1),
        } as f32;
    }
    Some(out)
}

pub(crate) fn decode(t: &Tiff, format: RawFormat, limits: &Limits) -> Result<Sensor> {
    let ifds = t.all_ifds();
    let Some(raw) = raw_ifd(t, &ifds) else {
        if is_sony_non_cfa(t, &ifds) {
            return Err(RawError::unsupported("Sony reduced-size (M / S) lossless ARW"));
        }
        return Err(RawError::unsupported(format!("{}: no CFA image found", format.name())));
    };
    let plane =
        if format == RawFormat::Arw && sony::is_craw(t, &raw) { sony::read_craw(t, &raw, limits)? } else { read_plane(t, &raw, limits, JpegLayout::Quads)? };
    if plane.samples != 1 {
        return Err(RawError::unsupported(format!("CFA data with {} samples per pixel", plane.samples)));
    }
    let cfa = cfa(t, &raw, &ifds).ok_or_else(|| RawError::unsupported(format!("{}: CFA pattern not recorded", format.name())))?;
    if !cfa.is_bayer() {
        return Err(RawError::unsupported(format!("{}x{} non-Bayer CFA", cfa.height, cfa.width)));
    }
    let mut warnings = Vec::new();
    let full = Rect::new(0, 0, plane.width, plane.height);
    let make = ifds.iter().find_map(|i| t.tag_ascii(i, tag::MAKE));
    let model = ifds.iter().find_map(|i| t.tag_ascii(i, tag::MODEL));

    let nikon = if format == RawFormat::Nef { nikon_maker_note(t, &ifds) } else { None };
    let bl = t.tag_floats(&raw, tag::BLACK_LEVEL);
    let vendor_black = match &nikon {
        Some((n, m)) => n.tag_floats(m, NIKON_BLACK_LEVEL),
        None => t.tag_floats(&raw, SONY_BLACK_LEVEL),
    };
    let black = if let Some(b) = bl.first() {
        BlackLevels::uniform(*b as f32)
    } else if let Some(v) = rggb_by_position(&cfa, &vendor_black) {
        BlackLevels { rows: 2, cols: 2, values: v.to_vec(), delta_h: Vec::new(), delta_v: Vec::new() }
    } else {
        warnings.push(format!("{}: black level not recorded in a documented tag; assumed 0", format.name()));
        BlackLevels::uniform(0.0)
    };
    let wl = t.tag_floats(&raw, tag::WHITE_LEVEL);
    let white = match wl.first() {
        Some(w) if *w > 0.0 => *w as f32,
        _ => clip_level(&plane.data, plane.bits),
    };
    let camera_wb = match &nikon {
        Some((n, m)) => match n.tag_floats(m, NIKON_WB_RB_LEVELS).as_slice() {
            [r, b, ..] => Some([*r, 1.0, *b]),
            _ => None,
        },
        None => match t.tag_floats(&raw, SONY_WB_RGGB_LEVELS).as_slice() {
            [r, g1, g2, b] if *g1 + *g2 > 0.0 => Some([2.0 * r / (g1 + g2), 1.0, 2.0 * b / (g1 + g2)]),
            _ => None,
        },
    }
    .filter(|m| m.iter().all(|v| (0.25..8.0).contains(v)));
    // DNG-style DefaultCropOrigin / DefaultCropSize (some vendors write them in the raw IFD).
    let mut crop = full;
    if let ([ox, oy], [cw, ch]) = (t.tag_floats(&raw, tag::DEFAULT_CROP_ORIGIN).as_slice(), t.tag_floats(&raw, tag::DEFAULT_CROP_SIZE).as_slice()) {
        let f = |v: f64| if v.is_finite() && v >= 0.0 { v.round() as usize } else { 0 };
        let r = Rect::new(f(*ox), f(*oy), f(*cw), f(*ch)).intersect(&full);
        if !r.is_empty() {
            crop = r;
        }
    }
    let ifd0 = t.ifd_at(t.first_ifd, 0);
    Ok(Sensor {
        format,
        make,
        model,
        width: plane.width,
        height: plane.height,
        samples: 1,
        data: plane.data,
        cfa: Some(cfa),
        linearization: None,
        black,
        white: [white; 3],
        active: full,
        crop,
        color: Default::default(),
        camera_wb,
        orientation: ifd0.and_then(|i| t.tag_uint(&i, tag::ORIENTATION)).map(|o| o as u16).filter(|o| (1..=8).contains(o)).unwrap_or(1),
        baseline_exposure: 0.0,
        gain_maps: Vec::new(),
        warnings,
    })
}

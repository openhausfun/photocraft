//! Digital Negative (Adobe DNG Specification 1.7): raw IFD selection, CFA
//! description, linearization, black/white levels, active area, default
//! crop, colour calibration and white balance.

use crate::color::{self, Calibration, ColorInfo, IDENTITY};
use crate::error::{RawError, Result};
use crate::sensor::{BlackLevels, Cfa, JpegLayout, Rect, Sensor, read_plane};
use crate::tiff::{Ifd, Tiff, tag};
use crate::{Limits, RawFormat, opcodes};

const PHOTOMETRIC_CFA: u32 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;

/// The main raw image: NewSubFileType 0 with CFA or LinearRaw data, largest first.
fn raw_ifd(t: &Tiff, ifds: &[Ifd]) -> Option<Ifd> {
    ifds.iter()
        .filter(|i| t.tag_uint(i, tag::NEW_SUBFILE_TYPE).unwrap_or(0) == 0)
        .filter(|i| matches!(t.tag_uint(i, tag::PHOTOMETRIC), Some(PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW)))
        .max_by_key(|i| u64::from(t.tag_uint(i, tag::IMAGE_WIDTH).unwrap_or(0)) * u64::from(t.tag_uint(i, tag::IMAGE_LENGTH).unwrap_or(0)))
        .cloned()
}

fn matrix(t: &Tiff, ifd: &Ifd, tg: u16) -> Option<color::Mat3> {
    let v = t.tag_floats(ifd, tg);
    if v.len() != 9 {
        return None;
    }
    color::from_slice(&v)
}

fn triple(t: &Tiff, ifd: &Ifd, tg: u16) -> Option<[f64; 3]> {
    let v = t.tag_floats(ifd, tg);
    (v.len() == 3 && v.iter().all(|x| x.is_finite() && *x > 0.0)).then(|| [v[0], v[1], v[2]])
}

/// Colour calibration from IFD0.
pub(crate) fn color_info(t: &Tiff, ifd0: &Ifd) -> ColorInfo {
    let mut cal = Vec::new();
    for (cm, cc, fm, ill, default_t) in [
        (tag::COLOR_MATRIX_1, tag::CAMERA_CALIBRATION_1, tag::FORWARD_MATRIX_1, tag::CALIBRATION_ILLUMINANT_1, 2856.0),
        (tag::COLOR_MATRIX_2, tag::CAMERA_CALIBRATION_2, tag::FORWARD_MATRIX_2, tag::CALIBRATION_ILLUMINANT_2, 6504.0),
    ] {
        let Some(color_matrix) = matrix(t, ifd0, cm) else { continue };
        if color::invert(&color_matrix).is_none() {
            continue;
        }
        let temperature = t.tag_uint(ifd0, ill).and_then(color::illuminant_temperature).unwrap_or(default_t);
        cal.push(Calibration {
            temperature,
            color_matrix,
            forward_matrix: matrix(t, ifd0, fm).filter(|m| color::invert(m).is_some()),
            camera_calibration: matrix(t, ifd0, cc).filter(|m| color::invert(m).is_some()).unwrap_or(IDENTITY),
        });
    }
    cal.sort_by(|a, b| a.temperature.total_cmp(&b.temperature));
    if cal.len() == 2 && (cal[0].forward_matrix.is_some() != cal[1].forward_matrix.is_some()) {
        for c in &mut cal {
            c.forward_matrix = None;
        }
    }
    let white_xy = {
        let v = t.tag_floats(ifd0, tag::AS_SHOT_WHITE_XY);
        (v.len() == 2 && v.iter().all(|x| *x > 0.0 && *x < 1.0)).then(|| [v[0], v[1]])
    };
    ColorInfo {
        calibrations: cal,
        analog_balance: triple(t, ifd0, tag::ANALOG_BALANCE),
        as_shot_neutral: triple(t, ifd0, tag::AS_SHOT_NEUTRAL),
        as_shot_white_xy: white_xy,
    }
}

/// CFA pattern of a DNG raw IFD, anchored at the active area origin.
fn cfa(t: &Tiff, ifd: &Ifd, active: &Rect) -> Result<Cfa> {
    let dim = t.tag_uints(ifd, tag::CFA_REPEAT_PATTERN_DIM);
    let (rows, cols) = match dim.as_slice() {
        [r, c] => (*r as usize, *c as usize),
        _ => (2, 2),
    };
    let pattern = t.tag_uints(ifd, tag::CFA_PATTERN);
    if rows == 0 || cols == 0 || rows > 16 || cols > 16 || pattern.len() != rows * cols {
        return Err(RawError::malformed("bad CFA pattern"));
    }
    let planes = match t.tag_uints(ifd, tag::CFA_PLANE_COLOR) {
        v if v.is_empty() => vec![0, 1, 2],
        v => v,
    };
    if planes != [0, 1, 2] {
        return Err(RawError::unsupported(format!("CFA plane colours {planes:?} (only red, green, blue)")));
    }
    if t.tag_uint(ifd, tag::CFA_LAYOUT).unwrap_or(1) != 1 {
        return Err(RawError::unsupported("non-rectangular CFA layout"));
    }
    let colors = pattern
        .into_iter()
        .map(|code| u8::try_from(code).ok().filter(|c| *c <= 2))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| RawError::unsupported("CFA colours other than red, green and blue"))?;
    let c = Cfa { width: cols, height: rows, colors, origin_x: active.x, origin_y: active.y };
    if !c.is_bayer() {
        return Err(RawError::unsupported(format!("{rows}x{cols} non-Bayer CFA (e.g. X-Trans)")));
    }
    Ok(c)
}

fn rect_from(t: &Tiff, ifd: &Ifd, width: usize, height: usize) -> Rect {
    let full = Rect::new(0, 0, width, height);
    match t.tag_uints(ifd, tag::ACTIVE_AREA).as_slice() {
        [top, left, bottom, right] if bottom > top && right > left => {
            Rect::new(*left as usize, *top as usize, (*right - *left) as usize, (*bottom - *top) as usize).intersect(&full)
        }
        _ => full,
    }
}

/// Decodes a DNG file's raw image and metadata.
pub(crate) fn decode(t: &Tiff, limits: &Limits) -> Result<Sensor> {
    let ifds = t.all_ifds();
    let ifd0 = t.ifd_at(t.first_ifd, 0).ok_or_else(|| RawError::malformed("DNG has no IFD0"))?;
    let raw = raw_ifd(t, &ifds).ok_or_else(|| RawError::malformed("DNG has no raw image"))?;
    let mut warnings = Vec::new();
    let photometric = t.tag_uint(&raw, tag::PHOTOMETRIC).unwrap_or(0);
    let plane = read_plane(t, &raw, limits, JpegLayout::Flat)?;
    let samples = plane.samples;
    let is_cfa = photometric == PHOTOMETRIC_CFA;
    if is_cfa && samples != 1 {
        return Err(RawError::unsupported(format!("CFA data with {samples} samples per pixel")));
    }
    if !is_cfa && samples != 3 {
        return Err(RawError::unsupported(format!("LinearRaw data with {samples} samples per pixel")));
    }
    let active = rect_from(t, &raw, plane.width, plane.height);
    if active.is_empty() {
        return Err(RawError::malformed("empty active area"));
    }
    let cfa = if is_cfa { Some(cfa(t, &raw, &active)?) } else { None };

    // Default crop, relative to the active area.
    let mut crop = active;
    let origin = t.tag_floats(&raw, tag::DEFAULT_CROP_ORIGIN);
    let size = t.tag_floats(&raw, tag::DEFAULT_CROP_SIZE);
    if let ([ox, oy], [cw, ch]) = (origin.as_slice(), size.as_slice()) {
        let f = |v: f64| if v.is_finite() && v >= 0.0 { v.round() as usize } else { 0 };
        let r = Rect::new(active.x + f(*ox), active.y + f(*oy), f(*cw), f(*ch)).intersect(&active);
        if !r.is_empty() {
            crop = r;
        }
    }
    let scale = t.tag_floats(&raw, tag::DEFAULT_SCALE);
    if scale.len() == 2 && (scale[0] - scale[1]).abs() > 1e-6 {
        warnings.push("non-square pixels (DefaultScale) are not resampled".to_string());
    }
    // Opcode lists: OpcodeList2 gain maps (lens shading) are applied; everything else is reported.
    let mut gain_maps = Vec::new();
    for (tg, what) in [(tag::OPCODE_LIST_1, 1), (tag::OPCODE_LIST_2, 2), (tag::OPCODE_LIST_3, 3)] {
        let Some(bytes) = raw.get(tg).and_then(|e| t.raw(e)) else { continue };
        let (maps, skipped) = opcodes::parse(bytes);
        if what == 2 {
            gain_maps = maps;
        } else if !maps.is_empty() {
            warnings.push(format!("DNG OpcodeList{what}: GainMap is not applied"));
        }
        if !skipped.is_empty() {
            warnings.push(format!("DNG OpcodeList{what}: {} not applied", skipped.join(", ")));
        }
    }

    let max = ((1u32 << plane.bits) - 1) as f32;
    let linearization = raw.get(tag::LINEARIZATION_TABLE).map(|e| t.uints(e)).filter(|v| !v.is_empty() && v.len() <= 65536);
    let linearization: Option<Vec<u16>> = linearization.map(|v| v.into_iter().map(|x| x.min(65535) as u16).collect());
    let white_default = match &linearization {
        Some(l) => f32::from(l.iter().copied().max().unwrap_or(u16::MAX)),
        None => max,
    };
    let wl = t.tag_floats(&raw, tag::WHITE_LEVEL);
    let white = [0, 1, 2].map(|s| wl.get(s).or_else(|| wl.first()).map(|v| *v as f32).filter(|v| *v > 0.0).unwrap_or(white_default));

    let (rows, cols) = match t.tag_uints(&raw, tag::BLACK_LEVEL_REPEAT_DIM).as_slice() {
        [r, c] if (1..=16).contains(r) && (1..=16).contains(c) => (*r as usize, *c as usize),
        _ => (1, 1),
    };
    let bl = t.tag_floats(&raw, tag::BLACK_LEVEL);
    let mut black = if bl.len() == rows * cols * samples {
        BlackLevels { rows, cols, values: bl.iter().map(|v| *v as f32).collect(), delta_h: Vec::new(), delta_v: Vec::new() }
    } else {
        BlackLevels::uniform(bl.first().copied().unwrap_or(0.0) as f32)
    };
    let dh = t.tag_floats(&raw, tag::BLACK_LEVEL_DELTA_H);
    if dh.len() == active.width {
        black.delta_h = dh.iter().map(|v| *v as f32).collect();
    }
    let dv = t.tag_floats(&raw, tag::BLACK_LEVEL_DELTA_V);
    if dv.len() == active.height {
        black.delta_v = dv.iter().map(|v| *v as f32).collect();
    }

    let color = color_info(t, &ifd0);
    if color.calibrations.is_empty() {
        warnings.push("the DNG has no colour matrix; colours are approximate".to_string());
    }
    let baseline_exposure = t.tag_floats(&ifd0, tag::BASELINE_EXPOSURE).first().copied().unwrap_or(0.0)
        + t.tag_floats(&raw, tag::BASELINE_EXPOSURE_OFFSET).first().copied().unwrap_or(0.0);
    let model = t.tag_ascii(&ifd0, tag::UNIQUE_CAMERA_MODEL).or_else(|| t.tag_ascii(&ifd0, tag::MODEL));
    Ok(Sensor {
        format: RawFormat::Dng,
        make: t.tag_ascii(&ifd0, tag::MAKE),
        model,
        width: plane.width,
        height: plane.height,
        samples,
        data: plane.data,
        cfa,
        linearization,
        black,
        white,
        active,
        crop,
        color,
        camera_wb: None,
        orientation: t.tag_uint(&ifd0, tag::ORIENTATION).map(|o| o as u16).filter(|o| (1..=8).contains(o)).unwrap_or(1),
        baseline_exposure: if baseline_exposure.is_finite() { baseline_exposure.clamp(-10.0, 10.0) } else { 0.0 },
        gain_maps,
        warnings,
    })
}

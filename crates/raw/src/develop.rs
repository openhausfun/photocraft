//! Raw development: linearize, subtract black, scale to white, white
//! balance, clip highlights, demosaic, camera → XYZ (D50) → linear ProPhoto,
//! exposure, gamma 1.8 encode to 16 bits, orientation.

use crate::color::{self, Mat3};
use crate::demosaic::{Demosaic, PAD, Padded, demosaic};
use crate::error::{RawError, Result};
use crate::sensor::Sensor;
use crate::{Limits, RawFormat, par};

/// White balance choice.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum WhiteBalance {
    /// The camera's as-shot white balance when the file records one, else automatic.
    #[default]
    AsShot,
    /// Grey-world estimate from the image.
    Auto,
    /// Explicit R, G, B multipliers applied to the camera channels.
    Multipliers([f64; 3]),
}

/// Development settings.
#[derive(Debug, Clone, PartialEq)]
pub struct DevelopOptions {
    pub demosaic: Demosaic,
    pub white_balance: WhiteBalance,
    /// Exposure compensation in EV, added to the file's baseline exposure.
    pub exposure: f64,
    /// Apply the file's orientation (rotate / flip the pixels).
    pub orient: bool,
    pub limits: Limits,
}

impl Default for DevelopOptions {
    fn default() -> Self {
        DevelopOptions { demosaic: Demosaic::default(), white_balance: WhiteBalance::default(), exposure: 0.0, orient: true, limits: Limits::default() }
    }
}

/// What was developed and how.
#[derive(Debug, Clone, PartialEq)]
pub struct RawInfo {
    pub format: RawFormat,
    pub make: Option<String>,
    pub model: Option<String>,
    /// Sensor data size.
    pub sensor_width: usize,
    pub sensor_height: usize,
    /// CFA pattern at the crop origin, e.g. `"RGGB"`; `None` for LinearRaw.
    pub cfa: Option<String>,
    /// White-balance multipliers used (R, G, B, normalized to a minimum of 1).
    pub wb_multipliers: [f64; 3],
    pub orientation: u16,
    pub baseline_exposure: f64,
}

/// A developed image: 16-bit RGB in ProPhoto RGB (ROMM primaries, D50, gamma 1.8).
#[derive(Debug, Clone)]
pub struct Developed {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGB, `width * height * 3` samples.
    pub rgb: Vec<u16>,
    pub info: RawInfo,
    /// Notes about anything approximated or not applied.
    pub warnings: Vec<String>,
}

/// Linearization + black/white scaling for one sample (pre white balance), 0..1.
struct Scale<'a> {
    s: &'a Sensor,
    lin: Option<&'a [u16]>,
}

impl Scale<'_> {
    #[inline]
    fn value(&self, x: usize, y: usize, sample: usize) -> f32 {
        let s = self.s;
        let raw = s.data[(y * s.width + x) * s.samples + sample];
        let v = match self.lin {
            Some(l) => f32::from(l.get(usize::from(raw)).or_else(|| l.last()).copied().unwrap_or(raw)),
            None => f32::from(raw),
        };
        let black = s.black.at(x.saturating_sub(s.active.x), y.saturating_sub(s.active.y), sample, s.samples);
        let white = s.white[sample.min(2)];
        let range = white - black;
        if range.is_nan() || range < 1.0 {
            return 0.0;
        }
        let n = (v - black) / range;
        if n.is_finite() { n } else { 0.0 }
    }
}

/// Grey-world neutral (camera-space R, G, B) from unclipped mid-range samples.
fn grey_world(s: &Sensor, sc: &Scale) -> [f64; 3] {
    let c = s.crop;
    let step = (c.height / 512).max(1);
    let rows: Vec<usize> = (0..c.height).step_by(step).collect();
    let parts = par::map(rows.len(), |i| {
        let y = c.y + rows[i];
        let mut sum = [0.0f64; 3];
        let mut n = [0u64; 3];
        for x in c.x..c.x + c.width {
            for k in 0..s.samples.min(3) {
                let ch = match &s.cfa {
                    Some(cfa) => usize::from(cfa.color(x, y)),
                    None => k,
                };
                let v = sc.value(x, y, k);
                if v > 0.002 && v < 0.9 {
                    sum[ch] += f64::from(v);
                    n[ch] += 1;
                }
            }
        }
        (sum, n)
    });
    let mut sum = [0.0f64; 3];
    let mut n = [0u64; 3];
    for (s, k) in parts {
        for c in 0..3 {
            sum[c] += s[c];
            n[c] += k[c];
        }
    }
    let mean = [0, 1, 2].map(|c| if n[c] > 0 { sum[c] / n[c] as f64 } else { 0.0 });
    if mean.iter().any(|m| m.is_nan() || *m <= 1e-6) {
        return [1.0; 3];
    }
    let m = mean.iter().cloned().fold(f64::MIN, f64::max);
    mean.map(|v| v / m)
}

fn normalize(n: [f64; 3]) -> Option<[f64; 3]> {
    let m = n.iter().cloned().fold(f64::MIN, f64::max);
    (m > 0.0 && n.iter().all(|v| v.is_finite() && *v > 1e-4)).then(|| n.map(|v| v / m))
}

/// Develops sensor data.
pub fn develop_sensor(s: &Sensor, opts: &DevelopOptions) -> Result<Developed> {
    let c = s.crop;
    if c.is_empty() || c.x + c.width > s.width || c.y + c.height > s.height || s.data.len() < s.width * s.height * s.samples {
        return Err(RawError::malformed("crop lies outside the sensor data"));
    }
    if s.samples != 1 && s.samples != 3 {
        return Err(RawError::unsupported(format!("{} samples per pixel", s.samples)));
    }
    // Working buffers: one f32 plane plus f32 RGB plus u16 RGB per pixel.
    opts.limits.check(c.width as u64, c.height as u64, 4 + 12 + 6)?;
    let mut warnings = s.warnings.clone();
    let sc = Scale { s, lin: s.linearization.as_deref() };

    // White balance: camera-space neutral, max component 1.
    let as_shot = s
        .color
        .as_shot_neutral
        .or_else(|| s.color.as_shot_white_xy.and_then(|xy| s.color.xy_to_neutral(xy)))
        .or_else(|| s.camera_wb.map(|m| m.map(|v| 1.0 / v)))
        .and_then(normalize);
    let neutral = match opts.white_balance {
        WhiteBalance::AsShot => match as_shot {
            Some(n) => n,
            None => {
                warnings.push("no as-shot white balance in the file; estimated automatically (grey world)".into());
                grey_world(s, &sc)
            }
        },
        WhiteBalance::Auto => grey_world(s, &sc),
        WhiteBalance::Multipliers(m) => {
            normalize(m.map(|v| 1.0 / v)).ok_or_else(|| RawError::Malformed("white-balance multipliers must be positive".into()))?
        }
    };
    let mult = neutral.map(|v| (1.0 / v) as f32);

    // Balanced camera → XYZ D50.
    let to_xyz: Mat3 = match s.color.balanced_to_xyz_d50(neutral) {
        Some(m) => m,
        None => {
            if s.format != RawFormat::Dng {
                warnings.push("no colour calibration for this camera; camera colours are treated as sRGB primaries".into());
            }
            color::srgb_to_xyz_d50()
        }
    };
    let gain = 2f64.powf(s.baseline_exposure + opts.exposure.clamp(-10.0, 10.0));
    let out_m = color::scale(&color::mul(&color::xyz_d50_to_prophoto(), &to_xyz), gain);
    let out_m: [[f32; 3]; 3] = out_m.map(|r| r.map(|v| v as f32));

    let (w, h) = (c.width, c.height);
    let band = par::band_rows(w);
    let mut rgb: Vec<f32>;
    let phase;
    match &s.cfa {
        Some(cfa) => {
            // Normalized, balanced, clipped CFA plane.
            let mut plane = Padded::new(w, h);
            let stride = plane.stride;
            // Fast path: black varies at most per 2×2 position (the Bayer period).
            let bl = &s.black;
            let periodic = bl.delta_h.is_empty() && bl.delta_v.is_empty() && matches!(bl.rows, 1 | 2) && matches!(bl.cols, 1 | 2);
            plane.fill_rows(band, |y0, chunk| {
                for (r, prow) in chunk.chunks_exact_mut(stride).enumerate() {
                    let row = &mut prow[PAD..PAD + w];
                    let y = c.y + y0 + r;
                    if periodic {
                        // Per column parity: black and the combined scale × white-balance gain.
                        let k: [(f32, f32); 2] = std::array::from_fn(|p| {
                            let x = c.x + p;
                            let black = bl.at(x.saturating_sub(s.active.x), y.saturating_sub(s.active.y), 0, 1);
                            let range = s.white[0] - black;
                            let gain = if range >= 1.0 { mult[usize::from(cfa.color(x, y)).min(2)] / range } else { 0.0 };
                            (black, gain)
                        });
                        let src = &s.data[y * s.width + c.x..y * s.width + c.x + w];
                        match &sc.lin {
                            None => {
                                for (i, (v, &raw)) in row.iter_mut().zip(src).enumerate() {
                                    let (black, gain) = k[i & 1];
                                    *v = ((f32::from(raw) - black) * gain).clamp(0.0, 1.0);
                                }
                            }
                            Some(l) => {
                                for (i, (v, &raw)) in row.iter_mut().zip(src).enumerate() {
                                    let (black, gain) = k[i & 1];
                                    let lin = f32::from(l.get(usize::from(raw)).or_else(|| l.last()).copied().unwrap_or(raw));
                                    *v = ((lin - black) * gain).clamp(0.0, 1.0);
                                }
                            }
                        }
                    } else {
                        for (i, v) in row.iter_mut().enumerate() {
                            let x = c.x + i;
                            let ch = usize::from(cfa.color(x, y)).min(2);
                            *v = (sc.value(x, y, 0) * mult[ch]).clamp(0.0, 1.0);
                        }
                    }
                    // Lens-shading gain maps, after the white-balance clip so clipped
                    // highlights stay neutral.
                    if let Some(g) = gain_row(s, y, c.x, w, 0) {
                        for (v, g) in row.iter_mut().zip(g) {
                            *v = (*v * g).clamp(0.0, 1.0);
                        }
                    }
                }
            });
            plane.fill_borders();
            let p = cfa.phase(c.x, c.y);
            phase = Some(p);
            rgb = demosaic(&plane, p, opts.demosaic, &to_xyz);
            drop(plane);
        }
        None => {
            phase = None;
            rgb = vec![0.0f32; w * h * 3];
            par::chunks_mut(&mut rgb, band * w * 3, |b, chunk| {
                for (r, row) in chunk.chunks_exact_mut(w * 3).enumerate() {
                    let y = c.y + b * band + r;
                    for i in 0..w {
                        for k in 0..3 {
                            row[i * 3 + k] = (sc.value(c.x + i, y, k) * mult[k]).clamp(0.0, 1.0);
                        }
                    }
                    for k in 0..3 {
                        if let Some(g) = gain_row(s, y, c.x, w, k) {
                            for (i, g) in g.into_iter().enumerate() {
                                row[i * 3 + k] = (row[i * 3 + k] * g).clamp(0.0, 1.0);
                            }
                        }
                    }
                }
            });
        }
    }

    // Camera → ProPhoto, exposure, gamma 1.8, 16 bits.
    let mut out = vec![0u16; w * h * 3];
    let lut = GammaLut::new();
    par::chunks_mut(&mut out, band * w * 3, |b, chunk| {
        let start = b * band * w * 3;
        let src = &rgb[start..start + chunk.len()];
        for (o, p) in chunk.as_chunks_mut::<3>().0.iter_mut().zip(src.as_chunks::<3>().0) {
            for k in 0..3 {
                o[k] = lut.encode(out_m[k][0] * p[0] + out_m[k][1] * p[1] + out_m[k][2] * p[2]);
            }
        }
    });
    rgb.clear();
    rgb.shrink_to_fit();

    let (out, ow, oh) = if opts.orient { orient(out, w, h, s.orientation) } else { (out, w, h) };
    let nmax = mult.iter().cloned().fold(f32::MAX, f32::min).max(1e-6);
    Ok(Developed {
        width: ow as u32,
        height: oh as u32,
        rgb: out,
        info: RawInfo {
            format: s.format,
            make: s.make.clone(),
            model: s.model.clone(),
            sensor_width: s.width,
            sensor_height: s.height,
            cfa: phase.map(|p| p.iter().map(|c| ['R', 'G', 'B'][usize::from(*c).min(2)]).collect()),
            wb_multipliers: mult.map(|m| f64::from(m / nmax)),
            orientation: s.orientation,
            baseline_exposure: s.baseline_exposure,
        },
        warnings,
    })
}

/// Gain-map factors (DNG OpcodeList2 GainMap) for data row `y`, columns
/// `x0..x0 + w`, sample `k`; `None` when no map touches the row.
fn gain_row(s: &Sensor, y: usize, x0: usize, w: usize, k: usize) -> Option<Vec<f32>> {
    let a = s.active;
    if s.gain_maps.is_empty() || y < a.y || a.width == 0 || a.height == 0 {
        return None;
    }
    let ay = y - a.y;
    // Map points are placed in coordinates relative to the (active) image.
    let rv = (ay as f64 + 0.5) / a.height as f64;
    let mut out: Option<Vec<f32>> = None;
    for m in &s.gain_maps {
        if k < m.plane || k >= m.plane.saturating_add(m.planes) || ay < m.top || ay >= m.bottom || !(ay - m.top).is_multiple_of(m.row_pitch) {
            continue;
        }
        let row = m.row(rv, k - m.plane);
        let o = out.get_or_insert_with(|| vec![1.0; w]);
        for (i, f) in o.iter_mut().enumerate() {
            let x = x0 + i;
            if x >= a.x && m.covers(x - a.x, ay) {
                *f *= m.at(&row, ((x - a.x) as f64 + 0.5) / a.width as f64);
            }
        }
    }
    out
}

/// Linear 0..1 → gamma-1.8 16-bit code. Tabulated over `s = sqrt(v)`, where
/// the curve `s^(2/1.8)` is smooth down to 0, with linear interpolation
/// (error below 0.2 code values).
struct GammaLut {
    table: Vec<f32>,
}

const GAMMA_STEPS: usize = 4096;

impl GammaLut {
    fn new() -> Self {
        GammaLut { table: (0..=GAMMA_STEPS + 1).map(|i| ((i as f32 / GAMMA_STEPS as f32).min(1.0)).powf(2.0 / 1.8) * 65535.0).collect() }
    }

    #[inline]
    fn encode(&self, v: f32) -> u16 {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        let x = v.sqrt() * GAMMA_STEPS as f32;
        let i = (x as usize).min(GAMMA_STEPS);
        let f = x - i as f32;
        let (a, b) = (self.table[i], self.table[i + 1]);
        (a + (b - a) * f + 0.5) as u16
    }
}

/// Applies a TIFF orientation (1–8) to interleaved RGB.
pub(crate) fn orient(src: Vec<u16>, w: usize, h: usize, o: u16) -> (Vec<u16>, usize, usize) {
    if !(2..=8).contains(&o) {
        return (src, w, h);
    }
    let swap = o >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u16; src.len()];
    let band = par::band_rows(ow);
    par::chunks_mut(&mut out, band * ow * 3, |b, chunk| {
        for (r, row) in chunk.chunks_exact_mut(ow * 3).enumerate() {
            let y2 = b * band + r;
            for x2 in 0..ow {
                let (x, y) = match o {
                    2 => (w - 1 - x2, y2),
                    3 => (w - 1 - x2, h - 1 - y2),
                    4 => (x2, h - 1 - y2),
                    5 => (y2, x2),
                    6 => (y2, h - 1 - x2),
                    7 => (w - 1 - y2, h - 1 - x2),
                    _ => (w - 1 - y2, x2),
                };
                let i = (y * w + x) * 3;
                row[x2 * 3..x2 * 3 + 3].copy_from_slice(&src[i..i + 3]);
            }
        }
    });
    (out, ow, oh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_table_matches_powf() {
        let lut = GammaLut::new();
        for i in 0..=100_000 {
            let v = i as f32 / 100_000.0;
            let exact = v.powf(1.0 / 1.8) * 65535.0;
            assert!((f32::from(lut.encode(v)) - exact).abs() <= 1.0, "{v}: {} vs {exact}", lut.encode(v));
        }
        assert_eq!(lut.encode(-1.0), 0);
        assert_eq!(lut.encode(2.0), 65535);
        assert_eq!(lut.encode(f32::NAN), 0);
    }

    #[test]
    fn orientations() {
        // 3×2 image, pixel value = index.
        let src: Vec<u16> = (0..6).flat_map(|i| [i, i, i]).collect();
        let px = |v: &[u16]| v.as_chunks::<3>().0.iter().map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(px(&orient(src.clone(), 3, 2, 1).0), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(px(&orient(src.clone(), 3, 2, 2).0), vec![2, 1, 0, 5, 4, 3]);
        assert_eq!(px(&orient(src.clone(), 3, 2, 3).0), vec![5, 4, 3, 2, 1, 0]);
        assert_eq!(px(&orient(src.clone(), 3, 2, 4).0), vec![3, 4, 5, 0, 1, 2]);
        let (v, w, h) = orient(src.clone(), 3, 2, 6);
        assert_eq!((w, h), (2, 3));
        // Rotated 90° clockwise: the left column (0, 3) becomes the top row (3, 0).
        assert_eq!(px(&v), vec![3, 0, 4, 1, 5, 2]);
        assert_eq!(px(&orient(src.clone(), 3, 2, 8).0), vec![2, 5, 1, 4, 0, 3]);
        assert_eq!(px(&orient(src.clone(), 3, 2, 5).0), vec![0, 3, 1, 4, 2, 5]);
        assert_eq!(px(&orient(src, 3, 2, 7).0), vec![5, 2, 4, 1, 3, 0]);
    }
}

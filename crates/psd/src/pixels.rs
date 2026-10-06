//! Sample helpers and RGBA8 convenience extraction.
//!
//! These are conveniences for previews and tests; they perform no color
//! management. 16-bit samples are scaled with rounding, 32-bit float samples
//! are clamped to 0..=1 (no tone mapping or gamma), CMYK is converted
//! naively (`r = c' * k' / 255` using the stored, inverted values).

use crate::error::{PsdError, Result};
use crate::file::PsdFile;
use crate::header::{ColorMode, Header, Version};
use crate::layer::{CHANNEL_REAL_USER_MASK, CHANNEL_TRANSPARENCY, CHANNEL_USER_MASK, LayerRecord, Rect};

/// Interprets planar big-endian bytes as u16 samples.
pub fn samples_u16(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
}

/// Interprets planar big-endian bytes as f32 samples.
pub fn samples_f32(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Encodes u16 samples as big-endian bytes.
pub fn u16_to_bytes(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_be_bytes()).collect()
}

/// Encodes f32 samples as big-endian bytes.
pub fn f32_to_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_be_bytes()).collect()
}

/// Expands a 1-bit plane (MSB first, rows padded to bytes) to one byte per
/// pixel (0 or 1).
pub fn unpack_bits(bytes: &[u8], width: usize, height: usize) -> Vec<u8> {
    let rb = width.div_ceil(8);
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let b = bytes.get(y * rb + x / 8).copied().unwrap_or(0);
            out.push((b >> (7 - (x % 8))) & 1);
        }
    }
    out
}

/// Converts one plane of samples at `depth` to 8-bit.
pub fn plane_to_u8(bytes: &[u8], depth: u16, width: usize, height: usize) -> Result<Vec<u8>> {
    Ok(match depth {
        1 => unpack_bits(bytes, width, height).into_iter().map(|b| if b == 1 { 0 } else { 255 }).collect(),
        8 => bytes.to_vec(),
        16 => samples_u16(bytes).into_iter().map(|v| ((u32::from(v) * 255 + 32767) / 65535) as u8).collect(),
        32 => samples_f32(bytes).into_iter().map(|v| if v.is_nan() { 0 } else { (v.clamp(0.0, 1.0) * 255.0).round() as u8 }).collect(),
        d => return Err(PsdError::Unsupported(format!("depth {d}"))),
    })
}

/// An 8-bit RGBA image positioned in document coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Left edge in document coordinates.
    pub left: i32,
    /// Top edge in document coordinates.
    pub top: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Interleaved RGBA, row-major.
    pub data: Vec<u8>,
}

/// An 8-bit single-channel image (e.g. a layer mask).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Samples.
    pub data: Vec<u8>,
    /// Value outside the rectangle (masks only).
    pub default_value: u8,
}

fn interleave(mode: ColorMode, color: &[Vec<u8>], alpha: Option<&[u8]>, palette: &[u8], n: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let (r, g, b) = match mode {
            ColorMode::Rgb => (color[0][i], color[1][i], color[2][i]),
            ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => (color[0][i], color[0][i], color[0][i]),
            ColorMode::Indexed => {
                let idx = usize::from(color[0][i]);
                let p = |k: usize| palette.get(k * 256 + idx).copied().unwrap_or(0);
                (p(0), p(1), p(2))
            }
            ColorMode::Cmyk => {
                let k = u32::from(color[3][i]);
                let f = |c: u8| ((u32::from(c) * k + 127) / 255) as u8;
                (f(color[0][i]), f(color[1][i]), f(color[2][i]))
            }
            m => return Err(PsdError::Unsupported(format!("rgba8 for color mode {m:?}"))),
        };
        out[i * 4] = r;
        out[i * 4 + 1] = g;
        out[i * 4 + 2] = b;
        out[i * 4 + 3] = alpha.map_or(255, |a| a[i]);
    }
    Ok(out)
}

fn color_channel_count(mode: ColorMode) -> Result<usize> {
    match mode {
        ColorMode::Rgb => Ok(3),
        ColorMode::Cmyk => Ok(4),
        ColorMode::Grayscale | ColorMode::Indexed | ColorMode::Bitmap | ColorMode::Duotone => Ok(1),
        m => Err(PsdError::Unsupported(format!("rgba8 for color mode {m:?}"))),
    }
}

/// A borrowed view of a layer with the file context needed to decode it.
#[derive(Debug, Clone, Copy)]
pub struct Layer<'a> {
    /// Index in [`PsdFile::layers`].
    pub index: usize,
    /// The record.
    pub record: &'a LayerRecord,
    header: &'a Header,
}

impl<'a> Layer<'a> {
    /// Name, preferring `luni`.
    pub fn name(&self) -> String {
        self.record.name()
    }

    /// PSD/PSB version of the containing file.
    pub fn version(&self) -> Version {
        self.header.version
    }

    /// Decodes a channel (planar, big-endian samples).
    pub fn channel_bytes(&self, id: i16) -> Result<Vec<u8>> {
        self.record.decode_channel(id, self.header.depth, self.header.version)
    }

    /// Layer pixels as RGBA8 (transparency channel as alpha; the user mask is
    /// *not* applied, see [`Self::user_mask`]). Supports RGB, grayscale and
    /// CMYK at 8/16/32 bits.
    pub fn rgba8(&self) -> Result<RgbaImage> {
        let rect = self.record.rect;
        let (w, h) = rect.size()?;
        let n = w * h;
        let depth = self.header.depth;
        let mode = self.header.color_mode;
        let cc = color_channel_count(mode)?;
        let mut color = Vec::with_capacity(cc);
        for id in 0..cc as i16 {
            let plane = if self.record.channel(id).is_some() {
                plane_to_u8(&self.channel_bytes(id)?, depth, w, h)?
            } else {
                vec![if mode == ColorMode::Cmyk { 255 } else { 0 }; n]
            };
            if plane.len() != n {
                return Err(PsdError::invalid("channel size mismatch"));
            }
            color.push(plane);
        }
        let alpha = match self.record.channel(CHANNEL_TRANSPARENCY) {
            Some(_) => Some(plane_to_u8(&self.channel_bytes(CHANNEL_TRANSPARENCY)?, depth, w, h)?),
            None => None,
        };
        let data = interleave(mode, &color, alpha.as_deref(), &[], n)?;
        Ok(RgbaImage { left: rect.left, top: rect.top, width: w as u32, height: h as u32, data })
    }

    /// The user mask (channel -3 if present, else -2) as 8-bit gray, with its
    /// rectangle and default color. `None` if the layer has no mask channel.
    pub fn user_mask(&self) -> Option<Result<GrayImage>> {
        let id = if self.record.channel(CHANNEL_REAL_USER_MASK).is_some() {
            CHANNEL_REAL_USER_MASK
        } else if self.record.channel(CHANNEL_USER_MASK).is_some() {
            CHANNEL_USER_MASK
        } else {
            return None;
        };
        Some((|| {
            let rect: Rect = self.record.channel_rect(id);
            let (w, h) = rect.size()?;
            let data = plane_to_u8(&self.channel_bytes(id)?, self.header.depth, w, h)?;
            let default_value = match (self.record.layer_mask(), id) {
                (Some(m), CHANNEL_REAL_USER_MASK) => m.real.map_or(m.default_color, |r| r.background),
                (Some(m), _) => m.default_color,
                (None, _) => 0,
            };
            Ok(GrayImage { left: rect.left, top: rect.top, width: w as u32, height: h as u32, data, default_value })
        })())
    }
}

impl PsdFile {
    /// Borrowed view of layer `index`.
    pub fn layer(&self, index: usize) -> Option<Layer<'_>> {
        self.layers().get(index).map(|record| Layer { index, record, header: &self.header })
    }

    /// Iterates over layers in file order (bottom-most first).
    pub fn iter_layers(&self) -> impl Iterator<Item = Layer<'_>> {
        self.layers().iter().enumerate().map(|(index, record)| Layer { index, record, header: &self.header })
    }

    /// The merged composite as RGBA8. Supports RGB, grayscale, CMYK, indexed,
    /// duotone (as gray) and bitmap at all depths. Alpha comes from the first
    /// extra channel when [`PsdFile::merged_has_alpha`] is true.
    pub fn composite_rgba8(&self) -> Result<RgbaImage> {
        let h = &self.header;
        let (w, hh) = (h.width as usize, h.height as usize);
        let n = w * hh;
        let cc = color_channel_count(h.color_mode)?;
        if usize::from(h.channels) < cc {
            return Err(PsdError::invalid("fewer channels than the color mode requires"));
        }
        let all = self.decode_merged()?;
        let plane = h.row_bytes() * hh;
        let get = |i: usize| -> Result<Vec<u8>> { plane_to_u8(&all[i * plane..(i + 1) * plane], h.depth, w, hh) };
        let mut color = Vec::with_capacity(cc);
        for i in 0..cc {
            color.push(get(i)?);
        }
        let alpha = if self.merged_has_alpha() { Some(get(cc)?) } else { None };
        let data = interleave(h.color_mode, &color, alpha.as_deref(), &self.color_mode_data, n)?;
        Ok(RgbaImage { left: 0, top: 0, width: h.width, height: h.height, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_conversions() {
        assert_eq!(samples_u16(&[0x12, 0x34, 0xff, 0xff]), vec![0x1234, 0xffff]);
        assert_eq!(u16_to_bytes(&[0x1234]), vec![0x12, 0x34]);
        let f = f32_to_bytes(&[0.5, -1.0]);
        assert_eq!(samples_f32(&f), vec![0.5, -1.0]);
    }

    #[test]
    fn unpack_bits_rows() {
        assert_eq!(unpack_bits(&[0b1010_0000, 0b1000_0000], 3, 2), vec![1, 0, 1, 1, 0, 0]);
    }

    #[test]
    fn plane_to_u8_depths() {
        assert_eq!(plane_to_u8(&[0xff, 0xff, 0, 0, 0x80, 0x00], 16, 3, 1).unwrap(), vec![255, 0, 128]);
        assert_eq!(plane_to_u8(&f32_to_bytes(&[0.0, 1.0, 2.0, -1.0, f32::NAN]), 32, 5, 1).unwrap(), vec![0, 255, 255, 0, 0]);
        assert_eq!(plane_to_u8(&[0b1000_0000], 1, 2, 1).unwrap(), vec![0, 255]);
        assert!(plane_to_u8(&[], 12, 0, 0).is_err());
    }
}

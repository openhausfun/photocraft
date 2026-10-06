//! Alpha / spot channel options <-> PSD `DisplayInfo` image resources (1077, and the older 1007).
//!
//! Layout (Adobe PSD spec, "DisplayInfo"): resource 1077 starts with a 4-byte version (1), then
//! one 14-byte record per extra channel: colour space (u16), four u16 colour components, opacity
//! (u16, 0..100), kind (u8: 0 = colour indicates selected areas, 1 = masked ("protected") areas,
//! 2 = spot channel) and one padding byte. Resource 1007 has the same records without the
//! version and without the spot kind.

use photocraft_color::{Color, ColorMode};
use photocraft_doc::{AlphaChannel, ColorIndicates};

pub(crate) const DISPLAY_INFO: u16 = 1077;
pub(crate) const DISPLAY_INFO_OLD: u16 = 1007;
/// Quick Mask information: channel id (u16) and an "initially empty" flag (u8).
pub(crate) const QUICK_MASK_INFO: u16 = 1022;

const SPACE_RGB: u16 = 0;
const SPACE_CMYK: u16 = 2;
const SPACE_LAB: u16 = 7;
const SPACE_GRAY: u16 = 8;

/// Resource 1077 for `channels` (in file order).
pub(crate) fn display_info(channels: &[&AlphaChannel]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    for c in channels {
        let (color, opacity, kind) = match c.spot {
            Some((ink, solidity)) => (ink, solidity, 2u8),
            None => (c.color, c.opacity, if c.indicates == ColorIndicates::SelectedAreas { 0 } else { 1 }),
        };
        let rgb = color.to_rgb();
        out.extend_from_slice(&SPACE_RGB.to_be_bytes());
        for v in [rgb[0], rgb[1], rgb[2], 0.0] {
            out.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes());
        }
        out.extend_from_slice(&((opacity.clamp(0.0, 1.0) * 100.0).round() as u16).to_be_bytes());
        out.push(kind);
        out.push(0);
    }
    out
}

/// Applies a DisplayInfo resource (`versioned` = 1077) to the imported channels in order.
pub(crate) fn apply_display_info(data: &[u8], versioned: bool, channels: &mut [AlphaChannel]) {
    let body = if versioned { data.get(4..).unwrap_or(&[]) } else { data };
    for (c, rec) in channels.iter_mut().zip(body.as_chunks::<14>().0) {
        let u = |i: usize| u16::from_be_bytes([rec[i], rec[i + 1]]);
        let comps = [u(2), u(4), u(6), u(8)];
        let color = decode_color(u(0), comps);
        let opacity = f32::from(u(10).min(100)) / 100.0;
        match rec[12] {
            2 => c.spot = Some((color, opacity)),
            k => {
                c.color = color;
                c.opacity = opacity;
                c.indicates = if k == 0 { ColorIndicates::SelectedAreas } else { ColorIndicates::MaskedAreas };
            }
        }
    }
}

pub(crate) fn decode_color(space: u16, c: [u16; 4]) -> Color {
    let n = |v: u16| f32::from(v) / 65535.0;
    match space {
        // Photoshop colour structures store CMYK inverted (65535 = no ink).
        SPACE_CMYK => {
            let cmyk = Color { mode: ColorMode::Cmyk, c: [1.0 - n(c[0]), 1.0 - n(c[1]), 1.0 - n(c[2]), 1.0 - n(c[3])], alpha: 1.0 };
            let rgb = cmyk.to_rgb();
            Color::rgb(rgb[0], rgb[1], rgb[2])
        }
        SPACE_LAB => {
            let l = f32::from(c[0].min(10000)) / 10000.0;
            let a = f32::from(c[1] as i16) / 100.0;
            let b = f32::from(c[2] as i16) / 100.0;
            let lab = Color { mode: ColorMode::Lab, c: [l, (a + 128.0) / 255.0, (b + 128.0) / 255.0, 0.0], alpha: 1.0 };
            let rgb = lab.to_rgb();
            Color::rgb(rgb[0], rgb[1], rgb[2])
        }
        SPACE_GRAY => {
            let v = 1.0 - f32::from(c[0].min(10000)) / 10000.0;
            Color::rgb(v, v, v)
        }
        _ => Color::rgb(n(c[0]), n(c[1]), n(c[2])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;
    use photocraft_raster::Surface;

    #[test]
    fn display_info_roundtrip() {
        let mut a = AlphaChannel::new("A", Surface::new(PixelFormat::GRAY8));
        a.color = Color::rgb(0.0, 1.0, 0.0);
        a.opacity = 0.3;
        a.indicates = ColorIndicates::SelectedAreas;
        let mut s = AlphaChannel::new("S", Surface::new(PixelFormat::GRAY8));
        s.spot = Some((Color::rgb(1.0, 0.0, 1.0), 0.8));
        let bytes = display_info(&[&a, &s]);
        assert_eq!(bytes.len(), 4 + 28);
        let mut back = vec![AlphaChannel::new("A", Surface::new(PixelFormat::GRAY8)), AlphaChannel::new("S", Surface::new(PixelFormat::GRAY8))];
        apply_display_info(&bytes, true, &mut back);
        assert_eq!(back[0].color, a.color);
        assert!((back[0].opacity - 0.3).abs() < 1e-6);
        assert_eq!(back[0].indicates, ColorIndicates::SelectedAreas);
        assert_eq!(back[1].spot, s.spot);
        // Truncated data is ignored rather than misread.
        let mut c = vec![AlphaChannel::new("x", Surface::new(PixelFormat::GRAY8))];
        apply_display_info(&bytes[..10], true, &mut c);
        assert_eq!(c[0].opacity, 0.5);
    }

    #[test]
    fn decodes_other_spaces() {
        let g = decode_color(SPACE_GRAY, [10000, 0, 0, 0]);
        assert_eq!(g.to_rgb(), [0.0; 3]);
        let lab = decode_color(SPACE_LAB, [10000, 0, 0, 0]).to_rgb();
        assert!(lab.iter().all(|v| *v > 0.95), "{lab:?}");
    }
}

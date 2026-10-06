//! Netpbm family, implemented here: P1–P6 (PBM/PGM/PPM, ASCII and binary),
//! P7 (PAM, including the common `CMYK`/`CMYK_ALPHA` tuple types) and PFM
//! (`PF` colour / `Pf` gray float).
//!
//! Writing picks the subtype from the image: float → PFM (gray/RGB), gray
//! → P5, RGB → P6, anything with alpha or CMYK → P7.

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, SampleType};
use crate::options::{EncodeOptions, Limits};

const F: Format = Format::Pnm;

fn err(msg: impl std::fmt::Display) -> CodecError {
    CodecError::malformed(F, msg)
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn skip_ws_and_comments(&mut self) {
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if c == b'#' {
                while self.pos < self.b.len() && self.b[self.pos] != b'\n' && self.b[self.pos] != b'\r' {
                    self.pos += 1;
                }
            } else if c.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn token(&mut self) -> Result<&'a [u8], CodecError> {
        self.skip_ws_and_comments();
        let start = self.pos;
        while self.pos < self.b.len() && !self.b[self.pos].is_ascii_whitespace() && self.b[self.pos] != b'#' {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(err("unexpected end of header"));
        }
        Ok(&self.b[start..self.pos])
    }

    fn uint(&mut self) -> Result<u32, CodecError> {
        let t = self.token()?;
        std::str::from_utf8(t).ok().and_then(|s| s.parse::<u32>().ok()).ok_or_else(|| err("invalid integer in header"))
    }

    /// Consume the single whitespace byte that separates header and raster.
    fn single_ws(&mut self) -> Result<(), CodecError> {
        match self.b.get(self.pos) {
            Some(c) if c.is_ascii_whitespace() => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(err("missing whitespace after header")),
        }
    }

    fn rest(&self) -> &'a [u8] {
        &self.b[self.pos.min(self.b.len())..]
    }
}

fn sample_for_maxval(maxval: u32) -> Result<SampleType, CodecError> {
    match maxval {
        1..=255 => Ok(SampleType::U8),
        256..=65535 => Ok(SampleType::U16),
        _ => Err(err(format!("invalid maxval {maxval}"))),
    }
}

/// Build an image from raw integer values in `[0, maxval]`.
fn from_values(w: u32, h: u32, layout: ChannelLayout, maxval: u32, values: Vec<u32>) -> Result<Image, CodecError> {
    let sample = sample_for_maxval(maxval)?;
    if values.iter().any(|&v| v > maxval) {
        return Err(err("sample exceeds maxval"));
    }
    match (sample, maxval) {
        (SampleType::U8, 255) => Image::from_u8(w, h, layout, values.into_iter().map(|v| v as u8).collect()),
        (SampleType::U16, 65535) => Image::from_u16(w, h, layout, &values.into_iter().map(|v| v as u16).collect::<Vec<_>>()),
        (SampleType::U8, m) => Image::from_u8(w, h, layout, values.into_iter().map(|v| ((v * 255 + m / 2) / m) as u8).collect()),
        (_, m) => Image::from_u16(w, h, layout, &values.into_iter().map(|v| ((v as u64 * 65535 + m as u64 / 2) / m as u64) as u16).collect::<Vec<_>>()),
    }
}

fn read_binary(data: &[u8], n: usize, maxval: u32) -> Result<Vec<u32>, CodecError> {
    if maxval < 256 {
        if data.len() < n {
            return Err(err("truncated raster"));
        }
        Ok(data[..n].iter().map(|&v| v as u32).collect())
    } else {
        if data.len() < n * 2 {
            return Err(err("truncated raster"));
        }
        Ok(data[..n * 2].as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]]) as u32).collect())
    }
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, CodecError> {
    if bytes.len() < 2 || bytes[0] != b'P' {
        return Err(err("not a Netpbm file"));
    }
    let kind = bytes[1];
    let mut r = Reader { b: bytes, pos: 2 };
    match kind {
        b'1' | b'4' => {
            let (w, h) = (r.uint()?, r.uint()?);
            limits.check(w, h, ChannelLayout::Gray, SampleType::U8)?;
            let n = w as usize * h as usize;
            let mut px = Vec::with_capacity(n);
            if kind == b'4' {
                r.single_ws()?;
                let row = (w as usize).div_ceil(8);
                let data = r.rest();
                if data.len() < row * h as usize {
                    return Err(err("truncated raster"));
                }
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        let bit = (data[y * row + x / 8] >> (7 - x % 8)) & 1;
                        px.push(if bit == 1 { 0 } else { 255 });
                    }
                }
            } else {
                while px.len() < n {
                    r.skip_ws_and_comments();
                    match r.b.get(r.pos) {
                        Some(b'0') => px.push(255),
                        Some(b'1') => px.push(0),
                        Some(_) => return Err(err("invalid PBM digit")),
                        None => return Err(err("truncated raster")),
                    }
                    r.pos += 1;
                }
            }
            Image::from_u8(w, h, ChannelLayout::Gray, px)
        }
        b'2' | b'3' | b'5' | b'6' => {
            let (w, h, maxval) = (r.uint()?, r.uint()?, r.uint()?);
            let layout = if matches!(kind, b'2' | b'5') { ChannelLayout::Gray } else { ChannelLayout::Rgb };
            limits.check(w, h, layout, sample_for_maxval(maxval)?)?;
            let n = w as usize * h as usize * layout.channels();
            let values = if matches!(kind, b'5' | b'6') {
                r.single_ws()?;
                read_binary(r.rest(), n, maxval)?
            } else {
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(r.uint()?);
                }
                v
            };
            from_values(w, h, layout, maxval, values)
        }
        b'7' => decode_pam(&mut r, limits),
        b'F' | b'f' => {
            let layout = if kind == b'F' { ChannelLayout::Rgb } else { ChannelLayout::Gray };
            let (w, h) = (r.uint()?, r.uint()?);
            let scale: f32 = std::str::from_utf8(r.token()?)
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|s: &f32| s.is_finite() && *s != 0.0)
                .ok_or_else(|| err("invalid PFM scale"))?;
            r.single_ws()?;
            limits.check(w, h, layout, SampleType::F32)?;
            let nc = layout.channels();
            let row = w as usize * nc;
            let data = r.rest();
            if data.len() < row * h as usize * 4 {
                return Err(err("truncated raster"));
            }
            let le = scale < 0.0;
            let mut out = vec![0f32; row * h as usize];
            for y in 0..h as usize {
                // PFM rows are stored bottom-to-top.
                let dst = (h as usize - 1 - y) * row;
                for i in 0..row {
                    let o = (y * row + i) * 4;
                    let b4 = [data[o], data[o + 1], data[o + 2], data[o + 3]];
                    out[dst + i] = if le { f32::from_le_bytes(b4) } else { f32::from_be_bytes(b4) };
                }
            }
            Image::from_f32(w, h, layout, &out)
        }
        _ => Err(err("unknown Netpbm subtype")),
    }
}

fn decode_pam(r: &mut Reader<'_>, limits: &Limits) -> Result<Image, CodecError> {
    let (mut w, mut h, mut depth, mut maxval) = (None, None, None, None);
    let mut tupltype = String::new();
    loop {
        let tok = r.token()?;
        match tok {
            b"WIDTH" => w = Some(r.uint()?),
            b"HEIGHT" => h = Some(r.uint()?),
            b"DEPTH" => depth = Some(r.uint()?),
            b"MAXVAL" => maxval = Some(r.uint()?),
            b"TUPLTYPE" => {
                let t = r.token()?;
                if !tupltype.is_empty() {
                    tupltype.push(' ');
                }
                tupltype.push_str(std::str::from_utf8(t).map_err(|_| err("bad TUPLTYPE"))?);
            }
            b"ENDHDR" => break,
            _ => return Err(err("unknown PAM header field")),
        }
    }
    r.single_ws()?;
    let (w, h, depth, maxval) = match (w, h, depth, maxval) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        _ => return Err(err("incomplete PAM header")),
    };
    let layout = match (tupltype.as_str(), depth) {
        ("BLACKANDWHITE" | "GRAYSCALE", 1) | ("", 1) => ChannelLayout::Gray,
        ("BLACKANDWHITE_ALPHA" | "GRAYSCALE_ALPHA", 2) | ("", 2) => ChannelLayout::GrayA,
        ("RGB", 3) | ("", 3) => ChannelLayout::Rgb,
        ("RGB_ALPHA", 4) | ("", 4) => ChannelLayout::Rgba,
        ("CMYK", 4) => ChannelLayout::Cmyk,
        ("CMYK_ALPHA", 5) => ChannelLayout::CmykA,
        (t, d) => {
            return Err(CodecError::unsupported(F, format!("PAM tuple type {t:?} with depth {d}")));
        }
    };
    limits.check(w, h, layout, sample_for_maxval(maxval)?)?;
    let n = w as usize * h as usize * layout.channels();
    let values = read_binary(r.rest(), n, maxval)?;
    from_values(w, h, layout, maxval, values)
}

pub(crate) fn encode(src: &Image, plan: Plan, _opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let img = src.converted(plan.layout, plan.sample);
    let (w, h) = img.dimensions();
    let layout = img.layout();
    let mut out = Vec::new();
    match img.sample_type() {
        SampleType::F32 => {
            let tag = match layout {
                ChannelLayout::Gray => "Pf",
                ChannelLayout::Rgb => "PF",
                l => return Err(CodecError::encode(F, format!("PFM cannot store {l:?}"))),
            };
            out.extend_from_slice(format!("{tag}\n{w} {h}\n-1.0\n").as_bytes());
            let s = img.to_f32_samples().unwrap_or_default();
            let row = w as usize * layout.channels();
            for y in (0..h as usize).rev() {
                for v in &s[y * row..(y + 1) * row] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        sample @ (SampleType::U8 | SampleType::U16) => {
            let maxval = if sample == SampleType::U8 { 255 } else { 65535 };
            match layout {
                ChannelLayout::Gray => out.extend_from_slice(format!("P5\n{w} {h}\n{maxval}\n").as_bytes()),
                ChannelLayout::Rgb => out.extend_from_slice(format!("P6\n{w} {h}\n{maxval}\n").as_bytes()),
                l => {
                    let t = match l {
                        ChannelLayout::GrayA => "GRAYSCALE_ALPHA",
                        ChannelLayout::Rgba => "RGB_ALPHA",
                        ChannelLayout::Cmyk => "CMYK",
                        _ => "CMYK_ALPHA",
                    };
                    out.extend_from_slice(format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH {}\nMAXVAL {maxval}\nTUPLTYPE {t}\nENDHDR\n", l.channels()).as_bytes());
                }
            }
            if sample == SampleType::U8 {
                out.extend_from_slice(img.data());
            } else {
                for v in img.to_u16_samples().unwrap_or_default() {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        s => return Err(CodecError::encode(F, format!("unsupported sample {s:?}"))),
    }
    Ok(out)
}

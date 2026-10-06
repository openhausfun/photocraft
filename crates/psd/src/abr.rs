//! Photoshop brush files (`.abr`): the legacy v1/v2 layout and the sectioned v6+ layout.
//!
//! Implemented clean-room from public descriptions of the format (the layout notes published with
//! open-source readers such as GIMP's and ag-psd's, and observation of files written by Photoshop).
//! No code was copied. All numbers are big-endian.
//!
//! ```text
//! v1 / v2   := version u16 (1|2), count u16, count × brush
//! brush     := type u16 (1 computed, 2 sampled), length u32, body[length]
//! computed  := misc u32, spacing u16 (%), diameter u16, roundness u16 (%), angle i16, hardness u16 (%)
//! sampled   := misc u32, spacing u16 (%), [v2: name (u32 length + UTF-16 incl. NUL)],
//!              anti-alias u8, short bounds 4 × i16, bounds 4 × i32 (top, left, bottom, right),
//!              depth u16 (8|16), compression u8 (0 raw, 1 PackBits with a u16 row-length table)
//!
//! v6+       := version u16 (6..=10), subversion u16 (1|2), sections…
//! section   := "8BIM", key [4], length u32, data[length] (padded to 4 bytes)
//! "samp"    := entries: length u32 (padded to 4), Pascal uuid, skip (10 if subversion 1,
//!              264 if 2), bounds 4 × i32, depth i16, compression u8, data
//! "patt"    := patterns as in the `Patt` global block
//! "desc"    := version u32 (16) + ActionDescriptor; key `Brsh` lists `brushPreset` objects
//! ```
//!
//! [`parse`] never panics: every length is bounds-checked, sizes are capped ([`MAX_EDGE`],
//! [`MAX_TOTAL_BYTES`]) and a damaged brush is skipped with a warning when the rest of the file
//! is still readable. [`write_v12`] and [`write_v6`] produce files in the same layouts (used by
//! the tests and fuzz seeds; exporting brushes reuses them).

use crate::compression::{Compression, PlaneLayout, decode_planes, encode_planes};
use crate::descriptor::{Descriptor, Value, VersionedDescriptor};
use crate::error::{PsdError, Result};
use crate::header::Version;
use crate::patterns::{PsdPattern, parse_pattern_block, write_pattern_block};

/// Largest sampled tip edge accepted (Photoshop's own limit is 5000 px).
pub const MAX_EDGE: u32 = 8192;
/// Most brushes read from one file.
pub const MAX_BRUSHES: usize = 20_000;
/// Cap on decoded sample bytes over the whole file.
pub const MAX_TOTAL_BYTES: usize = 512 << 20;

/// A sampled tip: one grayscale plane where the maximum value is full paint.
#[derive(Debug, Clone, PartialEq)]
pub struct AbrSample {
    /// The uuid that `sampledData` in a preset descriptor refers to (v6+), or empty.
    pub id: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per sample: 8 or 16.
    pub depth: u16,
    /// Decoded samples, row-major (`width × height × depth / 8` bytes, 16-bit big-endian).
    pub data: Vec<u8>,
}

impl AbrSample {
    /// Samples as 0..1 paint amounts.
    pub fn to_unit(&self) -> Vec<f32> {
        if self.depth == 16 {
            self.data.as_chunks::<2>().0.iter().map(|c| f32::from(u16::from_be_bytes(*c)) / 65535.0).collect()
        } else {
            self.data.iter().map(|&v| f32::from(v) / 255.0).collect()
        }
    }
}

/// A legacy (v1/v2) tip.
#[derive(Debug, Clone, PartialEq)]
pub enum LegacyTip {
    /// Computed elliptical tip.
    Computed {
        /// Diameter in pixels.
        diameter: u16,
        /// 0..100 %.
        hardness: u16,
        /// Degrees.
        angle: i16,
        /// 0..100 %.
        roundness: u16,
    },
    /// Sampled tip.
    Sampled(AbrSample),
}

/// A legacy (v1/v2) brush.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyBrush {
    /// Name (v2 sampled brushes only; empty otherwise).
    pub name: String,
    /// Spacing in percent of the diameter (0 = spacing off).
    pub spacing: u16,
    /// Anti-aliasing flag (sampled tips).
    pub anti_alias: bool,
    /// The tip.
    pub tip: LegacyTip,
}

/// A parsed brush file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AbrFile {
    /// File version (1, 2, 6, 7, 9 or 10).
    pub version: u16,
    /// v6+ subversion (1 or 2); 0 for v1/v2.
    pub subversion: u16,
    /// v1/v2 brushes, in file order.
    pub legacy: Vec<LegacyBrush>,
    /// v6+ sampled tips (`samp`), in file order.
    pub samples: Vec<AbrSample>,
    /// v6+ embedded patterns (`patt`), referenced by texture settings.
    pub patterns: Vec<PsdPattern>,
    /// v6+ brush presets: the `brushPreset` objects of the `desc` section, in file order.
    pub presets: Vec<Descriptor>,
    /// Problems that did not stop the parse (skipped brushes, unknown sections…).
    pub warnings: Vec<String>,
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }
    fn left(&self) -> usize {
        self.b.len().saturating_sub(self.p)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or(PsdError::LimitExceeded("abr length overflow"))?;
        let s = self.b.get(self.p..end).ok_or(PsdError::UnexpectedEof { offset: self.p, needed: end.saturating_sub(self.b.len()) })?;
        self.p = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.arr()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.arr()?))
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
}

/// Bytes budget shared by all samples of one file.
struct Budget(usize);

impl Budget {
    fn spend(&mut self, n: usize) -> Result<()> {
        self.0 = self.0.checked_sub(n).ok_or(PsdError::LimitExceeded("abr samples exceed MAX_TOTAL_BYTES"))?;
        Ok(())
    }
}

/// Reads bounds (top, left, bottom, right), depth and compression, then the pixel data.
fn read_sample_body(r: &mut Rd, id: String, budget: &mut Budget) -> Result<AbrSample> {
    let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
    let w = i64::from(right) - i64::from(left);
    let h = i64::from(bottom) - i64::from(top);
    if w <= 0 || h <= 0 {
        return Err(PsdError::invalid(format!("brush sample bounds {w}×{h}")));
    }
    if w > i64::from(MAX_EDGE) || h > i64::from(MAX_EDGE) {
        return Err(PsdError::LimitExceeded("brush sample larger than MAX_EDGE"));
    }
    let (w, h) = (w as u32, h as u32);
    let depth = r.u16()?;
    if depth != 8 && depth != 16 {
        return Err(PsdError::invalid(format!("brush sample depth {depth}")));
    }
    let comp = r.u8()?;
    let compression = match comp {
        0 => Compression::Raw,
        1 => Compression::Rle,
        c => return Err(PsdError::Unsupported(format!("brush sample compression {c}"))),
    };
    let layout = PlaneLayout { planes: 1, width: w as usize, height: h as usize, depth, version: Version::Psd };
    budget.spend(layout.decoded_len()?)?;
    let rest = r.take(r.left())?;
    let data = decode_planes(compression, rest, &layout)?;
    Ok(AbrSample { id, width: w, height: h, depth, data })
}

fn read_ucs2(r: &mut Rd) -> Result<String> {
    let n = r.u32()? as usize;
    if n > 4096 {
        return Err(PsdError::LimitExceeded("abr name length"));
    }
    let units: Vec<u16> = r.take(n * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    Ok(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
}

fn parse_v12(r: &mut Rd, version: u16, out: &mut AbrFile) -> Result<()> {
    let count = r.u16()?;
    let mut budget = Budget(MAX_TOTAL_BYTES);
    for i in 0..count {
        if r.left() == 0 {
            out.warnings.push(format!("file ends after {i} of {count} brushes"));
            break;
        }
        let ty = r.u16()?;
        let len = r.u32()? as usize;
        let body = match r.take(len) {
            Ok(b) => b,
            Err(_) => {
                out.warnings.push(format!("brush {}: truncated", i + 1));
                break;
            }
        };
        let mut b = Rd::new(body);
        let parsed = (|| -> Result<Option<LegacyBrush>> {
            match ty {
                1 => {
                    let _misc = b.u32()?;
                    let spacing = b.u16()?;
                    let diameter = b.u16()?;
                    let roundness = b.u16()?;
                    let angle = b.u16()? as i16;
                    let hardness = b.u16()?;
                    Ok(Some(LegacyBrush { name: String::new(), spacing, anti_alias: true, tip: LegacyTip::Computed { diameter, hardness, angle, roundness } }))
                }
                2 => {
                    let _misc = b.u32()?;
                    let spacing = b.u16()?;
                    let name = if version == 2 { read_ucs2(&mut b)? } else { String::new() };
                    let anti_alias = b.u8()? != 0;
                    b.skip(8)?; // short bounds (superseded by the 32-bit bounds)
                    let s = read_sample_body(&mut b, String::new(), &mut budget)?;
                    Ok(Some(LegacyBrush { name, spacing, anti_alias, tip: LegacyTip::Sampled(s) }))
                }
                other => {
                    out.warnings.push(format!("brush {}: unknown brush type {other} skipped", i + 1));
                    Ok(None)
                }
            }
        })();
        match parsed {
            Ok(Some(lb)) => out.legacy.push(lb),
            Ok(None) => {}
            Err(PsdError::LimitExceeded(m)) if m.contains("MAX_TOTAL_BYTES") => return Err(PsdError::LimitExceeded(m)),
            Err(e) => out.warnings.push(format!("brush {}: {e}; skipped", i + 1)),
        }
    }
    Ok(())
}

fn read_samp(data: &[u8], subversion: u16, out: &mut AbrFile, budget: &mut Budget) -> Result<()> {
    let mut r = Rd::new(data);
    while r.left() >= 4 {
        if out.samples.len() >= MAX_BRUSHES {
            out.warnings.push("too many sampled tips; the rest were skipped".into());
            break;
        }
        let len = r.u32()? as usize;
        if len == 0 {
            break;
        }
        let padded = len.checked_add(3).ok_or(PsdError::LimitExceeded("abr sample length"))? & !3;
        let body = match r.take(padded.min(r.left())) {
            Ok(b) if b.len() >= len => b,
            _ => {
                out.warnings.push(format!("sampled tip {}: truncated", out.samples.len() + 1));
                break;
            }
        };
        let mut b = Rd::new(body);
        let parsed = (|| -> Result<AbrSample> {
            let n = b.u8()? as usize;
            let id = String::from_utf8_lossy(b.take(n)?).to_string();
            b.skip(if subversion == 1 { 10 } else { 264 })?;
            read_sample_body(&mut b, id, budget)
        })();
        match parsed {
            Ok(s) => out.samples.push(s),
            Err(PsdError::LimitExceeded(m)) if m.contains("MAX_TOTAL_BYTES") => return Err(PsdError::LimitExceeded(m)),
            Err(e) => out.warnings.push(format!("sampled tip {}: {e}; skipped", out.samples.len() + 1)),
        }
    }
    Ok(())
}

fn read_desc(data: &[u8], out: &mut AbrFile) {
    match VersionedDescriptor::parse_prefix(data) {
        Ok((vd, _)) => match vd.descriptor.get("Brsh") {
            Some(Value::List(items)) => {
                for v in items.iter().take(MAX_BRUSHES) {
                    if let Value::Descriptor(d) = v {
                        out.presets.push(d.clone());
                    }
                }
            }
            _ => out.warnings.push("brush settings (desc) hold no brush list".into()),
        },
        Err(e) => out.warnings.push(format!("brush settings (desc) unreadable: {e}")),
    }
}

fn parse_v6(r: &mut Rd, out: &mut AbrFile) -> Result<()> {
    out.subversion = r.u16()?;
    if !matches!(out.subversion, 1 | 2) {
        return Err(PsdError::Unsupported(format!("brush file subversion {}", out.subversion)));
    }
    let mut budget = Budget(MAX_TOTAL_BYTES);
    while r.left() >= 12 {
        let sig = r.arr::<4>()?;
        if &sig != b"8BIM" {
            out.warnings.push(format!("unexpected data at offset {} ignored", r.p - 4));
            break;
        }
        let key = r.arr::<4>()?;
        let len = r.u32()? as usize;
        let data = match r.take(len) {
            Ok(d) => d,
            Err(_) => {
                out.warnings.push(format!("section {} truncated", String::from_utf8_lossy(&key)));
                r.take(r.left())?
            }
        };
        // Sections are padded to 4 bytes; tolerate writers that don't pad.
        let pad = (4 - len % 4) % 4;
        if pad > 0 && r.b.get(r.p..r.p + 4) != Some(b"8BIM") && r.b.get(r.p + pad..r.p + pad + 4) == Some(b"8BIM") {
            r.p += pad;
        }
        match &key {
            b"samp" => read_samp(data, out.subversion, out, &mut budget)?,
            b"patt" => match parse_pattern_block(data) {
                Ok(p) => out.patterns.extend(p),
                Err(e) => out.warnings.push(format!("embedded patterns unreadable: {e}")),
            },
            b"desc" => read_desc(data, out),
            // Preset hierarchy (groups) and tool presets: not needed for the tips.
            b"phry" | b"lPdc" => {}
            k => out.warnings.push(format!("section {} ignored", String::from_utf8_lossy(k))),
        }
    }
    Ok(())
}

/// Parses an `.abr` file.
pub fn parse(data: &[u8]) -> Result<AbrFile> {
    let mut r = Rd::new(data);
    let version = r.u16()?;
    let mut out = AbrFile { version, ..Default::default() };
    match version {
        1 | 2 => parse_v12(&mut r, version, &mut out)?,
        6..=10 => parse_v6(&mut r, &mut out)?,
        v => return Err(PsdError::Unsupported(format!("brush file version {v}"))),
    }
    if out.legacy.is_empty() && out.samples.is_empty() && out.presets.is_empty() {
        let why = out.warnings.first().map(|w| format!(" ({w})")).unwrap_or_default();
        return Err(PsdError::invalid(format!("the brush file holds no readable brushes{why}")));
    }
    Ok(out)
}

// ------------------------------------------------------------------ writing

fn put_u16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}

fn write_sample_body(o: &mut Vec<u8>, s: &AbrSample, rle: bool) -> Result<()> {
    if s.width == 0 || s.height == 0 || s.width > MAX_EDGE || s.height > MAX_EDGE || !matches!(s.depth, 8 | 16) {
        return Err(PsdError::LimitExceeded("brush sample size or depth"));
    }
    for v in [0, 0, s.height as i32, s.width as i32] {
        o.extend_from_slice(&v.to_be_bytes());
    }
    put_u16(o, s.depth);
    o.push(u8::from(rle));
    let layout = PlaneLayout { planes: 1, width: s.width as usize, height: s.height as usize, depth: s.depth, version: Version::Psd };
    o.extend_from_slice(&encode_planes(if rle { Compression::Rle } else { Compression::Raw }, &s.data, &layout)?);
    Ok(())
}

/// Writes a v1 or v2 file. Sampled tips are PackBits-compressed when `rle`.
pub fn write_v12(version: u16, brushes: &[LegacyBrush], rle: bool) -> Result<Vec<u8>> {
    if !matches!(version, 1 | 2) || brushes.len() > usize::from(u16::MAX) {
        return Err(PsdError::Unsupported(format!("writing brush file version {version}")));
    }
    let mut o = Vec::new();
    put_u16(&mut o, version);
    put_u16(&mut o, brushes.len() as u16);
    for b in brushes {
        let mut body = Vec::new();
        put_u32(&mut body, 0);
        put_u16(&mut body, b.spacing);
        let ty = match &b.tip {
            LegacyTip::Computed { diameter, hardness, angle, roundness } => {
                for v in [*diameter, *roundness, *angle as u16, *hardness] {
                    put_u16(&mut body, v);
                }
                1
            }
            LegacyTip::Sampled(s) => {
                if version == 2 {
                    let units: Vec<u16> = b.name.encode_utf16().chain(std::iter::once(0)).collect();
                    put_u32(&mut body, units.len() as u32);
                    for u in units {
                        put_u16(&mut body, u);
                    }
                }
                body.push(u8::from(b.anti_alias));
                for v in [0u16, 0, s.height.min(0xffff) as u16, s.width.min(0xffff) as u16] {
                    put_u16(&mut body, v);
                }
                write_sample_body(&mut body, s, rle)?;
                2
            }
        };
        put_u16(&mut o, ty);
        put_u32(&mut o, body.len() as u32);
        o.extend_from_slice(&body);
    }
    Ok(o)
}

fn section(o: &mut Vec<u8>, key: &[u8; 4], data: &[u8]) {
    o.extend_from_slice(b"8BIM");
    o.extend_from_slice(key);
    let pad = (4 - data.len() % 4) % 4;
    put_u32(o, (data.len() + pad) as u32);
    o.extend_from_slice(data);
    o.extend(std::iter::repeat_n(0u8, pad));
}

/// Writes a v6 (`subversion` 1 or 2) file: sampled tips, embedded patterns and the preset
/// descriptors (each a `brushPreset` object), listed under `Brsh`.
pub fn write_v6(subversion: u16, samples: &[AbrSample], patterns: &[PsdPattern], presets: &[Descriptor], rle: bool) -> Result<Vec<u8>> {
    if !matches!(subversion, 1 | 2) {
        return Err(PsdError::Unsupported(format!("brush file subversion {subversion}")));
    }
    let mut o = Vec::new();
    put_u16(&mut o, 6);
    put_u16(&mut o, subversion);
    let mut samp = Vec::new();
    for s in samples {
        let mut e = Vec::new();
        let id = s.id.as_bytes();
        let n = id.len().min(255);
        e.push(n as u8);
        e.extend_from_slice(&id[..n]);
        e.extend(std::iter::repeat_n(0u8, if subversion == 1 { 10 } else { 264 }));
        write_sample_body(&mut e, s, rle)?;
        put_u32(&mut samp, e.len() as u32);
        let pad = (4 - e.len() % 4) % 4;
        samp.extend_from_slice(&e);
        samp.extend(std::iter::repeat_n(0u8, pad));
    }
    section(&mut o, b"samp", &samp);
    if !patterns.is_empty() {
        section(&mut o, b"patt", &write_pattern_block(patterns)?);
    }
    let list = Value::List(presets.iter().cloned().map(Value::Descriptor).collect());
    let desc = VersionedDescriptor::new(Descriptor::new("null").with("Brsh", list));
    section(&mut o, b"desc", &desc.to_bytes());
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::UnicodeString;

    fn sample(id: &str, w: u32, h: u32, depth: u16) -> AbrSample {
        let bpp = usize::from(depth / 8);
        let data = (0..w as usize * h as usize * bpp).map(|i| if (i / bpp) % 3 == 0 { 255 } else { (i % 7) as u8 * 30 }).collect();
        AbrSample { id: id.into(), width: w, height: h, depth, data }
    }

    #[test]
    fn v1_v2_round_trip_raw_and_rle() {
        for version in [1u16, 2] {
            for rle in [false, true] {
                let brushes = vec![
                    LegacyBrush {
                        name: String::new(),
                        spacing: 25,
                        anti_alias: true,
                        tip: LegacyTip::Computed { diameter: 19, hardness: 80, angle: -30, roundness: 50 },
                    },
                    LegacyBrush {
                        name: if version == 2 { "Leaf ✓".into() } else { String::new() },
                        spacing: 40,
                        anti_alias: false,
                        tip: LegacyTip::Sampled(sample("", 13, 9, 8)),
                    },
                    LegacyBrush { name: String::new(), spacing: 10, anti_alias: true, tip: LegacyTip::Sampled(sample("", 4, 6, 16)) },
                ];
                let bytes = write_v12(version, &brushes, rle).unwrap();
                let f = parse(&bytes).unwrap();
                assert_eq!(f.version, version);
                assert_eq!(f.legacy, brushes, "v{version} rle={rle}");
                assert!(f.warnings.is_empty(), "{:?}", f.warnings);
            }
        }
    }

    #[test]
    fn v6_round_trip_both_subversions() {
        let samples = vec![sample("$abc", 20, 11, 8), sample("$def", 5, 5, 16)];
        let preset = Descriptor::new("brushPreset")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Grass")))
            .with("Brsh", Value::Descriptor(Descriptor::new("sampledBrush").with("sampledData", Value::Text(UnicodeString::new_nul("$abc")))));
        for sub in [1u16, 2] {
            for rle in [false, true] {
                let bytes = write_v6(sub, &samples, &[], std::slice::from_ref(&preset), rle).unwrap();
                let f = parse(&bytes).unwrap();
                assert_eq!((f.version, f.subversion), (6, sub));
                assert_eq!(f.samples, samples);
                assert_eq!(f.presets, vec![preset.clone()]);
                assert!(f.warnings.is_empty(), "{:?}", f.warnings);
            }
        }
        assert_eq!(samples[1].to_unit().len(), 25);
    }

    #[test]
    fn hostile_input_errors_without_panicking() {
        assert!(parse(&[]).is_err());
        assert!(parse(&[0, 3, 0, 0]).is_err());
        assert!(parse(&[0, 6, 0, 9]).is_err());
        // Absurd sample bounds.
        let mut s = sample("", 2, 2, 8);
        s.width = MAX_EDGE + 1;
        assert!(write_v12(1, &[LegacyBrush { name: String::new(), spacing: 1, anti_alias: true, tip: LegacyTip::Sampled(s) }], false).is_err());
        let good = write_v6(2, &[sample("$a", 30, 30, 8)], &[], &[Descriptor::new("brushPreset")], true).unwrap();
        for cut in 0..good.len() {
            let _ = parse(&good[..cut]);
        }
        let v2 = write_v12(2, &[LegacyBrush { name: "x".into(), spacing: 1, anti_alias: true, tip: LegacyTip::Sampled(sample("", 9, 9, 16)) }], true).unwrap();
        for cut in 0..v2.len() {
            let _ = parse(&v2[..cut]);
        }
        // Flip every byte once: never panics.
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0xa5;
            let _ = parse(&b);
        }
        // A huge declared count with no data is an error, not an allocation.
        assert!(parse(&[0, 2, 0xff, 0xff]).is_err());
    }
}

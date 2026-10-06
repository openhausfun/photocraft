//! Patterns: the global `Patt` / `Pat2` / `Pat3` tagged blocks (8 / 16 / 32-bit documents) and
//! standalone `.pat` files.
//!
//! Layout per the public Photoshop File Formats Specification ("Patterns" and "Virtual Memory
//! Array List"):
//!
//! ```text
//! pattern  := version u32 (1), image mode u32, height i16, width i16,
//!             name (Unicode string), unique id (Pascal string, unpadded),
//!             [indexed: 256 × RGB palette], virtual memory array list
//! VMA list := version u32 (3), length u32, rect (top, left, bottom, right i32),
//!             channel count u32, then count + 2 arrays (user mask, sheet mask)
//! array    := written u32 (0 = absent), length u32 (0 = absent), depth u32, rect,
//!             depth u16, compression u8 (0 raw, 1 PackBits RLE), data
//! ```
//!
//! In the tagged blocks each pattern is prefixed by its u32 length and padded to 4 bytes; a
//! `.pat` file is `8BPT`, version u16 (1), count u32, then the patterns without length prefixes.
//! Photoshop writes the channel count as 24 and only marks the real channels as written; like
//! ag-psd (MIT) we take the first written arrays as the colour channels and the next one as
//! transparency.

use crate::compression::{Compression, PlaneLayout, decode_planes, encode_planes};
use crate::error::{PsdError, Result};
use crate::header::Version;

/// Largest pattern edge accepted (Photoshop's own limit is far lower).
const MAX_EDGE: u32 = 30_000;

/// One pattern tile with planar, big-endian samples.
#[derive(Debug, Clone, PartialEq)]
pub struct PsdPattern {
    /// PSD colour mode number (1 gray, 2 indexed, 3 RGB, 4 CMYK, 7 multichannel, 8 duotone, 9 Lab).
    pub mode: u32,
    /// Tile width in pixels.
    pub width: u32,
    /// Tile height in pixels.
    pub height: u32,
    /// Name without the trailing NUL (built-ins look like `$$$/Patterns/…=Water`).
    pub name: String,
    /// Unique id (usually a UUID).
    pub id: String,
    /// Indexed-colour palette (768 bytes, RGB triplets).
    pub palette: Option<Vec<u8>>,
    /// Bits per sample: 1, 8, 16 or 32.
    pub depth: u16,
    /// Decoded colour planes (`width × height` samples each).
    pub channels: Vec<Vec<u8>>,
    /// Decoded transparency plane, if any.
    pub alpha: Option<Vec<u8>>,
}

/// Colour channels implied by a PSD colour mode.
pub fn mode_channels(mode: u32) -> usize {
    match mode {
        3 | 9 => 3,
        4 => 4,
        _ => 1,
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or(PsdError::LimitExceeded("pattern length overflow"))?;
        if end > self.b.len() {
            return Err(PsdError::UnexpectedEof { offset: self.p, needed: end - self.b.len() });
        }
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn rect(&mut self) -> Result<[i32; 4]> {
        Ok([self.i32()?, self.i32()?, self.i32()?, self.i32()?])
    }
}

fn read_pattern(r: &mut Rd) -> Result<PsdPattern> {
    let version = r.u32()?;
    if version != 1 {
        return Err(PsdError::invalid(format!("pattern version {version}")));
    }
    let mode = r.u32()?;
    let _h = r.u16()?;
    let _w = r.u16()?;
    let n = r.u32()? as usize;
    if n > 65_536 {
        return Err(PsdError::LimitExceeded("pattern name length"));
    }
    let units: Vec<u16> = r.take(n * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let name = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
    let idl = r.u8()? as usize;
    let id = String::from_utf8_lossy(r.take(idl)?).to_string();
    let palette = if mode == 2 {
        let p = r.take(768)?.to_vec();
        // Some writers follow the palette with 4 extra bytes (ag-psd skips them).
        if r.b.get(r.p..r.p + 4).is_some_and(|v| v != [0, 0, 0, 3]) {
            r.take(4)?;
        }
        Some(p)
    } else {
        None
    };
    let vver = r.u32()?;
    if vver != 3 {
        return Err(PsdError::invalid(format!("virtual memory array list version {vver}")));
    }
    let len = r.u32()? as usize;
    let body = r.take(len)?;
    let mut v = Rd { b: body, p: 0 };
    let [top, left, bottom, right] = v.rect()?;
    let (w, h) = ((right - left).max(0) as u32, (bottom - top).max(0) as u32);
    if w > MAX_EDGE || h > MAX_EDGE {
        return Err(PsdError::LimitExceeded("pattern size"));
    }
    let count = v.u32()?;
    if count > 64 {
        return Err(PsdError::LimitExceeded("pattern channel count"));
    }
    let mut planes = Vec::new();
    let mut depth = 8u16;
    for _ in 0..count + 2 {
        if v.p >= body.len() {
            break;
        }
        if v.u32()? == 0 {
            continue;
        }
        let alen = v.u32()? as usize;
        if alen == 0 {
            continue;
        }
        if alen < 23 {
            return Err(PsdError::invalid("short virtual memory array"));
        }
        let d32 = v.u32()?;
        let [ct, cl, cb, cr] = v.rect()?;
        let d16 = v.u16()?;
        let comp = v.u8()?;
        let data = v.take(alen - 23)?;
        let d = if matches!(d16, 1 | 8 | 16 | 32) { d16 } else { d32 as u16 };
        if !matches!(d, 1 | 8 | 16 | 32) {
            return Err(PsdError::invalid(format!("pattern depth {d}")));
        }
        depth = d;
        let (cw, ch) = ((cr - cl).max(0) as usize, (cb - ct).max(0) as usize);
        let layout = PlaneLayout { planes: 1, width: cw, height: ch, depth: d, version: Version::Psd };
        let compression = if comp == 1 {
            Compression::Rle
        } else if comp == 0 {
            Compression::Raw
        } else {
            Compression::Unknown(u16::from(comp))
        };
        let plane = decode_planes(compression, data, &layout)?;
        // Place a channel rect smaller than the pattern rect into a full plane.
        let bpp = (usize::from(d) / 8).max(1);
        let full = if d == 1 || (cw, ch) == (w as usize, h as usize) {
            plane
        } else {
            let mut out = vec![0u8; w as usize * h as usize * bpp];
            for y in 0..ch {
                let ty = y as i32 + ct - top;
                if ty < 0 || ty >= h as i32 {
                    continue;
                }
                for x in 0..cw {
                    let tx = x as i32 + cl - left;
                    if tx < 0 || tx >= w as i32 {
                        continue;
                    }
                    let s = (y * cw + x) * bpp;
                    let o = (ty as usize * w as usize + tx as usize) * bpp;
                    out[o..o + bpp].copy_from_slice(&plane[s..s + bpp]);
                }
            }
            out
        };
        planes.push(full);
    }
    let nc = mode_channels(mode);
    if planes.len() < nc {
        return Err(PsdError::invalid("pattern has fewer channels than its colour mode"));
    }
    let alpha = (planes.len() > nc).then(|| planes.remove(nc));
    planes.truncate(nc);
    Ok(PsdPattern { mode, width: w, height: h, name, id, palette, depth, channels: planes, alpha })
}

fn write_pattern(p: &PsdPattern, out: &mut Vec<u8>) -> Result<()> {
    if p.width > MAX_EDGE || p.height > MAX_EDGE || p.width > i16::MAX as u32 || p.height > i16::MAX as u32 {
        return Err(PsdError::LimitExceeded("pattern size"));
    }
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&p.mode.to_be_bytes());
    out.extend_from_slice(&(p.height as u16).to_be_bytes());
    out.extend_from_slice(&(p.width as u16).to_be_bytes());
    let units: Vec<u16> = p.name.encode_utf16().chain(std::iter::once(0)).collect();
    out.extend_from_slice(&(units.len() as u32).to_be_bytes());
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
    let id = p.id.as_bytes();
    let idl = id.len().min(255);
    out.push(idl as u8);
    out.extend_from_slice(&id[..idl]);
    if p.mode == 2 {
        let mut pal = p.palette.clone().unwrap_or_default();
        pal.resize(768, 0);
        out.extend_from_slice(&pal);
    }
    let rect = [0i32, 0, p.height as i32, p.width as i32];
    let mut body = Vec::new();
    for v in rect {
        body.extend_from_slice(&v.to_be_bytes());
    }
    const SLOTS: u32 = 24;
    body.extend_from_slice(&SLOTS.to_be_bytes());
    let layout = PlaneLayout { planes: 1, width: p.width as usize, height: p.height as usize, depth: p.depth, version: Version::Psd };
    let array = |plane: &[u8], body: &mut Vec<u8>| -> Result<()> {
        let (comp, data) =
            if p.depth == 8 { (1u8, encode_planes(Compression::Rle, plane, &layout)?) } else { (0u8, encode_planes(Compression::Raw, plane, &layout)?) };
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&((23 + data.len()) as u32).to_be_bytes());
        body.extend_from_slice(&u32::from(p.depth).to_be_bytes());
        for v in rect {
            body.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(&p.depth.to_be_bytes());
        body.push(comp);
        body.extend_from_slice(&data);
        Ok(())
    };
    let nc = p.channels.len().min(SLOTS as usize);
    for c in &p.channels[..nc] {
        array(c, &mut body)?;
    }
    for _ in nc..SLOTS as usize {
        body.extend_from_slice(&0u32.to_be_bytes());
    }
    // User mask slot: transparency; sheet mask slot: unused.
    match &p.alpha {
        Some(a) => array(a, &mut body)?,
        None => body.extend_from_slice(&0u32.to_be_bytes()),
    }
    body.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&3u32.to_be_bytes());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(())
}

/// Parses the data of a `Patt` / `Pat2` / `Pat3` global block.
pub fn parse_pattern_block(data: &[u8]) -> Result<Vec<PsdPattern>> {
    let mut r = Rd { b: data, p: 0 };
    let mut out = Vec::new();
    while r.p + 4 <= data.len() {
        let len = r.u32()? as usize;
        if len == 0 {
            break;
        }
        let body = r.take(len)?;
        out.push(read_pattern(&mut Rd { b: body, p: 0 })?);
        let pad = (4 - len % 4) % 4;
        r.p = (r.p + pad).min(data.len());
    }
    Ok(out)
}

/// Serializes patterns as `Patt` / `Pat2` / `Pat3` block data.
pub fn write_pattern_block(patterns: &[PsdPattern]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for p in patterns {
        let mut one = Vec::new();
        write_pattern(p, &mut one)?;
        out.extend_from_slice(&(one.len() as u32).to_be_bytes());
        let pad = (4 - one.len() % 4) % 4;
        out.extend_from_slice(&one);
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    Ok(out)
}

/// Tagged-block key for patterns of a document at `depth` bits.
pub fn block_key(depth: u16) -> [u8; 4] {
    match depth {
        16 => *b"Pat2",
        32 => *b"Pat3",
        _ => *b"Patt",
    }
}

/// Parses a `.pat` pattern file.
pub fn parse_pat_file(data: &[u8]) -> Result<Vec<PsdPattern>> {
    let mut r = Rd { b: data, p: 0 };
    let sig = r.take(4)?;
    if sig != b"8BPT" {
        return Err(PsdError::InvalidSignature { expected: "8BPT", found: [sig[0], sig[1], sig[2], sig[3]] });
    }
    let _version = r.u16()?;
    let count = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..count.min(100_000) {
        out.push(read_pattern(&mut r)?);
    }
    Ok(out)
}

/// Writes a `.pat` pattern file.
pub fn write_pat_file(patterns: &[PsdPattern]) -> Result<Vec<u8>> {
    let mut out = b"8BPT".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(patterns.len() as u32).to_be_bytes());
    for p in patterns {
        write_pattern(p, &mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(depth: u16, alpha: bool) -> PsdPattern {
        let (w, h) = (5u32, 3u32);
        let bpp = usize::from(depth / 8);
        let plane = |s: u8| (0..w as usize * h as usize * bpp).map(|i| (i as u8).wrapping_mul(7).wrapping_add(s)).collect::<Vec<u8>>();
        PsdPattern {
            mode: 3,
            width: w,
            height: h,
            name: "Test ✓".into(),
            id: "0bd2d3ba-1234-11d4-8f8f-aabbccddeeff".into(),
            palette: None,
            depth,
            channels: vec![plane(1), plane(2), plane(3)],
            alpha: alpha.then(|| plane(9)),
        }
    }

    #[test]
    fn block_and_pat_file_round_trip_at_every_depth() {
        for depth in [8, 16, 32] {
            for alpha in [false, true] {
                let ps = vec![pat(depth, alpha), PsdPattern { name: "Second".into(), ..pat(depth, false) }];
                let blk = write_pattern_block(&ps).unwrap();
                assert_eq!(blk.len() % 4, 0);
                assert_eq!(parse_pattern_block(&blk).unwrap(), ps);
                let f = write_pat_file(&ps).unwrap();
                assert_eq!(parse_pat_file(&f).unwrap(), ps);
                // Re-serialising is byte-stable.
                assert_eq!(write_pattern_block(&parse_pattern_block(&blk).unwrap()).unwrap(), blk);
            }
        }
    }

    #[test]
    fn malformed_input_errors_without_panicking() {
        let blk = write_pattern_block(&[pat(8, true)]).unwrap();
        for cut in [3, 10, 40, blk.len() - 5] {
            let _ = parse_pattern_block(&blk[..cut]);
        }
        assert!(parse_pat_file(b"8BPS\0\x01").is_err());
        assert!(parse_pattern_block(&[]).unwrap().is_empty());
    }
}

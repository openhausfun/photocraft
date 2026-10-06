//! Smart-filter caches: the global `FEid` / `FXid` ("Filter Effects") block.
//!
//! Layout (Adobe PSD spec, "Filter Effects", checked against Photoshop-authored files):
//!
//! ```text
//! u32 version (1..=3), u64 length of what follows, then until that length is used up:
//!   Pascal identifier (the smart object's `placed` id in its `SoLd` descriptor, unpadded)
//!   u32 item version (1), u64 item length, then inside the item:
//!     rect (top, left, bottom, right), u32 depth, u32 max channels,
//!     max channels + 2 slots: u32 written flag; if set u64 length, u16 compression, data
//!   after the item: u8 flag; if set rect, u64 length, u16 compression, data
//! ```
//!
//! The slots hold the unfiltered placed pixels (colour channels from slot 0, transparency in the
//! last slot) and the trailing image is the filter mask, in document coordinates. RLE row counts
//! are four bytes wide (PSB style) in every file version.

use crate::compression::{Compression, PlaneLayout, decode_planes, encode_planes};
use crate::error::{PsdError, Result};
use crate::header::Version;
use crate::io::{Reader, WriteExt, read_pascal, write_pascal};
use crate::layer::Rect;

/// Most slots an item may declare (Photoshop writes 24 channels + 2).
const MAX_SLOTS: u32 = 64;

/// One compressed plane of a filter-effects item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectsPlane {
    /// Compression of `data`.
    pub compression: Compression,
    /// Encoded data (without the compression field).
    pub data: Vec<u8>,
}

impl EffectsPlane {
    /// Encodes one plane of `w × h` big-endian samples of `depth` bits (RLE uses four-byte counts).
    pub fn encode(compression: Compression, decoded: &[u8], w: usize, h: usize, depth: u16) -> Result<Self> {
        let layout = PlaneLayout { planes: 1, width: w, height: h, depth, version: Version::Psb };
        Ok(EffectsPlane { compression, data: encode_planes(compression, decoded, &layout)? })
    }

    /// Decodes into `w × h` big-endian samples of `depth` bits. A size the encoded data can't
    /// possibly hold (PackBits expands at most 64×, zlib about 1032×) is refused before any
    /// allocation, so a tiny corrupt block can't ask for gigabytes.
    pub fn decode(&self, w: usize, h: usize, depth: u16) -> Result<Vec<u8>> {
        let layout = PlaneLayout { planes: 1, width: w, height: h, depth, version: Version::Psb };
        let n = self.data.len() as u64;
        let most = match self.compression {
            Compression::Raw => n,
            Compression::Rle => n.saturating_mul(64),
            _ => n.saturating_mul(1032).saturating_add(64),
        };
        if layout.total_bytes()? > most {
            return Err(PsdError::invalid("filter effects: plane larger than its data can hold"));
        }
        decode_planes(self.compression, &self.data, &layout)
    }
}

/// The cache of one smart object's filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEffectsItem {
    /// The smart object's `placed` id.
    pub id: String,
    /// Item version (1).
    pub version: u32,
    /// Bounds of the cached planes.
    pub rect: Rect,
    /// Bits per sample.
    pub depth: u32,
    /// Declared channel count; there are `max_channels + 2` slots.
    pub max_channels: u32,
    /// The slots (`None` = not written).
    pub slots: Vec<Option<EffectsPlane>>,
    /// The filter mask: bounds and plane.
    pub mask: Option<(Rect, EffectsPlane)>,
}

impl FilterEffectsItem {
    /// The decoded filter mask (8/16/32-bit big-endian samples) with its bounds.
    pub fn decoded_mask(&self) -> Option<(Rect, Vec<u8>)> {
        let (r, p) = self.mask.as_ref()?;
        let (w, h) = r.size().ok()?;
        let depth = u16::try_from(self.depth).ok()?;
        Some((*r, p.decode(w, h, depth).ok()?))
    }
}

/// A parsed `FEid`/`FXid` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEffects {
    /// Block version (1..=3).
    pub version: u32,
    /// One item per smart object with filters.
    pub items: Vec<FilterEffectsItem>,
}

fn read_rect(r: &mut Reader<'_>) -> Result<Rect> {
    Ok(Rect { top: r.i32()?, left: r.i32()?, bottom: r.i32()?, right: r.i32()? })
}

fn write_rect(out: &mut Vec<u8>, r: &Rect) {
    out.put_i32(r.top);
    out.put_i32(r.left);
    out.put_i32(r.bottom);
    out.put_i32(r.right);
}

fn read_plane(r: &mut Reader<'_>) -> Result<EffectsPlane> {
    let len = r.u64()?;
    if len < 2 {
        return Err(PsdError::invalid("filter effects: plane shorter than its compression field"));
    }
    let mut sub = r.sub(len)?;
    let compression = Compression::from_u16(sub.u16()?);
    Ok(EffectsPlane { compression, data: sub.peek_rest().to_vec() })
}

fn write_plane(out: &mut Vec<u8>, p: &EffectsPlane) {
    out.put_u64(p.data.len() as u64 + 2);
    out.put_u16(p.compression.as_u16());
    out.put(&p.data);
}

impl FilterEffects {
    /// Parses block data. Trailing zero padding (up to 3 bytes) is allowed.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        if !(1..=3).contains(&version) {
            return Err(PsdError::invalid(format!("filter effects version {version}")));
        }
        let len = r.u64()?;
        let mut body = r.sub(len)?;
        let rest = r.peek_rest();
        if rest.len() > 3 || rest.iter().any(|&b| b != 0) {
            return Err(PsdError::invalid(format!("filter effects: {} unexpected bytes after the data", rest.len())));
        }
        let mut items = Vec::new();
        while !body.is_empty() {
            let id = String::from_utf8_lossy(&read_pascal(&mut body, 1)?).into_owned();
            let item_version = body.u32()?;
            let item_len = body.u64()?;
            let mut it = body.sub(item_len)?;
            let rect = read_rect(&mut it)?;
            let depth = it.u32()?;
            let max_channels = it.u32()?;
            if max_channels > MAX_SLOTS {
                return Err(PsdError::LimitExceeded("filter effects: too many channels"));
            }
            let mut slots = Vec::with_capacity(max_channels as usize + 2);
            for _ in 0..max_channels + 2 {
                slots.push(if it.u32()? != 0 { Some(read_plane(&mut it)?) } else { None });
            }
            if !it.is_empty() {
                return Err(PsdError::invalid(format!("filter effects item {id}: {} unexpected bytes", it.remaining())));
            }
            let mask = if body.is_empty() || body.u8()? == 0 { None } else { Some((read_rect(&mut body)?, read_plane(&mut body)?)) };
            items.push(FilterEffectsItem { id, version: item_version, rect, depth, max_channels, slots, mask });
        }
        Ok(FilterEffects { version, items })
    }

    /// Serializes block data (without padding).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for it in &self.items {
            write_pascal(&mut body, it.id.as_bytes(), 1);
            body.put_u32(it.version);
            let at = body.begin_len(true);
            write_rect(&mut body, &it.rect);
            body.put_u32(it.depth);
            body.put_u32(it.max_channels);
            for i in 0..it.max_channels as usize + 2 {
                match it.slots.get(i).and_then(Option::as_ref) {
                    Some(p) => {
                        body.put_u32(1);
                        write_plane(&mut body, p);
                    }
                    None => body.put_u32(0),
                }
            }
            // A 64-bit length never overflows.
            let _ = body.end_len(at, true);
            match &it.mask {
                Some((r, p)) => {
                    body.put_u8(1);
                    write_rect(&mut body, r);
                    write_plane(&mut body, p);
                }
                None => body.put_u8(0),
            }
        }
        let mut out = Vec::with_capacity(body.len() + 12);
        out.put_u32(self.version);
        out.put_u64(body.len() as u64);
        out.put(&body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, mask: bool) -> FilterEffectsItem {
        let rect = Rect { top: 1, left: 2, bottom: 5, right: 6 };
        let plane = |v: u8| EffectsPlane::encode(Compression::Rle, &[v; 16], 4, 4, 8).unwrap();
        let mut slots = vec![None; 26];
        slots[0] = Some(plane(10));
        slots[25] = Some(plane(255));
        FilterEffectsItem { id: id.into(), version: 1, rect, depth: 8, max_channels: 24, slots, mask: mask.then(|| (rect, plane(128))) }
    }

    #[test]
    fn round_trips() {
        let fx = FilterEffects { version: 3, items: vec![item("a-1", true), item("b-2", false)] };
        let bytes = fx.to_bytes();
        let back = FilterEffects::parse(&bytes).unwrap();
        assert_eq!(back, fx);
        assert_eq!(back.items[0].decoded_mask().unwrap().1, vec![128; 16]);
        assert_eq!(back.items[0].slots[0].as_ref().unwrap().decode(4, 4, 8).unwrap(), vec![10; 16]);
        // Padding is allowed, garbage is not.
        let mut padded = bytes.clone();
        padded.extend([0, 0]);
        assert!(FilterEffects::parse(&padded).is_ok());
        padded.push(7);
        assert!(FilterEffects::parse(&padded).is_err());
    }

    #[test]
    fn mutations_never_panic() {
        let bytes = FilterEffects { version: 3, items: vec![item("x", true), item("y", false)] }.to_bytes();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let mut b = bytes.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() as usize) % b.len();
                b[i] = next() as u8;
            }
            if let Ok(fx) = FilterEffects::parse(&b) {
                for it in &fx.items {
                    let _ = it.decoded_mask();
                    for p in it.slots.iter().flatten() {
                        let _ = p.decode(4, 4, 8);
                    }
                }
                let _ = fx.to_bytes();
            }
        }
    }

    #[test]
    fn malformed_data_is_an_error() {
        let bytes = FilterEffects { version: 3, items: vec![item("x", true)] }.to_bytes();
        for n in 0..bytes.len() {
            assert!(FilterEffects::parse(&bytes[..n]).is_err(), "truncated at {n}");
        }
        let mut bad = bytes.clone();
        bad[3] = 9; // version
        assert!(FilterEffects::parse(&bad).is_err());
        let mut huge = bytes;
        let at = 4 + 8 + 2 + 4 + 8 + 16 + 4; // max channels
        huge[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(FilterEffects::parse(&huge).is_err());
    }
}

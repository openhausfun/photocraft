//! Finding the camera-rendered JPEG preview embedded in a raw file.

use crate::tiff::{Tiff, tag};

/// An embedded baseline-JPEG preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preview<'a> {
    /// The complete JPEG stream.
    pub jpeg: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Dimensions of a JPEG whose frame is DCT-coded (baseline / extended /
/// progressive), or `None` (lossless JPEG raw data, or not a JPEG).
fn dct_jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(0..3) != Some(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    let mut pos = 2usize;
    for _ in 0..256 {
        if *b.get(pos)? != 0xFF {
            return None;
        }
        let m = *b.get(pos + 1)?;
        if m == 0xFF {
            pos += 1;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([*b.get(pos + 2)?, *b.get(pos + 3)?]));
        if matches!(m, 0xC0..=0xC2) {
            let h = u16::from_be_bytes([*b.get(pos + 5)?, *b.get(pos + 6)?]);
            let w = u16::from_be_bytes([*b.get(pos + 7)?, *b.get(pos + 8)?]);
            return (w > 0 && h > 0).then_some((u32::from(w), u32::from(h)));
        }
        if matches!(m, 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA | 0xD9) || len < 2 {
            return None;
        }
        pos = pos.checked_add(2 + len)?;
    }
    None
}

fn candidate(data: &[u8], offset: usize, len: usize) -> Option<Preview<'_>> {
    let jpeg = data.get(offset..offset.checked_add(len)?)?;
    let (width, height) = dct_jpeg_size(jpeg)?;
    Some(Preview { jpeg, width, height })
}

/// The largest embedded JPEG preview of a raw file (TIFF-based raws and RAF).
pub fn embedded_preview(bytes: &[u8]) -> Option<Preview<'_>> {
    let mut found: Vec<Preview> = Vec::new();
    if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        // RAF header: big-endian JPEG offset and length at bytes 84 and 88.
        let be = |o: usize| bytes.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize);
        if let (Some(off), Some(len)) = (be(84), be(88)) {
            found.extend(candidate(bytes, off, len));
        }
    } else if let Some(t) = Tiff::new(bytes) {
        // Olympus ORF: the preview is referenced from the maker note.
        if matches!(bytes.get(0..4), Some(b"IIRO" | b"IIRS" | b"MMOR"))
            && let Some((off, len)) = crate::orf::preview_range(&t)
        {
            found.extend(candidate(bytes, off, len));
        }
        for ifd in t.all_ifds() {
            if let (Some(off), Some(len)) = (t.tag_uint(&ifd, tag::JPEG_INTERCHANGE_FORMAT), t.tag_uint(&ifd, tag::JPEG_INTERCHANGE_FORMAT_LENGTH)) {
                found.extend(candidate(bytes, off as usize, len as usize));
            }
            if matches!(t.tag_uint(&ifd, tag::COMPRESSION), Some(6 | 7)) {
                let offs = t.tag_uints(&ifd, tag::STRIP_OFFSETS);
                let lens = t.tag_uints(&ifd, tag::STRIP_BYTE_COUNTS);
                if let ([off], [len]) = (offs.as_slice(), lens.as_slice()) {
                    found.extend(candidate(bytes, *off as usize, *len as usize));
                }
            }
            // Panasonic RW2 JpgFromRaw (0x002E) holds a complete JPEG.
            if let Some(e) = ifd.get(0x002E).filter(|e| e.typ == 7)
                && let Some(raw) = t.raw(e)
                && let Some((width, height)) = dct_jpeg_size(raw)
            {
                found.push(Preview { jpeg: raw, width, height });
            }
        }
    }
    found.into_iter().max_by_key(|p| u64::from(p.width) * u64::from(p.height))
}

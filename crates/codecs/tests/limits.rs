//! Decompression-bomb guards.

mod common;
use common::*;
use photocraft_codecs::*;

fn opts(l: Limits) -> DecodeOptions {
    DecodeOptions { limits: l, ..Default::default() }
}

fn is_limit(r: Result<Image, CodecError>) -> bool {
    matches!(r, Err(CodecError::LimitExceeded(_)))
}

fn encoded(f: Format) -> Vec<u8> {
    let img = synth(64, 48, ChannelLayout::Rgba, SampleType::U8, 1, 0.1);
    encode(&img, f, &EncodeOptions::default()).unwrap()
}

#[test]
fn max_pixels_enforced_for_every_format() {
    for f in rw_formats() {
        let b = encoded(f);
        let l = Limits { max_pixels: 1000, ..Limits::default() };
        assert!(is_limit(decode_with(&b, &opts(l))), "{f:?}");
        let l = Limits { max_pixels: 64 * 48, ..Limits::default() };
        assert!(decode_with(&b, &opts(l)).is_ok(), "{f:?} exact limit should pass");
    }
}

#[test]
fn max_width_enforced_for_every_format() {
    for f in rw_formats() {
        let l = Limits { max_width: 63, ..Limits::default() };
        assert!(is_limit(decode_with(&encoded(f), &opts(l))), "{f:?}");
    }
}

#[test]
fn max_height_enforced_for_every_format() {
    for f in rw_formats() {
        let l = Limits { max_height: 47, ..Limits::default() };
        assert!(is_limit(decode_with(&encoded(f), &opts(l))), "{f:?}");
    }
}

#[test]
fn max_alloc_enforced_for_every_format() {
    for f in rw_formats() {
        let l = Limits { max_alloc: 64 * 48, ..Limits::default() };
        assert!(decode_with(&encoded(f), &opts(l)).is_err(), "{f:?}");
    }
}

#[test]
fn limits_none_accepts() {
    for f in rw_formats() {
        assert!(decode_with(&encoded(f), &opts(Limits::none())).is_ok(), "{f:?}");
    }
}

fn png_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(data);
    let mut crc_in = ty.to_vec();
    crc_in.extend_from_slice(data);
    v.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    v
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[test]
fn png_bomb_header_rejected_before_alloc() {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&100_000u32.to_be_bytes());
    ihdr.extend_from_slice(&100_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[16, 6, 0, 0, 0]);
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend(png_chunk(b"IHDR", &ihdr));
    b.extend(png_chunk(b"IDAT", &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]));
    b.extend(png_chunk(b"IEND", &[]));
    assert!(is_limit(decode(&b)));
}

#[test]
fn pnm_bomb_header_rejected() {
    assert!(is_limit(decode(b"P6\n200000 200000\n255\n\0\0\0")));
    assert!(is_limit(decode(b"P7\nWIDTH 99999\nHEIGHT 99999\nDEPTH 4\nMAXVAL 65535\nTUPLTYPE RGB_ALPHA\nENDHDR\n")));
    assert!(is_limit(decode(b"PF\n100000 100000\n-1.0\n")));
}

#[test]
fn jpeg_bomb_header_rejected() {
    // SOI + SOF0 claiming 65535x65535x3 with no scan data.
    let b = [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 8, 0xFF, 0xFF, 0xFF, 0xFF, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xFF, 0xD9];
    let l = Limits { max_pixels: 1 << 24, ..Limits::default() };
    assert!(is_limit(decode_with(&b, &opts(l))));
}

#[test]
fn tiff_bomb_header_rejected() {
    // Minimal little-endian TIFF claiming 1_000_000 x 1_000_000 8-bit gray.
    let mut b = b"II*\0\x08\0\0\0".to_vec();
    let entries: [(u16, u16, u32, u32); 6] = [(256, 4, 1, 1_000_000), (257, 4, 1, 1_000_000), (258, 3, 1, 8), (262, 3, 1, 1), (273, 4, 1, 0), (279, 4, 1, 0)];
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, ty, count, val) in entries {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&ty.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&val.to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    assert!(is_limit(decode(&b)));
}

#[test]
fn limits_check_api() {
    let l = Limits::default();
    assert!(l.check(100, 100, ChannelLayout::Rgba, SampleType::F32).is_ok());
    assert!(matches!(l.check(0, 1, ChannelLayout::Gray, SampleType::U8), Err(CodecError::InvalidImage(_))));
    assert!(matches!(l.check(u32::MAX, 1, ChannelLayout::Gray, SampleType::U8), Err(CodecError::LimitExceeded(_))));
    let l = Limits { max_alloc: 399, ..Limits::default() };
    assert!(l.check(10, 10, ChannelLayout::Rgba, SampleType::U8).is_err());
    assert!(l.check(10, 10, ChannelLayout::Rgb, SampleType::U8).is_ok());
}

//! Damaged or hostile bundles must fail cleanly, never panic.

mod common;
use common::*;
use photocraft_color::{ColorMode, SampleType};
use photocraft_format::*;
use proptest::prelude::*;

fn sample() -> Vec<u8> {
    save_to_bytes(&rich_doc(ColorMode::Rgb, SampleType::U8), &SaveOptions::default()).unwrap()
}

fn rebuild(bytes: &[u8], f: impl Fn(&str, Vec<u8>) -> Option<Vec<u8>>) -> Vec<u8> {
    let r = zip::ZipReader::new(bytes).unwrap();
    let mut z = zip::ZipWriter::new();
    for e in &r.entries {
        if let Some(d) = f(&e.name, r.read(e, usize::MAX).unwrap()) {
            z.add(&e.name, &d).unwrap();
        }
    }
    z.finish().unwrap()
}

#[test]
fn truncation_never_panics_and_fails() {
    let b = sample();
    let step = (b.len() / 200).max(1);
    for cut in (0..b.len()).step_by(step) {
        assert!(load_from_bytes(&b[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn byte_flips_detected() {
    let b = sample();
    let mut rng = common::Rng::new(9);
    let mut failures = 0;
    for _ in 0..300 {
        let mut c = b.clone();
        let i = (rng.next() as usize) % c.len();
        c[i] ^= 0x20;
        if load_from_bytes(&c).is_err() {
            failures += 1;
        }
    }
    // Some flips hit unused header fields; most must be caught (CRC + hashes).
    assert!(failures > 200, "{failures}");
}

#[test]
fn missing_tile_is_error() {
    let b = rebuild(&sample(), |n, d| (!n.starts_with("tiles/")).then_some(d));
    assert!(matches!(load_from_bytes(&b), Err(FormatError::Corrupt(_))));
}

#[test]
fn missing_manifest_is_error() {
    let b = rebuild(&sample(), |n, d| (n != "manifest.json").then_some(d));
    assert!(load_from_bytes(&b).is_err());
}

#[test]
fn swapped_tile_content_fails_hash_check() {
    let b = sample();
    let r = zip::ZipReader::new(&b).unwrap();
    let tiles: Vec<_> = r.entries.iter().filter(|e| e.name.starts_with("tiles/")).map(|e| e.name.clone()).collect();
    let other = r.read_by_name(&tiles[1], usize::MAX).unwrap();
    let first = tiles[0].clone();
    let b2 = rebuild(&b, |n, d| Some(if n == first { other.clone() } else { d }));
    let e = load_from_bytes(&b2).unwrap_err();
    assert!(e.to_string().contains("hash"), "{e}");
}

#[test]
fn bad_manifest_json() {
    let b = rebuild(&sample(), |n, d| Some(if n == "manifest.json" { b"{not json".to_vec() } else { d }));
    assert!(matches!(load_from_bytes(&b), Err(FormatError::Json(_))));
}

#[test]
fn too_new_version_rejected() {
    let b = rebuild(&sample(), |n, d| {
        Some(if n == "manifest.json" {
            let mut v: serde_json::Value = serde_json::from_slice(&d).unwrap();
            v["format_version"] = 999.into();
            serde_json::to_vec(&v).unwrap()
        } else {
            d
        })
    });
    assert!(matches!(load_from_bytes(&b), Err(FormatError::TooNew { found: 999, .. })));
}

#[test]
fn invalid_hash_strings_rejected() {
    let b = rebuild(&sample(), |n, d| {
        Some(if n == "manifest.json" {
            let s = String::from_utf8(d).unwrap();
            let i = s.find("\"hash\": \"").unwrap() + 9;
            let mut s = s.into_bytes();
            s[i] = b'/';
            s
        } else {
            d
        })
    });
    assert!(load_from_bytes(&b).is_err());
}

#[test]
fn manifest_size_limit() {
    let b = sample();
    let opts = LoadOptions { max_manifest_bytes: 100, ..Default::default() };
    assert!(matches!(load_from_bytes_with(&b, &opts), Err(FormatError::LimitExceeded(_))));
}

#[test]
fn total_size_limit() {
    let b = sample();
    let opts = LoadOptions { max_total_bytes: 1000, ..Default::default() };
    assert!(matches!(load_from_bytes_with(&b, &opts), Err(FormatError::LimitExceeded(_))));
}

#[test]
fn directory_bundle_tampered_tile() {
    let doc = rich_doc(ColorMode::Rgb, SampleType::U8);
    let dir = temp_dir("tamper");
    PcraftWriter::new().save_dir(&doc, &dir, &SaveOptions::default()).unwrap();
    let tile = std::fs::read_dir(dir.join("tiles")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(&tile, b"garbage").unwrap();
    assert!(load_path(&dir).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deflated_zip_entries_are_readable() {
    // Re-zip with DEFLATE (as a user's zip tool might) — must still load.
    use std::io::Write;
    let b = sample();
    let r = zip::ZipReader::new(&b).unwrap();
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in &r.entries {
        let data = r.read(e, usize::MAX).unwrap();
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).unwrap();
        let comp = enc.finish().unwrap();
        let crc = zip::crc32(&data);
        let off = out.len() as u32;
        let name = e.name.as_bytes();
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 8, 0, 0, 0, 0x21, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name);
        out.extend_from_slice(&comp);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0x21, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_off = out.len() as u32;
    let n = r.entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    assert_eq!(load_from_bytes(&out).unwrap(), rich_doc_ids_from(&b));
}

fn rich_doc_ids_from(b: &[u8]) -> photocraft_doc::Document {
    load_from_bytes(b).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, .. ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = load_from_bytes(&data);
        let _ = read_manifest(&data);
    }

    #[test]
    fn random_manifest_never_panics(json in "\\PC{0,300}") {
        let mut z = zip::ZipWriter::new();
        z.add("manifest.json", json.as_bytes()).unwrap();
        let _ = load_from_bytes(&z.finish().unwrap());
    }

    #[test]
    fn random_tile_payload_never_panics(data in proptest::collection::vec(any::<u8>(), 0..512), which in 0usize..8) {
        let b = sample_cached();
        let r = zip::ZipReader::new(b).unwrap();
        let tiles: Vec<String> = r.entries.iter().filter(|e| e.name.starts_with("tiles/")).map(|e| e.name.clone()).collect();
        let target = tiles[which % tiles.len()].clone();
        let b2 = rebuild(b, |n, d| Some(if n == target { data.clone() } else { d }));
        prop_assert!(load_from_bytes(&b2).is_err());
    }
}

fn sample_cached() -> &'static [u8] {
    static S: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    S.get_or_init(sample)
}

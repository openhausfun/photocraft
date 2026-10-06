//! Property-based round-trip tests.

use photocraft_psd::compression::{PlaneLayout, decode_planes, encode_planes, packbits};
use photocraft_psd::testgen;
use photocraft_psd::*;
use proptest::prelude::*;

fn version() -> impl Strategy<Value = Version> {
    prop_oneof![Just(Version::Psd), Just(Version::Psb)]
}
fn compression() -> impl Strategy<Value = Compression> {
    prop_oneof![Just(Compression::Raw), Just(Compression::Rle), Just(Compression::Zip), Just(Compression::ZipPrediction)]
}
fn depth() -> impl Strategy<Value = u16> {
    prop_oneof![Just(1u16), Just(8), Just(16), Just(32)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn packbits_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..600)) {
        let enc = packbits::encode_vec(&data);
        prop_assert_eq!(packbits::decode(&enc, data.len()).unwrap(), data.clone());
        // Worst case expansion is 1 header byte per 128 literal bytes.
        prop_assert!(enc.len() <= data.len() + data.len().div_ceil(128));
    }

    #[test]
    fn packbits_runs_roundtrip(runs in proptest::collection::vec((any::<u8>(), 1usize..300), 0..20)) {
        let data: Vec<u8> = runs.iter().flat_map(|&(b, n)| std::iter::repeat_n(b, n)).collect();
        let enc = packbits::encode_vec(&data);
        prop_assert_eq!(packbits::decode(&enc, data.len()).unwrap(), data);
    }

    #[test]
    fn packbits_decode_garbage_never_panics(data in proptest::collection::vec(any::<u8>(), 0..64), n in 0usize..512) {
        let _ = packbits::decode(&data, n);
    }

    #[test]
    fn channel_codecs_roundtrip(
        w in 0usize..40, h in 0usize..12, planes in 1usize..4,
        d in depth(), v in version(), c in compression(), seed in any::<u64>(),
    ) {
        let l = PlaneLayout { planes, width: w, height: h, depth: d, version: v };
        let n = l.decoded_len().unwrap();
        let mut x = seed;
        let data: Vec<u8> = (0..n).map(|_| { x = x.wrapping_mul(6364136223846793005).wrapping_add(1); if x >> 62 == 0 { 0 } else { (x >> 33) as u8 } }).collect();
        let enc = encode_planes(c, &data, &l).unwrap();
        prop_assert_eq!(decode_planes(c, &enc, &l).unwrap(), data);
    }

    #[test]
    fn channel_decode_garbage_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..128),
        w in 0usize..20, h in 0usize..8, d in depth(), v in version(), c in 0u16..5,
    ) {
        let l = PlaneLayout { planes: 1, width: w, height: h, depth: d, version: v };
        let _ = decode_planes(Compression::from_u16(c), &data, &l);
    }

    #[test]
    fn generated_small_files_roundtrip(v in version(), c in compression()) {
        let f = testgen::small(v, c);
        let b = f.to_bytes().unwrap();
        let p = PsdFile::from_bytes(&b).unwrap();
        prop_assert_eq!(&p, &f);
        prop_assert_eq!(p.to_bytes().unwrap(), b);
    }

    #[test]
    fn merged_only_random_sizes_roundtrip(
        w in 0u32..30, h in 0u32..10, v in version(), c in compression(), mode_ix in 0usize..8, depth_ix in 0usize..3,
    ) {
        let mode = testgen::MODES[mode_ix];
        let depths = testgen::mode_depths(mode);
        let d = depths[depth_ix % depths.len()];
        let f = testgen::merged_only(v, mode, d, c, w, h);
        let b = f.to_bytes().unwrap();
        let p = PsdFile::from_bytes(&b).unwrap();
        prop_assert_eq!(&p, &f);
        prop_assert_eq!(p.to_bytes().unwrap(), b);
    }

    #[test]
    fn builder_random_layers_roundtrip(
        layers in proptest::collection::vec(
            (-20i32..20, -20i32..20, 0u32..9, 0u32..9, any::<u8>(), 0usize..28, any::<bool>(), "[a-zA-Z0-9 äöü😀]{0,12}"),
            0..6,
        ),
        v in version(), c in compression(), sixteen in any::<bool>(),
    ) {
        let depth = if sixteen { 16 } else { 8 };
        let mut b = PsdBuilder::new(16, 16).version(v).compression(c).depth(depth);
        let mut expected = Vec::new();
        for (i, (x, y, w, h, op, bm, vis, name)) in layers.iter().enumerate() {
            let n = (*w * *h) as usize;
            let px8: Vec<u8> = (0..n * 4).map(|k| (k * 31 + i * 7) as u8).collect();
            let pixels = if sixteen {
                PixelData::Rgba16(px8.iter().map(|&p| u16::from(p) * 257).collect())
            } else {
                PixelData::Rgba8(px8.clone())
            };
            let mut s = LayerSpec::new(name.clone(), *x, *y, *w, *h, pixels);
            s.opacity = *op;
            s.blend_mode = BlendMode::ALL[*bm];
            s.visible = *vis;
            b.push_layer(s);
            expected.push((name.clone(), px8, *op, BlendMode::ALL[*bm], *vis));
        }
        let bytes = b.to_bytes().unwrap();
        let f = PsdFile::from_bytes(&bytes).unwrap();
        prop_assert_eq!(f.to_bytes().unwrap(), bytes);
        prop_assert_eq!(f.layers().len(), expected.len());
        for (l, (name, px, op, bm, vis)) in f.iter_layers().zip(expected) {
            prop_assert_eq!(l.name(), name);
            prop_assert_eq!(l.rgba8().unwrap().data, px);
            prop_assert_eq!(l.record.opacity, op);
            prop_assert_eq!(l.record.blend_mode, bm);
            prop_assert_eq!(l.record.is_visible(), vis);
        }
    }

    #[test]
    fn tagged_blocks_roundtrip(
        blocks in proptest::collection::vec(
            (any::<[u8; 4]>(), proptest::collection::vec(any::<u8>(), 0..40), any::<bool>(), 0usize..4),
            0..8,
        ),
        v in version(),
    ) {
        let mut f = testgen::small(v, Compression::Raw);
        for (key, data, b64, pad) in blocks {
            let mut tb = TaggedBlock::new(key, data);
            if b64 { tb.signature = *b"8B64"; }
            // Only padding patterns the detector can recover are modeled:
            // zeros up to 3 bytes.
            tb.padding = match pad { 0 => None, n => Some(vec![0; n - 1]) };
            if tb.padding.as_ref().is_some_and(|p| p.len() == tb.data.len() % 2) {
                tb.padding = None;
            }
            f.global_blocks.push(tb);
        }
        let b = f.to_bytes().unwrap();
        let p = PsdFile::from_bytes(&b).unwrap();
        prop_assert_eq!(&p, &f);
        prop_assert_eq!(p.to_bytes().unwrap(), b);
    }
}

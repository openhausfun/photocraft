# photocraft-raw

A clean-room, pure-Rust camera raw decoder and developer. The crate is standalone (no workspace
dependencies), has no `unsafe`, does no I/O (`&[u8]` in), builds for `wasm32-unknown-unknown`
(sequential there, rayon-parallel on native) and never panics on hostile input: every offset is
bounds-checked and sizes are checked against `Limits` before allocating.

```rust
use photocraft_raw::{develop, DevelopOptions, Demosaic};

let dev = develop(&bytes, &DevelopOptions { demosaic: Demosaic::Ahd, ..Default::default() })?;
// dev.rgb: interleaved 16-bit RGB in ProPhoto RGB (ROMM primaries, D50, gamma 1.8)
// dev.warnings: anything approximated or not applied
```

`photocraft-io` uses it so opening a raw file yields a normal 16-bit RGB document tagged with the
built-in ProPhoto-compatible profile.

## Sources (clean-room)

Implemented only from public specifications, papers and observation of files:

* TIFF 6.0, TIFF/EP (ISO 12234-2) and the Adobe DNG Specification 1.7.
* ITU-T T.81 (ISO 10918-1) Annex H: lossless JPEG, process 14 ("LJ92").
* The published description of Canon's CR2 container (header, raw IFD, slice tag 0xC640).
* Publicly documented maker-note / private tags (ExifTool's tag tables): Canon ModelID (0x0010),
  SensorInfo (0x00E0) and ColorData (0x4001), Nikon WB_RBLevels (0x000C) and BlackLevel (0x003D), Sony
  BlackLevel (0x7310), WB_RGGBLevels (0x7313), SonyRawFileType (0x7000) and SonyToneCurve
  (0x7010), the PanasonicRaw IFD0 tags, Olympus ImageProcessing (0x2040) and CameraSettings
  (0x2020) preview tags.
* Sony cRAW (ARW 2): H. Dietz, "Sony ARW2 Compression: Artifacts And Credible Repair"
  (Electronic Imaging 2016) and the RawDigger / diglloyd write-ups of the 11 + 7-bit scheme;
  the exact bit layout and tone-curve scale were established by observation of sample files.
* Panasonic RW2 RawFormat 5 and uncompressed Olympus ORF: established by observation of sample
  files (bit packing, page layout, sample justification).
* Demosaicing: Malvar, He & Cutler (ICASSP 2004); Hirakawa & Parks, "Adaptive
  homogeneity-directed demosaicing" (IEEE TIP 2005).
* McCamy's CCT approximation (1992); the Bradford chromatic adaptation transform.

No code from dcraw, LibRaw, rawspeed, rawler, rawloader or darktable was read or used, and no
camera colour tables were copied.

## Support matrix

| Format | Status |
|---|---|
| DNG | Uncompressed (8–16 bit, packed or not) and lossless JPEG; strips and tiles; CFA (Bayer) and LinearRaw; LinearizationTable, BlackLevel (+ repeat, DeltaH/V), WhiteLevel, ActiveArea, DefaultCrop, ColorMatrix1/2, CameraCalibration, ForwardMatrix, AnalogBalance, AsShotNeutral / AsShotWhiteXY, BaselineExposure, Orientation, OpcodeList2 GainMap (lens shading) |
| DNG (lossy JPEG, JPEG XL, floating point; opcodes other than GainMap) | Unsupported / not applied (reported) |
| CR2 | Lossless JPEG with slices, borders and as-shot white balance from the maker note, black measured on the masked border. CR2 has no CFA tag and the row phase varies by model, so it is measured from the data (the green diagonal), with a Canon model-ID table as the fallback (see `src/cr2.rs`) |
| CR2 sRAW / mRAW | Unsupported |
| NEF / NRW, ARW, PEF and other TIFF/EP raws | Uncompressed and lossless-JPEG (incl. Sony lossless ARW) CFA data |
| Sony compressed ARW ("cRAW", SonyRawFileType 2) | Decoded: 11-bit min/max + 7-bit delta blocks, SonyToneCurve to 14 bits |
| Panasonic / Leica RW2, RawFormat 5 (12- and 14-bit packed) | Decoded, with PanasonicRaw black / white / WB / sensor borders |
| Olympus ORF, uncompressed 16-bit (E-1, E-400…) | Decoded, with ImageProcessing black / WB / ValidBits / crop |
| Nikon compressed NEF (lossless and lossy), Sony "Compressed RAW 2", Pentax compressed PEF, RW2 RawFormat 4 and older, Olympus compressed ORF | Unsupported: no public description of these codes was found apart from GPL decoder source, which this crate may not use (clean-room). `photocraft-io` opens the embedded JPEG preview instead |
| CR3, RAF | Recognised, unsupported (preview fallback where a preview is found) |
| X-Trans and other non-Bayer CFAs | Unsupported |

## Development pipeline

1. Linearization table, per-position black level, scale to the white level.
2. White balance (as shot; or grey-world when the file has none; or explicit multipliers),
   normalized so the smallest multiplier is 1, then clip to 1 so blown highlights stay white.
3. Demosaic: `Bilinear`, `Mhc` (Malvar–He–Cutler) or `Ahd` (default).
4. Camera → XYZ (D50) per the DNG specification (ColorMatrix interpolated by the white's
   correlated colour temperature, or ForwardMatrix), → linear ProPhoto; exposure
   (BaselineExposure + user EV); gamma 1.8; 16 bits.
5. Orientation.

Files without colour calibration (CR2, NEF, ARW, RW2, ORF…) use a documented neutral fallback: the
white-balanced camera channels are treated as linear sRGB primaries (colours are plausible but
less saturated than a calibrated profile; converting to DNG gives calibrated colour). No tone
curve is applied: the result is a scene-referred rendering, flatter than a camera JPEG.

## Tools

`cargo run --release -p photocraft-raw --example rawinfo -- [--dump] [--demosaic ahd] [--png DIR] FILE...`
prints what was decoded, times decode and develop, and can write sRGB PNG previews and the
embedded JPEG previews.

# Raster formats

`photocraft-codecs` provides format detection, capability declarations, decode limits, metadata handling, and encoding for flat images. The default build declares read/write support for PNG, JPEG, TIFF, WebP, GIF, BMP, TGA, ICO, Netpbm/PFM, QOI, OpenEXR, and Radiance HDR. AVIF encoding is optional; AVIF decoding is not implemented in the current crate.

Capability details such as sample depth, channel layout, alpha, ICC, EXIF, XMP, DPI, and animation behavior are defined by `crates/codecs/src/format.rs`. Do not infer fidelity from the filename extension alone.

## Implemented decode limits

`DecodeOptions::default()` applies `Limits` before decoded pixel allocation:

| Limit | Default |
|---|---:|
| Maximum width | 262,144 pixels |
| Maximum height | 262,144 pixels |
| Maximum pixel count | 268,435,456 pixels |
| Maximum decoded allocation | 2 GiB |

Format adapters pass compatible allocation limits into PNG, TIFF, WebP, and `image`-based decoders, while other implementations perform explicit checked dimension/buffer validation. Tests cover bomb-like headers, truncation, bit flips, header overwrites, and random data.

`Limits::none()` exists for callers with a deliberate reason to relax policy. It remains bounded by representable memory sizes, but it removes the default resource policy and should not be used for untrusted input.

Animated containers are represented as a single decoded frame by the current codec API. Refer to [`crates/codecs/README.md`](https://github.com/storytold/photocraft/blob/main/crates/codecs/README.md) and source capability declarations for current behavior.

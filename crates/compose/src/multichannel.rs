//! Display of Multichannel documents: every spot (ink) channel printed over white paper.
//!
//! Ink density `v` (the channel value, 1 = solid) of an ink with display colour `c` and solidity
//! `s` turns the colour underneath `p` into `(1 − s·v)·p·(1 − v + v·c) + s·v·c`: transparent inks
//! (solidity 0) multiply like overprinted process inks, opaque ones cover what is below, which is
//! how Photoshop previews spot channels.

use photocraft_doc::Document;
use photocraft_geom::Rect;

use crate::Buffer;

/// The printed inks of `doc` over `rect` (opaque).
pub fn inks(doc: &Document, rect: Rect) -> Buffer {
    let mut buf = Buffer::filled(rect, [1.0, 1.0, 1.0, 1.0]);
    let mut vals = Vec::new();
    for ch in &doc.channels {
        let Some((ink, solidity)) = ch.spot else { continue };
        if ch.surface.content_bounds().intersect(&rect).is_empty() {
            continue;
        }
        let c = ink.to_rgb();
        let s = solidity.clamp(0.0, 1.0);
        let n = ch.surface.channels();
        ch.surface.read_region_into(rect, &mut vals);
        for (p, v) in buf.px.iter_mut().zip(vals.chunks_exact(n)) {
            let v = v[0].clamp(0.0, 1.0);
            if v <= 0.0 {
                continue;
            }
            let keep = 1.0 - s * v;
            for j in 0..3 {
                p[j] = keep * p[j] * (1.0 - v + v * c[j]) + s * v * c[j];
            }
        }
    }
    buf
}

/// The starting backdrop of a document render: the inks for Multichannel, else transparent.
pub(crate) fn backdrop(doc: &Document, rect: Rect) -> Buffer {
    if doc.mode == photocraft_color::ColorMode::Multichannel { inks(doc, rect) } else { Buffer::transparent(rect) }
}

#[cfg(test)]
mod tests {
    use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
    use photocraft_doc::{AlphaChannel, Document, Size};
    use photocraft_raster::Surface;

    use super::*;

    #[test]
    fn inks_multiply_over_paper() {
        let mut doc = Document::new("m", Size::new(4, 1), ColorMode::Multichannel, SampleType::U8);
        let fmt = PixelFormat::new(ColorMode::Grayscale, SampleType::U8, false);
        let mut a = Surface::new(fmt);
        a.write_region(Rect::new(0, 0, 4, 1), &[0.0, 1.0, 1.0, 0.0]);
        let mut b = Surface::new(fmt);
        b.write_region(Rect::new(0, 0, 4, 1), &[0.0, 0.0, 1.0, 1.0]);
        doc.channels.push(AlphaChannel { spot: Some((Color::rgb(0.0, 1.0, 1.0), 0.0)), ..AlphaChannel::new("Cyan", a) });
        doc.channels.push(AlphaChannel { spot: Some((Color::rgb(1.0, 0.0, 1.0), 0.0)), ..AlphaChannel::new("Magenta", b) });
        let r = crate::flatten(&doc);
        assert_eq!(r.px[0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(r.px[1], [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(r.px[2], [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(r.px[3], [1.0, 0.0, 1.0, 1.0]);
        // Alpha channels and other modes are not inks.
        doc.mode = ColorMode::Rgb;
        assert_eq!(crate::flatten(&doc).px[1][3], 0.0);
    }
}

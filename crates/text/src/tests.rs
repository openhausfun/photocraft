use photocraft_color::{Color, PixelFormat, SampleType};
use photocraft_doc::TextLayer;
use photocraft_doc::text::{CharStyle, FontFeature, ParagraphRun, ParagraphStyle, TextAlign, TextDirection, TextRun, TextShape};
use photocraft_geom::Affine;

use crate::{TextEngine, fonts};

fn point(text: &str, size_pt: f32) -> TextLayer {
    TextLayer { text: text.into(), font_family: "Inter".into(), size_pt, ..Default::default() }
}

fn styled(text: &str, style: CharStyle) -> TextLayer {
    TextLayer { text: text.into(), runs: vec![TextRun { len: text.len(), style }], ..Default::default() }
}

fn with_para(mut t: TextLayer, p: ParagraphStyle) -> TextLayer {
    t.paragraphs = vec![ParagraphRun { len: t.text.len(), style: p }];
    t
}

fn width(l: &crate::TextLayout) -> f32 {
    l.lines.iter().map(|l| l.x1 - l.x0).fold(0.0, f32::max)
}

fn alpha_sum(s: &photocraft_raster::Surface, r: photocraft_geom::Rect) -> f64 {
    let n = s.channels();
    s.read_region(r).chunks_exact(n).map(|p| f64::from(p[n - 1])).sum()
}

#[test]
fn bundled_fonts_cover_latin() {
    let mut e = TextEngine::new();
    let fams = e.fonts.families();
    assert!(fams.iter().any(|f| f == "Inter"), "{fams:?}");
    assert!(fams.iter().any(|f| f == "JetBrains Mono"), "{fams:?}");
    let l = e.layout(&point("Hello, Wörld! 0123", 12.0), 72.0);
    assert_eq!(l.glyphs.len(), "Hello, Wörld! 0123".chars().count());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef");
    assert_eq!(e.fonts.faces("Inter").len(), 3);
}

#[test]
fn metrics_are_stable_and_scale_with_dpi() {
    let mut e = TextEngine::new();
    let a = e.layout(&point("Hamburgefonstiv", 12.0), 72.0);
    let w = width(&a);
    // Inter 12 px: stable to a tenth of a pixel across runs.
    let again = width(&e.layout(&point("Hamburgefonstiv", 12.0), 72.0));
    assert_eq!(w, again);
    assert!((80.0..110.0).contains(&w), "{w}");
    let b = e.layout(&point("Hamburgefonstiv", 12.0), 144.0);
    assert!((width(&b) - 2.0 * w).abs() < 0.05, "{} vs {}", width(&b), 2.0 * w);
    // Point text: first baseline at the anchor.
    assert_eq!(a.lines[0].baseline, 0.0);
    assert!(a.lines[0].ascent > 8.0 && a.lines[0].ascent < 13.0);
}

#[test]
fn auto_and_explicit_leading() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("one\ntwo\rthree", 10.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[1].baseline - 12.0).abs() < 1e-3, "auto leading = 1.2 × size");
    assert!((l.lines[2].baseline - 24.0).abs() < 1e-3);
    assert_eq!(&"one\ntwo\rthree"[l.lines[2].range.clone()], "three");
    let t = styled("a\nb", CharStyle { size_pt: 10.0, leading_pt: Some(30.0), ..Default::default() });
    let l = e.layout(&t, 144.0);
    assert!((l.lines[1].baseline - 60.0).abs() < 1e-3, "{}", l.lines[1].baseline);
    // Space before/after in points.
    let t = with_para(point("a\nb", 10.0), ParagraphStyle { space_after_pt: 5.0, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!((l.lines[1].baseline - 17.0).abs() < 1e-3, "{}", l.lines[1].baseline);
}

#[test]
fn box_text_wraps_inside_width() {
    let mut e = TextEngine::new();
    let mut t = point("The quick brown fox jumps over the lazy dog again and again", 12.0);
    t.shape = TextShape::Box { x: 10.0, y: 20.0, width: 100.0, height: 1000.0 };
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() >= 3, "{}", l.lines.len());
    for line in &l.lines {
        assert!(line.x0 >= 10.0 - 1e-3 && line.x1 <= 110.0 + 1e-3, "{line:?}");
    }
    // First baseline = top + ascender height ('d'), a bit less than the hhea ascent.
    let drop = l.lines[0].baseline - 20.0;
    assert!(drop > 0.65 * 12.0 && drop < l.lines[0].ascent, "{drop}");
    // Lines cover the text in order.
    assert_eq!(l.lines[0].range.start, 0);
    for w in l.lines.windows(2) {
        assert!(w[1].range.start >= w[0].range.end);
    }
    // Overflowing lines are hidden, like Photoshop.
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 100.0, height: 30.0 };
    let l2 = e.layout(&t, 72.0);
    assert_eq!(l2.lines.len(), 2);
}

#[test]
fn alignment_point_and_box() {
    let mut e = TextEngine::new();
    let c = e.layout(&with_para(point("Centered", 20.0), ParagraphStyle { align: TextAlign::Center, ..Default::default() }), 72.0);
    let ln = &c.lines[0];
    assert!((ln.x0 + ln.x1).abs() < 0.01, "centered on anchor: {ln:?}");
    let r = e.layout(&with_para(point("Right", 20.0), ParagraphStyle { align: TextAlign::Right, ..Default::default() }), 72.0);
    assert!(r.lines[0].x1.abs() < 0.01 && r.lines[0].x0 < -10.0);

    let mut t = point("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk", 12.0);
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 120.0, height: 500.0 };
    let t = with_para(t, ParagraphStyle { align: TextAlign::JustifyLeft, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() > 1);
    let first = &l.lines[0];
    // Justified line: last glyph ends at the right edge (within a pixel).
    let last_glyph_x = l.glyphs.iter().filter(|g| (g.y - first.baseline).abs() < 1e-3).map(|g| g.x).fold(f32::MIN, f32::max);
    assert!(last_glyph_x > 110.0, "{last_glyph_x}");
    let right = with_para(
        TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: 200.0, height: 100.0 }, ..point("end", 12.0) },
        ParagraphStyle { align: TextAlign::Right, ..Default::default() },
    );
    let l = e.layout(&right, 72.0);
    assert!((l.lines[0].x1 - 200.0).abs() < 0.5, "{:?}", l.lines[0]);
}

#[test]
fn rtl_text_is_reordered() {
    let mut e = TextEngine::new();
    // Hebrew isn't in the bundled fonts, but bidi still resolves (glyphs may be .notdef).
    let text = "abc שלום def";
    let l = e.layout(&point(text, 12.0), 72.0);
    let heb: Vec<_> = l.clusters.iter().filter(|c| text[c.range.clone()].chars().all(|ch| ('\u{0590}'..='\u{05FF}').contains(&ch))).collect();
    assert_eq!(heb.len(), 4);
    assert!(heb.iter().all(|c| c.rtl));
    // Visual order: the first logical Hebrew letter is rightmost among them.
    let first = heb.iter().min_by_key(|c| c.range.start).unwrap();
    assert!(heb.iter().all(|c| c.x <= first.x + 1e-3));
    // Forced RTL paragraph: the Latin run ends up to the right of the Hebrew.
    let t = with_para(point("שלום abc", 12.0), ParagraphStyle { direction: TextDirection::Rtl, ..Default::default() });
    let l = e.layout(&t, 72.0);
    let a = l.clusters.iter().find(|c| &"שלום abc"[c.range.clone()] == "a").unwrap();
    let shin = l.clusters.iter().find(|c| c.range.start == 0).unwrap();
    assert!(a.x < shin.x, "abc left of the Hebrew in an RTL paragraph");
    assert!(l.clusters.iter().all(|c| c.range.end <= "שלום abc".len()));
}

/// Builds a TrueType collection from standalone fonts (table offsets rebased).
fn make_ttc(fonts: &[&[u8]]) -> Vec<u8> {
    let header_len = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
    let mut offsets = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    for f in fonts {
        while !(header_len + body.len()).is_multiple_of(4) {
            body.push(0);
        }
        let base = (header_len + body.len()) as u32;
        offsets.push(base);
        let mut copy = f.to_vec();
        let n = u16::from_be_bytes([copy[4], copy[5]]) as usize;
        for i in 0..n {
            let at = 12 + i * 16 + 8;
            let off = u32::from_be_bytes(copy[at..at + 4].try_into().unwrap());
            copy[at..at + 4].copy_from_slice(&(off + base).to_be_bytes());
        }
        body.extend(copy);
    }
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out.extend(body);
    out
}

#[test]
fn truetype_collections_load() {
    let ttc = make_ttc(&[fonts::BUNDLED[0].1, fonts::BUNDLED[3].1]);
    assert_eq!(fonts::face_count(&ttc), 2);
    let mut db = fonts::FontDb::new();
    // A fresh DB without the bundled mono font would miss it; register the collection anyway
    // and check both faces are indexed (families already present gain faces).
    let before = db.faces("JetBrains Mono").len();
    let names = db.register_font_data(ttc);
    assert!(names.iter().any(|n| n == "Inter") && names.iter().any(|n| n == "JetBrains Mono"), "{names:?}");
    assert_eq!(db.faces("JetBrains Mono").len(), before + 1);
    // Layout with the TTC-backed face works.
    let mut e = TextEngine { fonts: db, layouter: crate::layout::Layouter::new() };
    let t = styled("mono", CharStyle { font_family: "JetBrains Mono".into(), ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
}

#[test]
fn opentype_features_and_tracking() {
    let mut e = TextEngine::new();
    let ids = |e: &mut TextEngine, feats: Vec<FontFeature>, lig: bool| {
        let t = styled("a0", CharStyle { features: feats, ligatures: lig, ..Default::default() });
        e.layout(&t, 72.0).glyphs.iter().map(|g| g.id).collect::<Vec<_>>()
    };
    let plain = ids(&mut e, vec![], true);
    let alt = ids(&mut e, vec![FontFeature { tag: "zero".into(), value: 1 }], true);
    assert_ne!(plain, alt, "Inter's `zero` feature selects the slashed zero");
    let a = width(&e.layout(&styled("tracking", CharStyle::default()), 72.0));
    let b = width(&e.layout(&styled("tracking", CharStyle { tracking: 100.0, ..Default::default() }), 72.0));
    // 100/1000 em at 12 px per letter (8 letters; trailing spacing counts too).
    assert!((b - a - 8.0 * 1.2).abs() < 1.3, "{a} → {b}");
}

#[test]
fn raster_coverage_scales_and_depths_agree() {
    let mut e = TextEngine::new();
    let t = |size: f32| TextLayer { transform: Affine::translate(10.0, 50.0), ..point("Ink", size) };
    let (_, r12) = e.render(&t(12.0), 72.0, PixelFormat::RGBA8);
    let (_, r24) = e.render(&t(24.0), 72.0, PixelFormat::RGBA8);
    let s12 = alpha_sum(&r12.surface, r12.rect);
    let s24 = alpha_sum(&r24.surface, r24.rect);
    assert!(s12 > 20.0, "{s12}");
    let ratio = s24 / s12;
    assert!((3.6..4.4).contains(&ratio), "ink ∝ size²: {ratio}");
    // Placement: anchored at (10, 50) baseline.
    assert!(r12.rect.x0 >= 8 && r12.rect.x0 <= 11 && r12.rect.y1 <= 53 && r12.rect.y1 >= 50, "{:?}", r12.rect);
    for fmt in [PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAYA8, PixelFormat::CMYKA8] {
        let (_, r) = e.render(&t(12.0), 72.0, fmt);
        assert_eq!(r.rect, r12.rect);
        let s = alpha_sum(&r.surface, r.rect);
        assert!((s - s12).abs() / s12 < 0.01, "{fmt:?}: {s} vs {s12}");
    }
    // Deterministic.
    let (_, again) = e.render(&t(12.0), 72.0, PixelFormat::RGBA8);
    assert_eq!(again.surface, r12.surface);
    // Golden-ish total ink for "Ink" in Inter 12 px (±3%).
    assert!((s12 - GOLDEN_INK_12).abs() / GOLDEN_INK_12 < 0.03, "ink sum {s12}");
}

/// Sum of alpha for "Ink", Inter Regular 12 px (measured once; guards rasterizer regressions).
const GOLDEN_INK_12: f64 = 44.4;

#[test]
fn styles_change_pixels() {
    let mut e = TextEngine::new();
    let base = CharStyle { size_pt: 30.0, ..Default::default() };
    let ink = |e: &mut TextEngine, s: CharStyle| {
        let (_, r) = e.render(&TextLayer { transform: Affine::translate(5.0, 40.0), ..styled("Hi", s) }, 72.0, PixelFormat::RGBA8);
        (alpha_sum(&r.surface, r.rect), r.rect)
    };
    let (plain, prect) = ink(&mut e, base.clone());
    let (bold, _) = ink(&mut e, CharStyle { faux_bold: true, ..base.clone() });
    assert!(bold > plain * 1.1, "{plain} → {bold}");
    let (under, urect) = ink(&mut e, CharStyle { underline: true, ..base.clone() });
    assert!(under > plain && urect.y1 > prect.y1);
    let (_, irect) = ink(&mut e, CharStyle { faux_italic: true, ..base.clone() });
    assert!(irect.x1 > prect.x1, "slanted top extends right");
    let (_, srect) = ink(&mut e, CharStyle { baseline_shift_pt: 10.0, ..base.clone() });
    assert_eq!(srect.y0, prect.y0 - 10);
    let (_, hrect) = ink(&mut e, CharStyle { horizontal_scale: 2.0, ..base.clone() });
    assert!(hrect.width() as f32 > prect.width() as f32 * 1.7);
}

#[test]
fn multicolor_runs() {
    let mut e = TextEngine::new();
    let red = CharStyle { size_pt: 40.0, color: Color::rgb(1.0, 0.0, 0.0), ..Default::default() };
    let blue = CharStyle { color: Color::rgb(0.0, 0.0, 1.0), ..red.clone() };
    let t = TextLayer {
        text: "HH".into(),
        runs: vec![TextRun { len: 1, style: red }, TextRun { len: 1, style: blue }],
        transform: Affine::translate(0.0, 40.0),
        ..Default::default()
    };
    let (l, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
    assert_eq!(l.glyphs.len(), 2);
    let px = r.surface.read_region(r.rect);
    let opaque: Vec<&[f32; 4]> = px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0.99).collect();
    assert!(opaque.iter().any(|p| p[0] > 0.99 && p[2] < 0.01));
    assert!(opaque.iter().any(|p| p[2] > 0.99 && p[0] < 0.01));
    // CMYK target keeps colour in the document model.
    let (_, rc) = e.render(&t, 72.0, PixelFormat::CMYKA8);
    assert!(rc.surface.read_region(rc.rect).as_chunks::<5>().0.iter().any(|p| p[4] > 0.99 && p[1] > 0.9 && p[2] > 0.9));
}

#[test]
fn hit_test_and_caret() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("abc\ndef", 20.0), 72.0);
    assert_eq!(l.hit_test(-5.0, -5.0), 0);
    assert_eq!(l.hit_test(1000.0, -5.0), 3);
    assert_eq!(l.hit_test(-5.0, 24.0), 4);
    let (x, top, bottom) = l.caret(1);
    assert!(x > 0.0 && top < 0.0 && bottom > 0.0);
    let (x2, ..) = l.caret(2);
    assert!(x2 > x);
    assert_eq!(l.caret(3).0, l.lines[0].x1);
}

#[test]
fn empty_text_and_empty_lines() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("", 12.0), 72.0);
    assert_eq!(l.lines.len(), 1);
    assert!(l.glyphs.is_empty());
    let l = e.layout(&point("a\n\nb", 12.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[2].baseline - 2.0 * 14.4).abs() < 1e-3);
    let (_, r) = e.render(&point("", 12.0), 72.0, PixelFormat::RGBA8);
    assert_eq!(r.rect.width(), 0);
}

#[test]
fn postscript_names() {
    let g = fonts::guess_from_postscript("MyriadPro-BoldIt");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Myriad Pro", 700, true));
    let g = fonts::guess_from_postscript("TimesNewRomanPSMT");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Times New Roman", 400, false));
    let g = fonts::guess_from_postscript("Arial-BoldMT");
    assert_eq!((g.family.as_str(), g.weight), ("Arial", 700));
    let mut db = fonts::FontDb::new();
    let r = db.resolve_postscript("Inter-SemiBold");
    assert_eq!((r.family.as_str(), r.weight, r.exact), ("Inter", 600, true));
    // Unknown fonts fall back gracefully in layout.
    let mut e = TextEngine::new();
    let t = styled("x", CharStyle { font_family: "Nonexistent Sans".into(), postscript_name: Some("Nope-Bold".into()), ..Default::default() });
    assert!(e.layout(&t, 72.0).glyphs[0].id != 0);
}

#[test]
fn depth_is_respected() {
    let mut e = TextEngine::new();
    let (_, r) = e.render(&TextLayer { transform: Affine::translate(2.0, 20.0), ..point("a", 20.0) }, 72.0, PixelFormat::RGBA16);
    assert_eq!(r.surface.format().sample, SampleType::U16);
    assert!(r.surface.format().alpha);
}

/// Visual check: `PHOTOCRAFT_TEXT_DUMP=/tmp/t.ppm cargo test -p photocraft-text dump -- --ignored`.
#[test]
#[ignore]
fn dump_sample() {
    use photocraft_doc::text::{Caps, TextAlign};
    let Some(path) = std::env::var_os("PHOTOCRAFT_TEXT_DUMP") else {
        return;
    };
    let mut e = if std::env::var_os("PHOTOCRAFT_TEXT_SYSTEM").is_some() { TextEngine::with_system_fonts() } else { TextEngine::new() };
    let s = CharStyle { size_pt: 28.0, ..Default::default() };
    let text =
        "Photocraft Type\nBold faux, italic faux, underline\nשלום عربي mixed ✓\nJustified paragraph text wraps inside the box nicely and evenly across lines.";
    let runs = vec![
        TextRun { len: 16, style: CharStyle { size_pt: 40.0, color: Color::rgb(0.1, 0.3, 0.9), caps: Caps::Normal, ..s.clone() } },
        TextRun { len: 5, style: CharStyle { faux_bold: true, ..s.clone() } },
        TextRun { len: 13, style: CharStyle { faux_italic: true, color: Color::rgb(0.8, 0.1, 0.1), ..s.clone() } },
        TextRun { len: 11, style: CharStyle { underline: true, strikethrough: false, ..s.clone() } },
        TextRun { len: 1000, style: CharStyle { size_pt: 20.0, tracking: 20.0, ..s.clone() } },
    ];
    let t = TextLayer {
        text: text.into(),
        runs,
        paragraphs: vec![
            ParagraphRun { len: 45, style: ParagraphStyle { align: TextAlign::Left, ..Default::default() } },
            ParagraphRun { len: 1000, style: ParagraphStyle { align: TextAlign::JustifyLeft, space_before_pt: 6.0, ..Default::default() } },
        ],
        shape: TextShape::Box { x: 0.0, y: 0.0, width: 560.0, height: 400.0 },
        transform: Affine::translate(20.0, 20.0),
        ..Default::default()
    };
    let (_, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
    let (w, h) = (600u32, 360u32);
    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let p = r.surface.pixel(x, y);
            let a = p[3];
            for c in &p[..3] {
                out.push(((c * a + (1.0 - a)) * 255.0).round() as u8);
            }
        }
    }
    std::fs::write(path, out).unwrap();
}

/// Variable-font axes reach the outlines (uses a system variable font when one is present).
#[test]
fn variable_font_axes() {
    let candidates =
        ["/System/Library/Fonts/SFNS.ttf", "/usr/share/fonts/truetype/noto/NotoSans-VariableFont_wdth,wght.ttf", "C:\\Windows\\Fonts\\bahnschrift.ttf"];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut e = TextEngine::new();
    let fams = e.fonts.register_font_data(bytes);
    let Some(fam) = fams.first().cloned() else {
        return;
    };
    if !e.fonts.faces(&fam).iter().any(|f| f.axes.iter().any(|a| a.0 == "wght")) {
        return;
    }
    let ink = |e: &mut TextEngine, w: f32| {
        let s = CharStyle {
            font_family: fam.clone(),
            size_pt: 40.0,
            variations: vec![photocraft_doc::text::FontVariation { axis: "wght".into(), value: w }],
            ..Default::default()
        };
        let (_, r) = e.render(&TextLayer { transform: Affine::translate(0.0, 50.0), ..styled("Weight", s) }, 72.0, PixelFormat::RGBA8);
        alpha_sum(&r.surface, r.rect)
    };
    let light = ink(&mut e, 200.0);
    let heavy = ink(&mut e, 800.0);
    assert!(heavy > light * 1.5, "{light} → {heavy}");
}

#[test]
fn warp_bends_rendered_text_and_outlines() {
    use photocraft_doc::text::TextWarp;
    let mut e = TextEngine::new();
    let mut t = point("WARPED TEXT", 24.0);
    t.transform = Affine::translate(20.0, 60.0);
    let fmt = PixelFormat::RGBA8;
    let (_, flat) = e.render(&t, 72.0, fmt);
    t.warp = Some(TextWarp { style: "warpArc".into(), value: 60.0, horizontal: true, ..Default::default() });
    let (layout, arced) = e.render(&t, 72.0, fmt);
    // An arc pushes the ends down: the warped ink is taller and pixels differ.
    assert!(arced.rect.height() > flat.rect.height() + 10, "{:?} vs {:?}", arced.rect, flat.rect);
    assert!(alpha_sum(&arced.surface, arced.rect) > 0.0);
    // Outlines follow the same warp: the first glyph's outline sits lower than the middle one's.
    let warp = crate::render::layout_warp(&layout, t.warp.as_ref());
    let outs = crate::render::outlines(&layout, &t.transform, warp.as_ref());
    assert_eq!(outs.len(), layout.glyphs.iter().filter(|g| g.id != 0).count() - 1, "space has no outline");
    let low_y = |els: &Vec<crate::render::PathEl>| {
        els.iter()
            .filter_map(|e| match e {
                crate::render::PathEl::MoveTo(p) | crate::render::PathEl::LineTo(p) => Some(p[1]),
                _ => None,
            })
            .fold(f64::MIN, f64::max)
    };
    assert!(low_y(&outs[0]) > low_y(&outs[outs.len() / 2]) + 5.0);
    // Unwarped outlines stay within the flat raster's bounds.
    let flat_outs = crate::render::outlines(&layout, &t.transform, None);
    let r = flat.rect;
    for els in &flat_outs {
        for el in els {
            if let crate::render::PathEl::MoveTo(p) | crate::render::PathEl::LineTo(p) = el {
                assert!(p[0] >= f64::from(r.x0) && p[0] <= f64::from(r.x1) && p[1] >= f64::from(r.y0) && p[1] <= f64::from(r.y1));
            }
        }
    }
}

/// `Txt2` as Photoshop writes it (a bare `/key value` sequence; text objects at `/1 /1`, style
/// runs at `/0 /6 /0`, auto-kern mode at `/11`), for the layer text `text`.
fn txt2_with_modes(text: &str, modes: &[(usize, i64)]) -> Vec<u8> {
    let mut v = b"\n\n/98 << /0 14 >> /0 << >> /1 << /1 [ << /0 << /0 (\xfe\xff".to_vec();
    for u in format!("{text}\r").encode_utf16() {
        let b = u.to_be_bytes();
        for x in b {
            if matches!(x, b'(' | b')' | b'\\') {
                v.push(b'\\');
            }
            v.push(x);
        }
    }
    v.extend_from_slice(b") /6 << /0 [ ");
    for (len, m) in modes {
        v.extend_from_slice(format!("<< /0 << /0 << /0 (\u{fe}\u{ff}) /6 << /0 0 /11 {m} >> >> >> /1 {len} >> ").as_bytes());
    }
    v.extend_from_slice(b"] >> >> >> ] >>");
    v
}

fn with_text_index(tysh: &[u8], index: i32) -> Vec<u8> {
    let mut t = crate::psd::parse_tysh(tysh).unwrap();
    t.text.items.retain(|(k, _)| !k.is("TextIndex"));
    t.text.items.push((photocraft_psd::descriptor::Id::new("TextIndex"), photocraft_psd::descriptor::Value::Integer(index)));
    crate::psd::write_tysh(&t)
}

/// Optical (and "0") kerning only lives in `Txt2`: import applies it when the layer's
/// `TextIndex` object still holds the same text.
#[test]
fn txt2_carries_optical_kerning() {
    use photocraft_doc::text::Kerning;
    let t = styled("AVA", CharStyle::default());
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), 0);
    let mut back = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    assert!(back.char_runs().iter().all(|r| r.style.kerning == Kerning::Metrics));
    // "AVA\r": two optical characters, then manual ("0") for the last and the break.
    let txt2 = crate::psd::parse_txt2(&txt2_with_modes("AVA", &[(2, 2), (2, 0)])).unwrap();
    crate::psd::apply_txt2(&mut back, &tysh, &txt2);
    let modes: Vec<(usize, Kerning)> = back.char_runs().iter().map(|r| (r.len, r.style.kerning)).collect();
    assert_eq!(modes, vec![(2, Kerning::Optical), (1, Kerning::Off)]);
    // Stale Txt2 (other text), another index or garbage: nothing changes, nothing panics.
    for (data, index) in [(txt2_with_modes("AVX", &[(4, 2)]), 0), (txt2_with_modes("AVA", &[(4, 2)]), 3), (b"<< /1 [ (x".to_vec(), 0), (Vec::new(), 0)] {
        let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), index);
        let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
        if let Some(txt2) = crate::psd::parse_txt2(&data) {
            crate::psd::apply_txt2(&mut l, &tysh, &txt2);
        }
        assert!(
            l.char_runs().iter().all(|r| r.style.kerning == Kerning::Metrics),
            "case {index}: {:?}",
            l.char_runs().iter().map(|r| r.style.kerning).collect::<Vec<_>>()
        );
    }
    // Run lengths longer than the text, zero lengths and odd modes are tolerated.
    let txt2 = crate::psd::parse_txt2(&txt2_with_modes("AVA", &[(0, 2), (100, 2), (5, 9)])).unwrap();
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    crate::psd::apply_txt2(&mut l, &tysh, &txt2);
    assert!(l.char_runs().iter().all(|r| r.style.kerning == Kerning::Optical));
}

/// EngineData pair fields: manual kerning and "no automatic kerning" round-trip exactly, in
/// Photoshop's form (see `psd::pair_runs`).
#[test]
fn psd_round_trips_manual_kerning() {
    use photocraft_doc::text::Kerning::{Metrics as M, Off as O};
    let s = CharStyle::default();
    let cases: Vec<Vec<(usize, photocraft_doc::text::Kerning, f32)>> = vec![
        vec![(1, O, 100.0), (1, O, -50.0), (2, M, 0.0)],
        vec![(1, M, 0.0), (1, O, 0.0), (2, M, 0.0)],
        vec![(4, O, 0.0)],
        vec![(2, M, 0.0), (1, O, 25.0), (1, M, 0.0)],
        vec![(3, M, 0.0), (1, O, 300.0)],
    ];
    for runs in cases {
        let t = runs_of("AVAT", &runs.iter().map(|&(len, kerning, kern)| (len, CharStyle { kerning, kern, ..s.clone() })).collect::<Vec<_>>());
        let back = crate::psd::text_layer_from_tysh(&crate::psd::build_tysh(&t, 72.0, None), 72.0).unwrap();
        let per = |t: &TextLayer| t.char_runs().iter().flat_map(|r| std::iter::repeat_n((r.style.kerning, r.style.kern), r.len)).collect::<Vec<_>>();
        let mut want = per(&t);
        // The last character's mode has no EngineData slot (it reads back as Metrics unless
        // it has a manual kern).
        if let Some(last) = want.last_mut()
            && last.1 == 0.0
        {
            last.0 = M;
        }
        assert_eq!(per(&back), want, "{runs:?}");
    }
}

#[test]
fn psd_round_trips_antialias_opentype_and_warp() {
    use photocraft_doc::text::{AntiAlias, TextWarp};
    let style = CharStyle {
        font_family: "Inter".into(),
        size_pt: 20.0,
        features: vec![FontFeature { tag: "swsh".into(), value: 1 }, FontFeature { tag: "frac".into(), value: 1 }],
        discretionary_ligatures: true,
        ..Default::default()
    };
    for aa in [AntiAlias::Windows, AntiAlias::WindowsLcd, AntiAlias::Crisp, AntiAlias::None] {
        let mut t = styled("1/2 Swash", style.clone());
        t.antialias = aa;
        t.warp = Some(TextWarp { style: "warpFlag".into(), value: -35.0, horizontal_distortion: 10.0, vertical_distortion: 0.0, horizontal: false });
        let bytes = crate::psd::build_tysh(&t, 72.0, None);
        let back = crate::psd::text_layer_from_tysh(&bytes, 72.0).unwrap();
        assert_eq!(back.antialias, aa);
        assert_eq!(back.warp, t.warp);
        let st = &back.char_runs()[0].style;
        assert!(st.discretionary_ligatures);
        let mut tags: Vec<&str> = st.features.iter().map(|f| f.tag.as_str()).collect();
        tags.sort_unstable();
        assert_eq!(tags, ["frac", "swsh"]);
    }
}

/// Regression: a PSD whose text engine data has no `EngineDict` (or isn't a dictionary)
/// panicked with `expect("EngineDict")` when the layer was written back (PSD export).
#[test]
fn engine_data_template_without_engine_dict() {
    use crate::engine_data::{self as ed, Value as E};
    let t = styled("Hi", CharStyle::default());
    let no_engine_dict = ed::parse(b"<< /ResourceDict << >> >>").unwrap();
    for template in [no_engine_dict, E::Dict(vec![]), E::Int(3)] {
        let e = crate::psd::build_engine_data(&t, Some(template), 72.0);
        let text = e.path(&["EngineDict", "Editor", "Text"]);
        assert!(matches!(text, Some(E::String(s)) if s == "Hi\r"), "{text:?}");
    }
}

/// #123: the caret geometry (clusters, lines) sits on the rendered glyphs, so a click on a glyph
/// lands next to it: its left part before it, its right part after it. Plain text, tracking,
/// horizontal scale, mixed sizes, and centred, wrapped paragraph text.
#[test]
fn clusters_sit_on_rendered_glyphs() {
    let mut e = TextEngine::new();
    let big = CharStyle { size_pt: 40.0, ..Default::default() };
    let small = CharStyle { size_pt: 18.0, ..Default::default() };
    let mixed =
        TextLayer { text: "HOHOHO".into(), runs: vec![TextRun { len: 3, style: big.clone() }, TextRun { len: 3, style: small.clone() }], ..Default::default() };
    let boxed = with_para(
        TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: 150.0, height: 200.0 }, ..styled("HOH HOH HOH HOH", small.clone()) },
        ParagraphStyle { align: TextAlign::Center, ..Default::default() },
    );
    let cases = [
        ("plain", styled("HOHOH", big.clone())),
        ("tracking", styled("HOHOH", CharStyle { tracking: 300.0, ..big.clone() })),
        ("hscale", styled("HOHOH", CharStyle { horizontal_scale: 1.6, ..big.clone() })),
        ("kerned", styled("HOHOH", CharStyle { kern: 250.0, kerning: photocraft_doc::text::Kerning::Optical, ..big.clone() })),
        ("mixed", mixed),
        ("box", boxed),
    ];
    for (name, t) in cases {
        let t = TextLayer { transform: Affine::translate(7.0, 60.0), ..t };
        let (l, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
        assert!(l.lines.len() >= if name == "box" { 2 } else { 1 }, "{name}");
        for c in l.clusters.iter().filter(|c| !t.text[c.range.clone()].trim().is_empty()) {
            let ln = &l.lines[c.line];
            // Ink under the cluster's middle, between the line's ascent and descent.
            let col = photocraft_geom::Rect::new(
                (c.x + c.advance * 0.3 + 7.0).floor() as i32,
                (ln.baseline - ln.ascent + 60.0).floor() as i32,
                (c.x + c.advance * 0.55 + 7.0).ceil() as i32,
                (ln.baseline + ln.descent + 60.0).ceil() as i32,
            );
            assert!(alpha_sum(&r.surface, col) > 1.0, "{name}: no ink under cluster {:?} ({col:?})", c.range);
            let y = ln.baseline - ln.ascent * 0.4;
            assert_eq!(l.hit_test(c.x + c.advance * 0.2, y), c.range.start, "{name}: left part of {:?}", c.range);
            assert_eq!(l.hit_test(c.x + c.advance * 0.8, y), c.range.end, "{name}: right part of {:?}", c.range);
            let (cx, top, bottom) = l.caret(c.range.start);
            assert!((cx - c.x).abs() < 1e-3 && top < y && bottom > y, "{name}: caret at {:?}", c.range);
        }
    }
}

fn runs_of(text: &str, styles: &[(usize, CharStyle)]) -> TextLayer {
    TextLayer { text: text.into(), runs: styles.iter().map(|(len, style)| TextRun { len: *len, style: style.clone() }).collect(), ..Default::default() }
}

/// Manual kerning (1/1000 em) after a character moves everything after it by kern × size.
#[test]
fn manual_kerning_moves_the_next_glyph() {
    use photocraft_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let plain = e.layout(&styled("HOH", s.clone()), 72.0);
    let kerned = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: 100.0, ..s.clone() }), (2, s.clone())]), 72.0);
    let x = |l: &crate::TextLayout, i: usize| l.glyphs[i].x;
    assert_eq!(x(&kerned, 0), x(&plain, 0));
    // 100/1000 em at 100 px = 10 px, for the next glyph and everything after it.
    assert!((x(&kerned, 1) - x(&plain, 1) - 10.0).abs() < 1e-3, "{} vs {}", x(&kerned, 1), x(&plain, 1));
    assert!((x(&kerned, 2) - x(&plain, 2) - 10.0).abs() < 1e-3);
    assert!((width(&kerned) - width(&plain) - 10.0).abs() < 1e-3);
    // Carets follow: the cluster after the kerned pair starts 10 px later.
    assert!((kerned.caret(1).0 - plain.caret(1).0 - 10.0).abs() < 1e-3);
    // Negative kerning tightens; kerning on the last character doesn't move anything.
    let tight = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: -50.0, ..s.clone() }), (2, s.clone())]), 72.0);
    assert!((x(&tight, 1) - x(&plain, 1) + 5.0).abs() < 1e-3);
    let last = e.layout(&runs_of("HOH", &[(2, s.clone()), (1, CharStyle { kern: 500.0, ..s.clone() })]), 72.0);
    assert!((width(&last) - width(&plain)).abs() < 1e-3);
    // Off replaces the font's pair kerning: "AV" with Off is wider than with Metrics.
    let mut av = |st: CharStyle| e.layout(&styled("AV", st), 72.0).glyphs[1].x;
    let metric = av(s.clone());
    let off = av(CharStyle { kerning: Kerning::Off, ..s.clone() });
    assert!(off > metric + 1.0, "Inter kerns AV: {off} vs {metric}");
    // Centred point text stays centred around the anchor with kerning.
    let centred = with_para(
        runs_of("HOH", &[(1, CharStyle { kern: 300.0, ..s.clone() }), (2, s.clone())]),
        ParagraphStyle { align: TextAlign::Center, ..Default::default() },
    );
    let l = e.layout(&centred, 72.0);
    assert!((l.lines[0].x0 + l.lines[0].x1).abs() < 0.5, "{:?}", l.lines[0]);
}

/// Optical kerning computes pair spacing from the outlines: tighter for open pairs ("AV", "To")
/// than the unkerned advance, about neutral for straight stems, and never absurd.
#[test]
fn optical_kerning_tightens_open_pairs() {
    use photocraft_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let gap = |e: &mut TextEngine, text: &str, k: Kerning| {
        let l = e.layout(&styled(text, CharStyle { kerning: k, ..s.clone() }), 72.0);
        l.glyphs[1].x - l.glyphs[0].x
    };
    for pair in ["AV", "To", "LT", "Ty"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!(optical < off - 3.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    for pair in ["HH", "nn", "oo", "HO"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!((optical - off).abs() < 6.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    // A space breaks the pair; a manual kern replaces the automatic one (as in Photoshop).
    assert_eq!(gap(&mut e, "A V", Kerning::Optical), gap(&mut e, "A V", Kerning::Off));
    for mode in [Kerning::Optical, Kerning::Metrics] {
        let l = e.layout(&styled("AV", CharStyle { kerning: mode, kern: 100.0, ..s.clone() }), 72.0);
        let off = gap(&mut e, "AV", Kerning::Off);
        assert!((l.glyphs[1].x - l.glyphs[0].x - off - 10.0).abs() < 1e-3, "{mode:?}");
    }
}

/// A mode change splits shaping runs: the pairs on both sides of a manually kerned character
/// lose their automatic kerning (Photoshop renders it the same way).
#[test]
fn kerning_modes_split_pairs() {
    use photocraft_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let off = CharStyle { kerning: Kerning::Off, ..s.clone() };
    let xs = |e: &mut TextEngine, t: &TextLayer| e.layout(t, 72.0).glyphs.iter().map(|g| g.x).collect::<Vec<_>>();
    let metric = xs(&mut e, &styled("AVAV", s.clone()));
    let plain = xs(&mut e, &styled("AVAV", off.clone()));
    let mixed = xs(&mut e, &runs_of("AVAV", &[(2, s.clone()), (1, off.clone()), (1, s.clone())]));
    let adv = |v: &[f32], i: usize| v[i + 1] - v[i];
    assert!((adv(&mixed, 0) - adv(&metric, 0)).abs() < 1e-3, "AV before stays kerned");
    assert!((adv(&mixed, 1) - adv(&plain, 1)).abs() < 1e-3, "VA into the manual character");
    assert!((adv(&mixed, 2) - adv(&plain, 2)).abs() < 1e-3, "AV out of it");
}

#[test]
fn japanese_dictionary_word_boundaries() {
    // Use only the existing bundled Latin fonts: segmentation must not require a CJK font.
    let text = "私は学生です";
    let mut fonts = fonts::FontDb::new();
    let mut context = parley::LayoutContext::<[u8; 4]>::new();
    let mut layout = parley::Layout::new();
    let mut builder = context.ranged_builder(&mut fonts.fcx, text, 1.0, false);
    builder.push_default(parley::StyleProperty::FontFamily(parley::FontFamily::named("Inter")));
    // Suppress CJK line-break opportunities so these flags expose word boundaries alone.
    builder.push_default(parley::StyleProperty::WordBreak(parley::WordBreak::KeepAll));
    builder.build_into(&mut layout, text);
    layout.break_all_lines(None);
    let mut boundaries = Vec::new();
    for line in layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                if cluster.is_word_boundary() {
                    boundaries.push(cluster.text_range().start);
                }
            }
        }
    }
    assert_eq!(boundaries, vec![0, 3, 6, 12]);
}

#[test]
fn japanese_box_text_preserves_wrap_boundaries() {
    let mut engine = TextEngine::new();
    let mut text = point("日本語の文章を折り返します。", 12.0);
    text.shape = TextShape::Box { x: 0.0, y: 0.0, width: 40.0, height: 1000.0 };
    let layout = engine.layout(&text, 72.0);
    assert!(layout.lines.len() > 1);
    let mut end = 0;
    for line in &layout.lines {
        assert_eq!(line.range.start, end);
        let content = text.text.get(line.range.clone()).expect("UTF-8 line boundaries");
        assert!(!content.starts_with('。'), "closing punctuation must stay with its preceding text");
        end = line.range.end;
    }
    assert_eq!(end, text.text.len());
}

/// Which craft-fonts family drew the glyphs of `l` (matched by the embedded bytes).
fn craft_family_of(l: &crate::TextLayout) -> Vec<&'static str> {
    let mut v = Vec::new();
    for g in &l.glyphs {
        let Some(face) = l.faces.get(g.face as usize) else { continue };
        let data: &[u8] = face.font.data.as_ref();
        if let Some(f) = crate::CRAFT_FONTS.iter().find(|f| std::ptr::eq(f.bytes.as_ptr(), data.as_ptr()) && f.bytes.len() == data.len())
            && !v.contains(&f.family)
        {
            v.push(f.family);
        }
    }
    v
}

#[test]
fn craft_fonts_cover_japanese_without_system_fonts() {
    if !crate::CRAFT_FONTS.iter().any(|f| f.is_japanese()) {
        eprintln!("skipping: built without craft-fonts (set CRAFT_FONTS_DIR to a craft-fonts checkout)");
        return;
    }
    let mut e = TextEngine::new();
    let text = "日本語の文字、カタカナ。";
    let l = e.layout(&point(text, 24.0), 72.0);
    assert_eq!(l.glyphs.len(), text.chars().count());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef with craft-fonts");
    // Sans (Inter) runs fall back to the Gothic UI family.
    assert_eq!(craft_family_of(&l), vec![crate::craft_fonts::UI_JAPANESE_FAMILY]);
    // Serif runs fall back to a Mincho face when the build has one (not the web build).
    if crate::CRAFT_FONTS.iter().any(|f| f.is_mincho()) {
        let mut t = point(text, 24.0);
        t.font_family = "Times New Roman".into();
        let l = e.layout(&t, 72.0);
        assert!(l.glyphs.iter().all(|g| g.id != 0));
        let fams = craft_family_of(&l);
        assert!(fams.len() == 1 && fams[0].contains("Mincho"), "{fams:?}");
    }
    // Latin keeps Inter.
    let l = e.layout(&point("Layer 1", 24.0), 72.0);
    assert!(craft_family_of(&l).is_empty());
}

#[test]
fn works_without_craft_fonts() {
    // Whatever the build: the bundled fonts load, Latin lays out, and Japanese never panics
    // (without craft-fonts or system fonts it may be .notdef).
    let mut e = TextEngine::new();
    assert!(e.fonts.has_family("Inter"));
    let l = e.layout(&point("日本語 Latin", 12.0), 72.0);
    assert_eq!(l.glyphs.len(), "日本語 Latin".chars().count());
    for f in crate::CRAFT_FONTS {
        assert!(e.fonts.has_family(f.family), "{} registered under its manifest name", f.family);
    }
    if crate::CRAFT_FONTS.is_empty() {
        assert!(crate::craft_fonts::japanese_families().is_empty());
        let fb = fonts::fallback_candidates(&crate::cjk::script_order(Some("ja")));
        assert!(!fb.iter().any(|f| f.contains("BIZ UD")));
    }
}

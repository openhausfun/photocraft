//! Shaping and layout of a text layer's model into positioned glyphs (text-space pixels, y down,
//! first baseline of point text at y = 0).
//!
//! Each paragraph is shaped and line-broken by parley (HarfRust shaping, Unicode bidi and line
//! breaking); paragraphs are then stacked with Photoshop's rules: baseline-to-baseline distance =
//! the largest leading on the line (auto leading = paragraph factor × size), space before/after,
//! indents, and point-text alignment around the anchor.
//!
//! **Vertical type** (tategaki) is laid out in the same *line space* and then turned 90° clockwise
//! into text space: a line becomes a column running top to bottom, and successive columns advance
//! right to left. Within a column, CJK characters stay upright (centred on the column, placed with
//! the font's vertical metrics: `VORG`, `vhea`/`vmtx`, else the ideographic em box) and use the
//! font's vertical alternates (`vert`) for punctuation, brackets and the long-vowel mark; other
//! text (Latin, digits) runs rotated 90° clockwise, Photoshop's default. Lines, clusters, carets
//! and hit tests stay in line space; [`TextLayout::to_text`] / [`TextLayout::to_line`] convert.

use std::borrow::Cow;
use std::ops::Range;

use parley::{
    Alignment, AlignmentOptions, FontData, FontFamily, FontFeatures, FontStyle, FontVariations, FontWeight, IndentOptions, Layout, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};
use photocraft_doc::TextLayer;
use photocraft_doc::text::{Caps, CharStyle, Kerning, Orientation, TextAlign, TextDirection, TextShape};

use crate::fonts::FontDb;

/// Index of the character run whose style a glyph uses, plus the vertical-type class of its
/// characters ([`VClass`] as `u8`; always 0 in horizontal type).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunBrush(pub u32, pub u8);

/// How a character is set in vertical type (a simplification of Unicode's
/// `Vertical_Orientation`, UAX #50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VClass {
    /// Rotated 90° clockwise (Latin, digits, most symbols).
    Rotate = 0,
    /// Upright (ideographs, kana, Hangul, full-width forms, CJK punctuation such as 、。),
    /// using a vertical alternate when the font has one.
    Upright = 1,
    /// Upright through its vertical alternate (brackets, the long-vowel mark, dashes); rotated
    /// when the font has none.
    AlternateOrRotate = 2,
}

/// The vertical-type class of a character.
pub fn vertical_class(c: char) -> VClass {
    let u = c as u32;
    let alt = matches!(
        u,
        0x2014..=0x2016
            | 0x2025
            | 0x2026
            | 0x2329
            | 0x232A
            | 0x3008..=0x3011
            | 0x3014..=0x301F
            | 0x3030
            | 0x30A0
            | 0x30FC
            | 0xFE59..=0xFE5E
            | 0xFF08
            | 0xFF09
            | 0xFF0D
            | 0xFF1A..=0xFF1E
            | 0xFF3B
            | 0xFF3D
            | 0xFF3F
            | 0xFF5B..=0xFF60
            | 0xFFE3
    );
    if alt {
        return VClass::AlternateOrRotate;
    }
    // Half-width katakana and Hangul rotate like other half-width text.
    if (0xFF61..=0xFFDC).contains(&u) {
        return VClass::Rotate;
    }
    let upright = crate::cjk::classify(c).is_some()
        || matches!(u, 0xA7 | 0xA9 | 0xAE | 0xB1 | 0xBC..=0xBE | 0xD7 | 0xF7 | 0x2E80..=0x2EFF | 0xFE10..=0xFE1F | 0xFE30..=0xFE4F | 0x1F000..=0x1FAFF);
    if upright { VClass::Upright } else { VClass::Rotate }
}

/// How a placed glyph's outline is oriented in text space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphOrient {
    /// Horizontal type: origin on the baseline, outline upright.
    #[default]
    Horizontal,
    /// Vertical type, upright: the origin is the glyph's horizontal origin (left end of its
    /// advance, on its baseline); baseline shift moves it right.
    Upright,
    /// Vertical type, rotated 90° clockwise about its origin (the baseline runs downwards).
    Rotated,
}

/// A font instance used by some glyphs.
#[derive(Clone, Debug)]
pub struct GlyphFace {
    pub font: FontData,
    /// Normalised variation coordinates (F2Dot14 bits).
    pub coords: Vec<i16>,
    pub size_px: f32,
    /// Synthetic bold requested by font matching (face lacks the weight).
    pub embolden: bool,
    /// Synthetic oblique angle in degrees (face lacks italics).
    pub skew_deg: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub face: u32,
    pub id: u32,
    /// Pen position (text space px): x along the baseline, y = baseline (before baseline shift).
    pub x: f32,
    pub y: f32,
    /// Character run index (into [`TextLayout::styles`]).
    pub style: u32,
    /// Orientation of the outline (vertical type).
    pub orient: GlyphOrient,
}

/// Underline/strikethrough rectangle (text space px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub style: u32,
}

/// A line (a column in vertical type) in line space: `x` runs along the line and `baseline` is the
/// cross-line position. In vertical type `baseline` is the column's centre line and
/// `ascent`/`descent` are the column's half widths on either side.
#[derive(Clone, Debug, PartialEq)]
pub struct LineInfo {
    /// Byte range in the layer text (without the paragraph break).
    pub range: Range<usize>,
    pub baseline: f32,
    /// Visual extent along the baseline.
    pub x0: f32,
    pub x1: f32,
    pub ascent: f32,
    pub descent: f32,
    pub paragraph: usize,
}

/// A grapheme cluster (caret stops, hit testing, selection), in line space.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterInfo {
    pub range: Range<usize>,
    pub x: f32,
    pub advance: f32,
    pub line: usize,
    pub rtl: bool,
}

/// Result of laying out a text layer.
#[derive(Clone, Debug, Default)]
pub struct TextLayout {
    pub faces: Vec<GlyphFace>,
    pub glyphs: Vec<PlacedGlyph>,
    pub decorations: Vec<DecorationRect>,
    pub lines: Vec<LineInfo>,
    pub clusters: Vec<ClusterInfo>,
    /// Resolved character styles (one per run of the layer).
    pub styles: Vec<CharStyle>,
    /// Pixels per point used (dpi / 72).
    pub px_per_pt: f32,
    /// Vertical type: line space is turned 90° clockwise into text space.
    pub vertical: bool,
}

impl TextLayout {
    /// Line space → text space: the identity for horizontal type; for vertical type the line
    /// direction points down and the cross-line direction (towards the next line) points left.
    pub fn to_text(&self, x: f32, y: f32) -> (f32, f32) {
        if self.vertical { (-y, x) } else { (x, y) }
    }

    /// Text space → line space (inverse of [`Self::to_text`]).
    pub fn to_line(&self, x: f32, y: f32) -> (f32, f32) {
        if self.vertical { (y, -x) } else { (x, y) }
    }

    /// Logical bounds (x0, y0, x1, y1) of all lines, text space.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let b = self.line_bounds()?;
        Some(if self.vertical { [-b[3], b[0], -b[1], b[2]] } else { b })
    }

    /// Logical bounds of all lines in line space (equal to [`Self::bounds`] for horizontal type).
    pub fn line_bounds(&self) -> Option<[f32; 4]> {
        self.lines.iter().fold(None, |acc, l| {
            let r = [l.x0, l.baseline - l.ascent, l.x1, l.baseline + l.descent];
            Some(match acc {
                None => r,
                Some(a) => [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])],
            })
        })
    }

    /// Byte offset of the caret nearest to a text-space point.
    pub fn hit_test(&self, x: f32, y: f32) -> usize {
        let (x, y) = self.to_line(x, y);
        self.hit_test_line(x, y)
    }

    /// Byte offset of the caret nearest to a line-space point.
    pub fn hit_test_line(&self, x: f32, y: f32) -> usize {
        let Some((li, line)) = self.lines.iter().enumerate().min_by(|a, b| {
            let d = |l: &LineInfo| {
                if y < l.baseline - l.ascent {
                    l.baseline - l.ascent - y
                } else if y > l.baseline + l.descent {
                    y - l.baseline - l.descent
                } else {
                    0.0
                }
            };
            d(a.1).total_cmp(&d(b.1))
        }) else {
            return 0;
        };
        let mut best = (f32::MAX, line.range.end);
        for c in self.clusters.iter().filter(|c| c.line == li) {
            let mid = c.x + c.advance / 2.0;
            let (before, after) = if c.rtl { (c.range.end, c.range.start) } else { (c.range.start, c.range.end) };
            let (dist, off) = if x < mid { ((x - c.x).abs(), before) } else { ((x - c.x - c.advance).abs(), after) };
            if dist < best.0 {
                best = (dist, off);
            }
        }
        best.1
    }

    /// The caret for a byte offset as a text-space segment (its two end points).
    pub fn caret_segment(&self, offset: usize) -> [(f32, f32); 2] {
        let (x, top, bottom) = self.caret(offset);
        [self.to_text(x, top), self.to_text(x, bottom)]
    }

    /// Caret geometry for a byte offset: (x, top, bottom) in line space (the same as text space
    /// for horizontal type; see [`Self::caret_segment`]).
    pub fn caret(&self, offset: usize) -> (f32, f32, f32) {
        for c in &self.clusters {
            if c.range.start == offset
                && let Some(l) = self.lines.get(c.line)
            {
                let x = if c.rtl { c.x + c.advance } else { c.x };
                return (x, l.baseline - l.ascent, l.baseline + l.descent);
            }
        }
        // End of a line (or empty line): after the last cluster of the line containing it.
        let li = self.lines.iter().position(|l| offset >= l.range.start && offset <= l.range.end).unwrap_or(self.lines.len().saturating_sub(1));
        match self.lines.get(li) {
            Some(l) => {
                let x = self
                    .clusters
                    .iter()
                    .filter(|c| c.line == li && c.range.end == offset)
                    .map(|c| if c.rtl { c.x } else { c.x + c.advance })
                    .next()
                    .unwrap_or(l.x1);
                (x, l.baseline - l.ascent, l.baseline + l.descent)
            }
            None => (0.0, 0.0, 0.0),
        }
    }
}

const LRM: &str = "\u{200E}";
const RLM: &str = "\u{200F}";

pub(crate) struct Layouter {
    lcx: LayoutContext<RunBrush>,
    optical: crate::optical::Cache,
}

/// A cluster of a line in visual order, for kerning.
struct KernSlot {
    /// Index of the run in the paragraph layout.
    run: usize,
    /// Byte offset in the paragraph text.
    start: usize,
    first: Option<u32>,
    last: Option<u32>,
    font: FontData,
    coords: Vec<i16>,
    size: f32,
    rtl: bool,
    blank: bool,
    /// Vertical type, upright glyph (its outline doesn't run along the column).
    upright: bool,
}

impl Layouter {
    pub fn new() -> Self {
        Self { lcx: LayoutContext::new(), optical: crate::optical::Cache::default() }
    }

    pub fn layout(&mut self, fonts: &mut FontDb, t: &TextLayer, dpi: f32) -> TextLayout {
        let k = if dpi > 0.0 { dpi / 72.0 } else { 1.0 };
        let runs = t.char_runs();
        let paras = t.paragraph_runs();
        let vertical = t.orientation == Orientation::Vertical;
        let mut out = TextLayout { px_per_pt: k, vertical, ..Default::default() };
        // Resolve families (PostScript names from PSDs, unknown families).
        for r in &runs {
            let mut s = r.style.clone();
            if let Some(ps) = s.postscript_name.clone() {
                let f = fonts.resolve_postscript(&ps);
                // An exact face match wins; a guessed family only fills a missing family.
                if f.exact || (!fonts.has_family(&s.font_family) && fonts.has_family(&f.family)) {
                    s.font_family = f.family;
                    s.weight = f.weight;
                    s.italic = f.italic;
                }
            }
            out.styles.push(s);
        }
        let run_starts: Vec<usize> = runs
            .iter()
            .scan(0, |a, r| {
                let s = *a;
                *a += r.len;
                Some(s)
            })
            .collect();
        let style_at = |off: usize| run_starts.iter().rposition(|&s| s <= off).unwrap_or(0);
        let para_starts: Vec<usize> = paras
            .iter()
            .scan(0, |a, r| {
                let s = *a;
                *a += r.len;
                Some(s)
            })
            .collect();
        let para_style_at = |off: usize| &paras[para_starts.iter().rposition(|&s| s <= off).unwrap_or(0)].style;

        let text = &t.text;
        let (box_rect, is_box) = match t.shape {
            TextShape::Box { x, y, width, height } => ((x, y, width, height), true),
            TextShape::Point => ((0.0, 0.0, 0.0, 0.0), false),
        };
        let mut prev_baseline: Option<f32> = None;
        let mut pending_space = 0.0f32;
        let mut stop = false;
        for (pi, prange) in split_paragraphs(text).into_iter().enumerate() {
            if stop {
                break;
            }
            let ps = para_style_at(prange.start).clone();
            let content_end = strip_break(text, &prange);
            let content = &text[prange.start..content_end];
            let prefix = match ps.direction {
                TextDirection::Auto => "",
                TextDirection::Ltr => LRM,
                TextDirection::Rtl => RLM,
            };
            let mut ptext = String::with_capacity(prefix.len() + content.len());
            ptext.push_str(prefix);
            for (i, ch) in content.char_indices() {
                let caps = out.styles[style_at(prange.start + i)].caps;
                if caps == Caps::AllCaps {
                    let up: String = ch.to_uppercase().collect();
                    if up.len() == ch.len_utf8() {
                        ptext.push_str(&up);
                        continue;
                    }
                }
                ptext.push(ch);
            }
            let first_style = &out.styles[style_at(prange.start)];
            let first_px = first_style.size_pt * k;
            let fallback: Vec<String> = fonts.fallback_stack().map(str::to_string).collect();
            let mut layout: Layout<RunBrush> = {
                let mut b = self.lcx.ranged_builder(&mut fonts.fcx, &ptext, 1.0, false);
                // Paragraph-start style as the default (covers the direction mark and empty
                // paragraphs), then every run piece intersecting this paragraph.
                let si0 = style_at(prange.start);
                for p in style_props(&out.styles[si0], k, &fallback, si0 as u32) {
                    b.push_default(p);
                }
                for (ri, st) in out.styles.iter().enumerate() {
                    let rs = run_starts[ri];
                    let (a, z) = (rs.max(prange.start), (rs + runs[ri].len).min(content_end));
                    if a >= z {
                        continue;
                    }
                    let range = (a - prange.start + prefix.len())..(z - prange.start + prefix.len());
                    for p in style_props(st, k, &fallback, ri as u32) {
                        b.push(p, range.clone());
                    }
                    if vertical {
                        // Upright pieces get the vertical alternates and their class in the brush.
                        let piece = ptext.get(range.clone()).unwrap_or("");
                        let mut seg: Option<(usize, VClass)> = None;
                        let flush = |b: &mut parley::RangedBuilder<'_, RunBrush>, from: usize, to: usize, cls: VClass| {
                            if cls == VClass::Rotate || from >= to {
                                return;
                            }
                            let mut feats = feature_list(st);
                            feats.push("\"vert\" 1".into());
                            let r = (range.start + from)..(range.start + to);
                            b.push(StyleProperty::FontFeatures(FontFeatures::Source(Cow::Owned(feats.join(", ")))), r.clone());
                            b.push(StyleProperty::Brush(RunBrush(ri as u32, cls as u8)), r);
                        };
                        for (i, ch) in piece.char_indices() {
                            let cls = vertical_class(ch);
                            match seg {
                                Some((_, c)) if c == cls => {}
                                Some((from, c)) => {
                                    flush(&mut b, from, i, c);
                                    seg = Some((i, cls));
                                }
                                None => seg = Some((i, cls)),
                            }
                        }
                        if let Some((from, c)) = seg {
                            flush(&mut b, from, piece.len(), c);
                        }
                    }
                }
                b.build(&ptext)
            };
            let indent_start = ps.start_indent_pt * k;
            let indent_end = ps.end_indent_pt * k;
            if ps.first_line_indent_pt != 0.0 {
                layout.set_text_indent(ps.first_line_indent_pt * k, IndentOptions::default());
            }
            // Box extent along the lines: width, or height for vertical type (columns).
            let (line_origin, line_len) = if vertical { (box_rect.1, box_rect.3) } else { (box_rect.0, box_rect.2) };
            let avail = if is_box { Some((line_len - indent_start - indent_end).max(1.0)) } else { None };
            layout.break_all_lines(avail);
            let alignment = if is_box {
                match ps.align {
                    TextAlign::Left => Alignment::Left,
                    TextAlign::Center => Alignment::Center,
                    TextAlign::Right => Alignment::Right,
                    _ => Alignment::Justify,
                }
            } else {
                Alignment::Left
            };
            layout.align(alignment, AlignmentOptions { align_when_overflowing: !is_box });

            // Stack lines.
            if prev_baseline.is_some() {
                pending_space += ps.space_before_pt * k;
            }
            let nlines = layout.len();
            for (li, line) in layout.lines().enumerate() {
                let m = *line.metrics();
                // Largest leading among the glyph runs of the line.
                let mut leading = 0.0f32;
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(gr) = item {
                        let st = &out.styles[gr.style().brush.0 as usize];
                        let px = gr.run().font_size();
                        leading = leading.max(st.leading_pt.map_or(ps.auto_leading * px, |l| l * k));
                    }
                }
                if leading == 0.0 {
                    let st = &out.styles[style_at(prange.start)];
                    leading = st.leading_pt.map_or(ps.auto_leading * st.size_pt * k, |l| l * k);
                }
                // Vertical type: half the column width (the largest em on the column).
                let col_half = line
                    .items()
                    .filter_map(|it| match it {
                        PositionedLayoutItem::GlyphRun(gr) => Some(gr.run().font_size() * 0.5),
                        _ => None,
                    })
                    .fold(0.0f32, f32::max);
                let col_half = if col_half > 0.0 { col_half } else { first_px * 0.5 };
                let (ascent, descent) = if vertical {
                    (col_half, col_half)
                } else if m.ascent > 0.0 || m.descent > 0.0 {
                    (m.ascent, m.descent)
                } else {
                    (first_px * 0.8, first_px * 0.2)
                };
                let baseline = match prev_baseline {
                    // The first column's right edge touches the box's right edge.
                    None if is_box && vertical => -(box_rect.0 + box_rect.2) + col_half,
                    // Photoshop's "first baseline: ascent": the top of the tallest ascender
                    // (height of 'd') touches the box top, not the font's hhea ascent.
                    None if is_box => box_rect.1 + first_ascent(&line).unwrap_or(ascent),
                    None => 0.0,
                    Some(b) => b + leading + pending_space,
                };
                pending_space = 0.0;
                let overflow = if vertical { baseline + descent > -box_rect.0 + 0.5 } else { baseline + descent > box_rect.1 + box_rect.3 + 0.5 };
                if is_box && overflow {
                    stop = true;
                    break;
                }
                prev_baseline = Some(baseline);
                let last_line = li + 1 == nlines;
                let adv = m.advance - m.trailing_whitespace;
                let map = |o: usize| (prange.start + o.saturating_sub(prefix.len())).min(content_end);

                // Kerning after each cluster (px), per run in visual cluster order: manual
                // kerning plus optical pair kerning. Applied after shaping (like horizontal
                // scale) in line space, so it moves the following glyphs, carets and the line
                // extent; in vertical type that is along the column. Optical kerning measures
                // horizontal outlines, so it applies to rotated (Latin) glyphs only there.
                let mut slots: Vec<KernSlot> = Vec::new();
                let mut line_runs: Vec<usize> = Vec::new();
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(gr) = item else {
                        continue;
                    };
                    let run = gr.run();
                    if line_runs.contains(&run.index()) {
                        continue;
                    }
                    line_runs.push(run.index());
                    for c in run.visual_clusters() {
                        let mut gl = c.glyphs();
                        let first = gl.next().map(|g| g.id);
                        let last = gl.last().map(|g| g.id).or(first);
                        slots.push(KernSlot {
                            run: run.index(),
                            start: c.text_range().start,
                            first,
                            last,
                            font: run.font().clone(),
                            coords: run.normalized_coords().to_vec(),
                            size: run.font_size(),
                            rtl: c.is_rtl(),
                            blank: first.is_none() || c.is_space_or_nbsp() || c.text_range().end <= prefix.len(),
                            upright: vertical && c.first_style().brush.1 != VClass::Rotate as u8,
                        });
                    }
                }
                let mut kern_px: Vec<f32> = vec![0.0; slots.len()];
                for j in 0..slots.len().saturating_sub(1) {
                    let (a, b) = (&slots[j], &slots[j + 1]);
                    if a.start < prefix.len() {
                        continue;
                    }
                    let st = &out.styles[style_at(map(a.start))];
                    let next = &out.styles[style_at(map(b.start))];
                    let mut units = if st.kern.is_finite() { st.kern } else { 0.0 };
                    // Optical pairs: both characters optical (a mode change splits shaping
                    // runs, which ends automatic kerning, as with Metrics).
                    if st.kerning == Kerning::Optical
                        && next.kerning == Kerning::Optical
                        && st.kern == 0.0
                        && next.kern == 0.0
                        && !a.rtl
                        && !b.rtl
                        && !a.blank
                        && !b.blank
                        && !a.upright
                        && !b.upright
                        && a.font.data.id() == b.font.data.id()
                        && a.font.index == b.font.index
                        && a.coords == b.coords
                        && (a.size - b.size).abs() < 1e-3
                        && let (Some(l), Some(r)) = (a.last, b.first)
                    {
                        let coords: Vec<skrifa::instance::NormalizedCoord> =
                            a.coords.iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
                        if let Some(k) = self.optical.pair(a.font.data.id(), a.font.data.as_ref(), a.font.index, &coords, l, r) {
                            units += k + crate::optical::size_adjust(st.size_pt);
                        }
                    }
                    kern_px[j] = units / 1000.0 * a.size;
                }
                let line_kern: f32 = kern_px.iter().sum();
                // Per run on the line: (run index, glyph → slot, glyphs emitted, first slot whose
                // kerning isn't applied yet).
                let mut cursors: Vec<(usize, Vec<usize>, usize, usize)> = Vec::new();
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(gr) = item else {
                        continue;
                    };
                    let run = gr.run();
                    if cursors.iter().any(|c| c.0 == run.index()) {
                        continue;
                    }
                    let base = slots.iter().position(|s| s.run == run.index()).unwrap_or(0);
                    let mut g2s = Vec::new();
                    for (ci, c) in run.visual_clusters().enumerate() {
                        g2s.extend(c.glyphs().map(|_| base + ci));
                    }
                    cursors.push((run.index(), g2s, 0, base));
                }
                // Parley aligned box lines without the kerning.
                let kern_align = if is_box {
                    match ps.align {
                        TextAlign::Center => line_kern / 2.0,
                        TextAlign::Right => line_kern,
                        _ => 0.0,
                    }
                } else {
                    0.0
                };
                let dx = if is_box {
                    let base = line_origin + indent_start - kern_align;
                    let slack = avail.unwrap_or(0.0) - adv - line_kern;
                    base + if last_line {
                        match ps.align {
                            TextAlign::JustifyCenter => slack * 0.5 - m.offset,
                            TextAlign::JustifyRight => slack - m.offset,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    }
                } else {
                    let target = match ps.align {
                        TextAlign::Center | TextAlign::JustifyCenter => -(adv + line_kern) / 2.0,
                        TextAlign::Right | TextAlign::JustifyRight => -(adv + line_kern),
                        _ => indent_start,
                    };
                    target - m.offset
                };
                let justify_all = is_box && last_line && ps.align == TextAlign::JustifyAll;
                let line_index = out.lines.len();
                let lr = line.text_range();
                let g0 = out.glyphs.len();
                let c0 = out.clusters.len();
                let mut vinfo: Vec<VGlyph> = Vec::new();
                let mut extra = 0.0f32; // horizontal-scale growth and kerning along the line
                let mut seen_runs: Vec<usize> = Vec::new();
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(gr) = item else {
                        continue;
                    };
                    let run = gr.run();
                    let si = gr.style().brush.0;
                    let st = &out.styles[si as usize];
                    let hs = if st.horizontal_scale > 0.0 { st.horizontal_scale } else { 1.0 };
                    let synth = run.synthesis();
                    let face = out.faces.len() as u32;
                    out.faces.push(GlyphFace {
                        font: run.font().clone(),
                        coords: run.normalized_coords().to_vec(),
                        size_px: run.font_size(),
                        embolden: synthetic_bold(synth.embolden(), st.weight, run.font_attrs().weight.value()),
                        skew_deg: synth.skew().unwrap_or(0.0),
                    });
                    let run_x0 = dx + gr.offset() + extra;
                    if !seen_runs.contains(&run.index()) {
                        seen_runs.push(run.index());
                        let mut cx = run_x0;
                        let base = slots.iter().position(|s| s.run == run.index()).unwrap_or(0);
                        for (ci, c) in run.visual_clusters().enumerate() {
                            let a = c.advance() * hs + kern_px.get(base + ci).copied().unwrap_or(0.0);
                            let r = c.text_range();
                            if r.end > prefix.len() || prefix.is_empty() {
                                out.clusters.push(ClusterInfo { range: map(r.start)..map(r.end), x: cx, advance: a, line: line_index, rtl: c.is_rtl() });
                            }
                            cx += a;
                        }
                    }
                    let rm = run.metrics();
                    let vm = if vertical { Some(VMetrics::new(run.font(), run.normalized_coords(), run.font_size())) } else { None };
                    let class = gr.style().brush.1;
                    // Glyphs of the run's own characters (not substituted by `vert`).
                    let plain: Vec<u32> = match (&vm, class) {
                        (Some(v), 2) => ptext.get(run.text_range()).unwrap_or("").chars().filter_map(|c| v.cmap(c)).collect(),
                        _ => Vec::new(),
                    };
                    let mut pen = gr.offset();
                    let mut cursor = cursors.iter_mut().find(|c| c.0 == run.index());
                    for g in gr.glyphs() {
                        // Kerning of the clusters before this glyph's cluster.
                        if let Some(c) = cursor.as_deref_mut() {
                            if let Some(&slot) = c.1.get(c.2) {
                                while c.3 < slot {
                                    extra += kern_px.get(c.3).copied().unwrap_or(0.0);
                                    c.3 += 1;
                                }
                            }
                            c.2 += 1;
                        }
                        out.glyphs.push(PlacedGlyph {
                            face,
                            id: g.id,
                            x: dx + pen + g.x + extra,
                            y: baseline + g.y,
                            style: si,
                            orient: GlyphOrient::Horizontal,
                        });
                        if let Some(v) = &vm {
                            let upright = class == 1 || (class == 2 && !plain.contains(&g.id));
                            vinfo.push(VGlyph {
                                upright,
                                advance: g.advance * hs,
                                origin: if upright { v.origin(g.id) } else { 0.0 },
                                // Rotated text is centred on the column: its baseline sits
                                // (ascent − descent) / 2 from the centre line.
                                centre: (rm.ascent - rm.descent) * 0.5,
                            });
                        }
                        extra += g.advance * (hs - 1.0);
                        pen += g.advance;
                    }
                    // The run's last glyph: its remaining clusters' kerning follows it.
                    if let Some(c) = cursor
                        && c.2 >= c.1.len()
                    {
                        let end = slots.iter().rposition(|s| s.run == c.0).map_or(c.3, |p| p + 1);
                        while c.3 < end {
                            extra += kern_px.get(c.3).copied().unwrap_or(0.0);
                            c.3 += 1;
                        }
                    }
                    let run_x1 = dx + gr.offset() + gr.advance() + extra;
                    let shift = st.baseline_shift_pt * k;
                    if vertical {
                        // Underline to the right of the column, strikethrough through its centre
                        // (stored directly in text space).
                        let half = run.font_size() * 0.5;
                        let mut push = |v0: f32, v1: f32| out.decorations.push(DecorationRect { x0: -v1, y0: run_x0, x1: -v0, y1: run_x1, style: si });
                        if gr.style().underline.is_some() {
                            let v1 = baseline - half;
                            push(v1 - rm.underline_size.max(1.0), v1);
                        }
                        if gr.style().strikethrough.is_some() {
                            let sz = rm.strikethrough_size.max(1.0);
                            push(baseline - sz * 0.5, baseline + sz * 0.5);
                        }
                    } else if gr.style().underline.is_some() {
                        let y0 = baseline - rm.underline_offset - shift;
                        out.decorations.push(DecorationRect { x0: run_x0, y0, x1: run_x1, y1: y0 + rm.underline_size.max(1.0), style: si });
                    }
                    if !vertical && gr.style().strikethrough.is_some() {
                        let y0 = baseline - rm.strikethrough_offset - shift;
                        out.decorations.push(DecorationRect { x0: run_x0, y0, x1: run_x1, y1: y0 + rm.strikethrough_size.max(1.0), style: si });
                    }
                }
                if vertical {
                    extra -= squeeze_punctuation(text, &mut out.clusters[c0..], &mut out.glyphs[g0..]);
                }
                if justify_all {
                    let slack = avail.unwrap_or(0.0) - (adv + extra);
                    let n = out.clusters.len() - c0;
                    if n > 1 && slack > 0.0 {
                        let step = slack / (n - 1) as f32;
                        let starts: Vec<f32> = out.clusters[c0..].iter().map(|c| c.x).collect();
                        for (i, c) in out.clusters[c0..].iter_mut().enumerate() {
                            c.x += step * i as f32;
                        }
                        for g in &mut out.glyphs[g0..] {
                            let i = starts.iter().rposition(|&s| s <= g.x + 1e-3).unwrap_or(0);
                            g.x += step * i as f32;
                        }
                        extra += slack;
                    }
                }
                if vertical {
                    // Line space → text space (90° clockwise); upright glyphs centred on the column.
                    for (g, v) in out.glyphs[g0..].iter_mut().zip(&vinfo) {
                        let (u, cross) = (g.x, g.y);
                        if v.upright {
                            g.x = -baseline - v.advance * 0.5;
                            g.y = u + v.origin;
                            g.orient = GlyphOrient::Upright;
                        } else {
                            g.x = -(cross + v.centre);
                            g.y = u;
                            g.orient = GlyphOrient::Rotated;
                        }
                    }
                }
                let x0 = dx + m.offset;
                out.lines.push(LineInfo {
                    range: map(lr.start)..map(lr.end).min(content_end),
                    baseline,
                    x0,
                    x1: x0 + adv + extra,
                    ascent,
                    descent,
                    paragraph: pi,
                });
            }
            pending_space += ps.space_after_pt * k;
        }
        out
    }
}

/// Byte ranges of paragraphs (each including its `\r`, `\n` or `\r\n` terminator). Text ending
/// with a break yields a final empty paragraph, like Photoshop's trailing empty line.
pub fn split_paragraphs(text: &str) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut v = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                let end = if b.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
                v.push(start..end);
                start = end;
                i = end;
            }
            b'\n' => {
                v.push(start..i + 1);
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    v.push(start..b.len());
    v
}

fn strip_break(text: &str, r: &Range<usize>) -> usize {
    let s = &text[r.clone()];
    r.start + s.trim_end_matches(['\r', '\n']).len()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace(['\\', '"'], ""))
}

/// Placement data of one vertical-type glyph (before mapping to text space).
struct VGlyph {
    upright: bool,
    /// Advance along the column (px).
    advance: f32,
    /// Upright: distance from the top of the glyph's cell down to its baseline (px).
    origin: f32,
    /// Rotated: distance from the column centre line to the glyph's baseline (px).
    centre: f32,
}

/// Vertical metrics of a font instance: `VORG`, `vhea`/`vmtx`, else the ideographic em box.
struct VMetrics<'a> {
    font: Option<skrifa::FontRef<'a>>,
    coords: Vec<skrifa::instance::NormalizedCoord>,
    /// px per font unit.
    scale: f32,
    /// Em-box fallback origin (px below the cell top).
    em_origin: f32,
}

impl<'a> VMetrics<'a> {
    fn new(fd: &'a FontData, coords: &[i16], size: f32) -> Self {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::from_index(fd.data.as_ref(), fd.index).ok();
        let coords: Vec<skrifa::instance::NormalizedCoord> = coords.iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
        let (scale, em_origin) = match &font {
            Some(f) => {
                let m = f.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::new(&coords));
                let upem = f32::from(m.units_per_em).max(1.0);
                let (asc, desc) = (m.ascent, -m.descent);
                // Ideographic em box: the em centred on the font's ascent/descent span.
                let r = if asc + desc > 0.0 && asc.is_finite() && desc.is_finite() { (asc / (asc + desc)).clamp(0.0, 1.0) } else { 0.88 };
                (size / upem, size * r)
            }
            None => (0.0, size * 0.88),
        };
        Self { font, coords, scale, em_origin }
    }

    fn cmap(&self, c: char) -> Option<u32> {
        use skrifa::MetadataProvider;
        self.font.as_ref()?.charmap().map(c).map(|g| g.to_u32())
    }

    /// Distance from the top of an upright glyph's cell to its baseline (px).
    fn origin(&self, gid: u32) -> f32 {
        self.vertical_origin(gid).filter(|v| v.is_finite()).unwrap_or(self.em_origin)
    }

    fn vertical_origin(&self, gid: u32) -> Option<f32> {
        use skrifa::MetadataProvider;
        use skrifa::raw::TableProvider;
        let f = self.font.as_ref()?;
        let g = skrifa::GlyphId::new(gid);
        if let Ok(vorg) = f.vorg() {
            return Some(f32::from(vorg.vertical_origin_y(g)) * self.scale);
        }
        let tsb = f.vmtx().ok()?.side_bearing(g)?;
        // Top of the outline (font units), from the outline itself (glyf and CFF alike).
        let outline = f.outline_glyphs().get(g)?;
        let mut pen = YMax(None);
        let settings = skrifa::outline::DrawSettings::unhinted(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::new(&self.coords));
        outline.draw(settings, &mut pen).ok()?;
        Some((pen.0? + f32::from(tsb)) * self.scale)
    }
}

/// Outline pen recording the largest y.
struct YMax(Option<f32>);

impl YMax {
    fn add(&mut self, y: f32) {
        self.0 = Some(self.0.map_or(y, |m| m.max(y)));
    }
}

impl skrifa::outline::OutlinePen for YMax {
    fn move_to(&mut self, _x: f32, y: f32) {
        self.add(y);
    }
    fn line_to(&mut self, _x: f32, y: f32) {
        self.add(y);
    }
    fn quad_to(&mut self, _cx0: f32, cy0: f32, _x: f32, y: f32) {
        self.add(cy0);
        self.add(y);
    }
    fn curve_to(&mut self, _cx0: f32, cy0: f32, _cx1: f32, cy1: f32, _x: f32, y: f32) {
        self.add(cy0);
        self.add(cy1);
        self.add(y);
    }
    fn close(&mut self) {}
}

/// OpenType feature settings of a character style (CSS `font-feature-settings` items).
fn feature_list(st: &CharStyle) -> Vec<String> {
    let mut feats: Vec<String> = Vec::new();
    // Optical and manual kerning replace the font's kerning table; a character with a manual
    // kern is manually kerned whatever its mode (as in Photoshop).
    if st.kerning != Kerning::Metrics || st.kern != 0.0 {
        feats.push("\"kern\" 0".into());
    }
    if !st.ligatures {
        feats.push("\"liga\" 0".into());
        feats.push("\"clig\" 0".into());
    }
    if st.discretionary_ligatures {
        feats.push("\"dlig\" 1".into());
    }
    if st.caps == Caps::SmallCaps {
        feats.push("\"smcp\" 1".into());
    }
    for f in &st.features {
        if f.tag.len() == 4 && f.tag.is_ascii() {
            feats.push(format!("\"{}\" {}", f.tag, f.value));
        }
    }
    feats
}

fn style_props(st: &CharStyle, k: f32, fallback: &[String], idx: u32) -> Vec<StyleProperty<'static, RunBrush>> {
    let px = (st.size_pt * k).max(0.01);
    let mut fam: Vec<String> = Vec::new();
    if !st.font_family.is_empty() {
        fam.push(quote(&st.font_family));
    }
    // Serif runs fall back to a Mincho face for Japanese (craft-fonts), others to a Gothic one.
    if crate::craft_fonts::is_serif_family(&st.font_family) {
        fam.extend(crate::craft_fonts::mincho_first(fallback).iter().map(|f| quote(f)));
    } else {
        fam.extend(fallback.iter().map(|f| quote(f)));
    }
    fam.push("sans-serif".into());
    let feats = feature_list(st);
    let vars: Vec<String> = st.variations.iter().filter(|v| v.axis.len() == 4 && v.axis.is_ascii()).map(|v| format!("\"{}\" {}", v.axis, v.value)).collect();
    vec![
        StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(fam.join(", ")))),
        StyleProperty::FontSize(px),
        StyleProperty::FontWeight(FontWeight::new(st.weight.clamp(1, 1000) as f32)),
        StyleProperty::FontStyle(if st.italic { FontStyle::Italic } else { FontStyle::Normal }),
        StyleProperty::FontFeatures(FontFeatures::Source(Cow::Owned(feats.join(", ")))),
        StyleProperty::FontVariations(FontVariations::Source(Cow::Owned(vars.join(", ")))),
        StyleProperty::LetterSpacing(st.tracking / 1000.0 * px),
        StyleProperty::Underline(st.underline),
        StyleProperty::Strikethrough(st.strikethrough),
        StyleProperty::Brush(RunBrush(idx, 0)),
        StyleProperty::Locale(st.language.as_deref().and_then(|l| parley::fontique::Language::parse(l).ok())),
    ]
}

/// Closing CJK punctuation: its full-width cell has blank space after the mark.
fn is_closing(c: char) -> bool {
    matches!(c, '、' | '。' | '，' | '．' | '」' | '』' | '）' | '〕' | '】' | '〉' | '》' | '〙' | '〗' | '］' | '｝')
}

/// Opening CJK brackets: blank space before the mark.
fn is_opening(c: char) -> bool {
    matches!(c, '「' | '『' | '（' | '〔' | '【' | '〈' | '《' | '〘' | '〖' | '［' | '｛')
}

/// Basic Japanese punctuation squeeze (JIS X 4051 style) for one line in line space: between a
/// closing mark and a following opening or closing mark, the closing mark's trailing half-em
/// blank is removed; between two opening brackets, the second one's leading half-em. Clusters and
/// glyphs from the squeezed point on move back. Returns the total length removed.
pub(crate) fn squeeze_punctuation(text: &str, clusters: &mut [ClusterInfo], glyphs: &mut [PlacedGlyph]) -> f32 {
    let first = |c: &ClusterInfo| text.get(c.range.clone()).and_then(|s| s.chars().next());
    let last = |c: &ClusterInfo| text.get(c.range.clone()).and_then(|s| s.chars().next_back());
    // Shift to apply from each cluster on.
    let mut shifts = vec![0.0f32; clusters.len()];
    let mut total = 0.0f32;
    for i in 1..clusters.len() {
        let (Some(prev), Some(cur)) = (clusters.get(i - 1), clusters.get(i)) else { continue };
        if prev.rtl || cur.rtl {
            continue;
        }
        let (Some(a), Some(b)) = (last(prev), first(cur)) else { continue };
        let cut = if is_closing(a) && (is_opening(b) || is_closing(b)) {
            prev.advance * 0.5
        } else if is_opening(a) && is_opening(b) {
            cur.advance * 0.5
        } else {
            0.0
        };
        if cut.is_finite() && cut > 0.0 {
            total += cut;
        }
        if let Some(s) = shifts.get_mut(i) {
            *s = total;
        }
    }
    if total <= 0.0 {
        return 0.0;
    }
    let starts: Vec<f32> = clusters.iter().map(|c| c.x).collect();
    for (c, s) in clusters.iter_mut().zip(&shifts) {
        c.x -= s;
    }
    for g in glyphs.iter_mut() {
        let i = starts.iter().rposition(|&s| s <= g.x + 1e-3).unwrap_or(0);
        g.x -= shifts.get(i).copied().unwrap_or(0.0);
    }
    total
}

/// Whether to embolden a face synthetically. Font matching (fontique) asks for it whenever the
/// requested weight is above the face's, so Regular (400) from a family with only W3 (300) and W6
/// (600), like Hiragino Mincho ProN, came out as a smeared bold W3. Like Photoshop and CSS, only
/// a bold request (≥ 600) on a face that isn't bold (≤ 500) is emboldened; anything else uses the
/// nearest face as it is (Faux Bold stays a separate, explicit style).
pub(crate) fn synthetic_bold(matcher_says: bool, requested: u16, face_weight: f32) -> bool {
    matcher_says && requested >= 600 && face_weight <= 500.0
}

/// Height of the lowercase ascender ('d') of the tallest run on the line.
fn first_ascent(line: &parley::Line<'_, RunBrush>) -> Option<f32> {
    use skrifa::MetadataProvider;
    let mut best: Option<f32> = None;
    for run in line.runs() {
        let fd = run.font();
        let Ok(font) = skrifa::FontRef::from_index(fd.data.as_ref(), fd.index) else {
            continue;
        };
        let coords: Vec<skrifa::instance::NormalizedCoord> = run.normalized_coords().iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
        let loc = skrifa::instance::LocationRef::new(&coords);
        let size = skrifa::instance::Size::new(run.font_size());
        let h = font.charmap().map('d').and_then(|g| font.glyph_metrics(size, loc).bounds(g)).map(|b| b.y_max).or_else(|| {
            let m = font.metrics(size, loc);
            m.cap_height.or(Some(m.ascent * 0.75))
        });
        if let Some(h) = h {
            best = Some(best.map_or(h, |b: f32| b.max(h)));
        }
    }
    best
}

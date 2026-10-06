//! File › Export › Save for Web (Legacy), File › Export › Export Preferences, the Quick Export
//! pipeline that honours them, and File › Generate › Image Assets (Photoshop's Generator).
//!
//! Save for Web optimises the flattened (optionally resized, sRGB-converted) image as GIF,
//! PNG-8, PNG-24, JPEG or WBMP, per slice, into a folder with an optional HTML table; without an
//! output it only reports the optimised size (the dialog's 2-Up/4-Up annotations). Colour
//! reduction and dithering reuse `algo::quantize`; GIF/WBMP/progressive JPEG writers live in
//! `codecs::web`.
//!
//! Image Assets implements the naming grammar of Adobe's public Generator documentation: a
//! layer or group named `foo.png`, `200% foo@2x.png, 48x48 icons/foo.png8`, `photo.jpg80%`,
//! `?x100 thumb.gif` is exported into `<document>-assets/` whenever the document is saved while
//! the feature is on; a `default` layer (`default 50% low/ + 200% @2x`) adds variants.

use photocraft_algo::quantize::{self, Dither, Forced, PaletteKind};
use photocraft_doc::slices::{self, ResolvedSlice, SliceKind};
use photocraft_doc::{DocId, Document, LayerContent, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::file_cmds::{join, native_doc, stem, write_file};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

// ---------- settings ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebFormat {
    Gif,
    Png8,
    Png24,
    Jpeg,
    Wbmp,
}

impl WebFormat {
    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "gif" => WebFormat::Gif,
            "png8" | "png-8" => WebFormat::Png8,
            "png24" | "png-24" | "png" | "png32" => WebFormat::Png24,
            "jpeg" | "jpg" => WebFormat::Jpeg,
            "wbmp" => WebFormat::Wbmp,
            _ => return None,
        })
    }
    pub fn ext(self) -> &'static str {
        match self {
            WebFormat::Gif => "gif",
            WebFormat::Png8 | WebFormat::Png24 => "png",
            WebFormat::Jpeg => "jpg",
            WebFormat::Wbmp => "wbmp",
        }
    }
    pub fn indexed(self) -> bool {
        matches!(self, WebFormat::Gif | WebFormat::Png8)
    }
}

/// Which metadata Save for Web embeds (JPEG only, like Photoshop's legacy exporter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebMetadata {
    None,
    Copyright,
    CopyrightAndContact,
    All,
}

/// One optimisation preset (the right-hand side of the Save for Web dialog).
#[derive(Clone, Debug, PartialEq)]
pub struct WebSettings {
    pub format: WebFormat,
    pub palette: PaletteKind,
    pub colors: usize,
    pub dither: Dither,
    /// 0..=1.
    pub dither_amount: f32,
    pub transparency: bool,
    /// Edge / background colour (0..=1 RGB); None = no matte (hard alpha threshold).
    pub matte: Option<[f32; 3]>,
    pub interlaced: bool,
    /// Web Snap: palette entries within this distance (0..=1 of the channel range) snap to the
    /// web-safe cube.
    pub web_snap: f32,
    /// JPEG quality 0..=100.
    pub quality: u8,
    pub progressive: bool,
    pub optimized: bool,
    pub embed_icc: bool,
    pub metadata: WebMetadata,
    pub convert_to_srgb: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        WebSettings {
            format: WebFormat::Png24,
            palette: PaletteKind::Selective,
            colors: 256,
            dither: Dither::Diffusion,
            dither_amount: 0.88,
            transparency: true,
            matte: Some([1.0, 1.0, 1.0]),
            interlaced: false,
            web_snap: 0.0,
            quality: 60,
            progressive: false,
            optimized: true,
            embed_icc: false,
            metadata: WebMetadata::Copyright,
            convert_to_srgb: true,
        }
    }
}

fn hex3(s: &str) -> Option<[f32; 3]> {
    let s = s.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| f32::from(v) / 255.0);
    (s.len() == 6).then_some(())?;
    Some([b(0)?, b(2)?, b(4)?])
}

impl WebSettings {
    /// Settings from command parameters (missing keys keep the defaults; Photoshop's preset
    /// names such as "GIF 128 Dithered" or "JPEG High" are accepted as `"preset"`).
    pub fn from_params(p: &Value, cmd: &str) -> Result<Self> {
        let mut st = WebSettings::default();
        if let Some(preset) = p.get("preset").and_then(Value::as_str) {
            st = preset_settings(preset).ok_or_else(|| bad(cmd, format!("unknown preset `{preset}`")))?;
        }
        let s = |k: &str| p.get(k).and_then(Value::as_str);
        if let Some(f) = s("format") {
            st.format = WebFormat::from_id(f).ok_or_else(|| bad(cmd, format!("unknown format `{f}` (gif|png8|png24|jpeg|wbmp)")))?;
        }
        if let Some(k) = s("palette") {
            st.palette = match k {
                "restrictive" | "web" => PaletteKind::Web,
                k => PaletteKind::from_id(k).ok_or_else(|| {
                    bad(cmd, format!("unknown palette `{k}` (perceptual|selective|adaptive|restrictive|exact|systemMac|systemWindows|uniform)"))
                })?,
            };
        }
        if let Some(n) = crate::commands::int(p, "colors").filter(|v| *v > 0).map(|v| v as u64) {
            st.colors = (n as usize).clamp(2, 256);
        }
        if let Some(d) = s("dither") {
            st.dither = match d {
                "none" | "noDither" => Dither::None,
                "diffusion" => Dither::Diffusion,
                "pattern" => Dither::Pattern,
                "noise" => Dither::Noise,
                d => return Err(bad(cmd, format!("unknown dither `{d}` (none|diffusion|pattern|noise)"))),
            };
        }
        if let Some(a) = p.get("ditherAmount").and_then(Value::as_f64) {
            st.dither_amount = (a as f32 / 100.0).clamp(0.0, 1.0);
        }
        if let Some(b) = p.get("transparency").and_then(Value::as_bool) {
            st.transparency = b;
        }
        match s("matte") {
            Some("none") => st.matte = None,
            Some(c) => st.matte = Some(hex3(c).ok_or_else(|| bad(cmd, "matte is \"none\" or \"#rrggbb\""))?),
            None => {}
        }
        for (k, slot) in [
            ("interlaced", &mut st.interlaced),
            ("progressive", &mut st.progressive),
            ("optimized", &mut st.optimized),
            ("embedIcc", &mut st.embed_icc),
            ("convertToSrgb", &mut st.convert_to_srgb),
        ] {
            if let Some(b) = p.get(k).and_then(Value::as_bool) {
                *slot = b;
            }
        }
        if let Some(w) = p.get("webSnap").and_then(Value::as_f64) {
            st.web_snap = (w as f32 / 100.0).clamp(0.0, 1.0);
        }
        if let Some(q) = p.get("quality").and_then(Value::as_f64) {
            st.quality = q.clamp(0.0, 100.0).round() as u8;
        }
        if let Some(m) = s("metadata") {
            st.metadata = match m {
                "none" => WebMetadata::None,
                "copyright" => WebMetadata::Copyright,
                "copyrightAndContact" | "contact" => WebMetadata::CopyrightAndContact,
                "all" | "allExceptCameraInfo" => WebMetadata::All,
                m => return Err(bad(cmd, format!("unknown metadata `{m}` (none|copyright|copyrightAndContact|all)"))),
            };
        }
        Ok(st)
    }
}

impl WebSettings {
    /// The settings as command parameters (the inverse of [`WebSettings::from_params`]).
    pub fn to_params(&self) -> Value {
        let format = match self.format {
            WebFormat::Gif => "gif",
            WebFormat::Png8 => "png8",
            WebFormat::Png24 => "png24",
            WebFormat::Jpeg => "jpeg",
            WebFormat::Wbmp => "wbmp",
        };
        let palette = match self.palette {
            PaletteKind::Exact => "exact",
            PaletteKind::SystemMac => "systemMac",
            PaletteKind::SystemWindows => "systemWindows",
            PaletteKind::Web => "restrictive",
            PaletteKind::Uniform => "uniform",
            PaletteKind::Perceptual => "perceptual",
            PaletteKind::Selective => "selective",
            PaletteKind::Adaptive => "adaptive",
        };
        let dither = match self.dither {
            Dither::None => "none",
            Dither::Diffusion => "diffusion",
            Dither::Pattern => "pattern",
            Dither::Noise => "noise",
        };
        let metadata = match self.metadata {
            WebMetadata::None => "none",
            WebMetadata::Copyright => "copyright",
            WebMetadata::CopyrightAndContact => "copyrightAndContact",
            WebMetadata::All => "all",
        };
        let matte = self.matte.map_or("none".to_string(), |c| format!("#{:02x}{:02x}{:02x}", to8(c[0]), to8(c[1]), to8(c[2])));
        json!({"format": format, "palette": palette, "colors": self.colors, "dither": dither, "ditherAmount": (self.dither_amount * 100.0).round(), "transparency": self.transparency, "matte": matte, "interlaced": self.interlaced, "webSnap": (self.web_snap * 100.0).round(), "quality": self.quality, "progressive": self.progressive, "optimized": self.optimized, "embedIcc": self.embed_icc, "metadata": metadata, "convertToSrgb": self.convert_to_srgb})
    }
}

/// Save for Web's named presets.
pub const PRESETS: [&str; 12] = [
    "GIF 128 Dithered",
    "GIF 128 No Dither",
    "GIF 32 Dithered",
    "GIF 32 No Dither",
    "GIF 64 Dithered",
    "GIF 64 No Dither",
    "GIF Restrictive",
    "JPEG High",
    "JPEG Low",
    "JPEG Medium",
    "PNG-24",
    "PNG-8 128 Dithered",
];

pub fn preset_settings(name: &str) -> Option<WebSettings> {
    let d = WebSettings::default();
    let gif = |colors: usize, dither: bool| WebSettings {
        format: WebFormat::Gif,
        colors,
        dither: if dither { Dither::Diffusion } else { Dither::None },
        ..WebSettings::default()
    };
    Some(match name {
        "GIF 128 Dithered" => gif(128, true),
        "GIF 128 No Dither" => gif(128, false),
        "GIF 32 Dithered" => gif(32, true),
        "GIF 32 No Dither" => gif(32, false),
        "GIF 64 Dithered" => gif(64, true),
        "GIF 64 No Dither" => gif(64, false),
        "GIF Restrictive" => WebSettings { palette: PaletteKind::Web, ..gif(256, true) },
        "JPEG High" => WebSettings { format: WebFormat::Jpeg, quality: 60, ..d },
        "JPEG Low" => WebSettings { format: WebFormat::Jpeg, quality: 10, ..d },
        "JPEG Medium" => WebSettings { format: WebFormat::Jpeg, quality: 30, ..d },
        "JPEG Maximum" => WebSettings { format: WebFormat::Jpeg, quality: 100, ..d },
        "JPEG Very High" => WebSettings { format: WebFormat::Jpeg, quality: 80, ..d },
        "PNG-24" => WebSettings { format: WebFormat::Png24, ..d },
        "PNG-8 128 Dithered" => WebSettings { format: WebFormat::Png8, colors: 128, ..d },
        _ => return None,
    })
}

// ---------- the image ----------

/// The document as Save for Web sees it: RGB (converted to sRGB unless told otherwise), resized
/// by `"width"`/`"height"`/`"percent"`, flattened. Returns the document and the scale factors.
pub fn web_document(doc: &Document, p: &Value, st: &WebSettings) -> Result<(Document, f64, f64)> {
    let mut tmp = Session::new();
    tmp.add_document(doc.clone(), None);
    let mode = doc.mode;
    if mode != photocraft_color::ColorMode::Rgb {
        tmp.execute("image.mode.rgb", json!({}))?;
    }
    if st.convert_to_srgb && doc.icc_profile.is_some() {
        // Documents without a profile are treated as sRGB already.
        tmp.execute("edit.convertToProfile", json!({"profile": "srgb", "intent": "perceptual"}))?;
    }
    let (w0, h0) = (f64::from(doc.size.width), f64::from(doc.size.height));
    let pct = p.get("percent").and_then(Value::as_f64).filter(|v| *v > 0.0);
    let w = p.get("width").and_then(Value::as_f64).filter(|v| *v > 0.0);
    let h = p.get("height").and_then(Value::as_f64).filter(|v| *v > 0.0);
    let (nw, nh) = match (pct, w, h) {
        (Some(pc), _, _) => (w0 * pc / 100.0, h0 * pc / 100.0),
        (_, Some(w), Some(h)) => (w, h),
        (_, Some(w), None) => (w, h0 * w / w0),
        (_, None, Some(h)) => (w0 * h / h0, h),
        _ => (w0, h0),
    };
    let (nw, nh) = (nw.round().max(1.0), nh.round().max(1.0));
    if (nw - w0).abs() >= 1.0 || (nh - h0).abs() >= 1.0 {
        let resample = p.get("resample").and_then(Value::as_str).unwrap_or("bicubic");
        tmp.execute("image.imageSize", json!({"width": nw, "height": nh, "resample": resample}))?;
    }
    let out = (*tmp.active().ok_or(EngineError::NoDocument)?.doc).clone();
    Ok((out, nw / w0, nh / h0))
}

/// An optimised image.
#[derive(Clone, Debug)]
pub struct Optimized {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
    /// Colours in the palette (indexed formats).
    pub colors: Option<usize>,
    /// What the optimised file looks like, straight RGBA8 (for the dialog's previews).
    pub preview: Vec<u8>,
}

fn to8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// XMP to embed for the metadata choice.
pub fn web_xmp(doc: &Document, m: WebMetadata) -> Option<String> {
    let xmp = doc.metadata.xmp.as_deref()?;
    match m {
        WebMetadata::None => None,
        WebMetadata::All => Some(xmp.to_string()),
        WebMetadata::Copyright | WebMetadata::CopyrightAndContact => {
            let info = crate::file_cmds::read_file_info(Some(xmp));
            let mut keep = serde_json::Map::new();
            let mut keys = vec!["copyright", "copyrightStatus", "copyrightUrl"];
            if m == WebMetadata::CopyrightAndContact {
                keys.extend(["author", "authorTitle"]);
            }
            for k in keys {
                if let Some(v) = info.get(k).filter(|v| v.as_str().is_some_and(|s| !s.is_empty()) || v.is_array()) {
                    keep.insert(k.into(), v.clone());
                }
            }
            (!keep.is_empty()).then(|| crate::file_cmds::write_file_info(None, &Value::Object(keep)))
        }
    }
}

/// Optimises `rect` of `px` (straight RGBA, `bw` wide) with `st`. `preview` also returns what
/// the file looks like (the dialog's panes); exports skip it.
#[allow(clippy::too_many_arguments)]
pub fn optimize(px: &[[f32; 4]], bw: usize, rect: Rect, st: &WebSettings, icc: Option<&[u8]>, xmp: Option<&str>, dpi: f32, preview: bool) -> Result<Optimized> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if w == 0 || h == 0 {
        return Err(other("empty slice"));
    }
    let row = |y: i32| {
        let o = y as usize * bw;
        &px[o + rect.x0 as usize..o + rect.x1 as usize]
    };
    let region = || (rect.y0..rect.y1).flat_map(row);
    let matte = st.matte.unwrap_or([1.0, 1.0, 1.0]);
    let over = |p: &[f32; 4]| -> [f32; 4] {
        [p[0] * p[3] + matte[0] * (1.0 - p[3]), p[1] * p[3] + matte[1] * (1.0 - p[3]), p[2] * p[3] + matte[2] * (1.0 - p[3]), 1.0]
    };
    let (ww, hh) = (w as u32, h as u32);
    let e = |e: photocraft_codecs::CodecError| other(e);
    let mut look: Vec<u8> = Vec::new();
    let (bytes, colors) = match st.format {
        WebFormat::Png24 => {
            let alpha = st.transparency && region().any(|p| p[3] < 1.0);
            let k = if alpha { 4 } else { 3 };
            let mut data = Vec::with_capacity(w * h * k);
            for p in region() {
                let q = if alpha { *p } else { over(p) };
                data.extend_from_slice(&[to8(q[0]), to8(q[1]), to8(q[2]), to8(q[3])][..k]);
            }
            if preview {
                look = if alpha { data.clone() } else { data.as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect() };
            }
            let layout = if alpha { photocraft_codecs::ChannelLayout::Rgba } else { photocraft_codecs::ChannelLayout::Rgb };
            let meta = photocraft_codecs::Metadata { dpi: Some((dpi, dpi)), xmp: xmp.map(str::to_string), ..Default::default() };
            let img = photocraft_codecs::Image::from_u8(ww, hh, layout, data).map_err(e)?.with_icc(icc.map(<[u8]>::to_vec)).with_meta(meta);
            let opts = photocraft_codecs::EncodeOptions {
                png_interlaced: st.interlaced,
                embed_icc: icc.is_some(),
                embed_metadata: xmp.is_some(),
                ..Default::default()
            };
            (photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &opts).map_err(e)?, None)
        }
        WebFormat::Jpeg => {
            let mut rgb = Vec::with_capacity(w * h * 3);
            for p in region() {
                let q = over(p);
                rgb.extend_from_slice(&[to8(q[0]), to8(q[1]), to8(q[2])]);
            }
            // Save for Web's 0–100 quality → encoder 1–100.
            let q = st.quality.max(1);
            let bytes = photocraft_codecs::web::encode_jpeg_rgb8(ww, hh, &rgb, q, st.progressive, st.optimized, icc.filter(|_| st.embed_icc), Some(dpi), xmp)
                .map_err(e)?;
            if preview {
                // The preview shows the compression artefacts.
                look = match photocraft_codecs::decode(&bytes) {
                    Ok(img) => img.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8).data().to_vec(),
                    Err(_) => rgb.as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
                };
            }
            (bytes, None)
        }
        WebFormat::Wbmp => {
            let mut gray: Vec<f32> = region()
                .map(|p| {
                    let q = over(p);
                    0.299 * q[0] + 0.587 * q[1] + 0.114 * q[2]
                })
                .collect();
            let method = match st.dither {
                Dither::None => quantize::BitmapMethod::Threshold,
                Dither::Pattern => quantize::BitmapMethod::Pattern,
                _ => quantize::BitmapMethod::Diffusion,
            };
            quantize::to_bitmap(&mut gray, w, method);
            let white: Vec<bool> = gray.iter().map(|v| *v > 0.5).collect();
            if preview {
                look = white.iter().flat_map(|b| if *b { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
            }
            (photocraft_codecs::web::encode_wbmp(ww, hh, &white).map_err(e)?, Some(2))
        }
        WebFormat::Gif | WebFormat::Png8 => {
            let transparent = st.transparency && region().any(|p| p[3] < 0.5);
            // Partially transparent edges blend with the matte; with transparency off everything does.
            let mut work: Vec<[f32; 4]> = region()
                .map(|p| {
                    if st.transparency && p[3] < 0.5 {
                        [0.0, 0.0, 0.0, 0.0]
                    } else if st.transparency && st.matte.is_none() {
                        [p[0], p[1], p[2], 1.0]
                    } else {
                        over(p)
                    }
                })
                .collect();
            let room = if transparent { st.colors.saturating_sub(1).max(2) } else { st.colors };
            // Large images: build adaptive palettes from an even ~1 MP sample (Exact needs all).
            let n_opaque = work.iter().filter(|p| p[3] >= 0.5).count();
            let step = if n_opaque > 1 << 20 && st.palette != PaletteKind::Exact { n_opaque / (1 << 20) + 1 } else { 1 };
            let mut src: Vec<[f32; 4]> = work.iter().filter(|p| p[3] >= 0.5).step_by(step).copied().collect();
            if src.is_empty() {
                src = work.clone();
            }
            let mut pal = quantize::build_palette(&src, st.palette, room, Forced::None).map_err(other)?;
            drop(src);
            if st.web_snap > 0.0 {
                let tol = (st.web_snap * 255.0) as u8;
                for c in &mut pal {
                    let snap = c.map(|x| ((f32::from(x) / 51.0).round() * 51.0) as u8);
                    if c.iter().zip(snap).all(|(a, b)| a.abs_diff(b) <= tol) {
                        *c = snap;
                    }
                }
            }
            if transparent {
                pal.truncate(255);
            }
            let t = transparent.then(|| {
                pal.push(pal[0]);
                pal.len() - 1
            });
            let idx = quantize_rows(&mut work, w, &pal, st.dither, st.dither_amount, t);
            if preview {
                look = idx
                    .iter()
                    .flat_map(|k| {
                        let c = pal[*k as usize];
                        [c[0], c[1], c[2], if Some(*k as usize) == t { 0 } else { 255 }]
                    })
                    .collect();
            }
            let tt = t.map(|v| v as u8);
            let bytes = if st.format == WebFormat::Gif {
                photocraft_codecs::web::encode_gif_indexed(ww, hh, &idx, &pal, tt, st.interlaced).map_err(e)?
            } else {
                photocraft_codecs::encode_png_indexed(ww, hh, &idx, &pal, tt).map_err(e)?
            };
            (bytes, Some(pal.len()))
        }
    };
    Ok(Optimized { bytes, ext: st.format.ext(), width: ww, height: hh, colors, preview: look })
}

/// Nearest palette entry through a lazily filled 6-bit-per-channel inverse colour map: each
/// 4×4×4 cell is resolved once (by its centre), so large images cost a table lookup per pixel
/// instead of a palette scan. `t` (the transparent index) is never returned.
struct InverseMap<'a> {
    pal: &'a [[u8; 3]],
    t: Option<usize>,
    lut: Vec<u16>,
}

impl<'a> InverseMap<'a> {
    fn new(pal: &'a [[u8; 3]], t: Option<usize>) -> Self {
        InverseMap { pal, t, lut: vec![u16::MAX; 1 << 18] }
    }
    #[inline]
    fn get(&mut self, c: [f32; 3]) -> usize {
        let q = c.map(|v| ((v.clamp(0.0, 1.0) * 255.0).round() as usize) >> 2);
        let key = q[0] << 12 | q[1] << 6 | q[2];
        let v = self.lut[key];
        if v != u16::MAX {
            return usize::from(v);
        }
        let center = q.map(|v| (v * 4 + 2) as f32);
        let mut best = (f32::MAX, 0usize);
        for (i, e) in self.pal.iter().enumerate() {
            if Some(i) == self.t {
                continue;
            }
            let d: f32 = (0..3).map(|k| (center[k] - f32::from(e[k])).powi(2)).sum();
            if d < best.0 {
                best = (d, i);
            }
        }
        self.lut[key] = best.1 as u16;
        best.1
    }
}

fn hash01(x: usize, y: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ 0x2545_f491;
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

/// Palette mapping with dithering (Floyd–Steinberg diffusion, 8×8 Bayer pattern, noise), like
/// `algo::quantize::quantize` but through [`InverseMap`]. `y0` keeps patterns continuous across
/// bands. Pixels with alpha < ½ take index `t`.
fn quantize_band(px: &mut [[f32; 4]], w: usize, y0: usize, pal: &[[u8; 3]], dither: Dither, amount: f32, t: Option<usize>) -> Vec<u8> {
    let h = px.len() / w.max(1);
    let mut map = InverseMap::new(pal, t);
    let mut idx = vec![0u8; px.len()];
    let opaque_n = pal.len() - usize::from(t.is_some());
    let spread = (1.0 / (opaque_n as f32).cbrt().max(1.0)).min(0.5) * amount.clamp(0.0, 1.0);
    let a = amount.clamp(0.0, 1.0);
    let mut cur = vec![[0.0f32; 3]; if dither == Dither::Diffusion { w + 2 } else { 0 }];
    let mut next = cur.clone();
    for y in 0..h {
        if dither == Dither::Diffusion {
            std::mem::swap(&mut cur, &mut next);
            next.iter_mut().for_each(|e| *e = [0.0; 3]);
        }
        for x in 0..w {
            let i = y * w + x;
            let p = px[i];
            if p[3] < 0.5
                && let Some(t) = t
            {
                idx[i] = t as u8;
                continue;
            }
            let mut c = [p[0], p[1], p[2]];
            match dither {
                Dither::None => {}
                Dither::Diffusion => (0..3).for_each(|k| c[k] += cur[x + 1][k]),
                Dither::Pattern => {
                    let d = quantize::bayer8(x, y0 + y) * spread;
                    c = c.map(|v| v + d);
                }
                Dither::Noise => {
                    let d = (hash01(x, y0 + y) - 0.5) * spread;
                    c = c.map(|v| v + d);
                }
            }
            let c = c.map(|v| v.clamp(0.0, 1.0));
            let k = map.get(c);
            idx[i] = k as u8;
            if dither == Dither::Diffusion {
                let e = pal[k].map(|v| f32::from(v) / 255.0);
                let d: [f32; 3] = std::array::from_fn(|q| (c[q] - e[q]) * a);
                for q in 0..3 {
                    cur[x + 2][q] += d[q] * 7.0 / 16.0;
                    next[x][q] += d[q] * 3.0 / 16.0;
                    next[x + 1][q] += d[q] * 5.0 / 16.0;
                    next[x + 2][q] += d[q] / 16.0;
                }
            }
        }
    }
    idx
}

/// [`quantize_band`] over the whole image, in parallel bands of 64 rows when the dither is
/// position-independent. Error diffusion carries error down the image, so it stays sequential.
fn quantize_rows(px: &mut [[f32; 4]], w: usize, pal: &[[u8; 3]], dither: Dither, amount: f32, t: Option<usize>) -> Vec<u8> {
    let h = px.len() / w.max(1);
    if dither == Dither::Diffusion || h < 128 || cfg!(target_arch = "wasm32") {
        return quantize_band(px, w, 0, pal, dither, amount, t);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let band = w * 64;
        let mut idx = vec![0u8; px.len()];
        px.par_chunks_mut(band)
            .zip(idx.par_chunks_mut(band))
            .enumerate()
            .for_each(|(i, (p, out))| out.copy_from_slice(&quantize_band(p, w, i * 64, pal, dither, amount, t)));
        idx
    }
    // Single-threaded on wasm (the early return above always takes this path there).
    #[cfg(target_arch = "wasm32")]
    quantize_band(px, w, 0, pal, dither, amount, t)
}

/// A file-name-safe slice name.
fn slice_file(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' }).collect();
    if s.is_empty() { "slice".into() } else { s }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn scale_rect(r: Rect, sx: f64, sy: f64) -> Rect {
    Rect::new(
        (f64::from(r.x0) * sx).round() as i32,
        (f64::from(r.y0) * sy).round() as i32,
        (f64::from(r.x1) * sx).round() as i32,
        (f64::from(r.y1) * sy).round() as i32,
    )
}

/// The HTML table for `cells` (rect, td body) over a `w`×`h` image: one column per distinct x
/// edge, one row per y edge, colspan/rowspan per slice, and a spacer row fixing column widths.
fn html_table(title: &str, w: u32, h: u32, cells: &[(Rect, String, String)], spacer: Option<&str>) -> String {
    let mut xs: Vec<i32> = cells.iter().flat_map(|(r, _, _)| [r.x0, r.x1]).collect();
    let mut ys: Vec<i32> = cells.iter().flat_map(|(r, _, _)| [r.y0, r.y1]).collect();
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let col = |x: i32| xs.iter().position(|v| *v == x).unwrap_or(0);
    let row = |y: i32| ys.iter().position(|v| *v == y).unwrap_or(0);
    let (nc, nr) = (xs.len().saturating_sub(1), ys.len().saturating_sub(1));
    let mut taken = vec![false; nc * nr];
    let mut by_row: Vec<Vec<(usize, String)>> = vec![Vec::new(); nr];
    for (r, attrs, body) in cells {
        let (c0, c1, r0, r1) = (col(r.x0), col(r.x1), row(r.y0), row(r.y1));
        if (r0..r1).any(|j| (c0..c1).any(|i| taken[j * nc + i])) {
            continue; // overlapped by an earlier slice
        }
        for j in r0..r1 {
            for i in c0..c1 {
                taken[j * nc + i] = true;
            }
        }
        let mut td = String::from("\t\t<td");
        if c1 - c0 > 1 {
            td.push_str(&format!(" colspan=\"{}\"", c1 - c0));
        }
        if r1 - r0 > 1 {
            td.push_str(&format!(" rowspan=\"{}\"", r1 - r0));
        }
        td.push_str(attrs);
        td.push('>');
        td.push_str(body);
        td.push_str("</td>\n");
        by_row[r0].push((c0, td));
    }
    let mut s = format!(
        "<html>\n<head>\n<title>{t}</title>\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\n</head>\n<body bgcolor=\"#FFFFFF\" leftmargin=\"0\" topmargin=\"0\" marginwidth=\"0\" marginheight=\"0\">\n<!-- Save for Web Slices ({t}) -->\n<table id=\"Table_01\" width=\"{w}\" height=\"{h}\" border=\"0\" cellpadding=\"0\" cellspacing=\"0\">\n",
        t = html_escape(title)
    );
    for (j, mut tds) in by_row.into_iter().enumerate() {
        tds.sort_by_key(|(c, _)| *c);
        s.push_str("\t<tr>\n");
        for (_, td) in tds {
            s.push_str(&td);
        }
        if let Some(sp) = spacer {
            s.push_str(&format!("\t\t<td>\n\t\t\t<img src=\"{sp}\" width=\"1\" height=\"{}\" alt=\"\"></td>\n", ys[j + 1] - ys[j]));
        }
        s.push_str("\t</tr>\n");
    }
    if let Some(sp) = spacer {
        s.push_str("\t<tr>\n");
        for i in 0..nc {
            s.push_str(&format!("\t\t<td>\n\t\t\t<img src=\"{sp}\" width=\"{}\" height=\"1\" alt=\"\"></td>\n", xs[i + 1] - xs[i]));
        }
        s.push_str("\t\t<td></td>\n\t</tr>\n");
    }
    s.push_str("</table>\n<!-- End Save for Web Slices -->\n</body>\n</html>\n");
    s
}

/// File › Export › Save for Web (Legacy).
fn save_for_web(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.saveForWebLegacy";
    let st = WebSettings::from_params(p, cmd)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let (wdoc, sx, sy) = web_document(&doc, p, &st)?;
    let buf = photocraft_compose::flatten(&wdoc);
    let bw = wdoc.size.width as usize;
    let icc = wdoc.icc_profile.as_deref().map(|v| v.as_slice());
    let xmp = web_xmp(&doc, st.metadata);
    let dpi = wdoc.resolution_dpi;
    let all = slices::resolve(&doc);
    let pick = p.get("slices").and_then(Value::as_str).unwrap_or("all");
    let numbers: Option<Vec<u64>> = p.get("numbers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
    let chosen: Vec<&ResolvedSlice> = all
        .iter()
        .filter(|r| match (&numbers, pick) {
            (Some(n), _) => n.contains(&(r.number as u64)),
            (None, "user") => r.origin != photocraft_doc::SliceOrigin::Auto,
            _ => true,
        })
        .collect();
    let single = p.get("path").and_then(Value::as_str).filter(|v| !v.is_empty());
    let dir = p.get("dir").and_then(Value::as_str).filter(|v| !v.is_empty());
    s.file_menu.last_web = Some(p.clone());
    // Estimate only (the dialog's annotations): the whole image, or each chosen slice.
    if single.is_none() && dir.is_none() {
        let o = optimize(&buf.px, bw, wdoc.bounds(), &st, icc, xmp.as_deref(), dpi, false)?;
        return Ok(json!({"format": o.ext, "bytes": o.bytes.len(), "width": o.width, "height": o.height, "colors": o.colors}));
    }
    if let Some(path) = single.filter(|_| dir.is_none() && (all.len() == 1 || p.get("slices").is_none())) {
        let o = optimize(&buf.px, bw, wdoc.bounds(), &st, icc, xmp.as_deref(), dpi, false)?;
        write_file(path, &o.bytes)?;
        crate::automate_cmds::fire_event(s, "export");
        return Ok(json!({"files": [path], "bytes": o.bytes.len(), "width": o.width, "height": o.height, "colors": o.colors}));
    }
    let dir =
        dir.map(str::to_string).or_else(|| single.and_then(|f| std::path::Path::new(f).parent().map(|d| d.to_string_lossy().into_owned()))).unwrap_or_default();
    let html = p.get("html").and_then(Value::as_bool).unwrap_or(false);
    let images = p.get("imagesFolder").and_then(Value::as_str).unwrap_or("images").to_string();
    let base = single.map(stem).unwrap_or_else(|| slices::base_name(&doc));
    let mut files = Vec::new();
    let mut cells: Vec<(Rect, String, String)> = Vec::new();
    let mut total = 0usize;
    for r in &chosen {
        let rect = scale_rect(r.rect, sx, sy).intersect(&wdoc.bounds());
        if rect.is_empty() {
            continue;
        }
        let stored = r.id.and_then(|id| doc.slices.get(id));
        if r.kind == SliceKind::NoImage {
            let text = stored.map(|s| if s.cell_text_is_html { s.cell_text.clone() } else { html_escape(&s.cell_text) }).unwrap_or_default();
            let bg = stored.and_then(|s| s.background).map(|c| format!(" bgcolor=\"#{:02X}{:02X}{:02X}\"", c[1], c[2], c[3])).unwrap_or_default();
            cells.push((rect, bg, text));
            continue;
        }
        let o = optimize(&buf.px, bw, rect, &st, icc, xmp.as_deref(), dpi, false)?;
        let name = format!("{}.{}", slice_file(&r.name), o.ext);
        let rel = if images.is_empty() { name.clone() } else { format!("{images}/{name}") };
        let out = join(&dir, &rel);
        write_file(&out, &o.bytes)?;
        total += o.bytes.len();
        files.push(out);
        let alt = stored.map_or(String::new(), |s| html_escape(&s.alt));
        let img = format!("\n\t\t\t<img src=\"{}\" width=\"{}\" height=\"{}\" alt=\"{alt}\">", html_escape(&rel), rect.width(), rect.height());
        let body = match stored.filter(|s| !s.url.is_empty()) {
            Some(s) => {
                let target = if s.target.is_empty() { String::new() } else { format!(" target=\"{}\"", html_escape(&s.target)) };
                format!("\n\t\t\t<a href=\"{}\"{target}>{img}</a>", html_escape(&s.url))
            }
            None => img,
        };
        cells.push((rect, String::new(), body));
    }
    let mut html_path = None;
    if html {
        let grid = cells.len() > 1;
        let spacer = grid.then(|| if images.is_empty() { "spacer.gif".to_string() } else { format!("{images}/spacer.gif") });
        if let Some(sp) = &spacer {
            let gif = photocraft_codecs::web::encode_gif_indexed(1, 1, &[0], &[[0, 0, 0]], Some(0), false).map_err(other)?;
            write_file(&join(&dir, sp), &gif)?;
        }
        let page = html_table(&base, wdoc.size.width, wdoc.size.height, &cells, spacer.as_deref());
        let hp = join(&dir, &format!("{base}.html"));
        write_file(&hp, page.as_bytes())?;
        html_path = Some(hp);
    }
    crate::automate_cmds::fire_event(s, "export");
    Ok(json!({"files": files, "html": html_path, "bytes": total, "slices": chosen.len(), "width": wdoc.size.width, "height": wdoc.size.height}))
}

// ---------- Export Preferences / Quick Export ----------

fn export_preferences(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.exportPreferences";
    let mut values = serde_json::Map::new();
    for (k, key) in [
        ("quickExportFormat", "export.quickExportFormat"),
        ("quickExportLocation", "export.quickExportLocation"),
        ("jpegQuality", "export.jpegQuality"),
        ("metadata", "export.metadata"),
        ("convertToSrgb", "export.convertToSrgb"),
    ] {
        if let Some(v) = p.get(k) {
            values.insert(key.into(), v.clone());
        }
    }
    if !values.is_empty() {
        s.execute("prefs.set", json!({"values": values})).map_err(|e| bad(cmd, e.to_string()))?;
    }
    Ok(json!({"section": "export", "values": s.prefs().get("export").unwrap_or(Value::Null)}))
}

/// File › Export › Quick Export as <format>: the format, quality, metadata, colour space and
/// location from Export Preferences. `path` overrides the location ("ask" needs one).
fn quick_export(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.export.quickExport";
    let prefs = s.prefs().export.clone();
    let fmt = serde_json::to_value(prefs.quick_export_format).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "png".into());
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let path = match p.get("path").and_then(Value::as_str).filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None => {
            let same = serde_json::to_value(prefs.quick_export_location).ok().and_then(|v| v.as_str().map(str::to_string)).is_some_and(|v| v == "sameFolder");
            match (&d.path, same) {
                (Some(dp), true) => {
                    let dir = std::path::Path::new(dp).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                    join(&dir, &format!("{}.{fmt}", stem(dp)))
                }
                _ => return Err(bad(cmd, "missing `path` (Export Preferences › Location is \"ask\", or the document was never saved)")),
            }
        }
    };
    let metadata = serde_json::to_value(prefs.metadata).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let mut o = serde_json::Map::new();
    o.insert(
        "metadata".into(),
        json!(if metadata == "all" {
            "all"
        } else if metadata == "none" {
            "none"
        } else {
            "copyright"
        }),
    );
    o.insert("convertToSrgb".into(), json!(prefs.convert_to_srgb));
    o.insert("transparency".into(), json!(true));
    match fmt.as_str() {
        "jpg" => {
            o.insert("format".into(), json!("jpeg"));
            o.insert("quality".into(), json!(prefs.jpeg_quality.min(100)));
        }
        "gif" => {
            o.insert("format".into(), json!("gif"));
        }
        "webp" => {
            // WebP has no legacy Save for Web optimiser: write it through the regular exporter.
            let (wdoc, _, _) = web_document(&doc, &json!({}), &WebSettings { convert_to_srgb: prefs.convert_to_srgb, ..Default::default() })?;
            let warnings = crate::file_cmds::save_doc(&wdoc, &path, None)?;
            crate::automate_cmds::fire_event(s, "export");
            return Ok(json!({"path": path, "format": fmt, "warnings": warnings}));
        }
        _ => {
            o.insert("format".into(), json!("png24"));
        }
    }
    let q = Value::Object(o);
    let st = WebSettings::from_params(&q, cmd)?;
    let (wdoc, _, _) = web_document(&doc, &json!({}), &st)?;
    let buf = photocraft_compose::flatten(&wdoc);
    let xmp = web_xmp(&doc, st.metadata);
    let opt = optimize(
        &buf.px,
        wdoc.size.width as usize,
        wdoc.bounds(),
        &st,
        wdoc.icc_profile.as_deref().map(|v| v.as_slice()),
        xmp.as_deref(),
        wdoc.resolution_dpi,
        false,
    )?;
    write_file(&path, &opt.bytes)?;
    crate::automate_cmds::fire_event(s, "export");
    Ok(json!({"path": path, "format": fmt, "bytes": opt.bytes.len()}))
}

// ---------- Generate › Image Assets ----------

/// One asset requested by a layer name.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetSpec {
    /// Relative output path (folders allowed), extension included.
    pub file: String,
    /// png8 | png24 | png32 | jpg | gif | webp
    pub format: String,
    /// JPEG quality 1..=100.
    pub quality: Option<u8>,
    /// Uniform scale (1.0 = 100 %).
    pub scale: Option<f64>,
    /// Target width / height in px (`?` = keep aspect).
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// A `default` layer variant: size plus a folder (`low/`) or name suffix (`@2x`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetDefault {
    pub scale: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub folder: String,
    pub suffix: String,
}

fn unit_px(tok: &str, dpi: f64) -> Option<Option<f64>> {
    if tok == "?" {
        return Some(None);
    }
    let (num, k) = if let Some(n) = tok.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = tok.strip_suffix("in") {
        (n, dpi)
    } else if let Some(n) = tok.strip_suffix("cm") {
        (n, dpi / 2.54)
    } else if let Some(n) = tok.strip_suffix("mm") {
        (n, dpi / 25.4)
    } else {
        (tok, 1.0)
    };
    num.trim().parse::<f64>().ok().filter(|v| *v > 0.0).map(|v| Some(v * k))
}

/// `200%` or `WxH` (units px/in/cm/mm, `?` wildcard) at the start of `item`; returns the size and
/// the rest.
fn parse_size(item: &str, dpi: f64) -> (Option<f64>, Option<f64>, Option<f64>, &str) {
    let item = item.trim();
    let (first, rest) = item.split_once(char::is_whitespace).map_or((item, ""), |(a, b)| (a, b.trim()));
    if let Some(pc) = first.strip_suffix('%').and_then(|n| n.parse::<f64>().ok()).filter(|v| *v > 0.0) {
        return (Some(pc / 100.0), None, None, rest);
    }
    // "100x50", "2in x 1in", "?x100"
    if let Some((w, h)) = first.split_once(['x', 'X']).filter(|(w, h)| !w.is_empty() && !h.is_empty())
        && let (Some(w), Some(h)) = (unit_px(w, dpi), unit_px(h, dpi))
    {
        return (None, w, h, rest);
    }
    // Spaced form "2in x 1in name.png": three tokens.
    let toks: Vec<&str> = item.split_whitespace().collect();
    if toks.len() >= 4
        && toks[1].eq_ignore_ascii_case("x")
        && let (Some(w), Some(h)) = (unit_px(toks[0], dpi), unit_px(toks[2], dpi))
    {
        let pos = item.find(toks[3]).unwrap_or(item.len());
        return (None, w, h, &item[pos..]);
    }
    (None, None, None, item)
}

/// Parses a layer name into its asset specs (empty when it names no asset).
pub fn parse_asset_name(name: &str, dpi: f64) -> Vec<AssetSpec> {
    let mut out = Vec::new();
    for item in name.split([',', '+']) {
        let (scale, width, height, rest) = parse_size(item, dpi);
        let file = rest.trim();
        let Some((stem_part, ext)) = file.rsplit_once('.') else { continue };
        if stem_part.is_empty() {
            continue;
        }
        let ext_l = ext.to_ascii_lowercase();
        let (format, quality, out_ext) = if let Some(q) = ext_l.strip_prefix("jpeg").or_else(|| ext_l.strip_prefix("jpg")) {
            let q = q.trim();
            let quality = if q.is_empty() {
                None
            } else if let Some(pc) = q.strip_suffix('%') {
                pc.parse::<u32>().ok().map(|v| v.clamp(1, 100) as u8)
            } else {
                // 1–10 scale.
                q.parse::<u32>().ok().map(|v| (v.clamp(1, 10) * 10) as u8)
            };
            if !q.is_empty() && quality.is_none() {
                continue;
            }
            ("jpg".to_string(), quality, if ext_l.starts_with("jpeg") { "jpeg" } else { "jpg" })
        } else {
            match ext_l.as_str() {
                "png" | "png32" => ("png32".to_string(), None, "png"),
                "png24" => ("png24".to_string(), None, "png"),
                "png8" => ("png8".to_string(), None, "png"),
                "gif" => ("gif".to_string(), None, "gif"),
                "webp" => ("webp".to_string(), None, "webp"),
                _ => continue,
            }
        };
        out.push(AssetSpec { file: format!("{stem_part}.{out_ext}"), format, quality, scale, width, height });
    }
    out
}

/// Parses a `default …` layer name into its variants.
pub fn parse_defaults(name: &str, dpi: f64) -> Option<Vec<AssetDefault>> {
    let rest = name.trim().strip_prefix("default")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut v = Vec::new();
    for item in rest.split([',', '+']) {
        let (scale, width, height, tail) = parse_size(item, dpi);
        let tail = tail.trim();
        let (folder, suffix) = match tail.rfind('/') {
            Some(i) => (tail[..=i].to_string(), tail[i + 1..].to_string()),
            None => (String::new(), tail.to_string()),
        };
        if scale.is_none() && width.is_none() && height.is_none() && folder.is_empty() && suffix.is_empty() {
            continue;
        }
        v.push(AssetDefault { scale, width, height, folder, suffix });
    }
    Some(v)
}

fn asset_settings(spec: &AssetSpec) -> WebSettings {
    let base = WebSettings { transparency: true, matte: None, metadata: WebMetadata::None, ..WebSettings::default() };
    match spec.format.as_str() {
        "jpg" => WebSettings { format: WebFormat::Jpeg, quality: spec.quality.unwrap_or(90), matte: Some([1.0, 1.0, 1.0]), ..base },
        "png8" => WebSettings { format: WebFormat::Png8, dither: Dither::None, ..base },
        "png24" => WebSettings { format: WebFormat::Png24, transparency: false, matte: Some([1.0, 1.0, 1.0]), ..base },
        "gif" => WebSettings { format: WebFormat::Gif, dither: Dither::None, ..base },
        _ => WebSettings { format: WebFormat::Png24, ..base },
    }
}

/// Exports every asset named by `doc`'s layers into `dir`. Returns (files, errors).
pub fn generate_assets(doc: &Document, dir: &str) -> (Vec<String>, Vec<Value>) {
    let dpi = f64::from(doc.resolution_dpi.max(1.0));
    let mut defaults: Option<Vec<AssetDefault>> = None;
    let mut jobs: Vec<(LayerId, AssetSpec)> = Vec::new();
    for (_, _, l) in doc.walk() {
        if let Some(d) = parse_defaults(&l.name, dpi) {
            defaults = Some(d);
            continue;
        }
        if matches!(l.content, LayerContent::Adjustment(_)) {
            continue;
        }
        for spec in parse_asset_name(&l.name, dpi) {
            jobs.push((l.id, spec));
        }
    }
    let variants: Vec<AssetDefault> = match defaults {
        Some(d) if !d.is_empty() => d,
        _ => vec![AssetDefault::default()],
    };
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for (id, spec) in &jobs {
        for v in &variants {
            let r = (|| -> Result<String> {
                let mut one = crate::layer_menu_cmds::layer_document(doc, *id)?;
                one.metadata = Default::default();
                let (w0, h0) = (f64::from(one.size.width), f64::from(one.size.height));
                // The layer's own size spec wins; default variants scale on top of it.
                let mut k = spec.scale.unwrap_or(1.0);
                let (mut tw, mut th) = (w0 * k, h0 * k);
                match (spec.width, spec.height) {
                    (Some(w), Some(h)) => (tw, th) = (w, h),
                    (Some(w), None) => (tw, th) = (w, h0 * w / w0),
                    (None, Some(h)) => (tw, th) = (w0 * h / h0, h),
                    _ => {}
                }
                k = v.scale.unwrap_or(1.0);
                tw *= k;
                th *= k;
                match (v.width, v.height) {
                    (Some(w), Some(h)) => (tw, th) = (w, h),
                    (Some(w), None) => (tw, th) = (w, th * w / tw),
                    (None, Some(h)) => (tw, th) = (tw * h / th, h),
                    _ => {}
                }
                let st = asset_settings(spec);
                let p = json!({"width": tw.round().max(1.0), "height": th.round().max(1.0)});
                let (wdoc, _, _) = web_document(&one, &p, &WebSettings { convert_to_srgb: true, ..st.clone() })?;
                let (name_stem, ext) = spec.file.rsplit_once('.').unwrap_or((spec.file.as_str(), "png"));
                let rel = format!("{}{}{}.{ext}", v.folder, name_stem, v.suffix);
                let out = join(dir, &rel);
                if spec.format == "webp" {
                    crate::file_cmds::save_doc(&wdoc, &out, None)?;
                    return Ok(out);
                }
                let buf = photocraft_compose::flatten(&wdoc);
                let o = optimize(&buf.px, wdoc.size.width as usize, wdoc.bounds(), &st, None, None, wdoc.resolution_dpi, false)?;
                write_file(&out, &o.bytes)?;
                Ok(out)
            })();
            match r {
                Ok(f) => files.push(f),
                Err(e) => errors.push(json!({"asset": spec.file, "error": e.to_string()})),
            }
        }
    }
    (files, errors)
}

/// `<document folder>/<document name>-assets`.
pub fn assets_dir(doc_path: &str) -> String {
    let p = std::path::Path::new(doc_path);
    let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    join(&dir, &format!("{}-assets", stem(doc_path)))
}

fn image_assets(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id: DocId = d.doc.id;
    let was = s.file_menu.image_assets.contains(&id);
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(!was);
    if on {
        if !was {
            s.file_menu.image_assets.push(id);
        }
    } else {
        s.file_menu.image_assets.retain(|x| *x != id);
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let dir = p.get("dir").and_then(Value::as_str).map(str::to_string).or_else(|| d.path.as_deref().map(assets_dir));
    if !on {
        return Ok(json!({"enabled": false}));
    }
    match dir {
        Some(dir) if !cfg!(target_arch = "wasm32") => {
            let (files, errors) = generate_assets(&d.doc, &dir);
            Ok(json!({"enabled": true, "dir": dir, "files": files, "errors": errors}))
        }
        _ => Ok(json!({"enabled": true, "dir": null, "files": [], "note": "assets are written when the document is saved"})),
    }
}

/// After a save: regenerate the document's image assets when Generate › Image Assets is on.
pub fn on_saved(s: &Session, index: usize) -> Option<Value> {
    let d = s.documents().get(index)?;
    if !s.file_menu.image_assets.contains(&d.doc.id) || cfg!(target_arch = "wasm32") {
        return None;
    }
    let dir = assets_dir(d.path.as_deref()?);
    let (files, errors) = generate_assets(&d.doc, &dir);
    Some(json!({"dir": dir, "files": files, "errors": errors}))
}

pub fn specs() -> Vec<CommandSpec> {
    macro_rules! spec {
        ($id:expr, $label:expr, $menu:expr, $sc:expr, $params:expr, $enabled:expr, $run:expr) => {
            CommandSpec { id: $id, label: $label, menu: $menu, shortcut: $sc, params: $params, enabled: $enabled, journal: true, run: $run }
        };
    }
    fn has_doc(s: &Session) -> std::result::Result<(), String> {
        s.active().map(|_| ()).ok_or_else(|| "no document open".into())
    }
    vec![
        spec!(
            "file.export.saveForWebLegacy",
            "Save for Web (Legacy)…",
            &["File", "Export"],
            Some("Cmd+Alt+Shift+S"),
            r##"{"preset":"GIF 128 Dithered|JPEG High|PNG-24|…"?,"format":"gif|png8|png24|jpeg|wbmp"="png24","palette":"perceptual|selective|adaptive|restrictive|exact|systemMac|systemWindows|uniform"="selective","colors":2..256=256,"dither":"none|diffusion|pattern|noise"="diffusion","ditherAmount":0..100=88,"transparency":bool=true,"matte":"#rrggbb|none"="#ffffff","interlaced":bool=false,"webSnap":0..100=0,"quality":0..100=60,"progressive":bool=false,"optimized":bool=true,"embedIcc":bool=false,"metadata":"none|copyright|copyrightAndContact|all"="copyright","convertToSrgb":bool=true,"width"|"height"|"percent"? (image size),"resample":"bicubic|bilinear|nearest"?,"path":file? (whole image),"dir":folder? (one file per slice in images/),"html":bool=false,"slices":"all|user"="all","numbers":[n]?} → no path/dir: {bytes,width,height,colors} estimate; else {files, html, bytes}"##,
            has_doc,
            save_for_web
        ),
        spec!(
            "file.export.exportPreferences",
            "Export Preferences…",
            &["File", "Export"],
            None,
            r##"{"quickExportFormat":"png|jpg|gif|webp"?,"quickExportLocation":"ask|sameFolder"?,"jpegQuality":1..100?,"metadata":"none|copyright|all"?,"convertToSrgb":bool?} → {values}"##,
            |_| Ok(()),
            export_preferences
        ),
        spec!(
            "file.export.quickExport",
            "Quick Export",
            &[],
            None,
            r##"{"path":str? (required unless Export Preferences › Location is "sameFolder" and the document is saved)} → {path, format, bytes}"##,
            native_doc,
            quick_export
        ),
        spec!(
            "file.generate.imageAssets",
            "Image Assets",
            &["File", "Generate"],
            None,
            r##"{"on":bool? (default: toggle),"dir":folder? (default <document>-assets next to the file)} → {enabled, files, errors}; layers named like "foo.png", "200% foo@2x.png", "48x48 icons/a.png8", "photo.jpg80%" are exported, now and after each save"##,
            has_doc,
            image_assets
        ),
    ]
}

#[cfg(test)]
#[path = "web_cmds/tests.rs"]
mod tests;

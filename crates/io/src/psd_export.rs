//! [`Document`] → PSD/PSB.

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, LayerMask};
use photocraft_psd::file::{GlobalLayerMask, LayerInfoPlacement};
use photocraft_psd::layer::{BlendingRanges, ChannelData, LayerFlags, LayerInfo, LayerMask as PsdMask, MaskData, MaskParameters};
use photocraft_psd::resources::{ImageResource, ResolutionInfo, ids, version_info_resource};
use photocraft_psd::{
    BlendMode as PsdBlend, ColorMode as PsdMode, Compression, Header, ImageData, LayerRecord, PsdFile, Rect as PsdRect, SectionType, TaggedBlock, Version,
};
use photocraft_raster::Surface;

use crate::adjust_map;
use crate::blocks;
use crate::pixels::{deinterleave, encode_be, psd_depth};
use crate::psd_import::REGENERATED;

/// Options for [`document_to_psd_with`].
#[derive(Debug, Clone, Default)]
pub struct PsdExportOptions {
    /// Write PSB even when the document fits PSD limits.
    pub force_psb: bool,
}

struct Ex {
    fmt: PixelFormat,
    /// Document resolution (for generated type-layer data).
    dpi: f32,
    /// Character/paragraph styles written into every type layer's engine data.
    text_styles: photocraft_doc::TextStyles,
    mask_fmt: PixelFormat,
    cc: usize,
    cmyk: bool,
    version: Version,
    next_id: u32,
    /// PSD ids assigned to document layers (kept from `psd_id` when unique).
    layer_ids: std::collections::HashMap<photocraft_doc::LayerId, u32>,
    canvas: photocraft_geom::Rect,
    records: Vec<LayerRecord>,
    warnings: Vec<String>,
    /// Guides (artboard blocks list the guides inside each board).
    guides: photocraft_doc::Guides,
    /// Layer comps to regenerate into each layer's `cmls`; None = preserved data still matches.
    comps: Option<(Vec<photocraft_doc::LayerComp>, Option<photocraft_doc::LayerComp>)>,
    /// Smart objects: embedded files and filter caches for the global blocks.
    smart: SmartOut,
}

/// How deep embedded documents are exported inside each other before a smart object is written
/// as pixels instead.
const MAX_NESTING: u32 = 8;

/// An embedded smart-object source as written to `lnk2`.
#[derive(Clone, Debug)]
struct Source {
    uuid: String,
    /// Pixel size of the source document.
    size: (f64, f64),
    dpi: f64,
}

/// Smart-object state collected while emitting layers, written into the global blocks at the end.
#[derive(Default)]
struct SmartOut {
    /// Nesting depth of this export (embedded documents are exported recursively).
    depth: u32,
    /// The document's preserved global blocks (embedded files and filter caches from PSD import).
    globals: Vec<photocraft_doc::PsdGlobalBlock>,
    /// uuids of the files in the preserved linked-layer blocks.
    known: std::collections::HashSet<String>,
    /// uuids the layers reference.
    used: std::collections::HashSet<String>,
    /// Unreferenced preserved files may be dropped (every smart layer's file id is known).
    prune_ok: bool,
    /// Files to embed.
    add: Vec<crate::linked::LinkedFile>,
    /// Sources converted so far, by content hash (`Err` = can't be embedded).
    converted: std::collections::HashMap<[u8; 32], Result<Source, String>>,
    /// Embedded documents decoded while converting them, by uuid (for the filter caches).
    docs: std::collections::HashMap<String, Document>,
    /// Source composites (document pixel format, source space) by uuid; `None` = unreadable.
    composites: std::collections::HashMap<String, Option<(std::sync::Arc<Surface>, photocraft_geom::Rect)>>,
    /// Preserved `FEid` items (by placed id).
    old_fx: Vec<photocraft_psd::filter_effects::FilterEffectsItem>,
    /// Placed ids whose preserved `FEid` item stays.
    keep_fx: std::collections::HashSet<String>,
    /// Regenerated `FEid` items.
    new_fx: Vec<photocraft_psd::filter_effects::FilterEffectsItem>,
}

/// `FEid` data padded to 4 bytes inside the block length: Photoshop counts the padding in the
/// length and can't open a file whose `FEid` is padded outside it.
fn padded_fx(fx: &photocraft_psd::filter_effects::FilterEffects) -> Vec<u8> {
    let mut d = fx.to_bytes();
    d.resize(d.len().next_multiple_of(4), 0);
    d
}

const LINK_KEYS: [&[u8; 4]; 4] = [b"lnk2", b"lnk3", b"lnkD", b"lnkE"];
const FX_KEYS: [&[u8; 4]; 2] = [b"FEid", b"FXid"];

impl SmartOut {
    fn new(doc: &Document, depth: u32) -> Self {
        let globals = doc.metadata.psd_global_blocks.clone();
        let known = globals.iter().filter(|(_, k, _)| LINK_KEYS.contains(&k)).flat_map(|(_, _, d)| crate::linked::block_uuids(d)).collect();
        let old_fx = globals
            .iter()
            .filter(|(_, k, _)| FX_KEYS.contains(&k))
            .filter_map(|(_, _, d)| photocraft_psd::filter_effects::FilterEffects::parse(d).ok())
            .flat_map(|fx| fx.items)
            .collect();
        SmartOut { depth, globals, known, prune_ok: true, old_fx, ..Default::default() }
    }

    /// The document's global blocks with the linked-layer files and filter caches brought up to
    /// date: unreferenced files dropped (when every reference is known), new files appended,
    /// filter caches kept or regenerated per smart object. Unchanged blocks stay byte-identical.
    fn finish(&self, blocks: Vec<photocraft_doc::PsdGlobalBlock>) -> Vec<photocraft_doc::PsdGlobalBlock> {
        use std::sync::Arc;
        let keep = |u: &str| !self.prune_ok || self.used.contains(u);
        let mut out = Vec::with_capacity(blocks.len() + 2);
        let mut added = false;
        let mut fx_placed = false;
        for (sig, key, data) in blocks {
            if LINK_KEYS.contains(&&key) {
                let add: &[crate::linked::LinkedFile] = if !added && &key == b"lnk2" { &self.add } else { &[] };
                added |= &key == b"lnk2";
                match crate::linked::rebuild_block(&data, &keep, add) {
                    None => out.push((sig, key, data)),
                    Some(d) if d.is_empty() => {}
                    Some(d) => out.push((sig, key, Arc::new(d))),
                }
            } else if FX_KEYS.contains(&&key) {
                let Ok(mut fx) = photocraft_psd::filter_effects::FilterEffects::parse(&data) else {
                    out.push((sig, key, data));
                    continue;
                };
                let before = fx.items.len();
                fx.items.retain(|i| self.keep_fx.contains(&i.id));
                let fresh = !fx_placed && !self.new_fx.is_empty();
                if fresh {
                    fx.items.extend(self.new_fx.iter().cloned());
                    fx_placed = true;
                }
                if fx.items.len() == before && !fresh {
                    out.push((sig, key, data));
                } else if !fx.items.is_empty() {
                    out.push((sig, key, Arc::new(padded_fx(&fx))));
                }
            } else {
                out.push((sig, key, data));
            }
        }
        if !added && !self.add.is_empty() {
            let data: Vec<u8> = self.add.iter().flat_map(crate::linked::encode_linked_file).collect();
            out.push((*b"8BIM", *b"lnk2", Arc::new(data)));
        }
        if !fx_placed && !self.new_fx.is_empty() {
            let fx = photocraft_psd::filter_effects::FilterEffects { version: 3, items: self.new_fx.clone() };
            out.push((*b"8BIM", *b"FEid", Arc::new(padded_fx(&fx))));
            if !out.iter().any(|(_, k, _)| k == b"FMsk") {
                // Filter mask overlay: RGB red at 50 %, Photoshop's default.
                out.push((*b"8BIM", *b"FMsk", Arc::new(vec![0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 50])));
            }
        }
        out
    }
}

/// Size and resolution of a smart-object source file (PSD/PSB header, `.pcraft`, or any image).
fn source_geometry(name: &str, bytes: &[u8]) -> Result<((f64, f64), f64), String> {
    if crate::is_psd(bytes) {
        let rd = |at: usize| bytes.get(at..at + 4).map(|b| f64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])));
        let (h, w) = rd(14).zip(rd(18)).ok_or("truncated PSD header")?;
        return Ok(((w, h), 72.0));
    }
    let doc = crate::import(name, bytes).map_err(|e| e.to_string())?.document;
    Ok(((f64::from(doc.size.width), f64::from(doc.size.height)), f64::from(doc.resolution_dpi)))
}

/// The source size of a smart object whose file we can't decode: its rendered bounds taken back
/// through the inverse transform (the source's top-left is the origin).
fn size_from_layer(sm: &photocraft_doc::SmartObject) -> (f64, f64) {
    let r = sm.cache.as_ref().map(Surface::content_bounds).filter(|r| !r.is_empty());
    let (Some(r), Some(inv)) = (r, sm.transform.inverse()) else { return (1.0, 1.0) };
    let [a, b, c, d, e, f] = inv.m;
    let (mut w, mut h) = (1.0f64, 1.0f64);
    for (x, y) in [(r.x0, r.y0), (r.x1, r.y0), (r.x1, r.y1), (r.x0, r.y1)] {
        let (x, y) = (f64::from(x), f64::from(y));
        w = w.max((a * x + c * y + e).round());
        h = h.max((b * x + d * y + f).round());
    }
    (w, h)
}

fn upsert(raw: &mut Vec<([u8; 4], Vec<u8>)>, key: &[u8; 4], data: Vec<u8>, before: Option<&[u8; 4]>) {
    if let Some(e) = raw.iter_mut().find(|(k, _)| k == key) {
        e.1 = data;
        return;
    }
    match before.and_then(|b| raw.iter().position(|(k, _)| k == b)) {
        Some(i) => raw.insert(i, (*key, data)),
        None => raw.push((*key, data)),
    }
}

fn psd_mode(m: ColorMode) -> PsdMode {
    match m {
        ColorMode::Grayscale => PsdMode::Grayscale,
        ColorMode::Cmyk => PsdMode::Cmyk,
        ColorMode::Lab => PsdMode::Lab,
        _ => PsdMode::Rgb,
    }
}

fn to_psd_rect(r: photocraft_geom::Rect) -> PsdRect {
    PsdRect { top: r.y0, left: r.x0, bottom: r.y1, right: r.x1 }
}

fn q255(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Ex {
    fn compression(&self) -> Compression {
        if self.fmt.sample == SampleType::F32 { Compression::ZipPrediction } else { Compression::Rle }
    }

    fn encode(&self, id: i16, plane: &[u8], w: usize, h: usize) -> ChannelData {
        let depth = psd_depth(self.fmt.sample);
        ChannelData::encode(id, self.compression(), plane, w, h, depth, self.version)
            .or_else(|_| ChannelData::encode(id, Compression::Raw, plane, w, h, depth, self.version))
            .unwrap_or(ChannelData { id, compression: Some(Compression::Raw), data: plane.to_vec() })
    }

    fn empty_channels(&self) -> Vec<ChannelData> {
        (-1..self.cc as i16).map(|id| self.encode(id, &[], 0, 0)).collect()
    }

    fn pixel_channels(&mut self, s: &Surface, name: &str) -> (PsdRect, Vec<ChannelData>) {
        let s = if s.format() != self.fmt {
            self.warnings.push(format!("layer \"{name}\": pixels converted from {:?} to {:?}", s.format(), self.fmt));
            s.convert(self.fmt)
        } else {
            s.clone()
        };
        let r = s.content_bounds();
        if r.is_empty() {
            return (PsdRect::default(), self.empty_channels());
        }
        let mut bytes = s.to_interleaved(r);
        if self.fmt.mode == ColorMode::Lab && self.fmt.sample == SampleType::U16 {
            crate::pixels::lab16_chroma(&mut bytes, self.cc + 1, false);
        }
        let mut invert = vec![self.cmyk; self.cc];
        invert.push(false);
        let planes = deinterleave(&bytes, self.cc + 1, self.fmt.sample, &invert);
        drop(bytes);
        let (w, h) = (r.width() as usize, r.height() as usize);
        // Alpha (-1) first, then the colour channels; each compressed on its own thread.
        let order: Vec<(i16, &Vec<u8>)> =
            std::iter::once((-1, &planes[self.cc])).chain(planes.iter().take(self.cc).enumerate().map(|(c, p)| (c as i16, p))).collect();
        let ch = crate::pixels::par_map(order, |(id, plane)| self.encode(id, plane, w, h));
        (to_psd_rect(r), ch)
    }

    fn mask(&self, m: &LayerMask, vector: Option<(f32, f32)>) -> (MaskData, Option<ChannelData>) {
        let s = if m.surface.format() != self.mask_fmt { m.surface.convert(self.mask_fmt) } else { m.surface.clone() };
        let default = s.default_pixel().first().copied().unwrap_or(0.0);
        let r = s.content_bounds();
        let (w, h) = (r.width() as usize, r.height() as usize);
        let plane = if r.is_empty() { Vec::new() } else { deinterleave(&s.to_interleaved(r), 1, self.mask_fmt.sample, &[false]).remove(0) };
        let mut flags = 0u8;
        if !m.linked {
            flags |= PsdMask::FLAG_RELATIVE;
        }
        if !m.enabled {
            flags |= PsdMask::FLAG_DISABLED;
        }
        let parameters = mask_parameters(Some((m.density, m.feather)), vector);
        if parameters.is_some() {
            flags |= PsdMask::FLAG_PARAMETERS;
        }
        let pm = PsdMask {
            rect: if r.is_empty() { PsdRect::default() } else { to_psd_rect(r) },
            default_color: q255(default),
            flags,
            trailing: if parameters.is_none() { vec![0, 0] } else { Vec::new() },
            parameters,
            real: None,
            real_first: true,
        };
        (MaskData::Mask(pm), Some(self.encode(-2, &plane, w, h)))
    }

    /// Ids for section dividers: above every layer id.
    fn fresh_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn layer_id(&mut self, l: &Layer) -> u32 {
        match self.layer_ids.get(&l.id) {
            Some(id) => *id,
            None => self.fresh_id(),
        }
    }

    fn record(&mut self, l: &Layer, extra: Vec<TaggedBlock>, pixels: Option<&Surface>) -> LayerRecord {
        let id = self.layer_id(l);
        let (rect, mut channels) = match pixels {
            Some(s) => self.pixel_channels(s, &l.name),
            None => (PsdRect::default(), self.empty_channels()),
        };
        let mut mask = MaskData::None;
        // Vector mask density/feather live in the mask parameters, with or without a user mask.
        let vector = l.vector_mask.as_ref().map(|vm| (vm.density, vm.feather));
        if let Some(m) = &l.mask {
            let (md, ch) = self.mask(m, vector);
            mask = md;
            channels.extend(ch);
        } else if let Some(parameters) = mask_parameters(None, vector) {
            // Parameters only: no user mask channel, so nothing hides on re-import.
            mask = MaskData::Mask(PsdMask {
                rect: PsdRect::default(),
                default_color: 255,
                flags: PsdMask::FLAG_PARAMETERS,
                parameters: Some(parameters),
                real: None,
                trailing: Vec::new(),
                real_first: true,
            });
        }
        let mut flags = LayerFlags(0);
        flags.set_hidden(!l.visible);
        flags.set_transparency_protected(l.locks.transparency);
        let mut blocks = vec![TaggedBlock::unicode_name(&l.name), TaggedBlock::layer_id(id)];
        let lspf = blocks::lspf_from_locks(&l.locks);
        if lspf != 0 {
            blocks.push(TaggedBlock::protection(lspf));
        }
        if l.label != photocraft_doc::LabelColor::None {
            blocks.push(TaggedBlock::sheet_color(blocks::label_index(l.label)));
        }
        blocks.push(TaggedBlock::fill_opacity(q255(l.fill_opacity)));
        // Effects: the original lfx2 while the effects are unchanged (or
        // could not be decoded at all); otherwise regenerated.
        let fx = &l.effects;
        let lfx2 = match effects_unchanged(l) {
            Some(true) | None if fx.psd_raw.is_some() => fx.psd_raw.as_ref().map(|r| r.to_vec()),
            _ => (!fx.items.is_empty()).then(|| crate::effects_map::write_lfx2(fx.enabled, &fx.items)),
        };
        if let Some(d) = lfx2 {
            let key = if l.is_group() { *b"lfxs" } else { *b"lfx2" };
            blocks.push(TaggedBlock::new(key, d));
        }
        blocks.extend(extra);
        // Layer-level blocks keep their pad byte inside the length, as Photoshop writes them:
        // readers such as psd-tools do not skip a pad after an odd length, so a regenerated
        // odd-length `lfx2` misaligned every block after it (#200).
        for b in &mut blocks {
            if b.data.len() % 2 == 1 && b.padding.is_none() {
                b.data.push(0);
            }
        }
        LayerRecord {
            rect,
            channels,
            blend_mode: PsdBlend::from_key(l.blend.psd_key()),
            opacity: q255(l.opacity),
            clipping: u8::from(l.clipped),
            flags,
            filler: 0,
            mask,
            blending_ranges: crate::blocks::ranges_from_blend_if(&l.blend_if, self.cc),
            name: legacy_name(&l.name),
            blocks,
            extra_trailing: Vec::new(),
        }
    }

    /// Content blocks for `l`: preserved raw blocks (with `psd_raw`
    /// overrides) plus regenerated adjustment/fill blocks. A preserved
    /// adjustment or fill block is reused verbatim while it still decodes to
    /// the layer's current parameters; otherwise it is regenerated.
    fn content_blocks(&mut self, l: &Layer) -> Vec<TaggedBlock> {
        let mut raw: Vec<([u8; 4], Vec<u8>)> = l.psd_blocks.iter().filter(|(k, _)| !REGENERATED.contains(&k)).map(|(k, d)| (*k, d.to_vec())).collect();
        // A stale multi-effects block would override the regenerated lfx2.
        if effects_unchanged(l) == Some(false) {
            raw.retain(|(k, _)| k != b"lmfx");
        }
        let mut regenerated: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        // `keys` is in the importer's priority order (`psd_raw` came from the first key present),
        // not file order: Photoshop writes `PlLd` before `SoLd`, and overwriting the first match
        // in file order put `soLD` data under the `PlLd` key (#200).
        let set_principal = |raw: &mut Vec<([u8; 4], Vec<u8>)>, keys: &[&[u8; 4]], data: Option<&std::sync::Arc<Vec<u8>>>| {
            let Some(d) = data else { return };
            match keys.iter().find_map(|k| raw.iter().position(|(rk, _)| rk == *k)) {
                Some(i) => raw[i].1 = d.to_vec(),
                None => raw.push((*keys[0], d.to_vec())),
            }
        };
        let fill_rule = |raw: &mut Vec<([u8; 4], Vec<u8>)>, regenerated: &mut Vec<([u8; 4], Vec<u8>)>, f: &photocraft_doc::Fill| {
            let keep = raw.iter().any(|(k, d)| matches!(k, b"SoCo" | b"GdFl" | b"PtFl") && blocks::parse_fill(k, d).as_ref() == Some(f));
            if !keep {
                raw.retain(|(k, _)| !matches!(k, b"SoCo" | b"GdFl" | b"PtFl"));
                regenerated.push(blocks::write_fill(f));
            }
        };
        match &l.content {
            LayerContent::Adjustment(a) => {
                let channels = match self.fmt.mode {
                    ColorMode::Rgb => adjust_map::Channels::Rgb,
                    ColorMode::Grayscale => adjust_map::Channels::Gray,
                    ColorMode::Cmyk => adjust_map::Channels::Cmyk,
                    ColorMode::Lab => adjust_map::Channels::Lab,
                    _ => adjust_map::Channels::Other,
                };
                let cged = raw.iter().find(|(k, _)| k == b"CgEd").map(|(_, d)| d.clone());
                let keep = raw.iter().any(|(k, d)| adjust_map::ADJUSTMENT_KEYS.contains(&k) && adjust_map::parse(k, d, cged.as_deref(), channels) == *a);
                if !keep {
                    raw.retain(|(k, _)| !adjust_map::ADJUSTMENT_KEYS.contains(&k) && k != b"CgEd");
                    let w = adjust_map::write(a);
                    if w.is_empty() {
                        self.warnings.push(format!("layer \"{}\": {} adjustment is not yet written to PSD", l.name, a.label()));
                    }
                    regenerated.extend(w);
                }
            }
            LayerContent::Fill(f) => fill_rule(&mut raw, &mut regenerated, f),
            LayerContent::Shape(sh) => {
                if let Some(f) = &sh.fill {
                    fill_rule(&mut raw, &mut regenerated, f);
                }
                let (w, h) = (self.canvas.width(), self.canvas.height());
                crate::vector_map::shape_blocks(sh, &mut raw, w, h, self.dpi);
            }
            LayerContent::Text(t) => {
                // Text layers without PSD data (created here) get a generated TySh.
                let generated = t.psd_raw.is_none().then(|| std::sync::Arc::new(photocraft_text::psd::build_tysh(t, self.dpi, None)));
                let src = t.psd_raw.as_ref().or(generated.as_ref());
                // Character/paragraph style sheets from the document's styles.
                let styled = src.and_then(|d| crate::text_styles_map::export_tysh(d, t, &self.text_styles, self.dpi)).map(std::sync::Arc::new);
                set_principal(&mut raw, &[b"TySh"], styled.as_ref().or(src));
                if !raw.iter().any(|(k, _)| k == b"TySh") {
                    self.warnings.push(format!("layer \"{}\": text layer written as pixels (no TySh data)", l.name));
                }
            }
            LayerContent::Smart(sm) => {
                let parsed = sm.psd_raw.as_deref().and_then(|d| crate::smart_map::parse_sold(d));
                if sm.psd_raw.is_some() && parsed.is_none() {
                    // Placed-layer data we don't parse (`PlLd` only): kept verbatim.
                    self.smart.prune_ok = false;
                    set_principal(&mut raw, smart_keys(sm.psd_raw.as_deref().map(Vec::as_slice)), sm.psd_raw.as_ref());
                } else {
                    self.smart_blocks(l, sm, parsed, &mut raw);
                }
            }
            LayerContent::Raster(_) | LayerContent::Group(_) => {}
        }
        // Effects reference point: written from the field (in place, keeping block order).
        match l.effects.reference {
            Some((x, y)) => {
                let data: Vec<u8> = x.to_be_bytes().into_iter().chain(y.to_be_bytes()).collect();
                match raw.iter_mut().find(|(k, _)| k == b"fxrp") {
                    Some(e) => e.1 = data,
                    None => raw.push((*b"fxrp", data)),
                }
            }
            None => raw.retain(|(k, _)| k != b"fxrp"),
        }
        // Advanced Blending channel restrictions: written from the field (in place).
        match crate::blocks::brst_data(l.excluded_channels) {
            Some(data) => match raw.iter_mut().find(|(k, _)| k == b"brst") {
                Some(e) => e.1 = data,
                None => raw.push((*b"brst", data)),
            },
            None => raw.retain(|(k, _)| k != b"brst"),
        }
        if !matches!(l.content, LayerContent::Shape(_)) {
            self.vector_mask_block(l, &mut raw);
        }
        crate::comps_map::artboard_block(&self.guides, l, &mut raw);
        if let Some((comps, last)) = &self.comps {
            // Ids are assigned to every layer before emitting.
            let id = self.layer_ids.get(&l.id).copied().unwrap_or(0);
            crate::comps_map::set_cmls(&mut raw, crate::comps_map::write_cmls(comps, last.as_ref(), l, id));
        }
        regenerated.into_iter().chain(raw).map(|(k, d)| TaggedBlock::new(k, d)).collect()
    }

    /// Keeps, regenerates or removes the `vmsk`/`vsms` block of a non-shape layer.
    fn vector_mask_block(&mut self, l: &Layer, raw: &mut Vec<([u8; 4], Vec<u8>)>) {
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let pos = raw.iter().position(|(k, _)| k == b"vsms" || k == b"vmsk");
        match (&l.vector_mask, pos) {
            (None, Some(_)) => raw.retain(|(k, _)| k != b"vsms" && k != b"vmsk"),
            (None, None) => {}
            (Some(vm), Some(i)) if crate::vector_map::vector_mask_block_matches(&raw[i].1, vm, w, h) => {}
            (Some(vm), pos) => {
                let data = crate::vector_map::vector_mask_bytes(vm, w, h);
                match pos {
                    Some(i) => raw[i].1 = data,
                    None => raw.push((*b"vmsk", data)),
                }
            }
        }
    }

    /// Placed-layer blocks (`PlLd` + `SoLd`) of a smart object, its embedded file (`lnk2`) and
    /// filter cache (`FEid`). An imported placed layer that nothing changed is written verbatim;
    /// otherwise the blocks are regenerated (from the imported descriptor where the source is
    /// the same). Without a source that can be embedded the layer is written as pixels.
    fn smart_blocks(&mut self, l: &Layer, sm: &photocraft_doc::SmartObject, template: Option<crate::smart_map::Placed>, raw: &mut Vec<([u8; 4], Vec<u8>)>) {
        use crate::smart_map::{FilterStack, PlacedSpec, filter_fx, plld_bytes, sold_bytes, uuid_from};
        let src = match self.smart_source(sm, template.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                raw.retain(|(k, _)| !matches!(k, b"SoLd" | b"PlLd" | b"SoLE"));
                self.warnings.push(format!("layer \"{}\": smart object written as pixels ({e})", l.name));
                return;
            }
        };
        self.smart.used.insert(src.uuid.clone());
        let stack = FilterStack {
            enabled: sm.filters_enabled,
            filters: sm.smart_filters.clone(),
            mask_enabled: sm.filter_mask.as_ref().is_none_or(|m| m.enabled),
            mask_linked: sm.filter_mask.as_ref().is_some_and(|m| m.linked),
        };
        let same_source = template.as_ref().filter(|t| t.idnt == src.uuid);
        if let (Some(t), Some(data)) = (same_source, sm.psd_raw.as_ref())
            && self.placed_unchanged(t, data, sm, &stack)
        {
            let keys: &[&[u8; 4]] = if raw.iter().any(|(k, _)| k == b"SoLd") { &[b"SoLd"] } else { &[b"SoLE", b"SoLd"] };
            match keys.iter().find_map(|k| raw.iter().position(|(rk, _)| rk == *k)) {
                Some(i) => raw[i].1 = data.to_vec(),
                None => raw.push((*b"SoLd", data.to_vec())),
            }
            if !stack.filters.is_empty() {
                self.smart.keep_fx.insert(t.placed.clone());
            }
            return;
        }
        let placed = match same_source {
            Some(t) if !t.placed.is_empty() => t.placed.clone(),
            _ => uuid_from(format!("{}:{}", src.uuid, l.id.0).as_bytes()),
        };
        let size = same_source.and_then(|t| crate::smart_map::stored_size(&t.descriptor)).unwrap_or(src.size);
        let size = if size.0 > 0.0 && size.1 > 0.0 { size } else { size_from_layer(sm) };
        let mut warnings = Vec::new();
        let fx = (!stack.filters.is_empty()).then(|| filter_fx(&stack, &l.name, &mut warnings));
        let spec = PlacedSpec { idnt: &src.uuid, placed: &placed, transform: sm.transform, size, dpi: src.dpi, warp: sm.warp.as_ref(), filter_fx: fx };
        let sold = sold_bytes(same_source.map(|t| &t.descriptor), &spec, &mut warnings);
        let plld = plld_bytes(&spec, &mut warnings);
        warnings.dedup();
        let key = if same_source.is_some() && raw.iter().any(|(k, _)| k == b"SoLE") && !raw.iter().any(|(k, _)| k == b"SoLd") { *b"SoLE" } else { *b"SoLd" };
        raw.retain(|(k, _)| k == &key || !matches!(k, b"SoLd" | b"SoLE"));
        upsert(raw, b"PlLd", plld, Some(&key));
        upsert(raw, &key, sold, None);
        if !stack.filters.is_empty() {
            let bounds = sm.cache.as_ref().map_or(self.canvas, |c| c.content_bounds().union(&self.canvas));
            let item = match (sm.stack_mode, self.source_composite(&src.uuid)) {
                (None, Some((img, img_bounds))) => {
                    let unfiltered = photocraft_algo::warp::place_source(&img, img_bounds, &sm.transform, sm.warp.as_ref());
                    crate::smart_map::feid_item(&placed, &unfiltered, sm.filter_mask.as_ref(), bounds, self.fmt)
                }
                _ => None,
            };
            match item {
                Some(i) => self.smart.new_fx.push(i),
                // Without the unfiltered pixels Photoshop re-renders from the source itself; only
                // the filter mask has nowhere to go.
                None if sm.filter_mask.is_some() => self.warnings.push(format!("layer \"{}\": the smart filter mask was not written to PSD", l.name)),
                None => {}
            }
        }
        self.warnings.extend(warnings.into_iter().map(|w| if w.starts_with("layer ") { w } else { format!("layer \"{}\": {w}", l.name) }));
    }

    /// Whether an imported placed layer still says what the smart object is: transform, warp,
    /// filter stack and filter mask as stored.
    fn placed_unchanged(&self, t: &crate::smart_map::Placed, data: &[u8], sm: &photocraft_doc::SmartObject, stack: &crate::smart_map::FilterStack) -> bool {
        if t.transform != sm.transform || blocks::parse_placed_warp(b"SoLd", data) != sm.warp || t.stack.clone().unwrap_or_default() != *stack {
            return false;
        }
        if stack.filters.is_empty() {
            return true;
        }
        let old = self.smart.old_fx.iter().find(|i| i.id == t.placed);
        let mask = old.map(|i| crate::smart_map::mask_from_item(i, self.mask_fmt.sample, stack));
        match mask {
            Some(Ok(m)) => m == sm.filter_mask,
            Some(Err(_)) => false,
            None => sm.filter_mask.is_none(),
        }
    }

    /// The `lnk2` file a smart object's source is written as: a preserved embedded file, or a new
    /// one (a `.pcraft` source becomes a PSB of the nested document; image files are embedded as
    /// they are; a linked file is read and embedded).
    fn smart_source(&mut self, sm: &photocraft_doc::SmartObject, template: Option<&crate::smart_map::Placed>) -> Result<Source, String> {
        use photocraft_doc::SmartSource;
        match &sm.source {
            // A file of the imported document, or one its placed-layer data names (a missing
            // link stays a smart object, as in Photoshop).
            SmartSource::Linked { path } if self.smart.known.contains(path) || template.is_some_and(|t| t.idnt == *path) => {
                let stored = template.filter(|t| t.idnt == *path).and_then(|t| crate::smart_map::stored_size(&t.descriptor));
                let geometry = stored.map(|size| (size, f64::from(self.dpi))).or_else(|| {
                    let meta = photocraft_doc::Metadata { psd_global_blocks: self.smart.globals.clone(), ..Default::default() };
                    let f = crate::linked::find_linked_file(&meta, path)?;
                    source_geometry(&f.file_name, &f.bytes).ok()
                });
                let (size, dpi) = geometry.unwrap_or((size_from_layer(sm), f64::from(self.dpi)));
                Ok(Source { uuid: path.clone(), size, dpi })
            }
            SmartSource::Linked { path } => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let bytes = std::fs::read(path).map_err(|e| format!("the linked file {path} can't be read: {e}"))?;
                    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                    self.warnings.push(format!("the linked smart object {name} was embedded (PSD export keeps no external links)"));
                    self.embed(&name, &bytes)
                }
                #[cfg(target_arch = "wasm32")]
                Err(format!("the linked file {path} can't be read here"))
            }
            SmartSource::Embedded { file_name, bytes } => self.embed(file_name, bytes),
        }
    }

    /// The composite of the source `uuid` (an embedded file of this export or of the imported
    /// document) in the document's pixel format, its top-left at the origin; decoded once.
    fn source_composite(&mut self, uuid: &str) -> Option<(std::sync::Arc<Surface>, photocraft_geom::Rect)> {
        if let Some(c) = self.smart.composites.get(uuid) {
            return c.clone();
        }
        let doc = match self.smart.docs.get(uuid) {
            Some(d) => Some(d.clone()),
            None => {
                let file = self.smart.add.iter().find(|f| f.uuid == uuid).cloned().or_else(|| {
                    let meta = photocraft_doc::Metadata { psd_global_blocks: self.smart.globals.clone(), ..Default::default() };
                    crate::linked::find_linked_file(&meta, uuid)
                });
                file.and_then(|f| crate::import(&f.file_name, &f.bytes).ok()).map(|r| r.document)
            }
        };
        let c = doc.map(|d| {
            let mut s = Surface::new(self.fmt);
            let fmt = self.fmt;
            let _ = photocraft_compose::render_bands(&d, d.bounds(), 0, |band| -> Result<(), ()> {
                let mut vals = Vec::with_capacity(band.px.len() * fmt.channels());
                let mut v = [0.0f32; 5];
                for p in &band.px {
                    let n = photocraft_raster::from_rgba_into(&fmt, *p, &mut v);
                    vals.extend_from_slice(&v[..n]);
                }
                s.write_region(band.rect, &vals);
                Ok(())
            });
            s.prune();
            (std::sync::Arc::new(s), d.bounds())
        });
        self.smart.composites.insert(uuid.to_string(), c.clone());
        c
    }

    /// Adds `bytes` as an embedded file (converted to PSB when it is a `.pcraft` bundle), once
    /// per distinct content.
    fn embed(&mut self, file_name: &str, bytes: &[u8]) -> Result<Source, String> {
        let key = *blake3::hash(bytes).as_bytes();
        if let Some(r) = self.smart.converted.get(&key) {
            return r.clone();
        }
        let r = self.convert_source(file_name, bytes);
        self.smart.converted.insert(key, r.clone());
        r
    }

    fn convert_source(&mut self, file_name: &str, bytes: &[u8]) -> Result<Source, String> {
        let (name, data, size, dpi, nested) = if photocraft_format::is_pcraft(bytes) {
            if self.smart.depth >= MAX_NESTING {
                return Err(format!("smart objects nest more than {MAX_NESTING} deep"));
            }
            let doc = photocraft_format::load_from_bytes(bytes).map_err(|e| format!("its contents can't be read: {e}"))?;
            let (file, warnings) = document_to_psd_nested(&doc, &PsdExportOptions { force_psb: true }, self.smart.depth + 1);
            let data = file.to_bytes().map_err(|e| format!("its contents can't be written: {e}"))?;
            let stem = file_name.rsplit_once('.').map_or(file_name, |(a, _)| a);
            self.warnings.extend(warnings.into_iter().map(|w| format!("smart object {stem}: {w}")));
            let size = (f64::from(doc.size.width), f64::from(doc.size.height));
            (format!("{stem}.psb"), data, size, f64::from(doc.resolution_dpi), Some(doc))
        } else {
            // Formats we can't decode (vector PDF/AI…) are still embedded; their size comes from the layer.
            let (size, dpi) = source_geometry(file_name, bytes).unwrap_or(((0.0, 0.0), f64::from(self.dpi)));
            (file_name.to_string(), bytes.to_vec(), size, dpi, None)
        };
        let uuid = crate::smart_map::uuid_from(&data);
        if let Some(d) = nested {
            self.smart.docs.insert(uuid.clone(), d);
        }
        if !self.smart.known.contains(&uuid) && !self.smart.add.iter().any(|f| f.uuid == uuid) {
            self.smart.add.push(crate::linked::LinkedFile { uuid: uuid.clone(), file_name: name, bytes: data });
        }
        Ok(Source { uuid, size, dpi })
    }

    /// Pixels for a fill layer: Photoshop's cached rendering while valid,
    /// otherwise our own rendering of the fill over the canvas.
    fn fill_pixels(&self, l: &Layer, f: &photocraft_doc::Fill) -> Surface {
        if let Some(c) = &l.fill_cache
            && c.fill == *f
        {
            return c.surface.clone();
        }
        // In the frame the layer's masks give it, like the compositor (masks are stored apart).
        let buf = photocraft_compose::render_fill_content(l, f, self.canvas, &[]);
        let mut s = Surface::new(self.fmt);
        let vals: Vec<f32> = buf.px.iter().flat_map(|p| photocraft_raster::from_rgba(&self.fmt, *p)).collect();
        s.write_region(self.canvas, &vals);
        s.prune();
        s
    }

    fn emit(&mut self, layers: &[Layer]) {
        for l in layers {
            let extra = self.content_blocks(l);
            match &l.content {
                LayerContent::Group(g) => {
                    let id = self.fresh_id();
                    self.records.push(LayerRecord {
                        channels: self.empty_channels(),
                        blending_ranges: BlendingRanges::full(self.cc),
                        name: b"</Layer group>".to_vec(),
                        blocks: vec![
                            TaggedBlock::unicode_name("</Layer group>"),
                            TaggedBlock::layer_id(id),
                            TaggedBlock::section_divider(SectionType::BoundingDivider, None, None),
                        ],
                        ..Default::default()
                    });
                    self.emit(&g.children);
                    let kind = if g.expanded { SectionType::OpenFolder } else { SectionType::ClosedFolder };
                    let lsct = TaggedBlock::section_divider(kind, Some(PsdBlend::from_key(l.blend.psd_key())), None);
                    let r = self.record(l, std::iter::once(lsct).chain(extra).collect(), None);
                    self.records.push(r);
                }
                LayerContent::Raster(s) => {
                    let r = self.record(l, extra, Some(s));
                    self.records.push(r);
                }
                LayerContent::Adjustment(_) => {
                    let r = self.record(l, extra, None);
                    self.records.push(r);
                }
                LayerContent::Fill(f) => {
                    let px = self.fill_pixels(l, f);
                    let r = self.record(l, extra, Some(&px));
                    self.records.push(r);
                }
                LayerContent::Text(t) => {
                    let r = self.record(l, extra, t.cache.as_ref());
                    self.records.push(r);
                }
                LayerContent::Shape(sh) => {
                    let r = self.record(l, extra, sh.cache.as_ref());
                    self.records.push(r);
                }
                LayerContent::Smart(sm) => {
                    let r = self.record(l, extra, sm.cache.as_ref());
                    self.records.push(r);
                }
            }
        }
    }
}

/// Resource 1026 (Layer › Link Layers): one u16 group id per layer record, in the order
/// [`Ex::emit`] writes records (a group's bounding divider, its children, then the group).
/// Imported ids that fit in a u16 are kept, so unchanged files write the same bytes; otherwise
/// groups are renumbered by first appearance. `None` when no layer is linked.
fn link_group_resource(layers: &[Layer]) -> Option<Vec<u8>> {
    fn walk(layers: &[Layer], out: &mut Vec<Option<u64>>) {
        for l in layers {
            if let LayerContent::Group(g) = &l.content {
                out.push(None);
                walk(&g.children, out);
            }
            out.push(l.link_group);
        }
    }
    let mut per = Vec::new();
    walk(layers, &mut per);
    if per.iter().all(Option::is_none) {
        return None;
    }
    let fits = per.iter().flatten().all(|&g| (1..=u64::from(u16::MAX)).contains(&g));
    let mut seen: Vec<u64> = Vec::new();
    let ids = per.iter().map(|g| match *g {
        None => 0u16,
        Some(g) if fits => g as u16,
        Some(g) => {
            let i = seen.iter().position(|&s| s == g).unwrap_or_else(|| {
                seen.push(g);
                seen.len() - 1
            });
            u16::try_from(i + 1).unwrap_or(u16::MAX)
        }
    });
    Some(ids.flat_map(u16::to_be_bytes).collect())
}

/// Whether the layer's effects still equal what its preserved `lmfx`/`lfx2`
/// decodes to. `None` when there is nothing preserved or it can't be decoded.
/// Block keys a smart object's `psd_raw` may be written under, in priority order. The data says
/// which it is: `PlLd` holds a `plcL` structure, `SoLd`/`SoLE` a `soLD` one (Adobe PSD spec,
/// "Placed Layer" and "Placed Layer Data"). Writing one under the other's key makes the block
/// unreadable.
fn smart_keys(data: Option<&[u8]>) -> &'static [&'static [u8; 4]] {
    match data.and_then(|d| d.get(..4)) {
        Some(b"plcL") => &[b"PlLd"],
        Some(b"soLD") => &[b"SoLd", b"SoLE"],
        _ => &[b"SoLd", b"PlLd", b"SoLE"],
    }
}

fn effects_unchanged(l: &Layer) -> Option<bool> {
    let src = l.psd_blocks.iter().find(|(k, _)| k == b"lmfx").map(|(_, d)| d.clone()).or_else(|| l.effects.psd_raw.clone())?;
    let (m, items) = crate::effects_map::parse_lfx2(&src)?;
    Some(m == l.effects.enabled && items == l.effects.items)
}

fn legacy_name(s: &str) -> Vec<u8> {
    s.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c as u8 } else { b'?' }).take(255).collect()
}

pub(crate) fn unicode_names_resource(names: &[&str]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in names {
        let units: Vec<u16> = n.encode_utf16().chain(std::iter::once(0)).collect();
        v.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            v.extend_from_slice(&u.to_be_bytes());
        }
    }
    v
}

pub(crate) fn pascal_names_resource(names: &[&str]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in names {
        let b = legacy_name(n);
        v.push(b.len() as u8);
        v.extend_from_slice(&b);
    }
    v
}

fn guides_resource(doc: &Document) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&576u32.to_be_bytes());
    v.extend_from_slice(&576u32.to_be_bytes());
    let n = doc.guides.horizontal.len() + doc.guides.vertical.len();
    v.extend_from_slice(&(n as u32).to_be_bytes());
    for (list, dir) in [(&doc.guides.vertical, 0u8), (&doc.guides.horizontal, 1u8)] {
        for p in list {
            v.extend_from_slice(&((p * 32.0).round() as i32).to_be_bytes());
            v.push(dir);
        }
    }
    v
}

fn cmyk_of(fmt: &PixelFormat) -> bool {
    fmt.mode == ColorMode::Cmyk
}

/// The merged image's planes (colour channels, then alpha), big-endian, matted against white
/// when `matte`; with whether any pixel needs the alpha channel and whether any is below 1.
fn merged_planes(doc: &Document, fmt: &PixelFormat, cmyk: bool, matte: bool) -> (Vec<u8>, bool, bool) {
    let sample = fmt.sample;
    let cc = fmt.mode.color_channels();
    let bps = sample.bytes();
    let n = doc.size.area() as usize;
    let plane = n * bps;
    let mut planes = vec![0u8; plane * (cc + 1)];
    let (mut has_alpha, mut translucent) = (false, false);
    let white = photocraft_raster::from_rgba(fmt, [1.0, 1.0, 1.0, 1.0]);
    let canvas = doc.bounds();
    let w = canvas.width() as usize;
    let space = photocraft_compose::cmyk_space(doc);
    let lab16 = fmt.mode == ColorMode::Lab && sample == SampleType::U16;
    let _ = photocraft_compose::render_bands(doc, canvas, 0, |band| -> Result<(), ()> {
        // Retain alpha whenever it differs from opaque at the stored precision.
        has_alpha |= band.px.iter().any(|p| match sample {
            SampleType::U8 => q255(p[3]) < 255,
            SampleType::U16 => (p[3].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16 != u16::MAX,
            SampleType::F32 => p[3] != 1.0,
        });
        translucent |= band.px.iter().any(|p| p[3] < 1.0);
        let start = (band.rect.y0 - canvas.y0) as usize * w;
        // Converted and encoded on all cores, then copied into each plane.
        let parts = crate::pixels::par_map(crate::pixels::bands(band.px.len()), |range| {
            // The composite came through the document's CMYK profile: convert back through it too.
            photocraft_color::convert::with_cmyk_space(space.as_ref(), || {
                let mut out: Vec<Vec<u8>> = vec![Vec::with_capacity(range.len() * bps); cc + 1];
                let mut v = [0.0f32; 5];
                for p in &band.px[range.clone()] {
                    photocraft_raster::from_rgba_into(fmt, *p, &mut v);
                    for c in 0..=cc {
                        // Matte against white like Photoshop (see `pixels::matte`).
                        let m = if c < cc && matte { crate::pixels::matte(v[c], v[cc], white[c]) } else { v[c] };
                        let x = if cmyk && c < cc { 1.0 - m } else { m };
                        // 16-bit Lab a*/b* use Photoshop's 0..65280 scale (`pixels::lab16_chroma`).
                        let x = if lab16 && (c == 1 || c == 2) { x * (crate::pixels::LAB16_CHROMA_MAX / 65535.0) } else { x };
                        encode_be(x, sample, &mut out[c]);
                    }
                }
                (range.start, out)
            })
        });
        for (at, part) in parts {
            for (c, src) in part.iter().enumerate() {
                let o = c * plane + (start + at) * bps;
                planes[o..o + src.len()].copy_from_slice(src);
            }
        }
        Ok(())
    });
    (planes, has_alpha, translucent)
}

/// Converts a document to a PSD file model (see [`document_to_psd_with`]).
pub fn document_to_psd(doc: &Document) -> PsdFile {
    document_to_psd_with(doc, &PsdExportOptions::default()).0
}

/// Converts a document to a PSD/PSB file model, returning warnings about
/// anything that could not be represented. The merged composite is rendered
/// with `photocraft_compose::flatten`.
pub fn document_to_psd_with(doc: &Document, opts: &PsdExportOptions) -> (PsdFile, Vec<String>) {
    document_to_psd_nested(doc, opts, 0)
}

/// [`document_to_psd_with`] for a document embedded `depth` smart objects deep.
fn document_to_psd_nested(doc: &Document, opts: &PsdExportOptions, depth: u32) -> (PsdFile, Vec<String>) {
    if doc.mode == ColorMode::Multichannel {
        return crate::multichannel_map::document_to_psd(doc, opts.force_psb);
    }
    let fmt = doc.pixel_format();
    let sample = fmt.sample;
    let cc = fmt.mode.color_channels();
    let big = doc.size.width > 30_000 || doc.size.height > 30_000;
    let version = if opts.force_psb || big { Version::Psb } else { Version::Psd };
    let mut ex = Ex {
        fmt,
        dpi: doc.resolution_dpi,
        text_styles: doc.text_styles.clone(),
        mask_fmt: PixelFormat::new(ColorMode::Grayscale, sample, false),
        cc,
        cmyk: fmt.mode == ColorMode::Cmyk,
        version,
        next_id: 0,
        layer_ids: Default::default(),
        canvas: doc.bounds(),
        records: Vec::new(),
        warnings: Vec::new(),
        guides: doc.guides.clone(),
        comps: (!crate::comps_map::comps_unchanged(doc)).then(|| (doc.layer_comps.clone(), doc.last_document_state.clone())),
        smart: SmartOut::new(doc, depth),
    };
    if big && !opts.force_psb {
        ex.warnings.push("document exceeds 30000 px; written as PSB".into());
    }
    if fmt.mode != doc.mode {
        ex.warnings.push(format!("{:?} document written as {:?}", doc.mode, fmt.mode));
    }
    // Assign layer ids up front (stable across round trips): keep unique
    // `psd_id`s, number the rest after them; dividers come after all.
    {
        let walk = doc.walk();
        let mut used = std::collections::HashSet::new();
        for (_, _, l) in &walk {
            if let Some(id) = l.psd_id
                && used.insert(id)
            {
                ex.layer_ids.insert(l.id, id);
            }
        }
        let mut next = 1u32;
        for (_, _, l) in &walk {
            if let std::collections::hash_map::Entry::Vacant(e) = ex.layer_ids.entry(l.id) {
                while used.contains(&next) {
                    next += 1;
                }
                used.insert(next);
                e.insert(next);
            }
        }
        ex.next_id = used.iter().copied().max().unwrap_or(0);
    }
    ex.emit(&doc.layers);

    // Merged composite, rendered and encoded in bands (no full-size float composite). Matting
    // against white only changes pixels with alpha < 1; if some are slightly translucent but all
    // round to opaque (so no alpha channel is written), encode once more without the matte.
    let (mut planes, has_alpha, translucent) = merged_planes(doc, &fmt, cmyk_of(&fmt), true);
    if !has_alpha && translucent {
        planes = merged_planes(doc, &fmt, cmyk_of(&fmt), false).0;
    }
    let n = doc.size.area() as usize;
    if !has_alpha {
        planes.truncate(cc * n * sample.bytes());
    }
    let canvas = doc.bounds();
    let max_extra = 56 - cc - usize::from(has_alpha);
    if doc.channels.len() > max_extra {
        ex.warnings.push(format!("only {max_extra} alpha channels fit in PSD; the rest were dropped"));
    }
    let mut extra: Vec<_> = doc.channels.iter().take(max_extra).collect();
    // Saved in Quick Mask mode: the mask is written as the last extra channel and flagged by
    // resource 1022 (quick mask info: channel id, initially-empty flag), as Photoshop does.
    let quick_mask_id = match &doc.quick_mask {
        Some(q) if extra.len() < max_extra => {
            extra.push(q);
            Some((cc + usize::from(has_alpha) + extra.len() - 1) as u16)
        }
        Some(_) => {
            ex.warnings.push("Quick Mask was dropped because the PSD channel limit was reached".into());
            None
        }
        None => None,
    };
    for a in &extra {
        let s = if a.surface.format() != ex.mask_fmt { a.surface.convert(ex.mask_fmt) } else { a.surface.clone() };
        let bytes = s.to_interleaved(canvas);
        planes.extend(deinterleave(&bytes, 1, sample, &[false]).remove(0));
    }
    let channels = (cc + usize::from(has_alpha) + extra.len()) as u16;
    let header = Header::new(version, doc.size.width, doc.size.height, channels, psd_depth(sample), psd_mode(fmt.mode));
    let mcomp = if sample == SampleType::F32 { Compression::Raw } else { Compression::Rle };
    let image_data = ImageData::encode(mcomp, &planes, &header)
        .or_else(|_| ImageData::encode(Compression::Raw, &planes, &header))
        .unwrap_or(ImageData { compression: Compression::Raw, data: planes });

    // Resources.
    let mut resources = vec![
        ImageResource::new(ids::RESOLUTION_INFO, ResolutionInfo::from_dpi(f64::from(doc.resolution_dpi)).to_bytes()),
        ImageResource::new(ids::GLOBAL_ANGLE, (doc.global_light.angle.round() as i32).to_be_bytes().to_vec()),
        ImageResource::new(ids::GLOBAL_ALTITUDE, (doc.global_light.altitude.round() as i32).to_be_bytes().to_vec()),
    ];
    if let Some(icc) = &doc.icc_profile {
        resources.push(ImageResource::new(ids::ICC_PROFILE, icc.to_vec()));
    }
    if !doc.guides.horizontal.is_empty() || !doc.guides.vertical.is_empty() {
        resources.push(ImageResource::new(1032, guides_resource(doc)));
    }
    if !extra.is_empty() {
        let names: Vec<&str> = extra.iter().map(|a| a.name.as_str()).collect();
        resources.push(ImageResource::new(1006, pascal_names_resource(&names)));
        resources.push(ImageResource::new(1045, unicode_names_resource(&names)));
        resources.push(ImageResource::new(crate::channel_map::DISPLAY_INFO, crate::channel_map::display_info(&extra)));
    }
    if let Some(id) = quick_mask_id {
        let mut data = id.to_be_bytes().to_vec();
        data.push(0);
        resources.push(ImageResource::new(crate::channel_map::QUICK_MASK_INFO, data));
    }
    // Paths: saved paths (2000+), the work path (1025), the clipping path name (2999).
    {
        use crate::vector_map::{CLIPPING_PATH, WORK_PATH, path_from_resource, path_to_records};
        let (w, h) = (doc.size.width, doc.size.height);
        let max = (*crate::vector_map::SAVED_PATHS.end() - *crate::vector_map::SAVED_PATHS.start() + 1) as usize;
        if doc.paths.len() > max {
            ex.warnings.push(format!("only {max} saved paths fit in PSD; the rest were dropped"));
        }
        for (i, p) in doc.paths.iter().take(max).enumerate() {
            let data = match &p.psd_raw {
                Some(r) if path_from_resource(r, w, h).as_ref() == Some(&p.path) => r.to_vec(),
                _ => path_to_records(&p.path, w, h).to_bytes(),
            };
            let mut r = ImageResource::new(crate::vector_map::SAVED_PATHS.start() + i as u16, data);
            r.name = legacy_name(&p.name);
            resources.push(r);
        }
        if let Some(wp) = &doc.work_path {
            resources.push(ImageResource::new(WORK_PATH, path_to_records(wp, w, h).to_bytes()));
        }
        let raw_clip = doc.metadata.psd_resources.iter().find(|(id, _, _)| *id == CLIPPING_PATH);
        let raw_name = raw_clip.map(|(_, _, d)| {
            let n = usize::from(d.first().copied().unwrap_or(0));
            d.get(1..1 + n).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
        });
        if let Some(c) = &doc.clipping_path
            && raw_name.as_deref() != Some(c.name.as_str())
        {
            // Pascal name, then flatness as 16.16 fixed (best effort; see vector_map docs).
            let mut data = vec![0u8];
            let name = legacy_name(&c.name);
            data[0] = name.len() as u8;
            data.extend_from_slice(&name);
            data.extend_from_slice(&((c.flatness.max(0.0) * 65536.0) as u32).to_be_bytes());
            resources.push(ImageResource::new(CLIPPING_PATH, data));
        }
    }
    if let Some(groups) = link_group_resource(&doc.layers) {
        resources.push(ImageResource::new(ids::LAYER_GROUP_INFO, groups));
    }
    if let Some(x) = &doc.metadata.xmp {
        // The pixels are saved as they are shown: never let a reader rotate them again.
        resources.push(ImageResource::new(ids::XMP, photocraft_codecs::upright_xmp(x).as_bytes().to_vec()));
    }
    if let Some(e) = &doc.metadata.exif {
        resources.push(ImageResource::new(ids::EXIF, photocraft_codecs::upright_exif(e).into_owned()));
    }
    let mut global_blocks = Vec::new();
    for (sig, key, data) in &ex.smart.finish(crate::annotations_map::export_blocks(doc, crate::pattern_map::export_global_blocks(doc))) {
        let mut tb = TaggedBlock::new(*key, data.to_vec());
        tb.signature = *sig;
        // Photoshop pads document-level (global) blocks to a multiple of 4, and readers such as
        // psd-tools step to the next block that way: an even pad after `CAI ` (77 bytes)
        // misaligned every block after it (#200).
        tb.padding = Some(vec![0; (4 - data.len() % 4) % 4]);
        global_blocks.push(tb);
    }
    let comps_resource = ex.comps.is_some().then(|| crate::comps_map::write_comps_resource(doc));
    let slices_resource = crate::slices_map::export_resource(doc, &ex.layer_ids);
    for (id, name, data) in &doc.metadata.psd_resources {
        // Layer comps: the preserved list while unchanged, else regenerated (or dropped) below.
        if *id == crate::comps_map::LAYER_COMPS && comps_resource.is_some() {
            continue;
        }
        // Slices: the preserved resource while unchanged, else regenerated (or dropped) below.
        if *id == crate::slices_map::SLICES && slices_resource.is_some() {
            continue;
        }
        // Measurement scale / count resources: only while they still describe the document.
        if !crate::annotations_map::keep_resource(doc, *id) {
            continue;
        }
        // A preserved clipping-path resource is only valid while it names the current one.
        if *id == crate::vector_map::CLIPPING_PATH {
            let n = usize::from(data.first().copied().unwrap_or(0));
            let raw_name = data.get(1..1 + n).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            if doc.clipping_path.as_ref().is_none_or(|c| c.name != raw_name) {
                continue;
            }
        }
        let mut r = ImageResource::new(*id, data.to_vec());
        r.name = legacy_name(name);
        resources.push(r);
    }
    if let Some(data) = crate::annotations_map::fresh_scale_resource(doc) {
        resources.push(ImageResource::new(crate::annotations_map::MEASUREMENT_SCALE, data));
    }
    if let Some(Some(data)) = comps_resource {
        resources.push(ImageResource::new(crate::comps_map::LAYER_COMPS, data));
    }
    if let Some(Some(data)) = slices_resource {
        resources.push(ImageResource::new(crate::slices_map::SLICES, data));
    }
    resources.push(version_info_resource(true));

    let has_layers = !ex.records.is_empty();
    let placement = match (has_layers, sample) {
        (true, SampleType::U16) => LayerInfoPlacement::GlobalBlock { index: 0, signature: *b"8BIM", key: *b"Lr16", padding: None },
        (true, SampleType::F32) => LayerInfoPlacement::GlobalBlock { index: 0, signature: *b"8BIM", key: *b"Lr32", padding: None },
        _ => LayerInfoPlacement::Section,
    };
    let records = std::mem::take(&mut ex.records);
    let mut layer_info = has_layers.then_some(LayerInfo { merged_alpha: has_alpha, layers: records, padding: None });
    if let Some(info) = &mut layer_info {
        // Photoshop pads the layer info to 4 bytes (an even pad misaligns readers after a global
        // `Lr16`/`Lr32` block, #200). A length that cannot be computed keeps the even default.
        if info.pad_to(version, 4).is_err() {
            info.padding = None;
        }
    }
    // 32-bit files carry Photoshop's HDR toning records as Color Mode Data; Photoshop will not
    // open a 32-bit file without them (#291).
    let color_mode_data = photocraft_psd::hdr::color_mode_data_for_depth(header.depth);
    let file = PsdFile {
        header,
        color_mode_data,
        resources,
        layer_info,
        layer_info_placement: placement,
        global_layer_mask: (has_layers || !global_blocks.is_empty()).then(GlobalLayerMask::default),
        global_blocks,
        layer_mask_trailing: Vec::new(),
        image_data,
    };
    (file, ex.warnings)
}

/// PSD mask parameters for a user mask's and a vector mask's (density, feather); `None` when
/// every value is the default (density 1, feather 0).
fn mask_parameters(user: Option<(f32, f32)>, vector: Option<(f32, f32)>) -> Option<MaskParameters> {
    let density = |v: Option<(f32, f32)>| v.and_then(|(d, _)| (d < 1.0).then(|| q255(d)));
    let feather = |v: Option<(f32, f32)>| v.and_then(|(_, f)| (f != 0.0).then_some(f64::from(f)));
    let p =
        MaskParameters { flags: 0, user_density: density(user), user_feather: feather(user), vector_density: density(vector), vector_feather: feather(vector) };
    let flags = u8::from(p.user_density.is_some())
        | u8::from(p.user_feather.is_some()) << 1
        | u8::from(p.vector_density.is_some()) << 2
        | u8::from(p.vector_feather.is_some()) << 3;
    (flags != 0).then_some(MaskParameters { flags, ..p })
}

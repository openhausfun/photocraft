//! Smart Objects and Smart Filters: Layer › Smart Objects, Layer › Smart Filter and
//! Filter › Convert for Smart Filters.
//!
//! A smart object keeps its **source** (embedded file bytes, or a linked path) plus a
//! `transform` and a stack of smart filters; its `cache` is the rendered appearance the
//! compositors draw. The engine owns re-rendering (compose stays a pure function of the
//! document):
//!
//! ```text
//! source bytes ──decode + composite (cached by content hash)──► source image (source pixels)
//!              ──transform (exact shift, or bicubic warp)──────► placed (document pixels)
//!              ──each visible smart filter, blended (blend/opacity)──► filtered
//!              ──filter mask mix (unfiltered ↔ filtered)───────► cache
//! ```
//!
//! Converting a layer embeds it as an in-memory `.pcraft` bundle of a nested document (exactly
//! lossless, layered, any depth/model). PSD placed layers keep their source in the preserved
//! global `lnk2` block (found by uuid), and keep Photoshop's rendering until something changes,
//! so an unedited PSD still round-trips byte for byte. PSD export (`photocraft_io::smart_map`)
//! writes every smart object back as one: its source embedded in `lnk2` (a `.pcraft` source as a
//! PSB), its smart filters in `filterFX` and its filter mask in `FEid`.

use std::sync::{Arc, Mutex};

use photocraft_algo::resample::translate_surface;
use photocraft_color::{BlendMode, PixelFormat};
use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId, LayerMask, Metadata, SmartObject, SmartSource};
use photocraft_geom::{Affine, Rect, Size};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, blend_from_str, layer_param};
use crate::{EngineError, Result, Session};

/// An open Edit Contents document and the smart object it updates when saved or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmartLink {
    pub child: DocId,
    pub parent: DocId,
    pub layer: LayerId,
}

/// PSD placed-layer keys that describe the smart object's source; stale once we change it.
const PLACED_KEYS: [&[u8; 4]; 3] = [b"SoLd", b"PlLd", b"SoLE"];

/// Command id of a Photoshop smart filter PhotoCraft does not implement (kept verbatim from PSD).
pub use photocraft_io::smart_map::UNSUPPORTED_FILTER;

fn other(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

// ---------- source encoding / decoding ----------

/// Encodes a document as embedded smart-object contents: an in-memory `.pcraft` bundle.
pub fn encode_source(doc: &Document) -> Result<Vec<u8>> {
    photocraft_format::save_to_bytes(doc, &Default::default()).map_err(|e| other(format!("can't encode smart object contents: {e}")))
}

/// Decodes smart-object contents (a `.pcraft` bundle, PSD/PSB, or any flat image format).
pub fn decode_source(file_name: &str, bytes: &[u8]) -> Result<Document> {
    photocraft_io::import(file_name, bytes).map(|r| r.document).map_err(|e| other(format!("can't read smart object contents \"{file_name}\": {e}")))
}

fn base_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn read_file(path: &str) -> Option<Vec<u8>> {
    if path.is_empty() { None } else { std::fs::read(path).ok() }
}

/// The source file of a smart object: embedded bytes, the PSD's embedded linked-layer data (PSD
/// import keeps the placed layer's uuid as the path), or a linked file on disk.
pub fn source_bytes(meta: &Metadata, src: &SmartSource) -> Option<(String, Arc<Vec<u8>>)> {
    match src {
        SmartSource::Embedded { file_name, bytes } => Some((file_name.clone(), bytes.clone())),
        SmartSource::Linked { path } => {
            if let Some(f) = photocraft_io::linked::find_linked_file(meta, path) {
                return Some((f.file_name, Arc::new(f.bytes)));
            }
            read_file(path).map(|b| (base_name(path), Arc::new(b)))
        }
    }
}

/// A decoded, composited source in its own pixel space (`bounds` starts at the origin).
#[derive(Clone, Debug)]
pub struct SourceImage {
    pub surface: Arc<Surface>,
    pub bounds: Rect,
}

type CacheKey = ([u8; 32], PixelFormat);

struct CacheEntry {
    key: CacheKey,
    image: SourceImage,
    decodes: u32,
}

/// Decoded source composites by content hash, most recent last. Re-filtering or re-transforming
/// a smart object never decodes its contents again.
static SOURCE_CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());
const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 1 << 30;

fn cache_key(bytes: &[u8], fmt: PixelFormat) -> CacheKey {
    (*blake3::hash(bytes).as_bytes(), fmt)
}

fn cache_get(key: &CacheKey) -> Option<SourceImage> {
    let mut c = SOURCE_CACHE.lock().ok()?;
    let i = c.iter().position(|e| e.key == *key)?;
    let e = c.remove(i);
    let img = e.image.clone();
    c.push(e);
    Some(img)
}

fn cache_put(key: CacheKey, image: SourceImage) {
    let Ok(mut c) = SOURCE_CACHE.lock() else { return };
    let decodes = match c.iter().position(|e| e.key == key) {
        Some(i) => c.remove(i).decodes + 1,
        None => 1,
    };
    c.push(CacheEntry { key, image, decodes });
    let size = |e: &CacheEntry| e.image.bounds.width() as usize * e.image.bounds.height() as usize * e.key.1.channels() * 4;
    while c.len() > CACHE_ENTRIES || (c.len() > 1 && c.iter().map(size).sum::<usize>() > CACHE_BYTES) {
        c.remove(0);
    }
}

/// How many times these contents were decoded at this pixel format (0 = never / evicted).
pub fn source_decode_count(bytes: &[u8], fmt: PixelFormat) -> u32 {
    let key = cache_key(bytes, fmt);
    SOURCE_CACHE.lock().ok().and_then(|c| c.iter().find(|e| e.key == key).map(|e| e.decodes)).unwrap_or(0)
}

fn buffer_to_surface(buf: &photocraft_compose::Buffer, fmt: PixelFormat) -> Surface {
    let n = fmt.channels();
    let mut data = vec![0.0f32; buf.px.len() * n];
    for (p, out) in buf.px.iter().zip(data.chunks_exact_mut(n)) {
        photocraft_raster::from_rgba_into(&fmt, *p, out);
    }
    let mut s = Surface::new(fmt);
    if !buf.rect.is_empty() {
        s.write_region(buf.rect, &data);
    }
    s.prune();
    s
}

/// The composited source image of `bytes` in pixel format `fmt` (decoded once, then cached).
pub fn source_image(file_name: &str, bytes: &[u8], fmt: PixelFormat) -> Result<SourceImage> {
    let key = cache_key(bytes, fmt);
    if let Some(img) = cache_get(&key) {
        return Ok(img);
    }
    let doc = decode_source(file_name, bytes)?;
    let buf = photocraft_compose::flatten(&doc);
    let img = SourceImage { surface: Arc::new(buffer_to_surface(&buf, fmt)), bounds: doc.bounds() };
    cache_put(key, img.clone());
    Ok(img)
}

/// Layer › Smart Objects › Stack Mode: the source's layers (or, when it holds a single group, the
/// group's layers) combined per pixel with `mode`. Cached like [`source_image`], keyed by mode.
pub fn stack_image(file_name: &str, bytes: &[u8], fmt: PixelFormat, mode: photocraft_doc::StackMode) -> Result<SourceImage> {
    use photocraft_algo::stack::Stat;
    let mut key = cache_key(bytes, fmt);
    key.0 = *blake3::hash(&[&key.0[..], b"stack:", mode.id().as_bytes()].concat()).as_bytes();
    if let Some(img) = cache_get(&key) {
        return Ok(img);
    }
    let doc = decode_source(file_name, bytes)?;
    let layers: &[Layer] = match doc.layers.as_slice() {
        [only] if only.is_group() => only.children().unwrap_or_default(),
        all => all,
    };
    let bounds = doc.bounds();
    let frames: Vec<Vec<[f32; 4]>> = layers
        .iter()
        .filter(|l| l.visible)
        .map(|l| {
            let mut one = l.clone();
            one.blend = BlendMode::Normal;
            one.clipped = false;
            photocraft_compose::render_layer(&one, bounds).px
        })
        .collect();
    let stat = match mode {
        photocraft_doc::StackMode::Entropy => Stat::Entropy,
        photocraft_doc::StackMode::Kurtosis => Stat::Kurtosis,
        photocraft_doc::StackMode::Maximum => Stat::Maximum,
        photocraft_doc::StackMode::Mean => Stat::Mean,
        photocraft_doc::StackMode::Median => Stat::Median,
        photocraft_doc::StackMode::Minimum => Stat::Minimum,
        photocraft_doc::StackMode::Range => Stat::Range,
        photocraft_doc::StackMode::Skewness => Stat::Skewness,
        photocraft_doc::StackMode::StandardDeviation => Stat::StandardDeviation,
        photocraft_doc::StackMode::Summation => Stat::Summation,
        photocraft_doc::StackMode::Variance => Stat::Variance,
    };
    let buf = photocraft_compose::Buffer { rect: bounds, px: photocraft_algo::stack::combine(&frames, stat) };
    let img = SourceImage { surface: Arc::new(buffer_to_surface(&buf, fmt)), bounds };
    cache_put(key, img.clone());
    Ok(img)
}

// ---------- rendering ----------

/// `top` blended over `base` with `mode` at `opacity` (a smart filter's blending options).
fn blend_surfaces(base: &Surface, top: &Surface, mode: BlendMode, opacity: f32) -> Surface {
    let fmt = base.format();
    let area = base.content_bounds().union(&top.content_bounds());
    let mut out = base.clone();
    if area.is_empty() {
        return out;
    }
    let n = fmt.channels();
    let (b, t) = (base.read_region(area), top.read_region(area));
    let mut o = vec![0.0f32; b.len()];
    for ((bp, tp), op) in b.chunks_exact(n).zip(t.chunks_exact(n)).zip(o.chunks_exact_mut(n)) {
        let rgba = photocraft_color::blend::composite(mode, photocraft_raster::to_rgba(&fmt, bp), photocraft_raster::to_rgba(&fmt, tp), opacity);
        photocraft_raster::from_rgba_into(&fmt, rgba, op);
    }
    out.write_region(area, &o);
    out.prune();
    out
}

/// Mixes `filtered` over `base` through the filter mask (premultiplied, any channel layout).
fn mask_mix(base: &Surface, filtered: &Surface, mask: &LayerMask) -> Surface {
    let fmt = base.format();
    let area = base.content_bounds().union(&filtered.content_bounds());
    let mut out = filtered.clone();
    if area.is_empty() {
        return out;
    }
    let n = fmt.channels();
    let (b, f) = (base.read_region(area), filtered.read_region(area));
    let mut k = Vec::new();
    mask.values_into(area, &mut k);
    let mut o = vec![0.0f32; b.len()];
    for (i, ((bp, fp), op)) in b.chunks_exact(n).zip(f.chunks_exact(n)).zip(o.chunks_exact_mut(n)).enumerate() {
        let m = k.get(i).copied().unwrap_or(1.0).clamp(0.0, 1.0);
        if !fmt.alpha {
            for c in 0..n {
                op[c] = bp[c] + (fp[c] - bp[c]) * m;
            }
            continue;
        }
        let a = n - 1;
        let oa = bp[a] + (fp[a] - bp[a]) * m;
        op[a] = oa;
        for c in 0..a {
            let pm = bp[c] * bp[a] + (fp[c] * fp[a] - bp[c] * bp[a]) * m;
            op[c] = if oa > 0.0 { (pm / oa).clamp(0.0, 1.0) } else { 0.0 };
        }
    }
    out.write_region(area, &o);
    out.prune();
    out
}

/// Runs the smart filter stack on the placed (unfiltered) content.
pub fn apply_smart_filters(placed: &Surface, sm: &SmartObject, canvas: Rect) -> Surface {
    if !sm.filters_enabled || !sm.smart_filters.iter().any(|f| f.visible) {
        return placed.clone();
    }
    let mut cur = placed.clone();
    // Edge pixels repeat at the canvas ∪ placed-content edge, as for layer filters.
    let extent = canvas.union(&placed.content_bounds());
    for f in sm.smart_filters.iter().filter(|f| f.visible) {
        // Unknown ids (e.g. Photoshop filters we don't implement) leave the pixels alone.
        let Some(out) = crate::filters::apply_filter_to_surface(&f.command, &f.params, &cur, canvas, None, extent) else { continue };
        cur = if f.blend == BlendMode::Normal && f.opacity >= 1.0 { out } else { blend_surfaces(&cur, &out, f.blend, f.opacity.clamp(0.0, 1.0)) };
    }
    match sm.filter_mask.as_ref().filter(|m| m.enabled) {
        Some(m) => mask_mix(placed, &cur, m),
        None => cur,
    }
}

/// Renders a smart object from its source. `Ok(None)` when the source is unavailable (a missing
/// linked file, or a PSD placed layer without embedded data): callers keep the existing cache.
pub fn render(doc: &Document, sm: &SmartObject) -> Result<Option<Surface>> {
    let Some((name, bytes)) = source_bytes(&doc.metadata, &sm.source) else { return Ok(None) };
    let img = match sm.stack_mode {
        Some(mode) => stack_image(&name, &bytes, doc.pixel_format(), mode)?,
        None => source_image(&name, &bytes, doc.pixel_format())?,
    };
    // Through the warp (source space) and the transform in one pass; whole-pixel moves are exact.
    let placed = photocraft_algo::warp::place_source(&img.surface, img.bounds, &sm.transform, sm.warp.as_ref());
    Ok(Some(apply_smart_filters(&placed, sm, doc.bounds())))
}

/// Re-renders smart layer `l` (which lives in `doc`) in place. Returns false if its source is
/// unavailable (the cache is then left alone).
pub fn refresh_layer(doc: &Document, l: &mut Layer) -> Result<bool> {
    let LayerContent::Smart(sm) = &mut l.content else { return Ok(false) };
    match render(doc, sm)? {
        Some(px) => {
            sm.cache = Some(px);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Re-renders the smart object `id` of `doc`.
pub fn refresh(doc: &mut Document, id: LayerId) -> Result<bool> {
    let mut l = doc.layer(id).cloned().ok_or(EngineError::NoLayer(id))?;
    if !matches!(l.content, LayerContent::Smart(_)) {
        return Err(other("the layer is not a smart object"));
    }
    let ok = refresh_layer(doc, &mut l)?;
    if ok && let Some(dst) = doc.layer_mut(id) {
        dst.content = l.content;
    }
    Ok(ok)
}

fn refresh_or_fail(doc: &mut Document, id: LayerId) -> Result<()> {
    if refresh(doc, id)? { Ok(()) } else { Err(other("the smart object's contents are unavailable (missing linked file?), so it can't be re-rendered")) }
}

fn smart(doc: &Document, id: LayerId) -> Result<&SmartObject> {
    match &doc.layer(id).ok_or(EngineError::NoLayer(id))?.content {
        LayerContent::Smart(sm) => Ok(sm),
        _ => Err(other("the layer is not a smart object")),
    }
}

fn smart_mut(doc: &mut Document, id: LayerId) -> Result<&mut SmartObject> {
    match &mut doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.content {
        LayerContent::Smart(sm) => Ok(sm),
        _ => Err(other("the layer is not a smart object")),
    }
}

fn mask_from_selection(sel: &Surface) -> LayerMask {
    LayerMask { surface: sel.clone(), enabled: true, linked: true, density: 1.0, feather: 0.0 }
}

/// Appends a smart filter (from a `filter.*` command) and re-renders. A selection becomes the
/// filter mask, as in Photoshop. Without a renderable source the filter is applied to the cached
/// pixels (and still recorded).
pub(crate) fn add_smart_filter(doc: &mut Document, id: LayerId, sf: photocraft_doc::SmartFilter, selection: Option<&Surface>) -> Result<()> {
    let canvas = doc.bounds();
    let renderable = source_bytes(&doc.metadata, &smart(doc, id)?.source).is_some();
    let sm = smart_mut(doc, id)?;
    if renderable {
        if let Some(sel) = selection
            && sm.filter_mask.is_none()
        {
            sm.filter_mask = Some(mask_from_selection(sel));
        }
        sm.smart_filters.push(sf);
        return refresh_or_fail(doc, id);
    }
    let bounds = selection.map(Surface::content_bounds).filter(|b| !b.is_empty()).unwrap_or(canvas);
    let cache = sm.cache.as_ref().ok_or_else(|| other("smart object has no pixels"))?;
    let out = crate::filters::apply_filter_to_surface(&sf.command, &sf.params, cache, bounds, selection, canvas)
        .ok_or_else(|| other(format!("unknown filter {}", sf.command)))?;
    sm.cache = Some(out);
    sm.smart_filters.push(sf);
    Ok(())
}

/// Rounds float noise off a composed transform (scale 0.1 then 10 should be exactly 1), so
/// repeated transforms land back on exact, lossless placements.
pub(crate) fn snap_affine(a: Affine) -> Affine {
    Affine { m: a.m.map(|v| if (v - v.round()).abs() < 1e-9 { v.round() } else { v }) }
}

/// Moves a smart object by whole pixels without re-rendering.
pub(crate) fn shift_smart(sm: &mut SmartObject, dx: i32, dy: i32) {
    sm.transform = Affine::translate(dx as f64, dy as f64).mul(&sm.transform);
    if let Some(c) = &mut sm.cache {
        *c = crate::layer_multi_cmds::shift_surface(c, dx, dy);
    }
    if let Some(m) = &mut sm.filter_mask {
        m.surface = crate::layer_multi_cmds::shift_surface(&m.surface, dx, dy);
    }
}

/// Forget PSD placed-layer data that no longer describes the smart object (its source changed):
/// PSD export then writes fresh placed-layer blocks and embeds the new source.
fn detach_psd(l: &mut Layer) {
    if let LayerContent::Smart(sm) = &mut l.content {
        sm.psd_raw = None;
    }
    l.psd_blocks.retain(|(k, _)| !PLACED_KEYS.contains(&k));
}

// ---------- conversion ----------

/// Moves a layer (any kind) by whole pixels without re-rendering anything.
fn shift_layer(l: &mut Layer, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    let a = Affine::translate(dx as f64, dy as f64);
    let mv = |s: &Surface| translate_surface(s, dx, dy);
    if let Some(m) = &mut l.mask {
        m.surface = mv(&m.surface);
    }
    if let Some(vm) = &mut l.vector_mask {
        vm.path = vm.path.transform(&a);
    }
    if let Some(fc) = &mut l.fill_cache {
        fc.surface = mv(&fc.surface);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = mv(s),
        LayerContent::Text(t) => {
            t.transform = a.mul(&t.transform);
            if let Some(c) = &mut t.cache {
                *c = mv(c);
            }
        }
        LayerContent::Shape(sh) => {
            crate::vector_cmds::transform_shape(sh, &a);
            if let Some(c) = &mut sh.cache {
                *c = mv(c);
            }
        }
        LayerContent::Smart(sm) => shift_smart(sm, dx, dy),
        LayerContent::Group(g) => g.children.iter_mut().for_each(|c| shift_layer(c, dx, dy)),
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => {}
    }
}

fn subtree_bounds(l: &Layer, canvas: Rect) -> Rect {
    match &l.content {
        LayerContent::Group(g) => g.children.iter().map(|c| subtree_bounds(c, canvas)).fold(Rect::EMPTY, |a, b| a.union(&b)),
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => canvas,
        _ => l.surface().map_or(Rect::EMPTY, Surface::content_bounds),
    }
}

fn any_layer(l: &Layer, f: &dyn Fn(&Layer) -> bool) -> bool {
    f(l) || l.children().is_some_and(|c| c.iter().any(|c| any_layer(c, f)))
}

/// Builds the smart-object layer replacing `l` in `doc`: the layer (with its mask, effects and
/// fill opacity) becomes a nested document cropped to its rendered bounds; visibility, opacity,
/// blend mode, clipping and label stay on the smart layer. The cache is the exact composite, so
/// the document looks the same before and after.
pub fn layer_to_smart(doc: &Document, l: &Layer) -> Result<Layer> {
    let canvas = doc.bounds();
    let background = l.name == "Background" && l.locks.position;
    let mut inner = l.clone();
    inner.visible = true;
    inner.opacity = 1.0;
    inner.clipped = false;
    inner.locks = Default::default();
    if !(inner.is_group() && inner.blend == BlendMode::PassThrough) {
        inner.blend = BlendMode::Normal;
    }
    if background {
        inner.name = "Layer 0".into();
    }
    let mut sub = Document::new(format!("{}.pcraft", l.name), doc.size, doc.mode, doc.depth);
    sub.resolution_dpi = doc.resolution_dpi;
    sub.icc_profile = doc.icc_profile.clone();
    sub.global_light = doc.global_light;
    if any_layer(l, &|x| matches!(x.content, LayerContent::Smart(_))) {
        // Nested PSD placed layers find their embedded files here.
        sub.metadata.psd_global_blocks = doc.metadata.psd_global_blocks.clone();
    }
    sub.layers = vec![inner];

    let has_fx = any_layer(l, &|x| x.effects.enabled && !x.effects.items.is_empty());
    let region = subtree_bounds(l, canvas).union(&canvas).inflate(if has_fx { 256 } else { 0 });
    let buf = photocraft_compose::render(&sub, region);
    let w = region.width() as usize;
    let mut b = Rect::EMPTY;
    for (i, p) in buf.px.iter().enumerate() {
        if p[3] > 0.0 {
            let (x, y) = (region.x0 + (i % w) as i32, region.y0 + (i / w) as i32);
            b = b.union(&Rect::new(x, y, x + 1, y + 1));
        }
    }
    if b.is_empty() {
        return Err(other("the layer is empty, so there is nothing to convert"));
    }
    let fmt = doc.pixel_format();
    let mut cache = buffer_to_surface(&buf, fmt);
    cache = photocraft_algo::resample::crop_surface(&cache, b);
    cache.prune();

    shift_layer(&mut sub.layers[0], -b.x0, -b.y0);
    sub.size = Size::new(b.width(), b.height());
    let bytes = encode_source(&sub)?;
    // Seed the decode cache with the composite we already have.
    cache_put(cache_key(&bytes, fmt), SourceImage { surface: Arc::new(translate_surface(&cache, -b.x0, -b.y0)), bounds: sub.bounds() });

    let source = SmartSource::Embedded { file_name: sub.name.clone(), bytes: Arc::new(bytes) };
    let mut out = Layer::new(
        if background { "Layer 0".to_string() } else { l.name.clone() },
        LayerContent::Smart(SmartObject::new(source, Affine::translate(b.x0 as f64, b.y0 as f64), Some(cache))),
    );
    out.visible = l.visible;
    out.opacity = l.opacity;
    out.blend = if l.blend == BlendMode::PassThrough { BlendMode::Normal } else { l.blend };
    out.clipped = l.clipped;
    out.label = l.label;
    if !background {
        out.locks = l.locks;
    }
    Ok(out)
}

fn convert(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit("Convert to Smart Object", |doc, active| {
        let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let so = layer_to_smart(doc, l)?;
        let new_id = so.id;
        let path = doc.path_of(id).ok_or(EngineError::NoLayer(id))?;
        *doc.layer_at_mut(&path).ok_or(EngineError::NoLayer(id))? = so;
        *active = Some(new_id);
        Ok(json!({"layer": new_id.0}))
    })
}

// ---------- contents ----------

fn via_copy(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit("New Smart Object via Copy", |doc, active| {
        let src = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let mut copy = src.duplicate();
        copy.name = format!("{} copy", src.name);
        if let LayerContent::Smart(sm) = &mut copy.content {
            // Independent contents: resolve to embedded bytes and drop the shared PSD uuid.
            if let Some((file_name, bytes)) = source_bytes(&doc.metadata, &sm.source) {
                sm.source = SmartSource::Embedded { file_name, bytes };
            }
        } else {
            return Err(other("the layer is not a smart object"));
        }
        detach_psd(&mut copy);
        let new_id = doc.insert_above(Some(id), copy);
        *active = Some(new_id);
        Ok(json!({"layer": new_id.0}))
    })
}

fn path_param<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    p.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| bad(cmd, "pass `path`"))
}

/// Swaps a smart object's source, keeping its transform and filters, and re-renders.
fn set_source(s: &mut Session, p: &Value, label: &str, keep_psd: bool, make: impl FnOnce(&Metadata, &SmartSource) -> Result<SmartSource>) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit(label, |doc, _| {
        let new = make(&doc.metadata, &smart(doc, id)?.source)?;
        smart_mut(doc, id)?.source = new;
        if !keep_psd {
            detach_psd(doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?);
        }
        refresh_or_fail(doc, id)?;
        Ok(json!({"layer": id.0}))
    })
}

fn replace_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_param("layer.smartObjects.replaceContents", p)?.to_string();
    let bytes = std::fs::read(&path).map_err(|e| other(format!("can't read {path}: {e}")))?;
    let name = base_name(&path);
    decode_source(&name, &bytes)?; // fail before touching the document
    set_source(s, p, "Replace Contents", false, |_, _| Ok(SmartSource::Embedded { file_name: name, bytes: Arc::new(bytes) }))
}

fn export_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_param("layer.smartObjects.exportContents", p)?;
    let id = layer_param(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (name, bytes) = source_bytes(&st.doc.metadata, &smart(&st.doc, id)?.source).ok_or_else(|| other("the smart object's contents are unavailable"))?;
    crate::file_cmds::write_file(path, &bytes)?;
    Ok(json!({"path": path, "fileName": name, "bytes": bytes.len()}))
}

fn edit_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let parent = st.doc.id;
    let (name, bytes) = source_bytes(&st.doc.metadata, &smart(&st.doc, id)?.source).ok_or_else(|| other("the smart object's contents are unavailable"))?;
    let mut child = decode_source(&name, &bytes)?;
    // Bundles keep their document id; each open copy needs its own.
    child.id = DocId::fresh();
    child.name = name;
    let index = s.add_document(child, None);
    // Admission may replace an ID already owned by another open document.
    let child_id = s.documents().get(index).ok_or(EngineError::NoDocument)?.doc.id;
    s.smart_links.retain(|l| l.child != child_id);
    s.smart_links.push(SmartLink { child: child_id, parent, layer: id });
    Ok(json!({"document": index, "parentLayer": id.0}))
}

/// Writes an Edit Contents document back into its parent smart object (one undoable step in the
/// parent) and marks it saved. Returns false if `index` isn't an Edit Contents document.
pub fn commit_child(s: &mut Session, index: usize) -> Result<bool> {
    let Some(st) = s.docs.get(index) else { return Ok(false) };
    let child = st.doc.clone();
    let Some(link) = s.smart_links.iter().find(|l| l.child == child.id).copied() else { return Ok(false) };
    let parent = s.docs.iter().position(|d| d.doc.id == link.parent).ok_or_else(|| other("the smart object's document was closed"))?;
    let bytes = Arc::new(encode_source(&child)?);
    let prev = s.active;
    s.active = Some(parent);
    let r = s.edit("Edit Contents", |doc, _| {
        let sm = smart_mut(doc, link.layer)?;
        let stem = match &sm.source {
            SmartSource::Embedded { file_name, .. } => file_name.rsplit_once('.').map_or(file_name.as_str(), |(a, _)| a).to_string(),
            SmartSource::Linked { .. } => child.name.rsplit_once('.').map_or(child.name.as_str(), |(a, _)| a).to_string(),
        };
        sm.source = SmartSource::Embedded { file_name: format!("{stem}.pcraft"), bytes: bytes.clone() };
        detach_psd(doc.layer_mut(link.layer).ok_or(EngineError::NoLayer(link.layer))?);
        refresh_or_fail(doc, link.layer)
    });
    s.active = prev;
    r?;
    if let Some(st) = s.docs.get_mut(index) {
        st.saved_revision = st.revision;
    }
    Ok(true)
}

/// Called before a document closes: an edited Edit Contents document updates its parent.
pub(crate) fn on_close(s: &mut Session, index: usize) {
    let Some(st) = s.docs.get(index) else { return };
    let id = st.doc.id;
    if st.is_dirty() && s.smart_links.iter().any(|l| l.child == id) {
        let _ = commit_child(s, index);
    }
    s.smart_links.retain(|l| l.child != id && l.parent != id);
}

fn is_smart_child(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if s.smart_links.iter().any(|l| l.child == d.doc.id) { Ok(()) } else { Err("this document isn't smart object contents".into()) }
}

fn update_all(s: &mut Session) -> Result<Value> {
    let ids: Vec<LayerId> = s
        .active()
        .ok_or(EngineError::NoDocument)?
        .doc
        .walk()
        .into_iter()
        .filter(|(_, _, l)| matches!(&l.content, LayerContent::Smart(sm) if matches!(sm.source, SmartSource::Linked { .. })))
        .map(|(_, _, l)| l.id)
        .collect();
    let mut updated = Vec::new();
    s.edit("Update All Modified Content", |doc, _| {
        for id in &ids {
            if refresh(doc, *id)? {
                updated.push(id.0);
            }
        }
        Ok(())
    })?;
    Ok(json!({"updated": updated}))
}

// ---------- smart filter editing ----------

fn edit_filters(s: &mut Session, p: &Value, label: &str, f: impl FnOnce(&mut SmartObject) -> Result<()>) -> Result<Value> {
    let id = layer_param(s, p)?;
    s.edit(label, |doc, _| {
        f(smart_mut(doc, id)?)?;
        refresh_or_fail(doc, id)?;
        Ok(json!({"layer": id.0}))
    })
}

fn filter_index(cmd: &str, p: &Value, sm: &SmartObject) -> Result<usize> {
    let n = sm.smart_filters.len();
    if n == 0 {
        return Err(other("the smart object has no smart filters"));
    }
    match p.get("index") {
        None => Ok(n - 1),
        Some(v) => v.as_u64().map(|i| i as usize).filter(|i| *i < n).ok_or_else(|| bad(cmd, format!("`index` must be 0..{n} (0 = bottom filter)"))),
    }
}

fn set_filter_visible(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.smartFilter.setVisible";
    edit_filters(s, p, "Smart Filter Visibility", |sm| {
        let i = filter_index(CMD, p, sm)?;
        let f = &mut sm.smart_filters[i];
        f.visible = p.get("visible").and_then(Value::as_bool).unwrap_or(!f.visible);
        Ok(())
    })
}

fn set_filter_params(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.smartFilter.setParams";
    let new = p.get("params").cloned().ok_or_else(|| bad(CMD, "pass `params` (merged into the filter's parameters)"))?;
    edit_filters(s, p, "Edit Smart Filter", |sm| {
        let i = filter_index(CMD, p, sm)?;
        let f = &mut sm.smart_filters[i];
        if f.command == photocraft_io::smart_map::UNSUPPORTED_FILTER {
            return Err(bad(CMD, "this Photoshop filter isn't implemented in PhotoCraft: it is kept as is (it can be hidden, moved or deleted)"));
        }
        match (&mut f.params, new) {
            (Value::Object(old), Value::Object(n)) => old.extend(n),
            (slot, n) => *slot = n,
        }
        Ok(())
    })
}

fn blending_options(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.smartFilter.blendingOptions";
    let blend = match p.get("blend").and_then(Value::as_str) {
        Some(b) => Some(blend_from_str(b).filter(|m| *m != BlendMode::PassThrough).ok_or_else(|| bad(CMD, format!("unknown blend mode `{b}`")))?),
        None => None,
    };
    let opacity = p.get("opacity").and_then(Value::as_f64).map(|o| if o > 1.0 { o / 100.0 } else { o } as f32);
    edit_filters(s, p, "Smart Filter Blending Options", |sm| {
        let i = filter_index(CMD, p, sm)?;
        let f = &mut sm.smart_filters[i];
        if let Some(b) = blend {
            f.blend = b;
        }
        if let Some(o) = opacity {
            f.opacity = o.clamp(0.0, 1.0);
        }
        Ok(())
    })
}

fn delete_filter(s: &mut Session, p: &Value) -> Result<Value> {
    edit_filters(s, p, "Delete Smart Filter", |sm| {
        let i = filter_index("layer.smartFilter.delete", p, sm)?;
        sm.smart_filters.remove(i);
        Ok(())
    })
}

fn move_filter(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.smartFilter.move";
    edit_filters(s, p, "Move Smart Filter", |sm| {
        let i = filter_index(CMD, p, sm)?;
        let n = sm.smart_filters.len();
        let to = p.get("to").and_then(Value::as_u64).map(|t| t as usize).filter(|t| *t < n).ok_or_else(|| bad(CMD, format!("`to` must be 0..{n}")))?;
        let f = sm.smart_filters.remove(i);
        sm.smart_filters.insert(to, f);
        Ok(())
    })
}

// ---------- enablement ----------

fn active_smart(s: &Session) -> std::result::Result<&SmartObject, String> {
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    match &d.doc.layer(id).ok_or("no active layer")?.content {
        LayerContent::Smart(sm) => Ok(sm),
        other => Err(format!("the active layer is a {} layer, not a smart object", other.kind_name())),
    }
}
fn has_smart(s: &Session) -> std::result::Result<(), String> {
    active_smart(s).map(|_| ())
}
fn has_linked(s: &Session) -> std::result::Result<(), String> {
    match active_smart(s)?.source {
        SmartSource::Linked { .. } => Ok(()),
        _ => Err("the smart object is embedded".into()),
    }
}
fn has_smart_filters(s: &Session) -> std::result::Result<(), String> {
    if active_smart(s)?.smart_filters.is_empty() { Err("the smart object has no smart filters".into()) } else { Ok(()) }
}
fn has_filter_mask(s: &Session) -> std::result::Result<(), String> {
    active_smart(s)?.filter_mask.as_ref().map(|_| ()).ok_or_else(|| "the smart object has no filter mask".into())
}
fn convertible(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    d.doc.layer(id).map(|_| ()).ok_or_else(|| "no active layer".into())
}
fn not_smart(s: &Session) -> std::result::Result<(), String> {
    convertible(s)?;
    if active_smart(s).is_ok() { Err("the layer is already a smart object".into()) } else { Ok(()) }
}

macro_rules! spec {
    ($id:literal, $label:literal, $menu:expr, $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: $menu, shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

const SO: &[&str] = &["Layer", "Smart Objects"];
const SF: &[&str] = &["Layer", "Smart Filter"];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!("layer.smartObjects.convertToSmartObject", "Convert to Smart Object", SO, r##"{"layer":id?}"##, convertible, convert),
        spec!("filter.convertForSmartFilters", "Convert for Smart Filters", &["Filter"], r##"{"layer":id?}"##, not_smart, convert),
        spec!("layer.smartObjects.newSmartObjectViaCopy", "New Smart Object via Copy", SO, r##"{"layer":id?}"##, has_smart, via_copy),
        spec!("layer.smartObjects.rasterize", "Rasterize", SO, r##"{"layer":id?}"##, has_smart, |s, p| s.execute("layer.rasterize.smartObject", p.clone())),
        spec!(
            "layer.smartObjects.editContents",
            "Edit Contents",
            SO,
            r##"{"layer":id?} → opens the contents as a new document; saving (layer.smartObjects.saveContents) or closing it updates the smart object"##,
            has_smart,
            edit_contents
        ),
        spec!("layer.smartObjects.saveContents", "Save Contents", &[], "{} (in an Edit Contents document)", is_smart_child, |s, _| {
            let i = s.active_index().ok_or(EngineError::NoDocument)?;
            Ok(json!({"updated": commit_child(s, i)?}))
        }),
        spec!("layer.smartObjects.replaceContents", "Replace Contents…", SO, r##"{"layer":id?,"path":str}"##, has_smart, replace_contents),
        spec!("layer.smartObjects.exportContents", "Export Contents…", SO, r##"{"layer":id?,"path":str}"##, has_smart, export_contents),
        spec!("layer.smartObjects.relinkToFile", "Relink to File…", SO, r##"{"layer":id?,"path":str}"##, has_smart, |s, p| {
            let path = path_param("layer.smartObjects.relinkToFile", p)?.to_string();
            set_source(s, p, "Relink to File", false, |_, _| Ok(SmartSource::Linked { path }))
        }),
        spec!("layer.smartObjects.updateModifiedContent", "Update Modified Content", SO, r##"{"layer":id?}"##, has_linked, |s, p| {
            let id = layer_param(s, p)?;
            s.edit("Update Modified Content", |doc, _| refresh_or_fail(doc, id))?;
            Ok(json!({"layer": id.0}))
        }),
        spec!(
            "layer.smartObjects.updateAllModifiedContent",
            "Update All Modified Content",
            SO,
            "{}",
            |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            |s, _| update_all(s)
        ),
        spec!("layer.smartObjects.convertToEmbedded", "Convert to Embedded", SO, r##"{"layer":id?}"##, has_linked, |s, p| {
            set_source(s, p, "Convert to Embedded", true, |meta, src| {
                let (file_name, bytes) = source_bytes(meta, src).ok_or_else(|| other("the linked file can't be read"))?;
                Ok(SmartSource::Embedded { file_name, bytes })
            })
        }),
        spec!(
            "layer.smartObjects.convertToLinked",
            "Convert to Linked…",
            SO,
            r##"{"layer":id?,"path":str} (writes the contents there)"##,
            has_smart,
            |s, p| {
                let path = path_param("layer.smartObjects.convertToLinked", p)?.to_string();
                set_source(s, p, "Convert to Linked", false, |meta, src| {
                    let (_, bytes) = source_bytes(meta, src).ok_or_else(|| other("the smart object's contents are unavailable"))?;
                    crate::file_cmds::write_file(&path, &bytes)?;
                    Ok(SmartSource::Linked { path })
                })
            }
        ),
        // Smart filters
        spec!(
            "layer.smartFilter.disableSmartFilters",
            "Disable Smart Filters",
            SF,
            r##"{"layer":id?,"enabled":bool? (default: toggle)}"##,
            has_smart_filters,
            |s, p| {
                edit_filters(s, p, "Disable Smart Filters", |sm| {
                    sm.filters_enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(!sm.filters_enabled);
                    Ok(())
                })
            }
        ),
        spec!("layer.smartFilter.deleteFilterMask", "Delete Filter Mask", SF, r##"{"layer":id?}"##, has_filter_mask, |s, p| {
            edit_filters(s, p, "Delete Filter Mask", |sm| {
                sm.filter_mask = None;
                Ok(())
            })
        }),
        spec!(
            "layer.smartFilter.disableFilterMask",
            "Disable Filter Mask",
            SF,
            r##"{"layer":id?,"enabled":bool? (default: toggle)}"##,
            has_filter_mask,
            |s, p| {
                edit_filters(s, p, "Disable Filter Mask", |sm| {
                    if let Some(m) = &mut sm.filter_mask {
                        m.enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(!m.enabled);
                    }
                    Ok(())
                })
            }
        ),
        spec!(
            "layer.smartFilter.blendingOptions",
            "Blending Options…",
            SF,
            r##"{"layer":id?,"index":u32? (0 = bottom; default top),"blend":str?,"opacity":0..1?}"##,
            has_smart_filters,
            blending_options
        ),
        spec!("layer.smartFilter.clearSmartFilters", "Clear Smart Filters", SF, r##"{"layer":id?}"##, has_smart_filters, |s, p| {
            edit_filters(s, p, "Clear Smart Filters", |sm| {
                sm.smart_filters.clear();
                sm.filter_mask = None;
                Ok(())
            })
        }),
        spec!(
            "layer.smartFilter.setVisible",
            "Show/Hide Smart Filter",
            &[],
            r##"{"layer":id?,"index":u32?,"visible":bool? (default: toggle)}"##,
            has_smart_filters,
            set_filter_visible
        ),
        spec!(
            "layer.smartFilter.setParams",
            "Edit Smart Filter",
            &[],
            r##"{"layer":id?,"index":u32?,"params":{…} (merged)}"##,
            has_smart_filters,
            set_filter_params
        ),
        spec!("layer.smartFilter.delete", "Delete Smart Filter", &[], r##"{"layer":id?,"index":u32?}"##, has_smart_filters, delete_filter),
        spec!("layer.smartFilter.move", "Move Smart Filter", &[], r##"{"layer":id?,"index":u32?,"to":u32}"##, has_smart_filters, move_filter),
    ]
}

#[cfg(test)]
mod tests;

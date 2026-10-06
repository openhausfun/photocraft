//! Planner: walks the layer tree into a linear list of GPU passes over abstract buffer slots.
//!
//! The structure mirrors `photocraft_compose::composite_stack` / `composite_layer` /
//! `composite_atop` one to one, so both backends share Photoshop semantics (pass-through vs
//! isolated groups, clipping, masks, opacity × fill, adjustments, layer effects). Every pass
//! reads slots and writes a fresh slot; slots are recycled as soon as nothing refers to them.
//!
//! Layer effects follow `photocraft_compose::effects::composite_with_effects`: the layer's
//! effect maps (shadow / glow / satin / bevel coverage, stroke bands) are built once per layer
//! state by [`crate::fx`] and sampled here; exterior effects paint into a copy of the backdrop,
//! the layer at fill opacity takes the interior effects, and the two merge with the layer's mode
//! and opacity. Effect passes are clipped to the layer's effect region and the result is copied
//! back into the backdrop in place, so a small text layer costs only its own pixels.

use photocraft_color::BlendMode;
use photocraft_compose::adjust::{self, Transfer};
use photocraft_compose::effects::has_effects;
use photocraft_doc::adjust::ToneSpace;
use photocraft_doc::{Adjustment, Document, Effect, Fill, FxPaint, GlobalLight, Gradient, Layer, LayerContent, LayerId, Pattern, StrokePosition};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

use crate::bounds;

/// Index of a chunk-sized accumulator texture.
pub type Slot = u32;

/// Pipeline (fragment entry point) of a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kernel {
    /// Transparent fill (no draw).
    Clear,
    Content,
    Mask,
    Blend,
    Atop,
    Adjust,
    AdjMix,
    Lerp,
    /// Effect chain start: the layer at fill opacity, or an opaque copy of a clipping base.
    FxInit,
    /// Paint an effect through a coverage map (or the layer's alpha) into A.
    FxPaint,
    /// Layer + interior effects over the exterior result, then layer opacity against the backdrop.
    FxMerge,
    /// Outside strokes: accumulate one stroke's share (`kind` 0, premultiplied colour) or its
    /// coverage (`kind` 1).
    FxStroke,
    /// Outside strokes: resolve the accumulated strokes over the exterior result.
    FxStrokeEnd,
    /// Copy slot A into `dst` (an existing slot) over the pass clip (no draw).
    CopyRect,
    /// Copy all of slot A into the fresh slot `dst` (no draw).
    CopyFull,
    // Effect-map kernels (single-channel targets over an effect region, see `fx`).
    MShift,
    MDilate,
    MBlur,
    MGlow,
    MFinish,
    MBevelH,
    MBevelShade,
    MStroke,
    MBevelTex,
}

impl Kernel {
    /// Fragment entry point in `compose.wgsl` (`None` for copies and clears).
    pub fn entry(self) -> Option<&'static str> {
        Some(match self {
            Kernel::Clear | Kernel::CopyRect | Kernel::CopyFull => return None,
            Kernel::Content => "fs_content",
            Kernel::Mask => "fs_mask",
            Kernel::Blend => "fs_blend",
            Kernel::Atop => "fs_atop",
            Kernel::Adjust => "fs_adjust",
            Kernel::AdjMix => "fs_adjmix",
            Kernel::Lerp => "fs_lerp",
            Kernel::FxInit => "fs_fxinit",
            Kernel::FxPaint => "fs_fxpaint",
            Kernel::FxMerge => "fs_fxmerge",
            Kernel::FxStroke => "fs_fxstroke",
            Kernel::FxStrokeEnd => "fs_fxstrokeend",
            Kernel::MShift => "fs_mshift",
            Kernel::MDilate => "fs_mdilate",
            Kernel::MBlur => "fs_mblur",
            Kernel::MGlow => "fs_mglow",
            Kernel::MFinish => "fs_mfinish",
            Kernel::MBevelH => "fs_mbevelh",
            Kernel::MBevelShade => "fs_mbevelshade",
            Kernel::MStroke => "fs_mstroke",
            Kernel::MBevelTex => "fs_mbeveltex",
        })
    }

    /// Kernels that write effect maps (R32F / R16F targets) rather than RGBA accumulators.
    pub fn is_map(self) -> bool {
        matches!(
            self,
            Kernel::MShift
                | Kernel::MDilate
                | Kernel::MBlur
                | Kernel::MGlow
                | Kernel::MFinish
                | Kernel::MBevelH
                | Kernel::MBevelShade
                | Kernel::MStroke
                | Kernel::MBevelTex
        )
    }

    /// Every kernel with a pipeline.
    pub const DRAWN: [Kernel; 21] = [
        Kernel::Content,
        Kernel::Mask,
        Kernel::Blend,
        Kernel::Atop,
        Kernel::Adjust,
        Kernel::AdjMix,
        Kernel::Lerp,
        Kernel::FxInit,
        Kernel::FxPaint,
        Kernel::FxMerge,
        Kernel::FxStroke,
        Kernel::FxStrokeEnd,
        Kernel::MShift,
        Kernel::MDilate,
        Kernel::MBlur,
        Kernel::MGlow,
        Kernel::MFinish,
        Kernel::MBevelH,
        Kernel::MBevelShade,
        Kernel::MStroke,
        Kernel::MBevelTex,
    ];
}

/// Which resident texture a pass samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Content,
    Mask,
    /// A stroked shape's stroke alone (its fill uses `Content`).
    Stroke,
}

/// A surface the pass needs on the GPU.
#[derive(Clone, Debug)]
pub struct TexUse<'a> {
    pub layer: LayerId,
    pub role: Role,
    pub surface: SurfaceRef<'a>,
}

/// An effect map sampled by a pass: map `map` of enabled effect `item` of `plan.fx[fx]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRef {
    pub fx: usize,
    pub item: usize,
    pub map: usize,
}

#[derive(Clone, Debug)]
pub struct Pass<'a> {
    pub kernel: Kernel,
    pub dst: Slot,
    pub a: Option<Slot>,
    pub b: Option<Slot>,
    pub c: Option<Slot>,
    /// A fourth slot, bound in place of `tex` (passes without layer pixels).
    pub d: Option<Slot>,
    pub mode: BlendMode,
    pub opacity: f32,
    /// Layer pixels (raster / text / shape / smart cache / fill cache).
    pub tex: Option<TexUse<'a>>,
    /// Colour outside `tex` (the surface's default pixel), the solid fill colour, or the effect
    /// colour.
    pub color: [f32; 4],
    pub mask: Option<MaskUse<'a>>,
    /// Adjustment kind (see `adjust` in compose.wgsl) and parameters; the coverage kind of an
    /// effect paint.
    pub adjust_kind: i32,
    pub params: [[f32; 4]; 4],
    pub extra: [f32; 4],
    /// Extra shader flags (`F_KNOCKOUT`, …).
    pub flags: u32,
    /// 4096-entry LUT rows (Levels / Curves / Gradient map / gradient stops).
    pub lut: Option<Vec<[f32; 4096]>>,
    pub gradient: bool,
    /// Effect map sampled by `FxPaint`.
    pub map: Option<MapRef>,
    /// Pattern painted by `FxPaint`.
    pub pattern: Option<&'a Pattern>,
    /// Only pixels inside this rect (document coordinates) are computed; the pass is skipped for
    /// chunks it misses. Consumers of the slot must be clipped alike.
    pub clip: Option<Rect>,
}

/// Pixels a pass samples: a surface of the document, or one derived from it on the CPU (a
/// combined pixel × vector mask, a shape's fill or stroke alone).
#[derive(Clone, Debug)]
pub enum SurfaceRef<'a> {
    Doc(&'a Surface),
    Derived(std::sync::Arc<Surface>),
}

impl SurfaceRef<'_> {
    pub fn get(&self) -> &Surface {
        match self {
            SurfaceRef::Doc(s) => s,
            SurfaceRef::Derived(s) => s,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MaskUse<'a> {
    pub layer: LayerId,
    pub surface: SurfaceRef<'a>,
    pub density: f32,
    pub default: f32,
}

impl<'a> Pass<'a> {
    pub(crate) fn new(kernel: Kernel, dst: Slot) -> Self {
        Pass {
            kernel,
            dst,
            a: None,
            b: None,
            c: None,
            d: None,
            mode: BlendMode::Normal,
            opacity: 1.0,
            tex: None,
            color: [0.0; 4],
            mask: None,
            adjust_kind: 0,
            params: [[0.0; 4]; 4],
            extra: [0.0; 4],
            flags: 0,
            lut: None,
            gradient: false,
            map: None,
            pattern: None,
            clip: None,
        }
    }
}

/// Why a document can't be composited on the GPU (the caller falls back to the CPU compositor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GPU compositor: {}", self.0)
    }
}

impl std::error::Error for Unsupported {}

/// A layer whose effects the plan renders: its maps are built (or reused) before the chunks run.
#[derive(Clone, Copy, Debug)]
pub struct FxLayer<'a> {
    pub layer: &'a Layer,
    /// Region the maps cover (`compose::effect_maps`).
    pub region: Rect,
    /// Layer bounds (gradients and linked patterns are laid out in them).
    pub bounds: Rect,
}

#[derive(Debug)]
pub struct Plan<'a> {
    pub passes: Vec<Pass<'a>>,
    pub slots: u32,
    pub root: Slot,
    /// Effect layers in dependency order (a group's inner effect layers come first).
    pub fx: Vec<FxLayer<'a>>,
}

/// Document-level inputs of a plan.
#[derive(Clone, Copy)]
pub struct DocCtx<'a> {
    pub canvas: Rect,
    pub transfer: Transfer,
    pub light: GlobalLight,
    pub patterns: &'a [Pattern],
    /// Colour mode (channel restrictions name its channels).
    pub mode: photocraft_color::ColorMode,
    /// Sample depth (adjustment results are rounded to it).
    pub depth: photocraft_color::SampleType,
    /// The document (effect geometry the CPU computes, e.g. gradient stroke frames).
    pub doc: &'a Document,
}

impl<'a> DocCtx<'a> {
    pub fn of(doc: &'a Document) -> Self {
        DocCtx {
            canvas: doc.bounds(),
            transfer: Transfer::for_mode(doc.mode),
            light: doc.global_light,
            patterns: &doc.patterns,
            mode: doc.mode,
            depth: doc.depth,
            doc,
        }
    }
}

/// Build the pass list for `doc`.
pub fn plan(doc: &Document) -> Result<Plan<'_>, Unsupported> {
    if doc.mode == photocraft_color::ColorMode::Multichannel {
        return Err(Unsupported("Multichannel inks (printed on the CPU)".into()));
    }
    let mut p = Planner::new(DocCtx::of(doc));
    let root = p.clear();
    // Start at the topmost layer that hides everything beneath it (an opaque fill layer): the
    // layers below can't change the result, so they cost no passes or uploads.
    let start = doc.layers.iter().rposition(|l| photocraft_compose::occludes_below(l, doc.mode)).unwrap_or(0);
    let root = p.stack(doc.layers.get(start..).unwrap_or(&doc.layers), root)?;
    Ok(p.finish(root))
}

struct Planner<'a> {
    passes: Vec<Pass<'a>>,
    free: Vec<Slot>,
    refs: Vec<u32>,
    cx: DocCtx<'a>,
    fx: Vec<FxLayer<'a>>,
}

/// Coverage source of an effect paint (`kind` in `fs_fxpaint`); flags refine it.
#[derive(Clone, Copy)]
enum Cov {
    /// An effect map; `outside` beyond its region.
    Map(MapRef, f32),
    /// Full coverage.
    One,
}

/// Shader flags for effect passes (keep in sync with compose.wgsl). Coverage `m` of a paint, with
/// `a` the layer's alpha and `inside` = `a > 0.5/255`:
/// knockout `m × (1 − a × k)` (k in `p4.w`); gate `inside ? m : 0`; rel `inside ? min(m / a, 1) : 0`;
/// stroke-out `inside ? (vector ? 0 : 1) : m`.
pub const F_KNOCKOUT: u32 = 16;
/// Effect merge: A already holds the layer; only mix with the backdrop by the layer's opacity.
pub const F_NO_LAYER: u32 = 32;
/// Outside strokes along a filled shape's outline (`effects::outline_share`; the coverage slot
/// carries the layer's alpha in `.g`).
pub const F_OUTLINE: u32 = 131072;
/// Shape layer (outside strokes never show inside it).
pub const F_VECTOR: u32 = 64;
/// Merge onto an opaque clipping base, keeping its alpha.
pub const F_ATOP: u32 = 128;
pub const F_GATE: u32 = 256;
pub const F_REL: u32 = 512;
pub const F_STROKE_OUT: u32 = 1024;
/// First outside stroke (no accumulated share yet).
pub const F_FIRST: u32 = 2048;
/// `Lerp` per channel: weights in `p0` (channel restrictions), no opacity or mask.
pub const F_CHANNELS: u32 = 4096;
/// Lab document: Normal blending mixes in CIELAB (`psblend::LAB_MIX`).
pub const F_LAB: u32 = 65536;
/// `Lerp`: A rounded to `p0.x` steps per unit (adjustment results on integer documents).
pub const F_QUANT: u32 = 32768;
/// `Lerp` as A + (B − C) premultiplied (layers clipped to pass-through groups).
pub const F_ADD_DIFF: u32 = 16384;
/// Blend / Atop / FxMerge of a type layer: coverage mixed at `psblend::TEXT_GAMMA`.
pub const F_TEXT_GAMMA: u32 = 8192;

/// `F_TEXT_GAMMA` for type layers while text gamma blending is on; the gamma goes in `p4.w`.
fn gamma_flag(layer: &Layer) -> u32 {
    if photocraft_compose::text_gamma(layer) != 1.0 { F_TEXT_GAMMA } else { 0 }
}

/// `FxInit` kinds: A at `opacity` × alpha; an opaque copy of A; A's colour with alpha `opacity`
/// inside B's shape; A with alpha × B's alpha.
pub const INIT_SCALE: i32 = 0;
pub const INIT_OPAQUE: i32 = 1;
pub const INIT_INSIDE: i32 = 2;
pub const INIT_ALPHA: i32 = 3;
/// The vector stroke C over A relative to the shape B, at `opacity`.
pub const INIT_VSTROKE: i32 = 4;
/// A's colour with alpha `opacity` × A's alpha relative to B's.
pub const INIT_RELATIVE: i32 = 5;
/// Outside-stroke coverage seed: (0, A's alpha).
pub const INIT_COVER: i32 = 6;
/// A with its alpha joined with the effect shape (the map).
pub const INIT_OUTLINE: i32 = 7;

/// `MapRef::item` of the layer's effect shape (`compose::layer_shape`) instead of an effect map.
pub const SHAPE_MAP: usize = usize::MAX;

impl<'a> Planner<'a> {
    fn new(cx: DocCtx<'a>) -> Self {
        Planner { passes: Vec::new(), free: Vec::new(), refs: Vec::new(), cx, fx: Vec::new() }
    }

    fn finish(self, root: Slot) -> Plan<'a> {
        Plan { passes: self.passes, slots: self.refs.len() as u32, root, fx: self.fx }
    }

    fn alloc(&mut self) -> Slot {
        if let Some(s) = self.free.pop() {
            self.refs[s as usize] = 1;
            return s;
        }
        self.refs.push(1);
        (self.refs.len() - 1) as Slot
    }
    fn retain(&mut self, s: Slot) -> Slot {
        self.refs[s as usize] += 1;
        s
    }
    fn release(&mut self, s: Slot) {
        let r = &mut self.refs[s as usize];
        *r -= 1;
        if *r == 0 {
            self.free.push(s);
        }
    }
    /// Emit `pass` into a fresh slot, releasing its inputs.
    fn emit(&mut self, mut pass: Pass<'a>) -> Slot {
        let dst = self.alloc();
        pass.dst = dst;
        if self.cx.mode == photocraft_color::ColorMode::Lab {
            pass.flags |= F_LAB;
        }
        let (a, b, c, d) = (pass.a, pass.b, pass.c, pass.d);
        self.passes.push(pass);
        for s in [a, b, c, d].into_iter().flatten() {
            self.release(s);
        }
        dst
    }
    fn clear(&mut self) -> Slot {
        self.emit(Pass::new(Kernel::Clear, 0))
    }

    /// composite_stack: returns the new backdrop (consumes `backdrop`).
    fn stack(&mut self, layers: &'a [Layer], mut backdrop: Slot) -> Result<Slot, Unsupported> {
        let mut i = 0;
        while i < layers.len() {
            let base = &layers[i];
            let mut j = i + 1;
            while j < layers.len() && layers[j].clipped && !base.clipped {
                j += 1;
            }
            let clipped = &layers[i + 1..j];
            if base.visible {
                backdrop = self.layer(base, clipped, backdrop)?;
                if let (LayerContent::Adjustment(_), Some(q)) = (&base.content, photocraft_compose::adjustment_quantum(self.cx.depth)) {
                    // compose: adjustment results are rounded to the document's depth.
                    let mut p = Pass::new(Kernel::Lerp, 0);
                    p.a = Some(backdrop);
                    p.flags = F_QUANT;
                    p.params[0] = [q, 0.0, 0.0, 0.0];
                    backdrop = self.emit(p);
                }
            }
            i = j.max(i + 1);
        }
        Ok(backdrop)
    }

    fn mask_use(&self, layer: &'a Layer) -> Option<MaskUse<'a>> {
        if let Some(c) = photocraft_compose::masks::combined_mask(layer, self.cx.canvas) {
            let default = c.default_pixel().first().copied().unwrap_or(1.0);
            return Some(MaskUse { layer: layer.id, surface: SurfaceRef::Derived(std::sync::Arc::new(c)), density: 1.0, default });
        }
        let m = layer.mask.as_ref()?;
        if !m.enabled {
            return None;
        }
        Some(MaskUse {
            layer: layer.id,
            surface: SurfaceRef::Doc(&m.surface),
            density: m.density,
            default: m.surface.default_pixel().first().copied().unwrap_or(1.0),
        })
    }

    /// Blending Options › Blend If has no GPU pass yet: such documents use the CPU compositor.
    fn check_blend_if(&self, layer: &Layer) -> Result<(), Unsupported> {
        if photocraft_compose::blend_if_active(layer, self.cx.mode) {
            return Err(Unsupported(format!("Blend If on `{}` (composited on the CPU)", layer.name)));
        }
        Ok(())
    }

    /// composite_layer, honouring the layer's channel restrictions.
    fn layer(&mut self, layer: &'a Layer, clipped: &'a [Layer], backdrop: Slot) -> Result<Slot, Unsupported> {
        self.check_blend_if(layer)?;
        match photocraft_compose::channel_weights(layer, self.cx.mode) {
            Some(w) => {
                let before = self.retain(backdrop);
                let after = self.layer_any(layer, clipped, backdrop)?;
                Ok(self.restore_channels(before, after, w))
            }
            None => self.layer_any(layer, clipped, backdrop),
        }
    }

    /// `compose::restore_channels`: `after` with the channels a layer leaves out taken from
    /// `before` (consumes both).
    fn restore_channels(&mut self, before: Slot, after: Slot, w: [f32; 3]) -> Slot {
        let mut p = Pass::new(Kernel::Lerp, 0);
        p.a = Some(before);
        p.b = Some(after);
        p.flags = F_CHANNELS;
        p.params[0] = [w[0], w[1], w[2], 1.0];
        self.emit(p)
    }

    fn layer_any(&mut self, layer: &'a Layer, clipped: &'a [Layer], backdrop: Slot) -> Result<Slot, Unsupported> {
        match layer.artboard() {
            Some(ab) => self.artboard(layer, ab, clipped, backdrop),
            None => self.layer_plain(layer, clipped, backdrop),
        }
    }

    /// compose::composite_artboard: the board's background and the group composited over the
    /// backdrop, kept only inside the board (contents and effects outside it are clipped away).
    fn artboard(&mut self, layer: &'a Layer, ab: &photocraft_doc::Artboard, clipped: &'a [Layer], backdrop: Slot) -> Result<Slot, Unsupported> {
        let board = ab.rect.intersect(&self.cx.canvas);
        if board.is_empty() {
            return Ok(backdrop);
        }
        let before = self.retain(backdrop);
        let mut sub = backdrop;
        if let Some(bg) = ab.background.rgba() {
            let mut c = Pass::new(Kernel::Content, 0);
            c.color = bg;
            let c = self.emit(c);
            let mut p = Pass::new(Kernel::Blend, 0);
            p.a = Some(sub);
            p.b = Some(c);
            sub = self.emit(p);
        }
        let sub = self.layer_plain(layer, clipped, sub)?;
        let dst = if self.refs[before as usize] == 1 {
            before
        } else {
            let n = self.alloc();
            let mut p = Pass::new(Kernel::CopyFull, n);
            p.a = Some(before);
            self.passes.push(p);
            self.release(before);
            n
        };
        let mut p = Pass::new(Kernel::CopyRect, dst);
        p.a = Some(sub);
        p.clip = Some(board);
        self.passes.push(p);
        self.release(sub);
        Ok(dst)
    }

    fn layer_plain(&mut self, layer: &'a Layer, clipped: &'a [Layer], backdrop: Slot) -> Result<Slot, Unsupported> {
        let opacity = layer.opacity * layer.fill_opacity;
        let visible_clipped: Vec<&'a Layer> = clipped.iter().filter(|c| c.visible).collect();

        // Below 100% fill a pass-through group renders isolated (as the CPU compositor does).
        if let LayerContent::Group(g) = &layer.content
            && layer.blend == BlendMode::PassThrough
            && layer.fill_opacity >= 1.0
            && !has_effects(layer)
        {
            let before = self.retain(backdrop);
            let mut after = self.stack(&g.children, backdrop)?;
            if opacity < 1.0 || layer.mask.is_some() || layer.vector_mask.is_some() {
                let mut p = Pass::new(Kernel::Lerp, 0);
                p.a = Some(self.retain(before));
                p.b = Some(after);
                p.opacity = opacity;
                p.mask = self.mask_use(layer);
                after = self.emit(p);
            }
            if visible_clipped.is_empty() {
                self.release(before);
                return Ok(after);
            }
            // Layers clipped to a pass-through group (compose: their effect on the group's
            // isolated rendering, each placed over the original backdrop, is added to the
            // pass-through result, premultiplied).
            let iso = self.content(layer)?;
            let iso_keep = self.retain(iso);
            let mut clipped_iso = iso;
            for c in visible_clipped {
                clipped_iso = self.atop(c, clipped_iso)?;
            }
            let over = |s: &mut Self, src: Slot, before: Slot| {
                let mut p = Pass::new(Kernel::Blend, 0);
                p.a = Some(before);
                p.b = Some(src);
                p.opacity = opacity;
                s.emit(p)
            };
            let b2 = self.retain(before);
            let without = over(self, iso_keep, b2);
            let with = over(self, clipped_iso, before);
            let mut p = Pass::new(Kernel::Lerp, 0);
            p.a = Some(after);
            p.b = Some(with);
            p.c = Some(without);
            p.flags = F_ADD_DIFF;
            return Ok(self.emit(p));
        }

        if let LayerContent::Adjustment(adj) = &layer.content {
            let before = self.retain(backdrop);
            let mut adjusted = self.adjust(adj, backdrop)?;
            for c in visible_clipped {
                adjusted = self.atop(c, adjusted)?;
            }
            let mut p = Pass::new(Kernel::AdjMix, 0);
            p.a = Some(before);
            p.b = Some(adjusted);
            p.mode = layer.blend;
            p.opacity = opacity;
            p.mask = self.mask_use(layer);
            return Ok(self.emit(p));
        }

        if has_effects(layer) {
            return self.effects(layer, &visible_clipped, backdrop, false);
        }

        if let LayerContent::Shape(sh) = &layer.content
            && !visible_clipped.is_empty()
            && let Some((fill, stroke)) = photocraft_compose::shape_split::split(sh, self.cx.canvas)
        {
            // The vector stroke goes above the clipped layers: the fill is the clipping base,
            // the stroke is laid over the clipped result, then the masks apply to both
            // (compose::shape_parts).
            let mut content = self.shape_part(layer, Role::Content, fill);
            for c in visible_clipped {
                content = self.atop(c, content)?;
            }
            let stroke = self.shape_part(layer, Role::Stroke, stroke);
            let mut p = Pass::new(Kernel::Blend, 0);
            p.a = Some(content);
            p.b = Some(stroke);
            let mut content = self.emit(p);
            if let Some(m) = self.mask_use(layer) {
                let mut p = Pass::new(Kernel::Mask, 0);
                p.a = Some(content);
                p.mask = Some(m);
                content = self.emit(p);
            }
            let mut p = Pass::new(Kernel::Blend, 0);
            p.a = Some(backdrop);
            p.b = Some(content);
            p.mode = layer.blend;
            p.opacity = opacity;
            return Ok(self.emit(p));
        }

        // A layer that covers little of the canvas composites only over its bounds and is
        // copied back into the backdrop (#125): a layout with hundreds of small text and shape
        // layers otherwise shades every layer over every chunk.
        let clip = self.local_clip(layer);
        if clip.is_some_and(|c| c.is_empty()) {
            // Nothing to draw (an empty layer, or a group of them).
            for c in &visible_clipped {
                self.check_blend_if(c)?;
            }
            return Ok(backdrop);
        }
        let start = self.passes.len();
        let mut content = self.content(layer)?;
        for c in visible_clipped {
            content = self.atop(c, content)?;
        }
        if let Some(clip) = clip {
            // Everything that built the layer's content only matters inside its bounds.
            for p in &mut self.passes[start..] {
                if p.clip.is_none() && !matches!(p.kernel, Kernel::Clear | Kernel::CopyFull) {
                    p.clip = Some(clip);
                }
            }
        }
        let mut p = Pass::new(Kernel::Blend, 0);
        p.a = Some(if clip.is_some() { self.retain(backdrop) } else { backdrop });
        p.b = Some(content);
        p.mode = layer.blend;
        p.opacity = opacity;
        p.flags = gamma_flag(layer);
        p.extra[3] = photocraft_compose::text_gamma(layer);
        p.clip = clip;
        let merged = self.emit(p);
        match clip {
            Some(clip) => Ok(self.write_back(backdrop, merged, clip)),
            None => Ok(merged),
        }
    }

    /// The rect a plain layer composites over: its composite bounds when they cover at most half
    /// the canvas (beyond that, compositing everywhere costs less than the copy back).
    fn local_clip(&self, layer: &Layer) -> Option<Rect> {
        let canvas = self.cx.canvas;
        let b = bounds::composite_bounds(layer, canvas)?;
        (b.width() as u64 * b.height() as u64 * 2 <= canvas.width() as u64 * canvas.height() as u64).then_some(b)
    }

    /// Copy `merged` into `backdrop` over `clip` (in place when nothing else holds the backdrop);
    /// returns the new backdrop. Consumes both.
    fn write_back(&mut self, backdrop: Slot, merged: Slot, clip: Rect) -> Slot {
        let dst = if self.refs[backdrop as usize] == 1 {
            backdrop
        } else {
            let n = self.alloc();
            let mut p = Pass::new(Kernel::CopyFull, n);
            p.a = Some(backdrop);
            self.passes.push(p);
            self.release(backdrop);
            n
        };
        let mut p = Pass::new(Kernel::CopyRect, dst);
        p.a = Some(merged);
        p.clip = Some(clip);
        self.passes.push(p);
        self.release(merged);
        dst
    }

    /// A stroked shape's fill or vector stroke alone (`compose::shape_split`), unmasked.
    fn shape_part(&mut self, layer: &'a Layer, role: Role, surface: Surface) -> Slot {
        let mut p = Pass::new(Kernel::Content, 0);
        p.color = photocraft_raster::to_rgba(&surface.format(), &surface.default_pixel());
        if surface.tile_count() > 0 {
            p.tex = Some(TexUse { layer: layer.id, role, surface: SurfaceRef::Derived(std::sync::Arc::new(surface)) });
        }
        self.emit(p)
    }

    /// render_content for non-adjustment layers.
    fn content(&mut self, layer: &'a Layer) -> Result<Slot, Unsupported> {
        match &layer.content {
            LayerContent::Group(g) => {
                let empty = self.clear();
                let s = self.stack(&g.children, empty)?;
                if layer.mask.is_some() || layer.vector_mask.is_some() {
                    let mut p = Pass::new(Kernel::Mask, 0);
                    p.a = Some(s);
                    p.mask = self.mask_use(layer);
                    return Ok(self.emit(p));
                }
                Ok(s)
            }
            // Adjustments are handled by the caller and never get here.
            LayerContent::Adjustment(_) => Err(Unsupported("adjustment layer reached the content planner".into())),
            LayerContent::Fill(f @ Fill::Pattern { name, scale, id, angle, link, phase }) if layer.fill_cache.as_ref().is_none_or(|c| c.fill != *f) => {
                // compose::render_fill: the pattern tiled from the layer's frame (transparent
                // when missing), then the layer's masks.
                let empty = self.clear();
                let Some(pat) = photocraft_doc::pattern::find(self.cx.patterns, id, name).filter(|p| !p.is_empty()) else { return Ok(empty) };
                let frame = photocraft_compose::fill_frame(layer, self.cx.canvas);
                let anchor = if frame.is_empty() { (0.0, 0.0) } else { (f64::from(frame.x0), f64::from(frame.y0)) };
                let paint = Paint::Pattern(pat, placement(anchor, *link, *phase, *scale, *angle));
                let canvas = self.cx.canvas;
                let s = self.paint(empty, empty, Cov::One, &paint, BlendMode::Normal, 1.0, 0, canvas, canvas);
                if layer.mask.is_some() || layer.vector_mask.is_some() {
                    let mut p = Pass::new(Kernel::Mask, 0);
                    p.a = Some(s);
                    p.mask = self.mask_use(layer);
                    return Ok(self.emit(p));
                }
                Ok(s)
            }
            _ => {
                let p = self.content_pass(layer);
                Ok(self.emit(p))
            }
        }
    }

    /// The Content pass of a raster / text / shape / smart / fill layer.
    fn content_pass(&self, layer: &'a Layer) -> Pass<'a> {
        let mut p = Pass::new(Kernel::Content, 0);
        p.mask = self.mask_use(layer);
        match &layer.content {
            LayerContent::Fill(f) => match &layer.fill_cache {
                Some(c) if c.fill == *f => self.surface_tex(&mut p, layer.id, &c.surface),
                _ => self.fill(&mut p, f, photocraft_compose::fill_frame(layer, self.cx.canvas)),
            },
            _ => {
                if let Some(s) = layer.surface() {
                    self.surface_tex(&mut p, layer.id, s);
                }
            }
        }
        p
    }

    fn surface_tex(&self, p: &mut Pass<'a>, id: LayerId, s: &'a Surface) {
        let dp = s.default_pixel();
        p.color = photocraft_raster::to_rgba(&s.format(), &dp);
        if s.tile_count() > 0 {
            p.tex = Some(TexUse { layer: id, role: Role::Content, surface: SurfaceRef::Doc(s) });
        }
    }

    fn fill(&self, p: &mut Pass<'a>, f: &Fill, frame: photocraft_geom::Rect) {
        match f {
            Fill::Solid(c) => {
                let rgb = c.to_rgb();
                p.color = [rgb[0], rgb[1], rgb[2], c.alpha];
            }
            Fill::Gradient { angle, scale, style, reverse, offset, dither, .. } => {
                p.gradient = true;
                // compose::render_fill: whole-pixel end points (fill_layout).
                let (angle, scale, offset) = photocraft_compose::fill_layout::gradient_layout(*style, *angle, *scale, *offset, frame);
                p.params[0] = [angle, scale, if *reverse { 1.0 } else { 0.0 }, style_index(*style)];
                let c = frame;
                p.params[1] = [c.x0 as f32, c.y0 as f32, c.width() as f32, c.height() as f32];
                // p2.xy: centre offset; p2.w: dither (the shared position hash, see the shader).
                p.params[2] = [offset.0, offset.1, 0.0, if *dither { 1.0 } else { 0.0 }];
                let ramp = photocraft_compose::gradient_fill::Ramp::new(f);
                let mut rows = vec![[0.0f32; 4096]; 4];
                for k in 0..4096 {
                    let v = ramp.as_ref().map_or([0.0; 4], |r| r.sample(k as f32 / 4095.0));
                    for (ch, row) in rows.iter_mut().enumerate() {
                        row[k] = v[ch];
                    }
                }
                p.lut = Some(rows);
            }
            // Pattern fills fall back to the CPU compositor (see `check`).
            Fill::Pattern { .. } => {}
        }
    }

    /// composite_atop: `layer` onto `base`, restricted to the base's alpha, honouring the
    /// layer's channel restrictions.
    fn atop(&mut self, layer: &'a Layer, base: Slot) -> Result<Slot, Unsupported> {
        self.check_blend_if(layer)?;
        match photocraft_compose::channel_weights(layer, self.cx.mode) {
            Some(w) => {
                let before = self.retain(base);
                let after = self.atop_any(layer, base)?;
                Ok(self.restore_channels(before, after, w))
            }
            None => self.atop_any(layer, base),
        }
    }

    fn atop_any(&mut self, layer: &'a Layer, base: Slot) -> Result<Slot, Unsupported> {
        let opacity = layer.opacity * layer.fill_opacity;
        if let LayerContent::Adjustment(adj) = &layer.content {
            let before = self.retain(base);
            let adjusted = self.adjust(adj, base)?;
            let mut p = Pass::new(Kernel::AdjMix, 0);
            p.a = Some(before);
            p.b = Some(adjusted);
            p.mode = layer.blend;
            p.opacity = opacity;
            p.mask = self.mask_use(layer);
            return Ok(self.emit(p));
        }
        if has_effects(layer) {
            return self.effects(layer, &[], base, true);
        }
        let content = self.content(layer)?;
        let mut p = Pass::new(Kernel::Atop, 0);
        p.a = Some(base);
        p.b = Some(content);
        p.mode = layer.blend;
        p.opacity = opacity;
        p.flags = gamma_flag(layer);
        p.extra[3] = photocraft_compose::text_gamma(layer);
        Ok(self.emit(p))
    }

    /// adjust::apply_with on a slot (consumes it).
    fn adjust(&mut self, adj: &Adjustment, src: Slot) -> Result<Slot, Unsupported> {
        if !adjustment_on_gpu(adj) {
            return Err(Unsupported(format!("{} on CMYK/Lab channels (evaluated on the CPU)", adj.label())));
        }
        let mut p = Pass::new(Kernel::Adjust, 0);
        p.a = Some(src);
        let (kind, params, lut) = adjustment_program(adj, self.cx.transfer, photocraft_compose::adjustment_quantum(self.cx.depth));
        p.adjust_kind = kind;
        p.params = params;
        p.lut = lut;
        Ok(self.emit(p))
    }

    /// `composite_with_effects` (atop = false) or the clipped-layer variant of `composite_atop`
    /// (atop = true: effects over the base treated as opaque, keeping the base's alpha).
    /// Consumes `backdrop`.
    fn effects(&mut self, layer: &'a Layer, clipped: &[&'a Layer], backdrop: Slot, atop: bool) -> Result<Slot, Unsupported> {
        let canvas = self.cx.canvas;
        let region = bounds::effect_region(layer, canvas);
        let sb = photocraft_compose::paint_bounds(layer).unwrap_or_else(|| bounds::layer_bounds(layer, canvas));
        let clip = if bounds::transparent_outside(layer) { region } else { canvas };
        // A stroked shape's vector stroke goes above its clipped layers and interior effects
        // (compose::split_parts): the fill and the stroke unmasked, the masks applied after.
        let split = match &layer.content {
            LayerContent::Shape(sh) if sh.stroke.is_some() => photocraft_compose::shape_split::split(sh, canvas),
            _ => None,
        };
        let (mut content, vstroke) = match split {
            Some((fill, stroke)) => {
                let f = self.shape_part(layer, Role::Content, fill);
                let s = self.shape_part(layer, Role::Stroke, stroke);
                (f, Some(s))
            }
            None => (self.content(layer)?, None),
        };
        for p in self.passes.iter_mut().rev().take(if vstroke.is_some() { 2 } else { 1 }) {
            if !matches!(layer.content, LayerContent::Group(_)) {
                p.clip = Some(clip);
            }
        }
        for c in clipped {
            content = self.atop(c, content)?;
        }
        let fx = self.fx.len();
        self.fx.push(FxLayer { layer, region, bounds: sb });
        let outline = photocraft_compose::effect_outline(layer).is_some();
        let relative = outline || vstroke.is_some();
        // `content` becomes the effect shape (B of every effect pass); `lay_src` the layer's colour.
        let (lay_src, vstroke) = match vstroke {
            Some(s) => {
                let mut p = Pass::new(Kernel::Blend, 0);
                p.a = Some(self.retain(content));
                p.b = Some(self.retain(s));
                p.clip = Some(clip);
                let union = self.emit(p);
                let m = self.mask_use(layer);
                let masked = |me: &mut Self, slot: Slot| match &m {
                    Some(m) => {
                        let mut p = Pass::new(Kernel::Mask, 0);
                        p.a = Some(slot);
                        p.mask = Some(m.clone());
                        p.clip = Some(clip);
                        me.emit(p)
                    }
                    None => slot,
                };
                let fill = masked(self, content);
                content = masked(self, union);
                (fill, Some(masked(self, s)))
            }
            None => (self.retain(content), None),
        };
        if outline {
            let mut p = Pass::new(Kernel::FxInit, 0);
            p.a = Some(content);
            p.adjust_kind = INIT_OUTLINE;
            p.map = Some(MapRef { fx, item: SHAPE_MAP, map: 0 });
            p.clip = Some(clip);
            content = self.emit(p);
        }

        let items: Vec<&'a Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
        // Linked patterns tile from the effects reference point (else the layer's top-left).
        let anchor = layer.effects.reference.unwrap_or((f64::from(sb.x0), f64::from(sb.y0)));
        let vector_shape = matches!(layer.content, LayerContent::Shape(_)) && !outline;
        let map = |item: usize, map: usize| MapRef { fx, item, map };
        let rev: Vec<(usize, &'a Effect)> = items.iter().copied().enumerate().rev().collect();
        let init = |s: &mut Self, src: Slot, b: Option<Slot>, kind: i32, opacity: f32| {
            let mut p = Pass::new(Kernel::FxInit, 0);
            p.a = Some(src);
            p.b = b;
            p.adjust_kind = kind;
            p.opacity = opacity;
            p.clip = Some(clip);
            s.emit(p)
        };

        // Exterior effects over the backdrop (an opaque copy of it for clipped layers).
        let mut w = if atop {
            let b = self.retain(backdrop);
            init(self, b, None, INIT_OPAQUE, 1.0)
        } else {
            self.retain(backdrop)
        };
        for &(i, e) in &rev {
            if let Effect::DropShadow(s) = e {
                // The layer hides the shadow beneath it where its fill is see-through.
                let flags = if s.knocks_out { F_KNOCKOUT } else { 0 };
                let see_through = 1.0 - layer.fill_opacity.clamp(0.0, 1.0);
                w = self.paint_k(
                    w,
                    content,
                    Cov::Map(map(i, 0), 0.0),
                    &Paint::Color(s.color.to_rgb()),
                    s.common.blend,
                    s.common.opacity,
                    flags,
                    clip,
                    sb,
                    see_through,
                );
            }
        }
        for &(i, e) in &rev {
            if let Effect::OuterGlow(g) = e {
                let paint = self.fx_paint(&g.paint, anchor);
                w = self.paint(w, content, Cov::Map(map(i, 0), 0.0), &paint, g.common.blend, g.common.opacity, 0, clip, sb);
            }
        }

        // The layer: its colour at fill opacity inside its shape; interior effects are painted
        // relative to the shape (coverage within it), then the shape's alpha applies.
        let mut l = if relative {
            let c = self.retain(content);
            init(self, lay_src, Some(c), INIT_RELATIVE, layer.fill_opacity)
        } else {
            init(self, lay_src, None, INIT_INSIDE, layer.fill_opacity)
        };
        for &(_, e) in &rev {
            if let Effect::PatternOverlay { common, name, id, scale, angle, link, phase } = e
                && let Some(pat) = photocraft_doc::pattern::find(self.cx.patterns, id, name).filter(|p| !p.is_empty())
            {
                let paint = Paint::Pattern(pat, placement(anchor, *link, *phase, *scale, *angle));
                l = self.paint(l, content, Cov::One, &paint, common.blend, common.opacity, F_GATE, clip, sb);
            }
        }
        for &(_, e) in &rev {
            if let Effect::GradientOverlay { common, gradient, .. } = e {
                l = self.paint(l, content, Cov::One, &Paint::Gradient(gradient), common.blend, common.opacity, F_GATE, clip, sb);
            }
        }
        for &(_, e) in &rev {
            if let Effect::ColorOverlay { common, color } = e {
                l = self.paint(l, content, Cov::One, &Paint::Color(color.to_rgb()), common.blend, common.opacity, F_GATE, clip, sb);
            }
        }
        for &(i, e) in &rev {
            if let Effect::Satin(s) = e {
                l = self.paint(l, content, Cov::Map(map(i, 0), 0.0), &Paint::Color(s.color.to_rgb()), s.common.blend, s.common.opacity, F_REL, clip, sb);
            }
        }
        for &(i, e) in &rev {
            if let Effect::InnerGlow(g) = e {
                let paint = self.fx_paint(&g.paint, anchor);
                l = self.paint(l, content, Cov::Map(map(i, 0), 0.0), &paint, g.common.blend, g.common.opacity, F_REL, clip, sb);
            }
        }
        for &(i, e) in &rev {
            if let Effect::InnerShadow(s) = e {
                l = self.paint(l, content, Cov::Map(map(i, 0), 0.0), &Paint::Color(s.color.to_rgb()), s.common.blend, s.common.opacity, F_REL, clip, sb);
            }
        }
        if let Some(s) = vstroke {
            let mut p = Pass::new(Kernel::FxInit, 0);
            p.a = Some(l);
            p.b = Some(self.retain(content));
            p.c = Some(s);
            p.adjust_kind = INIT_VSTROKE;
            p.opacity = layer.fill_opacity;
            p.clip = Some(clip);
            l = self.emit(p);
        }
        // Inside stroke parts are painted over the layer (bottom instance first).
        for &(i, e) in &rev {
            if let Effect::Stroke(st) = e {
                let (in_w, _) = stroke_widths(st);
                if in_w > 0.0 {
                    let paint = self.fx_paint(&st.paint, anchor);
                    l = self.paint(
                        l,
                        content,
                        Cov::Map(map(i, 1), (in_w + 0.5).clamp(0.0, 1.0)),
                        &paint,
                        st.common.blend,
                        st.common.opacity,
                        F_GATE,
                        clip,
                        photocraft_compose::stroke_frame(self.cx.doc, layer, st).unwrap_or(sb),
                    );
                }
            }
        }
        let bevel_paint = |b: &photocraft_doc::Bevel| photocraft_compose::effects::bevel_geom(b).paint;
        for &(i, e) in &rev {
            if let Effect::BevelEmboss(b) = e
                && bevel_paint(b) == photocraft_compose::effects::BevelPaint::Inner
            {
                l = self.paint(
                    l,
                    content,
                    Cov::Map(map(i, 0), 0.0),
                    &Paint::Color(b.highlight_color.to_rgb()),
                    b.highlight.blend,
                    b.highlight.opacity,
                    F_REL,
                    clip,
                    sb,
                );
                l = self.paint(l, content, Cov::Map(map(i, 1), 0.0), &Paint::Color(b.shadow_color.to_rgb()), b.shadow.blend, b.shadow.opacity, F_REL, clip, sb);
            }
        }
        // The shape's own alpha.
        let c = self.retain(content);
        l = init(self, l, Some(c), INIT_ALPHA, 1.0);
        // Outside stroke parts lie beneath the layer and blend onto the exterior result with their
        // own mode; a higher stroke covers the ones below it (each takes its coverage × opacity
        // not yet taken above it, blended over the result as it was before the strokes).
        let outs: Vec<(usize, &photocraft_doc::StrokeFx)> = items
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(i, e)| if let Effect::Stroke(st) = e { Some((i, st)) } else { None })
            .filter(|(_, st)| stroke_widths(st).1 > 0.0)
            .collect();
        if !outs.is_empty() {
            let (mut acc, mut cover): (Option<Slot>, Option<Slot>) = (None, None);
            if outline {
                // The layer's alpha rides along the coverage (`effects::outline_share`).
                let ll = self.retain(l);
                cover = Some(init(self, ll, None, INIT_COVER, 1.0));
            }
            for (n, &(i, st)) in outs.iter().enumerate() {
                let paint = self.fx_paint(&st.paint, anchor);
                let flags = F_STROKE_OUT | if vector_shape { F_VECTOR } else { 0 } | if outline { F_OUTLINE } else { 0 } | if n == 0 { F_FIRST } else { 0 };
                // Share × blended colour, then coverage (both read the coverage so far).
                let mut p = self.fx_pass_kernel(
                    Kernel::FxStroke,
                    w,
                    content,
                    &paint,
                    st.common.blend,
                    st.common.opacity,
                    flags,
                    clip,
                    photocraft_compose::stroke_frame(self.cx.doc, layer, st).unwrap_or(sb),
                );
                if matches!(paint, Paint::None) {
                    // A missing pattern paints nothing: the stroke's share keeps the result as is.
                    p.params[2][2] = 3.0;
                }
                p.adjust_kind = 0;
                p.map = Some(map(i, 0));
                p.c = cover.map(|c| self.retain(c));
                p.d = acc;
                let a = self.emit(p);
                let mut p = self.fx_pass_kernel(Kernel::FxStroke, w, content, &Paint::Color([0.0; 3]), BlendMode::Normal, st.common.opacity, flags, clip, sb);
                p.adjust_kind = 1;
                p.map = Some(map(i, 0));
                p.c = cover;
                cover = Some(self.emit(p));
                acc = Some(a);
            }
            let mut p = Pass::new(Kernel::FxStrokeEnd, 0);
            p.a = Some(w);
            p.b = acc;
            p.c = cover;
            p.clip = Some(clip);
            w = self.emit(p);
        }
        for &(i, e) in &rev {
            if let Effect::BevelEmboss(b) = e
                && bevel_paint(b) == photocraft_compose::effects::BevelPaint::Outer
            {
                let k = 0;
                w = self.paint(
                    w,
                    content,
                    Cov::Map(map(i, k), 0.0),
                    &Paint::Color(b.highlight_color.to_rgb()),
                    b.highlight.blend,
                    b.highlight.opacity,
                    0,
                    clip,
                    sb,
                );
                w = self.paint(w, content, Cov::Map(map(i, k + 1), 0.0), &Paint::Color(b.shadow_color.to_rgb()), b.shadow.blend, b.shadow.opacity, 0, clip, sb);
            }
        }

        let mode = if layer.blend == BlendMode::PassThrough { BlendMode::Normal } else { layer.blend };
        let late: Vec<(usize, &'a photocraft_doc::Bevel)> = rev
            .iter()
            .filter_map(|&(i, e)| if let Effect::BevelEmboss(b) = e { Some((i, b)) } else { None })
            .filter(|(_, b)| bevel_paint(b) == photocraft_compose::effects::BevelPaint::Both)
            .collect();
        let merged = if late.is_empty() {
            let mut p = Pass::new(Kernel::FxMerge, 0);
            p.a = Some(w);
            p.b = Some(l);
            p.c = Some(self.retain(backdrop));
            p.mode = mode;
            p.opacity = layer.opacity;
            p.flags = if atop { F_ATOP } else { 0 } | gamma_flag(layer);
            p.extra[3] = photocraft_compose::text_gamma(layer);
            p.clip = Some(clip);
            self.emit(p)
        } else {
            // Emboss styles shade the composited layer (`composite_with_effects`): merge at full
            // opacity, paint their maps (inside and outside halves), then mix.
            let mut p = Pass::new(Kernel::FxMerge, 0);
            p.a = Some(w);
            p.b = Some(l);
            p.c = Some(self.retain(w));
            p.mode = mode;
            p.opacity = 1.0;
            p.flags = gamma_flag(layer);
            p.extra[3] = photocraft_compose::text_gamma(layer);
            p.clip = Some(clip);
            let mut m = self.emit(p);
            for (i, b) in late {
                for (k, color, fxc) in [(0, &b.highlight_color, &b.highlight), (1, &b.shadow_color, &b.shadow)] {
                    m = self.paint(m, content, Cov::Map(map(i, k), 0.0), &Paint::Color(color.to_rgb()), fxc.blend, fxc.opacity, 0, clip, sb);
                }
            }
            let mut p = Pass::new(Kernel::FxMerge, 0);
            p.b = Some(self.retain(m));
            p.a = Some(m);
            p.c = Some(self.retain(backdrop));
            p.mode = mode;
            p.opacity = layer.opacity;
            p.flags = if atop { F_ATOP } else { 0 } | F_NO_LAYER;
            p.clip = Some(clip);
            self.emit(p)
        };
        self.release(content);

        // Write the clipped result back into the backdrop (in place when nothing else holds it).
        Ok(self.write_back(backdrop, merged, clip))
    }

    fn fx_paint(&self, p: &'a FxPaint, anchor: (f64, f64)) -> Paint<'a> {
        match p {
            FxPaint::Color(c) => Paint::Color(c.to_rgb()),
            FxPaint::Gradient(g) => Paint::Gradient(g),
            FxPaint::Pattern { name, id, scale } => match photocraft_doc::pattern::find(self.cx.patterns, id, name).filter(|p| !p.is_empty()) {
                Some(pat) => Paint::Pattern(pat, placement(anchor, true, (0.0, 0.0), *scale, 0.0)),
                None => Paint::None,
            },
        }
    }

    /// An effect pass reading `dst` (A, retained) and the layer `content` (B, retained) with its
    /// paint set up.
    #[allow(clippy::too_many_arguments)]
    fn fx_pass(&mut self, dst: Slot, content: Slot, paint: &Paint<'a>, blend: BlendMode, opacity: f32, flags: u32, clip: Rect, sb: Rect) -> Pass<'a> {
        self.fx_pass_kernel(Kernel::FxPaint, dst, content, paint, blend, opacity, flags, clip, sb)
    }

    #[allow(clippy::too_many_arguments)]
    fn fx_pass_kernel(
        &mut self,
        kernel: Kernel,
        dst: Slot,
        content: Slot,
        paint: &Paint<'a>,
        blend: BlendMode,
        opacity: f32,
        flags: u32,
        clip: Rect,
        sb: Rect,
    ) -> Pass<'a> {
        let mut p = Pass::new(kernel, 0);
        p.a = Some(self.retain(dst));
        p.b = Some(self.retain(content));
        p.mode = blend;
        p.opacity = opacity;
        p.flags = flags;
        p.clip = Some(clip);
        match paint {
            Paint::None => {}
            Paint::Color(c) => p.color = [c[0], c[1], c[2], 1.0],
            Paint::Gradient(g) => {
                // effects::paint_fx: whole-pixel end points (fill_layout::gradient_layout).
                let (angle, scale, offset) = photocraft_compose::fill_layout::gradient_layout(g.style, g.angle, g.scale, g.offset, sb);
                p.params[0] = [angle, scale, if g.reverse { 1.0 } else { 0.0 }, style_index(g.style)];
                p.params[1] = [sb.x0 as f32, sb.y0 as f32, sb.width() as f32, sb.height() as f32];
                p.params[2][0] = offset.0;
                p.params[2][1] = offset.1;
                p.params[2][2] = 1.0;
                let mut rows = vec![[0.0f32; 4096]; 4];
                for k in 0..4096 {
                    let v = photocraft_compose::effects::sample_gradient(g, k as f32 / 4095.0);
                    for (ch, row) in rows.iter_mut().enumerate() {
                        row[k] = v[ch];
                    }
                }
                p.lut = Some(rows);
            }
            Paint::Pattern(pat, pl) => {
                p.params[2][2] = 2.0;
                p.params[3] = [pl.origin.0 as f32, pl.origin.1 as f32, pl.cs.0 as f32, pl.cs.1 as f32];
                p.extra = [pl.inv_scale as f32, pat.width as f32, pat.height as f32, 0.0];
                p.pattern = Some(pat);
            }
        }
        p
    }

    /// One effect paint into `dst` (consumed); reads the layer's alpha from `content` (kept).
    #[allow(clippy::too_many_arguments)]
    fn paint(&mut self, dst: Slot, content: Slot, cov: Cov, paint: &Paint<'a>, blend: BlendMode, opacity: f32, flags: u32, clip: Rect, sb: Rect) -> Slot {
        self.paint_k(dst, content, cov, paint, blend, opacity, flags, clip, sb, 1.0)
    }

    /// [`Self::paint`] with the knockout strength (`F_KNOCKOUT`: coverage × (1 − alpha × k)).
    #[allow(clippy::too_many_arguments)]
    fn paint_k(
        &mut self,
        dst: Slot,
        content: Slot,
        cov: Cov,
        paint: &Paint<'a>,
        blend: BlendMode,
        opacity: f32,
        flags: u32,
        clip: Rect,
        sb: Rect,
        knockout: f32,
    ) -> Slot {
        if matches!(paint, Paint::None) {
            // Missing pattern: compose paints nothing.
            return dst;
        }
        let mut p = self.fx_pass(dst, content, paint, blend, opacity, flags, clip, sb);
        self.release(dst);
        match cov {
            Cov::Map(m, outside) => {
                p.adjust_kind = 0;
                p.map = Some(m);
                p.params[2][3] = outside;
            }
            Cov::One => p.adjust_kind = 1,
        }
        p.extra[3] = knockout;
        self.emit(p)
    }
}

/// Paint source of an effect.
enum Paint<'a> {
    None,
    Color([f32; 3]),
    Gradient(&'a Gradient),
    Pattern(&'a Pattern, Placement),
}

/// `compose::pattern::Placement` (origin, inverse rotation, inverse scale).
#[derive(Clone, Copy, Debug)]
struct Placement {
    origin: (f64, f64),
    cs: (f64, f64),
    inv_scale: f64,
}

/// `Placement::anchored`: linked patterns tile from `anchor` (the effects reference point).
fn placement(anchor: (f64, f64), link: bool, phase: (f32, f32), scale: f32, angle: f32) -> Placement {
    let base = if link { anchor } else { (0.0, 0.0) };
    let origin = (base.0 + f64::from(phase.0), base.1 + f64::from(phase.1));
    let s = f64::from(scale);
    let inv_scale = if s.is_finite() && s > 1e-3 { 1.0 / s } else { 1.0 };
    let a = f64::from(angle).to_radians();
    Placement { origin, cs: (a.cos(), a.sin()), inv_scale }
}

/// (inside width, outside width) of a stroke.
pub fn stroke_widths(st: &photocraft_doc::StrokeFx) -> (f32, f32) {
    match st.position {
        StrokePosition::Outside => (0.0, st.size),
        StrokePosition::Inside => (st.size, 0.0),
        StrokePosition::Center => (st.size / 2.0, st.size / 2.0),
    }
}

fn style_index(s: photocraft_doc::GradientStyle) -> f32 {
    match s {
        photocraft_doc::GradientStyle::Linear => 0.0,
        photocraft_doc::GradientStyle::Radial => 1.0,
        photocraft_doc::GradientStyle::Angle => 2.0,
        photocraft_doc::GradientStyle::Reflected => 3.0,
        photocraft_doc::GradientStyle::Diamond => 4.0,
    }
}

fn sample_stops4(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    match stops {
        [] => [0.0; 4],
        [only] => only.1,
        _ => {
            if t <= stops[0].0 {
                return stops[0].1;
            }
            for w in stops.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                if t <= b.0 {
                    let k = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 };
                    return std::array::from_fn(|i| a.1[i] + (b.1[i] - a.1[i]) * k);
                }
            }
            stops[stops.len() - 1].1
        }
    }
}

fn lut_rows(f: impl Fn(usize, f32) -> f32, rows: usize) -> Vec<[f32; 4096]> {
    (0..rows)
        .map(|r| {
            let mut row = [0.0f32; 4096];
            for (k, v) in row.iter_mut().enumerate() {
                *v = f(r, k as f32 / 4095.0);
            }
            row
        })
        .collect()
}

type Program = (i32, [[f32; 4]; 4], Option<Vec<[f32; 4096]>>);

/// A CPU LUT (4096 entries) as one texture row.
fn to_row(t: &[f32]) -> [f32; 4096] {
    let mut row = [0.0f32; 4096];
    for (o, v) in row.iter_mut().zip(t) {
        *o = *v;
    }
    row
}

/// Whether the adjustment kernel can evaluate `adj`: Levels and Curves on CMYK ink or Lab
/// channels convert through ICC profiles per pixel, which only the CPU does.
pub fn adjustment_on_gpu(adj: &Adjustment) -> bool {
    !matches!(adj, Adjustment::Levels { space: ToneSpace::Cmyk | ToneSpace::Lab, .. } | Adjustment::Curves { space: ToneSpace::Cmyk | ToneSpace::Lab, .. })
}

/// Adjustment → (kernel kind, parameters, LUT rows). Kinds are the `switch` in `adjust()`.
pub fn adjustment_program(adj: &Adjustment, transfer: Transfer, quantum: Option<f32>) -> Program {
    let mut p = [[0.0f32; 4]; 4];
    match adj {
        Adjustment::Invert => (1, p, None),
        Adjustment::Threshold { level } => {
            p[0][0] = (level * 255.0).round();
            (2, p, None)
        }
        Adjustment::Posterize { levels } => {
            p[0][0] = (*levels).clamp(2, 255) as f32;
            (3, p, None)
        }
        Adjustment::BrightnessContrast { brightness, contrast, legacy: true } => {
            let c = contrast.clamp(-100.0, 99.0);
            let k = if c >= 0.0 { 1.0 / (1.0 - c / 100.0) } else { 1.0 + c / 100.0 };
            p[0] = [brightness / 255.0, k, 0.0, 0.0];
            (4, p, None)
        }
        Adjustment::BrightnessContrast { brightness, contrast, .. } => {
            // Modern B/C: the shader rebuilds the curves from raw slider values (see compose.wgsl
            // `mbright`/`mcontrast`, mirroring compose::adjust::modern_brightness/modern_contrast).
            p[0] = [*brightness, *contrast, 0.0, 0.0];
            (5, p, None)
        }
        Adjustment::Exposure { exposure, offset, gamma } => {
            let g = match transfer.for_exposure() {
                Transfer::Srgb => 0.0,
                Transfer::Gamma(g) => g,
            };
            p[0] = [2f32.powf(*exposure), *offset, gamma.max(0.01), g];
            (6, p, None)
        }
        // RGB space only (see `adjustment_on_gpu`); the rows are the CPU's channel∘master LUTs.
        Adjustment::Levels { .. } | Adjustment::Curves { .. } => (7, p, Some(adjust::tone_luts_q(adj, quantum).iter().take(3).map(|t| to_row(t)).collect())),
        Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges } => {
            p[0] = [*hue, saturation / 100.0, lightness / 100.0, if *colorize { 1.0 } else { 0.0 }];
            if !*colorize && ranges.iter().any(|r| !r.is_neutral()) {
                // Range edits: hue-indexed rows (shift, saturation, lightness), as on the CPU.
                p[1][0] = 1.0;
                (8, p, Some(adjust::hue_range_tables(ranges).iter().map(|t| to_row(t)).collect()))
            } else {
                (8, p, None)
            }
        }
        Adjustment::Vibrance { vibrance, saturation } => {
            p[0] = [vibrance / 100.0, saturation / 100.0, 0.0, 0.0];
            (9, p, None)
        }
        Adjustment::ChannelMixer { matrix, monochrome } => {
            p[0] = matrix[0];
            p[1] = matrix[1];
            p[2] = matrix[2];
            p[3][0] = if *monochrome { 1.0 } else { 0.0 };
            (10, p, None)
        }
        Adjustment::PhotoFilter { color, density, preserve_luminosity } => {
            p[0] = [color[0], color[1], color[2], *density];
            p[1][0] = if *preserve_luminosity { 1.0 } else { 0.0 };
            (11, p, None)
        }
        Adjustment::BlackWhite { weights, tint } => {
            p[0] = [weights[0], weights[1], weights[2], weights[3]];
            p[1] = [weights[4], weights[5], if tint.is_some() { 1.0 } else { 0.0 }, 0.0];
            if let Some(t) = tint {
                p[2] = [t[0], t[1], t[2], 0.0];
            }
            (12, p, None)
        }
        Adjustment::GradientMap { stops, reverse, dither } => {
            p[0][0] = if *reverse { 1.0 } else { 0.0 };
            p[0][1] = if *dither { 1.0 } else { 0.0 };
            let s4: Vec<(f32, [f32; 4])> = stops.iter().map(|(t, c)| (*t, [c[0], c[1], c[2], 1.0])).collect();
            let rows = lut_rows(|r, t| if s4.is_empty() { t } else { sample_stops4(&s4, t)[r] }, 3);
            (13, p, Some(rows))
        }
        Adjustment::ColorBalance { shadows, midtones, highlights, preserve_luminosity } => {
            p[0] = [shadows[0], shadows[1], shadows[2], 0.0];
            p[1] = [midtones[0], midtones[1], midtones[2], 0.0];
            p[2] = [highlights[0], highlights[1], highlights[2], 0.0];
            p[3][0] = if *preserve_luminosity { 1.0 } else { 0.0 };
            (14, p, None)
        }
        Adjustment::SelectiveColor { relative, adjustments } => {
            // 9 ranges × CMYK don't fit the 16 params: they go in LUT row 0 (read texel-exact).
            p[0][0] = if *relative { 1.0 } else { 0.0 };
            let mut row = [0.0f32; 4096];
            for (i, v) in adjustments.iter().flatten().enumerate() {
                row[i] = *v;
            }
            (15, p, Some(vec![row]))
        }
        Adjustment::ColorLookup { lut: Some(table), size, tetrahedral, dither, .. } if *size >= 2 && table.len() >= (*size as usize).pow(3) * 3 => {
            // The flattened table (n³ RGB triplets) spans as many 4096-wide rows as it needs.
            let n = *size as usize;
            let len = n * n * n * 3;
            let rows = table[..len]
                .chunks(4096)
                .map(|c| {
                    let mut row = [0.0f32; 4096];
                    row[..c.len()].copy_from_slice(c);
                    row
                })
                .collect();
            p[0] = [n as f32, if *tetrahedral { 1.0 } else { 0.0 }, if *dither { 1.0 } else { 0.0 }, 0.0];
            (16, p, Some(rows))
        }
        // Identity on the CPU too (not evaluated there).
        Adjustment::ColorLookup { .. } | Adjustment::Unsupported { .. } => (0, p, None),
    }
}

/// Mode index used by the shader (declaration order of [`BlendMode`]).
pub fn mode_index(m: BlendMode) -> i32 {
    match m {
        BlendMode::PassThrough => 0,
        BlendMode::Normal => 1,
        BlendMode::Dissolve => 2,
        BlendMode::Darken => 3,
        BlendMode::Multiply => 4,
        BlendMode::ColorBurn => 5,
        BlendMode::LinearBurn => 6,
        BlendMode::DarkerColor => 7,
        BlendMode::Lighten => 8,
        BlendMode::Screen => 9,
        BlendMode::ColorDodge => 10,
        BlendMode::LinearDodge => 11,
        BlendMode::LighterColor => 12,
        BlendMode::Overlay => 13,
        BlendMode::SoftLight => 14,
        BlendMode::HardLight => 15,
        BlendMode::VividLight => 16,
        BlendMode::LinearLight => 17,
        BlendMode::PinLight => 18,
        BlendMode::HardMix => 19,
        BlendMode::Difference => 20,
        BlendMode::Exclusion => 21,
        BlendMode::Subtract => 22,
        BlendMode::Divide => 23,
        BlendMode::Hue => 24,
        BlendMode::Saturation => 25,
        BlendMode::Color => 26,
        BlendMode::Luminosity => 27,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{Color, ColorMode, SampleType};
    use photocraft_geom::Size;

    #[test]
    fn slots_are_recycled() {
        let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        for i in 0..10 {
            d.layers.push(Layer::raster(format!("l{i}"), d.pixel_format()));
        }
        let p = plan(&d).unwrap();
        // Empty layers draw nothing.
        assert_eq!(p.passes.len(), 1 + 2);
        for (i, l) in d.layers.iter_mut().skip(1).enumerate() {
            l.surface_mut().unwrap().fill_rect(Rect::new(i as i32 % 4, 0, 4, 8), &[0.0, 0.0, 1.0, 1.0]);
        }
        let p = plan(&d).unwrap();
        assert!(p.slots <= 3, "{} slots", p.slots);
        // The background, then each small layer over its bounds and copied back.
        assert_eq!(p.passes.len(), 1 + 2 + 10 * 3);
    }

    #[test]
    fn small_layers_composite_only_over_their_bounds() {
        let mut d = Document::with_background("t", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut l = Layer::raster("small", d.pixel_format());
        l.surface_mut().unwrap().fill_rect(Rect::new(10, 12, 20, 30), &[1.0, 0.0, 0.0, 1.0]);
        let mut group = Layer::group("g", vec![l]);
        group.blend = BlendMode::Normal;
        d.layers.push(group);
        let p = plan(&d).unwrap();
        let r = Rect::new(10, 12, 20, 30);
        // The group's content, its blend and the copy back are all limited to the layer.
        assert!(p.passes.iter().filter(|p| matches!(p.kernel, Kernel::Content | Kernel::Blend)).skip(2).all(|p| p.clip == Some(r)), "{:?}", p.passes);
        let last = p.passes.last().unwrap();
        assert_eq!((last.kernel, last.dst, last.clip), (Kernel::CopyRect, p.root, Some(r)));
        // A layer that covers most of the canvas composites everywhere.
        d.layers[1].children_mut().unwrap()[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 60, 60), &[1.0, 0.0, 0.0, 1.0]);
        let p = plan(&d).unwrap();
        assert!(p.passes.iter().all(|p| p.clip.is_none()));
    }

    #[test]
    fn effects_are_planned_in_place() {
        let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut l = Layer::raster("fx", d.pixel_format());
        l.surface_mut().unwrap().fill_rect(Rect::new(2, 2, 5, 5), &[1.0, 0.0, 0.0, 1.0]);
        l.effects.items.push(photocraft_doc::Effect::ColorOverlay { common: photocraft_doc::FxCommon::new(BlendMode::Normal, 1.0), color: Color::WHITE });
        l.effects.items.push(photocraft_doc::Effect::default_drop_shadow());
        d.layers.push(l);
        let p = plan(&d).unwrap();
        assert_eq!(p.fx.len(), 1);
        let m = photocraft_compose::effects::margin(&d.layers[1]);
        assert_eq!(p.fx[0].region, Rect::new(2, 2, 5, 5).inflate(m).intersect(&d.bounds().inflate(m)));
        // The effect result lands back in the backdrop slot.
        let last = p.passes.last().unwrap();
        assert_eq!(last.kernel, Kernel::CopyRect);
        assert_eq!(last.dst, p.root);
        assert!(p.slots <= 6, "{} slots", p.slots);
    }

    #[test]
    fn opaque_fill_layers_skip_what_they_cover() {
        let opaque = || Fill::gradient(vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], 30.0, 1.0, photocraft_doc::GradientStyle::Linear, false);
        let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        for i in 0..4 {
            // Small painted layers (empty ones plan no passes at all).
            let mut l = Layer::raster(format!("l{i}"), d.pixel_format());
            l.surface_mut().unwrap().fill_rect(Rect::new(i, i, i + 2, i + 2), &[0.0, 0.0, 1.0, 1.0]);
            d.layers.push(l);
        }
        d.layers.push(Layer::new("g", LayerContent::Fill(opaque())));
        let mut top = Layer::raster("top", d.pixel_format());
        top.clipped = true;
        d.layers.push(top);
        // Clear, then the fill and its clipped layer: the background and four rasters are skipped.
        let skipped = plan(&d).unwrap().passes.len();
        let mut alone = d.clone();
        alone.layers.drain(..5);
        assert_eq!(skipped, plan(&alone).unwrap().passes.len());
        // Anything that lets the layers below show through keeps them.
        let see_through: [fn(&mut Layer); 6] = [
            |l| l.opacity = 0.99,
            |l| l.fill_opacity = 0.5,
            |l| l.blend = BlendMode::Multiply,
            |l| l.visible = false,
            |l| l.excluded_channels = 1,
            |l| {
                if let LayerContent::Fill(Fill::Gradient { opacity_stops, .. }) = &mut l.content {
                    opacity_stops.push((1.0, 0.5));
                }
            },
        ];
        for (k, f) in see_through.iter().enumerate() {
            let mut d2 = d.clone();
            f(&mut d2.layers[5]);
            assert!(plan(&d2).unwrap().passes.len() > skipped, "case {k}: the layers below are planned");
            assert!(!photocraft_compose::occludes_below(&d2.layers[5], ColorMode::Rgb), "case {k}");
        }
        let mut translucent = d.clone();
        translucent.layers[5].content = LayerContent::Fill(Fill::Solid(Color::rgba(1.0, 0.0, 0.0, 0.5)));
        assert!(!photocraft_compose::occludes_below(&translucent.layers[5], ColorMode::Rgb));
        assert!(photocraft_compose::occludes_below(&d.layers[5], ColorMode::Rgb));
    }

    #[test]
    fn blend_if_falls_back_to_the_cpu() {
        let mut d = Document::with_background("t", Size::new(8, 8), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut l = Layer::raster("bi", d.pixel_format());
        l.blend_if.set(0, [photocraft_doc::BlendRange { black: [40, 40], white: [255, 255] }, photocraft_doc::BlendRange::FULL]);
        d.layers.push(l.clone());
        assert!(plan(&d).unwrap_err().0.contains("Blend If"));
        // Clipped layers too.
        l.clipped = true;
        d.layers[1].blend_if = Default::default();
        d.layers.push(l);
        assert!(plan(&d).is_err());
    }
}

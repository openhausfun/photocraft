//! The Photocraft document model: pure data, no rendering, no UI.
//!
//! Layer order: inside every group (and the root), `children[0]` is the **bottom** layer, matching
//! compositing order and the PSD file order. UIs display the list reversed.
//!
//! Non-destructive kinds (adjustment, fill, text, shape, smart object) and colour modes beyond RGB are
//! part of the model from day one, even where rendering support lands later (architecture §1.1).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod adjust;
pub mod analysis;
pub mod blend_if;
pub mod comps;
pub mod effects;
pub mod mode;
pub mod pattern;
pub mod slices;
pub mod text;
pub mod text_styles;
pub mod variables;
pub mod vector;
pub mod video;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use adjust::Adjustment;
pub use analysis::{CountGroup, Measurement, MeasurementScale, Note, Ruler};
pub use blend_if::{BlendIf, BlendRange};
pub use comps::{Artboard, ArtboardBackground, CompAppearance, CompLayerState, LayerComp};
pub use effects::{
    Bevel, BevelContour, BevelStyle, BevelTechnique, BevelTexture, Contour, Effect, FxCommon, FxPaint, GlobalLight, Glow, GlowSource, GlowTechnique, Gradient,
    GradientStyle, Satin, Shadow, StrokeFx, StrokePosition,
};
pub use mode::{ColorTable, Duotone, DuotoneInk, StackMode};
pub use pattern::Pattern;
pub use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
pub use photocraft_geom::{Affine, Rect, Size};
pub use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};
pub use slices::{Slice, SliceKind, SliceOrigin, Slices};
pub use text_styles::TextStyles;
pub use variables::{DataSet, DataValue, PixelAlign, PixelMethod, VarKind, VariableDef, Variables};
pub use vector::{
    ClippingPath, FillRule, Knot, LineCap, LineJoin, LiveShape, NamedPath, Path, PathOp, ShapeLayer, ShapeStroke, StrokeAlign, Subpath, VectorMask,
};
pub use video::{Timeline, VideoData, VideoSource};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LayerId(pub u64);

impl LayerId {
    pub fn fresh() -> Self {
        LayerId(next_id())
    }
}

/// Advance the process-wide id counter (shared by layer and document ids) past `max`, so ids
/// loaded from a file never collide with ids minted later.
pub fn ensure_ids_above(max: u64) {
    NEXT_ID.fetch_max(max.saturating_add(1), Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocId(pub u64);

impl DocId {
    pub fn fresh() -> Self {
        DocId(next_id())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locks {
    pub transparency: bool,
    pub pixels: bool,
    pub position: bool,
    pub artboard: bool,
    pub all: bool,
}

/// Photoshop layer colour labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelColor {
    #[default]
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Violet,
    Gray,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerMask {
    /// Grayscale surface; untouched pixels read as `default` (0 = hide, 1 = reveal).
    pub surface: Surface,
    pub enabled: bool,
    pub linked: bool,
    pub density: f32,
    pub feather: f32,
}

impl LayerMask {
    pub fn reveal_all() -> Self {
        Self { surface: Surface::with_default(PixelFormat::GRAY8, &[1.0]), enabled: true, linked: true, density: 1.0, feather: 0.0 }
    }
    pub fn hide_all() -> Self {
        Self { surface: Surface::with_default(PixelFormat::GRAY8, &[0.0]), ..Self::reveal_all() }
    }
    /// Effective mask value at a pixel, including density.
    pub fn value(&self, x: i32, y: i32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let v = self.surface.sample_channel(x, y, 0);
        1.0 - self.density * (1.0 - v)
    }

    /// Effective mask values over `r` (row-major) into `out`, including
    /// density; all ones when disabled.
    pub fn values_into(&self, r: Rect, out: &mut Vec<f32>) {
        let n = r.width() as usize * r.height() as usize;
        if !self.enabled {
            out.clear();
            out.resize(n, 1.0);
            return;
        }
        self.surface.read_region_into(r, out);
        let ch = self.surface.channels();
        if ch > 1 {
            let v: Vec<f32> = out.chunks_exact(ch).map(|p| p[0]).collect();
            *out = v;
        }
        if self.density < 1.0 {
            for v in out.iter_mut() {
                *v = 1.0 - self.density * (1.0 - *v);
            }
        }
    }
}

/// Layer styles. Parsed PSD effect data is kept raw until the effects engine lands (M9).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effects {
    pub enabled: bool,
    pub items: Vec<Effect>,
    /// Original PSD `lfx2` (`lfxs` on groups) block data (object-effects version + descriptor),
    /// written back verbatim on PSD export. Not duplicated in `Layer::psd_blocks`.
    pub psd_raw: Option<Arc<Vec<u8>>>,
    /// Effects reference point (PSD `fxrp`): where linked patterns (Pattern Overlay, pattern
    /// strokes and glows) anchor their tiling. It moves with the layer. `None` = the layer's
    /// top-left.
    pub reference: Option<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub children: Vec<Layer>,
    pub expanded: bool,
    /// Set when this (top-level) group is an artboard: children are clipped to its rect.
    pub artboard: Option<Artboard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Fill {
    Solid(Color),
    /// Gradient between stops at `angle` degrees (`style` geometry, optionally reversed), laid
    /// out in the layer's frame (or the canvas when `align` is off), its centre moved by `offset`.
    Gradient {
        stops: Vec<(f32, Color)>,
        angle: f32,
        scale: f32,
        style: GradientStyle,
        reverse: bool,
        /// Opacity stops `(location, opacity)` (both `0..=1`); empty = each colour stop's alpha.
        /// When set, a pixel's alpha is the colour's alpha times the opacity at that location.
        #[serde(default)]
        opacity_stops: Vec<(f32, f32)>,
        /// Colour midpoints, one per segment between consecutive (sorted) colour stops: where
        /// the segment is half-way (`0..=1` of the segment). Missing entries are 0.5.
        #[serde(default)]
        midpoints: Vec<f32>,
        /// Centre offset as a fraction of the frame (PSD `Ofst`).
        #[serde(default)]
        offset: (f32, f32),
        /// Dither (breaks 8-bit banding with one level of noise).
        #[serde(default)]
        dither: bool,
        /// "Align with layer": lay out in the layer's frame (its masks' bounds), else the canvas.
        #[serde(default = "effects::yes")]
        align: bool,
    },
    /// A pattern from [`Document::patterns`] (looked up by `id`, then `name`).
    Pattern {
        name: String,
        scale: f32,
        #[serde(default)]
        id: String,
        /// Rotation in degrees (counter-clockwise).
        #[serde(default)]
        angle: f32,
        /// "Link with Layer": the pattern origin follows the layer's frame (else the canvas).
        #[serde(default = "effects::yes")]
        link: bool,
        /// Phase (origin offset) in pixels.
        #[serde(default)]
        phase: (f32, f32),
    },
}

impl Fill {
    /// A gradient fill with the classic settings: opaque stops at their alpha, 50 % midpoints,
    /// centred, no dither, aligned with the layer.
    pub fn gradient(stops: Vec<(f32, Color)>, angle: f32, scale: f32, style: GradientStyle, reverse: bool) -> Fill {
        Fill::Gradient { stops, angle, scale, style, reverse, opacity_stops: Vec::new(), midpoints: Vec::new(), offset: (0.0, 0.0), dither: false, align: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextLayer {
    pub text: String,
    pub font_family: String,
    pub size_pt: f32,
    pub color: Color,
    pub transform: Affine,
    /// Rasterized appearance (from PSD or our text engine).
    pub cache: Option<Surface>,
    /// Data of the PSD `TySh` block. On export it replaces the `TySh` entry in
    /// [`Layer::psd_blocks`] (the text engine updates this field when it edits text).
    pub psd_raw: Option<Arc<Vec<u8>>>,
    /// Character style runs (see [`text`]). Empty = one run built from `font_family`, `size_pt`
    /// and `color`, which otherwise mirror the first run (summary for simple UIs).
    pub runs: Vec<text::TextRun>,
    /// Paragraph style runs. Empty = default paragraph style.
    pub paragraphs: Vec<text::ParagraphRun>,
    /// Point or paragraph (box) text.
    pub shape: text::TextShape,
    pub orientation: text::Orientation,
    pub antialias: text::AntiAlias,
    /// Warp Text settings (applied to the glyph outlines when rendering).
    pub warp: Option<text::TextWarp>,
}

impl Default for TextLayer {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: String::new(),
            size_pt: 12.0,
            color: Color::BLACK,
            transform: Affine::IDENTITY,
            cache: None,
            psd_raw: None,
            runs: Vec::new(),
            paragraphs: Vec::new(),
            shape: text::TextShape::Point,
            orientation: text::Orientation::Horizontal,
            antialias: text::AntiAlias::Smooth,
            warp: None,
        }
    }
}

impl TextLayer {
    /// Character runs covering exactly `text.len()` bytes, each on a char boundary.
    pub fn char_runs(&self) -> Vec<text::TextRun> {
        let base = text::CharStyle { font_family: self.font_family.clone(), size_pt: self.size_pt, color: self.color, ..Default::default() };
        normalize_runs(&self.text, self.runs.iter().map(|r| (r.len, r.style.clone())).collect(), base)
            .into_iter()
            .map(|(len, style)| text::TextRun { len, style })
            .collect()
    }
    /// Paragraph runs covering exactly `text.len()` bytes.
    pub fn paragraph_runs(&self) -> Vec<text::ParagraphRun> {
        normalize_runs(&self.text, self.paragraphs.iter().map(|r| (r.len, r.style.clone())).collect(), text::ParagraphStyle::default())
            .into_iter()
            .map(|(len, style)| text::ParagraphRun { len, style })
            .collect()
    }
    /// Copies the first run's family, size and colour into the summary fields.
    pub fn sync_summary(&mut self) {
        if let Some(r) = self.runs.first() {
            self.font_family = r.style.font_family.clone();
            self.size_pt = r.style.size_pt;
            self.color = r.style.color;
        }
    }
}

/// Stretches/truncates `(len, style)` runs to cover `text` exactly, snapping to char boundaries
/// and dropping empty runs (keeps one run for empty text).
fn normalize_runs<S: Clone>(text: &str, runs: Vec<(usize, S)>, base: S) -> Vec<(usize, S)> {
    let total = text.len();
    if total == 0 {
        return vec![(0, runs.into_iter().next().map_or(base, |r| r.1))];
    }
    let mut out: Vec<(usize, S)> = Vec::new();
    let mut at = 0usize;
    for (len, style) in runs {
        if at >= total {
            break;
        }
        let mut end = (at + len).min(total);
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end > at {
            out.push((end - at, style));
            at = end;
        }
    }
    if at < total {
        match out.last_mut() {
            Some(last) => last.0 += total - at,
            None => out.push((total, base.clone())),
        }
    }
    if out.is_empty() {
        out.push((0, base));
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmartObject {
    /// Embedded source file bytes (PSD, PNG, …) or linked path.
    pub source: SmartSource,
    pub transform: Affine,
    pub smart_filters: Vec<SmartFilter>,
    /// Rendered appearance.
    pub cache: Option<Surface>,
    /// Data of the PSD placed-layer block (`SoLd`, `PlLd` or `SoLE`). On export
    /// it replaces that entry in [`Layer::psd_blocks`].
    pub psd_raw: Option<Arc<Vec<u8>>>,
    /// Layer › Smart Filter › Disable Smart Filters turns the whole stack off
    /// without touching each filter's own visibility.
    pub filters_enabled: bool,
    /// The smart filter mask (one per smart object, as in Photoshop): where the
    /// filter stack's result shows over the unfiltered content.
    pub filter_mask: Option<LayerMask>,
    /// Edit › Transform › Warp on the smart object, in source-image coordinates; applied before
    /// `transform` when re-rendering from the source, so warping stays lossless.
    pub warp: Option<photocraft_geom::warp::Warp>,
    /// Layer › Smart Objects › Stack Mode: when set, the source's top-level layers are combined
    /// per pixel with this statistic instead of composited.
    pub stack_mode: Option<StackMode>,
}

impl SmartObject {
    /// A smart object with no filters and no PSD data.
    pub fn new(source: SmartSource, transform: Affine, cache: Option<Surface>) -> Self {
        Self { source, transform, smart_filters: Vec::new(), cache, psd_raw: None, filters_enabled: true, filter_mask: None, warp: None, stack_mode: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SmartSource {
    Embedded { file_name: String, bytes: Arc<Vec<u8>> },
    Linked { path: String },
}

/// A filter applied non-destructively: a command id plus its parameters (data, not code).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmartFilter {
    pub command: String,
    pub params: serde_json::Value,
    pub blend: BlendMode,
    pub opacity: f32,
    pub visible: bool,
}

/// Pixels rendered by another application (Photoshop) for a fill layer.
///
/// Used by the compositor instead of rendering [`Fill`] itself, but only while
/// `fill` still equals the layer's current fill (editing the fill invalidates
/// the cache automatically).
#[derive(Clone, Debug, PartialEq)]
pub struct FillCache {
    /// The fill these pixels were rendered for.
    pub fill: Fill,
    /// Rendered pixels in document coordinates.
    pub surface: Surface,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    Raster(Surface),
    Group(Group),
    Adjustment(Adjustment),
    Fill(Fill),
    Text(TextLayer),
    Shape(ShapeLayer),
    Smart(SmartObject),
}

impl LayerContent {
    pub fn kind_name(&self) -> &'static str {
        match self {
            LayerContent::Raster(_) => "Pixel",
            LayerContent::Group(_) => "Group",
            LayerContent::Adjustment(_) => "Adjustment",
            LayerContent::Fill(_) => "Fill",
            LayerContent::Text(_) => "Type",
            LayerContent::Shape(_) => "Shape",
            LayerContent::Smart(_) => "Smart Object",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub locks: Locks,
    pub blend: BlendMode,
    /// Layer opacity (applies to content and effects).
    pub opacity: f32,
    /// Fill opacity (applies to content but not effects).
    pub fill_opacity: f32,
    /// Clipped to the layer below (clipping mask).
    pub clipped: bool,
    pub mask: Option<LayerMask>,
    /// Vector mask (Layer › Vector Mask), applied together with `mask`. Shape layers keep their
    /// outline in [`ShapeLayer::path`] instead.
    pub vector_mask: Option<VectorMask>,
    pub effects: Effects,
    pub label: LabelColor,
    pub content: LayerContent,
    /// Preserved PSD additional-layer-info blocks (key, data) for lossless
    /// round-trip of anything not modelled above: vector masks, blending
    /// options, layer version, and the original adjustment/fill blocks (the
    /// PSD writer reuses an adjustment or fill block verbatim while it still
    /// decodes to the layer's current parameters, so Levels/Curves/Hue-Sat
    /// extras survive). Keys regenerated from the fields above (`luni`,
    /// `lyid`, `lsct`, `lsdk`, `iOpa`, `lspf`, `lclr`, `lfx2`) are not stored.
    pub psd_blocks: Vec<([u8; 4], Arc<Vec<u8>>)>,
    /// PSD layer id (`lyid`), kept so ids stay stable across round trips.
    /// Cleared by [`Layer::duplicate`].
    pub psd_id: Option<u32>,
    /// Photoshop-rendered pixels for fill layers (see [`FillCache`]).
    pub fill_cache: Option<FillCache>,
    /// Layer › Link Layers: layers sharing a link group move together. `None` = not linked.
    pub link_group: Option<u64>,
    /// Blending Options › Advanced Blending › Channels: bit `i` set = colour channel `i` of the
    /// document's mode (R, G, B / C, M, Y, K / L, a, b / Gray) is left out of blending, so the
    /// backdrop's value is kept there. 0 = every channel blends (the default). PSD `brst`.
    pub excluded_channels: u32,
    /// Blending Options › Blend If: value ranges of this layer and of the layers beneath it
    /// outside which the layer's pixels are hidden. Default = everything blends. PSD layer-record
    /// blending ranges.
    pub blend_if: BlendIf,
    /// Layer › Video Layers frame stack (None for a normal layer).
    pub video: Option<VideoData>,
}

impl Layer {
    pub fn new(name: impl Into<String>, content: LayerContent) -> Self {
        Layer {
            id: LayerId::fresh(),
            name: name.into(),
            visible: true,
            locks: Locks::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            clipped: false,
            mask: None,
            vector_mask: None,
            effects: Effects { enabled: true, ..Default::default() },
            label: LabelColor::None,
            content,
            psd_blocks: Vec::new(),
            psd_id: None,
            fill_cache: None,
            link_group: None,
            excluded_channels: 0,
            blend_if: BlendIf::default(),
            video: None,
        }
    }
    pub fn raster(name: impl Into<String>, format: PixelFormat) -> Self {
        Self::new(name, LayerContent::Raster(Surface::new(format)))
    }
    pub fn group(name: impl Into<String>, children: Vec<Layer>) -> Self {
        let mut l = Self::new(name, LayerContent::Group(Group { children, expanded: true, artboard: None }));
        l.blend = BlendMode::PassThrough;
        l
    }
    pub fn is_group(&self) -> bool {
        matches!(self.content, LayerContent::Group(_))
    }
    pub fn children(&self) -> Option<&[Layer]> {
        match &self.content {
            LayerContent::Group(g) => Some(&g.children),
            _ => None,
        }
    }
    pub fn children_mut(&mut self) -> Option<&mut Vec<Layer>> {
        match &mut self.content {
            LayerContent::Group(g) => Some(&mut g.children),
            _ => None,
        }
    }
    pub fn surface(&self) -> Option<&Surface> {
        match &self.content {
            LayerContent::Raster(s) => Some(s),
            LayerContent::Text(t) => t.cache.as_ref(),
            LayerContent::Shape(s) => s.cache.as_ref(),
            LayerContent::Smart(s) => s.cache.as_ref(),
            _ => None,
        }
    }
    pub fn surface_mut(&mut self) -> Option<&mut Surface> {
        match &mut self.content {
            LayerContent::Raster(s) => Some(s),
            _ => None,
        }
    }
    /// Deep copy with fresh ids (for Duplicate Layer).
    pub fn duplicate(&self) -> Layer {
        let mut l = self.clone();
        l.reassign_ids();
        l
    }
    fn reassign_ids(&mut self) {
        self.id = LayerId::fresh();
        self.psd_id = None;
        if let Some(ch) = self.children_mut() {
            for c in ch {
                c.reassign_ids();
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guides {
    pub horizontal: Vec<f32>,
    pub vertical: Vec<f32>,
}

/// Which areas an alpha channel's overlay colour marks (Channel Options › Color Indicates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorIndicates {
    /// Colour shows over black (unselected) pixels: Photoshop's default.
    #[default]
    MaskedAreas,
    /// Colour shows over white (selected) pixels.
    SelectedAreas,
}

/// An extra (alpha or spot) channel, also used for the temporary Quick Mask channel.
///
/// The surface is grayscale at the document depth; white = selected (1.0), black = masked.
#[derive(Clone, Debug, PartialEq)]
pub struct AlphaChannel {
    pub name: String,
    pub surface: Surface,
    /// Spot colour channels carry their ink colour and solidity.
    pub spot: Option<(Color, f32)>,
    /// Overlay colour shown when the channel is viewed together with the composite.
    pub color: Color,
    /// Overlay opacity (0..1).
    pub opacity: f32,
    pub indicates: ColorIndicates,
}

impl AlphaChannel {
    /// Photoshop's default overlay: red at 50% over masked areas.
    pub const DEFAULT_COLOR: Color = Color::rgb(1.0, 0.0, 0.0);

    pub fn new(name: impl Into<String>, surface: Surface) -> Self {
        Self { name: name.into(), surface, spot: None, color: Self::DEFAULT_COLOR, opacity: 0.5, indicates: ColorIndicates::MaskedAreas }
    }

    /// Overlay coverage (0..1, before opacity) for channel value `v`: spot channels show ink
    /// where the channel is white, alpha channels follow [`ColorIndicates`].
    pub fn overlay_coverage(&self, v: f32) -> f32 {
        if self.spot.is_some() || self.indicates == ColorIndicates::SelectedAreas { v } else { 1.0 - v }
    }
}

/// A raw PSD global tagged block: (signature, key, data).
pub type PsdGlobalBlock = ([u8; 4], [u8; 4], Arc<Vec<u8>>);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub xmp: Option<String>,
    pub exif: Option<Arc<Vec<u8>>>,
    /// Raw PSD image resources we don't model yet: (id, name, data), for lossless round-trip.
    pub psd_resources: Vec<(u16, String, Arc<Vec<u8>>)>,
    /// Raw PSD global additional-layer-info blocks: (signature, key, data),
    /// e.g. `lnk2` embedded smart-object files, `Patt` patterns, `Txt2`.
    pub psd_global_blocks: Vec<PsdGlobalBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub id: DocId,
    pub name: String,
    pub size: Size,
    pub resolution_dpi: f32,
    pub mode: ColorMode,
    pub depth: SampleType,
    pub icc_profile: Option<Arc<Vec<u8>>>,
    /// Bottom-to-top.
    pub layers: Vec<Layer>,
    pub channels: Vec<AlphaChannel>,
    pub guides: Guides,
    /// Active selection as a grayscale coverage surface (None = no selection).
    pub selection: Option<Surface>,
    pub metadata: Metadata,
    /// Global light used by layer effects (PSD resources 1037/1049).
    pub global_light: GlobalLight,
    /// Saved paths (Paths panel), top to bottom.
    pub paths: Vec<NamedPath>,
    /// The (unsaved) work path.
    pub work_path: Option<Path>,
    /// Clipping path used on export (names one of `paths`).
    pub clipping_path: Option<ClippingPath>,
    /// Select › Edit in Quick Mask Mode: the temporary "Quick Mask" channel while the mode is on.
    /// Part of the document so entering and leaving are history steps (as in Photoshop).
    pub quick_mask: Option<AlphaChannel>,
    /// Patterns stored with the document (PSD `Patt`/`Pat2`/`Pat3`): those its layers use, plus
    /// any imported with it.
    pub patterns: Vec<Pattern>,
    /// Indexed Color palette (Image › Mode › Color Table). Set while `mode` is Indexed.
    pub color_table: Option<ColorTable>,
    /// Duotone inks. Set while `mode` is Duotone.
    pub duotone: Option<Duotone>,
    /// Window › Layer Comps, top to bottom as listed in the panel.
    pub layer_comps: Vec<LayerComp>,
    /// Id of the comp applied last (PSD `lastAppliedComp`); None = the document's own state.
    pub last_applied_comp: Option<u32>,
    /// "Last Document State": layer state saved when a comp is applied over the document's own
    /// state, restored by `layerComp.restoreLastDocumentState`. Its id is 0.
    pub last_document_state: Option<LayerComp>,
    /// Image › Analysis: measurement scale, Count tool groups and the Ruler line.
    pub measurement: Measurement,
    /// Note tool annotations (PSD `Anno`).
    pub notes: Vec<Note>,
    /// Character and paragraph styles (Window › Character Styles / Paragraph Styles).
    pub text_styles: TextStyles,
    /// Web slices (Slice tool, layer-based slices; PSD resource 1050). Auto slices are derived.
    pub slices: Slices,
    /// Image › Variables and Data Sets (data-driven graphics).
    pub variables: Variables,
    /// Window › Timeline (None until a video timeline is created).
    pub timeline: Option<Timeline>,
}

/// Where a layer lives in the tree: indices from the root down.
pub type LayerPath = Vec<usize>;

impl Document {
    pub fn new(name: impl Into<String>, size: Size, mode: ColorMode, depth: SampleType) -> Self {
        Document {
            id: DocId::fresh(),
            name: name.into(),
            size,
            resolution_dpi: 72.0,
            mode,
            depth,
            icc_profile: None,
            layers: Vec::new(),
            channels: Vec::new(),
            guides: Guides::default(),
            selection: None,
            metadata: Metadata::default(),
            global_light: GlobalLight::default(),
            paths: Vec::new(),
            work_path: None,
            clipping_path: None,
            quick_mask: None,
            patterns: Vec::new(),
            color_table: None,
            duotone: None,
            layer_comps: Vec::new(),
            last_applied_comp: None,
            last_document_state: None,
            measurement: Measurement::default(),
            notes: Vec::new(),
            text_styles: TextStyles::default(),
            slices: Slices::default(),
            variables: Variables::default(),
            timeline: None,
        }
    }

    /// New document with a filled "Background" layer (like File → New).
    pub fn with_background(name: impl Into<String>, size: Size, mode: ColorMode, depth: SampleType, fill: Color) -> Self {
        let mut d = Self::new(name, size, mode, depth);
        let mut bg = Layer::raster("Background", d.pixel_format());
        bg.locks.transparency = true;
        bg.locks.position = true;
        if let Some(s) = bg.surface_mut() {
            let rgba = fill.to_rgb();
            let px = photocraft_raster::from_rgba(&s.format(), [rgba[0], rgba[1], rgba[2], fill.alpha]);
            s.fill_rect(d.bounds(), &px);
        }
        d.layers.push(bg);
        d
    }

    /// Pixel format for new raster layers in this document.
    pub fn pixel_format(&self) -> PixelFormat {
        let mode = match self.mode {
            ColorMode::Indexed | ColorMode::Multichannel => ColorMode::Rgb,
            ColorMode::Bitmap | ColorMode::Duotone => ColorMode::Grayscale,
            m => m,
        };
        PixelFormat::new(mode, self.depth, true)
    }

    pub fn bounds(&self) -> Rect {
        Rect::from_size(self.size)
    }

    /// Depth-first walk yielding `(path, depth, layer)` bottom-to-top.
    pub fn walk(&self) -> Vec<(LayerPath, usize, &Layer)> {
        fn rec<'a>(layers: &'a [Layer], prefix: &mut LayerPath, out: &mut Vec<(LayerPath, usize, &'a Layer)>) {
            for (i, l) in layers.iter().enumerate() {
                prefix.push(i);
                out.push((prefix.clone(), prefix.len() - 1, l));
                if let Some(ch) = l.children() {
                    rec(ch, prefix, out);
                }
                prefix.pop();
            }
        }
        let mut out = Vec::new();
        rec(&self.layers, &mut Vec::new(), &mut out);
        out
    }

    pub fn layer_count(&self) -> usize {
        self.walk().len()
    }

    pub fn path_of(&self, id: LayerId) -> Option<LayerPath> {
        self.walk().into_iter().find(|(_, _, l)| l.id == id).map(|(p, _, _)| p)
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        let path = self.path_of(id)?;
        self.layer_at(&path)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        let path = self.path_of(id)?;
        self.layer_at_mut(&path)
    }

    pub fn layer_at(&self, path: &[usize]) -> Option<&Layer> {
        let (first, rest) = path.split_first()?;
        let mut cur = self.layers.get(*first)?;
        for &i in rest {
            cur = cur.children()?.get(i)?;
        }
        Some(cur)
    }

    pub fn layer_at_mut(&mut self, path: &[usize]) -> Option<&mut Layer> {
        let (first, rest) = path.split_first()?;
        let mut cur = self.layers.get_mut(*first)?;
        for &i in rest {
            cur = cur.children_mut()?.get_mut(i)?;
        }
        Some(cur)
    }

    /// The sibling list containing `path` and the index within it.
    fn siblings_mut(&mut self, path: &[usize]) -> Option<(&mut Vec<Layer>, usize)> {
        let (&last, parent) = path.split_last()?;
        if parent.is_empty() {
            return Some((&mut self.layers, last));
        }
        let p = self.layer_at_mut(parent)?;
        Some((p.children_mut()?, last))
    }

    /// Insert `layer` directly above the layer `above` (or at the top of the root if None).
    pub fn insert_above(&mut self, above: Option<LayerId>, layer: Layer) -> LayerId {
        let id = layer.id;
        match above.and_then(|a| self.path_of(a)).and_then(|path| self.siblings_mut(&path)) {
            Some((sib, idx)) => sib.insert(idx + 1, layer),
            None => self.layers.push(layer),
        }
        id
    }

    pub fn remove(&mut self, id: LayerId) -> Option<Layer> {
        let path = self.path_of(id)?;
        let (sib, idx) = self.siblings_mut(&path)?;
        Some(sib.remove(idx))
    }

    /// Move a layer up (+1) or down (-1) among its siblings. Returns false at the ends.
    pub fn shift(&mut self, id: LayerId, delta: i32) -> bool {
        let Some(path) = self.path_of(id) else { return false };
        let Some((sib, idx)) = self.siblings_mut(&path) else { return false };
        let to = idx as i64 + delta as i64;
        if to < 0 || to >= sib.len() as i64 {
            return false;
        }
        let l = sib.remove(idx);
        sib.insert(to as usize, l);
        true
    }

    /// Next unused "Layer N" name.
    pub fn next_layer_name(&self, base: &str) -> String {
        let names: std::collections::HashSet<&str> = self.walk().into_iter().map(|(_, _, l)| l.name.as_str()).collect();
        // At most `names.len() + 1` candidates are needed, so the search always succeeds.
        (1..=names.len() + 1).map(|n| format!("{base} {n}")).find(|n| !names.contains(n.as_str())).unwrap_or_else(|| base.to_string())
    }

    /// Top-most layer id, useful as the default active layer.
    pub fn top_layer(&self) -> Option<LayerId> {
        self.layers.last().map(|l| l.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::with_background("t", Size::new(100, 50), ColorMode::Rgb, SampleType::U8, Color::WHITE)
    }

    #[test]
    fn new_document_has_locked_background() {
        let d = doc();
        assert_eq!(d.layers.len(), 1);
        let bg = &d.layers[0];
        assert_eq!(bg.name, "Background");
        assert!(bg.locks.transparency);
        let s = bg.surface().unwrap();
        assert_eq!(s.pixel(0, 0), vec![1.0, 1.0, 1.0, 1.0]);
        assert_eq!(s.pixel(99, 49), vec![1.0, 1.0, 1.0, 1.0]);
        assert_eq!(s.pixel(100, 0), vec![0.0; 4]);
    }

    #[test]
    fn pixel_format_follows_mode_and_depth() {
        let d = Document::new("c", Size::new(1, 1), ColorMode::Cmyk, SampleType::U16);
        assert_eq!(d.pixel_format(), PixelFormat::new(ColorMode::Cmyk, SampleType::U16, true));
        let b = Document::new("b", Size::new(1, 1), ColorMode::Bitmap, SampleType::U8);
        assert_eq!(b.pixel_format().mode, ColorMode::Grayscale);
    }

    #[test]
    fn insert_remove_shift() {
        let mut d = doc();
        let bg = d.layers[0].id;
        let a = d.insert_above(Some(bg), Layer::raster("A", d.pixel_format()));
        let b = d.insert_above(Some(bg), Layer::raster("B", d.pixel_format()));
        // order bottom->top: bg, B, A
        let names: Vec<_> = d.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "B", "A"]);
        assert!(d.shift(b, 1));
        assert_eq!(d.layers[2].id, b);
        assert!(!d.shift(b, 1));
        assert_eq!(d.remove(a).unwrap().name, "A");
        assert!(d.layer(a).is_none());
    }

    #[test]
    fn groups_walk_and_paths() {
        let mut d = doc();
        let inner = Layer::raster("inner", d.pixel_format());
        let inner_id = inner.id;
        let g = Layer::group("G", vec![inner]);
        let gid = d.insert_above(None, g);
        let walk: Vec<_> = d.walk().into_iter().map(|(p, depth, l)| (p, depth, l.name.clone())).collect();
        assert_eq!(walk, vec![(vec![0], 0, "Background".into()), (vec![1], 0, "G".into()), (vec![1, 0], 1, "inner".into())]);
        assert_eq!(d.path_of(inner_id), Some(vec![1, 0]));
        assert_eq!(d.layer(gid).unwrap().blend, BlendMode::PassThrough);
        // insert above a nested layer stays inside the group
        let n = d.insert_above(Some(inner_id), Layer::raster("n", d.pixel_format()));
        assert_eq!(d.path_of(n), Some(vec![1, 1]));
        assert_eq!(d.layer_count(), 4);
    }

    #[test]
    fn duplicate_assigns_fresh_ids_recursively() {
        let g = Layer::group("G", vec![Layer::raster("x", PixelFormat::RGBA8)]);
        let dup = g.duplicate();
        assert_ne!(g.id, dup.id);
        assert_ne!(g.children().unwrap()[0].id, dup.children().unwrap()[0].id);
        assert_eq!(dup.children().unwrap()[0].name, "x");
    }

    #[test]
    fn duplicate_clears_psd_id_but_keeps_blocks() {
        let mut l = Layer::raster("x", PixelFormat::RGBA8);
        l.psd_id = Some(7);
        l.psd_blocks.push((*b"vmsk", Arc::new(vec![1])));
        let d = l.duplicate();
        assert_eq!(d.psd_id, None);
        assert_eq!(d.psd_blocks, l.psd_blocks);
    }

    #[test]
    fn layer_names_increment() {
        let mut d = doc();
        assert_eq!(d.next_layer_name("Layer"), "Layer 1");
        d.insert_above(None, Layer::raster("Layer 1", d.pixel_format()));
        assert_eq!(d.next_layer_name("Layer"), "Layer 2");
    }

    #[test]
    fn mask_density() {
        let mut m = LayerMask::hide_all();
        assert_eq!(m.value(5, 5), 0.0);
        m.density = 0.5;
        assert!((m.value(5, 5) - 0.5).abs() < 1e-6);
        m.enabled = false;
        assert_eq!(m.value(5, 5), 1.0);
    }

    #[test]
    fn snapshot_clone_is_independent() {
        let mut d = doc();
        let snap = d.clone();
        let bg = d.layers[0].id;
        d.layer_mut(bg).unwrap().surface_mut().unwrap().write_pixel(0, 0, &[0.0, 0.0, 0.0, 1.0]);
        assert_eq!(snap.layers[0].surface().unwrap().pixel(0, 0), vec![1.0; 4]);
    }
}

#[cfg(test)]
mod id_tests {
    #[test]
    fn ensure_ids_above_advances_counter() {
        let far = super::LayerId::fresh().0 + 1000;
        super::ensure_ids_above(far);
        assert!(super::LayerId::fresh().0 > far);
        super::ensure_ids_above(3); // never moves backwards
        assert!(super::DocId::fresh().0 > far);
    }
}

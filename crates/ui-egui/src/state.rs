//! UI state as plain data, so the control channel (and later MCP) can read and drive every
//! aspect of the interface: tools, zoom, panels, dialogs and windows.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Move,
    RectMarquee,
    EllipseMarquee,
    Lasso,
    PolygonLasso,
    MagicWand,
    Crop,
    Eyedropper,
    Ruler,
    Note,
    Count,
    Brush,
    Pencil,
    Eraser,
    BackgroundEraser,
    MagicEraser,
    Gradient,
    PaintBucket,
    Type,
    Hand,
    Zoom,
    SpotHealing,
    Healing,
    CloneStamp,
    HistoryBrush,
    Blur,
    Sharpen,
    Smudge,
    Dodge,
    Burn,
    Sponge,
    QuickSelection,
    ObjectSelection,
    Pen,
    PathSelection,
    Rectangle,
    EllipseShape,
    Triangle,
    Polygon,
    Line,
    CustomShape,
    Slice,
    SliceSelect,
}

impl Tool {
    pub const ALL: [Tool; 43] = [
        Tool::Move,
        Tool::RectMarquee,
        Tool::EllipseMarquee,
        Tool::Lasso,
        Tool::PolygonLasso,
        Tool::MagicWand,
        Tool::Crop,
        Tool::Eyedropper,
        Tool::Ruler,
        Tool::Note,
        Tool::Count,
        Tool::Brush,
        Tool::Pencil,
        Tool::Eraser,
        Tool::BackgroundEraser,
        Tool::MagicEraser,
        Tool::Gradient,
        Tool::PaintBucket,
        Tool::Type,
        Tool::Hand,
        Tool::Zoom,
        Tool::SpotHealing,
        Tool::Healing,
        Tool::CloneStamp,
        Tool::HistoryBrush,
        Tool::Blur,
        Tool::Sharpen,
        Tool::Smudge,
        Tool::Dodge,
        Tool::Burn,
        Tool::Sponge,
        Tool::QuickSelection,
        Tool::ObjectSelection,
        Tool::Pen,
        Tool::PathSelection,
        Tool::Rectangle,
        Tool::EllipseShape,
        Tool::Triangle,
        Tool::Polygon,
        Tool::Line,
        Tool::CustomShape,
        Tool::Slice,
        Tool::SliceSelect,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Move => "Move Tool",
            Tool::RectMarquee => "Rectangular Marquee Tool",
            Tool::EllipseMarquee => "Elliptical Marquee Tool",
            Tool::Brush => "Brush Tool",
            Tool::Pencil => "Pencil Tool",
            Tool::Eraser => "Eraser Tool",
            Tool::BackgroundEraser => "Background Eraser Tool",
            Tool::MagicEraser => "Magic Eraser Tool",
            Tool::Eyedropper => "Eyedropper Tool",
            Tool::Ruler => "Ruler Tool",
            Tool::Note => "Note Tool",
            Tool::Count => "Count Tool",
            Tool::Lasso => "Lasso Tool",
            Tool::PolygonLasso => "Polygonal Lasso Tool",
            Tool::MagicWand => "Magic Wand Tool",
            Tool::Crop => "Crop Tool",
            Tool::Slice => "Slice Tool",
            Tool::SliceSelect => "Slice Select Tool",
            Tool::Gradient => "Gradient Tool",
            Tool::PaintBucket => "Paint Bucket Tool",
            Tool::Type => "Horizontal Type Tool",
            Tool::Hand => "Hand Tool",
            Tool::Zoom => "Zoom Tool",
            Tool::SpotHealing => "Spot Healing Brush Tool",
            Tool::Healing => "Healing Brush Tool",
            Tool::CloneStamp => "Clone Stamp Tool",
            Tool::HistoryBrush => "History Brush Tool",
            Tool::Blur => "Blur Tool",
            Tool::Sharpen => "Sharpen Tool",
            Tool::Smudge => "Smudge Tool",
            Tool::Dodge => "Dodge Tool",
            Tool::Burn => "Burn Tool",
            Tool::Sponge => "Sponge Tool",
            Tool::QuickSelection => "Quick Selection Tool",
            Tool::ObjectSelection => "Object Selection Tool",
            Tool::Pen => "Pen Tool",
            Tool::PathSelection => "Path Selection Tool",
            Tool::Rectangle => "Rectangle Tool",
            Tool::EllipseShape => "Ellipse Tool",
            Tool::Triangle => "Triangle Tool",
            Tool::Polygon => "Polygon Tool",
            Tool::Line => "Line Tool",
            Tool::CustomShape => "Custom Shape Tool",
        }
    }
    /// Retouching and painting tools that stroke with the brush (share the brush cursor and chip).
    pub fn is_brushlike(self) -> bool {
        matches!(
            self,
            Tool::Brush
                | Tool::Pencil
                | Tool::Eraser
                | Tool::BackgroundEraser
                | Tool::SpotHealing
                | Tool::Healing
                | Tool::CloneStamp
                | Tool::HistoryBrush
                | Tool::Blur
                | Tool::Sharpen
                | Tool::Smudge
                | Tool::Dodge
                | Tool::Burn
                | Tool::Sponge
        )
    }
    /// Photoshop default single-key shortcut (`'\0'` = none, e.g. the Blur group).
    pub fn key(self) -> char {
        match self {
            Tool::Move => 'V',
            Tool::RectMarquee | Tool::EllipseMarquee => 'M',
            Tool::Brush | Tool::Pencil => 'B',
            Tool::Eraser | Tool::BackgroundEraser | Tool::MagicEraser => 'E',
            Tool::Eyedropper | Tool::Ruler | Tool::Note | Tool::Count => 'I',
            Tool::Lasso | Tool::PolygonLasso => 'L',
            Tool::MagicWand => 'W',
            Tool::Crop | Tool::Slice | Tool::SliceSelect => 'C',
            Tool::Gradient | Tool::PaintBucket => 'G',
            Tool::Type => 'T',
            Tool::Hand => 'H',
            Tool::Zoom => 'Z',
            Tool::SpotHealing | Tool::Healing => 'J',
            Tool::CloneStamp => 'S',
            Tool::HistoryBrush => 'Y',
            Tool::Blur | Tool::Sharpen | Tool::Smudge => '\0',
            Tool::Dodge | Tool::Burn | Tool::Sponge => 'O',
            Tool::QuickSelection | Tool::ObjectSelection => 'W',
            Tool::Pen => 'P',
            Tool::PathSelection => 'A',
            Tool::Rectangle | Tool::EllipseShape | Tool::Triangle | Tool::Polygon | Tool::Line | Tool::CustomShape => 'U',
        }
    }
    /// Glyph drawn in the toolbar (vector icons come later).
    pub fn glyph(self) -> &'static str {
        match self {
            Tool::Move => "✥",
            Tool::RectMarquee => "⬚",
            Tool::EllipseMarquee => "◌",
            Tool::Brush => "🖌",
            Tool::Eraser => "⌫",
            Tool::Eyedropper => "💧",
            Tool::Lasso | Tool::PolygonLasso => "L",
            Tool::MagicWand => "W",
            Tool::Crop => "C",
            Tool::Gradient | Tool::PaintBucket => "G",
            Tool::Type => "T",
            Tool::Hand => "✋",
            Tool::Zoom => "🔍",
            _ => "•",
        }
    }
    pub fn from_name(s: &str) -> Option<Tool> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        Tool::ALL.into_iter().find(|t| {
            let a = format!("{t:?}").to_ascii_lowercase();
            n == a || n == a.replace("marquee", "") || n == t.label().to_ascii_lowercase().replace(' ', "")
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Panels {
    pub layers: bool,
    pub history: bool,
    pub properties: bool,
    pub color: bool,
    pub navigator: bool,
    pub toolbar: bool,
    pub options_bar: bool,
    pub status_bar: bool,
    /// Window › Brush Settings (F5): floating, like Photoshop's.
    #[serde(default)]
    pub brush_settings: bool,
    /// Window › Character / Paragraph: the Character | Paragraph dock group (#150).
    #[serde(default)]
    pub character: bool,
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            layers: true,
            history: false,
            properties: true,
            color: true,
            navigator: false,
            toolbar: true,
            options_bar: true,
            status_bar: true,
            brush_settings: false,
            character: false,
        }
    }
}

/// A modal or modeless dialog, identified by `id`, with editable fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dialog {
    pub id: u64,
    pub kind: DialogKind,
    /// Field values by name. Dialog widgets read and write these; so can automation.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialogKind {
    NewDocument,
    About,
    /// Parameter dialog for a command (generated from its params).
    Command,
    Error,
    /// Photoshop Layer Style dialog (see `layer_style.rs`).
    LayerStyle,
}

/// Per-document view (camera) state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// Screen pixels per document pixel.
    pub zoom: f32,
    /// Document-space point shown at the canvas centre.
    pub center: [f32; 2],
    /// Recompute fit-to-screen on next frame.
    pub fit_pending: bool,
    /// Document size this view last showed; a change (Image/Canvas Size, crop) re-centres it.
    #[serde(default)]
    pub doc_size: [u32; 2],
}

impl Default for View {
    fn default() -> Self {
        Self { zoom: 1.0, center: [0.0, 0.0], fit_pending: true, doc_size: [0, 0] }
    }
}

/// An extra OS window showing a document ("Window → Arrange → New Window for Document").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocWindow {
    pub id: u64,
    pub document: usize,
    pub view: View,
    pub open: bool,
}

/// Options-bar state for tools (Photoshop keeps these per tool).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolOptions {
    pub tolerance: f32,
    pub contiguous: bool,
    pub anti_alias: bool,
    pub sample_all_layers: bool,
    pub feather: f32,
    /// linear | radial | angle | reflected | diamond
    pub gradient_style: String,
    pub gradient_reverse: bool,
    /// Gradient tool: dither to reduce 8-bit banding (Photoshop default on).
    pub gradient_dither: bool,
    /// Gradient tool mode: false = "Gradient" (live: a Gradient Fill layer, editable on canvas),
    /// true = "Classic gradient" (paints the pixels).
    pub gradient_classic: bool,
    pub fill_opacity: f32,
    /// Paint Bucket fill source: false = Foreground colour, true = Pattern (Patterns panel selection).
    pub bucket_fill_pattern: bool,
    /// Type tool: family, style name, size (pt), anti-aliasing and paragraph alignment.
    pub type_font: String,
    pub type_style: String,
    pub type_size: f32,
    pub type_aa: String,
    pub type_align: String,
    /// Clone Stamp / Healing Brush.
    pub clone_aligned: bool,
    /// current | currentAndBelow | all
    pub clone_sample: String,
    /// Spot Healing: contentAware | createTexture | proximityMatch
    pub spot_type: String,
    /// Dodge/Burn: shadows | midtones | highlights, exposure %, protect tones.
    pub tone_range: String,
    pub exposure: f32,
    pub protect_tones: bool,
    /// Sponge: desaturate | saturate, vibrance.
    pub sponge_mode: String,
    pub vibrance: bool,
    /// Blur/Sharpen/Smudge strength %, Sharpen protect detail, Smudge finger painting.
    pub strength: f32,
    pub protect_detail: bool,
    pub finger_painting: bool,
    /// Quick Selection enhance edge.
    pub enhance_edge: bool,
    /// Pen: "path" (work path) or "shape" (shape layer).
    pub vector_mode: String,
    /// Shape tools: fill with the foreground colour, stroke width (0 = none), rectangle corner
    /// radius, polygon sides, line weight.
    pub shape_fill: bool,
    pub stroke_width: f32,
    pub corner_radius: f32,
    pub polygon_sides: u32,
    pub line_weight: f32,
    /// Marquee options bar: "normal" | "fixedRatio" | "fixedSize", with the ratio or size.
    #[serde(default = "default_marquee_style")]
    pub marquee_style: String,
    #[serde(default = "one")]
    pub marquee_width: f32,
    #[serde(default = "one")]
    pub marquee_height: f32,
    /// Move tool: Auto-Select (with "layer" or "group" target) and Show Transform Controls.
    /// Auto-Select is on by default, as in current Photoshop; ⌘/Ctrl-click inverts it.
    #[serde(default = "yes")]
    pub move_auto_select: bool,
    #[serde(default = "default_move_target")]
    pub move_target: String,
    #[serde(default)]
    pub move_show_transform: bool,
    /// Crop options bar: aspect ratio ("" = Ratio/unconstrained, "w:h", or "original") and
    /// Delete Cropped Pixels.
    #[serde(default)]
    pub crop_ratio: String,
    #[serde(default = "yes")]
    pub crop_delete: bool,
    /// Magic Eraser opacity % (tolerance, anti-alias, contiguous and sample-all are shared with the
    /// Magic Wand and Paint Bucket).
    pub magic_eraser_opacity: f32,
    /// Background Eraser: sampling (continuous | once | backgroundSwatch), limits (discontiguous |
    /// contiguous | findEdges), tolerance % and Protect Foreground Color.
    pub bg_sampling: String,
    pub bg_limits: String,
    pub bg_tolerance: f32,
    pub bg_protect_fg: bool,
    /// Zoom tool › Scrubby Zoom: dragging left/right zooms continuously (else a zoom rectangle).
    #[serde(default = "yes")]
    pub zoom_scrubby: bool,
    /// Pencil › Auto Erase: a stroke that starts on the foreground colour paints the background colour.
    #[serde(default)]
    pub pencil_auto_erase: bool,
}

fn yes() -> bool {
    true
}

fn default_move_target() -> String {
    "layer".into()
}

fn default_marquee_style() -> String {
    "normal".into()
}

fn one() -> f32 {
    1.0
}

impl Default for ToolOptions {
    fn default() -> Self {
        Self {
            tolerance: 32.0,
            contiguous: true,
            anti_alias: true,
            sample_all_layers: false,
            feather: 0.0,
            gradient_style: "linear".into(),
            gradient_reverse: false,
            gradient_dither: true,
            gradient_classic: false,
            fill_opacity: 100.0,
            bucket_fill_pattern: false,
            type_font: "Inter".into(),
            type_style: "Regular".into(),
            type_size: 48.0,
            type_aa: "sharp".into(),
            type_align: "left".into(),
            clone_aligned: true,
            clone_sample: "current".into(),
            spot_type: "contentAware".into(),
            tone_range: "midtones".into(),
            exposure: 50.0,
            protect_tones: true,
            sponge_mode: "desaturate".into(),
            vibrance: true,
            strength: 50.0,
            protect_detail: true,
            finger_painting: false,
            enhance_edge: false,
            vector_mode: "path".into(),
            shape_fill: true,
            stroke_width: 0.0,
            corner_radius: 0.0,
            polygon_sides: 5,
            line_weight: 3.0,
            marquee_style: default_marquee_style(),
            marquee_width: 1.0,
            marquee_height: 1.0,
            move_auto_select: true,
            move_target: default_move_target(),
            move_show_transform: false,
            crop_ratio: String::new(),
            crop_delete: true,
            magic_eraser_opacity: 100.0,
            bg_sampling: "continuous".into(),
            bg_limits: "contiguous".into(),
            bg_tolerance: 50.0,
            bg_protect_fg: false,
            zoom_scrubby: true,
            pencil_auto_erase: false,
        }
    }
}

/// Free Transform in progress: the source frame `rect` and where its corners currently are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransformSession {
    pub session: u64,
    pub layer: u64,
    pub rect: [f64; 4],
    /// Corners clockwise from top-left (document px).
    pub quad: [[f64; 2]; 4],
    /// Reference point (rotation / ⌥-scale centre).
    pub pivot: [f64; 2],
    pub interpolation: String,
    /// Warp mode (Edit › Transform › Warp): the warp being edited over `rect`, in document px.
    #[serde(default)]
    pub warp: Option<photocraft_geom::warp::Warp>,
    /// Select › Transform Selection: the box transforms the selection outline, not pixels.
    #[serde(default)]
    pub selection: bool,
    /// `edit.transform`'s `"target"` when the box moves an unlinked layer mask, an alpha channel or
    /// the Quick Mask by itself (`None`: the layer, with its linked masks).
    #[serde(default)]
    pub target: Option<serde_json::Value>,
}

/// In-progress inline type editing (Type tool). Offsets are character indices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextEdit {
    pub layer: u64,
    pub caret: usize,
    pub anchor: usize,
    /// History coalescing key: the whole editing session is one "Edit Type Layer" step.
    pub session: String,
    /// The layer was created by this session (its name follows the text; empty on commit = delete).
    pub created: bool,
    #[serde(skip)]
    pub dragging: bool,
    /// IME composition in progress: (start, length) in characters. The preedit text lives in the
    /// layer so it lays out like typed text; each IME update replaces it.
    #[serde(skip)]
    pub preedit: Option<(usize, usize)>,
}

/// View-menu overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extras {
    pub rulers: bool,
    pub grid: bool,
    pub guides: bool,
    pub snap: bool,
    pub lock_guides: bool,
}

impl Default for Extras {
    fn default() -> Self {
        Self { rulers: false, grid: false, guides: true, snap: true, lock_guides: false }
    }
}

/// Selected tab per dock card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockTabs {
    pub properties: usize,
    pub color: usize,
    pub layers: usize,
    /// Navigator | Histogram | Info.
    pub navigator: usize,
    /// History | Actions.
    pub history: usize,
    /// Character | Paragraph.
    pub character: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiState {
    pub tool: Tool,
    /// Recently opened file paths, most-recent first (File › Open Recent). Capped; de-duplicated.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Inline type editing session, if any.
    #[serde(default)]
    pub text_edit: Option<TextEdit>,
    /// Free Transform session, if any.
    #[serde(default)]
    pub transform: Option<TransformSession>,
    /// Clone Stamp / Healing source point (⌥-click) and the aligned offset once a stroke started.
    #[serde(default)]
    pub clone_source: Option<[f64; 2]>,
    #[serde(default)]
    pub clone_offset: Option<[f64; 2]>,
    /// Painting targets the active layer's mask instead of its pixels.
    #[serde(default)]
    pub mask_target: bool,
    /// The active layer's vector mask is targeted (its Layers thumbnail is bracketed): the path
    /// tools edit it (#196). Never set together with `mask_target`.
    #[serde(default)]
    pub vector_mask_target: bool,
    /// Brush Preset picker opened by a right-click on the canvas: its screen position (points).
    #[serde(default)]
    pub brush_picker: Option<[f32; 2]>,
    /// Layers under the pointer, listed by a right-click on the canvas with the Move tool or
    /// ⌘/Ctrl+right-click with any tool (`layer_pick_ui`, #307).
    #[serde(default)]
    pub layer_menu: Option<crate::layer_pick_ui::LayerMenu>,
    /// Smoothing is a per-tool option (Brush and Eraser each keep theirs): the tool whose
    /// smoothing the session brush holds, and the other tools' saved values.
    #[serde(default)]
    pub smoothing_tool: Option<Tool>,
    #[serde(default)]
    pub tool_smoothing: Vec<(Tool, photocraft_engine::paint::brush::Smoothing)>,
    /// Pen path under construction.
    #[serde(default)]
    pub pen: Option<crate::vector_ui::PenPath>,
    /// Selected row in the Paths panel ("work" or a saved path name).
    #[serde(default)]
    pub selected_path: Option<String>,
    /// Layers panel kind filter ("pixel", "adjustment", "type", "shape", "smart"); empty = all.
    #[serde(default)]
    pub layer_filter: Vec<String>,
    /// Recorded actions (Actions panel).
    #[serde(default)]
    pub actions: crate::actions::Actions,
    /// Layer Comps panel: the selected comp (by comp id).
    #[serde(default)]
    pub layer_comp_selected: Option<u32>,
    /// Preset panels (Gradients, Patterns, Styles, Shapes, Tool Presets, Clone Source).
    #[serde(default)]
    pub presets_ui: crate::preset_panels::PresetUi,
    /// Character/Paragraph Styles, Glyphs and Check Spelling (see `type_panels_ui`).
    #[serde(default)]
    pub type_panels: crate::type_panels_ui::TypePanelsUi,
    /// Ruler/Count/Note tools, Measurement Log and Notes panels (see `analysis_ui`).
    #[serde(default)]
    pub analysis: crate::analysis_ui::AnalysisUi,
    pub timeline: crate::timeline_ui::TimelineUi,
    /// Slice and Slice Select tools (see `slice_ui`).
    #[serde(default)]
    pub slices: crate::slice_ui::SliceUi,
    /// Modifier Keys panel, custom pixel aspect ratios, workspace dialogs (see `workspace_ui`).
    #[serde(default)]
    pub shell: crate::workspace_ui::ShellUi,
    /// View extras: rulers (⌘R), grid (⌘'), guides (⌘;), snapping (⇧⌘;), locked guides (⌥⌘;).
    #[serde(default)]
    pub extras: Extras,
    /// View / Window / Type preferences: screen mode, Extras, Show and Snap To, flip, arrangement.
    #[serde(default)]
    pub view: crate::view_cmds::ViewOptions,
    pub panels: Panels,
    /// Views per open document (index-aligned with the session's documents).
    pub views: Vec<View>,
    pub dialogs: Vec<Dialog>,
    pub windows: Vec<DocWindow>,
    pub theme: crate::theme::ThemeKind,
    pub workspace: String,
    pub palette_open: bool,
    pub dock_tabs: DockTabs,
    /// Right-dock group order, heights and collapsed groups (see `dock`).
    #[serde(default)]
    pub dock: crate::dock::DockLayout,
    /// Brush Settings: selected section (0 = Brush Tip Shape) and tab (0 settings, 1 Brushes).
    #[serde(default)]
    pub brush_section: usize,
    #[serde(default)]
    pub brush_tab: usize,
    /// Brushes panel: collapsed groups and the search filter.
    #[serde(default)]
    pub brushes_panel: crate::brush_panel::BrushesPanelState,
    /// Marquee options-bar mode: 0 new, 1 add, 2 subtract, 3 intersect (modifier keys override).
    #[serde(default)]
    pub selection_mode: u8,
    #[serde(default)]
    pub tool_options: ToolOptions,
    /// In-progress polygonal lasso vertices (document coordinates).
    #[serde(default)]
    pub polygon: Vec<[f64; 2]>,
    /// Crop tool rectangle being edited [x0, y0, x1, y1] (document coordinates).
    #[serde(default)]
    pub crop_rect: Option<[f64; 4]>,
    pub next_id: u64,
    /// Last status message (errors from commands, hints).
    pub status: String,
    /// The status message is an error (shown in the warning colour).
    #[serde(default)]
    pub status_error: bool,
    /// Non-blocking notices (import/export warnings, files that couldn't open), newest last.
    #[serde(default)]
    pub notices: Vec<crate::notices::Notice>,
    /// Status bar info field, Home screen (see `chrome_ui`).
    #[serde(default)]
    pub chrome: crate::chrome_ui::ChromeState,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
            recent_files: Vec::new(),
            text_edit: None,
            transform: None,
            mask_target: false,
            vector_mask_target: false,
            brush_picker: None,
            layer_menu: None,
            smoothing_tool: None,
            tool_smoothing: Vec::new(),
            clone_source: None,
            clone_offset: None,
            extras: Extras::default(),
            view: Default::default(),
            actions: Default::default(),
            layer_comp_selected: None,
            presets_ui: Default::default(),
            type_panels: Default::default(),
            analysis: Default::default(),
            timeline: Default::default(),
            slices: Default::default(),
            shell: Default::default(),
            layer_filter: Vec::new(),
            pen: None,
            selected_path: None,
            panels: Panels::default(),
            views: Vec::new(),
            dialogs: Vec::new(),
            windows: Vec::new(),
            theme: crate::theme::ThemeKind::ProMedium,
            workspace: "Essentials".into(),
            palette_open: false,
            dock_tabs: DockTabs::default(),
            dock: Default::default(),
            brush_section: 0,
            brush_tab: 0,
            brushes_panel: Default::default(),
            selection_mode: 0,
            tool_options: ToolOptions::default(),
            polygon: Vec::new(),
            crop_rect: None,
            next_id: 1,
            status: String::new(),
            status_error: false,
            notices: Vec::new(),
            chrome: Default::default(),
        }
    }
}

impl UiState {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn open_dialog(&mut self, kind: DialogKind, fields: serde_json::Map<String, serde_json::Value>) -> u64 {
        let id = self.alloc_id();
        self.dialogs.push(Dialog { id, kind, fields });
        id
    }

    pub fn dialog_mut(&mut self, id: u64) -> Option<&mut Dialog> {
        self.dialogs.iter_mut().find(|d| d.id == id)
    }

    pub fn close_dialog(&mut self, id: u64) -> Option<Dialog> {
        let i = self.dialogs.iter().position(|d| d.id == id)?;
        Some(self.dialogs.remove(i))
    }

    pub fn new_document_fields() -> serde_json::Map<String, serde_json::Value> {
        let v = serde_json::json!({"name": "Untitled-1", "width": 1920, "height": 1080, "mode": "rgb", "depth": 8, "background": "white"});
        v.as_object().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_parse() {
        assert_eq!(Tool::from_name("brush"), Some(Tool::Brush));
        assert_eq!(Tool::from_name("Rect"), Some(Tool::RectMarquee));
        assert_eq!(Tool::from_name("RectMarquee"), Some(Tool::RectMarquee));
        assert_eq!(Tool::from_name("Eraser Tool"), Some(Tool::Eraser));
        assert_eq!(Tool::from_name("nope"), None);
    }

    #[test]
    fn dialogs_open_and_close() {
        let mut s = UiState::default();
        let a = s.open_dialog(DialogKind::About, Default::default());
        let b = s.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
        assert_ne!(a, b);
        assert_eq!(s.dialog_mut(b).unwrap().fields["width"], 1920);
        assert!(s.close_dialog(a).is_some());
        assert_eq!(s.dialogs.len(), 1);
    }

    #[test]
    fn ui_state_serializes() {
        let s = UiState::default();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["tool"], "Brush");
        let back: UiState = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }
}

//! The Photocraft engine façade: open documents, history, and the command registry.
//!
//! Every user-visible action is a command with a stable id (Photoshop-style, such as
//! `layer.newAdjustmentLayer.invert`) and JSON parameters. Every frontend goes through the same
//! [`Session::execute`] entry point: the egui UI, the CLI, the remote-control channel and the MCP
//! server. This is what makes the UI swappable and the app fully scriptable.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod adjust_cmds;
pub mod adjust_params;
pub mod align_cmds;
pub mod analysis_cmds;
pub mod artboard_cmds;
pub mod automate_cmds;
pub mod brush_cmds;
pub mod brush_preset_cmds;
pub mod build_info;
mod canvas_geom;
pub mod channel_cmds;
pub mod color_cmds;
pub mod commands;
pub mod comps_cmds;
pub mod display_color;
pub mod distort_cmds;
pub mod edit_cmds;
pub mod edit_menu_cmds;
pub mod eraser_cmds;
pub mod extra_cmds;
pub mod file_cmds;
pub mod fill_cmds;
pub mod fill_key_cmds;
pub mod filters;
pub mod filters_ext;
mod frame_cmds;
pub mod fx_view_cmds;
pub mod gallery_cmds;
pub mod gradient_fill_cmds;
pub mod group_view_cmds;
pub mod image_cmds;
pub mod inspect;
pub mod jobs;
pub mod layer_menu_cmds;
pub mod layer_multi_cmds;
pub mod layer_style;
pub mod lens_cmds;
pub mod mask_view_cmds;
mod migrate_cmds;
pub mod mode_cmds;
pub mod multichannel_cmds;
pub mod notes_cmds;
pub mod paint_cmds;
pub mod pattern_cmds;
pub mod photo_cmds;
pub mod pick_cmds;
mod pixels;
pub mod plugin_cmds;
pub mod prefs;
pub mod preset_import_cmds;
pub mod preset_store;
pub mod presets;
pub mod print_cmds;
pub mod proof_sim;
pub mod render_cmds;
pub mod retouch_cmds;
pub mod select_extra_cmds;
pub mod selection_cmds;
pub mod slice_cmds;
pub mod smart_cmds;
pub mod smartselect_cmds;
pub mod snap;
pub mod stamp_cmds;
mod timeline_cmds;
pub mod transform_cmds;
mod trap_cmds;
pub mod type_cmds;
pub mod type_extra_cmds;
pub mod type_spell_cmds;
pub mod type_styles_cmds;
mod variables_cmds;
pub mod vector_cmds;
mod video_cmds;
pub mod vp_cmds;
pub mod warp_cmds;
pub mod web_cmds;
mod wia_cmds;

use std::sync::Arc;

use photocraft_doc::{Document, LayerId};
use photocraft_ops::History;
use serde_json::Value;

pub use commands::{CommandSpec, command_specs};
pub use photocraft_doc as doc;
pub use photocraft_paint as paint;
pub use photocraft_paint::BrushSettings;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("command `{0}` is not available right now: {1}")]
    Disabled(String, String),
    #[error("invalid parameters for `{cmd}`: {msg}")]
    BadParams { cmd: String, msg: String },
    #[error("no active document")]
    NoDocument,
    #[error("no such layer {0:?}")]
    NoLayer(LayerId),
    #[error("{0}")]
    Other(String),
    /// A background job was cancelled (see [`jobs`]); nothing changed.
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// The pixels of a raster layer (typically one a command just created), as an error instead
/// of a panic if the layer has none.
pub(crate) fn pixels_mut(l: &mut photocraft_doc::Layer) -> Result<&mut photocraft_raster::Surface> {
    let id = l.id;
    l.surface_mut().ok_or_else(|| EngineError::Other(format!("layer {id:?} has no pixels")))
}

/// The active document's active layer, for `enabled` predicates.
pub(crate) fn active_layer_of(s: &Session) -> std::result::Result<&photocraft_doc::Layer, String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.and_then(|id| d.doc.layer(id)).ok_or_else(|| "no active layer".into())
}

/// Per-document editing state.
#[derive(Clone, Debug)]
pub struct DocState {
    pub doc: Arc<Document>,
    pub history: History,
    /// The primary ("key") layer: what single-layer commands act on. Always a member of
    /// `selected_layers` when set.
    pub active_layer: Option<LayerId>,
    /// Every selected layer in the Layers panel (⌘/⇧-click), in selection order. Like
    /// `active_layer` this is UI state, not history; see [`DocState::selected_layers`].
    pub selected_layers: Vec<LayerId>,
    /// Anchor of ⇧-click range selection (the last plainly or ⌘-clicked layer).
    pub layer_anchor: Option<LayerId>,
    pub path: Option<String>,
    /// Increments on every change; UIs re-render when it moves.
    pub revision: u64,
    pub saved_revision: u64,
    /// Area changed by the latest revision (None = assume everything changed).
    pub last_damage: Option<photocraft_geom::Rect>,
    /// Coalescing key of the latest history step (see [`Session::execute`]'s `coalesce` param).
    pub coalesce: Option<String>,
    /// Channels panel: targeted channel and eye toggles (view state, not history).
    pub channel_view: channel_cmds::ChannelView,
    /// Select › Isolate Layers: the Layers panel lists only these layers (empty = off; view state).
    pub isolated_layers: Vec<LayerId>,
    /// Layers panel: layers whose effects list is collapsed under their row (the fx triangle;
    /// view state, not history). Effects lists start open.
    pub fx_collapsed: Vec<LayerId>,
}

impl DocState {
    pub fn new(doc: Document, path: Option<String>) -> Self {
        let active_layer = doc.top_layer();
        Self {
            doc: Arc::new(doc),
            history: History::default(),
            active_layer,
            selected_layers: active_layer.into_iter().collect(),
            layer_anchor: active_layer,
            path,
            revision: 1,
            saved_revision: 1,
            last_damage: None,
            coalesce: None,
            channel_view: Default::default(),
            isolated_layers: Vec::new(),
            fx_collapsed: Vec::new(),
        }
    }
    /// The selected layers in bottom-to-top document order, always including the active layer.
    /// Robust against stale state: ids no longer in the document are skipped, and if the active
    /// layer was changed without updating the set, the selection is just the active layer.
    pub fn selected_layers(&self) -> Vec<LayerId> {
        let Some(active) = self.active_layer else { return Vec::new() };
        let set: &[LayerId] = if self.selected_layers.contains(&active) { &self.selected_layers } else { std::slice::from_ref(&active) };
        self.doc.walk().into_iter().map(|(_, _, l)| l.id).filter(|id| set.contains(id)).collect()
    }
    pub fn is_layer_selected(&self, id: LayerId) -> bool {
        self.active_layer == Some(id) || (self.active_layer.is_some_and(|a| self.selected_layers.contains(&a)) && self.selected_layers.contains(&id))
    }
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
}

/// App-wide tool state that commands read (foreground colour, brush…).
#[derive(Clone, Debug)]
pub struct ToolState {
    pub foreground: [f32; 4],
    pub background: [f32; 4],
    pub brush: photocraft_paint::BrushSettings,
    /// Brush presets (built-ins plus user presets; see `brush.presets.*`).
    pub presets: Vec<photocraft_paint::BrushPreset>,
    /// Bumped whenever `presets` changes ([`Session::brush_presets_changed`]); the preset store
    /// syncs when it moves.
    pub presets_rev: u64,
    /// Mixer Brush paint carried between strokes.
    pub mixer: photocraft_paint::mixer::MixerState,
    /// The coalescing key of the running `tools.setBrush` gesture and the brush before it, so the
    /// gesture journals as one call ([`brush_cmds::coalesce_journal`]).
    pub brush_gesture: Option<(String, photocraft_paint::BrushSettings)>,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 1.0],
            // Photoshop's Brush tool starts at 10 % Smoothing.
            brush: photocraft_paint::BrushSettings {
                smoothing: photocraft_paint::brush::Smoothing { amount: 0.1, ..Default::default() },
                ..Default::default()
            },
            presets: photocraft_paint::presets::builtin(),
            presets_rev: 0,
            mixer: Default::default(),
            brush_gesture: None,
        }
    }
}

#[derive(Default)]
pub struct Session {
    docs: Vec<DocState>,
    active: Option<usize>,
    pub tools: ToolState,
    /// Log of executed commands (for action recording and debugging).
    pub journal: Vec<(String, Value)>,
    /// Coalescing key of the command being executed.
    coalesce_request: Option<String>,
    /// Pixels copied with Edit › Copy / Cut (shared by all documents, like Photoshop).
    pub clipboard: Option<edit_cmds::Clip>,
    /// Layer › Layer Style › Copy Layer Style: effects, blend mode and fill opacity.
    pub style_clipboard: Option<(photocraft_doc::Effects, photocraft_color::BlendMode, f32)>,
    /// Colour management: proofing state, monitor profile, display transforms.
    pub color: color_cmds::ColorState,
    /// Open Edit Contents documents and the smart objects they update.
    pub smart_links: Vec<smart_cmds::SmartLink>,
    /// Type › Save Default Type Styles: character and paragraph style new type layers start from.
    pub type_defaults: Option<(photocraft_doc::text::CharStyle, photocraft_doc::text::ParagraphStyle)>,
    /// Quick Mask Options (colour, opacity, colour indicates) used when entering Quick Mask.
    pub quick_mask_options: channel_cmds::QuickMaskOptions,
    /// Colour channel the running command may change (a single colour channel is targeted).
    color_restrict: Option<usize>,
    /// Edit › Preferences, keyboard shortcuts, menu and toolbar customisation (see `prefs`).
    pub prefs: prefs::PrefsStore,
    /// Edit menu state: Fade source, custom shape library (see `edit_menu_cmds`).
    pub edit_state: edit_menu_cmds::EditState,
    /// The pattern library (Window › Patterns; see `pattern.*`).
    pub patterns: pattern_cmds::PatternLibrary,
    /// Image › Analysis: Measurement Log and Select Data Points (see `analysis_cmds`).
    pub analysis: analysis_cmds::AnalysisState,
    /// Window › Gradients, Patterns (groups), Styles, Shapes, Tool Presets and Clone Source.
    pub presets: presets::PresetState,
    /// File menu state: Lock Slices, Image Assets, last Print / Save for Web settings, script
    /// event log (see `automate_cmds`).
    pub file_menu: automate_cmds::FileMenuState,
    /// Persistent brush preset store (desktop only; `None` keeps presets session-only, as in
    /// headless and test sessions). See `preset_store`.
    pub preset_store: Option<preset_store::PresetStore>,
    /// Background jobs (see [`jobs`]).
    jobs: jobs::Jobs,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn documents(&self) -> &[DocState] {
        &self.docs
    }
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }
    pub fn active(&self) -> Option<&DocState> {
        self.active.and_then(|i| self.docs.get(i))
    }
    pub fn active_mut(&mut self) -> Option<&mut DocState> {
        self.active.and_then(|i| self.docs.get_mut(i))
    }
    pub fn set_active(&mut self, index: usize) -> bool {
        if index < self.docs.len() {
            self.active = Some(index);
            true
        } else {
            false
        }
    }

    /// Add a document (from File → New, an import, etc.) and make it active.
    /// Preserve its identity unless another open document already owns it.
    /// Identity-dependent callers must read the admitted document at the returned index.
    pub fn add_document(&mut self, mut doc: Document, path: Option<String>) -> usize {
        // Persisted IDs can overlap the allocator, so check every replacement too.
        while self.docs.iter().any(|st| st.doc.id == doc.id) {
            doc.id = photocraft_doc::DocId::fresh();
        }
        let mut st = DocState::new(doc, path);
        st.history.max_states = self.prefs.get().performance.history_states.max(1) as usize;
        st.history.max_bytes = self.prefs.get().performance.history_budget_bytes();
        self.docs.push(st);
        let i = self.docs.len() - 1;
        self.active = Some(i);
        i
    }

    pub fn close(&mut self, index: usize) -> Option<DocState> {
        if index >= self.docs.len() {
            return None;
        }
        smart_cmds::on_close(self, index);
        if let Some(id) = self.docs.get(index).map(|d| d.doc.id) {
            self.cancel_jobs_on(id);
        }
        let d = self.docs.remove(index);
        self.active = if self.docs.is_empty() { None } else { Some(index.min(self.docs.len() - 1)) };
        Some(d)
    }

    /// Run a command by id with JSON params. Returns a JSON result.
    ///
    /// Any command accepts an optional `"coalesce": "<key>"` param: consecutive edits with the same
    /// key (and no other edit, undo or redo in between) share one history step, like Photoshop's
    /// single "Edit Type Layer" step for a whole typing session or one step per slider drag.
    ///
    /// Runs synchronously, including job-capable commands (see [`jobs`]; [`Session::start`] runs
    /// those in the background).
    pub fn execute(&mut self, id: &str, params: Value) -> Result<Value> {
        match self.dispatch(id, params, false)? {
            jobs::Started::Done(v) => Ok(v),
            // Not reached: inline dispatch never starts a job. Waiting is still correct.
            jobs::Started::Job(j) => self.wait_job(j),
        }
    }

    /// Is the command currently runnable? (drives menu enablement)
    pub fn is_enabled(&self, id: &str) -> bool {
        commands::find(id).is_some_and(|s| (s.enabled)(self).is_ok() && self.job_conflict(id, s.journal).is_none())
    }

    /// Why the command can't run now (None = it can): its own precondition, or a background job
    /// running on the active document.
    pub fn disabled_reason(&self, id: &str) -> Option<String> {
        let Some(s) = commands::find(id) else { return Some(format!("unknown command `{id}`")) };
        (s.enabled)(self).err().or_else(|| self.job_conflict(id, s.journal))
    }

    /// Apply an undoable edit to the active document.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Document, &mut Option<LayerId>) -> Result<R>) -> Result<R> {
        let restrict = self.color_restrict;
        let st = self.active_mut().ok_or(EngineError::NoDocument)?;
        let before = st.doc.clone();
        let mut doc = (*before).clone();
        let mut active = st.active_layer;
        let r = f(&mut doc, &mut active)?;
        if let (Some(k), Some(id)) = (restrict, active) {
            channel_cmds::restrict_to_color(&before, &mut doc, id, k);
        }
        st.doc = Arc::new(doc);
        st.active_layer = active;
        fix_selection(st);
        channel_cmds::fix_view(st);
        let key = self.coalesce_request.clone();
        let st = self.active_mut().ok_or(EngineError::NoDocument)?;
        if key.is_none() || st.coalesce != key || !st.history.can_undo() {
            st.history.record(label, before);
            st.history.trim(&st.doc);
        }
        st.coalesce = key;
        st.revision += 1;
        st.last_damage = None;
        Ok(r)
    }

    /// Replace the active document's active layer without creating a history step.
    pub fn select_layer(&mut self, id: LayerId) -> Result<()> {
        let st = self.active_mut().ok_or(EngineError::NoDocument)?;
        if st.doc.layer(id).is_none() {
            return Err(EngineError::NoLayer(id));
        }
        st.active_layer = Some(id);
        st.selected_layers = vec![id];
        st.layer_anchor = Some(id);
        // Selecting a layer is not an edit: keep a clean document clean.
        let clean = st.saved_revision == st.revision;
        st.revision += 1;
        if clean {
            st.saved_revision = st.revision;
        }
        st.last_damage = Some(photocraft_geom::Rect::EMPTY);
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        // A background job is computing from the current state: it must not move under it.
        if self.active_job().is_some() {
            return false;
        }
        let Some(st) = self.active_mut() else { return false };
        st.coalesce = None;
        match st.history.undo(st.doc.clone()) {
            Some(d) => {
                st.doc = d;
                fix_active(st);
                st.revision += 1;
                st.last_damage = None;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.active_job().is_some() {
            return false;
        }
        let Some(st) = self.active_mut() else { return false };
        st.coalesce = None;
        match st.history.redo(st.doc.clone()) {
            Some(d) => {
                st.doc = d;
                fix_active(st);
                st.revision += 1;
                st.last_damage = None;
                true
            }
            None => false,
        }
    }
}

fn fix_active(st: &mut DocState) {
    if st.active_layer.is_none_or(|id| st.doc.layer(id).is_none()) {
        st.active_layer = st.doc.top_layer();
    }
    fix_selection(st);
    channel_cmds::fix_view(st);
}

/// Keep the layer selection valid after the document or the active layer changed: drop deleted
/// layers, and collapse to the active layer when a command made a non-member active.
pub(crate) fn fix_selection(st: &mut DocState) {
    let Some(active) = st.active_layer else {
        st.selected_layers.clear();
        return;
    };
    if !st.selected_layers.contains(&active) {
        st.selected_layers = vec![active];
    } else {
        let live: std::collections::HashSet<LayerId> = st.doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
        st.selected_layers.retain(|id| live.contains(id));
        let mut seen = std::collections::HashSet::new();
        st.selected_layers.retain(|id| seen.insert(*id));
    }
    if st.layer_anchor.is_some_and(|a| !st.selected_layers.contains(&a)) {
        st.layer_anchor = Some(active);
    }
}

#[cfg(test)]
mod tests;

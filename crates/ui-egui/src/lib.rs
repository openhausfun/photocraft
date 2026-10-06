//! Photocraft's first UI shell, built on egui/eframe.
//!
//! This crate is deliberately thin. Every action goes through
//! [`photocraft_engine::Session::execute`], and all UI state lives in [`state::UiState`] (plain
//! data). The [`control`] module exposes both to automation, so agents can drive and inspect every
//! part of the interface.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Translate a string literal into the current UI language: `tl!("Image Size…")`.
macro_rules! tl {
    ($s:expr) => {
        $crate::i18n::t($s)
    };
}

pub mod actions;
pub mod adjust_dialog;
pub mod adjust_editors;
pub mod adjust_preview;
pub mod adjust_ui;
pub mod analysis_ui;
pub mod artboard_ui;
pub mod brush_panel;
pub mod brush_picker;
pub mod brush_preview;
pub mod brush_resize;
pub mod brush_sections;
pub mod brushes_tab;
pub mod camera_raw_ui;
pub mod canvas;
pub mod channel_view;
pub mod channels_panel;
pub mod chrome_ui;
pub mod cjk_fonts;
pub mod color_picker_ui;
pub mod color_range_ui;
pub mod comps_ui;
pub mod control;
pub mod crop_ui;
pub mod dialogs;
pub mod discard_ui;
pub mod distort_ui;
pub mod doc_props_ui;
pub mod dock;
pub mod enable_rules;
pub mod eraser_ui;
pub mod export_dialog;
pub mod file_open;
pub mod file_ui;
pub mod fill_ui;
pub mod filter_dialog;
pub mod gallery_ui;
pub mod gpu_canvas;
pub mod gpu_status;
pub mod gradient_ui;
pub mod hold_keys;
pub mod i18n;
mod icon_data;
pub mod icons;
pub mod jobs_ui;
pub mod layer_menu_ui;
pub mod layer_pick_ui;
pub mod layer_props_ui;
mod layer_reveal;
pub mod layer_row_ui;
pub mod layer_style;
pub mod layer_tree_ui;
pub mod links;
pub mod liquify_ui;
pub mod mask_thumbs_ui;
pub mod menu_catalog;
pub mod menu_nav;
pub mod menus;
pub mod move_mods;
pub mod move_ui;
pub mod new_doc_ui;
pub mod notices;
pub mod outline;
pub mod paint_mouse;
pub mod palette;
pub mod panels;
pub mod parity;
pub mod perspective_ui;
pub mod plugin_ui;
pub mod prefs_ui;
pub mod preset_files_ui;
pub mod preset_panels;
pub mod props_layout;
pub mod proxy;
pub mod puppet_ui;
pub mod rasterize_prompt;
pub mod retouch_ui;
pub mod rulers;
pub mod scrollbars;
pub mod shortcut_dispatch;
pub mod shortcuts;
mod sizing;
pub mod slice_ui;
pub mod smart_ui;
pub mod snap_ui;
pub mod state;
pub mod stroke_constraint;
pub mod stroke_trail;
pub mod stylus;
mod tab_strip;
pub mod theme;
mod timeline_ui;
pub mod tone;
pub mod tool_feedback;
pub mod transform_tex;
pub mod transform_tool;
pub mod type_panels_ui;
pub mod type_tool;
mod variables_ui;
pub mod vector_ui;
pub mod view_cmds;
pub mod wheel_nav;
pub mod wide_angle_ui;
pub mod widgets;
pub mod work_area;
pub mod workspace_ui;
pub mod zoom_tool;

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use photocraft_doc::{DocId, Document};
use photocraft_engine::Session;
use serde_json::Value;

pub use control::{ControlRequest, ControlResponse};
pub use file_open::OsEvent;
pub use state::{Tool, UiState};

/// Decode a file: (document, warnings about anything approximated or dropped).
pub type ImportFn = Box<dyn Fn(&str, &[u8]) -> Result<(Document, Vec<String>), String>>;
/// Encoder settings chosen in Export As (the file format comes from the name's extension).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportSettings {
    /// JPEG quality 1–100 (None = codec default).
    pub jpeg_quality: Option<u8>,
}

/// Encode a document: (file bytes, warnings about anything approximated or dropped).
pub type ExportFn = Box<dyn Fn(&Document, &str, &ExportSettings) -> Result<(Vec<u8>, Vec<String>), String>>;
pub type PickOpenFn = Box<dyn FnMut() -> Option<(String, Vec<u8>)>>;
pub type PickSaveFn = Box<dyn FnMut(&str) -> Option<String>>;
pub type WriteFn = Box<dyn FnMut(&str, &[u8]) -> Result<(), String>>;
/// Read bytes through the desktop control session's authorized read root.
pub type AutomationReadFn = Box<dyn FnMut(&str) -> Result<(String, Vec<u8>), String>>;
/// Write bytes through the desktop control session's authorized write root.
pub type AutomationWriteFn = Box<dyn FnMut(&str, &[u8]) -> Result<(), String>>;
/// Reject engine commands which still perform ambient filesystem I/O.
pub type AutomationCommandFn = Box<dyn Fn(&str, &Value) -> Result<(), String>>;
/// Open a URL in the system browser (native) — reliable cross-platform, unlike `ctx.open_url`.
pub type OpenUrlFn = Box<dyn Fn(&str) -> Result<(), String>>;
pub type EncodePngFn = Box<dyn Fn(u32, u32, &[u8]) -> Result<Vec<u8>, String>>;
/// Put an RGBA8 image (width, height, pixels) on the OS clipboard.
pub type ClipboardSetFn = Box<dyn FnMut(u32, u32, &[u8]) -> Result<(), String>>;
/// Read an RGBA8 image from the OS clipboard.
pub type ClipboardGetFn = Box<dyn FnMut() -> Option<(u32, u32, Vec<u8>)>>;
/// Shared queue of (file name, bytes) delivered asynchronously.
pub type Inbox = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;
/// Read the saved preferences text (`None` when there is none yet).
pub type LoadTextFn = Box<dyn FnMut() -> Option<String>>;
/// Persist the preferences text.
pub type SaveTextFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Autosave a document snapshot for crash recovery: (snapshot, revision, original path).
pub type AutosaveFn = Box<dyn FnMut(&std::sync::Arc<Document>, u64, Option<&str>) -> Result<(), String>>;
/// Drop the recovery data of a document (by `DocId` value) once it is saved or closed.
pub type DiscardAutosaveFn = Box<dyn FnMut(u64)>;
/// Load recoverable documents left by a previous session: (original path, document).
pub type RecoverFn = Box<dyn FnMut() -> Vec<(Option<String>, Document)>>;
/// Append text to a file (History Log).
pub type AppendTextFn = Box<dyn FnMut(&str, &str) -> Result<(), String>>;
/// Requests from the operating system since the last call (see [`OsEvent`]).
pub type OsEventsFn = Box<dyn FnMut() -> Vec<OsEvent>>;

/// Platform services injected by the app binary (file dialogs, codecs), keeping this crate free of
/// I/O dependencies.
#[derive(Default)]
pub struct Services {
    /// Decode a file's bytes into a document (PSD, PNG, JPEG, …).
    pub import: Option<ImportFn>,
    /// Encode a document for a file name (format chosen by extension).
    pub export: Option<ExportFn>,
    /// Show an "open file" dialog; returns (name, bytes).
    pub pick_open: Option<PickOpenFn>,
    /// Show a "save file" dialog; returns a path/name to write.
    pub pick_save: Option<PickSaveFn>,
    /// Write bytes to a path (native) or trigger a download (web).
    pub write: Option<WriteFn>,
    /// File access used only by control/MCP requests. Interactive dialogs keep
    /// using `pick_open`, `pick_save` and `write` with the user's authority.
    pub automation_read: Option<AutomationReadFn>,
    pub automation_write: Option<AutomationWriteFn>,
    pub automation_command: Option<AutomationCommandFn>,
    /// Encode an RGBA8 image as PNG (used for screenshots and `ui.render`).
    pub encode_png: Option<EncodePngFn>,
    /// Open a URL in the system browser (native). Falls back to `ctx.open_url` (web) when unset.
    pub open_url: Option<OpenUrlFn>,
    /// Files delivered asynchronously (web file pickers, drag-and-drop): drained every frame.
    pub inbox: Option<Inbox>,
    /// OS clipboard images: copies go out, screenshots and images from other apps come in.
    pub clipboard_set_image: Option<ClipboardSetFn>,
    pub clipboard_get_image: Option<ClipboardGetFn>,
    /// Preferences persistence: the platform config directory on native, browser storage on
    /// the web (see `prefs_ui`).
    pub load_prefs: Option<LoadTextFn>,
    pub save_prefs: Option<SaveTextFn>,
    /// Crash-recovery autosave (Preferences › File Handling) and recovery at launch.
    pub autosave: Option<AutosaveFn>,
    pub discard_autosave: Option<DiscardAutosaveFn>,
    pub recover: Option<RecoverFn>,
    /// History Log text file output.
    pub append_text: Option<AppendTextFn>,
    /// OS requests (macOS open-documents / quit Apple events), polled every frame.
    pub os_events: Option<OsEventsFn>,
    /// The persistent brush preset store, loading in the background (desktop; see
    /// `photocraft_engine::preset_store`). Attached to the session once it arrives; without
    /// one, brush presets are session-only (web, tests).
    pub preset_store: Option<std::sync::mpsc::Receiver<photocraft_engine::preset_store::Opened>>,
}

pub struct PhotocraftApp {
    pub session: Session,
    pub ui: UiState,
    pub services: Services,
    canvases: HashMap<DocId, canvas::CanvasCache>,
    checker: Option<egui::TextureHandle>,
    drag: Option<canvas::Drag>,
    /// Brush/Eraser stroke being drawn, rendered by the engine (see `canvas::LiveStroke`).
    live_stroke: Option<canvas::LiveStroke>,
    /// Footprint trail of a retouching drag (see `stroke_trail`).
    trail: Option<stroke_trail::Trail>,
    /// Move tool drag shown live (`move_ui`).
    pub(crate) move_preview: Option<move_ui::MovePreview>,
    /// The next tool `Down` is a right-button drag that erases (see `paint_mouse`).
    secondary_erase: bool,
    /// While a batch of recovered pointer samples is replayed, defer the live-stroke update to one
    /// call for the whole frame (see `canvas::canvas_view`).
    defer_live_stroke: bool,
    /// End of the last painting stroke: ⇧-click draws a straight line from it (#178).
    last_stroke_end: Option<(DocId, [f64; 2])>,
    /// Control+Alt-drag brush resize in progress (`brush_resize`, #231).
    pub(crate) brush_resize: Option<brush_resize::Resize>,
    /// The next tool `Down` is an Alt+right-drag that resizes the brush (#297). `tool_event`
    /// takes it on every event, so a press another handler consumes can't leave it set.
    pub(crate) brush_resize_armed: bool,
    control_rx: Option<Receiver<ControlRequest>>,
    pending_screenshots: Vec<(u64, Option<String>, Sender<ControlResponse>)>,
    /// Screenshots not yet requested from the viewport: (token, earliest time in ms, frames seen).
    queued_screenshots: Vec<(u64, f64, u32)>,
    /// Control replies waiting for queued synthetic input to be processed.
    input_waiters: Vec<Sender<ControlResponse>>,
    /// Live (uncommitted) adjustment edit shown on canvas while a slider is dragged.
    pub live_adjust: Option<(photocraft_doc::LayerId, Value)>,
    /// Frames rendered (for tests and the status bar).
    pub frame: u64,
    /// Apply theme on first frame.
    styled: bool,
    /// Whether the window uses an integrated (transparent) macOS title bar.
    pub integrated_titlebar: bool,
    fonts_ready: bool,
    /// Screen rect of the main canvas last frame (for overlays and the navigator).
    pub last_canvas_rect: egui::Rect,
    pub fps: f32,
    last_frame_time: f64,
    thumbs: HashMap<(photocraft_doc::LayerId, u8), (u64, egui::TextureHandle)>,
    /// Snapshots whose live layer/mask keys were last used to prune thumbnail handles.
    thumb_documents: Vec<(DocId, std::sync::Weak<Document>)>,
    /// Content bounds cached per (key, revision): scanning a 36 MP layer every frame cost ~77 ms.
    bounds_cache: HashMap<u64, (u64, photocraft_geom::Rect)>,
    /// Downsampled proxy of the active document for live previews: (doc, revision, k, proxy).
    pub(crate) proxy: Option<(DocId, u64, u32, std::sync::Arc<Document>)>,
    /// Key of the preview currently uploaded to the GPU (doc, params hash).
    pub(crate) proxy_uploaded: Option<(DocId, u64)>,
    /// Selection outline cache: (doc, revision, segments).
    /// Live filter preview (proxy document with the filter applied).
    pub(crate) filter_preview: Option<filter_dialog::FilterPreview>,
    /// Select › Color Range dialog preview (proxy document + mask / image textures).
    pub(crate) color_range: Option<color_range_ui::Preview>,
    /// Image › Adjustments dialog preview through a temporary clipped adjustment layer.
    pub(crate) adjust_preview: Option<adjust_preview::AdjustPreview>,
    /// Synthetic input events queued by automation (`ui.click`, `ui.key`, …), injected next frame.
    pub(crate) synthetic: Vec<egui::Event>,
    /// True only while a frame is processing synthetic automation input. It
    /// makes shortcut- and dialog-triggered commands pass the same policy as
    /// direct control calls.
    pub(crate) automation_input: bool,
    /// Levels/Curves histogram cache: (document, adjustment layer, revision it is valid for).
    pub(crate) tone_hist: Option<(DocId, photocraft_doc::LayerId, u64, std::sync::Arc<tone::Histograms>)>,
    /// Histogram panel cache: (document, revision, computed at ms, histograms).
    pub(crate) doc_hist: Option<(DocId, u64, f64, std::sync::Arc<tone::Histograms>)>,
    /// Free Transform preview (document without the moving pixels + their texture).
    pub(crate) transform_preview: Option<transform_tool::TransformPreview>,
    /// Move-tool ⇧/⌥ drag state (move_mods).
    pub(crate) move_mods: move_mods::MoveDrag,
    /// Live Layer Style dialog preview: (key over revision + style fields, document with the style applied).
    pub(crate) style_preview: Option<(u64, Option<std::sync::Arc<Document>>)>,
    /// Liquify dialog, Puppet Warp and Perspective Warp sessions (distort_ui).
    pub(crate) distort: distort_ui::Distort,
    /// Gradient tool live-mode drags and previews (gradient_ui).
    pub(crate) gradient: gradient_ui::LiveGradient,
    /// Filter › Camera Raw Filter dialog (camera_raw_ui).
    pub(crate) camera_raw: Option<camera_raw_ui::CameraRawDialog>,
    /// Filter › Adaptive Wide Angle dialog (wide_angle_ui).
    pub(crate) wide_angle: Option<wide_angle_ui::WideAngleDialog>,
    /// Signature of the image we last put on the OS clipboard (to tell ours from other apps').
    os_clip_sig: Option<u64>,
    /// The session clipboard currently holds an image imported from the OS clipboard: it has no
    /// original position, so Paste centres it (see `menus` "edit.paste").
    pub(crate) clip_external: bool,
    /// The OS clipboard was already read for the paste in flight (Edit › Paste positions it first),
    /// so `run` doesn't read it a second time.
    pub(crate) clip_read_for_paste: bool,
    /// Pointer position over the canvas (document px), for the Info panel and status bar.
    pub(crate) hover_doc: Option<[f64; 2]>,
    /// Info panel sample cache: ((x, y, revision), composite RGBA).
    info_sample: Option<((i32, i32, u64), [f32; 4])>,
    /// Guide being dragged (from a ruler or with the Move tool).
    pub(crate) guide_drag: Option<rulers::GuideDrag>,
    /// Crop tool gesture in progress (see `crop_ui`).
    pub(crate) crop: crop_ui::CropState,
    /// Type tool layout cache: ((doc, revision, layer), layout).
    pub(crate) type_layout: Option<((u64, u64, u64), std::sync::Arc<photocraft_text::TextLayout>)>,
    /// Channel thumbnails for one document snapshot; view-only revisions reuse their pixels.
    channel_thumbs: Option<(DocId, std::sync::Weak<Document>, Vec<egui::TextureHandle>)>,
    /// Channels panel overlays / channel views drawn over the canvas, per document id.
    pub(crate) channel_views: HashMap<u64, channel_view::Cache>,
    /// Selection outline keyed by (document, mask identity × step × visible region).
    pub(crate) outline_cache: Option<(DocId, u64, std::sync::Arc<Vec<outline::Segment>>)>,
    /// GPU canvas renderer, when running on the wgpu backend (see [`Self::set_wgpu`]). Dropped
    /// for the rest of the session when the device is lost (see `gpu_status`).
    gpu: Option<gpu_canvas::GpuCanvas>,
    /// Run once the first frames have rendered (see [`Self::on_started`]).
    started: Option<gpu_status::StartedHook>,
    /// Frame and canvas-upload timings (exposed via `ui.inspect`).
    pub perf: gpu_canvas::Perf,
    /// Preferences, autosave and snapping runtime state (see `prefs_ui`, `snap_ui`).
    pub(crate) prefs_rt: prefs_ui::Runtime,
    /// Close, Revert or Exit parked behind the unsaved-changes prompt (see `discard_ui`).
    pub(crate) discard: Option<discard_ui::Prompt>,
    /// Set once the user has agreed to quit, so the resulting close request goes through.
    pub(crate) allow_close: bool,
    /// Pen pressure/tilt from the platform (see `stylus`).
    pub stylus: stylus::Stylus,
    /// Run long commands and file opens as background jobs with progress and Cancel (#210; see
    /// `jobs_ui`). The desktop app turns it on; off (the default), everything runs inline as
    /// before, which tests and scripts rely on.
    pub background_jobs: bool,
    /// Background job bookkeeping: opening tabs, control replies waiting on a job.
    pub jobs: jobs_ui::JobsUi,
    #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
    live_tokens: theme::live::LiveTokens,
}

impl PhotocraftApp {
    pub fn new(session: Session, services: Services) -> Self {
        let mut app = Self {
            session,
            ui: UiState::default(),
            services,
            canvases: HashMap::new(),
            checker: None,
            drag: None,
            live_stroke: None,
            trail: None,
            move_preview: None,
            secondary_erase: false,
            defer_live_stroke: false,
            last_stroke_end: None,
            brush_resize: None,
            brush_resize_armed: false,
            control_rx: None,
            pending_screenshots: Vec::new(),
            queued_screenshots: Vec::new(),
            input_waiters: Vec::new(),
            live_adjust: None,
            frame: 0,
            styled: false,
            integrated_titlebar: false,
            fonts_ready: false,
            last_canvas_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            fps: 0.0,
            last_frame_time: 0.0,
            thumbs: HashMap::new(),
            thumb_documents: Vec::new(),
            bounds_cache: HashMap::new(),
            proxy: None,
            proxy_uploaded: None,
            outline_cache: None,
            filter_preview: None,
            color_range: None,
            adjust_preview: None,
            synthetic: Vec::new(),
            automation_input: false,
            channel_thumbs: None,
            channel_views: HashMap::new(),
            type_layout: None,
            guide_drag: None,
            crop: Default::default(),
            hover_doc: None,
            info_sample: None,
            os_clip_sig: None,
            clip_external: false,
            clip_read_for_paste: false,
            transform_preview: None,
            move_mods: Default::default(),
            style_preview: None,
            distort: Default::default(),
            gradient: Default::default(),
            camera_raw: None,
            wide_angle: None,
            tone_hist: None,
            doc_hist: None,
            gpu: None,
            started: None,
            perf: Default::default(),
            prefs_rt: Default::default(),
            discard: None,
            allow_close: false,
            stylus: Default::default(),
            background_jobs: false,
            jobs: Default::default(),
            #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
            live_tokens: theme::live::LiveTokens::from_env(),
        };
        // Saved preferences (and recovered documents) are in place before the first frame.
        prefs_ui::load(&mut app);
        // File › Scripts › Script Events Manager: "Start Application".
        photocraft_engine::automate_cmds::fire_event(&mut app.session, "startApplication");
        app
    }

    /// Draw the document canvas on the GPU (custom WGSL shader) instead of via egui textures.
    /// Call from the app creator with `cc.wgpu_render_state`; without it the CPU path is used.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        // Preferences › Performance › cache tile size (PHOTOCRAFT_GPU_TILE still overrides).
        let tile = self.session.prefs().performance.cache_tile_size;
        let gpu = gpu_canvas::GpuCanvas::with_tile(&rs, Some(tile));
        self.perf.gpu_info.set_adapter(&gpu.adapter_info());
        self.perf.gpu_info.canvas = "gpu".into();
        self.gpu = Some(gpu);
        self.prefs_rt.gpu_style = None;
    }

    /// Whether the document canvas currently draws on the GPU (false on the CPU path, and after
    /// the device was lost).
    pub fn gpu_active(&self) -> bool {
        self.gpu.is_some()
    }

    /// The GPU canvas's device health flag (tests inject faults through it).
    pub fn gpu_health(&self) -> Option<photocraft_gpu::DeviceHealth> {
        self.gpu.as_ref().map(|g| g.health().clone())
    }

    /// Run `hook` once the app has rendered its first frames (and a document opened at launch
    /// has drawn): the desktop app clears its crash-safe GPU startup marker there.
    pub fn on_started(&mut self, hook: impl FnOnce(&mut PhotocraftApp) + 'static) {
        self.started = Some(Box::new(hook));
    }

    /// Per-frame GPU health check (called from `logic`; tests call it directly): falls back to
    /// the CPU canvas after a device loss and runs the started hook.
    pub fn check_gpu(&mut self, ctx: &egui::Context) {
        gpu_status::check(self, ctx);
    }

    /// Attach a control channel (requests arrive from a transport thread: TCP, stdin, tests).
    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Run an engine command, reporting errors in the status bar.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let clip_read = std::mem::take(&mut self.clip_read_for_paste);
        if self.automation_input
            && let Some(authorize) = self.services.automation_command.as_ref()
        {
            authorize(id, &params)?;
        }
        let suppress_events = self.automation_input && self.session.prefs().script_events.enabled;
        if suppress_events {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
        }
        let t0 = gpu_canvas::now_ms();
        // The OS clipboard is read only on an explicit paste, never in the background (privacy, CPU).
        if matches!(id, "edit.paste" | "edit.pasteSpecial.pasteInPlace") {
            if !clip_read {
                self.import_os_clipboard();
            }
            if self.session.clipboard.is_none() && self.session.active().is_some() && self.services.clipboard_get_image.is_some() {
                // Enabled on the strength of the OS clipboard, which held no image: a quiet no-op.
                self.ui.status = "Nothing to paste: the clipboard holds no image".into();
                self.ui.status_error = false;
                return Ok(serde_json::json!({"pasted": false}));
            }
        }
        // Long commands become background jobs when enabled (`jobs_ui`); the rest run inline.
        let r = jobs_ui::run(self, id, params);
        if r.is_ok() && matches!(id, "edit.copy" | "edit.cut" | "edit.copyMerged") {
            self.clip_external = false;
            self.export_os_clipboard();
        }
        self.perf.last_command = id.to_string();
        self.perf.command_ms = gpu_canvas::now_ms() - t0;
        match &r {
            Ok(_) => {
                self.sync_views();
                if self.ui.status_error {
                    self.ui.status.clear();
                    self.ui.status_error = false;
                }
            }
            Err(e) => {
                self.ui.status = e.clone();
                self.ui.status_error = true;
            }
        }
        if suppress_events {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
        }
        r
    }

    /// Keep one view per document.
    pub fn sync_views(&mut self) {
        let n = self.session.documents().len();
        self.ui.views.resize_with(n, Default::default);
        self.ui.windows.retain(|w| w.document < n);
        self.prune_thumbs();
        self.sync_mask_targets();
        // Channel-view textures outlive a hidden view (cheap re-show), not their document.
        if !self.channel_views.is_empty() {
            let docs = self.session.documents();
            self.channel_views.retain(|id, _| docs.iter().any(|d| d.doc.id.0 == *id));
        }
        // Commands may close and reopen a preserved-ID document before the next repaint.
        canvas::retain_gpu_documents(self);
    }

    /// Record `path` as the most-recently-opened file (File › Open Recent): de-duplicated, newest
    /// first, capped at Preferences › File Handling › Recent File List Contains. The list lives in
    /// the preferences (`fileHandling.recentFiles`), so it survives a restart; `ui.recent_files`
    /// mirrors it for the menus and the Home screen.
    pub fn push_recent(&mut self, path: &str) {
        let cap = self.recent_cap();
        let mut r = self.session.prefs().file_handling.recent_files.clone();
        r.retain(|p| p != path);
        r.insert(0, path.to_string());
        r.truncate(cap);
        self.ui.recent_files = r.clone();
        self.session.prefs.edit(|p| p.file_handling.recent_files = r);
    }

    /// File › Open Recent › Clear Recent File List.
    pub fn clear_recent(&mut self) {
        self.ui.recent_files.clear();
        self.session.prefs.edit(|p| p.file_handling.recent_files.clear());
    }

    /// How many recent files to remember ("Recent File List Contains", 0–100).
    pub fn recent_cap(&self) -> usize {
        self.session.prefs().file_handling.recent_file_count.min(100) as usize
    }

    /// Mirror the stored recent-file list into the UI state (after loading the preferences, or
    /// when the Preferences dialog or an agent changes the list or its length). Cheap enough to
    /// run every frame: a slice comparison of at most 100 paths, no allocation unless it changed.
    pub fn sync_recent(&mut self) {
        let cap = self.recent_cap();
        let stored = &self.session.prefs().file_handling.recent_files;
        let want = stored.get(..cap.min(stored.len())).unwrap_or_default();
        if self.ui.recent_files.as_slice() != want {
            self.ui.recent_files = want.to_vec();
        }
    }

    /// Open a file's bytes as a new document named `name`; returns the import warnings (also
    /// shown to the user). Files from disk go through [`open_file`](Self::open_file), which also
    /// remembers the path. Brush and gradient files go to the preset libraries instead.
    pub fn open_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
        if let Some(r) = preset_files_ui::open(self, name, bytes) {
            return r.map(|()| Vec::new());
        }
        // Decoded on a worker: a tab with progress appears now, the document when it's ready
        // (warnings are shown then).
        if self.background_jobs {
            jobs_ui::start_open(self, name, None, jobs_ui::bytes(bytes))?;
            return Ok(Vec::new());
        }
        let import = self.services.import.as_ref().ok_or("no importer configured")?;
        let (doc, warnings) = import(name, bytes)?;
        // Edit › Color Settings policies apply on open; mismatches can ask what to do.
        // No path yet: a bare name isn't a location to save back to (`open_file` sets the path).
        let (_, color) = self.session.open_document(doc, None);
        self.sync_views();
        self.ui.status = format!("Opened {name}");
        self.ui.status_error = false;
        notices::io_warnings(self, &format!("Opened {name}"), &warnings);
        // Script events bound to "Open Document".
        photocraft_engine::automate_cmds::document_opened(&mut self.session);
        self.sync_views();
        let ask = color.get("ask").and_then(Value::as_bool) == Some(true);
        if ask && (color.get("mismatch").and_then(Value::as_bool) == Some(true) || color.get("missing").is_some()) {
            prefs_ui::open_mismatch(self, &color);
        }
        Ok(warnings)
    }

    /// Open bytes supplied by an authenticated automation client without
    /// invoking user-configured script-event paths outside the granted root.
    /// Returns the import warnings (also shown to the user). Brush and gradient
    /// files go to the preset libraries, as for interactive opens.
    pub fn open_automation_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
        let events_enabled = self.session.prefs().script_events.enabled;
        if events_enabled {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
        }
        let result = match preset_files_ui::open(self, name, bytes) {
            Some(r) => r.map(|()| Vec::new()),
            None => self.import_automation_document(name, bytes),
        };
        if events_enabled {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
        }
        result
    }

    /// The document half of [`open_automation_bytes`](Self::open_automation_bytes): no script
    /// events and no Color Settings policy (which may read user-configured profile paths).
    fn import_automation_document(&mut self, name: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
        let import = self.services.import.as_ref().ok_or("no importer configured")?;
        let (doc, warnings) = import(name, bytes)?;
        self.session.add_document(doc, Some(name.to_string()));
        self.sync_views();
        self.ui.status = format!("Opened {name}");
        self.ui.status_error = false;
        notices::io_warnings(self, &format!("Opened {name}"), &warnings);
        Ok(warnings)
    }

    /// Run one engine command on behalf of automation while suppressing
    /// user-configured script-event file reads. Interactive commands retain
    /// their normal event behavior.
    pub fn run_automation(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let events_enabled = self.session.prefs().script_events.enabled;
        if events_enabled {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
        }
        let result = self.run(id, params);
        if events_enabled {
            self.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
        }
        result
    }

    /// File › Open: the platform dialog returns the chosen file's path (native; the web delivers
    /// picks through the inbox instead).
    pub fn open_dialog_file(&mut self) {
        let picked = self.services.pick_open.as_mut().and_then(|f| f());
        if let Some((path, bytes)) = picked
            && let Err(e) = self.open_file(&path, &bytes)
        {
            self.open_failed(&file_open::display_name(&path), &e);
        }
    }

    /// Save the active document to `path` (or a path chosen in the save dialog); returns the path
    /// and the export warnings (also shown to the user).
    pub fn save_as(&mut self, path: Option<String>) -> Result<(String, Vec<String>), String> {
        // Edit Contents documents save back into their smart object.
        if path.is_none() && self.session.is_enabled("layer.smartObjects.saveContents") {
            self.run("layer.smartObjects.saveContents", serde_json::json!({}))?;
            return Ok(("smart object".into(), Vec::new()));
        }
        let st = self.session.active().ok_or("no document")?;
        // Documents are named after their file ("cat.png"): suggest "cat.psd", not "cat.png.psd".
        let suggested = st.path.clone().unwrap_or_else(|| format!("{}.psd", st.doc.name.rsplit_once('.').map_or(st.doc.name.as_str(), |(stem, _)| stem)));
        let path = match path {
            Some(p) => p,
            None => self.services.pick_save.as_mut().and_then(|f| f(&suggested)).ok_or("cancelled")?,
        };
        let export = self.services.export.as_ref().ok_or("no exporter configured")?;
        let (bytes, warnings) = export(&st.doc, &path, &ExportSettings::default())?;
        let write = self.services.write.as_mut().ok_or("no writer configured")?;
        write(&path, &bytes)?;
        if let Some(st) = self.session.active_mut() {
            st.path = Some(path.clone());
            st.saved_revision = st.revision;
        }
        self.ui.status = format!("Saved {path}");
        // "Save Document" script events and File › Generate › Image Assets.
        if let Some(i) = self.session.active_index()
            && let Some(r) = photocraft_engine::automate_cmds::document_saved(&mut self.session, i)
        {
            self.ui.status = format!("Saved {path}; {} image assets in {}", r["files"].as_array().map_or(0, Vec::len), r["dir"].as_str().unwrap_or(""));
        }
        self.ui.status_error = false;
        notices::io_warnings(self, &format!("Saved {}", file_open::display_name(&path)), &warnings);
        self.sync_views();
        Ok((path, warnings))
    }

    /// Save through the control session's capability-scoped writer. No file
    /// picker or ambient writer is reachable from this path. Returns the path
    /// written and the export warnings (also shown to the user).
    pub fn save_automation(&mut self, path: Option<String>) -> Result<(String, Vec<String>), String> {
        let state = self.session.active().ok_or("no document")?;
        let target = path.or_else(|| state.path.clone()).ok_or("document has no relative path; pass `path`")?;
        let export = self.services.export.as_ref().ok_or("no exporter configured")?;
        let (bytes, warnings) = export(&state.doc, &target, &ExportSettings::default())?;
        let write = self.services.automation_write.as_mut().ok_or("automation write authority is not configured")?;
        write(&target, &bytes)?;
        if let Some(state) = self.session.active_mut() {
            state.path = Some(target.clone());
            state.saved_revision = state.revision;
        }
        self.ui.status = format!("Saved {target}");
        self.ui.status_error = false;
        notices::io_warnings(self, &format!("Saved {}", file_open::display_name(&target)), &warnings);
        self.sync_views();
        Ok((target, warnings))
    }

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::AfterJob(job) => self.jobs.waiters.push((job, reply)),
                control::Outcome::Screenshot { token, path } => {
                    // Wait out egui's fade animations (~83 ms) and a few rendered frames first.
                    let settle = ctx.global_style().animation_time as f64 * 2000.0 + 60.0;
                    self.queued_screenshots.push((token, gpu_canvas::now_ms() + settle, 0));
                    self.pending_screenshots.push((token, path, reply));
                }
            }
        }
        self.control_rx = Some(rx);
    }

    fn issue_screenshots(&mut self, ctx: &egui::Context) {
        let now = gpu_canvas::now_ms();
        self.queued_screenshots.retain_mut(|(token, at, frames)| {
            *frames += 1;
            if now >= *at && *frames >= 3 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(*token)));
                false
            } else {
                true
            }
        });
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|(t, _, _)| *t == token) {
                let (_, path, reply) = self.pending_screenshots.remove(i);
                let r = control::save_screenshot(self, &image, path.as_deref());
                let _ = reply.send(r);
            }
        }
    }
}

impl eframe::App for PhotocraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        i18n::set_current(i18n::Lang::from_pref(&self.session.prefs().interface.language));
        if !self.styled {
            Self::setup_context(ctx, self.ui.theme);
            self.styled = true;
        } else {
            self.fonts_ready = true;
        }
        self.frame += 1;
        let now = ctx.input(|i| i.time);
        let dt = (now - self.last_frame_time) as f32;
        if dt > 0.0 {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
        }
        self.last_frame_time = now;
        self.sync_views();
        self.check_gpu(ctx);
        #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
        if self.live_tokens.poll(ctx, self.ui.theme) {
            self.checker = None;
        }
        self.drain_control(ctx);
        if self.ui.text_edit.is_some() && self.ui.tool != state::Tool::Type {
            type_tool::commit(self);
        }
        if self.ui.pen.is_some() && self.ui.tool != state::Tool::Pen {
            vector_ui::pen_commit(self, false);
        }
        // A transform whose layer or document went away (undo, close) ends silently.
        if let Some(t) = &self.ui.transform
            && self.session.active().and_then(|s| s.doc.layer(photocraft_doc::LayerId(t.layer))).is_none()
        {
            transform_tool::cancel(self);
        }
        self.collect_screenshots(ctx);
        self.issue_screenshots(ctx);
        prefs_ui::tick(self, ctx);
        // A window bigger than its display (1440 × 900 on 1366 × 768) runs under the taskbar:
        // maximize it into the work area once (#315).
        work_area::fit_window(ctx);
        discard_ui::guard_window_close(self, ctx);
        // Background jobs: apply finished ones, keep frames coming, Esc cancels (before the
        // shortcuts see Esc).
        jobs_ui::tick(self, ctx);
        shortcuts::handle(self, ctx);
        let arrived: Vec<(String, Vec<u8>)> =
            self.services.inbox.as_ref().map(|q| std::mem::take(&mut *q.lock().unwrap_or_else(|e| e.into_inner()))).unwrap_or_default();
        for (name, bytes) in arrived {
            if let Err(e) = self.open_bytes(&name, &bytes) {
                self.open_failed(&name, &e);
            }
        }
        // Finder double-click / Open With / Dock drops (macOS open-documents events).
        self.drain_os_events(ctx);
        // Files dropped onto the window open as documents (with their path, like File › Open).
        self.open_dropped(ctx.input(|i| i.raw.dropped_files.clone()));
        // The control transport wakes the UI on arrival (ctx.request_repaint); only poll while a
        // screenshot is pending. (Polling every 50 ms here made idle apps render at 20 fps.)
        if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        shortcuts::clipboard_keys(ctx, ctx.text_edit_focused() || self.ui.text_edit.is_some(), raw_input);
        raw_input.events.extend(self.take_synthetic_step());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Fonts registered via set_fonts only take effect next frame; named families would panic now.
        if !self.fonts_ready {
            ctx.request_repaint();
            self.automation_input = false;
            return;
        }
        let t0 = gpu_canvas::now_ms();
        // View › Screen Mode › Full Screen Mode: only the image, on black (F or Esc returns).
        let chrome = !self.ui.view.hides_chrome();
        if !chrome && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            let _ = menus::invoke(self, &ctx, "view.screenMode.standard", serde_json::json!({}));
        }
        if chrome {
            panels::title_bar(self, ui);
        }
        if chrome && self.ui.panels.options_bar {
            panels::options_bar(self, ui);
        }
        if chrome && self.ui.panels.status_bar {
            panels::status_bar(self, ui);
        }
        if chrome && self.ui.panels.toolbar {
            panels::toolbar(self, ui);
        }
        if chrome {
            panels::right_dock(self, ui);
        }
        let t = theme::Tokens::get(&ctx);
        let backdrop = if chrome { prefs_ui::pasteboard_color(self).unwrap_or(t.canvas) } else { egui::Color32::BLACK };
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(backdrop)).show(ui, |ui| {
            canvas::document_area(self, ui);
        });
        panels::properties_window(self, &ctx);
        brush_panel::window(self, &ctx);
        preset_panels::windows(self, &ctx);
        type_panels_ui::windows(self, &ctx);
        analysis_ui::windows(self, &ctx);
        timeline_ui::windows(self, &ctx);
        workspace_ui::windows(self, &ctx);
        palette::show(self, &ctx);
        dialogs::show(self, &ctx);
        jobs_ui::dialog(self, &ctx);
        discard_ui::show(self, &ctx);
        distort_ui::show(self, &ctx);
        camera_raw_ui::show(self, &ctx);
        wide_angle_ui::show(self, &ctx);
        canvas::extra_windows(self, &ctx);
        notices::show(self, &ctx);
        // A device lost while drawing this frame: switch to the CPU canvas before the next one.
        gpu_status::check(self, &ctx);
        self.automation_input = false;
        self.perf.frame(gpu_canvas::now_ms() - t0);
        // Synthetic input is injected one press/release step per frame: keep frames coming until
        // the queue is empty, then release control replies waiting on it.
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(serde_json::json!({"ok": true, "result": null}));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_dropped(f: &(dyn egui::DroppedFile + Send + Sync)) -> Result<Vec<u8>, String> {
    f.bytes()
}

/// On the web, dropped-file bytes arrive asynchronously; the web shell handles drops itself.
#[cfg(target_arch = "wasm32")]
fn read_dropped(_f: &dyn egui::DroppedFile) -> Result<Vec<u8>, String> {
    Err("drag-and-drop on the web is handled by the page; use File → Open".into())
}

impl PhotocraftApp {
    /// Viewing a layer mask (#196) targets it; a vector-mask target needs a vector mask on the
    /// active layer (a shape layer's path is its content, not a mask).
    fn sync_mask_targets(&mut self) {
        let Some(st) = self.session.active() else { return };
        if photocraft_engine::mask_view_cmds::current(st).is_some() {
            self.ui.mask_target = true;
            self.ui.vector_mask_target = false;
        }
        if self.ui.vector_mask_target && !mask_thumbs_ui::has_vector_mask(st) {
            self.ui.vector_mask_target = false;
        }
    }

    fn prune_thumbs(&mut self) {
        let documents = self.session.documents();
        if self.thumb_documents.len() == documents.len()
            && self.thumb_documents.iter().zip(documents).all(|((id, snapshot), st)| *id == st.doc.id && snapshot.ptr_eq(&std::sync::Arc::downgrade(&st.doc)))
        {
            return;
        }
        // Check snapshots every sync, but walk the layer tree only after it changes. A Weak
        // tracks replacement (including undo and preserved-ID reopen) without retaining pixels.
        if !self.thumbs.is_empty() {
            let mut live = std::collections::HashSet::new();
            let mut pending: Vec<_> = documents.iter().flat_map(|st| &st.doc.layers).collect();
            while let Some(layer) = pending.pop() {
                live.insert((layer.id, mask_thumbs_ui::THUMB_LAYER));
                if layer.mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_MASK));
                }
                if layer.vector_mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_VECTOR));
                }
                if let Some(children) = layer.children() {
                    pending.extend(children);
                }
            }
            // Native files can restore IDs: retain the union across all open documents, rather
            // than deleting an ID just because one document containing it was closed.
            self.thumbs.retain(|key, _| live.contains(key));
        }
        self.thumb_documents = documents.iter().map(|st| (st.doc.id, std::sync::Arc::downgrade(&st.doc))).collect();
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, kind: theme::ThemeKind) {
        self.ui.theme = kind;
        theme::apply(ctx, kind);
        self.checker = None;
    }

    /// Cached 64px thumbnail of a pixel-ish layer, laid out in document space.
    pub fn layer_thumb(&mut self, ctx: &egui::Context, doc: &Document, layer: &photocraft_doc::Layer) -> egui::TextureId {
        // Key by content, not document revision: COW tiles change pointer only when their pixels
        // change, so unrelated edits (e.g. painting another layer) don't rebuild this thumbnail.
        let rev = layer.surface().map_or(0, surface_fingerprint) ^ (doc.size.width as u64) << 40;
        let key = (layer.id, mask_thumbs_ui::THUMB_LAYER);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
        {
            return tex.id();
        }
        let mut px = [0.0f32; 8];
        let img = thumb_image(doc, 64, |x, y| {
            let Some(s) = layer.surface() else { return [0.0; 4] };
            let n = s.channels();
            s.read_pixel(x, y, &mut px[..n]);
            photocraft_raster::to_rgba(&s.format(), &px[..n])
        });
        self.store_thumb(ctx, key, rev, img)
    }

    pub fn mask_thumb(&mut self, ctx: &egui::Context, doc: &Document, id: photocraft_doc::LayerId, mask: &photocraft_doc::LayerMask) -> egui::TextureId {
        let rev = surface_fingerprint(&mask.surface) ^ (doc.size.width as u64) << 40;
        let key = (id, mask_thumbs_ui::THUMB_MASK);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
        {
            return tex.id();
        }
        let mut v = [0.0f32; 1];
        let img = thumb_image(doc, 64, |x, y| {
            mask.surface.read_pixel(x, y, &mut v);
            [v[0], v[0], v[0], 1.0]
        });
        self.store_thumb(ctx, key, rev, img)
    }

    fn store_thumb(&mut self, ctx: &egui::Context, key: (photocraft_doc::LayerId, u8), rev: u64, img: egui::ColorImage) -> egui::TextureId {
        match self.thumbs.get_mut(&key) {
            Some((r, tex)) => {
                tex.set(img, egui::TextureOptions::LINEAR);
                *r = rev;
                tex.id()
            }
            None => {
                let tex = ctx.load_texture(format!("thumb-{}-{}", key.0.0, key.1), img, egui::TextureOptions::LINEAR);
                let id = tex.id();
                self.thumbs.insert(key, (rev, tex));
                id
            }
        }
    }
}

/// Cheap identity of a surface's pixels: tile coordinates and `Arc` pointers.
pub fn surface_fingerprint(s: &photocraft_raster::Surface) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ s.tile_count() as u64;
    for (c, t) in s.tiles() {
        let p = std::sync::Arc::as_ptr(t) as usize as u64;
        h = (h ^ p ^ ((c.tx as u64) << 32 | c.ty as u32 as u64)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Square thumbnail of the canvas area, letterboxed, sampling `f(x, y)` in document space.
fn thumb_image(doc: &Document, side: usize, mut f: impl FnMut(i32, i32) -> [f32; 4]) -> egui::ColorImage {
    let (w, h) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
    let scale = w.max(h) / side as f32;
    let (ox, oy) = ((side as f32 - w / scale) / 2.0, (side as f32 - h / scale) / 2.0);
    let mut px = vec![egui::Color32::TRANSPARENT; side * side];
    for ty in 0..side {
        for tx in 0..side {
            let dx = (tx as f32 - ox + 0.5) * scale;
            let dy = (ty as f32 - oy + 0.5) * scale;
            if dx < 0.0 || dy < 0.0 || dx >= w || dy >= h {
                continue;
            }
            let c = f(dx as i32, dy as i32);
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[ty * side + tx] = egui::Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]));
        }
    }
    egui::ColorImage::new([side, side], px)
}

impl PhotocraftApp {
    /// Next step of queued synthetic input (automation): events up to and including the first
    /// release, so egui sees press and release in separate frames. Hosts that don't call
    /// `raw_input_hook` (offscreen harnesses) feed these to their input themselves.
    pub fn take_synthetic_step(&mut self) -> Vec<egui::Event> {
        let n = self
            .synthetic
            .iter()
            .position(|e| matches!(e, egui::Event::PointerButton { pressed: false, .. } | egui::Event::Key { pressed: false, .. }))
            .map_or(self.synthetic.len(), |i| i + 1);
        let events: Vec<_> = self.synthetic.drain(..n).collect();
        if !events.is_empty() {
            self.automation_input = true;
        }
        events
    }

    /// Install fonts, image loaders and the theme. Call from the app creator when possible so the
    /// very first frame renders; otherwise `logic` does it and the first frame is skipped.
    pub fn setup_context(ctx: &egui::Context, kind: theme::ThemeKind) {
        theme::install_fonts(ctx);
        egui_extras::install_image_loaders(ctx);
        theme::apply(ctx, kind);
        // egui's own ⌘+ / ⌘- / ⌘0 scale the whole interface; PhotoCraft zooms the canvas instead
        // (shortcuts.rs), like Photoshop.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
    }
}

impl PhotocraftApp {
    /// Cached `Surface::content_bounds` keyed by an id and the surface's tile identity.
    pub fn cached_bounds(&mut self, key: u64, surface: &photocraft_raster::Surface) -> photocraft_geom::Rect {
        let rev = surface_fingerprint(surface);
        if let Some((r, b)) = self.bounds_cache.get(&key)
            && *r == rev
        {
            return *b;
        }
        let b = surface.content_bounds();
        if self.bounds_cache.len() > 256 {
            self.bounds_cache.clear();
        }
        self.bounds_cache.insert(key, (rev, b));
        b
    }
}

impl PhotocraftApp {
    /// Zoom of the active document's main view (screen points per document pixel).
    pub fn current_zoom(&self) -> f32 {
        self.session.active_index().and_then(|i| self.ui.views.get(i)).map_or(1.0, |v| v.zoom)
    }
}

impl PhotocraftApp {
    /// Channels panel thumbnails of the active document snapshot: the composite,
    /// each colour channel (when there is more than one), each alpha channel, then the Quick Mask.
    pub fn channel_thumbs(&mut self, ctx: &egui::Context) -> Vec<egui::TextureId> {
        let Some(st) = self.session.active() else {
            self.channel_thumbs = None;
            return Vec::new();
        };
        let (id, doc) = (st.doc.id, st.doc.clone());
        // View-only commands bump revision without changing pixels. Keeping a Weak pins allocation
        // identity against address reuse without retaining the document's pixel data.
        let snapshot = std::sync::Arc::downgrade(&doc);
        if !matches!(&self.channel_thumbs, Some((d, old, _)) if *d == id && old.ptr_eq(&snapshot)) {
            let comp = photocraft_compose::thumbnail(&doc, 56);
            let (w, h) = (comp.width as usize, comp.height as usize);
            let side = w.max(h);
            let make = |f: &dyn Fn(&[u8]) -> egui::Color32, name: &str| {
                let mut px = vec![egui::Color32::TRANSPARENT; side * side];
                let (ox, oy) = ((side - w) / 2, (side - h) / 2);
                for y in 0..h {
                    for x in 0..w {
                        let o = (y * w + x) * 4;
                        px[(y + oy) * side + x + ox] = f(&comp.pixels[o..o + 4]);
                    }
                }
                ctx.load_texture(format!("chan-{name}"), egui::ColorImage::new([side, side], px), egui::TextureOptions::LINEAR)
            };
            let fmt = doc.pixel_format();
            let colors = fmt.mode.color_channels();
            let mut texs = vec![make(&|p| egui::Color32::from_rgb(p[0], p[1], p[2]), "composite")];
            if colors > 1 {
                for k in 0..colors {
                    let cmyk = fmt.mode == photocraft_doc::ColorMode::Cmyk;
                    texs.push(make(
                        &|p| {
                            let rgba = [p[0], p[1], p[2], 255].map(|v| f32::from(v) / 255.0);
                            let x = photocraft_raster::from_rgba(&fmt, rgba)[k];
                            let g = if cmyk { 1.0 - x } else { x };
                            egui::Color32::from_gray((g.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                        },
                        &format!("c{k}"),
                    ));
                }
            }
            // Alpha channels and the Quick Mask, sampled at thumbnail resolution.
            let (dw, dh) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
            for (i, ch) in doc.channels.iter().chain(doc.quick_mask.as_ref()).enumerate() {
                let mut px = vec![egui::Color32::TRANSPARENT; side * side];
                let (ox, oy) = ((side - w) / 2, (side - h) / 2);
                for y in 0..h {
                    for x in 0..w {
                        let sx = ((x as f32 + 0.5) * dw / w as f32) as i32;
                        let sy = ((y as f32 + 0.5) * dh / h as f32) as i32;
                        let v = ch.surface.sample_channel(sx, sy, 0);
                        px[(y + oy) * side + x + ox] = egui::Color32::from_gray((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                    }
                }
                texs.push(ctx.load_texture(format!("chan-a{i}"), egui::ColorImage::new([side, side], px), egui::TextureOptions::LINEAR));
            }
            self.channel_thumbs = Some((id, snapshot, texs));
        }
        self.channel_thumbs.as_ref().map(|(_, _, t)| t.iter().map(|t| t.id()).collect()).unwrap_or_default()
    }
}

fn clip_signature(w: u32, h: u32, px: &[u8]) -> u64 {
    let mut sig = (w as u64) << 32 | h as u64;
    for b in px.iter().step_by(997) {
        sig = sig.rotate_left(5) ^ *b as u64;
    }
    sig
}

impl PhotocraftApp {
    /// Mirror the session clipboard onto the OS clipboard (RGBA8).
    fn export_os_clipboard(&mut self) {
        let (Some(set), Some(clip)) = (self.services.clipboard_set_image.as_mut(), self.session.clipboard.as_ref()) else { return };
        let b = clip.bounds;
        if b.is_empty() {
            return;
        }
        let mut px = vec![[0u8; 4]; b.width() as usize * b.height() as usize];
        clip.surface.read_rgba8_into(b, &mut px);
        let bytes: Vec<u8> = px.into_iter().flatten().collect();
        if set(b.width(), b.height(), &bytes).is_ok() {
            self.os_clip_sig = Some(clip_signature(b.width(), b.height(), &bytes));
        }
    }

    /// If the OS clipboard holds an image that isn't the one we put there, make it the session
    /// clipboard (so ⌘V pastes screenshots and images copied in other apps, like Photoshop).
    /// Returns true when a new external image was imported.
    pub(crate) fn import_os_clipboard(&mut self) -> bool {
        let Some(get) = self.services.clipboard_get_image.as_mut() else { return false };
        let Some((w, h, bytes)) = get() else { return false };
        if w == 0 || h == 0 || bytes.len() != w as usize * h as usize * 4 {
            return false;
        }
        let sig = clip_signature(w, h, &bytes);
        if self.os_clip_sig == Some(sig) && self.session.clipboard.is_some() {
            return false;
        }
        let r = photocraft_geom::Rect::new(0, 0, w as i32, h as i32);
        let mut surface = photocraft_raster::Surface::from_interleaved(photocraft_color::PixelFormat::RGBA8, r, &bytes);
        surface.prune();
        self.session.clipboard = Some(photocraft_engine::edit_cmds::Clip { surface, bounds: r });
        self.os_clip_sig = Some(sig);
        self.clip_external = true;
        true
    }
}

#[cfg(test)]
mod input_tests;

#[cfg(test)]
mod pencil_tests;

#[cfg(test)]
mod move_auto_select_tests;

#[cfg(test)]
mod marquee_tests;

#[cfg(test)]
mod stamp_tests;

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn os_clipboard_bridge() {
        let os: OsClip = Arc::default();
        let (a, b) = (os.clone(), os.clone());
        let services = Services {
            clipboard_set_image: Some(Box::new(move |w: u32, h: u32, px: &[u8]| {
                *a.lock().unwrap() = Some((w, h, px.to_vec()));
                Ok(())
            })),
            clipboard_get_image: Some(Box::new(move || b.lock().unwrap().clone())),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Session::new(), services);
        app.session.execute("file.new", serde_json::json!({"width": 64, "height": 64})).unwrap();
        app.sync_views();
        app.run("select.rect", serde_json::json!({"x": 0, "y": 0, "width": 8, "height": 4})).unwrap();
        app.run("edit.copy", serde_json::json!({})).unwrap();
        let (w, h, px) = os.lock().unwrap().clone().unwrap();
        assert_eq!((w, h, px.len()), (8, 4, 8 * 4 * 4));
        // Our own image comes back unchanged (no re-import, keeps the original position).
        assert!(!app.import_os_clipboard());
        // Another app puts a 3×2 red image on the clipboard: ⌘V pastes it.
        *os.lock().unwrap() = Some((3, 2, [255u8, 0, 0, 255].repeat(6)));
        app.run("edit.paste", serde_json::json!({})).unwrap();
        let st = app.session.active().unwrap();
        let surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        assert_eq!(surf.content_bounds().width(), 3);
    }

    type OsClip = Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>;

    /// An app whose OS clipboard is `os`, counting every read in `reads`.
    fn app_with_os_clipboard(os: &OsClip, reads: &Arc<std::sync::atomic::AtomicUsize>) -> PhotocraftApp {
        let (b, n) = (os.clone(), reads.clone());
        let get = move || {
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            b.lock().unwrap().clone()
        };
        let mut app = PhotocraftApp::new(Session::new(), Services { clipboard_get_image: Some(Box::new(get)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 64, "height": 64})).unwrap();
        app.sync_views();
        app
    }

    #[test]
    fn external_clipboard_enables_and_centres_paste() {
        // Paste is enabled from the clipboard service alone (no read); the explicit paste imports
        // the OS image, once, and centres it like Photoshop does for foreign clipboard content.
        let (os, reads) = (OsClip::default(), Arc::default());
        let mut app = app_with_os_clipboard(&os, &reads);
        *os.lock().unwrap() = Some((4, 4, [255u8, 0, 0, 255].repeat(16)));
        assert!(app.session.clipboard.is_none());
        assert!(crate::menus::is_enabled(&app, "edit.paste"));
        assert!(crate::menus::is_enabled(&app, "edit.pasteSpecial.pasteInPlace"));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0, "enablement never reads the clipboard");
        let r = crate::menus::invoke(&mut app, &egui::Context::default(), "edit.paste", serde_json::json!({})).unwrap();
        assert_ne!(r["offset"], serde_json::json!([0, 0]), "external image is centred: {r}");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1, "one read per explicit paste");
        // Without a clipboard service Paste greys until something is copied in the app.
        let mut plain = PhotocraftApp::new(Session::new(), Services::default());
        plain.session.execute("file.new", serde_json::json!({"width": 8, "height": 8})).unwrap();
        assert!(!crate::menus::is_enabled(&plain, "edit.paste"));
    }

    #[test]
    fn pasting_an_empty_or_non_image_os_clipboard_is_a_quiet_no_op() {
        let (os, reads) = (OsClip::default(), Arc::default());
        let mut app = app_with_os_clipboard(&os, &reads);
        let layers = app.session.active().unwrap().doc.layers.len();
        let r = crate::menus::invoke(&mut app, &egui::Context::default(), "edit.paste", serde_json::json!({})).unwrap();
        assert_eq!(r["pasted"], serde_json::json!(false));
        assert_eq!(app.run("edit.pasteSpecial.pasteInPlace", serde_json::json!({})).unwrap()["pasted"], serde_json::json!(false));
        assert_eq!(app.session.active().unwrap().doc.layers.len(), layers);
        assert!(!app.ui.status_error && !app.ui.status.is_empty());
    }

    #[test]
    fn idle_frames_never_read_the_os_clipboard() {
        let (os, reads) = (OsClip::default(), Arc::<std::sync::atomic::AtomicUsize>::default());
        *os.lock().unwrap() = Some((4, 4, [255u8, 0, 0, 255].repeat(16)));
        let (b, n) = (os.clone(), reads.clone());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let get = move || {
                n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                b.lock().unwrap().clone()
            };
            let mut app = PhotocraftApp::new(Session::new(), Services { clipboard_get_image: Some(Box::new(get)), ..Default::default() });
            app.session.execute("file.new", serde_json::json!({"width": 64, "height": 64})).unwrap();
            app
        });
        for _ in 0..20 {
            h.step();
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0, "the OS clipboard is read only on an explicit paste");
        assert!(h.state().session.clipboard.is_none());
    }
}

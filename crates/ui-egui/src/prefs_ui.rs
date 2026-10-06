//! Edit › Preferences, Keyboard Shortcuts and Menus, Color Settings and the rest of the Edit
//! menu's dialogs, plus the shell side of preferences: loading and saving them through
//! [`crate::Services`], applying the interface theme, autosave and crash recovery, and the
//! history log.
//!
//! The values live in the engine ([`photocraft_engine::prefs::Preferences`], so agents read and
//! change them with `prefs.get` / `prefs.set`); this module only edits a working copy in a dialog
//! and commits it with those commands on Apply or OK.

use std::collections::{BTreeMap, HashMap};

use egui::{Color32, RichText, Sense, vec2};
use photocraft_doc::DocId;
use photocraft_engine::prefs::{self, SECTIONS, Theme};
use photocraft_engine::snap::{SnapLine, SnapTargets};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::{ThemeKind, Tokens};

/// Shell runtime state for preferences, autosave and snapping (not serialised).
#[derive(Default)]
pub struct Runtime {
    loaded: bool,
    saved_rev: u64,
    theme_pref: Option<Theme>,
    next_autosave_ms: f64,
    autosaved: HashMap<DocId, u64>,
    log_len: usize,
    /// Snapping state of the drag in progress (see `snap_ui`).
    pub(crate) snap: Option<crate::snap_ui::ActiveSnap>,
    pub(crate) snap_lines: Vec<SnapLine>,
    pub(crate) guide_targets: Option<((DocId, u64), SnapTargets)>,
    /// Checkerboard colours the cached CPU checker texture was built with.
    pub(crate) checker_key: Option<[[u8; 3]; 2]>,
    /// Style last sent to the GPU canvas.
    pub(crate) gpu_style: Option<crate::gpu_canvas::CanvasStyle>,
}

/// The GPU canvas colours from Preferences › Transparency & Gamut.
pub fn canvas_style(app: &PhotocraftApp) -> crate::gpu_canvas::CanvasStyle {
    let t = &app.session.prefs().transparency_and_gamut;
    let [l, d] = t.colors();
    let f = |c: [u8; 3]| c.map(|v| v as f32 / 255.0);
    let g = prefs::parse_hex(&t.gamut_warning_color).unwrap_or([128; 3]);
    crate::gpu_canvas::CanvasStyle {
        checker_square: t.square().unwrap_or(0.0),
        checker_light: f(l),
        checker_dark: f(d),
        gamut_color: f(g),
        gamut_opacity: t.gamut_warning_opacity as f32 / 100.0,
    }
}

/// Pasteboard colour from Preferences › Interface (`None` = the theme's default canvas).
pub fn pasteboard_color(app: &PhotocraftApp) -> Option<Color32> {
    use prefs::CanvasColor;
    let i = &app.session.prefs().interface;
    Some(match i.canvas_color {
        CanvasColor::Default => return None,
        CanvasColor::Black => Color32::BLACK,
        CanvasColor::DarkGray => Color32::from_gray(40),
        CanvasColor::MediumGray => Color32::from_gray(83),
        CanvasColor::LightGray => Color32::from_gray(184),
        CanvasColor::Custom => color_of(&i.canvas_custom_color),
    })
}

fn theme_kind(t: Theme) -> ThemeKind {
    match t {
        Theme::Pro => ThemeKind::Pro,
        Theme::ProMedium => ThemeKind::ProMedium,
        Theme::Studio => ThemeKind::Studio,
        Theme::StudioLight => ThemeKind::StudioLight,
        Theme::Classic => ThemeKind::Classic,
    }
}

fn theme_pref(k: ThemeKind) -> Theme {
    match k {
        ThemeKind::Pro => Theme::Pro,
        ThemeKind::ProMedium => Theme::ProMedium,
        ThemeKind::Studio => Theme::Studio,
        ThemeKind::StudioLight => Theme::StudioLight,
        ThemeKind::Classic => Theme::Classic,
    }
}

// ------------------------------------------------------------------ lifecycle

/// Load saved preferences (once) and recover autosaved documents. Called when the app is
/// created.
pub fn load(app: &mut PhotocraftApp) {
    if app.prefs_rt.loaded {
        return;
    }
    app.prefs_rt.loaded = true;
    if let Some(text) = app.services.load_prefs.as_mut().and_then(|f| f())
        && let Err(e) = app.session.load_prefs_json(&text)
    {
        app.ui.status = format!("Preferences were reset: {e}");
    }
    crate::dock::restore(app);
    app.sync_recent();
    app.prefs_rt.saved_rev = app.session.prefs.rev();
    if app.session.prefs().file_handling.recover_on_launch
        && let Some(recover) = app.services.recover.as_mut()
    {
        let docs = recover();
        let n = docs.len();
        for (path, doc) in docs {
            app.session.add_document(doc, path);
            // Recovered documents are unsaved.
            if let Some(st) = app.session.active_mut() {
                st.saved_revision = 0;
            }
        }
        if n > 0 {
            app.sync_views();
            app.ui.status = format!("Recovered {n} document{}", if n == 1 { "" } else { "s" });
        }
    }
}

/// Attach the brush preset store once its background load finishes, and surface write
/// failures in the status bar.
fn presets_store(app: &mut PhotocraftApp) {
    if let Some(rx) = &app.services.preset_store {
        match rx.try_recv() {
            Ok(opened) => {
                app.services.preset_store = None;
                let warnings = app.session.attach_preset_store(opened);
                if let Some(w) = warnings.first() {
                    app.ui.status = if warnings.len() == 1 { w.clone() } else { format!("{w} (+{} more brush preset warnings)", warnings.len() - 1) };
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => app.services.preset_store = None,
        }
    }
    if let Some(w) = app.session.preset_store.as_mut().map(|s| s.take_warnings()).and_then(|w| w.into_iter().next()) {
        app.ui.status = w;
    }
}

/// Resolve an absolute pixels-per-point preference without multiplying the OS DPI twice.
/// Monitor dimensions are physical pixels, independent of window size and UI zoom.
fn display_scale(pref: prefs::UiScale, native: Option<f32>, monitor_px: Option<egui::Vec2>) -> f32 {
    let native = native.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
    match pref {
        prefs::UiScale::P100 => 1.0,
        prefs::UiScale::P200 => 2.0,
        prefs::UiScale::Auto => {
            // A 4K display needs at least 200%; preserve larger system scales.
            let is_4k = monitor_px.is_some_and(|s| s.x.is_finite() && s.y.is_finite() && s.x.min(s.y) >= 2160.0 && s.x.max(s.y) >= 3840.0);
            if is_4k { native.max(2.0) } else { native }
        }
    }
}

fn sync_display_scale(app: &PhotocraftApp, ctx: &egui::Context) {
    let native = ctx.native_pixels_per_point();
    // ViewportInfo uses egui points, including the current UI zoom. Converting back to
    // physical pixels avoids oscillating between 100% and 200% on successive frames.
    // logic() can receive new viewport DPI before InputState::pixels_per_point updates.
    let native_scale = native.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
    let monitor_px = ctx.input(|i| i.viewport().monitor_size).map(|s| s * (native_scale * ctx.zoom_factor()));
    let scale = display_scale(app.session.prefs().interface.ui_scale, native, monitor_px);
    ctx.set_zoom_factor(scale / native_scale);
}

/// Interface › Show Tooltips and Tools › Show Tooltips (either one off hides them): egui never
/// shows a tooltip whose delay is infinite. Re-checked every frame because a theme change
/// rebuilds the style; that's one style read, and a write only when it differs.
fn sync_tooltips(app: &PhotocraftApp, ctx: &egui::Context) {
    let p = app.session.prefs();
    let delay = if p.interface.show_tooltips && p.tools.show_tooltips { crate::theme::TOOLTIP_DELAY } else { f32::INFINITY };
    if ctx.global_style().interaction.tooltip_delay != delay {
        ctx.global_style_mut(|s| s.interaction.tooltip_delay = delay);
    }
}

/// Per-frame upkeep: theme sync, persistence, autosave and the history log.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.prefs_rt.loaded {
        load(app);
    }
    sync_display_scale(app, ctx);
    // Interface theme: a preference change applies to the UI; a theme picked from the Window
    // menu is stored as the preference.
    let pref = app.session.prefs().interface.theme;
    if app.prefs_rt.theme_pref != Some(pref) {
        app.prefs_rt.theme_pref = Some(pref);
        if app.ui.theme != theme_kind(pref) {
            app.set_theme(ctx, theme_kind(pref));
        }
    } else if theme_pref(app.ui.theme) != pref {
        let t = theme_pref(app.ui.theme);
        app.session.prefs.edit(|p| p.interface.theme = t);
        app.prefs_rt.theme_pref = Some(t);
    }
    presets_store(app);
    sync_tooltips(app, ctx);
    app.sync_recent();
    if app.session.prefs.rev() != app.prefs_rt.saved_rev {
        app.prefs_rt.saved_rev = app.session.prefs.rev();
        let text = app.session.prefs_to_json();
        if let Some(save) = app.services.save_prefs.as_mut()
            && let Err(e) = save(&text)
        {
            app.ui.status = format!("Couldn't save preferences: {e}");
        }
    }
    let style = canvas_style(app);
    if let Some(gpu) = app.gpu.as_ref()
        && app.prefs_rt.gpu_style != Some(style)
    {
        gpu.set_style(style);
        app.prefs_rt.gpu_style = Some(style);
    }
    autosave(app);
    history_log(app);
}

/// Background autosave of documents with unsaved changes every N minutes (File Handling).
fn autosave(app: &mut PhotocraftApp) {
    let fh = &app.session.prefs().file_handling;
    let (on, minutes) = (fh.autosave, fh.autosave_minutes.max(1));
    if app.services.autosave.is_none() {
        return;
    }
    let now = crate::gpu_canvas::now_ms();
    // Saved or closed documents drop their recovery data.
    let live: HashMap<DocId, bool> = app.session.documents().iter().map(|d| (d.doc.id, d.is_dirty())).collect();
    let stale: Vec<DocId> = app.prefs_rt.autosaved.keys().filter(|id| live.get(id) != Some(&true)).copied().collect();
    for id in stale {
        app.prefs_rt.autosaved.remove(&id);
        if let Some(d) = app.services.discard_autosave.as_mut() {
            d(id.0);
        }
    }
    if !on {
        return;
    }
    let interval = minutes as f64 * 60_000.0;
    if app.prefs_rt.next_autosave_ms == 0.0 || app.prefs_rt.next_autosave_ms > now + interval {
        app.prefs_rt.next_autosave_ms = now + interval;
        return;
    }
    if now < app.prefs_rt.next_autosave_ms {
        return;
    }
    app.prefs_rt.next_autosave_ms = now + interval;
    let jobs: Vec<_> = app
        .session
        .documents()
        .iter()
        .filter(|d| d.is_dirty() && app.prefs_rt.autosaved.get(&d.doc.id) != Some(&d.revision))
        .map(|d| (d.doc.clone(), d.revision, d.path.clone()))
        .collect();
    for (doc, rev, path) in jobs {
        if let Some(save) = app.services.autosave.as_mut() {
            match save(&doc, rev, path.as_deref()) {
                Ok(()) => {
                    app.prefs_rt.autosaved.insert(doc.id, rev);
                }
                Err(e) => app.ui.status = format!("Autosave failed: {e}"),
            }
        }
    }
}

/// Force the autosave timer to fire on the next tick (tests, `prefs` changes).
pub fn autosave_now(app: &mut PhotocraftApp) {
    app.prefs_rt.next_autosave_ms = f64::MIN_POSITIVE;
}

/// History Log preference: append executed commands to a text file.
fn history_log(app: &mut PhotocraftApp) {
    let n = app.session.journal.len();
    if n <= app.prefs_rt.log_len {
        app.prefs_rt.log_len = n;
        return;
    }
    let hl = app.session.prefs().history_log.clone();
    let start = app.prefs_rt.log_len;
    app.prefs_rt.log_len = n;
    if !hl.enabled || hl.file_path.is_empty() || hl.destination == prefs::LogDestination::Metadata {
        return;
    }
    let Some(append) = app.services.append_text.as_mut() else { return };
    let mut text = String::new();
    for (id, params) in &app.session.journal[start..] {
        let label = photocraft_engine::commands::find(id).map_or(id.as_str(), |c| c.label).trim_end_matches('…');
        match hl.detail {
            prefs::LogDetail::SessionsOnly => {
                if matches!(id.as_str(), "file.new" | "file.open" | "file.close" | "file.openAs") {
                    text.push_str(&format!("{label}\n"));
                }
            }
            prefs::LogDetail::Concise => text.push_str(&format!("{label}\n")),
            prefs::LogDetail::Detailed => text.push_str(&format!("{label}  {id} {params}\n")),
        }
    }
    if !text.is_empty() {
        let _ = append(&hl.file_path, &text);
    }
}

// ------------------------------------------------------------------ shortcuts

pub use crate::shortcuts::{default_shortcut, effective_shortcut};

/// Every shortcut-bearing command (engine registry plus shell commands): (id, label, menu path,
/// default shortcut), menu items first in Photoshop order.
pub fn shortcut_items(app: &PhotocraftApp) -> Vec<(String, String, Vec<String>, Option<String>)> {
    let mut out: Vec<(String, String, Vec<String>, Option<String>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for it in crate::menus::menu_items(app) {
        if it.label == "---" || !seen.insert(it.id.clone()) {
            continue;
        }
        let default = default_shortcut(&it.id).or(it.shortcut.clone().filter(|_| !app.session.prefs().shortcuts.contains_key(&it.id)));
        out.push((it.id, it.label, it.path, default));
    }
    // Commands without a menu item: the colour and fill keys (D, X, ⌥⌫…) sit under Tools, as
    // Photoshop lists its colour keys in the Tools shortcut set, after the rest; then the held
    // temporary tools (#249).
    let tools_key = |id: &str| id.starts_with("tools.") || photocraft_engine::fill_key_cmds::IDS.contains(&id);
    let mut tools = Vec::new();
    let mut layer = Vec::new();
    for c in photocraft_engine::command_specs() {
        if seen.insert(c.id.to_string()) && c.shortcut.is_some() {
            let item = (c.id.to_string(), c.label.to_string(), c.menu.iter().map(|s| s.to_string()).collect::<Vec<_>>(), c.shortcut.map(Into::into));
            if c.menu.is_empty() && tools_key(c.id) {
                tools.push((item.0, item.1, vec!["Tools".to_string()], item.3));
            } else if c.menu.is_empty() && c.id.starts_with("layer.") {
                // Stamp Visible / Stamp Down (#217): no menu item in Photoshop; listed at the end
                // of the Layer section.
                layer.push((item.0, item.1, vec!["Layer".to_string()], item.3));
            } else {
                out.push(item);
            }
        }
    }
    let at = out.iter().rposition(|i| i.2.first().map(String::as_str) == Some("Layer")).map_or(out.len(), |i| i + 1);
    out.splice(at..at, layer);
    out.extend(tools);
    for (id, label, def) in prefs::TEMPORARY_TOOLS {
        if seen.insert(id.to_string()) {
            out.push((id.to_string(), label.to_string(), vec!["Tools".into(), "Temporary".into()], Some(def.to_string())));
        }
    }
    out
}

/// Text of a key press for the shortcut editor (`Cmd+Shift+K`), `None` for bare modifiers.
pub fn shortcut_text(key: egui::Key, m: egui::Modifiers) -> Option<String> {
    if is_modifier_key(key) {
        return None;
    }
    let mut parts = Vec::new();
    if m.command || m.mac_cmd {
        parts.push(tl!("Cmd").to_string());
    }
    if m.ctrl && !m.command {
        parts.push(tl!("Ctrl").into());
    }
    if m.alt {
        parts.push(tl!("Alt").into());
    }
    if m.shift {
        parts.push(tl!("Shift").into());
    }
    let name = match key {
        egui::Key::Equals => "=",
        egui::Key::Minus => "-",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        egui::Key::Semicolon => ";",
        egui::Key::Quote => "'",
        egui::Key::Plus => "",
        k => k.name(),
    };
    parts.push(name.to_string());
    prefs::normalize_shortcut(&parts.join("+"))
}

/// A modifier key on its own. egui reports Ctrl, Shift, Alt and ⌘ presses as key events too
/// (`ControlLeft`…), before the key pressed with them: they are never a shortcut's key, so
/// shortcut capture waits for the real key (#292: Ctrl+F was recorded as "Ctrl+ControlLeft").
pub fn is_modifier_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight)
}

// ------------------------------------------------------------------ menu routing

/// Shell front ends for Edit-menu commands invoked without parameters (dialogs, pickers).
/// `None` when `id` isn't handled here.
pub fn invoke(app: &mut PhotocraftApp, _ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let empty = params.as_object().is_none_or(|o| o.is_empty());
    if !empty {
        return None;
    }
    if let Some(section) = id.strip_prefix("edit.preferences.") {
        return Some(Ok(json!({"dialog": open_preferences(app, section)})));
    }
    let dialog = |d: Option<u64>| Some(d.map(|d| json!({"dialog": d})).ok_or_else(|| "not available".to_string()));
    match id {
        "edit.keyboardShortcuts" => Some(Ok(json!({"dialog": open_shortcuts(app, 0)}))),
        "edit.menus" => Some(Ok(json!({"dialog": open_shortcuts(app, 1)}))),
        "edit.toolbar" => Some(Ok(json!({"dialog": open_shortcuts(app, 2)}))),
        "edit.colorSettings" => {
            let d = crate::filter_dialog::open(app, "edit.colorSettings");
            if let Some(dm) = d.and_then(|d| app.ui.dialog_mut(d)) {
                let cur = serde_json::to_value(&app.session.color.settings).unwrap_or_default();
                for (k, v) in cur.as_object().into_iter().flatten() {
                    if dm.fields.contains_key(k) {
                        dm.fields.insert(k.clone(), v.clone());
                    }
                }
            }
            dialog(d)
        }
        "edit.fade" | "edit.findAndReplaceText" | "edit.defineBrushPreset" | "edit.defineCustomShape" | "edit.autoAlignLayers" | "edit.autoBlendLayers" => {
            if !app.session.is_enabled(id) {
                return Some(Err(photocraft_engine::commands::find(id)
                    .map_or("not available".into(), |c| format!("{} is not available right now", c.label.trim_end_matches('…')))));
            }
            let d = crate::filter_dialog::open(app, id);
            if let Some(dm) = d.and_then(|d| app.ui.dialog_mut(d)) {
                // Optional parameters the generic dialog can't express keep their defaults.
                for k in ["reference", "path"] {
                    dm.fields.remove(k);
                }
            }
            dialog(d)
        }
        "edit.contentAwareFill" => {
            if !app.session.is_enabled(id) {
                return Some(Err("Content-Aware Fill needs a selection on a pixel layer".into()));
            }
            let d = crate::filter_dialog::open(app, id);
            if let Some(dm) = d.and_then(|d| app.ui.dialog_mut(d)) {
                dm.fields.insert("__preview".into(), json!(true));
                for k in ["margin", "seed", "channel"] {
                    dm.fields.remove(k);
                }
            }
            dialog(d)
        }
        "edit.contentAwareScale" => {
            let st = app.session.active()?;
            let b = match &st.doc.selection {
                Some(s) => s.content_bounds(),
                None => st.active_layer.and_then(|l| st.doc.layer(l)).and_then(|l| l.surface()).map(|s| s.content_bounds()).unwrap_or_default(),
            };
            let d = crate::filter_dialog::open(app, id);
            if let Some(dm) = d.and_then(|d| app.ui.dialog_mut(d)) {
                dm.fields.insert("width".into(), json!(b.width()));
                dm.fields.insert("height".into(), json!(b.height()));
                dm.fields.remove("scaleX");
                dm.fields.remove("scaleY");
                dm.fields.insert("__preview".into(), json!(true));
            }
            dialog(d)
        }
        "edit.presets.presetManager" => {
            Some(Ok(json!({"dialog": open_kind(app, "presets", "Preset Manager", json!({"kind": "brushes", "selected": 0, "newName": ""}))})))
        }
        "edit.presets.exportImportPresets" => Some(Ok(
            json!({"dialog": open_kind(app, "presetsIO", "Export/Import Presets", json!({"action": "export", "brushes": true, "customShapes": true}))}),
        )),
        _ => None,
    }
}

fn open_kind(app: &mut PhotocraftApp, kind: &str, label: &str, fields: Value) -> u64 {
    let mut f = Map::new();
    f.insert("__prefsui".into(), json!(kind));
    f.insert("__label".into(), json!(label));
    if let Value::Object(m) = fields {
        f.extend(m);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Is this a dialog rendered by this module?
pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key("__prefsui")
}

/// Only Preferences supports applying changes without closing the dialog.
pub fn is_preferences(fields: &Map<String, Value>) -> bool {
    fields.get("__prefsui").and_then(Value::as_str) == Some("prefs")
}

fn preference_values(p: &prefs::Preferences) -> Map<String, Value> {
    let values = p.to_json();
    SECTIONS.iter().filter_map(|(id, _)| Some((id.to_string(), values.get(id)?.clone()))).collect()
}

/// Compare the editable sections, excluding settings managed outside Preferences.
pub fn preferences_changed(app: &PhotocraftApp, fields: &Map<String, Value>) -> bool {
    is_preferences(fields) && fields.get("values").and_then(Value::as_object).is_some_and(|values| *values != preference_values(app.session.prefs()))
}

/// Commit the working copy and keep the current section open. Failure leaves the draft intact.
pub fn apply(app: &mut PhotocraftApp, id: u64) -> Result<Value, String> {
    let d = app.ui.dialogs.iter().find(|d| d.id == id).ok_or_else(|| format!("no dialog {id}"))?;
    if d.kind != DialogKind::Command || !is_preferences(&d.fields) {
        return Err("Apply is only available for Preferences".into());
    }
    let fields = d.fields.clone();
    let result = confirm(app, &fields)?;
    let values = Value::Object(preference_values(app.session.prefs()));
    if let Some(d) = app.ui.dialog_mut(id) {
        // Use the validated values as the next draft, including any normalisation by prefs.set.
        d.fields.insert("values".into(), values);
    }
    Ok(result)
}

/// Max dialog width for our dialogs.
pub fn width(fields: &Map<String, Value>) -> Option<f32> {
    match fields.get("__prefsui").and_then(Value::as_str)? {
        "prefs" => Some(780.0),
        "shortcuts" => Some(720.0),
        _ => Some(460.0),
    }
}

/// Open Edit › Preferences on `section`.
pub fn open_preferences(app: &mut PhotocraftApp, section: &str) -> u64 {
    let section = if SECTIONS.iter().any(|(id, _)| *id == section) { section } else { "general" };
    let working = preference_values(app.session.prefs());
    let order = field_order(app.session.prefs(), &working);
    let gpu = app.perf.gpu_info.lines();
    open_kind(app, "prefs", "Preferences", json!({"section": section, "values": working, "__order": order, "__gpuInfo": gpu}))
}

/// Each section's keys in declaration order (JSON objects sort their keys; the serialised text
/// keeps the struct's order, which groups related settings like Photoshop's dialog).
fn field_order(p: &prefs::Preferences, sections: &Map<String, Value>) -> Value {
    let text = serde_json::to_string(p).unwrap_or_default();
    let mut out = Map::new();
    for (id, v) in sections {
        let Some(start) = text.find(&format!("\"{id}\":{{")) else { continue };
        let body = &text[start..];
        let end = body.find('}').unwrap_or(body.len());
        let region = &body[..end];
        let mut keys: Vec<(usize, String)> =
            v.as_object().into_iter().flatten().map(|(k, _)| (region.find(&format!("\"{k}\":")).unwrap_or(usize::MAX), k.clone())).collect();
        keys.sort();
        out.insert(id.clone(), json!(keys.into_iter().map(|(_, k)| k).collect::<Vec<_>>()));
    }
    Value::Object(out)
}

/// Open Keyboard Shortcuts and Menus on a tab (0 shortcuts, 1 menus, 2 toolbar).
pub fn open_shortcuts(app: &mut PhotocraftApp, tab: u64) -> u64 {
    let p = app.session.prefs();
    let fields = json!({
        "tab": tab,
        "filter": "",
        "overrides": p.shortcuts,
        "hidden": p.menus.hidden,
        "colors": p.menus.colors,
        "toolbarHidden": p.toolbar.hidden,
        "selected": "",
        "capture": false,
        "message": "",
    });
    open_kind(app, "shortcuts", tl!("Keyboard Shortcuts and Menus"), fields)
}

/// Open the Embedded Profile Mismatch prompt for the active document.
pub fn open_mismatch(app: &mut PhotocraftApp, report: &Value) -> u64 {
    let msg = if report.get("missing").and_then(Value::as_bool) == Some(true) {
        format!("The document does not have an embedded colour profile. Working space: {}.", report["working"].as_str().unwrap_or("?"))
    } else {
        format!(
            "The document has an embedded colour profile that does not match the working space.\nEmbedded: {}\nWorking: {}",
            report["embedded"].as_str().unwrap_or("?"),
            report["working"].as_str().unwrap_or("?")
        )
    };
    let action = match report["action"].as_str() {
        Some("converted") => "convert",
        Some("discarded") => "discard",
        _ => "preserve",
    };
    open_kind(
        app,
        "mismatch",
        tl!("Embedded Profile Mismatch"),
        json!({"message": msg, "action": action, "applied": action, "missing": report.get("missing").is_some()}),
    )
}

// ------------------------------------------------------------------ bodies

/// Render one of our dialogs' bodies.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    match f.get("__prefsui").and_then(Value::as_str).unwrap_or("") {
        "prefs" => prefs_body(ui, f),
        "shortcuts" => shortcuts_body(app, ui, f),
        "presets" => presets_body(app, ui, f),
        "presetsIO" => presets_io_body(ui, f),
        "mismatch" => mismatch_body(ui, f),
        _ => {}
    }
}

fn humanize(key: &str) -> String {
    let mut s = String::new();
    for (i, ch) in key.chars().enumerate() {
        if i == 0 {
            s.extend(ch.to_uppercase());
        } else if ch.is_uppercase() {
            s.push(' ');
            s.extend(ch.to_lowercase());
        } else {
            s.push(ch);
        }
    }
    s.replace("Psd", "PSD").replace("Gpu", "GPU").replace("Ui ", "UI ").replace("Mb", "(MB)").replace("Exif", "EXIF").replace("Hud", "HUD")
}

fn choice_label(v: &str) -> String {
    match v {
        "cm" => "Centimeters".into(),
        "mm" => "Millimeters".into(),
        "100" => "100%".into(),
        "200" => "200%".into(),
        "8" => "8 Bits/Channel".into(),
        "16" => "16 Bits/Channel".into(),
        "postScript" => "PostScript (72 points/inch)".into(),
        "traditional" => "Traditional (72.27 points/inch)".into(),
        "vulkan" => "Vulkan".into(),
        "dx12" => "DirectX 12".into(),
        "metal" => "Metal".into(),
        "gl" => "OpenGL".into(),
        "cpu" => "CPU (no GPU acceleration)".into(),
        v => humanize(v),
    }
}

fn color_of(s: &str) -> Color32 {
    prefs::parse_hex(s).map_or(Color32::GRAY, |c| Color32::from_rgb(c[0], c[1], c[2]))
}

/// Preferences: section list on the left, the section's settings on the right.
fn prefs_body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mut section = f.get("section").and_then(Value::as_str).unwrap_or("general").to_string();
    let mut values = f.get("values").cloned().unwrap_or(Value::Null);
    // The dialog follows the language being edited, so a change shows before OK.
    let lang = crate::i18n::Lang::from_pref(values.pointer("/interface/language").and_then(Value::as_str).unwrap_or("auto"));
    ui.horizontal_top(|ui| {
        // Section list.
        ui.vertical(|ui| {
            ui.set_width(170.0);
            for (id, title) in SECTIONS {
                let sel = section == id;
                let empty = !has_visible_fields(&values, id);
                let (rect, resp) = ui.allocate_exact_size(vec2(170.0, 22.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(rect, t.radius_sm, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, t.radius_sm, t.hover);
                }
                let color = if sel {
                    t.text
                } else if empty {
                    t.text_faint
                } else {
                    t.text_dim
                };
                // Translated section names can be longer than the column: elide them (the full
                // name is the tooltip) instead of drawing over the settings.
                let mut job = egui::text::LayoutJob::simple_singleline(tl!(title).to_string(), crate::theme::medium(12.5), color);
                job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 12.0);
                let galley = ui.painter().layout_job(job);
                let elided = galley.elided;
                ui.painter().galley(rect.left_center() + vec2(8.0, -galley.size().y / 2.0), galley, color);
                let resp = if elided { resp.on_hover_text(tl!(title)) } else { resp };
                if resp.clicked() {
                    section = id.to_string();
                }
            }
        });
        crate::widgets::vline(ui, 420.0);
        ui.vertical(|ui| {
            ui.set_width(540.0);
            let title = SECTIONS.iter().find(|(id, _)| *id == section).map_or("General", |(_, t)| *t);
            ui.label(RichText::new(tl!(&title)).font(crate::theme::semibold(14.0)).color(t.text));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(390.0).id_salt("prefs-scroll").show(ui, |ui| {
                let order: Vec<String> =
                    f.get("__order").and_then(|o| o.get(&section)).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
                if !has_visible_fields(&values, &section) {
                    ui.add_space(4.0);
                    ui.label(RichText::new(tl!("These settings aren't available in PhotoCraft yet.")).color(t.text_faint));
                } else if let Some(obj) = values.get_mut(&section).and_then(Value::as_object_mut) {
                    section_fields(ui, &section, obj, &order, lang);
                    if section == "performance" {
                        gpu_status_rows(ui, f.get("__gpuInfo"), obj);
                    }
                    ui.add_space(8.0);
                }
                if has_visible_fields(&values, &section)
                    && crate::widgets::secondary_button(ui, tl!("Reset Section"), 110.0).clicked()
                    && let Some(def) = prefs::Preferences::default().get(&section)
                {
                    values[&section] = def;
                }
            });
        });
    });
    f.insert("section".into(), json!(section));
    f.insert("values".into(), values);
}

/// Preferences › Performance: what the app renders with now, and a reset of the GPU backend
/// (a crashed start may have moved it to a safer choice).
fn gpu_status_rows(ui: &mut egui::Ui, info: Option<&Value>, obj: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    ui.label(RichText::new(tl!("Graphics")).font(crate::theme::semibold(12.5)).color(t.text));
    for line in info.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        ui.label(RichText::new(line).color(t.text_dim));
    }
    let auto = obj.get("gpuBackend").and_then(Value::as_str) == Some("auto");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(!auto, egui::Button::new(tl!("Reset GPU Backend"))).clicked() {
            obj.insert("gpuBackend".into(), json!("auto"));
        }
        ui.label(RichText::new(tl!("Applies at next launch.")).color(t.text_faint));
    });
}

/// Does `section` have any setting the dialog shows (see [`prefs::HIDDEN_UNTIL_IMPLEMENTED`])?
fn has_visible_fields(values: &Value, section: &str) -> bool {
    values.get(section).and_then(Value::as_object).is_some_and(|o| o.keys().any(|k| !prefs::is_hidden(&format!("{section}.{k}"))))
}

/// Generic editor for a section's fields: checkboxes, dropdowns for choices, colour swatches,
/// number fields with the preference's range, text fields.
fn section_fields(ui: &mut egui::Ui, section: &str, obj: &mut Map<String, Value>, order: &[String], lang: crate::i18n::Lang) {
    let t = Tokens::get(ui.ctx());
    let mut keys: Vec<String> = order.iter().filter(|k| obj.contains_key(*k)).cloned().collect();
    keys.extend(obj.keys().filter(|k| !order.contains(k)).cloned());
    egui::Grid::new(("prefs-grid", section)).num_columns(2).spacing([14.0, 7.0]).show(ui, |ui| {
        for k in keys {
            let path = format!("{section}.{k}");
            // Settings nothing reads yet stay out of the dialog (issue #204); their stored values
            // pass through untouched.
            if prefs::is_hidden(&path) {
                continue;
            }
            let v = obj.get(&k).cloned().unwrap_or(Value::Null);
            let human = humanize(&k);
            let label = tl!(&human).to_string();
            match &v {
                Value::Bool(b) => {
                    ui.label("");
                    let mut b = *b;
                    crate::widgets::checkbox(ui, &mut b, &label);
                    obj.insert(k, json!(b));
                }
                Value::String(s) if path == "interface.language" => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    let mut pairs: Vec<(String, &str)> = vec![("auto".into(), tl!("Auto"))];
                    pairs.extend(crate::i18n::Lang::all().map(|l| (l.code().to_string(), l.name())));
                    let mut cur = s.clone();
                    crate::widgets::dropdown(ui, &format!("pref-{path}"), &mut cur, &pairs, 220.0);
                    obj.insert(k, json!(cur));
                }
                Value::String(s) if prefs::choices(&path).is_some() => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    let opts = prefs::choices(&path).unwrap_or(&[]);
                    let labels: Vec<String> = opts.iter().map(|o| choice_label(o)).collect();
                    let pairs: Vec<(String, &str)> = opts.iter().map(|o| o.to_string()).zip(labels.iter().map(String::as_str)).collect();
                    let mut cur = s.clone();
                    crate::widgets::dropdown(ui, &format!("pref-{path}"), &mut cur, &pairs, 220.0);
                    obj.insert(k, json!(cur));
                }
                Value::String(s) if prefs::is_color(&path) => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    let c = prefs::parse_hex(s).unwrap_or([128, 128, 128]);
                    let mut rgb = c;
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut rgb);
                        ui.label(RichText::new(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])).font(crate::theme::mono(11.5)).color(t.text_dim));
                    });
                    obj.insert(k, json!(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])));
                    let _ = color_of(s);
                }
                Value::String(s) => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    let mut s = s.clone();
                    ui.add(egui::TextEdit::singleline(&mut s).desired_width(260.0));
                    obj.insert(k, json!(s));
                }
                Value::Number(n) => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    let (lo, hi) = prefs::range(&path).unwrap_or((-1e9, 1e9));
                    if n.is_u64() || n.is_i64() {
                        let mut x = n.as_i64().unwrap_or(0);
                        ui.add(egui::DragValue::new(&mut x).range(lo as i64..=hi as i64));
                        obj.insert(k, json!(x));
                    } else {
                        let mut x = n.as_f64().unwrap_or(0.0);
                        ui.add(egui::DragValue::new(&mut x).range(lo..=hi).speed(0.1).max_decimals(3));
                        obj.insert(k, json!(x));
                    }
                }
                Value::Array(items) if k == "disks" => {
                    ui.label(RichText::new(tl!("Scratch disks")).color(t.text_dim));
                    let mut items = items.clone();
                    ui.vertical(|ui| {
                        for d in &mut items {
                            ui.horizontal(|ui| {
                                let mut on = d.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                                let mut path = d.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                                crate::widgets::checkbox(ui, &mut on, "");
                                ui.add(egui::TextEdit::singleline(&mut path).desired_width(240.0));
                                *d = json!({"enabled": on, "path": path});
                            });
                        }
                        if ui.small_button(tl!("Add disk")).clicked() {
                            items.push(json!({"enabled": true, "path": ""}));
                        }
                    });
                    obj.insert(k, Value::Array(items));
                }
                Value::Array(items) => {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                    ui.label(RichText::new(crate::i18n::trn(lang, items.len() as u64, "{n} item", "{n} items")).color(t.text_faint));
                    if !items.is_empty() && ui.small_button(tl!("Clear")).clicked() {
                        obj.insert(k, json!([]));
                    }
                }
                _ => continue,
            }
            ui.end_row();
        }
    });
}

/// Keyboard Shortcuts and Menus: a searchable list of commands by menu with editable shortcuts
/// (click a shortcut, press keys; ⌫ removes it), menu visibility and colours, toolbar tools.
fn shortcuts_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mut tab = f.get("tab").and_then(Value::as_u64).unwrap_or(0);
    ui.horizontal(|ui| {
        for (i, name) in [tl!("Keyboard Shortcuts"), tl!("Menus"), tl!("Toolbar")].iter().enumerate() {
            if crate::widgets::pill_tab(ui, name, tab == i as u64).clicked() {
                tab = i as u64;
                f.insert("capture".into(), json!(false));
            }
        }
    });
    f.insert("tab".into(), json!(tab));
    ui.add_space(6.0);
    if tab == 2 {
        toolbar_tab(ui, f);
        return;
    }
    let mut filter = f.get("filter").and_then(Value::as_str).unwrap_or("").to_string();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Search")).color(t.text_dim));
        ui.add(egui::TextEdit::singleline(&mut filter).desired_width(260.0).hint_text(tl!("command or shortcut")));
    });
    f.insert("filter".into(), json!(filter));
    let mut overrides: BTreeMap<String, String> = f.get("overrides").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let mut hidden: Vec<String> = f.get("hidden").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let mut colors: BTreeMap<String, String> = f.get("colors").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let mut selected = f.get("selected").and_then(Value::as_str).unwrap_or("").to_string();
    let mut capture = f.get("capture").and_then(Value::as_bool).unwrap_or(false);
    let mut message = f.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    let items = shortcut_items(app);
    let eff = |overrides: &BTreeMap<String, String>, id: &str, def: &Option<String>| -> Option<String> {
        match overrides.get(id) {
            Some(s) if s.is_empty() => None,
            Some(s) => Some(s.clone()),
            None => def.clone(),
        }
    };
    // Key capture for the selected command.
    if capture && tab == 0 && !selected.is_empty() {
        let pressed = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } if !is_modifier_key(*key) => Some((*key, *modifiers)),
                _ => None,
            })
        });
        if let Some((key, m)) = pressed {
            ui.input_mut(|i| {
                i.consume_key(m, key);
            });
            if key == egui::Key::Escape {
                capture = false;
            } else if matches!(key, egui::Key::Backspace | egui::Key::Delete) && m.is_none() {
                overrides.insert(selected.clone(), String::new());
                capture = false;
                message = tl!("Shortcut removed.").into();
            } else if let Some(sc) = shortcut_text(key, m) {
                let clash: Vec<String> = items
                    .iter()
                    .filter(|(id, _, _, def)| *id != selected && eff(&overrides, id, def).as_deref().and_then(prefs::normalize_shortcut) == Some(sc.clone()))
                    .map(|(_, label, path, _)| format!("{} › {}", path.join(" › "), label.trim_end_matches('…')))
                    .collect();
                overrides.insert(selected.clone(), sc.clone());
                message = if clash.is_empty() {
                    format!("{} assigned.", crate::shortcuts::pretty(&sc))
                } else {
                    format!("{} is already in use by {} and will be removed from it when you click OK.", crate::shortcuts::pretty(&sc), clash.join(", "))
                };
                capture = false;
            }
        }
    }
    let needle = filter.to_ascii_lowercase();
    egui::ScrollArea::vertical().max_height(420.0).id_salt("shortcuts-scroll").show(ui, |ui| {
        let mut last_top = String::new();
        egui::Grid::new("shortcut-grid").num_columns(3).spacing([12.0, 3.0]).striped(true).show(ui, |ui| {
            for (id, label, path, def) in &items {
                let cur = eff(&overrides, id, def);
                let hay = format!("{} {} {}", path.join(" "), label, cur.clone().unwrap_or_default()).to_ascii_lowercase();
                if !needle.is_empty() && !hay.contains(&needle) && !id.to_ascii_lowercase().contains(&needle) {
                    continue;
                }
                if tab == 0 && path.is_empty() && def.is_none() && cur.is_none() {
                    continue;
                }
                let top = path.first().cloned().unwrap_or_else(|| tl!("Other").into());
                if top != last_top {
                    ui.label(RichText::new(&top).font(crate::theme::semibold(12.5)).color(t.text));
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                    last_top = top;
                }
                let name = if path.len() > 1 { format!("{} › {}", path[1..].join(" › "), label) } else { label.clone() };
                let sel = selected == *id;
                if ui.selectable_label(sel, RichText::new(format!("   {name}")).color(t.text_dim)).clicked() {
                    selected = id.clone();
                }
                if tab == 0 {
                    let text = if sel && capture { tl!("Press keys…").to_string() } else { cur.as_deref().map(crate::shortcuts::pretty).unwrap_or_default() };
                    let changed = overrides.contains_key(id);
                    let r = ui.add(
                        egui::Button::new(RichText::new(tl!(&text)).size(12.0).color(if changed { t.accent_text } else { t.text })).min_size(vec2(120.0, 18.0)),
                    );
                    if r.clicked() {
                        selected = id.clone();
                        capture = true;
                        message.clear();
                    }
                    ui.label(if changed { RichText::new(tl!("modified")).color(t.text_faint).size(10.5) } else { RichText::new("") });
                } else {
                    let mut visible = !hidden.contains(id);
                    crate::widgets::checkbox(ui, &mut visible, tl!("Visible"));
                    if visible {
                        hidden.retain(|h| h != id);
                    } else if !hidden.contains(id) {
                        hidden.push(id.clone());
                    }
                    let mut col = colors.get(id).cloned().unwrap_or_else(|| "none".into());
                    let opts: Vec<(String, &str)> =
                        ["none", "red", "orange", "yellow", "green", "blue", "violet", "gray"].iter().map(|c| (c.to_string(), *c)).collect();
                    crate::widgets::dropdown(ui, &format!("menu-color-{id}"), &mut col, &opts, 90.0);
                    if col == "none" {
                        colors.remove(id);
                    } else {
                        colors.insert(id.clone(), col);
                    }
                }
                ui.end_row();
            }
        });
    });
    ui.add_space(6.0);
    if tab == 0 {
        ui.horizontal(|ui| {
            let has_sel = !selected.is_empty();
            if ui.add_enabled(has_sel, egui::Button::new(tl!("Use Default"))).clicked() {
                overrides.remove(&selected);
                message = tl!("Default restored.").into();
            }
            if ui.add_enabled(has_sel, egui::Button::new(tl!("Delete Shortcut"))).clicked() {
                overrides.insert(selected.clone(), String::new());
                message = tl!("Shortcut removed.").into();
            }
            if ui.button(tl!("Reset All to Defaults")).clicked() {
                overrides.clear();
                message = tl!("All shortcuts reset to Photoshop defaults.").into();
            }
        });
    } else if ui.button(tl!("Show All Menu Items")).clicked() {
        hidden.clear();
        colors.clear();
    }
    if !message.is_empty() {
        ui.label(RichText::new(&message).color(if message.contains("already in use") { t.warning } else { t.text_dim }));
    }
    f.insert("overrides".into(), json!(overrides));
    f.insert("hidden".into(), json!(hidden));
    f.insert("colors".into(), json!(colors));
    f.insert("selected".into(), json!(selected));
    f.insert("capture".into(), json!(capture));
    f.insert("message".into(), json!(message));
}

fn toolbar_tab(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let mut hidden: Vec<String> = f.get("toolbarHidden").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    egui::ScrollArea::vertical().max_height(380.0).id_salt("toolbar-scroll").show(ui, |ui| {
        egui::Grid::new("toolbar-grid").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for tool in crate::state::Tool::ALL {
                let name = format!("{tool:?}");
                let mut on = !hidden.contains(&name);
                crate::widgets::checkbox(ui, &mut on, tl!(tool.label()));
                if on {
                    hidden.retain(|h| *h != name);
                } else if !hidden.contains(&name) {
                    hidden.push(name);
                }
                let k = tool.key();
                ui.label(if k == '\0' { String::new() } else { k.to_string() });
                ui.end_row();
            }
        });
    });
    if ui.button(tl!("Restore Defaults")).clicked() {
        hidden.clear();
    }
    f.insert("toolbarHidden".into(), json!(hidden));
}

fn presets_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mut kind = f.get("kind").and_then(Value::as_str).unwrap_or("brushes").to_string();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Preset Type")).color(t.text_dim));
        let opts = [("brushes".to_string(), tl!("Brushes")), ("customShapes".to_string(), tl!("Custom Shapes")), ("patterns".to_string(), tl!("Patterns"))];
        crate::widgets::dropdown(ui, "preset-kind", &mut kind, &opts, 180.0);
    });
    let list = app
        .session
        .execute("edit.presets.presetManager", json!({"kind": kind}))
        .ok()
        .and_then(|v| v.get(&kind).cloned())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
        .unwrap_or_default();
    let mut selected = f.get("selected").and_then(Value::as_u64).unwrap_or(0) as usize;
    egui::ScrollArea::vertical().max_height(260.0).id_salt("preset-list").show(ui, |ui| {
        for (i, name) in list.iter().enumerate() {
            if ui.selectable_label(i == selected, name).clicked() {
                selected = i;
                f.insert("newName".into(), json!(name));
            }
        }
        if list.is_empty() {
            ui.label(RichText::new(tl!("No presets of this type.")).color(t.text_faint));
        }
    });
    let mut new_name = f.get("newName").and_then(Value::as_str).unwrap_or("").to_string();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut new_name).desired_width(200.0));
        if ui.add_enabled(selected < list.len() && !new_name.trim().is_empty(), egui::Button::new(tl!("Rename"))).clicked() {
            let _ = app.run("edit.presets.presetManager", json!({"action": "rename", "kind": kind, "index": selected, "newName": new_name}));
        }
        if ui.add_enabled(selected < list.len(), egui::Button::new(tl!("Delete"))).clicked() {
            let _ = app.run("edit.presets.presetManager", json!({"action": "delete", "kind": kind, "index": selected}));
        }
        // Load Photoshop brushes (.abr) into the library.
        if kind == "brushes" && ui.button(tl!("Load…")).on_hover_text(tl!("Import Photoshop brushes (.abr)")).clicked() {
            app.open_dialog_file();
        }
    });
    f.insert("kind".into(), json!(kind));
    f.insert("selected".into(), json!(selected));
    f.insert("newName".into(), json!(new_name));
}

fn presets_io_body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mut action = f.get("action").and_then(Value::as_str).unwrap_or("export").to_string();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Action")).color(t.text_dim));
        crate::widgets::dropdown(
            ui,
            "presets-io",
            &mut action,
            &[("export".to_string(), tl!("Export Presets")), ("import".to_string(), tl!("Import Presets"))],
            180.0,
        );
    });
    for (k, label) in [("brushes", tl!("Brushes")), ("customShapes", tl!("Custom Shapes"))] {
        let mut on = f.get(k).and_then(Value::as_bool).unwrap_or(true);
        crate::widgets::checkbox(ui, &mut on, label);
        f.insert(k.into(), json!(on));
    }
    f.insert("action".into(), json!(action));
}

fn mismatch_body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(f.get("message").and_then(Value::as_str).unwrap_or("")).color(t.text));
    ui.add_space(6.0);
    let mut action = f.get("action").and_then(Value::as_str).unwrap_or("preserve").to_string();
    let missing = f.get("missing").and_then(Value::as_bool).unwrap_or(false);
    let opts: &[(&str, &str)] = if missing {
        &[("preserve", tl!("Leave as is (don't color manage)")), ("assignWorking", tl!("Assign working space"))]
    } else {
        &[
            ("preserve", tl!("Use the embedded profile (instead of the working space)")),
            ("convert", tl!("Convert document's colors to the working space")),
            ("discard", tl!("Discard the embedded profile (don't color manage)")),
        ]
    };
    for (v, label) in opts {
        ui.radio_value(&mut action, v.to_string(), *label);
    }
    f.insert("action".into(), json!(action));
}

// ------------------------------------------------------------------ confirm

/// OK on one of our dialogs.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    match f.get("__prefsui").and_then(Value::as_str).unwrap_or("") {
        "prefs" => {
            let values = f.get("values").cloned().unwrap_or(Value::Null);
            app.run("prefs.set", json!({"path": "", "value": values}))
        }
        "shortcuts" => {
            let overrides = f.get("overrides").cloned().unwrap_or(json!({}));
            // Shortcuts moved to another command are taken from their old owner (shell
            // commands included, which the engine doesn't know).
            let mut ov: BTreeMap<String, String> = serde_json::from_value(overrides).unwrap_or_default();
            let items = shortcut_items(app);
            let changed: Vec<(String, String)> = ov.iter().filter(|(_, s)| !s.is_empty()).map(|(k, s)| (k.clone(), s.clone())).collect();
            for (id, sc) in changed {
                for (other, _, _, def) in &items {
                    if *other != id
                        && !ov.contains_key(other)
                        && def.as_deref().and_then(prefs::normalize_shortcut).as_deref() == prefs::normalize_shortcut(&sc).as_deref()
                    {
                        ov.insert(other.clone(), String::new());
                    }
                }
            }
            let hidden = f.get("hidden").cloned().unwrap_or(json!([]));
            let colors = f.get("colors").cloned().unwrap_or(json!({}));
            let toolbar = f.get("toolbarHidden").cloned().unwrap_or(json!([]));
            app.run("edit.keyboardShortcuts", json!({"reset": true, "set": ov, "allowUnknown": true, "removeConflicts": false}))?;
            app.run("prefs.set", json!({"values": {"menus": {"hidden": hidden, "colors": colors}, "toolbar": {"hidden": toolbar}}}))
        }
        "presets" => Ok(Value::Null),
        "presetsIO" => {
            let kinds: Vec<&str> = ["brushes", "customShapes"].into_iter().filter(|k| f.get(*k).and_then(Value::as_bool).unwrap_or(true)).collect();
            if f.get("action").and_then(Value::as_str) == Some("import") {
                let (name, bytes) = app.services.pick_open.as_mut().and_then(|p| p()).ok_or("cancelled")?;
                let text = String::from_utf8(bytes).map_err(|_| format!("{name} is not a preset file"))?;
                app.run("edit.presets.exportImportPresets", json!({"action": "import", "kinds": kinds, "data": text}))
            } else {
                let out = app.run("edit.presets.exportImportPresets", json!({"action": "export", "kinds": kinds}))?;
                let text = serde_json::to_string_pretty(&out["data"]).map_err(|e| e.to_string())?;
                let path = app.services.pick_save.as_mut().and_then(|p| p("Presets.pcpresets")).ok_or("cancelled")?;
                let write = app.services.write.as_mut().ok_or("no writer configured")?;
                write(&path, text.as_bytes())?;
                app.ui.status = format!("Exported presets to {path}");
                Ok(json!({"path": path}))
            }
        }
        "mismatch" => {
            let action = f.get("action").and_then(Value::as_str).unwrap_or("preserve");
            let applied = f.get("applied").and_then(Value::as_str).unwrap_or("preserve");
            if action == applied {
                return Ok(json!({"action": action}));
            }
            app.run("color.profileMismatch", json!({"action": action}))
        }
        _ => Err("unknown dialog".into()),
    }
}

#[cfg(test)]
#[path = "shortcut_capture_tests.rs"]
mod shortcut_capture_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Every generated preference label, section title and choice label has an entry in each
    /// language that claims complete menus (Japanese, Traditional Chinese, ...).
    #[test]
    fn preference_labels_are_translated() {
        let session = photocraft_engine::Session::new();
        let v: Value = serde_json::from_str(&session.prefs_to_json()).expect("prefs json");
        for lang in crate::i18n::Lang::all().filter(|l| l.complete_menus()) {
            let mut missing = Vec::new();
            for (sec, title) in SECTIONS {
                if !crate::i18n::has(lang, title) {
                    missing.push(title.to_string());
                }
                let Some(obj) = v.get(sec).and_then(Value::as_object) else { continue };
                for k in obj.keys() {
                    let mut labels = vec![humanize(k)];
                    labels.extend(prefs::choices(&format!("{sec}.{k}")).into_iter().flatten().map(|c| choice_label(c)));
                    missing.extend(labels.into_iter().filter(|l| !crate::i18n::has(lang, l)));
                }
            }
            missing.sort();
            missing.dedup();
            assert!(missing.is_empty(), "{}: untranslated preference labels: {missing:#?}", lang.code());
        }
    }

    fn app_with_store() -> (PhotocraftApp, Arc<Mutex<Option<String>>>) {
        app_with_saved(None)
    }

    fn app_with_saved(text: Option<String>) -> (PhotocraftApp, Arc<Mutex<Option<String>>>) {
        let store: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(text));
        let (a, b) = (store.clone(), store.clone());
        let services = crate::Services {
            load_prefs: Some(Box::new(move || a.lock().unwrap().clone())),
            save_prefs: Some(Box::new(move |s: &str| {
                *b.lock().unwrap() = Some(s.to_string());
                Ok(())
            })),
            ..Default::default()
        };
        (PhotocraftApp::new(photocraft_engine::Session::new(), services), store)
    }

    #[test]
    fn auto_scale_detects_4k_and_preserves_larger_system_dpi() {
        use prefs::UiScale::Auto;
        for size in [vec2(3840.0, 2160.0), vec2(4096.0, 2160.0), vec2(2160.0, 3840.0)] {
            assert_eq!(display_scale(Auto, Some(1.0), Some(size)), 2.0);
            for dpi in [1.25, 1.5, 2.0] {
                assert_eq!(display_scale(Auto, Some(dpi), Some(size)), 2.0);
            }
        }
        assert_eq!(display_scale(Auto, Some(3.0), Some(vec2(3840.0, 2160.0))), 3.0);
        for size in [vec2(1920.0, 1080.0), vec2(2560.0, 1440.0), vec2(3840.0, 1080.0)] {
            assert_eq!(display_scale(Auto, Some(1.0), Some(size)), 1.0);
        }
        assert_eq!(display_scale(Auto, None, None), 1.0);
        assert_eq!(display_scale(Auto, Some(f32::NAN), Some(vec2(f32::INFINITY, 2160.0))), 1.0);
    }

    #[test]
    fn scale_preferences_and_monitor_changes_apply_live() {
        let (mut app, _) = app_with_store();
        let ctx = egui::Context::default();
        {
            let mut step = |physical: egui::Vec2, native: f32, expected: f32| {
                let mut input = egui::RawInput::default();
                let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
                viewport.native_pixels_per_point = Some(native);
                viewport.monitor_size = Some(physical / (native * ctx.zoom_factor()));
                ctx.run_ui(input, |ui| tick(&mut app, ui.ctx())).textures_delta.clear();
                // Scale changes take effect on the following pass.
                let mut input = egui::RawInput::default();
                let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
                viewport.native_pixels_per_point = Some(native);
                viewport.monitor_size = Some(physical / (native * ctx.zoom_factor()));
                ctx.run_ui(input, |ui| tick(&mut app, ui.ctx())).textures_delta.clear();
                assert!((ctx.pixels_per_point() - expected).abs() < 1e-4);
            };
            for _ in 0..4 {
                step(vec2(3840.0, 2160.0), 1.0, 2.0);
            }
            step(vec2(1920.0, 1080.0), 1.0, 1.0);
            step(vec2(3840.0, 2160.0), 1.5, 2.0);
        }
        for (pref, expected) in [("200", 2.0), ("100", 1.0), ("auto", 1.5)] {
            app.run("prefs.set", json!({"values": {"interface.uiScale": pref}})).unwrap();
            let mut input = egui::RawInput::default();
            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.5);
            ctx.run_ui(input.clone(), |ui| tick(&mut app, ui.ctx())).textures_delta.clear();
            ctx.run_ui(input, |ui| tick(&mut app, ui.ctx())).textures_delta.clear();
            assert!((ctx.pixels_per_point() - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn recent_files_survive_a_restart_and_honour_the_count() {
        let (mut app, store) = app_with_store();
        let ctx = egui::Context::default();
        tick(&mut app, &ctx);
        app.push_recent("/work/a.psd");
        app.push_recent("/work/b.png");
        tick(&mut app, &ctx);
        let saved = store.lock().unwrap().clone().unwrap();
        // A new app instance (a restart) gets the list back, newest first.
        let (mut app2, _) = app_with_saved(Some(saved));
        tick(&mut app2, &ctx);
        assert_eq!(app2.ui.recent_files, vec!["/work/b.png".to_string(), "/work/a.psd".to_string()]);
        // Lowering "Recent File List Contains" shortens the menu at once; 0 turns it off.
        app2.run("prefs.set", json!({"values": {"fileHandling.recentFileCount": 1}})).unwrap();
        tick(&mut app2, &ctx);
        assert_eq!(app2.ui.recent_files, vec!["/work/b.png".to_string()]);
        app2.push_recent("/work/c.tif");
        assert_eq!(app2.session.prefs().file_handling.recent_files, vec!["/work/c.tif".to_string()]);
        app2.run("prefs.set", json!({"values": {"fileHandling.recentFileCount": 0}})).unwrap();
        tick(&mut app2, &ctx);
        app2.push_recent("/work/d.tif");
        assert!(app2.ui.recent_files.is_empty());
        // Clearing the list in the Preferences dialog (or by an agent) reaches the menu.
        app2.run("prefs.set", json!({"values": {"fileHandling.recentFileCount": 20, "fileHandling.recentFiles": ["/x.psd"]}})).unwrap();
        tick(&mut app2, &ctx);
        assert_eq!(app2.ui.recent_files, vec!["/x.psd".to_string()]);
        // A hostile count from a hand-edited preferences file is capped, not trusted.
        app2.run("prefs.set", json!({"values": {"fileHandling.recentFileCount": 100}})).unwrap();
        app2.session.prefs.edit(|p| p.file_handling.recent_file_count = u32::MAX);
        assert_eq!(app2.recent_cap(), 100);
    }

    #[test]
    fn show_tooltips_preferences_turn_tooltips_off() {
        let (mut app, _) = app_with_store();
        let ctx = egui::Context::default();
        tick(&mut app, &ctx);
        assert_eq!(ctx.global_style().interaction.tooltip_delay, crate::theme::TOOLTIP_DELAY);
        for path in ["interface.showTooltips", "tools.showTooltips"] {
            app.run("prefs.set", json!({"values": {path: false}})).unwrap();
            tick(&mut app, &ctx);
            assert!(ctx.global_style().interaction.tooltip_delay.is_infinite(), "{path} off hides tooltips");
            app.run("prefs.set", json!({"values": {path: true}})).unwrap();
            tick(&mut app, &ctx);
            assert_eq!(ctx.global_style().interaction.tooltip_delay, crate::theme::TOOLTIP_DELAY);
        }
    }

    #[test]
    fn unimplemented_preferences_are_hidden_from_the_dialog() {
        let values = prefs::Preferences::default().to_json();
        assert!(has_visible_fields(&values, "general"));
        assert!(has_visible_fields(&values, "fileHandling"));
        // Every setting of these sections is still unimplemented.
        for section in ["type", "enhancedControls", "rawDefaults", "integrations", "scratchDisks"] {
            assert!(!has_visible_fields(&values, section), "{section}");
        }
        assert!(prefs::is_hidden("rawDefaults.applyAutoTone"));
        assert!(!prefs::is_hidden("general.autoShowHomeScreen"));
        assert!(!prefs::is_hidden("interface.uiScale"));
        // Hidden values still round-trip through the dialog untouched.
        let (mut app, _) = app_with_store();
        app.run("prefs.set", json!({"values": {"type.smartQuotes": false}})).unwrap();
        let id = open_preferences(&mut app, "type");
        let fields = app.ui.dialogs.iter().find(|d| d.id == id).map(|d| d.fields.clone()).unwrap();
        confirm(&mut app, &fields).unwrap();
        assert!(!app.session.prefs().type_.smart_quotes);
    }

    #[test]
    fn preferences_persist_through_services_and_theme_follows() {
        let (mut app, store) = app_with_store();
        let ctx = egui::Context::default();
        tick(&mut app, &ctx);
        app.run("prefs.set", json!({"values": {"interface.theme": "studioLight", "performance.historyStates": 12}})).unwrap();
        tick(&mut app, &ctx);
        assert_eq!(app.ui.theme, ThemeKind::StudioLight);
        let saved = store.lock().unwrap().clone().unwrap();
        assert!(saved.contains("\"historyStates\": 12"));
        // A new app instance loads them.
        let (mut app2, store2) = app_with_saved(Some(saved));
        tick(&mut app2, &ctx);
        assert_eq!(app2.session.prefs().performance.history_states, 12);
        assert_eq!(app2.ui.theme, ThemeKind::StudioLight);
        // Picking a theme from the Window menu becomes the preference.
        crate::menus::invoke(&mut app2, &ctx, "window.theme.classic", json!({})).unwrap();
        tick(&mut app2, &ctx);
        assert_eq!(app2.session.prefs().interface.theme, Theme::Classic);
        assert!(store2.lock().unwrap().as_ref().unwrap().contains("classic"));
    }

    #[test]
    fn brush_preset_store_attaches_when_loaded_and_persists() {
        use photocraft_engine::preset_store::{MemBackend, open};
        let mem = MemBackend::default();
        let ctx = egui::Context::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services { preset_store: Some(rx), ..Default::default() });
        // Still loading: presets made now are kept and written once the store arrives.
        tick(&mut app, &ctx);
        app.run("brush.presets.save", json!({"name": "Early"})).unwrap();
        assert!(mem.files.lock().unwrap().is_empty());
        tx.send(open(Box::new(mem.clone()))).unwrap();
        tick(&mut app, &ctx);
        assert!(app.services.preset_store.is_none() && app.session.preset_store.is_some());
        app.run("brush.presets.save", json!({"name": "Late"})).unwrap();
        let mut s2 = photocraft_engine::Session::new();
        assert!(s2.attach_preset_store(open(Box::new(mem))).is_empty());
        for n in ["Early", "Late"] {
            assert!(s2.tools.presets.iter().any(|p| p.name == n), "{n}");
        }
    }

    #[test]
    fn preferences_dialog_round_trip() {
        let (mut app, _) = app_with_store();
        let ctx = egui::Context::default();
        let r = crate::menus::invoke(&mut app, &ctx, "edit.preferences.cursors", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let d = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().clone();
        assert_eq!(d.fields["section"], "cursors");
        let mut values = d.fields["values"].clone();
        values["cursors"]["painting"] = json!("fullSizeTip");
        values["unitsAndRulers"]["rulers"] = json!("inches");
        app.ui.dialog_mut(id).unwrap().fields.insert("values".into(), values);
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(app.session.prefs().cursors.painting, prefs::PaintingCursor::FullSizeTip);
        assert_eq!(app.session.prefs().units_and_rulers.rulers, prefs::Unit::Inches);
        // Invalid values are rejected and nothing changes.
        let id = open_preferences(&mut app, "performance");
        let mut values = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().fields["values"].clone();
        values["performance"]["historyStates"] = json!(0);
        app.ui.dialog_mut(id).unwrap().fields.insert("values".into(), values);
        assert!(crate::dialogs::confirm(&mut app, id).is_err());
        assert_eq!(app.session.prefs().performance.history_states, 50);
    }

    #[test]
    fn preferences_apply_button_saves_without_closing_and_cancel_keeps_applied_values() {
        use egui_kittest::{
            Harness,
            kittest::{NodeT, Queryable},
        };

        let (mut app, store) = app_with_store();
        app.run("prefs.set", json!({"values": {"interface.language": "en", "type.smartQuotes": false}})).unwrap();
        let id = open_preferences(&mut app, "interface");
        let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            app
        });
        h.run_steps(4);
        assert!(h.get_by_label("Apply").accesskit_node().is_disabled());

        let values = h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap();
        values["interface"]["theme"] = json!("studioLight");
        values["performance"]["historyStates"] = json!(12);
        h.run_steps(2);
        assert!(!h.get_by_label("Apply").accesskit_node().is_disabled());
        h.get_by_label("Apply").click();
        h.run_steps(4);

        assert_eq!(h.state().session.prefs().performance.history_states, 12);
        assert_eq!(h.state().ui.theme, ThemeKind::StudioLight);
        assert!(!h.state().session.prefs().type_.smart_quotes, "hidden settings round-trip unchanged");
        let d = h.state().ui.dialogs.iter().find(|d| d.id == id).unwrap();
        assert_eq!(d.fields["section"], "interface");
        assert_eq!(d.fields["values"]["performance"]["historyStates"], 12);
        assert!(h.get_by_label("Apply").accesskit_node().is_disabled());
        let saved: Value = serde_json::from_str(store.lock().unwrap().as_ref().unwrap()).unwrap();
        assert_eq!(saved["performance"]["historyStates"], 12);
        assert_eq!(saved["interface"]["theme"], "studioLight");

        // Repeated Apply starts from the validated values, not the dialog's original snapshot.
        h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["performance"]["historyStates"] = json!(22);
        h.run_steps(2);
        h.get_by_label("Apply").click();
        h.run_steps(4);
        assert_eq!(h.state().session.prefs().performance.history_states, 22);
        h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["performance"]["historyStates"] = json!(33);
        h.run_steps(2);
        h.get_by_label("Cancel").click();
        h.run_steps(4);
        assert!(h.state().ui.dialogs.iter().all(|d| d.id != id));
        assert_eq!(h.state().session.prefs().performance.history_states, 22);
        let saved = store.lock().unwrap().clone().unwrap();
        let (mut restarted, _) = app_with_saved(Some(saved));
        tick(&mut restarted, &egui::Context::default());
        assert_eq!(restarted.session.prefs().performance.history_states, 22);
        assert_eq!(restarted.ui.theme, ThemeKind::StudioLight);
    }

    #[test]
    fn preferences_confirm_after_apply_commits_later_edits() {
        let (mut app, _) = app_with_store();
        let id = open_preferences(&mut app, "cursors");
        app.ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["cursors"]["painting"] = json!("fullSizeTip");
        apply(&mut app, id).unwrap();
        app.ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["unitsAndRulers"]["rulers"] = json!("inches");
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert!(app.ui.dialogs.iter().all(|d| d.id != id));
        assert_eq!(app.session.prefs().cursors.painting, prefs::PaintingCursor::FullSizeTip);
        assert_eq!(app.session.prefs().units_and_rulers.rulers, prefs::Unit::Inches);
    }

    #[test]
    fn preferences_apply_rejects_invalid_drafts_and_keeps_them_open() {
        let (mut app, _) = app_with_store();
        let before = app.session.prefs().to_json();
        for invalid in [json!(0), json!("invalid"), Value::Null] {
            let id = open_preferences(&mut app, "performance");
            app.ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["performance"]["historyStates"] = invalid;
            let draft = app.ui.dialog_mut(id).unwrap().fields.clone();
            assert!(apply(&mut app, id).is_err());
            assert_eq!(app.session.prefs().to_json(), before);
            assert_eq!(app.ui.dialog_mut(id).unwrap().fields, draft);
            app.ui.close_dialog(id);
        }
        let id = open_preferences(&mut app, "interface");
        app.ui.dialog_mut(id).unwrap().fields.remove("values");
        assert!(apply(&mut app, id).is_err());
        assert!(app.ui.dialog_mut(id).is_some());
        assert_eq!(app.session.prefs().to_json(), before);
    }

    #[test]
    fn apply_rejects_missing_and_non_preferences_dialogs() {
        let (mut app, _) = app_with_store();
        let before = app.session.prefs().to_json();
        assert!(apply(&mut app, u64::MAX).is_err());
        let id = open_shortcuts(&mut app, 0);
        assert!(apply(&mut app, id).is_err());
        assert!(app.ui.dialog_mut(id).is_some());
        let fields = json!({"__prefsui": "prefs", "values": preference_values(app.session.prefs())}).as_object().unwrap().clone();
        let id = app.ui.open_dialog(DialogKind::About, fields);
        assert!(apply(&mut app, id).is_err());
        assert!(app.ui.dialog_mut(id).is_some());
        assert_eq!(app.session.prefs().to_json(), before);
    }

    #[test]
    fn shortcut_dialog_moves_shortcuts_and_shell_dispatch_follows() {
        let (mut app, _) = app_with_store();
        let ctx = egui::Context::default();
        let id = crate::menus::invoke(&mut app, &ctx, "edit.keyboardShortcuts", json!({})).unwrap()["dialog"].as_u64().unwrap();
        // ⌘O (File › Open, a shell command) given to Layer › New › Layer.
        app.ui.dialog_mut(id).unwrap().fields.insert("overrides".into(), json!({"layer.new.layer": "Cmd+O"}));
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(effective_shortcut(&app, "layer.new.layer", None).as_deref(), Some("Cmd+O"));
        assert_eq!(effective_shortcut(&app, "file.open", Some("Cmd+O")), None, "taken from File › Open");
        // Menu items show the effective shortcut.
        let items = crate::menus::menu_items(&app);
        assert!(items.iter().any(|i| i.id == "layer.new.layer" && i.shortcut.as_deref() == Some("Cmd+O")));
        assert!(items.iter().any(|i| i.id == "file.open" && i.shortcut.is_none()));
        assert_eq!(shortcut_text(egui::Key::K, egui::Modifiers { command: true, shift: true, ..Default::default() }).as_deref(), Some("Cmd+Shift+K"));
    }

    #[test]
    fn hidden_menu_items_disappear() {
        let (mut app, _) = app_with_store();
        app.run("edit.menus", json!({"hide": ["edit.fade"]})).unwrap();
        assert!(!crate::menus::menu_items(&app).iter().any(|i| i.id == "edit.fade"));
    }

    #[test]
    fn autosave_runs_for_dirty_documents() {
        type Saved = Arc<Mutex<Vec<(u64, u64)>>>;
        let saved: Saved = Arc::default();
        let s2 = saved.clone();
        let services = crate::Services {
            autosave: Some(Box::new(move |doc: &Arc<photocraft_doc::Document>, rev: u64, _path: Option<&str>| {
                s2.lock().unwrap().push((doc.id.0, rev));
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        tick(&mut app, &ctx);
        autosave_now(&mut app);
        tick(&mut app, &ctx);
        assert!(saved.lock().unwrap().is_empty(), "clean documents are not autosaved");
        app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
        autosave_now(&mut app);
        tick(&mut app, &ctx);
        assert_eq!(saved.lock().unwrap().len(), 1);
        autosave_now(&mut app);
        tick(&mut app, &ctx);
        assert_eq!(saved.lock().unwrap().len(), 1, "unchanged since the last autosave");

        // Loaded copies can have the same persisted identity and dirty revision.
        let original = app.session.active().unwrap().doc.clone();
        let original_id = original.id;
        app.session.add_document(original.as_ref().clone(), None);
        app.run("edit.fill", json!({"color": "#0000ff"})).unwrap();
        autosave_now(&mut app);
        tick(&mut app, &ctx);
        {
            let saved = saved.lock().unwrap();
            assert_eq!(saved.len(), 2, "both dirty copies need independent autosaves");
            assert_eq!(saved[0].0, original_id.0);
            assert_ne!(saved[0].0, saved[1].0, "recovery ownership must be distinct");
            assert_eq!(saved[0].1, saved[1].1, "the revisions must match to reproduce suppression");
        }
        app.run("prefs.set", json!({"path": "fileHandling.autosave", "value": false})).unwrap();
        app.run("edit.fill", json!({"color": "#00ff00"})).unwrap();
        autosave_now(&mut app);
        tick(&mut app, &ctx);
        assert_eq!(saved.lock().unwrap().len(), 2, "autosave off");
    }

    #[test]
    fn edit_dialogs_open() {
        let (mut app, _) = app_with_store();
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        app.sync_views();
        let d = crate::menus::invoke(&mut app, &ctx, "edit.colorSettings", json!({})).unwrap()["dialog"].as_u64().unwrap();
        assert_eq!(app.ui.dialogs.iter().find(|x| x.id == d).unwrap().fields["workingRgb"], "srgb");
        assert!(crate::menus::invoke(&mut app, &ctx, "edit.fade", json!({})).is_err());
        app.run("select.rect", json!({"x": 4, "y": 4, "width": 8, "height": 8})).unwrap();
        let d = crate::menus::invoke(&mut app, &ctx, "edit.contentAwareFill", json!({})).unwrap()["dialog"].as_u64().unwrap();
        assert_eq!(app.ui.dialogs.iter().find(|x| x.id == d).unwrap().fields["__preview"], true);
        crate::dialogs::confirm(&mut app, d).unwrap();
        let d = crate::menus::invoke(&mut app, &ctx, "edit.contentAwareScale", json!({})).unwrap()["dialog"].as_u64().unwrap();
        assert_eq!(app.ui.dialogs.iter().find(|x| x.id == d).unwrap().fields["width"], 8);
        let d = crate::menus::invoke(&mut app, &ctx, "edit.presets.presetManager", json!({})).unwrap()["dialog"].as_u64().unwrap();
        assert!(crate::dialogs::confirm(&mut app, d).is_ok());
    }
}

//! Shell-only Window and View items: Window › Workspace › New / Delete / Lock Workspace (saved
//! in the preferences), Window › Modifier Keys (sticky Shift/⌘/⌥ for touch and pen users),
//! View › Pixel Aspect Ratio › Custom Pixel Aspect Ratio…, View › Show › Show Extras Options…
//! and the View › 32-bit Preview Options dialog.
//!
//! State is [`ShellUi`] (serialisable, part of `UiState`); saved workspaces and the lock live in
//! `Preferences::workspaces` / `workspace_locked` so they persist with the preferences.

use egui::{Align2, RichText, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// A View › Pixel Aspect Ratio › Custom entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomPar {
    pub name: String,
    pub ratio: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellUi {
    /// Window › Modifier Keys panel open, and its latched keys.
    pub modifier_keys: bool,
    pub sticky_shift: bool,
    pub sticky_command: bool,
    pub sticky_alt: bool,
    /// Custom pixel aspect ratios (the active one is `ViewOptions::pixel_aspect` = `custom:<ratio>:<name>`).
    pub custom_pars: Vec<CustomPar>,
    /// Open dialog: (kind, fields). Kinds: newWorkspace, deleteWorkspace, customPar, preview32,
    /// extrasOptions.
    pub dialog: Option<(String, Map<String, Value>)>,
}

const IDS: [&str; 7] = [
    "window.panel.modifierKeys",
    "window.workspace.newWorkspace",
    "window.workspace.deleteWorkspace",
    "window.workspace.lockWorkspace",
    "window.workspace.select",
    "view.pixelAspectRatio.custom",
    "view.show.showExtrasOptions",
];

/// Menu ids handled here (live menu items).
pub fn handles(id: &str) -> bool {
    IDS.contains(&id)
}

pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    match id {
        "window.panel.modifierKeys" => Some(app.ui.shell.modifier_keys),
        "window.workspace.lockWorkspace" => Some(app.session.prefs().workspace_locked),
        "view.pixelAspectRatio.custom" => Some(app.ui.view.pixel_aspect.starts_with("custom:")),
        _ => None,
    }
}

pub fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    Some(match id {
        "window.workspace.deleteWorkspace" => !app.session.prefs().workspaces.is_empty(),
        "view.thirtyTwoBitPreviewOptions" => app.session.is_enabled(id),
        i if handles(i) => true,
        _ => return None,
    })
}

/// Latched Modifier Keys panel keys added to the real ones.
pub fn sticky_mods(app: &PhotocraftApp, mut m: egui::Modifiers) -> egui::Modifiers {
    let s = &app.ui.shell;
    m.shift |= s.sticky_shift;
    m.alt |= s.sticky_alt;
    if s.sticky_command {
        m.command = true;
        if cfg!(target_os = "macos") {
            m.mac_cmd = true;
        } else {
            m.ctrl = true;
        }
    }
    m
}

fn no_params(p: &Value) -> bool {
    p.as_object().is_none_or(|o| o.is_empty())
}

fn fields(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn open(app: &mut PhotocraftApp, kind: &str, f: Value) -> Result<Value, String> {
    app.ui.shell.dialog = Some((kind.to_string(), fields(f)));
    Ok(json!({"dialog": kind}))
}

/// Built-in workspace names (Window › Workspace presets).
const PRESETS: [&str; 6] = ["Essentials", "Photography", "Painting", "Pixel Art", "Graphic and Web", "Motion"];

/// Run a shell command, or open its dialog. `None` when `id` isn't ours.
pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "view.thirtyTwoBitPreviewOptions" && no_params(params) {
        let d = app.session.active()?;
        let h = app.session.color.hdr.get(&d.doc.id).copied().unwrap_or_default();
        return Some(open(
            app,
            "preview32",
            json!({"method": if h.highlight_compression { "highlightCompression" } else { "exposureGamma" }, "exposure": h.exposure, "gamma": h.gamma}),
        ));
    }
    if !handles(id) {
        return None;
    }
    let s = &mut app.ui.shell;
    Some(match id {
        "window.panel.modifierKeys" => {
            s.modifier_keys = params.get("show").and_then(Value::as_bool).unwrap_or(!s.modifier_keys);
            for (k, slot) in [("shift", &mut s.sticky_shift), ("command", &mut s.sticky_command), ("alt", &mut s.sticky_alt)] {
                if let Some(v) = params.get(k).and_then(Value::as_bool) {
                    *slot = v;
                }
            }
            Ok(json!({"visible": s.modifier_keys, "shift": s.sticky_shift, "command": s.sticky_command, "alt": s.sticky_alt}))
        }
        "window.workspace.lockWorkspace" => {
            let on = params.get("on").and_then(Value::as_bool).unwrap_or(!app.session.prefs().workspace_locked);
            app.session.prefs.edit(|p| p.workspace_locked = on);
            Ok(json!({"locked": on}))
        }
        "window.workspace.newWorkspace" if no_params(params) => {
            open(app, "newWorkspace", json!({"name": "", "keyboardShortcuts": false, "menus": false, "toolbar": false}))
        }
        "window.workspace.newWorkspace" => new_workspace(app, params),
        "window.workspace.deleteWorkspace" if no_params(params) => {
            let first = app.session.prefs().workspaces.keys().next().cloned().unwrap_or_default();
            open(app, "deleteWorkspace", json!({"name": first}))
        }
        "window.workspace.deleteWorkspace" => delete_workspace(app, params),
        "window.workspace.select" => select_workspace(app, params),
        "view.pixelAspectRatio.custom" if no_params(params) => open(app, "customPar", json!({"name": "", "ratio": 1.0})),
        "view.pixelAspectRatio.custom" => custom_par(app, params),
        "view.show.showExtrasOptions" if no_params(params) => {
            let show = serde_json::to_value(app.ui.view.show).unwrap_or_default();
            open(app, "extrasOptions", show)
        }
        "view.show.showExtrasOptions" => {
            let mut show = serde_json::to_value(app.ui.view.show).unwrap_or_default();
            if let (Some(o), Some(p)) = (show.as_object_mut(), params.as_object()) {
                for (k, v) in p {
                    if !o.contains_key(k) || !v.is_boolean() {
                        return Some(Err(format!("unknown Show item `{k}` ({})", o.keys().cloned().collect::<Vec<_>>().join("|"))));
                    }
                    o.insert(k.clone(), v.clone());
                }
            }
            app.ui.view.show = serde_json::from_value(show.clone()).map_err(|e| e.to_string()).ok()?;
            Ok(show)
        }
        _ => return None,
    })
}

/// Window › Workspace › New Workspace…: save the panel layout (and optionally keyboard
/// shortcuts, menus and toolbar) under a name, and make it current.
fn new_workspace(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let name = p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    if name.is_empty() {
        return Err("a workspace needs a name".into());
    }
    if PRESETS.contains(&name.as_str()) {
        return Err(format!("\"{name}\" is a built-in workspace"));
    }
    let prefs = app.session.prefs().clone();
    let mut ws = json!({"panels": app.ui.panels, "dockTabs": app.ui.dock_tabs, "dock": app.ui.dock});
    if p.get("keyboardShortcuts").and_then(Value::as_bool) == Some(true) {
        ws["shortcuts"] = json!(prefs.shortcuts);
    }
    if p.get("menus").and_then(Value::as_bool) == Some(true) {
        ws["menus"] = serde_json::to_value(&prefs.menus).unwrap_or_default();
    }
    if p.get("toolbar").and_then(Value::as_bool) == Some(true) {
        ws["toolbar"] = serde_json::to_value(&prefs.toolbar).unwrap_or_default();
    }
    let replaced = prefs.workspaces.contains_key(&name);
    app.session.prefs.edit(|pr| pr.workspaces.insert(name.clone(), ws));
    app.ui.workspace = name.clone();
    Ok(json!({"workspace": name, "replaced": replaced}))
}

fn delete_workspace(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let name = p.get("name").and_then(Value::as_str).ok_or("pass `name`")?.to_string();
    if !app.session.prefs().workspaces.contains_key(&name) {
        return Err(format!("no saved workspace \"{name}\""));
    }
    // Like Photoshop, the current workspace can't be deleted.
    if app.ui.workspace == name {
        return Err(format!("\"{name}\" is the current workspace; switch to another first"));
    }
    app.session.prefs.edit(|pr| pr.workspaces.remove(&name));
    Ok(json!({"deleted": name}))
}

fn select_workspace(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let name = p.get("name").and_then(Value::as_str).ok_or("pass `name`")?.to_string();
    if PRESETS.contains(&name.as_str()) {
        app.ui.workspace = name.clone();
        crate::menus::apply_workspace(app);
        return Ok(json!({"workspace": name}));
    }
    if !app.session.prefs().workspaces.contains_key(&name) {
        return Err(format!("no workspace \"{name}\""));
    }
    app.ui.workspace = name.clone();
    apply_custom(app);
    Ok(json!({"workspace": name}))
}

/// Apply the current workspace when it is a saved one. Returns false for the built-in presets.
pub fn apply_custom(app: &mut PhotocraftApp) -> bool {
    let Some(ws) = app.session.prefs().workspaces.get(&app.ui.workspace).cloned() else { return false };
    // Workspaces saved before the dock layout existed get the default heights.
    app.ui.dock = Default::default();
    crate::dock::apply(app, &ws);
    let (sc, menus, toolbar) = (ws.get("shortcuts").cloned(), ws.get("menus").cloned(), ws.get("toolbar").cloned());
    if sc.is_some() || menus.is_some() || toolbar.is_some() {
        app.session.prefs.edit(|p| {
            if let Some(v) = sc.and_then(|v| serde_json::from_value(v).ok()) {
                p.shortcuts = v;
            }
            if let Some(v) = menus.and_then(|v| serde_json::from_value(v).ok()) {
                p.menus = v;
            }
            if let Some(v) = toolbar.and_then(|v| serde_json::from_value(v).ok()) {
                p.toolbar = v;
            }
        });
    }
    true
}

/// Custom Pixel Aspect Ratio: `{name, ratio}` adds (or updates) and selects; `{delete: name}`
/// removes; `{select: name}` selects a saved one.
fn custom_par(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let s = &mut app.ui.shell;
    if let Some(n) = p.get("delete").and_then(Value::as_str) {
        let before = s.custom_pars.len();
        s.custom_pars.retain(|c| c.name != n);
        if s.custom_pars.len() == before {
            return Err(format!("no custom pixel aspect ratio \"{n}\""));
        }
        if app.ui.view.pixel_aspect.ends_with(&format!(":{n}")) {
            app.ui.view.pixel_aspect = "square".into();
        }
        return Ok(json!({"deleted": n}));
    }
    let c = if let Some(n) = p.get("select").and_then(Value::as_str) {
        s.custom_pars.iter().find(|c| c.name == n).cloned().ok_or_else(|| format!("no custom pixel aspect ratio \"{n}\""))?
    } else {
        let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or("pass `name`")?.to_string();
        let ratio = p.get("ratio").and_then(Value::as_f64).ok_or("pass `ratio`")? as f32;
        if !(0.1..=10.0).contains(&ratio) {
            return Err("the ratio must be between 0.100 and 10.000".into());
        }
        let c = CustomPar { name, ratio };
        match s.custom_pars.iter_mut().find(|x| x.name == c.name) {
            Some(x) => *x = c.clone(),
            None => s.custom_pars.push(c.clone()),
        }
        c
    };
    app.ui.view.pixel_aspect = format!("custom:{}:{}", c.ratio, c.name);
    Ok(json!({"pixelAspectRatio": c.ratio, "name": c.name}))
}

/// The open dialog's OK as (command id, params).
pub fn dialog_command(kind: &str, f: &Map<String, Value>) -> Option<(&'static str, Value)> {
    Some(match kind {
        "newWorkspace" => ("window.workspace.newWorkspace", Value::Object(f.clone())),
        "deleteWorkspace" => ("window.workspace.deleteWorkspace", json!({"name": f.get("name")})),
        "customPar" => ("view.pixelAspectRatio.custom", json!({"name": f.get("name"), "ratio": f.get("ratio")})),
        "preview32" => ("view.thirtyTwoBitPreviewOptions", Value::Object(f.clone())),
        "extrasOptions" => ("view.show.showExtrasOptions", Value::Object(f.clone())),
        _ => return None,
    })
}

/// Draws the Modifier Keys panel and the open dialog.
pub fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.shell.modifier_keys {
        let mut close = false;
        let mut s = app.ui.shell.clone();
        let cmd = crate::shortcuts::pretty("Cmd");
        let shift = crate::shortcuts::pretty("Shift");
        let alt = crate::shortcuts::pretty("Alt");
        crate::analysis_ui::panel_window(app, ctx, "modifier-keys", tl!("Modifier Keys"), vec2(-420.0, 80.0), 150.0, |ui| {
            close = crate::analysis_ui::title_row(ui, tl!("Modifier Keys"));
            ui.horizontal(|ui| {
                for (label, on) in [(&*shift, &mut s.sticky_shift), (&*cmd, &mut s.sticky_command), (&*alt, &mut s.sticky_alt)] {
                    if ui.add(egui::Button::new(RichText::new(label).size(14.0)).selected(*on).min_size(vec2(40.0, 30.0))).clicked() {
                        *on = !*on;
                    }
                }
            });
        });
        s.modifier_keys = !close;
        app.ui.shell = s;
    }
    if app.ui.shell.dialog.is_some() {
        dialog(app, ctx);
    }
}

fn dialog(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((kind, mut f)) = app.ui.shell.dialog.clone() else { return };
    let t = Tokens::get(ctx);
    let title = match kind.as_str() {
        "newWorkspace" => "New Workspace",
        "deleteWorkspace" => "Delete Workspace",
        "customPar" => "Save Pixel Aspect Ratio",
        "preview32" => "32-bit Preview Options",
        _ => "Show Extras Options",
    };
    let names: Vec<String> = app.session.prefs().workspaces.keys().cloned().collect();
    let mut result: Option<bool> = None;
    egui::Window::new(title).id(egui::Id::new("shell-dialog")).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(
        ctx,
        |ui| {
            ui.label(RichText::new(tl!(&title)).color(t.text).size(13.0).strong());
            crate::widgets::hairline(ui);
            ui.add_space(6.0);
            let text = |ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim).size(11.0));
                    let mut v = f.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(180.0)).changed() {
                        f.insert(key.into(), json!(v));
                    }
                });
            };
            let check = |ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str| {
                let mut on = f.get(key).and_then(Value::as_bool).unwrap_or(false);
                if crate::widgets::checkbox(ui, &mut on, label).changed() {
                    f.insert(key.into(), json!(on));
                }
            };
            let number = |ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: std::ops::RangeInclusive<f32>| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tl!(&label)).color(t.text_dim).size(11.0));
                    let mut v = f.get(key).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    if crate::widgets::slider(ui, &mut v, range.clone(), None).changed() | crate::widgets::value_field(ui, &mut v, range, "", 60.0).changed() {
                        f.insert(key.into(), json!(v));
                    }
                });
            };
            match kind.as_str() {
                "newWorkspace" => {
                    text(ui, &mut f, "name", tl!("Name:"));
                    ui.label(RichText::new(tl!("Capture")).color(t.text_dim).size(11.0));
                    ui.label(
                        RichText::new(tl!("Panel locations are saved in the workspace. Keyboard shortcuts, menus and toolbar are optional."))
                            .color(t.text_faint)
                            .size(10.5),
                    );
                    check(ui, &mut f, "keyboardShortcuts", tl!("Keyboard Shortcuts"));
                    check(ui, &mut f, "menus", tl!("Menus"));
                    check(ui, &mut f, "toolbar", tl!("Toolbar"));
                }
                "deleteWorkspace" => {
                    let mut cur = f.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let opts: Vec<(String, &str)> = names.iter().map(|n| (n.clone(), n.as_str())).collect();
                    if !opts.is_empty() && crate::widgets::dropdown(ui, "ws-delete", &mut cur, &opts, 180.0) {
                        f.insert("name".into(), json!(cur));
                    }
                }
                "customPar" => {
                    text(ui, &mut f, "name", tl!("Name:"));
                    number(ui, &mut f, "ratio", tl!("Factor:"), 0.1..=10.0);
                }
                "preview32" => {
                    let mut m = f.get("method").and_then(Value::as_str).unwrap_or("exposureGamma").to_string();
                    if crate::widgets::dropdown(
                        ui,
                        "p32-method",
                        &mut m,
                        &[("exposureGamma".to_string(), tl!("Exposure and Gamma")), ("highlightCompression".to_string(), tl!("Highlight Compression"))],
                        180.0,
                    ) {
                        f.insert("method".into(), json!(m));
                    }
                    ui.add_enabled_ui(m == "exposureGamma", |ui| {
                        number(ui, &mut f, "exposure", tl!("Exposure:"), -20.0..=20.0);
                        number(ui, &mut f, "gamma", tl!("Gamma:"), 0.1..=9.99);
                    });
                }
                _ => {
                    for (k, label) in [
                        ("layerEdges", tl!("Layer Edges")),
                        ("selectionEdges", tl!("Selection Edges")),
                        ("targetPath", tl!("Target Path")),
                        ("notes", tl!("Notes")),
                        ("pixelGrid", tl!("Pixel Grid")),
                        ("slices", tl!("Slices")),
                        ("count", tl!("Count")),
                        ("smartGuides", tl!("Smart Guides")),
                        ("brushPreview", tl!("Brush Preview")),
                        ("mesh", tl!("Mesh")),
                        ("editPins", tl!("Edit Pins")),
                        ("canvasGuides", tl!("Canvas Guides")),
                        ("artboardGuides", tl!("Artboard Guides")),
                    ] {
                        check(ui, &mut f, k, label);
                    }
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ok = if kind == "deleteWorkspace" { tl!("Delete") } else { tl!("OK") };
                    if crate::widgets::primary_button(ui, ok, 70.0).clicked() {
                        result = Some(true);
                    }
                    if crate::widgets::secondary_button(ui, tl!("Cancel"), 70.0).clicked() {
                        result = Some(false);
                    }
                });
            });
        },
    );
    match result {
        Some(true) => {
            if let Some((id, p)) = dialog_command(&kind, &f) {
                match crate::menus::invoke(app, ctx, id, p) {
                    Ok(_) => app.ui.shell.dialog = None,
                    Err(e) => {
                        app.ui.status = e;
                        app.ui.status_error = true;
                        app.ui.shell.dialog = Some((kind, f));
                    }
                }
            }
        }
        Some(false) => app.ui.shell.dialog = None,
        None => app.ui.shell.dialog = Some((kind, f)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tool;

    fn app() -> (PhotocraftApp, egui::Context) {
        (PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default()), egui::Context::default())
    }

    #[test]
    fn new_select_delete_workspaces() {
        let (mut app, ctx) = app();
        let inv = |app: &mut PhotocraftApp, id: &str, p: Value| crate::menus::invoke(app, &ctx, id, p);
        assert!(!crate::menus::is_enabled(&app, "window.workspace.deleteWorkspace"));
        assert_eq!(inv(&mut app, "window.workspace.newWorkspace", json!({})).unwrap()["dialog"], "newWorkspace");
        app.ui.panels.history = true;
        app.ui.panels.navigator = true;
        app.session.prefs.edit(|p| {
            p.shortcuts.insert("filter.blur.gaussian".into(), "Cmd+Alt+G".into());
        });
        inv(&mut app, "window.workspace.newWorkspace", json!({"name": "Retouch", "keyboardShortcuts": true})).unwrap();
        assert_eq!(app.ui.workspace, "Retouch");
        assert!(inv(&mut app, "window.workspace.newWorkspace", json!({"name": "Painting"})).is_err());
        assert!(inv(&mut app, "window.workspace.newWorkspace", json!({"name": " "})).is_err());
        // The current workspace can't be deleted.
        assert!(inv(&mut app, "window.workspace.deleteWorkspace", json!({"name": "Retouch"})).is_err());
        inv(&mut app, "window.workspace.painting", json!({})).unwrap();
        assert!(!app.ui.panels.history);
        app.session.prefs.edit(|p| p.shortcuts.clear());
        // Switching back restores the panels and the captured shortcuts.
        inv(&mut app, "window.workspace.select", json!({"name": "Retouch"})).unwrap();
        assert!(app.ui.panels.history && app.ui.panels.navigator);
        assert_eq!(app.session.prefs().shortcuts.get("filter.blur.gaussian").map(String::as_str), Some("Cmd+Alt+G"));
        // Reset Workspace re-applies the saved layout.
        app.ui.panels.history = false;
        inv(&mut app, "window.workspace.resetWorkspace", json!({})).unwrap();
        assert!(app.ui.panels.history);
        inv(&mut app, "window.workspace.essentials", json!({})).unwrap();
        assert!(crate::menus::is_enabled(&app, "window.workspace.deleteWorkspace"));
        inv(&mut app, "window.workspace.deleteWorkspace", json!({"name": "Retouch"})).unwrap();
        assert!(app.session.prefs().workspaces.is_empty());
        // Saved workspaces persist with the preferences.
        inv(&mut app, "window.workspace.newWorkspace", json!({"name": "Mine"})).unwrap();
        let json = app.session.prefs_to_json();
        let mut s2 = photocraft_engine::Session::new();
        s2.load_prefs_json(&json).unwrap();
        assert!(s2.prefs().workspaces.contains_key("Mine"));
    }

    #[test]
    fn lock_workspace_toggles_and_persists() {
        let (mut app, ctx) = app();
        crate::menus::invoke(&mut app, &ctx, "window.workspace.lockWorkspace", json!({})).unwrap();
        assert!(app.session.prefs().workspace_locked);
        assert_eq!(checked(&app, "window.workspace.lockWorkspace"), Some(true));
        let items = crate::menus::menu_items(&app);
        assert!(items.iter().any(|i| i.id == "window.workspace.lockWorkspace" && i.checked == Some(true)));
        crate::menus::invoke(&mut app, &ctx, "window.workspace.lockWorkspace", json!({"on": false})).unwrap();
        assert!(!app.session.prefs().workspace_locked);
    }

    #[test]
    fn modifier_keys_latch_into_tool_events() {
        let (mut app, ctx) = app();
        crate::menus::invoke(&mut app, &ctx, "window.panel.modifierKeys", json!({"alt": true})).unwrap();
        assert!(app.ui.shell.modifier_keys);
        let m = sticky_mods(&app, egui::Modifiers::NONE);
        assert!(m.alt && !m.shift);
        // ⌥ latched: a Count-tool click on a marker removes it.
        app.run("file.new", json!({"width": 50, "height": 50})).unwrap();
        app.run("count.add", json!({"x": 10, "y": 10})).unwrap();
        app.ui.tool = Tool::Count;
        crate::canvas::tool_event(&mut app, crate::canvas::ToolEvent::Down { x: 10.0, y: 10.0, pressure: 1.0 }, egui::Modifiers::NONE);
        crate::canvas::tool_event(&mut app, crate::canvas::ToolEvent::Up { x: 10.0, y: 10.0 }, egui::Modifiers::NONE);
        assert_eq!(app.session.active().unwrap().doc.measurement.count_total(), 0);
        crate::analysis_ui::tests::render(&mut app, &ctx, windows);
    }

    #[test]
    fn custom_pixel_aspect_ratio_and_extras_options() {
        let (mut app, ctx) = app();
        let inv = |app: &mut PhotocraftApp, id: &str, p: Value| crate::menus::invoke(app, &ctx, id, p);
        assert_eq!(inv(&mut app, "view.pixelAspectRatio.custom", json!({})).unwrap()["dialog"], "customPar");
        inv(&mut app, "view.pixelAspectRatio.custom", json!({"name": "Cinema", "ratio": 1.25})).unwrap();
        assert_eq!(crate::view_cmds::pixel_aspect_ratio(&app.ui.view.pixel_aspect), 1.25);
        assert_eq!(checked(&app, "view.pixelAspectRatio.custom"), Some(true));
        assert!(inv(&mut app, "view.pixelAspectRatio.custom", json!({"name": "x", "ratio": 50})).is_err());
        inv(&mut app, "view.pixelAspectRatio.square", json!({})).unwrap();
        inv(&mut app, "view.pixelAspectRatio.custom", json!({"select": "Cinema"})).unwrap();
        assert_eq!(crate::view_cmds::pixel_aspect_ratio(&app.ui.view.pixel_aspect), 1.25);
        inv(&mut app, "view.pixelAspectRatio.custom", json!({"delete": "Cinema"})).unwrap();
        assert_eq!(app.ui.view.pixel_aspect, "square");
        // Show Extras Options.
        assert_eq!(inv(&mut app, "view.show.showExtrasOptions", json!({})).unwrap()["dialog"], "extrasOptions");
        let (kind, mut f) = app.ui.shell.dialog.clone().unwrap();
        f.insert("count".into(), json!(false));
        let (id, p) = dialog_command(&kind, &f).unwrap();
        inv(&mut app, id, p).unwrap();
        assert!(!app.ui.view.show.count);
        assert!(inv(&mut app, "view.show.showExtrasOptions", json!({"bogus": true})).is_err());
        crate::analysis_ui::tests::render(&mut app, &ctx, windows);
    }

    #[test]
    fn thirty_two_bit_preview_dialog() {
        let (mut app, ctx) = app();
        app.run("file.new", json!({"width": 20, "height": 20, "depth": 32})).unwrap();
        let r = crate::menus::invoke(&mut app, &ctx, "view.thirtyTwoBitPreviewOptions", json!({})).unwrap();
        assert_eq!(r["dialog"], "preview32");
        let (kind, mut f) = app.ui.shell.dialog.clone().unwrap();
        f.insert("exposure".into(), json!(1.5));
        let (id, p) = dialog_command(&kind, &f).unwrap();
        crate::menus::invoke(&mut app, &ctx, id, p).unwrap();
        let d = app.session.active().unwrap().doc.id;
        assert_eq!(app.session.color.hdr[&d].exposure, 1.5);
    }
}

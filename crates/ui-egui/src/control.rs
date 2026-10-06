//! Programmatic control of the running app, for agents, tests and (soon) MCP.
//!
//! Transport-agnostic: a transport thread (TCP in the desktop app, a channel in tests) sends
//! [`ControlRequest`]s; the UI thread handles them between frames and replies with JSON.
//!
//! Methods:
//! - `engine.execute {command, params}`: run any engine or UI command by id
//! - `engine.commands`: list commands with enablement
//! - `ui.inspect`: full UI state (tool, panels, views, dialogs, windows, menu tree, window size)
//! - `ui.set {tool?, panels?, zoom?, center?, dark?}`: change UI state
//! - `ui.menu.invoke {id}` / `ui.menu.list`: activate a menu item by id; list the menu tree
//! - `ui.dialog.open {kind, fields?}` (kinds: newDocument, about, layerStyle {effect?}, colorPicker {target: foreground|background}, command {command}) / `ui.dialog.set {dialog, field, value}` / `ui.dialog.confirm {dialog}` / `ui.dialog.cancel {dialog}`
//! - `ui.dialog.apply {dialog}`: commit Preferences changes without closing the dialog
//! - `ui.window.open {document?}` / `ui.window.close {window}`: extra document windows
//! - `ui.pointer {events: [{kind: down|move|up, x, y, pressure?, tiltX?, tiltY?, rotation?}], modifiers?, button?}`: drive the active tool in document coordinates (`button: "secondary"` = the right button: opens the Brush Preset picker, or erases with Preferences › Tools › Right-click with painting tools = erase)
//! - `ui.click {x, y, button?, count?}` / `ui.move {x, y}`: synthetic pointer input in screen points
//! - `ui.key {key, command?, shift?, alt?, ctrl?}` / `ui.type {text}`: synthetic keyboard input
//! - `ui.resize {width, height}`: resize the main window
//! - `ui.gpu.simulateLoss {error?}`: act as if the wgpu device was lost (or, with `error: true`,
//!   reported an error): the app switches to the CPU renderer for the rest of the session, as on a
//!   real loss. For testing the fallback; returns whether a GPU canvas was active
//! - `ui.screenshot {path?, focus?}`: capture the main window (PNG). Raises the window first (default)
//!   because occluded macOS windows stop rendering
//! - `ui.focus`: bring the main window to the front
//! - `app.open {path}` / `app.save {path}`: relative file I/O under the automation roots; reply with `warnings`
//! - `app.quit`
//! - `jobs.list` / `jobs.cancel {job?}`: background jobs (#210) with progress; cancel one (or all).
//!   `engine.execute` waits for a command that runs as a job unless `wait: false` (then the reply
//!   is `{job, pending: true}`)

use std::sync::mpsc::Sender;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, tool_event};
use crate::state::{DialogKind, Tool, UiState};

pub type ControlResponse = Value;

pub struct ControlRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<ControlResponse>,
}

impl ControlRequest {
    pub fn new(method: impl Into<String>, params: Value) -> (Self, std::sync::mpsc::Receiver<ControlResponse>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { method: method.into(), params, reply: tx }, rx)
    }
}

pub enum Outcome {
    Done(Value),
    Screenshot {
        token: u64,
        path: Option<String>,
    },
    /// Synthetic input queued: reply once the app has processed all of it (so a following
    /// `ui.inspect`/`engine.execute` observes the effect).
    AfterInput,
    /// A command started a background job and the caller waits for it: reply with its result
    /// once it has been applied (or with its error / "cancelled").
    AfterJob(photocraft_engine::jobs::JobId),
}

fn ok(v: Value) -> Outcome {
    Outcome::Done(json!({"ok": true, "result": v}))
}
fn err(e: impl std::fmt::Display) -> Outcome {
    Outcome::Done(json!({"ok": false, "error": e.to_string()}))
}
fn wrap(r: Result<Value, String>) -> Outcome {
    match r {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

pub fn handle(app: &mut PhotocraftApp, ctx: &egui::Context, req: &ControlRequest) -> Outcome {
    let p = &req.params;
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    let u = |k: &str| p.get(k).and_then(Value::as_u64);
    match req.method.as_str() {
        "engine.execute" | "ui.menu.invoke" => {
            let Some(id) = s("command").or(s("id")) else { return err("missing `command`") };
            let params = p.get("params").cloned().unwrap_or(json!({}));
            if let Some(authorize) = app.services.automation_command.as_ref()
                && let Err(error) = authorize(id, &params)
            {
                return err(error);
            }
            // `engine.execute` is programmatic: engine commands run directly with their default
            // params and never open a dialog (an agent would otherwise get a modal instead of a
            // result). `ui.menu.invoke` behaves like a menu click, so it may open the dialog.
            if req.method == "engine.execute" && photocraft_engine::commands::find(id).is_some() {
                // Long commands may run as background jobs: by default the reply waits for the
                // result (backward compatible); with `"wait": false` it is `{job, pending}`.
                let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(true);
                app.jobs.last_started = None;
                let r = app.run_automation(id, params);
                if let (Ok(_), Some(job)) = (&r, app.jobs.last_started.take())
                    && wait
                {
                    return Outcome::AfterJob(job);
                }
                return wrap(r);
            }
            let events_enabled = app.session.prefs().script_events.enabled;
            if events_enabled {
                app.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
            }
            let result = crate::menus::invoke(app, ctx, id, params);
            if events_enabled {
                app.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
            }
            wrap(result)
        }
        "engine.commands" => wrap(app.run("command.list", json!({}))),
        // Background jobs (#210): running ones with progress, then the last few that ended.
        "jobs.list" => wrap(app.session.execute("jobs.list", json!({})).map_err(|e| e.to_string())),
        "jobs.cancel" => {
            let all = p.get("job").is_none_or(Value::is_null);
            match p.get("job").and_then(Value::as_u64) {
                Some(j) if app.session.job(photocraft_engine::jobs::JobId(j)).is_some() => {
                    crate::jobs_ui::cancel(app, photocraft_engine::jobs::JobId(j));
                    ok(json!({"cancelled": 1}))
                }
                Some(j) => err(format!("no running job {j}")),
                None if all => {
                    let ids: Vec<_> = app.session.jobs().into_iter().map(|j| j.id).collect();
                    for j in &ids {
                        crate::jobs_ui::cancel(app, *j);
                    }
                    ok(json!({"cancelled": ids.len()}))
                }
                None => err("`job` must be a job id"),
            }
        }
        "ui.menu.list" => ok(serde_json::to_value(crate::menus::menu_items(app)).unwrap_or_default()),
        "ui.inspect" => ok(inspect(app, ctx)),
        "ui.set" => {
            if let Some(t) = s("tool") {
                match Tool::from_name(t) {
                    Some(t) => app.ui.tool = t,
                    None => return err(format!("unknown tool `{t}`")),
                }
            }
            if let Some(panels) = p.get("panels") {
                let mut cur = serde_json::to_value(&app.ui.panels).unwrap_or_default();
                if let (Some(c), Some(n)) = (cur.as_object_mut(), panels.as_object()) {
                    for (k, v) in n {
                        c.insert(k.clone(), v.clone());
                    }
                }
                match serde_json::from_value(cur) {
                    Ok(v) => app.ui.panels = v,
                    Err(e) => return err(e),
                }
            }
            if let Some(m) = p.get("maskTarget").and_then(Value::as_bool) {
                app.ui.mask_target = m;
                app.ui.vector_mask_target &= !m;
            }
            if let Some(m) = p.get("vectorMaskTarget").and_then(Value::as_bool) {
                app.ui.vector_mask_target = m;
                app.ui.mask_target &= !m;
            }
            // Selection tools' options-bar mode: 0 New, 1 Add, 2 Subtract, 3 Intersect.
            if let Some(m) = p.get("selectionMode").and_then(Value::as_u64) {
                app.ui.selection_mode = m.min(3) as u8;
            }
            if let Some(tabs) = p.get("dockTabs") {
                let mut cur = serde_json::to_value(app.ui.dock_tabs).unwrap_or_default();
                if let (Some(c), Some(n)) = (cur.as_object_mut(), tabs.as_object()) {
                    for (k, v) in n {
                        c.insert(k.clone(), v.clone());
                    }
                }
                match serde_json::from_value(cur) {
                    Ok(v) => app.ui.dock_tabs = v,
                    Err(e) => return err(e),
                }
            }
            // Dock group order, heights and collapsed groups (see `dock::DockLayout`).
            if let Some(d) = p.get("dock") {
                match serde_json::from_value(d.clone()) {
                    Ok(v) => app.ui.dock = v,
                    Err(e) => return err(e),
                }
            }
            // Right dock width in points (clamped to the dock's 250..=520 range), applied next frame.
            if let Some(w) = p.get("dockWidth").and_then(Value::as_f64) {
                crate::panels::request_dock_width(ctx, w as f32);
            }
            if let Some(i) = app.session.active_index() {
                if let Some(z) = p.get("zoom").and_then(Value::as_f64) {
                    app.ui.views[i].zoom = (z as f32).clamp(0.01, 64.0);
                    app.ui.views[i].fit_pending = false;
                }
                if let Some(c) = p.get("center").and_then(Value::as_array)
                    && c.len() == 2
                {
                    app.ui.views[i].center = [c[0].as_f64().unwrap_or(0.0) as f32, c[1].as_f64().unwrap_or(0.0) as f32];
                    app.ui.views[i].fit_pending = false;
                }
                if p.get("fit").and_then(Value::as_bool) == Some(true) {
                    app.ui.views[i].fit_pending = true;
                }
            }
            if let Some(name) = s("theme") {
                match crate::theme::ThemeKind::from_name(name) {
                    Some(k) => app.set_theme(ctx, k),
                    None => return err(format!("unknown theme `{name}` (studio, studioLight, classic)")),
                }
            }
            if let Some(i) = u("brushSection") {
                app.ui.brush_section = (i as usize).min(crate::brush_panel::SECTIONS.len() - 1);
            }
            if let Some(i) = u("brushTab") {
                app.ui.brush_tab = (i as usize).min(1);
            }
            if let Some(v) = p.get("brushesView").cloned() {
                match serde_json::from_value(v) {
                    Ok(v) => app.ui.brushes_panel.view = v,
                    Err(e) => return err(format!("brushesView: {e} (list, grid)")),
                }
            }
            if let Some(size) = p.get("brushSize").and_then(Value::as_f64) {
                app.session.tools.brush.size = size as f32;
            }
            ok(Value::Null)
        }
        "ui.dialog.open" => {
            let kind = match s("kind").unwrap_or("") {
                "newDocument" | "NewDocument" => DialogKind::NewDocument,
                "about" | "About" => DialogKind::About,
                "layerStyle" | "LayerStyle" => {
                    return match crate::layer_style::open(app, s("effect")) {
                        Some(id) => ok(json!({"dialog": id})),
                        None => err("no active layer"),
                    };
                }
                "colorPicker" | "ColorPicker" => {
                    let target = if s("target") == Some("background") { "background" } else { "foreground" };
                    return ok(json!({"dialog": crate::color_picker_ui::open(app, target)}));
                }
                "command" | "Command" => {
                    let Some(cmd) = s("command") else { return err("command dialogs need `command`") };
                    let label = photocraft_engine::commands::find(cmd).map(|c| c.label).unwrap_or(cmd);
                    return ok(json!({"dialog": crate::dialogs::open_command_dialog(app, cmd, label)}));
                }
                other => return err(format!("unknown dialog kind `{other}`")),
            };
            let mut fields = if kind == DialogKind::NewDocument { UiState::new_document_fields() } else { Default::default() };
            if let Some(f) = p.get("fields").and_then(Value::as_object) {
                fields.extend(f.clone());
            }
            ok(json!({"dialog": app.ui.open_dialog(kind, fields)}))
        }
        "ui.dialog.set" => {
            let (Some(id), Some(field)) = (u("dialog"), s("field")) else { return err("need `dialog` and `field`") };
            let value = p.get("value").cloned().unwrap_or(Value::Null);
            match app.ui.dialog_mut(id) {
                Some(d) => {
                    d.fields.insert(field.to_string(), value);
                    ok(Value::Null)
                }
                None => err(format!("no dialog {id}")),
            }
        }
        "ui.dialog.confirm" | "ui.dialog.apply" => match u("dialog") {
            Some(id) => {
                let command = app.ui.dialogs.iter().find(|dialog| dialog.id == id).and_then(|dialog| {
                    if req.method == "ui.dialog.apply" {
                        dialog.fields.get("values").map(|values| ("prefs.set".to_string(), json!({"path": "", "value": values})))
                    } else {
                        dialog.fields.get("__command").and_then(Value::as_str).map(|command| (command.to_string(), Value::Object(dialog.fields.clone())))
                    }
                });
                if let Some((command, params)) = command
                    && let Some(authorize) = app.services.automation_command.as_ref()
                    && let Err(error) = authorize(&command, &params)
                {
                    return err(error);
                }
                let events_enabled = app.session.prefs().script_events.enabled;
                if events_enabled {
                    app.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
                }
                let result = if req.method == "ui.dialog.apply" { crate::prefs_ui::apply(app, id) } else { crate::dialogs::confirm(app, id) };
                if events_enabled {
                    app.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
                }
                wrap(result)
            }
            None => err("missing `dialog`"),
        },
        "ui.dialog.cancel" => match u("dialog").and_then(|id| app.ui.close_dialog(id)) {
            Some(_) => ok(Value::Null),
            None => err("no such dialog"),
        },
        "ui.window.open" => {
            if let Some(d) = u("document") {
                app.session.set_active(d as usize);
            }
            wrap(crate::menus::invoke(app, ctx, "window.newWindowForDocument", json!({})))
        }
        "ui.window.close" => {
            let Some(id) = u("window") else { return err("missing `window`") };
            let before = app.ui.windows.len();
            app.ui.windows.retain(|w| w.id != id);
            if app.ui.windows.len() < before { ok(Value::Null) } else { err(format!("no window {id}")) }
        }
        "ui.pointer" => {
            let Some(events) = p.get("events").and_then(Value::as_array) else { return err("missing `events`") };
            // Modifier flags may be top-level or grouped under "modifiers".
            let m = p.get("modifiers").unwrap_or(p);
            let flag = |k: &str| m.get(k).and_then(Value::as_bool).unwrap_or(false);
            let mods = egui::Modifiers {
                shift: flag("shift"),
                alt: flag("alt"),
                command: flag("command"),
                mac_cmd: cfg!(target_os = "macos") && flag("command"),
                ctrl: flag("ctrl"),
            };
            if let Some(t) = s("tool").and_then(Tool::from_name) {
                app.ui.tool = t;
            }
            // Space held: the Crop tool moves the frame being drawn, a marquee, lasso or shape
            // being drawn moves instead of growing (hold_keys.rs).
            let space = flag("space");
            crate::crop_ui::set_space(app, space);
            for e in events {
                let x = e.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = e.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                let pr = e.get("pressure").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                let ev = match e.get("kind").and_then(Value::as_str).unwrap_or("move") {
                    "down" => ToolEvent::Down { x, y, pressure: pr },
                    "up" => ToolEvent::Up { x, y },
                    _ => ToolEvent::Move { x, y, pressure: pr },
                };
                // Right-click with the Move tool, or ⌘/Ctrl+right-click: list the layers there.
                if matches!(s("button"), Some("secondary" | "right")) && crate::layer_pick_ui::is_gesture(app.ui.tool, mods) {
                    if matches!(ev, ToolEvent::Down { .. }) {
                        let at = app.last_canvas_rect.center();
                        crate::layer_pick_ui::open(app, [at.x, at.y], x, y);
                    }
                    continue;
                }
                if matches!(s("button"), Some("secondary" | "right")) && !crate::paint_mouse::pointer_secondary(app, matches!(ev, ToolEvent::Down { .. }), mods)
                {
                    continue;
                }
                // A simulated pen: tilt/rotation reach the stroke like a real stylus's (see `stylus`).
                let tilt = |k: &str| e.get(k).and_then(Value::as_f64).map(|v| v as f32);
                let pen = (tilt("tiltX"), tilt("tiltY"), tilt("rotation"));
                if pen != (None, None, None) {
                    let (tilt_x, tilt_y, rotation) = (pen.0.unwrap_or(0.0), pen.1.unwrap_or(0.0), pen.2.unwrap_or(0.0));
                    app.stylus.feed.set(Some(crate::stylus::PenSample { pressure: pr, tilt_x, tilt_y, rotation, eraser: false }));
                }
                if let Some(d) = app.drag.as_mut().filter(|d| crate::hold_keys::repositions(d.tool)) {
                    d.reposition = space;
                }
                tool_event(app, ev, mods);
            }
            app.stylus.feed.set(None);
            ok(json!({"status": app.ui.status}))
        }
        "ui.click" | "ui.move" => {
            // Screen coordinates in points (as reported by ui.inspect window size).
            let x = p.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let y = p.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let pos = egui::pos2(x, y);
            let button = match s("button").unwrap_or("left") {
                "right" | "secondary" => egui::PointerButton::Secondary,
                "middle" => egui::PointerButton::Middle,
                _ => egui::PointerButton::Primary,
            };
            app.synthetic.push(egui::Event::PointerMoved(pos));
            if req.method == "ui.click" {
                let clicks = p.get("count").and_then(Value::as_u64).unwrap_or(1);
                for _ in 0..clicks {
                    app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: true, modifiers: Default::default() });
                    app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: false, modifiers: Default::default() });
                }
            }
            ctx.request_repaint();
            Outcome::AfterInput
        }
        "ui.key" => {
            let Some(name) = s("key") else { return err("missing `key`") };
            let Some(key) = egui::Key::from_name(name) else { return err(format!("unknown key `{name}`")) };
            // Modifier flags may be top-level or grouped under "modifiers".
            let m = p.get("modifiers").unwrap_or(p);
            let flag = |k: &str| m.get(k).and_then(Value::as_bool).unwrap_or(false);
            let modifiers = egui::Modifiers {
                command: flag("command"),
                mac_cmd: cfg!(target_os = "macos") && flag("command"),
                shift: flag("shift"),
                alt: flag("alt"),
                ctrl: flag("ctrl"),
            };
            app.synthetic.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
            app.synthetic.push(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
            ctx.request_repaint();
            Outcome::AfterInput
        }
        "ui.type" => {
            let Some(text) = s("text") else { return err("missing `text`") };
            app.synthetic.push(egui::Event::Text(text.to_string()));
            ctx.request_repaint();
            Outcome::AfterInput
        }
        "ui.resize" => {
            let (w, h) = (p.get("width").and_then(Value::as_f64).unwrap_or(1280.0), p.get("height").and_then(Value::as_f64).unwrap_or(800.0));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w as f32, h as f32)));
            ok(Value::Null)
        }
        "ui.gpu.simulateLoss" => {
            let error = p.get("error").and_then(Value::as_bool).unwrap_or(false);
            let fault = if error {
                photocraft_gpu::Fault::Error("simulated error (ui.gpu.simulateLoss)".into())
            } else {
                photocraft_gpu::Fault::Lost("simulated (ui.gpu.simulateLoss)".into())
            };
            let active = match app.gpu_health() {
                Some(h) => {
                    h.mark(fault);
                    true
                }
                None => false,
            };
            app.check_gpu(ctx);
            ctx.request_repaint();
            ok(json!({"wasActive": active, "gpuInfo": app.perf.gpu_info}))
        }
        "ui.focus" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.screenshot" => {
            // Occluded windows don't render on macOS, so the screenshot would never arrive: raise first.
            if p.get("focus").and_then(Value::as_bool).unwrap_or(true) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            // The capture itself is issued by `PhotocraftApp::issue_screenshots` once open/close
            // animations (modals, popups) have settled.
            let token = app.ui.alloc_id();
            ctx.request_repaint();
            Outcome::Screenshot { token, path: s("path").map(str::to_string) }
        }
        "app.open" => match s("path") {
            Some(path) => {
                let opened = match app.services.automation_read.as_mut() {
                    Some(read) => read(path),
                    None => Err("automation read authority is not configured".into()),
                };
                wrap(opened.and_then(|(name, bytes)| {
                    let warnings = app.open_automation_bytes(&name, &bytes)?;
                    // Brushes/gradients go to the preset libraries: no document, no Open Recent entry.
                    if !crate::preset_files_ui::is_preset_file(&name) {
                        if let Some(state) = app.session.active_mut() {
                            state.path = Some(path.to_string());
                        }
                        app.push_recent(path);
                    }
                    Ok(json!({"path": path, "name": name, "warnings": warnings}))
                }))
            }
            None => err("missing `path`"),
        },
        "app.save" => wrap(app.save_automation(s("path").map(str::to_string)).map(|(p, w)| json!({"path": p, "warnings": w}))),
        "app.quit" => {
            app.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ok(Value::Null)
        }
        other => err(format!("unknown method `{other}`")),
    }
}

/// Snapshot of everything on screen, addressable by id.
pub fn inspect(app: &PhotocraftApp, ctx: &egui::Context) -> Value {
    let screen = ctx.content_rect();
    let dialogs: Vec<Value> =
        app.ui.dialogs.iter().map(|d| json!({"id": d.id, "kind": d.kind, "title": crate::dialogs::title(d), "fields": d.fields})).collect();
    json!({
        "window": {"width": screen.width(), "height": screen.height(), "pixelsPerPoint": ctx.pixels_per_point()},
        "tool": app.ui.tool,
        "textEdit": app.ui.text_edit,
        "layerMenu": app.ui.layer_menu,
        "panels": app.ui.panels,
        "views": app.ui.views,
        "dialogs": dialogs,
        "windows": app.ui.windows,
        "theme": app.ui.theme,
        "status": app.ui.status,
        "statusError": app.ui.status_error,
        "notices": app.ui.notices,
        "frame": app.frame,
        "session": photocraft_engine::inspect::session(&app.session),
        "document": app.session.active().map(photocraft_engine::inspect::document),
        "perf": {"fps": app.fps, "timings": app.perf},
        "brush": {"size": app.session.tools.brush.size, "hardness": app.session.tools.brush.hardness, "opacity": app.session.tools.brush.opacity},
        "distort": app.distort.describe(),
        "jobs": crate::jobs_ui::inspect(app),
    })
}

pub fn save_screenshot(app: &mut PhotocraftApp, image: &egui::ColorImage, path: Option<&str>) -> Value {
    let [w, h] = image.size;
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    let png = match app.services.encode_png.as_ref() {
        Some(enc) => enc(w as u32, h as u32, &rgba),
        None => Err("no PNG encoder configured".into()),
    };
    let Some(path) = path else {
        return match png {
            Ok(bytes) => json!({
                "ok": true,
                "result": {
                    "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "mime": "image/png",
                    "width": w,
                    "height": h,
                }
            }),
            Err(error) => json!({"ok": false, "error": error}),
        };
    };
    let result = png.and_then(|bytes| match app.services.automation_write.as_mut() {
        Some(write) => write(path, &bytes),
        None => Err("automation write authority is not configured".into()),
    });
    match result {
        Ok(()) => json!({"ok": true, "result": {"path": path, "width": w, "height": h}}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(app: &mut PhotocraftApp, ctx: &egui::Context, method: &str, params: Value) -> Value {
        let (req, _rx) = ControlRequest::new(method, params);
        match handle(app, ctx, &req) {
            Outcome::Done(v) => v,
            _ => panic!("{method}: expected an immediate reply"),
        }
    }

    #[test]
    fn engine_execute_runs_with_defaults_but_menu_invoke_opens_the_dialog() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        app.run("edit.fill", json!({"color": "#808080"})).unwrap();
        let rev = app.session.active().unwrap().revision;
        let r = call(&mut app, &ctx, "engine.execute", json!({"command": "filter.blur.gaussianBlur"}));
        assert!(!r.to_string().contains("\"dialog\""), "engine.execute opened a dialog: {r}");
        assert!(app.session.active().unwrap().revision > rev, "engine.execute didn't run the filter");
        let rev = app.session.active().unwrap().revision;
        let r = call(&mut app, &ctx, "ui.menu.invoke", json!({"id": "filter.blur.gaussianBlur"}));
        assert!(r.to_string().contains("dialog"), "ui.menu.invoke should open the dialog: {r}");
        assert_eq!(app.session.active().unwrap().revision, rev, "opening a dialog must not edit the document");
    }

    #[test]
    fn preferences_apply_is_available_over_control_without_closing() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        let r = call(&mut app, &ctx, "ui.menu.invoke", json!({"id": "edit.preferences.interface"}));
        let id = r["result"]["dialog"].as_u64().unwrap();
        let mut values = app.ui.dialog_mut(id).unwrap().fields["values"].clone();
        values["interface"]["uiScale"] = json!("200");
        assert_eq!(call(&mut app, &ctx, "ui.dialog.set", json!({"dialog": id, "field": "values", "value": values}))["ok"], true);
        assert_eq!(call(&mut app, &ctx, "ui.dialog.apply", json!({"dialog": id}))["ok"], true);
        assert_eq!(app.session.prefs().interface.ui_scale, photocraft_engine::prefs::UiScale::P200);
        assert!(app.ui.dialog_mut(id).is_some());
        assert_eq!(call(&mut app, &ctx, "ui.dialog.cancel", json!({"dialog": id}))["ok"], true);
        assert_eq!(app.session.prefs().interface.ui_scale, photocraft_engine::prefs::UiScale::P200);
        for params in [json!({}), json!({"dialog": "invalid"}), json!({"dialog": u64::MAX})] {
            assert_eq!(call(&mut app, &ctx, "ui.dialog.apply", params)["ok"], false);
        }
    }

    #[test]
    fn preferences_apply_respects_the_automation_command_policy() {
        let services = crate::Services {
            automation_command: Some(Box::new(|id, _| if id == "prefs.set" { Err("preference changes denied".into()) } else { Ok(()) })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        let before = app.session.prefs().to_json();
        let id = crate::prefs_ui::open_preferences(&mut app, "interface");
        app.ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap()["interface"]["uiScale"] = json!("200");
        let r = call(&mut app, &ctx, "ui.dialog.apply", json!({"dialog": id}));
        assert_eq!(r["ok"], false);
        assert_eq!(r["error"], "preference changes denied");
        assert_eq!(app.session.prefs().to_json(), before);
        assert!(app.ui.dialog_mut(id).is_some());
    }

    #[test]
    fn synthetic_shortcuts_use_the_automation_command_policy() {
        let services = crate::Services {
            automation_command: Some(Box::new(|id, _| if id.starts_with("file.") { Err("filesystem command denied".into()) } else { Ok(()) })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        app.automation_input = true;
        let error = crate::menus::invoke(&mut app, &ctx, "file.open", json!({})).unwrap_err();
        assert_eq!(error, "filesystem command denied");
    }

    #[test]
    fn automation_open_and_save_use_the_roots_and_reply_with_warnings() {
        use photocraft_color::{ColorMode, SampleType};
        use photocraft_doc::Document;
        use photocraft_geom::Size;
        use std::cell::RefCell;
        use std::rc::Rc;

        // Without granted roots both fail closed, even with an exporter and an ambient writer.
        let services = crate::Services {
            export: Some(Box::new(|_d: &Document, _p: &str, _s: &crate::ExportSettings| Ok((b"out".to_vec(), Vec::new())))),
            write: Some(Box::new(|_p: &str, _b: &[u8]| Err("ambient writer used".into()))),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        let r = call(&mut app, &ctx, "app.open", json!({"path": "in/warn.psd"}));
        assert!(r.to_string().contains("automation read authority is not configured"), "{r}");
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        let r = call(&mut app, &ctx, "app.save", json!({"path": "out.png"}));
        assert!(r.to_string().contains("automation write authority is not configured"), "{r}");

        let written: Rc<RefCell<Vec<String>>> = Rc::default();
        let w = written.clone();
        let services = crate::Services {
            import: Some(Box::new(|name: &str, _b: &[u8]| {
                Ok((Document::new(name, Size::new(4, 4), ColorMode::Rgb, SampleType::U8), vec!["Adjustment layer flattened".to_string()]))
            })),
            export: Some(Box::new(|_d: &Document, _p: &str, _s: &crate::ExportSettings| Ok((b"out".to_vec(), vec!["Layers were flattened".to_string()])))),
            automation_read: Some(Box::new(|path: &str| Ok((path.rsplit('/').next().unwrap_or(path).to_string(), b"x".to_vec())))),
            automation_write: Some(Box::new(move |path: &str, _b: &[u8]| {
                w.borrow_mut().push(path.to_string());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let r = call(&mut app, &ctx, "app.open", json!({"path": "in/warn.psd"}));
        assert_eq!(r["result"]["warnings"], json!(["Adjustment layer flattened"]), "{r}");
        assert_eq!(r["result"]["name"], "warn.psd");
        assert_eq!(app.session.active().unwrap().path.as_deref(), Some("in/warn.psd"));
        assert_eq!(app.ui.recent_files, vec!["in/warn.psd".to_string()]);
        let r = call(&mut app, &ctx, "app.save", json!({"path": "out.png"}));
        assert_eq!(r["result"], json!({"path": "out.png", "warnings": ["Layers were flattened"]}), "{r}");
        assert_eq!(*written.borrow(), vec!["out.png".to_string()]);
    }
}

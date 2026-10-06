//! Background jobs in the shell (#210): long commands and file opens run on worker threads (see
//! `photocraft_engine::jobs`) while the window keeps drawing at full frame rate.
//!
//! - [`run`] starts a job-capable command in the background (when [`PhotocraftApp::background_jobs`]
//!   is on: the desktop app; tests and the web run inline as before).
//! - [`tick`] (every frame) applies finished jobs, reports failures, answers control requests
//!   waiting on a job, finishes background opens and keeps frames coming while anything runs.
//! - [`status_progress`] draws the status-bar progress with a Cancel button; [`dialog`] the modal
//!   progress window for jobs that lock the active document (filters, Content-Aware Fill, …),
//!   shown after a short delay so quick jobs don't flash it. Esc cancels.
//! - Files opening in the background get a document tab at once ([`OpenTab`]); its canvas area
//!   shows the progress, and closing the tab cancels the open.
//!
//! The canvas keeps showing the pre-job state: workers never touch the session's documents.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use egui::{Color32, Rect, RichText, Sense, Stroke, vec2};
use photocraft_engine::jobs::{JobEvent, JobId, JobInfo, JobOutcome, OPEN_JOB, OpenSource, Started};
use serde_json::{Value, json};

use crate::theme::Tokens;
use crate::{ControlResponse, PhotocraftApp, notices};

/// Delay before the modal progress dialog appears (quick jobs finish without it).
pub const DIALOG_DELAY_MS: f64 = 250.0;

/// A file opening in the background: shown as a document tab with progress.
#[derive(Clone, Debug)]
pub struct OpenTab {
    pub job: JobId,
    /// Display name (the file name).
    pub name: String,
    /// Where it was read from (File › Save writes back there; added to Open Recent).
    pub path: Option<String>,
}

/// The shell's job bookkeeping.
#[derive(Default)]
pub struct JobsUi {
    /// Control replies waiting for a job to end.
    pub(crate) waiters: Vec<(JobId, Sender<ControlResponse>)>,
    /// Background opens, in tab order.
    pub opens: Vec<OpenTab>,
    /// The open tab shown in the canvas area instead of the active document.
    pub focus: Option<JobId>,
    /// The job the latest [`run`] started (the control channel waits on it).
    pub(crate) last_started: Option<JobId>,
}

/// Run command `id`: in the background when background jobs are on and the command is a job
/// (the result is then `{"job": id, "pending": true}` and the edit lands when [`tick`] applies
/// it), else synchronously like [`photocraft_engine::Session::execute`].
pub fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Result<Value, String> {
    if !app.background_jobs {
        return app.session.execute(id, params).map_err(|e| e.to_string());
    }
    match app.session.start(id, params).map_err(|e| e.to_string())? {
        Started::Done(v) => Ok(v),
        Started::Job(job) => {
            app.jobs.last_started = Some(job);
            // The status bar's progress readout names the job; clear any old message.
            app.ui.status.clear();
            app.ui.status_error = false;
            Ok(json!({"job": job.0, "pending": true}))
        }
    }
}

/// Open a file in the background: a tab appears at once and shows the progress; the document
/// replaces it when decoded. `path` is remembered for File › Save and Open Recent.
pub fn start_open(app: &mut PhotocraftApp, name: &str, path: Option<String>, source: OpenSource) -> Result<(), String> {
    match app.session.start_open(name, source).map_err(|e| e.to_string())? {
        // Inline (wasm): finish now, like a background open that ended at once.
        Started::Done(v) => finish_open(app, name, path.as_deref(), &v),
        Started::Job(job) => {
            app.jobs.opens.push(OpenTab { job, name: name.to_string(), path });
            app.jobs.focus = Some(job);
            app.ui.chrome.home = None;
            app.ui.status = crate::i18n::fmt(tl!("Opening {name}…"), &[("name", name)]);
            app.ui.status_error = false;
            Ok(())
        }
    }
}

/// Per frame: apply finished jobs and react to them; keep frames coming while jobs run; Esc
/// cancels the job in view.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    for e in app.session.poll_jobs() {
        on_event(app, e);
    }
    // A focused open tab that ended (or was cancelled elsewhere) gives way to the documents.
    if app.jobs.focus.is_some_and(|j| !app.jobs.opens.iter().any(|o| o.job == j)) {
        app.jobs.focus = app.jobs.opens.last().map(|o| o.job);
    }
    if !app.session.has_jobs() {
        return;
    }
    // Animate progress and poll for results at display rate; the UI thread only draws.
    ctx.request_repaint_after(std::time::Duration::from_millis(16));
    if let Some(job) = job_in_view(app)
        && !ctx.text_edit_focused()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        cancel(app, job.id);
    }
}

/// The job the user is looking at: the open tab in view, the active document's job, or (for
/// the status bar) the newest running job.
pub fn job_in_view(app: &PhotocraftApp) -> Option<JobInfo> {
    if let Some(j) = app.jobs.focus.and_then(|j| app.session.job(j)) {
        return Some(j);
    }
    app.session.active_job().or_else(|| app.session.jobs().pop())
}

/// Cancel job `id` (the status bar's ×, the dialog's Cancel, Esc, closing an opening tab).
pub fn cancel(app: &mut PhotocraftApp, id: JobId) {
    let label = app.session.job(id).map(|j| j.label);
    if app.session.cancel_job(id) {
        // The event (and its status text) arrives with the next poll; say so now as well.
        if let Some(l) = label {
            app.ui.status = crate::i18n::fmt(tl!("Cancelled {label}"), &[("label", &l)]);
            app.ui.status_error = false;
        }
    }
}

fn on_event(app: &mut PhotocraftApp, e: JobEvent) {
    // Control requests waiting on this job.
    let reply = match &e.outcome {
        JobOutcome::Done(v) => json!({"ok": true, "result": v}),
        JobOutcome::Failed(err) => json!({"ok": false, "error": err}),
        JobOutcome::Cancelled => json!({"ok": false, "error": "cancelled"}),
    };
    app.jobs.waiters.retain(|(job, tx)| {
        if *job == e.id {
            let _ = tx.send(reply.clone());
            false
        } else {
            true
        }
    });
    if e.command == OPEN_JOB {
        let Some(i) = app.jobs.opens.iter().position(|o| o.job == e.id) else { return };
        let tab = app.jobs.opens.remove(i);
        match e.outcome {
            JobOutcome::Done(v) => {
                if let Err(err) = finish_open(app, &tab.name, tab.path.as_deref(), &v) {
                    app.open_failed(&tab.name, &err);
                }
            }
            JobOutcome::Failed(err) => app.open_failed(&tab.name, &err),
            JobOutcome::Cancelled => {
                app.ui.status = crate::i18n::fmt(tl!("Cancelled opening {name}"), &[("name", &tab.name)]);
                app.ui.status_error = false;
            }
        }
        return;
    }
    match e.outcome {
        JobOutcome::Done(v) if e.command == "brush.presets.importAbr" => {
            let n = v.get("count").and_then(Value::as_u64).unwrap_or(0);
            let group = v.get("group").and_then(Value::as_str).unwrap_or_default();
            app.ui.status = format!("Imported {n} brushes from {group}");
            app.ui.status_error = false;
            app.ui.panels.brush_settings = true;
            app.ui.brush_tab = 1;
        }
        JobOutcome::Done(_) => {
            app.sync_views();
            app.ui.status = e.label;
            app.ui.status_error = false;
        }
        JobOutcome::Failed(err) => {
            app.sync_views();
            notices::error(app, format!("{}: {err}", e.label));
        }
        JobOutcome::Cancelled => {
            app.ui.status = crate::i18n::fmt(tl!("Cancelled {label}"), &[("label", &e.label)]);
            app.ui.status_error = false;
        }
    }
}

/// What [`crate::PhotocraftApp::open_bytes`] does after decoding, for a background open's result
/// (`{document, warnings, color}`).
fn finish_open(app: &mut PhotocraftApp, name: &str, path: Option<&str>, v: &Value) -> Result<(), String> {
    let index = v.get("document").and_then(Value::as_u64).ok_or("the open job returned no document")? as usize;
    let warnings: Vec<String> =
        v.get("warnings").and_then(Value::as_array).map(|a| a.iter().filter_map(|w| w.as_str().map(str::to_string)).collect()).unwrap_or_default();
    app.session.set_active(index);
    if let (Some(p), Some(st)) = (path, app.session.active_mut()) {
        st.path = Some(p.to_string());
    }
    if let Some(p) = path {
        app.push_recent(p);
    }
    app.sync_views();
    app.ui.status = format!("Opened {name}");
    app.ui.status_error = false;
    notices::io_warnings(app, &format!("Opened {name}"), &warnings);
    // Script events bound to "Open Document".
    photocraft_engine::automate_cmds::document_opened(&mut app.session);
    app.sync_views();
    let color = v.get("color").cloned().unwrap_or(Value::Null);
    let ask = color.get("ask").and_then(Value::as_bool) == Some(true);
    if ask && (color.get("mismatch").and_then(Value::as_bool) == Some(true) || color.get("missing").is_some()) {
        crate::prefs_ui::open_mismatch(app, &color);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- drawing

/// A progress bar: `frac` of `rect` filled with the accent; `None` = indeterminate (a sliding
/// segment), for jobs that haven't reported progress yet.
pub fn bar(ui: &egui::Ui, rect: Rect, frac: Option<f32>, t: &Tokens) {
    let p = ui.painter();
    let r = rect.height() / 2.0;
    p.rect_filled(rect, r, t.field);
    p.rect_stroke(rect, r, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    let fill = match frac {
        Some(f) => Rect::from_min_size(rect.min, vec2((rect.width() * f.clamp(0.0, 1.0)).max(rect.height()), rect.height())),
        None => {
            let time = ui.input(|i| i.time) as f32;
            let w = rect.width() * 0.3;
            let x = rect.left() + ((time * 0.8).fract() * (rect.width() + w)) - w;
            Rect::from_min_max(egui::pos2(x.max(rect.left()), rect.top()), egui::pos2((x + w).min(rect.right()), rect.bottom()))
        }
    };
    if fill.width() > 0.0 {
        p.rect_filled(fill, r, t.accent);
    }
}

/// Progress that has been reported, or `None` (indeterminate) while it's still 0.
fn shown_fraction(j: &JobInfo) -> Option<f32> {
    (j.progress > 0.0).then_some(j.progress)
}

fn percent(j: &JobInfo) -> String {
    // Nothing yet reported: the bar is indeterminate, so no number.
    if j.progress > 0.0 { format!("{:.0}%", j.progress * 100.0) } else { String::new() }
}

/// The status bar's job readout: "Gaussian Blur ▬▬▬▭▭ 45 %  ×". Draws nothing when no job runs.
pub fn status_progress(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(j) = job_in_view(app) else { return };
    let t = Tokens::get(ui.ctx());
    let (bar_w, bar_h) = if t.pro { (120.0, 6.0) } else { (140.0, 6.0) };
    let more = app.session.jobs().len().saturating_sub(1);
    // Right to left: ×, percentage, bar, label.
    let (xr, xresp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    if xresp.hovered() {
        ui.painter().rect_filled(xr, t.radius_sm, t.hover);
    }
    crate::icons::paint(ui, xr, "x", 11.0, if xresp.hovered() { t.text } else { t.text_dim });
    let xresp = xresp.on_hover_text(tl!("Cancel (Esc)"));
    ui.label(RichText::new(percent(&j)).color(t.text_dim).size(11.5).monospace());
    let (br, _) = ui.allocate_exact_size(vec2(bar_w, bar_h), Sense::hover());
    bar(ui, br, shown_fraction(&j), &t);
    let label = if more > 0 { format!("{} (+{more})", j.label) } else { j.label.clone() };
    ui.label(RichText::new(label).color(t.text_dim).size(12.0));
    if xresp.clicked() {
        cancel(app, j.id);
    }
}

/// The modal progress window for a job locking the active document (Photoshop shows one for
/// long filters): title, status, bar, percentage and elapsed time, Cancel. Appears after
/// [`DIALOG_DELAY_MS`]; Esc cancels (see [`tick`]).
pub fn dialog(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(j) = app.session.active_job() else { return };
    if j.elapsed_ms < DIALOG_DELAY_MS {
        return;
    }
    let t = Tokens::get(ctx);
    let mut cancel_it = false;
    // Photoshop doesn't dim the window behind its progress dialog either.
    let modal = egui::Modal::new(egui::Id::new("job-progress")).backdrop_color(Color32::from_black_alpha(36)).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(RichText::new(&j.label).font(crate::theme::semibold(15.0)));
        ui.add_space(4.0);
        crate::widgets::hairline(ui);
        ui.add_space(10.0);
        let msg = if j.message.is_empty() || j.message == j.label { tl!("Working…").to_string() } else { j.message.clone() };
        ui.label(RichText::new(msg).color(t.text_dim));
        ui.add_space(8.0);
        let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        bar(ui, br, shown_fraction(&j), &t);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(percent(&j)).color(t.text_faint).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(elapsed(j.elapsed_ms)).color(t.text_faint).size(11.5));
            });
        });
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            cancel_it = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
            ui.label(RichText::new(tl!("Esc to cancel")).color(t.text_faint).size(11.0));
        });
    });
    // A click on the backdrop doesn't cancel the work; Esc is handled in `tick`.
    let _ = modal;
    if cancel_it {
        cancel(app, j.id);
    }
}

fn elapsed(ms: f64) -> String {
    let s = (ms / 1000.0).max(0.0);
    if s < 60.0 { format!("{s:.1} s") } else { format!("{}:{:02}", (s / 60.0).floor() as u64, (s % 60.0).floor() as u64) }
}

/// The canvas area of an opening tab: the file name, a bar and Cancel, centred on the
/// workspace backdrop.
pub fn open_card(app: &mut PhotocraftApp, ui: &mut egui::Ui, job: JobId) {
    let Some(tab) = app.jobs.opens.iter().find(|o| o.job == job).cloned() else { return };
    let Some(j) = app.session.job(job) else { return };
    let t = Tokens::get(ui.ctx());
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, 0.0, t.canvas);
    let card = Rect::from_center_size(area.center(), vec2(340.0, 132.0));
    ui.painter().rect_filled(card, t.radius_lg, t.card);
    ui.painter().rect_stroke(card, t.radius_lg, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink(18.0)));
    child.label(RichText::new(crate::i18n::fmt(tl!("Opening {name}"), &[("name", &tab.name)])).font(crate::theme::semibold(14.0)));
    child.add_space(4.0);
    let msg = if j.message.is_empty() { tl!("Reading…").to_string() } else { format!("{}…", j.message) };
    child.label(RichText::new(msg).color(t.text_dim).size(12.0));
    child.add_space(10.0);
    let (br, _) = child.allocate_exact_size(vec2(child.available_width(), 8.0), Sense::hover());
    bar(&child, br, shown_fraction(&j), &t);
    child.add_space(10.0);
    let mut cancel_it = false;
    child.horizontal(|ui| {
        ui.label(RichText::new(percent(&j)).color(t.text_faint).size(11.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            cancel_it = crate::widgets::secondary_button(ui, tl!("Cancel"), 76.0).clicked();
        });
    });
    if cancel_it {
        cancel(app, job);
    }
}

/// Open tabs for the tab strips: (job, title, progress).
pub fn open_tabs(app: &PhotocraftApp) -> Vec<(JobId, String, f32)> {
    app.jobs.opens.iter().map(|o| (o.job, o.name.clone(), app.session.job(o.job).map_or(0.0, |j| j.progress))).collect()
}

/// Draw one opening tab's progress underline inside tab rect `r`.
pub fn tab_underline(ui: &egui::Ui, r: Rect, frac: f32, t: &Tokens) {
    let line = Rect::from_min_max(egui::pos2(r.left() + 4.0, r.bottom() - 3.0), egui::pos2(r.right() - 4.0, r.bottom() - 1.0));
    ui.painter().rect_filled(line, 1.0, t.field_border);
    let f = Rect::from_min_size(line.min, vec2(line.width() * frac.clamp(0.0, 1.0), line.height()));
    ui.painter().rect_filled(f, 1.0, t.accent);
}

/// Jobs for `ui.inspect`.
pub fn inspect(app: &PhotocraftApp) -> Value {
    json!({
        "running": app.session.jobs(),
        "opening": app.jobs.opens.iter().map(|o| json!({"job": o.job.0, "name": o.name, "path": o.path})).collect::<Vec<_>>(),
        "focus": app.jobs.focus.map(|j| j.0),
        "background": app.background_jobs,
    })
}

/// Bytes shared with a background open without copying them again.
pub fn bytes(b: &[u8]) -> OpenSource {
    OpenSource::Bytes(Arc::new(b.to_vec()))
}

#[cfg(test)]
#[path = "jobs_ui_tests.rs"]
mod tests;

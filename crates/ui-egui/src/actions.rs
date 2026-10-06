//! Actions panel: record and replay command sequences.
//!
//! Every edit is an engine command appended to `Session::journal`, so recording is just remembering
//! where the journal was when ● was pressed, and ▶ replays those exact commands (strokes, transforms,
//! type edits included) on whatever document is active.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PhotocraftApp;
use crate::theme::Tokens;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    pub steps: Vec<(String, Value)>,
    #[serde(default)]
    pub expanded: bool,
}

/// Actions and recording state (persisted with the UI state).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Actions {
    pub list: Vec<Action>,
    pub selected: Option<usize>,
    /// Journal length when recording started, and the action being recorded into.
    pub recording: Option<(usize, usize)>,
}

/// Commands that are queries or UI plumbing, not worth replaying.
fn replayable(id: &str) -> bool {
    !matches!(id, "document.pixel" | "document.inspect" | "session.inspect" | "command.list" | "type.info" | "type.fonts" | "edit.undo" | "edit.redo")
}

pub fn start_recording(app: &mut PhotocraftApp) {
    let n = app.ui.actions.list.len();
    app.ui.actions.list.push(Action { name: format!("Action {}", n + 1), steps: Vec::new(), expanded: true });
    app.ui.actions.selected = Some(n);
    app.ui.actions.recording = Some((app.session.journal.len(), n));
}

/// Stop recording: move the new journal entries into the action.
pub fn stop_recording(app: &mut PhotocraftApp) {
    let Some((from, idx)) = app.ui.actions.recording.take() else { return };
    let steps: Vec<(String, Value)> = app.session.journal.iter().skip(from).filter(|(id, _)| replayable(id)).cloned().collect();
    if let Some(a) = app.ui.actions.list.get_mut(idx) {
        a.steps.extend(steps);
    }
}

/// Replay an action; stops at the first failing step and reports it.
pub fn play(app: &mut PhotocraftApp, idx: usize) -> Result<usize, String> {
    let steps = app.ui.actions.list.get(idx).map(|a| a.steps.clone()).ok_or("no such action")?;
    for (i, (id, p)) in steps.iter().enumerate() {
        app.run(id, p.clone()).map_err(|e| format!("Step {} ({id}) failed: {e}", i + 1))?;
    }
    Ok(steps.len())
}

fn label_of(id: &str) -> String {
    photocraft_engine::commands::find(id).map(|c| c.label.trim_end_matches('…').to_string()).unwrap_or_else(|| id.to_string())
}

pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let live = app.ui.actions.recording.map(|(from, _)| app.session.journal.len().saturating_sub(from)).unwrap_or(0);
    let max_h = (ui.available_height() - 70.0).clamp(80.0, 320.0);
    let mut play_idx = None;
    egui::ScrollArea::vertical().id_salt("actions-rows").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        if app.ui.actions.list.is_empty() {
            ui.label(egui::RichText::new(tl!("Record ● a sequence of edits, then play ▶ it on any document.")).color(t.text_faint).size(11.5));
        }
        let n = app.ui.actions.list.len();
        for i in 0..n {
            let a = app.ui.actions.list[i].clone();
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let sel = app.ui.actions.selected == Some(i);
            if sel {
                ui.painter().rect_filled(rect, 0.0, t.row_selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.5));
            }
            // Expander triangle.
            let tri = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(8.0, 8.0));
            let pts = if a.expanded {
                vec![tri.left_top(), tri.right_top(), tri.center_bottom()]
            } else {
                vec![tri.left_top(), tri.right_center(), tri.left_bottom()]
            };
            ui.painter().add(egui::Shape::convex_polygon(pts, t.text_dim, Stroke::NONE));
            crate::icons::paint(ui, Rect::from_center_size(pos2(rect.left() + 30.0, rect.center().y), vec2(16.0, 16.0)), "play", 11.0, t.icon);
            let recording_this = app.ui.actions.recording.is_some_and(|(_, r)| r == i);
            let steps = a.steps.len() + if recording_this { live } else { 0 };
            ui.painter().text(pos2(rect.left() + 44.0, rect.center().y), Align2::LEFT_CENTER, &a.name, egui::FontId::proportional(12.0), t.text);
            ui.painter().text(
                pos2(rect.right() - 8.0, rect.center().y),
                Align2::RIGHT_CENTER,
                format!("{steps} steps"),
                egui::FontId::proportional(11.0),
                t.text_faint,
            );
            if recording_this {
                ui.painter().circle_filled(pos2(rect.right() - 64.0, rect.center().y), 4.0, Color32::from_rgb(230, 60, 60));
            }
            if resp.clicked() {
                app.ui.actions.selected = Some(i);
                if resp.interact_pointer_pos().is_some_and(|p| p.x < rect.left() + 20.0) {
                    app.ui.actions.list[i].expanded = !a.expanded;
                }
            }
            if resp.double_clicked() {
                play_idx = Some(i);
            }
            if a.expanded {
                for (id, _) in &a.steps {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                    ui.painter().text(pos2(r.left() + 44.0, r.center().y), Align2::LEFT_CENTER, label_of(id), egui::FontId::proportional(11.5), t.text_dim);
                }
            }
        }
    });
    // Footer: stop, record, play, new, delete (Photoshop's order).
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let recording = app.ui.actions.recording.is_some();
        if crate::icons::button(ui, "square", 24.0, false, tl!("Stop playing/recording")).clicked() {
            stop_recording(app);
        }
        let (r, rec) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
        if rec.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        ui.painter().circle_filled(r.center(), 5.5, if recording { Color32::from_rgb(230, 60, 60) } else { t.icon });
        if rec.on_hover_text(tl!("Begin recording")).clicked() && !recording {
            start_recording(app);
        }
        if crate::icons::button(ui, "play", 24.0, false, tl!("Play selection")).clicked() {
            play_idx = app.ui.actions.selected;
        }
        if crate::icons::button(ui, "plus", 24.0, false, tl!("Create new action")).clicked() && !recording {
            start_recording(app);
        }
        if crate::icons::button(ui, "trash", 24.0, false, tl!("Delete")).clicked()
            && !recording
            && let Some(i) = app.ui.actions.selected.take()
            && i < app.ui.actions.list.len()
        {
            app.ui.actions.list.remove(i);
        }
    });
    if let Some(i) = play_idx {
        match play(app, i) {
            Ok(n) => app.ui.status = format!("Played {n} steps"),
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn record_and_replay_on_another_document() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 40})).unwrap();
        app.sync_views();
        start_recording(&mut app);
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
        app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
        let _ = app.run("document.pixel", json!({"x": 1, "y": 1}));
        stop_recording(&mut app);
        let a = &app.ui.actions.list[0];
        assert_eq!(a.steps.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["layer.new.layer", "select.rect", "edit.fill"]);
        // Replay on a second document.
        app.run("file.new", json!({"width": 40, "height": 40})).unwrap();
        app.sync_views();
        assert_eq!(play(&mut app, 0).unwrap(), 3);
        let d = &app.session.active().unwrap().doc;
        assert_eq!(d.layers.len(), 2);
        assert_eq!(d.layers[1].surface().unwrap().pixel(5, 5), vec![1.0, 0.0, 0.0, 1.0]);
    }
}

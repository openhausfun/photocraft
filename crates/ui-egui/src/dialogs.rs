//! Dialogs rendered from `UiState::dialogs`. Field values live in the dialog data, so automation can
//! set them (`ui.dialog.set`) and confirm (`ui.dialog.confirm`) exactly like a user.

use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::{Dialog, DialogKind};

/// Where this frame's dialogs are on screen (the canvas reads last frame's: it draws first).
const RECTS: &str = "pc-dialog-rects";

fn rects(ctx: &egui::Context) -> Vec<egui::Rect> {
    ctx.data(|m| m.get_temp(egui::Id::new(RECTS))).unwrap_or_default()
}

/// With a dialog open the rest of the window is inert (egui's modal layer), but like Photoshop the
/// image can still be panned and zoomed under it. The pointer position when it is over `canvas`
/// and not over a dialog or one of its popups.
pub fn free_pointer_over(ctx: &egui::Context, canvas: egui::Rect) -> Option<egui::Pos2> {
    if egui::Popup::is_any_open(ctx) {
        return None;
    }
    let p = ctx.pointer_hover_pos()?;
    let rects = rects(ctx);
    (canvas.contains(p) && !rects.iter().any(|r| r.contains(p))).then_some(p)
}

/// Pan drag under an open dialog: Space-drag, middle-drag, or a drag with the Hand tool, started on
/// the free canvas. Returns this frame's pointer movement.
pub fn pan_delta(ctx: &egui::Context, canvas: egui::Rect, hand: bool) -> Option<egui::Vec2> {
    let rects = rects(ctx);
    let (origin, panning, delta) = ctx.input(|i| {
        let p = &i.pointer;
        (p.press_origin(), p.middle_down() || (p.primary_down() && (hand || i.key_down(egui::Key::Space))), p.delta())
    });
    let origin = origin?;
    (panning && canvas.contains(origin) && !rects.iter().any(|r| r.contains(origin))).then_some(delta)
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let dialogs = app.ui.dialogs.clone();
    let mut shown = Vec::new();
    for d in dialogs {
        let mut fields = d.fields.clone();
        let mut outcome: Option<bool> = None; // Some(true)=OK, Some(false)=Cancel
        let mut apply_requested = false;
        let title = display_title(&d);
        let id = egui::Id::new(("dialog", d.id));
        // Offset from centre, moved by dragging the title bar (view state only, so egui memory).
        let offset: egui::Vec2 = ctx.data(|m| m.get_temp(id)).unwrap_or_default();
        let mut drag = egui::Vec2::ZERO;
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_CENTER, offset);
        // Photoshop doesn't dim the window behind dialogs: previews must be judged at true contrast.
        let modal = egui::Modal::new(id).area(area).backdrop_color(egui::Color32::TRANSPARENT).show(ctx, |ui| {
            ui.set_min_width(380.0);
            let wide = crate::prefs_ui::width(&d.fields);
            if let Some(w) = wide {
                ui.set_min_width(w.min(460.0));
            }
            if d.kind == DialogKind::NewDocument {
                ui.set_min_width(800.0);
            }
            ui.set_max_width(wide.unwrap_or(if d.kind == DialogKind::NewDocument {
                800.0
            } else if d.kind == DialogKind::LayerStyle || d.fields.contains_key("__export") || crate::color_picker_ui::owns(&d.fields) {
                600.0
            } else {
                440.0
            }));
            if let Some(w) = crate::file_ui::dialog_width(&d.fields) {
                ui.set_min_width(w);
                ui.set_max_width(w);
            }
            let t = ui.add(egui::Label::new(egui::RichText::new(&title).font(crate::theme::semibold(15.0))).selectable(false)).rect;
            let bar = egui::Rect::from_min_max(t.min, egui::pos2(ui.max_rect().right(), t.bottom()));
            drag = ui.interact(bar, id.with("title"), egui::Sense::drag()).drag_delta();
            ui.add_space(4.0);
            crate::widgets::hairline(ui);
            ui.add_space(8.0);
            match d.kind {
                DialogKind::NewDocument => crate::new_doc_ui::body(ui, &mut fields),
                DialogKind::About if fields.get("systemInfo").and_then(Value::as_bool) == Some(true) => {
                    let lines = crate::gpu_status::system_info(app);
                    for l in &lines {
                        ui.add(egui::Label::new(egui::RichText::new(l).font(crate::theme::mono(12.0))).selectable(true));
                    }
                    ui.add_space(8.0);
                    if crate::widgets::secondary_button(ui, tl!("Copy"), 84.0).clicked() {
                        ui.ctx().copy_text(lines.join("\n"));
                    }
                }
                DialogKind::About => {
                    ui.label(tl!("PhotoCraft — an open-source, native image editor written in Rust."));
                    ui.label(crate::i18n::fmt(tl!("Version {version}"), &[("version", &photocraft_engine::build_info::long_version())]));
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        crate::links::discord_button(app, ui, 220.0);
                        ui.add_space(8.0);
                        crate::links::link_row(app, ui);
                    });
                    ui.add_space(10.0);
                    ui.weak("egui · wgpu · photocraft-engine");
                }
                DialogKind::Command if crate::fill_ui::owns(&fields) => crate::fill_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::rasterize_prompt::owns(&fields) => crate::rasterize_prompt::body(ui, &fields),
                DialogKind::Command if crate::variables_ui::owns(&fields) => crate::variables_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::file_ui::owns(&fields) => crate::file_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::color_picker_ui::owns(&fields) => crate::color_picker_ui::body(ui, &mut fields),
                DialogKind::Command if crate::color_range_ui::owns(&fields) => crate::color_range_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::prefs_ui::owns(&fields) => crate::prefs_ui::body(app, ui, &mut fields),
                DialogKind::Command if fields.contains_key("__export") => crate::export_dialog::body(app, ui, &mut fields),
                DialogKind::Command if fields.contains_key("__sizing") => crate::sizing::body(ui, &mut fields),
                DialogKind::Command if crate::adjust_dialog::owns(&fields) => crate::adjust_dialog::body(app, ui, &mut fields),
                DialogKind::Command if fields.contains_key("__filter") => crate::filter_dialog::body(ui, &mut fields),
                DialogKind::Command if fields.contains_key("__form") => crate::view_cmds::form_body(ui, &mut fields),
                DialogKind::Command => {}
                DialogKind::LayerStyle => crate::layer_style::body(ui, &mut fields),
                DialogKind::Error => {
                    ui.label(fields.get("message").and_then(Value::as_str).unwrap_or("Error"));
                }
            }
            ui.add_space(8.0);
            // Align::Min, not Center: a centred row fills the height left over from last frame's
            // (larger) size, so a dialog whose body gets shorter would never shrink back.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if matches!(d.kind, DialogKind::About | DialogKind::Error) {
                    if crate::widgets::primary_button(ui, tl!("OK"), 84.0).clicked() {
                        outcome = Some(false);
                    }
                } else {
                    let ok_label = if d.kind == DialogKind::NewDocument {
                        tl!("Create")
                    } else if d.fields.contains_key("__export") {
                        tl!("Export")
                    } else {
                        crate::file_ui::ok_label(&d.fields).unwrap_or(tl!("OK"))
                    };
                    if crate::widgets::primary_button(ui, ok_label, 84.0).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        outcome = Some(true);
                    }
                    if d.kind == DialogKind::Command && crate::prefs_ui::is_preferences(&fields) {
                        let changed = crate::prefs_ui::preferences_changed(app, &fields);
                        apply_requested = ui.add_enabled_ui(changed, |ui| crate::widgets::secondary_button(ui, tl!("Apply"), 84.0)).inner.clicked();
                    }
                    if crate::widgets::secondary_button(ui, if d.kind == DialogKind::NewDocument { tl!("Close") } else { tl!("Cancel") }, 84.0).clicked() {
                        outcome = Some(false);
                    }
                }
            });
            // The frame around the content (Frame::popup's margin and stroke).
            ui.min_rect().expand(ui.spacing().menu_margin.sum().max_elem() + 2.0)
        });
        shown.push(modal.inner);
        if drag != egui::Vec2::ZERO {
            // Keep the whole dialog (and so its title bar) on screen.
            let room = ((ctx.content_rect().size() - modal.response.rect.size()) / 2.0).max(egui::Vec2::ZERO);
            ctx.data_mut(|m| m.insert_temp(id, (offset + drag).clamp(-room, room)));
        }
        // Esc cancels (topmost dialog, no popup open). A click outside does nothing: Photoshop keeps
        // the dialog, and the pointer may be panning or zooming the canvas under it.
        if outcome.is_none()
            && (modal.response.should_close()
                || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))))
        {
            outcome = Some(false);
        }
        if let Some(dm) = app.ui.dialog_mut(d.id) {
            dm.fields = fields;
        }
        if apply_requested && outcome.is_none() {
            let _ = crate::prefs_ui::apply(app, d.id);
        }
        match outcome {
            Some(true) => {
                let _ = confirm(app, d.id);
            }
            Some(false) => {
                app.ui.close_dialog(d.id);
                app.filter_preview = None;
                app.color_range = None;
            }
            None => {}
        }
    }
    ctx.data_mut(|m| m.insert_temp(egui::Id::new(RECTS), shown));
}

/// The dialog title as shown: [`title`] in the UI language.
fn display_title(d: &Dialog) -> String {
    match d.kind {
        DialogKind::Command => {
            let label = d.fields.get("__label").and_then(Value::as_str).unwrap_or("Command");
            // Dialog labels are catalogued with their "…" ("Export As…"); some commands omit it.
            let with_dots = format!("{}…", label.trim_end_matches('…'));
            let shown = if crate::i18n::has(crate::i18n::current(), label) { tl!(label) } else { tl!(&with_dots) };
            shown.trim_end_matches('…').to_string()
        }
        _ => tl!(&title(d)).to_string(),
    }
}

pub fn title(d: &Dialog) -> String {
    match d.kind {
        DialogKind::NewDocument => "New Document".into(),
        DialogKind::About if d.fields.get("systemInfo").and_then(Value::as_bool) == Some(true) => "System Info".into(),
        DialogKind::About => "About PhotoCraft".into(),
        DialogKind::LayerStyle => "Layer Style".into(),
        DialogKind::Command => d.fields.get("__label").and_then(Value::as_str).unwrap_or("Command").trim_end_matches('…').to_string(),
        DialogKind::Error => "Error".into(),
    }
}

/// Confirm a dialog: run its action and close it. Used by the OK button and by automation.
pub fn confirm(app: &mut PhotocraftApp, id: u64) -> Result<Value, String> {
    let d = app.ui.close_dialog(id).ok_or_else(|| format!("no dialog {id}"))?;
    match d.kind {
        DialogKind::NewDocument => {
            let r = app.run("file.new", crate::new_doc_ui::command_params(&d.fields));
            if let Some(i) = app.session.active_index() {
                app.ui.views[i].fit_pending = true;
            }
            r
        }
        DialogKind::Command if crate::fill_ui::owns(&d.fields) => crate::fill_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::rasterize_prompt::owns(&d.fields) => crate::rasterize_prompt::confirm(app, &d.fields),
        DialogKind::Command if crate::variables_ui::owns(&d.fields) => crate::variables_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::file_ui::owns(&d.fields) => crate::file_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::color_picker_ui::owns(&d.fields) => crate::color_picker_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::color_range_ui::owns(&d.fields) => crate::color_range_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::prefs_ui::owns(&d.fields) => crate::prefs_ui::confirm(app, &d.fields),
        DialogKind::Command if d.fields.contains_key("__export") => crate::export_dialog::confirm(app, &d.fields),
        DialogKind::Command => {
            app.filter_preview = None;
            let cmd = d.fields.get("__command").and_then(|v| v.as_str().map(str::to_string)).ok_or("dialog has no command")?;
            let (cmd, params) = crate::smart_ui::confirm_command(&d.fields, cmd, crate::filter_dialog::params_of(&d.fields));
            app.run(&cmd, params)
        }
        DialogKind::LayerStyle => crate::layer_style::confirm(app, &d.fields),
        DialogKind::About | DialogKind::Error => Ok(Value::Null),
    }
}

/// Open the parameter dialog of `command`: the adjustment editor for `image.adjustments.*`, the
/// schema dialog for filters, the Color Range dialog for `select.colorRange`, otherwise a bare
/// confirm dialog.
pub fn open_command_dialog(app: &mut PhotocraftApp, command: &str, label: &str) -> u64 {
    if command == crate::color_range_ui::COMMAND {
        return crate::color_range_ui::open(app);
    }
    if let Some(id) = crate::adjust_dialog::open(app, command) {
        return id;
    }
    if crate::filter_dialog::has_dialog(command)
        && let Some(id) = crate::filter_dialog::open(app, command)
    {
        return id;
    }
    let mut fields = serde_json::Map::new();
    fields.insert("__command".into(), json!(command));
    fields.insert("__label".into(), json!(label));
    app.ui.open_dialog(DialogKind::Command, fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_the_title_bar_moves_the_dialog() {
        use egui_kittest::{Harness, kittest::Queryable};

        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let mut harness = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        harness.state_mut().ui.open_dialog(DialogKind::LayerStyle, serde_json::Map::new());
        harness.run_steps(3);
        let before = harness.get_by_label("Layer Style").rect();

        // Grab the title text itself: it must move the dialog, not select the text.
        let from = before.center();
        harness.hover_at(from);
        harness.drag_at(from);
        harness.run_steps(2);
        for i in 1..=10 {
            harness.hover_at(from + egui::vec2(-12.0, 8.0) * i as f32);
            harness.run_steps(1);
        }
        harness.drop_at(from + egui::vec2(-120.0, 80.0));
        harness.run_steps(3);

        let moved = harness.get_by_label("Layer Style").rect().min - before.min;
        assert!((moved - egui::vec2(-120.0, 80.0)).length() < 1.0, "dialog moved by {moved:?}");
        assert_eq!(harness.state().ui.dialogs.len(), 1, "dragging must not close the dialog");
    }
}

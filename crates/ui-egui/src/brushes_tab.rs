//! Window › Brushes: the brush presets in collapsible groups, as a list (tip, size, stroke
//! preview, name) or a grid of tips, with a size slider and a search field.
//!
//! Presets and groups are organised like Photoshop's panel: drag a preset within its group or
//! into another group (onto a preset, or onto a group's header to append), drag a group's header
//! to reorder groups, and right-click for Rename and Delete. Every change is a `brush.presets.*`
//! command, so it is journaled, drivable, and persisted by the preset store.

use egui::{Color32, RichText, Sense, Stroke, pos2, vec2};
use photocraft_engine::paint::BrushPreset;
use serde_json::json;

use crate::brush_panel::{BrushesView, Renaming, UNGROUPED, WIDTH, commit_gesture, full_uv, grouped_presets, is_current, new_preset_name, run_or_status};
use crate::brush_preview;
use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

/// What a drag in the Brushes tab carries.
#[derive(Clone, Debug, PartialEq)]
pub enum BrushDrag {
    Preset(String),
    Group(String),
}

/// What the tab does after drawing (the presets are borrowed while it draws).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Select(String),
    ToggleGroup(String),
    Move { name: String, group: String, index: Option<usize> },
    MoveGroup { group: String, before: Option<String> },
    Rename(Renaming),
    Delete(String),
    DeleteGroup(String),
}

/// The group key (`BrushPreset::group`) behind a panel label: ungrouped presets show as
/// [`UNGROUPED`].
pub fn group_key(presets: &[BrushPreset], label: &str) -> String {
    if label == UNGROUPED && !presets.iter().any(|p| p.group == UNGROUPED) { String::new() } else { label.to_string() }
}

/// Where a preset dropped onto `target` lands: (group, index in that group after the dragged
/// preset is taken out). `after` = dropped on the target's lower (list) or right (grid) half.
pub fn drop_target(presets: &[BrushPreset], dragged: &str, target: &str, after: bool) -> Option<(String, usize)> {
    let t = presets.iter().find(|p| p.name == target)?;
    let members: Vec<&str> = presets.iter().filter(|p| p.group == t.group).map(|p| p.name.as_str()).collect();
    let tp = members.iter().position(|n| *n == target)?;
    let mut idx = tp + usize::from(after);
    if let Some(dp) = members.iter().position(|n| *n == dragged)
        && dp < idx
    {
        idx -= 1;
    }
    Some((t.group.clone(), idx))
}

/// Turn the tab's actions into commands.
pub fn apply(app: &mut PhotocraftApp, acts: Vec<Action>) {
    for a in acts {
        match a {
            Action::Select(name) => run_or_status(app, "tools.setBrush", json!({ "preset": name })),
            Action::ToggleGroup(g) => {
                let c = &mut app.ui.brushes_panel.collapsed;
                match c.iter().position(|x| *x == g) {
                    Some(i) => drop(c.remove(i)),
                    None => c.push(g),
                }
            }
            Action::Move { name, group, index } => {
                let mut p = json!({ "name": name, "group": group });
                if let Some(i) = index {
                    p["index"] = json!(i);
                }
                run_or_status(app, "brush.presets.move", p);
            }
            Action::MoveGroup { group, before } => run_or_status(app, "brush.presets.moveGroup", json!({ "group": group, "before": before })),
            Action::Rename(r) => {
                let text = r.text.trim().to_string();
                if !text.is_empty() && text != r.name {
                    if r.group {
                        run_or_status(app, "brush.presets.renameGroup", json!({ "group": r.name, "newName": text }));
                    } else {
                        run_or_status(app, "brush.presets.rename", json!({ "name": r.name, "newName": text }));
                    }
                }
                app.ui.brushes_panel.renaming = None;
            }
            Action::Delete(name) => run_or_status(app, "brush.presets.delete", json!({ "name": name })),
            Action::DeleteGroup(g) => run_or_status(app, "brush.presets.deleteGroup", json!({ "group": g })),
        }
    }
}

/// An accent insertion line on one edge of `r` while a drag hovers it.
fn drop_line(ui: &egui::Ui, r: egui::Rect, after: bool, vertical: bool, t: &Tokens) {
    let s = Stroke::new(2.0, t.accent);
    if vertical {
        let x = if after { r.right() } else { r.left() };
        ui.painter().line_segment([pos2(x, r.top() + 2.0), pos2(x, r.bottom() - 2.0)], s);
    } else {
        let y = if after { r.bottom() } else { r.top() };
        ui.painter().line_segment([pos2(r.left() + 4.0, y), pos2(r.right() - 4.0, y)], s);
    }
}

/// Is the pointer on the second half of `r` (lower in a list, right in a grid)?
fn second_half(ui: &egui::Ui, r: egui::Rect, grid: bool) -> bool {
    ui.ctx().pointer_interact_pos().is_some_and(|p| if grid { p.x > r.center().x } else { p.y > r.center().y })
}

/// Drag, drop and context menu of one preset cell or row.
fn preset_interactions(ui: &egui::Ui, resp: &egui::Response, r: egui::Rect, p: &BrushPreset, presets: &[BrushPreset], grid: bool, acts: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    resp.dnd_set_drag_payload(BrushDrag::Preset(p.name.clone()));
    if let Some(d) = resp.dnd_hover_payload::<BrushDrag>()
        && matches!(&*d, BrushDrag::Preset(n) if *n != p.name)
    {
        drop_line(ui, r, second_half(ui, r, grid), grid, &t);
    }
    if let Some(d) = resp.dnd_release_payload::<BrushDrag>()
        && let BrushDrag::Preset(n) = &*d
        && *n != p.name
        && let Some((group, index)) = drop_target(presets, n, &p.name, second_half(ui, r, grid))
    {
        acts.push(Action::Move { name: n.clone(), group, index: Some(index) });
    }
    if resp.clicked() {
        acts.push(Action::Select(p.name.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button(tl!("Rename Brush…")).clicked() {
            acts.push(Action::Rename(Renaming { group: false, name: p.name.clone(), text: String::new() }));
            ui.close();
        }
        if ui.button(tl!("Delete Brush")).clicked() {
            acts.push(Action::Delete(p.name.clone()));
            ui.close();
        }
    });
}

fn list_row(ui: &mut egui::Ui, p: &BrushPreset, current: bool, presets: &[BrushPreset], acts: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click_and_drag());
    if !ui.is_rect_visible(r) {
        return;
    }
    if current {
        ui.painter().rect_filled(r, t.radius_sm, t.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, t.radius_sm, t.hover);
    }
    let pb = &p.brush;
    let tip = brush_preview::tip_texture(ui.ctx(), &format!("brushes-tip:{}", p.name), &pb.tip, (pb.hardness, pb.angle, pb.roundness), 36, t.text);
    let cell = egui::Rect::from_min_size(r.left_top() + vec2(22.0, 2.0), vec2(36.0, 30.0));
    let tr = egui::Rect::from_center_size(cell.center(), vec2(30.0, 30.0) * crate::brush_sections::thumb_scale(pb.size));
    ui.painter().image(tip.id(), tr, full_uv(), Color32::WHITE);
    ui.painter().text(
        pos2(cell.center().x, r.bottom() - 1.0),
        egui::Align2::CENTER_BOTTOM,
        format!("{}", pb.size.round() as i64),
        egui::FontId::proportional(9.0),
        t.text_faint,
    );
    let stroke = brush_preview::stroke_texture(ui.ctx(), &format!("brushes-stroke:{}", p.name), pb, 170, 36, t.text);
    let sr = egui::Rect::from_min_size(pos2(cell.right() + 8.0, r.top() + 4.0), vec2(170.0, 36.0));
    ui.painter().image(stroke.id(), sr, full_uv(), Color32::WHITE);
    ui.painter().text(pos2(sr.right() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, &p.name, egui::FontId::proportional(12.0), t.text_dim);
    preset_interactions(ui, &resp, r, p, presets, false, acts);
}

fn grid_cell(ui: &mut egui::Ui, p: &BrushPreset, current: bool, presets: &[BrushPreset], acts: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(52.0, 58.0), Sense::click_and_drag());
    if !ui.is_rect_visible(r) {
        return;
    }
    if current {
        ui.painter().rect_filled(r, 3.0, t.accent_soft);
        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    let pb = &p.brush;
    let tip = brush_preview::tip_texture(ui.ctx(), &format!("brushes-tip:{}", p.name), &pb.tip, (pb.hardness, pb.angle, pb.roundness), 36, t.text);
    let ir = egui::Rect::from_center_size(pos2(r.center().x, r.top() + 23.0), vec2(38.0, 38.0) * crate::brush_sections::thumb_scale(pb.size));
    ui.painter().image(tip.id(), ir, full_uv(), Color32::WHITE);
    ui.painter().text(
        pos2(r.center().x, r.bottom() - 7.0),
        egui::Align2::CENTER_CENTER,
        format!("{}", pb.size.round() as i64),
        egui::FontId::proportional(9.5),
        t.text_dim,
    );
    let resp = resp.on_hover_text(&p.name);
    preset_interactions(ui, &resp, r, p, presets, true, acts);
}

/// A group's header: chevron, folder, name, count. Click toggles; drag reorders groups; a
/// preset dropped here moves to the end of the group.
fn group_header(ui: &mut egui::Ui, label: &str, key: &str, open: bool, count: usize, acts: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click_and_drag());
    if resp.hovered() {
        ui.painter().rect_filled(r, t.radius_sm, t.hover);
    }
    let x = r.left() + 4.0;
    icons::paint(
        ui,
        egui::Rect::from_min_size(pos2(x, r.center().y - 7.0), vec2(14.0, 14.0)),
        if open { "chevron-down" } else { "chevron-right" },
        12.0,
        t.text_dim,
    );
    icons::paint(
        ui,
        egui::Rect::from_min_size(pos2(x + 18.0, r.center().y - 8.0), vec2(16.0, 16.0)),
        if open { "folder-open" } else { "folder" },
        14.0,
        t.icon,
    );
    ui.painter().text(pos2(x + 40.0, r.center().y), egui::Align2::LEFT_CENTER, label, theme::semibold(12.0), t.text);
    ui.painter().text(pos2(r.right() - 8.0, r.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), theme::medium(11.0), t.text_faint);
    resp.dnd_set_drag_payload(BrushDrag::Group(key.to_string()));
    if let Some(d) = resp.dnd_hover_payload::<BrushDrag>() {
        match &*d {
            BrushDrag::Group(g) if g != key => drop_line(ui, r, false, false, &t),
            BrushDrag::Preset(_) => {
                ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
            }
            _ => {}
        }
    }
    if let Some(d) = resp.dnd_release_payload::<BrushDrag>() {
        match &*d {
            BrushDrag::Group(g) if g != key => acts.push(Action::MoveGroup { group: g.clone(), before: Some(key.to_string()) }),
            BrushDrag::Preset(n) => acts.push(Action::Move { name: n.clone(), group: key.to_string(), index: None }),
            _ => {}
        }
    }
    if resp.clicked() {
        acts.push(Action::ToggleGroup(label.to_string()));
    }
    resp.context_menu(|ui| {
        if ui.button(tl!("Rename Group…")).clicked() {
            acts.push(Action::Rename(Renaming { group: true, name: key.to_string(), text: String::new() }));
            ui.close();
        }
        if ui.button(tl!("Delete Group")).clicked() {
            acts.push(Action::DeleteGroup(key.to_string()));
            ui.close();
        }
    });
}

/// The rename bar shown while a preset or group is being renamed. Enter or OK renames, Escape or
/// Cancel stops.
fn rename_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
    let Some(r) = app.ui.brushes_panel.renaming.as_mut() else { return };
    let t = Tokens::get(ui.ctx());
    if r.text.is_empty() {
        r.text = r.name.clone();
    }
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(if r.group { tl!("Group name") } else { tl!("Brush name") }).color(t.text_dim));
        let resp = ui.add(egui::TextEdit::singleline(&mut r.text).desired_width(WIDTH - 230.0).id_salt("brush-rename"));
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if widgets::primary_button(ui, tl!("OK"), 52.0).clicked() || enter {
            acts.push(Action::Rename(r.clone()));
        }
        if widgets::secondary_button(ui, tl!("Cancel"), 60.0).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
    });
    if cancel {
        app.ui.brushes_panel.renaming = None;
    }
    ui.add_space(4.0);
}

/// The Brushes tab.
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // Size of the current brush (Photoshop's Brushes panel slider).
    let before = app.session.tools.brush.clone();
    let mut b = before.clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Size")).color(t.text_dim));
        let mut lv = b.size.max(1.0).ln();
        ui.add_sized(vec2(WIDTH - 140.0, 18.0), |ui: &mut egui::Ui| {
            let r = widgets::slider(ui, &mut lv, 0.0..=5000f32.ln(), None);
            if r.changed() {
                b.size = lv.exp().round().clamp(1.0, 5000.0);
            }
            r
        });
        let mut s = b.size;
        if widgets::value_field(ui, &mut s, 1.0..=5000.0, "px", 74.0).changed() {
            b.size = s.round().clamp(1.0, 5000.0);
        }
    });
    commit_gesture(app, ui.ctx(), &before, &b);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        icons::paint(ui, egui::Rect::from_min_size(ui.cursor().min + vec2(0.0, 3.0), vec2(16.0, 16.0)), "search", 14.0, t.text_faint);
        ui.add_space(20.0);
        ui.add(egui::TextEdit::singleline(&mut app.ui.brushes_panel.filter).hint_text(tl!("Search Brushes")).desired_width(WIDTH - 120.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let view = &mut app.ui.brushes_panel.view;
            if icons::button(ui, "grid-2x2", 24.0, *view == BrushesView::Grid, "Grid view").clicked() {
                *view = BrushesView::Grid;
            }
            if icons::button(ui, "align-justify", 24.0, *view == BrushesView::List, "List view").clicked() {
                *view = BrushesView::List;
            }
        });
    });
    ui.add_space(6.0);
    let mut acts = Vec::new();
    rename_bar(app, ui, &mut acts);
    let filter = app.ui.brushes_panel.filter.trim().to_lowercase();
    let grid = app.ui.brushes_panel.view == BrushesView::Grid;
    let presets = &app.session.tools.presets;
    let groups = grouped_presets(presets);
    let brush = &app.session.tools.brush;
    let collapsed = &app.ui.brushes_panel.collapsed;
    egui::ScrollArea::vertical().id_salt("brush-presets").max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        for (label, items) in groups {
            let items: Vec<usize> =
                items.into_iter().filter(|i| filter.is_empty() || presets.get(*i).is_some_and(|p| p.name.to_lowercase().contains(&filter))).collect();
            if items.is_empty() {
                continue;
            }
            let key = group_key(presets, &label);
            let open = !filter.is_empty() || !collapsed.contains(&label);
            group_header(ui, &label, &key, open, items.len(), &mut acts);
            if !open {
                continue;
            }
            if grid {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    ui.add_space(20.0);
                    for i in items {
                        if let Some(p) = presets.get(i) {
                            grid_cell(ui, p, is_current(&p.brush, brush), presets, &mut acts);
                        }
                    }
                });
            } else {
                for i in items {
                    if let Some(p) = presets.get(i) {
                        list_row(ui, p, is_current(&p.brush, brush), presets, &mut acts);
                    }
                }
            }
            ui.add_space(2.0);
        }
    });
    // The dragged preset or group follows the pointer.
    if let Some(d) = egui::DragAndDrop::payload::<BrushDrag>(ui.ctx())
        && let Some(p) = ui.ctx().pointer_interact_pos()
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let name = match &*d {
            BrushDrag::Preset(n) => n.clone(),
            BrushDrag::Group(g) => {
                if g.is_empty() {
                    UNGROUPED.to_string()
                } else {
                    g.clone()
                }
            }
        };
        egui::Area::new(egui::Id::new("brush-drag-label")).order(egui::Order::Tooltip).fixed_pos(p + vec2(12.0, 8.0)).interactable(false).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(name));
            },
        );
    }
    apply(app, acts);
    ui.add_space(4.0);
    widgets::hairline(ui);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} presets", app.session.tools.presets.len())).color(t.text_faint));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let current = app.session.tools.presets.iter().find(|p| is_current(&p.brush, &app.session.tools.brush)).map(|p| p.name.clone());
            if ui.add_enabled_ui(current.is_some(), |ui| icons::button(ui, "trash", 24.0, false, "Delete brush")).inner.clicked()
                && let Some(name) = current
            {
                run_or_status(app, "brush.presets.delete", json!({ "name": name }));
            }
            if icons::button(ui, "square-plus", 24.0, false, "Create new brush from the current settings").clicked() {
                let name = new_preset_name(&app.session.tools.presets);
                run_or_status(app, "brush.presets.save", json!({ "name": name }));
            }
            if icons::button(ui, "folder-open", 24.0, false, "Import Brushes… (.abr)").clicked() {
                app.open_dialog_file();
            }
        });
    });
    // Forget previews of presets that no longer exist.
    let names: std::collections::HashSet<&str> = app.session.tools.presets.iter().map(|p| p.name.as_str()).collect();
    brush_preview::with_cache(ui.ctx(), |c| {
        c.retain(|slot| slot.split_once(':').is_none_or(|(_, n)| names.contains(n)));
    });
}

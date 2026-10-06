//! Photoshop's Brush Preset picker: the options-bar brush chip's popup and the picker a
//! right-click on the canvas opens (`paint_mouse`). It is the quick picker (Size, Hardness and the
//! preset library in its groups); the full editor is the Brush Settings panel (F5), which the
//! picker and the options bar open with one click (#258).
//!
//! The picker edits a copy of the brush; the caller sends the size and hardness edits through
//! `tools.setBrush` ([`crate::brush_panel::commit_gesture`]) and applies the returned [`Pick`]
//! with [`apply`], so every change is a journaled command (Rule 1).

use std::collections::BTreeSet;

use egui::{Color32, RichText, Sense, Stroke, pos2, vec2};
use photocraft_engine::BrushSettings;
use photocraft_engine::paint::BrushPreset;
use serde_json::json;

use crate::brush_panel::{full_uv, grouped_presets, is_current, new_preset_name, run_or_status};
use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, brush_preview, icons, widgets};

/// Width of the picker's contents.
pub const WIDTH: f32 = 300.0;
/// A preset cell in the picker's grid.
const CELL: egui::Vec2 = vec2(44.0, 50.0);

/// What a click in the picker asks for beyond the size and hardness edits.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// Pick a preset (`tools.setBrush {"preset": name}`).
    Preset(String),
    /// Open the Brush Settings panel (Window › Brush Settings, F5).
    OpenSettings,
    /// Save the current brush as a new preset.
    NewPreset,
}

/// Picker view state kept in egui memory: the name filter and the collapsed groups.
#[derive(Clone, Debug, Default)]
struct View {
    filter: String,
    collapsed: BTreeSet<String>,
}

fn view_id() -> egui::Id {
    egui::Id::new("brush-picker-view")
}

/// Run what the picker asked for.
pub fn apply(app: &mut PhotocraftApp, ctx: &egui::Context, pick: Option<Pick>) {
    match pick {
        None => {}
        Some(Pick::Preset(name)) => run_or_status(app, "tools.setBrush", json!({ "preset": name })),
        Some(Pick::OpenSettings) => open_settings(app, ctx),
        Some(Pick::NewPreset) => {
            let name = new_preset_name(&app.session.tools.presets);
            run_or_status(app, "brush.presets.save", json!({ "name": name }));
        }
    }
}

/// Show the Brush Settings panel on its settings tab (what F5 does when it is hidden).
pub fn open_settings(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.panels.brush_settings {
        app.ui.brush_tab = 0;
        return;
    }
    if let Err(e) = crate::menus::invoke(app, ctx, "window.panel.brushSettings", json!({})) {
        app.ui.status = e;
    }
}

/// The options-bar button beside the brush chip that shows and hides the Brush Settings panel
/// (Photoshop's "Toggle the Brush Settings panel"). It runs Window › Brush Settings (F5).
pub fn settings_toggle(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let on = app.ui.panels.brush_settings;
    if named(icons::button(ui, "sliders-horizontal", 24.0, on, "Toggle the Brush Settings panel  (F5)"), "Toggle the Brush Settings panel").clicked()
        && let Err(e) = crate::menus::invoke(app, ui.ctx(), "window.panel.brushSettings", json!({}))
    {
        app.ui.status = e;
    }
}

/// An icon button's accessible name (icon buttons have only a tooltip).
fn named(resp: egui::Response, label: &str) -> egui::Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    resp
}

/// The picker's contents: Size, Hardness, a search field with the Brush Settings and New Preset
/// buttons, and the presets in their groups as tip thumbnails.
pub fn body(ui: &mut egui::Ui, b: &mut BrushSettings, presets: &[BrushPreset]) -> Option<Pick> {
    let t = Tokens::get(ui.ctx());
    let mut pick = None;
    ui.set_width(WIDTH);
    // Size: value field plus a logarithmic slider (small sizes get most of the travel).
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Size")).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut size = b.size;
            if widgets::value_field(ui, &mut size, 1.0..=5000.0, "px", 72.0).changed() {
                b.size = size.round().clamp(1.0, 5000.0);
            }
        });
    });
    let mut lv = b.size.max(1.0).ln();
    if widgets::slider(ui, &mut lv, 0.0..=5000f32.ln(), None).changed() {
        b.size = lv.exp().round().clamp(1.0, 5000.0);
    }
    let mut hard = b.hardness * 100.0;
    if widgets::slider_row(ui, "Hardness", &mut hard, 0.0..=100.0, "%", None).changed() {
        b.hardness = (hard / 100.0).clamp(0.0, 1.0);
    }
    ui.add_space(6.0);
    widgets::hairline(ui);
    ui.add_space(6.0);
    let mut view = ui.data(|d| d.get_temp::<View>(view_id())).unwrap_or_default();
    ui.horizontal(|ui| {
        icons::paint(ui, egui::Rect::from_min_size(ui.cursor().min + vec2(0.0, 3.0), vec2(16.0, 16.0)), "search", 14.0, t.text_faint);
        ui.add_space(20.0);
        ui.add(egui::TextEdit::singleline(&mut view.filter).hint_text(tl!("Search Brushes")).desired_width(WIDTH - 90.0).id_salt("brush-picker-search"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if named(icons::button(ui, "square-plus", 24.0, false, "Create new brush preset from the current settings"), "New Brush Preset").clicked() {
                pick = Some(Pick::NewPreset);
            }
            if named(icons::button(ui, "sliders-horizontal", 24.0, false, "Brush Settings…  (F5): every option of this brush"), "Brush Settings…").clicked()
            {
                pick = Some(Pick::OpenSettings);
            }
        });
    });
    ui.add_space(4.0);
    let filter = view.filter.trim().to_lowercase();
    egui::ScrollArea::vertical().id_salt("brush-picker-presets").max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        for (label, items) in grouped_presets(presets) {
            let items: Vec<&BrushPreset> =
                items.into_iter().filter_map(|i| presets.get(i)).filter(|p| filter.is_empty() || p.name.to_lowercase().contains(&filter)).collect();
            if items.is_empty() {
                continue;
            }
            let open = !filter.is_empty() || !view.collapsed.contains(&label);
            if group_header(ui, &label, open, items.len()) && !view.collapsed.remove(&label) {
                view.collapsed.insert(label.clone());
            }
            if !open {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                ui.add_space(4.0);
                for p in items {
                    if cell(ui, p, is_current(&p.brush, b)) {
                        pick = Some(Pick::Preset(p.name.clone()));
                    }
                }
            });
            ui.add_space(3.0);
        }
        if presets.is_empty() {
            ui.label(RichText::new(tl!("No brush presets")).color(t.text_faint));
        }
    });
    ui.data_mut(|d| d.insert_temp(view_id(), view));
    pick
}

/// A group's header row: chevron, folder, name, count. Returns true when clicked (toggle).
fn group_header(ui: &mut egui::Ui, label: &str, open: bool, count: usize) -> bool {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, t.radius_sm, t.hover);
    }
    let x = r.left() + 2.0;
    let chevron = if open { "chevron-down" } else { "chevron-right" };
    icons::paint(ui, egui::Rect::from_min_size(pos2(x, r.center().y - 7.0), vec2(14.0, 14.0)), chevron, 12.0, t.text_dim);
    let folder = if open { "folder-open" } else { "folder" };
    icons::paint(ui, egui::Rect::from_min_size(pos2(x + 16.0, r.center().y - 8.0), vec2(16.0, 16.0)), folder, 14.0, t.icon);
    ui.painter().text(pos2(x + 37.0, r.center().y), egui::Align2::LEFT_CENTER, label, theme::semibold(12.0), t.text);
    ui.painter().text(pos2(r.right() - 6.0, r.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), theme::medium(11.0), t.text_faint);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, label));
    resp.clicked()
}

/// One preset: its tip thumbnail (scaled by size) and its size. Returns true when clicked.
fn cell(ui: &mut egui::Ui, p: &BrushPreset, current: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(CELL, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, current, &p.name));
    if !ui.is_rect_visible(r) {
        return resp.clicked();
    }
    if current {
        ui.painter().rect_filled(r, 3.0, t.accent_soft);
        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    } else {
        ui.painter().rect_filled(r, 3.0, t.field);
    }
    let pb = &p.brush;
    // The Brushes tab's thumbnail slot, so both share one cached texture per preset.
    let tip = brush_preview::tip_texture(ui.ctx(), &format!("brushes-tip:{}", p.name), &pb.tip, (pb.hardness, pb.angle, pb.roundness), 36, t.text);
    let ir = egui::Rect::from_center_size(pos2(r.center().x, r.top() + 20.0), vec2(32.0, 32.0) * crate::brush_sections::thumb_scale(pb.size));
    ui.painter().image(tip.id(), ir, full_uv(), Color32::WHITE);
    ui.painter().text(
        pos2(r.center().x, r.bottom() - 7.0),
        egui::Align2::CENTER_CENTER,
        format!("{}", pb.size.round() as i64),
        egui::FontId::proportional(9.5),
        t.text_dim,
    );
    resp.on_hover_text(&p.name).clicked()
}

//! Chrome around the canvas: title bar, options bar, toolbar, status bar, dock cards, Properties.

use egui::{Align2, Color32, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use photocraft_color::BlendMode;
use photocraft_doc::{Layer, LayerContent, LayerId};
use serde_json::{Value, json};

use crate::state::Tool;
use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

// ----------------------------------------------------------------------------- toolbar

/// Tool groups separated by hairlines, Photoshop order.
/// Photoshop toolbar: divider-separated sections of slots; each slot is a flyout group whose button
/// shows the group's last-used tool.
const TOOL_SECTIONS: &[&[&[Tool]]] = &[
    &[&[Tool::Move]],
    &[
        &[Tool::RectMarquee, Tool::EllipseMarquee],
        &[Tool::Lasso, Tool::PolygonLasso],
        &[Tool::ObjectSelection, Tool::QuickSelection, Tool::MagicWand],
        &[Tool::Crop, Tool::Slice, Tool::SliceSelect],
        &[Tool::Eyedropper, Tool::Ruler, Tool::Note, Tool::Count],
    ],
    &[
        &[Tool::SpotHealing, Tool::Healing],
        &[Tool::Brush, Tool::Pencil],
        &[Tool::CloneStamp],
        &[Tool::HistoryBrush],
        &[Tool::Eraser, Tool::BackgroundEraser, Tool::MagicEraser],
        &[Tool::Gradient, Tool::PaintBucket],
        &[Tool::Blur, Tool::Sharpen, Tool::Smudge],
        &[Tool::Dodge, Tool::Burn, Tool::Sponge],
    ],
    &[
        &[Tool::Pen],
        &[Tool::Type],
        &[Tool::PathSelection],
        &[Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line, Tool::CustomShape],
    ],
    &[&[Tool::Hand], &[Tool::Zoom]],
];

/// The tool a slot shows: the current tool if it belongs to the slot, else the last one used.
fn slot_tool(ui: &egui::Ui, current: Tool, slot: &[Tool], key: egui::Id) -> Tool {
    if slot.contains(&current) {
        ui.data_mut(|d| d.insert_temp(key, current));
        return current;
    }
    ui.data(|d| d.get_temp::<Tool>(key)).filter(|t| slot.contains(t)).unwrap_or(slot[0])
}

pub fn toolbar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (w1, bx, m) = if t.pro { (40.0, 30.0, 5i8) } else { (50.0, 36.0, 7i8) };
    // Photoshop switches to a double-column toolbar only when one column doesn't fit.
    let slots: usize = TOOL_SECTIONS.iter().map(|g| g.len()).sum();
    let double = toolbar_needs_double(slots, TOOL_SECTIONS.len(), bx, t.pro, ui.available_rect_before_wrap().height());
    let w = if double { w1 + bx + 2.0 } else { w1 };
    egui::Panel::left("toolbar").resizable(false).exact_size(w).frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(m, 8))).show(
        ui,
        |ui| {
            if t.pro {
                let r = ui.max_rect();
                ui.painter().line_segment([r.right_top() + vec2(m as f32, -8.0), r.right_bottom() + vec2(m as f32, 8.0)], Stroke::new(1.0, t.separator));
                // collapse chevrons like Photoshop's toolbar header
                let (cr, _) = ui.allocate_exact_size(vec2(bx, 14.0), Sense::hover());
                icons::paint(ui, cr, "chevrons-right", 11.0, t.text_faint);
                ui.add_space(4.0);
            }
            // Subtle violet wash at the bottom of the toolbar.
            let full = ui.max_rect();
            if !t.bevel && !t.pro && t.dark() {
                let mut mesh = egui::Mesh::default();
                let r = Rect::from_min_max(pos2(full.left() - 7.0, full.bottom() - 260.0), pos2(full.right() + 7.0, full.bottom() + 8.0));
                let top = Color32::TRANSPARENT;
                let bottom = Color32::from_rgba_unmultiplied(90, 70, 190, 34);
                mesh.colored_vertex(r.left_top(), top);
                mesh.colored_vertex(r.right_top(), top);
                mesh.colored_vertex(r.right_bottom(), bottom);
                mesh.colored_vertex(r.left_bottom(), bottom);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                ui.painter().add(mesh);
            }
            ui.spacing_mut().item_spacing = vec2(2.0, 3.0);
            let flyout_id = egui::Id::new("tool-flyout");
            let mut slot_index = 0usize;
            for (si, section) in TOOL_SECTIONS.iter().enumerate() {
                // Photoshop 2026 draws one uninterrupted column (no group dividers).
                if si > 0 && !t.pro {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(vec2(if double { bx * 2.0 + 2.0 } else { bx }, 1.0), Sense::hover());
                    ui.painter().line_segment([r.left_center() + vec2(8.0, 0.0), r.right_center() - vec2(8.0, 0.0)], Stroke::new(1.0, t.separator));
                    ui.add_space(4.0);
                }
                let rows: Vec<&[&[Tool]]> = if double { section.chunks(2).collect() } else { section.chunks(1).collect() };
                for row in rows {
                    ui.horizontal(|ui| {
                        for slot in row.iter() {
                            let key = egui::Id::new(("tool-slot", slot_index));
                            slot_index += 1;
                            let tool = slot_tool(ui, app.ui.tool, slot, key);
                            let sel = slot.contains(&app.ui.tool);
                            let tip = if tool.key() == '\0' { tl!(tool.label()).to_string() } else { format!("{}  ({})", tl!(tool.label()), tool.key()) };
                            let resp = icons::button(ui, icons::tool_icon(tool), bx, sel, &tip);
                            if slot.len() > 1 {
                                let r = resp.rect;
                                let tri = vec![r.right_bottom() + vec2(-2.0, -2.0), r.right_bottom() + vec2(-6.0, -2.0), r.right_bottom() + vec2(-2.0, -6.0)];
                                ui.painter().add(egui::Shape::convex_polygon(tri, t.text_faint, Stroke::NONE));
                            }
                            if resp.clicked() {
                                app.ui.tool = tool;
                            }
                            // Right-click or long-press opens the flyout (Photoshop).
                            let long_press =
                                resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.press_start_time().is_some_and(|t0| i.time - t0 > 0.35));
                            if slot.len() > 1 && (resp.secondary_clicked() || long_press) {
                                ui.data_mut(|d| d.insert_temp(flyout_id, (key, resp.rect)));
                            }
                            if slot.len() > 1 && long_press {
                                ui.ctx().request_repaint();
                            }
                            if let Some((open_key, anchor)) = ui.data(|d| d.get_temp::<(egui::Id, Rect)>(flyout_id))
                                && open_key == key
                            {
                                let area = egui::Area::new(key.with("flyout"))
                                    .order(egui::Order::Foreground)
                                    .fixed_pos(anchor.right_top() + vec2(6.0, 0.0))
                                    .show(ui.ctx(), |ui| {
                                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                                            let label_w = slot
                                                .iter()
                                                .map(|it| {
                                                    ui.painter().layout_no_wrap(tl!(it.label()).to_string(), egui::FontId::proportional(12.5), t.text).size().x
                                                })
                                                .fold(0.0f32, f32::max);
                                            let fw = (label_w + 42.0 + 40.0).max(180.0);
                                            for &item in slot.iter() {
                                                let (r, ir) = ui.allocate_exact_size(vec2(fw, 26.0), Sense::click());
                                                if ir.hovered() {
                                                    ui.painter().rect_filled(r, 3.0, t.hover);
                                                }
                                                if item == tool {
                                                    ui.painter().rect_filled(
                                                        Rect::from_center_size(pos2(r.left() + 8.0, r.center().y), vec2(4.0, 4.0)),
                                                        0.0,
                                                        t.text,
                                                    );
                                                }
                                                icons::paint(
                                                    ui,
                                                    Rect::from_center_size(pos2(r.left() + 26.0, r.center().y), vec2(18.0, 18.0)),
                                                    icons::tool_icon(item),
                                                    14.0,
                                                    t.icon,
                                                );
                                                ui.painter().text(
                                                    pos2(r.left() + 42.0, r.center().y),
                                                    Align2::LEFT_CENTER,
                                                    tl!(item.label()),
                                                    egui::FontId::proportional(12.5),
                                                    t.text,
                                                );
                                                if item.key() != '\0' {
                                                    ui.painter().text(
                                                        pos2(r.right() - 8.0, r.center().y),
                                                        Align2::RIGHT_CENTER,
                                                        item.key().to_string(),
                                                        egui::FontId::proportional(12.0),
                                                        t.text_dim,
                                                    );
                                                }
                                                if ir.clicked() {
                                                    app.ui.tool = item;
                                                    ui.data_mut(|d| d.remove::<(egui::Id, Rect)>(flyout_id));
                                                }
                                            }
                                        });
                                    });
                                let clicked_outside = ui.input(|i| i.pointer.any_click()) && !area.response.hovered() && !resp.hovered();
                                if clicked_outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    ui.data_mut(|d| d.remove::<(egui::Id, Rect)>(flyout_id));
                                }
                            }
                        }
                    });
                }
            }
            let ctx = ui.ctx().clone();
            if t.pro && icons::button(ui, "ellipsis", bx, false, tl!("Edit Toolbar…")).clicked() {
                let _ = crate::menus::invoke(app, &ctx, "edit.toolbar", json!({}));
            }
            ui.add_space(if t.pro { 8.0 } else { 14.0 });
            color_chips(app, ui);
            if t.pro {
                ui.add_space(8.0);
                let quick_mask = app.session.active().is_some_and(|s| s.doc.quick_mask.is_some());
                if icons::button(
                    ui,
                    "square-dashed",
                    bx,
                    quick_mask,
                    if quick_mask { tl!("Edit in Standard Mode  (Q)") } else { tl!("Edit in Quick Mask Mode  (Q)") },
                )
                .clicked()
                {
                    let _ = crate::menus::invoke(app, &ctx, "select.editInQuickMaskMode", json!({}));
                }
                let sm = icons::button(ui, "app-window", bx, false, tl!("Change Screen Mode  (F)"));
                if sm.clicked() {
                    let _ = crate::menus::invoke(app, &ctx, "view.screenMode.cycle", json!({}));
                }
                // Right-click (or long-press) lists the modes, like Photoshop's flyout.
                egui::Popup::context_menu(&sm).show(|ui| {
                    ui.set_min_width(220.0);
                    for (id, label) in [
                        ("view.screenMode.standard", tl!("Standard Screen Mode")),
                        ("view.screenMode.fullScreenWithMenuBar", tl!("Full Screen Mode With Menu Bar")),
                        ("view.screenMode.fullScreen", tl!("Full Screen Mode")),
                    ] {
                        let on = crate::view_cmds::checked(app, id).unwrap_or(false);
                        if ui.add(egui::Button::selectable(on, label)).clicked() {
                            let _ = crate::menus::invoke(app, &ctx, id, json!({}));
                            ui.close();
                        }
                    }
                });
            }
        },
    );
}

/// Does the toolbar need two columns? Height of one column (header, tool slots, Edit Toolbar,
/// colour chips, Quick Mask and Screen Mode) against the height the toolbar gets.
pub fn toolbar_needs_double(slots: usize, sections: usize, bx: f32, pro: bool, avail_h: f32) -> bool {
    let pitch = bx + 3.0;
    let needed = if pro {
        // margins + header + slots + "…" + gap + chips (38 + swap row) + gap + 2 buttons
        16.0 + 21.0 + (slots + 1) as f32 * pitch + 8.0 + 59.0 + 8.0 + 2.0 * pitch
    } else {
        16.0 + slots as f32 * pitch + sections.saturating_sub(1) as f32 * 12.0 + 14.0 + 59.0
    };
    needed > avail_h
}

fn c32(c: [f32; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, (c[3] * 255.0) as u8)
}

fn color_chips(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 38.0), Sense::hover());
    let bg = Rect::from_min_size(rect.min + vec2(13.0, 13.0), vec2(21.0, 21.0));
    let fg = Rect::from_min_size(rect.min + vec2(2.0, 2.0), vec2(21.0, 21.0));
    let p = ui.painter();
    p.rect_filled(bg, 5.0, c32(app.session.tools.background));
    p.rect_stroke(bg, 5.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
    p.rect_filled(fg, 5.0, c32(app.session.tools.foreground));
    p.rect_stroke(fg, 5.0, Stroke::new(1.5, t.chrome), StrokeKind::Outside);
    p.rect_stroke(fg, 5.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    // Photoshop: clicking a chip opens the Color Picker for that colour.
    let bg_resp = ui.interact(bg, ui.id().with("bgchip"), Sense::click());
    let fg_resp = ui.interact(fg, ui.id().with("fgchip"), Sense::click());
    if fg_resp.on_hover_text(tl!("Set foreground color")).clicked() {
        crate::color_picker_ui::open(app, "foreground");
    } else if bg_resp.on_hover_text(tl!("Set background color")).clicked() {
        crate::color_picker_ui::open(app, "background");
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if icons::button(ui, "arrow-left-right", 18.0, false, tl!("Swap colours (X)")).clicked() {
            let _ = app.run("tools.swapColors", json!({}));
        }
        if icons::button(ui, "contrast", 18.0, false, tl!("Default colours (D)")).clicked() {
            let _ = app.run("tools.defaultColors", json!({}));
        }
    });
}

// ----------------------------------------------------------------------------- title bar

pub fn title_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let left = if cfg!(target_os = "macos") && app.integrated_titlebar { 78 } else { 10 };
    egui::Panel::top("title_bar")
        .exact_size(if t.pro { 32.0 } else { 38.0 })
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left, right: 10, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let drag = ui.interact(full, ui.id().with("titledrag"), Sense::click_and_drag());
            if drag.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            let title = app.session.active().map(|d| format!("{}{}", d.doc.name, if d.is_dirty() { "  •" } else { "" })).unwrap_or_else(|| "PhotoCraft".into());
            // The menus and the right-hand controls are laid out first; the title is centred in
            // whatever room is left between them, shortened or dropped rather than drawn over them.
            let (mut menus_right, mut controls_left) = (full.left(), full.right());
            ui.horizontal_centered(|ui| {
                menus_right = crate::menus::menu_bar(app, ui);
                controls_left = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut ws = app.ui.workspace.clone();
                        let opts = [
                            // Keys stay English (`apply_workspace` matches them); the dropdown
                            // shows the labels in the UI language.
                            ("Essentials".to_string(), tl!("Essentials")),
                            ("Photography".to_string(), tl!("Photography")),
                            ("Painting".to_string(), tl!("Painting")),
                            ("Graphic and Web".to_string(), tl!("Graphic and Web")),
                        ];
                        if widgets::dropdown(ui, "workspace", &mut ws, &opts, 130.0) {
                            app.ui.workspace = ws;
                            crate::menus::apply_workspace(app);
                        }
                        if icons::button(ui, "search", 28.0, app.ui.palette_open, &crate::shortcuts::tip_label(app, "Search commands", "edit.search")).clicked()
                        {
                            app.ui.palette_open = !app.ui.palette_open;
                        }
                        let theme_icon = if t.dark() { "sun" } else { "moon" };
                        if icons::button(ui, theme_icon, 28.0, false, tl!("Switch theme")).clicked() {
                            let next = app.ui.theme.next();
                            app.set_theme(ui.ctx(), next);
                        }
                        // Always one click away: the community Discord.
                        let discord = egui::Button::image_and_text(
                            icons::image("message-square", 14.0, t.text_dim),
                            egui::RichText::new(tl!("Discord")).color(t.text_dim).size(12.0),
                        )
                        .frame(false);
                        if ui.add(discord).on_hover_text(format!("Join the ArtCraft Discord ({})", crate::links::DISCORD)).clicked() {
                            crate::links::open(app, ui.ctx(), crate::links::DISCORD);
                        }
                        ui.min_rect().left()
                    })
                    .inner;
            });
            let font = theme::medium(13.0);
            let galley = ui.painter().layout_no_wrap(title.clone(), font.clone(), t.text_dim);
            if let Some(x) = title_x(full.center().x, menus_right, controls_left, galley.size().x) {
                ui.painter().galley(egui::pos2(x, full.center().y - galley.size().y / 2.0), galley, t.text_dim);
            } else if controls_left - menus_right > 80.0 {
                // Too narrow for the whole name: show the start of it, elided, in the free gap.
                let avail = controls_left - menus_right - 2.0 * TITLE_GAP;
                let mut job = egui::text::LayoutJob::simple_singleline(title, font, t.text_dim);
                job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(egui::pos2(menus_right + TITLE_GAP, full.center().y - g.size().y / 2.0), g, t.text_dim);
            }
        });
}

/// Minimum space kept between the window title and the menus or controls beside it.
const TITLE_GAP: f32 = 16.0;

/// Left edge for a title `width` wide: centred on `center` when it fits between the menus
/// (ending at `menus_right`) and the right-hand controls (starting at `controls_left`), else
/// slid into that gap; None when the gap is too small for the whole title.
fn title_x(center: f32, menus_right: f32, controls_left: f32, width: f32) -> Option<f32> {
    let (lo, hi) = (menus_right + TITLE_GAP, controls_left - TITLE_GAP - width);
    if !(lo.is_finite() && hi.is_finite() && width.is_finite()) || hi < lo {
        return None;
    }
    if !center.is_finite() {
        return Some(lo);
    }
    Some((center - width / 2.0).clamp(lo, hi))
}

#[cfg(test)]
mod title_tests {
    use super::title_x;

    #[test]
    fn title_never_overlaps_menus_or_controls() {
        // Wide window: centred.
        assert_eq!(title_x(800.0, 420.0, 1300.0, 60.0), Some(770.0));
        // Centre would hit the menus: slid right into the gap.
        assert_eq!(title_x(400.0, 420.0, 1300.0, 60.0), Some(436.0));
        // Centre would hit the controls: slid left.
        assert_eq!(title_x(1280.0, 420.0, 1300.0, 60.0), Some(1224.0));
        // No room for the whole title: the caller elides or drops it.
        assert_eq!(title_x(400.0, 420.0, 480.0, 60.0), None);
        assert_eq!(title_x(f32::NAN, 420.0, 1300.0, 60.0), Some(436.0));
        assert_eq!(title_x(400.0, f32::INFINITY, 1300.0, 60.0), None);
    }
}

// ----------------------------------------------------------------------------- options bar

pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("options_bar")
        .exact_size(if t.pro { 36.0 } else { 42.0 })
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
            ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.separator));
            ui.horizontal_centered(|ui| {
                if t.pro {
                    crate::chrome_ui::home_button(app, ui);
                    widgets::vline(ui, 22.0);
                }
                let _ = icons::button(ui, icons::tool_icon(app.ui.tool), if t.pro { 26.0 } else { 28.0 }, !t.pro, tl!(app.ui.tool.label()));
                if t.pro {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 20.0), Sense::hover());
                    icons::paint(ui, r, "chevron-down", 10.0, t.text_faint);
                }
                widgets::vline(ui, 22.0);
                if app.ui.transform.is_some() {
                    crate::transform_tool::options_bar(app, ui);
                    return;
                }
                if crate::distort_ui::options_bar(app, ui) {
                    return;
                }
                let tool = app.ui.tool;
                // Brush edits here go through `tools.setBrush`, one journal entry per gesture (Rule 1).
                if (tool.is_brushlike() && !matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser)) || tool == Tool::QuickSelection {
                    let before = app.session.tools.brush.clone();
                    let mut b = before.clone();
                    let pick = brush_preset_chip(ui, &mut b, &app.session.tools.presets);
                    crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &b);
                    crate::brush_picker::apply(app, ui.ctx(), pick);
                    crate::brush_picker::settings_toggle(app, ui);
                    widgets::vline(ui, 22.0);
                }
                if crate::eraser_ui::options_bar(app, ui, tool)
                    || crate::retouch_ui::options_bar(app, ui, tool)
                    || crate::vector_ui::options_bar(app, ui, tool)
                    || crate::analysis_ui::options_bar(app, ui, tool)
                    || crate::slice_ui::options_bar(app, ui, tool)
                {
                    return;
                }
                crate::paint_mouse::sync_tool_smoothing(app);
                let brush_before = app.session.tools.brush.clone();
                let mut brush = brush_before.clone();
                let b = &mut brush;
                let mut picked = None;
                match app.ui.tool {
                    Tool::Brush | Tool::Eraser if t.pro => {
                        picked = brush_preset_chip(ui, b, &app.session.tools.presets);
                        crate::brush_picker::settings_toggle(app, ui);
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Mode"));
                        let mut mode = b.mode;
                        let opts: Vec<(BlendMode, &str)> = BlendMode::LAYER_MODES.iter().map(|m| (*m, m.label())).collect();
                        if widgets::dropdown(ui, "brush-mode", &mut mode, &opts, 96.0) {
                            b.mode = mode;
                        }
                        opt_label(ui, tl!("Opacity"));
                        let mut o = b.opacity * 100.0;
                        if widgets::value_field(ui, &mut o, 0.0..=100.0, "%", 62.0).changed() {
                            b.opacity = o / 100.0;
                        }
                        if icons::button(ui, "circle-dot", 24.0, b.pressure_opacity, tl!("Always use pressure for opacity")).clicked() {
                            b.pressure_opacity = !b.pressure_opacity;
                        }
                        opt_label(ui, tl!("Flow"));
                        let mut f = b.flow * 100.0;
                        if widgets::value_field(ui, &mut f, 1.0..=100.0, "%", 62.0).changed() {
                            b.flow = f / 100.0;
                        }
                        let _ = icons::button(ui, "sparkles", 24.0, false, tl!("Enable airbrush-style build-up effects"));
                        opt_label(ui, tl!("Smoothing"));
                        smoothing_field(ui, b, 58.0);
                        let _ = icons::button(ui, "settings", 24.0, false, tl!("Set additional smoothing options"));
                        widgets::vline(ui, 22.0);
                        if icons::button(ui, "circle-dot", 24.0, b.pressure_size, tl!("Always use pressure for size")).clicked() {
                            b.pressure_size = !b.pressure_size;
                        }
                        let _ = icons::button(ui, "arrow-left-right", 24.0, false, tl!("Set painting symmetry options"));
                    }
                    // Pencil: Photoshop's options (no hardness or flow: the pencil is always hard).
                    Tool::Pencil => {
                        picked = brush_preset_chip(ui, b, &app.session.tools.presets);
                        crate::brush_picker::settings_toggle(app, ui);
                        widgets::vline(ui, 22.0);
                        if !t.pro {
                            opt_label(ui, tl!("Size"));
                            widgets::value_field(ui, &mut b.size, 1.0..=5000.0, "px", 76.0);
                            widgets::vline(ui, 22.0);
                        }
                        opt_label(ui, tl!("Mode"));
                        let mut mode = b.mode;
                        let opts: Vec<(BlendMode, &str)> = BlendMode::LAYER_MODES.iter().map(|m| (*m, m.label())).collect();
                        if widgets::dropdown(ui, "pencil-mode", &mut mode, &opts, 96.0) {
                            b.mode = mode;
                        }
                        opt_label(ui, tl!("Opacity"));
                        let mut o = b.opacity * 100.0;
                        if widgets::value_field(ui, &mut o, 0.0..=100.0, "%", if t.pro { 62.0 } else { 66.0 }).changed() {
                            b.opacity = o / 100.0;
                        }
                        opt_label(ui, tl!("Smoothing"));
                        smoothing_field(ui, b, if t.pro { 58.0 } else { 66.0 });
                        widgets::vline(ui, 22.0);
                        widgets::checkbox(ui, &mut app.ui.tool_options.pencil_auto_erase, tl!("Auto Erase"));
                    }
                    Tool::Brush | Tool::Eraser => {
                        picked = brush_preset_chip(ui, b, &app.session.tools.presets);
                        crate::brush_picker::settings_toggle(app, ui);
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Size"));
                        widgets::value_field(ui, &mut b.size, 1.0..=2500.0, "px", 76.0);
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Hardness"));
                        let mut h = b.hardness * 100.0;
                        if widgets::value_field(ui, &mut h, 0.0..=100.0, "%", 66.0).changed() {
                            b.hardness = h / 100.0;
                        }
                        opt_label(ui, tl!("Opacity"));
                        let mut o = b.opacity * 100.0;
                        if widgets::value_field(ui, &mut o, 0.0..=100.0, "%", 66.0).changed() {
                            b.opacity = o / 100.0;
                        }
                        opt_label(ui, tl!("Flow"));
                        let mut f = b.flow * 100.0;
                        if widgets::value_field(ui, &mut f, 1.0..=100.0, "%", 66.0).changed() {
                            b.flow = f / 100.0;
                        }
                        opt_label(ui, tl!("Smoothing"));
                        smoothing_field(ui, b, 66.0);
                        widgets::vline(ui, 22.0);
                        widgets::toggle(ui, &mut b.pressure_size, tl!("Pressure for Size"));
                        widgets::toggle(ui, &mut b.pressure_opacity, tl!("Pressure for Opacity"));
                    }
                    Tool::RectMarquee | Tool::EllipseMarquee if t.pro => {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (i, (icon, tip)) in [
                            ("square", tl!("New selection").to_string()),
                            ("plus", crate::i18n::fmt(tl!("Add to selection  ({key})"), &[("key", &crate::shortcuts::pretty("Shift"))])),
                            ("minus", crate::i18n::fmt(tl!("Subtract from selection  ({key})"), &[("key", &crate::shortcuts::pretty("Alt"))])),
                            (
                                "squares-subtract",
                                crate::i18n::fmt(tl!("Intersect with selection  ({key})"), &[("key", &crate::shortcuts::pretty("Shift+Alt"))]),
                            ),
                        ]
                        .iter()
                        .enumerate()
                        {
                            if icons::button(ui, icon, 24.0, app.ui.selection_mode == i as u8, tip).clicked() {
                                app.ui.selection_mode = i as u8;
                            }
                        }
                        ui.spacing_mut().item_spacing.x = 8.0;
                        widgets::vline(ui, 22.0);
                        let o = &mut app.ui.tool_options;
                        opt_label(ui, tl!("Feather"));
                        widgets::value_field(ui, &mut o.feather, 0.0..=1000.0, "px", 62.0);
                        // Photoshop greys Anti-alias for the Rectangular Marquee (its edges are always hard).
                        ui.add_enabled_ui(app.ui.tool == Tool::EllipseMarquee, |ui| widgets::checkbox(ui, &mut o.anti_alias, tl!("Anti-alias")));
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Style"));
                        let styles = [
                            ("normal".to_string(), tl!("Normal")),
                            ("fixedRatio".to_string(), tl!("Fixed Ratio")),
                            ("fixedSize".to_string(), tl!("Fixed Size")),
                        ];
                        if widgets::dropdown(ui, "marquee-style", &mut o.marquee_style, &styles, 96.0) {
                            (o.marquee_width, o.marquee_height) = if o.marquee_style == "fixedSize" { (64.0, 64.0) } else { (1.0, 1.0) };
                        }
                        let fixed = o.marquee_style != "normal";
                        let (unit, range) = if o.marquee_style == "fixedSize" { ("px", 1.0..=300_000.0) } else { ("", 0.001..=999.0) };
                        ui.add_enabled_ui(fixed, |ui| {
                            opt_label(ui, tl!("Width"));
                            widgets::value_field(ui, &mut o.marquee_width, range.clone(), unit, 62.0);
                            if icons::button(ui, "arrow-left-right", 22.0, false, tl!("Swaps height and width")).clicked() {
                                std::mem::swap(&mut o.marquee_width, &mut o.marquee_height);
                            }
                            opt_label(ui, tl!("Height"));
                            widgets::value_field(ui, &mut o.marquee_height, range, unit, 62.0);
                        });
                        widgets::vline(ui, 22.0);
                        if widgets::secondary_button(ui, tl!("Select and Mask…"), 0.0).clicked() {
                            let _ = crate::menus::invoke(app, ui.ctx(), "select.selectAndMask", json!({}));
                        }
                    }
                    Tool::Lasso | Tool::PolygonLasso if t.pro => {
                        selection_mode_buttons(app, ui);
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Feather"));
                        widgets::value_field(ui, &mut app.ui.tool_options.feather, 0.0..=1000.0, "px", 62.0);
                        widgets::checkbox(ui, &mut app.ui.tool_options.anti_alias, tl!("Anti-alias"));
                        widgets::vline(ui, 22.0);
                        if widgets::secondary_button(ui, tl!("Select and Mask…"), 0.0).clicked() {
                            let _ = crate::menus::invoke(app, ui.ctx(), "select.selectAndMask", json!({}));
                        }
                        if app.ui.tool == Tool::PolygonLasso && !app.ui.polygon.is_empty() {
                            hint(
                                ui,
                                &crate::i18n::fmt(
                                    tl!("Click the first point or press {key} to close · Esc cancels"),
                                    &[("key", &crate::shortcuts::pretty("Enter"))],
                                ),
                            );
                        }
                    }
                    Tool::MagicWand => {
                        selection_mode_buttons(app, ui);
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Tolerance"));
                        widgets::value_field(ui, &mut app.ui.tool_options.tolerance, 0.0..=255.0, "", 58.0);
                        widgets::checkbox(ui, &mut app.ui.tool_options.anti_alias, tl!("Anti-alias"));
                        widgets::checkbox(ui, &mut app.ui.tool_options.contiguous, tl!("Contiguous"));
                        widgets::checkbox(ui, &mut app.ui.tool_options.sample_all_layers, tl!("Sample All Layers"));
                        widgets::vline(ui, 22.0);
                        if widgets::secondary_button(ui, tl!("Select Subject"), 0.0).clicked() {
                            let _ = app.run("select.subject", json!({}));
                        }
                        if widgets::secondary_button(ui, tl!("Select and Mask…"), 0.0).clicked() {
                            let _ = crate::menus::invoke(app, ui.ctx(), "select.selectAndMask", json!({}));
                        }
                    }
                    Tool::PaintBucket if t.pro => {
                        opt_label(ui, tl!("Fill"));
                        let mut src = u8::from(app.ui.tool_options.bucket_fill_pattern);
                        if widgets::dropdown(ui, "bucket-src", &mut src, &[(0u8, tl!("Foreground")), (1, tl!("Pattern"))], 100.0) {
                            app.ui.tool_options.bucket_fill_pattern = src == 1;
                        }
                        opt_label(ui, tl!("Opacity"));
                        widgets::value_field(ui, &mut app.ui.tool_options.fill_opacity, 1.0..=100.0, "%", 62.0);
                        opt_label(ui, tl!("Tolerance"));
                        widgets::value_field(ui, &mut app.ui.tool_options.tolerance, 0.0..=255.0, "", 58.0);
                        widgets::checkbox(ui, &mut app.ui.tool_options.anti_alias, tl!("Anti-alias"));
                        widgets::checkbox(ui, &mut app.ui.tool_options.contiguous, tl!("Contiguous"));
                        widgets::checkbox(ui, &mut app.ui.tool_options.sample_all_layers, tl!("All Layers"));
                    }
                    Tool::Gradient if t.pro => {
                        // Live "Gradient" (a Gradient Fill layer) or "Classic gradient" (pixels).
                        let mut classic = app.ui.tool_options.gradient_classic;
                        if widgets::dropdown(ui, "gradient-mode", &mut classic, &[(false, "Gradient"), (true, "Classic gradient")], 118.0) {
                            app.ui.tool_options.gradient_classic = classic;
                        }
                        let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
                        if classic {
                            gradient_swatch(ui, fg, bg);
                        } else {
                            crate::gradient_ui::preset_swatch(ui, &app.session.presets.gradient.resolve(fg, bg));
                        }
                        widgets::vline(ui, 22.0);
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let before = app.ui.tool_options.clone();
                        for (style, icon, tip) in [
                            ("linear", "blend", tl!("Linear Gradient")),
                            ("radial", "circle", tl!("Radial Gradient")),
                            ("angle", "rotate-cw", tl!("Angle Gradient")),
                            ("reflected", "arrow-left-right", tl!("Reflected Gradient")),
                            ("diamond", "diamond", tl!("Diamond Gradient")),
                        ] {
                            if icons::button(ui, icon, 24.0, app.ui.tool_options.gradient_style == style, tip).clicked() {
                                app.ui.tool_options.gradient_style = style.into();
                            }
                        }
                        ui.spacing_mut().item_spacing.x = 8.0;
                        widgets::vline(ui, 22.0);
                        opt_label(ui, tl!("Opacity"));
                        widgets::value_field(ui, &mut app.ui.tool_options.fill_opacity, 1.0..=100.0, "%", 62.0);
                        widgets::checkbox(ui, &mut app.ui.tool_options.gradient_reverse, tl!("Reverse"));
                        widgets::checkbox(ui, &mut app.ui.tool_options.gradient_dither, tl!("Dither"));
                        // Live mode: the options also change a selected gradient fill layer.
                        crate::gradient_ui::options_changed(app, &before);
                    }
                    Tool::Crop if t.pro => {
                        let o = &mut app.ui.tool_options;
                        let ratios: Vec<(String, &str)> = crate::chrome_ui::CROP_RATIOS.iter().map(|(k, l)| (k.to_string(), *l)).collect();
                        widgets::dropdown(ui, "crop-ratio", &mut o.crop_ratio, &ratios, 120.0);
                        // Custom ratio fields (Photoshop shows the preset's numbers here).
                        let (mut rw, mut rh) = crate::chrome_ui::crop_ratio(&o.crop_ratio, 0.0, 0.0).map_or((0.0, 0.0), |(w, h)| (w as f32, h as f32));
                        let cw = widgets::value_field(ui, &mut rw, 0.0..=99_999.0, "", 54.0).changed();
                        let swap = icons::button(ui, "arrow-left-right", 22.0, false, tl!("Swaps height and width")).clicked();
                        let ch = widgets::value_field(ui, &mut rh, 0.0..=99_999.0, "", 54.0).changed();
                        if swap {
                            std::mem::swap(&mut rw, &mut rh);
                        }
                        if (cw || ch || swap) && rw > 0.0 && rh > 0.0 {
                            o.crop_ratio = format!("{}:{}", widgets::fmt_num(rw as f64), widgets::fmt_num(rh as f64));
                        }
                        widgets::vline(ui, 22.0);
                        if widgets::secondary_button(ui, tl!("Clear"), 0.0).clicked() {
                            o.crop_ratio.clear();
                        }
                        let _ = icons::button(ui, "grid-3x3", 24.0, true, tl!("Overlay: Rule of Thirds"));
                        widgets::checkbox(ui, &mut o.crop_delete, tl!("Delete Cropped Pixels"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::button(
                                ui,
                                "check",
                                26.0,
                                false,
                                &crate::i18n::fmt(tl!("Commit current crop operation  ({key})"), &[("key", &crate::shortcuts::pretty("Enter"))]),
                            )
                            .clicked()
                            {
                                crate::canvas::commit_crop(app);
                            }
                            if icons::button(ui, "ban", 26.0, false, tl!("Cancel current crop operation  (Esc)")).clicked() {
                                app.ui.crop_rect = None;
                            }
                        });
                    }
                    Tool::Type if t.pro => crate::type_tool::options_bar(app, ui),
                    Tool::Move if t.pro => {
                        let o = &mut app.ui.tool_options;
                        widgets::checkbox(ui, &mut o.move_auto_select, tl!("Auto-Select:"));
                        widgets::dropdown(
                            ui,
                            "move-target",
                            &mut o.move_target,
                            &[("layer".to_string(), tl!("Layer")), ("group".to_string(), tl!("Group"))],
                            76.0,
                        );
                        widgets::vline(ui, 22.0);
                        widgets::checkbox(ui, &mut o.move_show_transform, tl!("Show Transform Controls"));
                        widgets::vline(ui, 22.0);
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (icon, tip, cmd) in [
                            ("panels-top-left", tl!("Align left edges"), "layer.align.leftEdges"),
                            ("app-window", tl!("Align horizontal centers"), "layer.align.horizontalCenters"),
                            ("panel-right", tl!("Align right edges"), "layer.align.rightEdges"),
                        ] {
                            let on = app.session.is_enabled(cmd);
                            if ui.add_enabled_ui(on, |ui| icons::button(ui, icon, 24.0, false, tip)).inner.clicked() {
                                let _ = app.run(cmd, json!({}));
                            }
                        }
                        ui.add_space(6.0);
                        let more = icons::button(ui, "ellipsis", 24.0, false, tl!("Align and distribute"));
                        egui::Popup::menu(&more).show(|ui| {
                            ui.set_min_width(200.0);
                            let mut section = |ui: &mut egui::Ui, title: &str, prefix: &str| {
                                ui.label(egui::RichText::new(tl!(&title)).small().color(t.text_dim));
                                for c in photocraft_engine::command_specs().iter().filter(|c| c.id.starts_with(prefix)) {
                                    if ui.add_enabled(app.session.is_enabled(c.id), egui::Button::new(c.label)).clicked() {
                                        let _ = app.run(c.id, json!({}));
                                        ui.close();
                                    }
                                }
                            };
                            section(ui, tl!("Align"), "layer.align.");
                            ui.separator();
                            section(ui, tl!("Distribute"), "layer.distribute.");
                        });
                    }
                    Tool::RectMarquee | Tool::EllipseMarquee => hint(
                        ui,
                        &format!(
                            "Drag to select  ·  {} add  ·  {} subtract  ·  {} intersect  ·  click to deselect",
                            crate::shortcuts::pretty("Shift"),
                            crate::shortcuts::pretty("Alt"),
                            crate::shortcuts::pretty("Shift+Alt")
                        ),
                    ),
                    Tool::Move => hint(ui, tl!("Drag to move the active layer")),
                    Tool::Eyedropper => hint(
                        ui,
                        &crate::i18n::fmt(
                            tl!("Click to sample the foreground colour  ·  {key}-click for background"),
                            &[("key", &crate::shortcuts::pretty("Alt"))],
                        ),
                    ),
                    Tool::Zoom => {
                        widgets::checkbox(ui, &mut app.ui.tool_options.zoom_scrubby, tl!("Scrubby Zoom"));
                        hint(
                            ui,
                            &crate::i18n::fmt(
                                tl!("Click to zoom in  ·  {key}-click to zoom out  ·  drag right/left to zoom in/out"),
                                &[("key", &crate::shortcuts::pretty("Alt"))],
                            ),
                        );
                        if widgets::secondary_button(ui, tl!("Fit Screen"), 0.0).clicked()
                            && let Some(i) = app.session.active_index()
                        {
                            app.ui.views[i].fit_pending = true;
                        }
                        if widgets::secondary_button(ui, "100%", 0.0).clicked()
                            && let Some(i) = app.session.active_index()
                        {
                            app.ui.views[i].zoom = 1.0;
                        }
                    }
                    Tool::Hand => hint(ui, tl!("Drag to pan  ·  hold Space with any tool")),
                    Tool::Lasso | Tool::PolygonLasso => hint(
                        ui,
                        &crate::i18n::fmt(
                            tl!("Drag (lasso) or click points (polygonal) · {add} add · {sub} subtract"),
                            &[("add", &crate::shortcuts::pretty("Shift")), ("sub", &crate::shortcuts::pretty("Alt"))],
                        ),
                    ),
                    Tool::Crop => hint(
                        ui,
                        &crate::i18n::fmt(
                            tl!("Drag a crop box · drag inside to move · edges resize ({ratio} ratio, {centre} centre) · Space moves while drawing · {commit} commits · Esc cancels"),
                            &[
                                ("ratio", &crate::shortcuts::pretty("Shift")),
                                ("centre", &crate::shortcuts::pretty("Alt")),
                                ("commit", &crate::shortcuts::pretty("Enter")),
                            ],
                        ),
                    ),
                    Tool::Gradient => hint(ui, tl!("Drag to draw a gradient")),
                    Tool::PaintBucket => hint(ui, tl!("Click to fill similar colours")),
                    Tool::Type => hint(ui, tl!("Click to add text")),
                    // Retouching and smart-selection tools draw their bar in `retouch_ui::options_bar`.
                    _ => {}
                }
                crate::brush_panel::commit_gesture(app, ui.ctx(), &brush_before, &brush);
                crate::brush_picker::apply(app, ui.ctx(), picked);
            });
        });
}

fn label(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(s).color(t.text_dim));
}

/// Width of `s` as [`label`] draws it (body text).
fn body_text_width(ui: &egui::Ui, s: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter().layout_no_wrap(s.to_string(), font, Color32::WHITE).size().x
}

/// Options-bar label: Photoshop writes "Size:" with a colon.
fn opt_label(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    let text = if t.pro && !s.ends_with(':') { format!("{s}:") } else { s.to_string() };
    ui.label(RichText::new(tl!(&text)).color(t.text_dim));
}

fn hint(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(s).color(t.text_faint));
}

// ----------------------------------------------------------------------------- status bar

pub fn status_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("status_bar")
        .exact_size(if t.pro { 24.0 } else { 30.0 })
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
            ui.horizontal_centered(|ui| {
                if t.pro {
                    crate::chrome_ui::status_bar_pro(app, ui);
                    // Background job progress with Cancel, at the right end (#210).
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| crate::jobs_ui::status_progress(app, ui));
                    return;
                }
                if let (Some(st), Some(i)) = (app.session.active(), app.session.active_index()) {
                    let (w, h, mode, bits, layers) =
                        (st.doc.size.width, st.doc.size.height, crate::canvas::mode_label(&st.doc), st.doc.depth.bits(), st.doc.layer_count());
                    let mut pct = app.ui.views[i].zoom * 100.0;
                    if widgets::value_field(ui, &mut pct, 1.0..=3200.0, "%", 78.0).changed() {
                        app.ui.views[i].zoom = pct / 100.0;
                        app.ui.views[i].fit_pending = false;
                    }
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::fmt(tl!("{mode} Color · {bits} bit"), &[("mode", mode), ("bits", &bits.to_string())]));
                    widgets::vline(ui, 16.0);
                    label(ui, &format!("{w} × {h} px"));
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::trn(crate::i18n::current(), layers as u64, "{n} layer", "{n} layers"));
                } else {
                    label(ui, tl!("No document"));
                }
                if !app.ui.status.is_empty() {
                    widgets::vline(ui, 16.0);
                    let is_err = app.ui.status_error || app.ui.status.starts_with("Couldn");
                    ui.label(RichText::new(&app.ui.status).color(if is_err { t.warning } else { t.text_faint }));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if app.session.active().is_some()
                        && !t.pro
                        && widgets::secondary_button(ui, tl!("Fit"), 0.0).clicked()
                        && let Some(i) = app.session.active_index()
                    {
                        app.ui.views[i].fit_pending = true;
                    }
                    if app.session.has_jobs() {
                        ui.add_space(6.0);
                        crate::jobs_ui::status_progress(app, ui);
                    }
                });
            });
        });
}

// ----------------------------------------------------------------------------- dock

pub fn right_dock(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let p = app.ui.panels.clone();
    if t.pro {
        dock_panels(app, ui, &p, &t);
    }
    // Narrow icon rail (always visible): shows, expands or collapses panel groups.
    let (rw, rb) = if t.pro { (36.0, 28.0) } else { (44.0, 32.0) };
    egui::Panel::right("rail").resizable(false).exact_size(rw).frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(4, 8))).show(
        ui,
        |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([r.left_top() - vec2(6.0, 8.0), r.left_bottom() + vec2(-6.0, 8.0)], Stroke::new(1.0, t.separator));
            ui.spacing_mut().item_spacing.y = 4.0;
            use crate::dock::Group;
            let entries: [(&str, &str, Group); 5] = [
                ("sliders-horizontal", tl!("Properties"), Group::Properties),
                ("navigation", tl!("Navigator"), Group::Navigator),
                ("palette", tl!("Color & Swatches"), Group::Color),
                ("layers", tl!("Layers"), Group::Layers),
                ("clock", tl!("History"), Group::History),
            ];
            for (icon, name, g) in entries {
                // Studio floats Properties outside the dock.
                let docked = t.pro || g != Group::Properties;
                let on = g.shown(&p) && !(docked && app.ui.dock.is_collapsed(g));
                if icons::rail_button(ui, icon, rb, on, name).clicked() {
                    crate::dock::rail_click(app, g, docked);
                }
            }
        },
    );
    if !t.pro {
        dock_panels(app, ui, &p, &t);
    }
    crate::dock::persist(app, ui.ctx());
}

/// The right dock's width range (points).
const DOCK_WIDTH: std::ops::RangeInclusive<f32> = 250.0..=520.0;

fn dock_width_id() -> egui::Id {
    egui::Id::new("dock-width-request")
}

/// Set the right dock's width on the next frame (`ui.set {dockWidth}`); clamped to its range.
pub fn request_dock_width(ctx: &egui::Context, w: f32) {
    let w = if w.is_finite() { w.clamp(*DOCK_WIDTH.start(), *DOCK_WIDTH.end()) } else { *DOCK_WIDTH.start() };
    ctx.data_mut(|d| d.insert_temp(dock_width_id(), w));
}

fn dock_panels(app: &mut PhotocraftApp, ui: &mut egui::Ui, p: &crate::state::Panels, t: &Tokens) {
    use crate::dock::Group;
    // Floating in Studio, Properties docks only in Pro (Photoshop).
    let shown: Vec<Group> = [
        (Group::Color, p.color),
        (Group::Properties, t.pro && p.properties),
        (Group::Character, p.character),
        (Group::Navigator, p.navigator),
        (Group::History, p.history),
        (Group::Layers, p.layers),
    ]
    .into_iter()
    .filter_map(|(g, on)| on.then_some(g))
    .collect();
    if shown.is_empty() {
        return;
    }
    let margin = if t.pro { 2 } else { 8 };
    let mut panel = egui::Panel::right("dock").resizable(true).default_size(if t.pro { 290.0 } else { 300.0 }).size_range(DOCK_WIDTH);
    if let Some(w) = ui.ctx().data_mut(|d| d.remove_temp::<f32>(dock_width_id())) {
        panel = panel.exact_size(w);
    }
    panel.frame(egui::Frame::NONE.fill(t.dock).inner_margin(egui::Margin::same(margin))).show(ui, |ui| {
        // Groups keep their heights whatever they show (#88): see `dock`.
        crate::dock::show(app, ui, &shown, dock_body);
    });
}

/// One dock group's tab content; `dock` bounds it and scrolls it when it's taller.
fn dock_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, group: crate::dock::Group, tab: usize) {
    use crate::dock::Group;
    let pro = Tokens::get(ui.ctx()).pro;
    match (group, tab) {
        (Group::Color, 2) => crate::preset_panels::gradients_panel(app, ui),
        (Group::Color, 3) => crate::preset_panels::patterns_panel(app, ui),
        (Group::Color, 0) if pro => color_field(app, ui),
        (Group::Color, _) if pro => swatches(app, ui),
        (Group::Color, 0) => swatches(app, ui),
        (Group::Color, _) => color_picker(app, ui),
        (Group::Properties, 0) => properties_body(app, ui),
        (Group::Properties, _) => adjustments_grid(app, ui),
        (Group::Character, tab) => crate::type_tool::character_panel(app, ui, tab == 1),
        (Group::Navigator, 0) => navigator(app, ui),
        (Group::Navigator, 1) => crate::tone::histogram_panel(app, ui),
        (Group::Navigator, _) => info_panel(app, ui),
        (Group::History, 0) => history(app, ui),
        (Group::History, 1) => crate::actions::panel(app, ui),
        (Group::History, _) => crate::comps_ui::panel(app, ui),
        (Group::Layers, 0) => layers(app, ui),
        (Group::Layers, 1) => channels(app, ui),
        (Group::Layers, _) => crate::vector_ui::paths_panel(app, ui),
    }
}

/// Photoshop's Info panel: colour under the pointer (RGB and CMYK), position, selection size.
fn info_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        empty(ui, tl!("No document"));
        return;
    };
    let (rev, size) = (st.revision, st.doc.size);
    // Positions and sizes in the ruler unit (Preferences › Units & Rulers).
    let units = app.session.prefs().units_and_rulers.clone();
    let dpi = st.doc.resolution_dpi as f64;
    let fx = move |px: f64| units.format(px, dpi, size.width as f64);
    let units_y = app.session.prefs().units_and_rulers.clone();
    let fy = move |px: f64| units_y.format(px, dpi, size.height as f64);
    let sel = st.doc.selection.as_ref().map(photocraft_compose::bounds::content_bounds);
    let pos = app
        .hover_doc
        .map(|p| (p[0].floor() as i32, p[1].floor() as i32))
        .filter(|(x, y)| *x >= 0 && *y >= 0 && *x < size.width as i32 && *y < size.height as i32);
    let rgba = pos.and_then(|(x, y)| {
        if let Some(((cx, cy, cr), v)) = app.info_sample
            && (cx, cy, cr) == (x, y, rev)
        {
            return Some(v);
        }
        let v: Vec<f32> = serde_json::from_value(app.session.execute("document.pixel", json!({"x": x, "y": y})).ok()?).ok()?;
        let v = [v[0], v[1], v[2], v[3]];
        app.info_sample = Some(((x, y, rev), v));
        Some(v)
    });
    let mono = theme::mono(11.5);
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.add_sized([18.0, 16.0], egui::Label::new(RichText::new(k).color(t.text_dim).size(11.5)));
            ui.label(RichText::new(v).font(mono.clone()).color(t.text));
        });
    };
    ui.columns(2, |cols| {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
        let dash = || "—".to_string();
        row(&mut cols[0], "R:", rgba.map_or_else(dash, |c| q(c[0]).to_string()));
        row(&mut cols[0], "G:", rgba.map_or_else(dash, |c| q(c[1]).to_string()));
        row(&mut cols[0], "B:", rgba.map_or_else(dash, |c| q(c[2]).to_string()));
        // Naive device CMYK (no ICC profile yet), in percent like Photoshop's readout.
        let cmyk = rgba.map(|c| {
            let k = 1.0 - c[0].max(c[1]).max(c[2]);
            let f = |v: f32| if k >= 1.0 { 0.0 } else { (1.0 - v - k) / (1.0 - k) };
            [f(c[0]), f(c[1]), f(c[2]), k].map(|v| (v * 100.0).round() as i32)
        });
        for (i, k) in ["C:", "M:", "Y:", "K:"].iter().enumerate() {
            row(&mut cols[1], k, cmyk.map_or_else(dash, |c| format!("{}%", c[i])));
        }
    });
    widgets::hairline(ui);
    ui.columns(2, |cols| {
        let dash = || "—".to_string();
        row(&mut cols[0], "X:", pos.map_or_else(dash, |p| fx(p.0 as f64)));
        row(&mut cols[0], "Y:", pos.map_or_else(dash, |p| fy(p.1 as f64)));
        row(&mut cols[1], "W:", sel.map_or_else(dash, |r| fx(r.width() as f64)));
        row(&mut cols[1], "H:", sel.map_or_else(dash, |r| fy(r.height() as f64)));
    });
    widgets::hairline(ui);
    ui.label(
        RichText::new(crate::i18n::fmt(tl!("Doc: {w} × {h} px"), &[("w", &size.width.to_string()), ("h", &size.height.to_string())]))
            .color(t.text_dim)
            .size(11.5),
    );
}

fn navigator(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(idx) = app.session.active_index() else {
        empty(ui, tl!("No document"));
        return;
    };
    let ctx = ui.ctx().clone();
    let Some(tex) = crate::canvas::navigator_texture(app, &ctx, idx) else { return };
    let size = app.session.documents()[idx].doc.size;
    let avail = ui.available_width();
    let box_h = 150.0;
    let (frame, resp) = ui.allocate_exact_size(vec2(avail, box_h), Sense::click_and_drag());
    ui.painter().rect_filled(frame, t.radius_sm, t.canvas);
    let aspect = size.width as f32 / size.height.max(1) as f32;
    let (w, h) = if aspect > avail / box_h { (avail - 16.0, (avail - 16.0) / aspect) } else { ((box_h - 16.0) * aspect, box_h - 16.0) };
    let rect = Rect::from_center_size(frame.center(), vec2(w, h));
    widgets::checker(ui.painter(), rect, 6.0);
    ui.painter().image(tex, rect, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    let s = w / size.width as f32;
    if (resp.clicked() || resp.dragged())
        && let Some(pp) = resp.interact_pointer_pos()
    {
        let d = (pp - rect.min) / s;
        app.ui.views[idx].center = [d.x.clamp(0.0, size.width as f32), d.y.clamp(0.0, size.height as f32)];
        app.ui.views[idx].fit_pending = false;
    }
    // Visible-area rectangle.
    let v = app.ui.views[idx].clone();
    let canvas = app.last_canvas_rect;
    let vw = canvas.width() / v.zoom * s;
    let vh = canvas.height() / v.zoom * s;
    let c = pos2(rect.min.x + v.center[0] * s, rect.min.y + v.center[1] * s);
    let vr = Rect::from_center_size(c, vec2(vw, vh)).intersect(frame.shrink(1.0));
    ui.painter().rect_stroke(vr, 2.0, Stroke::new(1.5, Color32::from_rgb(255, 84, 84)), StrokeKind::Middle);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        label(ui, tl!("Zoom"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (lbl, z) in [("200%", 2.0), ("100%", 1.0)] {
                if widgets::pill_tab(ui, lbl, (v.zoom - z).abs() < 1e-3).clicked() {
                    app.ui.views[idx].zoom = z;
                    app.ui.views[idx].fit_pending = false;
                }
            }
            if widgets::pill_tab(ui, tl!("Fit"), false).clicked() {
                app.ui.views[idx].fit_pending = true;
            }
        });
    });
    let mut lz = v.zoom.max(0.01).log2();
    if widgets::slider(ui, &mut lz, -6.64..=5.0, None).changed() {
        app.ui.views[idx].zoom = 2f32.powf(lz);
        app.ui.views[idx].fit_pending = false;
    }
}

fn empty(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(6.0);
    ui.label(RichText::new(s).color(t.text_faint));
    ui.add_space(6.0);
}

const SWATCHES: [[u8; 3]; 40] = [
    [0, 0, 0],
    [26, 26, 26],
    [51, 51, 51],
    [77, 77, 77],
    [102, 102, 102],
    [128, 128, 128],
    [153, 153, 153],
    [179, 179, 179],
    [204, 204, 204],
    [255, 255, 255],
    [236, 128, 128],
    [244, 176, 132],
    [250, 224, 128],
    [214, 240, 128],
    [150, 232, 150],
    [128, 232, 200],
    [128, 220, 240],
    [128, 176, 244],
    [168, 144, 244],
    [232, 144, 232],
    [230, 40, 40],
    [245, 120, 30],
    [250, 210, 30],
    [160, 220, 40],
    [40, 200, 80],
    [30, 200, 170],
    [30, 170, 230],
    [40, 100, 230],
    [120, 70, 220],
    [210, 50, 180],
    [120, 20, 20],
    [130, 60, 10],
    [130, 110, 10],
    [80, 120, 20],
    [20, 100, 40],
    [10, 100, 90],
    [10, 80, 120],
    [20, 50, 120],
    [60, 30, 110],
    [110, 20, 90],
];

fn swatches(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let cols = 10;
    let gap = 4.0;
    let w = ui.available_width();
    let cell = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
    let rows = SWATCHES.len().div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(vec2(w, rows as f32 * (cell + gap)), Sense::hover());
    for (i, s) in SWATCHES.iter().enumerate() {
        let (cx, cy) = ((i % cols) as f32, (i / cols) as f32);
        let r = Rect::from_min_size(area.min + vec2(cx * (cell + gap), cy * (cell + gap)), Vec2::splat(cell));
        let resp = ui.interact(r, ui.id().with(("sw", i)), Sense::click());
        ui.painter().rect_filled(r, 4.0, Color32::from_rgb(s[0], s[1], s[2]));
        if resp.hovered() {
            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.text), StrokeKind::Outside);
        }
        let c = [s[0] as f32 / 255.0, s[1] as f32 / 255.0, s[2] as f32 / 255.0, 1.0];
        if resp.clicked() {
            app.session.tools.foreground = c;
        }
        if resp.secondary_clicked() {
            app.session.tools.background = c;
        }
    }
    ui.add_space(2.0);
    ui.label(RichText::new(tl!("Click sets foreground · right-click sets background")).small().color(t.text_faint));
}

fn color_picker(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let fg = app.session.tools.foreground;
    let hsva0 = srgb_hsva(fg);
    let mut h = hsva0.h * 360.0;
    let mut s = hsva0.s * 100.0;
    let mut v = hsva0.v * 100.0;
    let hue = widgets::hue_stops();
    widgets::slider_row(ui, tl!("Hue"), &mut h, 0.0..=360.0, "°", Some(&hue));
    let sat_stops =
        [egui::ecolor::Hsva::new(hsva0.h, 0.0, hsva0.v.max(0.2), 1.0), egui::ecolor::Hsva::new(hsva0.h, 1.0, hsva0.v.max(0.2), 1.0)].map(Color32::from);
    widgets::slider_row(ui, tl!("Saturation"), &mut s, 0.0..=100.0, "%", Some(&sat_stops));
    let val_stops = [Color32::BLACK, Color32::from(egui::ecolor::Hsva::new(hsva0.h, hsva0.s, 1.0, 1.0))];
    widgets::slider_row(ui, tl!("Brightness"), &mut v, 0.0..=100.0, "%", Some(&val_stops));
    let hsva = egui::ecolor::Hsva::new(h / 360.0, s / 100.0, v / 100.0, 1.0);
    if hsva != hsva0 {
        app.session.tools.foreground = hsva_srgb(hsva);
    }
    let [r, g, b, _] = hsva.to_srgba_unmultiplied();
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let (sw, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
        ui.painter().rect_filled(sw, 6.0, Color32::from_rgb(r, g, b));
        ui.label(RichText::new(format!("#{r:02X}{g:02X}{b:02X}")).font(theme::mono(12.5)).color(t.text));
        ui.label(RichText::new(format!("RGB {r} {g} {b}")).font(theme::mono(11.5)).color(t.text_faint));
    });
}

// ----------------------------------------------------------------------------- layers

/// The simple themes' single lock button. On the Background it converts the layer to a normal
/// one, as clicking the Background's lock does in Photoshop. Elsewhere it clears every lock when
/// any is set, and locks all otherwise. Toggling only "lock all" would leave the Background's
/// transparency lock (still showing as locked), and a second click would lock its pixels,
/// shutting out the Eraser and every other paint tool (#76).
fn simple_lock_toggle(background: bool, l: &Layer) -> (String, Value) {
    if background {
        return ("layer.new.layerFromBackground".into(), json!({}));
    }
    let k = &l.locks;
    let locked = k.transparency || k.pixels || k.position || k.artboard || k.all;
    let locks = if locked { json!({"transparency": false, "pixels": false, "position": false, "artboard": false, "all": false}) } else { json!({"all": true}) };
    ("layer.setProps".into(), json!({"layer": l.id.0, "locks": locks}))
}

fn blend_options(groups: bool) -> Vec<(BlendMode, &'static str)> {
    std::iter::once(BlendMode::PassThrough).filter(|_| groups).chain(BlendMode::LAYER_MODES).map(|m| (m, m.label())).collect()
}

fn layers(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    // A new active layer opens its parent groups and is scrolled into view (#152).
    let reveal = crate::layer_reveal::track(app, ui.ctx());
    let Some(st) = app.session.active() else {
        empty(ui, tl!("No document"));
        return;
    };
    let doc = st.doc.clone();
    let active = st.active_layer;
    let active_layer = active.and_then(|id| doc.layer(id));
    let selection = st.selected_layers();
    let isolated = st.isolated_layers.clone();
    let mut actions: Vec<(String, Value)> = Vec::new();

    let t = Tokens::get(ui.ctx());
    if t.pro {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 22.0), Sense::hover());
            ui.painter().rect_filled(r, t.radius_sm, t.field);
            ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
            icons::paint(ui, Rect::from_center_size(r.left_center() + vec2(11.0, 0.0), vec2(14.0, 14.0)), "search", 11.0, t.text_faint);
            ui.painter().text(r.left_center() + vec2(22.0, 0.0), Align2::LEFT_CENTER, tl!("Kind"), egui::FontId::proportional(11.5), t.text_dim);
            ui.add_space(4.0);
            for (kind, icon, tip) in [
                ("pixel", "image", tl!("Filter for pixel layers")),
                ("adjustment", "contrast", tl!("Filter for adjustment layers")),
                ("type", "type", tl!("Filter for type layers")),
                ("shape", "square", tl!("Filter for shape layers")),
                ("smart", "app-window", tl!("Filter for smart objects")),
            ] {
                let on = app.ui.layer_filter.iter().any(|k| k == kind);
                if icons::button(ui, icon, 22.0, on, tip).clicked() {
                    if on {
                        app.ui.layer_filter.retain(|k| k != kind);
                    } else {
                        app.ui.layer_filter.push(kind.to_string());
                    }
                }
            }
        });
        ui.add_space(4.0);
    }
    if let Some(l) = active_layer {
        // Photoshop greys blend mode, Opacity and Fill for the Background layer.
        let bg = crate::doc_props_ui::is_background(&doc, l);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!bg, |ui| {
                let mut m = l.blend;
                // Leave room for the Opacity label and field: a translated label ("Непрозрачность:")
                // can be much wider than the English one, and must not slide under the dropdown.
                let opacity_label = if t.pro { tl!("Opacity:") } else { tl!("Opacity") };
                let right = (body_text_width(ui, opacity_label) + 66.0 + 2.0 * ui.spacing().item_spacing.x + 16.0).max(150.0);
                let w = ui.available_width() - right;
                if widgets::dropdown(ui, "blend", &mut m, &blend_options(l.is_group()), w.max(100.0)) {
                    actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "blend": m.label()})));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut o = l.opacity * 100.0;
                    if widgets::value_field(ui, &mut o, 0.0..=100.0, "%", 66.0).changed() {
                        actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "opacity": o / 100.0})));
                    }
                    label(ui, opacity_label);
                });
            });
        });
        // Docks zero the item spacing: keep the Opacity and Fill fields apart (#155).
        ui.add_space((theme::ROW_GAP - ui.spacing().item_spacing.y).max(0.0));
        ui.horizontal(|ui| {
            let lock_label = if t.pro { tl!("Lock:") } else { tl!("Lock") };
            let fill_label = if t.pro { tl!("Fill:") } else { tl!("Fill") };
            // In a narrow panel with long translated labels, drop the "Lock:" text (the icons keep
            // their tooltips) rather than let the Fill label run over the lock icons.
            let gap = ui.spacing().item_spacing.x;
            let icons_w = if t.pro { 5.0 * 20.0 } else { 22.0 + gap };
            let fits = body_text_width(ui, lock_label) + icons_w + body_text_width(ui, fill_label) + 66.0 + 2.0 * gap <= ui.available_width();
            if fits {
                label(ui, lock_label);
            }
            if t.pro {
                ui.spacing_mut().item_spacing.x = 0.0;
                let lk = l.locks;
                for (key, icon, tip, on) in [
                    ("transparency", "grid-3x3", tl!("Lock transparent pixels"), lk.transparency),
                    ("pixels", "brush", tl!("Lock image pixels"), lk.pixels),
                    ("position", "move", tl!("Lock position"), lk.position),
                    ("artboard", "scan", tl!("Prevent auto-nesting in and out of Artboards and Frames"), lk.artboard),
                    ("all", "lock", tl!("Lock all"), lk.all),
                ] {
                    if icons::button(ui, icon, 20.0, on, tip).clicked() {
                        actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "locks": { key: !on }})));
                    }
                }
            }
            if !t.pro {
                let locked = l.locks.transparency || l.locks.position || l.locks.all;
                if icons::button(ui, if locked { "lock" } else { "lock-open" }, 22.0, locked, tl!("Lock layer")).clicked() {
                    actions.push(simple_lock_toggle(bg, l));
                }
            }
            ui.add_enabled_ui(!bg, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut f = l.fill_opacity * 100.0;
                    if widgets::value_field(ui, &mut f, 0.0..=100.0, "%", 66.0).changed() {
                        actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "fill": f / 100.0})));
                    }
                    label(ui, fill_label);
                })
            });
        });
        ui.add_space(2.0);
    }

    // Top of the stack first, groups above their contents, closed groups' contents hidden (#126).
    let rows = crate::layer_tree_ui::display_rows(&doc, !app.ui.layer_filter.is_empty());
    let ctx = ui.ctx().clone();
    let footer = 38.0;
    let fill = ui.available_height() > footer + 60.0;
    let rows_h = if fill { ui.available_height() - footer } else { f32::INFINITY };
    egui::ScrollArea::vertical()
        .id_salt("layer-rows")
        .max_height(rows_h)
        .min_scrolled_height(if fill { rows_h } else { 0.0 })
        .auto_shrink([false, !fill])
        .show(ui, |ui| {
            let filter = app.ui.layer_filter.clone();
            let fx_collapsed = app.session.active().map(|d| d.fx_collapsed.clone()).unwrap_or_default();
            crate::layer_row_ui::begin(ui.ctx());
            for &(depth, l) in &rows {
                // Select › Isolate Layers.
                if !photocraft_engine::select_extra_cmds::isolation_shows(&doc, &isolated, l.id) {
                    continue;
                }
                if !filter.is_empty() {
                    let kind = match &l.content {
                        LayerContent::Raster(_) => "pixel",
                        LayerContent::Adjustment(_) | LayerContent::Fill(_) => "adjustment",
                        LayerContent::Text(_) => "type",
                        LayerContent::Shape(_) => "shape",
                        LayerContent::Smart(_) => "smart",
                        LayerContent::Group(_) => "",
                    };
                    if !filter.iter().any(|k| k == kind) {
                        continue;
                    }
                }
                let row = RowSel { selected: selection.contains(&l.id), primary: active == Some(l.id), multi: selection.len() > 1 };
                let top = ui.cursor().top();
                layer_row(app, &ctx, ui, &doc, l, depth, row, &mut actions);
                if reveal == Some(l.id) {
                    crate::layer_reveal::scroll_to_row(ui, top);
                }
                if !l.effects.items.is_empty() && fx_collapsed.iter().all(|id| *id != l.id) {
                    effect_rows(app, ui, l, depth);
                }
                crate::smart_ui::filter_rows(app, ui, l, depth, &mut actions);
            }
            // A rename whose row is gone (deleted, filtered out, inside a closed group) ends,
            // committed: nothing else could commit or cancel it.
            if let Some(layer) = crate::layer_row_ui::renaming(ui.ctx())
                && !crate::layer_row_ui::recorded(ui.ctx()).iter().any(|r| r.layer == layer)
                && let Some(done) = crate::layer_row_ui::end_rename(ui.ctx(), true)
            {
                actions.push(done);
            }
        });
    // End any layer drag after every row has had a chance to accept the drop.
    if ctx.input(|i| i.pointer.any_released()) {
        ctx.data_mut(|d| d.remove::<u64>(egui::Id::new("layer-drag")));
    }
    ui.add_space(4.0);
    widgets::hairline(ui);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, "trash", 26.0, false, tl!("Delete layer")).clicked() {
                actions.push(("layer.delete".into(), json!({})));
            }
            if icons::button(ui, "square-plus", 26.0, false, &crate::shortcuts::tip_label(app, "Create a new layer", "layer.new.layer")).clicked() {
                actions.push(("layer.new.layer".into(), json!({})));
            }
            if icons::button(ui, "folder", 26.0, false, tl!("Create a new group")).clicked() {
                actions.push(("layer.new.group".into(), json!({})));
            }
            let adj = icons::button(ui, "contrast", 26.0, false, tl!("Create new fill or adjustment layer"));
            egui::Popup::menu(&adj).show(|ui| {
                ui.set_min_width(190.0);
                for c in photocraft_engine::command_specs().iter().filter(|c| c.id.starts_with("layer.newAdjustmentLayer.")) {
                    if ui.button(c.label.trim_end_matches('…')).clicked() {
                        actions.push((c.id.into(), json!({})));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(tl!("Solid Color…")).clicked() {
                    actions.push(("layer.newFillLayer.solidColor".into(), json!({})));
                    ui.close();
                }
                if ui.button(tl!("Gradient…")).clicked() {
                    actions.push(("layer.newFillLayer.gradient".into(), json!({})));
                    ui.close();
                }
            });
            if icons::button(
                ui,
                "square-dot",
                26.0,
                false,
                &crate::i18n::fmt(tl!("Add a mask  (from the selection; {key} inverts)"), &[("key", &crate::shortcuts::pretty("Alt"))]),
            )
            .clicked()
            {
                let alt = ui.input(|i| i.modifiers.alt);
                actions.push((crate::layer_menu_ui::add_mask_command(doc.selection.is_some(), alt).into(), json!({})));
            }
            let fx = fx_button(ui, 26.0, tl!("Add a layer style"));
            egui::Popup::menu(&fx).show(|ui| {
                ui.set_min_width(180.0);
                if ui.button(tl!("Blending Options…")).clicked() {
                    crate::layer_style::open(app, None);
                    ui.close();
                }
                ui.separator();
                for &(kind, label) in crate::layer_style::KINDS {
                    if ui.button(format!("{label}…")).clicked() {
                        crate::layer_style::open(app, Some(kind));
                        ui.close();
                    }
                }
            });
            // Photoshop's footer starts with Link Layers (enabled with two or more layers selected).
            let can_link = app.session.is_enabled("layer.linkLayers");
            if ui.add_enabled_ui(can_link, |ui| icons::button(ui, "link", 26.0, false, tl!("Link layers"))).inner.clicked() {
                actions.push(("layer.linkLayers".into(), json!({})));
            }
        });
    });
    for (id, p) in actions {
        if id == "ui.maskTarget" {
            app.ui.mask_target = p.as_bool().unwrap_or(false);
            app.ui.vector_mask_target = false;
            continue;
        }
        if id == "ui.vectorMaskTarget" {
            app.ui.vector_mask_target = p.as_bool().unwrap_or(false);
            app.ui.mask_target = false;
            continue;
        }
        if p.is_null() {
            // Context-menu items behave like their menu-bar twins (dialogs included).
            let ctx = ui.ctx().clone();
            if let Err(e) = crate::menus::invoke(app, &ctx, &id, json!({})) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
            continue;
        }
        let _ = app.run(&id, p);
    }
}

/// Photoshop's italic "fx" footer button (no icon-font equivalent).
fn fx_button(ui: &mut egui::Ui, size: f32, tip: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, t.radius_sm, t.hover);
    }
    let mut job = egui::text::LayoutJob::default();
    job.append("fx", 0.0, egui::TextFormat { font_id: egui::FontId::proportional(15.0), color: t.icon, italics: true, ..Default::default() });
    let g = ui.painter().layout_job(job);
    ui.painter().galley(r.center() - g.size() / 2.0, g, t.icon);
    resp.on_hover_text(tip)
}

/// How a Layers panel row is selected: in the (multi-)selection, the primary/active layer, and
/// whether several layers are selected.
#[derive(Clone, Copy)]
struct RowSel {
    selected: bool,
    primary: bool,
    multi: bool,
}

/// `layer.select` mode for a click with these modifiers (Photoshop: ⌘-click toggles, ⇧-click
/// selects a range, a plain click selects one layer).
fn select_mode(m: egui::Modifiers) -> &'static str {
    if m.command {
        "toggle"
    } else if m.shift {
        "range"
    } else {
        "replace"
    }
}

#[allow(clippy::too_many_arguments)]
fn layer_row(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    doc: &photocraft_doc::Document,
    l: &Layer,
    depth: usize,
    row: RowSel,
    actions: &mut Vec<(String, Value)>,
) {
    let selected = row.selected;
    let t = Tokens::get(ctx);
    // Photoshop's default (medium) thumbnails: 32 pt rows.
    let row_h = if t.pro { 32.0 } else { 46.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
    layer_drag_and_drop(ctx, ui, l, rect, &resp, actions);
    // Rows are painted: name them for screen readers and UI tests.
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &l.name));
    // A row scrolled out of view only keeps its place (#125): a layout's hundreds of rows would
    // otherwise lay out names, icons and thumbnails every frame. Group rows stay whole: their
    // disclosure triangle is a widget (accessibility, scroll-to).
    if !ui.is_rect_visible(rect) && !l.is_group() && !resp.context_menu_opened() && crate::layer_row_ui::renaming(ctx) != Some(l.id.0) {
        return;
    }
    let painter = ui.painter_at(rect.expand(1.0));
    if t.pro {
        if selected {
            painter.rect_filled(rect, 0.0, t.row_selected);
        } else if resp.hovered() {
            painter.rect_filled(rect, 0.0, t.hover.gamma_multiply(0.5));
        }
        // eye column divider + row divider, as in Photoshop
        painter.line_segment([pos2(rect.left() + 30.0, rect.top()), pos2(rect.left() + 30.0, rect.bottom())], Stroke::new(1.0, t.separator));
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.separator));
    } else if selected {
        painter.rect_filled(rect, t.radius, t.hover);
        painter.rect_stroke(rect, t.radius, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, t.radius, t.hover.gamma_multiply(0.5));
    }
    let mut x = rect.left() + 6.0;
    let eye = Rect::from_min_size(pos2(x, rect.center().y - 11.0), vec2(22.0, 22.0));
    let eye_resp = ui.interact(eye, ui.id().with(("eye", l.id.0)), Sense::click());
    icons::paint(ui, eye, if l.visible { "eye" } else { "eye-off" }, 15.0, if l.visible { t.icon } else { t.text_faint });
    if eye_resp.clicked() {
        actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "visible": !l.visible})));
    }
    // Everything but the thumbnails, the indentation and the name, so a narrow panel squeezes
    // the indentation first, then the thumbnails (a layer with two masks has three).
    let others = 6.0
        + 28.0
        + if l.is_group() { crate::layer_tree_ui::TRIANGLE_W } else { 0.0 }
        + if l.clipped { 12.0 } else { 0.0 }
        + crate::layer_row_ui::reserved_width(l);
    let ts = crate::mask_thumbs_ui::thumb_size(l, if t.pro { 24.0 } else { 34.0 }, rect.width() - others);
    let fixed = others + ts + 6.0 + crate::mask_thumbs_ui::width(l, ts);
    x += 28.0 + crate::layer_row_ui::indent(depth, rect.width(), fixed);
    let toggled = crate::layer_tree_ui::disclosure(ui, rect, &mut x, l, actions);
    if l.clipped {
        painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, "↳", egui::FontId::proportional(13.0), t.text_faint);
        x += 12.0;
    }
    let thumb = Rect::from_min_size(pos2(x, rect.center().y - ts / 2.0), vec2(ts, ts));
    draw_layer_thumb(app, ctx, ui, doc, l, thumb, row.primary);
    x += ts + 6.0;
    // Link chains and pixel / vector mask thumbnails (#153).
    let masks = crate::mask_thumbs_ui::paint(app, ctx, ui, &painter, doc, l, &mut x, rect.center().y, ts, actions);
    let mask_rect = masks.thumb(crate::mask_thumbs_ui::MaskKind::Pixel);
    let vector_rect = masks.thumb(crate::mask_thumbs_ui::MaskKind::Vector);
    // Photoshop frames the targeted thumbnail (pixels, mask or vector mask) of the active layer
    // with corner brackets.
    if row.primary {
        let target = if app.ui.vector_mask_target && vector_rect.is_some() {
            vector_rect
        } else if app.ui.mask_target {
            mask_rect.or(Some(thumb))
        } else {
            Some(thumb)
        };
        if let Some(r) = target {
            crate::mask_thumbs_ui::paint_brackets(&painter, r, t.text);
        }
    }
    // Right-hand indicators first; the name gets what is left and ends in "…" (#144).
    let fx_open = app.session.active().is_none_or(|d| !d.fx_collapsed.contains(&l.id));
    let (name_right, indicators, fx_toggled) = crate::layer_row_ui::indicators(ui, &painter, rect, x, l, fx_open, actions);
    let name_color = if l.visible { t.text } else { t.text_faint };
    let font = if selected && !t.pro { theme::medium(13.0) } else { egui::FontId::proportional(if t.pro { 12.0 } else { 13.0 }) };
    // Photoshop before 2026 set the Background layer's name in italics; 2026 sets it upright.
    let italic = !t.pro && l.name == "Background" && l.locks.transparency;
    // Photoshop rows show only the name; the kind sub-label is a Studio-theme addition.
    let is_pixel = t.pro || matches!(l.content, LayerContent::Raster(_));
    let name_rect = crate::layer_row_ui::truncated(&painter, &l.name, font, name_color, italic, name_right - x).map(|galley| {
        let text_pos = pos2(x, rect.center().y - galley.size().y / 2.0 - if is_pixel { 0.0 } else { 7.0 });
        let r = Rect::from_min_size(text_pos, galley.size());
        painter.galley(text_pos, galley, name_color);
        r
    });
    if !is_pixel {
        let sub = match &l.content {
            LayerContent::Adjustment(a) => tl!(a.label()).to_string(),
            LayerContent::Group(g) => crate::i18n::trn(crate::i18n::current(), g.children.len() as u64, "Group · {n} layer", "Group · {n} layers"),
            other => tl!(other.kind_name()).to_string(),
        };
        crate::layer_row_ui::label(&painter, x, rect.center().y + 8.0, name_right, &sub, egui::FontId::proportional(11.0), t.text_faint);
    }
    crate::layer_row_ui::record(ctx, crate::layer_row_ui::RowRects { layer: l.id.0, row: rect, name: name_rect, indicators });
    // ⌘-click a layer, mask or vector-mask thumbnail loads its transparency / mask / path as a
    // selection (⇧ add, ⌥ subtract, ⇧⌥ intersect) instead of changing the layer selection.
    let thumb_load = resp.clicked().then(|| (ui.input(|i| i.modifiers), resp.interact_pointer_pos())).and_then(|(m, pos)| {
        let p = pos.filter(|_| m.command)?;
        if let Some(kind) = masks.hit(p) {
            return Some(crate::mask_thumbs_ui::load_params(l, kind, m));
        }
        thumb.expand(2.0).contains(p).then(|| json!({"channel": "transparency", "layer": l.id.0, "operation": crate::channels_panel::load_operation(m)}))
    });
    // ⇧-click a mask thumbnail: disable / enable that mask; ⌥-click a layer mask: view it.
    let mask_toggle = resp.clicked().then(|| (ui.input(|i| i.modifiers), resp.interact_pointer_pos())).and_then(|(m, pos)| {
        let kind = masks.hit(pos?)?;
        crate::mask_thumbs_ui::click_command(l, kind, m)
    });
    if let Some(p) = thumb_load {
        actions.push(("select.loadSelection".into(), p));
    } else if let Some(cmd) = mask_toggle {
        actions.push(cmd);
    } else if resp.clicked() && !eye_resp.clicked() && !toggled && !fx_toggled && !masks.clicked {
        let mode = select_mode(ui.input(|i| i.modifiers));
        actions.push(("layer.select".into(), json!({"layer": l.id.0, "mode": mode})));
        // Clicking a thumbnail picks what painting targets; adjustment/fill layers target their mask.
        let pos = resp.interact_pointer_pos();
        let on_mask = mask_rect.zip(pos).is_some_and(|(r, p)| r.expand(2.0).contains(p));
        let on_thumb = pos.is_some_and(|p| thumb.expand(2.0).contains(p));
        let on_vector = pos.and_then(|p| masks.hit(p)) == Some(crate::mask_thumbs_ui::MaskKind::Vector);
        // Clicking the layer thumbnail leaves mask view (#196).
        let viewing = app.session.active().and_then(photocraft_engine::mask_view_cmds::current).is_some_and(|v| v.layer == l.id);
        if on_thumb && viewing {
            actions.push((photocraft_engine::mask_view_cmds::ID.into(), json!({"layer": l.id.0, "mode": "off"})));
        }
        let content_less = matches!(l.content, LayerContent::Adjustment(_) | LayerContent::Fill(_));
        if on_vector {
            // The vector mask thumbnail targets the vector mask: the path tools edit it and the
            // Paths panel selects the layer's path.
            app.ui.selected_path = Some("layer".into());
            actions.push(("ui.vectorMaskTarget".into(), json!(true)));
        } else if on_mask || (content_less && l.mask.is_some()) {
            actions.push(("ui.maskTarget".into(), json!(true)));
        } else if on_thumb || !row.primary {
            actions.push(("ui.maskTarget".into(), json!(false)));
        }
    }
    // Double-click the name to rename in place (Photoshop ergonomics). The Background can't be
    // renamed while it's locked, so a double-click turns it into a normal layer instead.
    if resp.double_clicked() {
        if crate::doc_props_ui::is_background(doc, l) {
            actions.push(("layer.new.layerFromBackground".into(), json!({})));
        } else if let Some(done) = crate::layer_row_ui::start_rename(ctx, l.id.0, &l.name) {
            // One rename at a time: starting this one commits any other (#314).
            actions.push(done);
        }
    }
    let edit_rect = Rect::from_min_max(pos2(x - 3.0, rect.center().y - 11.0), pos2(name_right.max(x + 40.0), rect.center().y + 11.0));
    if let Some(done) = crate::layer_row_ui::rename_field(ui, l.id.0, edit_rect) {
        actions.push(done);
    }
    // Right-click context menu.
    resp.context_menu(|ui| {
        // Right-clicking inside a multi-selection keeps it and acts on every selected layer.
        let on_set = row.multi && selected;
        if crate::layer_menu_ui::show(app, ui, l, on_set, actions)
            && let Some(done) = crate::layer_row_ui::start_rename(ui.ctx(), l.id.0, &l.name)
        {
            actions.push(done);
        }
    });
}

fn draw_layer_thumb(app: &mut PhotocraftApp, ctx: &egui::Context, ui: &egui::Ui, doc: &photocraft_doc::Document, l: &Layer, rect: Rect, selected: bool) {
    let t = Tokens::get(ctx);
    let p = ui.painter();
    match &l.content {
        LayerContent::Adjustment(_) | LayerContent::Group(_) => {
            p.rect_filled(rect, 6.0, t.field);
            let icon = if l.is_group() { "folder" } else { "contrast" };
            icons::paint(ui, rect, icon, 18.0, t.icon);
        }
        LayerContent::Text(_) => {
            // Photoshop: type layers show a "T" tile rather than a pixel thumbnail.
            p.rect_filled(rect, if t.pro { 0.0 } else { 6.0 }, Color32::from_gray(if t.pro { 222 } else { 236 }));
            icons::paint(ui, rect, "type", 20.0, Color32::from_gray(40));
        }
        // Gradient fills show the gradient itself (Photoshop); solid ones their colour.
        LayerContent::Fill(f) if crate::gradient_ui::paint_thumbnail(ui, l.id, f, rect) => {}
        LayerContent::Fill(f) => {
            let c = match f {
                photocraft_doc::Fill::Solid(c) => c.to_rgba8(),
                _ => [128, 128, 128, 255],
            };
            p.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
        }
        _ => {
            // Keep row layout and outside thumbnail decorations when the image is clipped.
            if ui.is_rect_visible(rect) {
                widgets::checker(p, rect, 5.0);
                let tex = app.layer_thumb(ctx, doc, l);
                p.image(tex, rect, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
    }
    let stroke = if t.pro {
        Stroke::new(1.0, Color32::from_gray(20))
    } else if selected {
        Stroke::new(1.5, t.text)
    } else {
        Stroke::new(1.0, t.field_border)
    };
    p.rect_stroke(rect, CornerRadius::same(if t.pro { 0 } else { 5 }), stroke, StrokeKind::Outside);
    crate::smart_ui::thumb_badge(ui, l, rect);
}

fn channels(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::channels_panel::show(app, ui);
}

fn history(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        empty(ui, tl!("No document"));
        return;
    };
    let doc = st.doc.clone();
    let entries = st.history.entries();
    let current = entries.len() - 1;
    let redo: Vec<String> = {
        let mut v = Vec::new();
        let mut h = st.history.clone();
        let mut d = st.doc.clone();
        while let Some(label) = h.redo_label().map(str::to_string) {
            v.push(label);
            match h.redo(d.clone()) {
                Some(n) => d = n,
                None => break,
            }
        }
        v
    };
    // Snapshot row (Photoshop shows the document's opening state with a thumbnail).
    {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
        let thumb = Rect::from_min_size(pos2(rect.left() + 28.0, rect.center().y - 14.0), vec2(28.0, 28.0));
        let ctx = ui.ctx().clone();
        if let Some(layer) = doc.layers.first() {
            widgets::checker(ui.painter(), thumb, 4.0);
            let tex = app.layer_thumb(&ctx, &doc, layer);
            ui.painter().image(tex, thumb, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        ui.painter().rect_stroke(thumb, 0.0, Stroke::new(1.0, t.separator), StrokeKind::Outside);
        ui.painter().text(pos2(thumb.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, &doc.name, egui::FontId::proportional(12.0), t.text);
        ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.separator));
    }
    let mut target: Option<isize> = None;
    let all = entries.iter().map(|e| (e.clone(), false)).chain(redo.iter().map(|e| (e.clone(), true)));
    // The dock gives History a fixed height: the rows scroll above the footer.
    let footer = if t.pro { 34.0 } else { 0.0 };
    let max_h = (ui.available_height() - footer).max(40.0);
    egui::ScrollArea::vertical().id_salt("history-rows").max_height(max_h).auto_shrink([false, false]).show(ui, |ui| {
        for (i, (e, is_redo)) in all.enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
            if i == current {
                ui.painter().rect_filled(rect, if t.pro { 0.0 } else { t.radius_sm }, if t.pro { t.row_selected } else { t.accent_soft });
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.5));
            }
            let icon = match e.as_str() {
                s if s.contains("Brush") || s.contains("Eraser") => "brush",
                s if s.contains("Layer") => "layers",
                s if s.contains("Select") || s.contains("Marquee") || s.contains("Deselect") || s.contains("Lasso") => "square-dashed",
                s if s.contains("Fill") => "paint-bucket",
                "Open" => "image",
                _ => "sliders-horizontal",
            };
            let color = if is_redo { t.text_faint } else { t.text };
            icons::paint(
                ui,
                Rect::from_center_size(pos2(rect.left() + 42.0, rect.center().y), vec2(16.0, 16.0)),
                icon,
                12.0,
                if is_redo { t.text_faint } else { t.icon },
            );
            ui.painter().text(pos2(rect.left() + 58.0, rect.center().y), Align2::LEFT_CENTER, &e, egui::FontId::proportional(12.0), color);
            if resp.clicked() {
                target = Some(i as isize - current as isize);
            }
        }
    });
    if t.pro {
        widgets::hairline(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let _ = icons::button(ui, "trash", 24.0, false, tl!("Delete current state"));
                let _ = icons::button(ui, "scan", 24.0, false, tl!("Create new snapshot"));
                let _ = icons::button(ui, "file-plus", 24.0, false, tl!("Create new document from current state"));
            });
        });
    }
    if let Some(delta) = target {
        let (cmd, n) = if delta < 0 { ("edit.undo", -delta) } else { ("edit.redo", delta) };
        for _ in 0..n {
            if app.run(cmd, json!({})).is_err() {
                break;
            }
        }
    }
}

// ----------------------------------------------------------------------------- properties

/// Floating Properties card anchored to the canvas' top-right corner.
pub fn properties_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // Pro (Photoshop) docks Properties; Studio floats it over the canvas.
    if !app.ui.panels.properties || Tokens::get(ctx).pro {
        return;
    }
    let Some(st) = app.session.active() else { return };
    let Some(id) = st.active_layer else { return };
    let Some(layer) = st.doc.layer(id) else { return };
    // The floating card appears for adjustment and fill layers (their controls live here).
    if !matches!(layer.content, LayerContent::Adjustment(_) | LayerContent::Fill(_)) {
        return;
    }
    let layer = layer.clone();
    let t = Tokens::get(ctx);
    let canvas = app.last_canvas_rect;
    let width = 320.0;
    let pos = pos2(canvas.right() - width - 12.0, canvas.top() + 44.0);
    let frame = egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 12], blur: 36, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(14));
    // A floating panel (Order::Middle like other panels, so menus, popups and dialogs stay above
    // it), anchored to the canvas corner and dragged by its title off the part being worked on.
    let card_id = egui::Id::new("properties-card");
    let offset: egui::Vec2 = ctx.data(|m| m.get_temp(card_id)).unwrap_or_default();
    let mut drag = egui::Vec2::ZERO;
    let shown = egui::Area::new(card_id).order(egui::Order::Middle).fixed_pos(pos + offset).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(width - 28.0);
            let title = ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Properties")).font(theme::semibold(13.5)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "minus", 22.0, false, tl!("Hide Properties")).clicked() {
                        app.ui.panels.properties = false;
                    }
                });
            });
            let bar = title.response.rect.with_max_x(title.response.rect.right() - 28.0);
            let grip = ui.interact(bar, card_id.with("title"), Sense::drag());
            if grip.hovered() || grip.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
            drag = grip.drag_delta();
            ui.add_space(6.0);
            let icon = match &layer.content {
                LayerContent::Adjustment(_) => "sliders-horizontal",
                LayerContent::Group(_) => "folder",
                LayerContent::Fill(_) => "paint-bucket",
                _ => "image",
            };
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                ui.painter().rect_filled(r, t.radius_sm, t.field);
                icons::paint(ui, r, icon, 17.0, t.icon);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&layer.name).font(theme::medium(13.0)).color(t.text));
                    let kind = match &layer.content {
                        LayerContent::Adjustment(a) => crate::i18n::fmt(tl!("{name} Properties"), &[("name", tl!(a.label()))]),
                        // Whole phrases ("Type Layer"), as the Properties header translates them.
                        other => tl!(&format!("{} Layer", other.kind_name())).to_string(),
                    };
                    ui.label(RichText::new(kind).small().color(t.text_faint));
                });
            });
            ui.add_space(8.0);
            widgets::hairline(ui);
            ui.add_space(8.0);
            if let LayerContent::Adjustment(adj) = &layer.content {
                adjustment_controls(app, ui, id, adj);
            } else {
                layer_controls(app, ui, &layer);
            }
        });
    });
    if drag != egui::Vec2::ZERO && canvas.is_positive() {
        // Keep the title bar inside the canvas.
        let r = shown.response.rect.translate(drag);
        let dx = (canvas.left() - r.left()).max(0.0) - (r.right() - canvas.right()).max(0.0);
        let dy = (canvas.top() - r.top()).max(0.0) - (r.top() + 40.0 - canvas.bottom()).max(0.0);
        ctx.data_mut(|m| m.insert_temp(card_id, offset + drag + egui::vec2(dx, dy)));
    }
}

fn adjustment_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId, adj: &photocraft_doc::Adjustment) {
    crate::adjust_editors::layer_editor(app, ui, id, adj);
    ui.add_space(6.0);
    let layer = app.session.active().and_then(|s| s.doc.layer(id).cloned());
    if let Some(l) = layer {
        ui.horizontal(|ui| {
            let mut vis = l.visible;
            if widgets::toggle(ui, &mut vis, tl!("Adjustment visible")).changed() {
                let _ = app.run("layer.setProps", json!({"layer": id.0, "visible": vis}));
            }
            let mut clip = l.clipped;
            if widgets::toggle(ui, &mut clip, tl!("Clip to layer below")).changed() {
                let _ = app.run("layer.setProps", json!({"layer": id.0, "clipped": clip}));
            }
        });
    }
    ui.add_space(6.0);
    if widgets::secondary_button(ui, tl!("Reset to defaults"), ui.available_width()).clicked() {
        let _ = app.run("layer.setAdjustment", json!({"layer": id.0}));
        app.live_adjust = None;
    }
}

fn layer_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, layer: &Layer) {
    let t = Tokens::get(ui.ctx());
    if let Some(s) = layer.surface() {
        let b = app.cached_bounds(layer.id.0, s);
        egui::Grid::new("props-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for (k, v) in [("X", b.x0.to_string()), ("Y", b.y0.to_string()), ("W", b.width().to_string()), ("H", b.height().to_string())] {
                ui.label(RichText::new(k).color(t.text_dim));
                ui.label(RichText::new(format!("{v} px")).font(theme::mono(12.0)).color(t.text));
                ui.end_row();
            }
        });
        ui.add_space(6.0);
    }
    let mut o = layer.opacity * 100.0;
    let r = widgets::slider_row(ui, tl!("Opacity"), &mut o, 0.0..=100.0, "%", None);
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("layer.setProps", json!({"layer": layer.id.0, "opacity": o / 100.0}));
    }
    let mut f = layer.fill_opacity * 100.0;
    let r = widgets::slider_row(ui, tl!("Fill"), &mut f, 0.0..=100.0, "%", None);
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("layer.setProps", json!({"layer": layer.id.0, "fill": f / 100.0}));
    }
}

/// Docked Properties body (Pro theme).
fn properties_body(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        empty(ui, tl!("No properties"));
        return;
    };
    // Photoshop shows the Document properties when nothing or the Background layer is selected.
    if crate::doc_props_ui::shows_document(&st.doc, st.active_layer) {
        crate::doc_props_ui::properties(app, ui);
        return;
    }
    let Some(id) = st.active_layer else { return };
    // Borrowed from the document snapshot: cloning the layer every frame copied whole groups.
    let doc = st.doc.clone();
    let Some(layer) = doc.layer(id) else { return };
    // Header: kind icon, layer name and kind (#155); sections below draw their own separators.
    crate::props_layout::header(ui, layer);
    let is_adjustment = matches!(layer.content, LayerContent::Adjustment(_));
    if is_adjustment || layer.artboard().is_some() || !t.pro {
        ui.add_space(4.0);
        widgets::hairline(ui);
        ui.add_space(6.0);
    }
    if let LayerContent::Adjustment(adj) = &layer.content {
        adjustment_controls(app, ui, id, adj);
    } else if layer.artboard().is_some() {
        crate::artboard_ui::properties(app, ui, layer);
    } else if t.pro {
        // Transform, Align, the kind's sections and Quick Actions.
        crate::layer_props_ui::properties(app, ui, layer);
    } else {
        if matches!(layer.content, LayerContent::Fill(photocraft_doc::Fill::Gradient { .. })) {
            crate::gradient_ui::properties(app, ui, layer);
            ui.add_space(6.0);
        }
        layer_controls(app, ui, layer);
        if matches!(layer.content, LayerContent::Text(_)) {
            crate::type_tool::type_properties(app, ui);
        }
        if matches!(layer.content, LayerContent::Shape(_)) {
            crate::vector_ui::shape_properties(app, ui, id);
        }
    }
}

/// Photoshop's Adjustments panel: a grid of one-click adjustment layers.
fn adjustments_grid(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Add an adjustment")).color(t.text_dim));
    ui.add_space(4.0);
    let items: [(&str, &str, &str); 16] = [
        ("brightnessContrast", "sun", tl!("Brightness/Contrast")),
        ("levels", "gauge", tl!("Levels")),
        ("curves", "pen-tool", tl!("Curves")),
        ("exposure", "scan", tl!("Exposure")),
        ("vibrance", "sparkles", tl!("Vibrance")),
        ("hueSaturation", "droplet", tl!("Hue/Saturation")),
        ("colorBalance", "blend", tl!("Color Balance")),
        ("blackWhite", "contrast", tl!("Black & White")),
        ("photoFilter", "circle-dot", tl!("Photo Filter")),
        ("channelMixer", "sliders-horizontal", tl!("Channel Mixer")),
        ("invert", "squares-subtract", tl!("Invert")),
        ("posterize", "layers", tl!("Posterize")),
        ("threshold", "square", tl!("Threshold")),
        ("gradientMap", "palette", tl!("Gradient Map")),
        ("selectiveColor", "swatch-book", tl!("Selective Color")),
        ("colorLookup", "grid-3x3", tl!("Color Lookup")),
    ];
    let mut run = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        for (kind, icon, tip) in items {
            if icons::button(ui, icon, 30.0, false, tip).clicked() {
                run = Some(format!("layer.newAdjustmentLayer.{kind}"));
            }
        }
    });
    if let Some(id) = run {
        let _ = app.run(&id, json!({}));
        app.ui.dock_tabs.properties = 0;
    }
}

/// Photoshop Color panel: saturation/brightness field + hue strip, drawn as shaded meshes.
fn color_field(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let fg = app.session.tools.foreground;
    let key = egui::Id::new("color-field-hue");
    let mut hsva = srgb_hsva(fg);
    // Keep hue stable for greys (where RGB->HSV hue is undefined).
    let remembered: f32 = ui.data(|d| d.get_temp(key)).unwrap_or(hsva.h);
    if hsva.s < 0.01 || hsva.v < 0.01 {
        hsva.h = remembered;
    }
    let w = ui.available_width();
    let strip_w = 14.0;
    let h = 120.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // Colour chips (fg/bg) at left, Photoshop style.
        let (chips, _) = ui.allocate_exact_size(vec2(34.0, h), Sense::hover());
        let bgr = Rect::from_min_size(chips.min + vec2(10.0, 10.0), vec2(22.0, 22.0));
        let fgr = Rect::from_min_size(chips.min, vec2(22.0, 22.0));
        ui.painter().rect_filled(bgr, 2.0, c32(app.session.tools.background));
        ui.painter().rect_stroke(bgr, 2.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
        ui.painter().rect_filled(fgr, 2.0, c32(fg));
        ui.painter().rect_stroke(fgr, 2.0, Stroke::new(1.0, Color32::from_gray(210)), StrokeKind::Outside);
        // SV field.
        let field_w = w - 34.0 - strip_w - 16.0;
        let (field, fresp) = ui.allocate_exact_size(vec2(field_w, h), Sense::click_and_drag());
        let mut mesh = egui::Mesh::default();
        let n = 16;
        for j in 0..=n {
            for i in 0..=n {
                let (sx, vy) = (i as f32 / n as f32, j as f32 / n as f32);
                let c = Color32::from(egui::ecolor::Hsva::new(hsva.h, sx, 1.0 - vy, 1.0));
                mesh.colored_vertex(pos2(field.left() + sx * field.width(), field.top() + vy * field.height()), c);
            }
        }
        for j in 0..n {
            for i in 0..n {
                let a = (j * (n + 1) + i) as u32;
                let b = a + 1;
                let c = a + (n + 1) as u32;
                let d = c + 1;
                mesh.add_triangle(a, b, d);
                mesh.add_triangle(a, d, c);
            }
        }
        ui.painter().add(mesh);
        ui.painter().rect_stroke(field, 0.0, Stroke::new(1.0, t.separator), StrokeKind::Outside);
        if (fresp.dragged() || fresp.clicked())
            && let Some(p) = fresp.interact_pointer_pos()
        {
            hsva.s = ((p.x - field.left()) / field.width()).clamp(0.0, 1.0);
            hsva.v = 1.0 - ((p.y - field.top()) / field.height()).clamp(0.0, 1.0);
        }
        let knob = pos2(field.left() + hsva.s * field.width(), field.top() + (1.0 - hsva.v) * field.height());
        ui.painter().circle_stroke(knob, 5.0, Stroke::new(1.5, Color32::WHITE));
        ui.painter().circle_stroke(knob, 6.5, Stroke::new(1.0, Color32::from_black_alpha(160)));
        // Hue strip.
        let (strip, sresp) = ui.allocate_exact_size(vec2(strip_w, h), Sense::click_and_drag());
        let mut m2 = egui::Mesh::default();
        let steps = 24;
        for k in 0..=steps {
            let f = k as f32 / steps as f32;
            let c = Color32::from(egui::ecolor::Hsva::new(1.0 - f, 1.0, 1.0, 1.0));
            m2.colored_vertex(pos2(strip.left(), strip.top() + f * strip.height()), c);
            m2.colored_vertex(pos2(strip.right(), strip.top() + f * strip.height()), c);
        }
        for k in 0..steps {
            let a = (k * 2) as u32;
            m2.add_triangle(a, a + 1, a + 3);
            m2.add_triangle(a, a + 3, a + 2);
        }
        ui.painter().add(m2);
        if (sresp.dragged() || sresp.clicked())
            && let Some(p) = sresp.interact_pointer_pos()
        {
            hsva.h = 1.0 - ((p.y - strip.top()) / strip.height()).clamp(0.0, 0.9999);
        }
        let y = strip.top() + (1.0 - hsva.h) * strip.height();
        let tri = vec![pos2(strip.right() + 1.0, y), pos2(strip.right() + 6.0, y - 4.0), pos2(strip.right() + 6.0, y + 4.0)];
        ui.painter().add(egui::Shape::convex_polygon(tri, t.text, Stroke::NONE));
        if fresp.dragged() || fresp.clicked() || sresp.dragged() || sresp.clicked() {
            app.session.tools.foreground = hsva_srgb(hsva);
            ui.data_mut(|d| d.insert_temp(key, hsva.h));
        }
    });
    let [r, g, b, _] = hsva.to_srgba_unmultiplied();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("#{r:02X}{g:02X}{b:02X}")).font(theme::mono(12.0)).color(t.text));
        ui.label(RichText::new(format!("R {r}  G {g}  B {b}")).font(theme::mono(11.0)).color(t.text_faint));
    });
}

/// Tool colours are sRGB-encoded floats; egui's Hsva works on sRGB bytes via these helpers
/// (its `from_rgba_unmultiplied` expects *linear* RGB, which gave wrong readouts).
fn srgb_hsva(c: [f32; 4]) -> egui::ecolor::Hsva {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    egui::ecolor::Hsva::from_srgb([q(c[0]), q(c[1]), q(c[2])])
}

fn hsva_srgb(h: egui::ecolor::Hsva) -> [f32; 4] {
    let [r, g, b] = h.to_srgb();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// Photoshop's brush preset picker chip: a soft/hard round tip preview with the size underneath.
/// Draw a round brush tip preview (hard core fading to a soft edge).
fn brush_tip(p: &egui::Painter, c: egui::Pos2, rad: f32, hardness: f32, color: Color32) {
    let inner = rad * hardness.clamp(0.05, 1.0);
    let steps = 8;
    for k in (0..=steps).rev() {
        let f = k as f32 / steps as f32;
        let rr = inner + (rad - inner) * f;
        let a = if hardness >= 0.99 { 1.0 } else { (1.0 - f).powf(1.5) };
        p.circle_filled(c, rr, color.gamma_multiply(a));
    }
}

/// Options-bar Smoothing % (the brush's stroke smoothing; the live stroke and the commit use it).
fn smoothing_field(ui: &mut egui::Ui, b: &mut photocraft_engine::BrushSettings, width: f32) {
    let mut sm = (b.smoothing.amount * 100.0).round();
    if widgets::value_field(ui, &mut sm, 0.0..=100.0, "%", width).changed() {
        b.smoothing.amount = (sm / 100.0).clamp(0.0, 1.0);
    }
}

/// Options-bar brush chip; opens Photoshop's Brush Preset picker (size, hardness, the preset
/// library). Returns what the picker asked for beyond the size and hardness edits in `b`.
fn brush_preset_chip(
    ui: &mut egui::Ui,
    b: &mut photocraft_engine::BrushSettings,
    presets: &[photocraft_engine::paint::BrushPreset],
) -> Option<crate::brush_picker::Pick> {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(44.0, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, t.radius_sm, t.hover);
    }
    let c = pos2(r.left() + 14.0, r.top() + 11.0);
    brush_tip(ui.painter(), c, 7.0, b.hardness, Color32::WHITE);
    ui.painter().text(pos2(c.x, r.bottom() - 5.0), Align2::CENTER_CENTER, format!("{}", b.size.round() as i64), egui::FontId::proportional(9.5), t.text_dim);
    icons::paint(ui, Rect::from_center_size(pos2(r.right() - 9.0, c.y), vec2(10.0, 10.0)), "chevron-down", 9.0, t.text_faint);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Brush Preset picker")));
    let resp = resp.on_hover_text(tl!("Brush Preset picker"));
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| crate::brush_picker::body(ui, b, presets))
        .and_then(|r| r.inner)
}

/// Drag a layer row to reorder: drop on the upper/lower half to place above/below, or on the middle
/// of a group to move into it. One `layer.moveTo` command (one undo step).
fn layer_drag_and_drop(ctx: &egui::Context, ui: &egui::Ui, l: &Layer, rect: Rect, resp: &egui::Response, actions: &mut Vec<(String, Value)>) {
    let t = Tokens::get(ctx);
    let key = egui::Id::new("layer-drag");
    if resp.drag_started() {
        ctx.data_mut(|d| d.insert_temp(key, l.id.0));
    }
    let Some(dragged) = ctx.data(|d| d.get_temp::<u64>(key)) else { return };
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let released = ctx.input(|i| i.pointer.any_released());
    if dragged == l.id.0 {
        // Ghost label following the pointer.
        if let Some(p) = pointer {
            let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("layer-drag-ghost"));
            let painter = ctx.layer_painter(layer);
            let g = painter.layout_no_wrap(l.name.clone(), egui::FontId::proportional(12.0), t.text);
            let r = Rect::from_min_size(p + vec2(12.0, -10.0), g.size() + vec2(16.0, 8.0));
            painter.rect_filled(r, t.radius_sm, t.card.gamma_multiply(0.95));
            painter.rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.accent), StrokeKind::Inside);
            painter.galley(r.min + vec2(8.0, 4.0), g, t.text);
        }
        return;
    }
    let Some(p) = pointer else { return };
    if !rect.contains(p) {
        return;
    }
    let f = (p.y - rect.top()) / rect.height();
    let position = if l.is_group() && (0.3..0.7).contains(&f) {
        "into"
    } else if f < 0.5 {
        "above"
    } else {
        "below"
    };
    let painter = ui.painter();
    match position {
        "into" => {
            painter.rect_stroke(rect.shrink(1.0), t.radius_sm, Stroke::new(2.0, t.accent), StrokeKind::Inside);
        }
        "above" => {
            painter.line_segment([rect.left_top(), rect.right_top()], Stroke::new(2.0, t.accent));
        }
        _ => {
            painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(2.0, t.accent));
        }
    }
    if released {
        actions.push(("layer.moveTo".into(), json!({"layer": dragged, "target": l.id.0, "position": position})));
    }
}

/// Photoshop shows a layer's effects as indented sub-rows ("Effects", then each effect).
fn effect_rows(app: &mut PhotocraftApp, ui: &mut egui::Ui, l: &Layer, depth: usize) {
    let t = Tokens::get(ui.ctx());
    let indent = 30.0 + depth as f32 * 14.0 + 34.0;
    let mut rows: Vec<(String, bool, Option<&'static str>)> = vec![("Effects".into(), l.effects.enabled, None)];
    for e in &l.effects.items {
        let kind = crate::layer_style::KINDS.iter().find(|k| k.1 == e.label()).map(|k| k.0);
        rows.push((e.label().to_string(), e.enabled(), kind));
    }
    for (i, (name, on, kind)) in rows.into_iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.35));
        }
        if t.pro {
            ui.painter().line_segment([pos2(rect.left() + 30.0, rect.top()), pos2(rect.left() + 30.0, rect.bottom())], Stroke::new(1.0, t.separator));
        }
        let eye = Rect::from_min_size(pos2(rect.left() + 6.0, rect.center().y - 9.0), vec2(18.0, 18.0));
        icons::paint(ui, eye, if on { "eye" } else { "eye-off" }, 12.0, if on { t.icon } else { t.text_faint });
        let x = rect.left() + indent + if i == 0 { 0.0 } else { 16.0 };
        if i == 0 {
            icons::paint(ui, Rect::from_center_size(pos2(x - 12.0, rect.center().y), vec2(14.0, 14.0)), "sparkles", 11.0, t.text_dim);
        }
        let right = rect.right() - crate::layer_row_ui::RIGHT_PAD;
        crate::layer_row_ui::label(
            ui.painter(),
            x,
            rect.center().y,
            right,
            &name,
            egui::FontId::proportional(11.5),
            if on { t.text_dim } else { t.text_faint },
        );
        if resp.double_clicked() {
            crate::layer_style::open(app, kind);
        }
    }
}

/// New / add / subtract / intersect selection buttons (shared by selection tools).
fn selection_mode_buttons(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.x = 2.0;
    for (i, (icon, tip)) in [
        ("square", tl!("New selection").to_string()),
        ("plus", crate::i18n::fmt(tl!("Add to selection  ({key})"), &[("key", &crate::shortcuts::pretty("Shift"))])),
        ("minus", crate::i18n::fmt(tl!("Subtract from selection  ({key})"), &[("key", &crate::shortcuts::pretty("Alt"))])),
        ("squares-subtract", crate::i18n::fmt(tl!("Intersect with selection  ({key})"), &[("key", &crate::shortcuts::pretty("Shift+Alt"))])),
    ]
    .iter()
    .enumerate()
    {
        if icons::button(ui, icon, 24.0, app.ui.selection_mode == i as u8, tip).clicked() {
            app.ui.selection_mode = i as u8;
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

/// Gradient picker swatch (foreground → background), Photoshop options-bar style.
fn gradient_swatch(ui: &mut egui::Ui, a: [f32; 4], b: [f32; 4]) {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(96.0, 20.0), Sense::click());
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(r.left_top(), c32(a));
    mesh.colored_vertex(r.right_top(), c32(b));
    mesh.colored_vertex(r.right_bottom(), c32(b));
    mesh.colored_vertex(r.left_bottom(), c32(a));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
    let _ = resp.on_hover_text(tl!("Click to edit the gradient"));
}

#[cfg(test)]
mod color_tests {
    use super::*;

    #[test]
    fn srgb_hsva_roundtrip() {
        for c in [[0.847, 0.271, 0.180, 1.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0], [0.2, 0.6, 0.4, 1.0]] {
            let back = hsva_srgb(srgb_hsva(c));
            for i in 0..3 {
                assert!((back[i] - c[i]).abs() <= 1.0 / 255.0, "{c:?} -> {back:?}");
            }
        }
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use crate::canvas::{ToolEvent, tool_event};
    use egui::Modifiers;

    fn click_lock(app: &mut PhotocraftApp) {
        let st = app.session.active().unwrap();
        let l = st.active_layer.and_then(|id| st.doc.layer(id)).unwrap();
        let (cmd, p) = simple_lock_toggle(crate::doc_props_ui::is_background(&st.doc, l), l);
        app.run(&cmd, p).unwrap();
    }

    fn erase_line(app: &mut PhotocraftApp) {
        app.ui.tool = Tool::Eraser;
        let m = Modifiers::NONE;
        tool_event(app, ToolEvent::Down { x: 10.0, y: 40.0, pressure: 1.0 }, m);
        tool_event(app, ToolEvent::Move { x: 100.0, y: 40.0, pressure: 1.0 }, m);
        tool_event(app, ToolEvent::Up { x: 100.0, y: 40.0 }, m);
    }

    #[test]
    fn simple_lock_button_never_locks_the_eraser_out_of_the_background() {
        // #76: the simple themes' lock button used to toggle "lock all" on the Background, which
        // kept showing as locked and then refused every paint tool.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 120, "height": 80})).unwrap();
        app.run("tools.setColors", json!({"background": "#ff0000"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 12, "hardness": 1.0}})).unwrap();
        // Clicking the Background's lock converts it to a normal layer (Photoshop).
        click_lock(&mut app);
        let l = &app.session.active().unwrap().doc.layers[0];
        assert_eq!(l.name, "Layer 0");
        assert!(!(l.locks.transparency || l.locks.pixels || l.locks.position || l.locks.all));
        erase_line(&mut app);
        assert!(!app.ui.status_error, "{}", app.ui.status);
        assert_eq!(app.session.active().unwrap().doc.layers[0].surface().unwrap().rgba(50, 40)[3], 0.0, "erased to transparency");
        // On a normal layer the button locks all, then unlocks everything.
        click_lock(&mut app);
        assert!(app.session.active().unwrap().doc.layers[0].locks.all);
        click_lock(&mut app);
        let k = app.session.active().unwrap().doc.layers[0].locks;
        assert!(!(k.transparency || k.pixels || k.position || k.artboard || k.all));
        // A layer with only transparency locked unlocks in one click.
        app.run("layer.setProps", json!({"locks": {"transparency": true}})).unwrap();
        click_lock(&mut app);
        assert!(!app.session.active().unwrap().doc.layers[0].locks.transparency);
    }
}

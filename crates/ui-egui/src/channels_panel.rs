//! The Channels panel (tab of the Layers card): composite, colour, alpha / spot and Quick Mask
//! rows with thumbnails and eye toggles. Every action is an engine command (`channel.*`,
//! `select.loadSelection`, `select.saveSelection`, …); the targeted channel and the eyes live in
//! the engine's `DocState::channel_view`, so agents see the same state through `inspect`.

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_doc::ColorIndicates;
use photocraft_engine::channel_cmds::{self, ChannelTarget};
use serde_json::{Value, json};

use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Composite,
    Color(usize),
    Alpha(usize),
    QuickMask,
    /// The selected layer's mask: a temporary channel, listed while the layer is selected.
    LayerMask,
}

impl Row {
    /// The channel reference the engine commands take.
    fn reference(self) -> Value {
        match self {
            Row::Composite => json!("composite"),
            Row::Color(k) => json!({ "color": k }),
            Row::Alpha(i) => json!(i),
            Row::QuickMask => json!("quickMask"),
            Row::LayerMask => json!("mask"),
        }
    }
}

/// ⌘ = new selection, ⌘⇧ add, ⌘⌥ subtract, ⌘⌥⇧ intersect (Photoshop's thumbnail ⌘-clicks).
pub fn load_operation(m: egui::Modifiers) -> &'static str {
    match (m.shift, m.alt) {
        (true, true) => "intersect",
        (true, false) => "add",
        (false, true) => "subtract",
        _ => "new",
    }
}

pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        ui.add_space(6.0);
        ui.label(RichText::new(tl!("No document")).color(t.text_faint));
        return;
    };
    let doc = st.doc.clone();
    let view = st.channel_view.clone();
    // ⌥-click mask view (#196): gray shows the mask alone (colour eyes off), overlay over the composite.
    let mask_view = photocraft_engine::mask_view_cmds::current(st).map(|v| v.mode);
    let gray_view = mask_view == Some(photocraft_engine::mask_view_cmds::MaskViewMode::Gray);
    let active_layer = st.active_layer;
    let mode = doc.pixel_format().mode;
    let colors = mode.color_channels();
    let quick = doc.quick_mask.is_some();
    let ctx = ui.ctx().clone();
    let thumbs = app.channel_thumbs(&ctx);
    // The active layer's mask (Photoshop lists it, in italics, below the colour channels).
    let masked = active_layer.and_then(|id| doc.layer(id)).filter(|l| l.mask.is_some()).cloned();
    let mask_tex = masked.as_ref().and_then(|l| Some(app.mask_thumb(&ctx, &doc, l.id, l.mask.as_ref()?)));
    let mask_targeted = masked.is_some() && app.ui.mask_target && view.target == ChannelTarget::Composite && !quick;
    // Multichannel images are their ink channels only (no composite or colour rows).
    let multichannel = doc.mode == photocraft_doc::ColorMode::Multichannel;
    let mut rows = if multichannel { Vec::new() } else { vec![Row::Composite] };
    if colors > 1 && !multichannel {
        rows.extend((0..colors).map(Row::Color));
    }
    if masked.is_some() {
        rows.push(Row::LayerMask);
    }
    rows.extend((0..doc.channels.len()).map(Row::Alpha));
    if quick {
        rows.push(Row::QuickMask);
    }
    let shown_colors = if colors > 1 { colors } else { 0 };
    let mut actions: Vec<(String, Value)> = Vec::new();
    // The buttons sit in a footer at the panel's bottom, like Photoshop's.
    let footer = 34.0;
    let fill = ui.available_height() > footer + 60.0;
    let rows_h = if fill { ui.available_height() - footer } else { f32::INFINITY };
    let mut mask_click = None;
    let mut drawn = Vec::new();
    egui::ScrollArea::vertical().max_height(rows_h).min_scrolled_height(if fill { rows_h } else { 0.0 }).auto_shrink([false, !fill]).show(ui, |ui| {
        for row in rows {
            let (name, thumb_idx, slot, visible, selected) = match row {
                Row::Composite => (
                    channel_cmds::composite_name(mode).to_string(),
                    0,
                    Some(2),
                    view.visible_colors(colors) == colors && !gray_view,
                    view.target == ChannelTarget::Composite && !quick && !mask_targeted,
                ),
                Row::Color(k) => (
                    channel_cmds::color_names(mode)[k].to_string(),
                    1 + k,
                    Some(3 + k),
                    view.color_visible(k) && !gray_view,
                    (view.target == ChannelTarget::Composite && !quick && !mask_targeted) || view.target == ChannelTarget::Color(k),
                ),
                Row::Alpha(i) => (
                    doc.channels[i].name.clone(),
                    1 + shown_colors + i,
                    Some(3 + shown_colors + i).filter(|s| *s <= 9),
                    view.alpha_shown(i),
                    view.target == ChannelTarget::Alpha(i),
                ),
                Row::QuickMask => {
                    ("Quick Mask".to_string(), 1 + shown_colors + doc.channels.len(), None, !view.quick_mask_hidden, view.target == ChannelTarget::Composite)
                }
                Row::LayerMask => {
                    (masked.as_ref().map(|l| format!("{} Mask", l.name)).unwrap_or_default(), usize::MAX, None, mask_view.is_some(), mask_targeted)
                }
            };
            let row_h = if t.pro { 36.0 } else { 40.0 };
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let painter = ui.painter_at(rect.expand(1.0));
            if selected {
                painter.rect_filled(rect, if t.pro { 0.0 } else { t.radius }, if t.pro { t.row_selected } else { t.hover });
            } else if resp.hovered() {
                painter.rect_filled(rect, if t.pro { 0.0 } else { t.radius }, t.hover.gamma_multiply(0.5));
            }
            if t.pro {
                painter.line_segment([pos2(rect.left() + 30.0, rect.top()), pos2(rect.left() + 30.0, rect.bottom())], Stroke::new(1.0, t.separator));
                painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.separator));
            }
            let eye = Rect::from_min_size(pos2(rect.left() + 6.0, rect.center().y - 11.0), vec2(22.0, 22.0));
            let eye_resp = ui.interact(eye, ui.id().with(("chan-eye", format!("{:?}", row.reference()))), Sense::click());
            // Like Photoshop, the temporary mask row's eye is empty until the mask is shown.
            if row != Row::LayerMask || visible || eye_resp.hovered() {
                icons::paint(ui, eye, if visible { "eye" } else { "eye-off" }, 14.0, if visible { t.icon } else { t.text_faint });
            }
            let eye_resp = eye_resp.on_hover_text(if row == Row::LayerMask { tl!("Show the layer mask as an overlay") } else { tl!("Toggle visibility") });
            eye_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Visibility {name}")));
            let layer_id = masked.as_ref().map(|l| l.id.0);
            if eye_resp.clicked() {
                let view_cmd = photocraft_engine::mask_view_cmds::ID.to_string();
                match row {
                    // The mask's eye: the overlay on, or the mask view off.
                    Row::LayerMask => actions.push((view_cmd, json!({ "layer": layer_id, "mode": if visible { "off" } else { "overlay" } }))),
                    // Showing the composite again over a gray mask view keeps the mask as an overlay.
                    Row::Composite if gray_view => actions.push((view_cmd, json!({ "layer": layer_id, "mode": "overlay" }))),
                    _ => actions.push(("channel.setVisible".into(), json!({ "channel": row.reference(), "visible": !visible }))),
                }
            }
            let ts = if t.pro { 28.0 } else { 30.0 };
            let cell = Rect::from_min_size(pos2(rect.left() + 36.0, rect.center().y - ts / 2.0), vec2(ts, ts));
            // Thumbnails keep the document's aspect ratio (the textures are letterboxed squares).
            let (thumb, uv) = fit_thumb(cell, doc.size.width, doc.size.height);
            let tex = if row == Row::LayerMask { mask_tex } else { thumbs.get(thumb_idx).copied() };
            if let Some(tex) = tex {
                painter.image(tex, thumb, uv, Color32::WHITE);
            }
            drawn.push((name.clone(), thumb));
            // Rows are painted: name them for screen readers and UI tests.
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name));
            // Spot channels: a swatch of the ink on the thumbnail corner.
            if let Row::Alpha(i) = row
                && let Some((ink, _)) = doc.channels[i].spot
            {
                let c = ink.to_rgb().map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8);
                painter.rect_filled(Rect::from_min_size(thumb.right_bottom() - vec2(9.0, 9.0), vec2(9.0, 9.0)), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
            }
            painter.rect_stroke(thumb, 0.0, Stroke::new(1.0, if t.pro { Color32::from_gray(20) } else { t.field_border }), StrokeKind::Outside);
            let italic = matches!(row, Row::QuickMask | Row::LayerMask) || matches!(row, Row::Alpha(i) if doc.channels[i].spot.is_some());
            let mut job = egui::text::LayoutJob::default();
            let font = if selected && !t.pro { theme::medium(12.5) } else { egui::FontId::proportional(12.0) };
            job.append(&name, 0.0, egui::TextFormat { font_id: font, color: t.text, italics: italic, ..Default::default() });
            let galley = painter.layout_job(job);
            let text_pos = pos2(cell.right() + 10.0, rect.center().y - galley.size().y / 2.0);
            painter.galley(text_pos, galley, t.text);
            if let Some(slot) = slot {
                painter.text(
                    pos2(rect.right() - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    crate::shortcuts::pretty(&format!("Cmd+{slot}")),
                    theme::mono(11.0),
                    t.text_faint,
                );
            }
            let rename_id = egui::Id::new(("chan-rename", doc.id.0, format!("{:?}", row.reference())));
            if resp.clicked() && !eye_resp.clicked() {
                let m = ui.input(|i| i.modifiers);
                if m.command && row == Row::LayerMask {
                    let layer = masked.as_ref().map(|l| l.id.0);
                    actions.push(("select.loadSelection".into(), json!({ "channel": "mask", "layer": layer, "operation": load_operation(m) })));
                } else if m.command {
                    actions.push(("select.loadSelection".into(), json!({ "channel": row.reference(), "operation": load_operation(m) })));
                } else if row == Row::LayerMask {
                    // Targets the mask, as clicking its thumbnail in the Layers panel does.
                    mask_click = Some(true);
                    actions.push(("channel.target".into(), json!({ "channel": "composite" })));
                } else {
                    if matches!(row, Row::Composite | Row::Color(_)) {
                        mask_click = Some(false);
                    }
                    let target = if row == Row::QuickMask { json!("composite") } else { row.reference() };
                    actions.push(("channel.target".into(), json!({ "channel": target })));
                }
            }
            if let Row::Alpha(i) = row {
                if resp.double_clicked() {
                    ctx.data_mut(|d| d.insert_temp(rename_id, name.clone()));
                }
                if let Some(mut text) = ctx.data(|d| d.get_temp::<String>(rename_id)) {
                    let edit_rect = Rect::from_min_max(pos2(text_pos.x - 3.0, rect.center().y - 11.0), pos2(rect.right() - 36.0, rect.center().y + 11.0));
                    let te = ui.put(edit_rect, egui::TextEdit::singleline(&mut text).font(egui::FontId::proportional(12.5)));
                    te.request_focus();
                    let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                    if esc {
                        ctx.data_mut(|d| d.remove::<String>(rename_id));
                    } else if enter || te.lost_focus() {
                        ctx.data_mut(|d| d.remove::<String>(rename_id));
                        if !text.trim().is_empty() && text != name {
                            actions.push(("channel.rename".into(), json!({ "channel": i, "name": text.trim() })));
                        }
                    } else {
                        ctx.data_mut(|d| d.insert_temp(rename_id, text));
                    }
                }
            }
            resp.context_menu(|ui| {
                ui.set_min_width(210.0);
                let a = &mut actions;
                match row {
                    Row::Alpha(i) => {
                        let ch = &doc.channels[i];
                        item(ui, a, "Duplicate Channel", "channel.duplicate", json!({ "channel": i }));
                        item(ui, a, "Delete Channel", "channel.delete", json!({ "channel": i }));
                        item(ui, a, "Rename Channel…", "ui.renameChannel", json!(i));
                        ui.separator();
                        if ch.spot.is_some() {
                            item(ui, a, "Merge Spot Channel", "channel.mergeSpot", json!({ "channel": i }));
                            item(ui, a, "Convert to Alpha Channel", "channel.options", json!({ "channel": i, "indicates": "masked" }));
                        } else {
                            indicates_items(ui, a, json!(i), ch.indicates);
                            overlay_colors(ui, a, json!(i));
                        }
                    }
                    Row::QuickMask => {
                        item(ui, a, "Exit Quick Mask", "select.editInQuickMaskMode", json!({ "on": false }));
                        indicates_items(ui, a, json!("quickMask"), doc.quick_mask.as_ref().map(|q| q.indicates).unwrap_or_default());
                        overlay_colors(ui, a, json!("quickMask"));
                    }
                    Row::Composite | Row::Color(_) => {
                        item(ui, a, "Duplicate Channel", "channel.duplicate", json!({ "channel": row.reference() }));
                    }
                    Row::LayerMask => {
                        let enabled = masked.as_ref().and_then(|l| l.mask.as_ref()).is_none_or(|m| m.enabled);
                        item(ui, a, if enabled { "Disable Layer Mask" } else { "Enable Layer Mask" }, "layer.layerMask.enabled", json!({}));
                        item(ui, a, "Delete Layer Mask", "layer.layerMask.delete", json!({}));
                    }
                }
                ui.separator();
                item(ui, a, "New Channel…", "channel.new", json!({}));
                item(ui, a, "New Spot Channel…", "channel.newSpot", json!({}));
                ui.separator();
                item(ui, a, "Split Channels", "channel.split", json!({}));
                item(ui, a, "Merge Channels…", "channel.merge", json!({}));
            });
        }
    });
    ui.add_space(4.0);
    widgets::hairline(ui);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let target_ref = match view.target {
            ChannelTarget::Alpha(i) => json!(i),
            ChannelTarget::Color(k) => json!({ "color": k }),
            ChannelTarget::Composite => json!("composite"),
        };
        if icons::button(ui, "circle-dashed", 26.0, false, tl!("Load channel as selection")).clicked() {
            actions.push(("select.loadSelection".into(), json!({ "channel": target_ref })));
        }
        if icons::button(ui, "square-dashed", 26.0, false, tl!("Save selection as channel")).clicked() {
            actions.push(("select.saveSelection".into(), json!({})));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, "trash", 26.0, false, tl!("Delete current channel")).clicked() {
                actions.push(("channel.delete".into(), json!({})));
            }
            if icons::button(ui, "plus", 26.0, false, tl!("Create new channel")).clicked() {
                actions.push(("channel.new".into(), json!({})));
            }
        });
    });
    ctx.data_mut(|d| d.insert_temp(thumbs_id(), drawn));
    if let Some(on) = mask_click {
        app.ui.mask_target = on;
    }
    for (id, p) in actions {
        if id == "ui.renameChannel" {
            if let Some(i) = p.as_u64().and_then(|i| doc.channels.get(i as usize)) {
                let row = Row::Alpha(p.as_u64().unwrap_or(0) as usize);
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(("chan-rename", doc.id.0, format!("{:?}", row.reference()))), i.name.clone()));
            }
            continue;
        }
        let _ = app.run(&id, p);
    }
}

fn thumbs_id() -> egui::Id {
    egui::Id::new("channel-thumb-rects")
}

/// The Channels rows drawn last frame: (name, thumbnail rect).
pub fn recorded(ctx: &egui::Context) -> Vec<(String, Rect)> {
    ctx.data(|d| d.get_temp(thumbs_id())).unwrap_or_default()
}

/// The part of a square `cell` a `w`×`h` document fills, and the matching UVs of a square,
/// letterboxed thumbnail texture: thumbnails keep the document's aspect ratio.
pub fn fit_thumb(cell: Rect, w: u32, h: u32) -> (Rect, Rect) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let (fw, fh) = (w / w.max(h), h / w.max(h));
    let size = vec2(cell.width() * fw, cell.height() * fh).max(vec2(1.0, 1.0));
    let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2(fw, fh));
    (Rect::from_center_size(cell.center(), size), uv)
}

fn item(ui: &mut egui::Ui, actions: &mut Vec<(String, Value)>, label: &str, cmd: &str, p: Value) {
    if ui.button(tl!(&label)).clicked() {
        actions.push((cmd.into(), p));
        ui.close();
    }
}

/// Channel Options › Color Indicates.
fn indicates_items(ui: &mut egui::Ui, actions: &mut Vec<(String, Value)>, channel: Value, current: ColorIndicates) {
    let masked = current == ColorIndicates::MaskedAreas;
    let check = |on: bool, s: &str| if on { format!("✓ {s}") } else { s.to_string() };
    item(ui, actions, &check(masked, "Color Indicates Masked Areas"), "channel.options", json!({ "channel": channel.clone(), "indicates": "masked" }));
    item(ui, actions, &check(!masked, "Color Indicates Selected Areas"), "channel.options", json!({ "channel": channel, "indicates": "selected" }));
}

/// Overlay colour presets (Channel Options › Color) and opacity.
fn overlay_colors(ui: &mut egui::Ui, actions: &mut Vec<(String, Value)>, channel: Value) {
    ui.menu_button(tl!("Overlay Color"), |ui| {
        for (label, hex) in [
            (tl!("Red"), "#ff0000"),
            (tl!("Green"), "#00c000"),
            (tl!("Blue"), "#0050ff"),
            (tl!("Cyan"), "#00d0e0"),
            (tl!("Magenta"), "#e000c0"),
            (tl!("Yellow"), "#f0d000"),
        ] {
            if ui.button(tl!(&label)).clicked() {
                actions.push(("channel.options".into(), json!({ "channel": channel.clone(), "color": hex })));
                ui.close();
            }
        }
    });
    ui.menu_button(tl!("Overlay Opacity"), |ui| {
        for o in [25, 50, 75, 100] {
            if ui.button(format!("{o}%")).clicked() {
                actions.push(("channel.options".into(), json!({ "channel": channel.clone(), "opacity": o })));
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_operations() {
        let m = |shift, alt| egui::Modifiers { command: true, shift, alt, ..Default::default() };
        assert_eq!(load_operation(m(false, false)), "new");
        assert_eq!(load_operation(m(true, false)), "add");
        assert_eq!(load_operation(m(false, true)), "subtract");
        assert_eq!(load_operation(m(true, true)), "intersect");
    }
}

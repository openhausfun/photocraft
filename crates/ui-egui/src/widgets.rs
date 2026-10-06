//! Custom widgets for the Photocraft look: cards with pill tabs, thin sliders with round knobs,
//! monospace value fields with dimmed units, toggle switches, primary/secondary buttons.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::{self, Tokens};

/// Draw a bevelled box (Classic theme) or a flat rounded box.
pub fn surface(ui: &Ui, rect: Rect, fill: Color32, raised: bool) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter();
    p.rect_filled(rect, t.radius_sm, fill);
    if t.bevel {
        let (hi, lo) = if raised { (Color32::WHITE, Color32::from_gray(64)) } else { (Color32::from_gray(64), Color32::WHITE) };
        p.line_segment([rect.left_bottom(), rect.left_top()], Stroke::new(1.0, hi));
        p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, hi));
        p.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, lo));
        p.line_segment([rect.right_bottom(), rect.left_bottom()], Stroke::new(1.0, lo));
    }
}

/// A dock card: rounded container with a header of pill tabs and optional trailing actions.
/// Returns the index of the selected tab.
pub fn card(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, body: impl FnOnce(&mut Ui, usize)) {
    let _ = card_ex(ui, id, tabs, selected, false, body);
}

/// What happened on a card's tab strip this frame (see [`card_ex`]).
pub struct CardResponse {
    /// The tab strip background: drag to move the group, double-click to collapse it.
    pub strip: Response,
    /// The panel menu button (hamburger in Pro, ellipsis in Studio).
    pub menu: Response,
    /// A tab was double-clicked (Photoshop collapses the group).
    pub tab_double_clicked: bool,
    /// Rects of the tabs on the strip, `(tab index, rect)`; tabs that don't fit are in the
    /// chevron menu instead (#151).
    pub tabs: Vec<(usize, Rect)>,
    /// The » overflow button, when some tabs didn't fit.
    pub chevron: Option<Rect>,
}

/// [`card`] that can be collapsed to its tab strip and reports strip and menu interactions.
pub fn card_ex(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, collapsed: bool, body: impl FnOnce(&mut Ui, usize)) -> CardResponse {
    let t = Tokens::get(ui.ctx());
    if t.pro {
        return pro_panel(ui, id, tabs, selected, collapsed, body);
    }
    let frame = egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius as u8))
        .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: if collapsed { 6 } else { 10 } });
    let out = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Registered before the tabs so they keep their clicks; drags fall through to it.
            let strip_rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 22.0));
            let strip = ui.interact(strip_rect, ui.id().with((id, "strip")), Sense::click_and_drag());
            // Pill tabs left of the menu button; they elide or overflow into a chevron (#151).
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let tip = crate::i18n::fmt(tl!("{name} options"), &[("name", tl!(tabs.get(*selected).copied().unwrap_or(id)))]);
            let menu_rect = Rect::from_min_max(pos2(row.right() - 22.0, row.top() + 1.0), pos2(row.right(), row.bottom() - 1.0));
            let menu = crate::icons::button(&mut ui.new_child(egui::UiBuilder::new().max_rect(menu_rect)), "ellipsis", 22.0, false, &tip);
            let area = Rect::from_min_max(row.min, pos2((menu_rect.left() - 4.0).max(row.left()), row.bottom()));
            let tabs_out = crate::tab_strip::pill_tabs(ui, ui.id().with((id, "tabs")), area, tabs, selected);
            if !collapsed {
                ui.add_space(6.0);
                body(ui, *selected);
            }
            CardResponse { strip, menu, tab_double_clicked: tabs_out.double_clicked, tabs: tabs_out.tabs, chevron: tabs_out.chevron }
        })
        .inner;
    ui.add_space(6.0);
    out
}

/// Photoshop-grammar panel group: dark tab strip with flat tabs, flat body, hamburger menu.
fn pro_panel(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, collapsed: bool, body: impl FnOnce(&mut Ui, usize)) -> CardResponse {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width();
    // Tab strip. Its background senses drags (move the group) and double-clicks (collapse);
    // the tabs, registered after it, keep their clicks.
    let (strip, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let strip_resp = ui.interact(strip, ui.id().with((id, "strip")), Sense::click_and_drag());
    let rounding = if collapsed { CornerRadius::same(3) } else { CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 } };
    ui.painter().rect_filled(strip, rounding, t.tab_strip);
    // Panel menu (hamburger); the tabs stay left of it, eliding or overflowing (#151).
    let menu = Rect::from_center_size(pos2(strip.right() - 14.0, strip.center().y), vec2(20.0, 18.0));
    let tabs_out = crate::tab_strip::pro_tabs(ui, ui.id().with((id, "tabs")), strip, menu.left(), tabs, selected, collapsed);
    let mresp = ui.interact(menu, ui.id().with((id, "menu")), Sense::click());
    let c = if mresp.hovered() { t.text } else { t.text_faint };
    for k in 0..3 {
        let y = menu.center().y - 3.5 + k as f32 * 3.5;
        ui.painter().line_segment([pos2(menu.center().x - 5.0, y), pos2(menu.center().x + 5.0, y)], Stroke::new(1.0, c));
    }
    // Body.
    if !collapsed {
        egui::Frame::NONE
            .fill(t.card)
            .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 3, se: 3 })
            .inner_margin(egui::Margin { left: 8, right: 8, top: 8, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(width - 16.0);
                body(ui, *selected);
            });
    }
    ui.add_space(2.0);
    CardResponse { strip: strip_resp, menu: mresp, tab_double_clicked: tabs_out.double_clicked, tabs: tabs_out.tabs, chevron: tabs_out.chevron }
}

pub fn pill_tab(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(12.5);
    let galley = ui.painter().layout_no_wrap(tl!(label).to_owned(), font, t.text);
    let size = vec2(galley.size().x + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if selected {
        surface(ui, rect, t.hover, true);
        if !t.bevel {
            ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
        }
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover.gamma_multiply(0.6));
    }
    let color = if selected { t.text } else { t.text_dim };
    ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Monospace numeric field with a dimmed unit suffix, Photoshop style. Drag to scrub.
pub fn value_field(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    surface(ui, rect, t.field, false);
    if !t.bevel {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    let suffix_w = if suffix.is_empty() { 0.0 } else { 16.0 };
    let field = Rect::from_min_max(rect.min + vec2(4.0, 2.0), rect.max - vec2(4.0 + suffix_w, 2.0));
    // Small ranges (gamma 0.01–9.99, 0–1 centres) need two decimals and a finer drag, like Photoshop.
    let fine = range.end() - range.start() <= 10.0;
    // new_child (not scope_builder): a scope would move the parent cursor back to the child rect.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
    let resp = {
        let ui = &mut child;
        {
            ui.style_mut().visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.hovered.weak_bg_fill = Color32::TRANSPARENT;
            ui.style_mut().override_font_id = Some(theme::mono(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    field.size(),
                    egui::DragValue::new(value)
                        .range(range)
                        .speed(if fine { 0.01 } else { 0.5 })
                        .custom_formatter(|v, _| if fine { fmt_num2(v) } else { fmt_num(v) }),
                )
            })
            .inner
        }
    };
    if !suffix.is_empty() {
        ui.painter().text(pos2(rect.right() - 6.0, rect.center().y), Align2::RIGHT_CENTER, suffix, theme::mono(11.0), t.text_faint);
    }
    resp
}

/// Thin-track slider with a round knob. `gradient` paints the track (e.g. hue spectrum).
pub fn slider(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, gradient: Option<&[Color32]>) -> Response {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width().max(60.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 14.0, if gradient.is_some() { 5.0 } else { 3.0 }));
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let f = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
        let nv = lo + f * (hi - lo);
        if (nv - *value).abs() > f32::EPSILON {
            *value = nv;
            resp.mark_changed();
        }
    }
    let f = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0);
    let knob_x = track.left() + f * track.width();
    let painter = ui.painter();
    match gradient {
        Some(colors) if colors.len() >= 2 => {
            let n = colors.len() - 1;
            for (i, w) in colors.windows(2).enumerate() {
                let x0 = track.left() + track.width() * i as f32 / n as f32;
                let x1 = track.left() + track.width() * (i + 1) as f32 / n as f32;
                let mut mesh = egui::Mesh::default();
                let r = Rect::from_min_max(pos2(x0, track.top()), pos2(x1, track.bottom()));
                mesh.colored_vertex(r.left_top(), w[0]);
                mesh.colored_vertex(r.right_top(), w[1]);
                mesh.colored_vertex(r.right_bottom(), w[1]);
                mesh.colored_vertex(r.left_bottom(), w[0]);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(mesh);
            }
        }
        _ => {
            painter.rect_filled(track, 2.0, t.field_border);
            let filled = Rect::from_min_max(track.min, pos2(knob_x, track.max.y));
            painter.rect_filled(filled, 2.0, if t.bevel { t.accent } else { t.text_dim });
        }
    }
    let knob = pos2(knob_x, rect.center().y);
    if t.bevel {
        let kr = Rect::from_center_size(knob, vec2(10.0, 16.0));
        surface(ui, kr, t.card, true);
    } else {
        let kr = if t.pro { 6.0 } else { 7.0 };
        painter.circle_filled(knob + vec2(0.0, 1.0), kr + 0.5, Color32::from_black_alpha(90));
        painter.circle_filled(knob, kr, if t.pro { Color32::from_gray(236) } else { Color32::WHITE });
        if t.pro {
            painter.circle_stroke(knob, kr, Stroke::new(1.0, Color32::from_gray(40)));
        }
        if resp.hovered() || resp.dragged() {
            painter.circle_stroke(knob, 9.5, Stroke::new(2.0, t.accent_soft));
        }
    }
    resp
}

/// Labelled slider row: `Label ........ [value field]` above a full-width thin slider.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, gradient: Option<&[Color32]>) -> Response {
    let t = Tokens::get(ui.ctx());
    let mut changed_resp = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!(label)).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed_resp = Some(value_field(ui, value, range.clone(), suffix, 74.0));
        });
    });
    let s = slider(ui, value, range, gradient);
    let mut r = s.clone();
    if let Some(v) = changed_resp
        && v.changed()
    {
        r.mark_changed();
    }
    ui.add_space(4.0);
    r
}

/// iOS-style toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    if t.pro {
        return checkbox(ui, on, label);
    }
    let mut resp = ui
        .horizontal(|ui| {
            let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 17.0), Sense::click());
            let how_on = ui.ctx().animate_bool(resp.id, *on);
            let bg = if *on { t.accent } else { t.field_border };
            if t.bevel {
                surface(ui, rect, if *on { t.accent } else { t.field }, false);
            } else {
                ui.painter().rect_filled(rect, 8.5, bg);
            }
            let x = egui::lerp((rect.left() + 8.5)..=(rect.right() - 8.5), how_on);
            ui.painter().circle_filled(pos2(x, rect.center().y), 6.5, Color32::WHITE);
            ui.label(egui::RichText::new(tl!(label)).color(if *on { t.text } else { t.text_dim }));
            resp
        })
        .inner;
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

/// Big white primary button.
pub fn primary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    button_impl(ui, label, min_width, t.primary_bg, t.primary_text, true)
}

pub fn secondary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    button_impl(ui, label, min_width, t.field, t.text, false)
}

fn button_impl(ui: &mut Ui, label: &str, min_width: f32, bg: Color32, fg: Color32, primary: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(tl!(label).to_owned(), theme::medium(13.0), fg);
    let h = if t.pro { 28.0 } else { 30.0 };
    let size = vec2((galley.size().x + 28.0).max(min_width), h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // Painted text: name the button for accessibility (and so tests and agents can find it).
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    if t.pro {
        // Spectrum buttons: fully rounded; primary = filled accent, secondary = outline.
        let down = resp.is_pointer_button_down_on();
        let r = h / 2.0;
        if primary {
            let fill = if down {
                bg.gamma_multiply(0.8)
            } else if resp.hovered() {
                bg.gamma_multiply(0.9)
            } else {
                bg
            };
            ui.painter().rect_filled(rect, r, fill);
        } else {
            if resp.hovered() || down {
                ui.painter().rect_filled(rect, r, t.hover);
            }
            ui.painter().rect_stroke(rect, r, Stroke::new(1.5, if resp.hovered() { t.text } else { t.text_dim }), StrokeKind::Inside);
        }
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
        return resp;
    }
    let fill = if resp.is_pointer_button_down_on() {
        bg.gamma_multiply(0.85)
    } else if resp.hovered() {
        if primary { bg.gamma_multiply(0.93) } else { t.hover }
    } else {
        bg
    };
    surface(ui, rect, fill, !resp.is_pointer_button_down_on());
    if !t.bevel && !primary {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

/// Small caps section label.
pub fn section_label(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!(text)).font(theme::medium(11.5)).color(t.text_faint));
}

/// Hairline separator.
pub fn hairline(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.separator));
}

/// Vertical hairline for horizontal layouts.
pub fn vline(ui: &mut Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, t.separator));
}

/// Hue spectrum stops for colour sliders.
pub fn hue_stops() -> Vec<Color32> {
    (0..=12)
        .map(|i| {
            let h = i as f32 / 12.0;
            let rgb = egui::ecolor::Hsva::new(h, 0.85, 1.0, 1.0).to_srgb();
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        })
        .collect()
}

/// A compact labelled dropdown in the studio style.
pub fn dropdown<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    let label = options.iter().find(|(v, _)| v == current).map(|(_, l)| tl!(l)).unwrap_or("—");
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(420.0).icon(chevron_icon).show_ui(ui, |ui| {
        for (v, l) in options {
            if ui.selectable_label(v == current, tl!(l)).clicked() {
                *current = v.clone();
                changed = true;
            }
        }
    });
    changed
}

/// Paint a small checkerboard (transparency) in `rect`.
pub fn checker(painter: &egui::Painter, rect: Rect, cell: f32) {
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    let nx = (rect.width() / cell).ceil() as i32;
    let ny = (rect.height() / cell).ceil() as i32;
    for j in 0..ny {
        for i in 0..nx {
            if (i + j) % 2 == 1 {
                let r = Rect::from_min_size(Pos2::new(rect.left() + i as f32 * cell, rect.top() + j as f32 * cell), Vec2::splat(cell)).intersect(rect);
                painter.rect_filled(r, 0.0, Color32::from_gray(214));
            }
        }
    }
}

/// Spectrum-style checkbox (blue when checked).
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let mut resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            let p = ui.painter();
            if *on {
                p.rect_filled(rect, 2.0, t.accent);
                let a = rect.left_center() + vec2(3.0, 0.5);
                let b = rect.center_bottom() + vec2(-1.0, -3.5);
                let c = rect.right_top() + vec2(-3.0, 3.5);
                p.line_segment([a, b], Stroke::new(1.8, Color32::WHITE));
                p.line_segment([b, c], Stroke::new(1.8, Color32::WHITE));
            } else {
                p.rect_filled(rect, 2.0, t.field);
                p.rect_stroke(rect, 2.0, Stroke::new(1.5, if resp.hovered() { t.text_dim } else { t.text_faint }), StrokeKind::Inside);
            }
            let l = ui.add(egui::Label::new(egui::RichText::new(tl!(label)).color(t.text_dim)).sense(Sense::click()));
            resp.union(l)
        })
        .inner;
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

/// Small chevron for dropdowns (replaces egui's filled triangle).
pub fn chevron_icon(ui: &Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, _open: bool) {
    let c = rect.center();
    let s = 3.2;
    let stroke = Stroke::new(1.3, visuals.fg_stroke.color.gamma_multiply(0.8));
    ui.painter().line_segment([c + vec2(-s, -s * 0.5), c + vec2(0.0, s * 0.5)], stroke);
    ui.painter().line_segment([c + vec2(0.0, s * 0.5), c + vec2(s, -s * 0.5)], stroke);
}

/// Photoshop-style numbers: "100", "12.5" (never "100.0").
pub fn fmt_num(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 1e-9 { format!("{}", r.round() as i64) } else { format!("{r:.1}") }
}

/// Two-decimal variant of [`fmt_num`] for small ranges: `1.05`, `0.78`, `2`.
pub fn fmt_num2(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{r:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn two_decimal_numbers_trim_zeros() {
        assert_eq!(super::fmt_num2(1.05), "1.05");
        assert_eq!(super::fmt_num2(0.78), "0.78");
        assert_eq!(super::fmt_num2(0.5), "0.5");
        assert_eq!(super::fmt_num2(2.0), "2");
    }

    #[test]
    fn numbers_drop_trailing_zero() {
        assert_eq!(super::fmt_num(100.0), "100");
        assert_eq!(super::fmt_num(12.46), "12.5");
        assert_eq!(super::fmt_num(-3.0), "-3");
    }
}

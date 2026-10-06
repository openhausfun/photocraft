//! Dock tab strips that fit any width (#151), like Photoshop's: when the tabs don't fit beside
//! the panel menu button they shrink, eliding their labels with '…', and once they are at their
//! minimum width the ones that still don't fit move into a » chevron menu at the end of the
//! strip. The selected tab always stays on the strip, and no tab ever runs under the menu button.

use egui::{CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::theme::{self, Tokens};

/// Which tabs a strip shows, at what widths, and which overflow into the chevron menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabFit {
    /// `(tab index, width)` for the tabs on the strip, in tab order.
    pub shown: Vec<(usize, f32)>,
    /// Tabs in the chevron menu, in tab order (empty: no chevron).
    pub overflow: Vec<usize>,
}

/// Fit tabs with `natural` widths into `avail` points. Tabs shrink (never below `min_w`, or
/// their natural width if smaller) before any overflow; overflowing tabs leave `chevron_w`
/// for the chevron. `selected` is kept on the strip whatever happens.
pub fn fit(natural: &[f32], selected: usize, avail: f32, min_w: f32, chevron_w: f32) -> TabFit {
    let clean = |w: f32| if w.is_finite() { w.max(0.0) } else { 0.0 };
    let avail = clean(avail);
    let min_w = clean(min_w);
    let natural: Vec<f32> = natural.iter().map(|w| clean(*w)).collect();
    let floor = |w: f32| w.min(min_w);
    let all: Vec<usize> = (0..natural.len()).collect();
    if natural.iter().sum::<f32>() <= avail {
        return TabFit { shown: all.iter().map(|&i| (i, natural.get(i).copied().unwrap_or(0.0))).collect(), overflow: Vec::new() };
    }
    if natural.iter().map(|w| floor(*w)).sum::<f32>() <= avail {
        return TabFit { shown: squeeze(&natural, &all, avail, min_w), overflow: Vec::new() };
    }
    // Overflow: the selected tab first, then the others in order, while they fit at their minimum.
    let budget = (avail - clean(chevron_w)).max(0.0);
    let sel = selected.min(natural.len().saturating_sub(1));
    let mut keep: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for i in std::iter::once(sel).chain(all.iter().copied().filter(|i| *i != sel)) {
        let w = natural.get(i).map_or(0.0, |w| floor(*w));
        if used + w <= budget || (i == sel && !natural.is_empty()) {
            keep.push(i);
            used += w;
        }
    }
    keep.sort_unstable();
    let mut shown = squeeze(&natural, &keep, budget, min_w);
    // A strip too narrow even for the selected tab at its minimum: it takes what there is.
    if used > budget
        && let Some(s) = shown.iter_mut().find(|(i, _)| *i == sel)
    {
        s.1 = budget;
    }
    let overflow = all.into_iter().filter(|i| !keep.contains(i)).collect();
    TabFit { shown, overflow }
}

/// Widths for the `idx` tabs filling `total`: the widest shrink first (a common cap), none
/// below `min(natural, min_w)`.
fn squeeze(natural: &[f32], idx: &[usize], total: f32, min_w: f32) -> Vec<(usize, f32)> {
    let ws: Vec<f32> = idx.iter().map(|&i| natural.get(i).copied().unwrap_or(0.0)).collect();
    let sum_at = |cap: f32| ws.iter().map(|w| w.min(cap.max(w.min(min_w)))).sum::<f32>();
    let (mut lo, mut hi) = (min_w, ws.iter().copied().fold(min_w, f32::max));
    if sum_at(hi) > total {
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if sum_at(mid) <= total {
                lo = mid;
            } else {
                hi = mid;
            }
        }
    } else {
        lo = hi;
    }
    idx.iter().zip(ws).map(|(&i, w)| (i, w.min(lo.max(w.min(min_w))))).collect()
}

/// A label laid out on one row no wider than `max_w`, cut with '…' when it doesn't fit.
pub fn elided(ui: &Ui, text: &str, font: egui::FontId, color: egui::Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_w.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// What a strip reported this frame.
pub struct StripOut {
    pub double_clicked: bool,
    /// Rects of the tabs on the strip, `(tab index, rect)`.
    pub tabs: Vec<(usize, Rect)>,
    /// The » overflow button, when some tabs didn't fit.
    pub chevron: Option<Rect>,
}

/// Width of the » overflow button.
const CHEVRON_W: f32 = 18.0;

/// Draw the tabs of a strip in `area` (the strip minus the menu button). `paint_tab` draws one
/// tab: (ui, rect, index, label galley, response, active).
#[allow(clippy::too_many_arguments)]
fn tabs_in(
    ui: &mut Ui,
    id: egui::Id,
    area: Rect,
    tabs: &[&str],
    selected: &mut usize,
    font: egui::FontId,
    pad: f32,
    min_w: f32,
    active: impl Fn(usize) -> bool,
    mut paint_tab: impl FnMut(&Ui, Rect, usize, std::sync::Arc<egui::Galley>, &Response, bool),
) -> StripOut {
    let t = Tokens::get(ui.ctx());
    // Panel names are English keys; draw them in the UI language.
    let names: Vec<&str> = tabs.iter().map(|n| tl!(n)).collect();
    let tabs = names.as_slice();
    let natural: Vec<f32> = tabs.iter().map(|n| ui.painter().layout_no_wrap((*n).to_owned(), font.clone(), t.text).size().x + pad).collect();
    let f = fit(&natural, *selected, area.width(), min_w, CHEVRON_W);
    let mut x = area.left();
    let mut out = StripOut { double_clicked: false, tabs: Vec::with_capacity(f.shown.len()), chevron: None };
    for &(i, w) in &f.shown {
        let Some(name) = tabs.get(i) else { continue };
        let r = Rect::from_min_size(pos2(x, area.top()), vec2(w, area.height()));
        let resp = ui.interact(r, id.with(("tab", i)), Sense::click());
        // The padding gives way (down to a third) before the label is cut.
        let galley = elided(ui, name, font.clone(), t.text, (w - pad / 3.0).max(1.0));
        let cut = galley.size().x + pad + 0.5 < natural.get(i).copied().unwrap_or(0.0) && galley.size().x + pad / 3.0 >= w - 0.5;
        paint_tab(ui, r, i, galley, &resp, active(i));
        let resp = if cut { resp.on_hover_text(*name) } else { resp };
        out.double_clicked |= resp.double_clicked();
        if resp.clicked() {
            *selected = i;
        }
        out.tabs.push((i, r));
        x = r.right();
    }
    if !f.overflow.is_empty() {
        let r = Rect::from_min_size(pos2(x, area.top()), vec2(CHEVRON_W, area.height()));
        let resp = ui.interact(r, id.with("tab-overflow"), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r.shrink2(vec2(1.0, 3.0)), t.radius_sm, t.hover.gamma_multiply(0.6));
        }
        crate::icons::paint(ui, r, "chevrons-right", 12.0, if resp.hovered() { t.text } else { t.text_dim });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("More panels")));
        let resp = resp.on_hover_text(tl!("More panels"));
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(140.0);
            for &i in &f.overflow {
                if let Some(name) = tabs.get(i)
                    && ui.button(*name).clicked()
                {
                    *selected = i;
                    ui.close();
                }
            }
        });
        out.chevron = Some(r);
    }
    out
}

/// Photoshop-grammar strip (Pro themes): flat tabs on the dark strip; `menu_left` is where the
/// panel menu button starts.
pub fn pro_tabs(ui: &mut Ui, id: egui::Id, strip: Rect, menu_left: f32, tabs: &[&str], selected: &mut usize, collapsed: bool) -> StripOut {
    let t = Tokens::get(ui.ctx());
    let area = Rect::from_min_max(strip.min, pos2(menu_left.max(strip.left()), strip.bottom()));
    let sel = *selected;
    tabs_in(
        ui,
        id,
        area,
        tabs,
        selected,
        egui::FontId::proportional(11.5),
        22.0,
        40.0,
        |i| i == sel && !collapsed,
        |ui, r, i, galley, resp, active| {
            if active {
                ui.painter().rect_filled(r, CornerRadius { nw: if i == 0 { 3 } else { 0 }, ne: 0, sw: 0, se: 0 }, t.card);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.4));
            }
            let color = if active {
                t.text
            } else if resp.hovered() {
                t.text_dim
            } else {
                t.text_faint
            };
            ui.painter().galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, color);
        },
    )
}

/// Studio strip: pill tabs in `area` (the row minus the menu button).
pub fn pill_tabs(ui: &mut Ui, id: egui::Id, area: Rect, tabs: &[&str], selected: &mut usize) -> StripOut {
    let t = Tokens::get(ui.ctx());
    let sel = *selected;
    tabs_in(
        ui,
        id,
        area,
        tabs,
        selected,
        theme::medium(12.5),
        20.0,
        44.0,
        |i| i == sel,
        |ui, r, _, galley, resp, active| {
            // 2 pt between pills, as the old horizontal layout had.
            let r = Rect::from_min_max(r.min, pos2((r.right() - 2.0).max(r.left()), r.bottom()));
            if active {
                crate::widgets::surface(ui, r, t.hover, true);
                if !t.bevel {
                    ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
                }
            } else if resp.hovered() {
                ui.painter().rect_filled(r, t.radius_sm, t.hover.gamma_multiply(0.6));
            }
            let color = if active { t.text } else { t.text_dim };
            ui.painter().galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, color);
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(f: &TabFit) -> f32 {
        f.shown.iter().map(|(_, w)| w).sum()
    }

    #[test]
    fn tabs_that_fit_keep_their_widths() {
        let f = fit(&[60.0, 70.0, 80.0], 0, 400.0, 40.0, 18.0);
        assert_eq!(f.shown, vec![(0, 60.0), (1, 70.0), (2, 80.0)]);
        assert!(f.overflow.is_empty());
    }

    #[test]
    fn narrow_strips_shrink_the_widest_tabs_first() {
        let f = fit(&[50.0, 70.0, 120.0], 0, 200.0, 40.0, 18.0);
        assert!(f.overflow.is_empty());
        assert!((total(&f) - 200.0).abs() < 0.1, "{f:?}");
        assert_eq!(f.shown[0], (0, 50.0), "a tab narrower than the cap keeps its width");
        assert_eq!(f.shown[1], (1, 70.0));
        assert!((f.shown[2].1 - 80.0).abs() < 0.1, "only the widest shrank: {f:?}");
    }

    #[test]
    fn overflow_keeps_the_selected_tab_and_leaves_room_for_the_chevron() {
        let natural = [80.0, 90.0, 85.0, 95.0];
        for sel in 0..4 {
            let f = fit(&natural, sel, 120.0, 40.0, 18.0);
            assert!(f.shown.iter().any(|(i, _)| *i == sel), "{sel}: {f:?}");
            assert!(!f.overflow.is_empty() && !f.overflow.contains(&sel));
            assert!(total(&f) <= 120.0 - 18.0 + 1e-3, "{f:?}");
            assert_eq!(f.shown.len() + f.overflow.len(), 4);
            assert!(f.shown.windows(2).all(|w| w[0].0 < w[1].0), "tab order kept");
        }
    }

    #[test]
    fn hostile_inputs_never_panic_or_go_negative() {
        for avail in [0.0, -10.0, 5.0, f32::NAN, f32::INFINITY] {
            for natural in [&[][..], &[f32::NAN, 50.0][..], &[1e9, -5.0, 30.0][..]] {
                for sel in [0, 1, 99] {
                    let f = fit(natural, sel, avail, 40.0, 18.0);
                    assert!(f.shown.iter().all(|(_, w)| w.is_finite() && *w >= 0.0), "{avail} {natural:?} {f:?}");
                    assert_eq!(f.shown.len() + f.overflow.len(), natural.len());
                }
            }
        }
    }
}

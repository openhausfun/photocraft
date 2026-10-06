//! ⌘K command palette: fuzzy search over every menu command and tool.

use egui::{Align2, Color32, CornerRadius, RichText, Sense, Stroke, vec2};

use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons};

/// Case-insensitive subsequence match score (higher is better); None = no match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut last = None;
    for qc in query.to_lowercase().chars() {
        let mut found = false;
        while ti < t.len() {
            if t[ti] == qc {
                score += if last == Some(ti.wrapping_sub(1)) { 8 } else { 1 };
                if ti == 0 || t[ti - 1] == ' ' {
                    score += 5;
                }
                last = Some(ti);
                ti += 1;
                found = true;
                break;
            }
            ti += 1;
        }
        if !found {
            return None;
        }
    }
    Some(score - (t.len() as i32 / 8))
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.palette_open {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    // Scrim.
    egui::Area::new(egui::Id::new("palette-scrim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let r = ui.allocate_exact_size(screen.size(), Sense::click()).1;
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(110));
        if r.clicked() {
            app.ui.palette_open = false;
        }
    });
    let width = 560.0;
    let mut run: Option<String> = None;
    egui::Area::new(egui::Id::new("palette"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(egui::pos2(screen.center().x, screen.top() + 110.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.card_border))
                .corner_radius(CornerRadius::same(t.radius_lg as u8))
                .shadow(egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: t.shadow })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(width);
                    let qid = egui::Id::new("palette-query");
                    let mut q: String = ui.data_mut(|d| d.get_temp(qid).unwrap_or_default());
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                        icons::paint(ui, r, "search", 16.0, t.text_dim);
                        let te = egui::TextEdit::singleline(&mut q)
                            .hint_text(tl!("Search commands, tools and panels…"))
                            .frame(egui::Frame::NONE)
                            .font(egui::FontId::proportional(15.0))
                            .desired_width(width - 40.0);
                        let resp = ui.add(te);
                        resp.request_focus();
                    });
                    ui.add_space(6.0);
                    crate::widgets::hairline(ui);
                    ui.add_space(6.0);
                    let lang = crate::i18n::Lang::from_pref(&app.session.prefs().interface.language);
                    let mut hits: Vec<(i32, String, String, Option<String>, bool)> = crate::menus::menu_items(app)
                        .into_iter()
                        .filter_map(|m| {
                            let path_en = m.path.join(" › ");
                            let path = m.path.iter().map(|p| crate::i18n::tr(lang, p)).collect::<Vec<_>>().join(" › ");
                            let label = crate::i18n::tr_id(lang, &m.id, &m.label);
                            // Match what is shown and the English name (commands are documented in English).
                            let s = fuzzy_score(&q, &format!("{label} {path} {} {path_en}", m.label))?;
                            Some((
                                s,
                                m.id,
                                label.trim_end_matches('…').to_string(),
                                Some(path + &m.shortcut.map(|s| format!("   {}", crate::shortcuts::pretty(&s))).unwrap_or_default()),
                                m.enabled,
                            ))
                        })
                        .collect();
                    for tool in crate::state::Tool::ALL {
                        if let Some(s) = fuzzy_score(&q, &format!("{} {}", tl!(tool.label()), tool.label())) {
                            hits.push((s + 2, format!("tool:{tool:?}"), tl!(tool.label()).into(), Some(format!("Tool   {}", tool.key())), true));
                        }
                    }
                    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
                    hits.truncate(12);
                    let sel_id = egui::Id::new("palette-sel");
                    let mut sel: usize = ui.data_mut(|d| d.get_temp(sel_id).unwrap_or(0));
                    let (down, up, enter, esc) = ui.input(|i| {
                        (
                            i.key_pressed(egui::Key::ArrowDown),
                            i.key_pressed(egui::Key::ArrowUp),
                            i.key_pressed(egui::Key::Enter),
                            i.key_pressed(egui::Key::Escape),
                        )
                    });
                    if down {
                        sel = (sel + 1).min(hits.len().saturating_sub(1));
                    }
                    if up {
                        sel = sel.saturating_sub(1);
                    }
                    sel = sel.min(hits.len().saturating_sub(1));
                    if hits.is_empty() {
                        ui.label(RichText::new(tl!("No matching commands")).color(t.text_faint));
                    }
                    for (i, (_, id, label, detail, enabled)) in hits.iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
                        if i == sel || resp.hovered() {
                            ui.painter().rect_filled(rect, t.radius_sm, if i == sel { t.accent_soft } else { t.hover });
                        }
                        let color = if *enabled { t.text } else { t.text_faint };
                        ui.painter().text(rect.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, label, theme::medium(13.0), color);
                        if let Some(d) = detail {
                            ui.painter().text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, d, egui::FontId::proportional(11.5), t.text_faint);
                        }
                        if resp.clicked() && *enabled {
                            run = Some(id.clone());
                        }
                    }
                    if enter && let Some(h) = hits.get(sel).filter(|h| h.4) {
                        run = Some(h.1.clone());
                    }
                    if esc {
                        app.ui.palette_open = false;
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(crate::i18n::fmt(tl!("↑↓ navigate   {key} run   esc close"), &[("key", &crate::shortcuts::pretty("Enter"))]))
                            .small()
                            .color(t.text_faint),
                    );
                    ui.data_mut(|d| {
                        d.insert_temp(qid, q);
                        d.insert_temp(sel_id, sel);
                    });
                });
        });
    if let Some(id) = run {
        app.ui.palette_open = false;
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("palette-query"), String::new()));
        if let Some(tool) = id.strip_prefix("tool:") {
            if let Some(t) = crate::state::Tool::from_name(tool) {
                app.ui.tool = t;
            }
        } else if let Err(e) = crate::menus::invoke(app, ctx, &id, serde_json::json!({})) {
            app.ui.status = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_ranks_prefix_and_contiguous_higher() {
        assert!(fuzzy_score("hue", "Hue/Saturation…").unwrap() > fuzzy_score("hue", "Channel Mixer Hue").unwrap_or(-99));
        assert!(fuzzy_score("gblur", "Gaussian Blur").is_some());
        assert!(fuzzy_score("xyz", "Gaussian Blur").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }
}

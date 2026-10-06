//! Non-blocking notices: a small stack of cards in the lower-right corner of the window for things
//! the user should see but that must not interrupt work (import/export warnings such as "adjustment
//! flattened", files that couldn't be opened). Notices live in [`UiState`](crate::UiState) so the
//! control channel can read them (`ui.inspect`) and clear them (`ui.set`).

use serde::{Deserialize, Serialize};

use crate::PhotocraftApp;

/// At most this many notices are kept; older ones drop off.
pub const MAX_NOTICES: usize = 3;
/// Lines shown per notice before "…and N more".
const MAX_LINES: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub lines: Vec<String>,
    /// An error (warning colour) rather than an informational notice.
    #[serde(default)]
    pub error: bool,
}

/// Show a notice (newest last).
pub fn post(app: &mut PhotocraftApp, title: impl Into<String>, lines: Vec<String>, error: bool) {
    let id = app.ui.alloc_id();
    app.ui.notices.push(Notice { id, title: title.into(), lines, error });
    let n = app.ui.notices.len();
    if n > MAX_NOTICES {
        app.ui.notices.drain(..n - MAX_NOTICES);
    }
}

/// Report import/export `warnings` for the file operation `what` (e.g. "Opened a.psd"): the status
/// bar says how many there were, and a notice lists them. Nothing happens when there are none.
pub fn io_warnings(app: &mut PhotocraftApp, what: &str, warnings: &[String]) {
    let Some(first) = warnings.first() else { return };
    let n = warnings.len();
    app.ui.status = if n == 1 { format!("{what}: {first}") } else { format!("{what} with {n} warnings: {first} …") };
    app.ui.status_error = true;
    post(app, format!("{what} with {n} warning{}", if n == 1 { "" } else { "s" }), warnings.to_vec(), false);
}

/// Report a failed file operation: the status bar shows it as an error and a notice keeps it on
/// screen until dismissed.
pub fn error(app: &mut PhotocraftApp, message: String) {
    app.ui.status = message.clone();
    app.ui.status_error = true;
    post(app, message, Vec::new(), true);
}

/// Draw the notices; each has a close button.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.notices.is_empty() {
        return;
    }
    let t = crate::theme::Tokens::get(ctx);
    let mut dismiss = None;
    // Clear the status bar (~24 px) and leave the dock's edge some air.
    egui::Area::new(egui::Id::new("photocraft-notices"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -36.0))
        .interactable(true)
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            for n in &app.ui.notices {
                egui::Frame::NONE
                    .fill(t.card)
                    .stroke(egui::Stroke::new(1.0, if n.error { t.warning } else { t.card_border }))
                    .corner_radius(t.radius)
                    .inner_margin(egui::Margin::same(10))
                    .shadow(egui::Shadow { offset: [0, 2], blur: 8, spread: 0, color: t.shadow })
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.horizontal(|ui| {
                            let title = egui::RichText::new(&n.title).strong().color(if n.error { t.warning } else { t.text });
                            ui.add(egui::Label::new(title).wrap());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                if ui.add(egui::Button::new(egui::RichText::new("×").color(t.text_dim)).frame(false)).on_hover_text(tl!("Dismiss")).clicked() {
                                    dismiss = Some(n.id);
                                }
                            });
                        });
                        for line in n.lines.iter().take(MAX_LINES) {
                            ui.add(egui::Label::new(egui::RichText::new(format!("• {line}")).color(t.text_dim)).wrap());
                        }
                        if n.lines.len() > MAX_LINES {
                            ui.label(egui::RichText::new(format!("…and {} more", n.lines.len() - MAX_LINES)).color(t.text_faint));
                        }
                    });
                ui.add_space(6.0);
            }
        });
    if let Some(id) = dismiss {
        app.ui.notices.retain(|n| n.id != id);
    }
}

//! Edit › Fill… (Shift+F5, Shift+Backspace): Photoshop's Fill dialog in front of `edit.fill`.
//!
//! Contents (Foreground, Background, Color…, Content-Aware, Pattern, History, Black, 50% Gray,
//! White) with the content's own options, then Blending: Mode, Opacity and Preserve
//! Transparency. The fields are the command's params, so `ui.dialog.set` / `ui.dialog.confirm`
//! drive it like any dialog. OK remembers the choices in the preferences (`dialogs["edit.fill"]`),
//! which are saved to disk, so the dialog opens with them again after a restart.

use egui::{Align2, Sense, pos2, vec2};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

pub const COMMAND: &str = "edit.fill";
const MARK: &str = "__fill";

/// The Contents menu: (param, label), in Photoshop's order.
pub const CONTENTS: [(&str, &str); 9] = [
    ("foreground", "Foreground Color"),
    ("background", "Background Color"),
    ("color", "Color…"),
    ("contentAware", "Content-Aware"),
    ("pattern", "Pattern"),
    ("history", "History"),
    ("black", "Black"),
    ("gray", "50% Gray"),
    ("white", "White"),
];

/// Params the dialog edits and remembers.
const KEYS: [&str; 7] = ["contents", "color", "pattern", "colorAdaptation", "mode", "opacity", "preserveTransparency"];

/// Is this dialog the Fill dialog?
pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// A remembered value, if it is one the dialog can show (a corrupt preference is ignored).
fn valid(app: &PhotocraftApp, key: &str, v: &Value) -> bool {
    match key {
        "contents" => v.as_str().is_some_and(|c| CONTENTS.iter().any(|(k, _)| *k == c)),
        "mode" => v.as_str().and_then(photocraft_engine::commands::blend_from_str).is_some_and(|m| m != photocraft_color::BlendMode::PassThrough),
        "opacity" => v.as_f64().is_some_and(|o| (0.0..=100.0).contains(&o)),
        "color" => v.as_str().is_some_and(|h| h.len() == 7 && h.starts_with('#') && h.get(1..).is_some_and(|d| d.chars().all(|c| c.is_ascii_hexdigit()))),
        "pattern" => v.as_str().is_some_and(|p| app.session.patterns.items.iter().any(|q| q.id == p)),
        "colorAdaptation" | "preserveTransparency" => v.is_boolean(),
        _ => false,
    }
}

/// The dialog's fields: Photoshop's defaults, overridden by the last choices.
pub fn fields(app: &PhotocraftApp) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(COMMAND));
    f.insert("__label".into(), json!("Fill"));
    f.insert("contents".into(), json!("foreground"));
    f.insert("color".into(), json!(hex(app.session.tools.foreground)));
    if let Some(p) = app.session.patterns.items.first() {
        f.insert("pattern".into(), json!(p.id));
    }
    f.insert("colorAdaptation".into(), json!(true));
    f.insert("mode".into(), json!("normal"));
    f.insert("opacity".into(), json!(100.0));
    f.insert("preserveTransparency".into(), json!(false));
    if let Some(Value::Object(saved)) = app.session.prefs().dialogs.get(COMMAND) {
        for k in KEYS {
            if let Some(v) = saved.get(k).filter(|v| valid(app, k, v)) {
                f.insert(k.into(), v.clone());
            }
        }
    }
    f
}

/// Open the Fill dialog; returns its id.
pub fn open(app: &mut PhotocraftApp) -> u64 {
    let f = fields(app);
    app.ui.open_dialog(crate::state::DialogKind::Command, f)
}

/// The `edit.fill` params of the dialog's fields: only the options of the chosen contents.
pub fn params(f: &Map<String, Value>) -> Value {
    let contents = f.get("contents").and_then(Value::as_str).unwrap_or("foreground");
    let mut p = json!({ "contents": contents });
    for k in ["mode", "opacity", "preserveTransparency"] {
        if let Some(v) = f.get(k) {
            p[k] = v.clone();
        }
    }
    let extra = match contents {
        "color" => Some("color"),
        "pattern" => Some("pattern"),
        "contentAware" => Some("colorAdaptation"),
        _ => None,
    };
    if let Some(k) = extra
        && let Some(v) = f.get(k)
    {
        p[k] = v.clone();
    }
    p
}

/// OK: remember the choices, then fill.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let remembered: Map<String, Value> = KEYS.iter().filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
    app.session.prefs.edit(|p| p.dialogs.insert(COMMAND.into(), Value::Object(remembered)));
    app.run(COMMAND, params(f))
}

fn label(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(112.0, 22.0), Sense::hover());
    ui.painter().text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), t.text_dim);
}

fn get_str(f: &Map<String, Value>, k: &str, d: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}

fn parse_hex(h: &str) -> [u8; 3] {
    let d = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0);
    [d(1), d(3), d(5)]
}

/// The dialog body.
pub fn body(app: &PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    let mut contents = get_str(f, "contents", "foreground");
    ui.horizontal(|ui| {
        label(ui, "Contents:");
        let opts: Vec<(String, &str)> = CONTENTS.iter().map(|(k, l)| (k.to_string(), *l)).collect();
        if crate::widgets::dropdown(ui, "fill-contents", &mut contents, &opts, 170.0) {
            f.insert("contents".into(), json!(contents));
        }
    });
    match contents.as_str() {
        "color" => {
            ui.horizontal(|ui| {
                label(ui, "Color:");
                let mut rgb = parse_hex(&get_str(f, "color", "#000000"));
                if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                    f.insert("color".into(), json!(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])));
                }
            });
        }
        "pattern" => {
            ui.horizontal(|ui| {
                label(ui, "Custom Pattern:");
                let mut pat = get_str(f, "pattern", "");
                let opts: Vec<(String, &str)> = app.session.patterns.items.iter().map(|p| (p.id.clone(), p.display_name())).collect();
                if opts.is_empty() {
                    ui.label(egui::RichText::new("No patterns").color(Tokens::get(ui.ctx()).text_faint));
                } else if crate::widgets::dropdown(ui, "fill-pattern", &mut pat, &opts, 170.0) {
                    f.insert("pattern".into(), json!(pat));
                }
            });
        }
        "contentAware" => {
            ui.horizontal(|ui| {
                label(ui, "");
                let mut on = f.get("colorAdaptation").and_then(Value::as_bool).unwrap_or(true);
                if crate::widgets::checkbox(ui, &mut on, "Color Adaptation").changed() {
                    f.insert("colorAdaptation".into(), json!(on));
                }
            });
        }
        _ => {}
    }
    ui.add_space(4.0);
    crate::widgets::section_label(ui, "Blending");
    crate::widgets::hairline(ui);
    ui.horizontal(|ui| {
        label(ui, "Mode:");
        let mut mode = get_str(f, "mode", "normal");
        let opts: Vec<(String, &str)> = photocraft_color::BlendMode::LAYER_MODES.iter().map(|m| (mode_key(*m), m.label())).collect();
        if crate::widgets::dropdown(ui, "fill-mode", &mut mode, &opts, 170.0) {
            f.insert("mode".into(), json!(mode));
        }
    });
    ui.horizontal(|ui| {
        label(ui, "Opacity:");
        let mut o = f.get("opacity").and_then(Value::as_f64).unwrap_or(100.0) as f32;
        if crate::widgets::value_field(ui, &mut o, 0.0..=100.0, "%", 70.0).changed() {
            f.insert("opacity".into(), json!(o.round()));
        }
    });
    ui.horizontal(|ui| {
        label(ui, "");
        let mut on = f.get("preserveTransparency").and_then(Value::as_bool).unwrap_or(false);
        if crate::widgets::checkbox(ui, &mut on, "Preserve Transparency").changed() {
            f.insert("preserveTransparency".into(), json!(on));
        }
    });
}

/// The `mode` param naming a blend mode (`"normal"`, `"colorBurn"`…), which the engine parses.
fn mode_key(m: photocraft_color::BlendMode) -> String {
    let s = format!("{m:?}");
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_lowercase().to_string() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
#[path = "fill_ui_tests.rs"]
mod tests;

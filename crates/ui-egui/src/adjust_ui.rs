//! Properties-panel editors for the Selective Color and Color Lookup adjustment layers.
//!
//! Both commit through `layer.setAdjustment` (which merges these kinds' params into the current
//! values). Selective Color previews live while a slider drags, via `app.live_adjust` carrying
//! the full parameter set; Color Lookup commits on every change (its table isn't in the params).

use photocraft_doc::{Adjustment, LayerId};
use photocraft_engine::adjust_cmds::RANGES;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::widgets;

const RANGE_LABELS: [&str; 9] = ["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"];

/// The full parameter set of a Selective Color adjustment (every range as `[c, m, y, k]`).
pub fn selective_values(adj: &Adjustment) -> Value {
    let Adjustment::SelectiveColor { relative, adjustments } = adj else {
        return json!({});
    };
    let mut v = json!({"method": if *relative { "relative" } else { "absolute" }});
    for (i, key) in RANGES.iter().enumerate() {
        v[*key] = json!(adjustments[i]);
    }
    v
}

pub fn selective_color_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId, adj: &Adjustment) {
    let t = Tokens::get(ui.ctx());
    let committed = selective_values(adj);
    let mut values = match &app.live_adjust {
        Some((l, v)) if *l == id && v.get("reds").is_some() => v.clone(),
        _ => committed,
    };
    let key = egui::Id::new(("selc-range", id.0));
    let mut range: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Colors")).color(t.text_dim));
        let opts: Vec<(usize, &str)> = RANGE_LABELS.iter().copied().enumerate().collect();
        if widgets::dropdown(ui, &format!("selc-colors-{}", id.0), &mut range, &opts, 150.0) {
            ui.data_mut(|d| d.insert_temp(key, range));
        }
    });
    ui.add_space(4.0);
    let mut live = false;
    let mut commit = false;
    for (k, label) in [tl!("Cyan"), tl!("Magenta"), tl!("Yellow"), tl!("Black")].iter().enumerate() {
        let mut v = values[RANGES[range]][k].as_f64().unwrap_or(0.0) as f32;
        let r = widgets::slider_row(ui, label, &mut v, -100.0..=100.0, "%", None);
        if r.changed() {
            values[RANGES[range]][k] = json!(v.round());
            live = true;
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            commit = true;
        }
    }
    ui.add_space(4.0);
    let mut method = values["method"].as_str().unwrap_or("relative").to_string();
    ui.horizontal(|ui| {
        for (m, label) in [("relative", tl!("Relative")), ("absolute", tl!("Absolute"))] {
            if ui.radio(method == m, label).clicked() && method != m {
                method = m.to_string();
                values["method"] = json!(m);
                commit = true;
            }
        }
    });
    if live {
        app.live_adjust = Some((id, values.clone()));
    }
    if commit {
        let mut p = values;
        p["layer"] = json!(id.0);
        let _ = app.run("layer.setAdjustment", p);
        app.live_adjust = None;
    }
}

pub fn color_lookup_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId, adj: &Adjustment) {
    let t = Tokens::get(ui.ctx());
    let Adjustment::ColorLookup { name, lut, size, tetrahedral, dither } = adj else {
        return;
    };
    let builtins = photocraft_engine::adjust_cmds::LOOKS;
    let current =
        if lut.is_none() { "none".to_string() } else { builtins.iter().find(|b| b.1 == name).map_or_else(|| "custom".to_string(), |b| b.0.to_string()) };
    let custom_label = format!("{name} ({size}³)");
    let mut opts: Vec<(String, &str)> = vec![("none".into(), tl!("Load 3D LUT…"))];
    opts.extend(builtins.iter().map(|(id, label)| (id.to_string(), *label)));
    if current == "custom" {
        opts.push(("custom".into(), custom_label.as_str()));
    }
    let mut sel = current.clone();
    let mut params: Option<Value> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("3D LUT File")).color(t.text_dim));
        if widgets::dropdown(ui, &format!("clrl-{}", id.0), &mut sel, &opts, 170.0) && sel != current && sel != "custom" {
            params = Some(json!({"lut": sel}));
        }
    });
    // A path field stands in for the platform file picker (the desktop app's File menu has one).
    let path_key = egui::Id::new(("clrl-path", id.0));
    let mut path: String = ui.data(|d| d.get_temp(path_key)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut path).hint_text(".cube / .3dl / .look path").desired_width(170.0));
        if r.changed() {
            ui.data_mut(|d| d.insert_temp(path_key, path.clone()));
        }
        if widgets::secondary_button(ui, tl!("Load"), 60.0).clicked() && !path.trim().is_empty() {
            params = Some(json!({"file": path.trim()}));
        }
    });
    ui.add_space(4.0);
    let mut tet = *tetrahedral;
    ui.horizontal(|ui| {
        for (on, label) in [(false, tl!("Trilinear")), (true, tl!("Tetrahedral"))] {
            if ui.radio(tet == on, label).clicked() && tet != on {
                tet = on;
                params = Some(json!({"tetrahedral": on}));
            }
        }
    });
    let mut d = *dither;
    if widgets::toggle(ui, &mut d, tl!("Dither")).changed() {
        params = Some(json!({"dither": d}));
    }
    if let Some(mut p) = params {
        p["layer"] = json!(id.0);
        // Errors (an unreadable LUT file) land in the status bar.
        let _ = app.run("layer.setAdjustment", p);
    }
}

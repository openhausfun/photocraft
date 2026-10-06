//! Modal Image › Adjustments dialogs (Levels, Curves, Hue/Saturation, …) built from the same
//! editors as the adjustment-layer Properties panel (`adjust_editors`).
//!
//! The dialog's fields are the command's complete parameter set (so automation can read and set
//! them with `ui.dialog.set`) plus private `__` keys. While it is open the canvas previews the
//! settings as a temporary adjustment layer clipped to the target (`adjust_preview`, composited on
//! the GPU); where that can't match the command, the *real command* runs on the proxy document
//! through the filter-preview machinery (`__filter` + `__preview`). OK runs the command once (one
//! history step) and Cancel drops the preview, leaving the document untouched.

use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::adjust_editors::{self, EditorCx};
use crate::tone::{self, HistSource};

const PREFIX: &str = "image.adjustments.";

/// Whether `command` opens an adjustment dialog (when run without params).
pub fn has_dialog(command: &str) -> bool {
    command.strip_prefix(PREFIX).is_some_and(adjust_editors::has_editor)
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key("__adjust")
}

/// Opens the dialog for `command` with the kind's neutral settings.
pub fn open(app: &mut PhotocraftApp, command: &str) -> Option<u64> {
    let kind = command.strip_prefix(PREFIX).filter(|k| adjust_editors::has_editor(k))?;
    let spec = photocraft_engine::commands::find(command)?;
    let mode = app.session.active().map_or(photocraft_doc::ColorMode::Rgb, |s| s.doc.mode);
    let defaults = photocraft_engine::adjust_params::default_for(kind, mode).ok()?;
    let mut fields = Map::new();
    fields.insert("__command".into(), json!(command));
    fields.insert("__label".into(), json!(spec.label));
    fields.insert("__adjust".into(), json!(kind));
    fields.insert("__filter".into(), json!(true));
    fields.insert("__preview".into(), json!(true));
    if let Value::Object(p) = photocraft_engine::adjust_params::to_params(&defaults) {
        fields.extend(p);
    }
    Some(app.ui.open_dialog(crate::state::DialogKind::Command, fields))
}

/// Dialog body: the kind's editor over the fields, then the Preview checkbox.
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) {
    let kind = fields.get("__adjust").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut values = crate::filter_dialog::params_of(fields);
    let active = app.session.active().and_then(|s| s.active_layer);
    let gray = app.session.active().is_some_and(|s| adjust_editors::is_gray(s.doc.mode));
    let hist = match active {
        Some(id) if adjust_editors::needs_histogram(&kind) => Some(tone::histograms(app, HistSource::Layer(id), adjust_editors::space_of(&values))),
        _ => None,
    };
    let cx = EditorCx { mem: egui::Id::new(("adjust-dialog", kind.as_str())), hist, gray, swatches: adjust_editors::swatches(app) };
    let e = adjust_editors::editor(ui, &kind, &mut values, &cx);
    if e.changed
        && let Value::Object(v) = values
    {
        // Replace the parameter keys (an editor may remove one), keep the private ones.
        fields.retain(|k, _| k.starts_with("__"));
        fields.extend(v);
    }
    let mut preview = fields.get("__preview").and_then(Value::as_bool).unwrap_or(true);
    ui.add_space(6.0);
    if crate::widgets::checkbox(ui, &mut preview, tl!("Preview")).changed() {
        fields.insert("__preview".into(), json!(preview));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    fn app_with_image() -> PhotocraftApp {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 32, "height": 48})).unwrap();
        s.execute("edit.fill", json!({"color": "#b04020"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        PhotocraftApp::new(s, crate::Services::default())
    }

    #[test]
    fn every_adjustment_with_an_editor_has_a_dialog() {
        for kind in adjust_editors::KINDS {
            assert!(has_dialog(&format!("image.adjustments.{kind}")), "{kind}");
        }
        for id in ["image.adjustments.invert", "image.adjustments.selectiveColor", "image.adjustments.desaturate", "filter.blur.gaussianBlur"] {
            assert!(!has_dialog(id), "{id}");
        }
    }

    /// Opens each dialog, renders it, changes a value, then cancels (document untouched) or
    /// confirms (one history step).
    #[test]
    fn dialogs_preview_cancel_and_commit() {
        for kind in adjust_editors::KINDS {
            let cmd = format!("image.adjustments.{kind}");
            let mut harness =
                Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app_with_image());
            PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
            let id = open(harness.state_mut(), &cmd).unwrap();
            harness.run_steps(3);
            let before = harness.state().session.active().unwrap().revision;
            let steps = harness.state().session.active().unwrap().history.past_len();
            // Change the settings the way automation does.
            let sample = photocraft_engine::adjust_params::to_params(
                &photocraft_engine::adjust_params::from_params(kind, &sample_params(kind), None, photocraft_doc::ColorMode::Rgb).unwrap(),
            );
            let d = harness.state_mut().ui.dialog_mut(id).unwrap();
            if let Value::Object(p) = sample {
                d.fields.extend(p);
            }
            harness.run_steps(2);
            // The preview runs the real command on the proxy.
            let fields = harness.state().ui.dialogs[0].fields.clone();
            let doc = harness.state().session.active().unwrap().doc.clone();
            let prev = crate::filter_dialog::preview_document(
                &doc,
                harness.state().session.active().unwrap().active_layer,
                &cmd,
                &crate::filter_dialog::params_of(&fields),
                1,
            );
            assert!(prev.is_some(), "{kind}: preview");
            assert_eq!(harness.state().session.active().unwrap().revision, before, "{kind}: previewing leaves the document alone");
            let r = crate::dialogs::confirm(harness.state_mut(), id);
            assert!(r.is_ok(), "{kind}: {r:?}");
            assert_eq!(harness.state().session.active().unwrap().history.past_len(), steps + 1, "{kind}: one history step");
        }
    }

    fn sample_params(kind: &str) -> Value {
        match kind {
            "brightnessContrast" => json!({"brightness": 40}),
            "levels" => json!({"inBlack": 30, "gamma": 1.3}),
            "curves" => json!({"points": [[0, 0], [100, 150], [255, 255]]}),
            "exposure" => json!({"exposure": 1}),
            "vibrance" => json!({"vibrance": 50}),
            "hueSaturation" => json!({"reds": {"hue": 40}}),
            "colorBalance" => json!({"midtones": [40, 0, -20]}),
            "blackWhite" => json!({"reds": 150, "tint": true}),
            "photoFilter" => json!({"filter": "cooling80", "density": 60}),
            "channelMixer" => json!({"red": [50, 50, 0, 0]}),
            "posterize" => json!({"levels": 3}),
            "threshold" => json!({"level": 90}),
            "gradientMap" => json!({"stops": [[0, "#200040"], [1, "#ffd080"]]}),
            _ => json!({}),
        }
    }

    #[test]
    fn cancel_leaves_the_document_untouched() {
        let mut harness =
            Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app_with_image());
        PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        let id = open(harness.state_mut(), "image.adjustments.curves").unwrap();
        harness.run_steps(2);
        harness.state_mut().ui.dialog_mut(id).unwrap().fields.insert("points".into(), json!([[0, 255], [255, 0]]));
        harness.run_steps(2);
        let before = harness.state().session.active().unwrap().doc.clone();
        harness.state_mut().ui.close_dialog(id);
        harness.run_steps(2);
        let after = harness.state().session.active().unwrap().doc.clone();
        assert!(std::sync::Arc::ptr_eq(&before, &after));
        assert!(harness.state().ui.dialogs.is_empty());
    }
}

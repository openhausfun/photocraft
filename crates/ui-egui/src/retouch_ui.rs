//! UI for the retouching tools (healing, clone, history brush, blur/sharpen/smudge, dodge/burn/
//! sponge) and the smart selection tools (Quick Selection, Object Selection): gesture → engine
//! command, options bars, and the clone-source marker.

use egui::{Color32, Stroke, vec2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;
use crate::theme::Tokens;

/// Finish a stroke with a retouching tool. Returns false if `tool` isn't one.
pub fn finish_stroke(app: &mut PhotocraftApp, tool: Tool, points: &[[f64; 3]], mods: egui::Modifiers) -> bool {
    let o = app.ui.tool_options.clone();
    let pts = json!(points);
    let (cmd, mut p): (&str, Value) = match tool {
        Tool::SpotHealing => ("paint.spotHealing", json!({"type": o.spot_type})),
        Tool::Healing | Tool::CloneStamp => {
            let mut p = json!({"aligned": o.clone_aligned, "sampleLayer": o.clone_sample});
            // The Clone Source panel's active slot (set by ⌥-click) drives the stroke: the engine
            // keeps the aligned pairing and applies the slot's scale/rotation/flip.
            let slot = app.session.presets.clone.active().source.is_some();
            match (app.ui.clone_offset.filter(|_| o.clone_aligned && !slot), app.ui.clone_source.filter(|_| !slot)) {
                _ if slot => {}
                (Some(off), _) => p["offset"] = json!(off),
                (None, Some(src)) => p["source"] = json!(src),
                (None, None) => {
                    // Photoshop says Option-click on the Mac and Alt-click on Windows.
                    app.ui.status = if cfg!(target_os = "macos") {
                        tl!("Option-click to define a source point to clone from")
                    } else {
                        tl!("Alt-click to define a source point to clone from")
                    }
                    .into();
                    app.ui.status_error = true;
                    return true;
                }
            }
            (if tool == Tool::Healing { "paint.healingBrush" } else { "paint.cloneStamp" }, p)
        }
        Tool::HistoryBrush => ("paint.historyBrush", json!({})),
        Tool::Blur => ("paint.blur", json!({"strength": o.strength, "sampleAllLayers": o.sample_all_layers})),
        Tool::Sharpen => ("paint.sharpen", json!({"strength": o.strength, "protectDetail": o.protect_detail, "sampleAllLayers": o.sample_all_layers})),
        Tool::Smudge => ("paint.smudge", json!({"strength": o.strength, "fingerPainting": o.finger_painting, "sampleAllLayers": o.sample_all_layers})),
        Tool::Dodge | Tool::Burn => (
            if tool == Tool::Dodge { "paint.dodge" } else { "paint.burn" },
            json!({"range": o.tone_range, "exposure": o.exposure, "protectTones": o.protect_tones}),
        ),
        Tool::Sponge => ("paint.sponge", json!({"mode": o.sponge_mode, "vibrance": o.vibrance})),
        Tool::QuickSelection => {
            let size = app.session.tools.brush.size;
            let mode = if mods.alt { "subtract" } else { "add" };
            let xy: Vec<[f64; 2]> = points.iter().map(|q| [q[0], q[1]]).collect();
            let _ = app
                .run("select.quick", json!({"points": xy, "size": size, "mode": mode, "enhanceEdge": o.enhance_edge, "sampleAllLayers": o.sample_all_layers}));
            return true;
        }
        _ => return false,
    };
    p["points"] = pts;
    // The layer mask, an alpha channel or the Quick Mask when targeted, as the Brush paints.
    p["target"] = crate::canvas::paint_target(app);
    match app.run(cmd, p) {
        Ok(r) if matches!(tool, Tool::Healing | Tool::CloneStamp) => {
            // Aligned: keep the offset for later strokes; non-aligned: every stroke restarts at the source.
            app.ui.clone_offset = r.get("offset").and_then(|v| serde_json::from_value(v.clone()).ok()).filter(|_| o.clone_aligned);
        }
        Ok(_) => {}
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    true
}

/// Object Selection: the dragged rectangle.
pub fn finish_object_selection(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) {
    let (x, y) = (start[0].min(end[0]), start[1].min(end[1]));
    let (w, h) = ((end[0] - start[0]).abs(), (end[1] - start[1]).abs());
    if w < 2.0 || h < 2.0 {
        return;
    }
    let mode = if mods.alt {
        "subtract"
    } else if mods.shift {
        "add"
    } else {
        "replace"
    };
    let _ = app.run(
        "select.object",
        json!({"rect": [x.round(), y.round(), w.round(), h.round()], "mode": mode, "sampleAllLayers": app.ui.tool_options.sample_all_layers}),
    );
}

/// ⌥-click with Clone Stamp / Healing Brush sets the source.
pub fn set_source(app: &mut PhotocraftApp, x: f64, y: f64) {
    app.ui.clone_source = Some([x.round(), y.round()]);
    app.ui.clone_offset = None;
    let _ = app.run("cloneSource.set", json!({"source": [x.round(), y.round()]}));
    app.ui.status = format!("Clone source set at {:.0}, {:.0}", x, y);
    app.ui.status_error = false;
}

/// Crosshair where the clone source is sampled from for the current pointer position.
pub fn draw_source_marker(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if !matches!(app.ui.tool, Tool::CloneStamp | Tool::Healing) {
        return;
    }
    let at =
        app.drag.as_ref().filter(|d| matches!(d.tool, Tool::CloneStamp | Tool::Healing)).and_then(|d| d.points.last().map(|p| [p[0], p[1]])).or(app.hover_doc);
    let src = match (crate::preset_panels::clone_sample_point(app, at), app.ui.clone_offset, at, app.ui.clone_source) {
        (Some(p), ..) => Some(p),
        (None, Some(off), Some(h), _) => Some([h[0] + off[0], h[1] + off[1]]),
        (None, None, _, Some(s)) => Some(s),
        _ => None,
    };
    let Some(s) = src else { return };
    let c = xf.to_screen(s[0] as f32, s[1] as f32);
    for (w, col) in [(3.0, Color32::from_black_alpha(160)), (1.0, Color32::WHITE)] {
        painter.line_segment([c - vec2(7.0, 0.0), c + vec2(7.0, 0.0)], Stroke::new(w, col));
        painter.line_segment([c - vec2(0.0, 7.0), c + vec2(0.0, 7.0)], Stroke::new(w, col));
    }
}

fn opt(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!(&text)).color(t.text_dim).size(12.0));
}

fn pct(ui: &mut egui::Ui, label: &str, v: &mut f32) {
    opt(ui, label);
    crate::widgets::value_field(ui, v, 1.0..=100.0, "%", 58.0);
}

/// Options bar for the retouching and smart-selection tools. Returns false for other tools.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) -> bool {
    if !tool.is_brushlike() && !matches!(tool, Tool::QuickSelection | Tool::ObjectSelection) || matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser) {
        return false;
    }
    let o = &mut app.ui.tool_options;
    match tool {
        Tool::SpotHealing => {
            opt(ui, tl!("Type:"));
            for (k, l) in [("contentAware", tl!("Content-Aware")), ("createTexture", tl!("Create Texture")), ("proximityMatch", tl!("Proximity Match"))] {
                let mut on = o.spot_type == k;
                if crate::widgets::checkbox(ui, &mut on, l).clicked() {
                    o.spot_type = k.into();
                }
            }
            crate::widgets::vline(ui, 22.0);
            crate::widgets::checkbox(ui, &mut o.sample_all_layers, tl!("Sample All Layers"));
        }
        Tool::Healing | Tool::CloneStamp => {
            crate::widgets::checkbox(ui, &mut o.clone_aligned, tl!("Aligned"));
            opt(ui, tl!("Sample:"));
            let opts = [
                ("current".to_string(), tl!("Current Layer")),
                ("currentAndBelow".to_string(), tl!("Current & Below")),
                ("all".to_string(), tl!("All Layers")),
            ];
            crate::widgets::dropdown(ui, "clone-sample", &mut o.clone_sample, &opts, 130.0);
            if app.ui.clone_source.is_none() {
                crate::widgets::vline(ui, 22.0);
                opt(ui, &crate::i18n::fmt(tl!("{key}-click to set the source"), &[("key", &crate::shortcuts::pretty("Alt"))]));
            }
        }
        Tool::Dodge | Tool::Burn => {
            opt(ui, tl!("Range:"));
            let opts = [("shadows".to_string(), tl!("Shadows")), ("midtones".to_string(), tl!("Midtones")), ("highlights".to_string(), tl!("Highlights"))];
            crate::widgets::dropdown(ui, "tone-range", &mut o.tone_range, &opts, 100.0);
            pct(ui, tl!("Exposure:"), &mut o.exposure);
            crate::widgets::checkbox(ui, &mut o.protect_tones, tl!("Protect Tones"));
        }
        Tool::Sponge => {
            opt(ui, tl!("Mode:"));
            let opts = [("desaturate".to_string(), tl!("Desaturate")), ("saturate".to_string(), tl!("Saturate"))];
            crate::widgets::dropdown(ui, "sponge-mode", &mut o.sponge_mode, &opts, 110.0);
            crate::widgets::checkbox(ui, &mut o.vibrance, tl!("Vibrance"));
        }
        Tool::Blur | Tool::Sharpen | Tool::Smudge => {
            pct(ui, tl!("Strength:"), &mut o.strength);
            crate::widgets::checkbox(ui, &mut o.sample_all_layers, tl!("Sample All Layers"));
            if tool == Tool::Sharpen {
                crate::widgets::checkbox(ui, &mut o.protect_detail, tl!("Protect Detail"));
            }
            if tool == Tool::Smudge {
                crate::widgets::checkbox(ui, &mut o.finger_painting, tl!("Finger Painting"));
            }
        }
        Tool::HistoryBrush => opt(ui, "Paints from the document's opening state"),
        Tool::QuickSelection => {
            crate::widgets::checkbox(ui, &mut o.sample_all_layers, tl!("Sample All Layers"));
            crate::widgets::checkbox(ui, &mut o.enhance_edge, tl!("Enhance Edge"));
            opt(ui, &crate::i18n::fmt(tl!("{key} to subtract"), &[("key", &crate::shortcuts::pretty("Alt"))]));
            crate::widgets::vline(ui, 22.0);
            if crate::widgets::secondary_button(ui, tl!("Select Subject"), 0.0).clicked() {
                let _ = app.run("select.subject", json!({}));
            }
        }
        Tool::ObjectSelection => {
            crate::widgets::checkbox(ui, &mut o.sample_all_layers, tl!("Sample All Layers"));
            opt(ui, tl!("Drag a rectangle around the object"));
            crate::widgets::vline(ui, 22.0);
            if crate::widgets::secondary_button(ui, tl!("Select Subject"), 0.0).clicked() {
                let _ = app.run("select.subject", json!({}));
            }
        }
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{ToolEvent, tool_event};

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 100, "height": 60})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 16, "hardness": 1.0}})).unwrap();
        app
    }

    fn drag(app: &mut PhotocraftApp, tool: Tool) {
        app.ui.tool = tool;
        let m = egui::Modifiers::NONE;
        tool_event(app, ToolEvent::Down { x: 10.0, y: 30.0, pressure: 1.0 }, m);
        tool_event(app, ToolEvent::Move { x: 50.0, y: 30.0, pressure: 1.0 }, m);
        tool_event(app, ToolEvent::Up { x: 50.0, y: 30.0 }, m);
        assert!(!app.ui.status_error, "{tool:?}: {}", app.ui.status);
    }

    fn active(app: &PhotocraftApp) -> &photocraft_doc::Layer {
        let st = app.session.active().unwrap();
        st.active_layer.and_then(|id| st.doc.layer(id)).unwrap()
    }

    fn stripes(app: &mut PhotocraftApp, step: usize, target: &str) {
        for x in (0..100).step_by(step) {
            app.run("paint.pencil", json!({"points": [[x, 0], [x, 60]], "size": 2, "color": "#606060", "target": target})).unwrap();
        }
    }

    #[test]
    fn clone_without_a_source_names_the_platform_modifier() {
        // Option-click on the Mac, Alt-click elsewhere (#251).
        for tool in [Tool::CloneStamp, Tool::Healing] {
            let mut app = app();
            app.ui.tool = tool;
            assert!(finish_stroke(&mut app, tool, &[[10.0, 30.0, 1.0], [50.0, 30.0, 1.0]], egui::Modifiers::NONE));
            assert!(app.ui.status_error, "{tool:?}");
            let want = if cfg!(target_os = "macos") { "Option-click" } else { "Alt-click" };
            assert!(app.ui.status.starts_with(want), "{tool:?}: {}", app.ui.status);
        }
    }

    #[test]
    fn retouch_strokes_paint_the_targeted_mask() {
        // #207: with the mask targeted, retouching changes the mask, never the pixels.
        for tool in [Tool::Blur, Tool::Sharpen, Tool::Smudge, Tool::Dodge, Tool::Burn] {
            let mut app = app();
            app.run("layer.new.layer", json!({})).unwrap();
            stripes(&mut app, 4, "pixels");
            app.run("layer.layerMask.revealAll", json!({})).unwrap();
            stripes(&mut app, 6, "mask");
            app.ui.mask_target = true;
            app.ui.tool_options.protect_detail = false;
            let b = app.session.active().unwrap().doc.bounds();
            let px0 = active(&app).surface().unwrap().read_region(b);
            let mask0 = active(&app).mask.as_ref().unwrap().surface.read_region(b);
            drag(&mut app, tool);
            assert_eq!(active(&app).surface().unwrap().read_region(b), px0, "{tool:?}: pixels untouched");
            assert_ne!(active(&app).mask.as_ref().unwrap().surface.read_region(b), mask0, "{tool:?}: mask changed");
        }
    }

    #[test]
    fn sample_all_layers_reaches_blur_sharpen_and_smudge() {
        // #207: the options-bar checkbox reaches the command.
        for tool in [Tool::Blur, Tool::Sharpen, Tool::Smudge] {
            for all in [false, true] {
                let mut app = app();
                stripes(&mut app, 4, "pixels");
                app.run("layer.new.layer", json!({})).unwrap();
                app.ui.tool_options.sample_all_layers = all;
                drag(&mut app, tool);
                let a = active(&app).surface().unwrap().rgba(30, 30)[3];
                assert_eq!(a > 0.0, all, "{tool:?} sampleAllLayers={all}: alpha {a}");
            }
        }
    }
}

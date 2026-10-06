//! File › New: Photoshop's New Document dialog. Category tabs with blank-document presets on the
//! left, Preset Details on the right. Values live in the dialog fields (`width`/`height` in pixels,
//! `resolution` in ppi, `mode`, `depth`, `background`, `name`), so `ui.dialog.set` drives it and
//! Create runs `file.new` with them.

use egui::{Align2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Map, Value, json};

use crate::theme::Tokens;
use crate::{icons, widgets};

/// A blank-document preset: (name, width px, height px, ppi).
pub type Preset = (&'static str, u32, u32, f32);

/// Photoshop's New Document categories and their blank-document presets.
pub const CATEGORIES: &[(&str, &[Preset])] = &[
    ("Recent", &[("Default Photoshop Size", 2100, 1500, 300.0), ("HDTV 1080p", 1920, 1080, 72.0)]),
    (
        "Photo",
        &[
            ("Landscape, 6 x 4", 1800, 1200, 300.0),
            ("Landscape, 7 x 5", 2100, 1500, 300.0),
            ("Landscape, 10 x 8", 3000, 2400, 300.0),
            ("Portrait, 4 x 6", 1200, 1800, 300.0),
            ("Portrait, 5 x 7", 1500, 2100, 300.0),
            ("Square, 5 x 5", 1500, 1500, 300.0),
        ],
    ),
    (
        "Print",
        &[
            ("Letter", 2550, 3300, 300.0),
            ("Legal", 2550, 4200, 300.0),
            ("Tabloid", 3300, 5100, 300.0),
            ("A4", 2480, 3508, 300.0),
            ("A3", 3508, 4961, 300.0),
            ("A5", 1748, 2480, 300.0),
        ],
    ),
    (
        "Art & Illustration",
        &[("Poster", 5400, 7200, 300.0), ("Postcard", 1800, 1200, 300.0), ("Comic Book", 1988, 3075, 300.0), ("Square, 12 x 12", 3600, 3600, 300.0)],
    ),
    (
        "Web",
        &[
            ("Web Most Common", 1366, 768, 72.0),
            ("Web Minimum", 1024, 768, 72.0),
            ("Web Large", 1920, 1080, 72.0),
            ("MacBook Pro 16\"", 3456, 2234, 72.0),
            ("iMac 24\"", 4480, 2520, 72.0),
        ],
    ),
    (
        "Mobile",
        &[
            ("iPhone 16", 1179, 2556, 72.0),
            ("iPhone 16 Pro Max", 1320, 2868, 72.0),
            ("iPad Pro 13\"", 2064, 2752, 72.0),
            ("Android 1080p", 1080, 1920, 72.0),
            ("Apple Watch 45mm", 396, 484, 72.0),
        ],
    ),
    (
        "Film & Video",
        &[
            ("HDTV 1080p", 1920, 1080, 72.0),
            ("HDTV 720p", 1280, 720, 72.0),
            ("UHD 4K", 3840, 2160, 72.0),
            ("DCI 4K", 4096, 2160, 72.0),
            ("UHD 8K", 7680, 4320, 72.0),
        ],
    ),
];

/// Width/Height units: (key, label, units per inch; 0 = pixels).
pub const UNITS: &[(&str, &str, f32)] =
    &[("px", "Pixels", 0.0), ("in", "Inches", 1.0), ("cm", "Centimeters", 2.54), ("mm", "Millimeters", 25.4), ("pt", "Points", 72.0), ("pica", "Picas", 6.0)];

/// Pixels to the display unit at `ppi`.
pub fn to_unit(px: f32, unit: &str, ppi: f32) -> f32 {
    match UNITS.iter().find(|u| u.0 == unit) {
        Some((_, _, per_in)) if *per_in > 0.0 => px / ppi.max(1.0) * per_in,
        _ => px,
    }
}

/// Display unit back to pixels at `ppi`.
pub fn from_unit(v: f32, unit: &str, ppi: f32) -> f32 {
    match UNITS.iter().find(|u| u.0 == unit) {
        Some((_, _, per_in)) if *per_in > 0.0 => (v / per_in * ppi.max(1.0)).round(),
        _ => v.round(),
    }
}

/// A typed size as the whole pixel count `file.new` takes (#254: a float like `512.0` isn't one, so
/// the command fell back to its 1920 x 1080 default). Clamped to the command's 1–300000 range.
pub fn px_value(px: f32) -> Value {
    json!(if px.is_finite() { px.round().clamp(1.0, 300_000.0) as u32 } else { 1 })
}

/// Apply a preset to the dialog fields.
pub fn apply_preset(f: &mut Map<String, Value>, p: &Preset) {
    f.insert("width".into(), json!(p.1));
    f.insert("height".into(), json!(p.2));
    f.insert("resolution".into(), json!(p.3));
    f.insert("__preset".into(), json!(p.0));
    // Print and photo presets are specified in inches, screen presets in pixels.
    f.insert("__unit".into(), json!(if p.3 >= 300.0 { "in" } else { "px" }));
}

/// Fields `file.new` takes (drops the dialog's `__` UI keys).
pub fn command_params(f: &Map<String, Value>) -> Value {
    Value::Object(f.iter().filter(|(k, _)| !k.starts_with("__")).map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn get_f(f: &Map<String, Value>, k: &str, d: f32) -> f32 {
    f.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}

fn get_s(f: &Map<String, Value>, k: &str, d: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}

fn small_label(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(s).size(11.5).color(t.text_dim));
}

/// Paint a page thumbnail with the preset's aspect ratio.
fn page_icon(ui: &egui::Ui, r: Rect, w: u32, h: u32, t: &Tokens) {
    let s = 30.0 / (w.max(h) as f32);
    let page = Rect::from_center_size(r.center(), vec2(w as f32 * s, h as f32 * s));
    ui.painter().rect_stroke(page, 1.0, Stroke::new(1.2, t.text_dim), StrokeKind::Inside);
}

pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let cat = get_s(f, "__category", "Recent");
    // Category tabs.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (name, _) in CATEGORIES {
            let on = cat == *name;
            let r = ui.add(egui::Label::new(RichText::new(tl!(name)).size(13.0).color(if on { t.text } else { t.text_dim })).sense(Sense::click()));
            if on {
                ui.painter().line_segment([r.rect.left_bottom() + vec2(0.0, 3.0), r.rect.right_bottom() + vec2(0.0, 3.0)], Stroke::new(2.0, t.text));
            }
            if r.clicked() {
                f.insert("__category".into(), json!(name));
            }
        }
    });
    ui.add_space(6.0);
    widgets::hairline(ui);
    ui.add_space(8.0);
    let presets = CATEGORIES.iter().find(|c| c.0 == cat).map_or(CATEGORIES[0].1, |c| c.1);
    let chosen = get_s(f, "__preset", "");
    ui.horizontal_top(|ui| {
        // Left: preset grid.
        ui.vertical(|ui| {
            ui.set_width(520.0);
            ui.label(RichText::new(crate::i18n::fmt(tl!("BLANK DOCUMENT PRESETS ({n})"), &[("n", &presets.len().to_string())])).size(11.0).color(t.text_faint));
            ui.add_space(6.0);
            let card = vec2(164.0, 112.0);
            for row in presets.chunks(3) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for p in row {
                        let (r, resp) = ui.allocate_exact_size(card, Sense::click());
                        let on = chosen == p.0;
                        ui.painter().rect_filled(
                            r,
                            t.radius,
                            if on {
                                t.row_selected
                            } else if resp.hovered() {
                                t.hover
                            } else {
                                t.field
                            },
                        );
                        if on {
                            ui.painter().rect_stroke(r, t.radius, Stroke::new(1.5, t.accent), StrokeKind::Inside);
                        }
                        page_icon(ui, Rect::from_center_size(pos2(r.center().x, r.top() + 34.0), vec2(40.0, 40.0)), p.1, p.2, &t);
                        ui.painter().text(pos2(r.center().x, r.top() + 72.0), Align2::CENTER_CENTER, tl!(p.0), egui::FontId::proportional(12.0), t.text);
                        let unit = if p.3 >= 300.0 { "in" } else { "px" };
                        let size = if unit == "in" {
                            format!(
                                "{} x {} in @ {} ppi",
                                widgets::fmt_num(to_unit(p.1 as f32, "in", p.3) as f64),
                                widgets::fmt_num(to_unit(p.2 as f32, "in", p.3) as f64),
                                p.3
                            )
                        } else {
                            format!("{} x {} px @ {} ppi", p.1, p.2, p.3)
                        };
                        ui.painter().text(pos2(r.center().x, r.top() + 90.0), Align2::CENTER_CENTER, size, egui::FontId::proportional(10.5), t.text_faint);
                        if resp.clicked() {
                            apply_preset(f, p);
                        }
                    }
                });
                ui.add_space(8.0);
            }
        });
        ui.add_space(10.0);
        // Right: Preset Details.
        ui.vertical(|ui| {
            ui.set_width(260.0);
            ui.label(RichText::new(tl!("PRESET DETAILS")).size(11.0).color(t.text_faint));
            ui.add_space(4.0);
            let mut name = get_s(f, "name", tl!("Untitled-1"));
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(250.0).font(egui::FontId::proportional(15.0))).changed() {
                f.insert("name".into(), json!(name));
            }
            ui.add_space(8.0);
            let ppi = get_f(f, "resolution", 72.0);
            let mut unit = get_s(f, "__unit", "px");
            small_label(ui, tl!("Width"));
            ui.horizontal(|ui| {
                let mut w = to_unit(get_f(f, "width", 1920.0), &unit, ppi);
                if widgets::value_field(ui, &mut w, 0.01..=300_000.0, "", 110.0).changed() {
                    f.insert("width".into(), px_value(from_unit(w, &unit, ppi)));
                    f.remove("__preset");
                }
                let opts: Vec<(String, &str)> = UNITS.iter().map(|u| (u.0.to_string(), u.1)).collect();
                if widgets::dropdown(ui, "nd-unit", &mut unit, &opts, 120.0) {
                    f.insert("__unit".into(), json!(unit));
                }
            });
            small_label(ui, tl!("Height"));
            ui.horizontal(|ui| {
                let mut h = to_unit(get_f(f, "height", 1080.0), &unit, ppi);
                if widgets::value_field(ui, &mut h, 0.01..=300_000.0, "", 110.0).changed() {
                    f.insert("height".into(), px_value(from_unit(h, &unit, ppi)));
                    f.remove("__preset");
                }
                ui.add_space(6.0);
                small_label(ui, tl!("Orientation"));
                let (w, h) = (get_f(f, "width", 1920.0), get_f(f, "height", 1080.0));
                for (icon, portrait) in [("rectangle-vertical", true), ("rectangle-horizontal", false)] {
                    if icons::button(ui, icon, 24.0, (h > w) == portrait, if portrait { "Portrait" } else { "Landscape" }).clicked() && (h > w) != portrait {
                        f.insert("width".into(), px_value(h));
                        f.insert("height".into(), px_value(w));
                    }
                }
            });
            ui.add_space(4.0);
            small_label(ui, tl!("Resolution"));
            ui.horizontal(|ui| {
                let per_cm = get_s(f, "__resUnit", "in") == "cm";
                let mut r = if per_cm { ppi / 2.54 } else { ppi };
                if widgets::value_field(ui, &mut r, 1.0..=30_000.0, "", 110.0).changed() {
                    f.insert("resolution".into(), json!(if per_cm { r * 2.54 } else { r }));
                }
                let mut ru = get_s(f, "__resUnit", "in");
                if widgets::dropdown(ui, "nd-resunit", &mut ru, &[("in".to_string(), tl!("Pixels/Inch")), ("cm".to_string(), tl!("Pixels/Centimeter"))], 120.0)
                {
                    f.insert("__resUnit".into(), json!(ru));
                }
            });
            ui.add_space(4.0);
            small_label(ui, tl!("Color Mode"));
            ui.horizontal(|ui| {
                let mut mode = get_s(f, "mode", "rgb");
                if widgets::dropdown(
                    ui,
                    "nd-mode",
                    &mut mode,
                    &[
                        ("gray".to_string(), tl!("Grayscale")),
                        ("rgb".to_string(), tl!("RGB Color")),
                        ("cmyk".to_string(), tl!("CMYK Color")),
                        ("lab".to_string(), tl!("Lab Color")),
                    ],
                    110.0,
                ) {
                    f.insert("mode".into(), json!(mode));
                }
                let mut depth = f.get("depth").and_then(Value::as_u64).unwrap_or(8);
                if widgets::dropdown(ui, "nd-depth", &mut depth, &[(8u64, "8 bit"), (16, "16 bit"), (32, "32 bit")], 120.0) {
                    f.insert("depth".into(), json!(depth));
                }
            });
            ui.add_space(4.0);
            small_label(ui, tl!("Background Contents"));
            let mut bg = get_s(f, "background", "white");
            let opts = [
                ("white".to_string(), tl!("White")),
                ("black".to_string(), tl!("Black")),
                ("backgroundColor".to_string(), tl!("Background Color")),
                ("transparent".to_string(), tl!("Transparent")),
            ];
            if widgets::dropdown(ui, "nd-bg", &mut bg, &opts, 240.0) {
                f.insert("background".into(), json!(bg));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_round_trip_through_pixels() {
        assert_eq!(to_unit(2100.0, "in", 300.0), 7.0);
        assert_eq!(from_unit(7.0, "in", 300.0), 2100.0);
        assert_eq!(from_unit(2.54, "cm", 300.0), 300.0);
        assert_eq!(to_unit(640.0, "px", 72.0), 640.0);
    }

    #[test]
    fn preset_sets_size_resolution_and_create_params_drop_ui_keys() {
        let mut f = crate::state::UiState::new_document_fields();
        let a4 = CATEGORIES.iter().find(|c| c.0 == "Print").unwrap().1.iter().find(|p| p.0 == "A4").unwrap();
        apply_preset(&mut f, a4);
        let p = command_params(&f);
        assert_eq!(p["width"], 2480);
        assert_eq!(p["height"], 3508);
        assert_eq!(p["resolution"], 300.0);
        assert!(p.get("__preset").is_none() && p.get("__unit").is_none());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", p).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!((d.size.width, d.size.height, d.resolution_dpi), (2480, 3508, 300.0));
    }
    /// The real dialog (#254): a typed size must reach `file.new`, however it is confirmed.
    mod dialog {
        use super::super::{CATEGORIES, apply_preset};
        use crate::PhotocraftApp;
        use crate::state::{DialogKind, UiState};
        use egui::accesskit::Role;
        use egui_kittest::{Harness, kittest::Queryable};

        fn harness() -> Harness<'static, PhotocraftApp> {
            let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app);
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run_steps(3);
            h
        }

        fn click_at(h: &mut Harness<'static, PhotocraftApp>, at: egui::Pos2) {
            h.hover_at(at);
            h.run_steps(1);
            h.drag_at(at);
            h.run_steps(1);
            h.drop_at(at);
            h.run_steps(2);
        }

        /// The Width (0), Height (1) and Resolution (2) fields.
        fn field(h: &Harness<'static, PhotocraftApp>, i: usize) -> egui::Rect {
            h.query_all_by_role(Role::SpinButton).nth(i).map(|n| n.rect()).expect("a size field")
        }

        /// Click into field `i` (which selects its text) and type `text`, as a user does.
        fn type_into(h: &mut Harness<'static, PhotocraftApp>, i: usize, text: &str) {
            let r = field(h, i);
            click_at(h, r.center());
            for c in text.chars() {
                h.event(egui::Event::Text(c.to_string()));
                h.run_steps(1);
            }
        }

        fn fields(h: &Harness<'static, PhotocraftApp>) -> serde_json::Map<String, serde_json::Value> {
            h.state().ui.dialogs.first().map(|d| d.fields.clone()).expect("the dialog is open")
        }

        fn set_fields(h: &mut Harness<'static, PhotocraftApp>, f: serde_json::Map<String, serde_json::Value>) {
            h.state_mut().ui.dialogs[0].fields = f;
            h.run_steps(2);
        }

        fn enter(h: &mut Harness<'static, PhotocraftApp>) {
            h.key_press(egui::Key::Enter);
            h.run_steps(3);
        }

        fn created(h: &Harness<'static, PhotocraftApp>) -> (u32, u32, f32) {
            assert!(h.state().ui.dialogs.is_empty(), "the dialog closed");
            let d = &h.state().session.active().expect("a new document").doc;
            (d.size.width, d.size.height, d.resolution_dpi)
        }

        #[test]
        fn typed_size_then_enter_creates_that_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            let f = fields(&h);
            assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(512), Some(512)), "whole pixels: {f:?}");
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn typed_size_then_create_without_leaving_the_field_creates_that_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "300");
            // Still editing Height: click Create straight away.
            let create = h.get_by_label("Create").rect();
            click_at(&mut h, create.center());
            assert_eq!(created(&h), (512, 300, 72.0));
        }

        #[test]
        fn typing_over_a_preset_wins() {
            let mut h = harness();
            let mut f = fields(&h);
            let web = CATEGORIES.iter().find(|c| c.0 == "Web").unwrap().1.iter().find(|p| p.0 == "Web Minimum").unwrap();
            apply_preset(&mut f, web);
            set_fields(&mut h, f);
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            assert!(fields(&h).get("__preset").is_none(), "typing deselects the preset");
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn clicking_a_preset_card_after_typing_sets_its_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            h.get_by_label("Photo").click();
            h.run_steps(2);
            // The first card ("Landscape, 6 x 4") sits under the presets heading.
            let heading = h.get_by_label_contains("BLANK DOCUMENT PRESETS").rect();
            click_at(&mut h, heading.left_bottom() + egui::vec2(80.0, 60.0));
            assert_eq!(fields(&h).get("__preset").and_then(|v| v.as_str()), Some("Landscape, 6 x 4"));
            enter(&mut h);
            assert_eq!(created(&h), (1800, 1200, 300.0));
        }

        #[test]
        fn typed_size_in_inches_converts_at_the_resolution() {
            let mut h = harness();
            type_into(&mut h, 2, "300");
            let mut f = fields(&h);
            f.insert("__unit".into(), serde_json::json!("in"));
            set_fields(&mut h, f);
            type_into(&mut h, 0, "2");
            type_into(&mut h, 1, "1.5");
            enter(&mut h);
            assert_eq!(created(&h), (600, 450, 300.0));
        }

        #[test]
        fn changing_units_keeps_the_typed_pixel_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            for unit in ["in", "cm", "mm", "pt", "pica", "px"] {
                let mut f = fields(&h);
                f.insert("__unit".into(), serde_json::json!(unit));
                set_fields(&mut h, f);
            }
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn orientation_swap_keeps_whole_pixels() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "256");
            // The Portrait icon button follows the "Orientation" label (icons have tooltips only).
            let label = h.get_by_label("Orientation").rect();
            let gap = h.ctx.global_style().spacing.item_spacing.x;
            click_at(&mut h, egui::pos2(label.right() + gap + 12.0, label.center().y));
            enter(&mut h);
            assert_eq!(created(&h), (256, 512, 72.0));
        }
    }
}

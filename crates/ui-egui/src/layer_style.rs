//! Photoshop-style Layer Style dialog: effect list with checkboxes on the left, parameters on the right.
//! State lives in the dialog's `fields` (JSON), so automation can drive it like any other dialog.

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, vec2};
use photocraft_doc::effects::{FxPaint, StrokePosition};
use photocraft_doc::{Effect, Layer};
use serde_json::{Map, Value, json};

use crate::theme::Tokens;
use crate::{PhotocraftApp, widgets};

#[derive(Clone, Copy)]
enum P {
    Slider(f32, f32, &'static str),
    Color,
    Blend,
    Choice(&'static [(&'static str, &'static str)]),
    Check,
    /// A pattern from the dialog's `patternList` field (`[[id, name], …]`).
    Pattern,
}

/// Effect kinds in Photoshop's list order: (command kind, label, params).
/// The Blending Options page id (not an effect).
pub const BLENDING: &str = "blendingOptions";

pub const KINDS: &[(&str, &str)] = &[
    ("bevelEmboss", "Bevel & Emboss"),
    ("stroke", "Stroke"),
    ("innerShadow", "Inner Shadow"),
    ("innerGlow", "Inner Glow"),
    ("satin", "Satin"),
    ("colorOverlay", "Color Overlay"),
    ("gradientOverlay", "Gradient Overlay"),
    ("patternOverlay", "Pattern Overlay"),
    ("outerGlow", "Outer Glow"),
    ("dropShadow", "Drop Shadow"),
];

fn spec(kind: &str) -> &'static [(&'static str, &'static str, P)] {
    const POS: &[(&str, &str)] = &[("outside", "Outside"), ("inside", "Inside"), ("center", "Center")];
    const GSTYLE: &[(&str, &str)] = &[("linear", "Linear"), ("radial", "Radial"), ("angle", "Angle"), ("reflected", "Reflected"), ("diamond", "Diamond")];
    const BSTYLE: &[(&str, &str)] = &[("inner", "Inner Bevel"), ("outer", "Outer Bevel"), ("emboss", "Emboss"), ("pillow", "Pillow Emboss")];
    const DIR: &[(&str, &str)] = &[("up", "Up"), ("down", "Down")];
    const SRC: &[(&str, &str)] = &[("edge", "Edge"), ("center", "Center")];
    match kind {
        "dropShadow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("distance", "Distance", P::Slider(0.0, 300.0, "px")),
            ("spread", "Spread", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("knocksOut", "Layer Knocks Out Drop Shadow", P::Check),
        ],
        "innerShadow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("distance", "Distance", P::Slider(0.0, 300.0, "px")),
            ("choke", "Choke", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
        ],
        "outerGlow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
            ("spread", "Spread", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("range", "Range", P::Slider(1.0, 100.0, "%")),
        ],
        "innerGlow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
            ("source", "Source", P::Choice(SRC)),
            ("choke", "Choke", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
        ],
        "stroke" => &[
            ("size", "Size", P::Slider(1.0, 250.0, "px")),
            ("position", "Position", P::Choice(POS)),
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
        ],
        "colorOverlay" => &[("blend", "Blend Mode", P::Blend), ("color", "Color", P::Color), ("opacity", "Opacity", P::Slider(0.0, 100.0, "%"))],
        "gradientOverlay" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("from", "From", P::Color),
            ("to", "To", P::Color),
            ("reverse", "Reverse", P::Check),
            ("style", "Style", P::Choice(GSTYLE)),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("scale", "Scale", P::Slider(10.0, 150.0, "%")),
        ],
        "patternOverlay" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("pattern", "Pattern", P::Pattern),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("scale", "Scale", P::Slider(1.0, 1000.0, "%")),
            ("link", "Link with Layer", P::Check),
        ],
        "bevelEmboss" => &[
            ("style", "Style", P::Choice(BSTYLE)),
            ("depth", "Depth", P::Slider(1.0, 1000.0, "%")),
            ("direction", "Direction", P::Choice(DIR)),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("soften", "Soften", P::Slider(0.0, 16.0, "px")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("altitude", "Altitude", P::Slider(0.0, 90.0, "°")),
        ],
        "satin" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("distance", "Distance", P::Slider(0.0, 250.0, "px")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("invert", "Invert", P::Check),
        ],
        BLENDING => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("fillOpacity", "Fill Opacity", P::Slider(0.0, 100.0, "%")),
        ],
        _ => &[],
    }
}

fn defaults(kind: &str) -> Value {
    match kind {
        "dropShadow" => json!({"blend": "Multiply", "color": "#000000", "opacity": 75, "angle": 120, "distance": 5, "spread": 0, "size": 5, "knocksOut": true}),
        "innerShadow" => json!({"blend": "Multiply", "color": "#000000", "opacity": 75, "angle": 120, "distance": 5, "choke": 0, "size": 5}),
        "outerGlow" => json!({"blend": "Screen", "opacity": 75, "color": "#ffffbe", "spread": 0, "size": 5, "range": 50}),
        "innerGlow" => json!({"blend": "Screen", "opacity": 75, "color": "#ffffbe", "source": "edge", "choke": 0, "size": 5}),
        "stroke" => json!({"size": 3, "position": "outside", "blend": "Normal", "opacity": 100, "color": "#000000"}),
        "colorOverlay" => json!({"blend": "Normal", "color": "#ff0000", "opacity": 100}),
        "gradientOverlay" => {
            json!({"blend": "Normal", "opacity": 100, "from": "#000000", "to": "#ffffff", "reverse": false, "style": "linear", "angle": 90, "scale": 100})
        }
        "patternOverlay" => json!({"blend": "Normal", "opacity": 100, "pattern": "", "angle": 0, "scale": 100, "link": true}),
        "bevelEmboss" => json!({"style": "inner", "depth": 100, "direction": "up", "size": 5, "soften": 0, "angle": 120, "altitude": 30}),
        "satin" => json!({"blend": "Multiply", "color": "#000000", "opacity": 50, "angle": 19, "distance": 11, "size": 14, "invert": true}),
        _ => json!({}),
    }
}

fn hex(c: &photocraft_doc::Color) -> String {
    let [r, g, b, _] = c.to_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn kind_of(e: &Effect) -> &'static str {
    match e {
        Effect::DropShadow(_) => "dropShadow",
        Effect::InnerShadow(_) => "innerShadow",
        Effect::OuterGlow(_) => "outerGlow",
        Effect::InnerGlow(_) => "innerGlow",
        Effect::Stroke(_) => "stroke",
        Effect::ColorOverlay { .. } => "colorOverlay",
        Effect::GradientOverlay { .. } => "gradientOverlay",
        Effect::PatternOverlay { .. } => "patternOverlay",
        Effect::Satin(_) => "satin",
        Effect::BevelEmboss(_) => "bevelEmboss",
    }
}

/// Current values of an existing effect, in the command's parameter units.
fn values_of(e: &Effect) -> Value {
    let mut v = defaults(kind_of(e));
    let set = |v: &mut Value, k: &str, x: Value| {
        v[k] = x;
    };
    match e {
        Effect::DropShadow(s) | Effect::InnerShadow(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(&s.color)));
            set(&mut v, "angle", json!(s.angle));
            set(&mut v, "distance", json!(s.distance));
            set(&mut v, if matches!(e, Effect::DropShadow(_)) { "spread" } else { "choke" }, json!((s.spread * 100.0).round()));
            set(&mut v, "size", json!(s.size));
        }
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => {
            set(&mut v, "blend", json!(g.common.blend.label()));
            set(&mut v, "opacity", json!((g.common.opacity * 100.0).round()));
            if let FxPaint::Color(c) = &g.paint {
                set(&mut v, "color", json!(hex(c)));
            }
            set(&mut v, if matches!(e, Effect::OuterGlow(_)) { "spread" } else { "choke" }, json!((g.spread * 100.0).round()));
            set(&mut v, "size", json!(g.size));
            set(&mut v, "range", json!((g.range * 100.0).round()));
        }
        Effect::Stroke(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "size", json!(s.size));
            set(
                &mut v,
                "position",
                json!(match s.position {
                    StrokePosition::Inside => "inside",
                    StrokePosition::Center => "center",
                    _ => "outside",
                }),
            );
            if let FxPaint::Color(c) = &s.paint {
                set(&mut v, "color", json!(hex(c)));
            }
        }
        Effect::ColorOverlay { common, color } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(color)));
        }
        Effect::GradientOverlay { common, .. } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
        }
        Effect::Satin(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(&s.color)));
            set(&mut v, "angle", json!(s.angle));
            set(&mut v, "distance", json!(s.distance));
            set(&mut v, "size", json!(s.size));
            set(&mut v, "invert", json!(s.invert));
        }
        Effect::BevelEmboss(b) => {
            set(&mut v, "depth", json!(b.depth));
            set(&mut v, "size", json!(b.size));
            set(&mut v, "soften", json!(b.soften));
            set(&mut v, "angle", json!(b.angle));
            set(&mut v, "altitude", json!(b.altitude));
            set(&mut v, "direction", json!(if b.up { "up" } else { "down" }));
        }
        Effect::PatternOverlay { common, name, id, scale, angle, link, phase } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
            set(&mut v, "pattern", json!(if id.is_empty() { name } else { id }));
            set(&mut v, "scale", json!((scale * 100.0).round()));
            set(&mut v, "angle", json!(angle));
            set(&mut v, "link", json!(link));
            set(&mut v, "phaseX", json!(phase.0));
            set(&mut v, "phaseY", json!(phase.1));
        }
    }
    v
}

/// Dialog fields for a layer: selected kind, and per kind {enabled, params}.
pub fn initial_fields(layer: &Layer, select: Option<&str>) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert("layer".into(), json!(layer.id.0));
    f.insert(
        format!("p:{BLENDING}"),
        json!({"blend": layer.blend.label(), "opacity": (layer.opacity * 100.0).round(), "fillOpacity": (layer.fill_opacity * 100.0).round()}),
    );
    let mut first = None;
    for &(kind, _) in KINDS {
        let existing = layer.effects.items.iter().find(|e| kind_of(e) == kind);
        if existing.is_some() && first.is_none() {
            first = Some(kind);
        }
        f.insert(format!("on:{kind}"), json!(existing.is_some_and(|e| e.enabled())));
        f.insert(format!("p:{kind}"), existing.map(values_of).unwrap_or_else(|| defaults(kind)));
    }
    let sel = select.or(first).unwrap_or("dropShadow");
    f.insert("selected".into(), json!(sel));
    if select.is_some() {
        f.insert(format!("on:{sel}"), json!(true));
    }
    f
}

pub fn open(app: &mut PhotocraftApp, select: Option<&str>) -> Option<u64> {
    let st = app.session.active()?;
    let layer = st.doc.layer(st.active_layer?)?.clone();
    let mut f = initial_fields(&layer, select);
    f.insert("patternList".into(), pattern_list(app));
    Some(app.ui.open_dialog(crate::state::DialogKind::LayerStyle, f))
}

/// `[[id, name], …]` of the patterns a style can use (document's, then the library's).
pub fn pattern_list(app: &PhotocraftApp) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let doc_pats = app.session.active().map(|d| d.doc.patterns.clone()).unwrap_or_default();
    for p in doc_pats.iter().chain(app.session.patterns.items.iter()) {
        if seen.insert(p.id.clone()) {
            out.push(json!([p.id, p.display_name()]));
        }
    }
    Value::Array(out)
}

/// Apply the dialog: replace the layer's effects with the enabled ones.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    apply(f, |id, p| app.run(id, p))
}

/// Runs the dialog's commands through `run`: blending options, clear, then each enabled effect.
fn apply(f: &Map<String, Value>, mut run: impl FnMut(&str, Value) -> Result<Value, String>) -> Result<Value, String> {
    let layer = f.get("layer").cloned().unwrap_or(Value::Null);
    if let Some(Value::Object(bo)) = f.get(&format!("p:{BLENDING}")) {
        let mut p = Value::Object(bo.clone());
        p["layer"] = layer.clone();
        run("layer.layerStyle.blendingOptions", p)?;
    }
    let _ = run("layer.layerStyle.clear", json!({"layer": layer}));
    for &(kind, _) in KINDS {
        if f.get(&format!("on:{kind}")).and_then(Value::as_bool) == Some(true) {
            let mut p = f.get(&format!("p:{kind}")).cloned().unwrap_or_else(|| json!({}));
            p["layer"] = layer.clone();
            run(&format!("layer.layerStyle.{kind}"), p)?;
        }
    }
    Ok(Value::Null)
}

/// Hash of the fields that change the rendered style (not the selected page).
pub fn preview_hash(f: &Map<String, Value>) -> u64 {
    f.iter()
        .filter(|(k, _)| k.as_str() == "layer" || k.starts_with("on:") || k.starts_with("p:"))
        .flat_map(|(k, v)| k.bytes().chain(v.to_string().into_bytes()))
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// `doc` with the dialog's style applied, run on a scratch session (no history) for the live preview.
pub fn preview_document(
    doc: &photocraft_doc::Document,
    patterns: &photocraft_engine::pattern_cmds::PatternLibrary,
    f: &Map<String, Value>,
) -> Option<photocraft_doc::Document> {
    let mut s = photocraft_engine::Session::new();
    s.patterns = patterns.clone();
    s.add_document(doc.clone(), None);
    apply(f, |id, p| s.execute(id, p).map_err(|e| e.to_string())).ok()?;
    s.active().map(|d| (*d.doc).clone())
}

/// Dialog body (left list, right parameters).
pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let selected = f.get("selected").and_then(Value::as_str).unwrap_or("dropShadow").to_string();
    ui.horizontal_top(|ui| {
        // Left: effect list.
        ui.vertical(|ui| {
            ui.set_width(190.0);
            ui.label(RichText::new(tl!("Styles")).color(t.text_faint));
            // Blending Options page (layer blend mode, opacity and fill opacity).
            let bo = ui.add(
                egui::Label::new(RichText::new(tl!("Blending Options")).color(if selected == BLENDING { t.text } else { t.text_dim })).sense(Sense::click()),
            );
            if bo.clicked() {
                f.insert("selected".into(), json!(BLENDING));
            }
            ui.add_space(4.0);
            for &(kind, label) in KINDS {
                let (rect, resp) = ui.allocate_exact_size(vec2(190.0, 26.0), Sense::click());
                let is_sel = kind == selected;
                if is_sel {
                    ui.painter().rect_filled(rect, t.radius_sm, t.row_selected.gamma_multiply(if t.pro { 1.0 } else { 0.0 }).max_alpha(t.hover));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, t.radius_sm, t.hover.gamma_multiply(0.5));
                }
                let key = format!("on:{kind}");
                let mut on = f.get(&key).and_then(Value::as_bool).unwrap_or(false);
                let cb = egui::Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(14.0, 14.0));
                let cresp = ui.interact(cb, ui.id().with(("fxcb", kind)), Sense::click());
                if on {
                    ui.painter().rect_filled(cb, 2.0, t.accent);
                    ui.painter().line_segment([cb.left_center() + vec2(3.0, 0.5), cb.center_bottom() + vec2(-1.0, -3.5)], Stroke::new(1.8, Color32::WHITE));
                    ui.painter().line_segment([cb.center_bottom() + vec2(-1.0, -3.5), cb.right_top() + vec2(-3.0, 3.5)], Stroke::new(1.8, Color32::WHITE));
                } else {
                    ui.painter().rect_stroke(cb, 2.0, Stroke::new(1.5, t.text_faint), StrokeKind::Inside);
                }
                ui.painter().text(
                    rect.left_center() + vec2(28.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    tl!(label),
                    egui::FontId::proportional(12.5),
                    if on || is_sel { t.text } else { t.text_dim },
                );
                if cresp.clicked() {
                    on = !on;
                    f.insert(key, json!(on));
                    f.insert("selected".into(), json!(kind));
                } else if resp.clicked() {
                    f.insert("selected".into(), json!(kind));
                    f.insert(key, json!(true));
                }
            }
        });
        widgets::vline(ui, 330.0);
        // Right: parameters of the selected effect.
        ui.vertical(|ui| {
            ui.set_width(330.0);
            let label = KINDS.iter().find(|k| k.0 == selected).map(|k| k.1).unwrap_or(if selected == BLENDING { "Blending Options" } else { "" });
            ui.label(RichText::new(tl!(&label)).font(crate::theme::semibold(14.0)).color(t.text));
            ui.add_space(6.0);
            let pkey = format!("p:{selected}");
            let mut p = f.get(&pkey).cloned().unwrap_or_else(|| defaults(&selected));
            for &(key, label, kind) in spec(&selected) {
                match kind {
                    P::Slider(min, max, unit) => {
                        let mut v = p.get(key).and_then(Value::as_f64).unwrap_or(min as f64) as f32;
                        if widgets::slider_row(ui, label, &mut v, min..=max, unit, None).changed() {
                            p[key] = json!(v.round());
                        }
                    }
                    P::Color => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                            let hexs = p.get(key).and_then(Value::as_str).unwrap_or("#000000").to_string();
                            let mut c = parse_hex(&hexs);
                            if ui.color_edit_button_srgba(&mut c).changed() {
                                p[key] = json!(format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()));
                            }
                        });
                    }
                    P::Blend => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                            let mut cur = p.get(key).and_then(Value::as_str).unwrap_or("Normal").to_string();
                            let opts: Vec<(String, &str)> =
                                photocraft_color::BlendMode::LAYER_MODES.iter().map(|m| (m.label().to_string(), m.label())).collect();
                            if widgets::dropdown(ui, &format!("fx-blend-{selected}"), &mut cur, &opts, 150.0) {
                                p[key] = json!(cur);
                            }
                        });
                    }
                    P::Choice(options) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                            let mut cur = p.get(key).and_then(Value::as_str).unwrap_or(options[0].0).to_string();
                            let opts: Vec<(String, &str)> = options.iter().map(|(v, l)| (v.to_string(), *l)).collect();
                            if widgets::dropdown(ui, &format!("fx-{selected}-{key}"), &mut cur, &opts, 150.0) {
                                p[key] = json!(cur);
                            }
                        });
                    }
                    P::Pattern => {
                        let list: Vec<(String, String)> = f
                            .get("patternList")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_str()?.to_string()))).collect())
                            .unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                            let mut cur = p
                                .get(key)
                                .and_then(Value::as_str)
                                .filter(|c| !c.is_empty())
                                .map(str::to_string)
                                .or_else(|| list.first().map(|l| l.0.clone()))
                                .unwrap_or_default();
                            let opts: Vec<(String, &str)> = list.iter().map(|(id, n)| (id.clone(), n.as_str())).collect();
                            if widgets::dropdown(ui, &format!("fx-{selected}-{key}"), &mut cur, &opts, 180.0)
                                || p.get(key).and_then(Value::as_str).is_none_or(str::is_empty)
                            {
                                p[key] = json!(cur);
                            }
                        });
                    }
                    P::Check => {
                        let mut b = p.get(key).and_then(Value::as_bool).unwrap_or(false);
                        if widgets::checkbox(ui, &mut b, label).changed() {
                            p[key] = json!(b);
                        }
                    }
                }
                ui.add_space(2.0);
            }
            f.insert(pkey, p);
        });
    });
}

fn parse_hex(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color32::from_rgb(b(0), b(2), b(4))
}

trait MaxAlpha {
    fn max_alpha(self, other: Color32) -> Color32;
}

impl MaxAlpha for Color32 {
    /// Fall back to `other` when this colour is fully transparent (non-Pro themes have no row colour).
    fn max_alpha(self, other: Color32) -> Color32 {
        if self.a() == 0 { other } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_spec_and_defaults() {
        for &(kind, _) in KINDS {
            assert!(!spec(kind).is_empty(), "{kind}");
            let d = defaults(kind);
            for &(key, _, _) in spec(kind) {
                assert!(d.get(key).is_some(), "{kind}.{key} has no default");
            }
        }
    }

    #[test]
    fn initial_fields_select_requested_kind() {
        let l = Layer::raster("x", photocraft_doc::PixelFormat::RGBA8);
        let f = initial_fields(&l, Some("stroke"));
        assert_eq!(f["selected"], "stroke");
        assert_eq!(f["on:stroke"], true);
        assert_eq!(f["on:dropShadow"], false);
    }

    #[test]
    fn preview_applies_the_style_without_touching_the_document() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let st = s.active().unwrap();
        let mut f = initial_fields(st.doc.layer(st.active_layer.unwrap()).unwrap(), Some("colorOverlay"));
        let h = preview_hash(&f);
        f.insert("selected".into(), json!("stroke"));
        assert_eq!(preview_hash(&f), h, "switching pages doesn't re-render");
        f.insert("on:stroke".into(), json!(true));
        assert_ne!(preview_hash(&f), h);
        let shown = preview_document(&st.doc, &s.patterns, &f).unwrap();
        let fx = |d: &photocraft_doc::Document| d.layer(st.active_layer.unwrap()).unwrap().effects.items.len();
        assert_eq!((fx(&shown), fx(&st.doc)), (2, 0));
    }

    #[test]
    fn percent_fields_round_trip_through_the_engine() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("layer.layerStyle.outerGlow", json!({"spread": 6, "range": 40})).unwrap();
        s.execute("layer.layerStyle.dropShadow", json!({"spread": 12, "add": true})).unwrap();
        let st = s.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
        let f = initial_fields(l, None);
        assert_eq!(f["p:outerGlow"]["spread"], json!(6.0));
        assert_eq!(f["p:outerGlow"]["range"], json!(40.0));
        assert_eq!(f["p:dropShadow"]["spread"], json!(12.0));
    }
}

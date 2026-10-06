//! One editor per adjustment kind, shared by the adjustment-layer Properties panel and the modal
//! Image › Adjustments dialogs (`adjust_dialog`).
//!
//! Every editor edits the kind's complete parameter set (`photocraft_engine::adjust_params`) as a
//! JSON object and reports an [`Edit`]: `changed` while values move (the host previews them live)
//! and `commit` when a gesture ends (the Properties host then runs one `layer.setAdjustment`; a
//! dialog commits on OK). Selective Color and Color Lookup keep their editors in `adjust_ui`.

use std::sync::Arc;

use egui::{Color32, Key, Modifiers, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_doc::adjust::ToneSpace;
use photocraft_doc::{Adjustment, LayerId};
use photocraft_engine::adjust_params::{self, HUE_RANGES, PHOTO_FILTERS};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::tone::{self, HistSource, Histograms};
use crate::widgets;

/// Kinds edited here (Selective Color and Color Lookup have their own editors; Invert has none).
pub const KINDS: [&str; 13] = [
    "brightnessContrast",
    "levels",
    "curves",
    "exposure",
    "vibrance",
    "hueSaturation",
    "colorBalance",
    "blackWhite",
    "photoFilter",
    "channelMixer",
    "posterize",
    "threshold",
    "gradientMap",
];

pub fn has_editor(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Kinds that draw a histogram behind their controls.
pub fn needs_histogram(kind: &str) -> bool {
    matches!(kind, "levels" | "curves" | "threshold")
}

/// What an editor did this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    /// Values changed (preview them).
    pub changed: bool,
    /// A gesture ended or a discrete control changed (commit them).
    pub commit: bool,
}

impl Edit {
    fn add(&mut self, o: Edit) {
        self.changed |= o.changed;
        self.commit |= o.commit;
    }
    fn discrete(changed: bool) -> Self {
        Edit { changed, commit: changed }
    }
    fn of(r: &egui::Response) -> Self {
        Edit { changed: r.changed(), commit: r.drag_stopped() || (r.changed() && !r.dragged()) }
    }
}

/// What an editor needs from its host.
pub struct EditorCx {
    /// Root of the editor's view state in egui memory (selected channel, point, range…).
    pub mem: egui::Id,
    /// Histograms in the adjustment's space ([`needs_histogram`] kinds).
    pub hist: Option<Arc<Histograms>>,
    /// Grayscale (or Duotone/Bitmap) document: one Gray channel.
    pub gray: bool,
    /// Current foreground and background colours (Gradient Map preset).
    pub swatches: [[f32; 3]; 2],
}

/// The Levels/Curves channel space the values address.
pub fn space_of(v: &Value) -> ToneSpace {
    if v.get("cyan").is_some() || v.get("black").is_some() {
        ToneSpace::Cmyk
    } else if v.get("a").is_some() {
        ToneSpace::Lab
    } else {
        ToneSpace::Rgb
    }
}

/// The editor for `kind` over its parameter set `v`.
pub fn editor(ui: &mut egui::Ui, kind: &str, v: &mut Value, cx: &EditorCx) -> Edit {
    if !v.is_object() {
        *v = json!({});
    }
    match kind {
        "brightnessContrast" => brightness_contrast(ui, v),
        "levels" => levels(ui, v, cx),
        "curves" => curves(ui, v, cx),
        "exposure" => sliders(
            ui,
            v,
            &[
                ("exposure", tl!("Exposure"), -20.0, 20.0, 0.0, ""),
                ("offset", tl!("Offset"), -0.5, 0.5, 0.0, ""),
                ("gamma", tl!("Gamma Correction"), 0.01, 9.99, 1.0, ""),
            ],
        ),
        "vibrance" => sliders(ui, v, &[("vibrance", "Vibrance", -100.0, 100.0, 0.0, "%"), ("saturation", "Saturation", -100.0, 100.0, 0.0, "%")]),
        "hueSaturation" => hue_saturation(ui, v, cx),
        "colorBalance" => color_balance(ui, v, cx),
        "blackWhite" => black_white(ui, v),
        "photoFilter" => photo_filter(ui, v, cx),
        "channelMixer" => channel_mixer(ui, v, cx),
        "posterize" => sliders(ui, v, &[("levels", "Levels", 2.0, 255.0, 4.0, "")]),
        "threshold" => threshold(ui, v, cx),
        "gradientMap" => gradient_map(ui, v, cx),
        _ => Edit::default(),
    }
}

// ---------------------------------------------------------------------------------------------
// JSON helpers

fn num(v: &Value, key: &str, default: f32) -> f32 {
    v.get(key).and_then(Value::as_f64).map_or(default, |x| x as f32)
}

fn flag(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn nums<const N: usize>(v: &Value, key: &str, default: [f32; N]) -> [f32; N] {
    let a = v.get(key).and_then(Value::as_array);
    std::array::from_fn(|i| a.and_then(|a| a.get(i)).and_then(Value::as_f64).map_or(default[i], |x| x as f32))
}

fn round(x: f32, step: f32) -> f32 {
    (x / step).round() * step
}

fn rgb_of(v: &Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    match v.get(key) {
        Some(Value::String(s)) => {
            let h = s.trim_start_matches('#');
            let b = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map(|x| f32::from(x) / 255.0);
            match (b(0), b(2), b(4)) {
                (Some(r), Some(g), Some(bb)) => [r, g, bb],
                _ => default,
            }
        }
        Some(Value::Array(a)) => std::array::from_fn(|i| a.get(i).and_then(Value::as_f64).map_or(default[i], |x| x as f32)),
        _ => default,
    }
}

fn color32(c: [f32; 3]) -> Color32 {
    let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

/// A colour swatch button (egui's picker); returns the edit.
fn color_button(ui: &mut egui::Ui, c: &mut [f32; 3]) -> Edit {
    let mut srgb = [c[0], c[1], c[2]].map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
    let r = ui.color_edit_button_srgb(&mut srgb);
    if r.changed() {
        *c = srgb.map(|x| f32::from(x) / 255.0);
    }
    // The popup edits continuously; each change is a complete value.
    Edit::discrete(r.changed())
}

fn label(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!(&text)).color(t.text_dim).size(12.0));
}

/// Labelled slider rows over top-level keys: (key, label, min, max, default, unit).
fn sliders(ui: &mut egui::Ui, v: &mut Value, rows: &[(&str, &str, f32, f32, f32, &str)]) -> Edit {
    let mut e = Edit::default();
    for &(key, text, min, max, default, unit) in rows {
        let mut x = num(v, key, default).clamp(min, max);
        let r = widgets::slider_row(ui, text, &mut x, min..=max, unit, None);
        if r.changed() {
            v[key] = json!(if max - min <= 10.0 { round(x, 0.01) } else { x.round() });
        }
        e.add(Edit::of(&r));
    }
    e
}

fn gradient_slider(ui: &mut egui::Ui, text: &str, x: &mut f32, range: std::ops::RangeInclusive<f32>, unit: &str, from: Color32, to: Color32) -> Edit {
    let r = widgets::slider_row(ui, text, x, range, unit, Some(&[from, to]));
    Edit::of(&r)
}

// ---------------------------------------------------------------------------------------------
// Brightness/Contrast

fn brightness_contrast(ui: &mut egui::Ui, v: &mut Value) -> Edit {
    let legacy = flag(v, "legacy", false);
    let lo = if legacy { -100.0 } else { -50.0 };
    let mut e = sliders(ui, v, &[("brightness", tl!("Brightness"), -150.0, 150.0, 0.0, ""), ("contrast", tl!("Contrast"), lo, 100.0, 0.0, "")]);
    let mut l = legacy;
    if widgets::checkbox(ui, &mut l, tl!("Use Legacy")).changed() {
        v["legacy"] = json!(l);
        if !l {
            v["contrast"] = json!(num(v, "contrast", 0.0).max(-50.0));
        }
        e.add(Edit::discrete(true));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Channels of Levels / Curves

/// A channel of a tone editor: the params key (None = the top-level / composite keys for Levels;
/// Curves uses "points"), its label, and its colour name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneChannel {
    pub key: &'static str,
    pub label: &'static str,
}

/// The channels a tone editor lists for `space` (histogram index = position).
pub fn tone_channels(space: ToneSpace, gray: bool) -> &'static [ToneChannel] {
    const RGB: [ToneChannel; 4] = [
        ToneChannel { key: "", label: "RGB" },
        ToneChannel { key: "red", label: "Red" },
        ToneChannel { key: "green", label: "Green" },
        ToneChannel { key: "blue", label: "Blue" },
    ];
    const GRAY: [ToneChannel; 1] = [ToneChannel { key: "", label: "Gray" }];
    const CMYK: [ToneChannel; 5] = [
        ToneChannel { key: "", label: "CMYK" },
        ToneChannel { key: "cyan", label: "Cyan" },
        ToneChannel { key: "magenta", label: "Magenta" },
        ToneChannel { key: "yellow", label: "Yellow" },
        ToneChannel { key: "black", label: "Black" },
    ];
    const LAB: [ToneChannel; 3] =
        [ToneChannel { key: "lightness", label: "Lightness" }, ToneChannel { key: "a", label: "a" }, ToneChannel { key: "b", label: "b" }];
    match space {
        ToneSpace::Rgb if gray => &GRAY,
        ToneSpace::Rgb => &RGB,
        ToneSpace::Cmyk => &CMYK,
        ToneSpace::Lab => &LAB,
    }
}

fn channel_picker(ui: &mut egui::Ui, id: egui::Id, chans: &[ToneChannel], text: &str) -> usize {
    let mut ch: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    if ch >= chans.len() {
        ch = 0;
    }
    ui.horizontal(|ui| {
        label(ui, text);
        let opts: Vec<(usize, &str)> = chans.iter().enumerate().map(|(i, c)| (i, c.label)).collect();
        widgets::dropdown(ui, &format!("{id:?}-ch"), &mut ch, &opts, 110.0);
    });
    ui.data_mut(|d| d.insert_temp(id, ch));
    ch
}

fn hist_color(ch: &ToneChannel, t: &Tokens) -> Color32 {
    match ch.key {
        "" | "lightness" | "black" => Color32::from_gray(if t.pro { 150 } else { 110 }),
        k => tone::channel_color(k, t),
    }
}

fn bar_color(ch: &ToneChannel, t: &Tokens) -> Color32 {
    match ch.key {
        "" | "lightness" | "black" => Color32::WHITE,
        k => tone::channel_color(k, t),
    }
}

/// Horizontal (or vertical) black → `to` gradient bar.
fn gradient_bar(p: &egui::Painter, r: Rect, from: Color32, to: Color32, vertical: bool) {
    let mut mesh = egui::Mesh::default();
    let (a, b, c, d) = if vertical {
        (r.left_bottom(), r.right_bottom(), r.right_top(), r.left_top())
    } else {
        (r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom())
    };
    let (ca, cb) = if vertical { (from, from) } else { (from, to) };
    let (cc, cd) = if vertical { (to, to) } else { (to, from) };
    mesh.colored_vertex(a, ca);
    mesh.colored_vertex(b, cb);
    mesh.colored_vertex(c, cc);
    mesh.colored_vertex(d, cd);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
}

// ---------------------------------------------------------------------------------------------
// Curves

/// Curve point editing as pure functions on `[input, output]` points in 0..=255, sorted by input.
pub mod curve_edit {
    use egui::Pos2;

    /// Most points a curve can hold (Photoshop's limit).
    pub const MAX_POINTS: usize = 16;
    /// Points closer than this (input levels) are the same point.
    pub const MIN_GAP: f32 = 2.0;

    /// Index of the point nearest `pos` (screen) within `radius` pixels.
    pub fn hit(pts: &[[f32; 2]], to_scr: impl Fn([f32; 2]) -> Pos2, pos: Pos2, radius: f32) -> Option<usize> {
        pts.iter().enumerate().map(|(i, q)| (i, to_scr(*q).distance(pos))).filter(|(_, d)| *d <= radius).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
    }

    /// Adds a point at `v` (kept sorted). A point already at that input is selected instead; a
    /// full curve takes no more points.
    pub fn insert(pts: &mut Vec<[f32; 2]>, v: [f32; 2]) -> Option<usize> {
        let v = [v[0].clamp(0.0, 255.0), v[1].clamp(0.0, 255.0)];
        if let Some(i) = pts.iter().position(|q| (q[0] - v[0]).abs() < MIN_GAP) {
            return Some(i);
        }
        if pts.len() >= MAX_POINTS {
            return None;
        }
        let i = pts.iter().position(|q| q[0] > v[0]).unwrap_or(pts.len());
        pts.insert(i, v);
        Some(i)
    }

    /// Endpoints stay, and a curve keeps at least two points.
    pub fn can_delete(pts: &[[f32; 2]], i: usize) -> bool {
        pts.len() > 2 && i > 0 && i + 1 < pts.len()
    }

    pub fn delete(pts: &mut Vec<[f32; 2]>, i: usize) -> bool {
        if !can_delete(pts, i) {
            return false;
        }
        pts.remove(i);
        true
    }

    /// Moves point `i` to `v`, kept strictly between its neighbours' inputs.
    pub fn move_to(pts: &mut [[f32; 2]], i: usize, v: [f32; 2]) -> bool {
        let n = pts.len();
        if i >= n {
            return false;
        }
        let lo = if i > 0 { pts[i - 1][0] + 1.0 } else { 0.0 };
        let hi = if i + 1 < n { pts[i + 1][0] - 1.0 } else { 255.0 };
        let new = [v[0].round().clamp(lo, hi.max(lo)), v[1].round().clamp(0.0, 255.0)];
        let changed = new != pts[i];
        pts[i] = new;
        changed
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CurveUi {
    channel: usize,
    sel: Option<usize>,
    /// The point being dragged (None while dragged off the graph), the grab offset and whether
    /// the drag removed it.
    drag: Option<(Option<usize>, [f32; 2], bool)>,
}

/// Dragging a point this far outside the graph removes it (it comes back if dragged back in).
pub const DRAG_OFF: f32 = 12.0;
const HIT_RADIUS: f32 = 9.0;
const CURVE_HANDLE_SIZE: f32 = 7.0;
const CURVE_GRAPH_PADDING: f32 = 5.0;

fn curve_graph(full: Rect, side: f32) -> Rect {
    let base = Rect::from_min_size(full.min + vec2(14.0, 0.0), vec2(side - 14.0, side - 14.0));
    Rect::from_min_max(base.min + vec2(CURVE_GRAPH_PADDING, CURVE_GRAPH_PADDING), base.max - vec2(CURVE_GRAPH_PADDING, CURVE_GRAPH_PADDING))
}

fn read_curve(v: &Value, key: &str) -> Vec<[f32; 2]> {
    let pts: Vec<[f32; 2]> = v
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
                })
                .collect()
        })
        .unwrap_or_default();
    if pts.len() < 2 { vec![[0.0, 0.0], [255.0, 255.0]] } else { pts }
}

fn curve_key(ch: &ToneChannel) -> &'static str {
    if ch.key.is_empty() { "points" } else { ch.key }
}

fn curves(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let space = space_of(v);
    let chans = tone_channels(space, cx.gray);
    let state_id = cx.mem.with("curves");
    let mut st: CurveUi = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let ch = channel_picker(ui, cx.mem.with("curves-ch"), chans, tl!("Channel:"));
    if ch != st.channel {
        st = CurveUi { channel: ch, ..Default::default() };
    }
    let chan = chans.get(ch).copied().unwrap_or(ToneChannel { key: "", label: "" });
    let key = curve_key(&chan);
    let mut pts = read_curve(v, key);
    let side = ui.available_width().clamp(120.0, 300.0);
    ui.add_space(4.0);
    let (full, resp) = ui.allocate_exact_size(vec2(side, side + 14.0), Sense::click_and_drag());
    let graph = curve_graph(full, side);
    ui.data_mut(|d| d.insert_temp(cx.mem.with("curves-graph"), graph));
    let to_scr = |q: [f32; 2]| pos2(graph.left() + q[0] / 255.0 * graph.width(), graph.bottom() - q[1] / 255.0 * graph.height());
    let to_val =
        |s: Pos2| [((s.x - graph.left()) / graph.width() * 255.0).clamp(0.0, 255.0), ((graph.bottom() - s.y) / graph.height() * 255.0).clamp(0.0, 255.0)];
    let mut e = Edit::default();
    let mut changed = false;

    // Press: hit-test where the button went down (not where a drag is first recognised).
    let press = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed());
    if press && let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
        resp.request_focus();
        let delete = ui.input(|i| i.modifiers.command);
        match curve_edit::hit(&pts, to_scr, pos, HIT_RADIUS) {
            Some(i) if delete => {
                if curve_edit::delete(&mut pts, i) {
                    changed = true;
                    e.commit = true;
                }
                st.sel = None;
                st.drag = None;
            }
            Some(i) => {
                st.sel = Some(i);
                let g = to_val(pos);
                st.drag = Some((Some(i), [pts[i][0] - g[0], pts[i][1] - g[1]], false));
            }
            None if graph.contains(pos) && !delete => {
                let before = pts.len();
                st.sel = curve_edit::insert(&mut pts, to_val(pos));
                changed = pts.len() != before;
                st.drag = st.sel.map(|i| (Some(i), [0.0, 0.0], false));
            }
            None => st.drag = None,
        }
    }
    // Drag: follow the pointer; off the graph the point is removed (back on, it returns).
    if resp.is_pointer_button_down_on()
        && !press
        && let (Some((index, off, removed)), Some(pos)) = (st.drag, ui.input(|i| i.pointer.interact_pos()))
    {
        let outside = !graph.expand(DRAG_OFF).contains(pos);
        let g = to_val(pos);
        let target = [g[0] + off[0], g[1] + off[1]];
        match index {
            Some(i) if outside && curve_edit::can_delete(&pts, i) => {
                pts.remove(i);
                st.drag = Some((None, off, true));
                st.sel = None;
                changed = true;
            }
            Some(i) => changed |= curve_edit::move_to(&mut pts, i, target),
            None if removed && !outside => {
                st.sel = curve_edit::insert(&mut pts, target);
                st.drag = Some((st.sel, off, true));
                changed |= st.sel.is_some();
            }
            None => {}
        }
    }
    if st.drag.is_some() && !resp.is_pointer_button_down_on() {
        st.drag = None;
        e.commit = true;
    }
    // Keys while the graph has focus: Delete/Backspace removes, arrows nudge (Shift: ×10).
    if resp.has_focus()
        && let Some(i) = st.sel.filter(|i| *i < pts.len())
    {
        let del = ui.input_mut(|inp| inp.consume_key(Modifiers::NONE, Key::Delete) || inp.consume_key(Modifiers::NONE, Key::Backspace));
        if del && curve_edit::delete(&mut pts, i) {
            st.sel = None;
            changed = true;
            e.commit = true;
        } else {
            let step = if ui.input(|inp| inp.modifiers.shift) { 10.0 } else { 1.0 };
            let mut d = [0.0f32; 2];
            for (k, dx, dy) in [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, 1.0), (Key::ArrowDown, 0.0, -1.0)] {
                if ui.input_mut(|inp| inp.consume_key(Modifiers::NONE, k) || inp.consume_key(Modifiers::SHIFT, k)) {
                    d = [d[0] + dx * step, d[1] + dy * step];
                }
            }
            let to = [pts[i][0] + d[0], pts[i][1] + d[1]];
            if d != [0.0, 0.0] && curve_edit::move_to(&mut pts, i, to) {
                changed = true;
                e.commit = true;
            }
        }
    }

    // Draw: histogram, quarter grid, baseline, gradient bars, other channels, the curve, points.
    let p = ui.painter_at(full.expand(2.0));
    p.rect_filled(graph, 0.0, t.field);
    if let Some(h) = &cx.hist {
        tone::draw_histogram(&p, graph, &h[ch], Color32::from_gray(if t.pro { 88 } else { 190 }).gamma_multiply(0.75));
    }
    for i in 1..4 {
        let f = i as f32 / 4.0;
        let g = Stroke::new(1.0, t.separator);
        p.line_segment([pos2(graph.left() + f * graph.width(), graph.top()), pos2(graph.left() + f * graph.width(), graph.bottom())], g);
        p.line_segment([pos2(graph.left(), graph.top() + f * graph.height()), pos2(graph.right(), graph.top() + f * graph.height())], g);
    }
    p.line_segment([graph.left_bottom(), graph.right_top()], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
    p.rect_stroke(
        graph,
        0.0,
        Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, if resp.has_focus() { t.accent } else { t.field_border }),
        StrokeKind::Outside,
    );
    let hbar = Rect::from_min_max(pos2(graph.left(), graph.bottom() + 4.0), pos2(graph.right(), graph.bottom() + 12.0));
    let vbar = Rect::from_min_max(pos2(full.left(), graph.top()), pos2(full.left() + 8.0, graph.bottom()));
    gradient_bar(&p, hbar, Color32::BLACK, bar_color(&chan, &t), false);
    gradient_bar(&p, vbar, Color32::BLACK, bar_color(&chan, &t), true);
    let draw_curve = |pts: &[[f32; 2]], color: Color32, width: f32| {
        let cp: Vec<photocraft_doc::adjust::CurvePoint> =
            pts.iter().map(|q| photocraft_doc::adjust::CurvePoint { input: q[0] / 255.0, output: q[1] / 255.0 }).collect();
        let lut = photocraft_compose::adjust::curve_lut(&cp);
        let n = lut.len().max(2);
        let line: Vec<Pos2> =
            (0..n).step_by((n / 256).max(1)).map(|i| to_scr([i as f32 / (n - 1) as f32 * 255.0, lut.get(i).copied().unwrap_or(0.0) * 255.0])).collect();
        p.add(egui::Shape::line(line, Stroke::new(width, color)));
    };
    if ch == 0 {
        // Photoshop overlays the edited channel curves on the composite.
        for c in chans.iter().skip(1) {
            let cp = read_curve(v, c.key);
            if cp.len() > 2 || cp.first().is_some_and(|q| q[1] != 0.0) || cp.last().is_some_and(|q| q[1] != 255.0) {
                draw_curve(&cp, bar_color(c, &t).gamma_multiply(0.8), 1.0);
            }
        }
    }
    draw_curve(&pts, if ch == 0 { t.text } else { bar_color(&chan, &t) }, 1.5);
    for (i, q) in pts.iter().enumerate() {
        let r = Rect::from_center_size(to_scr(*q), vec2(CURVE_HANDLE_SIZE, CURVE_HANDLE_SIZE));
        if Some(i) == st.sel {
            p.rect_filled(r, 0.0, t.text);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Outside);
        } else {
            p.rect_filled(r, 0.0, t.field);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
        }
    }

    // Input / Output of the selected point.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let i = st.sel.filter(|i| *i < pts.len());
        let (mut vi, mut vo) = i.map_or((0.0, 0.0), |i| (pts[i][0], pts[i][1]));
        label(ui, tl!("Input:"));
        let ri = ui.add_enabled_ui(i.is_some(), |ui| widgets::value_field(ui, &mut vi, 0.0..=255.0, "", 52.0)).inner;
        label(ui, tl!("Output:"));
        let ro = ui.add_enabled_ui(i.is_some(), |ui| widgets::value_field(ui, &mut vo, 0.0..=255.0, "", 52.0)).inner;
        if let Some(i) = i
            && (ri.changed() || ro.changed())
            && curve_edit::move_to(&mut pts, i, [vi, vo])
        {
            changed = true;
            e.commit |= !(ri.dragged() || ro.dragged());
        }
        if ri.drag_stopped() || ro.drag_stopped() {
            e.commit = true;
        }
    });
    let t2 = Tokens::get(ui.ctx());
    ui.label(
        RichText::new(crate::i18n::fmt(
            tl!("Click to add a point · drag off or {key}-click or Delete to remove"),
            &[("key", &crate::shortcuts::pretty("Cmd"))],
        ))
        .color(t2.text_faint)
        .size(11.0),
    );
    ui.data_mut(|d| d.insert_temp(state_id, st));
    if changed {
        v[key] = json!(pts.iter().map(|q| [q[0].round(), q[1].round()]).collect::<Vec<_>>());
        if space == ToneSpace::Lab {
            // Lab has no composite: "points" would alias lightness.
            if let Some(o) = v.as_object_mut() {
                o.remove("points");
            }
        }
        e.changed = true;
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Levels

type Lv = [f32; 5]; // in_black, gamma, in_white, out_black, out_white (0–255, gamma as is)

fn read_levels(v: &Value, key: &str) -> Lv {
    let o = if key.is_empty() { Some(v) } else { v.get(key) };
    let g = |k: &str, d: f32| o.and_then(|o| o.get(k)).and_then(Value::as_f64).map_or(d, |x| x as f32);
    [g("inBlack", 0.0), g("gamma", 1.0), g("inWhite", 255.0), g("outBlack", 0.0), g("outWhite", 255.0)]
}

fn write_levels(v: &mut Value, key: &str, l: &Lv) {
    let obj = json!({"inBlack": l[0].round(), "gamma": round(l[1], 0.01), "inWhite": l[2].round(), "outBlack": l[3].round(), "outWhite": l[4].round()});
    if key.is_empty() {
        if let (Some(dst), Value::Object(src)) = (v.as_object_mut(), obj) {
            dst.extend(src);
        }
    } else {
        v[key] = obj;
    }
}

/// A triangular slider handle below a bar.
fn handle(p: &egui::Painter, x: f32, y: f32, fill: Color32, stroke: Color32) {
    p.add(egui::Shape::convex_polygon(vec![pos2(x, y), pos2(x + 5.5, y + 9.0), pos2(x - 5.5, y + 9.0)], fill, Stroke::new(1.0, stroke)));
}

/// Input level (0–255) where the gamma slider sits: the input that maps to 50% grey.
pub fn gamma_pos(black: f32, white: f32, gamma: f32) -> f32 {
    black + (white - black) * 0.5f32.powf(gamma)
}

pub fn gamma_from_pos(black: f32, white: f32, x: f32) -> f32 {
    let t = ((x - black) / (white - black).max(1.0)).clamp(0.01, 0.99);
    (t.ln() / 0.5f32.ln()).clamp(0.01, 9.99)
}

/// Black and white input levels clipping 0.1 % at each end of `h` (Photoshop's Auto).
pub fn auto_levels(h: &[u32; 256]) -> (f32, f32) {
    let total: u64 = h.iter().map(|x| u64::from(*x)).sum();
    let clip = total / 1000;
    let (mut acc, mut lo) = (0u64, 0usize);
    while lo < 253 && acc + u64::from(h[lo]) <= clip {
        acc += u64::from(h[lo]);
        lo += 1;
    }
    let (mut acc, mut hi) = (0u64, 255usize);
    while hi > lo + 2 && acc + u64::from(h[hi]) <= clip {
        acc += u64::from(h[hi]);
        hi -= 1;
    }
    (lo as f32, hi as f32)
}

/// Which handle of a slider row a press at `x` grabs (nearest), among `xs`.
fn nearest(xs: &[f32], x: f32) -> Option<u8> {
    xs.iter().enumerate().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map(|(i, _)| i as u8)
}

fn levels(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let chans = tone_channels(space_of(v), cx.gray);
    let mut ch = 0;
    let mut auto = false;
    ui.horizontal(|ui| {
        ch = channel_picker(ui, cx.mem.with("levels-ch"), chans, tl!("Channel:"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            auto = widgets::secondary_button(ui, tl!("Auto"), 0.0).clicked();
        });
    });
    let chan = chans.get(ch).copied().unwrap_or(ToneChannel { key: "", label: "" });
    let mut l = read_levels(v, chan.key);
    let mut e = Edit::default();
    let mut changed = false;
    if auto && let Some(h) = &cx.hist {
        let (lo, hi) = auto_levels(&h[ch]);
        l[0] = lo;
        l[2] = hi;
        l[1] = 1.0;
        changed = true;
        e.commit = true;
    }
    let w = ui.available_width().min(300.0);
    // Histogram.
    let (hr, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
    let hr = hr.shrink2(vec2(6.0, 0.0));
    let p = ui.painter_at(hr.expand(1.0));
    p.rect_filled(hr, 0.0, t.field);
    if let Some(h) = &cx.hist {
        tone::draw_histogram(&p, hr, &h[ch], hist_color(&chan, &t));
    }
    p.rect_stroke(hr, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    // Input sliders: black, gamma, white.
    let (sr, sresp) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click_and_drag());
    let track = sr.shrink2(vec2(6.0, 0.0));
    let xpos = |val: f32| track.left() + val / 255.0 * track.width();
    let xval = |x: f32| ((x - track.left()) / track.width() * 255.0).clamp(0.0, 255.0);
    let drag_id = cx.mem.with("levels-drag");
    let mut which: Option<u8> = ui.data(|d| d.get_temp(drag_id)).flatten();
    let ctx = ui.ctx().clone();
    let pressed = |r: &egui::Response| r.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_pressed());
    if pressed(&sresp)
        && let Some(pos) = ui.input(|i| i.pointer.press_origin())
    {
        which = nearest(&[xpos(l[0]), xpos(gamma_pos(l[0], l[2], l[1])), xpos(l[2])], pos.x);
    }
    if sresp.is_pointer_button_down_on()
        && let (Some(k), Some(pos)) = (which, ui.input(|i| i.pointer.interact_pos()))
    {
        let x = xval(pos.x);
        let before = l;
        match k {
            0 => l[0] = x.min(l[2] - 2.0).round(),
            2 => l[2] = x.max(l[0] + 2.0).round(),
            _ => l[1] = round(gamma_from_pos(l[0], l[2], x), 0.01),
        }
        changed |= l != before;
    }
    if which.is_some() && !sresp.is_pointer_button_down_on() {
        which = None;
        e.commit = true;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, which));
    let sp = ui.painter_at(sr.expand(6.0));
    handle(&sp, xpos(l[0]), sr.top() + 2.0, Color32::BLACK, t.text_dim);
    handle(&sp, xpos(gamma_pos(l[0], l[2], l[1])), sr.top() + 2.0, Color32::from_gray(128), t.text_dim);
    handle(&sp, xpos(l[2]), sr.top() + 2.0, Color32::WHITE, t.text_dim);
    ui.horizontal(|ui| {
        let fw = ((w - 16.0) / 3.0).min(70.0);
        let (mut nb, mut ng, mut nw) = (l[0], l[1], l[2]);
        let gap = ((w - 3.0 * fw) / 2.0 - 12.0).max(0.0);
        let r0 = widgets::value_field(ui, &mut nb, 0.0..=253.0, "", fw);
        ui.add_space(gap);
        let r1 = widgets::value_field(ui, &mut ng, 0.01..=9.99, "", fw);
        ui.add_space(gap);
        let r2 = widgets::value_field(ui, &mut nw, 2.0..=255.0, "", fw);
        if r0.changed() {
            l[0] = nb.round().min(l[2] - 2.0);
        }
        if r1.changed() {
            l[1] = round(ng.clamp(0.01, 9.99), 0.01);
        }
        if r2.changed() {
            l[2] = nw.round().max(l[0] + 2.0);
        }
        for r in [&r0, &r1, &r2] {
            changed |= r.changed();
            e.add(Edit::of(r));
        }
    });
    // Output levels.
    ui.add_space(6.0);
    label(ui, tl!("Output Levels:"));
    let (ob_r, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
    gradient_bar(ui.painter(), ob_r.shrink2(vec2(6.0, 0.0)), Color32::BLACK, bar_color(&chan, &t), false);
    let (or, oresp) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click_and_drag());
    let otrack = or.shrink2(vec2(6.0, 0.0));
    let oxpos = |val: f32| otrack.left() + val / 255.0 * otrack.width();
    let oval = |x: f32| ((x - otrack.left()) / otrack.width() * 255.0).clamp(0.0, 255.0);
    let odrag_id = cx.mem.with("levels-odrag");
    let mut owhich: Option<u8> = ui.data(|d| d.get_temp(odrag_id)).flatten();
    if pressed(&oresp)
        && let Some(pos) = ui.input(|i| i.pointer.press_origin())
    {
        owhich = nearest(&[oxpos(l[3]), oxpos(l[4])], pos.x);
    }
    if oresp.is_pointer_button_down_on()
        && let (Some(k), Some(pos)) = (owhich, ui.input(|i| i.pointer.interact_pos()))
    {
        let slot = if k == 0 { 3 } else { 4 };
        let nv = oval(pos.x).round();
        changed |= l[slot] != nv;
        l[slot] = nv;
    }
    if owhich.is_some() && !oresp.is_pointer_button_down_on() {
        owhich = None;
        e.commit = true;
    }
    ui.data_mut(|d| d.insert_temp(odrag_id, owhich));
    let op = ui.painter_at(or.expand(6.0));
    handle(&op, oxpos(l[3]), or.top() + 2.0, Color32::BLACK, t.text_dim);
    handle(&op, oxpos(l[4]), or.top() + 2.0, Color32::WHITE, t.text_dim);
    ui.horizontal(|ui| {
        let fw = ((w - 16.0) / 3.0).min(70.0);
        let (mut a, mut c) = (l[3], l[4]);
        let ra = widgets::value_field(ui, &mut a, 0.0..=255.0, "", fw);
        ui.add_space((w - 2.0 * fw - 16.0).max(0.0));
        let rc = widgets::value_field(ui, &mut c, 0.0..=255.0, "", fw);
        if ra.changed() {
            l[3] = a.round();
        }
        if rc.changed() {
            l[4] = c.round();
        }
        for r in [&ra, &rc] {
            changed |= r.changed();
            e.add(Edit::of(r));
        }
    });
    if changed {
        write_levels(v, chan.key, &l);
        e.changed = true;
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Threshold

fn threshold(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width().min(300.0);
    let (hr, _) = ui.allocate_exact_size(vec2(w, 90.0), Sense::hover());
    let hr = hr.shrink2(vec2(6.0, 0.0));
    let p = ui.painter_at(hr.expand(1.0));
    p.rect_filled(hr, 0.0, t.field);
    if let Some(h) = &cx.hist {
        tone::draw_histogram(&p, hr, &h[0], Color32::from_gray(if t.pro { 150 } else { 110 }));
    }
    let level = num(v, "level", 128.0);
    let x = hr.left() + level / 255.0 * hr.width();
    p.line_segment([pos2(x, hr.top()), pos2(x, hr.bottom())], Stroke::new(1.0, t.accent));
    p.rect_stroke(hr, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    ui.add_space(4.0);
    sliders(ui, v, &[("level", tl!("Threshold Level"), 1.0, 255.0, 128.0, "")])
}

// ---------------------------------------------------------------------------------------------
// Hue/Saturation

const RANGE_LABELS: [&str; 6] = ["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas"];

fn hue_color(deg: f32) -> Color32 {
    let rgb = egui::ecolor::Hsva::new(deg.rem_euclid(360.0) / 360.0, 1.0, 1.0, 1.0).to_srgb();
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// The spectrum bar after the adjustment (what each hue becomes), sampled at `n` hues.
fn adjusted_spectrum(v: &Value, n: usize) -> Vec<Color32> {
    let adj = photocraft_engine::commands::adjustment_from_params("hueSaturation", v);
    let rect = photocraft_geom::Rect::new(0, 0, n as i32, 1);
    let px = (0..n)
        .map(|i| {
            let c = egui::ecolor::Hsva::new(i as f32 / (n - 1).max(1) as f32, 1.0, 1.0, 1.0).to_rgb();
            [c[0], c[1], c[2], 1.0]
        })
        .collect();
    let mut buf = photocraft_compose::Buffer { rect, px };
    photocraft_compose::adjust::apply(&adj, &mut buf);
    buf.px.iter().map(|p| color32([p[0], p[1], p[2]])).collect()
}

fn spectrum_bar(p: &egui::Painter, r: Rect, colors: &[Color32]) {
    let n = colors.len().saturating_sub(1).max(1);
    for (i, w) in colors.windows(2).enumerate() {
        let x0 = r.left() + r.width() * i as f32 / n as f32;
        let x1 = r.left() + r.width() * (i + 1) as f32 / n as f32;
        gradient_bar(p, Rect::from_min_max(pos2(x0, r.top()), pos2(x1, r.bottom())), w[0], w[1], false);
    }
}

fn hue_saturation(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let range_id = cx.mem.with("hs-range");
    let mut range: usize = ui.data(|d| d.get_temp(range_id)).unwrap_or(0);
    let colorize = flag(v, "colorize", false);
    if colorize {
        range = 0;
    }
    ui.horizontal(|ui| {
        let opts: Vec<(usize, &str)> = std::iter::once((0, tl!("Master"))).chain(RANGE_LABELS.iter().enumerate().map(|(i, l)| (i + 1, *l))).collect();
        ui.add_enabled_ui(!colorize, |ui| widgets::dropdown(ui, &format!("{range_id:?}"), &mut range, &opts, 120.0));
    });
    ui.data_mut(|d| d.insert_temp(range_id, range));
    let mut e = Edit::default();
    let hue_grad = widgets::hue_stops();
    if range == 0 {
        let (hlo, hhi, slo) = if colorize { (0.0, 360.0, 0.0) } else { (-180.0, 180.0, -100.0) };
        let mut h = num(v, "hue", 0.0).clamp(hlo, hhi);
        let r = widgets::slider_row(ui, tl!("Hue"), &mut h, hlo..=hhi, "°", Some(&hue_grad));
        if r.changed() {
            v["hue"] = json!(h.round());
        }
        e.add(Edit::of(&r));
        let mut s = num(v, "saturation", 0.0).clamp(slo, 100.0);
        let r = widgets::slider_row(ui, tl!("Saturation"), &mut s, slo..=100.0, "%", None);
        if r.changed() {
            v["saturation"] = json!(s.round());
        }
        e.add(Edit::of(&r));
        e.add(sliders(ui, v, &[("lightness", tl!("Lightness"), -100.0, 100.0, 0.0, "%")]));
    } else {
        let key = HUE_RANGES[range - 1];
        if !v.get(key).is_some_and(Value::is_object) {
            v[key] = json!({});
        }
        let mut o = v[key].clone();
        let mut sub = Edit::default();
        let mut h = num(&o, "hue", 0.0);
        let r = widgets::slider_row(ui, tl!("Hue"), &mut h, -180.0..=180.0, "°", Some(&hue_grad));
        if r.changed() {
            o["hue"] = json!(h.round());
        }
        sub.add(Edit::of(&r));
        sub.add(sliders(ui, &mut o, &[("saturation", tl!("Saturation"), -100.0, 100.0, 0.0, "%"), ("lightness", tl!("Lightness"), -100.0, 100.0, 0.0, "%")]));
        if sub.changed {
            v[key] = o;
        }
        e.add(sub);
    }
    let mut c = colorize;
    if widgets::checkbox(ui, &mut c, tl!("Colorize")).changed() {
        v["colorize"] = json!(c);
        let h = num(v, "hue", 0.0);
        v["hue"] = json!(if c {
            h.rem_euclid(360.0)
        } else if h > 180.0 {
            h - 360.0
        } else {
            h
        });
        if c {
            v["saturation"] = json!(num(v, "saturation", 25.0).max(0.0).max(25.0));
        }
        e.add(Edit::discrete(true));
    }
    // Spectrum bars: input hues above, adjusted below; the selected range's sliders between.
    ui.add_space(6.0);
    let w = ui.available_width().min(300.0);
    let (bars, bresp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click_and_drag());
    let top = Rect::from_min_size(bars.min, vec2(w, 10.0));
    let bottom = Rect::from_min_size(pos2(bars.left(), bars.bottom() - 10.0), vec2(w, 10.0));
    let p = ui.painter_at(bars.expand(4.0));
    let input: Vec<Color32> = (0..=36).map(|i| hue_color(i as f32 * 10.0)).collect();
    spectrum_bar(&p, top, &input);
    spectrum_bar(&p, bottom, &adjusted_spectrum(v, 37));
    if range > 0 {
        let key = HUE_RANGES[range - 1];
        let neutral = photocraft_doc::adjust::HueRange::neutral(range - 1).bounds;
        let mut b = nums(&v[key], "range", neutral);
        let xof = |deg: f32| bars.left() + deg.rem_euclid(360.0) / 360.0 * w;
        let deg_of = |x: f32| ((x - bars.left()) / w * 360.0).clamp(0.0, 360.0);
        let mid = bars.center().y;
        // Range band (fully affected) and fall-off.
        let band = |a: f32, z: f32, color: Color32| {
            let (xa, xz) = (xof(a), xof(z));
            if xa <= xz {
                p.rect_filled(Rect::from_min_max(pos2(xa, mid - 3.0), pos2(xz, mid + 3.0)), 0.0, color);
            } else {
                p.rect_filled(Rect::from_min_max(pos2(xa, mid - 3.0), pos2(bars.right(), mid + 3.0)), 0.0, color);
                p.rect_filled(Rect::from_min_max(pos2(bars.left(), mid - 3.0), pos2(xz, mid + 3.0)), 0.0, color);
            }
        };
        band(b[0], b[3], t.text_faint);
        band(b[1], b[2], t.text_dim);
        for (k, d) in b.iter().enumerate() {
            let x = xof(*d);
            let fill = if k == 1 || k == 2 { t.text } else { t.field };
            p.rect_filled(Rect::from_center_size(pos2(x, mid), vec2(5.0, 12.0)), 1.0, fill);
            p.rect_stroke(Rect::from_center_size(pos2(x, mid), vec2(5.0, 12.0)), 1.0, Stroke::new(1.0, t.text_dim), StrokeKind::Inside);
        }
        let drag_id = cx.mem.with("hs-range-drag");
        let mut which: Option<u8> = ui.data(|d| d.get_temp(drag_id)).flatten();
        if bresp.is_pointer_button_down_on()
            && ui.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        {
            which = nearest(&b.map(xof), pos.x).filter(|k| (xof(b[*k as usize]) - pos.x).abs() < 10.0);
        }
        if bresp.is_pointer_button_down_on()
            && let (Some(k), Some(pos)) = (which, ui.input(|i| i.pointer.interact_pos()))
        {
            let k = k as usize;
            let deg = deg_of(pos.x);
            let before = b;
            if k == 0 {
                let delta = (deg - b[0] + 180.0).rem_euclid(360.0) - 180.0;
                b[0] = (b[0] + delta).min(b[1]).max(b[1] - 180.0);
            } else {
                let rel = (deg - b[0]).rem_euclid(360.0);
                let hi = if k == 3 { b[0] + 359.0 } else { b[k + 1] };
                b[k] = (b[0] + rel).clamp(b[k - 1], hi);
            }
            if b != before {
                v[key]["range"] = json!(b.map(|x| x.round()));
                e.changed = true;
            }
        }
        if which.is_some() && !bresp.is_pointer_button_down_on() {
            which = None;
            e.commit = true;
        }
        ui.data_mut(|d| d.insert_temp(drag_id, which));
        let lab =
            format!("{:.0}° / {:.0}°  ·  {:.0}° \\ {:.0}°", b[0].rem_euclid(360.0), b[1].rem_euclid(360.0), b[2].rem_euclid(360.0), b[3].rem_euclid(360.0));
        ui.label(RichText::new(lab).font(crate::theme::mono(11.0)).color(t.text_faint));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Color Balance

fn color_balance(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let tone_id = cx.mem.with("cb-tone");
    let mut tone_ix: usize = ui.data(|d| d.get_temp(tone_id)).unwrap_or(1);
    ui.horizontal(|ui| {
        label(ui, tl!("Tone:"));
        for (i, l) in [tl!("Shadows"), tl!("Midtones"), tl!("Highlights")].iter().enumerate() {
            if ui.radio(tone_ix == i, *l).clicked() {
                tone_ix = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(tone_id, tone_ix));
    let key = ["shadows", "midtones", "highlights"][tone_ix.min(2)];
    let mut vals = nums(v, key, [0.0; 3]);
    let mut e = Edit::default();
    let rows = [
        (tl!("Cyan  ·  Red"), Color32::from_rgb(0, 190, 210), Color32::from_rgb(225, 40, 40)),
        (tl!("Magenta  ·  Green"), Color32::from_rgb(210, 40, 190), Color32::from_rgb(40, 190, 60)),
        (tl!("Yellow  ·  Blue"), Color32::from_rgb(230, 210, 30), Color32::from_rgb(40, 80, 230)),
    ];
    let mut changed = false;
    for (i, (text, a, b)) in rows.iter().enumerate() {
        let r = gradient_slider(ui, text, &mut vals[i], -100.0..=100.0, "", *a, *b);
        changed |= r.changed;
        e.add(r);
    }
    if changed {
        v[key] = json!(vals.map(f32::round));
    }
    let mut pl = flag(v, "preserveLuminosity", true);
    if widgets::checkbox(ui, &mut pl, tl!("Preserve Luminosity")).changed() {
        v["preserveLuminosity"] = json!(pl);
        e.add(Edit::discrete(true));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Black & White

fn black_white(ui: &mut egui::Ui, v: &mut Value) -> Edit {
    let mut e = Edit::default();
    let colors = [[230, 50, 50], [235, 215, 40], [50, 200, 70], [40, 200, 220], [50, 90, 235], [220, 50, 210]];
    for (i, key) in HUE_RANGES.iter().enumerate() {
        let mut x = num(v, key, adjust_params::BW_DEFAULTS[i]).clamp(-200.0, 300.0);
        let c = colors[i];
        let r = gradient_slider(ui, RANGE_LABELS[i], &mut x, -200.0..=300.0, "%", Color32::BLACK, Color32::from_rgb(c[0], c[1], c[2]));
        if r.changed {
            v[*key] = json!(x.round());
        }
        e.add(r);
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let mut on = flag(v, "tint", false);
        if widgets::checkbox(ui, &mut on, tl!("Tint")).changed() {
            v["tint"] = json!(on);
            e.add(Edit::discrete(true));
        }
        let mut c = rgb_of(v, "tintColor", [0.882, 0.827, 0.702]);
        let r = ui.add_enabled_ui(on, |ui| color_button(ui, &mut c)).inner;
        if r.changed {
            v["tintColor"] = json!(adjust_params::hex(c));
        }
        e.add(r);
        if widgets::secondary_button(ui, tl!("Default"), 0.0).clicked() {
            for (i, key) in HUE_RANGES.iter().enumerate() {
                v[*key] = json!(adjust_params::BW_DEFAULTS[i]);
            }
            e.add(Edit::discrete(true));
        }
    });
    e
}

// ---------------------------------------------------------------------------------------------
// Photo Filter

fn photo_filter(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let mut e = Edit::default();
    let mut c = rgb_of(v, "color", [236.0 / 255.0, 138.0 / 255.0, 0.0]);
    let hex = adjust_params::hex(c);
    let preset = PHOTO_FILTERS.iter().find(|f| adjust_params::hex(f.2.map(|x| f32::from(x) / 255.0)) == hex).map(|f| f.0);
    let mode_id = cx.mem.with("pf-mode");
    let mut use_color: bool = ui.data(|d| d.get_temp(mode_id)).unwrap_or(preset.is_none());
    ui.horizontal(|ui| {
        if ui.radio(!use_color, tl!("Filter:")).clicked() {
            use_color = false;
        }
        let mut sel = preset.unwrap_or("custom").to_string();
        let mut opts: Vec<(String, &str)> = PHOTO_FILTERS.iter().map(|f| (f.0.to_string(), f.1)).collect();
        if preset.is_none() {
            opts.insert(0, ("custom".into(), "Custom"));
        }
        let changed = ui.add_enabled_ui(!use_color, |ui| widgets::dropdown(ui, &format!("{mode_id:?}-filter"), &mut sel, &opts, 170.0)).inner;
        if changed && let Some(f) = PHOTO_FILTERS.iter().find(|f| f.0 == sel) {
            c = f.2.map(|x| f32::from(x) / 255.0);
            v["color"] = json!(adjust_params::hex(c));
            e.add(Edit::discrete(true));
        }
    });
    ui.horizontal(|ui| {
        if ui.radio(use_color, tl!("Color:")).clicked() {
            use_color = true;
        }
        let r = ui.add_enabled_ui(use_color, |ui| color_button(ui, &mut c)).inner;
        if r.changed {
            v["color"] = json!(adjust_params::hex(c));
        }
        e.add(r);
    });
    ui.data_mut(|d| d.insert_temp(mode_id, use_color));
    e.add(sliders(ui, v, &[("density", tl!("Density"), 0.0, 100.0, 25.0, "%")]));
    let mut pl = flag(v, "preserveLuminosity", true);
    if widgets::checkbox(ui, &mut pl, tl!("Preserve Luminosity")).changed() {
        v["preserveLuminosity"] = json!(pl);
        e.add(Edit::discrete(true));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Channel Mixer

fn channel_mixer(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mono = flag(v, "monochrome", false);
    let out_id = cx.mem.with("cm-out");
    let mut out: usize = ui.data(|d| d.get_temp(out_id)).unwrap_or(0);
    let outs: &[(usize, &str)] = if mono { &[(0, tl!("Gray"))] } else { &[(0, tl!("Red")), (1, tl!("Green")), (2, tl!("Blue"))] };
    if out >= outs.len() {
        out = 0;
    }
    ui.horizontal(|ui| {
        label(ui, tl!("Output Channel:"));
        widgets::dropdown(ui, &format!("{out_id:?}"), &mut out, outs, 110.0);
    });
    ui.data_mut(|d| d.insert_temp(out_id, out));
    let key = if mono { "gray" } else { ["red", "green", "blue"][out] };
    let identity: [f32; 4] = if mono { [40.0, 40.0, 20.0, 0.0] } else { std::array::from_fn(|i| if i == out { 100.0 } else { 0.0 }) };
    let mut row = nums(v, key, if mono { nums(v, "red", identity) } else { identity });
    let mut e = Edit::default();
    let mut changed = false;
    for (i, (text, c)) in [(tl!("Red"), [225, 50, 50]), (tl!("Green"), [50, 190, 70]), (tl!("Blue"), [60, 100, 235])].iter().enumerate() {
        let r = gradient_slider(ui, text, &mut row[i], -200.0..=200.0, "%", Color32::BLACK, Color32::from_rgb(c[0], c[1], c[2]));
        changed |= r.changed;
        e.add(r);
    }
    let total = row[0] + row[1] + row[2];
    ui.horizontal(|ui| {
        label(ui, tl!("Total:"));
        let warn = total > 100.0;
        ui.label(RichText::new(format!("{total:+.0}%")).font(crate::theme::mono(12.0)).color(if warn { t.warning } else { t.text }));
    });
    let r = widgets::slider_row(ui, tl!("Constant"), &mut row[3], -200.0..=200.0, "%", None);
    changed |= r.changed();
    e.add(Edit::of(&r));
    if changed {
        v[key] = json!(row.map(f32::round));
    }
    let mut m = mono;
    if widgets::checkbox(ui, &mut m, tl!("Monochrome")).changed() {
        v["monochrome"] = json!(m);
        if m {
            v["gray"] = json!([40.0, 40.0, 20.0, 0.0]);
        } else if let Some(o) = v.as_object_mut() {
            o.remove("gray");
        }
        e.add(Edit::discrete(true));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// Gradient Map

type Stop = (f32, [f32; 3]);

fn read_stops(v: &Value) -> Vec<Stop> {
    let mut s: Vec<Stop> = v
        .get("stops")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let p = s.as_array()?;
                    let t = p.first()?.as_f64()? as f32;
                    let c = rgb_of(&json!({"c": p.get(1)?.clone()}), "c", [0.0; 3]);
                    Some((t.clamp(0.0, 1.0), c))
                })
                .collect()
        })
        .unwrap_or_default();
    if s.len() < 2 {
        s = vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])];
    }
    s
}

fn write_stops(v: &mut Value, s: &[Stop]) {
    let mut s = s.to_vec();
    s.sort_by(|a, b| a.0.total_cmp(&b.0));
    v["stops"] = json!(s.iter().map(|(t, c)| json!([round(*t, 0.001), adjust_params::hex(*c)])).collect::<Vec<_>>());
}

fn sample_stops(s: &[Stop], t: f32) -> [f32; 3] {
    let mut sorted = s.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(first) = sorted.first() else { return [t; 3] };
    if t <= first.0 {
        return first.1;
    }
    for w in sorted.windows(2) {
        if t <= w[1].0 {
            let k = if w[1].0 > w[0].0 { (t - w[0].0) / (w[1].0 - w[0].0) } else { 0.0 };
            return std::array::from_fn(|i| w[0].1[i] + (w[1].1[i] - w[0].1[i]) * k);
        }
    }
    sorted.last().map_or([t; 3], |l| l.1)
}

fn gradient_map(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mut e = Edit::default();
    let mut stops = read_stops(v);
    let presets: [(&str, Vec<Stop>); 5] = [
        (tl!("Black, White"), vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])]),
        (tl!("Foreground to Background"), vec![(0.0, cx.swatches[0]), (1.0, cx.swatches[1])]),
        (tl!("Violet, Orange"), vec![(0.0, [0.161, 0.039, 0.349]), (1.0, [1.0, 0.486, 0.0])]),
        (tl!("Blue, Red, Yellow"), vec![(0.0, [0.039, 0.0, 0.698]), (0.5, [1.0, 0.0, 0.0]), (1.0, [1.0, 0.988, 0.0])]),
        (tl!("Copper"), vec![(0.0, [0.592, 0.275, 0.102]), (0.4, [0.984, 0.847, 0.773]), (0.7, [0.424, 0.180, 0.086]), (1.0, [0.937, 0.859, 0.804])]),
    ];
    let mut preset = usize::MAX;
    ui.horizontal(|ui| {
        label(ui, tl!("Preset:"));
        let mut opts: Vec<(usize, &str)> = vec![(usize::MAX, tl!("Custom"))];
        opts.extend(presets.iter().enumerate().map(|(i, p)| (i, p.0)));
        if widgets::dropdown(ui, &format!("{:?}-gm-preset", cx.mem), &mut preset, &opts, 190.0)
            && let Some(p) = presets.get(preset)
        {
            stops = p.1.clone();
            write_stops(v, &stops);
            e.add(Edit::discrete(true));
        }
    });
    // The gradient.
    let w = ui.available_width().min(300.0);
    let (r, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    let reverse = flag(v, "reverse", false);
    let cols: Vec<Color32> = (0..=48)
        .map(|i| {
            let x = i as f32 / 48.0;
            color32(sample_stops(&stops, if reverse { 1.0 - x } else { x }))
        })
        .collect();
    spectrum_bar(ui.painter(), r, &cols);
    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
    ui.add_space(4.0);
    // Stops: colour, location, remove.
    let mut remove = None;
    let mut changed = false;
    for (i, s) in stops.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let r = color_button(ui, &mut s.1);
            changed |= r.changed;
            e.add(r);
            let mut loc = s.0 * 100.0;
            let rl = widgets::value_field(ui, &mut loc, 0.0..=100.0, "%", 70.0);
            if rl.changed() {
                s.0 = (loc / 100.0).clamp(0.0, 1.0);
                changed = true;
            }
            e.add(Edit::of(&rl));
            if crate::icons::button(ui, "x", 20.0, false, tl!("Remove stop")).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove
        && stops.len() > 2
    {
        stops.remove(i);
        changed = true;
        e.add(Edit::discrete(true));
    }
    if stops.len() < 64 && widgets::secondary_button(ui, tl!("Add Stop"), 0.0).clicked() {
        // Halfway along the widest gap, in the gradient's colour there.
        let mut sorted = stops.clone();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let at = sorted.windows(2).max_by(|a, b| (a[1].0 - a[0].0).total_cmp(&(b[1].0 - b[0].0))).map_or(0.5, |w| (w[0].0 + w[1].0) / 2.0);
        stops.push((at, sample_stops(&stops, at)));
        changed = true;
        e.add(Edit::discrete(true));
    }
    if changed {
        write_stops(v, &stops);
        e.changed = true;
    }
    ui.horizontal(|ui| {
        for (key, text) in [("reverse", tl!("Reverse")), ("dither", tl!("Dither"))] {
            let mut b = flag(v, key, false);
            if widgets::checkbox(ui, &mut b, text).changed() {
                v[key] = json!(b);
                e.add(Edit::discrete(true));
            }
        }
    });
    e
}

// ---------------------------------------------------------------------------------------------
// Hosts

/// Whether a document of this mode edits Levels/Curves through a single Gray channel.
pub fn is_gray(mode: photocraft_doc::ColorMode) -> bool {
    matches!(mode, photocraft_doc::ColorMode::Grayscale | photocraft_doc::ColorMode::Duotone | photocraft_doc::ColorMode::Bitmap)
}

pub fn swatches(app: &PhotocraftApp) -> [[f32; 3]; 2] {
    let c = |x: [f32; 4]| [x[0], x[1], x[2]];
    [c(app.session.tools.foreground), c(app.session.tools.background)]
}

/// The Properties-panel editor of an adjustment layer: previews live (`app.live_adjust`) while a
/// control moves and commits one `layer.setAdjustment` per gesture.
pub fn layer_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId, adj: &Adjustment) {
    let t = Tokens::get(ui.ctx());
    let kind = photocraft_engine::commands::adjustment_kind(adj);
    match adj {
        Adjustment::SelectiveColor { .. } => return crate::adjust_ui::selective_color_editor(app, ui, id, adj),
        Adjustment::ColorLookup { .. } => return crate::adjust_ui::color_lookup_editor(app, ui, id, adj),
        Adjustment::Invert => {
            ui.label(RichText::new(tl!("Invert has no settings.")).color(t.text_faint));
            return;
        }
        Adjustment::Unsupported { psd_key, .. } => {
            ui.label(RichText::new(format!("This adjustment ({psd_key}) is kept as imported and can't be edited yet.")).color(t.text_faint));
            return;
        }
        _ => {}
    }
    let committed = adjust_params::to_params(adj);
    let mut values = match &app.live_adjust {
        Some((l, v)) if *l == id => v.clone(),
        _ => committed.clone(),
    };
    let gray = app.session.active().is_some_and(|s| is_gray(s.doc.mode));
    let hist = needs_histogram(kind).then(|| tone::histograms(app, HistSource::BelowLayer(id), space_of(&values)));
    let cx = EditorCx { mem: egui::Id::new(("adjust-layer", id.0)), hist, gray, swatches: swatches(app) };
    let e = editor(ui, kind, &mut values, &cx);
    if e.changed {
        app.live_adjust = Some((id, values.clone()));
    }
    if e.commit {
        if values != committed {
            let rev = app.session.active().map(|s| s.revision);
            let mut p = values;
            p["layer"] = json!(id.0);
            if let Err(err) = app.run("layer.setAdjustment", p) {
                app.ui.status = err;
            }
            tone::keep_after_commit(app, id, rev);
        }
        app.live_adjust = None;
    }
}

#[cfg(test)]
mod tests {
    use super::curve_edit::*;
    use super::*;

    fn line() -> Vec<[f32; 2]> {
        vec![[0.0, 0.0], [255.0, 255.0]]
    }

    #[test]
    fn insert_keeps_order_dedupes_and_respects_the_limit() {
        let mut p = line();
        assert_eq!(insert(&mut p, [128.0, 150.0]), Some(1));
        assert_eq!(insert(&mut p, [64.0, 40.0]), Some(1));
        assert_eq!(p, vec![[0.0, 0.0], [64.0, 40.0], [128.0, 150.0], [255.0, 255.0]]);
        // A click on an existing input selects that point instead of stacking a second one.
        assert_eq!(insert(&mut p, [129.0, 10.0]), Some(2));
        assert_eq!(p.len(), 4);
        let mut full: Vec<[f32; 2]> = (0..MAX_POINTS).map(|i| [i as f32 * 10.0, 0.0]).collect();
        assert_eq!(insert(&mut full, [255.0, 0.0]), None);
        assert_eq!(insert(&mut p, [-50.0, 900.0]), Some(0), "clamped onto the black point");
    }

    #[test]
    fn endpoints_and_the_last_two_points_stay() {
        let mut p = vec![[0.0, 0.0], [100.0, 120.0], [255.0, 255.0]];
        assert!(!delete(&mut p, 0) && !delete(&mut p, 2) && !delete(&mut p, 7));
        assert!(delete(&mut p, 1));
        assert_eq!(p, line());
        assert!(!delete(&mut p, 1) && !delete(&mut p, 0));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn moves_stay_between_neighbours() {
        let mut p = vec![[0.0, 0.0], [100.0, 120.0], [200.0, 180.0], [255.0, 255.0]];
        assert!(move_to(&mut p, 1, [250.0, 300.0]));
        assert_eq!(p[1], [199.0, 255.0]);
        move_to(&mut p, 0, [150.0, 10.0]);
        assert_eq!(p[0], [150.0, 10.0]);
        move_to(&mut p, 3, [10.0, 5.0]);
        assert_eq!(p[3], [201.0, 5.0]);
        assert!(!move_to(&mut p, 9, [0.0, 0.0]));
    }

    #[test]
    fn hit_test_picks_the_nearest_point_in_radius() {
        let p = vec![[0.0, 0.0], [100.0, 100.0], [110.0, 110.0], [255.0, 255.0]];
        let to_scr = |q: [f32; 2]| Pos2::new(q[0], 255.0 - q[1]);
        assert_eq!(hit(&p, to_scr, Pos2::new(108.0, 146.0), 9.0), Some(2));
        assert_eq!(hit(&p, to_scr, Pos2::new(50.0, 50.0), 9.0), None);
    }

    #[test]
    fn curve_endpoint_handles_fit_inside_the_allocated_area() {
        for side in [120.0, 200.0, 300.0] {
            let full = Rect::from_min_size(pos2(100.0, 50.0), vec2(side, side + 14.0));
            let graph = curve_graph(full, side);
            let to_scr = |q: [f32; 2]| pos2(graph.left() + q[0] / 255.0 * graph.width(), graph.bottom() - q[1] / 255.0 * graph.height());
            for q in [[0.0, 0.0], [255.0, 255.0]] {
                let r = Rect::from_center_size(to_scr(q), vec2(CURVE_HANDLE_SIZE, CURVE_HANDLE_SIZE)).expand(1.0);
                assert!(r.left() >= full.left() && r.right() <= full.right());
                assert!(r.top() >= full.top() && r.bottom() <= full.bottom());
            }
        }
    }

    #[test]
    fn levels_helpers() {
        for g in [0.3f32, 1.0, 1.8, 4.0] {
            let x = gamma_pos(20.0, 230.0, g);
            assert!((gamma_from_pos(20.0, 230.0, x) - g).abs() < 1e-3, "{g}");
        }
        // Moving the grey slider left brightens (gamma > 1), as in Photoshop.
        assert!(gamma_from_pos(0.0, 255.0, 80.0) > 1.0);
        let mut h = [0u32; 256];
        h[10] = 1000;
        h[200] = 1000;
        assert_eq!(auto_levels(&h), (10.0, 200.0));
        assert_eq!(auto_levels(&[0; 256]), (253.0, 255.0), "an empty histogram is no panic");
    }
}

#[cfg(test)]
#[path = "adjust_editors_tests.rs"]
mod ui_tests;

//! Document canvas: display texture cache, view transform, tool input, extra document windows.
//!
//! Rendering is CPU (`photocraft-compose`) for now, uploaded into an egui texture. Brush strokes
//! update only their damage rectangle (`set_partial`). The wgpu compositor (M5) will replace this
//! with direct GPU rendering behind the same `CanvasCache` interface.

use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, TextureOptions, Vec2, pos2, vec2};
use photocraft_doc::{Document, LayerContent};
use photocraft_geom::Rect as DRect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::{Tool, View};

/// Largest texture side we upload; bigger documents display downsampled until the GPU path lands.
pub const MAX_TEXTURE: u32 = 4096;

pub struct CanvasCache {
    pub revision: u64,
    pub texture: Option<egui::TextureHandle>,
    /// Display pixels per document pixel of the cached image (≤ 1).
    pub scale: f32,
    /// Hash of the live-adjust preview used for this render (0 = none).
    pub preview_key: u64,
    /// The current image lives in the GPU canvas (`gpu_canvas`) rather than `texture`.
    pub on_gpu: bool,
    /// Revision and preview key `texture` was rendered at. Tracked apart from the GPU canvas's
    /// `revision`, so the Navigator never shows a stale CPU image after GPU-path edits.
    pub tex_revision: u64,
    pub tex_preview_key: u64,
}

/// An in-progress pointer gesture on the canvas.
#[derive(Clone, Debug)]
pub struct Drag {
    pub tool: Tool,
    pub start: [f64; 2],
    pub points: Vec<[f64; 3]>,
    pub modifiers: egui::Modifiers,
    /// A Brush/Eraser stroke that erases: the Eraser, or a right-button Brush drag with
    /// Preferences › Tools › Right-click with painting tools set to Erase.
    pub erase: bool,
    /// ⇧ constraint of a painting stroke (stroke_constraint.rs).
    pub constrain: Option<crate::stroke_constraint::Axis>,
    /// Modifiers held now (updated on every pointer event of the gesture).
    pub live: egui::Modifiers,
    /// Which of the modifiers held at the press have been released since.
    pub released: egui::Modifiers,
    /// The reposition key (Space) is held: pointer moves move the whole gesture instead of
    /// sizing it (marquees, lasso, shapes; `hold_keys`).
    pub reposition: bool,
}

impl Drag {
    pub fn new(tool: Tool, start: [f64; 2], points: Vec<[f64; 3]>, modifiers: egui::Modifiers, erase: bool) -> Self {
        Self { tool, start, points, modifiers, erase, constrain: None, live: modifiers, released: egui::Modifiers::NONE, reposition: false }
    }

    /// Reposition: move the start and every point so the last one lands on `to` (same size).
    pub fn shift_to(&mut self, to: [f64; 2]) {
        let last = self.points.last().map_or(self.start, |p| [p[0], p[1]]);
        let (dx, dy) = (to[0] - last[0], to[1] - last[1]);
        if !(dx.is_finite() && dy.is_finite()) {
            return;
        }
        self.start = [self.start[0] + dx, self.start[1] + dy];
        for p in &mut self.points {
            p[0] += dx;
            p[1] += dy;
        }
        if self.points.is_empty() {
            self.points.push([to[0], to[1], 1.0]);
        }
    }

    /// Record the modifiers of a pointer event.
    fn track(&mut self, mods: egui::Modifiers) {
        self.live = mods;
        self.released.shift |= !mods.shift;
        self.released.alt |= !mods.alt;
    }

    /// ⇧ (square / circle) and ⌥ (from the centre) for a marquee drag. A modifier held at the press
    /// picks the selection mode instead (add / subtract, #188) until it is released and pressed
    /// again, as in Photoshop.
    pub fn marquee_mods(&self) -> (bool, bool) {
        let fresh = |now: bool, at_press: bool, released: bool| now && (!at_press || released);
        (fresh(self.live.shift, self.modifiers.shift, self.released.shift), fresh(self.live.alt, self.modifiers.alt, self.released.alt))
    }
}

/// The marquee dragged from `d.start` to `end` as two document-space corners: the options-bar
/// style (fixed ratio / fixed size), ⇧ for a square or circle, ⌥ to draw from the centre.
pub fn marquee_corners(o: &crate::state::ToolOptions, d: &Drag, end: [f64; 2]) -> ([f64; 2], [f64; 2]) {
    let (shift, alt) = d.marquee_mods();
    let e = crate::chrome_ui::marquee_end(&o.marquee_style, o.marquee_width as f64, o.marquee_height as f64, shift, d.start, end);
    if !alt {
        return (d.start, e);
    }
    let (dx, dy) = (e[0] - d.start[0], e[1] - d.start[1]);
    // A fixed size is centred on the press point; otherwise the drag is the half extent.
    let (hx, hy) = if o.marquee_style == "fixedSize" { (dx / 2.0, dy / 2.0) } else { (dx, dy) };
    ([d.start[0] - hx, d.start[1] - hy], [d.start[0] + hx, d.start[1] + hy])
}

/// The pixel rectangle `[x0, y0, x1, y1]` a marquee between two corners selects.
pub fn marquee_px(a: [f64; 2], b: [f64; 2]) -> [f64; 4] {
    [a[0].min(b[0]).floor(), a[1].min(b[1]).floor(), a[0].max(b[0]).ceil(), a[1].max(b[1]).ceil()]
}

/// The size readout shown beside the cursor while dragging a marquee: the width and height values.
pub fn marquee_readout(r: [f64; 4]) -> [String; 2] {
    [format!("{} px", r[2] - r[0]), format!("{} px", r[3] - r[1])]
}

/// Draw the marquee size readout below-right of the cursor (kept on screen), like Photoshop's:
/// two rows, `W:` / `H:` labels on the left and the values right-aligned.
fn draw_marquee_readout(ctx: &egui::Context, cursor: Pos2, values: [String; 2]) {
    draw_readout(ctx, "marquee-readout", cursor, ["W:", "H:"], values);
}

/// A two-row readout beside the pointer (labels left, values right-aligned), above everything.
pub(crate) fn draw_readout(ctx: &egui::Context, id: &str, cursor: Pos2, labels: [&str; 2], values: [String; 2]) {
    let t = crate::theme::Tokens::get(ctx);
    let font = egui::FontId::proportional(11.5);
    let width = |text: &str| ctx.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font.clone(), t.text).size().x);
    let (lw, vw) = (labels.map(width), [width(&values[0]), width(&values[1])]);
    let (label_col, value_col) = (lw[0].max(lw[1]), vw[0].max(vw[1]));
    egui::Area::new(egui::Id::new(id)).order(egui::Order::Tooltip).fixed_pos(cursor + vec2(16.0, 18.0)).interactable(false).constrain(true).show(ctx, |ui| {
        egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius_sm).inner_margin(egui::Margin::symmetric(7, 4)).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
                for (i, value) in values.into_iter().enumerate() {
                    // Labels left, values right-aligned on one edge, sharing a baseline.
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(labels[i]).font(font.clone()).color(t.text_dim)).extend());
                        ui.add_space(label_col - lw[i] + 12.0 + value_col - vw[i]);
                        ui.add(egui::Label::new(egui::RichText::new(value).font(font.clone()).color(t.text)).extend());
                    });
                }
            },
        );
    });
}

/// A Brush/Eraser stroke shown while it is drawn: the engine renders the real dabs onto a copy of
/// the document, and the canvas redraws only what each step changed.
pub(crate) struct LiveStroke {
    stroke: photocraft_engine::brush_cmds::LiveStroke,
    doc: photocraft_doc::DocId,
    revision: u64,
    /// Preview key of the stroke; step `n` displays as `key + n`.
    key: u64,
    /// Damage of each step (step 0 = the document before the stroke).
    damage: Vec<DRect>,
    /// Drag points rendered so far.
    fed: usize,
}

impl LiveStroke {
    fn display_key(&self) -> u64 {
        self.key + self.damage.len() as u64
    }

    /// What changed since the canvas showed preview `key` (0 = the document before the stroke).
    fn since(&self, key: u64) -> Option<DRect> {
        let seen = if key == 0 { 0 } else { usize::try_from(key.checked_sub(self.key)?).ok()? };
        Some(self.damage.get(seen..)?.iter().fold(DRect::EMPTY, |a, r| a.union(r)))
    }
}

/// The live stroke on document `idx`, while it is current.
fn live_stroke(app: &PhotocraftApp, idx: usize) -> Option<&LiveStroke> {
    let st = app.session.documents().get(idx)?;
    app.live_stroke.as_ref().filter(|l| app.drag.is_some() && l.doc == st.doc.id && l.revision == st.revision)
}

/// `paint.stroke` params for a Brush/Eraser drag (shared by the live preview and the commit). The
/// stroke smoothing is the session brush's (the options bar's Smoothing %).
fn stroke_params(app: &PhotocraftApp, tool: Tool, erase: bool, points: &[Vec<f64>]) -> serde_json::Value {
    let mut p = json!({ "points": points, "erase": erase, "zoom": app.current_zoom(), "target": paint_target(app) });
    if tool == Tool::Pencil {
        p["autoErase"] = json!(app.ui.tool_options.pencil_auto_erase);
    }
    p
}

/// Tools whose strokes the engine renders while they are drawn (`LiveStroke`).
pub(crate) fn strokes_live(tool: Tool) -> bool {
    matches!(tool, Tool::Brush | Tool::Pencil | Tool::Eraser)
}

/// The command a live-stroking tool commits: the Pencil's `paint.pencil`, else `paint.stroke`.
pub(crate) fn stroke_command(tool: Tool) -> &'static str {
    if tool == Tool::Pencil { "paint.pencil" } else { "paint.stroke" }
}

/// The Pencil's cursor at `doc` (document pixels): the whole-pixel square its dab fills
/// (`paint::grid_square`), in screen points with its edges on physical pixels (`ppp` = pixels
/// per point), so it lines up with the pixel grid at any zoom.
pub(crate) fn pencil_cursor_rect(xf: &ViewXform, doc: [f64; 2], size: f32, ppp: f32) -> Rect {
    let [x0, y0, x1, y1] = photocraft_engine::paint::grid_square(doc[0], doc[1], size);
    let r = Rect::from_two_pos(xf.to_screen(x0 as f32, y0 as f32), xf.to_screen(x1 as f32, y1 as f32));
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(snap(r.min.x), snap(r.min.y)), pos2(snap(r.max.x), snap(r.max.y)))
}

/// Whether a brush-tip circle draws its centre mark.
/// Clone Stamp and Healing Brush stay an empty circle until Option is held, the source-point
/// cursor. Other brushes follow the cursor preference, the large-tip mark, or the Background Eraser.
fn brush_tip_centre(tool: Tool, option: bool, show_crosshair: bool, radius: f32) -> bool {
    if tool == Tool::QuickSelection {
        return false;
    }
    if matches!(tool, Tool::CloneStamp | Tool::Healing) {
        return option || show_crosshair;
    }
    show_crosshair || radius > 6.0 || tool == Tool::BackgroundEraser
}

fn begin_live_stroke(app: &PhotocraftApp) -> Option<LiveStroke> {
    static STROKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let st = app.session.active()?;
    let d = app.drag.as_ref()?;
    let p = stroke_params(app, d.tool, d.erase, &app.stylus.stroke_points(&d.points));
    let stroke = photocraft_engine::brush_cmds::LiveStroke::begin_with(&app.session, stroke_command(d.tool), &p).ok()?;
    let n = STROKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xff_ffff;
    let damage = vec![stroke.bounds()];
    Some(LiveStroke { stroke, doc: st.doc.id, revision: st.revision, key: (1 << 44) | (n << 20), damage, fed: d.points.len() })
}

/// Render the drag points the live stroke hasn't seen yet, with the pen pressure, tilt and
/// rotation the commit's `paint.stroke` gets for them.
fn feed_live_stroke(app: &mut PhotocraftApp) {
    // A batched move replays several samples in one frame and calls this once at the end, so the
    // per-sample calls from `tool_event` are skipped (see `canvas_view`).
    if app.defer_live_stroke {
        return;
    }
    let (Some(l), Some(d)) = (app.live_stroke.as_mut(), app.drag.as_ref()) else { return };
    let pose = &app.stylus.stroke;
    let pts: Vec<_> = (l.fed..d.points.len())
        .filter_map(|i| {
            let p = d.points.get(i)?;
            let t = pose.get(i).or(pose.last()).copied().unwrap_or_default();
            let mut sp = photocraft_engine::paint::StrokePoint::new(p[0], p[1], p[2] as f32);
            (sp.tilt_x, sp.tilt_y, sp.rotation) = (t[0], t[1], t[2]);
            Some(sp)
        })
        .collect();
    if pts.is_empty() {
        return;
    }
    l.fed = d.points.len();
    match l.stroke.push(&pts) {
        Ok(r) => l.damage.push(r),
        Err(_) => app.live_stroke = None,
    }
}

/// Tools whose gesture follows a freehand path (a polyline of the input points), so every pointer
/// sample the OS delivered improves the result. Other tools are driven by the pointer's latest
/// position (endpoints, anchors, guides), so feeding them a whole frame's moves only repeats work.
pub(crate) fn freehand_tool(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush
            | Tool::Pencil
            | Tool::Eraser
            | Tool::BackgroundEraser
            | Tool::HistoryBrush
            | Tool::SpotHealing
            | Tool::Healing
            | Tool::CloneStamp
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
            | Tool::Lasso
            | Tool::QuickSelection
    )
}

/// The pointer moves this frame delivered while `button` was held, in order. `down_at_start` is
/// whether the button was already down when the frame began. A move before a press or after a
/// release in the same frame is dropped, so a gesture never picks up input from outside its own
/// press..release interval.
fn pointer_moves(events: &[egui::Event], button: egui::PointerButton, down_at_start: bool) -> Vec<Pos2> {
    let mut down = down_at_start;
    let mut out = Vec::new();
    for e in events {
        match e {
            egui::Event::PointerButton { button: b, pressed, .. } if *b == button => down = *pressed,
            egui::Event::PointerMoved(p) if down => out.push(*p),
            _ => {}
        }
    }
    out
}

/// Abstract tool event, produced by the mouse or by automation (`ui.pointer`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolEvent {
    Down { x: f64, y: f64, pressure: f32 },
    Move { x: f64, y: f64, pressure: f32 },
    Up { x: f64, y: f64 },
}

/// Document ↔ screen mapping for a canvas rect and a view.
#[derive(Clone, Copy, Debug)]
pub struct ViewXform {
    pub rect: Rect,
    pub zoom: f32,
    pub center: [f32; 2],
    /// View › Flip Horizontal: the view is mirrored left-right (the document is not).
    pub flip: bool,
}

impl ViewXform {
    pub fn to_screen(&self, x: f32, y: f32) -> Pos2 {
        let sx = if self.flip { -1.0 } else { 1.0 };
        self.rect.center() + vec2((x - self.center[0]) * self.zoom * sx, (y - self.center[1]) * self.zoom)
    }
    pub fn to_doc(&self, p: Pos2) -> [f64; 2] {
        let d = (p - self.rect.center()) / self.zoom;
        let dx = if self.flip { -d.x } else { d.x };
        [(dx + self.center[0]) as f64, (d.y + self.center[1]) as f64]
    }
    pub fn doc_rect(&self, r: DRect) -> Rect {
        Rect::from_two_pos(self.to_screen(r.x0 as f32, r.y0 as f32), self.to_screen(r.x1 as f32, r.y1 as f32))
    }
}

pub fn fit_view(view: &mut View, doc: &Document, area: Vec2) {
    let (w, h) = (doc.size.width as f32, doc.size.height as f32);
    let zoom = ((area.x - 40.0) / w).min((area.y - 40.0) / h).clamp(0.01, 1.0);
    view.zoom = zoom;
    view.center = [w / 2.0, h / 2.0];
    view.fit_pending = false;
}

/// Zoom steps like Photoshop's (⌘+ / ⌘−).
pub fn zoom_step(z: f32, dir: i32) -> f32 {
    const STEPS: [f32; 22] =
        [0.01, 0.02, 0.03, 0.05, 0.0667, 0.1, 0.125, 0.1667, 0.25, 0.333, 0.5, 0.6667, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 12.0, 16.0, 32.0];
    if dir > 0 { STEPS.iter().copied().find(|s| *s > z * 1.001).unwrap_or(32.0) } else { STEPS.iter().rev().copied().find(|s| *s < z * 0.999).unwrap_or(0.01) }
}

fn checker(app: &mut PhotocraftApp, ctx: &egui::Context) -> egui::TextureId {
    // Preferences › Transparency & Gamut colours (the texture is rebuilt when they change).
    let [ca, cb] = app.session.prefs().transparency_and_gamut.colors();
    if app.prefs_rt.checker_key != Some([ca, cb]) {
        app.prefs_rt.checker_key = Some([ca, cb]);
        app.checker = None;
    }
    app.checker
        .get_or_insert_with(|| {
            let (a, b) = (Color32::from_rgb(ca[0], ca[1], ca[2]), Color32::from_rgb(cb[0], cb[1], cb[2]));
            let img = egui::ColorImage::new([2, 2], vec![a, b, b, a]);
            let opts = TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                minification: egui::TextureFilter::Nearest,
                wrap_mode: egui::TextureWrapMode::Repeat,
                mipmap_mode: None,
            };
            ctx.load_texture("checker", img, opts)
        })
        .id()
}

fn buffer_to_image(buf: &photocraft_compose::Buffer) -> egui::ColorImage {
    let img = buf.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels)
}

/// A composite as monitor pixels: document profile → monitor profile (nothing to do in the
/// common sRGB-on-sRGB case).
fn display_image(display: Option<&photocraft_engine::display_color::CanvasDisplay>, buf: &photocraft_compose::Buffer) -> egui::ColorImage {
    match display {
        Some(d) if !d.is_identity() => {
            let img = d.to_rgba8(buf);
            egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels)
        }
        _ => buffer_to_image(buf),
    }
}

/// The canvas display of `doc` and a key that changes with it (folded into the canvas caches'
/// preview keys, so a monitor or profile change re-renders).
fn canvas_display(app: &PhotocraftApp, doc: &Document) -> (Option<std::sync::Arc<photocraft_engine::display_color::CanvasDisplay>>, u64) {
    match app.session.color.canvas_display(doc) {
        Ok(d) => {
            let k = d.key;
            (Some(d), k)
        }
        Err(_) => (None, 0),
    }
}

/// The document to render: the committed one, or a clone with the live adjustment preview applied.
fn display_doc(app: &mut PhotocraftApp, idx: usize) -> (std::sync::Arc<Document>, u64) {
    // Puppet / Perspective Warp previews hide the layer they draw on a mesh.
    if let Some(shown) = crate::distort_ui::display_doc(app, idx) {
        return shown;
    }
    // Image › Adjustments dialog: a temporary adjustment layer clipped to the target.
    if let Some(shown) = crate::adjust_preview::display_doc(app, idx) {
        return shown;
    }
    // Gradient tool (live) drag, or a stop dragged in the Properties panel.
    if let Some(shown) = crate::gradient_ui::display_doc(app, idx) {
        return shown;
    }
    // Move tool drag: the moving layers at the pointer.
    if let Some(shown) = crate::move_ui::display_doc(app, idx) {
        return shown;
    }
    let st = &app.session.documents()[idx];
    if let Some(l) = live_stroke(app, idx) {
        return (l.stroke.doc.clone(), l.display_key());
    }
    if let (Some(t), Some(pv)) = (&app.ui.transform, &app.transform_preview)
        && app.session.active_index() == Some(idx)
        && st.doc.layer(photocraft_doc::LayerId(t.layer)).is_some()
    {
        return (pv.doc.clone(), (1 << 40) + pv.session);
    }
    // Layer Style dialog: show its effects live (Cancel just drops the preview).
    let style = app.ui.dialogs.iter().find(|d| d.kind == crate::state::DialogKind::LayerStyle);
    if style.is_none() {
        app.style_preview = None;
    }
    if let Some(d) = style
        && app.session.active_index() == Some(idx)
    {
        let key = (crate::layer_style::preview_hash(&d.fields) ^ st.revision.wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1 << 63;
        if app.style_preview.as_ref().map(|p| p.0) != Some(key) {
            let shown = crate::layer_style::preview_document(&st.doc, &app.session.patterns, &d.fields).map(std::sync::Arc::new);
            app.style_preview = Some((key, shown));
        }
        if let Some((_, Some(doc))) = &app.style_preview {
            return (doc.clone(), key);
        }
    }
    if let Some((layer, params)) = &app.live_adjust
        && let Some(l) = st.doc.layer(*layer)
        && let LayerContent::Adjustment(a) = &l.content
    {
        let kind = photocraft_engine::commands::adjustment_kind(a);
        let preview = photocraft_engine::adjust_params::from_params(kind, params, Some(a), st.doc.mode).unwrap_or_else(|_| a.clone());
        let mut doc = (*st.doc).clone();
        if let Some(lm) = doc.layer_mut(*layer) {
            lm.content = LayerContent::Adjustment(preview);
        }
        let key = 1 + params.to_string().bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        return (std::sync::Arc::new(doc), key);
    }
    // Duotone documents display through their inks.
    if let Some(shown) = photocraft_engine::mode_cmds::display_document(&st.doc) {
        return (std::sync::Arc::new(shown), 1 << 41);
    }
    (st.doc.clone(), 0)
}

/// Longest side of the Navigator panel's image (about twice the panel's width, for HiDPI).
pub const NAVIGATOR_SIDE: u32 = 512;
/// How long adjustment-dialog settings must stay unchanged before the navigator shows them.
const NAVIGATOR_SETTLE_MS: f64 = 200.0;

/// The Navigator panel's image of document `idx`. With the CPU canvas that is the canvas texture
/// itself; with the GPU canvas a thumbnail cached per revision, so an edit to a huge document
/// doesn't also pay a full-resolution CPU composite for the navigator.
pub fn navigator_texture(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Option<egui::TextureId> {
    if app.gpu.is_none() {
        return ensure_texture(app, ctx, idx).map(|(t, _)| t);
    }
    // Keyed by the document snapshot, not its revision: selecting a layer changes no pixels.
    let (snapshot, id) = app.session.documents().get(idx).map(|st| (std::sync::Arc::downgrade(&st.doc), st.doc.id))?;
    let (doc, preview_key) = display_doc(app, idx);
    let (display, display_key) = canvas_display(app, &doc);
    let preview_key = preview_key ^ display_key;
    let key = egui::Id::new(("navigator", id.0));
    type Cached = (std::sync::Weak<Document>, u64, egui::TextureHandle);
    let cached: Option<Cached> = ctx.data(|d| d.get_temp(key));
    if let Some((r, p, t)) = &cached
        && r.ptr_eq(&snapshot)
        && *p == preview_key
    {
        return Some(t.id());
    }
    // While a Move drag is under way the navigator keeps its image and catches up on release.
    if let Some((_, _, t)) = &cached
        && crate::move_ui::showing(app)
    {
        return Some(t.id());
    }
    // While an adjustment dialog's settings are changing, the navigator keeps its image and
    // catches up once they settle (a thumbnail composite per change would cost more than the canvas).
    if let Some((r, _, t)) = &cached
        && r.ptr_eq(&snapshot)
        && crate::adjust_preview::settling(app, NAVIGATOR_SETTLE_MS)
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(NAVIGATOR_SETTLE_MS as u64));
        return Some(t.id());
    }
    let t0 = crate::gpu_canvas::now_ms();
    let image = display_image(display.as_deref(), &photocraft_compose::thumbnail_buffer(&doc, NAVIGATOR_SIDE));
    let tex = match cached {
        Some((_, _, mut t)) => {
            t.set(image, TextureOptions::LINEAR);
            t
        }
        None => ctx.load_texture(format!("navigator-{}", id.0), image, TextureOptions::LINEAR),
    };
    app.perf.span("navigator", crate::gpu_canvas::now_ms() - t0);
    ctx.data_mut(|d| d.insert_temp(key, (snapshot, preview_key, tex.clone())));
    Some(tex.id())
}

/// Make sure the canvas texture for document `idx` is current; returns (texture id, scale).
pub fn ensure_texture(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Option<(egui::TextureId, f32)> {
    let (revision, last_damage, id) = {
        let st = app.session.documents().get(idx)?;
        (st.revision, st.last_damage.map(|r| if r.is_empty() { r } else { r.inflate(effect_reach(&st.doc.layers)) }), st.doc.id)
    };
    let (mut doc, mut preview_key) = display_doc(app, idx);
    // A flipped view draws a GPU machine's canvas through here: an adjustment dialog's preview of
    // a large document (taken by the GPU path) would cost a full-size CPU composite per change.
    if crate::adjust_preview::shown_key(app) == Some(preview_key)
        && let Some(st) = app.session.documents().get(idx)
        && crate::proxy::factor(&st.doc) > 1
    {
        (doc, preview_key) = (st.doc.clone(), 0);
    }
    let (display, display_key) = canvas_display(app, &doc);
    let seen = app.canvases.get(&id).map(|c| (c.tex_revision, c.tex_preview_key));
    let damage = seen.and_then(|seen| damage_since(app, idx, seen, (revision, preview_key), display_key, last_damage));
    let preview_key = preview_key ^ display_key;
    let cache = app.canvases.entry(id).or_insert(CanvasCache {
        revision: 0,
        texture: None,
        scale: 1.0,
        preview_key: 0,
        on_gpu: false,
        tex_revision: 0,
        tex_preview_key: 0,
    });
    if cache.tex_revision != revision || cache.texture.is_none() || cache.tex_preview_key != preview_key {
        let t0 = crate::gpu_canvas::now_ms();
        let longest = doc.size.width.max(doc.size.height);
        let factor = longest.div_ceil(MAX_TEXTURE).max(1);
        let (w, h) = ((doc.size.width / factor).max(1), (doc.size.height / factor).max(1));
        let current = cache.texture.as_mut().filter(|t| t.size() == [w as usize, h as usize]);
        if let (Some(d), Some(t)) = (damage, current) {
            // Only what the edit or stroke touched: the reduced texture's pixels over it, with the
            // values the whole reduction gives them (factor 1 is the plain composite of `d`).
            let buf = photocraft_compose::render_reduced_damage(&doc, w, h, d);
            let r = buf.rect;
            if !r.is_empty() {
                let t1 = crate::gpu_canvas::now_ms();
                t.set_partial([r.x0 as usize, r.y0 as usize], display_image(display.as_deref(), &buf), TextureOptions::LINEAR);
                let px = (r.width() as u64 * r.height() as u64).saturating_mul(u64::from(factor).pow(2));
                app.perf.record("rect", px, t1 - t0, crate::gpu_canvas::now_ms() - t1);
            }
        } else {
            // Reduced in bands straight from the compositor: no full-size composite in memory.
            let full = photocraft_compose::render_reduced(&doc, w, h);
            let t1 = crate::gpu_canvas::now_ms();
            let (img, scale) = (display_image(display.as_deref(), &full), 1.0 / factor as f32);
            match cache.texture.as_mut() {
                Some(t) if t.size() == img.size => t.set(img, TextureOptions::LINEAR),
                _ => cache.texture = Some(ctx.load_texture(format!("canvas-{}", id.0), img, TextureOptions::LINEAR)),
            }
            cache.scale = scale;
            app.perf.record("full", doc.size.width as u64 * doc.size.height as u64, t1 - t0, crate::gpu_canvas::now_ms() - t1);
        }
        cache.tex_revision = revision;
        cache.tex_preview_key = preview_key;
    }
    Some((cache.texture.as_ref()?.id(), cache.scale))
}

/// What changed since a canvas cache showed (`revision`, `preview key`) `seen`, when only a
/// rectangle did: the last edit's damage, or the live stroke's dabs since then. Cached keys have
/// the colour display's key folded in (`^ display_key`); `now`'s is still raw.
fn damage_since(app: &PhotocraftApp, idx: usize, seen: (u64, u64), now: (u64, u64), display_key: u64, last_damage: Option<DRect>) -> Option<DRect> {
    if seen.1 == now.1 ^ display_key && seen.0 + 1 == now.0 {
        return last_damage;
    }
    // Between an adjustment dialog's previews (and the document): the target's area.
    if seen.0 == now.0
        && let Some(st) = app.session.documents().get(idx)
        && let Some(r) = crate::adjust_preview::switch_region(app, st.doc.id, now.0, seen.1 ^ display_key, now.1)
    {
        return Some(r);
    }
    // Between a Move drag's offsets: where the moving layers were and are (Auto-Select's pick
    // at the press changes no pixels).
    if (seen.0 == now.0 || (seen.0 + 1 == now.0 && last_damage.is_some_and(|r| r.is_empty())))
        && let Some(st) = app.session.documents().get(idx)
        && let Some(r) = crate::move_ui::damage(app, st.doc.id, now.0, seen.1 ^ display_key, now.1)
    {
        return Some(if r.is_empty() { r } else { r.inflate(effect_reach(&st.doc.layers)) });
    }
    let l = live_stroke(app, idx).filter(|l| seen.0 == now.0 && l.display_key() == now.1)?;
    let r = l.since(seen.1 ^ display_key)?;
    Some(if r.is_empty() { r } else { r.inflate(effect_reach(&l.stroke.doc.layers)) })
}

/// Document `doc`'s canvas caches showed a preview that the edit just committed reproduces
/// (`was_preview` tells its keys): count it as the document itself, so the commit's damage rect
/// refreshes only that area instead of everything.
pub(crate) fn shown_as_document(app: &mut PhotocraftApp, doc: photocraft_doc::DocId, was_preview: impl Fn(u64) -> bool) {
    let display_key = app.session.active().map_or(0, |st| canvas_display(app, &st.doc).1);
    if let Some(c) = app.canvases.get_mut(&doc) {
        if was_preview(c.preview_key ^ display_key) {
            c.preview_key = display_key;
        }
        if was_preview(c.tex_preview_key ^ display_key) {
            c.tex_preview_key = display_key;
        }
    }
}

/// How far beyond an edit's damage rect the composite can change: layer effects (shadows, glows,
/// strokes, …) on the edited layer and on the groups around it reach that far.
pub(crate) fn effect_reach(layers: &[photocraft_doc::Layer]) -> i32 {
    layers
        .iter()
        .filter(|l| l.visible)
        .map(|l| {
            let own = if photocraft_compose::effects::has_effects(l) { photocraft_compose::effects::margin(l) } else { 0 };
            let inner = match &l.content {
                LayerContent::Group(g) => effect_reach(&g.children),
                _ => 0,
            };
            own + inner
        })
        .max()
        .unwrap_or(0)
}

/// GPU path: make sure document `idx` is current in the GPU canvas. Brush strokes re-composite
/// and upload only their damage rect; everything else re-composites the whole document.
/// Returns false if there is no GPU canvas.
fn ensure_gpu(app: &mut PhotocraftApp, idx: usize, visible: DRect) -> bool {
    let Some(gpu) = app.gpu.clone() else { return false };
    let Some((revision, last_damage, id)) = app
        .session
        .documents()
        .get(idx)
        .map(|st| (st.revision, st.last_damage.map(|r| if r.is_empty() { r } else { r.inflate(effect_reach(&st.doc.layers)) }), st.doc.id))
    else {
        return false;
    };
    let (doc, raw_key) = display_doc(app, idx);
    let (display, display_key) = canvas_display(app, &doc);
    let seen = app.canvases.get(&id).map(|c| (c.revision, c.preview_key));
    let mut damage = seen.and_then(|seen| damage_since(app, idx, seen, (revision, raw_key), display_key, last_damage));
    // To an adjustment dialog's preview: only what the view shows now (the rest as it moves there).
    if damage.is_some()
        && let Some(r) = seen.filter(|s| s.0 == revision).and_then(|s| crate::adjust_preview::switch_region(app, id, revision, s.1 ^ display_key, raw_key))
    {
        damage = Some(crate::adjust_preview::switch_damage(app, raw_key, r, visible));
    }
    let preview_key = raw_key ^ display_key;
    let size = [doc.size.width, doc.size.height];
    let cache = app.canvases.entry(id).or_insert(CanvasCache {
        revision: 0,
        texture: None,
        scale: 1.0,
        preview_key: 0,
        on_gpu: false,
        tex_revision: 0,
        tex_preview_key: 0,
    });
    let present = cache.on_gpu && gpu.has(id.0, size);
    if present && cache.revision == revision && cache.preview_key == preview_key {
        // An adjustment preview composited where the view was: catch up where it moved to.
        if let Some(r) = crate::adjust_preview::uncovered(app, id, raw_key, visible) {
            let r = gpu.refresh(id.0, &doc, Some(r), display.as_deref());
            app.perf.record(r.kind, r.px, r.composite_ms, r.upload_ms);
        }
        return true;
    }
    let partial = present && damage.is_some();
    gpu_budget(app, &gpu, idx, partial, visible);
    let r = gpu.refresh(id.0, &doc, if partial { damage } else { None }, display.as_deref());
    if r.kind == "lost" {
        // The device is gone: draw this frame on the CPU path; `gpu_status` drops the GPU canvas.
        return false;
    }
    if let Some(e) = &r.fallback
        && app.perf.gpu_fallback.as_deref() != Some(e.as_str())
    {
        log::info!("{e}; using the CPU compositor");
    }
    app.perf.record(r.kind, r.px, r.composite_ms, r.upload_ms);
    if r.kind.starts_with("gpu") {
        app.perf.gpu_uploads = r.uploads;
    } else {
        app.perf.gpu_fallback = r.fallback;
    }
    let Some(cache) = app.canvases.get_mut(&id) else { return true };
    cache.revision = revision;
    cache.preview_key = preview_key;
    cache.on_gpu = true;
    cache.texture = None;
    true
}

/// Point the wgpu compositor at what the view shows, and size its memory budget from Memory
/// Usage, the document's pixels and History, and physical memory: on full refreshes (pixels
/// counted once per structural change, not per brush dab) and whenever the preference changes.
fn gpu_budget(app: &mut PhotocraftApp, gpu: &crate::gpu_canvas::GpuCanvas, idx: usize, partial: bool, visible: DRect) {
    gpu.set_focus(Some(visible));
    let allowance = u64::from(app.session.prefs().performance.memory_usage_mb).saturating_mul(1 << 20);
    if partial && app.perf.gpu_budget_allowance == allowance && gpu.memory_budget().is_some() {
        return;
    }
    let Some(st) = app.session.documents().get(idx) else { return };
    let pixels = st.history.pixel_bytes(&st.doc) as u64;
    let budget = crate::gpu_canvas::memory_budget(allowance, pixels, crate::gpu_canvas::physical_memory());
    gpu.set_memory_budget(budget);
    app.perf.gpu_budget_mb = budget >> 20;
    app.perf.gpu_budget_allowance = allowance;
}

/// Live preview for an open filter dialog: run the filter on the proxy and upload it.
fn ensure_filter_preview(app: &mut PhotocraftApp, idx: usize) -> Option<(u32, u64)> {
    if crate::adjust_preview::on_layer(app, idx) {
        return None;
    }
    let d = app.ui.dialogs.iter().find(|d| d.fields.contains_key("__filter"))?;
    if d.fields.get("__preview").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let cmd = d.fields.get("__command")?.as_str()?.to_string();
    let params = crate::filter_dialog::params_of(&d.fields);
    let (doc_id, revision, doc, active) = {
        let st = app.session.documents().get(idx)?;
        (st.doc.id, st.revision, st.doc.clone(), st.active_layer)
    };
    let k = crate::proxy::factor(&doc);
    let hash = format!("{cmd}{params}").bytes().fold(k as u64 ^ revision.wrapping_mul(0x9e37), |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    let key = doc_id.0 ^ (1u64 << 61);
    let fresh = matches!(&app.filter_preview, Some(p) if p.doc == doc_id && p.hash == hash);
    if !fresh {
        let t0 = crate::gpu_canvas::now_ms();
        let result = crate::filter_dialog::preview_document(&doc, active, &cmd, &params, k).map(std::sync::Arc::new);
        if let Some(r) = &result {
            let buf = photocraft_compose::flatten(r);
            let t1 = crate::gpu_canvas::now_ms();
            let (display, _) = canvas_display(app, &doc);
            app.gpu.as_ref()?.upload_buffer_full(key, &texture_buffer(display.as_deref(), &buf), doc.depth);
            app.perf.record("filter-preview", r.size.area(), t1 - t0, crate::gpu_canvas::now_ms() - t1);
        }
        app.filter_preview = Some(crate::filter_dialog::FilterPreview { doc: doc_id, revision, hash, k, result });
    }
    app.filter_preview.as_ref().filter(|p| p.result.is_some()).map(|p| (p.k, key))
}

/// The document pixels a (non-rotated) view shows, with a margin for filtering.
fn visible_doc_rect(xf: &ViewXform) -> DRect {
    let (a, b) = (xf.to_doc(xf.rect.min), xf.to_doc(xf.rect.max));
    let c = |v: f64| v.clamp(-1e9, 1e9) as i32;
    DRect::new(c(a[0].min(b[0]).floor()) - 2, c(a[1].min(b[1]).floor()) - 2, c(a[0].max(b[0]).ceil()) + 2, c(a[1].max(b[1]).ceil()) + 2)
}

/// Zoomed-out Image › Adjustments preview on a large document: the wgpu compositor renders the
/// reduced preview document (`adjust_preview::gpu_proxy`) into its own texture, over the target's
/// area after the first frame. Returns (factor, gpu key) to draw.
fn ensure_adjust_proxy(app: &mut PhotocraftApp, idx: usize, zoom: f32) -> Option<(u32, u64)> {
    let frame = crate::adjust_preview::gpu_proxy(app, idx, zoom)?;
    let key = frame.doc.id.0;
    let size = [frame.doc.size.width, frame.doc.size.height];
    let gpu = app.gpu.clone()?;
    if frame.stale || !gpu.has(key, size) {
        let doc = app.session.documents().get(idx)?.doc.clone();
        let (display, _) = canvas_display(app, &doc);
        let r = gpu.refresh(key, &frame.doc, frame.damage, display.as_deref());
        app.perf.record(if r.kind.starts_with("gpu") { "gpu-adjust-proxy" } else { "adjust-proxy" }, r.px, r.composite_ms, r.upload_ms);
        crate::adjust_preview::proxy_drawn(app, &frame);
    }
    Some((frame.k, key))
}

/// If a live adjustment preview is active on a large document, composite it on the proxy and upload
/// it under its own GPU key. Returns (factor, gpu key) when the proxy should be drawn.
fn ensure_proxy_preview(app: &mut PhotocraftApp, idx: usize) -> Option<(u32, u64)> {
    let (layer, params) = app.live_adjust.clone()?;
    let (doc_id, revision, doc) = {
        let st = app.session.documents().get(idx)?;
        (st.doc.id, st.revision, st.doc.clone())
    };
    let k = crate::proxy::factor(&doc);
    if k <= 1 {
        return None;
    }
    let fresh = matches!(&app.proxy, Some((d, r, kk, _)) if *d == doc_id && *r == revision && *kk == k);
    if !fresh {
        app.proxy = Some((doc_id, revision, k, std::sync::Arc::new(crate::proxy::proxy_document(&doc, k))));
    }
    let proxy = app.proxy.as_ref()?.3.clone();
    let key = doc_id.0 ^ (1u64 << 62);
    let hash = params.to_string().bytes().fold(k as u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    if app.proxy_uploaded != Some((doc_id, hash)) {
        let l = proxy.layer(layer)?;
        let LayerContent::Adjustment(a) = &l.content else { return None };
        let kind = photocraft_engine::commands::adjustment_kind(a);
        let preview = photocraft_engine::adjust_params::from_params(kind, &params, Some(a), proxy.mode).unwrap_or_else(|_| a.clone());
        let mut p = (*proxy).clone();
        if let Some(lm) = p.layer_mut(layer) {
            lm.content = LayerContent::Adjustment(preview);
        }
        let t0 = crate::gpu_canvas::now_ms();
        let buf = photocraft_compose::flatten(&p);
        let t1 = crate::gpu_canvas::now_ms();
        let (display, _) = canvas_display(app, &doc);
        app.gpu.as_ref()?.upload_buffer_full(key, &texture_buffer(display.as_deref(), &buf), doc.depth);
        app.perf.record("proxy", p.size.area(), t1 - t0, crate::gpu_canvas::now_ms() - t1);
        app.proxy_uploaded = Some((doc_id, hash));
    }
    Some((k, key))
}

/// A CPU composite as the GPU canvas texture stores it (sRGB-encoded for linear documents).
fn texture_buffer<'a>(
    display: Option<&photocraft_engine::display_color::CanvasDisplay>,
    buf: &'a photocraft_compose::Buffer,
) -> std::borrow::Cow<'a, photocraft_compose::Buffer> {
    match display {
        Some(d) => d.texture_buffer(buf),
        None => std::borrow::Cow::Borrowed(buf),
    }
}

pub(crate) fn retain_gpu_documents(app: &mut PhotocraftApp) {
    // Remove a closed adjustment owner's proxy before collecting its live GPU keys.
    crate::adjust_preview::retain_documents(app);
    if let Some(gpu) = &app.gpu {
        // Keep each document's texture plus its preview textures (filter preview, adjustment proxy);
        // retaining only document ids freed the previews every frame (blank canvas while previewing).
        let mut live: Vec<u64> = app.session.documents().iter().flat_map(|st| [st.doc.id.0, st.doc.id.0 ^ (1u64 << 61), st.doc.id.0 ^ (1u64 << 62)]).collect();
        live.extend(crate::adjust_preview::gpu_keys(app));
        gpu.retain(&live);
    }
    // Upload markers must not outlive the resources they describe: native reopen can reuse
    // both the document ID and revision before the next frame while a preview dialog stays open.
    let documents = app.session.documents();
    if app.filter_preview.as_ref().is_some_and(|preview| !documents.iter().any(|st| st.doc.id == preview.doc)) {
        app.filter_preview = None;
    }
    if app.proxy_uploaded.is_some_and(|(doc, _)| !documents.iter().any(|st| st.doc.id == doc)) {
        app.proxy_uploaded = None;
        app.proxy = None;
    }
}

/// Tabs + canvas for the active document, or the start screen.
pub fn document_area(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    retain_gpu_documents(app);
    let n = app.session.documents().len();
    // Files opening in the background (#210) have tabs before they have documents.
    let opening = !app.jobs.opens.is_empty();
    if !opening && app.ui.chrome.shows_home(n, app.session.prefs().general.auto_show_home_screen) {
        start_screen(app, ui);
        return;
    }
    if n == 0 && !opening {
        // Auto show the Home Screen is off: an empty workspace, like Photoshop.
        paint_dots(ui, ui.available_rect_before_wrap());
        return;
    }
    if !app.ui.view.hides_tabs() || opening {
        tabs(app, ui);
    }
    if let Some(job) = app.jobs.focus.or_else(|| (n == 0).then(|| app.jobs.opens.last().map(|o| o.job)).flatten()) {
        crate::jobs_ui::open_card(app, ui, job);
        return;
    }
    let Some(idx) = app.session.active_index() else { return };
    let rect = ui.available_rect_before_wrap();
    app.last_canvas_rect = rect;
    let n = app.session.documents().len();
    // Window › Arrange: tiled / n-up layouts show several documents side by side; the active one
    // takes input, a click elsewhere activates that document.
    if let Some(cells) = crate::view_cmds::cells(&app.ui.view.arrange, rect, n) {
        let order: Vec<usize> = (0..n).map(|k| (idx + k) % n).collect();
        let mut shown: Vec<(usize, Rect)> = order.into_iter().zip(cells).collect();
        shown.sort_by_key(|(d, _)| *d);
        let t = crate::theme::Tokens::get(ui.ctx());
        for (d, cell) in shown {
            let cell = cell.shrink(1.0);
            let view = app.ui.views[d].clone();
            let v = canvas_view(app, ui, d, cell, view, d == idx);
            if d != idx {
                app.ui.views[d] = v;
                if ui.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| cell.contains(p))) {
                    app.session.set_active(d);
                }
            }
            let stroke = if d == idx { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, Color32::from_black_alpha(160)) };
            ui.painter().rect_stroke(cell, 0, stroke, egui::StrokeKind::Inside);
        }
        return;
    }
    let view = app.ui.views[idx].clone();
    canvas_view(app, ui, idx, rect, view, true);
}

fn tabs(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    if t.pro {
        return pro_tabs(app, ui);
    }
    let active = app.session.active_index();
    let mut activate = None;
    let mut close = None;
    let (mut focus_open, mut cancel_open) = (None, None);
    let focused_open = app.jobs.focus.is_some();
    egui::Frame::NONE.fill(t.canvas).inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, st) in app.session.documents().iter().enumerate() {
                let sel = Some(i) == active && !focused_open;
                let name = format!("{}{}", st.doc.name, if st.is_dirty() { " *" } else { "" });
                let meta = format!("{}/{}", mode_label(&st.doc), st.doc.depth.bits());
                let name_g = ui.painter().layout_no_wrap(name, crate::theme::medium(12.5), t.text);
                let meta_g = ui.painter().layout_no_wrap(meta, egui::FontId::proportional(10.5), t.text_faint);
                let w = name_g.size().x + meta_g.size().x + 44.0;
                let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(r, t.radius_sm, t.card);
                    ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, t.radius_sm, t.hover.gamma_multiply(0.5));
                }
                let color = if sel { t.text } else { t.text_dim };
                let ny = r.center().y - name_g.size().y / 2.0;
                ui.painter().galley_with_override_text_color(egui::pos2(r.left() + 10.0, ny), name_g.clone(), color);
                ui.painter().galley(egui::pos2(r.left() + 16.0 + name_g.size().x, r.center().y - meta_g.size().y / 2.0), meta_g, t.text_faint);
                let xr = Rect::from_center_size(egui::pos2(r.right() - 12.0, r.center().y), egui::vec2(16.0, 16.0));
                let xresp = ui.interact(xr, ui.id().with(("tabx", i)), Sense::click());
                if xresp.hovered() {
                    ui.painter().rect_filled(xr, 4.0, t.hover);
                }
                crate::icons::paint(ui, xr, "x", 11.0, if xresp.hovered() { t.text } else { t.text_faint });
                if xresp.clicked() {
                    close = Some(i);
                } else if resp.clicked() {
                    activate = Some(i);
                }
            }
            // Files opening in the background: a tab with a progress underline; × cancels.
            for (job, name, frac) in crate::jobs_ui::open_tabs(app) {
                let sel = app.jobs.focus == Some(job);
                let name_g = ui.painter().layout_no_wrap(name, crate::theme::medium(12.5), t.text);
                let meta_g = ui.painter().layout_no_wrap(format!("{:.0}%", frac * 100.0), egui::FontId::proportional(10.5), t.text_faint);
                let w = name_g.size().x + meta_g.size().x + 44.0;
                let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(r, t.radius_sm, t.card);
                    ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, t.radius_sm, t.hover.gamma_multiply(0.5));
                }
                let ny = r.center().y - name_g.size().y / 2.0;
                ui.painter().galley_with_override_text_color(egui::pos2(r.left() + 10.0, ny), name_g.clone(), if sel { t.text } else { t.text_dim });
                ui.painter().galley(egui::pos2(r.left() + 16.0 + name_g.size().x, r.center().y - meta_g.size().y / 2.0), meta_g, t.text_faint);
                crate::jobs_ui::tab_underline(ui, r, frac, &t);
                let xr = Rect::from_center_size(egui::pos2(r.right() - 12.0, r.center().y), egui::vec2(16.0, 16.0));
                let xresp = ui.interact(xr, ui.id().with(("tabjobx", job.0)), Sense::click());
                if xresp.hovered() {
                    ui.painter().rect_filled(xr, 4.0, t.hover);
                }
                crate::icons::paint(ui, xr, "x", 11.0, if xresp.hovered() { t.text } else { t.text_faint });
                if xresp.on_hover_text(tl!("Cancel opening")).clicked() {
                    cancel_open = Some(job);
                } else if resp.clicked() {
                    focus_open = Some(job);
                }
            }
        });
    });
    open_tab_clicks(app, activate, focus_open, cancel_open);
    if let Some(i) = close {
        let _ = crate::menus::invoke(app, ui.ctx(), "file.close", json!({"document": i}));
    }
}

/// Apply tab-strip clicks: a document tab shows that document, an opening tab its progress, and
/// an opening tab's × cancels the open (closing the half-open tab).
fn open_tab_clicks(
    app: &mut PhotocraftApp,
    activate: Option<usize>,
    focus_open: Option<photocraft_engine::jobs::JobId>,
    cancel_open: Option<photocraft_engine::jobs::JobId>,
) {
    if let Some(i) = activate {
        app.session.set_active(i);
        app.jobs.focus = None;
    }
    if let Some(job) = focus_open {
        app.jobs.focus = Some(job);
    }
    if let Some(job) = cancel_open {
        crate::jobs_ui::cancel(app, job);
    }
}

/// Photoshop document tabs: "name @ 33.3% (RGB/8)" on a dark strip; active tab matches panels.
fn pro_tabs(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let active = app.session.active_index().filter(|_| app.jobs.focus.is_none());
    let (mut activate, mut close) = (None, None);
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().rect_filled(strip, 0.0, t.tab_strip);
    let mut x = strip.left();
    for (i, st) in app.session.documents().iter().enumerate() {
        let zoom = app.ui.views.get(i).map_or(100.0, |v| v.zoom * 100.0);
        // Photoshop: "name @ 50% (Layer 1, RGB/8)", "(Layer 1, Layer Mask/8)" when the mask is targeted;
        // the Background layer's name is omitted.
        let active_layer = st.active_layer.and_then(|id| st.doc.layer(id)).filter(|l| !(l.name == "Background" && l.locks.transparency));
        let is_active_doc = app.session.active_index() == Some(i);
        let mask = is_active_doc && app.ui.mask_target && active_layer.is_some_and(|l| l.mask.is_some());
        let model = if mask { tl!("Layer Mask").to_string() } else { mode_label(&st.doc).to_string() };
        let layer = active_layer.map(|l| format!("{}, ", l.name)).unwrap_or_default();
        let title = format!("{} @ {}% ({layer}{model}/{}){}", st.doc.name, fmt_zoom(zoom), st.doc.depth.bits(), if st.is_dirty() { "*" } else { "" });
        let g = ui.painter().layout_no_wrap(title, egui::FontId::proportional(11.5), t.text);
        let r = Rect::from_min_size(egui::pos2(x, strip.top()), egui::vec2(g.size().x + 42.0, strip.height()));
        let resp = ui.interact(r, ui.id().with(("ptab", i)), Sense::click());
        let sel = Some(i) == active;
        if sel {
            ui.painter().rect_filled(r, 0.0, t.chrome);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.35));
        }
        ui.painter().line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0, t.separator));
        let xr = Rect::from_center_size(egui::pos2(r.left() + 13.0, r.center().y), egui::vec2(14.0, 14.0));
        let xresp = ui.interact(xr, ui.id().with(("ptabx", i)), Sense::click());
        crate::icons::paint(ui, xr, "x", 10.0, if xresp.hovered() { t.text } else { t.text_faint });
        ui.painter().galley_with_override_text_color(egui::pos2(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, if sel { t.text } else { t.text_faint });
        if xresp.clicked() {
            close = Some(i);
        } else if resp.clicked() {
            activate = Some(i);
        }
        x = r.right();
    }
    // Files opening in the background (#210): "name (Opening… 45%)" with a progress underline.
    let (mut focus_open, mut cancel_open) = (None, None);
    for (job, name, frac) in crate::jobs_ui::open_tabs(app) {
        let title = format!("{name} ({} {:.0}%)", tl!("Opening…"), frac * 100.0);
        let g = ui.painter().layout_no_wrap(title, egui::FontId::proportional(11.5), t.text);
        let r = Rect::from_min_size(egui::pos2(x, strip.top()), egui::vec2(g.size().x + 42.0, strip.height()));
        let resp = ui.interact(r, ui.id().with(("ptabjob", job.0)), Sense::click());
        let sel = app.jobs.focus == Some(job);
        if sel {
            ui.painter().rect_filled(r, 0.0, t.chrome);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.35));
        }
        ui.painter().line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0, t.separator));
        crate::jobs_ui::tab_underline(ui, r, frac, &t);
        let xr = Rect::from_center_size(egui::pos2(r.left() + 13.0, r.center().y), egui::vec2(14.0, 14.0));
        let xresp = ui.interact(xr, ui.id().with(("ptabjobx", job.0)), Sense::click());
        crate::icons::paint(ui, xr, "x", 10.0, if xresp.hovered() { t.text } else { t.text_faint });
        ui.painter().galley_with_override_text_color(egui::pos2(r.left() + 26.0, r.center().y - g.size().y / 2.0), g, if sel { t.text } else { t.text_faint });
        if xresp.on_hover_text(tl!("Cancel opening")).clicked() {
            cancel_open = Some(job);
        } else if resp.clicked() {
            focus_open = Some(job);
        }
        x = r.right();
    }
    open_tab_clicks(app, activate, focus_open, cancel_open);
    if let Some(i) = close {
        let _ = crate::menus::invoke(app, ui.ctx(), "file.close", json!({"document": i}));
    }
}

/// Photoshop-style zoom label: "33.3", "100", "12.5".
pub fn fmt_zoom(pct: f32) -> String {
    if (pct - pct.round()).abs() < 0.05 { format!("{}", pct.round() as i64) } else { format!("{pct:.1}") }
}

/// Repeating dot-grid texture for the canvas surround.
fn dots(ctx: &egui::Context, t: &crate::theme::Tokens) -> Option<egui::TextureId> {
    if t.bevel {
        return None;
    }
    let key = egui::Id::new(("canvas-dots", format!("{:?}", t.kind)));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(tex.id());
    }
    let n = 22usize;
    let mut px = vec![Color32::TRANSPARENT; n * n];
    for (dx, dy, a) in [(0usize, 0usize, 255u8), (1, 0, 110), (0, 1, 110), (1, 1, 60)] {
        let c = t.canvas_dot;
        px[(n / 2 + dy) * n + n / 2 + dx] = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
    }
    let opts = TextureOptions {
        magnification: egui::TextureFilter::Linear,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::Repeat,
        mipmap_mode: None,
    };
    let tex = ctx.load_texture("canvas-dots", egui::ColorImage::new([n, n], px), opts);
    let id = tex.id();
    ctx.data_mut(|d| d.insert_temp(key, tex));
    Some(id)
}

fn paint_dots(ui: &egui::Ui, rect: Rect) {
    let t = crate::theme::Tokens::get(ui.ctx());
    if let Some(id) = dots(ui.ctx(), &t) {
        let uv = Rect::from_min_max(Pos2::ZERO, pos2(rect.width() / 22.0, rect.height() / 22.0));
        ui.painter_at(rect).image(id, rect, uv, Color32::WHITE);
    }
}

pub fn mode_label(doc: &Document) -> &'static str {
    match doc.mode {
        photocraft_doc::ColorMode::Rgb => "RGB",
        photocraft_doc::ColorMode::Grayscale => "Gray",
        photocraft_doc::ColorMode::Cmyk => "CMYK",
        photocraft_doc::ColorMode::Lab => "Lab",
        photocraft_doc::ColorMode::Indexed => "Indexed",
        photocraft_doc::ColorMode::Bitmap => "Bitmap",
        photocraft_doc::ColorMode::Duotone => "Duotone",
        photocraft_doc::ColorMode::Multichannel => "Multichannel",
    }
}

fn start_screen(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let area = ui.available_rect_before_wrap();
    paint_dots(ui, area);
    // File › Open Recent, newest first (on the web there are no paths to reopen).
    let recent: Vec<String> = if cfg!(target_arch = "wasm32") { Vec::new() } else { app.ui.recent_files.iter().take(HOME_RECENT).cloned().collect() };
    let recent_h = if recent.is_empty() { 0.0 } else { 34.0 + recent.len() as f32 * HOME_RECENT_ROW };
    let card = Rect::from_center_size(area.center(), egui::vec2(460.0, 330.0 + recent_h));
    let new_label = crate::shortcuts::command_label(app, "New document…", "file.new");
    let open_label = crate::shortcuts::command_label(app, "Open…", "file.open");
    ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                let title = ui.painter().layout_no_wrap(tl!("PhotoCraft").into(), crate::theme::semibold(38.0), t.text);
                let by = ui.painter().layout_no_wrap("open source".into(), egui::FontId::proportional(13.0), t.text_faint);
                let total = title.size().x + by.size().x + 10.0;
                ui.add_space(((card.width() - total) / 2.0).max(0.0));
                let (r, _) = ui.allocate_exact_size(title.size(), Sense::hover());
                ui.painter().galley(r.min, title, t.text);
                let (r2, _) = ui.allocate_exact_size(egui::vec2(by.size().x + 10.0, r.height()), Sense::hover());
                ui.painter().galley(egui::pos2(r2.left() + 10.0, r.bottom() - by.size().y - 8.0), by, t.text_faint);
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tl!("Create a new document or open an existing file.")).color(t.text_dim).size(14.0));
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                ui.add_space(((card.width() - 2.0 * 190.0 - 12.0) / 2.0).max(0.0));
                ui.spacing_mut().item_spacing.x = 12.0;
                if crate::widgets::primary_button(ui, &new_label, 190.0).clicked() {
                    app.ui.open_dialog(crate::state::DialogKind::NewDocument, crate::state::UiState::new_document_fields());
                }
                if crate::widgets::secondary_button(ui, &open_label, 190.0).clicked() {
                    app.open_dialog_file();
                }
            });
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                let msg = tl!("Drop an image or PSD anywhere to open it.");
                let g = ui.painter().layout_no_wrap(msg.into(), egui::FontId::proportional(12.5), t.text_faint);
                ui.add_space(((card.width() - g.size().x - 24.0) / 2.0).max(0.0));
                let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::hover());
                crate::icons::paint(ui, r, "image", 15.0, t.text_faint);
                ui.label(egui::RichText::new(msg).color(t.text_faint));
            });
            if !recent.is_empty() {
                ui.add_space(22.0);
                home_recent(app, ui, &recent);
            }
            ui.add_space(26.0);
            crate::links::discord_button(app, ui, 190.0);
            ui.add_space(10.0);
            crate::links::link_row(app, ui);
        });
    });
}

/// Recent files listed on the Home screen.
const HOME_RECENT: usize = 6;
/// Height of one Home-screen recent-file row.
const HOME_RECENT_ROW: f32 = 30.0;

/// The Home screen's "Recent" list: file name, its folder on the right, click to open.
fn home_recent(app: &mut PhotocraftApp, ui: &mut egui::Ui, recent: &[String]) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let width = 380.0;
    ui.horizontal(|ui| {
        // Line the heading up with the file icons.
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0) + 8.0);
        ui.label(egui::RichText::new(tl!("Recent")).font(crate::theme::semibold(12.5)).color(t.text_dim));
    });
    ui.add_space(4.0);
    let mut open = None;
    for path in recent {
        let name = crate::file_open::display_name(path);
        let folder = std::path::Path::new(path).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let (row, resp) = ui.allocate_exact_size(egui::vec2(width, HOME_RECENT_ROW), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(row, t.radius_sm, t.hover);
        }
        let icon = Rect::from_center_size(egui::pos2(row.left() + 16.0, row.center().y), egui::vec2(16.0, 16.0));
        crate::icons::paint(ui, icon, "file", 14.0, t.text_dim);
        let x = row.left() + 32.0;
        let name_g = crate::tab_strip::elided(ui, &name, egui::FontId::proportional(12.5), t.text, 170.0);
        let name_w = name_g.size().x;
        ui.painter().galley(egui::pos2(x, row.center().y - name_g.size().y / 2.0), name_g, t.text);
        let folder_max = (row.right() - 10.0 - (x + name_w + 14.0)).max(0.0);
        if folder_max > 20.0 && !folder.is_empty() {
            let fg = crate::tab_strip::elided(ui, &folder, egui::FontId::proportional(11.5), t.text_faint, folder_max);
            ui.painter().galley(egui::pos2(row.right() - 10.0 - fg.size().x, row.center().y - fg.size().y / 2.0), fg, t.text_faint);
        }
        if resp.on_hover_text(path).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            open = Some(path.clone());
        }
    }
    if let Some(path) = open
        && let Err(e) = app.open_path(&path)
    {
        app.open_failed(&crate::file_open::display_name(&path), &e);
    }
}

/// Lattice size of the canvas display LUT (colour management, Proof Colors, Gamut Warning).
const DISPLAY_LUT: usize = 33;

/// Keep the GPU display LUT under `key` (the document's texture or a preview of it) in step with
/// `doc`'s colour management: document → monitor profile and View › Proof Colors / Gamut
/// Warning (the 32-bit preview is applied by the canvas shader, see [`hdr_preview`]). Returns the canvas `display` mode (0 none — the identity, e.g. sRGB on
/// an sRGB monitor —, 1 LUT, 2 LUT + gamut warning).
fn sync_display_lut(app: &mut PhotocraftApp, doc: &photocraft_doc::Document, key: u64) -> u8 {
    let Some(gpu) = app.gpu.clone() else { return 0 };
    // Rebuild only when anything feeding the LUT changes.
    let sig = app.session.color.display_signature(doc);
    if let Some((s, mode)) = gpu.display_lut_signature(key)
        && s == sig
    {
        return mode;
    }
    let gamut = app.session.color.proof(doc.id).gamut_warning;
    let mode = match app.session.color.gpu_canvas_lut(doc, DISPLAY_LUT) {
        Ok(Some(bytes)) => {
            gpu.set_display_lut(key, DISPLAY_LUT as u32, Some(&bytes));
            if gamut { 2 } else { 1 }
        }
        Ok(None) => {
            gpu.set_display_lut(key, DISPLAY_LUT as u32, None);
            0
        }
        Err(e) => {
            app.ui.status = format!("Color management: {e}");
            gpu.set_display_lut(key, DISPLAY_LUT as u32, None);
            0
        }
    };
    gpu.cache_display_lut_signature(key, sig, mode);
    mode
}

/// View › 32-bit Preview Options for the GPU canvas shader: (exposure, gamma) when active.
fn hdr_preview(app: &PhotocraftApp, doc: &photocraft_doc::Document) -> Option<[f32; 2]> {
    app.session.color.hdr_preview(doc).map(|h| [h.exposure, h.gamma])
}

/// Draw one canvas view and handle its input. `primary` = main window (tools active).
pub fn canvas_view(app: &mut PhotocraftApp, ui: &mut egui::Ui, idx: usize, rect: Rect, mut view: View, primary: bool) -> View {
    let ctx = ui.ctx().clone();
    let full = rect;
    let rect = if primary { crate::rulers::content_rect(app, rect) } else { rect };
    let doc = app.session.documents()[idx].doc.clone();
    let size = [doc.size.width, doc.size.height];
    if view.doc_size != size {
        // Like Photoshop: keep the zoom level, re-centre the resized document.
        if view.doc_size != [0, 0] {
            view.center = [size[0] as f32 / 2.0, size[1] as f32 / 2.0];
        }
        view.doc_size = size;
    }
    if view.fit_pending && rect.width() > 50.0 {
        fit_view(&mut view, &doc, rect.size());
    }
    // Preferences › Tools › Overscroll off: clamp before anything is drawn (scrollbars.rs).
    if !app.session.prefs().tools.overscroll && crate::scrollbars::clamp_view(&mut view, rect.size()) {
        ctx.request_repaint();
    }
    let flip = app.ui.view.flip_horizontal;
    let xf = ViewXform { rect, zoom: view.zoom, center: view.center, flip };
    let pixel_grid = app.ui.view.shows(app.ui.view.show.pixel_grid);
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    match crate::prefs_ui::pasteboard_color(app) {
        Some(c) => {
            ui.painter_at(rect).rect_filled(rect, 0.0, c);
        }
        None => paint_dots(ui, rect),
    }
    let border = app.session.prefs().interface.canvas_border;
    let drop_shadow = border == photocraft_engine::prefs::CanvasBorder::DropShadow;
    // Drop shadow, checkerboard, document image.
    let img_rect = xf.doc_rect(doc.bounds());
    // Live adjustment previews on big documents use a downsampled proxy (see proxy.rs).
    let mut on_gpu = false;
    // A flipped view draws through the CPU path (the GPU canvas shader has no mirroring).
    if app.gpu.is_some()
        && !flip
        && let Some((k, key)) = ensure_adjust_proxy(app, idx, view.zoom * ctx.pixels_per_point())
            .or_else(|| ensure_filter_preview(app, idx))
            .or_else(|| ensure_proxy_preview(app, idx))
    {
        on_gpu = true;
        let params = crate::gpu_canvas::ViewParams {
            doc: key,
            doc_size: [doc.size.width.div_ceil(k), doc.size.height.div_ceil(k)],
            zoom: view.zoom * k as f32,
            center: [view.center[0] / k as f32, view.center[1] / k as f32],
            shadow: {
                let t = crate::theme::Tokens::get(&ctx);
                !t.bevel && !t.pro && drop_shadow
            },
            pixel_grid: false,
            view_key: egui::Id::new(("pc-canvas-proxy", ctx.viewport_id(), idx)).value(),
            display: sync_display_lut(app, &doc, key),
            hdr: hdr_preview(app, &doc),
        };
        crate::gpu_canvas::GpuCanvas::paint(&painter, rect, params);
    } else if !flip && ensure_gpu(app, idx, visible_doc_rect(&xf)) {
        on_gpu = true;
        app.perf.gpu = true;
        // Shadow, checkerboard, document and pixel grid in one custom shader (gpu_canvas.rs).
        let params = crate::gpu_canvas::ViewParams {
            doc: doc.id.0,
            doc_size: [doc.size.width, doc.size.height],
            zoom: view.zoom,
            center: view.center,
            shadow: {
                let t = crate::theme::Tokens::get(&ctx);
                !t.bevel && !t.pro && drop_shadow
            },
            pixel_grid,
            view_key: egui::Id::new(("pc-canvas", ctx.viewport_id(), idx)).value(),
            display: sync_display_lut(app, &doc, doc.id.0),
            hdr: hdr_preview(app, &doc),
        };
        crate::gpu_canvas::GpuCanvas::paint(&painter, rect, params);
    } else {
        if !crate::theme::Tokens::get(&ctx).bevel && drop_shadow {
            painter.add(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(150) }.as_shape(img_rect, 0));
        }
        match app.session.prefs().transparency_and_gamut.square() {
            Some(square) => {
                let checker_id = checker(app, &ctx);
                let tiles = img_rect.size() / (2.0 * square);
                painter.image(checker_id, img_rect, Rect::from_min_max(Pos2::ZERO, pos2(tiles.x, tiles.y)), Color32::WHITE);
            }
            None => {
                painter.rect_filled(img_rect, 0.0, Color32::WHITE);
            }
        }
        if let Some((tex, _scale)) = ensure_texture(app, &ctx, idx) {
            let uv = if flip { Rect::from_min_max(pos2(1.0, 0.0), pos2(0.0, 1.0)) } else { Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)) };
            painter.image(tex, img_rect, uv, Color32::WHITE);
        }
    }
    // Channels panel: alpha / Quick Mask overlays and single-channel views (channel_view.rs).
    if let Some(tex) = crate::channel_view::ensure(app, &ctx, idx) {
        let uv = if flip { Rect::from_min_max(pos2(1.0, 0.0), pos2(0.0, 1.0)) } else { Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)) };
        painter.image(tex, img_rect, uv, Color32::WHITE);
    }
    // Artboards: pasteboard between the boards, outlines and names (artboard_ui.rs).
    if doc.has_artboards() {
        let t = crate::theme::Tokens::get(&ctx);
        let pasteboard = crate::prefs_ui::pasteboard_color(app);
        let dot_tex = dots(&ctx, &t);
        for r in crate::artboard_ui::pasteboard_rects(&xf, &doc) {
            let r = r.intersect(rect);
            if !r.is_positive() {
                continue;
            }
            match pasteboard {
                Some(c) => {
                    painter.rect_filled(r, 0.0, c);
                }
                None => {
                    painter.rect_filled(r, 0.0, t.canvas);
                    if let Some(id) = dot_tex {
                        // Same phase as the dots around the document.
                        let uv = Rect::from_min_max(((r.min - rect.min) / 22.0).to_pos2(), ((r.max - rect.min) / 22.0).to_pos2());
                        painter.image(id, r, uv, Color32::WHITE);
                    }
                }
            }
        }
        if primary {
            crate::artboard_ui::draw_frames(app, &painter, &xf);
        }
    }

    // Pixel grid at high zoom (the GPU path draws its own).
    if !on_gpu && pixel_grid && view.zoom >= 12.0 {
        let vis = img_rect.intersect(rect);
        let a = xf.to_doc(vis.min);
        let b = xf.to_doc(vis.max);
        let grid = Stroke::new(1.0, Color32::from_white_alpha(40));
        for x in (a[0].floor() as i32)..=(b[0].ceil() as i32) {
            let sx = xf.to_screen(x as f32, 0.0).x;
            painter.line_segment([pos2(sx, vis.top()), pos2(sx, vis.bottom())], grid);
        }
        for y in (a[1].floor() as i32)..=(b[1].ceil() as i32) {
            let sy = xf.to_screen(0.0, y as f32).y;
            painter.line_segment([pos2(vis.left(), sy), pos2(vis.right(), sy)], grid);
        }
    }

    // View › Show › Layer Edges: the active layer's content bounds.
    if app.ui.view.shows(app.ui.view.show.layer_edges)
        && let Some(st) = app.session.documents().get(idx)
        && let Some(b) = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(|l| l.surface()).map(photocraft_compose::bounds::content_bounds)
        && !b.is_empty()
    {
        painter.rect_stroke(xf.doc_rect(b), 0, Stroke::new(1.0, Color32::from_rgb(0x2d, 0x8c, 0xeb)), egui::StrokeKind::Outside);
    }

    // Selection outline: true boundary, animated marching ants (cached per revision).
    if let Some(sel) = doc.selection.as_ref().filter(|_| app.ui.view.shows(app.ui.view.show.selection_edges)) {
        // Trace at display resolution over the visible part only; key by the mask's tile identity
        // (not the document revision) so unrelated edits don't re-trace it.
        let step = (1.0 / view.zoom.max(1e-3)).log2().floor().exp2().clamp(1.0, 64.0) as u32;
        let tl = xf.to_doc(rect.min);
        let br = xf.to_doc(rect.max);
        let q = 256 * step as i32; // quantise the region so small pans reuse the cache
        let vis = photocraft_geom::Rect::new(
            (tl[0].floor() as i32).div_euclid(q) * q - q,
            (tl[1].floor() as i32).div_euclid(q) * q - q,
            ((br[0].ceil() as i32).div_euclid(q) + 2) * q,
            ((br[1].ceil() as i32).div_euclid(q) + 2) * q,
        );
        let key = crate::surface_fingerprint(sel)
            ^ (step as u64) << 56
            ^ doc.id.0.rotate_left(17)
            ^ (vis.x0 as u64) << 8
            ^ (vis.y0 as u64) << 24
            ^ (vis.x1 as u64) << 36
            ^ (vis.y1 as u64) << 48;
        let fresh = matches!(&app.outline_cache, Some((d, k, _)) if *d == doc.id && *k == key);
        if !fresh {
            let t0 = crate::gpu_canvas::now_ms();
            let b = app.cached_bounds(u64::MAX - doc.id.0, sel).intersect(&vis);
            let segs = crate::outline::outline_scaled(sel, b, step);
            app.perf.span("outline", crate::gpu_canvas::now_ms() - t0);
            app.outline_cache = Some((doc.id, key, std::sync::Arc::new(segs)));
        }
        if let Some((_, _, segs)) = &app.outline_cache {
            let time = ui.input(|i| i.time);
            marching_ants_segments(&painter, &xf, segs, time);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    // Under an open dialog the canvas widget is inert, but the image still pans and zooms.
    let under_dialog = !app.ui.dialogs.is_empty();
    let free_hover = under_dialog && crate::dialogs::free_pointer_over(&ctx, rect).is_some();
    // Navigation (wheel_nav.rs): scroll pans; pinch, ⌘-scroll and ⌥-scroll zoom around the pointer.
    let wheel = crate::wheel_nav::read(&ctx, app.session.prefs().general.zoom_with_scroll_wheel);
    // The wheel also scrolls over the scrollbars drawn on top of the canvas (last frame's hover).
    let bars_id = egui::Id::new(("pc-canvas-bars-hover", idx));
    let over_bars = ctx.data(|d| d.get_temp::<bool>(bars_id)).unwrap_or(false);
    if response.hovered() || free_hover || over_bars {
        let pointer = ui.input(|i| i.pointer.hover_pos());
        match (wheel, pointer) {
            (Some(crate::wheel_nav::Wheel::Zoom(f)), Some(p)) => {
                let nz = (view.zoom * f).clamp(0.01, 64.0);
                zoom_about(&mut view, &xf, p, nz);
            }
            (Some(crate::wheel_nav::Wheel::Pan(scroll)), _) => {
                view.center[0] -= scroll.x / view.zoom * if flip { -1.0 } else { 1.0 };
                view.center[1] -= scroll.y / view.zoom;
            }
            _ => {}
        }
    }

    // Pen pressure/tilt for this frame's tool events (mouse = 1.0), unless Preferences › Tools ›
    // Use Tablet Pressure is off; the pen's eraser end selects the Eraser.
    app.stylus.use_pressure = app.session.prefs().tools.use_tablet_pressure;
    app.stylus.update(&ui.input(|i| i.events.clone()));
    crate::stylus::Stylus::sync_eraser_tool(app);
    // Held keys (hold_keys.rs): Space repositions a crop frame, marquee, lasso or shape being
    // drawn; otherwise Space is the Hand and ⌘Space / ⌘⌥Space the Zoom tool while held.
    let reposition = crate::hold_keys::reposition_held(app, &ctx);
    crate::crop_ui::set_space(app, reposition);
    let mut drawing = crate::crop_ui::active(app);
    if let Some(d) = app.drag.as_mut().filter(|d| crate::hold_keys::repositions(d.tool)) {
        d.reposition = reposition;
        drawing = true;
    }
    let temporary = crate::hold_keys::for_frame(app, &ctx, drawing);
    let space_pan = temporary == Some(crate::hold_keys::Temporary::Hand);
    let middle = ui.input(|i| i.pointer.middle_down());
    let tool = match temporary {
        Some(t) => t.tool(),
        None if middle => Tool::Hand,
        None => app.ui.tool,
    };
    // Zoom direction: the temporary zoom key decides, else ⌥ (Zoom tool).
    let zoom_out = |alt: bool| match temporary {
        Some(crate::hold_keys::Temporary::ZoomOut) => true,
        Some(crate::hold_keys::Temporary::ZoomIn) => false,
        _ => alt,
    };

    if under_dialog {
        if let Some(d) = crate::dialogs::pan_delta(&ctx, rect, app.ui.tool == Tool::Hand) {
            view.center[0] -= d.x / view.zoom * if flip { -1.0 } else { 1.0 };
            view.center[1] -= d.y / view.zoom;
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if free_hover && (space_pan || app.ui.tool == Tool::Hand) {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    if tool == Tool::Hand && response.dragged() {
        let d = response.drag_delta();
        view.center[0] -= d.x / view.zoom * if flip { -1.0 } else { 1.0 };
        view.center[1] -= d.y / view.zoom;
    } else if primary {
        let mods = ui.input(|i| i.modifiers);
        // Tools follow the left button; the right one opens the Brush Preset picker or erases
        // (Preferences › Tools, `paint_mouse`).
        crate::paint_mouse::sync_tool_smoothing(app);
        let mut buttons = crate::paint_mouse::canvas_buttons(app, &response, tool);
        // Right-click with the Move tool, or ⌘/Ctrl+right-click: the layers under the pointer.
        if response.secondary_clicked()
            && crate::layer_pick_ui::is_gesture(tool, mods)
            && let Some(p) = response.interact_pointer_pos()
        {
            let d = xf.to_doc(p);
            crate::layer_pick_ui::open(app, [p.x, p.y], d[0], d[1]);
        }
        // The (temporary) Hand pans above; its gestures never reach the tool underneath.
        if tool == Tool::Hand {
            (buttons.started, buttons.dragged, buttons.stopped) = (false, false, false);
        }
        // Zoom tool drags: scrubby zoom or a zoom rectangle (zoom_tool.rs); clicks step below.
        if tool == Tool::Zoom && crate::zoom_tool::drag(app, &ctx, &mut view, &xf, &buttons, response.interact_pointer_pos()) {
            (buttons.started, buttons.dragged, buttons.stopped) = (false, false, false);
        }
        // A drag is only recognised once the pointer has moved past egui's click distance: the
        // gesture starts where the button went down, not where it is now (#123).
        let gesture_active_before = app.drag.is_some();
        if buttons.started
            && let Some(p) = ui.input(|i| i.pointer.press_origin()).filter(|p| rect.contains(*p)).or(response.interact_pointer_pos())
        {
            if tool == Tool::Move && app.ui.transform.is_none() {
                begin_transform_controls_at(app, &ctx, &xf, p);
            }
            let d = xf.to_doc(p);
            tool_event(app, ToolEvent::Down { x: d[0], y: d[1], pressure: app.stylus.pressure() }, mods);
        }
        if buttons.dragged || buttons.stopped {
            // Feed every pointer move the OS delivered this frame, not just the latest position, so
            // a fast stroke is sampled densely and renders as a smooth curve instead of a coarse
            // polyline. egui-winit pushes one `PointerMoved` per `CursorMoved`, and they accumulate
            // while a frame is slow, so reading them all recovers the moves a per-frame
            // `interact_pointer_pos()` would drop. Only freehand tools take the whole batch; the
            // rest follow the pointer's latest position. The moves are bounded to the gesture's own
            // press..release interval (`pointer_moves`), so the start and stop frames keep their
            // valid samples without leaking a move from outside the gesture.
            let events = ui.input(|i| i.events.clone());
            let button = if response.dragged_by(PointerButton::Secondary)
                || response.drag_started_by(PointerButton::Secondary)
                || response.drag_stopped_by(PointerButton::Secondary)
            {
                PointerButton::Secondary
            } else {
                PointerButton::Primary
            };
            let press_this_frame = events.iter().any(|e| matches!(e, egui::Event::PointerButton { button: b, pressed: true, .. } if *b == button));
            let down_at_start = !press_this_frame && (gesture_active_before || buttons.started);
            let mut positions = if freehand_tool(tool) {
                pointer_moves(&events, button, down_at_start)
            } else if buttons.dragged {
                response.interact_pointer_pos().into_iter().collect()
            } else {
                Vec::new()
            };
            // A frame with no raw move still tracks a held gesture (a still pointer, a modifier
            // change): fall back to the latest position, as the old code always did.
            if positions.is_empty()
                && buttons.dragged
                && let Some(p) = response.interact_pointer_pos()
            {
                positions.push(p);
            }
            app.defer_live_stroke = true;
            for p in positions {
                let d = xf.to_doc(p);
                tool_event(app, ToolEvent::Move { x: d[0], y: d[1], pressure: app.stylus.pressure() }, mods);
            }
            app.defer_live_stroke = false;
            // One live-stroke update for the whole frame, not one per recovered sample.
            feed_live_stroke(app);
        }
        if buttons.stopped {
            let p = response.interact_pointer_pos().map(|p| xf.to_doc(p)).or_else(|| app.drag.as_ref().and_then(|d| d.points.last().map(|q| [q[0], q[1]])));
            if let Some(d) = p {
                tool_event(app, ToolEvent::Up { x: d[0], y: d[1] }, mods);
            }
        }
        if buttons.clicked
            && let Some(p) = response.interact_pointer_pos()
        {
            let d = xf.to_doc(p);
            match tool {
                Tool::Zoom => {
                    let nz = zoom_step(view.zoom, if zoom_out(mods.alt) { -1 } else { 1 });
                    zoom_about(&mut view, &xf, p, nz);
                }
                // A click with the (temporary) Hand does nothing, never the tool underneath.
                Tool::Hand => {}
                _ => {
                    if tool == Tool::Move && app.ui.transform.is_none() {
                        begin_transform_controls_at(app, &ctx, &xf, p);
                    }
                    tool_event(app, ToolEvent::Down { x: d[0], y: d[1], pressure: 1.0 }, mods);
                    tool_event(app, ToolEvent::Up { x: d[0], y: d[1] }, mods);
                }
            }
        }
        if app.ui.transform.is_some() && response.double_clicked() {
            crate::transform_tool::commit(app);
        }
        if tool == Tool::Type && response.double_clicked() {
            crate::type_tool::select_word(app);
        }
        if app.ui.extras.grid && app.ui.view.extras {
            crate::rulers::draw_grid(app, &painter, &xf, &doc);
        }
        if app.ui.view.shows(app.ui.view.show.canvas_guides) {
            crate::rulers::draw_guides(app, &painter, &xf, &doc);
        }
        draw_drag_preview(app, &painter, &xf);
        crate::zoom_tool::draw(&ctx, &painter);
        let resizing = crate::brush_resize::draw(app, &painter, &xf);
        draw_transform_controls(app, &painter, &xf);
        crate::paint_mouse::show_picker(app, &ctx);
        crate::layer_pick_ui::show(app, &ctx);
        crate::snap_ui::draw(app, &painter, &xf);
        if border == photocraft_engine::prefs::CanvasBorder::Line {
            painter.rect_stroke(img_rect, 0.0, Stroke::new(1.0, Color32::from_gray(20)), egui::StrokeKind::Outside);
        }
        crate::type_tool::draw_overlay(app, &painter, &xf);
        crate::transform_tool::draw_overlay(app, &painter, &xf);
        crate::distort_ui::draw_overlay(app, &painter, &xf);
        crate::retouch_ui::draw_source_marker(app, &painter, &xf);
        crate::vector_ui::draw_overlay(app, &painter, &xf, &doc);
        crate::analysis_ui::draw_overlay(app, &painter, &xf);
        crate::gradient_ui::draw_overlay(app, &painter, &xf);
        crate::slice_ui::draw_overlay(app, &painter, &xf);
        // Tool cursors (Photoshop-style).
        let guide_hover = response.hover_pos().filter(|_| tool == Tool::Move).and_then(|p| {
            let d = xf.to_doc(p);
            crate::rulers::guide_at(app, d[0], d[1])
        });
        if let Some((vertical, _)) = guide_hover {
            ui.ctx().set_cursor_icon(if vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
        } else if let Some(c) = response.hover_pos().and_then(|p| crate::transform_tool::cursor(app, xf.to_doc(p))) {
            ui.ctx().set_cursor_icon(c);
        } else if let Some(c) = response.hover_pos().filter(|_| tool == Tool::Crop).and_then(|p| crate::crop_ui::cursor(app, xf.to_doc(p))) {
            ui.ctx().set_cursor_icon(c);
        } else if let Some(p) = response.hover_pos() {
            let alt = ui.input(|i| i.modifiers.alt);
            let icon = match tool {
                // Resizing the brush: the circle stays where the drag began (`brush_resize`).
                t if resizing && crate::brush_resize::applies(t) => egui::CursorIcon::None,
                t if t.is_brushlike() || t == Tool::QuickSelection => {
                    // Preferences › Cursors: brush tip outline (normal = the 50% contour, or
                    // full size), precise crosshair, or the standard pointer.
                    use photocraft_engine::prefs::PaintingCursor;
                    let cur = app.session.prefs().cursors.clone();
                    let painting = app.drag.is_some();
                    let brush = &app.session.tools.brush;
                    let full = (brush.size / 2.0 * view.zoom).max(1.0);
                    let r = if cur.painting == PaintingCursor::NormalTip { (full * (0.5 + 0.5 * brush.hardness.clamp(0.0, 1.0))).max(1.0) } else { full };
                    let crosshair = |len: f32| {
                        for (w, c) in [(2.5, Color32::from_black_alpha(140)), (1.0, Color32::from_white_alpha(220))] {
                            painter.line_segment([p - vec2(len, 0.0), p + vec2(len, 0.0)], Stroke::new(w, c));
                            painter.line_segment([p - vec2(0.0, len), p + vec2(0.0, len)], Stroke::new(w, c));
                        }
                    };
                    match cur.painting {
                        PaintingCursor::Standard => egui::CursorIcon::Default,
                        PaintingCursor::Precise => {
                            crosshair(6.0);
                            egui::CursorIcon::None
                        }
                        _ if painting && cur.show_only_crosshair_while_painting => {
                            crosshair(5.0);
                            egui::CursorIcon::None
                        }
                        // The Pencil: the square of whole pixels its dab fills, on the pixel grid.
                        _ if tool == Tool::Pencil => {
                            let ppp = painter.ctx().pixels_per_point();
                            let sq = pencil_cursor_rect(&xf, xf.to_doc(p), brush.size, ppp);
                            let px = 1.0 / ppp;
                            painter.rect_stroke(sq, 0.0, Stroke::new(px, Color32::from_black_alpha(160)), egui::StrokeKind::Outside);
                            painter.rect_stroke(sq, 0.0, Stroke::new(px, Color32::from_white_alpha(230)), egui::StrokeKind::Inside);
                            // Too small to see where it is: the hotspot as well.
                            if cur.show_crosshair_in_brush_tip || sq.width() < 6.0 {
                                crosshair(4.0);
                            }
                            egui::CursorIcon::None
                        }
                        _ => {
                            painter.circle_stroke(p, r + 0.5, Stroke::new(1.0, Color32::from_black_alpha(140)));
                            painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_white_alpha(220)));
                            if brush_tip_centre(tool, alt || app.ui.shell.sticky_alt, cur.show_crosshair_in_brush_tip, r) {
                                crosshair(3.0);
                            }
                            egui::CursorIcon::None
                        }
                    }
                }
                // Preferences › Cursors › Other Cursors: Precise shows a crosshair for every tool.
                Tool::Move | Tool::Type | Tool::Eyedropper if app.session.prefs().cursors.other == photocraft_engine::prefs::OtherCursor::Precise => {
                    egui::CursorIcon::Crosshair
                }
                Tool::Move => egui::CursorIcon::Move,
                Tool::Hand => {
                    if response.dragged() {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    }
                }
                Tool::Zoom => {
                    if zoom_out(alt) {
                        egui::CursorIcon::ZoomOut
                    } else {
                        egui::CursorIcon::ZoomIn
                    }
                }
                Tool::Type => egui::CursorIcon::Text,
                _ => egui::CursorIcon::Crosshair,
            };
            ui.ctx().set_cursor_icon(icon);
            // Selection tools: + / − / × badge for the effective mode (#170). A gesture keeps the
            // mode it started with (⇧ then constrains the marquee instead of adding).
            let held = crate::workspace_ui::sticky_mods(app, ui.input(|i| i.modifiers));
            let mods = app.drag.as_ref().map_or(held, |d| d.modifiers);
            if let Some(b) = crate::tool_feedback::badge(app, tool, mods) {
                crate::tool_feedback::draw_badge(&painter, p, b, tool == Tool::QuickSelection);
            }
            // ⇧ after a stroke: the straight line a click would paint (#257).
            crate::stroke_constraint::draw_line_preview(app, &painter, &xf, p, tool, held.shift);
        }
    }
    // Scrollbars (scrollbars.rs): drawn over the canvas edges, they take the pointer there.
    let t0 = crate::gpu_canvas::now_ms();
    let before = view.center;
    let over_bars = crate::scrollbars::show(ui, rect, &mut view, flip, egui::Id::new(("pc-canvas", idx)));
    ctx.data_mut(|d| d.insert_temp(bars_id, over_bars));
    if view.center != before {
        ctx.request_repaint();
    }
    if primary {
        app.perf.span("scrollbars", crate::gpu_canvas::now_ms() - t0);
        app.hover_doc = response.hover_pos().map(|p| xf.to_doc(p));
    }
    if primary && app.ui.extras.rulers {
        crate::rulers::draw_rulers(app, ui, full, &xf);
    }
    if primary {
        app.ui.views[idx] = view.clone();
    }
    view
}

fn zoom_about(view: &mut View, xf: &ViewXform, p: Pos2, new_zoom: f32) {
    let before = xf.to_doc(p);
    view.zoom = new_zoom;
    let d = (p - xf.rect.center()) / new_zoom;
    let dx = if xf.flip { -d.x } else { d.x };
    view.center = [before[0] as f32 - dx, before[1] as f32 - d.y];
}

/// Draw boundary segments as marching ants: white base, black dashes phased along x + y.
fn marching_ants_segments(painter: &egui::Painter, xf: &ViewXform, segs: &[crate::outline::Segment], time: f64) {
    let dash = 4.0f32;
    let phase = ((time * 10.0) % (dash as f64 * 2.0)) as f32;
    let white = Stroke::new(1.0, Color32::WHITE);
    let black = Stroke::new(1.0, Color32::BLACK);
    let clip = painter.clip_rect();
    for (a, b) in segs {
        let pa = xf.to_screen(a[0] as f32, a[1] as f32);
        let pb = xf.to_screen(b[0] as f32, b[1] as f32);
        if !clip.intersects(Rect::from_two_pos(pa, pb).expand(1.0)) {
            continue;
        }
        let pa = pos2(pa.x.round() + 0.5, pa.y.round() + 0.5);
        let pb = pos2(pb.x.round() + 0.5, pb.y.round() + 0.5);
        painter.line_segment([pa, pb], white);
        let len = pa.distance(pb);
        if len < 0.5 {
            continue;
        }
        let dir = (pb - pa) / len;
        // Phase by screen position so dashes line up across joined segments.
        let start = (pa.x + pa.y + phase).rem_euclid(dash * 2.0);
        let mut t = -start;
        while t < len {
            let s0 = t.max(0.0);
            let s1 = (t + dash).min(len);
            if s1 > s0 {
                painter.line_segment([pa + dir * s0, pa + dir * s1], black);
            }
            t += dash * 2.0;
        }
    }
}

#[allow(dead_code)]
fn marching_ants(painter: &egui::Painter, r: Rect, time: f64) {
    let dash = 4.0;
    let offset = ((time * 8.0) % (dash as f64 * 2.0)) as f32;
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Middle);
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        let dir = (b - a) / len.max(1e-3);
        let mut t = -offset;
        while t < len {
            let s = t.max(0.0);
            let e = (t + dash).min(len);
            if e > s {
                painter.line_segment([a + dir * s, a + dir * e], Stroke::new(1.0, Color32::BLACK));
            }
            t += dash * 2.0;
        }
    }
}

/// Photoshop crop overlay: dimmed outside, bright frame, rule-of-thirds grid, corner and edge handles.
fn crop_overlay(painter: &egui::Painter, r: Rect) {
    let clip = painter.clip_rect();
    let dim = Color32::from_black_alpha(130);
    for band in [
        Rect::from_min_max(clip.min, egui::pos2(clip.max.x, r.min.y)),
        Rect::from_min_max(egui::pos2(clip.min.x, r.max.y), clip.max),
        Rect::from_min_max(egui::pos2(clip.min.x, r.min.y), egui::pos2(r.min.x, r.max.y)),
        Rect::from_min_max(egui::pos2(r.max.x, r.min.y), egui::pos2(clip.max.x, r.max.y)),
    ] {
        painter.rect_filled(band, 0.0, dim);
    }
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Middle);
    let thin = Stroke::new(1.0, Color32::from_white_alpha(90));
    for i in 1..3 {
        let fx = r.left() + r.width() * i as f32 / 3.0;
        let fy = r.top() + r.height() * i as f32 / 3.0;
        painter.line_segment([egui::pos2(fx, r.top()), egui::pos2(fx, r.bottom())], thin);
        painter.line_segment([egui::pos2(r.left(), fy), egui::pos2(r.right(), fy)], thin);
    }
    let h = Stroke::new(3.0, Color32::WHITE);
    let l = 14.0f32.min(r.width() / 3.0).min(r.height() / 3.0);
    for (c, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)] {
        painter.line_segment([c, c + vec2(l * dx, 0.0)], h);
        painter.line_segment([c, c + vec2(0.0, l * dy)], h);
    }
    // Edge handles: short bars at the middle of each side.
    let (lx, ly) = (l.min(r.width() / 4.0) / 2.0, l.min(r.height() / 4.0) / 2.0);
    for (c, horizontal) in [(r.center_top(), true), (r.center_bottom(), true), (r.left_center(), false), (r.right_center(), false)] {
        let e = if horizontal { vec2(lx, 0.0) } else { vec2(0.0, ly) };
        painter.line_segment([c - e, c + e], h);
    }
}

/// Overlays that persist between gestures: polygonal lasso in progress, pending crop box.
fn draw_tool_state(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, hover: Option<Pos2>) {
    if !app.ui.polygon.is_empty() {
        let mut pts: Vec<Pos2> = app.ui.polygon.iter().map(|p| xf.to_screen(p[0] as f32, p[1] as f32)).collect();
        if let Some(h) = hover {
            pts.push(h);
        }
        crate::tool_feedback::draw_ants(painter, &pts, false);
        for p in pts.iter().take(app.ui.polygon.len()) {
            painter.rect_filled(Rect::from_center_size(*p, vec2(5.0, 5.0)), 0.0, Color32::WHITE);
            painter.rect_stroke(Rect::from_center_size(*p, vec2(5.0, 5.0)), 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
        }
    }
    if let Some(c) = app.ui.crop_rect {
        let r = Rect::from_two_pos(xf.to_screen(c[0] as f32, c[1] as f32), xf.to_screen(c[2] as f32, c[3] as f32));
        crop_overlay(painter, r);
    }
}

/// Move tool › Show Transform Controls: the active layer's transform bounds.
fn transform_controls_rect(app: &PhotocraftApp, xf: &ViewXform) -> Option<Rect> {
    if app.ui.tool != Tool::Move || !app.ui.tool_options.move_show_transform || app.ui.transform.is_some() || app.drag.is_some() {
        return None;
    }
    let st = app.session.active()?;
    let l = st.active_layer.and_then(|id| st.doc.layer(id))?;
    if crate::doc_props_ui::is_background(&st.doc, l) {
        return None;
    }
    let b = photocraft_engine::transform_cmds::transform_bounds(&st.doc, l);
    if b.is_empty() {
        return None;
    }
    Some(Rect::from_two_pos(xf.to_screen(b.x0 as f32, b.y0 as f32), xf.to_screen(b.x1 as f32, b.y1 as f32)))
}

/// A visible handle starts scaling; the narrow band just outside the box starts rotation.
/// The interior stays the normal Move-tool drag target.
fn transform_controls_hit(r: Rect, p: Pos2) -> bool {
    let handles = [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()];
    handles.iter().any(|h| h.distance(p) <= 8.0) || (r.expand(18.0).contains(p) && !r.expand(5.0).contains(p))
}

/// Enter the existing Free Transform session when a Move-tool transform control is pressed.
fn begin_transform_controls_at(app: &mut PhotocraftApp, ctx: &egui::Context, xf: &ViewXform, p: Pos2) -> bool {
    let Some(r) = transform_controls_rect(app, xf) else { return false };
    if !transform_controls_hit(r, p) {
        return false;
    }
    match crate::transform_tool::begin(app, ctx) {
        Ok(()) => true,
        Err(e) => {
            app.ui.status = e;
            false
        }
    }
}

/// Move tool › Show Transform Controls: the active layer's bounding box with its eight handles.
fn draw_transform_controls(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let Some(r) = transform_controls_rect(app, xf) else { return };
    let accent = crate::theme::Tokens::get(painter.ctx()).accent;
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Middle);
    for p in [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()] {
        let h = Rect::from_center_size(p, vec2(7.0, 7.0));
        painter.rect_filled(h, 0.0, Color32::WHITE);
        painter.rect_stroke(h, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    }
}

fn draw_drag_preview(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    draw_tool_state(app, painter, xf, painter.ctx().input(|i| i.pointer.hover_pos()));
    let Some(d) = &app.drag else {
        app.trail = None;
        return;
    };
    let last = d.points.last().map(|p| [p[0], p[1]]).unwrap_or(d.start);
    let marquee = matches!(d.tool, Tool::RectMarquee | Tool::EllipseMarquee).then(|| marquee_corners(&app.ui.tool_options, d, last));
    if let Some((a, b)) = marquee {
        draw_marquee_readout(painter.ctx(), xf.to_screen(last[0] as f32, last[1] as f32), marquee_readout(marquee_px(a, b)));
    }
    match d.tool {
        // The canvas shows the live stroke itself (`LiveStroke`).
        Tool::Brush | Tool::Pencil | Tool::Eraser => {}
        t if t.is_brushlike() || t == Tool::QuickSelection => {
            // Retouching strokes preview as a translucent trail of the brush footprint: a mask,
            // not a brush-wide egui polyline (which zoomed in tessellates into wedges, #189).
            let col = Color32::from_white_alpha(if t == Tool::QuickSelection { 40 } else { 60 });
            let Some(st) = app.session.active() else { return };
            let size = [st.doc.size.width, st.doc.size.height];
            let doc_rect = xf.doc_rect(st.doc.bounds());
            let trail = app.trail.get_or_insert_with(|| crate::stroke_trail::Trail::new(size));
            trail.feed(&d.points, app.session.tools.brush.size);
            trail.draw(painter, doc_rect, xf.flip, col);
        }
        t if crate::vector_ui::is_shape_tool(t) => crate::vector_ui::draw_shape_preview(app, painter, xf, t, d.start, last, d.modifiers),
        Tool::RectMarquee | Tool::EllipseMarquee | Tool::ObjectSelection => {
            // Marching ants, visible on any pixels (#172).
            let (a, b) = marquee.unwrap_or((d.start, last));
            let r = Rect::from_two_pos(xf.to_screen(a[0] as f32, a[1] as f32), xf.to_screen(b[0] as f32, b[1] as f32));
            let r = Rect::from_min_max(r.min.round() + vec2(0.5, 0.5), r.max.round() + vec2(0.5, 0.5));
            let pts = if d.tool == Tool::EllipseMarquee {
                crate::tool_feedback::ellipse_points(r)
            } else {
                vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
            };
            crate::tool_feedback::draw_ants(painter, &pts, true);
        }
        Tool::Lasso => {
            let pts: Vec<Pos2> = d.points.iter().map(|p| xf.to_screen(p[0] as f32, p[1] as f32)).collect();
            crate::tool_feedback::draw_ants(painter, &pts, false);
        }
        Tool::Gradient => {
            let a = xf.to_screen(d.start[0] as f32, d.start[1] as f32);
            let b = xf.to_screen(last[0] as f32, last[1] as f32);
            painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
            painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
            painter.circle_filled(a, 3.0, Color32::WHITE);
            painter.circle_filled(b, 3.0, Color32::WHITE);
        }
        Tool::Type => {
            let r = Rect::from_two_pos(xf.to_screen(d.start[0] as f32, d.start[1] as f32), xf.to_screen(last[0] as f32, last[1] as f32));
            let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.0, Color32::WHITE)));
            painter.add(egui::Shape::dashed_line(&pts, Stroke::new(1.0, Color32::BLACK), 3.0, 3.0));
        }
        // The layers themselves follow the pointer (`move_ui`); the arrow only when they can't.
        Tool::Move if !crate::move_ui::showing(app) => {
            let off = vec2(((last[0] - d.start[0]) as f32) * xf.zoom, ((last[1] - d.start[1]) as f32) * xf.zoom);
            painter.arrow(xf.to_screen(d.start[0] as f32, d.start[1] as f32), off, Stroke::new(2.0, crate::theme::Tokens::get(painter.ctx()).accent));
        }
        _ => {}
    }
}

fn sample_eyedropper(app: &mut PhotocraftApp, x: f64, y: f64, mods: egui::Modifiers) {
    if let Ok(v) = app.run("document.pixel", json!({"x": x.floor(), "y": y.floor()})) {
        let color: Vec<f32> = serde_json::from_value(v).unwrap_or_default();
        if color.len() == 4 && color[3] > 0.0 {
            let key = if mods.alt { "background" } else { "foreground" };
            let _ = app.run("tools.setColors", json!({ key: [color[0], color[1], color[2], 1.0] }));
        }
    }
}

/// The body of a `Move` for every tool: tracked position, ⇧ constraint, the moving layer, and so
/// on. It does not feed the live stroke, so the canvas can push a whole frame's recovered samples
/// and update the live stroke once (see `canvas_view`).
fn tool_move(app: &mut PhotocraftApp, x: f64, y: f64, pressure: f32, mods: egui::Modifiers) {
    let tool = app.ui.tool;
    if tool == Tool::Type && app.drag.is_none() {
        crate::type_tool::pointer_move(app, x, y);
    }
    if tool == Tool::Pen {
        crate::vector_ui::pen_move(app, x, y);
    }
    let zoom = app.current_zoom();
    if let Some(d) = app.drag.as_mut().filter(|d| d.reposition) {
        d.track(mods);
        d.shift_to([x, y]);
    } else if let Some(d) = &mut app.drag {
        d.track(mods);
        // ⇧: straight 0/45/90° strokes, 45° gradient angles (stroke_constraint.rs).
        let last = d.points.last().map_or(d.start, |p| [p[0], p[1]]);
        let [x, y] = crate::stroke_constraint::constrain(d.tool, &mut d.constrain, d.start, last, [x, y], mods.shift, zoom);
        if d.points.last().is_none_or(|p| (p[0] - x).abs() + (p[1] - y).abs() > 0.25) {
            d.points.push([x, y, pressure as f64]);
            app.stylus.record_point();
        }
    }
}

/// Tool state machine. Shared by mouse input and automation.
pub fn tool_event(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) {
    // An Alt+right-drag armed for this press (`paint_mouse`): taken before anything else can
    // consume the event, so it never outlives the press it was armed for (#297).
    let armed = std::mem::take(&mut app.brush_resize_armed);
    // View › Snap / Snap To and smart guides (snap_ui.rs).
    let raw = ev;
    let ev = crate::snap_ui::filter_event(app, ev, mods);
    if crate::transform_tool::pointer(app, ev, mods) {
        return;
    }
    if crate::distort_ui::pointer(app, ev, mods) {
        return;
    }
    // Window › Modifier Keys: sticky Shift/⌘/⌥ act as held keys.
    let mods = crate::workspace_ui::sticky_mods(app, mods);
    // Control+Alt-drag or Alt+right-drag with a painting tool resizes the brush instead of
    // painting (#231, #297).
    if crate::brush_resize::pointer(app, ev, mods, armed) {
        return;
    }
    // Move tool: ⇧ locks the axis, ⌥ duplicates (move_mods.rs).
    let ev = crate::move_mods::filter_event(app, ev, mods);
    // Ruler, Count and Note tools.
    if crate::analysis_ui::pointer(app, ev, mods) {
        return;
    }
    // Slice and Slice Select tools.
    if crate::slice_ui::pointer(app, ev, mods) {
        return;
    }
    // Crop tool: draw, move and resize the frame.
    if crate::crop_ui::pointer(app, ev, mods) {
        return;
    }
    // Gradient tool, live mode: draw and edit Gradient Fill layers.
    if crate::gradient_ui::pointer(app, ev, mods) {
        return;
    }
    let tool = app.ui.tool;
    if tool == Tool::Eyedropper {
        match ev {
            ToolEvent::Down { x, y, .. } | ToolEvent::Move { x, y, .. } => {
                sample_eyedropper(app, x, y, mods);
                return;
            }
            ToolEvent::Up { .. } => return,
        }
    }
    // Move tool over a guide drags the guide (off the canvas deletes it).
    match ev {
        ToolEvent::Down { x, y, .. } if tool == Tool::Move => {
            if let Some((vertical, i)) = crate::rulers::guide_at(app, x, y) {
                app.guide_drag = Some(crate::rulers::GuideDrag { vertical, index: Some(i), pos: if vertical { x } else { y } });
                return;
            }
            // Auto-Select (or ⌘-click while it is off) picks the layer under the pointer first.
            if app.ui.tool_options.move_auto_select != mods.command {
                let target = app.ui.tool_options.move_target.clone();
                let mode = if mods.shift { "add" } else { "replace" };
                let _ = app.run("layer.pickAt", json!({"x": x, "y": y, "target": target, "mode": mode}));
            }
        }
        ToolEvent::Move { x, y, .. } => {
            if let Some(d) = app.guide_drag.as_mut().filter(|d| d.index.is_some()) {
                d.pos = if d.vertical { x } else { y };
                return;
            }
        }
        ToolEvent::Up { x, y } => {
            if let Some(mut d) = app.guide_drag.filter(|d| d.index.is_some()) {
                app.guide_drag = None;
                d.pos = if d.vertical { x } else { y };
                crate::rulers::finish_drag(app, d);
                return;
            }
        }
        _ => {}
    }
    match ev {
        ToolEvent::Down { x, y, pressure } => {
            // Painting a type, shape, Smart Object or fill layer asks to rasterize it first
            // (⌥-click with the Clone Stamp or Healing Brush only sets the source).
            let sets_source = matches!(tool, Tool::CloneStamp | Tool::Healing) && mods.alt;
            if !sets_source && crate::rasterize_prompt::intercept(app, tool, x, y, pressure) {
                return;
            }
            match tool {
                Tool::Pen => {
                    crate::vector_ui::pen_down(app, x, y);
                    return;
                }
                Tool::CloneStamp | Tool::Healing if mods.alt => {
                    crate::retouch_ui::set_source(app, x, y);
                    return;
                }
                Tool::MagicWand => {
                    let o = app.ui.tool_options.clone();
                    let mode = selection_mode(app, mods);
                    let _ = app.run("select.magicWand", json!({"x": x.floor(), "y": y.floor(), "tolerance": o.tolerance, "contiguous": o.contiguous, "antiAlias": o.anti_alias, "sampleAllLayers": o.sample_all_layers, "mode": mode}));
                    return;
                }
                Tool::MagicEraser => {
                    crate::eraser_ui::click(app, tool, x, y);
                    return;
                }
                Tool::PaintBucket => {
                    let o = app.ui.tool_options.clone();
                    let contents = if o.bucket_fill_pattern { "pattern" } else { "foreground" };
                    let _ = app.run("paint.bucket", json!({"x": x.floor(), "y": y.floor(), "tolerance": o.tolerance, "contiguous": o.contiguous, "antiAlias": o.anti_alias, "opacity": o.fill_opacity, "contents": contents, "target": paint_target(app)}));
                    return;
                }
                Tool::PolygonLasso => {
                    // Click adds a vertex; clicking near the first vertex closes the polygon.
                    let close = app.ui.polygon.first().is_some_and(|p0| {
                        app.ui.polygon.len() >= 3 && ((p0[0] - x).powi(2) + (p0[1] - y).powi(2)).sqrt() < 8.0 / app.current_zoom().max(0.01) as f64
                    });
                    if close {
                        commit_polygon(app, mods);
                    } else {
                        app.ui.polygon.push([x, y]);
                    }
                    return;
                }
                Tool::Type if crate::type_tool::pointer_down(app, x, y, mods.shift) => return,
                _ => {}
            }
            crate::paint_mouse::sync_tool_smoothing(app);
            let erase = tool == Tool::Eraser || std::mem::take(&mut app.secondary_erase);
            // ⇧-click after a stroke: a straight line from where it ended (stroke_constraint.rs).
            let active = app.session.active().map(|st| st.doc.id);
            let from = app.last_stroke_end.filter(|(doc, _)| mods.shift && crate::stroke_constraint::connects(tool) && Some(*doc) == active).map(|(_, p)| p);
            let mut points = vec![[x, y, pressure as f64]];
            if let Some(p) = from {
                points.insert(0, [p[0], p[1], pressure as f64]);
            }
            app.drag = Some(Drag::new(tool, from.unwrap_or([x, y]), points, mods, erase));
            app.trail = None;
            app.stylus.begin_stroke();
            if from.is_some() {
                app.stylus.record_point();
            }
            app.live_stroke = if strokes_live(tool) { begin_live_stroke(app) } else { None };
        }
        ToolEvent::Move { x, y, pressure } => {
            tool_move(app, x, y, pressure, mods);
            feed_live_stroke(app);
        }
        ToolEvent::Up { x, y } => {
            if tool == Tool::Type
                && let Some(e) = app.ui.text_edit.as_mut()
            {
                e.dragging = false;
            }
            if tool == Tool::Pen {
                crate::vector_ui::pen_up(app);
            }
            let zoom = app.current_zoom();
            let Some(mut d) = app.drag.take() else { return };
            d.track(mods);
            if d.reposition {
                d.shift_to([x, y]);
            }
            let last = d.points.last().map_or(d.start, |p| [p[0], p[1]]);
            let [x, y] = crate::stroke_constraint::constrain(d.tool, &mut d.constrain, d.start, last, [x, y], mods.shift, zoom);
            // A Move-tool click (released where it was pressed) selects, it never moves: snapping
            // the release point would otherwise nudge the layer onto a nearby edge.
            if d.tool == Tool::Move && d.points.len() < 2 && matches!(raw, ToolEvent::Up { x, y } if [x, y] == d.start) {
                app.move_preview = None;
                crate::move_mods::finish(app);
                return;
            }
            if d.points.last().is_none_or(|p| p[0] != x || p[1] != y) {
                d.points.push([x, y, d.points.last().map_or(1.0, |p| p[2])]);
                app.stylus.record_point();
            }
            finish_gesture(app, d);
            crate::move_mods::finish(app);
        }
    }
}

fn finish_gesture(app: &mut PhotocraftApp, d: Drag) {
    let end = d.points.last().copied().unwrap_or([d.start[0], d.start[1], 1.0]);
    // Where the next ⇧-click line starts.
    if crate::stroke_constraint::connects(d.tool)
        && let Some(st) = app.session.active()
    {
        app.last_stroke_end = Some((st.doc.id, [end[0], end[1]]));
    }
    if crate::eraser_ui::finish_stroke(app, d.tool, &d.points) || crate::retouch_ui::finish_stroke(app, d.tool, &d.points, d.modifiers) {
        return;
    }
    match d.tool {
        Tool::ObjectSelection => crate::retouch_ui::finish_object_selection(app, d.start, [end[0], end[1]], d.modifiers),
        t if crate::vector_ui::is_shape_tool(t) => crate::vector_ui::finish_shape(app, t, d.start, [end[0], end[1]], d.modifiers),
        Tool::PathSelection => crate::vector_ui::path_selection_finish(app, d.start, [end[0], end[1]]),
        Tool::Type => crate::type_tool::pointer_up(app, d.start, [end[0], end[1]]),
        Tool::Brush | Tool::Pencil | Tool::Eraser => {
            let live = app.live_stroke.take();
            let mut p = stroke_params(app, d.tool, d.erase, &app.stylus.stroke_points(&d.points));
            if let Some(l) = &live {
                p["seed"] = json!(l.stroke.seed);
            }
            // The canvas already shows the stroke: let the commit's damage rect refresh it rather
            // than recompositing the whole document.
            if app.run(stroke_command(d.tool), p).is_ok()
                && let Some(l) = live
            {
                let display_key = app.session.active().map_or(0, |st| canvas_display(app, &st.doc).1);
                if let Some(c) = app.canvases.get_mut(&l.doc) {
                    // Raw preview key 0 = the document itself (its colour display folded in).
                    if l.since(c.preview_key ^ display_key).is_some() {
                        c.preview_key = display_key;
                    }
                    if l.since(c.tex_preview_key ^ display_key).is_some() {
                        c.tex_preview_key = display_key;
                    }
                }
            }
        }
        Tool::RectMarquee | Tool::EllipseMarquee => {
            let (a, b) = marquee_corners(&app.ui.tool_options, &d, [end[0], end[1]]);
            let [x0, y0, x1, y1] = marquee_px(a, b);
            if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
                if app.session.is_enabled("select.deselect") {
                    let _ = app.run("select.deselect", json!({}));
                }
                return;
            }
            let mode = selection_mode(app, d.modifiers);
            let (aa, feather) = (app.ui.tool_options.anti_alias, app.ui.tool_options.feather);
            let _ = app.run("select.rect", json!({"x": x0, "y": y0, "width": x1 - x0, "height": y1 - y0, "mode": mode, "ellipse": d.tool == Tool::EllipseMarquee, "antiAlias": aa, "feather": feather}));
        }
        Tool::Lasso => {
            let pts: Vec<[f64; 2]> = d.points.iter().map(|p| [p[0], p[1]]).collect();
            if pts.len() >= 3 {
                let mode = selection_mode(app, d.modifiers);
                let _ = app.run("select.lasso", json!({"points": pts, "mode": mode, "antiAlias": app.ui.tool_options.anti_alias}));
            } else if app.session.is_enabled("select.deselect") {
                let _ = app.run("select.deselect", json!({}));
            }
        }
        Tool::Gradient => {
            if (end[0] - d.start[0]).abs() + (end[1] - d.start[1]).abs() >= 2.0 {
                let o = app.ui.tool_options.clone();
                let fg = app.session.tools.foreground;
                let bg = app.session.tools.background;
                let _ = app.run(
                    "paint.gradient",
                    json!({"from": [d.start[0], d.start[1]], "to": [end[0], end[1]], "style": o.gradient_style, "reverse": o.gradient_reverse, "dither": o.gradient_dither, "colors": [hex(fg), hex(bg)], "opacity": o.fill_opacity, "target": paint_target(app)}),
                );
            }
        }
        Tool::Move => {
            let (dx, dy) = ((end[0] - d.start[0]).round(), (end[1] - d.start[1]).round());
            crate::move_ui::finish(app, dx, dy);
        }
        _ => {}
    }
}

/// Extra OS windows showing documents (multi-window / multi-monitor).
pub fn extra_windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let wins = app.ui.windows.clone();
    for w in wins.into_iter().filter(|w| w.open) {
        let Some(st) = app.session.documents().get(w.document) else { continue };
        let title = format!("{} — window {}", st.doc.name, w.id);
        let vid = egui::ViewportId::from_hash_of(("docwin", w.id));
        let builder = egui::ViewportBuilder::default().with_title(title).with_inner_size([800.0, 600.0]);
        let mut view = w.view.clone();
        let mut close = false;
        ctx.show_viewport_immediate(vid, builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                close = true;
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(crate::theme::Tokens::get(ui.ctx()).canvas)).show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                view = canvas_view(app, ui, w.document, rect, view.clone(), false);
            });
        });
        if let Some(win) = app.ui.windows.iter_mut().find(|x| x.id == w.id) {
            win.view = view;
            if close {
                win.open = false;
            }
        }
    }
    app.ui.windows.retain(|w| w.open);
}

/// Selection mode from the options bar, overridden by modifier keys (⇧ add, ⌥ subtract, ⇧⌥ intersect).
/// The cursor badge announces the same mode (`tool_feedback`).
fn selection_mode(app: &PhotocraftApp, m: egui::Modifiers) -> &'static str {
    crate::tool_feedback::selection_mode(Tool::Lasso, app.ui.selection_mode, m)
}

/// Close the polygonal lasso and make the selection.
pub fn commit_polygon(app: &mut PhotocraftApp, mods: egui::Modifiers) {
    let pts = std::mem::take(&mut app.ui.polygon);
    if pts.len() >= 3 {
        let mode = selection_mode(app, mods);
        let _ = app.run("select.lasso", json!({"points": pts, "mode": mode, "antiAlias": app.ui.tool_options.anti_alias}));
    }
}

/// Apply the crop tool's rectangle.
pub fn commit_crop(app: &mut PhotocraftApp) {
    let Some(r) = app.ui.crop_rect.take() else { return };
    let (x, y) = (r[0].round(), r[1].round());
    let (w, h) = ((r[2] - r[0]).round().max(1.0), (r[3] - r[1]).round().max(1.0));
    let delete = app.ui.tool_options.crop_delete;
    if app.run("image.crop", json!({"x": x, "y": y, "width": w, "height": h, "deleteCroppedPixels": delete})).is_ok()
        && let Some(i) = app.session.active_index()
    {
        app.ui.views[i].fit_pending = true;
    }
}

/// "mask" when the Layers panel targets (or the canvas shows) the active layer's mask, else "pixels".
pub fn paint_target(app: &PhotocraftApp) -> serde_json::Value {
    use photocraft_engine::channel_cmds::ChannelTarget;
    let Some(st) = app.session.active() else { return json!("pixels") };
    // A targeted alpha channel (Channels panel) or Quick Mask mode wins over the layer.
    match st.channel_view.target {
        ChannelTarget::Alpha(i) if i < st.doc.channels.len() => return json!({ "channel": i }),
        ChannelTarget::Composite if st.doc.quick_mask.is_some() => return json!("quickMask"),
        _ => {}
    }
    let has_mask = st.active_layer.and_then(|id| st.doc.layer(id)).is_some_and(|l| l.mask.is_some());
    // Viewing the mask (⌥-click its thumbnail, #196) paints the mask.
    let viewing = photocraft_engine::mask_view_cmds::current(st).is_some();
    json!(if (app.ui.mask_target || viewing) && has_mask { "mask" } else { "pixels" })
}

/// `#rrggbb` for an sRGB colour (the engine's colour parameter notation).
fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_moves_are_bounded_by_the_press_and_release() {
        use egui::{Event, PointerButton, pos2};
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let events = [
            Event::PointerMoved(pos2(1.0, 0.0)),
            button(pos2(2.0, 0.0), true),
            Event::PointerMoved(pos2(3.0, 0.0)),
            Event::PointerMoved(pos2(4.0, 0.0)),
            button(pos2(5.0, 0.0), false),
            Event::PointerMoved(pos2(6.0, 0.0)),
        ];
        // The press is in this frame: only the moves between press and release count.
        assert_eq!(pointer_moves(&events, PointerButton::Primary, false), vec![pos2(3.0, 0.0), pos2(4.0, 0.0)]);
        // The button was already down when the frame began: the move before the release counts,
        // the one after it does not.
        assert_eq!(pointer_moves(&events, PointerButton::Primary, true), vec![pos2(1.0, 0.0), pos2(3.0, 0.0), pos2(4.0, 0.0)]);
    }

    #[test]
    fn freehand_tools_are_the_ones_that_follow_a_path() {
        for t in
            [Tool::Brush, Tool::Pencil, Tool::Eraser, Tool::BackgroundEraser, Tool::CloneStamp, Tool::Smudge, Tool::Dodge, Tool::Lasso, Tool::QuickSelection]
        {
            assert!(freehand_tool(t), "{t:?} paints or retouches along a path");
        }
        for t in [Tool::Move, Tool::Eyedropper, Tool::Gradient, Tool::Crop, Tool::RectMarquee, Tool::Type, Tool::Hand] {
            assert!(!freehand_tool(t), "{t:?} is driven by the pointer's latest position");
        }
    }

    #[test]
    fn clone_stamp_centre_appears_only_with_option() {
        assert!(!brush_tip_centre(Tool::CloneStamp, false, false, 20.0));
        assert!(brush_tip_centre(Tool::CloneStamp, true, false, 20.0));
        assert!(brush_tip_centre(Tool::Healing, true, false, 20.0));
        assert!(!brush_tip_centre(Tool::Healing, false, false, 20.0));
        assert!(brush_tip_centre(Tool::CloneStamp, false, true, 20.0));
        assert!(brush_tip_centre(Tool::Brush, false, false, 20.0));
        assert!(!brush_tip_centre(Tool::QuickSelection, false, false, 20.0));
        assert!(brush_tip_centre(Tool::BackgroundEraser, false, false, 2.0));
    }

    #[test]
    fn view_transform_roundtrip() {
        for flip in [false, true] {
            let xf = ViewXform { rect: Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0)), zoom: 2.5, center: [320.0, 240.0], flip };
            let s = xf.to_screen(10.0, 20.0);
            let d = xf.to_doc(s);
            assert!((d[0] - 10.0).abs() < 1e-3 && (d[1] - 20.0).abs() < 1e-3);
            assert_eq!(xf.to_screen(320.0, 240.0), xf.rect.center());
            // Flipped, document x grows to the left.
            assert_eq!(xf.to_screen(330.0, 240.0).x > xf.rect.center().x, !flip);
            let r = xf.doc_rect(DRect::new(0, 0, 10, 10));
            assert!(r.width() > 0.0 && r.height() > 0.0);
        }
    }

    #[test]
    fn eyedropper_drag_updates_the_sampled_colour() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 40, "height": 20, "background": "transparent"})).unwrap();
        app.run("shape.create", json!({"kind": "rect", "rect": [0, 0, 20, 20], "fill": "#ff0000"})).unwrap();
        app.run("shape.create", json!({"kind": "rect", "rect": [20, 0, 20, 20], "fill": "#00ff00"})).unwrap();
        app.ui.tool = Tool::Eyedropper;

        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 10.0, pressure: 1.0 }, egui::Modifiers::NONE);
        assert!(app.session.tools.foreground[0] > 0.99 && app.session.tools.foreground[1] < 0.01);

        tool_event(&mut app, ToolEvent::Move { x: 30.0, y: 10.0, pressure: 1.0 }, egui::Modifiers::NONE);
        assert!(app.session.tools.foreground[1] > 0.99 && app.session.tools.foreground[0] < 0.01);
    }

    #[test]
    fn brush_drag_shows_the_real_stroke_and_commits_it() {
        // The drag used to draw a hard, flat stand-in and only showed the soft brush on release.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 120, "height": 60, "background": "transparent"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 20, "hardness": 0.0}})).unwrap();
        app.ui.tool = Tool::Brush;
        let m = egui::Modifiers::NONE;
        let alpha = |d: &Document, x, y| d.layers[0].surface().unwrap().rgba(x, y)[3];
        let rev = app.session.documents()[0].revision;
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 30.0, pressure: 1.0 }, m);
        for x in [40.0, 70.0, 100.0] {
            tool_event(&mut app, ToolEvent::Move { x, y: 30.0, pressure: 1.0 }, m);
        }
        let (shown, key) = display_doc(&mut app, 0);
        assert_ne!(key, 0);
        assert!(alpha(&shown, 40, 30) > 0.5 && (0.01..0.5).contains(&alpha(&shown, 40, 38)), "soft stroke while drawing");
        assert_eq!(alpha(&app.session.documents()[0].doc, 40, 30), 0.0, "not committed yet");
        // The canvas redraws only what the stroke touched.
        let dk = canvas_display(&app, &app.session.documents()[0].doc).1;
        let d = damage_since(&app, 0, (rev, dk), (rev, key), dk, None).unwrap();
        assert!(d.contains(40, 30) && !d.contains(40, 2) && d.width() < 120, "{d:?}");
        tool_event(&mut app, ToolEvent::Up { x: 100.0, y: 30.0 }, m);
        let doc = app.session.documents()[0].doc.clone();
        assert!(app.live_stroke.is_none() && display_doc(&mut app, 0).1 == 0);
        assert_eq!((alpha(&doc, 40, 30), alpha(&doc, 40, 38)), (alpha(&shown, 40, 30), alpha(&shown, 40, 38)), "commit matches the preview");
    }

    #[test]
    fn live_stroke_uses_pen_tilt_and_updates_a_reduced_texture_partially() {
        // Pen tilt drives the size; the preview must use the tilt the commit gets. The document
        // is wider than MAX_TEXTURE, so the CPU texture is reduced (factor 2) and each step
        // must update only the reduced pixels it touched.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 4097, "height": 90, "background": "transparent"})).unwrap();
        let tilt = json!({"size": 30, "hardness": 1.0, "spacing": 0.05, "shapeDynamics": {"enabled": true, "size": {"control": "penTilt"}}});
        app.run("tools.setBrush", json!({ "brush": tilt })).unwrap();
        app.ui.tool = Tool::Brush;
        assert_eq!(ensure_texture(&mut app, &ctx, 0).map(|t| t.1), Some(0.5));
        let m = egui::Modifiers::NONE;
        app.stylus.feed.set(Some(crate::stylus::PenSample { pressure: 1.0, tilt_x: 60.0, tilt_y: 0.0, rotation: 0.0, eraser: false }));
        tool_event(&mut app, ToolEvent::Down { x: 100.0, y: 45.0, pressure: 1.0 }, m);
        for x in [300.0, 600.0, 900.0] {
            tool_event(&mut app, ToolEvent::Move { x, y: 45.0, pressure: 1.0 }, m);
            ensure_texture(&mut app, &ctx, 0);
            assert_eq!(app.perf.last_refresh, "rect");
            assert!(app.perf.last_refresh_px < 4097 * 90 / 4, "{}", app.perf.last_refresh_px);
        }
        let shown = display_doc(&mut app, 0).0;
        tool_event(&mut app, ToolEvent::Up { x: 900.0, y: 45.0 }, m);
        ensure_texture(&mut app, &ctx, 0);
        assert_eq!(app.perf.last_refresh, "rect", "the commit refreshes only the stroke");
        let doc = app.session.documents()[0].doc.clone();
        let (a, b) = (doc.layers[0].surface().unwrap(), shown.layers[0].surface().unwrap());
        // 60° tilt shrinks the dab well below 30 px: the preview shows that size, as committed.
        assert!(a.rgba(500, 45)[3] > 0.5 && a.rgba(500, 45 + 12)[3] == 0.0);
        // (The smoothed tail catches up to the end point only when the stroke finishes.)
        assert!((0..90).all(|y| (0..700).all(|x| a.rgba(x, y) == b.rgba(x, y))), "commit matches the preview");
    }

    /// Brush drag along y = 40 with one canvas frame per pointer move; returns the document the
    /// canvas showed at the last move, the committed one, and whether the release refreshed only
    /// the stroke's rectangle.
    fn drag_frames(smoothing: f32, xs: &[f64]) -> (std::sync::Arc<Document>, std::sync::Arc<Document>, bool, PhotocraftApp) {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 200, "height": 80, "background": "transparent"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 16, "hardness": 0.5}})).unwrap();
        // What the options bar's Smoothing field writes.
        app.session.tools.brush.smoothing.amount = smoothing;
        app.ui.tool = Tool::Brush;
        ensure_texture(&mut app, &ctx, 0);
        let m = egui::Modifiers::NONE;
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 40.0, pressure: 1.0 }, m);
        for &x in xs {
            tool_event(&mut app, ToolEvent::Move { x, y: 40.0 + (x / 7.0).sin() * 8.0, pressure: 1.0 }, m);
            ensure_texture(&mut app, &ctx, 0);
        }
        let live = display_doc(&mut app, 0).0;
        let last = *xs.last().unwrap();
        tool_event(&mut app, ToolEvent::Up { x: last, y: 40.0 + (last / 7.0).sin() * 8.0 }, m);
        let (next, key) = display_doc(&mut app, 0);
        assert_eq!(key, 0, "the frame after release shows the committed document");
        ensure_texture(&mut app, &ctx, 0);
        let partial = app.perf.last_refresh == "rect";
        (live, next, partial, app)
    }

    fn same_pixels(a: &Document, b: &Document) -> bool {
        let (a, b) = (a.layers[0].surface().unwrap(), b.layers[0].surface().unwrap());
        (0..80).all(|y| (0..200).all(|x| a.rgba(x, y) == b.rgba(x, y)))
    }

    #[test]
    fn release_shows_nothing_new_without_smoothing() {
        // #73: at 0 % the stroke follows the pointer; the last frame drawn while dragging is
        // exactly the committed stroke, so release changes nothing on screen.
        let (live, done, partial, app) = drag_frames(0.0, &[40.0, 70.0, 100.0, 130.0, 160.0]);
        assert!(same_pixels(&live, &done), "preview at the last move = committed stroke");
        assert!(partial, "the handover refreshes only the stroke");
        let p = app.session.journal.iter().rev().find(|(id, _)| id == "paint.stroke").map(|(_, p)| p.clone()).unwrap();
        assert!(p.get("smoothing").is_none(), "the commit uses the session brush's smoothing, not a hard-coded one");
    }

    #[test]
    fn smoothed_stroke_shows_its_catch_up_tail_while_drawing() {
        // #73: with smoothing the brush lags behind the pointer and catches up at the end; the
        // preview draws that tail live, so no frame after release is missing the end.
        let xs = [30.0, 50.0, 70.0, 90.0, 110.0, 130.0, 150.0, 170.0];
        let (live, done, partial, _) = drag_frames(0.5, &xs);
        assert!(same_pixels(&live, &done), "preview at the last move = committed stroke, tail included");
        assert!(partial);
        let end = |d: &Document| (0..200).rev().find(|&x| (0..80).any(|y| d.layers[0].surface().unwrap().rgba(x, y)[3] > 0.0)).unwrap();
        assert!(end(&live) >= 170, "the end reaches the pointer while drawing ({})", end(&live));
        // The options-bar value reaches the stroke: 50 % smooths the wiggle, 0 % doesn't.
        let (_, rough, _, _) = drag_frames(0.0, &xs);
        assert!(!same_pixels(&rough, &done), "smoothing changes the stroke");
        // And the commit is what `paint.stroke` gives with that smoothing.
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 200, "height": 80, "background": "transparent"})).unwrap();
        s.execute("tools.setBrush", json!({"brush": {"size": 16, "hardness": 0.5, "smoothing": {"amount": 0.5}}})).unwrap();
        let mut pts = vec![json!([10.0, 40.0, 1.0])];
        pts.extend(xs.iter().map(|&x| json!([x, 40.0 + (x / 7.0).sin() * 8.0, 1.0])));
        // (Round tips without dynamics draw the same for any seed.)
        s.execute("paint.stroke", json!({"points": pts, "seed": 0})).unwrap();
        assert!(same_pixels(&s.active().unwrap().doc, &done));
    }

    #[test]
    fn smoothing_preview_redraws_the_tail_each_step() {
        // The tail drawn at one step must not linger once the brush moves on: a sharp turn would
        // leave a stale tail behind if it weren't restored.
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 200, "height": 120, "background": "transparent"})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 10, "hardness": 1.0, "smoothing": {"amount": 0.6}}})).unwrap();
        app.ui.tool = Tool::Brush;
        let m = egui::Modifiers::NONE;
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 20.0, pressure: 1.0 }, m);
        for (x, y) in [(60.0, 20.0), (110.0, 20.0), (110.0, 70.0), (110.0, 110.0), (60.0, 110.0)] {
            tool_event(&mut app, ToolEvent::Move { x, y, pressure: 1.0 }, m);
        }
        let live = display_doc(&mut app, 0).0;
        tool_event(&mut app, ToolEvent::Up { x: 60.0, y: 110.0 }, m);
        let done = app.session.documents()[0].doc.clone();
        let (a, b) = (live.layers[0].surface().unwrap(), done.layers[0].surface().unwrap());
        assert!((0..120).all(|y| (0..200).all(|x| a.rgba(x, y) == b.rgba(x, y))), "no stale tails in the preview");
    }

    /// Shapes the drag preview paints for `app` this frame.
    fn drag_preview_shapes(app: &mut PhotocraftApp, ctx: &egui::Context, zoom: f32) -> Vec<egui::epaint::ClippedShape> {
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx();
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
            let painter = ctx.layer_painter(egui::LayerId::background()).with_clip_rect(rect);
            let xf = ViewXform { rect, zoom, center: [60.0, 30.0], flip: false };
            draw_drag_preview(app, &painter, &xf);
        });
        let egui::FullOutput { mut textures_delta, shapes, .. } = out;
        textures_delta.clear();
        shapes
    }

    #[test]
    fn brush_drags_paint_no_stand_in_shape_over_the_canvas() {
        // #189: v0.1 drew the stroke being dragged as a foreground-coloured egui polyline as wide
        // as the brush; zoomed in, egui tessellated it into hard black wedges fanning out from the
        // start. The canvas shows the real stroke (`LiveStroke`) and nothing is drawn over it.
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 120, "height": 60})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 30, "hardness": 0.0}})).unwrap();
        for tool in [Tool::Brush, Tool::Eraser] {
            app.ui.tool = tool;
            let idle = drag_preview_shapes(&mut app, &ctx, 12.0).len();
            tool_event(&mut app, ToolEvent::Down { x: 20.0, y: 30.0, pressure: 1.0 }, egui::Modifiers::NONE);
            for i in 1..40 {
                let t = f64::from(i);
                tool_event(&mut app, ToolEvent::Move { x: 20.0 + t, y: 30.0 + (t / 3.0).sin() * 4.0, pressure: 1.0 }, egui::Modifiers::NONE);
            }
            assert!(app.live_stroke.is_some(), "{tool:?}: the canvas shows the live stroke");
            assert_eq!(drag_preview_shapes(&mut app, &ctx, 12.0).len(), idle, "{tool:?}: no overlay while dragging");
            tool_event(&mut app, ToolEvent::Up { x: 59.0, y: 30.0 }, egui::Modifiers::NONE);
        }
    }

    #[test]
    fn retouch_drags_show_the_footprint_without_wedges() {
        // The retouching tools' trail was the same brush-wide polyline (#189): now a mask of the
        // footprint, with nothing outside the brush radius of the path, however small the steps.
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 120, "height": 60})).unwrap();
        app.run("tools.setBrush", json!({"brush": {"size": 30}})).unwrap();
        app.ui.tool = Tool::Dodge;
        tool_event(&mut app, ToolEvent::Down { x: 20.0, y: 30.0, pressure: 1.0 }, egui::Modifiers::NONE);
        let mut pts = vec![[20.0, 30.0]];
        for i in 1..80 {
            let t = f64::from(i) * 0.5;
            let p = [20.0 + t, 30.0 + (t / 2.0).sin() * 3.0];
            tool_event(&mut app, ToolEvent::Move { x: p[0], y: p[1], pressure: 1.0 }, egui::Modifiers::NONE);
            pts.push(p);
            if i % 20 == 0 {
                // One mesh for the trail, however long the drag.
                let shapes = drag_preview_shapes(&mut app, &ctx, 12.0);
                assert!(!shapes.iter().any(|s| matches!(s.shape, egui::Shape::Path(_))), "no polyline");
            }
        }
        drag_preview_shapes(&mut app, &ctx, 12.0);
        let trail = app.trail.as_ref().unwrap();
        assert_eq!(trail.scale(), 1.0);
        for y in 0..60 {
            for x in 0..120 {
                let d = pts.iter().map(|p| (x as f64 + 0.5 - p[0]).hypot(y as f64 + 0.5 - p[1])).fold(f64::MAX, f64::min);
                if d < 14.0 {
                    assert_eq!(trail.coverage(x, y), 255, "({x}, {y}) inside the footprint");
                } else if d > 16.0 {
                    assert_eq!(trail.coverage(x, y), 0, "({x}, {y}) outside the footprint");
                }
            }
        }
        tool_event(&mut app, ToolEvent::Up { x: 59.5, y: 30.0 }, egui::Modifiers::NONE);
        drag_preview_shapes(&mut app, &ctx, 12.0);
        assert!(app.trail.is_none(), "the trail ends with the drag");
    }

    #[test]
    fn layer_style_dialog_previews_live_and_cancel_restores() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        let fx = |d: &Document| d.layers.iter().map(|l| l.effects.items.len()).sum::<usize>();
        let id = crate::layer_style::open(&mut app, Some("colorOverlay")).unwrap();
        let (shown, key) = display_doc(&mut app, 0);
        assert_eq!((fx(&shown), fx(&app.session.documents()[0].doc)), (1, 0), "previewed, not committed");
        app.ui.dialog_mut(id).unwrap().fields.insert("on:stroke".into(), json!(true));
        let (shown, key2) = display_doc(&mut app, 0);
        assert_eq!(fx(&shown), 2);
        assert_ne!(key, key2, "an edit re-renders the canvas");
        app.ui.close_dialog(id);
        let (shown, key) = display_doc(&mut app, 0);
        assert_eq!((fx(&shown), key), (0, 0));
        assert!(app.style_preview.is_none());
    }

    #[test]
    fn zoom_steps_monotone() {
        assert_eq!(zoom_step(1.0, 1), 2.0);
        assert_eq!(zoom_step(1.0, -1), 0.6667);
        assert_eq!(zoom_step(0.4, 1), 0.5);
        assert_eq!(zoom_step(32.0, 1), 32.0);
    }

    #[test]
    fn damage_grows_by_nested_effect_reach() {
        use photocraft_doc::{Effect, Layer};
        let fmt = photocraft_color::PixelFormat::RGBA8;
        assert_eq!(effect_reach(&[Layer::raster("plain", fmt)]), 0);
        let mut inner = Layer::raster("inner", fmt);
        inner.effects.items.push(Effect::default_drop_shadow());
        let m = photocraft_compose::effects::margin(&inner);
        let mut group = Layer::group("g", vec![inner.clone()]);
        group.effects.items.push(Effect::default_drop_shadow());
        assert_eq!(effect_reach(std::slice::from_ref(&inner)), m);
        // A child's edit moves the group's shape, whose effects reach further.
        assert_eq!(effect_reach(&[group.clone()]), 2 * m);
        group.visible = false;
        assert_eq!(effect_reach(&[group]), 0);
    }
}

#[cfg(test)]
mod transform_controls_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn move_transform_controls_start_free_transform_for_vector_shapes() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 200, "height": 200})).unwrap();
        app.session.execute("shape.create", json!({"kind": "rect", "rect": [20, 30, 80, 40], "fill": "#ff0000"})).unwrap();
        app.sync_views();
        app.ui.tool = Tool::Move;
        app.ui.tool_options.move_show_transform = true;

        let xf = ViewXform { rect: Rect::from_min_size(Pos2::ZERO, vec2(200.0, 200.0)), zoom: 1.0, center: [100.0, 100.0], flip: false };
        let r = transform_controls_rect(&app, &xf).expect("shape layers have transform bounds");
        assert!(transform_controls_hit(r, r.right_bottom()));

        let ctx = egui::Context::default();
        assert!(begin_transform_controls_at(&mut app, &ctx, &xf, r.right_bottom()));
        let t = app.ui.transform.as_ref().expect("control press starts Free Transform");
        assert_eq!(t.rect, [20.0, 30.0, 100.0, 70.0]);
    }

    #[test]
    fn transform_control_hit_keeps_move_interior_free_and_has_rotation_band() {
        let r = Rect::from_min_max(pos2(20.0, 30.0), pos2(100.0, 70.0));
        assert!(transform_controls_hit(r, r.left_top()));
        assert!(transform_controls_hit(r, pos2(60.0, 18.0)));
        assert!(!transform_controls_hit(r, r.center()));
        assert!(!transform_controls_hit(r, pos2(60.0, 4.0)));
    }
}

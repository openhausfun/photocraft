//! Zoom tool drags (#171).
//!
//! With Scrubby Zoom on (the default, as in Photoshop) press-and-drag zooms continuously: right
//! zooms in, left zooms out, anchored at the point where the button went down. With it off, the
//! drag draws a rectangle and the release zooms to fit it. A click (no drag) still steps the zoom
//! in, or out with ⌥ (handled by the canvas).

use egui::{Pos2, Rect, vec2};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::paint_mouse::Buttons;
use crate::state::View;

/// Zoom limits of the canvas (1 % … 6400 %).
pub const MIN_ZOOM: f32 = 0.01;
pub const MAX_ZOOM: f32 = 64.0;
/// Horizontal drag (screen points) that doubles, or halves, the zoom.
pub const DOUBLING: f32 = 100.0;

/// A Zoom-tool drag in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ZoomDrag {
    /// Screen point where the button went down.
    anchor: Pos2,
    /// The document point under it when the drag began (it stays under the anchor).
    doc: [f64; 2],
    zoom0: f32,
    /// Scrubby Zoom off: a zoom rectangle, its current corner.
    rect_to: Option<Pos2>,
}

fn id() -> egui::Id {
    egui::Id::new("pc-zoom-tool-drag")
}

/// The scrubby zoom for a horizontal drag of `dx` points from a press at zoom `zoom0`.
pub fn scrub_zoom(zoom0: f32, dx: f32) -> f32 {
    // ±40 doublings already span far more than the zoom range; it keeps the power finite.
    let e = if dx.is_finite() { (dx / DOUBLING).clamp(-40.0, 40.0) } else { 0.0 };
    let z = zoom0 * e.exp2();
    if z.is_finite() { z.clamp(MIN_ZOOM, MAX_ZOOM) } else { 1.0 }
}

/// Set `view` to `zoom` with document point `doc` shown at screen point `screen`.
pub fn anchor_view(view: &mut View, xf: &ViewXform, screen: Pos2, doc: [f64; 2], zoom: f32) {
    view.zoom = zoom;
    let d = (screen - xf.rect.center()) / zoom;
    let dx = if xf.flip { -d.x } else { d.x };
    view.center = [doc[0] as f32 - dx, doc[1] as f32 - d.y];
}

/// Route the canvas buttons for the Zoom tool. Returns true when it consumed the drag (the
/// canvas then handles only the click).
pub fn drag(app: &PhotocraftApp, ctx: &egui::Context, view: &mut View, xf: &ViewXform, b: &Buttons, pointer: Option<Pos2>) -> bool {
    let scrubby = app.ui.tool_options.zoom_scrubby;
    if b.started {
        let anchor = ctx.input(|i| i.pointer.press_origin()).filter(|p| xf.rect.contains(*p)).or(pointer);
        if let Some(anchor) = anchor {
            let z = ZoomDrag { anchor, doc: xf.to_doc(anchor), zoom0: view.zoom, rect_to: (!scrubby).then_some(anchor) };
            ctx.data_mut(|d| d.insert_temp(id(), z));
        }
    }
    let Some(mut z) = ctx.data(|d| d.get_temp::<ZoomDrag>(id())) else { return b.started || b.dragged || b.stopped };
    if let Some(p) = pointer.filter(|_| b.dragged || b.stopped) {
        match z.rect_to.as_mut() {
            Some(to) => *to = p,
            None => anchor_view(view, xf, z.anchor, z.doc, scrub_zoom(z.zoom0, p.x - z.anchor.x)),
        }
        ctx.data_mut(|d| d.insert_temp(id(), z));
    }
    if b.stopped {
        ctx.data_mut(|d| d.remove::<ZoomDrag>(id()));
        if let Some(to) = z.rect_to {
            zoom_to_rect(view, xf, Rect::from_two_pos(z.anchor, to), z.anchor, ctx.input(|i| i.modifiers.alt));
        }
    }
    true
}

/// Scrubby Zoom off: zoom so the dragged rectangle fills the canvas (a tiny one steps the zoom
/// at the press point; ⌥ steps out).
fn zoom_to_rect(view: &mut View, xf: &ViewXform, r: Rect, anchor: Pos2, out: bool) {
    if out || r.width() < 4.0 || r.height() < 4.0 {
        let doc = xf.to_doc(anchor);
        anchor_view(view, xf, anchor, doc, crate::canvas::zoom_step(view.zoom, if out { -1 } else { 1 }).clamp(MIN_ZOOM, MAX_ZOOM));
        return;
    }
    let k = (xf.rect.width() / r.width()).min(xf.rect.height() / r.height());
    let zoom = (view.zoom * k).clamp(MIN_ZOOM, MAX_ZOOM);
    let c = xf.to_doc(r.center());
    anchor_view(view, xf, xf.rect.center(), c, zoom);
}

/// Scrubby Zoom off: the zoom rectangle being dragged, as marching ants.
pub fn draw(ctx: &egui::Context, painter: &egui::Painter) {
    let Some(z) = ctx.data(|d| d.get_temp::<ZoomDrag>(id())) else { return };
    let Some(to) = z.rect_to else { return };
    let r = Rect::from_two_pos(z.anchor, to);
    let r = Rect::from_min_max(r.min.round() + vec2(0.5, 0.5), r.max.round() + vec2(0.5, 0.5));
    crate::tool_feedback::draw_ants(painter, &[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()], true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton};
    use egui_kittest::Harness;
    use serde_json::json;

    #[test]
    fn scrub_zoom_is_continuous_and_clamped() {
        assert_eq!(scrub_zoom(1.0, 0.0), 1.0);
        assert!((scrub_zoom(1.0, DOUBLING) - 2.0).abs() < 1e-5);
        assert!((scrub_zoom(1.0, -DOUBLING) - 0.5).abs() < 1e-5);
        // Monotone: every point of drag to the right zooms in a little more.
        let zs: Vec<f32> = (0..50).map(|i| scrub_zoom(0.7, i as f32 * 3.0)).collect();
        assert!(zs.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(scrub_zoom(1.0, 1e6), MAX_ZOOM);
        assert_eq!(scrub_zoom(1.0, -1e6), MIN_ZOOM);
        assert_eq!(scrub_zoom(2.0, f32::NAN), 2.0);
    }

    fn harness(scrubby: bool) -> Harness<'static, PhotocraftApp> {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 300})).unwrap();
        app.sync_views();
        app.ui.extras.rulers = false;
        app.ui.tool = crate::state::Tool::Zoom;
        app.ui.tool_options.zoom_scrubby = scrubby;
        let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                // Fonts set up after the first frame only apply from the next one.
                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        h
    }

    fn view_xf(h: &Harness<'static, PhotocraftApp>) -> ViewXform {
        let app = h.state();
        let v = &app.ui.views[0];
        ViewXform { rect: app.last_canvas_rect, zoom: v.zoom, center: v.center, flip: false }
    }

    fn press(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, pressed: bool) {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() });
        h.step();
    }

    #[test]
    fn scrubby_zoom_drags_continuously_about_the_press_point() {
        let mut h = harness(true);
        let r = h.state().last_canvas_rect;
        let p0 = r.center() + vec2(-150.0, 60.0);
        let doc0 = view_xf(&h).to_doc(p0);
        let z0 = h.state().current_zoom();
        h.event(Event::PointerMoved(p0));
        h.step();
        press(&mut h, p0, true);
        // Right: zooms in a little more each frame, the pressed document point stays put.
        let mut last = z0;
        for i in 1..=8 {
            h.event(Event::PointerMoved(p0 + vec2(i as f32 * 15.0, (i % 3) as f32)));
            h.step();
            let z = h.state().current_zoom();
            assert!(z > last, "frame {i}: {last} -> {z}");
            last = z;
            let s = view_xf(&h).to_screen(doc0[0] as f32, doc0[1] as f32);
            assert!(s.distance(p0) < 0.5, "anchor drifted to {s:?} (pressed at {p0:?})");
        }
        // Back left past the press point: zooms out below the start.
        h.event(Event::PointerMoved(p0 - vec2(60.0, 0.0)));
        h.step();
        let z = h.state().current_zoom();
        assert!(z < z0 && (z - scrub_zoom(z0, -60.0)).abs() < 1e-4, "{z0} -> {z}");
        press(&mut h, p0 - vec2(60.0, 0.0), false);
        h.run_steps(2);
        // Release keeps the zoom; nothing was painted or recorded.
        assert!((h.state().current_zoom() - z).abs() < 1e-5);
        assert_eq!(h.state().session.documents()[0].history.past_len(), 0);
        // A plain click still steps in.
        let zc = h.state().current_zoom();
        h.event(Event::PointerMoved(p0));
        h.step();
        press(&mut h, p0, true);
        press(&mut h, p0, false);
        h.run_steps(2);
        assert_eq!(h.state().current_zoom(), crate::canvas::zoom_step(zc, 1));
    }

    #[test]
    fn zoom_rectangle_without_scrubby_zoom() {
        let mut h = harness(false);
        let r = h.state().last_canvas_rect;
        let z0 = h.state().current_zoom();
        let a = r.center() - vec2(60.0, 40.0);
        let b = r.center() + vec2(60.0, 40.0);
        let mid = view_xf(&h).to_doc(r.center());
        h.event(Event::PointerMoved(a));
        h.step();
        press(&mut h, a, true);
        for t in [0.3, 0.7, 1.0] {
            h.event(Event::PointerMoved(a + (b - a) * t));
            h.step();
        }
        assert_eq!(h.state().current_zoom(), z0, "no zoom while the rectangle is drawn");
        press(&mut h, b, false);
        h.run_steps(2);
        let z = h.state().current_zoom();
        let k = (r.width() / 120.0).min(r.height() / 80.0);
        assert!((z - z0 * k).abs() < z0 * k * 0.02, "{z0} * {k} vs {z}");
        // The rectangle's centre is now the view centre.
        let c = h.state().ui.views[0].center;
        assert!((c[0] as f64 - mid[0]).abs() < 1.0 && (c[1] as f64 - mid[1]).abs() < 1.0, "{c:?} vs {mid:?}");
    }
}

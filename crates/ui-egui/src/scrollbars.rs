//! Canvas scrollbars (#300) and Preferences › Tools › Overscroll.
//!
//! Like Photoshop's document window: a horizontal bar along the bottom of the view and a
//! vertical one along the right, each shown only when the image doesn't fit that way. The thumb
//! is the visible fraction of the scrollable extent (the image, plus wherever the view has been
//! scrolled past it); drag it to scroll, or click the track to page by one screenful.
//!
//! With Overscroll off the view is clamped before anything is drawn: the image can't be scrolled
//! past its edges, and an image smaller than the window stays centred on that axis.
//!
//! Everything here is a few rectangles per frame: no allocation and no per-pixel work.

use egui::{Color32, CornerRadius, Id, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::state::View;

/// Thickness of a bar, in points.
pub const THICKNESS: f32 = 12.0;
/// The thumb never gets shorter than this, so it stays easy to grab on huge images.
pub const MIN_THUMB: f32 = 24.0;
/// A visible span within this many screen points of the image edge counts as fitting.
const FIT_SLACK: f32 = 0.5;

/// One axis of the view, in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    /// Image length on this axis.
    pub len: f32,
    /// First and last visible document coordinate.
    pub v0: f32,
    pub v1: f32,
}

impl Span {
    /// The axis of a view: `size` screen points at `zoom` around `center`.
    pub fn of(len: f32, center: f32, size: f32, zoom: f32) -> Option<Span> {
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { return None };
        let half = size / zoom / 2.0;
        let s = Span { len, v0: center - half, v1: center + half };
        (s.len.is_finite() && s.len > 0.0 && s.v0.is_finite() && s.v1.is_finite() && s.v1 > s.v0).then_some(s)
    }

    /// The scrollable extent: the image and the visible span. `None` when the image fits (no bar).
    pub fn extent(&self, zoom: f32) -> Option<(f32, f32)> {
        let slack = FIT_SLACK / zoom.max(1e-6);
        if self.v0 <= slack && self.v1 >= self.len - slack {
            return None;
        }
        Some((self.v0.min(0.0), self.v1.max(self.len)))
    }

    /// The thumb's start and length along a track of `track` points, for extent `e`.
    pub fn thumb(&self, e: (f32, f32), track: f32) -> (f32, f32) {
        let total = (e.1 - e.0).max(1e-6);
        let visible = (self.v1 - self.v0).min(total);
        let len = (visible / total * track).clamp(MIN_THUMB.min(track), track.max(0.0));
        let travel = (track - len).max(0.0);
        let room = total - visible;
        let at = if room > 1e-6 { ((self.v0 - e.0) / room).clamp(0.0, 1.0) * travel } else { 0.0 };
        (at, len)
    }

    /// Document pixels per point of thumb movement along `track`, for extent `e`.
    pub fn doc_per_point(&self, e: (f32, f32), track: f32) -> f32 {
        let (_, len) = self.thumb(e, track);
        let travel = track - len;
        let room = (e.1 - e.0) - (self.v1 - self.v0);
        if travel > 1e-3 && room > 0.0 { room / travel } else { 0.0 }
    }
}

/// Clamp one axis of the view centre with Overscroll off: the image can't leave its edges; an
/// image smaller than the view is centred.
pub fn clamp_axis(center: f32, len: f32, size: f32, zoom: f32) -> f32 {
    if !(zoom.is_finite() && zoom > 0.0 && len.is_finite() && len > 0.0 && size.is_finite() && size > 0.0) {
        return center;
    }
    let half = size / zoom / 2.0;
    if 2.0 * half >= len {
        len / 2.0
    } else if center.is_finite() {
        center.clamp(half, len - half)
    } else {
        len / 2.0
    }
}

/// Apply Preferences › Tools › Overscroll (off) to `view` for a canvas of `size` points. True
/// when the view moved (the caller then repaints).
pub fn clamp_view(view: &mut View, size: egui::Vec2) -> bool {
    let [w, h] = view.doc_size;
    if w == 0 || h == 0 {
        return false;
    }
    let c = [clamp_axis(view.center[0], w as f32, size.x, view.zoom), clamp_axis(view.center[1], h as f32, size.y, view.zoom)];
    // Ignore sub-pixel float noise so the clamp never keeps requesting frames.
    let moved = (c[0] - view.center[0]).abs() > 1e-3 || (c[1] - view.center[1]).abs() > 1e-3;
    if moved {
        view.center = c;
    }
    moved
}

/// A thumb drag in progress: the extent and centre when it began (the extent grows as the view
/// moves past the image, so it is frozen for the drag).
#[derive(Clone, Copy, Debug)]
struct Drag {
    extent: (f32, f32),
    center: f32,
    origin: f32,
}

/// The two bars' rectangles inside `rect` (bottom, right), given which are shown.
pub fn bar_rects(rect: Rect, horizontal: bool, vertical: bool) -> (Option<Rect>, Option<Rect>) {
    let right = if vertical { rect.right() - THICKNESS } else { rect.right() };
    let bottom = if horizontal { rect.bottom() - THICKNESS } else { rect.bottom() };
    let h = horizontal.then(|| Rect::from_min_max(pos2(rect.left(), bottom), pos2(right, rect.bottom())));
    let v = vertical.then(|| Rect::from_min_max(pos2(right, rect.top()), pos2(rect.right(), bottom)));
    (h, v)
}

/// Show the scrollbars of the canvas `rect` and apply their drags and page clicks to `view`.
/// `flip` is View › Flip Horizontal (the bar then runs the other way through the image).
/// Returns true when the pointer is over a bar (the canvas then leaves the pointer alone).
pub fn show(ui: &Ui, rect: Rect, view: &mut View, flip: bool, key: Id) -> bool {
    let [w, h] = view.doc_size;
    let zoom = view.zoom;
    // Horizontal axis in "screen order": with the view flipped, run the image backwards.
    let hx = |c: f32| if flip { w as f32 - c } else { c };
    let sx = Span::of(w as f32, hx(view.center[0]), rect.width(), zoom);
    let sy = Span::of(h as f32, view.center[1], rect.height(), zoom);
    let ex = sx.and_then(|s| s.extent(zoom).map(|e| (s, e)));
    let ey = sy.and_then(|s| s.extent(zoom).map(|e| (s, e)));
    if ex.is_none() && ey.is_none() {
        return false;
    }
    let (hr, vr) = bar_rects(rect, ex.is_some(), ey.is_some());
    let t = crate::theme::Tokens::get(ui.ctx());
    let painter = ui.painter_at(rect);
    let mut over = false;
    if let (Some(r), Some(_)) = (hr, vr) {
        // The corner square where the bars meet.
        painter.rect_filled(Rect::from_min_max(pos2(r.right(), r.top()), rect.max), 0.0, t.chrome);
    }
    for (axis, bar, span) in [(0usize, hr, ex), (1usize, vr, ey)] {
        let (Some(bar), Some((s, e))) = (bar, span) else { continue };
        let id = key.with(("scrollbar", axis));
        let resp = ui.interact(bar, id, Sense::click_and_drag());
        over |= resp.hovered() || resp.dragged();
        let along = |p: Pos2| if axis == 0 { p.x - bar.left() } else { p.y - bar.top() };
        let track = if axis == 0 { bar.width() } else { bar.height() };
        let dragging = ui.ctx().data(|d| d.get_temp::<Drag>(id));
        // A drag keeps the extent it began with.
        let e = dragging.map_or(e, |d| d.extent);
        let (t0, tl) = s.thumb(e, track);
        let set = |view: &mut View, c: f32| {
            if !c.is_finite() {
                return;
            }
            if axis == 0 {
                view.center[0] = if flip { w as f32 - c } else { c };
            } else {
                view.center[1] = c;
            }
        };
        let center = (s.v0 + s.v1) / 2.0;
        // The drag starts where the button went down, not where it passed egui's drag threshold.
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
        {
            let a = along(p);
            if a >= t0 && a <= t0 + tl {
                ui.ctx().data_mut(|d| d.insert_temp(id, Drag { extent: e, center, origin: a }));
            }
        }
        if let Some(d) = ui.ctx().data(|m| m.get_temp::<Drag>(id)) {
            if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
                let c = d.center + (along(p) - d.origin) * s.doc_per_point(d.extent, track);
                set(view, c);
            }
            if !resp.dragged() || resp.drag_stopped() {
                ui.ctx().data_mut(|m| m.remove::<Drag>(id));
            }
        } else if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            // Page by one screenful toward the click, stopping at the end of the extent.
            let a = along(p);
            let page = s.v1 - s.v0;
            let c = if a < t0 {
                (center - page).max(e.0 + page / 2.0)
            } else if a > t0 + tl {
                (center + page).min(e.1 - page / 2.0)
            } else {
                center
            };
            set(view, c);
        }
        // Track and thumb.
        painter.rect_filled(bar, 0.0, t.chrome);
        let edge = if axis == 0 { [bar.left_top(), bar.right_top()] } else { [bar.left_top(), bar.left_bottom()] };
        painter.line_segment(edge, egui::Stroke::new(1.0, t.separator));
        let thumb = if axis == 0 {
            Rect::from_min_size(pos2(bar.left() + t0, bar.top()), vec2(tl, bar.height()))
        } else {
            Rect::from_min_size(pos2(bar.left(), bar.top() + t0), vec2(bar.width(), tl))
        }
        .shrink(3.0);
        let hot = resp.dragged() || resp.hover_pos().is_some_and(|p| thumb.expand(3.0).contains(p));
        let fill = if hot { t.text_dim } else { thumb_color(&t) };
        painter.rect_filled(thumb, CornerRadius::same((thumb.width().min(thumb.height()) / 2.0) as u8), fill);
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
    }
    over
}

fn thumb_color(t: &crate::theme::Tokens) -> Color32 {
    t.text_faint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_the_image_fits() {
        // 1000 px image, 500 points at 0.4 zoom shows 1250 px: fits.
        let s = Span::of(1000.0, 500.0, 500.0, 0.4).unwrap();
        assert_eq!(s.extent(0.4), None);
        // At 100 % only half is visible: a bar, its thumb half the track.
        let s = Span::of(1000.0, 500.0, 500.0, 1.0).unwrap();
        let e = s.extent(1.0).unwrap();
        assert_eq!(e, (0.0, 1000.0));
        let (at, len) = s.thumb(e, 400.0);
        assert!((len - 200.0).abs() < 1e-3 && (at - 100.0).abs() < 1e-3, "{at} {len}");
    }

    #[test]
    fn thumb_reaches_both_ends_and_has_a_minimum() {
        let left = Span::of(10_000.0, 250.0, 500.0, 1.0).unwrap();
        let e = left.extent(1.0).unwrap();
        assert_eq!(left.thumb(e, 300.0), (0.0, MIN_THUMB));
        let right = Span::of(10_000.0, 9750.0, 500.0, 1.0).unwrap();
        let (at, len) = right.thumb(right.extent(1.0).unwrap(), 300.0);
        assert!((at + len - 300.0).abs() < 1e-3);
        // Dragging the thumb the whole travel scrolls the whole room.
        let k = left.doc_per_point(e, 300.0);
        assert!(((300.0 - MIN_THUMB) * k - 9500.0).abs() < 1e-2, "{k}");
    }

    #[test]
    fn overscrolled_views_extend_the_extent() {
        // Scrolled 300 px past the left edge.
        let s = Span::of(1000.0, -50.0, 500.0, 1.0).unwrap();
        let e = s.extent(1.0).unwrap();
        assert_eq!(e, (-300.0, 1000.0));
        assert_eq!(s.thumb(e, 400.0).0, 0.0);
    }

    #[test]
    fn overscroll_off_clamps() {
        assert_eq!(clamp_axis(-400.0, 1000.0, 500.0, 1.0), 250.0);
        assert_eq!(clamp_axis(4000.0, 1000.0, 500.0, 1.0), 750.0);
        assert_eq!(clamp_axis(600.0, 1000.0, 500.0, 1.0), 600.0);
        // Smaller than the view: centred.
        assert_eq!(clamp_axis(-90.0, 1000.0, 500.0, 0.2), 500.0);
        let mut v = View { zoom: 1.0, center: [-500.0, 20.0], fit_pending: false, doc_size: [1000, 800] };
        assert!(clamp_view(&mut v, vec2(500.0, 400.0)));
        assert_eq!(v.center, [250.0, 200.0]);
        assert!(!clamp_view(&mut v, vec2(500.0, 400.0)), "stable: no repaint loop");
    }

    #[test]
    fn hostile_numbers_are_harmless() {
        for z in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(Span::of(1000.0, 10.0, 500.0, z).is_none());
            assert_eq!(clamp_axis(10.0, 1000.0, 500.0, z), 10.0);
        }
        assert!(Span::of(f32::NAN, 10.0, 500.0, 1.0).is_none());
        assert!(Span::of(1000.0, f32::INFINITY, 500.0, 1.0).is_none());
        assert_eq!(clamp_axis(f32::NAN, 1000.0, 500.0, 1.0), 500.0);
        let s = Span::of(1000.0, 500.0, 500.0, 1.0).unwrap();
        let (at, len) = s.thumb((0.0, 1000.0), 0.0);
        assert!(at.is_finite() && len.is_finite());
        assert_eq!(s.doc_per_point((0.0, 1000.0), 0.0), 0.0);
        let mut v = View { zoom: 1.0, center: [0.0, 0.0], fit_pending: false, doc_size: [0, 0] };
        assert!(!clamp_view(&mut v, vec2(500.0, 400.0)));
    }

    mod canvas {
        use egui::{Modifiers, PointerButton, Pos2, pos2, vec2};
        use egui_kittest::Harness;
        use serde_json::json;

        use super::super::*;
        use crate::PhotocraftApp;

        fn harness(overscroll: bool) -> Harness<'static, PhotocraftApp> {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.run("file.new", json!({"width": 2000, "height": 1500})).unwrap();
            app.run("prefs.set", json!({"path": "tools.overscroll", "value": overscroll})).unwrap();
            app.sync_views();
            let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).build_ui_state(
                |ui, app: &mut PhotocraftApp| {
                    let ctx = ui.ctx().clone();
                    if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        return;
                    }
                    egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
                },
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
            h.run_steps(4);
            h
        }

        fn view(h: &Harness<'static, PhotocraftApp>) -> View {
            h.state().ui.views[0].clone()
        }

        fn set_view(h: &mut Harness<'static, PhotocraftApp>, zoom: f32, center: [f32; 2]) {
            let v = &mut h.state_mut().ui.views[0];
            v.zoom = zoom;
            v.center = center;
            h.run_steps(2);
        }

        /// Centre of the horizontal thumb, in screen points.
        fn h_thumb(h: &Harness<'static, PhotocraftApp>) -> Option<Pos2> {
            let r = h.state().last_canvas_rect;
            let v = view(h);
            let s = Span::of(v.doc_size[0] as f32, v.center[0], r.width(), v.zoom)?;
            let e = s.extent(v.zoom)?;
            let sy = Span::of(v.doc_size[1] as f32, v.center[1], r.height(), v.zoom).and_then(|s| s.extent(v.zoom));
            let (bar, _) = bar_rects(r, true, sy.is_some());
            let bar = bar?;
            let (at, len) = s.thumb(e, bar.width());
            Some(pos2(bar.left() + at + len / 2.0, bar.center().y))
        }

        fn click(h: &mut Harness<'static, PhotocraftApp>, p: Pos2) {
            h.event(egui::Event::PointerMoved(p));
            h.run_steps(1);
            h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
            h.run_steps(1);
            h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
            h.run_steps(2);
        }

        #[test]
        fn hidden_when_the_image_fits() {
            let h = harness(true);
            assert!(h_thumb(&h).is_none(), "fit on screen: no bars");
        }

        #[test]
        fn dragging_the_thumb_scrolls_and_paints_nothing() {
            let mut h = harness(true);
            h.state_mut().ui.tool = crate::state::Tool::Brush;
            set_view(&mut h, 1.0, [700.0, 750.0]);
            let p = h_thumb(&h).expect("a horizontal bar at 100 %");
            let c0 = view(&h).center;
            h.event(egui::Event::PointerMoved(p));
            h.run_steps(1);
            h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
            h.run_steps(1);
            for k in 1..=10 {
                h.event(egui::Event::PointerMoved(p + vec2(10.0 * k as f32, 0.0)));
                h.run_steps(1);
            }
            h.event(egui::Event::PointerButton { pos: p + vec2(100.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
            h.run_steps(2);
            let c1 = view(&h).center;
            // 100 points of thumb travel scroll about 2 px per point (1000 px of room, ~500 of travel).
            assert!(c1[0] - c0[0] > 150.0, "{c0:?} -> {c1:?}");
            assert_eq!(c1[1], c0[1]);
            assert!(!h.state().session.journal.iter().any(|(id, _)| id == "paint.stroke"), "the bar never reaches the tool");
            let t = h_thumb(&h).unwrap();
            assert!((t.x - (p.x + 100.0)).abs() < 3.0, "the thumb followed the pointer: {t:?} vs {p:?}");
        }

        #[test]
        fn clicking_the_track_pages_by_a_screenful() {
            let mut h = harness(true);
            set_view(&mut h, 1.0, [500.0, 750.0]);
            let p = h_thumb(&h).unwrap();
            let r = h.state().last_canvas_rect;
            let x0 = view(&h).center[0];
            click(&mut h, pos2(r.right() - 30.0, p.y));
            let x1 = view(&h).center[0];
            assert!((x1 - x0 - r.width()).abs() < 1.0, "paged right by the visible width: {x0} -> {x1}");
            // Paging stops at the end of the image.
            click(&mut h, pos2(r.right() - 30.0, p.y));
            click(&mut h, pos2(r.right() - 30.0, p.y));
            let end = 2000.0 - r.width() / 2.0;
            assert!((view(&h).center[0] - end).abs() < 1.0, "{}", view(&h).center[0]);
            click(&mut h, pos2(r.left() + 5.0, p.y));
            assert!((view(&h).center[0] - (end - r.width())).abs() < 1.0);
        }

        #[test]
        fn overscroll_off_keeps_the_image_in_view() {
            for overscroll in [true, false] {
                let mut h = harness(overscroll);
                set_view(&mut h, 1.0, [-3000.0, 750.0]);
                let x = view(&h).center[0];
                let w = h.state().last_canvas_rect.width();
                if overscroll {
                    assert_eq!(x, -3000.0, "overscroll on: the view is free");
                } else {
                    assert!((x - w / 2.0).abs() < 1e-3, "clamped to the left edge: {x}");
                }
                // Zoomed out below fit: centred.
                set_view(&mut h, 0.1, [-50.0, 9000.0]);
                if !overscroll {
                    assert_eq!(view(&h).center, [1000.0, 750.0]);
                }
            }
        }
    }
}

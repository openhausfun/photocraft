//! Stylus input: real pen pressure (and tilt / barrel rotation where the platform reports them)
//! for the painting tools. A mouse always paints at pressure 1 with no tilt.
//!
//! Sources, by platform (eframe 0.36.2, egui-winit 0.36.2, winit 0.30.13):
//!
//! - **Windows**: winit turns `WM_POINTER` pen and touch input into `WindowEvent::Touch` with a
//!   normalised force (`POINTER_PEN_INFO::pressure / 1024`); egui-winit forwards it as
//!   `egui::Event::Touch { force }` alongside the emulated pointer. [`Stylus::update`] reads it.
//!   winit drops the pen's tilt and rotation, so those stay 0.
//! - **Web**: eframe forwards touch force but not pen pointer events, so the web runner listens
//!   to `pointerdown`/`pointermove` itself and writes `pressure`, `tiltX`, `tiltY`, `twist` and
//!   the eraser button of `pointerType == "pen"` events into the [`StylusFeed`].
//! - **macOS**: winit 0.30 drops `NSEvent` tablet data, so the desktop app installs an AppKit
//!   local event monitor (the `photocraft-tablet` crate) that writes pressure, tilt, rotation and
//!   the eraser end into the [`StylusFeed`] before winit handles each event.
//! - **Linux X11**: the desktop app reads XInput2 raw valuator events on its own X connection
//!   (`photocraft-tablet`, x11rb) and writes them into the [`StylusFeed`].
//! - **Linux Wayland**: no tablet input yet (`zwp_tablet_v2` would have to share winit's
//!   connection); pressure is 1. Launching with `WAYLAND_DISPLAY=` runs the app under Xwayland,
//!   which reports tablet valuators.
//!
//! Preferences › Tools › Use Tablet Pressure off makes a pen paint like a mouse. Flipping the pen
//! to its eraser end selects the Eraser tool and flipping back restores the previous tool, as in
//! Photoshop.
//!
//! Automation simulates a pen with `ui.pointer` events carrying `pressure`, `tiltX`, `tiltY`
//! and `rotation`.

use std::sync::{Arc, Mutex};

/// One stylus reading. Tilt is in degrees (-90..90, W3C Pointer Events convention), rotation in
/// degrees 0..360.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenSample {
    pub pressure: f32,
    pub tilt_x: f32,
    pub tilt_y: f32,
    pub rotation: f32,
    /// The pen's eraser end is in use.
    pub eraser: bool,
}

impl Default for PenSample {
    fn default() -> Self {
        Self { pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, eraser: false }
    }
}

impl PenSample {
    /// Clamp to the documented ranges (and replace non-finite values).
    pub fn sanitized(self) -> Self {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        Self {
            pressure: f(self.pressure, 0.0, 1.0, 1.0),
            tilt_x: f(self.tilt_x, -90.0, 90.0, 0.0),
            tilt_y: f(self.tilt_y, -90.0, 90.0, 0.0),
            // `rem_euclid` rounds tiny negative angles up to exactly 360.
            rotation: if self.rotation.is_finite() { self.rotation.rem_euclid(360.0) % 360.0 } else { 0.0 },
            eraser: self.eraser,
        }
    }
}

/// Shared slot a platform backend writes the current pen sample into (`None` = no pen down).
/// Cloning shares the slot.
#[derive(Clone, Debug, Default)]
pub struct StylusFeed(Arc<Mutex<Option<PenSample>>>);

impl StylusFeed {
    pub fn set(&self, s: Option<PenSample>) {
        if let Ok(mut g) = self.0.lock() {
            *g = s.map(PenSample::sanitized);
        }
    }
    pub fn get(&self) -> Option<PenSample> {
        self.0.lock().ok().and_then(|g| *g)
    }
}

/// Per-app stylus state.
#[derive(Clone, Debug)]
pub struct Stylus {
    /// Pen samples pushed by the platform (desktop tablet monitor, web runner) or by automation.
    pub feed: StylusFeed,
    /// Preferences › Tools › Use Tablet Pressure: off, a pen paints like a mouse.
    pub use_pressure: bool,
    /// The pen end last seen (`Some(true)` = eraser), for the eraser tool switch.
    end: Option<bool>,
    /// The tool to restore when the pen tip comes back after the eraser end switched tools.
    pub(crate) tool_before_eraser: Option<crate::state::Tool>,
    /// Force of the touch/pen contact currently down (egui `Event::Touch`).
    touch: Option<f32>,
    /// The contact ended this frame: keep its force for this frame's last tool events, clear next frame.
    lifted: bool,
    /// Tilt X, tilt Y, rotation of each point of the current drag (parallel to its points).
    pub(crate) stroke: Vec<[f32; 3]>,
}

impl Default for Stylus {
    fn default() -> Self {
        Self { feed: StylusFeed::default(), use_pressure: true, end: None, tool_before_eraser: None, touch: None, lifted: false, stroke: Vec::new() }
    }
}

impl Stylus {
    /// Track touch/pen contacts from this frame's input events.
    pub fn update(&mut self, events: &[egui::Event]) {
        if std::mem::take(&mut self.lifted) {
            self.touch = None;
        }
        for e in events {
            if let egui::Event::Touch { phase, force, .. } = e {
                match phase {
                    egui::TouchPhase::Start | egui::TouchPhase::Move => {
                        if let Some(f) = force.filter(|f| f.is_finite()) {
                            self.touch = Some(f.clamp(0.0, 1.0));
                        }
                    }
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => self.lifted = true,
                }
            }
        }
    }

    /// The current pen sample, `None` for a mouse (and for any pen while Use Tablet Pressure is
    /// off).
    pub fn sample(&self) -> Option<PenSample> {
        if !self.use_pressure {
            return None;
        }
        self.feed.get().or(self.touch.map(|pressure| PenSample { pressure, ..Default::default() }))
    }

    /// Did the pen just flip to its eraser end (`Some(true)`) or back to its tip (`Some(false)`)?
    /// Each flip is reported once; a mouse in between changes nothing.
    pub(crate) fn take_end_flip(&mut self) -> Option<bool> {
        let eraser = self.feed.get()?.eraser;
        let flipped = self.end.map_or(eraser, |e| e != eraser);
        self.end = Some(eraser);
        flipped.then_some(eraser)
    }

    /// Switch to the Eraser when the pen's eraser end comes in, and back to the previous tool when
    /// the tip does (Photoshop). Not during a drag. Returns whether the tool changed.
    pub fn sync_eraser_tool(app: &mut crate::PhotocraftApp) -> bool {
        use crate::state::Tool;
        if app.drag.is_some() {
            return false;
        }
        match app.stylus.take_end_flip() {
            Some(true) if app.ui.tool != Tool::Eraser => {
                app.stylus.tool_before_eraser = Some(app.ui.tool);
                app.ui.tool = Tool::Eraser;
                true
            }
            Some(false) => match app.stylus.tool_before_eraser.take() {
                Some(t) if app.ui.tool == Tool::Eraser => {
                    app.ui.tool = t;
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Pressure for the next tool event (1 for a mouse).
    pub fn pressure(&self) -> f32 {
        self.sample().map_or(1.0, |s| s.pressure)
    }

    /// Start recording a drag's tilt/rotation.
    pub(crate) fn begin_stroke(&mut self) {
        self.stroke.clear();
        self.record_point();
    }

    /// Record the tilt/rotation of a point just added to the drag.
    pub(crate) fn record_point(&mut self) {
        let s = self.sample().unwrap_or_default();
        self.stroke.push([s.tilt_x, s.tilt_y, s.rotation]);
    }

    /// Stroke points for `paint.stroke`: `[x, y, pressure]`, extended with
    /// `tiltX, tiltY, rotation` when the drag carried any tilt or rotation.
    pub fn stroke_points(&self, points: &[[f64; 3]]) -> Vec<Vec<f64>> {
        let has_pose = self.stroke.iter().any(|t| t.iter().any(|v| *v != 0.0));
        points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut v = vec![p[0], p[1], p[2]];
                if has_pose {
                    let t = self.stroke.get(i).or(self.stroke.last()).copied().unwrap_or_default();
                    v.extend(t.map(f64::from));
                }
                v
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(phase: egui::TouchPhase, force: Option<f32>) -> egui::Event {
        egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(0), phase, pos: egui::pos2(1.0, 1.0), force }
    }

    #[test]
    fn mouse_is_full_pressure() {
        let mut s = Stylus::default();
        s.update(&[egui::Event::PointerMoved(egui::pos2(3.0, 4.0))]);
        assert_eq!(s.pressure(), 1.0);
        assert_eq!(s.sample(), None);
    }

    #[test]
    fn touch_force_drives_pressure_until_lift() {
        let mut s = Stylus::default();
        s.update(&[touch(egui::TouchPhase::Start, Some(0.25))]);
        assert_eq!(s.pressure(), 0.25);
        s.update(&[touch(egui::TouchPhase::Move, Some(0.8)), touch(egui::TouchPhase::Move, Some(1.7))]);
        assert_eq!(s.pressure(), 1.0, "clamped");
        s.update(&[touch(egui::TouchPhase::Move, Some(f32::NAN))]);
        assert_eq!(s.pressure(), 1.0);
        s.update(&[touch(egui::TouchPhase::Move, Some(0.4))]);
        assert_eq!(s.pressure(), 0.4);
        // The lift frame still paints at the last force; the next frame is back to the mouse.
        s.update(&[touch(egui::TouchPhase::End, None)]);
        assert_eq!(s.pressure(), 0.4);
        s.update(&[]);
        assert_eq!(s.pressure(), 1.0);
    }

    #[test]
    fn feed_wins_and_is_sanitized() {
        let mut s = Stylus::default();
        s.update(&[touch(egui::TouchPhase::Start, Some(0.3))]);
        s.feed.set(Some(PenSample { pressure: 0.6, tilt_x: 120.0, tilt_y: -30.0, rotation: -90.0, eraser: false }));
        assert_eq!(s.sample(), Some(PenSample { pressure: 0.6, tilt_x: 90.0, tilt_y: -30.0, rotation: 270.0, eraser: false }));
        s.feed.set(None);
        assert_eq!(s.pressure(), 0.3);
    }

    #[test]
    fn pen_samples_reach_the_brush_stroke() {
        use crate::canvas::{ToolEvent, tool_event};
        use serde_json::json;
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 120, "height": 80})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.ui.tool = crate::state::Tool::Brush;
        let m = egui::Modifiers::NONE;
        app.stylus.feed.set(Some(PenSample { pressure: 0.2, tilt_x: 40.0, tilt_y: 0.0, rotation: 30.0, eraser: false }));
        let pr = app.stylus.pressure();
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 40.0, pressure: pr }, m);
        app.stylus.feed.set(Some(PenSample { pressure: 0.9, tilt_x: 10.0, tilt_y: -5.0, rotation: 60.0, eraser: false }));
        let pr = app.stylus.pressure();
        tool_event(&mut app, ToolEvent::Move { x: 100.0, y: 40.0, pressure: pr }, m);
        tool_event(&mut app, ToolEvent::Up { x: 100.0, y: 40.0 }, m);
        let (id, p) = app.session.journal.iter().rev().find(|(id, _)| id == "paint.stroke").cloned().unwrap();
        assert_eq!(id, "paint.stroke");
        assert_eq!(p["points"][0], json!([10.0, 40.0, 0.2f32 as f64, 40.0, 0.0, 30.0]));
        assert_eq!(p["points"][1], json!([100.0, 40.0, 0.9f32 as f64, 10.0, -5.0, 60.0]));
        // A mouse stroke stays at full pressure with plain [x, y, p] points.
        app.stylus.feed.set(None);
        let pr = app.stylus.pressure();
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 60.0, pressure: pr }, m);
        tool_event(&mut app, ToolEvent::Up { x: 90.0, y: 60.0 }, m);
        let (_, p) = app.session.journal.iter().rev().find(|(id, _)| id == "paint.stroke").cloned().unwrap();
        assert_eq!(p["points"][0], json!([10.0, 60.0, 1.0]));
    }

    #[test]
    fn stroke_points_carry_tilt_only_when_present() {
        let mut s = Stylus::default();
        let pts = [[0.0, 0.0, 0.5], [10.0, 0.0, 0.7], [20.0, 0.0, 0.7]];
        s.begin_stroke();
        s.record_point();
        assert_eq!(s.stroke_points(&pts)[1], vec![10.0, 0.0, 0.7]);
        s.feed.set(Some(PenSample { pressure: 0.7, tilt_x: 30.0, tilt_y: 10.0, rotation: 45.0, eraser: false }));
        s.record_point();
        let out = s.stroke_points(&pts);
        assert_eq!(out[0], vec![0.0, 0.0, 0.5, 0.0, 0.0, 0.0]);
        assert_eq!(out[2], vec![20.0, 0.0, 0.7, 30.0, 10.0, 45.0]);
    }

    #[test]
    fn use_pressure_off_ignores_pen_and_touch() {
        let mut s = Stylus { use_pressure: false, ..Default::default() };
        s.update(&[touch(egui::TouchPhase::Start, Some(0.3))]);
        s.feed.set(Some(PenSample { pressure: 0.2, tilt_x: 40.0, ..Default::default() }));
        assert_eq!((s.sample(), s.pressure()), (None, 1.0));
        s.begin_stroke();
        assert_eq!(s.stroke_points(&[[1.0, 2.0, 1.0]]), vec![vec![1.0, 2.0, 1.0]], "no tilt either");
    }

    #[test]
    fn eraser_end_switches_tools_once_and_never_mid_drag() {
        use crate::canvas::{ToolEvent, tool_event};
        use crate::state::Tool;
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", serde_json::json!({"width": 60, "height": 60})).unwrap();
        app.ui.tool = Tool::Brush;
        let pen = |eraser| Some(PenSample { pressure: 0.5, eraser, ..Default::default() });
        // A mouse never switches.
        assert!(!Stylus::sync_eraser_tool(&mut app));
        app.stylus.feed.set(pen(true));
        assert!(Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Eraser);
        assert!(!Stylus::sync_eraser_tool(&mut app), "reported once");
        // Flipping back during a drag waits for the drag to end.
        tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 10.0, pressure: 0.5 }, egui::Modifiers::NONE);
        app.stylus.feed.set(pen(false));
        assert!(!Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Eraser);
        tool_event(&mut app, ToolEvent::Up { x: 20.0, y: 10.0 }, egui::Modifiers::NONE);
        assert!(Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Brush, "the tip restores the previous tool");
        // The eraser end while the Eraser is already chosen remembers nothing to restore.
        app.ui.tool = Tool::Eraser;
        app.stylus.feed.set(pen(true));
        assert!(!Stylus::sync_eraser_tool(&mut app));
        app.stylus.feed.set(pen(false));
        assert!(!Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Eraser);
    }
}

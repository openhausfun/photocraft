//! Pen tablet pressure, tilt, rotation and eraser end on macOS and Linux X11 (issue #79).
//!
//! winit 0.30 drops tablet data on both, so `photocraft-tablet` reads it beside winit (an AppKit
//! local event monitor; XInput2 raw events on a second X connection) and this module writes each
//! sample into the UI's [`StylusFeed`], where the canvas reads it exactly like the web runner's
//! Pointer Events and automation's simulated pen. Windows needs nothing here: winit forwards
//! `WM_POINTER` pressure as touch force. Wayland has no tablet input yet (see the crate docs).

use photocraft_tablet::Sample;
use photocraft_ui_egui::stylus::{PenSample, StylusFeed};

/// A tablet sample as the UI's pen sample.
pub fn pen_sample(s: Sample) -> PenSample {
    PenSample { pressure: s.pressure, tilt_x: s.tilt_x, tilt_y: s.tilt_y, rotation: s.rotation, eraser: s.eraser }.sanitized()
}

/// Writes tablet samples into `feed` (`None` = a mouse).
pub fn sink(feed: StylusFeed) -> impl Fn(Option<Sample>) + Send + 'static {
    move |s| feed.set(s.map(pen_sample))
}

/// Install the AppKit tablet monitor (main thread, before the event loop). Keep the result until
/// the event loop returns. On failure this logs why: pens then paint like a mouse.
#[cfg(target_os = "macos")]
pub fn install_macos(feed: &StylusFeed) -> Option<photocraft_tablet::macos::Monitor> {
    photocraft_tablet::macos::Monitor::install(sink(feed.clone())).map_err(|e| log::warn!("{e}")).ok()
}

/// Start the XInput2 reader when eframe runs on X11. On Wayland, `$DISPLAY` is Xwayland, which
/// sees none of this window's input (and a pen sample from another X app would outlive a mouse
/// stroke here), so nothing starts.
#[cfg(target_os = "linux")]
pub fn spawn_x11(feed: &StylusFeed, display: Option<DisplayKind>) {
    if display != Some(DisplayKind::X11) {
        log::info!("tablet pressure: not available on this display server ({display:?}); Wayland is a follow-up to #79");
        return;
    }
    if let Err(e) = photocraft_tablet::x11::spawn(None, sink(feed.clone())) {
        log::warn!("{e}");
    }
}

/// The windowing system eframe ended up on.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    X11,
    Wayland,
    Other,
}

#[cfg(target_os = "linux")]
impl DisplayKind {
    /// From the creation context's display handle.
    pub fn of(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        use eframe::wgpu::rwh::{HasDisplayHandle, RawDisplayHandle};
        Some(match cc.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => DisplayKind::X11,
            RawDisplayHandle::Wayland(_) => DisplayKind::Wayland,
            _ => DisplayKind::Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_tablet::{Update, appkit, xi};
    use photocraft_ui_egui::PhotocraftApp;
    use photocraft_ui_egui::canvas::{ToolEvent, tool_event};
    use photocraft_ui_egui::state::Tool;
    use serde_json::{Value, json};

    #[test]
    fn samples_map_and_sanitize() {
        let p = pen_sample(Sample { pressure: 0.25, tilt_x: 30.0, tilt_y: -10.0, rotation: 90.0, eraser: true });
        assert_eq!(p, PenSample { pressure: 0.25, tilt_x: 30.0, tilt_y: -10.0, rotation: 90.0, eraser: true });
        let p = pen_sample(Sample { pressure: f32::NAN, tilt_x: 500.0, tilt_y: f32::NEG_INFINITY, rotation: -45.0, eraser: false });
        assert_eq!(p, PenSample { pressure: 1.0, tilt_x: 90.0, tilt_y: 0.0, rotation: 315.0, eraser: false });
        let feed = StylusFeed::default();
        let f = sink(feed.clone());
        f(Some(Sample { pressure: 0.5, ..Default::default() }));
        assert_eq!(feed.get().map(|p| p.pressure), Some(0.5));
        f(None);
        assert_eq!(feed.get(), None);
    }

    /// The platform path end to end, minus the OS: raw AppKit / XInput2 values → the crate's
    /// mapping → this module's sink → the stylus feed → the canvas's tool events → `paint.stroke`.
    /// A light-to-heavy pen stroke must paint thin and faint at the light end and wide and opaque
    /// at the heavy end (Size and Opacity on Pen Pressure); a mouse paints full strength.
    fn pressure_stroke(mut next: impl FnMut(f32) -> Option<Sample>) -> (f64, f64, f64, f64) {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let feed = app.stylus.feed.clone();
        let sink = sink(feed);
        app.session.execute("file.new", json!({"width": 240, "height": 80, "background": "white"})).unwrap();
        app.session.execute("tools.setColors", json!({"foreground": [0.0, 0.0, 0.0, 1.0]})).unwrap();
        let brush = json!({"size": 30, "hardness": 1.0, "spacing": 0.05, "pressureSize": true, "pressureOpacity": true, "smoothing": {"amount": 0}});
        app.session.execute("tools.setBrush", json!({ "brush": brush })).unwrap();
        app.ui.tool = Tool::Brush;
        let m = egui::Modifiers::NONE;
        let n = 20;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            sink(next(0.1 + 0.9 * t));
            let (x, y) = (20.0 + 200.0 * f64::from(t), 40.0);
            let pressure = app.stylus.pressure();
            let ev = if i == 0 { ToolEvent::Down { x, y, pressure } } else { ToolEvent::Move { x, y, pressure } };
            tool_event(&mut app, ev, m);
        }
        tool_event(&mut app, ToolEvent::Up { x: 220.0, y: 40.0 }, m);
        sink(None);
        // Darkness (1 - luma) along x = 50 (light end) and x = 200 (heavy end), and the painted
        // half-width there.
        let mut dark = |x: i32, y: i32| -> f64 {
            let px: Value = app.session.execute("document.pixel", json!({"x": x, "y": y})).unwrap();
            1.0 - px[0].as_f64().unwrap()
        };
        let (light, heavy) = (dark(50, 40), dark(200, 40));
        let light_w = (0..40).take_while(|d| dark(50, 40 + d) > 0.02).count() as f64;
        let heavy_w = (0..40).take_while(|d| dark(200, 40 + d) > 0.02).count() as f64;
        (light, light_w, heavy, heavy_w)
    }

    #[test]
    fn appkit_tablet_pressure_drives_stroke_width_and_opacity() {
        let mut st = appkit::State::default();
        let (light_dark, light_w, heavy_dark, heavy_w) = pressure_stroke(|p| {
            let e =
                appkit::RawEvent { kind: appkit::event_type::LEFT_MOUSE_DRAGGED, subtype: appkit::subtype::TABLET_POINT, pressure: p, ..Default::default() };
            match st.handle(&e) {
                Update::Set(s) => s,
                Update::Keep => None,
            }
        });
        assert!(heavy_w > light_w * 2.0, "width responds: {light_w} → {heavy_w}");
        assert!(heavy_dark > light_dark + 0.3, "opacity responds: {light_dark} → {heavy_dark}");
        assert!(heavy_dark > 0.9, "full pressure paints (nearly) opaque: {heavy_dark}");

        // A mouse (non-tablet subtype) through the same path: full size and opacity throughout.
        let mut st = appkit::State::default();
        let (m_dark, m_w, _, _) = pressure_stroke(|p| {
            let e = appkit::RawEvent { kind: appkit::event_type::LEFT_MOUSE_DRAGGED, subtype: appkit::subtype::MOUSE, pressure: p, ..Default::default() };
            match st.handle(&e) {
                Update::Set(s) => s,
                Update::Keep => None,
            }
        });
        assert!(m_dark > 0.9 && m_w >= heavy_w - 1.0, "mouse = pressure 1: {m_dark}, {m_w}");
    }

    #[test]
    fn xinput_tablet_pressure_drives_stroke_width_and_opacity() {
        let mut st = xi::State::default();
        st.set_devices([xi::Device::new(9, "Wacom Intuos Pen stylus", [(2, "Abs Pressure".to_string(), 0.0, 65535.0)])]);
        let (light_dark, light_w, heavy_dark, heavy_w) = pressure_stroke(|p| match st.handle(9, [(2, f64::from(p) * 65535.0)]) {
            Update::Set(s) => s,
            Update::Keep => None,
        });
        assert!(heavy_w > light_w * 2.0, "width responds: {light_w} → {heavy_w}");
        assert!(heavy_dark > light_dark + 0.3, "opacity responds: {light_dark} → {heavy_dark}");
    }

    #[test]
    fn use_tablet_pressure_off_paints_like_a_mouse() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("prefs.set", json!({"path": "tools.useTabletPressure", "value": false})).unwrap();
        // The canvas copies the preference each frame; do what it does.
        app.stylus.use_pressure = app.session.prefs().tools.use_tablet_pressure;
        sink(app.stylus.feed.clone())(Some(Sample { pressure: 0.2, tilt_x: 40.0, ..Default::default() }));
        assert_eq!(app.stylus.pressure(), 1.0);
        assert_eq!(app.stylus.sample(), None);
    }

    #[test]
    fn eraser_end_switches_to_the_eraser_and_back() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let sink = sink(app.stylus.feed.clone());
        let mut st = appkit::State::default();
        let mut send = |e: appkit::RawEvent| {
            if let Update::Set(s) = st.handle(&e) {
                sink(s);
            }
        };
        let prox = |device, entering| appkit::RawEvent { kind: appkit::event_type::TABLET_PROXIMITY, device, entering, ..Default::default() };
        app.ui.tool = Tool::Brush;
        send(prox(appkit::device::PEN, true));
        assert!(!photocraft_ui_egui::stylus::Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Brush, "the tip changes nothing");
        send(prox(appkit::device::PEN, false));
        send(prox(appkit::device::ERASER, true));
        assert!(photocraft_ui_egui::stylus::Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Eraser);
        // Mouse moves in between change nothing; the tip coming back restores the Brush.
        send(appkit::RawEvent { kind: appkit::event_type::MOUSE_MOVED, ..Default::default() });
        assert!(!photocraft_ui_egui::stylus::Stylus::sync_eraser_tool(&mut app));
        send(prox(appkit::device::PEN, true));
        assert!(photocraft_ui_egui::stylus::Stylus::sync_eraser_tool(&mut app));
        assert_eq!(app.ui.tool, Tool::Brush);
    }
}

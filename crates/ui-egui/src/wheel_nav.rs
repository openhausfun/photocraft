//! Mouse wheel and trackpad navigation over the canvas (#293).
//!
//! - Scrolling pans (Shift scrolls sideways; egui folds that in).
//! - Pinch, or ⌘/Ctrl + scroll, zooms around the pointer (egui's zoom gesture).
//! - ⌥/Alt + scroll zooms around the pointer in gentle steps, about 5% per wheel notch, as in
//!   Photoshop with Preferences › General › Zoom with Scroll Wheel off.
//! - With Zoom with Scroll Wheel on, a plain scroll zooms too.
//!
//! egui smooths a wheel notch over several frames, so the Alt state is taken from the wheel
//! events themselves and kept until the next wheel event: releasing Alt while the last notch
//! is still settling must not turn the rest of it into a pan.

use egui::{Context, Event, Id, Vec2};

/// Zoom factor of one wheel notch with ⌥/Alt held.
pub const ALT_NOTCH: f32 = 1.05;

/// What the wheel does to the view this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wheel {
    /// Multiply the zoom by this factor around the pointer.
    Zoom(f32),
    /// Move the image by this many screen points (egui's content direction).
    Pan(Vec2),
}

/// Inputs of one frame's wheel gesture.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    /// Smoothed scroll delta (points, content direction).
    pub scroll: Vec2,
    /// egui's pinch / ⌘-scroll zoom factor (1 = none).
    pub zoom_delta: f32,
    /// ⌥/Alt was held for the wheel gesture in progress.
    pub alt: bool,
    /// Preferences › General › Zoom with Scroll Wheel.
    pub zoom_with_wheel: bool,
    /// Points per wheel notch (egui's `line_scroll_speed`).
    pub notch: f32,
}

/// The view change for one frame of wheel input, if any.
pub fn classify(i: Input) -> Option<Wheel> {
    if i.zoom_delta.is_finite() && i.zoom_delta > 0.0 && i.zoom_delta != 1.0 {
        return Some(Wheel::Zoom(i.zoom_delta));
    }
    if !(i.scroll.x.is_finite() && i.scroll.y.is_finite()) || i.scroll == Vec2::ZERO {
        return None;
    }
    if i.alt {
        // egui folds a scroll with Alt held into y. One notch (`notch` points) is 5%.
        let notch = if i.notch.is_finite() && i.notch > 0.0 { i.notch } else { 40.0 };
        let dy = i.scroll.y + i.scroll.x;
        let f = ALT_NOTCH.powf(dy / notch);
        return (f.is_finite() && f > 0.0 && f != 1.0).then_some(Wheel::Zoom(f));
    }
    if i.zoom_with_wheel && i.scroll.y != 0.0 {
        let f = (i.scroll.y / 200.0).exp();
        return f.is_finite().then_some(Wheel::Zoom(f));
    }
    Some(Wheel::Pan(i.scroll))
}

fn alt_id() -> Id {
    Id::new("pc-wheel-alt")
}

/// Read this frame's wheel input. Call it every frame (hovered or not) so the Alt state of the
/// gesture follows the latest wheel event.
pub fn read(ctx: &Context, zoom_with_wheel: bool) -> Option<Wheel> {
    let notch = ctx.options(|o| o.input_options.line_scroll_speed);
    let (scroll, zoom_delta, latest) = ctx.input(|i| {
        let latest = i.events.iter().rev().find_map(|e| match e {
            Event::MouseWheel { modifiers, .. } => Some(modifiers.alt && !modifiers.command && !modifiers.ctrl),
            _ => None,
        });
        (i.smooth_scroll_delta, i.zoom_delta(), latest)
    });
    let alt = match latest {
        Some(a) => {
            ctx.data_mut(|d| d.insert_temp(alt_id(), a));
            a
        }
        None => ctx.data(|d| d.get_temp::<bool>(alt_id())).unwrap_or(false),
    };
    classify(Input { scroll, zoom_delta, alt, zoom_with_wheel, notch })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(scroll: Vec2, alt: bool) -> Input {
        Input { scroll, zoom_delta: 1.0, alt, zoom_with_wheel: false, notch: 40.0 }
    }

    #[test]
    fn plain_scroll_pans() {
        assert_eq!(classify(input(Vec2::new(3.0, -40.0), false)), Some(Wheel::Pan(Vec2::new(3.0, -40.0))));
        assert_eq!(classify(input(Vec2::ZERO, false)), None);
    }

    #[test]
    fn alt_scroll_zooms_five_percent_per_notch() {
        let Some(Wheel::Zoom(f)) = classify(input(Vec2::new(0.0, 40.0), true)) else { panic!("zoom") };
        assert!((f - 1.05).abs() < 1e-5, "{f}");
        let Some(Wheel::Zoom(f)) = classify(input(Vec2::new(0.0, -40.0), true)) else { panic!("zoom") };
        assert!((f - 1.0 / 1.05).abs() < 1e-5, "{f}");
        // Smoothed over frames, the parts multiply back to one notch.
        let parts = [12.0, 16.0, 8.0, 4.0];
        let total: f32 = parts
            .iter()
            .map(|d| match classify(input(Vec2::new(0.0, *d), true)) {
                Some(Wheel::Zoom(f)) => f,
                _ => 1.0,
            })
            .product();
        assert!((total - 1.05).abs() < 1e-4, "{total}");
    }

    #[test]
    fn pinch_and_command_scroll_still_zoom() {
        let i = Input { zoom_delta: 1.2, ..input(Vec2::ZERO, false) };
        assert_eq!(classify(i), Some(Wheel::Zoom(1.2)));
        // The gesture zoom wins over Alt (⌘⌥ + scroll).
        let i = Input { zoom_delta: 0.8, ..input(Vec2::new(0.0, 40.0), true) };
        assert_eq!(classify(i), Some(Wheel::Zoom(0.8)));
    }

    #[test]
    fn zoom_with_scroll_wheel_preference() {
        let i = Input { zoom_with_wheel: true, ..input(Vec2::new(0.0, 200.0), false) };
        let Some(Wheel::Zoom(f)) = classify(i) else { panic!("zoom") };
        assert!((f - std::f32::consts::E).abs() < 1e-4);
    }

    #[test]
    fn hostile_numbers_never_produce_a_bad_zoom() {
        for s in [f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30] {
            for alt in [false, true] {
                for notch in [0.0, -1.0, f32::NAN, 40.0] {
                    for zd in [f32::NAN, 0.0, -1.0, f32::INFINITY, 1.0] {
                        let i = Input { scroll: Vec2::new(0.0, s), zoom_delta: zd, alt, zoom_with_wheel: alt, notch };
                        if let Some(Wheel::Zoom(f)) = classify(i) {
                            assert!(f.is_finite() && f > 0.0, "{f} from {i:?}");
                        }
                    }
                }
            }
        }
    }
}

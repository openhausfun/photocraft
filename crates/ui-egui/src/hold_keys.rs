//! Held keys: Photoshop's temporary tools (#249).
//!
//! - Space: the Hand tool while held.
//! - ⌘Space (Ctrl+Space off the Mac): Zoom In while held; a drag is a scrubby zoom.
//! - ⌘⌥Space: Zoom Out while held.
//! - Space while a marquee, lasso or shape is being dragged repositions it; releasing Space
//!   goes back to sizing it (the Crop tool does the same for its frame, `crop_ui`).
//!
//! The current tool is never changed, so releasing the key gives the previous tool back. A
//! temporary tool that started a drag lasts until the button is released, as in Photoshop.
//!
//! The keys are bindings, not commands: they come from
//! [`photocraft_engine::prefs::TEMPORARY_TOOLS`] with Edit › Keyboard Shortcuts overrides on top
//! (Tools › Temporary), and the reposition key is whatever the Hand key is.

use egui::{InputState, KeyboardShortcut};

use crate::PhotocraftApp;
use crate::shortcuts::parse;
use crate::state::Tool;

/// A tool in effect only while its key is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Temporary {
    Hand,
    ZoomIn,
    ZoomOut,
}

impl Temporary {
    pub fn id(self) -> &'static str {
        match self {
            Temporary::Hand => "tools.temporary.hand",
            Temporary::ZoomIn => "tools.temporary.zoomIn",
            Temporary::ZoomOut => "tools.temporary.zoomOut",
        }
    }

    pub fn tool(self) -> Tool {
        match self {
            Temporary::Hand => Tool::Hand,
            Temporary::ZoomIn | Temporary::ZoomOut => Tool::Zoom,
        }
    }

    const ALL: [Temporary; 3] = [Temporary::Hand, Temporary::ZoomIn, Temporary::ZoomOut];
}

/// Is `id` a held temporary tool rather than a command?
pub fn is_temporary(id: &str) -> bool {
    photocraft_engine::prefs::TEMPORARY_TOOLS.iter().any(|t| t.0 == id)
}

/// The key binding of a temporary tool in effect (`None` when removed or unparsable).
pub fn binding(app: &PhotocraftApp, t: Temporary) -> Option<KeyboardShortcut> {
    let def = photocraft_engine::prefs::TEMPORARY_TOOLS.iter().find(|x| x.0 == t.id()).map(|x| x.2);
    parse(app.session.prefs().shortcut(t.id(), def)?)
}

/// Is `sc` held: its key down with at least its modifiers (more may be held, so ⇧ still squares
/// a marquee while Space is down).
fn held(i: &InputState, sc: &KeyboardShortcut) -> bool {
    let (want, m) = (sc.modifiers, i.modifiers);
    i.key_down(sc.logical_key)
        && (!want.alt || m.alt)
        && (!want.shift || m.shift)
        && (!want.command || m.command)
        && (!want.ctrl || m.ctrl)
        && (!want.mac_cmd || m.mac_cmd)
}

/// The temporary tool whose key is held now, the most specific binding first (⌘⌥Space before
/// ⌘Space before Space). Never while a text field has the keyboard.
pub fn held_tool(app: &PhotocraftApp, ctx: &egui::Context) -> Option<Temporary> {
    if ctx.text_edit_focused() {
        return None;
    }
    let count = |sc: &KeyboardShortcut| {
        let m = sc.modifiers;
        m.alt as u8 + m.shift as u8 + (m.command || m.mac_cmd) as u8 + (m.ctrl && !m.command) as u8
    };
    let mut best: Option<(u8, Temporary)> = None;
    ctx.input(|i| {
        for t in Temporary::ALL {
            if let Some(sc) = binding(app, t)
                && held(i, &sc)
                && best.is_none_or(|(n, _)| count(&sc) > n)
            {
                best = Some((count(&sc), t));
            }
        }
    });
    best.map(|(_, t)| t)
}

/// Is the reposition key (the Hand key, any modifiers) down?
pub fn reposition_held(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    !ctx.text_edit_focused() && binding(app, Temporary::Hand).is_some_and(|sc| ctx.input(|i| i.key_down(sc.logical_key)))
}

/// Tools whose in-progress drag the reposition key moves.
pub fn repositions(t: Tool) -> bool {
    matches!(t, Tool::RectMarquee | Tool::EllipseMarquee | Tool::Lasso | Tool::ObjectSelection) || crate::vector_ui::is_shape_tool(t)
}

fn latch_id() -> egui::Id {
    egui::Id::new("pc-temporary-tool")
}

/// The temporary tool for this canvas frame: the held one, except while a selection is being
/// drawn (the key repositions it instead); a temporary tool that was in effect when the button
/// went down stays until the button comes up.
pub fn for_frame(app: &PhotocraftApp, ctx: &egui::Context, drawing: bool) -> Option<Temporary> {
    let down = ctx.input(|i| i.pointer.primary_down());
    let latched = ctx.data(|d| d.get_temp::<Option<Temporary>>(latch_id())).flatten();
    let t = if down && latched.is_some() {
        latched
    } else if drawing {
        None
    } else {
        held_tool(app, ctx)
    };
    ctx.data_mut(|d| d.insert_temp(latch_id(), if down { t } else { None }));
    t
}

#[cfg(test)]
mod tests;

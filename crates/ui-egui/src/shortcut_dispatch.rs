//! Keyboard shortcut table and dispatch (#127): every shortcut the menus display fires.
//!
//! The menus show Photoshop's default shortcuts from [`crate::menu_catalog::CATALOG`], while
//! commands also declare their own (`CommandSpec::shortcut`, [`crate::menus::UI_COMMANDS`]).
//! The binding table merges all three, with Edit › Keyboard Shortcuts overrides on top, so a
//! shortcut printed next to a live menu item always reaches that item. A shortcut whose command
//! is disabled in the current state reports why on the status bar instead of doing nothing
//! silently, as the greyed menu item shows.

use egui::{Key, KeyboardShortcut};
use serde_json::json;

use crate::PhotocraftApp;
use crate::shortcuts::{consume, parse};

/// What a dispatched shortcut did (kept for tests and `ui.inspect`-style debugging).
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Ran,
    Disabled(String),
    Failed(String),
}

fn log_id() -> egui::Id {
    egui::Id::new("pc-shortcut-log")
}

fn dry_run_id() -> egui::Id {
    egui::Id::new("pc-shortcut-dry-run")
}

/// Tests: resolve and check shortcuts without running them (File › Exit, Open, browser links…).
pub fn set_dry_run(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(dry_run_id(), on));
}

/// The shortcuts dispatched since the last call (command id and outcome), oldest first.
pub fn take_log(ctx: &egui::Context) -> Vec<(String, Outcome)> {
    ctx.data_mut(|d| d.remove_temp::<Vec<(String, Outcome)>>(log_id())).unwrap_or_default()
}

/// Alternative default shortcuts Photoshop gives some commands besides the one its menu shows:
/// (command, shortcut). Edit › Fill… is Shift+F5 and also Shift+Backspace (Shift+Delete on a Mac).
pub const SECONDARY: &[(&str, &str)] = &[("edit.fill", "Shift+Backspace")];

/// Every key binding, most modifiers first (so ⇧⌘Z wins over ⌘Z), each key owned by one command:
/// command and shell shortcuts, then the menu catalogue's for live items without their own,
/// then Edit › Keyboard Shortcuts assignments to any other menu item. D and X
/// (`tools.defaultColors` / `tools.swapColors`) and the fill keys are commands like any other, so
/// their overrides apply. Held temporary tools (Space…) are not here: see [`crate::hold_keys`].
pub fn bindings(app: &PhotocraftApp) -> Vec<(String, KeyboardShortcut)> {
    let prefs = app.session.prefs();
    let ui = crate::menus::UI_COMMANDS.iter().map(|(id, _, _, sc)| (*id, prefs.shortcut(id, *sc)));
    let engine = photocraft_engine::command_specs().iter().map(|c| (c.id, prefs.shortcut(c.id, c.shortcut)));
    let own: std::collections::HashSet<&str> = crate::menus::UI_COMMANDS
        .iter()
        .map(|c| c.0)
        .chain(photocraft_engine::command_specs().iter().filter(|c| c.shortcut.is_some() || prefs.shortcuts.contains_key(c.id)).map(|c| c.id))
        .collect();
    let catalog = crate::menu_catalog::CATALOG
        .iter()
        .filter(|(_, _, sc, id)| sc.is_some() && !own.contains(id) && crate::menus::is_live(id))
        .map(|&(_, _, sc, id)| (id, prefs.shortcut(id, sc)));
    let overrides = prefs
        .shortcuts
        .iter()
        .filter(|(id, sc)| {
            !sc.is_empty()
                && photocraft_engine::commands::find(id).is_none()
                && !crate::menus::UI_COMMANDS.iter().any(|c| c.0 == id.as_str())
                && !crate::hold_keys::is_temporary(id)
        })
        .map(|(id, sc)| (id.as_str(), Some(sc.as_str())));
    // Photoshop's second shortcuts, kept while the command's main one is the default.
    let secondary = SECONDARY.iter().filter(|(id, _)| !prefs.shortcuts.contains_key(*id)).map(|&(id, sc)| (id, Some(sc)));
    let mut all: Vec<(String, KeyboardShortcut)> = Vec::new();
    for (id, sc) in ui.chain(engine).chain(catalog).chain(overrides).chain(secondary) {
        let Some(sc) = sc.and_then(parse) else { continue };
        if !all.iter().any(|(_, b)| *b == sc) {
            all.push((id.to_string(), sc));
        }
    }
    all.sort_by_key(|(_, sc)| std::cmp::Reverse(sc.modifiers.shift as u8 + sc.modifiers.alt as u8 + sc.modifiers.command as u8));
    all
}

/// Where keyboard focus is: nowhere (the canvas), on a widget (a slider, the Curves graph, a
/// button reached with Tab), or in a text field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    None,
    Widget,
    Text,
}

impl Focus {
    pub fn of(ctx: &egui::Context) -> Focus {
        // A layer rename keeps its keys even in the frame egui drops its focus (Esc, Tab).
        if ctx.text_edit_focused() || crate::layer_row_ui::rename_active(ctx) {
            Focus::Text
        } else if ctx.egui_wants_keyboard_input() {
            Focus::Widget
        } else {
            Focus::None
        }
    }

    /// May `sc` fire with this focus? A focused widget keeps its navigation keys (arrows, ↩,
    /// Space, Delete…); a text field keeps everything but ⌘ shortcuts (minus its own editing
    /// ones: select all, clipboard, undo) and the function keys.
    pub fn allows(self, sc: &KeyboardShortcut) -> bool {
        let k = sc.logical_key;
        let function = matches!(
            k,
            Key::F1
                | Key::F2
                | Key::F3
                | Key::F4
                | Key::F5
                | Key::F6
                | Key::F7
                | Key::F8
                | Key::F9
                | Key::F10
                | Key::F11
                | Key::F12
                | Key::F13
                | Key::F14
                | Key::F15
        );
        let navigation = matches!(
            k,
            Key::ArrowLeft
                | Key::ArrowRight
                | Key::ArrowUp
                | Key::ArrowDown
                | Key::Enter
                | Key::Space
                | Key::Tab
                | Key::Escape
                | Key::Backspace
                | Key::Delete
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
        );
        match self {
            Focus::None => true,
            Focus::Widget => sc.modifiers.command || !navigation,
            Focus::Text => {
                let text_edit = !sc.modifiers.alt && matches!(k, Key::A | Key::C | Key::X | Key::V | Key::Z | Key::Y) || navigation;
                function || (sc.modifiers.command && !text_edit)
            }
        }
    }
}

/// The command whose shortcut was pressed this frame (its key press consumed), if any.
/// While typing (inline type), clipboard and select-all shortcuts belong to the text.
pub fn pressed_command(app: &PhotocraftApp, ctx: &egui::Context, focus: Focus, editing: bool) -> Option<String> {
    // Building the table walks the registry: only do it when a key went down.
    if !ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))) {
        return None;
    }
    for (id, sc) in bindings(app) {
        if !focus.allows(&sc) {
            continue;
        }
        if editing && matches!(id.as_str(), "edit.copy" | "edit.cut" | "edit.paste" | "edit.copyMerged" | "select.all" | "edit.pasteSpecial.pasteInPlace") {
            continue;
        }
        if consume(ctx, &sc) {
            return Some(id);
        }
    }
    None
}

/// Why `id` can't run now (the engine's reason when it has one).
pub fn disabled_reason(app: &PhotocraftApp, id: &str) -> String {
    photocraft_engine::commands::find(id)
        .and_then(|_| app.session.disabled_reason(id))
        .unwrap_or_else(|| if app.session.active().is_none() { "no document open".into() } else { "not available in the current state".into() })
}

fn label(id: &str) -> String {
    crate::menu_catalog::CATALOG
        .iter()
        .find(|c| c.3 == id)
        .map(|c| c.1)
        .or_else(|| crate::menus::UI_COMMANDS.iter().find(|c| c.0 == id).map(|c| c.1))
        .or_else(|| photocraft_engine::commands::find(id).map(|c| c.label))
        .unwrap_or(id)
        .trim_end_matches('…')
        .to_string()
}

/// Run a shortcut's command like its menu item (dialogs included), or say why it can't run.
pub fn dispatch(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str) {
    let outcome = if !crate::menus::is_enabled(app, id) {
        let why = disabled_reason(app, id);
        app.ui.status = format!("{} is not available: {why}", label(id));
        app.ui.status_error = true;
        Outcome::Disabled(why)
    } else if ctx.data(|d| d.get_temp::<bool>(dry_run_id())).unwrap_or(false) {
        Outcome::Ran
    } else {
        let r = if crate::adjust_dialog::has_dialog(id) {
            crate::adjust_dialog::open(app, id);
            Ok(serde_json::Value::Null)
        } else {
            crate::menus::invoke(app, ctx, id, json!({}))
        };
        match r {
            Ok(_) => Outcome::Ran,
            Err(e) => {
                app.ui.status = e.clone();
                app.ui.status_error = true;
                Outcome::Failed(e)
            }
        }
    };
    ctx.data_mut(|d| {
        let log = d.get_temp_mut_or_default::<Vec<(String, Outcome)>>(log_id());
        if log.len() >= 64 {
            log.remove(0);
        }
        log.push((id.to_string(), outcome));
    });
}

#[cfg(test)]
mod tests;

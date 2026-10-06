//! Opening files from disk: File › Open, Open Recent, drag-and-drop onto the window, command-line
//! paths at launch and the OS's "open these documents" requests (macOS Finder double-click, Open
//! With, drops on the Dock icon) all go through [`PhotocraftApp::open_file`], so each one names the
//! document after the file, remembers its path (File › Save writes back to it) and adds it to Open
//! Recent. Failures are shown as errors (status bar + notice); import warnings as a notice.

use crate::{PhotocraftApp, notices};

/// A request from the operating system, delivered by the platform shell through
/// [`Services::os_events`](crate::Services::os_events).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OsEvent {
    /// Open these files (absolute paths), e.g. a Finder double-click.
    Open(Vec<String>),
    /// Quit (e.g. the Dock menu's Quit, log out): goes through the normal close path, which asks
    /// about unsaved changes.
    Quit,
}

/// The file name of `path` (the document's display name), or `path` itself when it has none.
pub fn display_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string())
}

impl PhotocraftApp {
    /// Open `bytes` read from the file at `path`: the document is named after the file, keeps
    /// `path` for File › Save, and `path` goes to the top of Open Recent. Returns the import
    /// warnings (also shown to the user).
    pub fn open_file(&mut self, path: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
        // Brushes/gradients go to the preset libraries (no document, no Open Recent entry).
        if let Some(r) = crate::preset_files_ui::open(self, path, bytes) {
            return r.map(|()| Vec::new());
        }
        if self.background_jobs {
            // The path and Open Recent are recorded when the background open finishes.
            crate::jobs_ui::start_open(self, &display_name(path), Some(path.to_string()), crate::jobs_ui::bytes(bytes))?;
            return Ok(Vec::new());
        }
        let warnings = self.open_bytes(&display_name(path), bytes)?;
        if let Some(st) = self.session.active_mut() {
            st.path = Some(path.to_string());
        }
        self.push_recent(path);
        Ok(warnings)
    }

    /// Read and open the file at `path` (see [`open_file`](Self::open_file)).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(&mut self, path: &str) -> Result<Vec<String>, String> {
        // Documents opening in the background are read on the worker too (a 2 GB PSB read
        // would block the window). Preset files (brushes, gradients) go the usual way.
        let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if self.background_jobs && !crate::preset_files_ui::PRESET_EXTS.contains(&ext.as_str()) {
            crate::jobs_ui::start_open(self, &display_name(path), Some(path.to_string()), photocraft_engine::jobs::OpenSource::Path(path.to_string()))?;
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        self.open_file(path, &bytes)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn open_path(&mut self, path: &str) -> Result<Vec<String>, String> {
        Err(format!("cannot open paths on the web: {path}"))
    }

    /// Open several files, reporting each failure to the user instead of stopping. Relative paths
    /// are resolved against the working directory so the remembered path stays valid. Returns how
    /// many opened.
    pub fn open_paths(&mut self, paths: &[String]) -> usize {
        let mut opened = 0;
        for p in paths {
            let abs = absolute(p);
            match self.open_path(&abs) {
                Ok(_) => opened += 1,
                Err(e) => self.open_failed(&display_name(p), &e),
            }
        }
        opened
    }

    /// Show "Couldn't open <name>: <error>" as an error (status bar + notice).
    pub fn open_failed(&mut self, name: &str, err: &str) {
        let msg = format!("Couldn't open {name}: {err}");
        log::warn!("{msg}");
        notices::error(self, msg);
    }

    /// Files dropped onto the window. Native drops carry an absolute path and open like File ›
    /// Open; anything else (no path) opens by name only.
    pub fn open_dropped(&mut self, files: Vec<egui::DroppedFileHandle>) {
        for f in files {
            let path = f.path().to_string_lossy().to_string();
            let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped".into());
            let opened =
                crate::read_dropped(&*f).and_then(|bytes| if f.path().is_absolute() { self.open_file(&path, &bytes) } else { self.open_bytes(&name, &bytes) });
            if let Err(e) = opened {
                self.open_failed(&name, &e);
            }
        }
    }

    /// Handle the OS requests queued since the last frame.
    pub(crate) fn drain_os_events(&mut self, ctx: &egui::Context) {
        let events = self.services.os_events.as_mut().map(|f| f()).unwrap_or_default();
        for e in events {
            match e {
                OsEvent::Open(paths) => {
                    self.open_paths(&paths);
                    // Bring the window forward when another app (Finder, the Dock) asked us to open.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.request_repaint();
                }
                OsEvent::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn absolute(p: &str) -> String {
    std::path::absolute(p).map(|a| a.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string())
}

#[cfg(target_arch = "wasm32")]
fn absolute(p: &str) -> String {
    p.to_string()
}

#[cfg(test)]
mod tests;

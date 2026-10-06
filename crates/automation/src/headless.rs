//! Headless backend: a `photocraft_engine::Session` plus file I/O. Synchronous
//! and UI-free; the MCP server and the CLI both drive it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use photocraft_engine::{Session, command_specs};
use photocraft_format::PcraftWriter;
use photocraft_io::ExportOptions;
use serde_json::{Value, json};

use crate::workspace::authorize_engine_command;
use crate::{AuthorizedWorkspace, AutomationError, files};

enum Filesystem {
    Denied,
    TrustedLocal,
    Workspace(AuthorizedWorkspace),
}

/// A headless editing session.
pub struct Headless {
    pub session: Session,
    /// Incremental `.pcraft` writers per document id.
    writers: HashMap<u64, PcraftWriter>,
    filesystem: Filesystem,
}

impl Default for Headless {
    fn default() -> Self {
        Self { session: Session::new(), writers: HashMap::new(), filesystem: Filesystem::Denied }
    }
}

impl Headless {
    /// Create an automation session with no filesystem authority.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a session for an explicit local CLI invocation. The CLI caller,
    /// not a remote automation client, supplies these host paths.
    pub fn trusted_local() -> Self {
        Self { session: Session::new(), writers: HashMap::new(), filesystem: Filesystem::TrustedLocal }
    }

    /// Create an automation session with capability-scoped file access.
    pub fn with_workspace(workspace: AuthorizedWorkspace) -> Self {
        Self { session: Session::new(), writers: HashMap::new(), filesystem: Filesystem::Workspace(workspace) }
    }

    fn doc_index(&self, index: Option<usize>) -> Result<usize, AutomationError> {
        match index {
            Some(i) if i < self.session.documents().len() => Ok(i),
            Some(i) => Err(AutomationError::BadRequest(format!("no document at index {i}"))),
            None => self.session.active_index().ok_or_else(|| AutomationError::BadRequest("no document open".into())),
        }
    }

    /// Open a file and make it the active document.
    pub fn open(&mut self, path: &Path) -> Result<Value, AutomationError> {
        let requested = path.to_str().ok_or_else(|| AutomationError::BadRequest("automation paths must be valid UTF-8".into()))?;
        let o = match &self.filesystem {
            Filesystem::Denied => return Err(AutomationError::BadRequest("automation filesystem access is not granted: read authority is absent".into())),
            Filesystem::TrustedLocal => files::open(path)?,
            Filesystem::Workspace(workspace) => {
                let bytes = workspace.read(requested)?;
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(requested);
                files::open_bytes(name, &bytes)?
            }
        };
        let index = self.session.add_document(o.document, Some(requested.to_string()));
        let d = &self.session.documents()[index];
        Ok(json!({
            "index": index,
            "name": d.doc.name,
            "width": d.doc.size.width,
            "height": d.doc.size.height,
            "layers": d.doc.layer_count(),
            "warnings": o.warnings,
        }))
    }

    /// Save (`.pcraft` or any export format by extension). With no path,
    /// saves to the document's own path.
    pub fn save(&mut self, index: Option<usize>, path: Option<&Path>, format: Option<&str>, opts: &ExportOptions) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        let (stored_path, doc) = {
            let state = &self.session.documents()[i];
            (state.path.clone(), state.doc.clone())
        };
        let target: PathBuf = match (path, stored_path.as_deref()) {
            (Some(p), _) => p.to_path_buf(),
            (None, Some(p)) => PathBuf::from(p),
            (None, None) => {
                return Err(AutomationError::BadRequest("document has no path; pass `path`".into()));
            }
        };
        let target_text = target.to_str().ok_or_else(|| AutomationError::BadRequest("automation paths must be valid UTF-8".into()))?;
        let warnings = match &self.filesystem {
            Filesystem::Denied => return Err(AutomationError::BadRequest("automation filesystem access is not granted: write authority is absent".into())),
            Filesystem::TrustedLocal => {
                let writer = self.writers.entry(doc.id.0).or_default();
                files::save(&doc, &target, format, opts, Some(writer))?
            }
            Filesystem::Workspace(workspace) => {
                let (bytes, warnings) = files::save_bytes(&doc, target_text, format, opts)?;
                workspace.write(target_text, &bytes)?;
                warnings
            }
        };
        let is_native = format
            .map(|f| f.trim_start_matches('.').eq_ignore_ascii_case("pcraft"))
            .unwrap_or_else(|| target.extension().is_some_and(|e| e.eq_ignore_ascii_case("pcraft")));
        if is_native {
            self.session.set_active(i);
            if let Some(st) = self.session.active_mut() {
                st.saved_revision = st.revision;
                st.path = Some(target.to_string_lossy().into_owned());
            }
        }
        Ok(json!({ "path": target.to_string_lossy(), "warnings": warnings }))
    }

    /// Write rendered bytes through the configured write authority.
    pub fn write_render(&self, path: &Path, bytes: &[u8]) -> Result<(), AutomationError> {
        let requested = path.to_str().ok_or_else(|| AutomationError::BadRequest("automation paths must be valid UTF-8".into()))?;
        match &self.filesystem {
            Filesystem::Denied => Err(AutomationError::BadRequest("automation filesystem access is not granted: write authority is absent".into())),
            Filesystem::TrustedLocal => photocraft_format::atomic_write(path, bytes).map_err(|error| AutomationError::Io(error.to_string())),
            Filesystem::Workspace(workspace) => workspace.write(requested, bytes),
        }
    }

    pub fn inspect(&self, index: Option<usize>) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        Ok(photocraft_engine::inspect::document(&self.session.documents()[i]))
    }

    pub fn render_png(&self, index: Option<usize>, max_side: u32) -> Result<Vec<u8>, AutomationError> {
        let i = self.doc_index(index)?;
        let doc = &self.session.documents()[i].doc;
        if !matches!(&self.filesystem, Filesystem::TrustedLocal) {
            crate::budgets::check_preview(doc.size.width, doc.size.height, max_side)?;
        }
        let png = files::render_png(doc, max_side)?;
        if !matches!(&self.filesystem, Filesystem::TrustedLocal) {
            crate::budgets::check_png(png.len())?;
        }
        Ok(png)
    }

    pub fn session_list(&self) -> Value {
        photocraft_engine::inspect::session(&self.session)
    }

    pub fn select(&mut self, index: usize) -> Result<Value, AutomationError> {
        if self.session.set_active(index) { Ok(self.session_list()) } else { Err(AutomationError::BadRequest(format!("no document at index {index}"))) }
    }

    pub fn close(&mut self, index: Option<usize>) -> Result<Value, AutomationError> {
        let i = self.doc_index(index)?;
        if let Some(d) = self.session.close(i) {
            self.writers.remove(&d.doc.id.0);
        }
        Ok(self.session_list())
    }

    pub fn command_list(&self) -> Value {
        Value::Array(
            command_specs()
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "menu": c.menu,
                        "shortcut": c.shortcut,
                        "params": c.params,
                        "enabled": self.session.is_enabled(c.id),
                    })
                })
                .collect(),
        )
    }

    pub fn command_run(&mut self, id: &str, params: Value) -> Result<Value, AutomationError> {
        self.command_start(id, params, true)
    }

    /// Run a command; with `wait` false, a job-capable command (filters, Content-Aware Fill,
    /// Photomerge, …) runs in the background and this returns `{"job": id}` at once (poll with
    /// `jobs.list`, stop with `jobs.cancel`). Other commands finish before returning either way.
    pub fn command_start(&mut self, id: &str, params: Value, wait: bool) -> Result<Value, AutomationError> {
        let params = if params.is_null() { json!({}) } else { params };
        if !matches!(&self.filesystem, Filesystem::TrustedLocal) {
            authorize_engine_command(id, &params)?;
        }
        // Background jobs that finished since the last request are applied first.
        self.session.poll_jobs();
        if wait {
            return Ok(self.session.execute(id, params)?);
        }
        Ok(match self.session.start(id, params)? {
            photocraft_engine::jobs::Started::Done(v) => v,
            photocraft_engine::jobs::Started::Job(job) => json!({"job": job.0, "pending": true}),
        })
    }
}

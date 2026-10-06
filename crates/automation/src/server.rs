//! The MCP server (rmcp). Tool names use `snake_case` with underscores (dots
//! are not valid in every client's tool-name grammar).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bridge::BridgeClient;
use crate::budgets::{BatchReplyBudget, MAX_PREVIEW_SIDE, check_png, json_bytes};
use crate::headless::Headless;
use crate::security::MAX_BATCH_STEPS;
use crate::{AuthorizedWorkspace, AutomationError, files};

/// Where tools are executed.
pub enum Backend {
    /// In-process engine session.
    Headless(Arc<Mutex<Headless>>),
    /// A running desktop app reached over the control protocol.
    Bridge(Arc<BridgeClient>),
}

#[derive(Clone)]
pub struct PhotocraftMcp {
    backend: Arc<Backend>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DocIndex {
    /// Document index from `session_list` (default: the active document).
    #[serde(default)]
    pub index: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenParams {
    /// Forward-slash relative path beneath the configured automation read root.
    pub path: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NewParams {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// rgb | gray | cmyk | lab
    #[serde(default)]
    pub mode: Option<String>,
    /// 8 | 16 | 32
    #[serde(default)]
    pub depth: Option<u32>,
    /// white | black | transparent | #rrggbb
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveParams {
    /// Forward-slash relative target beneath the configured automation write root.
    /// The extension selects the format. Omit to save to the document's own relative path.
    #[serde(default)]
    pub path: Option<String>,
    /// Format override as an extension (pcraft, psd, png, jpg, tif, webp, exr, …).
    #[serde(default)]
    pub format: Option<String>,
    /// JPEG quality 1..100 for lossy exports.
    #[serde(default)]
    pub quality: Option<u8>,
    #[serde(default)]
    pub index: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PreviewParams {
    #[serde(default)]
    pub index: Option<usize>,
    /// Longest side of a headless preview (default 1024, maximum 2048).
    /// Zero requests full size within that ceiling. Bridge mode returns the window screenshot.
    #[serde(default)]
    pub max_side: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectParams {
    pub index: usize,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Only commands whose id or label contains this text (case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
    /// Only commands that can run right now.
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Command id, e.g. `layer.new.layer`, `filter.blur.gaussian`.
    pub id: String,
    /// Command parameters as a JSON object (see the `params` doc in `command_list`).
    #[serde(default)]
    pub params: Option<Value>,
    /// Wait for a long command (filters, Content-Aware Fill, Photomerge, …) to finish (default
    /// true). With false it runs as a background job and the result is `{"job": id}`: poll it
    /// with `jobs_list`, stop it with `jobs_cancel`.
    #[serde(default)]
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobCancelParams {
    /// The job id from `command_run` / `jobs_list` (omit to cancel every running job).
    #[serde(default)]
    pub job: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchParams {
    /// Commands to run in order: `[{"id": "layer.new.layer", "params": {"name": "Ink"}}, …]`.
    pub steps: Vec<RunParams>,
    /// Stop at the first failing step (default true).
    #[serde(default)]
    pub stop_on_error: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PointerParams {
    /// Events in document coordinates: `[{"kind":"down|move|up","x":..,"y":..,"pressure":..}]`.
    pub events: Vec<Value>,
    /// Modifier keys, e.g. `{"shift":true}`.
    #[serde(default)]
    pub modifiers: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MenuParams {
    /// Menu item / command id.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UiSetParams {
    /// Fields accepted by the control method `ui.set` (tool, panels, zoom, center, dark).
    pub fields: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ControlParams {
    /// Any control-protocol method, e.g. `ui.dialog.open`.
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ok_json(v: &Value) -> CallToolResult {
    let text = match json_bytes(v).and_then(|bytes| String::from_utf8(bytes).map_err(|error| AutomationError::Other(error.to_string()))) {
        Ok(text) => text,
        Err(error) => return fail(format!("{error}; operation may have completed")),
    };
    bounded_tool_result(CallToolResult::success(vec![Content::text(text)]))
}

fn bounded_tool_result(result: CallToolResult) -> CallToolResult {
    match json_bytes(&result) {
        Ok(_) => result,
        Err(error) => fail(format!("{error}; operation may have completed")),
    }
}

fn fail(e: impl std::fmt::Display) -> CallToolResult {
    let result = CallToolResult::error(vec![Content::text(e.to_string())]);
    if json_bytes(&result).is_err() {
        return CallToolResult::error(vec![Content::text("response budget exceeded; operation may have completed")]);
    }
    result
}

/// Neither backend took the call. The backend is always headless or a bridge, so this only
/// guards against a future backend kind.
fn no_backend() -> CallToolResult {
    fail("internal error: no headless session or app bridge")
}

fn png_result(png: &[u8], note: String) -> CallToolResult {
    if let Err(error) = check_png(png.len()) {
        return fail(error);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    bounded_tool_result(CallToolResult::success(vec![Content::image(b64, "image/png"), Content::text(note)]))
}

fn to_result(r: Result<Value, AutomationError>) -> Result<CallToolResult, McpError> {
    Ok(match r {
        Ok(v) => ok_json(&v),
        Err(e) => fail(e),
    })
}

fn bridge_only(name: &str) -> Result<CallToolResult, McpError> {
    Ok(fail(format!(
        "`{name}` drives the live GUI and needs bridge mode: start the app with \
         `photocraft --control <port> --control-token-file <path>`, then run \
         `photocraft-cli mcp --bridge 127.0.0.1:<port> --control-token-file <path>`"
    )))
}

impl PhotocraftMcp {
    pub fn headless() -> Self {
        Self::with_backend(Backend::Headless(Arc::new(Mutex::new(Headless::new()))))
    }

    pub fn headless_with_workspace(workspace: AuthorizedWorkspace) -> Self {
        Self::with_backend(Backend::Headless(Arc::new(Mutex::new(Headless::with_workspace(workspace)))))
    }

    pub fn bridge(addr: &str, token: &str) -> Result<Self, AutomationError> {
        Ok(Self::with_backend(Backend::Bridge(Arc::new(BridgeClient::new(addr, token)?))))
    }

    pub fn with_backend(backend: Backend) -> Self {
        PhotocraftMcp { backend: Arc::new(backend), tool_router: Self::tool_router() }
    }

    /// Serve MCP over stdin/stdout until the client disconnects.
    pub async fn serve_stdio(self) -> Result<(), AutomationError> {
        let running = self.serve(rmcp::transport::stdio()).await.map_err(|e| AutomationError::Other(format!("MCP init: {e}")))?;
        running.waiting().await.map_err(|e| AutomationError::Other(e.to_string()))?;
        Ok(())
    }

    /// Run `f` on the headless session on a blocking thread.
    async fn headless_op<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Headless) -> Result<T, AutomationError> + Send + 'static,
    ) -> Option<Result<T, AutomationError>> {
        let Backend::Headless(h) = &*self.backend else {
            return None;
        };
        let h = h.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                // A panicking command poisons the lock, but edits run on a copy
                // of the document (`Session::edit`), so the session is still
                // consistent: keep serving instead of failing every later call.
                let mut g = h.lock().unwrap_or_else(PoisonError::into_inner);
                f(&mut g)
            })
            .await
            .unwrap_or_else(|e| Err(AutomationError::Other(format!("task failed: {e}")))),
        )
    }

    fn bridge_client(&self) -> Option<&BridgeClient> {
        match &*self.backend {
            Backend::Bridge(b) => Some(b),
            Backend::Headless(_) => None,
        }
    }

    async fn screenshot(&self, b: &BridgeClient, max_side: Option<u32>) -> Result<CallToolResult, McpError> {
        if max_side.is_some_and(|side| side > MAX_PREVIEW_SIDE) {
            return Ok(fail(format!("automation preview side exceeds {MAX_PREVIEW_SIDE} pixels")));
        }
        let response = match b.call("ui.screenshot", json!({})).await {
            Ok(response) => response,
            Err(error) => return Ok(fail(error)),
        };
        let Some(encoded) = response.get("base64").and_then(Value::as_str) else {
            return Ok(fail("app screenshot response did not contain PNG data"));
        };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(bytes) => bytes,
            Err(error) => return Ok(fail(format!("app screenshot data: {error}"))),
        };
        if let Err(error) = check_png(bytes.len()) {
            return Ok(fail(error));
        }
        let bytes = match downscale_png(&bytes, max_side.unwrap_or(0)) {
            Ok(Some(small)) => small,
            Ok(None) => bytes,
            Err(error) => return Ok(fail(error)),
        };
        Ok(png_result(&bytes, "screenshot of the live app window".into()))
    }
}

fn downscale_png(png: &[u8], max_side: u32) -> Result<Option<Vec<u8>>, AutomationError> {
    // A compressed reply's byte limit does not bound its decoded pixel allocation.
    let opts = photocraft_codecs::DecodeOptions {
        limits: photocraft_codecs::Limits { max_width: 8192, max_height: 8192, max_pixels: 16 << 20, max_alloc: 64 << 20 },
        ..Default::default()
    };
    let img = photocraft_codecs::decode_as_with(photocraft_codecs::Format::Png, png, &opts).map_err(|error| AutomationError::Other(error.to_string()))?;
    let (w, h) = img.dimensions();
    if max_side == 0 || w.max(h) <= max_side {
        return Ok(None);
    }
    let s = max_side as f32 / w.max(h) as f32;
    let (nw, nh) = (((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1));
    let src = img.to_rgba8();
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let sx = ((x as f32 + 0.5) / s) as u32;
            let sy = ((y as f32 + 0.5) / s) as u32;
            let si = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
            let di = ((y * nw + x) * 4) as usize;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    let small =
        photocraft_codecs::Image::from_u8(nw, nh, photocraft_codecs::ChannelLayout::Rgba, out).map_err(|error| AutomationError::Other(error.to_string()))?;
    let bytes =
        photocraft_codecs::encode(&small, photocraft_codecs::Format::Png, &Default::default()).map_err(|error| AutomationError::Other(error.to_string()))?;
    Ok(Some(bytes))
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl PhotocraftMcp {
    #[tool(description = "List open documents (index, name, size, dirty) and the active index.")]
    async fn session_list(&self) -> Result<CallToolResult, McpError> {
        if let Some(r) = self.headless_op(|h| Ok(h.session_list())).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        to_result(b.call("ui.inspect", json!({})).await)
    }

    #[tool(description = "Open an image or document file and make it active. Returns index, size and import warnings.")]
    async fn doc_open(&self, Parameters(p): Parameters<OpenParams>) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&p.path);
        if let Some(r) = self.headless_op(move |h| h.open(&path)).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        to_result(b.call("app.open", json!({"path": p.path})).await)
    }

    #[tool(description = "Create a new document (defaults: 1920x1080 RGB 8-bit white).")]
    async fn doc_new(&self, Parameters(p): Parameters<NewParams>) -> Result<CallToolResult, McpError> {
        let mut params = serde_json::Map::new();
        if let Some(v) = p.width {
            params.insert("width".into(), v.into());
        }
        if let Some(v) = p.height {
            params.insert("height".into(), v.into());
        }
        if let Some(v) = p.mode {
            params.insert("mode".into(), v.into());
        }
        if let Some(v) = p.depth {
            params.insert("depth".into(), v.into());
        }
        if let Some(v) = p.background {
            params.insert("background".into(), v.into());
        }
        if let Some(v) = p.name {
            params.insert("name".into(), v.into());
        }
        self.run_command("file.new".into(), Value::Object(params), true).await
    }

    #[tool(description = "Save the document. `.pcraft` is the lossless native format (incremental); other extensions \
        (psd, png, jpg, tif, webp, exr, …) export. Returns warnings about anything the format cannot hold.")]
    async fn doc_save(&self, Parameters(p): Parameters<SaveParams>) -> Result<CallToolResult, McpError> {
        self.save_impl(p).await
    }

    #[tool(description = "Export the document to another format (same as doc_save with an explicit path/format).")]
    async fn doc_export(&self, Parameters(p): Parameters<SaveParams>) -> Result<CallToolResult, McpError> {
        if p.path.is_none() {
            return Ok(fail("doc_export needs `path`"));
        }
        self.save_impl(p).await
    }

    #[tool(description = "Document state as JSON: layer tree (top to bottom), history, selection, active layer.")]
    async fn doc_inspect(&self, Parameters(p): Parameters<DocIndex>) -> Result<CallToolResult, McpError> {
        if let Some(r) = self.headless_op(move |h| h.inspect(p.index)).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        let params = p.index.map(|index| json!({"document": index})).unwrap_or_else(|| json!({}));
        to_result(b.call("engine.execute", json!({"command": "document.inspect", "params": params})).await)
    }

    #[tool(description = "Render the flattened document and return it as a PNG image (bridge mode: window screenshot).")]
    async fn doc_render_preview(&self, Parameters(p): Parameters<PreviewParams>) -> Result<CallToolResult, McpError> {
        let max = p.max_side.unwrap_or(1024);
        if let Some(r) = self.headless_op(move |h| h.render_png(p.index, max)).await {
            return Ok(match r {
                Ok(png) => png_result(&png, format!("flattened preview (max side {max})")),
                Err(e) => fail(e),
            });
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        self.screenshot(b, Some(max)).await
    }

    #[tool(description = "Make the document at `index` active.")]
    async fn doc_select(&self, Parameters(p): Parameters<SelectParams>) -> Result<CallToolResult, McpError> {
        match self.headless_op(move |h| h.select(p.index)).await {
            Some(r) => to_result(r),
            None => bridge_only_headless("doc_select"),
        }
    }

    #[tool(description = "Close a document (default: the active one) without saving.")]
    async fn doc_close(&self, Parameters(p): Parameters<DocIndex>) -> Result<CallToolResult, McpError> {
        match self.headless_op(move |h| h.close(p.index)).await {
            Some(r) => to_result(r),
            None => bridge_only_headless("doc_close"),
        }
    }

    #[tool(description = "List engine commands: id, label, menu path, shortcut, parameter doc, enabled now.")]
    async fn command_list(&self, Parameters(p): Parameters<ListParams>) -> Result<CallToolResult, McpError> {
        let all = if let Some(r) = self.headless_op(|h| Ok(h.command_list())).await {
            r
        } else {
            let Some(b) = self.bridge_client() else {
                return Ok(no_backend());
            };
            b.call("engine.commands", json!({})).await
        };
        let all = match all {
            Ok(v) => v,
            Err(e) => return Ok(fail(e)),
        };
        let needle = p.filter.map(|f| f.to_lowercase());
        let enabled_only = p.enabled_only.unwrap_or(false);
        let items: Vec<Value> = all
            .as_array()
            .cloned()
            .unwrap_or_else(|| all.get("commands").and_then(Value::as_array).cloned().unwrap_or_default())
            .into_iter()
            .filter(|c| {
                let hay =
                    format!("{} {}", c.get("id").and_then(Value::as_str).unwrap_or(""), c.get("label").and_then(Value::as_str).unwrap_or("")).to_lowercase();
                needle.as_ref().is_none_or(|n| hay.contains(n)) && (!enabled_only || c.get("enabled").and_then(Value::as_bool).unwrap_or(true))
            })
            .collect();
        Ok(ok_json(&Value::Array(items)))
    }

    #[tool(description = "Run an engine command by id with JSON params (see command_list). Returns the command's JSON result. \
        Long commands (filters, Content-Aware Fill/Scale, Photomerge, brush import) wait to finish unless `wait` is false: \
        then they run as a background job and the result is {job: id} (see jobs_list, jobs_cancel).")]
    async fn command_run(&self, Parameters(p): Parameters<RunParams>) -> Result<CallToolResult, McpError> {
        self.run_command(p.id, p.params.unwrap_or_else(|| json!({})), p.wait.unwrap_or(true)).await
    }

    #[tool(description = "List background jobs: running ones with progress (0-1), message and elapsed time, then the last few \
        that ended (state done|failed|cancelled with their result or error). Finished jobs are applied first.")]
    async fn jobs_list(&self) -> Result<CallToolResult, McpError> {
        self.jobs_call("jobs.list", json!({})).await
    }

    #[tool(description = "Cancel a background job by id (or every running job when `job` is omitted). The document is left \
        unchanged.")]
    async fn jobs_cancel(&self, Parameters(p): Parameters<JobCancelParams>) -> Result<CallToolResult, McpError> {
        self.jobs_call("jobs.cancel", p.job.map_or_else(|| json!({}), |j| json!({"job": j}))).await
    }

    #[tool(description = "Run several engine commands in one call (fewer round trips). Returns {completed, failed, \
        results:[{ok, result|error}]}; stops at the first error unless stop_on_error is false.")]
    async fn command_batch(&self, Parameters(p): Parameters<BatchParams>) -> Result<CallToolResult, McpError> {
        if p.steps.len() > MAX_BATCH_STEPS {
            return Ok(fail(format!("batch contains {} steps; maximum is {MAX_BATCH_STEPS}", p.steps.len())));
        }
        let stop = p.stop_on_error.unwrap_or(true);
        let steps: Vec<Value> = p.steps.into_iter().map(|s| json!({"command": s.id, "params": s.params.unwrap_or_else(|| json!({}))})).collect();
        let args = json!({"steps": steps, "stopOnError": stop});
        if let Some(r) = self.headless_op(move |h| h.batch(&args)).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        let mut results = Vec::new();
        let mut failed = 0;
        let mut reply_budget = BatchReplyBudget::default();
        for s in &steps {
            let response = b.call("engine.execute", s.clone()).await;
            let was_error = response.is_err();
            let result = match response {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({"ok": false, "error": error.to_string()}),
            };
            if let Err(error) = reply_budget.charge(&result) {
                failed += 1;
                results.push(json!({"ok": false, "error": format!("batch response budget exceeded; current step may have completed: {error}")}));
                break;
            }
            results.push(result);
            if was_error {
                failed += 1;
                if stop {
                    break;
                }
            }
        }
        Ok(ok_json(&json!({"completed": results.len() - failed, "failed": failed, "results": results})))
    }

    // ----- live-GUI tools (bridge mode) -----

    #[tool(description = "Bridge mode: full UI state of the live app (tool, panels, views, dialogs, menu tree).")]
    async fn ui_inspect(&self) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => to_result(b.call("ui.inspect", json!({})).await),
            None => bridge_only("ui_inspect"),
        }
    }

    #[tool(description = "Bridge mode: screenshot of the live app window as PNG.")]
    async fn ui_screenshot(&self, Parameters(p): Parameters<PreviewParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => self.screenshot(b, p.max_side).await,
            None => bridge_only("ui_screenshot"),
        }
    }

    #[tool(description = "Bridge mode: send pointer events (document coordinates) to the active tool.")]
    async fn ui_pointer(&self, Parameters(p): Parameters<PointerParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => {
                let mut params = json!({"events": p.events});
                if let Some(m) = p.modifiers {
                    params["modifiers"] = m;
                }
                to_result(b.call("ui.pointer", params).await)
            }
            None => bridge_only("ui_pointer"),
        }
    }

    #[tool(description = "Bridge mode: activate a menu item by id.")]
    async fn ui_menu_invoke(&self, Parameters(p): Parameters<MenuParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => to_result(b.call("ui.menu.invoke", json!({"id": p.id})).await),
            None => bridge_only("ui_menu_invoke"),
        }
    }

    #[tool(description = "Bridge mode: change UI state (tool, panels, zoom, center, dark).")]
    async fn ui_set(&self, Parameters(p): Parameters<UiSetParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => to_result(b.call("ui.set", p.fields).await),
            None => bridge_only("ui_set"),
        }
    }

    #[tool(description = "Bridge mode: call any control-protocol method (docs/control-protocol.md) with raw params.")]
    async fn control_call(&self, Parameters(p): Parameters<ControlParams>) -> Result<CallToolResult, McpError> {
        match self.bridge_client() {
            Some(b) => to_result(b.call(&p.method, p.params.unwrap_or_else(|| json!({}))).await),
            None => bridge_only("control_call"),
        }
    }
}

fn bridge_only_headless(name: &str) -> Result<CallToolResult, McpError> {
    Ok(fail(format!("`{name}` is only available in headless mode (in the GUI use the window/tab UI or control_call)")))
}

impl PhotocraftMcp {
    async fn run_command(&self, id: String, params: Value, wait: bool) -> Result<CallToolResult, McpError> {
        let (id2, params2) = (id.clone(), params.clone());
        if let Some(r) = self.headless_op(move |h| h.command_start(&id2, params2, wait)).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        to_result(b.call("engine.execute", json!({"command": id, "params": params, "wait": wait})).await)
    }

    /// `jobs.list` / `jobs.cancel` in either backend.
    async fn jobs_call(&self, method: &'static str, params: Value) -> Result<CallToolResult, McpError> {
        let p2 = params.clone();
        if let Some(r) = self.headless_op(move |h| h.command_start(method, p2, true)).await {
            return to_result(r);
        }
        let Some(b) = self.bridge_client() else {
            return Ok(no_backend());
        };
        to_result(b.call(method, params).await)
    }

    async fn save_impl(&self, p: SaveParams) -> Result<CallToolResult, McpError> {
        if let Some(b) = self.bridge_client() {
            let Some(path) = p.path else {
                return Ok(fail("bridge mode needs `path`"));
            };
            return to_result(b.call("app.save", json!({"path": path})).await);
        }
        let Some(r) = self
            .headless_op(move |h| {
                let mut opts = photocraft_io::ExportOptions::default();
                if let Some(q) = p.quality {
                    opts.encode.jpeg_quality = q.clamp(1, 100);
                }
                let path = p.path.map(PathBuf::from);
                h.save(p.index, path.as_deref(), p.format.as_deref(), &opts)
            })
            .await
        else {
            return Ok(no_backend());
        };
        to_result(r)
    }
}

#[tool_handler(router = self.tool_router, name = "photocraft", instructions = "Photocraft image editor. Every edit is an engine command: call `command_list` to discover ids and parameter docs, then `command_run` (or `command_batch` for several at once). Use `doc_open`/`doc_new` first, `doc_inspect` for the layer tree, `doc_render_preview` to see the result, and `doc_save` (.pcraft is lossless native; .psd/.png/.jpg/.tif… export). In bridge mode the `ui_*` tools drive the live app (inspect, screenshot, pointer, menus).")]
impl ServerHandler for PhotocraftMcp {}

/// Used by the render helper in tests and the CLI.
pub fn render_document_png(doc: &photocraft_doc::Document, max_side: u32) -> Result<Vec<u8>, AutomationError> {
    files::render_png(doc, max_side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_text_checks_the_encoded_tool_envelope() {
        // JSON escaping in the outer text-content envelope also consumes the budget.
        let result = ok_json(&json!("\n".repeat(crate::budgets::MAX_RESPONSE_BYTES / 3)));
        assert_eq!(result.is_error, Some(true));
        assert!(json_bytes(&result).is_ok());
        let error = fail("x".repeat(crate::budgets::MAX_RESPONSE_BYTES));
        assert_eq!(error.is_error, Some(true));
        assert!(json_bytes(&error).is_ok());
    }

    #[test]
    fn bridge_screenshot_decode_limits_apply_even_without_downscaling() {
        let image = photocraft_codecs::Image::from_u8(8193, 1, photocraft_codecs::ChannelLayout::Rgba, vec![0; 8193 * 4]).unwrap();
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        assert!(downscale_png(&png, 0).is_err());
        assert!(downscale_png(&png, 1024).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_command_does_not_wedge_the_session() {
        let mcp = PhotocraftMcp::headless();
        let r = mcp.headless_op(|_| -> Result<(), AutomationError> { panic!("boom") }).await.unwrap();
        assert!(r.is_err());
        let r = mcp.headless_op(|h| h.command_run("file.new", json!({"width": 8, "height": 8}))).await.unwrap();
        assert!(r.is_ok(), "session still usable after a panic: {r:?}");
    }
}

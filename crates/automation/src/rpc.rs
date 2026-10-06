//! Headless JSON-lines server: the control protocol's envelope
//! (`{"id","method","params"}` → `{"id","ok","result"|"error"}`) over a
//! [`Headless`] session, so scripts and agents can keep one editing session
//! open without MCP or the GUI. `photocraft-cli serve` runs it on stdio or a
//! loopback TCP port.
//!
//! Methods (camelCase params):
//! - `engine.execute {command, params?}` / `engine.commands {filter?}`
//! - `session.list`, `doc.open {path}`, `doc.new {…file.new params}`,
//!   `doc.save {path?, format?, quality?, index?}`, `doc.inspect {index?}`,
//!   `doc.render {index?, maxSide?, path?}` (writes a PNG to `path`, else
//!   returns it base64-encoded), `doc.select {index}`, `doc.close {index?}`
//! - `batch {steps: [{command, params?} | {method, params?}], stopOnError?}`
//! - `methods`: this list.

use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::budgets::{BatchReplyBudget, write_reply};
use crate::security::{
    ConnectionLimiter, LineRead, MAX_BATCH_STEPS, MAX_CONNECTIONS, MAX_REQUEST_BYTES, authentication_reply, configure_stream, read_bounded_line,
};
use crate::{AutomationError, Headless};

/// Method names served by [`Headless::handle`].
pub const METHODS: &[&str] = &[
    "engine.execute",
    "engine.commands",
    "session.list",
    "doc.open",
    "doc.new",
    "doc.save",
    "doc.inspect",
    "doc.render",
    "doc.select",
    "doc.close",
    "batch",
    "methods",
];

fn bad(msg: impl Into<String>) -> AutomationError {
    AutomationError::BadRequest(msg.into())
}

fn index_of(p: &Value) -> Option<usize> {
    p.get("index").and_then(Value::as_u64).map(|i| i as usize)
}

fn str_of<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

impl Headless {
    /// Dispatch one request. Unknown methods and bad params are errors, never panics.
    pub fn handle(&mut self, method: &str, params: Value) -> Result<Value, AutomationError> {
        let p = if params.is_null() { json!({}) } else { params };
        match method {
            "engine.execute" => {
                let id = str_of(&p, "command").ok_or_else(|| bad("engine.execute needs `command`"))?;
                let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(true);
                self.command_start(id, p.get("params").cloned().unwrap_or(Value::Null), wait)
            }
            // Background jobs (#210): list (applying any that finished) and cancel.
            "jobs.list" => self.command_start("jobs.list", json!({}), true),
            "jobs.cancel" => self.command_start("jobs.cancel", p, true),
            "engine.commands" => {
                let all = self.command_list();
                Ok(match str_of(&p, "filter").map(str::to_lowercase) {
                    None => all,
                    Some(n) => Value::Array(
                        all.as_array()
                            .into_iter()
                            .flatten()
                            .filter(|c| {
                                let hay = format!("{} {}", c["id"].as_str().unwrap_or(""), c["label"].as_str().unwrap_or(""));
                                hay.to_lowercase().contains(&n)
                            })
                            .cloned()
                            .collect(),
                    ),
                })
            }
            "session.list" => Ok(self.session_list()),
            "doc.open" => {
                let path = str_of(&p, "path").ok_or_else(|| bad("doc.open needs `path`"))?;
                self.open(&PathBuf::from(path))
            }
            "doc.new" => self.command_run("file.new", p),
            "doc.save" => {
                let mut opts = photocraft_io::ExportOptions::default();
                if let Some(q) = p.get("quality").and_then(Value::as_u64) {
                    opts.encode.jpeg_quality = q.clamp(1, 100) as u8;
                }
                let path = str_of(&p, "path").map(PathBuf::from);
                self.save(index_of(&p), path.as_deref(), str_of(&p, "format"), &opts)
            }
            "doc.inspect" => self.inspect(index_of(&p)),
            "doc.render" => {
                let max = match p.get("maxSide") {
                    None => 1024,
                    Some(value) => {
                        value.as_u64().and_then(|value| u32::try_from(value).ok()).ok_or_else(|| bad("maxSide must be an unsigned 32-bit integer"))?
                    }
                };
                let png = self.render_png(index_of(&p), max)?;
                match str_of(&p, "path") {
                    Some(path) => {
                        self.write_render(&PathBuf::from(path), &png)?;
                        Ok(json!({"path": path, "bytes": png.len()}))
                    }
                    None => Ok(json!({
                        "mime": "image/png",
                        "base64": base64::engine::general_purpose::STANDARD.encode(&png),
                    })),
                }
            }
            "doc.select" => {
                let i = index_of(&p).ok_or_else(|| bad("doc.select needs `index`"))?;
                self.select(i)
            }
            "doc.close" => self.close(index_of(&p)),
            "batch" => self.batch(&p),
            "methods" => Ok(json!(METHODS)),
            other => Err(bad(format!("unknown method `{other}` (try `methods`)"))),
        }
    }

    /// Run `steps` in order. Each step is `{command, params?}` (an engine command) or
    /// `{method, params?}` (any [`METHODS`] entry). Stops at the first error unless
    /// `stopOnError` is false; the reply lists every step's result.
    pub fn batch(&mut self, p: &Value) -> Result<Value, AutomationError> {
        let steps = p.get("steps").and_then(Value::as_array).ok_or_else(|| bad("batch needs `steps`"))?;
        if steps.len() > MAX_BATCH_STEPS {
            return Err(bad(format!("batch contains {} steps; maximum is {MAX_BATCH_STEPS}", steps.len())));
        }
        let stop = p.get("stopOnError").and_then(Value::as_bool).unwrap_or(true);
        let mut results = Vec::with_capacity(steps.len());
        let mut failed = 0usize;
        let mut reply_budget = BatchReplyBudget::default();
        for (i, s) in steps.iter().enumerate() {
            let params = s.get("params").cloned().unwrap_or(Value::Null);
            let r = if let Some(c) = str_of(s, "command") {
                self.command_run(c, params)
            } else if let Some(m) = str_of(s, "method") {
                if m == "batch" { Err(bad("nested batch")) } else { self.handle(m, params) }
            } else {
                Err(bad(format!("step {i} needs `command` or `method`")))
            };
            let was_error = r.is_err();
            let result = match r {
                Ok(v) => json!({"ok": true, "result": v}),
                Err(e) => json!({"ok": false, "error": e.to_string()}),
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
        Ok(json!({"completed": results.len() - failed, "failed": failed, "results": results}))
    }
}

/// Answer one request line. Malformed JSON gets an error reply with `id: null`.
pub fn respond(h: &Mutex<Headless>, line: &str) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"id": null, "ok": false, "error": format!("bad JSON: {e}")}),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return json!({"id": id, "ok": false, "error": "missing `method`"});
    };
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    // Last-resort guard (AGENTS.md, Never crash): a command that panics answers this request
    // with an error instead of taking down the connection (or the whole stdio server). Edits
    // run on a copy of the document (`Session::edit`), so the session stays consistent and a
    // poisoned lock is safe to keep using, the same as the MCP server does.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.lock().unwrap_or_else(PoisonError::into_inner).handle(method, params)))
        .unwrap_or_else(|_| Err(AutomationError::Other(format!("internal error: `{method}` panicked"))));
    match r {
        Ok(v) => json!({"id": id, "ok": true, "result": v}),
        Err(e) => json!({"id": id, "ok": false, "error": e.to_string()}),
    }
}

/// Serve JSON lines from `r` to `w` until EOF. Blank lines are ignored.
pub fn serve_lines(h: &Mutex<Headless>, mut r: impl BufRead, mut w: impl Write) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        match read_bounded_line(&mut r, &mut line)? {
            LineRead::Eof => return Ok(()),
            LineRead::TooLong => {
                write_reply(&mut w, &json!({"id": null, "ok": false, "error": format!("request exceeds {MAX_REQUEST_BYTES} bytes")}))?;
                w.flush()?;
                return Ok(());
            }
            LineRead::Line => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let reply = respond(h, &line);
        write_reply(&mut w, &reply)?;
        w.flush()?;
    }
}

fn serve_tcp_connection(h: &Mutex<Headless>, stream: std::net::TcpStream, token: &str) -> std::io::Result<()> {
    configure_stream(&stream)?;
    let read = stream.try_clone()?;
    let mut reader = std::io::BufReader::new(read);
    let mut out = stream;
    let mut line = String::new();
    let mut authenticated = false;
    loop {
        match read_bounded_line(&mut reader, &mut line)? {
            LineRead::Eof => return Ok(()),
            LineRead::TooLong => {
                let reply = json!({
                    "id": null,
                    "ok": false,
                    "error": format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
                });
                write_reply(&mut out, &reply)?;
                out.flush()?;
                return Ok(());
            }
            LineRead::Line if line.trim().is_empty() => continue,
            LineRead::Line => {}
        }
        let reply = if authenticated {
            respond(h, &line)
        } else {
            let (reply, ok) = authentication_reply(&line, token);
            authenticated = ok;
            reply
        };
        write_reply(&mut out, &reply)?;
        out.flush()?;
        if !authenticated {
            return Ok(());
        }
    }
}

/// Serve authenticated JSON lines on a loopback TCP address with one shared session.
/// Refuses non-loopback addresses and caps active connections and request bytes.
pub fn serve_tcp(addr: &str, h: Arc<Mutex<Headless>>, token: String, ready: impl FnOnce(std::net::SocketAddr)) -> Result<(), AutomationError> {
    let listener = TcpListener::bind(addr).map_err(|e| AutomationError::Io(format!("bind {addr}: {e}")))?;
    let local = listener.local_addr().map_err(|e| AutomationError::Io(e.to_string()))?;
    if !local.ip().is_loopback() {
        return Err(bad(format!("{addr} is not a loopback address")));
    }
    ready(local);
    let limiter = ConnectionLimiter::new(MAX_CONNECTIONS);
    let token = Arc::new(token);
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let Some(permit) = limiter.try_acquire() else {
            let _ = configure_stream(&stream);
            let _ = writeln!(stream, "{}", json!({"id": null, "ok": false, "error": "connection limit reached"}));
            continue;
        };
        let h = h.clone();
        let token = Arc::clone(&token);
        std::thread::spawn(move || {
            let _permit = permit;
            let _ = serve_tcp_connection(&h, stream, &token);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Mutex<Headless> {
        Mutex::new(Headless::trusted_local())
    }

    #[test]
    fn lines_round_trip_and_errors() {
        let h = session();
        let input = concat!(
            r#"{"id":1,"method":"doc.new","params":{"width":64,"height":32,"background":"white"}}"#,
            "\n",
            "\n",
            r#"{"id":2,"method":"engine.execute","params":{"command":"layer.new.layer","params":{"name":"Ink"}}}"#,
            "\n",
            r#"{"id":3,"method":"doc.inspect"}"#,
            "\n",
            r#"{"id":4,"method":"nope"}"#,
            "\n",
            "not json\n",
        );
        let mut out = Vec::new();
        serve_lines(&h, input.as_bytes(), &mut out).unwrap();
        let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 5);
        assert!(replies[0]["ok"].as_bool().unwrap());
        assert_eq!(replies[1]["id"], 2);
        assert!(replies[2]["result"].to_string().contains("Ink"));
        assert!(!replies[3]["ok"].as_bool().unwrap());
        assert!(replies[3]["error"].as_str().unwrap().contains("unknown method"));
        assert_eq!(replies[4]["id"], Value::Null);
    }

    /// Regression: after a panic while the session lock was held, every later request
    /// failed with "session lock poisoned"; the server now keeps serving.
    #[test]
    fn poisoned_session_keeps_serving() {
        let h = session();
        let _ = std::thread::scope(|sc| {
            sc.spawn(|| {
                let _g = h.lock().unwrap();
                panic!("poison the session lock");
            })
            .join()
        });
        assert!(h.is_poisoned());
        let r = respond(&h, r#"{"id":1,"method":"doc.new","params":{"width":8,"height":8}}"#);
        assert_eq!(r["ok"], true, "{r}");
    }

    #[test]
    fn batch_stops_on_error_unless_told_not_to() {
        let h = session();
        let mut g = h.lock().unwrap();
        g.handle("doc.new", json!({"width": 16, "height": 16})).unwrap();
        let steps = json!([
            {"command": "layer.new.layer", "params": {"name": "A"}},
            {"command": "no.such.command"},
            {"command": "layer.new.layer", "params": {"name": "B"}},
            {"method": "doc.inspect"}
        ]);
        let r = g.handle("batch", json!({"steps": steps})).unwrap();
        assert_eq!(r["completed"], 1);
        assert_eq!(r["failed"], 1);
        assert_eq!(r["results"].as_array().unwrap().len(), 2);
        let r = g.handle("batch", json!({"steps": steps, "stopOnError": false})).unwrap();
        assert_eq!(r["completed"], 3);
        let insp = &r["results"][3]["result"];
        assert!(insp.to_string().contains("\"B\""));
    }

    #[test]
    fn batch_rejects_too_many_steps() {
        let h = session();
        let mut g = h.lock().unwrap();
        let steps = vec![json!({"command": "command.list"}); MAX_BATCH_STEPS + 1];
        let error = g.handle("batch", json!({"steps": steps})).unwrap_err();
        assert!(error.to_string().contains("maximum is 256"));
    }

    #[test]
    fn render_to_file_and_base64() {
        let h = session();
        let mut g = h.lock().unwrap();
        g.handle("doc.new", json!({"width": 40, "height": 20, "background": "black"})).unwrap();
        let b = g.handle("doc.render", json!({"maxSide": 10})).unwrap();
        let png = base64::engine::general_purpose::STANDARD.decode(b["base64"].as_str().unwrap()).unwrap();
        let img = photocraft_codecs::decode(&png).unwrap();
        assert_eq!(img.dimensions(), (10, 5));
        let path = std::env::temp_dir().join(format!("pc-rpc-{}.png", std::process::id()));
        let r = g.handle("doc.render", json!({"path": path.to_string_lossy(), "maxSide": 0})).unwrap();
        assert!(r["bytes"].as_u64().unwrap() > 0);
        assert_eq!(photocraft_codecs::decode(&std::fs::read(&path).unwrap()).unwrap().dimensions(), (40, 20));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn background_jobs_list_wait_and_cancel() {
        let mut h = Headless::new();
        h.handle("doc.new", json!({"width": 600, "height": 400})).unwrap();
        h.handle("engine.execute", json!({"command": "layer.new.layer"})).unwrap();
        h.handle("engine.execute", json!({"command": "edit.fill", "params": {"color": "#808080"}})).unwrap();
        // Default: waits, the result is the command's.
        let r = h.handle("engine.execute", json!({"command": "filter.noise.addNoise", "params": {"amount": 20}})).unwrap();
        assert!(r["filter"].is_object(), "{r}");
        // wait: false returns a job id at once.
        let r = h.handle("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 40}, "wait": false})).unwrap();
        let job = r["job"].as_u64().unwrap();
        let listed = h.handle("jobs.list", json!({})).unwrap();
        assert!(listed["jobs"].as_array().unwrap().iter().any(|j| j["id"] == job), "{listed}");
        // Wait for it via jobs.list (which applies finished jobs).
        let t = std::time::Instant::now();
        loop {
            let l = h.handle("jobs.list", json!({})).unwrap();
            let state = l["jobs"].as_array().unwrap().iter().find(|j| j["id"] == job).map(|j| j["state"].clone());
            if state == Some(json!("done")) {
                break;
            }
            assert!(t.elapsed().as_secs() < 60, "{l}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Cancel: a second job, cancelled at once, leaves the document unchanged.
        let before = h.session.active().unwrap().doc.clone();
        let r = h.handle("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 40}, "wait": false})).unwrap();
        let job = r["job"].as_u64().unwrap();
        h.handle("jobs.cancel", json!({"job": job})).unwrap();
        assert!(h.handle("jobs.cancel", json!({"job": "nope"})).is_err());
        let l = h.handle("jobs.list", json!({})).unwrap();
        assert_eq!(l["jobs"].as_array().unwrap().iter().find(|j| j["id"] == job).unwrap()["state"], "cancelled");
        assert!(std::sync::Arc::ptr_eq(&h.session.active().unwrap().doc, &before));
    }

    #[test]
    fn automation_preview_rejects_oversized_full_size_and_numeric_wraparound() {
        let mut h = Headless::new();
        h.handle("doc.new", json!({"width": 2049, "height": 1})).unwrap();
        for max in [json!(0), json!(2049), json!(u64::from(u32::MAX) + 1), json!(-1), json!(1.5)] {
            assert!(h.handle("doc.render", json!({"maxSide": max})).is_err());
        }
        // A smaller preview still works, and rejection leaves the source document intact.
        let rendered = h.handle("doc.render", json!({"maxSide": 32})).unwrap();
        let png = base64::engine::general_purpose::STANDARD.decode(rendered["base64"].as_str().unwrap()).unwrap();
        assert_eq!(photocraft_codecs::decode(&png).unwrap().dimensions(), (32, 1));
        assert_eq!(h.session.active().unwrap().doc.size.width, 2049);
    }

    #[test]
    fn stdio_rejects_oversized_frames_before_dispatch() {
        let h = Mutex::new(Headless::new());
        let input = " ".repeat(MAX_REQUEST_BYTES + 1);
        let mut out = Vec::new();
        serve_lines(&h, input.as_bytes(), &mut out).unwrap();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("request exceeds"));
        assert!(h.lock().unwrap().session.documents().is_empty());
    }

    #[test]
    fn preview_rejection_precedes_output_file_creation() {
        let root = std::env::temp_dir().join(format!("pc-preview-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::AuthorizedWorkspace::new(None, Some(&root)).unwrap();
        let mut h = Headless::with_workspace(workspace);
        h.handle("doc.new", json!({"width": 16, "height": 8})).unwrap();
        let result = h.handle("doc.render", json!({"maxSide": 2049, "path": "rejected.png"}));
        assert!(result.unwrap_err().to_string().contains("preview side exceeds"));
        assert!(!root.join("rejected.png").exists());
        assert!(h.handle("doc.render", json!({"maxSide": 16, "path": "accepted.png"})).is_ok());
        assert!(root.join("accepted.png").exists());
        // Release directory capabilities before cleanup on Windows.
        drop(h);
        std::fs::remove_file(root.join("accepted.png")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn trusted_local_render_keeps_full_size_behavior() {
        let mut h = Headless::trusted_local();
        h.handle("doc.new", json!({"width": 2049, "height": 1})).unwrap();
        let rendered = h.handle("doc.render", json!({"maxSide": 0})).unwrap();
        let png = base64::engine::general_purpose::STANDARD.decode(rendered["base64"].as_str().unwrap()).unwrap();
        assert_eq!(photocraft_codecs::decode(&png).unwrap().dimensions(), (2049, 1));
    }

    #[test]
    fn batch_response_budget_stops_later_steps_even_when_stop_on_error_is_false() {
        let mut h = Headless::new();
        let mut steps = vec![json!({"command": "command.list"}); MAX_BATCH_STEPS - 1];
        steps.push(json!({"command": "file.new", "params": {"width": 8, "height": 8}}));
        let result = h.batch(&json!({"steps": steps, "stopOnError": false})).unwrap();
        assert_eq!(result["failed"], 1);
        assert!(result["results"].as_array().unwrap().last().unwrap()["error"].as_str().unwrap().contains("batch response budget exceeded"));
        assert!(h.session.documents().is_empty(), "later edit must not execute after the quota is exhausted");
        assert!(crate::budgets::json_bytes(&result).is_ok());
        assert!(h.handle("methods", Value::Null).is_ok());
    }

    #[test]
    fn tcp_serves_loopback() {
        use std::io::{BufReader, Write as _};
        const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let h = Arc::new(Mutex::new(Headless::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = serve_tcp("127.0.0.1:0", h, TOKEN.into(), move |a| tx.send(a).unwrap());
        });
        let addr = rx.recv().unwrap();
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        writeln!(s, r#"{{"id":"auth","method":"auth","params":{{"token":"{TOKEN}"}}}}"#).unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let auth: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(auth["result"]["authenticated"], true);
        writeln!(s, r#"{{"id":"a","method":"methods"}}"#).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "a");
        assert!(v["result"].as_array().unwrap().iter().any(|m| m == "batch"));
    }

    #[test]
    fn tcp_rejects_requests_before_authentication() {
        use std::io::{BufReader, Write as _};
        const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let h = Arc::new(Mutex::new(Headless::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = serve_tcp("127.0.0.1:0", h, TOKEN.into(), move |a| tx.send(a).unwrap());
        });
        let addr = rx.recv().unwrap();
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        writeln!(s, r#"{{"id":1,"method":"methods"}}"#).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "authentication required");
        assert!(v.get("result").is_none());
    }
}

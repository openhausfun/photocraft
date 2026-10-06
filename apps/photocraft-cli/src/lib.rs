//! Headless Photocraft command line. `run` is the whole program, so it can be
//! tested in-process as well as through the binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::Write;
use std::path::{Path, PathBuf};

use photocraft_automation::{AuthorizedWorkspace, Headless, PhotocraftMcp, files, security};
use photocraft_io::ExportOptions;
use serde_json::{Value, json};

pub const USAGE: &str = "\
photocraft-cli: headless Photocraft

USAGE:
  photocraft-cli convert <in> <out> [--format <ext>] [--quality <1-100>]
      Convert between formats (.pcraft, .psd, .png, .jpg, .tif, .webp, .exr, …).
  photocraft-cli info <file> [--compact]
      Print the document as JSON (size, mode, depth, layer tree).
  photocraft-cli run (<file> | --new <json>) --cmd <id> [--params <json>] [--cmd …] [--out <file>] [--format <ext>]
      Open a file, run engine commands in order, save the result. Each --params
      applies to the preceding --cmd. Prints each command's JSON result.
  photocraft-cli batch --actions <actions.json> --in <dir> --out <dir> [--format <ext>]
      Apply an action list ([{\"command\": id, \"params\": {…}}, …]) to every image in a directory.
  photocraft-cli droplet <file.pcdroplet> <file-or-dir>… [--out <dir>]
      Run a droplet (File › Automate › Create Droplet) on images and folders.
  photocraft-cli commands [--json] [--filter <text>]
      List the engine command registry.
  photocraft-cli mcp [--bridge <127.0.0.1:port>] [--control-token <64-hex> | --control-token-file <path>]
      [--automation-read-root <dir>] [--automation-write-root <dir>]
      Run the MCP server on stdio (headless engine, or bridge to a running `photocraft --control <port>`).
  photocraft-cli serve [--port <port>] [--control-token <64-hex> | --control-token-file <path>]
      [--automation-read-root <dir>] [--automation-write-root <dir>]
      Keep one headless session open and answer JSON lines ({\"id\",\"method\",\"params\"}) on stdio,
      or on 127.0.0.1:<port>. Methods: engine.execute, engine.commands, doc.open/new/save/inspect/render/
      select/close, session.list, batch, methods (docs/control-protocol.md#headless-server).
";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const VALUE_FLAGS: &[&str] = &[
    "--format",
    "--quality",
    "--new",
    "--cmd",
    "--params",
    "--out",
    "--actions",
    "--in",
    "--filter",
    "--bridge",
    "--port",
    "--control-token",
    "--control-token-file",
    "--automation-read-root",
    "--automation-write-root",
];

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { positional: Vec::new(), flags: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let s = &args[i];
        if let Some((k, v)) = s.split_once('=').filter(|(k, _)| k.starts_with("--")) {
            a.flags.push((k.to_owned(), Some(v.to_owned())));
        } else if VALUE_FLAGS.contains(&s.as_str()) {
            let v = args.get(i + 1).ok_or_else(|| format!("{s} needs a value"))?;
            a.flags.push((s.clone(), Some(v.clone())));
            i += 1;
        } else if s.starts_with("--") {
            a.flags.push((s.clone(), None));
        } else {
            a.positional.push(s.clone());
        }
        i += 1;
    }
    Ok(a)
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| f == k).and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }
}

type R = Result<(), String>;

/// Run the CLI. Returns the process exit code (0 ok, 1 failure, 2 usage).
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some(cmd) = args.first() else {
        let _ = write!(err, "{USAGE}");
        return 2;
    };
    let parsed = match parse(&args[1..]) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "error: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let r = match cmd.as_str() {
        "convert" => convert(&parsed, out, err),
        "info" => info(&parsed, out),
        "run" => run_cmds(&parsed, out, err),
        "batch" => batch(&parsed, out, err),
        "commands" => commands(&parsed, out),
        "droplet" => droplet(&parsed, out, err),
        "mcp" => mcp(&parsed),
        "serve" => serve(&parsed, err),
        "-h" | "--help" | "help" => {
            let _ = write!(out, "{USAGE}");
            return 0;
        }
        "--version" | "version" => {
            let _ = writeln!(out, "photocraft-cli {}", photocraft_engine::build_info::long_version());
            return 0;
        }
        other => {
            let _ = writeln!(err, "error: unknown command `{other}`\n\n{USAGE}");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}

fn export_opts(a: &Args) -> Result<ExportOptions, String> {
    let mut o = ExportOptions::default();
    if let Some(q) = a.get("--quality") {
        let q: u8 = q.parse().map_err(|_| format!("bad --quality `{q}`"))?;
        o.encode.jpeg_quality = q.clamp(1, 100);
    }
    Ok(o)
}

fn warn_all(err: &mut dyn Write, ws: &[String]) {
    for w in ws {
        let _ = writeln!(err, "warning: {w}");
    }
}

fn automation_workspace(args: &Args) -> Result<AuthorizedWorkspace, String> {
    AuthorizedWorkspace::new(args.get("--automation-read-root").map(Path::new), args.get("--automation-write-root").map(Path::new))
        .map_err(|error| error.to_string())
}

fn convert(a: &Args, _out: &mut dyn Write, err: &mut dyn Write) -> R {
    let [input, output] = a.positional.as_slice() else {
        return Err("convert needs <in> <out>".into());
    };
    let o = files::open(Path::new(input)).map_err(|e| e.to_string())?;
    warn_all(err, &o.warnings);
    let ws = files::save(&o.document, Path::new(output), a.get("--format"), &export_opts(a)?, None).map_err(|e| e.to_string())?;
    warn_all(err, &ws);
    Ok(())
}

fn print_json(out: &mut dyn Write, v: &Value, compact: bool) -> R {
    let s = if compact { serde_json::to_string(v) } else { serde_json::to_string_pretty(v) }.map_err(|e| e.to_string())?;
    writeln!(out, "{s}").map_err(|e| e.to_string())
}

fn info(a: &Args, out: &mut dyn Write) -> R {
    let [file] = a.positional.as_slice() else {
        return Err("info needs <file>".into());
    };
    let mut h = Headless::trusted_local();
    let opened = h.open(Path::new(file)).map_err(|e| e.to_string())?;
    let mut doc = h.inspect(None).map_err(|e| e.to_string())?;
    doc["warnings"] = opened["warnings"].clone();
    doc["file"] = json!(file);
    // Drop per-session noise.
    if let Value::Object(m) = &mut doc {
        for k in ["history", "canUndo", "canRedo", "revision"] {
            m.remove(k);
        }
    }
    print_json(out, &doc, a.has("--compact"))
}

/// `(id, params)` pairs from repeated `--cmd`/`--params` flags.
fn command_list(a: &Args) -> Result<Vec<(String, Value)>, String> {
    let mut cmds: Vec<(String, Value)> = Vec::new();
    for (k, v) in &a.flags {
        match (k.as_str(), v) {
            ("--cmd", Some(id)) => cmds.push((id.clone(), json!({}))),
            ("--params", Some(p)) => {
                let last = cmds.last_mut().ok_or("--params must follow a --cmd")?;
                last.1 = serde_json::from_str(p).map_err(|e| format!("bad --params JSON for `{}`: {e}", last.0))?;
            }
            _ => {}
        }
    }
    Ok(cmds)
}

fn run_cmds(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let mut h = Headless::trusted_local();
    match (a.positional.as_slice(), a.get("--new")) {
        ([file], None) => {
            let o = h.open(Path::new(file)).map_err(|e| e.to_string())?;
            let ws: Vec<String> = serde_json::from_value(o["warnings"].clone()).unwrap_or_default();
            warn_all(err, &ws);
        }
        ([], Some(new)) => {
            let p: Value = serde_json::from_str(new).map_err(|e| format!("bad --new JSON: {e}"))?;
            h.command_run("file.new", p).map_err(|e| e.to_string())?;
        }
        _ => return Err("run needs exactly one of <file> or --new <json>".into()),
    }
    let cmds = command_list(a)?;
    if cmds.is_empty() && a.get("--out").is_none() {
        return Err("run needs at least one --cmd (or --out)".into());
    }
    for (id, params) in cmds {
        let r = h.command_run(&id, params).map_err(|e| format!("`{id}`: {e}"))?;
        print_json(out, &json!({"command": id, "result": r}), true)?;
    }
    if let Some(o) = a.get("--out") {
        let r = h.save(None, Some(Path::new(o)), a.get("--format"), &export_opts(a)?).map_err(|e| e.to_string())?;
        let ws: Vec<String> = serde_json::from_value(r["warnings"].clone()).unwrap_or_default();
        warn_all(err, &ws);
    }
    Ok(())
}

/// Parse an actions file: `[{"command": id, "params": {…}}]` or
/// `{"actions": [...]}`; `"id"` is accepted for `"command"`.
pub fn parse_actions(text: &str) -> Result<Vec<(String, Value)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("actions JSON: {e}"))?;
    let list = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(m) => m.get("actions").and_then(Value::as_array).cloned().ok_or("actions JSON: expected an array or {\"actions\": [...]}")?,
        _ => return Err("actions JSON: expected an array".into()),
    };
    list.into_iter()
        .enumerate()
        .map(|(i, a)| {
            let id = a.get("command").or_else(|| a.get("id")).and_then(Value::as_str).ok_or_else(|| format!("action {i}: missing \"command\""))?;
            Ok((id.to_owned(), a.get("params").cloned().unwrap_or_else(|| json!({}))))
        })
        .collect()
}

fn is_input(p: &Path) -> bool {
    let known = |e: &str| matches!(e, "pcraft" | "psd" | "psb") || photocraft_codecs::from_extension(e).is_some_and(|f| photocraft_codecs::caps(f).read);
    p.is_file() && p.extension().is_some_and(|e| known(&e.to_string_lossy().to_ascii_lowercase()))
}

fn batch(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let actions_path = a.get("--actions").ok_or("batch needs --actions <file>")?;
    let in_dir = PathBuf::from(a.get("--in").ok_or("batch needs --in <dir>")?);
    let out_dir = PathBuf::from(a.get("--out").ok_or("batch needs --out <dir>")?);
    let text = std::fs::read_to_string(actions_path).map_err(|e| format!("{actions_path}: {e}"))?;
    let actions = parse_actions(&text)?;
    let opts = export_opts(a)?;
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut inputs: Vec<PathBuf> =
        std::fs::read_dir(&in_dir).map_err(|e| format!("{}: {e}", in_dir.display()))?.flatten().map(|e| e.path()).filter(|p| is_input(p)).collect();
    inputs.sort();
    let (mut ok, mut failed) = (0, 0);
    for input in &inputs {
        let ext = a.get("--format").map(str::to_owned).or_else(|| input.extension().map(|e| e.to_string_lossy().into_owned())).unwrap_or_else(|| "png".into());
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let target = out_dir.join(format!("{stem}.{ext}"));
        let r = (|| -> Result<Vec<String>, String> {
            let mut h = Headless::trusted_local();
            h.open(input).map_err(|e| e.to_string())?;
            for (id, p) in &actions {
                h.command_run(id, p.clone()).map_err(|e| format!("`{id}`: {e}"))?;
            }
            let r = h.save(None, Some(&target), Some(&ext), &opts).map_err(|e| e.to_string())?;
            Ok(serde_json::from_value(r["warnings"].clone()).unwrap_or_default())
        })();
        match r {
            Ok(ws) => {
                ok += 1;
                let _ = writeln!(out, "ok    {} -> {}", input.display(), target.display());
                warn_all(err, &ws);
            }
            Err(e) => {
                failed += 1;
                let _ = writeln!(err, "FAIL  {}: {e}", input.display());
            }
        }
    }
    let _ = writeln!(out, "{ok} succeeded, {failed} failed");
    if failed > 0 { Err(format!("{failed} file(s) failed")) } else { Ok(()) }
}

fn droplet(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let (file, inputs) = a.positional.split_first().ok_or("droplet needs <file.pcdroplet> and inputs")?;
    if inputs.is_empty() {
        return Err("droplet needs at least one input file or folder".into());
    }
    let mut p = json!({"droplet": file, "input": inputs});
    if let Some(o) = a.get("--out") {
        p["output"] = json!(o);
    }
    let mut s = photocraft_engine::Session::new();
    let r = s.execute("file.automate.runDroplet", p).map_err(|e| e.to_string())?;
    for f in r["files"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "ok    {}", f.as_str().unwrap_or_default());
    }
    let errors = r["errors"].as_array().cloned().unwrap_or_default();
    for e in &errors {
        let _ = writeln!(err, "FAIL  {}: {}", e["file"].as_str().unwrap_or_default(), e["error"].as_str().unwrap_or_default());
    }
    if errors.is_empty() { Ok(()) } else { Err(format!("{} file(s) failed", errors.len())) }
}

fn commands(a: &Args, out: &mut dyn Write) -> R {
    let h = Headless::new();
    let list = h.command_list();
    let needle = a.get("--filter").map(str::to_lowercase);
    let items: Vec<&Value> = list
        .as_array()
        .map(|v| v.iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            needle.as_ref().is_none_or(|n| format!("{} {}", c["id"].as_str().unwrap_or(""), c["label"].as_str().unwrap_or("")).to_lowercase().contains(n))
        })
        .collect();
    if a.has("--json") {
        return print_json(out, &Value::Array(items.into_iter().cloned().collect()), false);
    }
    for c in items {
        let menu = c["menu"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" > ")).unwrap_or_default();
        writeln!(out, "{:<44} {:<32} {}", c["id"].as_str().unwrap_or(""), c["label"].as_str().unwrap_or(""), menu).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn serve(a: &Args, err: &mut dyn Write) -> R {
    use std::sync::{Arc, Mutex};
    let h = Arc::new(Mutex::new(Headless::with_workspace(automation_workspace(a)?)));
    match a.get("--port") {
        Some(port) => {
            let port: u16 = port.parse().map_err(|_| format!("bad --port `{port}`"))?;
            let addr = format!("127.0.0.1:{port}");
            let (supplied, token_file) = security::token_inputs(a.get("--control-token").map(str::to_owned), a.get("--control-token-file").map(PathBuf::from));
            let token = security::server_token(supplied.as_deref(), token_file.as_deref()).map_err(|e| e.to_string())?;
            if let Some(path) = &token_file {
                let _ = writeln!(err, "photocraft-cli control token file: {}", path.display());
            } else if supplied.is_none() {
                let _ = writeln!(err, "photocraft-cli control token: {token}");
            }
            photocraft_automation::rpc::serve_tcp(&addr, h, token, |local| {
                let _ = writeln!(err, "photocraft-cli serving on {local}");
            })
            .map_err(|e| e.to_string())
        }
        None => {
            let stdin = std::io::stdin();
            photocraft_automation::rpc::serve_lines(&h, stdin.lock(), std::io::stdout()).map_err(|e| e.to_string())
        }
    }
}

fn mcp(a: &Args) -> R {
    let server = match a.get("--bridge") {
        Some(addr) => {
            let (supplied, token_file) = security::token_inputs(a.get("--control-token").map(str::to_owned), a.get("--control-token-file").map(PathBuf::from));
            let token = security::client_token(supplied.as_deref(), token_file.as_deref()).map_err(|e| e.to_string())?;
            PhotocraftMcp::bridge(addr, &token).map_err(|e| e.to_string())?
        }
        None => PhotocraftMcp::headless_with_workspace(automation_workspace(a)?),
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(server.serve_stdio()).map_err(|e| e.to_string())
}

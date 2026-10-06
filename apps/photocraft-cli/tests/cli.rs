//! Process-level tests of the `photocraft-cli` binary.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_photocraft-cli"))
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pc-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(path: &Path, w: u32, h: u32, seed: u8) {
    let data: Vec<u8> = (0..w * h * 4).map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed)).collect();
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, data).unwrap();
    std::fs::write(path, photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
}

fn ok(cmd: &mut Command) -> (String, String) {
    let o = cmd.output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned());
    assert!(o.status.success(), "exit {:?}\nstdout: {out}\nstderr: {err}", o.status);
    (out, err)
}

#[test]
fn usage_and_unknown_command() {
    let o = bin().output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("USAGE"));
    let o = bin().arg("frobnicate").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let (out, _) = ok(bin().arg("--help"));
    assert!(out.contains("convert"));
    let (out, _) = ok(bin().arg("--version"));
    assert!(out.starts_with("photocraft-cli "));
}

#[test]
fn convert_png_pcraft_png_is_lossless() {
    let d = tmp("convert");
    let (a, b, c) = (d.join("a.png"), d.join("b.pcraft"), d.join("c.png"));
    write_png(&a, 37, 23, 7);
    ok(bin().args(["convert"]).arg(&a).arg(&b));
    assert!(photocraft_format::is_pcraft(&std::fs::read(&b).unwrap()));
    ok(bin().args(["convert"]).arg(&b).arg(&c));
    let x = photocraft_codecs::decode(&std::fs::read(&a).unwrap()).unwrap();
    let y = photocraft_codecs::decode(&std::fs::read(&c).unwrap()).unwrap();
    assert_eq!(x.to_rgba8(), y.to_rgba8());
    // Format override and lossy warning path.
    ok(bin().args(["convert"]).arg(&a).arg(d.join("out.bin")).args(["--format", "jpg", "--quality", "60"]));
    assert!(photocraft_codecs::detect(&std::fs::read(d.join("out.bin")).unwrap()) == Some(photocraft_codecs::Format::Jpeg));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn convert_errors() {
    let d = tmp("converr");
    let o = bin().args(["convert", "/missing/in.png"]).arg(d.join("x.png")).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = bin().args(["convert", "only-one"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn info_prints_layer_tree_json() {
    let d = tmp("info");
    let a = d.join("a.png");
    write_png(&a, 10, 6, 3);
    let (out, _) = ok(bin().arg("info").arg(&a));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["width"], 10);
    assert_eq!(v["height"], 6);
    assert_eq!(v["layers"][0]["name"], "Background");
    assert!(v.get("history").is_none());
    let (out, _) = ok(bin().arg("info").arg(&a).arg("--compact"));
    assert_eq!(out.trim().lines().count(), 1);
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn run_commands_and_save() {
    let d = tmp("run");
    let out_pc = d.join("r.pcraft");
    let (out, _) = ok(bin()
        .args(["run", "--new", r#"{"width":40,"height":30,"name":"Run"}"#])
        .args(["--cmd", "layer.new.layer", "--params", r#"{"name":"Ink"}"#])
        .args(["--cmd", "paint.stroke", "--params", r##"{"points":[[2,2,1],[30,20,1]],"size":5,"color":"#00ff00"}"##])
        .args(["--cmd", "document.inspect"])
        .arg("--out")
        .arg(&out_pc));
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2]["command"], "document.inspect");
    let (info, _) = ok(bin().arg("info").arg(&out_pc));
    let v: Value = serde_json::from_str(&info).unwrap();
    assert_eq!(v["name"], "Run");
    let names: Vec<&str> = v["layers"].as_array().unwrap().iter().filter_map(|l| l["name"].as_str()).collect();
    assert_eq!(names, ["Ink", "Background"]);
    // Continue from the saved file and export PNG.
    let png = d.join("r.png");
    ok(bin().arg("run").arg(&out_pc).args(["--cmd", "layer.new.layer"]).arg("--out").arg(&png));
    assert_eq!(photocraft_codecs::decode(&std::fs::read(&png).unwrap()).unwrap().dimensions(), (40, 30));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn run_errors() {
    let o = bin().args(["run", "--new", "{}", "--cmd", "no.such"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown command"));
    let o = bin().args(["run", "--new", "{}", "--params", "{}"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = bin().args(["run", "--new", "{", "--cmd", "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn batch_applies_actions_to_directory() {
    let d = tmp("batch");
    let (inp, outp) = (d.join("in"), d.join("out"));
    std::fs::create_dir_all(&inp).unwrap();
    write_png(&inp.join("one.png"), 12, 8, 1);
    write_png(&inp.join("two.png"), 9, 9, 2);
    std::fs::write(inp.join("notes.txt"), "ignored").unwrap();
    let actions = d.join("actions.json");
    std::fs::write(&actions, json!([{"command": "layer.new.layer", "params": {"name": "Batch"}}]).to_string()).unwrap();
    let (out, _) = ok(bin().args(["batch", "--actions"]).arg(&actions).arg("--in").arg(&inp).arg("--out").arg(&outp).args(["--format", "pcraft"]));
    assert!(out.contains("2 succeeded, 0 failed"), "{out}");
    for n in ["one", "two"] {
        let (info, _) = ok(bin().arg("info").arg(outp.join(format!("{n}.pcraft"))));
        let v: Value = serde_json::from_str(&info).unwrap();
        assert_eq!(v["layers"][0]["name"], "Batch");
    }
    // A failing action reports and exits 1.
    std::fs::write(&actions, r#"{"actions":[{"id":"no.such.command"}]}"#).unwrap();
    let o = bin().args(["batch", "--actions"]).arg(&actions).arg("--in").arg(&inp).arg("--out").arg(&outp).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("FAIL"));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn parse_actions_formats() {
    let a = photocraft_cli::parse_actions(r#"[{"command":"a","params":{"x":1}},{"id":"b"}]"#).unwrap();
    assert_eq!(a, vec![("a".to_string(), json!({"x":1})), ("b".to_string(), json!({}))]);
    assert!(photocraft_cli::parse_actions(r#"{"actions":[{"command":"c"}]}"#).unwrap().len() == 1);
    assert!(photocraft_cli::parse_actions(r#"[{"params":{}}]"#).is_err());
    assert!(photocraft_cli::parse_actions("7").is_err());
}

#[test]
fn commands_listing() {
    let (out, _) = ok(bin().arg("commands"));
    assert!(out.contains("file.new"));
    let (out, _) = ok(bin().args(["commands", "--json", "--filter", "layer.new"]));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|c| { c["id"].as_str().unwrap().contains("layer.new") || c["label"].as_str().unwrap().to_lowercase().contains("layer.new") })
    );
}

#[test]
fn in_process_run_matches_binary() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = photocraft_cli::run(&["commands".into(), "--filter".into(), "file.new".into()], &mut out, &mut err);
    assert_eq!(code, 0);
    assert!(String::from_utf8(out).unwrap().contains("file.new"));
}

#[test]
fn mcp_stdio_handshake_and_tool_call() {
    let mut child = bin().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = |id: u64| -> Value {
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "server closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    };
    send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}));
    let init = recv(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "photocraft", "{init}");
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let tools = recv(2);
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"command_run") && names.contains(&"doc_render_preview"), "{names:?}");
    send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"doc_new","arguments":{"width":8,"height":8}}}));
    let r = recv(3);
    assert_ne!(r["result"]["isError"], true, "{r}");
    send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"doc_render_preview","arguments":{}}}));
    let r = recv(4);
    assert_eq!(r["result"]["content"][0]["type"], "image", "{r}");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn droplet_runs_an_action_on_files() {
    let d = tmp("droplet");
    write_png(&d.join("a.png"), 8, 4, 3);
    write_png(&d.join("b.png"), 6, 2, 5);
    let droplet = d.join("rot.pcdroplet");
    std::fs::write(
        &droplet,
        json!({"photocraftDroplet": 1, "name": "rot", "action": {"steps": [["image.imageRotation.90cw", {}]]}, "options": {"format": "png"}}).to_string(),
    )
    .unwrap();
    let out_dir = d.join("out");
    let (out, _) = ok(bin().arg("droplet").arg(&droplet).arg(d.join("a.png")).arg(d.join("b.png")).arg("--out").arg(&out_dir));
    assert_eq!(out.lines().filter(|l| l.starts_with("ok")).count(), 2, "{out}");
    let img = photocraft_codecs::decode(&std::fs::read(out_dir.join("a.png")).unwrap()).unwrap();
    assert_eq!(img.dimensions(), (4, 8));
    let o = bin().arg("droplet").arg(d.join("a.png")).arg(d.join("b.png")).output().unwrap();
    assert!(!o.status.success(), "not a droplet");
}

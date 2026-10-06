//! Spin the MCP server in-process (tokio duplex), call tools as a client.

use std::io::Write;

use base64::Engine as _;
use photocraft_automation::{AuthorizedWorkspace, PhotocraftMcp};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CONTROL_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Clone, Default)]
struct Client;
impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

async fn connect(server: PhotocraftMcp) -> RunningService<RoleClient, Client> {
    let (s, c) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(s).await {
            let _ = running.waiting().await;
        }
    });
    Client.serve(c).await.expect("client init")
}

async fn call(client: &RunningService<RoleClient, Client>, name: &str, args: Value) -> CallToolResult {
    let mut p = CallToolRequestParams::new(name.to_owned());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    client.call_tool(p).await.expect("call_tool transport")
}

fn text(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
}

fn json_of(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "tool error: {}", text(r));
    serde_json::from_str(&text(r)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(r)))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pc-mcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cleanup(path: &std::path::Path) {
    for _ in 0..20 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    std::fs::remove_dir_all(path).expect("remove test workspace after server shutdown");
}

fn headless_in(root: &std::path::Path) -> PhotocraftMcp {
    let workspace = AuthorizedWorkspace::new(Some(root), Some(root)).expect("test workspace");
    PhotocraftMcp::headless_with_workspace(workspace)
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_expected_tools() {
    let client = connect(PhotocraftMcp::headless()).await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    for n in [
        "session_list",
        "doc_open",
        "doc_new",
        "doc_save",
        "doc_export",
        "doc_inspect",
        "doc_render_preview",
        "doc_select",
        "doc_close",
        "command_list",
        "command_run",
        "command_batch",
        "ui_inspect",
        "ui_screenshot",
        "ui_pointer",
        "ui_menu_invoke",
        "ui_set",
        "control_call",
    ] {
        assert!(names.contains(&n.to_string()), "missing tool {n}: {names:?}");
    }
    for t in &tools {
        assert!(t.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "tool name `{}`", t.name);
        assert!(t.description.as_ref().is_some_and(|d| !d.is_empty()));
    }
    let info = client.peer_info().expect("server info");
    assert_eq!(info.server_info.as_ref().unwrap().name, "photocraft");
    assert!(info.instructions.as_ref().is_some_and(|i| i.contains("command_list")));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_edit_render_save_roundtrip() {
    let dir = tmp("edit");
    let client = connect(headless_in(&dir)).await;

    let r = call(&client, "doc_new", json!({"width": 64, "height": 48, "background": "white", "name": "Agent"})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    let r = call(&client, "command_run", json!({"id": "layer.new.layer", "params": {"name": "Ink"}})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    let r =
        call(&client, "command_run", json!({"id": "paint.stroke", "params": {"points": [[5, 5, 1.0], [50, 40, 1.0]], "size": 6, "color": "#ff0000"}})).await;
    assert_ne!(r.is_error, Some(true), "{}", text(&r));

    let doc = json_of(&call(&client, "doc_inspect", json!({})).await);
    assert_eq!(doc["width"], 64);
    let names: Vec<&str> = doc["layers"].as_array().unwrap().iter().filter_map(|l| l["name"].as_str()).collect();
    assert_eq!(names, ["Ink", "Background"]);

    let r = call(&client, "doc_render_preview", json!({"max_side": 32})).await;
    let img = r.content.iter().find_map(|c| c.as_image()).expect("image content");
    assert_eq!(img.mime_type, "image/png");
    let png = base64::engine::general_purpose::STANDARD.decode(&img.data).unwrap();
    let decoded = photocraft_codecs::decode(&png).unwrap();
    assert_eq!(decoded.dimensions(), (32, 24));

    let r = json_of(&call(&client, "doc_save", json!({"path": "agent.pcraft"})).await);
    assert_eq!(r["path"], "agent.pcraft");
    let png_path = dir.join("agent.png");
    json_of(&call(&client, "doc_export", json!({"path": "agent.png"})).await);
    let jpg_path = dir.join("agent.jpg");
    json_of(&call(&client, "doc_export", json!({"path": "agent.jpg", "quality": 70})).await);
    assert!(photocraft_codecs::decode(&std::fs::read(&png_path).unwrap()).is_ok());
    assert!(std::fs::metadata(&jpg_path).unwrap().len() > 100);

    // Re-open the native file: identical layer tree.
    let o = json_of(&call(&client, "doc_open", json!({"path": "agent.pcraft"})).await);
    assert_eq!(o["index"], 1);
    let doc2 = json_of(&call(&client, "doc_inspect", json!({"index": 1})).await);
    assert_eq!(doc2["layers"].as_array().unwrap().len(), 2);
    let sess = json_of(&call(&client, "session_list", json!({})).await);
    assert_eq!(sess["documents"].as_array().unwrap().len(), 2);
    json_of(&call(&client, "doc_close", json!({"index": 1})).await);
    json_of(&call(&client, "doc_select", json!({"index": 0})).await);
    client.cancel().await.unwrap();
    cleanup(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn command_list_filters() {
    let client = connect(PhotocraftMcp::headless()).await;
    let all = json_of(&call(&client, "command_list", json!({})).await);
    let n_all = all.as_array().unwrap().len();
    assert!(n_all > 20);
    let blur = json_of(&call(&client, "command_list", json!({"filter": "blur"})).await);
    assert!(!blur.as_array().unwrap().is_empty() && blur.as_array().unwrap().len() < n_all);
    assert!(blur.as_array().unwrap().iter().all(|c| c["params"].is_string()));
    let enabled = json_of(&call(&client, "command_list", json!({"enabled_only": true})).await);
    assert!(enabled.as_array().unwrap().iter().any(|c| c["id"] == "file.new"));
    assert!(!enabled.as_array().unwrap().iter().any(|c| c["id"] == "layer.new.layer"), "needs a document");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_are_tool_errors_not_crashes() {
    let client = connect(PhotocraftMcp::headless()).await;
    let r = call(&client, "command_run", json!({"id": "no.such.command"})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("unknown command"));
    let r = call(&client, "doc_inspect", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    let r = call(&client, "doc_open", json!({"path": "/definitely/missing.png"})).await;
    assert_eq!(r.is_error, Some(true));
    let r = call(&client, "ui_inspect", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("bridge"));
    let r = call(&client, "doc_export", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    // Server still alive.
    json_of(&call(&client, "session_list", json!({})).await);
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_budget_failure_preserves_the_mcp_session() {
    let client = connect(PhotocraftMcp::headless()).await;
    json_of(&call(&client, "doc_new", json!({"width": 2049, "height": 1})).await);
    for side in [0, 2049] {
        let reply = call(&client, "doc_render_preview", json!({"max_side": side})).await;
        assert_eq!(reply.is_error, Some(true));
        assert!(text(&reply).contains("preview side exceeds"));
    }
    let preview = call(&client, "doc_render_preview", json!({"max_side": 32})).await;
    assert!(preview.content.iter().any(|content| content.as_image().is_some()));
    let inspected = json_of(&call(&client, "doc_inspect", json!({})).await);
    assert_eq!(inspected["width"], 2049);
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_response_budget_drops_connection_without_retrying_the_operation() {
    use photocraft_automation::{BridgeClient, budgets::MAX_RESPONSE_BYTES};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (read, mut write) = sock.into_split();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let auth: Value = serde_json::from_str(&line).unwrap();
        let reply = json!({"id": auth["id"], "ok": true, "result": {"authenticated": true}});
        write.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "engine.execute");
        let mut oversized = vec![b'x'; MAX_RESPONSE_BYTES];
        // The read limit cuts this multi-byte character in half. Classification must
        // still be a non-retryable budget error, not a UTF-8 transport failure.
        oversized.extend_from_slice("é".as_bytes());
        write.write_all(&oversized).await.unwrap();
        // A retry would connect to this still-live listener. The budget error must return
        // directly instead, without waiting for another authentication exchange.
        let next = tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await;
        assert!(next.is_err(), "oversized reply must not retry the operation");
    });
    let bridge = BridgeClient::new(&addr, CONTROL_TOKEN).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), bridge.call("engine.execute", json!({"command": "command.list"}))).await.unwrap();
    assert!(result.unwrap_err().to_string().contains("bridge response exceeds"));
    app.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn open_png_and_inspect() {
    let dir = tmp("open");
    let img = photocraft_codecs::Image::from_u8(8, 4, photocraft_codecs::ChannelLayout::Rgb, vec![200; 96]).unwrap();
    let path = dir.join("in.png");
    std::fs::File::create(&path).unwrap().write_all(&photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
    let client = connect(headless_in(&dir)).await;
    let o = json_of(&call(&client, "doc_open", json!({"path": "in.png"})).await);
    assert_eq!((o["width"].as_u64(), o["height"].as_u64()), (Some(8), Some(4)));
    let px = json_of(&call(&client, "command_run", json!({"id": "document.pixel", "params": {"x": 1, "y": 1}})).await);
    assert!(px.to_string().contains("0.78"), "{px}");
    client.cancel().await.unwrap();
    cleanup(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn filesystem_policy_rejects_absolute_and_escaping_paths_before_effects() {
    let base = tmp("filesystem-policy");
    let root = base.join("workspace");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let img = photocraft_codecs::Image::from_u8(4, 3, photocraft_codecs::ChannelLayout::Rgb, vec![42; 36]).unwrap();
    let bytes = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(root.join("inside.png"), bytes).unwrap();
    let client = connect(headless_in(&root)).await;

    assert_ne!(call(&client, "doc_open", json!({"path": "inside.png"})).await.is_error, Some(true));
    let absolute = outside.join("absolute.png");
    let response = call(&client, "doc_save", json!({"path": absolute.to_string_lossy()})).await;
    assert_eq!(response.is_error, Some(true));
    assert!(text(&response).contains("automation path rejected"), "{}", text(&response));
    assert!(!absolute.exists());

    for path in ["../outside/traversal.png", "..\\outside\\mixed.png", "C:/outside/prefix.png", ""] {
        let response = call(&client, "doc_save", json!({"path": path})).await;
        assert_eq!(response.is_error, Some(true), "path {path:?}: {}", text(&response));
    }
    assert!(!outside.join("traversal.png").exists());
    assert!(!outside.join("mixed.png").exists());

    let response = call(&client, "command_run", json!({"id": "layer.smartObjects.exportContents", "params": {"path": "outside.bin"}})).await;
    assert_eq!(response.is_error, Some(true));
    assert!(text(&response).contains("ambient filesystem paths"), "{}", text(&response));
    client.cancel().await.unwrap();
    cleanup(&base);
}

// ---------------------------------------------------------------------------
// Bridge mode against a fake control server
// ---------------------------------------------------------------------------

async fn fake_app() -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let h = tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut authenticated = false;
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["id"].clone();
            let method = req["method"].as_str().unwrap_or("").to_owned();
            let reply = match method.as_str() {
                "auth" if req["params"]["token"] == CONTROL_TOKEN => {
                    authenticated = true;
                    json!({"id": id, "ok": true, "result": {"authenticated": true}})
                }
                _ if !authenticated => {
                    json!({"id": id, "ok": false, "error": "authentication required"})
                }
                "ui.inspect" => {
                    json!({"id": id, "ok": true, "result": {"tool": "brush", "panels": ["layers"]}})
                }
                "engine.execute" => {
                    json!({"id": id, "ok": true, "result": {"ran": req["params"]["command"], "params": req["params"]["params"]}})
                }
                "engine.commands" => {
                    json!({"id": id, "ok": true, "result": [{"id": "file.new", "label": "New…", "enabled": true}]})
                }
                "ui.screenshot" => {
                    let img = photocraft_codecs::Image::from_u8(40, 20, photocraft_codecs::ChannelLayout::Rgba, vec![9; 3200]).unwrap();
                    let bytes = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
                    let png = base64::engine::general_purpose::STANDARD.encode(bytes);
                    json!({"id": id, "ok": true, "result": {"mimeType": "image/png", "base64": png}})
                }
                _ => json!({"id": id, "ok": false, "error": format!("unknown tool `{method}`")}),
            };
            // A stale line first, to check id matching.
            w.write_all(b"{\"id\":999999,\"ok\":true,\"result\":null}\n").await.unwrap();
            w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
            seen.push(req);
        }
        seen
    });
    (addr, h)
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_forwards_to_control_protocol() {
    let (addr, app) = fake_app().await;
    let client = connect(PhotocraftMcp::bridge(&addr, CONTROL_TOKEN).unwrap()).await;

    let ui = json_of(&call(&client, "ui_inspect", json!({})).await);
    assert_eq!(ui["tool"], "brush");
    let r = json_of(&call(&client, "command_run", json!({"id": "layer.new.layer", "params": {"name": "X"}})).await);
    assert_eq!(r["ran"], "layer.new.layer");
    let inspected = json_of(&call(&client, "doc_inspect", json!({"index": 1})).await);
    assert_eq!(inspected["ran"], "document.inspect");
    assert_eq!(inspected["params"], json!({"document": 1}));
    let l = json_of(&call(&client, "command_list", json!({})).await);
    assert_eq!(l[0]["id"], "file.new");
    let shot = call(&client, "ui_screenshot", json!({"max_side": 20})).await;
    let img = shot.content.iter().find_map(|c| c.as_image()).expect("image");
    let png = base64::engine::general_purpose::STANDARD.decode(&img.data).unwrap();
    assert_eq!(photocraft_codecs::decode(&png).unwrap().dimensions(), (20, 10));
    let e = call(&client, "control_call", json!({"method": "bogus.method"})).await;
    assert_eq!(e.is_error, Some(true));
    assert!(text(&e).contains("unknown tool"));
    let r = call(&client, "doc_select", json!({"index": 0})).await;
    assert_eq!(r.is_error, Some(true));

    client.cancel().await.unwrap();
    app.abort();
}

#[test]
fn bridge_rejects_non_loopback() {
    assert!(PhotocraftMcp::bridge("10.0.0.5:7878", CONTROL_TOKEN).is_err());
    assert!(PhotocraftMcp::bridge("127.0.0.1:7878", CONTROL_TOKEN).is_ok());
    assert!(PhotocraftMcp::bridge("localhost:1", CONTROL_TOKEN).is_ok());
    assert!(PhotocraftMcp::bridge("localhost:1", "short").is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_rejects_wrong_token_before_control_methods() {
    let (addr, app) = fake_app().await;
    let wrong = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let client = connect(PhotocraftMcp::bridge(&addr, wrong).unwrap()).await;
    let r = call(&client, "ui_inspect", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("authentication required"), "{}", text(&r));
    client.cancel().await.unwrap();
    app.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn bridge_reports_unreachable_app() {
    let client = connect(PhotocraftMcp::bridge("127.0.0.1:1", CONTROL_TOKEN).unwrap()).await;
    let r = call(&client, "ui_inspect", json!({})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("--control"), "{}", text(&r));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn command_batch_runs_steps_in_order() {
    let client = connect(PhotocraftMcp::headless()).await;
    json_of(&call(&client, "doc_new", json!({"width": 32, "height": 32})).await);
    let r = json_of(
        &call(
            &client,
            "command_batch",
            json!({"steps": [
                {"id": "layer.new.layer", "params": {"name": "One"}},
                {"id": "layer.new.layer", "params": {"name": "Two"}},
                {"id": "no.such.command"},
                {"id": "layer.new.layer", "params": {"name": "Three"}}
            ]}),
        )
        .await,
    );
    assert_eq!(r["completed"], 2);
    assert_eq!(r["failed"], 1);
    let doc = json_of(&call(&client, "doc_inspect", json!({})).await).to_string();
    assert!(doc.contains("\"Two\"") && !doc.contains("\"Three\""));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn command_batch_rejects_too_many_steps() {
    let client = connect(PhotocraftMcp::headless()).await;
    let steps: Vec<Value> = (0..=photocraft_automation::security::MAX_BATCH_STEPS).map(|_| json!({"id": "command.list"})).collect();
    let r = call(&client, "command_batch", json!({"steps": steps})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("maximum is 256"), "{}", text(&r));
    client.cancel().await.unwrap();
}

//! M11 acceptance: an agent completes 10 scripted edit tasks over MCP (roadmap M11 DoD).
//!
//! Each task uses only MCP tools (`doc_*`, `command_run`) the way an agent would, and checks the
//! outcome through `doc_inspect`, `document.pixel` or the exported files, never engine internals.

use std::io::Write;

use photocraft_automation::{AuthorizedWorkspace, PhotocraftMcp};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Client;
impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

type Conn = RunningService<RoleClient, Client>;

async fn connect(root: &std::path::Path) -> Conn {
    let (s, c) = tokio::io::duplex(1 << 20);
    let workspace = AuthorizedWorkspace::new(Some(root), Some(root)).expect("test workspace");
    tokio::spawn(async move {
        if let Ok(running) = PhotocraftMcp::headless_with_workspace(workspace).serve(s).await {
            let _ = running.waiting().await;
        }
    });
    Client.serve(c).await.expect("client init")
}

fn text(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
}

/// Call a tool and parse its JSON result, failing the test on a tool error.
async fn tool(c: &Conn, name: &str, args: Value) -> Value {
    let mut p = CallToolRequestParams::new(name.to_owned());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    let r = c.call_tool(p).await.expect("call_tool transport");
    assert_ne!(r.is_error, Some(true), "{name} failed: {}", text(&r));
    serde_json::from_str(&text(&r)).unwrap_or(Value::String(text(&r)))
}

async fn run(c: &Conn, id: &str, params: Value) -> Value {
    tool(c, "command_run", json!({"id": id, "params": params})).await
}

async fn inspect(c: &Conn) -> Value {
    tool(c, "doc_inspect", json!({})).await
}

/// Composite RGBA at (x, y), whatever envelope `command_run` wraps the result in.
async fn pixel(c: &Conn, x: i32, y: i32) -> Vec<f64> {
    let v = run(c, "document.pixel", json!({"x": x, "y": y})).await;
    let arr = if v.is_array() { v } else { v.get("result").cloned().unwrap_or(v) };
    arr.as_array().unwrap_or_else(|| panic!("pixel: {arr}")).iter().map(|x| x.as_f64().unwrap()).collect()
}

fn top_layer(doc: &Value) -> &Value {
    &doc["layers"][0]
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pc-agent-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 64×48 RGB gradient PNG (red ramps across, green down).
fn gradient_png(dir: &std::path::Path) -> &'static str {
    let (w, h) = (64u32, 48u32);
    let mut px = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, 128]);
        }
    }
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgb, px).unwrap();
    let path = dir.join("gradient.png");
    std::fs::File::create(&path).unwrap().write_all(&photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
    "gradient.png"
}

fn close(a: &[f64], b: &[f64], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_completes_ten_scripted_tasks() {
    let dir = tmp("tasks");
    let png = gradient_png(&dir);
    let c = connect(&dir).await;

    // 1. Make a title card: type layer with a drop shadow, exported as PNG.
    tool(&c, "doc_new", json!({"width": 200, "height": 120, "background": "white"})).await;
    run(&c, "type.create", json!({"x": 20, "y": 70, "text": "Hello", "size": 36})).await;
    run(&c, "layer.layerStyle.dropShadow", json!({"distance": 4, "size": 3})).await;
    let doc = inspect(&c).await;
    let t = top_layer(&doc);
    assert_eq!(t["kind"], "Type", "task 1: {t}");
    assert_eq!(t["text"]["text"], "Hello", "task 1: {t}");
    assert_eq!(t["effects"]["items"][0]["kind"], "Drop Shadow", "task 1: {t}");
    let out = dir.join("card.png");
    tool(&c, "doc_export", json!({"path": "card.png"})).await;
    let card = photocraft_codecs::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(card.dimensions(), (200, 120), "task 1");
    tool(&c, "doc_close", json!({})).await;

    // 2. Colour grade a photo non-destructively: Curves brightens the midtones, Vibrance on top.
    tool(&c, "doc_open", json!({"path": png})).await;
    let before = pixel(&c, 16, 24).await;
    run(&c, "layer.newAdjustmentLayer.curves", json!({"points": [[0, 0], [128, 200], [255, 255]]})).await;
    run(&c, "layer.newAdjustmentLayer.vibrance", json!({"vibrance": 40, "saturation": 0})).await;
    let after = pixel(&c, 16, 24).await;
    assert!(after[1] > before[1] + 0.05, "task 2: curves should brighten green {before:?} -> {after:?}");
    let doc = inspect(&c).await;
    let kinds: Vec<&str> = doc["layers"].as_array().unwrap().iter().filter_map(|l| l["kind"].as_str()).collect();
    assert_eq!(kinds, ["Adjustment", "Adjustment", "Pixel"], "task 2");

    // 3. Undo/redo: remove both adjustments, restore one.
    run(&c, "edit.undo", json!({})).await;
    run(&c, "edit.undo", json!({})).await;
    assert!(close(&pixel(&c, 16, 24).await, &before, 1e-6), "task 3: undo should restore the original");
    run(&c, "edit.redo", json!({})).await;
    assert_eq!(inspect(&c).await["layers"].as_array().unwrap().len(), 2, "task 3: redo restores Curves");
    run(&c, "edit.undo", json!({})).await;

    // 4. Editable blur: smart object + Gaussian Blur, retune the radius, then hide the filter.
    run(&c, "layer.smartObjects.convertToSmartObject", json!({})).await;
    run(&c, "filter.blur.gaussianBlur", json!({"radius": 2})).await;
    run(&c, "layer.smartFilter.setParams", json!({"index": 0, "params": {"radius": 6}})).await;
    let t = inspect(&c).await;
    let sf = &top_layer(&t)["smartFilters"][0];
    assert_eq!(sf["command"], "filter.blur.gaussianBlur", "task 4: {t}");
    assert_eq!(sf["params"]["radius"].as_f64(), Some(6.0), "task 4: {sf}");
    run(&c, "layer.smartFilter.setVisible", json!({"index": 0, "visible": false})).await;
    assert!(close(&pixel(&c, 16, 24).await, &before, 1.0 / 255.0), "task 4: hiding the filter restores the pixels");

    // 5. Selection → layer mask: an elliptical reveal on a red layer.
    run(&c, "layer.new.layer", json!({"name": "Red"})).await;
    run(&c, "select.all", json!({})).await;
    run(&c, "edit.fill", json!({"color": "#ff0000"})).await;
    run(&c, "select.rect", json!({"x": 16, "y": 12, "width": 32, "height": 24, "ellipse": true})).await;
    run(&c, "layer.layerMask.revealSelection", json!({})).await;
    assert_eq!(top_layer(&inspect(&c).await)["hasMask"], true, "task 5");
    let inside = pixel(&c, 32, 24).await;
    let outside = pixel(&c, 1, 1).await;
    assert!(inside[0] > 0.99 && inside[1] < 0.01, "task 5: centre is red {inside:?}");
    assert!(outside[0] < 0.1, "task 5: corner shows the photo {outside:?}");
    run(&c, "select.deselect", json!({})).await;

    // 6. Save a selection to a channel and load it back.
    run(&c, "select.rect", json!({"x": 4, "y": 6, "width": 20, "height": 10})).await;
    run(&c, "select.saveSelection", json!({"name": "Keep"})).await;
    run(&c, "select.deselect", json!({})).await;
    assert_eq!(inspect(&c).await["hasSelection"], false, "task 6");
    run(&c, "select.loadSelection", json!({"channel": "Keep"})).await;
    let doc = inspect(&c).await;
    assert_eq!(doc["selectionBounds"], json!([4, 6, 20, 10]), "task 6: {}", doc["selectionBounds"]);
    assert_eq!(doc["channels"]["alpha"][0]["name"], "Keep", "task 6");
    run(&c, "select.deselect", json!({})).await;
    tool(&c, "doc_close", json!({})).await;

    // 7. Lay out three swatches and align their left edges.
    tool(&c, "doc_new", json!({"width": 120, "height": 90, "background": "white"})).await;
    for (i, x) in [10, 40, 70].into_iter().enumerate() {
        run(&c, "layer.new.layer", json!({"name": format!("S{i}")})).await;
        run(&c, "select.rect", json!({"x": x, "y": 10 + 25 * i as i32, "width": 20, "height": 15})).await;
        run(&c, "edit.fill", json!({"color": "#3366cc"})).await;
    }
    run(&c, "select.deselect", json!({})).await;
    run(&c, "select.allLayers", json!({})).await;
    run(&c, "layer.align.leftEdges", json!({})).await;
    let doc = inspect(&c).await;
    let xs: Vec<i64> = doc["layers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["name"].as_str().is_some_and(|n| n.starts_with('S')))
        .map(|l| l["bounds"][0].as_i64().unwrap())
        .collect();
    assert_eq!(xs, [10, 10, 10], "task 7: {}", doc["layers"]);

    // 8. Export through the capability-scoped document API.
    let layers_dir = dir.join("layers");
    std::fs::create_dir_all(&layers_dir).unwrap();
    tool(&c, "doc_export", json!({"path": "layers/swatch.png"})).await;
    let n = std::fs::read_dir(&layers_dir).unwrap().filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "png"))).count();
    assert_eq!(n, 1, "task 8: capability-scoped export");

    // 9. Resize, then crop to a square.
    run(&c, "image.imageSize", json!({"width": 60, "height": 45})).await;
    run(&c, "image.crop", json!({"x": 5, "y": 0, "width": 45, "height": 45})).await;
    let doc = inspect(&c).await;
    assert_eq!((doc["width"].as_u64(), doc["height"].as_u64()), (Some(45), Some(45)), "task 9");

    // 10. Prepare for print: convert to CMYK and save natively, then reopen.
    run(&c, "image.mode.cmyk", json!({"intent": "perceptual"})).await;
    assert_eq!(inspect(&c).await["mode"], "Cmyk", "task 10");
    tool(&c, "doc_save", json!({"path": "print.pcraft"})).await;
    tool(&c, "doc_close", json!({})).await;
    tool(&c, "doc_open", json!({"path": "print.pcraft"})).await;
    let doc = inspect(&c).await;
    assert_eq!((doc["mode"].as_str(), doc["width"].as_u64()), (Some("Cmyk"), Some(45)), "task 10");
    assert_eq!(doc["layers"].as_array().unwrap().len(), 4, "task 10");

    c.cancel().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

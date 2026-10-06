//! Render the full Photocraft UI offscreen (no window, no focus stealing) and save a PNG.
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example snapshot -- \
//!     --out ui.png --size 1440x900 --scale 2 --open photo.jpg \
//!     --script '[["ui.set", {"tool": "type"}], ["ui.menu.invoke", {"id": "image.imageSize"}]]'
//! ```
//!
//! `--safe-gpu` draws the canvas on the CPU path, like the app's `--safe-gpu` launch.
//!
//! `--monitor 1366x768 --window-top 31` simulates the display the window is on (in points) and
//! where its content starts on it, e.g. a window running under a Windows taskbar.
//!
//! `--script` is a JSON array of `[method, params]` control-protocol calls (see
//! docs/control-protocol.md), applied in order with a few frames between them.

use photocraft_ui_egui::control::{ControlRequest, Outcome, handle};
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::Value;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = arg(&args, "--out").unwrap_or_else(|| "snapshot.png".into());
    let (w, h) = arg(&args, "--size")
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))))
        .unwrap_or((1440.0, 900.0));
    let scale: f32 = arg(&args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let script: Vec<(String, Value)> =
        arg(&args, "--script").map(|s| serde_json::from_str::<Vec<(String, Value)>>(&s).expect("--script must be [[method, params], …]")).unwrap_or_default();

    let services = Services {
        import: Some(Box::new(|name: &str, bytes: &[u8]| photocraft_io::import(name, bytes).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string()))),
        export: Some(Box::new(|doc: &photocraft_doc::Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let mut opts = photocraft_io::ExportOptions::default();
            if let Some(q) = settings.jpeg_quality {
                opts.encode.jpeg_quality = q;
            }
            photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        write: Some(Box::new(|path: &str, bytes: &[u8]| photocraft_format::atomic_write(std::path::Path::new(path), bytes).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    let open = arg(&args, "--open");
    let safe_gpu = args.iter().any(|a| a == "--safe-gpu");
    // `--background-jobs`: long commands run as background jobs, as in the desktop app (#210).
    let background_jobs = args.iter().any(|a| a == "--background-jobs");
    // `--settle-ms N`: keep rendering frames for N ms before the capture (e.g. mid-job).
    let settle_ms: u64 = arg(&args, "--settle-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(w, h)).with_pixels_per_point(scale).with_max_steps(64).wgpu().build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
            app.background_jobs = background_jobs;
            // `--safe-gpu`: the CPU canvas, as the desktop app's `--safe-gpu` launch.
            if safe_gpu {
                app.perf.gpu_info.selected = "cpu".into();
                app.perf.gpu_info.canvas = "cpu".into();
                app.perf.gpu_info.fallback = Some("--safe-gpu: CPU renderer for this launch".into());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
                }
            } else if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
            }
            if let Some(path) = &open {
                app.open_path(path).expect("open --open file");
            }
            app
        });
    if let Some((mw, mh)) = arg(&args, "--monitor").and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))) {
        let top: f32 = arg(&args, "--window-top").and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let v = harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        v.monitor_size = Some(egui::vec2(mw, mh));
        v.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, top), egui::vec2(w, h)));
        v.maximized = Some(false);
    }
    harness.run_steps(4);
    let ctx = harness.ctx.clone();
    let timing = std::env::var_os("SNAPSHOT_TIMING").is_some();
    for (method, params) in script {
        let t0 = std::time::Instant::now();
        let label = format!("{method} {}", params.get("command").and_then(Value::as_str).unwrap_or(""));
        let (req, _rx) = ControlRequest::new(&method, params);
        let outcome = handle(harness.state_mut(), &ctx, &req);
        if let Outcome::Done(v) = &outcome
            && v.get("ok") == Some(&Value::Bool(false))
        {
            eprintln!("{method}: {v}");
        }
        // The harness doesn't call raw_input_hook: feed queued synthetic input step by step.
        loop {
            let step = harness.state_mut().take_synthetic_step();
            if step.is_empty() {
                break;
            }
            for e in step {
                harness.event(e);
            }
            harness.step();
        }
        harness.run_steps(4);
        if timing {
            eprintln!("{:>8.1} ms  {label}", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let t_settle = std::time::Instant::now();
    while t_settle.elapsed() < std::time::Duration::from_millis(settle_ms) {
        harness.step();
    }
    // Let fade animations settle.
    for _ in 0..12 {
        harness.step();
    }
    let (req, _rx) = ControlRequest::new("ui.inspect", Value::Null);
    if let Outcome::Done(v) = handle(harness.state_mut(), &ctx, &req) {
        println!("perf: {}", v["result"]["perf"]["timings"]);
    }
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    println!("wrote {out} ({}×{})", img.width(), img.height());
}

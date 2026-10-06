//! Photocraft desktop app.
//!
//! Usage: `photocraft [--control <port>] [--control-token <64-hex> |
//! --control-token-file <path>] [--automation-read-root <dir>]
//! [--automation-write-root <dir>] [--safe-gpu] [files…]`
//!
//! `--safe-gpu` starts with the CPU renderer (no GPU canvas; a software adapter for the window
//! where the platform has one) for this launch, e.g. after a graphics driver crash. A start that
//! crashes inside the driver also falls back by itself next time (see `gpu_startup`).
//!
//! `--control <port>` (or `PHOTOCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server.
//! The first line must authenticate; subsequent request lines get reply lines.
//! `{"id":1,"ok":true,"result":…}`. See `photocraft_ui_egui::control` for the methods.

// Release builds on Windows are GUI-subsystem apps, so launching from the Start Menu or Explorer
// doesn't open a console window. (`--version` output then only shows when redirected.)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app_dirs;
mod app_icon;
#[cfg(target_os = "macos")]
mod apple_events;
mod control_server;
mod crash_guard;
mod gpu_startup;
// Pure logic is tested on every platform; only Linux runs the check.
#[cfg(any(target_os = "linux", test))]
mod linux_libs;
mod monitor_profile;
mod services;
// Windows gets pen pressure from winit (WM_POINTER); the web runner has its own listener.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
mod tablet;

use photocraft_engine::Session;
use photocraft_ui_egui::PhotocraftApp;

/// Matches the `.desktop` file and hicolor icon name, so Wayland docks pick up the icon.
const APP_ID: &str = "ai.storyteller.photocraft";

fn main() -> eframe::Result {
    crash_guard::install_hook();
    let mut control_port: Option<u16> = std::env::var("PHOTOCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok());
    let mut control_token = None;
    let mut control_token_file = None;
    let mut automation_read_root = std::env::var_os("PHOTOCRAFT_AUTOMATION_READ_ROOT").map(std::path::PathBuf::from);
    let mut automation_write_root = std::env::var_os("PHOTOCRAFT_AUTOMATION_WRITE_ROOT").map(std::path::PathBuf::from);
    let mut files = Vec::new();
    let mut safe_gpu = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = args.next().and_then(|p| p.parse().ok()),
            "--control-token" => control_token = args.next(),
            "--control-token-file" => control_token_file = args.next().map(std::path::PathBuf::from),
            "--automation-read-root" => automation_read_root = args.next().map(std::path::PathBuf::from),
            "--automation-write-root" => automation_write_root = args.next().map(std::path::PathBuf::from),
            "--safe-gpu" => safe_gpu = true,
            "--version" => {
                println!("photocraft {}", photocraft_engine::build_info::long_version());
                return Ok(());
            }
            // Old macOS passes a process serial number when launched from Finder.
            _ if a.starts_with("-psn_") => {}
            _ => files.push(a),
        }
    }

    // winit and wgpu dlopen the windowing and GPU libraries, and some of those crates panic when
    // one is missing (issue #201). Name the package to install and exit instead.
    #[cfg(target_os = "linux")]
    if let Err(message) = linux_libs::preflight() {
        eprint!("{message}");
        std::process::exit(1);
    }

    let control = if let Some(port) = control_port {
        let (supplied, token_file) = photocraft_automation::security::token_inputs(control_token, control_token_file);
        let token = match photocraft_automation::security::server_token(supplied.as_deref(), token_file.as_deref()) {
            Ok(token) => token,
            Err(e) => {
                eprintln!("photocraft: cannot configure control authentication: {e}");
                return Ok(());
            }
        };
        if let Some(path) = token_file {
            eprintln!("photocraft: control token file: {}", path.display());
        } else if supplied.is_none() {
            eprintln!("photocraft: control token: {token}");
        } else {
            eprintln!("photocraft: using supplied control token");
        }
        let workspace = match photocraft_automation::AuthorizedWorkspace::new(automation_read_root.as_deref(), automation_write_root.as_deref()) {
            Ok(workspace) => workspace,
            Err(error) => {
                eprintln!("photocraft: cannot configure automation workspace: {error}");
                return Ok(());
            }
        };
        Some((port, token, workspace))
    } else {
        None
    };
    // Finder / Dock / Open With deliver files as Apple events, not arguments; catch the one that
    // launched us as well as later ones. Lives until the event loop returns.
    #[cfg(target_os = "macos")]
    let apple_events = apple_events::AppleEvents::install();
    #[cfg(target_os = "macos")]
    let apple_events = &apple_events;

    // Pen tablet samples on macOS (AppKit event monitor, before winit sees each event) and X11
    // (started once eframe says which display server it is on). The monitor lives until the event
    // loop returns.
    let stylus_feed = photocraft_ui_egui::stylus::StylusFeed::default();
    #[cfg(target_os = "macos")]
    let _tablet = tablet::install_macos(&stylus_feed);

    // Read the main display's ICC profile while the window opens (colour-managed canvas).
    let monitor = monitor_profile::detect_async();
    // Brush presets load in the background; the app attaches them when they arrive.
    let presets = services::presets_dir().map(photocraft_engine::preset_store::open_dir_async);
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(app_icon::window_icon())
            .with_app_id(APP_ID)
            .with_title("PhotoCraft")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true)
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    // Crash-safe GPU startup (#4): pick the backend (a marker left by a start that died in the
    // driver moves to a safer one), and lock this start's marker until the first frames render.
    let t_sentinel = std::time::Instant::now();
    let os = gpu_startup::Os::current();
    let (pref, _) = gpu_startup::read_prefs(services::prefs_file().as_deref());
    let (previous, sentinel) = match services::config_dir() {
        Some(dir) => gpu_startup::Sentinel::begin(&dir),
        None => (gpu_startup::Previous::Clean, None),
    };
    let env_backend = std::env::var("WGPU_BACKEND").ok();
    let plan = gpu_startup::plan(pref, previous.crashed(), env_backend.as_deref(), safe_gpu, os);
    if let Some(m) = previous.crashed() {
        log::warn!("the previous start didn't finish (GPU backend {}, adapter {:?}); {}", m.backend, m.adapter, plan.reason.as_deref().unwrap_or(""));
    }
    let sentinel: gpu_startup::SharedSentinel = std::sync::Arc::new(std::sync::Mutex::new(sentinel));
    if let Some(s) = sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
        let backend = match &plan.env {
            Some(v) => format!("env:{v}"),
            None => plan.backend.name().to_string(),
        };
        let marker = gpu_startup::Marker { backend, version: photocraft_engine::build_info::long_version().to_string(), ..Default::default() };
        if let Err(e) = s.write(marker) {
            log::warn!("GPU startup marker: {e}");
        }
    }
    let gpu_note: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
    // The adapter's real texture limits (egui asks for 8192 px), so big documents stay on the GPU.
    gpu_startup::configure(&mut options.wgpu_options.wgpu_setup, &plan, os, sentinel.clone(), gpu_note.clone());
    let sentinel_ms = t_sentinel.elapsed().as_secs_f64() * 1000.0;
    log::info!("GPU startup: {:?} ({sentinel_ms:.2} ms)", plan);
    let started_sentinel = sentinel.clone();
    let result = eframe::run_native(
        "Photocraft",
        options,
        Box::new(move |cc| {
            let automation = control.as_ref().map(|(_, _, workspace)| workspace.clone());
            let mut services = services::native(automation);
            services.preset_store = presets;
            let mut app = PhotocraftApp::new(Session::new(), services);
            app.integrated_titlebar = cfg!(target_os = "macos");
            // Long commands and file opens run as background jobs with progress and Cancel (#210).
            app.background_jobs = std::env::var_os("PHOTOCRAFT_INLINE_JOBS").is_none();
            if let Ok(Some(icc)) = monitor.recv_timeout(std::time::Duration::from_secs(2)) {
                app.session.color.monitor_profile = Some(std::sync::Arc::new(icc));
            }
            // Preferences › Performance › Use Graphics Processor (and the GPU backend: `cpu`
            // composites on the CPU).
            let info = &mut app.perf.gpu_info;
            info.preference = pref.name().to_string();
            info.selected = if plan.env.is_some() { "env".into() } else { plan.backend.name().to_string() };
            info.fallback = plan.reason.clone().or_else(|| gpu_note.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
            info.canvas = "cpu".into();
            if let Some(rs) = cc.wgpu_render_state.clone() {
                app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
                if plan.backend != photocraft_engine::prefs::GpuBackend::Cpu
                    && std::env::var_os("PHOTOCRAFT_CPU_CANVAS").is_none()
                    && app.session.prefs().performance.use_gpu
                {
                    app.set_wgpu(rs);
                } else {
                    // The window still draws with wgpu: record its errors instead of panicking.
                    let _ = photocraft_ui_egui::gpu_canvas::DeviceHealth::watch(&rs.device);
                }
            }
            app.perf.span("gpuSentinel", sentinel_ms);
            // Once the first frames rendered: clear the marker, and keep a crash fallback.
            let remember = plan.remember.then_some(plan.backend);
            app.on_started(move |app| {
                if let Some(s) = started_sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                    s.finish();
                }
                if let Some(b) = remember
                    && app.session.prefs().performance.gpu_backend != b
                    && let Err(e) = app.run("prefs.set", serde_json::json!({"path": "performance.gpuBackend", "value": b.name()}))
                {
                    log::warn!("couldn't remember GPU backend {}: {e}", b.name());
                }
            });
            if let Some((port, token, _)) = control {
                let rx = control_server::start(port, token, cc.egui_ctx.clone());
                app = app.with_control(rx);
            }
            #[cfg(target_os = "macos")]
            {
                app.services.os_events = Some(apple_events.connect(&cc.egui_ctx));
            }
            // Tablet pressure/tilt/eraser (winit drops them): the macOS monitor installed above
            // and the X11 reader write into this feed.
            app.stylus.feed = stylus_feed;
            #[cfg(target_os = "linux")]
            tablet::spawn_x11(&app.stylus.feed, tablet::DisplayKind::of(cc));
            // Paths on the command line (Linux/Windows file associations, `photocraft a.psd`).
            app.open_paths(&files);
            // Portable marker found but its data folder isn't writable (#228): say where settings went.
            if let Some(w) = &app_dirs::current().warning {
                photocraft_ui_egui::notices::post(&mut app, "Portable mode is off", vec![w.clone()], false);
            }
            Ok(Box::new(app))
        }),
    );
    // Closed before the first frames rendered: not a driver crash. (A start that failed to
    // create its device keeps the marker, so the next one tries a safer backend.)
    if result.is_ok()
        && let Some(s) = sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    {
        s.finish();
    }
    result
}

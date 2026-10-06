use super::*;
use crate::{Services, menus, notices};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::Document;
use photocraft_geom::Size;
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;

/// Writes recorded by the fake writer: (path, bytes).
type Written = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

/// An app whose importer names the document after the file (like `photocraft-io`) and reports a
/// warning for names containing "warn", and fails for bytes "bad"; the exporter warns for ".png"
/// and the writer records what it wrote.
fn app_with(pick_open: Option<(String, Vec<u8>)>, pick_save: Option<String>) -> (PhotocraftApp, Written) {
    let written: Written = Rc::default();
    let w = written.clone();
    let mut pick_open = pick_open;
    let services = Services {
        import: Some(Box::new(|name: &str, bytes: &[u8]| {
            if bytes == b"bad" {
                return Err("not an image".into());
            }
            let warnings = if name.contains("warn") { vec!["Adjustment layer \"Curves 1\" was flattened".to_string()] } else { Vec::new() };
            Ok((Document::new(name, Size::new(4, 4), ColorMode::Rgb, SampleType::U8), warnings))
        })),
        export: Some(Box::new(|_doc: &Document, path: &str, _s: &crate::ExportSettings| {
            let warnings = if path.ends_with(".png") { vec!["Layers were flattened".to_string()] } else { Vec::new() };
            Ok((b"out".to_vec(), warnings))
        })),
        pick_open: Some(Box::new(move || pick_open.take())),
        pick_save: Some(Box::new(move |_s: &str| pick_save.clone())),
        write: Some(Box::new(move |p: &str, b: &[u8]| {
            w.borrow_mut().push((p.to_string(), b.to_vec()));
            Ok(())
        })),
        ..Default::default()
    };
    (PhotocraftApp::new(photocraft_engine::Session::new(), services), written)
}

#[test]
fn display_name_is_the_file_name() {
    assert_eq!(display_name("/a/b/photo.psd"), "photo.psd");
    assert_eq!(display_name("photo.psd"), "photo.psd");
    assert_eq!(display_name(""), "");
    assert_eq!(display_name("/"), "/");
}

#[test]
fn open_file_sets_name_path_and_recent() {
    let (mut app, _) = app_with(None, None);
    let w = app.open_file("/pics/cat.psd", b"x").unwrap();
    assert!(w.is_empty());
    let st = app.session.active().unwrap();
    assert_eq!(st.doc.name, "cat.psd");
    assert_eq!(st.path.as_deref(), Some("/pics/cat.psd"));
    assert_eq!(app.ui.recent_files, vec!["/pics/cat.psd".to_string()]);
    assert!(!app.ui.status_error);
    assert!(app.ui.notices.is_empty());
}

#[test]
fn file_open_dialog_sets_path_so_save_writes_in_place() {
    // The dialog returns a full path: the document is named after the file, not the path.
    let (mut app, written) = app_with(Some(("/pics/cat.psd".into(), b"x".to_vec())), None);
    let ctx = egui::Context::default();
    menus::invoke(&mut app, &ctx, "file.open", json!({})).unwrap();
    let st = app.session.active().unwrap();
    assert_eq!(st.doc.name, "cat.psd");
    assert_eq!(st.path.as_deref(), Some("/pics/cat.psd"));
    assert_eq!(app.ui.recent_files.first().map(String::as_str), Some("/pics/cat.psd"));
    // File › Save goes straight back to the file (pick_save would return None = cancelled).
    let r = menus::invoke(&mut app, &ctx, "file.save", json!({})).unwrap();
    assert_eq!(r["path"], "/pics/cat.psd");
    assert_eq!(written.borrow().last().map(|(p, _)| p.clone()).as_deref(), Some("/pics/cat.psd"));
}

#[test]
fn pcraft_documents_save_in_place_but_flat_files_ask() {
    let (mut app, written) = app_with(None, None);
    let ctx = egui::Context::default();
    app.open_file("/pics/work.pcraft", b"x").unwrap();
    let r = menus::invoke(&mut app, &ctx, "file.save", json!({})).unwrap();
    assert_eq!(r["path"], "/pics/work.pcraft");
    assert_eq!(written.borrow().len(), 1);
    // A PNG goes through Save As (cancelled here), not silently flattened over the original.
    app.open_file("/pics/flat.png", b"x").unwrap();
    assert_eq!(menus::invoke(&mut app, &ctx, "file.save", json!({})).unwrap_err(), "cancelled");
    assert_eq!(written.borrow().len(), 1);
}

#[test]
fn import_warnings_reach_status_notice_and_control_response() {
    let dir = std::env::temp_dir().join(format!("photocraft-open-warn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("warn.psd");
    std::fs::write(&path, b"x").unwrap();
    let p = path.to_string_lossy().to_string();
    let (mut app, _) = app_with(None, None);
    let ctx = egui::Context::default();
    let r = menus::invoke(&mut app, &ctx, "file.open", json!({ "path": p })).unwrap();
    assert_eq!(r["warnings"][0], "Adjustment layer \"Curves 1\" was flattened");
    assert!(app.ui.status.contains("was flattened"), "{}", app.ui.status);
    assert!(app.ui.status_error);
    assert_eq!(app.ui.notices.len(), 1);
    assert!(!app.ui.notices[0].error);
    assert_eq!(app.ui.notices[0].lines.len(), 1);
    // The document still opened, with its path.
    assert_eq!(app.session.active().and_then(|s| s.path.clone()), Some(p));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_warnings_reach_status_notice_and_control_response() {
    let (mut app, _) = app_with(None, None);
    let ctx = egui::Context::default();
    app.open_file("/pics/cat.psd", b"x").unwrap();
    let r = menus::invoke(&mut app, &ctx, "file.saveAs", json!({ "path": "/pics/cat.png" })).unwrap();
    assert_eq!(r["path"], "/pics/cat.png");
    assert_eq!(r["warnings"][0], "Layers were flattened");
    assert!(app.ui.status.contains("Saved cat.png"), "{}", app.ui.status);
    assert_eq!(app.ui.notices.len(), 1);
    // No warnings: no notice, and the response says so.
    let r = menus::invoke(&mut app, &ctx, "file.saveAs", json!({ "path": "/pics/cat.psd" })).unwrap();
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(app.ui.notices.len(), 1);
}

#[test]
fn open_failures_are_errors_and_leave_no_document() {
    let (mut app, _) = app_with(Some(("/pics/broken.psd".into(), b"bad".to_vec())), None);
    let ctx = egui::Context::default();
    menus::invoke(&mut app, &ctx, "file.open", json!({})).unwrap();
    assert!(app.session.documents().is_empty());
    assert!(app.ui.recent_files.is_empty());
    assert!(app.ui.status_error);
    assert!(app.ui.status.starts_with("Couldn't open broken.psd"), "{}", app.ui.status);
    assert!(app.ui.notices.last().is_some_and(|n| n.error));
}

#[test]
fn open_paths_reports_each_failure_without_panicking() {
    let (mut app, _) = app_with(None, None);
    let missing = std::env::temp_dir().join("photocraft-definitely-missing-file.psd").to_string_lossy().to_string();
    let dir = std::env::temp_dir().to_string_lossy().to_string();
    let n = app.open_paths(&[missing, String::new(), dir, "\u{0}".into()]);
    assert_eq!(n, 0);
    assert!(app.session.documents().is_empty());
    assert!(app.ui.status_error);
    assert_eq!(app.ui.notices.len(), notices::MAX_NOTICES);
    // Bad control params error instead of panicking.
    let ctx = egui::Context::default();
    assert!(menus::invoke(&mut app, &ctx, "file.open", json!({ "path": "" })).is_err());
}

#[test]
fn os_open_events_open_files_with_paths() {
    let dir = std::env::temp_dir().join(format!("photocraft-os-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.psd");
    let b = dir.join("b.png");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();
    let paths: Vec<String> = [&a, &b].iter().map(|p| p.to_string_lossy().to_string()).collect();
    let (mut app, _) = app_with(None, None);
    let mut queue = vec![OsEvent::Open(paths.clone())];
    app.services.os_events = Some(Box::new(move || std::mem::take(&mut queue)));
    let ctx = egui::Context::default();
    app.drain_os_events(&ctx);
    assert_eq!(app.session.documents().len(), 2);
    assert_eq!(app.session.active().and_then(|s| s.path.clone()).as_deref(), Some(paths[1].as_str()));
    assert_eq!(app.ui.recent_files, vec![paths[1].clone(), paths[0].clone()]);
    // Drained: a second poll opens nothing more.
    app.drain_os_events(&ctx);
    assert_eq!(app.session.documents().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn os_quit_and_empty_events_do_not_panic() {
    let (mut app, _) = app_with(None, None);
    let mut queue = vec![OsEvent::Open(Vec::new()), OsEvent::Quit];
    app.services.os_events = Some(Box::new(move || std::mem::take(&mut queue)));
    app.drain_os_events(&egui::Context::default());
    assert!(app.session.documents().is_empty());
}

#[derive(Debug)]
struct FakeDrop(std::path::PathBuf, Result<Vec<u8>, String>);

impl egui::DroppedFile for FakeDrop {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        self.1.clone()
    }
}

#[test]
fn dropped_files_open_with_path_and_recent() {
    let (mut app, _) = app_with(None, None);
    let abs = std::env::temp_dir().join("dropped.psd");
    let abs_s = abs.to_string_lossy().to_string();
    app.open_dropped(vec![
        std::sync::Arc::new(FakeDrop(abs, Ok(b"x".to_vec()))),
        // Unreadable and undecodable drops are errors; no path means no recent entry.
        std::sync::Arc::new(FakeDrop("gone.psd".into(), Err("permission denied".into()))),
        std::sync::Arc::new(FakeDrop("rel.psd".into(), Ok(b"x".to_vec()))),
        std::sync::Arc::new(FakeDrop("".into(), Ok(b"bad".to_vec()))),
    ]);
    assert_eq!(app.session.documents().len(), 2);
    assert_eq!(app.session.documents()[0].path.as_deref(), Some(abs_s.as_str()));
    assert_eq!(app.session.documents()[0].doc.name, "dropped.psd");
    assert_eq!(app.session.documents()[1].path, None);
    assert_eq!(app.ui.recent_files, vec![abs_s]);
    assert!(app.ui.status_error);
    assert_eq!(app.ui.notices.iter().filter(|n| n.error).count(), 2);
}

#[test]
fn notices_are_capped_and_dismissable_state_round_trips() {
    let (mut app, _) = app_with(None, None);
    for i in 0..10 {
        notices::post(&mut app, format!("n{i}"), vec![], false);
    }
    assert_eq!(app.ui.notices.len(), notices::MAX_NOTICES);
    assert_eq!(app.ui.notices.last().map(|n| n.title.as_str()), Some("n9"));
    // Notices are part of the serialized UI state (ui.inspect / ui.set).
    let v = serde_json::to_value(&app.ui).unwrap();
    assert_eq!(v["notices"].as_array().map(Vec::len), Some(notices::MAX_NOTICES));
    let back: crate::UiState = serde_json::from_value(v).unwrap();
    assert_eq!(back.notices, app.ui.notices);
}

#[test]
fn notices_render_without_panicking() {
    let (mut app, _) = app_with(None, None);
    notices::post(&mut app, "Opened a.psd with 9 warnings", (0..9).map(|i| format!("warning {i}")).collect(), false);
    notices::error(&mut app, "Couldn't open b.psd: not an image".into());
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(Default::default(), |ui| notices::show(&mut app, ui.ctx()));
    out.textures_delta.clear();
    assert_eq!(app.ui.notices.len(), 2);
}

//! UI tests for background jobs (#210): the whole app (eframe harness, CPU canvas) keeps
//! rendering frames while a slow job runs, shows the status-bar progress and the modal progress
//! dialog, and Esc / Cancel leave the document unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_engine::jobs::{JobId, Started};
use serde_json::{Value, json};

use crate::PhotocraftApp;

fn app_harness() -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(8).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 320, "height": 240})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("edit.fill", json!({"color": "#3366cc"})).unwrap();
        let mut app = PhotocraftApp::new(s, crate::Services::default());
        app.background_jobs = true;
        app
    });
    h.run_steps(4);
    h
}

/// A fake document job that runs until `gate` opens (or it is cancelled), reporting 40 %.
fn slow_job(app: &mut PhotocraftApp, gate: &Arc<AtomicBool>) -> JobId {
    let gate = gate.clone();
    let r = app
        .session
        .start_job(
            "test.slow",
            json!({}),
            "Slow Filter",
            true,
            move |ctx| {
                ctx.progress(0.4, "Filtering tiles");
                while !gate.load(Ordering::Relaxed) {
                    ctx.check()?;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
            |s, ()| {
                s.edit("Slow Filter", |_, _| Ok(()))?;
                Ok(json!({"slow": true}))
            },
        )
        .unwrap();
    match r {
        Started::Job(id) => id,
        Started::Done(v) => panic!("ran inline: {v}"),
    }
}

/// Step frames for `ms` of wall time; returns (frames, slowest frame in ms).
fn frames_for(h: &mut Harness<'_, PhotocraftApp>, ms: u64) -> (u64, f64) {
    let t = Instant::now();
    let (mut n, mut worst) = (0, 0.0f64);
    while t.elapsed() < Duration::from_millis(ms) {
        let f = Instant::now();
        h.step();
        worst = worst.max(f.elapsed().as_secs_f64() * 1000.0);
        n += 1;
    }
    (n, worst)
}

#[test]
fn the_frame_loop_keeps_running_during_a_slow_job_and_esc_cancels_it() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let doc_before = h.state().session.active().unwrap().doc.clone();
    let job = slow_job(h.state_mut(), &gate);
    let frame0 = h.state().frame;
    let (n, worst) = frames_for(&mut h, 400);
    assert!(h.state().frame - frame0 >= 10, "the UI kept rendering ({n} frames)");
    eprintln!("{n} frames during the job, slowest {worst:.1} ms");
    // The status bar shows the job, and after the delay the modal progress dialog.
    assert!(h.query_all_by_label("Slow Filter").count() >= 2, "job label in the status bar and the dialog title");
    assert!(h.query_by_label("Cancel").is_some(), "the progress dialog's Cancel");
    assert!(h.query_by_label("Filtering tiles").is_some(), "the job's status message");
    // Commands that would edit the busy document are disabled with a clear reason.
    assert!(!h.state().session.is_enabled("layer.new.layer"));
    let err = h.state_mut().run("layer.new.layer", json!({})).unwrap_err();
    assert!(err.contains("Slow Filter"), "{err}");
    // Esc cancels; the document is untouched.
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().session.job(job).is_none());
    assert!(Arc::ptr_eq(&h.state().session.active().unwrap().doc, &doc_before));
    assert!(h.state().ui.status.contains("Cancelled"), "{}", h.state().ui.status);
    assert!(h.query_by_label("Esc to cancel").is_none(), "the dialog closed");
}

#[test]
fn a_finished_job_lands_as_one_undo_step_and_the_dialog_cancel_button_works() {
    let mut h = app_harness();
    let gate = Arc::new(AtomicBool::new(false));
    let steps = h.state().session.active().unwrap().history.past_len();
    slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 320);
    gate.store(true, Ordering::Relaxed);
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(10) {
        h.step();
    }
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);

    // A second job, cancelled from the dialog's button.
    let gate = Arc::new(AtomicBool::new(false));
    let job = slow_job(h.state_mut(), &gate);
    frames_for(&mut h, 320);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().session.job(job).is_none());
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);
}

#[test]
fn filters_from_the_ui_run_in_the_background() {
    let mut h = app_harness();
    let steps = h.state().session.active().unwrap().history.past_len();
    let r = h.state_mut().run("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    assert_eq!(r["pending"], true, "{r}");
    let t = Instant::now();
    while h.state().session.has_jobs() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.history.past_len(), steps + 1, "one step for the blur");
    assert_eq!(h.state().ui.status, "Gaussian Blur");
}

#[test]
fn control_requests_wait_for_jobs_unless_asked_not_to() {
    let mut h = app_harness();
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::ControlRequest::new("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 2}}));
    let job = match crate::control::handle(h.state_mut(), &ctx, &req) {
        crate::control::Outcome::AfterJob(job) => job,
        _ => panic!("expected a deferred reply"),
    };
    assert!(h.state().session.jobs().iter().any(|j| j.id == job) || h.state().session.jobs_with_recent().iter().any(|j| j.id == job));
    // The app's frame loop answers the request when the job lands.
    let (req, rx) = crate::ControlRequest::new("engine.execute", json!({"command": "jobs.list"}));
    let _ = crate::control::handle(h.state_mut(), &ctx, &req);
    drop(rx);
    let (tx, rx) = std::sync::mpsc::channel();
    h.state_mut().jobs.waiters.push((job, tx));
    let t = Instant::now();
    let reply: Value = loop {
        h.step();
        if let Ok(v) = rx.try_recv() {
            break v;
        }
        assert!(t.elapsed() < Duration::from_secs(30));
    };
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["result"]["filter"].is_object(), "{reply}");

    // `wait: false` replies with the job id at once.
    let (req, _rx) = crate::ControlRequest::new("engine.execute", json!({"command": "filter.blur.gaussianBlur", "params": {"radius": 2}, "wait": false}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!("expected an immediate reply") };
    let id = v["result"]["job"].as_u64().unwrap();
    let (req, _rx) = crate::ControlRequest::new("jobs.cancel", json!({"job": id}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!() };
    assert_eq!(v["ok"], true, "{v}");
    let (req, _rx) = crate::ControlRequest::new("jobs.cancel", json!({"job": 99999}));
    let crate::control::Outcome::Done(v) = crate::control::handle(h.state_mut(), &ctx, &req) else { panic!() };
    assert_eq!(v["ok"], false);
}

fn psd_bytes() -> Vec<u8> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let doc = (*s.active().unwrap().doc).clone();
    photocraft_io::export(&doc, "x.psd", &Default::default()).unwrap().bytes
}

#[test]
fn files_open_in_the_background_with_a_tab_and_cancel_closes_it() {
    let mut h = app_harness();
    let bytes = psd_bytes();
    h.state_mut().open_file("/tmp/photocraft-test/bg.psd", &bytes).unwrap();
    // The tab is there before the document.
    assert_eq!(h.state().jobs.opens.len(), 1);
    assert_eq!(h.state().session.documents().len(), 1);
    let t = Instant::now();
    while !h.state().jobs.opens.is_empty() && t.elapsed() < Duration::from_secs(30) {
        h.step();
    }
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.doc.size.width, 64);
    assert_eq!(st.path.as_deref(), Some("/tmp/photocraft-test/bg.psd"));
    assert_eq!(h.state().ui.recent_files.first().map(String::as_str), Some("/tmp/photocraft-test/bg.psd"));

    // Cancel: the half-open tab closes, no document is added.
    h.state_mut().open_file("/tmp/photocraft-test/bg2.psd", &bytes).unwrap();
    let job = h.state().jobs.opens[0].job;
    crate::jobs_ui::cancel(h.state_mut(), job);
    h.run_steps(3);
    assert!(h.state().jobs.opens.is_empty());
    assert_eq!(h.state().session.documents().len(), 2);
    assert!(h.state().jobs.focus.is_none());
}

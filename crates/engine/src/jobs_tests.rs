use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::Session;

/// A document `w`×`h` with a textured pixel layer (so blurs change every pixel).
fn session(w: u32, h: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("pattern", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let r = photocraft_geom::Rect::new(0, 0, w as i32, h as i32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 7 + y * 13) % 31) as f32 / 30.0;
                px.extend_from_slice(&[v, 1.0 - v, ((x / 5 + y / 3) % 2) as f32, 1.0]);
            }
        }
        surf.write_region(r, &px);
        Ok(())
    })
    .unwrap();
    s
}

fn pixels(s: &Session) -> Vec<f32> {
    let d = s.active().unwrap();
    let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
    l.surface().unwrap().read_region(d.doc.bounds())
}

/// Poll until job `id` ends (or 60 s pass); returns its event.
fn wait_event(s: &mut Session, id: JobId) -> JobEvent {
    let t = Instant::now();
    loop {
        if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == id) {
            return e;
        }
        assert!(t.elapsed() < Duration::from_secs(60), "job {id:?} did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn job(r: Started) -> JobId {
    match r {
        Started::Job(id) => id,
        Started::Done(v) => panic!("expected a background job, ran inline: {v}"),
    }
}

#[test]
fn a_job_applies_as_one_undo_step_and_matches_the_inline_result() {
    let mut inline = session(300, 200);
    inline.execute("filter.blur.gaussianBlur", json!({"radius": 4})).unwrap();

    let mut s = session(300, 200);
    let before = pixels(&s);
    let steps = s.active().unwrap().history.past_len();
    let journal = s.journal.len();
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 4})).unwrap());
    // The canvas shows the pre-job state until the job is applied.
    assert_eq!(pixels(&s), before);
    let e = wait_event(&mut s, id);
    let JobOutcome::Done(v) = &e.outcome else { panic!("{e:?}") };
    assert!(v["filter"].is_object(), "{v}");
    assert_eq!(e.command, "filter.blur.gaussianBlur");
    assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "one undo step");
    assert_eq!(s.journal.len(), journal + 1, "journaled like execute");
    assert_eq!(s.journal.last().unwrap().0, "filter.blur.gaussianBlur");
    assert_eq!(pixels(&s), pixels(&inline), "same pixels as the synchronous command");
    assert!(s.undo());
    assert_eq!(pixels(&s), before);
    assert!(s.jobs().is_empty());
}

#[test]
fn cancel_leaves_the_document_byte_identical() {
    let mut s = session(1600, 1200);
    let doc_before = s.active().unwrap().doc.clone();
    let before = pixels(&s);
    let (rev, steps) = (s.active().unwrap().revision, s.active().unwrap().history.past_len());
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 60})).unwrap());
    assert!(s.cancel_job(id));
    let e = wait_event(&mut s, id);
    assert_eq!(e.outcome, JobOutcome::Cancelled);
    s.join_cancelled_jobs();
    // Nothing arrives later either.
    assert!(s.poll_jobs().is_empty());
    let st = s.active().unwrap();
    assert!(Arc::ptr_eq(&st.doc, &doc_before), "the document snapshot was not replaced");
    assert_eq!((st.revision, st.history.past_len()), (rev, steps));
    assert_eq!(pixels(&s), before);
    assert!(matches!(s.wait_job(id), Err(EngineError::Cancelled)));
}

#[test]
fn a_panic_in_the_worker_becomes_an_error_and_the_document_is_unchanged() {
    let mut s = session(64, 64);
    let doc_before = s.active().unwrap().doc.clone();
    let id = job(s
        .start_job(
            "test.panics",
            json!({}),
            "Panicking job",
            true,
            |_ctx| -> Result<()> {
                let v: Vec<u8> = Vec::new();
                // An out-of-bounds index: a panic inside the worker.
                #[allow(clippy::indexing_slicing)]
                let _ = v[usize::MAX / 2];
                Ok(())
            },
            |_s, ()| Ok(json!(null)),
        )
        .unwrap());
    let e = wait_event(&mut s, id);
    let JobOutcome::Failed(msg) = &e.outcome else { panic!("{e:?}") };
    assert!(msg.contains("internal error"), "{msg}");
    assert!(Arc::ptr_eq(&s.active().unwrap().doc, &doc_before));
    // The session still works.
    s.execute("filter.blur.gaussianBlur", json!({"radius": 1})).unwrap();
}

#[test]
fn progress_is_monotonic_and_reaches_one() {
    let ctx = JobCtx::new();
    ctx.progress(0.5, "half");
    ctx.progress(0.3, "");
    assert_eq!(ctx.fraction(), 0.5, "progress never goes backwards");
    assert_eq!(ctx.message(), "half");
    ctx.progress(f32::NAN, "");
    ctx.progress(7.0, "");
    assert_eq!(ctx.fraction(), 1.0);

    let mut s = session(2000, 1500);
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 30})).unwrap());
    let mut seen = Vec::new();
    let e = loop {
        if let Some(j) = s.jobs().into_iter().find(|j| j.id == id) {
            seen.push(j.progress);
            assert_eq!(j.label, "Gaussian Blur");
            assert_eq!(j.state, "running");
        }
        if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == id) {
            break e;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(e.outcome, JobOutcome::Done(_)), "{e:?}");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "monotonic: {seen:?}");
    let info = s.jobs_with_recent().into_iter().find(|j| j.id == id).unwrap();
    assert_eq!((info.state, info.progress), ("done", 1.0));
}

/// A fake document job that runs until `gate` opens (or it is cancelled), then inverts nothing
/// but records one undo step named "Gated Job".
pub(super) fn gated_job(s: &mut Session, gate: &Arc<std::sync::atomic::AtomicBool>) -> JobId {
    let gate = gate.clone();
    job(s
        .start_job(
            "test.gated",
            json!({}),
            "Gated Job",
            true,
            move |ctx| {
                while !gate.load(std::sync::atomic::Ordering::Relaxed) {
                    ctx.check()?;
                    ctx.progress(0.5, "waiting");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
            |s, ()| {
                s.edit("Gated Job", |_, _| Ok(()))?;
                Ok(json!({"gated": true}))
            },
        )
        .unwrap())
}

#[test]
fn a_running_job_locks_its_document_but_not_others() {
    let mut s = session(64, 64);
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let id = gated_job(&mut s, &gate);
    // Edits of the busy document are disabled with a message naming the job.
    let err = s.execute("layer.new.layer", json!({})).unwrap_err();
    assert!(matches!(&err, EngineError::Disabled(_, why) if why.contains("Gated Job")), "{err}");
    assert!(!s.is_enabled("layer.new.layer"));
    assert!(s.disabled_reason("filter.blur.gaussianBlur").unwrap().contains("still running"));
    assert!(!s.undo(), "undo waits for the job");
    // Queries and document-independent commands still run.
    s.execute("jobs.list", json!({})).unwrap();
    // A new document is free to edit while the first one is busy.
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let r = s.execute("jobs.list", json!({})).unwrap();
    assert_eq!(r["jobs"][0]["id"], json!(id.0));
    // The result applies to its own document, and the active document stays the new one.
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    let e = wait_event(&mut s, id);
    assert!(matches!(e.outcome, JobOutcome::Done(_)), "{e:?}");
    assert_eq!(s.active_index(), Some(1));
    assert_eq!(s.documents()[0].history.past_len(), 3);
}

#[test]
fn closing_the_document_cancels_its_job() {
    let mut s = session(1600, 1200);
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 80})).unwrap());
    s.close(0);
    let e = wait_event(&mut s, id);
    assert_eq!(e.outcome, JobOutcome::Cancelled);
}

#[test]
fn jobs_cancel_command_and_bad_params() {
    let mut s = session(1600, 1200);
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 80})).unwrap());
    assert!(s.execute("jobs.cancel", json!({"job": "x"})).is_err());
    assert!(s.execute("jobs.cancel", json!({"job": 999})).is_err());
    s.execute("jobs.cancel", json!({"job": id.0})).unwrap();
    assert_eq!(wait_event(&mut s, id).outcome, JobOutcome::Cancelled);
    assert_eq!(s.execute("jobs.cancel", json!({})).unwrap()["cancelled"], 0);
}

#[test]
fn non_job_commands_still_finish_inline_with_start() {
    let mut s = session(16, 16);
    let r = s.start("layer.new.layer", json!({})).unwrap();
    assert!(matches!(r, Started::Done(_)), "{r:?}");
    // Errors are reported as before.
    assert!(matches!(s.start("no.such.command", json!({})), Err(EngineError::UnknownCommand(_))));
}

#[test]
fn content_aware_fill_and_scale_run_as_jobs() {
    let mut inline = session(160, 120);
    let mut s = session(160, 120);
    for t in [&mut inline, &mut s] {
        t.execute("select.rect", json!({"x": 60, "y": 40, "width": 30, "height": 30})).unwrap();
    }
    inline.execute("edit.contentAwareFill", json!({})).unwrap();
    let id = job(s.start("edit.contentAwareFill", json!({})).unwrap());
    let e = wait_event(&mut s, id);
    assert!(matches!(e.outcome, JobOutcome::Done(_)), "{e:?}");
    assert_eq!(pixels(&s), pixels(&inline), "same fill as the synchronous command");

    s.execute("select.deselect", json!({})).unwrap();
    let id = job(s.start("edit.contentAwareScale", json!({"width": 120})).unwrap());
    let e = wait_event(&mut s, id);
    let JobOutcome::Done(v) = &e.outcome else { panic!("{e:?}") };
    assert_eq!(v["to"][0], 120);
}

#[test]
fn open_runs_as_a_job_and_cancel_adds_nothing() {
    let mut src = session(40, 30);
    let doc = (*src.active().unwrap().doc).clone();
    let bytes = photocraft_io::export(&doc, "x.psd", &Default::default()).unwrap().bytes;
    src.close(0);
    let mut s = Session::new();
    let id = job(s.start_open("x.psd", OpenSource::Bytes(Arc::new(bytes.clone()))).unwrap());
    let e = wait_event(&mut s, id);
    let JobOutcome::Done(v) = &e.outcome else { panic!("{e:?}") };
    assert_eq!(e.command, OPEN_JOB);
    assert_eq!(v["document"], 0);
    assert_eq!(s.documents().len(), 1);
    assert_eq!(s.documents()[0].doc.size.width, 40);

    let id = job(s.start_open("y.psd", OpenSource::Bytes(Arc::new(bytes))).unwrap());
    s.cancel_job(id);
    assert_eq!(wait_event(&mut s, id).outcome, JobOutcome::Cancelled);
    assert_eq!(s.documents().len(), 1, "a cancelled open adds no document");

    let id = job(s.start_open("missing.psd", OpenSource::Path("/no/such/file.psd".into())).unwrap());
    assert!(matches!(wait_event(&mut s, id).outcome, JobOutcome::Failed(_)));
}

#[test]
fn wait_job_blocks_until_applied() {
    let mut s = session(400, 300);
    let id = job(s.start("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap());
    let v = s.wait_job(id).unwrap();
    assert!(v["filter"].is_object());
    assert_eq!(s.active().unwrap().history.past_len(), 3);
    // Waiting again reports the recorded result.
    assert_eq!(s.wait_job(id).unwrap(), v);
    assert!(s.wait_job(JobId(12345)).is_err());
}

/// Cancel latency on a 24 MP document (6000×4000): from `cancel_job` until the worker thread
/// has exited. Release only: debug builds are ~20× slower per tile.
#[test]
// Wall-clock timing depends on machine load (the release corpus job runs every test), so it is
// opt-in; perf_scenarios tracks the same numbers as P25/P49.
#[ignore = "timing: cargo test --release -p photocraft-engine --lib cancel_takes -- --ignored"]
fn cancel_takes_effect_within_200_ms_on_24_mp() {
    for (cmd, params, select) in [("filter.blur.gaussianBlur", json!({"radius": 50}), false), ("edit.contentAwareFill", json!({}), true)] {
        let mut s = session(6000, 4000);
        if select {
            s.execute("select.rect", json!({"x": 2000, "y": 1500, "width": 1200, "height": 900})).unwrap();
        }
        let doc_before = s.active().unwrap().doc.clone();
        // Cancel at several points of the run (early, middle, late stages).
        let mut worst: f64 = 0.0;
        for after_ms in [20, 150, 400, 600, 800, 1000, 1300] {
            let id = job(s.start(cmd, params.clone()).unwrap());
            std::thread::sleep(Duration::from_millis(after_ms));
            if s.jobs().iter().all(|j| j.id != id) || s.jobs().iter().any(|j| j.id == id && j.progress >= 1.0) {
                let _ = wait_event(&mut s, id);
                s.undo();
                continue;
            }
            let at = s.jobs().iter().find(|j| j.id == id).map(|j| j.progress).unwrap_or(0.0);
            let t = Instant::now();
            assert!(s.cancel_job(id));
            s.join_cancelled_jobs();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("{cmd}: cancelled after {after_ms} ms (at {:.0} %), latency {ms:.1} ms", at * 100.0);
            worst = worst.max(ms);
            assert_eq!(wait_event(&mut s, id).outcome, JobOutcome::Cancelled);
            assert!(Arc::ptr_eq(&s.active().unwrap().doc, &doc_before));
        }
        eprintln!("{cmd}: worst cancel latency {worst:.1} ms");
        assert!(worst < 200.0, "{cmd}: cancel took {worst:.1} ms");
    }
}

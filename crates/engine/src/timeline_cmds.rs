//! Window › Timeline: create and drive the document timeline (playhead over N frames at a frame
//! rate). Navigation doesn't add history steps. Video layers (Layer › Video Layers, added later)
//! read `current` to show their frame. Headless and scriptable so agents can render frame ranges.

use std::sync::Arc;

use photocraft_doc::Timeline;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

fn has_timeline(s: &Session) -> std::result::Result<(), String> {
    match s.active() {
        Some(d) if d.doc.timeline.is_some() => Ok(()),
        Some(_) => Err("no timeline (create one first)".into()),
        None => Err("no document".into()),
    }
}

/// Mutate the document timeline without a history step (navigation / metadata).
fn with_timeline(s: &mut Session, f: impl FnOnce(&mut Option<Timeline>)) -> Result<Value> {
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    let mut doc = (*st.doc).clone();
    f(&mut doc.timeline);
    if let Some(t) = &mut doc.timeline {
        t.clamp();
    }
    crate::video_cmds::sync(&mut doc);
    st.doc = Arc::new(doc);
    st.revision += 1;
    info(s)
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let duration = p.get("duration").and_then(Value::as_u64).unwrap_or(30) as usize;
    let fps = p.get("fps").and_then(Value::as_f64).unwrap_or(30.0) as f32;
    with_timeline(s, |t| *t = Some(Timeline::new(duration, fps)))
}

fn delete(s: &mut Session) -> Result<Value> {
    with_timeline(s, |t| *t = None)
}

fn set_frame(s: &mut Session, p: &Value) -> Result<Value> {
    let f =
        p.get("frame").and_then(Value::as_u64).ok_or_else(|| EngineError::BadParams { cmd: "timeline.setFrame".into(), msg: "need `frame`".into() })? as usize;
    with_timeline(s, |t| {
        if let Some(t) = t {
            t.current = f;
        }
    })
}

fn step(s: &mut Session, delta: i64) -> Result<Value> {
    with_timeline(s, |t| {
        if let Some(t) = t {
            t.current = (t.current as i64 + delta).clamp(0, t.duration as i64 - 1) as usize;
        }
    })
}

fn set_props(s: &mut Session, p: &Value) -> Result<Value> {
    with_timeline(s, |t| {
        if let Some(t) = t {
            if let Some(fps) = p.get("fps").and_then(Value::as_f64) {
                t.fps = fps as f32;
            }
            if let Some(d) = p.get("duration").and_then(Value::as_u64) {
                t.duration = d as usize;
            }
            if let Some(ws) = p.get("workStart").and_then(Value::as_u64) {
                t.work_start = ws as usize;
            }
            if let Some(we) = p.get("workEnd").and_then(Value::as_u64) {
                t.work_end = we as usize;
            }
        }
    })
}

fn info(s: &mut Session) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(match &st.doc.timeline {
        Some(t) => {
            json!({"timeline": {"fps": t.fps, "duration": t.duration, "current": t.current, "workStart": t.work_start, "workEnd": t.work_end, "time": t.time()}})
        }
        None => json!({"timeline": null}),
    })
}

macro_rules! spec {
    ($id:expr, $label:expr, $params:expr, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $en, journal: false, run: $run }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!("timeline.create", "Create Video Timeline", r#"{duration?:30, fps?:30} → {timeline}"#, has_doc, |s, p| create(s, p)),
        spec!("timeline.delete", "Delete Timeline", "{} → {timeline:null}", has_timeline, |s, _| delete(s)),
        spec!("timeline.setFrame", "Go to Frame", r#"{frame} → {timeline}"#, has_timeline, |s, p| set_frame(s, p)),
        spec!("timeline.nextFrame", "Next Frame", "{} → {timeline}", has_timeline, |s, _| step(s, 1)),
        spec!("timeline.previousFrame", "Previous Frame", "{} → {timeline}", has_timeline, |s, _| step(s, -1)),
        spec!("timeline.setProps", "Timeline Settings", r#"{fps?, duration?, workStart?, workEnd?} → {timeline}"#, has_timeline, |s, p| set_props(s, p)),
        spec!("timeline.info", "Timeline Info", "{} → {timeline}", has_doc, |s, _| info(s)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_navigate_delete() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        assert_eq!(s.execute("timeline.info", json!({})).unwrap()["timeline"], Value::Null);
        let r = s.execute("timeline.create", json!({"duration": 24, "fps": 24.0})).unwrap();
        assert_eq!(r["timeline"]["duration"], 24);
        assert_eq!(r["timeline"]["current"], 0);
        // Navigate.
        s.execute("timeline.setFrame", json!({"frame": 10})).unwrap();
        s.execute("timeline.nextFrame", json!({})).unwrap();
        assert_eq!(s.execute("timeline.info", json!({})).unwrap()["timeline"]["current"], 11);
        // Clamp beyond the end.
        let r = s.execute("timeline.setFrame", json!({"frame": 999})).unwrap();
        assert_eq!(r["timeline"]["current"], 23);
        // Props.
        s.execute("timeline.setProps", json!({"fps": 30.0})).unwrap();
        assert_eq!(s.execute("timeline.info", json!({})).unwrap()["timeline"]["fps"], 30.0);
        // Delete.
        s.execute("timeline.delete", json!({})).unwrap();
        assert_eq!(s.execute("timeline.info", json!({})).unwrap()["timeline"], Value::Null);
    }

    #[test]
    fn navigation_without_timeline_errors() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(s.execute("timeline.setFrame", json!({"frame": 1})).is_err());
    }
}

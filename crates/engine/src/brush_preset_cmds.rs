//! Organising brush presets (the Brushes panel's drag and drop and context menus): rename, move a
//! preset within or between groups, move, rename and delete whole groups.
//!
//! The library order is the panel order: groups appear in order of their first preset. Every
//! change syncs to the preset store ([`crate::preset_store`]), whose index keeps the group order
//! and the order of every preset, built-ins included. A built-in that is renamed or moved to
//! another group becomes the user's preset (built-ins are regenerated with their own group).

use photocraft_paint::BrushPreset;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// Longest preset or group name accepted.
pub const MAX_NAME: usize = 255;

fn text(p: &Value, k: &str, cmd: &str) -> Result<String> {
    let v = p.get(k).and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, format!("missing `{k}`")))?;
    if v.chars().count() > MAX_NAME {
        return Err(bad(cmd, format!("`{k}` is longer than {MAX_NAME} characters")));
    }
    Ok(v.to_string())
}

/// A group name param: may be empty (the ungrouped presets), never absurdly long.
fn group_param(p: &Value, k: &str, cmd: &str) -> Result<Option<String>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(g)) if g.chars().count() <= MAX_NAME => Ok(Some(g.trim().to_string())),
        Some(_) => Err(bad(cmd, format!("`{k}` must be a group name (string, at most {MAX_NAME} characters)"))),
    }
}

fn index_of(presets: &[BrushPreset], name: &str) -> Option<usize> {
    presets.iter().position(|x| x.name.eq_ignore_ascii_case(name))
}

fn find(s: &Session, p: &Value, k: &str, cmd: &str) -> Result<usize> {
    let name = text(p, k, cmd)?;
    index_of(&s.tools.presets, &name).ok_or_else(|| bad(cmd, format!("no brush preset named `{name}`")))
}

/// Groups in panel order (first appearance).
pub fn group_order(presets: &[BrushPreset]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in presets {
        if !out.contains(&p.group) {
            out.push(p.group.clone());
        }
    }
    out
}

fn position_in_group(presets: &[BrushPreset], i: usize) -> usize {
    presets.get(i).map_or(0, |pr| presets[..i].iter().filter(|x| x.group == pr.group).count())
}

fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.presets.rename";
    let i = find(s, p, "name", cmd)?;
    let new = text(p, "newName", cmd)?;
    if index_of(&s.tools.presets, &new).is_some_and(|j| j != i) {
        return Err(bad(cmd, format!("a brush preset named `{new}` already exists")));
    }
    let pr = s.tools.presets.get_mut(i).ok_or_else(|| bad(cmd, "no such preset"))?;
    if pr.name != new {
        pr.name = new.clone();
        pr.builtin = false;
        s.brush_presets_changed();
    }
    Ok(json!({ "name": new }))
}

fn move_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.presets.move";
    let i = find(s, p, "name", cmd)?;
    let before = match p.get("before") {
        None | Some(Value::Null) => None,
        Some(_) => {
            let b = find(s, p, "before", cmd)?;
            if b == i {
                return Err(bad(cmd, "`before` names the preset being moved"));
            }
            Some(b)
        }
    };
    let index = match p.get("index") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(cmd, "`index` must be a non-negative integer"))?),
    };
    let lib = &mut s.tools.presets;
    let target = match (group_param(p, "group", cmd)?, before) {
        (Some(g), _) => g,
        (None, Some(b)) => lib[b].group.clone(),
        (None, None) => lib[i].group.clone(),
    };
    if let Some(b) = before
        && lib[b].group != target
    {
        return Err(bad(cmd, format!("`before` is not in group `{target}`")));
    }
    let before_name = before.map(|b| lib[b].name.clone());
    let mut pr = lib.remove(i);
    if pr.group != target {
        pr.group = target.clone();
        // A built-in moved elsewhere is the user's now (built-ins regenerate in their own group).
        pr.builtin = false;
    }
    let members: Vec<usize> = lib.iter().enumerate().filter(|(_, x)| x.group == target).map(|(k, _)| k).collect();
    let pos = match (before_name, index) {
        (Some(n), _) => index_of(lib, &n).unwrap_or(lib.len()),
        (None, Some(n)) => match members.get(n.min(usize::MAX as u64) as usize) {
            Some(&k) => k,
            None => members.last().map_or(lib.len(), |k| k + 1),
        },
        (None, None) => members.last().map_or(lib.len(), |k| k + 1),
    };
    let pos = pos.min(lib.len());
    let name = pr.name.clone();
    lib.insert(pos, pr);
    let at = position_in_group(lib, pos);
    s.brush_presets_changed();
    Ok(json!({ "name": name, "group": target, "index": at }))
}

fn group_block(s: &Session, g: &str, cmd: &str) -> Result<()> {
    if s.tools.presets.iter().any(|x| x.group == g) { Ok(()) } else { Err(bad(cmd, format!("no brush preset group named `{g}`"))) }
}

fn move_group(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.presets.moveGroup";
    let g = group_param(p, "group", cmd)?.ok_or_else(|| bad(cmd, "missing `group`"))?;
    group_block(s, &g, cmd)?;
    let before = group_param(p, "before", cmd)?;
    if let Some(b) = &before {
        group_block(s, b, cmd)?;
        if *b == g {
            return Err(bad(cmd, "`before` names the group being moved"));
        }
    }
    let index = match p.get("index") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| bad(cmd, "`index` must be a non-negative integer"))?),
    };
    let lib = std::mem::take(&mut s.tools.presets);
    let (block, mut rest): (Vec<BrushPreset>, Vec<BrushPreset>) = lib.into_iter().partition(|x| x.group == g);
    let order = group_order(&rest);
    let first = |name: &str, rest: &[BrushPreset]| rest.iter().position(|x| x.group == name).unwrap_or(rest.len());
    let pos = match (&before, index) {
        (Some(b), _) => first(b, &rest),
        (None, Some(n)) => order.get(n.min(usize::MAX as u64) as usize).map_or(rest.len(), |name| first(name, &rest)),
        (None, None) => rest.len(),
    };
    let tail = rest.split_off(pos.min(rest.len()));
    rest.extend(block);
    rest.extend(tail);
    s.tools.presets = rest;
    s.brush_presets_changed();
    let at = group_order(&s.tools.presets).iter().position(|x| *x == g).unwrap_or(0);
    Ok(json!({ "group": g, "index": at }))
}

fn rename_group(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.presets.renameGroup";
    let g = group_param(p, "group", cmd)?.ok_or_else(|| bad(cmd, "missing `group`"))?;
    group_block(s, &g, cmd)?;
    let new = text(p, "newName", cmd)?;
    if new != g && s.tools.presets.iter().any(|x| x.group == new) {
        return Err(bad(cmd, format!("a group named `{new}` already exists")));
    }
    if new != g {
        for x in s.tools.presets.iter_mut().filter(|x| x.group == g) {
            x.group = new.clone();
            x.builtin = false;
        }
        s.brush_presets_changed();
    }
    Ok(json!({ "group": new }))
}

fn delete_group(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.presets.deleteGroup";
    let g = group_param(p, "group", cmd)?.ok_or_else(|| bad(cmd, "missing `group`"))?;
    group_block(s, &g, cmd)?;
    let before = s.tools.presets.len();
    s.tools.presets.retain(|x| x.group != g);
    s.brush_presets_changed();
    Ok(json!({ "deleted": before - s.tools.presets.len(), "count": s.tools.presets.len() }))
}

macro_rules! spec {
    ($id:literal, $label:literal, $params:literal, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: always, run: $run, journal: true }
    };
}

/// Brush preset organisation specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!("brush.presets.rename", "Rename Brush", r##"{"name":string,"newName":string} → {name}"##, rename),
        spec!(
            "brush.presets.move",
            "Move Brush",
            r##"{"name":string,"group":string?=its group ("" = ungrouped),"before":name? (preset to land before) | "index":n? (position in the group)=end} → {name, group, index}"##,
            move_preset
        ),
        spec!(
            "brush.presets.moveGroup",
            "Move Brush Group",
            r##"{"group":string,"before":group? | "index":n? (group position)=end} → {group, index}"##,
            move_group
        ),
        spec!("brush.presets.renameGroup", "Rename Brush Group", r##"{"group":string,"newName":string} → {group}"##, rename_group),
        spec!("brush.presets.deleteGroup", "Delete Brush Group", r##"{"group":string} → {deleted, count}"##, delete_group),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &Session, g: &str) -> Vec<String> {
        s.tools.presets.iter().filter(|x| x.group == g).map(|x| x.name.clone()).collect()
    }

    fn with_groups() -> Session {
        let mut s = Session::new();
        for (n, g) in [("A1", "Alpha"), ("A2", "Alpha"), ("A3", "Alpha"), ("B1", "Beta"), ("B2", "Beta")] {
            s.tools.presets.push(BrushPreset { name: n.into(), brush: Default::default(), builtin: false, group: g.into() });
        }
        s
    }

    #[test]
    fn move_within_and_between_groups() {
        let mut s = with_groups();
        let r = s.execute("brush.presets.move", json!({"name": "A3", "before": "A1"})).unwrap();
        assert_eq!(r, json!({"name": "A3", "group": "Alpha", "index": 0}));
        assert_eq!(names(&s, "Alpha"), ["A3", "A1", "A2"]);
        s.execute("brush.presets.move", json!({"name": "A3", "index": 99})).unwrap();
        assert_eq!(names(&s, "Alpha"), ["A1", "A2", "A3"]);
        s.execute("brush.presets.move", json!({"name": "A1", "index": 1})).unwrap();
        assert_eq!(names(&s, "Alpha"), ["A2", "A1", "A3"]);
        // Into another group, before a member (the group comes from `before`).
        s.execute("brush.presets.move", json!({"name": "A1", "before": "B2"})).unwrap();
        assert_eq!((names(&s, "Alpha"), names(&s, "Beta")), (vec!["A2".to_string(), "A3".into()], vec!["B1".to_string(), "A1".into(), "B2".into()]));
        // Into a group by name (appended), and into a new group.
        s.execute("brush.presets.move", json!({"name": "A2", "group": "Beta"})).unwrap();
        assert_eq!(names(&s, "Beta"), ["B1", "A1", "B2", "A2"]);
        s.execute("brush.presets.move", json!({"name": "A3", "group": "Gamma"})).unwrap();
        assert_eq!(group_order(&s.tools.presets).last().map(String::as_str), Some("Gamma"));
        assert!(names(&s, "Alpha").is_empty());
        // A built-in moved to another group becomes the user's.
        let b = s.tools.presets.iter().find(|x| x.builtin).unwrap().name.clone();
        s.execute("brush.presets.move", json!({"name": b, "group": "Beta", "index": 0})).unwrap();
        let moved = photocraft_paint::presets::find(&s.tools.presets, &b).unwrap();
        assert!(!moved.builtin && moved.group == "Beta");
        assert_eq!(names(&s, "Beta")[0], b);
    }

    #[test]
    fn groups_move_rename_and_delete() {
        let mut s = with_groups();
        let order = group_order(&s.tools.presets);
        let n = order.len();
        assert_eq!(&order[n - 2..], ["Alpha", "Beta"]);
        let r = s.execute("brush.presets.moveGroup", json!({"group": "Beta", "index": 0})).unwrap();
        assert_eq!(r["index"], 0);
        assert_eq!(group_order(&s.tools.presets)[0], "Beta");
        assert_eq!(names(&s, "Beta"), ["B1", "B2"], "a group moves as a block, in order");
        s.execute("brush.presets.moveGroup", json!({"group": "Alpha", "before": "Beta"})).unwrap();
        assert_eq!(&group_order(&s.tools.presets)[..2], ["Alpha", "Beta"]);
        s.execute("brush.presets.moveGroup", json!({"group": "Alpha"})).unwrap();
        assert_eq!(group_order(&s.tools.presets).last().map(String::as_str), Some("Alpha"));
        s.execute("brush.presets.renameGroup", json!({"group": "Alpha", "newName": "First"})).unwrap();
        assert_eq!(names(&s, "First"), ["A1", "A2", "A3"]);
        assert!(s.execute("brush.presets.renameGroup", json!({"group": "First", "newName": "Beta"})).is_err());
        let r = s.execute("brush.presets.deleteGroup", json!({"group": "First"})).unwrap();
        assert_eq!(r["deleted"], 3);
        assert!(names(&s, "First").is_empty());
    }

    #[test]
    fn rename_checks_names() {
        let mut s = with_groups();
        s.execute("brush.presets.rename", json!({"name": "a1", "newName": "Alpha One"})).unwrap();
        assert!(photocraft_paint::presets::find(&s.tools.presets, "Alpha One").is_some());
        assert!(s.execute("brush.presets.rename", json!({"name": "A2", "newName": "alpha one"})).is_err());
        // Case-only renames of itself are fine.
        s.execute("brush.presets.rename", json!({"name": "Alpha One", "newName": "ALPHA ONE"})).unwrap();
        let b = s.tools.presets.iter().find(|x| x.builtin).unwrap().name.clone();
        s.execute("brush.presets.rename", json!({"name": b, "newName": "My Round"})).unwrap();
        assert!(!photocraft_paint::presets::find(&s.tools.presets, "My Round").unwrap().builtin);
    }

    #[test]
    fn bad_params_fail_without_changes() {
        let mut s = with_groups();
        let before = s.tools.presets.clone();
        let long = "x".repeat(MAX_NAME + 1);
        for (id, p) in [
            ("brush.presets.move", json!({})),
            ("brush.presets.move", json!({"name": "Nope"})),
            ("brush.presets.move", json!({"name": "A1", "before": "A1"})),
            ("brush.presets.move", json!({"name": "A1", "before": "Nope"})),
            ("brush.presets.move", json!({"name": "A1", "group": "Alpha", "before": "B1"})),
            ("brush.presets.move", json!({"name": "A1", "index": -1})),
            ("brush.presets.move", json!({"name": "A1", "index": "first"})),
            ("brush.presets.move", json!({"name": "A1", "group": 7})),
            ("brush.presets.move", json!({"name": "A1", "group": long})),
            ("brush.presets.rename", json!({"name": "A1"})),
            ("brush.presets.rename", json!({"name": "A1", "newName": "  "})),
            ("brush.presets.rename", json!({"name": "A1", "newName": long})),
            ("brush.presets.moveGroup", json!({"group": "Nope"})),
            ("brush.presets.moveGroup", json!({"group": "Alpha", "before": "Alpha"})),
            ("brush.presets.moveGroup", json!({"group": "Alpha", "index": 1.5})),
            ("brush.presets.renameGroup", json!({"group": "Alpha"})),
            ("brush.presets.deleteGroup", json!({"group": null})),
            ("brush.presets.deleteGroup", json!({"group": "Nope"})),
        ] {
            assert!(s.execute(id, p.clone()).is_err(), "{id} {p}");
        }
        assert_eq!(s.tools.presets, before);
        // Huge indices clamp to the end.
        s.execute("brush.presets.move", json!({"name": "A1", "index": u64::MAX})).unwrap();
        s.execute("brush.presets.moveGroup", json!({"group": "Alpha", "index": u64::MAX})).unwrap();
    }
}

//! Edit › Presets › Migrate Presets. Photoshop imports presets from a previous version; we merge a
//! presets file (another install's preferences, or an exported preset library) into the current
//! library, appending groups the library doesn't already have (by name). Headless and scriptable.

use serde_json::{Value, json};

use crate::presets::{Group, PresetState};
use crate::{EngineError, Result, Session};

/// Append incoming groups whose name the library doesn't already have; returns how many were added.
fn merge_groups<T>(cur: &mut Vec<Group<T>>, incoming: Vec<Group<T>>) -> usize {
    let mut added = 0;
    for g in incoming {
        if !cur.iter().any(|e| e.name == g.name) {
            cur.push(g);
            added += 1;
        }
    }
    added
}

/// Deserialize `v[key]` into `Vec<Group<T>>`, defaulting to empty.
fn groups<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Vec<Group<T>> {
    v.get(key).cloned().and_then(|g| serde_json::from_value(g).ok()).unwrap_or_default()
}

fn migrate(s: &mut Session, p: &Value) -> Result<Value> {
    let path = p
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::BadParams { cmd: "edit.presets.migratePresets".into(), msg: "need `path` to a presets or preferences file".into() })?;
    let text = std::fs::read_to_string(path).map_err(|e| EngineError::Other(format!("read `{path}`: {e}")))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| EngineError::Other(format!("`{path}` is not JSON: {e}")))?;
    // Accept a full preferences file (has a `presets` section) or a bare presets object.
    let v = root.get("presets").cloned().unwrap_or(root);

    let ps: &mut PresetState = &mut s.presets;
    let mut counts = serde_json::Map::new();
    let g = merge_groups(&mut ps.gradients, groups(&v, "gradients"));
    let st = merge_groups(&mut ps.styles, groups(&v, "styles"));
    let sh = merge_groups(&mut ps.shapes, groups(&v, "shapes"));
    let pt = merge_groups(&mut ps.pattern_groups, groups(&v, "pattern_groups"));
    // Tool presets are a flat list, matched by name.
    let mut tp = 0usize;
    if let Some(arr) = v.get("tool_presets").cloned()
        && let Ok(incoming) = serde_json::from_value::<Vec<crate::presets::tools::ToolPreset>>(arr)
    {
        for t in incoming {
            if !ps.tool_presets.iter().any(|e| e.name == t.name) {
                ps.tool_presets.push(t);
                tp += 1;
            }
        }
    }
    let total = g + st + sh + pt + tp;
    if total > 0 {
        ps.rev += 1;
    }
    counts.insert("gradients".into(), json!(g));
    counts.insert("styles".into(), json!(st));
    counts.insert("shapes".into(), json!(sh));
    counts.insert("patternGroups".into(), json!(pt));
    counts.insert("toolPresets".into(), json!(tp));
    Ok(json!({"migrated": total, "added": counts}))
}

pub fn specs() -> Vec<crate::commands::CommandSpec> {
    vec![crate::commands::CommandSpec {
        id: "edit.presets.migratePresets",
        label: "Migrate Presets",
        menu: &["Edit", "Presets"],
        shortcut: None,
        params: r#"{path} → {migrated, added:{gradients,styles,shapes,patternGroups,toolPresets}}: merge a presets/preferences file into the library (appends groups by name)"#,
        enabled: |_s| Ok(()),
        journal: false,
        run: |s, p| migrate(s, p),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_new_groups_only() {
        let mut s = Session::new();
        let before = s.presets.gradients.len();
        let existing = s.presets.gradients[0].name.clone();
        // A presets file with one new gradient group and one that already exists.
        let file = std::env::temp_dir().join(format!("pc-migrate-{}.json", std::process::id()));
        let data = json!({
            "presets": {
                "gradients": [
                    {"name": "My Imported Set", "items": []},
                    {"name": existing, "items": []},
                ]
            }
        });
        std::fs::write(&file, serde_json::to_string(&data).unwrap()).unwrap();
        let r = s.execute("edit.presets.migratePresets", json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(r["added"]["gradients"], 1, "only the new group is added");
        assert_eq!(s.presets.gradients.len(), before + 1);
        assert!(s.presets.gradients.iter().any(|g| g.name == "My Imported Set"));
        // Idempotent: migrating again adds nothing.
        let r2 = s.execute("edit.presets.migratePresets", json!({"path": file.to_string_lossy()})).unwrap();
        assert_eq!(r2["migrated"], 0);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn bad_path_is_an_error() {
        let mut s = Session::new();
        assert!(s.execute("edit.presets.migratePresets", json!({"path": "/no/such/file.json"})).is_err());
        assert!(s.execute("edit.presets.migratePresets", json!({})).is_err());
    }
}

use super::*;
use photocraft_psd::abr::{AbrSample, LegacyBrush, LegacyTip, write_v12};
use serde_json::json;
use std::path::PathBuf;

/// A fresh, empty directory under the system temp dir (tests never write anywhere else).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("pc-preset-store-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        TempDir(d)
    }
    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = DirBackend::new(&self.0).list().unwrap().into_iter().map(|(n, _)| n).collect();
        v.sort();
        v
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session with the store in `dir` attached.
fn session(dir: &TempDir) -> (Session, Vec<String>) {
    let mut s = Session::new();
    let w = s.attach_preset_store(open_dir(&dir.0));
    (s, w)
}

fn tip8(w: u32, h: u32, seed: u32) -> GrayTile {
    GrayTile::from_fn(w, h, |x, y| ((x * 7 + y * 13 + seed) % 256) as f32 / 255.0)
}

fn tip16(w: u32, h: u32, seed: u32) -> GrayTile {
    let data = (0..w * h).map(|i| (i.wrapping_mul(2654435761).wrapping_add(seed) >> 16) as u16).collect();
    GrayTile { width: w, height: h, data }
}

fn sampled(name: &str, group: &str, tip: GrayTile) -> BrushPreset {
    let brush = BrushSettings { size: 42.0, spacing: 0.1, tip: TipShape::Sampled(tip), ..Default::default() };
    BrushPreset { name: name.into(), brush, builtin: false, group: group.into() }
}

fn user(s: &Session) -> Vec<BrushPreset> {
    s.tools.presets.iter().filter(|p| !p.builtin).cloned().collect()
}

#[test]
fn sessions_have_no_store_by_default() {
    let mut s = Session::new();
    assert!(s.preset_store.is_none(), "headless sessions must stay hermetic");
    s.execute("brush.presets.save", json!({"name": "Mine"})).unwrap();
    assert!(s.preset_store.is_none());
}

#[test]
fn tip_codec_round_trips_8_and_16_bit() {
    let t8 = tip8(37, 21, 3);
    let t16 = tip16(40, 33, 9);
    let e8 = encode_tip(&t8);
    let e16 = encode_tip(&t16);
    assert_eq!(e8[6], 8, "8-bit sourced tips are stored at 8 bits");
    assert_eq!(e16[6], 16);
    assert_eq!(decode_tip(&e8).unwrap(), t8);
    assert_eq!(decode_tip(&e16).unwrap(), t16);
}

#[test]
fn tip_decoder_rejects_garbage() {
    let good = encode_tip(&tip16(16, 16, 1));
    for n in 0..good.len() {
        assert!(decode_tip(&good[..n]).is_err(), "truncated at {n}");
    }
    let mut bad = good.clone();
    bad[6] = 12;
    assert!(decode_tip(&bad).is_err());
    // A header claiming a huge bitmap is refused before allocating.
    let mut huge = good.clone();
    huge[7..11].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_tip(&huge).is_err());
    let mut big = good.clone();
    big[7..11].copy_from_slice(&MAX_TIP_SIDE.to_le_bytes());
    big[11..15].copy_from_slice(&MAX_TIP_SIDE.to_le_bytes());
    assert!(decode_tip(&big).is_err());
    let mut rng = 0x1234_5678u32;
    for _ in 0..200 {
        let len = (rng % 64) as usize;
        let v: Vec<u8> = (0..len)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                rng as u8
            })
            .collect();
        let _ = decode_tip(&v);
        let mut w = TIP_MAGIC.to_vec();
        w.extend_from_slice(&v);
        let _ = decode_tip(&w);
    }
}

#[test]
fn group_with_sampled_8_and_16_bit_tips_round_trips() {
    let dir = TempDir::new("roundtrip");
    let (mut s, w) = session(&dir);
    assert!(w.is_empty(), "{w:?}");
    assert!(dir.files().is_empty(), "attaching an empty store writes nothing");
    let mut a = sampled("Eight", "Inks", tip8(64, 48, 1));
    a.brush.dual_brush.tip = TipShape::Sampled(tip8(12, 12, 5));
    a.brush.texture.pattern = Pattern::Tile(tip16(32, 32, 7));
    let b = sampled("Sixteen", "Inks", tip16(50, 70, 2));
    let mut c = sampled("Shared tip", "Other", tip8(64, 48, 1));
    c.brush.size = 9.0;
    s.tools.presets.extend([a, b, c]);
    s.brush_presets_changed();
    s.sync_preset_store();
    let files = dir.files();
    assert_eq!(files.iter().filter(|f| f.ends_with(".pcbrushes")).count(), 2, "{files:?}");
    // Four distinct bitmaps: the 64×48 tip is shared by two presets.
    assert_eq!(files.iter().filter(|f| f.starts_with("tips/")).count(), 4, "{files:?}");
    assert!(files.contains(&INDEX_FILE.to_string()));

    let (t, w) = session(&dir);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(user(&t), user(&s));
    let order: Vec<String> = user(&t).into_iter().map(|p| p.name).collect();
    assert_eq!(order, ["Eight", "Sixteen", "Shared tip"]);
    // Nothing changed: reloading rewrote nothing.
    let before: Vec<_> = DirBackend::new(&dir.0).list().unwrap();
    let mut t = t;
    t.brush_presets_changed();
    t.sync_preset_store();
    assert_eq!(DirBackend::new(&dir.0).list().unwrap(), before);
}

#[test]
fn commands_persist_and_delete_removes_from_disk() {
    let dir = TempDir::new("commands");
    let (mut s, _) = session(&dir);
    s.tools.brush.tip = TipShape::Sampled(tip16(20, 20, 4));
    s.execute("brush.presets.save", json!({"name": "Scribble"})).unwrap();
    assert_eq!(dir.files().iter().filter(|f| f.starts_with("tips/")).count(), 1);
    let (t, _) = session(&dir);
    let p = photocraft_paint::presets::find(&t.tools.presets, "Scribble").expect("saved preset reloads");
    assert_eq!(p.brush.tip, s.tools.brush.tip);

    // Rename through the Preset Manager, then delete.
    s.execute("edit.presets.presetManager", json!({"action": "rename", "kind": "brushes", "name": "Scribble", "newName": "Doodle"})).unwrap();
    let (t, _) = session(&dir);
    assert!(photocraft_paint::presets::find(&t.tools.presets, "Doodle").is_some());
    assert!(photocraft_paint::presets::find(&t.tools.presets, "Scribble").is_none());
    s.execute("brush.presets.delete", json!({"name": "Doodle"})).unwrap();
    assert_eq!(dir.files(), [INDEX_FILE], "the group file and its tip are gone");
    let (t, _) = session(&dir);
    assert!(photocraft_paint::presets::find(&t.tools.presets, "Doodle").is_none());
}

#[test]
fn imported_abr_group_persists() {
    let dir = TempDir::new("abr");
    let (mut s, _) = session(&dir);
    let tip = AbrSample { id: String::new(), width: 6, height: 4, depth: 8, data: (0..24).map(|i| i * 10).collect() };
    let abr = write_v12(
        2,
        &[
            LegacyBrush { name: "Grit".into(), spacing: 25, anti_alias: true, tip: LegacyTip::Sampled(tip) },
            LegacyBrush {
                name: "Round 9".into(),
                spacing: 10,
                anti_alias: true,
                tip: LegacyTip::Computed { diameter: 9, hardness: 100, angle: 0, roundness: 100 },
            },
        ],
        true,
    )
    .unwrap();
    let data = photocraft_paint::tile::b64_encode(&abr);
    s.execute("brush.presets.importAbr", json!({"data": data, "group": "Legacy Set"})).unwrap();
    let (t, w) = session(&dir);
    assert!(w.is_empty(), "{w:?}");
    let grp: Vec<&BrushPreset> = t.tools.presets.iter().filter(|p| p.group == "Legacy Set").collect();
    assert_eq!(grp.len(), 2);
    assert_eq!(user(&t), user(&s));
    // Deleting every preset of the group removes its file and tips.
    let names: Vec<String> = grp.iter().map(|p| p.name.clone()).collect();
    for n in names {
        s.execute("brush.presets.delete", json!({"name": n})).unwrap();
    }
    assert!(!dir.files().iter().any(|f| f.ends_with(".pcbrushes") || f.starts_with("tips/")), "{:?}", dir.files());
}

#[test]
fn deleted_builtins_stay_deleted_and_overrides_replace_them() {
    let dir = TempDir::new("builtins");
    let (mut s, _) = session(&dir);
    let first = s.tools.presets[0].name.clone();
    let second = s.tools.presets[1].name.clone();
    s.execute("brush.presets.delete", json!({"name": first})).unwrap();
    s.tools.brush.size = 77.0;
    s.execute("brush.presets.save", json!({"name": second})).unwrap();
    let (t, _) = session(&dir);
    assert!(photocraft_paint::presets::find(&t.tools.presets, &first).is_none());
    let p = photocraft_paint::presets::find(&t.tools.presets, &second).unwrap();
    assert_eq!((p.builtin, p.brush.size), (false, 77.0));
    assert_eq!(t.tools.presets.iter().filter(|p| p.name.eq_ignore_ascii_case(&second)).count(), 1);
    // No built-in is ever written.
    let n = dir.files().iter().filter(|f| f.ends_with(".pcbrushes")).count();
    assert_eq!(n, 1);
}

#[test]
fn startup_with_many_groups_loads_them_all() {
    let dir = TempDir::new("many");
    let (mut s, _) = session(&dir);
    for g in 0..25 {
        for i in 0..4 {
            s.tools.presets.push(sampled(&format!("P{g}-{i}"), &format!("Group {g}"), tip8(16 + i, 16, g)));
        }
    }
    s.brush_presets_changed();
    s.sync_preset_store();
    let (t, w) = session(&dir);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(user(&t).len(), 100);
    let groups: Vec<String> = user(&t).iter().map(|p| p.group.clone()).fold(Vec::new(), |mut v, g| {
        if !v.contains(&g) {
            v.push(g);
        }
        v
    });
    assert_eq!(groups, (0..25).map(|g| format!("Group {g}")).collect::<Vec<_>>(), "group order survives");
}

#[test]
fn reordering_presets_and_groups_persists() {
    let names = |s: &Session| s.tools.presets.iter().map(|p| (p.name.clone(), p.group.clone())).collect::<Vec<_>>();
    let dir = TempDir::new("reorder");
    let (mut s, _) = session(&dir);
    for (n, g) in [("Ink 1", "Inks"), ("Ink 2", "Inks"), ("Wash", "Washes")] {
        s.execute("brush.presets.save", json!({"name": n})).unwrap();
        s.tools.presets.iter_mut().find(|p| p.name == n).unwrap().group = g.into();
    }
    s.brush_presets_changed();
    s.sync_preset_store();
    // Untouched order: no order list is written (the default order is implied).
    let (t, _) = session(&dir);
    assert_eq!(names(&t), names(&s));
    // Reorder a built-in within its group, a user preset across groups, and whole groups.
    let builtin_group = s.tools.presets[0].group.clone();
    let last_builtin = s.tools.presets.iter().rfind(|p| p.builtin && p.group == builtin_group).unwrap().name.clone();
    let first = s.tools.presets[0].name.clone();
    s.execute("brush.presets.move", json!({"name": last_builtin, "before": first})).unwrap();
    s.execute("brush.presets.move", json!({"name": "Ink 2", "group": "Washes", "index": 0})).unwrap();
    s.execute("brush.presets.moveGroup", json!({"group": "Washes", "index": 0})).unwrap();
    s.execute("brush.presets.moveGroup", json!({"group": "Inks", "before": builtin_group})).unwrap();
    assert_eq!(s.tools.presets[0].name, "Ink 2");
    let (t, w) = session(&dir);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(names(&t), names(&s), "the panel order survives a restart");
    assert!(t.tools.presets.iter().find(|p| p.name == last_builtin).unwrap().builtin, "reordering keeps a built-in built-in");
    // Renaming and deleting a group persist too.
    s.execute("brush.presets.renameGroup", json!({"group": "Inks", "newName": "Pens"})).unwrap();
    s.execute("brush.presets.deleteGroup", json!({"group": "Washes"})).unwrap();
    let (t, _) = session(&dir);
    assert_eq!(names(&t), names(&s));
    assert!(t.tools.presets.iter().all(|p| p.group != "Washes" && p.group != "Inks"));
}

#[test]
fn corrupt_and_oversized_files_are_skipped_with_a_warning() {
    let dir = TempDir::new("corrupt");
    let (mut s, _) = session(&dir);
    s.tools.presets.push(sampled("Good", "Fine", tip8(8, 8, 1)));
    s.tools.presets.push(sampled("Lost", "Broken tip", tip16(9, 9, 2)));
    s.brush_presets_changed();
    s.sync_preset_store();
    let be = DirBackend::new(&dir.0);
    be.write("junk-00000000.pcbrushes", b"{not json").unwrap();
    be.write("wrong-00000000.pcbrushes", br#"{"format":"other","version":1,"group":"x","presets":[]}"#).unwrap();
    // Corrupt the 16-bit tip of "Broken tip".
    let h = tip_hash(&tip16(9, 9, 2));
    be.write(&tip_file(&h), b"PCTIP1\x10garbage").unwrap();
    let (mut t, w) = session(&dir);
    assert_eq!(w.len(), 4, "{w:?}");
    assert!(photocraft_paint::presets::find(&t.tools.presets, "Good").is_some());
    assert!(photocraft_paint::presets::find(&t.tools.presets, "Lost").is_none());
    // The broken files are left alone for the user (never deleted by a sync).
    t.execute("brush.presets.save", json!({"name": "Another"})).unwrap();
    let files = dir.files();
    assert!(files.contains(&"junk-00000000.pcbrushes".to_string()) && files.contains(&tip_file(&h)), "{files:?}");

    // Oversized files are refused without reading them whole.
    let mem = MemBackend::default();
    mem.files.lock().unwrap().insert("big-00000000.pcbrushes".into(), vec![b' '; MAX_GROUP_BYTES as usize + 1]);
    mem.files.lock().unwrap().insert(INDEX_FILE.into(), b"[1,2".to_vec());
    let o = open(Box::new(mem));
    assert!(o.presets.is_empty());
    assert_eq!(o.warnings.len(), 2, "{:?}", o.warnings);
}

#[test]
fn write_failures_are_warnings() {
    let dir = TempDir::new("readonly");
    // The store "directory" is a file: every write fails.
    std::fs::write(&dir.0, b"not a dir").unwrap();
    let mut s = Session::new();
    s.attach_preset_store(open_dir(&dir.0));
    s.tools.brush.tip = TipShape::Sampled(tip8(4, 4, 0));
    s.execute("brush.presets.save", json!({"name": "Nope"})).unwrap();
    let w = s.preset_store.as_mut().unwrap().take_warnings();
    assert!(!w.is_empty());
    let _ = std::fs::remove_file(&dir.0);
}

#[test]
fn backends_refuse_paths_outside_the_store() {
    let mem = MemBackend::default();
    for n in ["../x.pcbrushes", "a/b.pcbrushes", "tips/../x", ".hidden", "", "tips/"] {
        assert!(mem.write(n, b"x").is_err(), "{n}");
    }
    assert!(group_file_name("../../etc/passwd").ends_with(".pcbrushes"));
    assert!(valid_name(&group_file_name("../../etc/passwd")));
    assert!(valid_name(&group_file_name("")));
    assert!(valid_name(&group_file_name("日本語 ブラシ")));
    assert_ne!(group_file_name("a/b"), group_file_name("a_b"));
}

/// `cargo test --release -p photocraft-engine --lib preset_store::tests::bench_load_500 -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_load_500_sampled_presets() {
    let dir = TempDir::new("bench");
    let (mut s, _) = session(&dir);
    for i in 0..500u32 {
        // A mix of 8-bit (most .abr tips) and 16-bit tips, 64..319 px.
        let side = 64 + (i * 37) % 256;
        let t = if i % 4 == 0 { tip16(side, side, i) } else { tip8(side, side, i) };
        s.tools.presets.push(sampled(&format!("Brush {i}"), &format!("Set {}", i / 50), t));
    }
    s.brush_presets_changed();
    let t0 = std::time::Instant::now();
    s.sync_preset_store();
    let write = t0.elapsed();
    let bytes = s.preset_store.as_ref().unwrap().bytes();
    let t0 = std::time::Instant::now();
    let o = open_dir(&dir.0);
    let load = t0.elapsed();
    assert_eq!(o.presets.len(), 500);
    let t0 = std::time::Instant::now();
    s.execute("edit.presets.presetManager", json!({"action": "rename", "kind": "brushes", "name": "Brush 3", "newName": "Renamed"})).unwrap();
    let rename = t0.elapsed();
    eprintln!("500 sampled presets: {:.1} MB on disk; write {write:?}, load {load:?}, rename {rename:?}", bytes as f64 / 1e6);
}

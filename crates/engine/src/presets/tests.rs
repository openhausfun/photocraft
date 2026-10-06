use serde_json::{Value, json};

use crate::Session;
use photocraft_doc::{Fill, LayerContent, LayerId};

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": depth})).unwrap();
    s
}

fn active(s: &Session) -> LayerId {
    s.active().unwrap().active_layer.unwrap()
}

fn layer_px(s: &Session, x: i32, y: i32) -> Vec<f32> {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
}

// ------------------------------------------------------------------ gradients

#[test]
fn gradient_builtins_and_listing() {
    let mut s = Session::new();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    let groups = v["groups"].as_array().unwrap();
    let names: Vec<&str> = groups.iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "Basics");
    assert!(names.contains(&"Blues") && names.contains(&"Grays"));
    assert_eq!(v["current"]["name"], "Foreground to Background");
    assert_eq!(groups[0]["presets"][1]["transparency"][1], json!([1.0, 0.0]));
}

#[test]
fn gradient_tool_uses_selected_preset_at_every_depth() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        // Default (Foreground to Background): black → white, as before presets existed.
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
        assert!(layer_px(&s, 0, 10)[0] < 0.02, "depth {depth}");
        assert!(layer_px(&s, 63, 10)[0] > 0.98);
        s.execute("gradient.presets.select", json!({"preset": "Red 01"})).unwrap();
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
        let left = layer_px(&s, 0, 10);
        // Red 01 starts at #7f0000.
        assert!((left[0] - 127.0 / 255.0).abs() < 0.02 && left[1] < 0.01, "depth {depth}: {left:?}");
        // Explicit stops and a named preset override the selection.
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0], "stops": [[0, "#00ff00"], [1, "#0000ff"]]})).unwrap();
        assert!(layer_px(&s, 0, 10)[1] > 0.98);
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0], "gradient": "Black, White"})).unwrap();
        assert!(layer_px(&s, 63, 10)[0] > 0.98);
    }
}

#[test]
fn foreground_to_transparent_paints_alpha() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("gradient.presets.select", json!({"preset": "Foreground to Transparent"})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    assert!(layer_px(&s, 0, 5)[3] > 0.95);
    assert!(layer_px(&s, 63, 5)[3] < 0.05);
    let mid = layer_px(&s, 32, 5)[3];
    assert!((mid - 0.5).abs() < 0.05, "{mid}");
}

#[test]
fn gradient_apply_creates_fill_layer_and_select_recolours_it() {
    let mut s = session(8);
    let r = s.execute("gradient.presets.apply", json!({"preset": "Blue 01", "angle": 0})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let stops = |s: &Session| match &s.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Fill(Fill::Gradient { stops, angle, .. }) => (stops.clone(), *angle),
        _ => panic!("not a gradient fill"),
    };
    let (st, angle) = stops(&s);
    assert_eq!(angle, 0.0);
    assert!((st[0].1.c[2] - 0x91 as f32 / 255.0).abs() < 1e-3);
    // Clicking another preset with the fill layer selected changes it (one undo step).
    let r = s.execute("gradient.presets.select", json!({"preset": "Green 01"})).unwrap();
    assert_eq!(r["layer"], json!(id.0));
    assert!(stops(&s).0[0].1.c[1] > 0.25);
    s.execute("edit.undo", json!({})).unwrap();
    assert!((stops(&s).0[0].1.c[2] - 0x91 as f32 / 255.0).abs() < 1e-3);
}

#[test]
fn gradient_new_rename_move_delete_groups() {
    let mut s = Session::new();
    let rev = s.prefs.rev();
    s.execute("gradient.presets.select", json!({"stops": [[0, "#112233"], [0.5, "foreground"], [1, "#ffffff"]], "transparency": [[0, 100], [1, 50]]})).unwrap();
    let r = s.execute("gradient.presets.new", json!({"name": "Mine", "group": "My Gradients"})).unwrap();
    assert_eq!(r["group"], "My Gradients");
    assert!(s.prefs.rev() > rev, "preset edits mark preferences dirty");
    let dup = s.execute("gradient.presets.new", json!({"name": "Mine"})).unwrap();
    assert_eq!(dup["name"], "Mine 2");
    s.execute("gradient.presets.edit", json!({"action": "rename", "preset": "Mine", "name": "Ours"})).unwrap();
    assert!(s.execute("gradient.presets.edit", json!({"action": "rename", "preset": "Ours", "name": "Mine 2"})).is_err());
    s.execute("gradient.presets.edit", json!({"action": "move", "preset": "Mine 2", "to": "My Gradients", "index": 0})).unwrap();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    let mine = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "My Gradients").unwrap().clone();
    assert_eq!(mine["presets"][0]["name"], "Mine 2");
    assert_eq!(mine["presets"][1]["stops"][1], json!([0.5, "foreground"]));
    assert_eq!(mine["presets"][1]["transparency"][1], json!([1.0, 50.0]));
    s.execute("gradient.presets.edit", json!({"action": "delete", "preset": ["Ours", "Mine 2"]})).unwrap();
    s.execute("gradient.presets.edit", json!({"action": "deleteGroup", "group": "My Gradients"})).unwrap();
    s.execute("gradient.presets.edit", json!({"action": "deleteGroup", "group": "Blues"})).unwrap();
    s.execute("gradient.presets.reset", json!({})).unwrap();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    assert!(v["groups"].as_array().unwrap().iter().any(|g| g["name"] == "Blues"));
    // Bad params.
    assert!(s.execute("gradient.presets.select", json!({"preset": "nope"})).is_err());
    assert!(s.execute("gradient.presets.select", json!({"stops": [[0, "#zzzzzz"], [1, "#000000"]]})).is_err());
    assert!(s.execute("gradient.presets.edit", json!({"action": "explode"})).is_err());
}

#[test]
fn presets_persist_through_preferences_json() {
    let mut s = Session::new();
    s.execute("gradient.presets.new", json!({"name": "Saved", "stops": [[0, "#ff0000"], [1, "#0000ff"]]})).unwrap();
    s.execute("tool.presets.new", json!({"name": "Big Brush", "tool": "brush", "options": {"brush": {"size": 300.0}}})).unwrap();
    s.execute("shape.presets.new", json!({"name": "Tri", "path": "M 0 0 L 10 0 L 5 10 Z"})).unwrap();
    s.execute("style.presets.edit", json!({"action": "deleteGroup", "group": "Buttons"})).unwrap();
    let text = s.prefs_to_json();
    let mut t = Session::new();
    t.load_prefs_json(&text).unwrap();
    let names = |t: &mut Session, cmd: &str, key: &str| -> Vec<String> {
        let v = t.execute(cmd, json!({})).unwrap();
        v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g[key].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_string()).collect::<Vec<_>>())
            .collect()
    };
    assert!(names(&mut t, "gradient.presets.list", "presets").contains(&"Saved".to_string()));
    assert!(names(&mut t, "shape.presets.list", "shapes").contains(&"Tri".to_string()));
    let styles = t.execute("style.presets.list", json!({})).unwrap();
    assert!(!styles["groups"].as_array().unwrap().iter().any(|g| g["name"] == "Buttons"));
    let tp = t.execute("tool.presets.list", json!({"tool": "Brush Tool"})).unwrap();
    assert!(tp["presets"].as_array().unwrap().iter().any(|p| p["name"] == "Big Brush"));
    // Older preference files without presets keep the defaults.
    let mut u = Session::new();
    u.load_prefs_json("{}").unwrap();
    assert_eq!(names(&mut u, "gradient.presets.list", "presets").len(), Session::new().presets.gradients.iter().map(|g| g.items.len()).sum::<usize>());
}

// ------------------------------------------------------------------ patterns

#[test]
fn pattern_groups_select_apply_and_sync() {
    let mut s = session(8);
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups[0]["name"], "Geometric");
    assert_eq!(groups[0]["patterns"].as_array().unwrap().len(), 4);
    s.execute("pattern.presets.select", json!({"pattern": "Bricks"})).unwrap();
    // Pattern fill layers and Fill default to the selected pattern.
    let r = s.execute("pattern.presets.apply", json!({})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let name_of = |s: &Session| match &s.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Fill(Fill::Pattern { name, .. }) => name.clone(),
        _ => panic!(),
    };
    assert_eq!(name_of(&s), "Bricks");
    let r = s.execute("pattern.presets.select", json!({"pattern": "Dots"})).unwrap();
    assert_eq!(r["layer"], json!(id.0));
    assert_eq!(name_of(&s), "Dots");
    // A newly defined pattern lands in the trailing group, and can be moved / grouped.
    let r = s.execute("pattern.presets.new", json!({"name": "Mine", "rect": [0, 0, 8, 8], "group": "Custom"})).unwrap();
    let pid = r["pattern"].as_str().unwrap().to_string();
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    let custom = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "Custom").unwrap().clone();
    assert_eq!(custom["patterns"][0]["id"], json!(pid));
    s.execute("pattern.presets.edit", json!({"action": "move", "preset": "Mine", "to": "Geometric"})).unwrap();
    s.execute("pattern.presets.edit", json!({"action": "rename", "preset": "Mine", "name": "Ours"})).unwrap();
    s.execute("pattern.presets.edit", json!({"action": "delete", "preset": "Ours"})).unwrap();
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    assert_eq!(v["groups"][0]["patterns"].as_array().unwrap().len(), 4);
    // Deleting a group deletes its patterns from the library.
    s.execute("pattern.presets.edit", json!({"action": "deleteGroup", "group": "Textures"})).unwrap();
    assert!(crate::pattern_cmds::resolve(&s, "Weave").is_none());
}

// ------------------------------------------------------------------ styles

#[test]
fn styles_apply_to_selected_layers_and_new_from_layer() {
    let mut s = session(16);
    let a = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().map(LayerId).unwrap_or_else(|| active(&s));
    let b = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().map(LayerId).unwrap_or_else(|| active(&s));
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    s.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
    let r = s.execute("style.presets.apply", json!({"preset": "Gold"})).unwrap();
    assert_eq!(r["layers"].as_array().unwrap().len(), 2);
    let fx = |s: &Session, id: LayerId| s.active().unwrap().doc.layer(id).unwrap().effects.items.len();
    assert_eq!((fx(&s, a), fx(&s, b)), (3, 3));
    // ⇧-click adds; plain apply replaces; Default Style clears.
    s.execute("style.presets.apply", json!({"preset": "Black Stroke", "add": true, "layer": a.0})).unwrap();
    assert_eq!(fx(&s, a), 4);
    s.execute("style.presets.apply", json!({"preset": "Hollow", "layer": b.0})).unwrap();
    let lb = s.active().unwrap().doc.layer(b).unwrap().clone();
    assert_eq!((lb.effects.items.len(), lb.fill_opacity), (1, 0.0));
    s.execute("style.presets.apply", json!({"preset": "Default Style (None)", "layer": b.0})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(b).unwrap().fill_opacity, 1.0);
    // One undo step per apply.
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(fx(&s, b), 1);
    // New Style from the layer, applied elsewhere.
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    s.execute("style.presets.new", json!({"name": "From A", "group": "Mine"})).unwrap();
    s.execute("style.presets.apply", json!({"preset": "From A", "layer": b.0})).unwrap();
    assert_eq!(fx(&s, b), 4);
    assert!(s.execute("style.presets.apply", json!({"preset": "nope"})).is_err());
    // Thumbnails render the effect around the swatch.
    let st = s.presets.styles[0].items.iter().find(|x| x.name == "Outer Glow").unwrap().clone();
    let px = super::styles::thumbnail(&s, &st, 48);
    assert_eq!(px.len(), 48 * 48 * 4);
    assert!(px[(4 * 48 + 24) * 4 + 3] == 0 || px[(7 * 48 + 24) * 4 + 3] > 0, "glow reaches outside the square");
    assert!(px[(24 * 48 + 24) * 4 + 3] == 255);
}

#[test]
fn styles_with_pattern_overlay_copy_the_pattern() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    let id = active(&s);
    s.presets.styles[0].items.push(super::styles::StylePreset {
        name: "Bricky".into(),
        effects: vec![crate::layer_style::effect_from_params("patternOverlay", &json!({"pattern": "Bricks"})).unwrap()],
        blend: None,
        fill_opacity: None,
    });
    s.execute("style.presets.apply", json!({"preset": "Bricky", "layer": id.0})).unwrap();
    assert!(s.active().unwrap().doc.patterns.iter().any(|p| p.name == "Bricks"));
}

// ------------------------------------------------------------------ shapes

#[test]
fn shape_path_language_parses() {
    let p = super::shapes::parse("M 0 0 L 10 0 L 10 10 Z ! O 5 5 2").unwrap();
    assert_eq!(p.subpaths.len(), 2);
    assert_eq!(p.subpaths[0].knots.len(), 3);
    assert_eq!(p.subpaths[1].op, photocraft_doc::vector::PathOp::Subtract);
    assert!(super::shapes::parse("M 0 0 L x").is_err());
    assert!(super::shapes::parse("").is_err());
    assert!(super::shapes::parse("L 1 1").is_err());
    // Every built-in parsed and has an area.
    for g in super::shapes::builtin() {
        assert!(!g.items.is_empty(), "{}", g.name);
        for sh in g.items {
            let cov = super::shapes::thumbnail(&sh.path, 32);
            assert!(cov.iter().sum::<f32>() > 20.0, "{} is empty", sh.name);
        }
    }
    let n: usize = super::shapes::builtin().iter().map(|g| g.items.len()).sum();
    assert!(n >= 30, "{n}");
}

#[test]
fn shapes_place_as_shape_layers() {
    let mut s = session(8);
    let r = s.execute("shape.presets.place", json!({"preset": "Heart", "rect": [10, 10, 40, 20], "fill": "#ff0000"})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let l = s.active().unwrap().doc.layer(id).unwrap().clone();
    assert_eq!(l.name, "Heart");
    let LayerContent::Shape(sh) = &l.content else { panic!("not a shape layer") };
    let (x0, y0, x1, y1) = sh.path.control_bounds().unwrap();
    // Keep aspect: 20 px tall, centred horizontally in the 40 px rect.
    assert!((y0 - 10.0).abs() < 0.01 && (y1 - 30.0).abs() < 0.01, "{y0} {y1}");
    // The heart's box is 100 × 92: 20 px tall → 21.7 px wide, centred on x = 30.
    assert!(((x1 - x0) - 20.0 * 100.0 / 92.0).abs() < 0.05 && ((x0 + x1) / 2.0 - 30.0).abs() < 0.01, "{x0} {x1}");
    // Free (dragged without ⇧) fills the rect; negative drags flip.
    let r = s.execute("shape.presets.place", json!({"preset": "Arrow Right", "rect": [50, 40, -40, -20], "keepAspect": false})).unwrap();
    let l = s.active().unwrap().doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap().clone();
    let LayerContent::Shape(sh) = &l.content else { panic!() };
    assert_eq!(sh.path.control_bounds().unwrap(), (10.0, 20.0, 50.0, 40.0));
    // Default placement: centred, half the short side.
    s.execute("shape.presets.place", json!({"preset": "Ring"})).unwrap();
    assert!(s.execute("shape.presets.place", json!({"preset": "Unicorn"})).is_err());
    assert!(s.execute("shape.presets.place", json!({"preset": "Heart", "rect": [0, 0, 0, 5]})).is_err());
    // Undo removes the layer.
    let n = s.active().unwrap().doc.layers.len();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers.len(), n - 1);
}

#[test]
fn custom_shapes_join_the_shapes_panel() {
    let mut s = session(8);
    s.execute("shape.create", json!({"kind": "star", "rect": [0, 0, 30, 30]})).unwrap();
    s.execute("edit.defineCustomShape", json!({"name": "My Star"})).unwrap();
    let v = s.execute("shape.presets.list", json!({})).unwrap();
    let last = v["groups"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["name"], super::shapes::CUSTOM_GROUP);
    s.execute("shape.presets.place", json!({"preset": "My Star"})).unwrap();
    s.execute("shape.presets.edit", json!({"action": "rename", "preset": "My Star", "name": "Star 2"})).unwrap();
    s.execute("shape.presets.edit", json!({"action": "delete", "preset": "Star 2"})).unwrap();
    assert!(s.edit_state.custom_shapes.is_empty());
    // New preset from the active shape layer, normalised to the 100 box.
    s.execute("shape.presets.new", json!({"name": "From Layer", "group": "Mine"})).unwrap();
    let g = s.presets.shapes.iter().find(|g| g.name == "Mine").unwrap();
    let b = g.items[0].path.control_bounds().unwrap();
    assert!(b.2.max(b.3) > 99.9);
}

// ------------------------------------------------------------------ tool presets

#[test]
fn tool_presets_save_filter_select() {
    let mut s = Session::new();
    s.execute("tools.setBrush", json!({"size": 77.0})).unwrap();
    s.execute("tool.presets.new", json!({"name": "Seventy Seven", "tool": "brush", "includeColor": true, "options": {"toolOptions": {"x": 1}}})).unwrap();
    s.execute("tools.setBrush", json!({"size": 5.0})).unwrap();
    s.tools.foreground = [1.0, 0.0, 0.0, 1.0];
    let v = s.execute("tool.presets.list", json!({"tool": "eraser"})).unwrap();
    assert!(v["presets"].as_array().unwrap().iter().all(|p| p["tool"] == "eraser"));
    let r = s.execute("tool.presets.select", json!({"preset": "Seventy Seven"})).unwrap();
    assert_eq!(r["tool"], "brush");
    assert_eq!(r["options"]["toolOptions"]["x"], 1);
    // Options passed explicitly win over the current brush.
    assert!(r["options"]["brush"].is_null() || r["options"]["brush"].is_object());
    assert_eq!(s.tools.foreground, [0.0, 0.0, 0.0, 1.0]);
    s.execute("tool.presets.new", json!({"name": "Eraser 77", "tool": "eraser"})).unwrap();
    let r = s.execute("tool.presets.select", json!({"preset": "Eraser 77"})).unwrap();
    // Selecting "Seventy Seven" restored its 77 px brush, which the new preset captured.
    assert_eq!(r["options"]["brush"]["size"], json!(77.0));
    s.execute("tool.presets.select", json!({"preset": "Soft Round 100 px"})).unwrap();
    assert_eq!(s.tools.brush.size, 100.0);
    s.execute("tool.presets.edit", json!({"action": "rename", "preset": "Eraser 77", "name": "E"})).unwrap();
    s.execute("tool.presets.edit", json!({"action": "delete", "preset": "E"})).unwrap();
    assert!(s.execute("tool.presets.select", json!({"preset": "E"})).is_err());
    assert!(s.execute("tool.presets.edit", json!({"action": "newGroup"})).is_err());
    assert!(s.execute("tool.presets.new", json!({"name": "x"})).is_err());
}

// ------------------------------------------------------------------ clone source

fn clone_doc(depth: u32) -> Session {
    let mut s = session(depth);
    // A horizontal ramp in red, green = row, so transforms are measurable.
    s.edit("ramp", |doc, _| {
        let surf = doc.layers[0].surface_mut().unwrap();
        let fmt = surf.format();
        let mut px = Vec::new();
        for y in 0..48 {
            for x in 0..64 {
                px.extend(photocraft_raster::from_rgba(&fmt, [x as f32 / 63.0, y as f32 / 47.0, 0.0, 1.0]));
            }
        }
        surf.write_region(photocraft_geom::Rect::new(0, 0, 64, 48), &px);
        Ok(())
    })
    .unwrap();
    s
}

#[test]
fn clone_source_slots_and_identity_matches_offset() {
    for depth in [8, 16, 32] {
        let mut s = clone_doc(depth);
        s.execute("cloneSource.set", json!({"index": 2, "source": [10, 10]})).unwrap();
        let v = s.execute("cloneSource.list", json!({})).unwrap();
        assert_eq!(v["active"], 2);
        assert_eq!(v["sources"][2]["source"], json!([10.0, 10.0]));
        // Stroke at (40, 30) without params samples the active slot: offset (−30, −20).
        s.execute("paint.cloneStamp", json!({"points": [[40, 30]], "size": 5, "hardness": 100})).unwrap();
        let px = layer_px(&s, 40, 30);
        assert!((px[0] - 10.0 / 63.0).abs() < 0.01 && (px[1] - 10.0 / 47.0).abs() < 0.01, "depth {depth}: {px:?}");
        // Aligned: the pairing stuck, the next stroke keeps the offset.
        let v = s.execute("cloneSource.list", json!({})).unwrap();
        assert_eq!(v["sources"][2]["offset"], json!([-30.0, -20.0]));
        s.execute("paint.cloneStamp", json!({"points": [[50, 40]], "size": 3, "hardness": 100})).unwrap();
        let px = layer_px(&s, 50, 40);
        assert!((px[0] - 20.0 / 63.0).abs() < 0.01, "{px:?}");
    }
}

#[test]
fn clone_source_flip_scale_rotation() {
    let mut s = clone_doc(16);
    s.execute("cloneSource.set", json!({"source": [20, 20], "flipH": true})).unwrap();
    s.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    // Flipped: 2 px right of the anchor samples 2 px left of the source.
    let px = layer_px(&s, 42, 24);
    assert!((px[0] - 18.0 / 63.0).abs() < 0.02, "{px:?}");
    s.execute("cloneSource.resetTransform", json!({})).unwrap();
    s.execute("cloneSource.set", json!({"source": [20, 20], "width": 200, "height": 200})).unwrap();
    let mut s2 = clone_doc(16);
    s2.presets.clone = s.presets.clone.clone();
    s2.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    // 200 %: 4 px right of the anchor samples 2 px right of the source.
    let px = layer_px(&s2, 44, 24);
    assert!((px[0] - 22.0 / 63.0).abs() < 0.02, "{px:?}");
    // Rotation 90° (counter-clockwise on screen): 3 px right of the anchor comes from 3 px
    // below the source.
    let mut s3 = clone_doc(32);
    s3.execute("cloneSource.set", json!({"source": [20, 20], "rotation": 90})).unwrap();
    s3.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    let px = layer_px(&s3, 43, 24);
    assert!((px[0] - 20.0 / 63.0).abs() < 0.02 && (px[1] - 23.0 / 47.0).abs() < 0.02, "{px:?}");
    // Explicit params override the slot; healing follows the slot too.
    let mut s4 = clone_doc(8);
    s4.execute("cloneSource.set", json!({"source": [20, 20], "rotation": 45})).unwrap();
    s4.execute("paint.healingBrush", json!({"points": [[40, 24]], "size": 7})).unwrap();
    s4.execute("paint.cloneStamp", json!({"points": [[10, 40]], "offset": [5, 0], "rotation": 0, "size": 3, "hardness": 100})).unwrap();
    let px = layer_px(&s4, 10, 40);
    assert!((px[0] - 15.0 / 63.0).abs() < 0.01, "{px:?}");
}

#[test]
fn clone_source_errors_and_overlay() {
    let mut s = clone_doc(8);
    assert!(s.execute("paint.cloneStamp", json!({"points": [[4, 4]]})).is_err());
    assert!(s.execute("cloneSource.select", json!({"index": 5})).is_err());
    assert!(s.execute("cloneSource.set", json!({"offset": [1, 1]})).is_err());
    let o = s.execute("cloneSource.overlay", json!({"opacity": 40, "clipped": false, "blend": "difference"})).unwrap();
    assert_eq!(o["opacity"], json!(40.0));
    assert!(s.execute("cloneSource.overlay", json!({"blend": "screen"})).is_err());
    let v: Value = s.execute("cloneSource.set", json!({"source": [1, 1], "width": 5000, "rotation": 270})).unwrap();
    assert_eq!(v["width"], json!(1000.0));
    assert_eq!(v["rotation"], json!(-90.0));
}

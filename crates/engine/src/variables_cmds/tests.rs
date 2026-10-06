use super::*;
use photocraft_doc::{Layer, TextLayer};
use photocraft_geom::Rect;

/// A document with a raster layer "photo", a togglable raster "badge" and a text layer "title".
fn session() -> (Session, LayerId, LayerId, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    let ids = s
        .edit("setup", |doc, _| {
            let fmt = doc.pixel_format();
            let mut photo = Layer::raster("photo", fmt);
            photo.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &photocraft_raster::from_rgba(&fmt, [0.5, 0.5, 0.5, 1.0]));
            let mut badge = Layer::raster("badge", fmt);
            badge.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 10, 10), &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
            let title = Layer::new("title", LayerContent::Text(TextLayer { text: "Old".into(), ..Default::default() }));
            let (p, b, t) = (photo.id, badge.id, title.id);
            doc.layers.push(photo);
            doc.layers.push(badge);
            doc.layers.push(title);
            Ok((p, b, t))
        })
        .unwrap();
    (s, ids.0, ids.1, ids.2)
}

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

fn text_of(s: &Session, id: LayerId) -> String {
    match &doc(s).layer(id).unwrap().content {
        LayerContent::Text(t) => t.text.clone(),
        _ => panic!("not a text layer"),
    }
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("pc-vars-{}-{name}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.to_string_lossy().into_owned()
}

#[test]
fn define_apply_visibility_and_text() {
    let (mut s, _photo, badge, title) = session();
    s.execute(
        "image.variables.define",
        json!({"defs": [
            {"name": "showBadge", "layer": badge.0, "type": "visibility"},
            {"name": "headline", "layer": title.0, "type": "textReplacement"},
        ]}),
    )
    .unwrap();
    s.execute(
        "image.variables.dataSets",
        json!({"dataSets": [
            {"name": "A", "values": [
                {"variable": "showBadge", "kind": "visibility", "value": false},
                {"variable": "headline", "kind": "text", "value": "Hello"},
            ]},
        ]}),
    )
    .unwrap();
    assert!(doc(&s).layer(badge).unwrap().visible);
    assert_eq!(text_of(&s, title), "Old");

    // document.inspect surfaces the variables for agents.
    let insp = s.execute("document.inspect", json!({})).unwrap();
    assert_eq!(insp["variables"]["defs"].as_array().unwrap().len(), 2);
    assert_eq!(insp["variables"]["dataSets"][0], "A");

    let r = s.execute("image.applyDataSet", json!({"name": "A"})).unwrap();
    assert_eq!(r["applied"], "A");
    assert!(!doc(&s).layer(badge).unwrap().visible, "visibility applied");
    assert_eq!(text_of(&s, title), "Hello", "text applied");
    assert_eq!(doc(&s).variables.active, Some(0));

    // One history step, undoable back to the original.
    assert_eq!(s.active().unwrap().history.entries().last().map(|e| e.as_str()), Some("Apply Data Set \"A\""));
    assert!(s.undo());
    assert!(doc(&s).layer(badge).unwrap().visible);
    assert_eq!(text_of(&s, title), "Old");
}

#[test]
fn bad_params_are_errors() {
    let (mut s, _p, badge, _t) = session();
    assert!(s.execute("image.variables.define", json!({"defs": [{"name": "x", "layer": 999999, "type": "visibility"}]})).is_err());
    assert!(s.execute("image.variables.define", json!({"defs": [{"name": "x", "layer": badge.0, "type": "nope"}]})).is_err());
    assert!(s.execute("image.applyDataSet", json!({"name": "missing"})).is_err());
}

#[test]
fn csv_import_and_apply() {
    let (mut s, _p, badge, title) = session();
    s.execute(
        "image.variables.define",
        json!({"defs": [
            {"name": "showBadge", "layer": badge.0, "type": "visibility"},
            {"name": "headline", "layer": title.0, "type": "textReplacement"},
        ]}),
    )
    .unwrap();
    let dir = tmp("csv");
    let csv = format!("{dir}/sets.csv");
    std::fs::write(&csv, "DataSet,showBadge,headline\nrow-on,true,On Sale\nrow-off,false,Sold Out\n").unwrap();
    let r = s.execute("file.import.variableDataSets", json!({"path": csv})).unwrap();
    assert_eq!(r["imported"], 2);

    s.execute("image.applyDataSet", json!({"name": "row-off"})).unwrap();
    assert!(!doc(&s).layer(badge).unwrap().visible);
    assert_eq!(text_of(&s, title), "Sold Out");

    s.execute("image.applyDataSet", json!({"name": "row-on"})).unwrap();
    assert!(doc(&s).layer(badge).unwrap().visible);
    assert_eq!(text_of(&s, title), "On Sale");
}

#[test]
fn export_data_sets_as_files() {
    let (mut s, _p, badge, title) = session();
    s.execute(
        "image.variables.define",
        json!({"defs": [
            {"name": "showBadge", "layer": badge.0, "type": "visibility"},
            {"name": "headline", "layer": title.0, "type": "textReplacement"},
        ]}),
    )
    .unwrap();
    s.execute(
        "image.variables.dataSets",
        json!({"dataSets": [
            {"name": "one", "values": [{"variable": "headline", "kind": "text", "value": "1"}]},
            {"name": "two", "values": [{"variable": "showBadge", "kind": "visibility", "value": false}]},
        ]}),
    )
    .unwrap();
    let dir = tmp("out");
    let r = s.execute("file.export.dataSetsAsFiles", json!({"dir": dir, "format": "png"})).unwrap();
    assert_eq!(r["count"], 2);
    assert!(std::path::Path::new(&format!("{dir}/one.png")).exists());
    assert!(std::path::Path::new(&format!("{dir}/two.png")).exists());
    // Filename template with {index}.
    let d2 = tmp("tmpl");
    s.execute("file.export.dataSetsAsFiles", json!({"dir": d2, "format": "png", "naming": "row-{index}"})).unwrap();
    assert!(std::path::Path::new(&format!("{d2}/row-001.png")).exists());
    assert!(std::path::Path::new(&format!("{d2}/row-002.png")).exists());
    // Exporting doesn't mutate the live document.
    assert!(doc(&s).layer(badge).unwrap().visible);
    assert_eq!(text_of(&s, title), "Old");
}

#[test]
fn pixel_replacement_changes_the_layer() {
    let (mut s, photo, _b, _t) = session();
    // Make a 8x8 solid-blue PNG to drop in.
    let dir = tmp("px");
    let png = format!("{dir}/blue.png");
    {
        let mut src = Session::new();
        src.execute("file.new", json!({"width": 8, "height": 8, "background": "#0000ff"})).unwrap();
        let bytes = photocraft_io::export(&src.active().unwrap().doc, "png", &photocraft_io::ExportOptions::default()).unwrap().bytes;
        std::fs::write(&png, bytes).unwrap();
    }
    s.execute(
        "image.variables.define",
        json!({"defs": [
            {"name": "img", "layer": photo.0, "type": "pixelReplacement", "method": "conform"},
        ]}),
    )
    .unwrap();
    s.execute(
        "image.variables.dataSets",
        json!({"dataSets": [
            {"name": "blue", "values": [{"variable": "img", "kind": "pixels", "value": png}]},
        ]}),
    )
    .unwrap();
    // Before: grey.
    let before = s.execute("document.pixel", json!({"x": 32, "y": 24})).unwrap();
    s.execute("image.applyDataSet", json!({"name": "blue"})).unwrap();
    let after = s.execute("document.pixel", json!({"x": 32, "y": 24})).unwrap();
    assert_ne!(before, after, "pixel layer content changed");
    // Blue dominates.
    let rgba = after["rgba"].as_array().cloned().unwrap_or_default();
    if rgba.len() == 4 {
        let b = rgba[2].as_f64().unwrap();
        let r = rgba[0].as_f64().unwrap();
        assert!(b > r, "replacement is blue-ish: {rgba:?}");
    }
}

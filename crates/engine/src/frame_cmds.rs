//! Layer › New › Frame from Layers. A frame clips its contents to a rectangle, so a template can
//! hold an image (or placeholder) that is cropped to the frame. We build it from existing
//! primitives: group the selected layers, then clip the group with a rectangular layer mask the
//! size of their bounds — the same visual result Photoshop's Frame tool produces for this command.
//! Headless and scriptable like every other command.

use photocraft_doc::{LayerId, LayerMask, PixelFormat};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    match s.active() {
        Some(d) if !d.selected_layers.is_empty() || d.active_layer.is_some() => Ok(()),
        Some(_) => Err("select a layer".into()),
        None => Err("no document".into()),
    }
}

/// A reveal mask that is on inside `rect` and off (hidden) everywhere else.
fn clip_mask(rect: Rect) -> LayerMask {
    let mut mask = LayerMask::hide_all();
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if w > 0 && h > 0 {
        mask.surface.write_region(rect, &vec![1.0f32; w * h]);
        mask.surface.prune();
    }
    // Default to GRAY8 like the other mask constructors.
    debug_assert_eq!(mask.surface.format(), PixelFormat::GRAY8);
    mask
}

fn frame_from_layers(s: &mut Session, p: &Value) -> Result<Value> {
    // Group the selected layers (one step), then turn the group into a frame.
    let mut gp = serde_json::Map::new();
    if let Some(name) = p.get("name") {
        gp.insert("name".into(), name.clone());
    }
    let grouped = s.execute("layer.new.groupFromLayers", Value::Object(gp))?;
    let gid = LayerId(grouped.get("layer").and_then(Value::as_u64).ok_or_else(|| EngineError::Other("grouping did not return a layer".into()))?);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let rect =
        crate::layer_multi_cmds::layer_bounds(d.doc.layer(gid).ok_or(EngineError::NoLayer(gid))?).filter(|r| !r.is_empty()).unwrap_or_else(|| d.doc.bounds());

    s.edit("Frame from Layers", |doc, _| {
        let g = doc.layer_mut(gid).ok_or(EngineError::NoLayer(gid))?;
        g.mask = Some(clip_mask(rect));
        Ok(())
    })?;
    Ok(json!({"layer": gid.0, "frame": [rect.x0, rect.y0, rect.width(), rect.height()]}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "layer.new.frameFromLayers",
        label: "Frame from Layers",
        menu: &["Layer", "New"],
        shortcut: None,
        params: r#"{"name":str?} → {layer, frame:[x,y,w,h]}: groups the selected layers and clips them to a frame rect"#,
        enabled: has_layer,
        journal: true,
        run: |s, p| frame_from_layers(s, p),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Layer, LayerContent};
    use photocraft_geom::Rect;

    fn session() -> (Session, LayerId, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 80, "height": 60})).unwrap();
        let (a, b) = s
            .edit("setup", |doc, _| {
                let fmt = doc.pixel_format();
                let mut a = Layer::raster("A", fmt);
                a.surface_mut().unwrap().fill_rect(Rect::new(4, 4, 24, 24), &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
                let mut b = Layer::raster("B", fmt);
                b.surface_mut().unwrap().fill_rect(Rect::new(30, 20, 50, 40), &photocraft_raster::from_rgba(&fmt, [0.0, 0.0, 1.0, 1.0]));
                let (ia, ib) = (a.id, b.id);
                doc.layers.push(a);
                doc.layers.push(b);
                Ok((ia, ib))
            })
            .unwrap();
        (s, a, b)
    }

    #[test]
    fn frames_selected_layers_into_a_masked_group() {
        let (mut s, a, b) = session();
        s.execute("layer.select", json!({"layer": a.0, "mode": "replace"})).unwrap();
        s.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
        let r = s.execute("layer.new.frameFromLayers", json!({"name": "Frame 1"})).unwrap();
        let gid = LayerId(r["layer"].as_u64().unwrap());
        // frame rect = union of the two layers' bounds (4,4)..(50,40).
        let frame = r["frame"].as_array().unwrap();
        assert_eq!(frame[2].as_i64().unwrap(), 50 - 4); // width
        assert_eq!(frame[3].as_i64().unwrap(), 40 - 4); // height
        let st = s.active().unwrap();
        let g = st.doc.layer(gid).unwrap();
        assert!(matches!(g.content, LayerContent::Group(_)), "it's a group");
        assert!(g.mask.is_some(), "the group has a clip mask");
        // Undoable (last step is the frame).
        assert_eq!(st.history.entries().last().map(|e| e.as_str()), Some("Frame from Layers"));
    }

    #[test]
    fn needs_a_selected_layer() {
        let mut s = Session::new();
        assert!(s.execute("layer.new.frameFromLayers", json!({})).is_err());
    }
}

use super::*;
use photocraft_doc::effects::Effect;

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 120, "height": 90, "depth": depth, "name": "site"})).unwrap();
    s
}

fn doc(s: &Session) -> &Document {
    &s.active().unwrap().doc
}

fn add_square(s: &mut Session, r: Rect) -> LayerId {
    s.edit("sq", |doc, active| {
        let fmt = doc.pixel_format();
        let mut l = Layer::raster(doc.next_layer_name("Layer"), fmt);
        l.surface_mut().unwrap().fill_rect(r, &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
        let id = doc.insert_above(None, l);
        *active = Some(id);
        Ok(id)
    })
    .unwrap()
}

#[test]
fn user_slices_and_options() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        let r = s.execute("slice.new", json!({"rect": [20, 10, 40, 20], "name": "logo", "url": "https://example.org", "alt": "Logo"})).unwrap();
        let id = r["slice"].as_u64().unwrap() as u32;
        assert_eq!(r["number"], json!(3), "top band, left cell, then the slice");
        let l = s.execute("slice.list", json!({})).unwrap();
        let all = l["slices"].as_array().unwrap();
        assert_eq!(all.len(), 5);
        let me = all.iter().find(|v| v["id"] == json!(id)).unwrap();
        assert_eq!(me["name"], "logo");
        assert_eq!(me["rect"], json!([20, 10, 40, 20]));
        assert_eq!(me["origin"], "user");
        assert!(all.iter().filter(|v| v["origin"] == "auto").count() == 4);
        // Slice Options by number; then undo.
        s.execute("slice.set", json!({"number": 3, "kind": "noImage", "cellText": "hi", "background": "#ff0000"})).unwrap();
        let sl = doc(&s).slices.get(id).unwrap();
        assert_eq!(sl.kind, SliceKind::NoImage);
        assert_eq!(sl.background, Some([255, 255, 0, 0]));
        assert!(s.undo());
        assert_eq!(doc(&s).slices.get(id).unwrap().kind, SliceKind::Image);
        assert!(s.execute("slice.set", json!({"slice": id, "kind": "bogus"})).is_err());
        assert!(s.execute("slice.new", json!({"rect": [500, 500, 10, 10]})).is_err(), "outside the canvas");
    }
}

#[test]
fn promote_auto_slice_and_divide() {
    let mut s = session(8);
    s.execute("slice.new", json!({"rect": [0, 0, 60, 45]})).unwrap();
    // An auto slice promoted through Slice Options keeps one history step.
    let before = s.active().unwrap().history.past_len();
    let r = s.execute("slice.set", json!({"number": 2, "name": "right"})).unwrap();
    assert_eq!(r["origin"], "user");
    assert_eq!(r["name"], "right");
    assert_eq!(s.active().unwrap().history.past_len(), before + 1);
    assert_eq!(doc(&s).slices.list.len(), 2);
    let id = doc(&s).slices.list[0].id;
    let r = s.execute("slice.divide", json!({"slice": id, "horizontal": 3, "vertical": 2})).unwrap();
    assert_eq!(r["count"], 6);
    assert_eq!(doc(&s).slices.list.len(), 7);
    let total: u64 = doc(&s).slices.list.iter().take(6).map(|s| u64::from(s.rect.width()) * u64::from(s.rect.height())).sum();
    assert_eq!(total, 60 * 45);
    assert!(s.execute("slice.divide", json!({"slice": id})).is_err());
}

#[test]
fn slices_from_guides_and_clear() {
    let mut s = session(8);
    s.edit("guides", |d, _| {
        d.guides.vertical = vec![40.0, 80.0];
        d.guides.horizontal = vec![30.0];
        Ok(())
    })
    .unwrap();
    s.slice_from_guides_check();
    assert!(s.is_enabled("view.clearSlices"));
    let r = s.execute("view.clearSlices", json!({})).unwrap();
    assert_eq!(r["cleared"], 6);
    assert!(doc(&s).slices.is_empty());
    assert!(!s.is_enabled("view.clearSlices"));
}

impl Session {
    fn slice_from_guides_check(&mut self) {
        let r = self.execute("slice.fromGuides", json!({})).unwrap();
        assert_eq!(r["slices"], 6);
        let rects: Vec<Rect> = self.active().unwrap().doc.slices.list.iter().map(|s| s.rect).collect();
        assert!(rects.contains(&Rect::new(40, 30, 80, 90)));
    }
}

#[test]
fn lock_slices_blocks_editing() {
    let mut s = session(8);
    s.execute("slice.new", json!({"rect": [0, 0, 10, 10]})).unwrap();
    assert_eq!(s.execute("view.lockSlices", json!({})).unwrap()["locked"], true);
    assert!(!s.is_enabled("slice.new"));
    assert!(!s.is_enabled("view.clearSlices"));
    assert!(s.execute("slice.new", json!({"rect": [0, 0, 10, 10]})).is_err());
    s.execute("view.lockSlices", json!({"on": false})).unwrap();
    assert!(s.is_enabled("slice.new"));
}

#[test]
fn layer_based_slice_follows_the_layer_and_its_effects() {
    for depth in [8, 16] {
        let mut s = session(depth);
        assert!(!s.is_enabled("layer.newLayerBasedSlice"), "Background layer");
        let id = add_square(&mut s, Rect::new(10, 10, 30, 20));
        let r = s.execute("layer.newLayerBasedSlice", json!({})).unwrap();
        assert_eq!(r["rect"], json!([10, 10, 20, 10]));
        assert!(!s.is_enabled("layer.newLayerBasedSlice"), "one per layer");
        let sid = r["slice"].as_u64().unwrap() as u32;
        // Moving the layer moves the slice.
        s.execute("layer.translate", json!({"layer": id.0, "dx": 5, "dy": 7})).unwrap();
        assert_eq!(doc(&s).slices.get(sid).unwrap().rect, Rect::new(15, 17, 35, 27));
        // A drop shadow grows it.
        s.edit("fx", |d, _| {
            let l = d.layer_mut(id).unwrap();
            l.effects.enabled = true;
            l.effects.items.push(Effect::default_drop_shadow());
            Ok(())
        })
        .unwrap();
        s.execute("slice.list", json!({})).unwrap();
        let grown = doc(&s).slices.get(sid).unwrap().rect;
        assert!(grown.contains_rect(&Rect::new(15, 17, 35, 27)) && grown != Rect::new(15, 17, 35, 27), "{grown:?}");
        // Deleting the layer deletes its slice.
        s.execute("layer.delete", json!({"layer": id.0})).unwrap();
        assert!(doc(&s).slices.get(sid).is_none());
    }
}

#[test]
fn slices_round_trip_through_psd_and_pcraft() {
    let mut s = session(8);
    add_square(&mut s, Rect::new(50, 40, 70, 60));
    s.execute("layer.newLayerBasedSlice", json!({})).unwrap();
    s.execute("slice.new", json!({"rect": [5, 5, 30, 20], "name": "hero", "url": "https://example.org", "alt": "Hero", "target": "_blank"})).unwrap();
    let d = doc(&s).clone();
    for ext in ["psd", "pcraft"] {
        let bytes = photocraft_io::export(&d, &format!("x.{ext}"), &Default::default()).unwrap().bytes;
        let back = photocraft_io::import(&format!("x.{ext}"), &bytes).unwrap().document;
        assert_eq!(back.slices.list.len(), 2, "{ext}");
        let hero = back.slices.list.iter().find(|s| s.name == "hero").unwrap();
        assert_eq!((hero.url.as_str(), hero.alt.as_str(), hero.target.as_str()), ("https://example.org", "Hero", "_blank"));
        assert_eq!(hero.rect, Rect::new(5, 5, 35, 25));
        let ls = back.slices.list.iter().find(|s| s.origin == SliceOrigin::Layer).unwrap();
        let layer = back.layer(ls.layer.unwrap()).unwrap();
        assert_eq!(layer.name, "Layer 1", "{ext}: the slice still names its layer");
        assert_eq!(ls.rect, Rect::new(50, 40, 70, 60));
    }
}

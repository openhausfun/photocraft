//! JSON views of engine state for automation, tests and debugging.

use photocraft_doc::{Layer, LayerContent};
use serde_json::{Value, json};

use crate::{DocState, Session};

pub fn session(s: &Session) -> Value {
    json!({
        "active": s.active_index(),
        "documents": s.documents().iter().enumerate().map(|(i, d)| json!({
            "index": i,
            "name": d.doc.name,
            "path": d.path,
            "width": d.doc.size.width,
            "height": d.doc.size.height,
            "dirty": d.is_dirty(),
            "revision": d.revision,
        })).collect::<Vec<_>>(),
        "foreground": s.tools.foreground,
        "background": s.tools.background,
    })
}

pub fn document(d: &DocState) -> Value {
    let doc = &d.doc;
    let selected = d.selected_layers();
    json!({
        "name": doc.name,
        "width": doc.size.width,
        "height": doc.size.height,
        "mode": format!("{:?}", doc.mode),
        "depth": doc.depth.bits(),
        "resolution": doc.resolution_dpi,
        "activeLayer": d.active_layer.map(|l| l.0),
        "selectedLayers": selected.iter().map(|l| l.0).collect::<Vec<_>>(),
        "isolatedLayers": d.isolated_layers.iter().map(|l| l.0).collect::<Vec<_>>(),
        "hasSelection": doc.selection.is_some(),
        "selectionBounds": doc.selection.as_ref().map(|s| { let r = s.content_bounds(); [r.x0, r.y0, r.width() as i32, r.height() as i32] }),
        "layers": doc.layers.iter().rev().map(|l| layer_sel(l, &selected)).collect::<Vec<_>>(),
        "history": d.history.entries(),
        "canUndo": d.history.can_undo(),
        "canRedo": d.history.can_redo(),
        "revision": d.revision,
        "channels": crate::channel_cmds::channels_json(d),
        "quickMask": doc.quick_mask.is_some(),
        "layerMaskView": crate::mask_view_cmds::view_json(d),
        "layerComps": doc.layer_comps.iter().map(|c| json!({"id": c.id, "name": c.name})).collect::<Vec<_>>(),
        "lastAppliedComp": doc.last_applied_comp,
        "measurement": {"scale": doc.measurement.scale.describe(), "ruler": doc.measurement.ruler.map(|r| json!({"start": r.start, "end": r.end, "protractor": r.protractor})), "count": doc.measurement.count_total(), "countGroups": doc.measurement.count_groups.len()},
        "notes": doc.notes.iter().map(|n| json!({"author": n.author, "text": n.text, "position": n.position})).collect::<Vec<_>>(),
        "timeline": doc.timeline.as_ref().map(|t| json!({"fps": t.fps, "duration": t.duration, "current": t.current})),
        "variables": (!doc.variables.is_empty()).then(|| json!({
            "defs": doc.variables.defs.iter().map(|vd| json!({"name": vd.name, "layer": vd.layer.0})).collect::<Vec<_>>(),
            "dataSets": doc.variables.data_sets.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "active": doc.variables.active,
        })),
    })
}

/// Layer tree top-to-bottom (display order).
pub fn layer(l: &Layer) -> Value {
    layer_sel(l, &[])
}

/// [`layer`] with a `selected` flag on every node (Layers panel multi-selection).
fn layer_sel(l: &Layer, selected: &[photocraft_doc::LayerId]) -> Value {
    let bounds = l.surface().map(|s| {
        let r = s.content_bounds();
        [r.x0, r.y0, r.width() as i32, r.height() as i32]
    });
    let mut v = json!({
        "id": l.id.0,
        "name": l.name,
        "kind": l.content.kind_name(),
        "visible": l.visible,
        "opacity": l.opacity,
        "fill": l.fill_opacity,
        "blend": l.blend.label(),
        "clipped": l.clipped,
        "hasMask": l.mask.is_some(),
        "bounds": bounds,
        "selected": selected.contains(&l.id),
        "linkGroup": l.link_group,
    });
    match &l.content {
        LayerContent::Group(g) => {
            v["children"] = Value::Array(g.children.iter().rev().map(|c| layer_sel(c, selected)).collect());
            v["expanded"] = json!(g.expanded);
            if let Some(a) = &g.artboard {
                v["artboard"] = json!({"rect": [a.rect.x0, a.rect.y0, a.rect.width(), a.rect.height()], "background": a.background.name(), "preset": a.preset});
            }
        }
        LayerContent::Adjustment(a) => {
            v["adjustment"] = serde_json::to_value(a).unwrap_or(Value::Null);
        }
        LayerContent::Smart(sm) => {
            v["smartFilters"] = Value::Array(
                sm.smart_filters
                    .iter()
                    .map(|f| json!({"command": f.command, "params": f.params, "visible": f.visible, "opacity": f.opacity, "blend": f.blend.label()}))
                    .collect(),
            );
            v["smartFiltersEnabled"] = json!(sm.filters_enabled);
        }
        LayerContent::Text(t) => {
            v["text"] = json!({"text": t.text, "font": t.font_family, "sizePt": t.size_pt});
        }
        _ => {}
    }
    // Advanced Blending › Channels (only when some channel is left out).
    if l.excluded_channels != 0 {
        v["channels"] = json!((0..4).map(|i| l.excluded_channels & (1 << i) == 0).collect::<Vec<_>>());
    }
    // Blending Options › Blend If (only when set): range index (0 = Gray, then the mode's
    // channels) with This Layer / Underlying Layer as [blackLo, blackHi, whiteLo, whiteHi].
    if !l.blend_if.is_default() {
        v["blendIf"] = l
            .blend_if
            .ranges
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.iter().all(photocraft_doc::BlendRange::is_full))
            .map(|(i, [this, under])| json!({"channel": i, "thisLayer": this.to_bytes(), "underlying": under.to_bytes()}))
            .collect();
    }
    // Layer styles, so agents can verify what they applied (full settings via the style commands).
    if !l.effects.items.is_empty() {
        v["effects"] = json!({
            "enabled": l.effects.enabled,
            "items": l.effects.items.iter().map(|e| json!({"kind": e.label(), "enabled": e.enabled()})).collect::<Vec<_>>(),
        });
    }
    // Video layer (Layer › Video Layers): the frame stack behind the displayed content.
    if let Some(vid) = &l.video {
        let source = match &vid.source {
            photocraft_doc::VideoSource::Blank => Value::String("blank".into()),
            photocraft_doc::VideoSource::File { path } => json!({ "file": path }),
        };
        v["video"] = json!({"frames": vid.frames.len(), "fps": vid.fps, "showAltered": vid.show_altered, "source": source});
    }
    v
}

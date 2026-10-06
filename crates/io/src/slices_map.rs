//! Document slices ↔ PSD resource 1050 (see `photocraft_psd::slices`).
//!
//! Import keeps user and layer-based slices (auto slices are regenerated from them) and links
//! layer-based slices to the layer whose `lyid` they name. Export writes the preserved resource
//! byte-for-byte while the slices are unchanged, else a fresh version 6 resource that also lists
//! the current auto slices, as Photoshop saves them.

use std::collections::HashMap;

use photocraft_doc::slices::{self, Slice, SliceKind, SliceOrigin, Slices};
use photocraft_doc::{Document, LayerId, Rect};
use photocraft_psd::slices::{SliceRecord, SlicesResource};

/// Image resource id of the slices.
pub const SLICES: u16 = photocraft_psd::slices::SLICES;

fn raw(doc: &Document) -> Option<&[u8]> {
    doc.metadata.psd_resources.iter().find(|(id, _, _)| *id == SLICES).map(|(_, _, d)| d.as_slice())
}

/// The document's slices from the raw resource (None or unreadable = no slices).
pub fn slices_from_psd(raw: Option<&[u8]>, doc: &Document) -> Slices {
    let Some(res) = raw.and_then(|r| SlicesResource::from_bytes(r).ok()) else { return Slices::default() };
    let by_psd_id: HashMap<u32, LayerId> = doc.walk().into_iter().filter_map(|(_, _, l)| l.psd_id.map(|p| (p, l.id))).collect();
    let list = res
        .slices
        .iter()
        .filter(|s| s.origin != 0)
        .map(|s| {
            let layer = s.layer_id.and_then(|p| by_psd_id.get(&p).copied()).filter(|_| s.origin == 1);
            Slice {
                id: s.id,
                group_id: s.group_id,
                // A layer-based slice whose layer is gone keeps its rect as a user slice.
                origin: if s.origin == 1 && layer.is_some() { SliceOrigin::Layer } else { SliceOrigin::User },
                layer,
                name: s.name.clone(),
                rect: Rect::new(s.rect[0], s.rect[1], s.rect[2], s.rect[3]),
                kind: SliceKind::from_code(s.kind),
                url: s.url.clone(),
                target: s.target.clone(),
                message: s.message.clone(),
                alt: s.alt.clone(),
                cell_text_is_html: s.cell_text_is_html,
                cell_text: s.cell_text.clone(),
                horizontal_align: s.horizontal_align,
                vertical_align: s.vertical_align,
                background: (s.color[0] != 0).then_some(s.color),
                outsets: s.outsets,
            }
        })
        .collect();
    Slices { group_name: res.group_name, list }
}

/// Sets `doc.slices` from its preserved resource (called after layers and ids are imported).
pub fn import(doc: &mut Document) {
    doc.slices = slices_from_psd(raw(doc), doc);
}

/// Do the document's slices still match the preserved resource?
pub fn unchanged(doc: &Document) -> bool {
    slices_from_psd(raw(doc), doc) == doc.slices
}

/// The resource to write: `None` keeps the preserved one, `Some(None)` drops it, `Some(Some)` is
/// a fresh resource. `layer_ids` maps document layers to the PSD layer ids being written.
pub fn export_resource(doc: &Document, layer_ids: &HashMap<LayerId, u32>) -> Option<Option<Vec<u8>>> {
    if unchanged(doc) {
        return None;
    }
    if doc.slices.is_empty() {
        return Some(None);
    }
    let mut records: Vec<SliceRecord> = doc
        .slices
        .list
        .iter()
        .map(|s| SliceRecord {
            id: s.id,
            group_id: s.group_id,
            origin: s.origin.code(),
            layer_id: s.layer.and_then(|l| layer_ids.get(&l).copied()),
            name: s.name.clone(),
            kind: s.kind.code(),
            rect: [s.rect.x0, s.rect.y0, s.rect.x1, s.rect.y1],
            url: s.url.clone(),
            target: s.target.clone(),
            message: s.message.clone(),
            alt: s.alt.clone(),
            cell_text_is_html: s.cell_text_is_html,
            cell_text: s.cell_text.clone(),
            horizontal_align: s.horizontal_align,
            vertical_align: s.vertical_align,
            color: s.background.unwrap_or([0; 4]),
            outsets: s.outsets,
        })
        .map(|mut r| {
            // A layer slice whose layer isn't written becomes a user slice.
            if r.origin == 1 && r.layer_id.is_none() {
                r.origin = 2;
            }
            r
        })
        .collect();
    let taken: Vec<Rect> = doc.slices.list.iter().map(|s| s.rect).collect();
    for (id, r) in (doc.slices.next_id()..).zip(slices::auto_slices(doc.bounds(), &taken)) {
        records.push(SliceRecord { id, origin: 0, kind: 1, rect: [r.x0, r.y0, r.x1, r.y1], ..Default::default() });
    }
    let group_name = if doc.slices.group_name.is_empty() { slices::base_name(doc) } else { doc.slices.group_name.clone() };
    let res = SlicesResource { version: 6, bounds: [0, 0, doc.size.height as i32, doc.size.width as i32], group_name, slices: records };
    Some(Some(res.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{ColorMode, Layer, SampleType, Size};

    #[test]
    fn slices_survive_a_psd_round_trip() {
        let mut d = Document::new("site.psd", Size::new(120, 90), ColorMode::Rgb, SampleType::U8);
        let mut l = Layer::raster("Logo", d.pixel_format());
        l.psd_id = Some(5);
        let lid = l.id;
        d.layers.push(l);
        d.slices.list.push(Slice {
            id: 1,
            name: "hero".into(),
            rect: Rect::new(10, 10, 60, 40),
            url: "https://example.org".into(),
            alt: "Hero".into(),
            ..Default::default()
        });
        d.slices.list.push(Slice {
            id: 2,
            origin: SliceOrigin::Layer,
            layer: Some(lid),
            rect: Rect::new(70, 50, 100, 80),
            kind: SliceKind::NoImage,
            outsets: [1, 1, 1, 1],
            ..Default::default()
        });
        let ids: HashMap<LayerId, u32> = [(lid, 5)].into_iter().collect();
        let data = export_resource(&d, &ids).unwrap().unwrap();
        // Read back into a copy of the document without slices.
        let mut back = d.clone();
        back.slices = Slices::default();
        back.metadata.psd_resources.push((SLICES, String::new(), std::sync::Arc::new(data)));
        import(&mut back);
        assert_eq!(back.slices.list, d.slices.list);
        assert!(unchanged(&back));
        assert!(export_resource(&back, &ids).is_none(), "unchanged → keep the raw resource");
    }
}

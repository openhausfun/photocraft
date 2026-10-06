//! Icon rendering. SVGs are embedded (see `icon_data.rs`), recoloured to white, rasterized by the
//! egui_extras SVG loader at the exact on-screen pixel size (crisp at any DPI), and tinted per use.
//!
//! (History: the first shell used Unicode glyphs; half of them rendered as tofu boxes because the
//! bundled fonts lacked them. Vector icons fix that for good.)

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use egui::{Color32, Rect, Response, Sense, Vec2};

use crate::icon_data::ICONS;
use crate::state::Tool;
use crate::theme::Tokens;

fn white_icons() -> &'static HashMap<&'static str, Arc<[u8]>> {
    static MAP: OnceLock<HashMap<&'static str, Arc<[u8]>>> = OnceLock::new();
    MAP.get_or_init(|| {
        ICONS
            .iter()
            .map(|(name, bytes)| {
                let svg = String::from_utf8_lossy(bytes).replace("currentColor", "#ffffff").replace("stroke-width=\"2\"", "stroke-width=\"1.75\"");
                (*name, Arc::from(svg.into_bytes().into_boxed_slice()))
            })
            .collect()
    })
}

pub fn exists(name: &str) -> bool {
    white_icons().contains_key(name)
}

/// An egui image for an icon, tinted.
pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
    let bytes = white_icons().get(name).or_else(|| white_icons().get("square")).cloned().unwrap_or_default();
    egui::Image::from_bytes(format!("bytes://icons/{name}.svg"), egui::load::Bytes::Shared(bytes)).fit_to_exact_size(Vec2::splat(size)).tint(tint)
}

/// Paint an icon centred in `rect`.
pub fn paint(ui: &egui::Ui, rect: Rect, name: &str, size: f32, tint: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(size));
    image(name, size, tint).paint_at(ui, r);
}

pub fn tool_icon(t: Tool) -> &'static str {
    match t {
        Tool::Move => "move",
        Tool::RectMarquee => "square-dashed",
        Tool::EllipseMarquee => "circle-dashed",
        Tool::Brush => "brush",
        Tool::Pencil => "pencil",
        Tool::Eraser => "eraser",
        Tool::BackgroundEraser => "eraser-background",
        Tool::MagicEraser => "eraser-magic",
        Tool::Eyedropper => "pipette",
        Tool::Ruler => "ruler",
        Tool::Note => "message-square",
        Tool::Count => "circle-dot",
        Tool::Lasso => "lasso",
        Tool::PolygonLasso => "pentagon",
        Tool::MagicWand => "wand-sparkles",
        Tool::Crop => "crop",
        Tool::Slice => "slice-knife",
        Tool::SliceSelect => "square-dashed-mouse-pointer",
        Tool::Gradient => "blend",
        Tool::PaintBucket => "paint-bucket",
        Tool::Type => "type",
        Tool::Hand => "hand",
        Tool::Zoom => "zoom-in",
        Tool::SpotHealing | Tool::Healing => "bandage",
        Tool::CloneStamp => "stamp",
        Tool::HistoryBrush => "clock",
        Tool::Blur => "droplet",
        Tool::Sharpen => "triangle",
        Tool::Smudge => "pointer",
        Tool::Dodge => "lollipop",
        Tool::Burn => "flame",
        Tool::Sponge => "cloud",
        Tool::QuickSelection => "circle-dashed",
        Tool::ObjectSelection => "square-dashed-mouse-pointer",
        Tool::Pen => "pen-tool",
        Tool::PathSelection => "mouse-pointer-2",
        Tool::Rectangle => "rectangle-horizontal",
        Tool::EllipseShape => "circle",
        Tool::Triangle => "triangle",
        Tool::Polygon => "pentagon",
        Tool::Line => "slash",
        Tool::CustomShape => "cloud",
    }
}

/// Square icon button: transparent until hovered; `selected` gets the accent treatment.
pub fn button(ui: &mut egui::Ui, name: &str, box_size: f32, selected: bool, tooltip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(box_size), Sense::click());
    let hovered = resp.hovered();
    if selected {
        ui.painter().rect_filled(rect, t.radius_sm, t.accent_soft);
        ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.accent_border), egui::StrokeKind::Inside);
    } else if hovered {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    let tint = if selected {
        t.accent_text
    } else if hovered {
        t.text
    } else {
        t.icon
    };
    paint(ui, rect, name, (box_size * 0.52).round(), tint);
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tl!(tooltip)) }
}

/// Rail toggle: "on" gets a quiet filled background and full-strength icon (no accent).
pub fn rail_button(ui: &mut egui::Ui, name: &str, box_size: f32, on: bool, tooltip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(box_size), Sense::click());
    if on {
        ui.painter().rect_filled(rect, t.radius_sm, t.card);
        ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    let tint = if on || resp.hovered() { t.text } else { t.text_faint };
    paint(ui, rect, name, (box_size * 0.52).round(), tint);
    resp.on_hover_text(tl!(tooltip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_icon() {
        for t in Tool::ALL {
            assert!(exists(tool_icon(t)), "{t:?}");
        }
    }

    #[test]
    fn icons_are_recoloured() {
        let m = white_icons();
        assert!(m.len() >= 60);
        for (name, b) in m.iter() {
            let s = std::str::from_utf8(b).unwrap();
            assert!(!s.contains("currentColor"), "{name}");
        }
    }
}

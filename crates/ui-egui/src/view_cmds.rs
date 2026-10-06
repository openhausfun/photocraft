//! Shell-side View, Window and Type menu items: screen modes, Extras and the Show / Snap To
//! toggles, zoom presets, view flipping, document arrangement (tiles, n-up, floating windows,
//! Match Zoom/Location), panel shortcuts that pick the right dock tab, type UI preferences, and
//! the picker/dialog front ends of the engine's File commands.
//!
//! Everything here is view or window state ([`ViewOptions`], serde, part of `UiState`, so the
//! control channel reads and drives it). Anything that changes a document is an engine command;
//! this module only opens its dialog or file picker first.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;

/// View › Show flags (Photoshop defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Show {
    pub layer_edges: bool,
    pub selection_edges: bool,
    pub target_path: bool,
    pub pixel_grid: bool,
    pub smart_guides: bool,
    pub brush_preview: bool,
    pub slices: bool,
    pub notes: bool,
    pub mesh: bool,
    pub edit_pins: bool,
    pub count: bool,
    pub canvas_guides: bool,
    pub artboard_guides: bool,
}

impl Default for Show {
    fn default() -> Self {
        Self {
            layer_edges: false,
            selection_edges: true,
            target_path: true,
            pixel_grid: true,
            smart_guides: true,
            brush_preview: true,
            slices: true,
            notes: true,
            mesh: false,
            edit_pins: true,
            count: true,
            canvas_guides: true,
            artboard_guides: true,
        }
    }
}

/// View › Snap To targets (snapping itself is the `view.snap` master switch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapTo {
    pub guides: bool,
    pub grid: bool,
    pub layers: bool,
    pub slices: bool,
    pub document_bounds: bool,
}

impl Default for SnapTo {
    fn default() -> Self {
        Self { guides: true, grid: true, layers: true, slices: true, document_bounds: true }
    }
}

/// View-, window- and type-preference state of the shell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOptions {
    /// "standard" | "fullScreenWithMenuBar" | "fullScreen" (View › Screen Mode, F cycles).
    pub screen_mode: String,
    /// View › Extras (⌘H): master switch for guides, grid, edges and other overlays.
    pub extras: bool,
    pub show: Show,
    pub snap_to: SnapTo,
    /// View › Flip Horizontal: mirror the view only (the document is untouched).
    pub flip_horizontal: bool,
    /// View › Pixel Aspect Ratio preset id ("square", "d1DvNtsc", …) and its correction toggle.
    pub pixel_aspect: String,
    pub pixel_aspect_correction: bool,
    pub pattern_preview: bool,
    pub pixel_art_preview: bool,
    /// Window › Arrange layout of the document area: "tabs" or a tile/n-up layout id.
    pub arrange: String,
    /// Type › Font Preview Size: "small" | "medium" | "large" | "extraLarge" | "huge".
    pub font_preview_size: String,
    /// Type › Language Options: "defaultFeatures" | "eastAsianFeatures" | "middleEasternFeatures",
    /// and the Middle Eastern & South Asian composer.
    pub language_features: String,
    pub middle_eastern_composer: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            screen_mode: "standard".into(),
            extras: true,
            show: Show::default(),
            snap_to: SnapTo::default(),
            flip_horizontal: false,
            pixel_aspect: "square".into(),
            pixel_aspect_correction: false,
            pattern_preview: false,
            pixel_art_preview: false,
            arrange: "tabs".into(),
            font_preview_size: "medium".into(),
            language_features: "defaultFeatures".into(),
            middle_eastern_composer: false,
        }
    }
}

impl ViewOptions {
    /// Is an overlay with this Show flag visible (Extras on and the flag on)?
    pub fn shows(&self, flag: bool) -> bool {
        self.extras && flag
    }
    pub fn hides_chrome(&self) -> bool {
        self.screen_mode == "fullScreen"
    }
    pub fn hides_tabs(&self) -> bool {
        self.screen_mode != "standard"
    }
}

const SCREEN_MODES: [&str; 3] = ["standard", "fullScreenWithMenuBar", "fullScreen"];
const PIXEL_ASPECTS: [&str; 8] = ["square", "d1DvNtsc", "d1DvPal", "d1DvNtscWidescreen", "hdv1080", "d1DvPalWidescreen", "dvcproHd1080", "anamorphic2To1"];
const FONT_PREVIEW: [&str; 5] = ["small", "medium", "large", "extraLarge", "huge"];
const LANGUAGE: [&str; 3] = ["defaultFeatures", "eastAsianFeatures", "middleEasternFeatures"];
/// Window › Arrange layouts drawn by [`cells`].
const LAYOUTS: [&str; 10] = [
    "tileAllVertically",
    "tileAllHorizontally",
    "twoUpVertical",
    "twoUpHorizontal",
    "threeUpVertical",
    "threeUpHorizontal",
    "threeUpStacked",
    "fourUp",
    "sixUp",
    "tile",
];

/// Pixel aspect ratio (width / height of a pixel) of a preset id.
pub fn pixel_aspect_ratio(id: &str) -> f32 {
    // View › Pixel Aspect Ratio › Custom: `custom:<ratio>:<name>` (see `workspace_ui`).
    if let Some(r) = id.strip_prefix("custom:").and_then(|r| r.split(':').next()?.parse::<f32>().ok()) {
        return r;
    }
    match id {
        "d1DvNtsc" => 0.91,
        "d1DvPal" => 1.09,
        "d1DvNtscWidescreen" => 1.21,
        "hdv1080" => 1.33,
        "d1DvPalWidescreen" => 1.46,
        "dvcproHd1080" => 1.5,
        "anamorphic2To1" => 2.0,
        _ => 1.0,
    }
}

fn show_slot<'a>(s: &'a mut Show, key: &str) -> Option<&'a mut bool> {
    Some(match key {
        "layerEdges" => &mut s.layer_edges,
        "selectionEdges" => &mut s.selection_edges,
        "targetPath" => &mut s.target_path,
        "pixelGrid" => &mut s.pixel_grid,
        "smartGuides" => &mut s.smart_guides,
        "brushPreview" => &mut s.brush_preview,
        "slices" => &mut s.slices,
        "notes" => &mut s.notes,
        "mesh" => &mut s.mesh,
        "editPins" => &mut s.edit_pins,
        "count" => &mut s.count,
        "canvasGuides" => &mut s.canvas_guides,
        "artboardGuides" => &mut s.artboard_guides,
        _ => return None,
    })
}

fn snap_slot<'a>(s: &'a mut SnapTo, key: &str) -> Option<&'a mut bool> {
    Some(match key {
        "guides" => &mut s.guides,
        "grid" => &mut s.grid,
        "layers" => &mut s.layers,
        "slices" => &mut s.slices,
        "documentBounds" => &mut s.document_bounds,
        _ => return None,
    })
}

/// Window › <panel>: (panel, dock tab) for panels that live as tabs of a dock card.
fn panel_tab(app: &PhotocraftApp, id: &str) -> Option<(&'static str, usize)> {
    let pro = matches!(app.ui.theme, crate::theme::ThemeKind::Pro | crate::theme::ThemeKind::ProMedium);
    Some(match id {
        "window.panel.info" => ("navigator", 2),
        "window.panel.histogram" => ("navigator", 1),
        "window.panel.navigator" => ("navigator", 0),
        "window.panel.actions" => ("history", 1),
        "window.panel.layerComps" => ("history", 2),
        "window.panel.history" => ("history", 0),
        "window.panel.channels" => ("layers", 1),
        "window.panel.paths" => ("layers", 2),
        "window.panel.layers" => ("layers", 0),
        "window.panel.adjustments" => ("properties", 1),
        "window.panel.properties" => ("properties", 0),
        // Their own Character | Paragraph group, so Properties stays open (#150).
        "window.panel.character" | "type.panels.character" => ("character", 0),
        "window.panel.paragraph" | "type.panels.paragraph" => ("character", 1),
        "window.panel.swatches" => ("color", usize::from(pro)),
        "window.panel.color" => ("color", usize::from(!pro)),
        _ => return None,
    })
}

fn panel_state<'a>(app: &'a mut PhotocraftApp, panel: &str) -> (&'a mut bool, &'a mut usize) {
    let (p, t) = (&mut app.ui.panels, &mut app.ui.dock_tabs);
    match panel {
        "navigator" => (&mut p.navigator, &mut t.navigator),
        "history" => (&mut p.history, &mut t.history),
        "layers" => (&mut p.layers, &mut t.layers),
        "color" => (&mut p.color, &mut t.color),
        "character" => (&mut p.character, &mut t.character),
        _ => (&mut p.properties, &mut t.properties),
    }
}

/// Ids handled here (live menu items).
pub fn handles(id: &str) -> bool {
    if let Some(k) = id.strip_prefix("view.show.") {
        return show_slot(&mut Show::default(), k).is_some() || k == "all";
    }
    if let Some(k) = id.strip_prefix("view.snapTo.") {
        return snap_slot(&mut SnapTo::default(), k).is_some() || k == "all" || k == "none";
    }
    if let Some(k) = id.strip_prefix("view.screenMode.") {
        return SCREEN_MODES.contains(&k) || k == "cycle";
    }
    if let Some(k) = id.strip_prefix("view.pixelAspectRatio.") {
        return PIXEL_ASPECTS.contains(&k);
    }
    if let Some(k) = id.strip_prefix("type.fontPreviewSize.") {
        return FONT_PREVIEW.contains(&k);
    }
    if let Some(k) = id.strip_prefix("type.languageOptions.") {
        return LANGUAGE.contains(&k) || k == "middleEasternAndSouthAsianComposer";
    }
    if let Some(k) = id.strip_prefix("window.arrange.") {
        return LAYOUTS.contains(&k)
            || matches!(
                k,
                "consolidateAllToTabs"
                    | "cascade"
                    | "floatInWindow"
                    | "floatAllInWindows"
                    | "newWindowForDocument"
                    | "matchZoom"
                    | "matchLocation"
                    | "matchRotation"
                    | "matchAll"
            );
    }
    matches!(
        id,
        "view.extras"
            | "view.twoHundredPercent"
            | "view.printSize"
            | "view.fitLayersOnScreen"
            | "view.fitArtboardOnScreen"
            | "window.panel.layerComps"
            | "view.flipHorizontal"
            | "view.pixelAspectRatioCorrection"
            | "view.patternPreview"
            | "view.pixelArtPreview"
            | "window.panel.layers"
            | "window.panel.history"
            | "window.panel.navigator"
            | "window.panel.properties"
            | "window.panel.color"
            | "window.panel.actions"
            | "window.panel.channels"
            | "window.panel.paths"
            | "window.panel.character"
            | "window.panel.paragraph"
            | "window.panel.info"
            | "window.panel.histogram"
            | "window.panel.swatches"
            | "window.panel.adjustments"
            | "type.panels.character"
            | "type.panels.paragraph"
    )
}

pub fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    if !handles(id) && !wraps(id) {
        return None;
    }
    let doc = app.session.active().is_some();
    Some(match id {
        "view.screenMode.cycle" => app.ui.text_edit.is_none(),
        "view.twoHundredPercent" | "view.printSize" | "view.fitLayersOnScreen" => doc,
        "view.fitArtboardOnScreen" => app.session.active().is_some_and(|d| d.doc.has_artboards()),
        i if i.starts_with("window.arrange.") => match &i["window.arrange.".len()..] {
            "consolidateAllToTabs" => true,
            "floatInWindow" | "newWindowForDocument" => doc,
            "matchZoom" | "matchLocation" | "matchRotation" | "matchAll" => app.session.documents().len() > 1 || !app.ui.windows.is_empty(),
            _ => {
                app.session.documents().len() > 1
                    || (doc && matches!(&i["window.arrange.".len()..], "floatAllInWindows" | "cascade" | "tile" | "tileAllVertically" | "tileAllHorizontally"))
            }
        },
        i if wraps(i) => app.session.is_enabled(i),
        _ => true,
    })
}

pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    let o = &app.ui.view;
    if let Some(k) = id.strip_prefix("view.show.") {
        let mut s = o.show;
        return show_slot(&mut s, k).map(|b| *b);
    }
    if let Some(k) = id.strip_prefix("view.snapTo.") {
        let mut s = o.snap_to;
        return snap_slot(&mut s, k).map(|b| *b);
    }
    if let Some(k) = id.strip_prefix("view.screenMode.") {
        return SCREEN_MODES.contains(&k).then(|| o.screen_mode == k);
    }
    if let Some(k) = id.strip_prefix("view.pixelAspectRatio.") {
        return PIXEL_ASPECTS.contains(&k).then(|| o.pixel_aspect == k);
    }
    if let Some(k) = id.strip_prefix("type.fontPreviewSize.") {
        return FONT_PREVIEW.contains(&k).then(|| o.font_preview_size == k);
    }
    if let Some(k) = id.strip_prefix("type.languageOptions.") {
        return if k == "middleEasternAndSouthAsianComposer" {
            Some(o.middle_eastern_composer)
        } else {
            LANGUAGE.contains(&k).then(|| o.language_features == k)
        };
    }
    if let Some(k) = id.strip_prefix("window.arrange.")
        && LAYOUTS.contains(&k)
    {
        return Some(o.arrange == k);
    }
    if let Some((panel, tab)) = panel_tab(app, id) {
        let (p, t) = (&app.ui.panels, &app.ui.dock_tabs);
        let (vis, cur) = match panel {
            "navigator" => (p.navigator, t.navigator),
            "history" => (p.history, t.history),
            "layers" => (p.layers, t.layers),
            "color" => (p.color, t.color),
            "character" => (p.character, t.character),
            _ => (p.properties, t.properties),
        };
        return Some(vis && cur == tab);
    }
    if let Some(c) = type_checked(app, id) {
        return Some(c);
    }
    Some(match id {
        "view.extras" => o.extras,
        "view.flipHorizontal" => o.flip_horizontal,
        "view.pixelAspectRatioCorrection" => o.pixel_aspect_correction,
        "view.patternPreview" => o.pattern_preview,
        "view.pixelArtPreview" => o.pixel_art_preview,
        _ => return None,
    })
}

/// Checkmarks of the Type menu's anti-aliasing, orientation and OpenType items.
fn type_checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    use photocraft_doc::LayerContent;
    let st = app.session.active()?;
    let LayerContent::Text(t) = &st.doc.layer(st.active_layer?)?.content else { return None };
    if let Some(k) = id.strip_prefix("type.antiAlias.") {
        return Some(photocraft_engine::type_extra_cmds::aa_name(t.antialias) == k);
    }
    if let Some(k) = id.strip_prefix("type.orientation.") {
        return Some((t.orientation == photocraft_doc::text::Orientation::Vertical) == (k == "vertical"));
    }
    let tag = match id.strip_prefix("type.openType.")? {
        "standardLigatures" => "liga",
        "contextualAlternates" => "calt",
        "discretionaryLigatures" => "dlig",
        "swash" => "swsh",
        "oldstyle" => "onum",
        "stylisticAlternates" => "salt",
        "titlingAlternates" => "titl",
        "ornaments" => "ornm",
        "ordinals" => "ordn",
        "fractions" => "frac",
        _ => return None,
    };
    Some(t.char_runs().first().is_some_and(|r| photocraft_engine::type_extra_cmds::feature_on(&r.style, tag)))
}

/// Engine commands that get a dialog or file picker here when invoked without parameters.
fn wraps(id: &str) -> bool {
    matches!(
        id,
        "file.openAs"
            | "file.saveACopy"
            | "file.placeEmbedded"
            | "file.placeLinked"
            | "file.closeAll"
            | "file.closeOthers"
            | "file.fileInfo"
            | "file.automate.fitImage"
            | "file.automate.conditionalModeChange"
            | "file.automate.batch"
            | "file.scripts.imageProcessor"
            | "file.scripts.loadFilesIntoStack"
            | "file.automate.photomerge"
            | "file.automate.mergeToHdrPro"
            | "file.automate.lensCorrection"
            | "file.export.layersToFiles"
            | "file.export.layerCompsToFiles"
            | "file.export.artboardsToFiles"
            | "file.export.artboardsToPdf"
            | "layer.new.artboard"
            | "file.export.colorLookupTables"
            | "view.newGuideLayout"
            | "type.warpText"
            | "type.pasteLoremIpsum"
    )
}

fn no_params(p: &Value) -> bool {
    p.as_object().is_none_or(|o| o.is_empty())
}

/// Run a View/Window/Type shell command, or front an engine command with its dialog. `None` when
/// `id` isn't ours.
pub fn invoke(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if wraps(id) {
        return front(app, id, params);
    }
    if !handles(id) {
        return None;
    }
    Some(run(app, ctx, id, params))
}

fn flag_param(p: &Value, cur: bool) -> bool {
    p.get("on").and_then(Value::as_bool).unwrap_or(!cur)
}

fn run(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    if let Some((panel, tab)) = panel_tab(app, id) {
        let group = crate::dock::Group::from_key(panel);
        let collapsed = group.is_some_and(|g| app.ui.dock.is_collapsed(g));
        let (vis, cur) = panel_state(app, panel);
        // Like Photoshop: choosing a visible panel's menu item again hides it. A collapsed
        // group is expanded instead, so the menu item always brings the panel back (#129).
        if *vis && *cur == tab && !collapsed && !id.starts_with("type.panels.") {
            *vis = false;
        } else {
            *vis = true;
            *cur = tab;
        }
        let visible = *vis;
        if let Some(g) = group.filter(|_| visible) {
            crate::dock::reveal(app, g);
        }
        return Ok(json!({"panel": panel, "tab": tab, "visible": visible}));
    }
    let o = &mut app.ui.view;
    if let Some(k) = id.strip_prefix("view.show.") {
        if k == "all" {
            o.show = Show { layer_edges: true, mesh: true, ..Show::default() };
            o.extras = true;
            app.ui.extras.grid = true;
            app.ui.extras.guides = true;
            return Ok(json!(true));
        }
        let slot = show_slot(&mut o.show, k).ok_or("unknown Show item")?;
        *slot = flag_param(p, *slot);
        let v = *slot;
        if v {
            // Showing an item turns Extras back on, as in Photoshop.
            o.extras = true;
        }
        return Ok(json!(v));
    }
    if let Some(k) = id.strip_prefix("view.snapTo.") {
        if k == "all" || k == "none" {
            let on = k == "all";
            o.snap_to = SnapTo { guides: on, grid: on, layers: on, slices: on, document_bounds: on };
            return Ok(json!(on));
        }
        let slot = snap_slot(&mut o.snap_to, k).ok_or("unknown Snap To item")?;
        *slot = flag_param(p, *slot);
        return Ok(json!(*slot));
    }
    if let Some(k) = id.strip_prefix("view.screenMode.") {
        let next = if k == "cycle" {
            let i = SCREEN_MODES.iter().position(|m| *m == o.screen_mode).unwrap_or(0);
            SCREEN_MODES[(i + 1) % SCREEN_MODES.len()]
        } else {
            k
        };
        o.screen_mode = next.to_string();
        // Both full-screen modes take the whole display; the menu bar stays in the first.
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(next != "standard"));
        return Ok(json!({"screenMode": next}));
    }
    if let Some(k) = id.strip_prefix("view.pixelAspectRatio.") {
        o.pixel_aspect = k.to_string();
        return Ok(json!({"pixelAspectRatio": pixel_aspect_ratio(k)}));
    }
    if let Some(k) = id.strip_prefix("type.fontPreviewSize.") {
        o.font_preview_size = k.to_string();
        return Ok(json!(k));
    }
    if let Some(k) = id.strip_prefix("type.languageOptions.") {
        if k == "middleEasternAndSouthAsianComposer" {
            o.middle_eastern_composer = flag_param(p, o.middle_eastern_composer);
            return Ok(json!(o.middle_eastern_composer));
        }
        o.language_features = k.to_string();
        return Ok(json!(k));
    }
    if let Some(k) = id.strip_prefix("window.arrange.") {
        return arrange(app, k);
    }
    match id {
        "view.extras" => {
            o.extras = flag_param(p, o.extras);
            Ok(json!(o.extras))
        }
        "view.flipHorizontal" => {
            o.flip_horizontal = flag_param(p, o.flip_horizontal);
            Ok(json!(o.flip_horizontal))
        }
        "view.pixelAspectRatioCorrection" => {
            o.pixel_aspect_correction = flag_param(p, o.pixel_aspect_correction);
            Ok(json!(o.pixel_aspect_correction))
        }
        "view.patternPreview" => {
            o.pattern_preview = flag_param(p, o.pattern_preview);
            Ok(json!(o.pattern_preview))
        }
        "view.pixelArtPreview" => {
            o.pixel_art_preview = flag_param(p, o.pixel_art_preview);
            Ok(json!(o.pixel_art_preview))
        }
        "view.twoHundredPercent" | "view.printSize" => {
            let i = app.session.active_index().ok_or("no document")?;
            // Print Size assumes Photoshop's default 72 ppi screen resolution.
            let dpi = app.session.active().map_or(72.0, |d| d.doc.resolution_dpi.max(1.0));
            let z = if id == "view.twoHundredPercent" { 2.0 } else { 72.0 / dpi };
            app.ui.views[i].zoom = z.clamp(0.01, 64.0);
            Ok(json!({"zoom": app.ui.views[i].zoom}))
        }
        "view.fitLayersOnScreen" => fit_layers(app),
        "view.fitArtboardOnScreen" => crate::artboard_ui::fit_artboard(app),
        _ => Err(format!("unhandled {id}")),
    }
}

/// View › Fit Layer(s) on Screen: zoom and centre on the selected layers' bounds.
fn fit_layers(app: &mut PhotocraftApp) -> Result<Value, String> {
    let i = app.session.active_index().ok_or("no document")?;
    let st = app.session.active().ok_or("no document")?;
    let mut b = photocraft_geom::Rect::EMPTY;
    for id in st.selected_layers() {
        if let Some(r) = st.doc.layer(id).and_then(|l| l.surface()).map(|s| s.content_bounds()) {
            b = if b.is_empty() { r } else { b.union(&r) };
        }
    }
    if b.is_empty() {
        b = st.doc.bounds();
    }
    let area = app.last_canvas_rect.size();
    let area = if area.x > 50.0 { area } else { egui::vec2(1200.0, 800.0) };
    let v = &mut app.ui.views[i];
    v.zoom = ((area.x - 40.0) / b.width().max(1) as f32).min((area.y - 40.0) / b.height().max(1) as f32).clamp(0.01, 64.0);
    v.center = [(b.x0 + b.x1) as f32 / 2.0, (b.y0 + b.y1) as f32 / 2.0];
    v.fit_pending = false;
    Ok(json!({"zoom": v.zoom, "bounds": [b.x0, b.y0, b.x1, b.y1]}))
}

fn float_window(app: &mut PhotocraftApp, doc: usize, offset: usize) -> u64 {
    let wid = app.ui.alloc_id();
    let mut view = app.ui.views.get(doc).cloned().unwrap_or_default();
    view.fit_pending = offset > 0 || view.fit_pending;
    app.ui.windows.push(crate::state::DocWindow { id: wid, document: doc, view, open: true });
    wid
}

fn arrange(app: &mut PhotocraftApp, k: &str) -> Result<Value, String> {
    if LAYOUTS.contains(&k) {
        app.ui.view.arrange = k.to_string();
        // Each document fits its new cell.
        app.ui.views.iter_mut().for_each(|v| v.fit_pending = true);
        return Ok(json!({"arrange": k}));
    }
    match k {
        "consolidateAllToTabs" => {
            app.ui.view.arrange = "tabs".into();
            app.ui.windows.clear();
            Ok(json!({"arrange": "tabs"}))
        }
        "newWindowForDocument" | "floatInWindow" => {
            let doc = app.session.active_index().ok_or("no document")?;
            Ok(json!({"window": float_window(app, doc, 0)}))
        }
        "floatAllInWindows" | "cascade" => {
            let n = app.session.documents().len();
            let ids: Vec<u64> = (0..n).map(|d| float_window(app, d, d)).collect();
            Ok(json!({"windows": ids}))
        }
        "matchZoom" | "matchLocation" | "matchRotation" | "matchAll" => {
            let i = app.session.active_index().ok_or("no document")?;
            let src = app.ui.views[i].clone();
            let (zoom, loc) = (matches!(k, "matchZoom" | "matchAll"), matches!(k, "matchLocation" | "matchAll"));
            let apply = |v: &mut crate::state::View| {
                if zoom {
                    v.zoom = src.zoom;
                }
                if loc {
                    v.center = src.center;
                }
                v.fit_pending = false;
            };
            app.ui.views.iter_mut().for_each(apply);
            app.ui.windows.iter_mut().for_each(|w| apply(&mut w.view));
            // Views never rotate in Photocraft, so Match Rotation has nothing to align.
            Ok(json!({"zoom": src.zoom, "center": src.center, "rotation": 0}))
        }
        _ => Err(format!("unknown arrangement {k}")),
    }
}

/// Screen rectangles for `n` documents in an arrangement (`None` = tabs). The active document
/// always gets a cell; n-up layouts show that many documents starting from the active one.
pub fn cells(layout: &str, rect: egui::Rect, n: usize) -> Option<Vec<egui::Rect>> {
    if n < 2 || !LAYOUTS.contains(&layout) {
        return None;
    }
    let grid = |cols: usize, rows: usize, count: usize| -> Vec<egui::Rect> {
        let (w, h) = (rect.width() / cols as f32, rect.height() / rows as f32);
        (0..count.min(cols * rows))
            .map(|i| egui::Rect::from_min_size(rect.min + egui::vec2((i % cols) as f32 * w, (i / cols) as f32 * h), egui::vec2(w, h)))
            .collect()
    };
    Some(match layout {
        "tileAllVertically" => grid(n, 1, n),
        "tileAllHorizontally" => grid(1, n, n),
        "twoUpVertical" => grid(2, 1, 2),
        "twoUpHorizontal" => grid(1, 2, 2),
        "threeUpVertical" => grid(3, 1, 3),
        "threeUpHorizontal" => grid(1, 3, 3),
        "threeUpStacked" => {
            let half = rect.width() / 2.0;
            let left = egui::Rect::from_min_size(rect.min, egui::vec2(half, rect.height()));
            let r = egui::Rect::from_min_max(egui::pos2(rect.min.x + half, rect.min.y), rect.max);
            let top = egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, r.center().y));
            let bottom = egui::Rect::from_min_max(egui::pos2(r.min.x, r.center().y), r.max);
            vec![left, top, bottom]
        }
        "fourUp" => grid(2, 2, 4),
        "sixUp" => grid(3, 2, 6),
        _ => {
            let cols = (n as f32).sqrt().ceil() as usize;
            grid(cols, n.div_ceil(cols), n)
        }
    })
}

// ---------- dialogs and pickers in front of engine commands ----------

/// A generic form dialog that runs `command` with its fields on OK (rendered by [`form_body`]).
fn form(app: &mut PhotocraftApp, command: &str, label: &str, fields: Value, choices: Value) -> u64 {
    let mut f = Map::new();
    f.insert("__command".into(), json!(command));
    f.insert("__label".into(), json!(label));
    f.insert("__form".into(), json!(true));
    f.insert("__choices".into(), choices);
    if let Value::Object(m) = fields {
        f.extend(m);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

fn label_of(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Body of a `__form` dialog: text fields, number fields, checkboxes and `__choices` dropdowns.
pub fn form_body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let choices = f.get("__choices").cloned().unwrap_or(Value::Null);
    let keys: Vec<String> = f.keys().filter(|k| !k.starts_with("__")).cloned().collect();
    egui::Grid::new("form-dialog").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for k in keys {
            let v = f.get(&k).cloned().unwrap_or(Value::Null);
            let new = match &v {
                Value::Array(_) | Value::Object(_) | Value::Null => continue,
                Value::Bool(b) => {
                    ui.label("");
                    let mut b = *b;
                    ui.checkbox(&mut b, label_of(&k));
                    json!(b)
                }
                Value::String(s) => {
                    ui.label(label_of(&k));
                    let mut s = s.clone();
                    match choices.get(&k).and_then(Value::as_array) {
                        Some(opts) => {
                            let opts: Vec<(String, String)> = opts.iter().filter_map(Value::as_str).map(|o| (o.to_string(), label_of(o))).collect();
                            let refs: Vec<(String, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
                            crate::widgets::dropdown(ui, &format!("form-{k}"), &mut s, &refs, 200.0);
                        }
                        None => {
                            ui.add(egui::TextEdit::singleline(&mut s).desired_width(260.0));
                        }
                    }
                    json!(s)
                }
                Value::Number(n) => {
                    ui.label(label_of(&k));
                    if let Some(mut i) = n.as_i64() {
                        ui.add(egui::DragValue::new(&mut i));
                        json!(i)
                    } else {
                        let mut x = n.as_f64().unwrap_or(0.0);
                        ui.add(egui::DragValue::new(&mut x).speed(0.5));
                        json!(x)
                    }
                }
            };
            f.insert(k, new);
            ui.end_row();
        }
    });
}

/// Folder the batch dialogs start from: next to the active document, else the working directory.
fn default_dir(app: &PhotocraftApp) -> String {
    app.session.active().and_then(|d| d.path.as_deref()).and_then(|p| p.rfind(['/', '\\']).map(|i| p[..i].to_string())).unwrap_or_else(|| ".".into())
}

fn front(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "file.closeAll" || id == "file.closeOthers" {
        // Keep the remaining document's view (views are index-aligned with documents).
        let keep = app.session.active_index().and_then(|i| app.ui.views.get(i).cloned());
        let r = app.run(id, params.clone());
        if r.is_ok() {
            app.ui.views = if id == "file.closeAll" { Vec::new() } else { keep.into_iter().collect() };
            app.ui.windows.clear();
            app.sync_views();
        }
        return Some(r);
    }
    if id == "type.pasteLoremIpsum" && no_params(params) {
        // While typing, paste at the caret.
        if let Some(te) = app.ui.text_edit.clone() {
            let r = app.run(id, json!({"layer": te.layer, "at": te.caret, "coalesce": te.session}));
            if let (Ok(v), Some(te)) = (&r, app.ui.text_edit.as_mut())
                && let Some(c) = v.get("caret").and_then(Value::as_u64)
            {
                te.caret = c as usize;
                te.anchor = c as usize;
            }
            return Some(r);
        }
        return None;
    }
    if !no_params(params) {
        return None;
    }
    let doc = app.session.active().map(|d| (d.doc.size.width, d.doc.size.height, d.doc.name.clone()));
    let dir = default_dir(app);
    let label = photocraft_engine::commands::find(id).map_or(id, |c| c.label);
    let dialog = |app: &mut PhotocraftApp, fields: Value, choices: Value| Some(Ok(json!({"dialog": form(app, id, label, fields, choices)})));
    match id {
        "file.openAs" => {
            app.open_dialog_file();
            Some(Ok(Value::Null))
        }
        "file.saveACopy" => Some(save_a_copy(app)),
        "file.placeEmbedded" | "file.placeLinked" => {
            let (name, bytes) = app.services.pick_open.as_mut().and_then(|f| f())?;
            let linked = (id == "file.placeLinked").then(|| name.clone());
            let r = photocraft_engine::file_cmds::place_bytes(&mut app.session, &name, bytes, linked, &json!({})).map_err(|e| e.to_string());
            app.sync_views();
            Some(r)
        }
        "file.fileInfo" => {
            let info = app.session.execute("file.fileInfo", json!({})).ok()?;
            let kw = info["keywords"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default();
            dialog(
                app,
                json!({"title": info["title"], "author": info["author"], "authorTitle": info["authorTitle"], "description": info["description"], "keywords": kw, "copyright": info["copyright"], "copyrightStatus": info["copyrightStatus"], "copyrightUrl": info["copyrightUrl"]}),
                json!({"copyrightStatus": ["unknown", "copyrighted", "publicDomain"]}),
            )
        }
        "file.automate.fitImage" => {
            let (w, h, _) = doc?;
            dialog(app, json!({"width": w, "height": h, "dontEnlarge": false}), json!({}))
        }
        "file.automate.conditionalModeChange" => dialog(
            app,
            json!({"from": "any", "to": "rgb"}),
            json!({"from": ["any", "rgb", "grayscale", "cmyk", "lab", "indexed", "bitmap", "duotone", "multichannel"], "to": ["rgb", "grayscale", "cmyk", "lab"]}),
        ),
        "view.newGuideLayout" => {
            dialog(app, json!({"columns": 8, "gutter": 20, "rows": 0, "rowGutter": 0, "margin": 0, "centerColumns": false, "clearExisting": false}), json!({}))
        }
        "type.warpText" => {
            let styles: Vec<&str> = std::iter::once("none").chain(photocraft_text::warp::STYLES.iter().map(|(_, s)| *s)).collect();
            dialog(
                app,
                json!({"style": "arc", "orientation": "horizontal", "bend": 50.0, "horizontalDistortion": 0.0, "verticalDistortion": 0.0}),
                json!({"style": styles, "orientation": ["horizontal", "vertical"]}),
            )
        }
        "file.export.layersToFiles" => {
            let (_, _, name) = doc?;
            dialog(
                app,
                json!({"dir": dir, "prefix": name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a), "format": "png", "visibleOnly": true}),
                json!({"format": ["png", "jpg", "psd", "tiff", "webp", "bmp"]}),
            )
        }
        "file.export.layerCompsToFiles" | "file.export.artboardsToFiles" => {
            let (_, _, name) = doc?;
            let prefix = name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a).to_string();
            let fields = if id == "file.export.layerCompsToFiles" {
                json!({"dir": dir, "prefix": prefix, "format": "png", "selectedOnly": false})
            } else {
                json!({"dir": dir, "prefix": prefix, "format": "png"})
            };
            dialog(app, fields, json!({"format": ["png", "jpg", "psd", "tiff", "webp", "bmp"]}))
        }
        "file.export.artboardsToPdf" => {
            let (_, _, name) = doc?;
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a).to_string();
            dialog(app, json!({"path": format!("{dir}/{stem}.pdf"), "quality": 10}), json!({}))
        }
        "layer.new.artboard" => {
            let (w, h, _) = doc?;
            let presets: Vec<&str> = std::iter::once("").chain(photocraft_engine::artboard_cmds::PRESETS.iter().map(|p| p.0)).collect();
            dialog(
                app,
                json!({"name": "", "preset": "", "width": w, "height": h, "background": "white"}),
                json!({"preset": presets, "background": ["white", "black", "transparent"]}),
            )
        }
        "file.export.colorLookupTables" => {
            let (_, _, name) = doc?;
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a).to_string();
            dialog(app, json!({"path": format!("{dir}/{stem}.cube"), "size": 33, "title": stem}), json!({}))
        }
        "file.scripts.loadFilesIntoStack" => dialog(app, json!({"paths": dir}), json!({})),
        // Photography automation (photo_cmds / lens_cmds): a folder (or the open documents).
        "file.automate.photomerge" => dialog(
            app,
            json!({"paths": dir, "useOpenDocuments": false, "layout": "auto", "blend": true, "vignetteRemoval": false, "geometricCorrection": false, "contentAwareFill": false}),
            json!({"layout": ["auto", "perspective", "cylindrical", "spherical", "collage", "reposition"]}),
        ),
        "file.automate.mergeToHdrPro" => dialog(
            app,
            json!({"paths": dir, "useOpenDocuments": false, "align": true, "removeGhosts": false, "mode": "32", "method": "localAdaptation", "radius": 7.0, "strength": 0.52, "gamma": 1.0, "exposure": 0.0, "detail": 30.0, "saturation": 20.0}),
            json!({"mode": ["32", "16", "8"], "method": ["localAdaptation", "exposureGamma", "highlightCompression", "equalizeHistogram"]}),
        ),
        "file.automate.lensCorrection" => dialog(
            app,
            json!({"input": dir, "output": format!("{dir}/corrected"), "format": "same", "profile": "auto", "correctDistortion": true, "correctVignette": true, "correctCA": true, "autoScale": true, "edge": "transparency"}),
            json!({"format": ["same", "png", "jpg", "psd", "tiff"], "profile": ["auto", "generic", "none"], "edge": ["transparency", "edgeExtension", "black", "white"]}),
        ),
        "file.scripts.imageProcessor" => dialog(
            app,
            json!({"input": dir, "output": format!("{dir}/processed"), "format": "jpg", "quality": 8, "width": 0, "height": 0, "convertToSrgb": true}),
            json!({"format": ["jpg", "png", "psd", "tiff"]}),
        ),
        "file.automate.batch" => {
            let a = &app.ui.actions;
            let action = a.selected.and_then(|i| a.list.get(i)).or(a.list.first());
            let Some(action) = action else {
                return Some(Err("record an action in the Actions panel first".into()));
            };
            let steps: Vec<Value> = action.steps.iter().map(|(id, p)| json!([id, p])).collect();
            let name = action.name.clone();
            dialog(
                app,
                json!({"action": name, "steps": steps, "input": dir, "output": format!("{dir}/batch"), "format": "same"}),
                json!({"format": ["same", "png", "jpg", "psd", "tiff"]}),
            )
        }
        // Channel workflow dialogs (see channels_panel.rs for the channel references).
        "select.saveSelection" | "select.loadSelection" | "image.applyImage" | "image.calculations" => {
            let st = app.session.active()?;
            let d = &st.doc;
            let mode = d.pixel_format().mode;
            let mut chans: Vec<String> = vec![photocraft_engine::channel_cmds::composite_name(mode).to_string()];
            if mode.color_channels() > 1 {
                chans.extend(photocraft_engine::channel_cmds::color_names(mode).iter().map(|n| n.to_string()));
            }
            chans.extend(d.channels.iter().map(|c| c.name.clone()));
            let alphas: Vec<String> = std::iter::once("new".to_string()).chain(d.channels.iter().map(|c| c.name.clone())).collect();
            let blends = [
                "multiply",
                "screen",
                "normal",
                "overlay",
                "softLight",
                "hardLight",
                "darken",
                "lighten",
                "colorDodge",
                "colorBurn",
                "linearBurn",
                "linearDodge",
                "difference",
                "exclusion",
                "add",
                "subtract",
            ];
            let mut load = chans.clone();
            load.extend(["transparency", "mask", "selection"].map(String::from));
            match id {
                "select.saveSelection" => dialog(
                    app,
                    json!({"channel": "new", "name": "", "operation": "new"}),
                    json!({"channel": alphas, "operation": ["new", "replace", "add", "subtract", "intersect"]}),
                ),
                "select.loadSelection" => {
                    let first = d.channels.first().map_or_else(|| chans[0].clone(), |c| c.name.clone());
                    dialog(
                        app,
                        json!({"channel": first, "invert": false, "operation": "new"}),
                        json!({"channel": load, "operation": ["new", "add", "subtract", "intersect"]}),
                    )
                }
                "image.applyImage" => {
                    let mut masks = vec!["none".to_string()];
                    masks.extend(load.iter().cloned());
                    dialog(
                        app,
                        json!({"sourceChannel": chans[0], "sourceInvert": false, "blending": "multiply", "opacity": 100.0, "preserveTransparency": false, "maskChannel": "none"}),
                        json!({"sourceChannel": load, "blending": blends, "maskChannel": masks}),
                    )
                }
                _ => dialog(
                    app,
                    json!({"source1Channel": chans[0], "source1Invert": false, "source2Channel": chans[0], "source2Invert": false, "blending": "multiply", "opacity": 100.0, "result": "newChannel"}),
                    json!({"source1Channel": load.clone(), "source2Channel": load, "blending": blends, "result": ["newChannel", "newDocument", "selection"]}),
                ),
            }
        }
        _ => None,
    }
}

/// File › Save a Copy: pick a name, encode with the export service, write; the document's path
/// and saved state are untouched.
fn save_a_copy(app: &mut PhotocraftApp) -> Result<Value, String> {
    let st = app.session.active().ok_or("no document")?;
    let stem = st.doc.name.rsplit_once('.').map_or(st.doc.name.as_str(), |(a, _)| a).to_string();
    let suggested = format!("{stem} copy.psd");
    let path = app.services.pick_save.as_mut().and_then(|f| f(&suggested)).ok_or("cancelled")?;
    let export = app.services.export.as_ref().ok_or("no exporter configured")?;
    let doc = app.session.active().ok_or("no document")?.doc.clone();
    let (bytes, warnings) = export(&doc, &path, &crate::ExportSettings::default())?;
    let write = app.services.write.as_mut().ok_or("no writer configured")?;
    write(&path, &bytes)?;
    app.ui.status = format!("Saved a copy as {path}");
    app.ui.status_error = false;
    crate::notices::io_warnings(app, &format!("Saved a copy as {}", crate::file_open::display_name(&path)), &warnings);
    Ok(json!({"path": path, "warnings": warnings}))
}

#[cfg(test)]
#[path = "view_cmds/tests.rs"]
mod tests;

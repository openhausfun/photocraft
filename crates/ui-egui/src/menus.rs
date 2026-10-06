//! Menu bar generated from the engine command registry plus UI-level commands.

use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::{DialogKind, UiState};

/// Top-level menus in Photoshop order.
pub const TOP_MENUS: [&str; 10] = ["File", "Edit", "Image", "Layer", "Type", "Select", "Filter", "View", "Window", "Help"];

/// UI-level commands (handled by the shell rather than the engine): id, label, menu, shortcut.
pub const UI_COMMANDS: &[(&str, &str, &[&str], Option<&str>)] = &[
    ("file.open", "Open…", &["File"], Some("Cmd+O")),
    ("file.save", "Save", &["File"], Some("Cmd+S")),
    ("file.saveAs", "Save As…", &["File"], Some("Cmd+Shift+S")),
    ("file.exit", "Exit", &["File"], Some("Cmd+Q")),
    ("edit.freeTransform", "Free Transform", &["Edit"], Some("Cmd+T")),
    ("file.export.exportAs", "Export As…", &["File", "Export"], Some("Cmd+Alt+Shift+W")),
    ("file.export.quickExportAsPng", "Quick Export as PNG", &["File", "Export"], None),
    ("edit.transform.scale", "Scale", &["Edit", "Transform"], None),
    ("edit.transform.rotate", "Rotate", &["Edit", "Transform"], None),
    ("edit.transform.skew", "Skew", &["Edit", "Transform"], None),
    ("edit.transform.distort", "Distort", &["Edit", "Transform"], None),
    ("edit.transform.perspective", "Perspective", &["Edit", "Transform"], None),
    ("select.selectAndMask", "Select and Mask…", &["Select"], Some("Cmd+Alt+R")),
    ("view.rulers", "Rulers", &["View"], Some("Cmd+R")),
    ("view.show.grid", "Grid", &["View", "Show"], Some("Cmd+'")),
    ("view.show.guides", "Guides", &["View", "Show"], Some("Cmd+;")),
    ("view.snap", "Snap", &["View"], Some("Cmd+Shift+;")),
    ("view.lockGuides", "Lock Guides", &["View"], Some("Cmd+Alt+;")),
    ("view.extras", "Extras", &["View"], Some("Cmd+H")),
    ("view.show.targetPath", "Target Path", &["View", "Show"], Some("Cmd+Shift+H")),
    ("view.screenMode.cycle", "Cycle Screen Mode", &[], Some("F")),
    ("view.zoomIn", "Zoom In", &["View"], Some("Cmd+=")),
    ("view.zoomOut", "Zoom Out", &["View"], Some("Cmd+-")),
    ("view.fitOnScreen", "Fit on Screen", &["View"], Some("Cmd+0")),
    ("view.actualPixels", "100%", &["View"], Some("Cmd+1")),
    ("window.newWindowForDocument", "New Window for Document", &["Window", "Arrange"], None),
    ("window.toggle.layers", "Layers", &["Window"], Some("F7")),
    ("window.toggle.history", "History", &["Window"], None),
    ("window.toggle.properties", "Properties", &["Window"], None),
    ("window.toggle.color", "Color", &["Window"], Some("F6")),
    ("window.toggle.brushSettings", "Brush Settings", &["Window"], Some("F5")),
    ("window.toggle.navigator", "Navigator", &["Window"], None),
    ("window.toggle.toolbar", "Tools", &["Window"], None),
    ("window.toggle.options", "Options", &["Window"], None),
    ("window.theme.toggle", "Next Theme", &["Window"], None),
    ("window.theme.pro", "Pro Theme", &["Window", "Theme"], None),
    ("window.theme.proMedium", "Pro Medium Gray Theme", &["Window", "Theme"], None),
    ("window.theme.studio", "Studio Theme", &["Window", "Theme"], None),
    ("window.theme.studioLight", "Studio Light Theme", &["Window", "Theme"], None),
    ("window.theme.classic", "Classic Theme", &["Window", "Theme"], None),
    ("edit.search", "Search…", &["Edit"], Some("Cmd+K")),
    ("help.discord", "Join the ArtCraft Discord…", &["Help"], None),
    ("help.website", "PhotoCraft Website", &["Help"], None),
    ("help.artcraftWebsite", "ArtCraft Website", &["Help"], None),
    ("help.github", "PhotoCraft on GitHub", &["Help"], None),
    ("help.reportIssue", "Report an Issue…", &["Help"], None),
    ("help.systemInfo", "System Info…", &["Help"], None),
    ("help.about", "About PhotoCraft", &["Help"], None),
];

/// Photoshop's Window › <panel> ids for the panels the shell already has, as `window.toggle.*`.
pub(crate) fn panel_alias(id: &str) -> Option<&'static str> {
    Some(match id.strip_prefix("window.panel.")? {
        "layers" => "window.toggle.layers",
        "history" => "window.toggle.history",
        "properties" | "adjustments" => "window.toggle.properties",
        "color" | "swatches" => "window.toggle.color",
        "navigator" | "info" | "histogram" => "window.toggle.navigator",
        "tools" => "window.toggle.toolbar",
        "options" => "window.toggle.options",
        "brushSettings" | "brushes" => "window.toggle.brushSettings",
        _ => return None,
    })
}

/// View › Proof Setup presets we can simulate, as `view.proofSetup` profiles.
fn proof_preset(id: &str) -> Option<&'static str> {
    Some(match id.strip_prefix("view.proofSetup.")? {
        "workingCmyk" => "working-cmyk",
        "internetStandardRgb" | "monitorRgb" => "srgb",
        _ => return None,
    })
}

/// Window › Workspace presets by id.
fn workspace_name(id: &str) -> Option<&'static str> {
    Some(match id.strip_prefix("window.workspace.")? {
        "essentials" | "resetWorkspace" => "",
        "photography" => "Photography",
        "painting" => "Painting",
        "graphicAndWeb" => "Graphic and Web",
        "pixelArt" => "Pixel Art",
        "motion" => "Motion",
        _ => return None,
    })
}

/// Run a command id from any source (menu, shortcut, palette, automation).
pub fn invoke(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if app.automation_input
        && let Some(authorize) = app.services.automation_command.as_ref()
    {
        authorize(id, &params)?;
    }
    if crate::discard_ui::intercept(app, id, &params) {
        return Ok(Value::Null);
    }
    invoke_unguarded(app, ctx, id, params)
}

/// [`invoke`] without the unsaved-changes prompt, for once the user has already answered it.
pub(crate) fn invoke_unguarded(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    // Help › Discord, website, GitHub, Report an Issue.
    if let Some(url) = crate::links::url_for(id) {
        return Ok(crate::links::open(app, ctx, url));
    }
    if id == "view.proofSetup.custom" {
        return Ok(json!({"dialog": crate::filter_dialog::open(app, "view.proofSetup")}));
    }
    // Image › Analysis tools/dialogs, Measurement Log and Notes panels, File › Import › Notes.
    if let Some(r) = crate::analysis_ui::menu(app, id, &params) {
        return r;
    }
    // Workspace New/Delete/Lock, Modifier Keys, custom pixel aspect, Extras/32-bit options.
    if let Some(r) = crate::workspace_ui::menu(app, id, &params) {
        return r;
    }
    // Window › Gradients, Patterns, Styles, Shapes, Tool Presets, Clone Source.
    if let Some(r) = crate::preset_panels::menu(app, id, &params) {
        return r;
    }
    // Character/Paragraph Styles, Glyphs, Edit › Check Spelling dialog.
    if let Some(r) = crate::type_panels_ui::menu(app, id, &params) {
        return r;
    }
    // Image > Variables (Define / Data Sets), Apply Data Set.
    if let Some(r) = crate::variables_ui::menu(app, id, &params) {
        return r;
    }
    // Window > Timeline panel.
    if let Some(r) = crate::timeline_ui::menu(app, id, &params) {
        return r;
    }
    // Filter › Plug-ins (installed WebAssembly plug-ins, Install Plug-in…).
    if let Some(r) = crate::plugin_ui::menu(app, id, &params) {
        return r;
    }
    if id == "window.panel.brushes" {
        // Window › Brushes opens the Brush Settings window on its presets tab.
        app.ui.panels.brush_settings = true;
        app.ui.brush_tab = 1;
        return Ok(Value::Null);
    }
    if id == "window.panel.brushSettings" {
        app.ui.brush_tab = 0;
    }
    // View/Window/Type shell items, and dialogs/pickers in front of File commands.
    // Edit › Preferences, Keyboard Shortcuts, Color Settings and other Edit dialogs.
    if let Some(r) = crate::prefs_ui::invoke(app, ctx, id, &params) {
        return r;
    }
    // Save for Web, Print and the other File-menu dialogs added with slices.
    if let Some(r) = crate::file_ui::invoke(app, ctx, id, &params) {
        return r;
    }
    if let Some(r) = crate::view_cmds::invoke(app, ctx, id, &params) {
        return r;
    }
    // Liquify dialog, Puppet Warp and Perspective Warp modes (and their control params).
    if let Some(r) = crate::distort_ui::menu(app, ctx, id, &params) {
        return r;
    }
    // Camera Raw Filter dialog (and its control params).
    if let Some(r) = crate::camera_raw_ui::menu(app, ctx, id, &params) {
        return r;
    }
    if let Some(r) = crate::wide_angle_ui::menu(app, ctx, id, &params) {
        return r;
    }
    if let Some(profile) = proof_preset(id) {
        // View › Proof Setup presets: set the proof profile and turn Proof Colors on.
        app.run("view.proofSetup", json!({"profile": profile}))?;
        return app.run("view.proofColors", json!({"on": true}));
    }
    if let Some(alias) = panel_alias(id) {
        return invoke(app, ctx, alias, params);
    }
    if let Some(ws) = workspace_name(id) {
        // Reset re-applies the current workspace; Essentials is the default layout.
        match (ws, id) {
            (_, "window.workspace.resetWorkspace") => {}
            ("", _) => app.ui.workspace = "Essentials".into(),
            (name, _) => app.ui.workspace = name.into(),
        }
        apply_workspace(app);
        return Ok(json!({"workspace": app.ui.workspace}));
    }
    match id {
        // ⌘↩ / Ctrl+Enter (#306): load the path selected in the Paths panel (or the one being
        // drawn) as a selection.
        "path.toSelection" if params.get("name").is_none() => crate::vector_ui::path_to_selection(app, params),
        // Layer › Rename Layer from a menu starts in-place renaming in the Layers panel.
        "layer.renameLayer" if params.get("name").is_none() => {
            let st = app.session.active().ok_or("no document")?;
            let id = params.get("layer").and_then(Value::as_u64).or(st.active_layer.map(|l| l.0)).ok_or("no active layer")?;
            let name = st.doc.layer(photocraft_doc::LayerId(id)).map(|l| l.name.clone()).ok_or("no such layer")?;
            // Another rename in progress is committed first (#314).
            if let Some((cmd, p)) = crate::layer_row_ui::start_rename(ctx, id, &name) {
                app.run(&cmd, p)?;
            }
            Ok(Value::Null)
        }
        "file.new" if params.as_object().is_none_or(|o| o.is_empty()) => {
            let d = app.ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            Ok(json!({"dialog": d}))
        }
        "file.open" => {
            if let Some(path) = params.get("path").and_then(Value::as_str) {
                open_path(app, path)
            } else {
                app.open_dialog_file();
                Ok(Value::Null)
            }
        }
        "file.save" => {
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| app.session.active().and_then(|d| d.path.clone()).filter(|p| saves_in_place(p)));
            app.save_as(path).map(|(p, w)| json!({"path": p, "warnings": w}))
        }
        "file.exit" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            Ok(Value::Null)
        }
        "file.clearRecent" => {
            app.clear_recent();
            Ok(Value::Null)
        }
        id if id.starts_with("file.openRecent.") => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                match id.rsplit('.').next().and_then(|s| s.parse::<usize>().ok()).and_then(|i| app.ui.recent_files.get(i).cloned()) {
                    Some(p) => open_path(app, &p),
                    None => Err("no such recent file".to_string()),
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = app;
                Err("Open Recent is unavailable on the web".to_string())
            }
        }
        "file.saveAs" => app.save_as(params.get("path").and_then(Value::as_str).map(str::to_string)).map(|(p, w)| json!({"path": p, "warnings": w})),
        "view.zoomIn" | "view.zoomOut" | "view.fitOnScreen" | "view.actualPixels" => {
            let i = app.session.active_index().ok_or("no document")?;
            let v = &mut app.ui.views[i];
            match id {
                "view.zoomIn" => v.zoom = crate::canvas::zoom_step(v.zoom, 1),
                "view.zoomOut" => v.zoom = crate::canvas::zoom_step(v.zoom, -1),
                "view.fitOnScreen" => v.fit_pending = true,
                _ => v.zoom = 1.0,
            }
            Ok(Value::Null)
        }
        "window.newWindowForDocument" => {
            let doc = app.session.active_index().ok_or("no document")?;
            let wid = app.ui.alloc_id();
            let view = app.ui.views[doc].clone();
            app.ui.windows.push(crate::state::DocWindow { id: wid, document: doc, view, open: true });
            Ok(json!({"window": wid}))
        }
        "window.theme.toggle" => {
            let next = app.ui.theme.next();
            app.set_theme(ctx, next);
            Ok(Value::Null)
        }
        "window.theme.pro" | "window.theme.proMedium" | "window.theme.studio" | "window.theme.studioLight" | "window.theme.classic" => {
            let k = crate::theme::ThemeKind::from_name(&id["window.theme.".len()..]).unwrap_or_default();
            app.set_theme(ctx, k);
            Ok(Value::Null)
        }
        "edit.search" => {
            app.ui.palette_open = !app.ui.palette_open;
            Ok(Value::Null)
        }
        "help.about" => Ok(json!({"dialog": app.ui.open_dialog(DialogKind::About, Default::default())})),
        "help.systemInfo" => {
            let mut fields = serde_json::Map::new();
            fields.insert("systemInfo".into(), json!(true));
            let dialog = app.ui.open_dialog(DialogKind::About, fields);
            Ok(json!({"dialog": dialog, "info": crate::gpu_status::system_info_json(app)}))
        }
        "file.export.exportAs" => Ok(json!({"dialog": crate::export_dialog::open(app)?})),
        "file.export.quickExportAsPng" => crate::export_dialog::quick_export_png(app),
        // Layer › Export As… / Quick Export as PNG: the export pipeline on just the active layer.
        l if matches!(l, "layer.exportAs" | "layer.quickExportAsPng") && params.as_object().is_none_or(|o| o.is_empty()) => {
            let layer = app.session.active().and_then(|d| d.active_layer).ok_or("no active layer")?;
            if l == "layer.exportAs" {
                Ok(json!({"dialog": crate::export_dialog::open_layer(app, layer)?}))
            } else {
                crate::export_dialog::quick_export_layer_png(app, layer)
            }
        }
        "layer.layerStyle.blendingOptions" if params.as_object().is_none_or(|o| o.is_empty()) => {
            crate::layer_style::open(app, Some(crate::layer_style::BLENDING)).map(|d| json!({"dialog": d})).ok_or_else(|| "no active layer".to_string())
        }
        // Layer Content Options…: the adjustment / fill controls live in Properties.
        "layer.layerContentOptions" => {
            let r = app.run(id, params)?;
            app.ui.panels.properties = true;
            app.ui.dock_tabs.properties = 0;
            Ok(r)
        }
        // Photoshop's "Select and Mask…" is the engine's select.refineEdge.
        // Select › Color Range… from the menu: the dialog (with params: the engine directly).
        "select.colorRange" if params.as_object().is_none_or(|o| o.is_empty()) => Ok(json!({"dialog": crate::color_range_ui::open(app)})),
        // Edit › Fill… from the menu or its shortcuts: the Fill dialog (with params: the engine).
        crate::fill_ui::COMMAND if params.as_object().is_none_or(|o| o.is_empty()) => {
            if let Some(Err(why)) = photocraft_engine::commands::find(id).map(|c| (c.enabled)(&app.session)) {
                return Err(why);
            }
            Ok(json!({"dialog": crate::fill_ui::open(app)}))
        }
        "select.selectAndMask" => Ok(json!({"dialog": crate::filter_dialog::open(app, "select.refineEdge")})),
        // Select › Transform Selection from the menu: the interactive box (with params: the engine).
        "select.transformSelection" if params.as_object().is_none_or(|o| o.is_empty()) => {
            crate::transform_tool::begin_selection(app, ctx).map(|_| json!({"transform": app.ui.transform}))
        }
        "edit.paste" if params.as_object().is_none_or(|o| o.is_empty()) => {
            // Photoshop: paste in place when the copied area is visible, else centred in the view;
            // images from other apps are always centred.
            app.import_os_clipboard();
            app.clip_read_for_paste = true;
            let external = app.clip_external;
            let visible = !external
                && app.session.active_index().zip(app.session.clipboard.as_ref()).is_some_and(|(i, clip)| {
                    let v = &app.ui.views[i];
                    let (hw, hh) = (app.last_canvas_rect.width() / 2.0 / v.zoom, app.last_canvas_rect.height() / 2.0 / v.zoom);
                    let r =
                        photocraft_geom::Rect::new((v.center[0] - hw) as i32, (v.center[1] - hh) as i32, (v.center[0] + hw) as i32, (v.center[1] + hh) as i32);
                    !clip.bounds.intersect(&r).is_empty()
                });
            let p = match app.session.active_index() {
                Some(i) if !visible => json!({"center": app.ui.views[i].center}),
                _ => json!({}),
            };
            app.run("edit.paste", p)
        }
        "view.rulers" | "view.show.grid" | "view.show.guides" | "view.snap" | "view.lockGuides" => {
            let e = &mut app.ui.extras;
            let slot = match id {
                "view.rulers" => &mut e.rulers,
                "view.show.grid" => &mut e.grid,
                "view.show.guides" => &mut e.guides,
                "view.snap" => &mut e.snap,
                _ => &mut e.lock_guides,
            };
            *slot = !*slot;
            Ok(json!(*slot))
        }
        "edit.freeTransform"
        | "edit.transform.scale"
        | "edit.transform.rotate"
        | "edit.transform.skew"
        | "edit.transform.distort"
        | "edit.transform.perspective" => crate::transform_tool::begin(app, ctx).map(|_| json!({"transform": app.ui.transform})),
        // Edit › Transform › Warp from the menu: interactive Warp mode (with params: the engine).
        "edit.transform.warp" | "layer.smartObjects.warp" if params.as_object().is_none_or(|o| o.is_empty()) => {
            crate::transform_tool::begin_warp(app, ctx).map(|_| json!({"transform": app.ui.transform}))
        }
        // Split warps edit the mesh of an active Warp session.
        "edit.transform.splitWarpCrosswise"
        | "edit.transform.splitWarpHorizontally"
        | "edit.transform.splitWarpVertically"
        | "edit.transform.removeWarpSplit"
            if app.ui.transform.as_ref().is_some_and(|t| t.warp.is_some()) && params.get("warp").is_none() =>
        {
            let at = params.get("at").and_then(Value::as_array).and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]));
            crate::transform_tool::split(app, id, at).map(|_| json!({"transform": app.ui.transform}))
        }
        sz if crate::sizing::is_sizing(sz) && params.as_object().is_none_or(|o| o.is_empty()) => {
            Ok(json!({"dialog": crate::sizing::open(app, sz).ok_or("no document")?}))
        }
        fl if crate::filter_dialog::has_dialog(fl) && params.as_object().is_none_or(|o| o.is_empty()) => {
            Ok(json!({"dialog": crate::filter_dialog::open(app, fl)}))
        }
        a if crate::adjust_dialog::has_dialog(a) && params.as_object().is_none_or(|o| o.is_empty()) => {
            Ok(json!({"dialog": crate::adjust_dialog::open(app, a).ok_or("no document")?}))
        }
        t if t.starts_with("window.toggle.") => {
            // A shown but collapsed dock group is expanded rather than hidden (#129).
            if let Some(g) = crate::dock::Group::from_key(&t["window.toggle.".len()..]).filter(|g| g.shown(&app.ui.panels) && app.ui.dock.is_collapsed(*g)) {
                crate::dock::reveal(app, g);
                return Ok(Value::Null);
            }
            let p = &mut app.ui.panels;
            let slot = match &t["window.toggle.".len()..] {
                "layers" => &mut p.layers,
                "history" => &mut p.history,
                "properties" => &mut p.properties,
                "color" => &mut p.color,
                "navigator" => &mut p.navigator,
                "toolbar" => &mut p.toolbar,
                "options" => &mut p.options_bar,
                "brushSettings" => &mut p.brush_settings,
                _ => return Err(format!("unknown panel in {t}")),
            };
            *slot = !*slot;
            if let Some(g) = crate::dock::Group::from_key(&t["window.toggle.".len()..]).filter(|g| g.shown(&app.ui.panels)) {
                crate::dock::reveal(app, g);
            }
            Ok(Value::Null)
        }
        // Layer › Layer Style › <effect>… opens the Layer Style dialog on that effect.
        ls if params.as_object().is_none_or(|o| o.is_empty())
            && ls.strip_prefix("layer.layerStyle.").is_some_and(|k| crate::layer_style::KINDS.iter().any(|(kind, _)| *kind == k)) =>
        {
            let kind = &ls["layer.layerStyle.".len()..];
            crate::layer_style::open(app, Some(kind)).map(|d| json!({"dialog": d})).ok_or_else(|| "no active layer".to_string())
        }
        _ => app.run(id, params),
    }
}

fn open_path(app: &mut PhotocraftApp, path: &str) -> Result<Value, String> {
    app.open_path(path).map(|w| json!({"warnings": w}))
}

/// File › Save writes back to the document's own file for layered formats (PSD, PSB, .pcraft);
/// flat files go through Save As, like Photoshop.
fn saves_in_place(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "psd" | "psb" | "pcraft")
}

pub fn is_enabled(app: &PhotocraftApp, id: &str) -> bool {
    // Photoshop greys these for the Background layer, other layer kinds or single-layer documents.
    if crate::enable_rules::disabled(app, id) {
        return false;
    }
    if let Some(e) = crate::workspace_ui::is_enabled(app, id) {
        return e;
    }
    if crate::analysis_ui::handles(id) {
        return true;
    }
    if crate::preset_panels::handles(id) || crate::type_panels_ui::handles(id) || crate::timeline_ui::handles(id) {
        return true;
    }
    if let Some(e) = crate::view_cmds::is_enabled(app, id) {
        return e;
    }
    if let Some(e) = crate::plugin_ui::is_enabled(app, id) {
        return e;
    }
    match id {
        "file.open" | "file.exit" | "file.clearRecent" | "help.about" | "help.systemInfo" | "edit.search" => true,
        i if i.starts_with("file.openRecent.") => true,
        i if crate::links::url_for(i).is_some() => true,
        i if i.starts_with("window.theme.") => true,
        "file.save" | "file.saveAs" | "file.export.exportAs" | "file.export.quickExportAsPng" => {
            app.session.active().is_some() && app.services.export.is_some()
        }
        i if i.starts_with("window.toggle.") => true,
        i if panel_alias(i).is_some() || workspace_name(i).is_some() => true,
        i if proof_preset(i).is_some() => app.session.active().is_some(),
        // "Custom…" is the full Proof Setup dialog.
        "view.proofSetup.custom" => app.session.active().is_some(),
        "view.rulers" | "view.show.grid" | "view.show.guides" | "view.snap" | "view.lockGuides" => true,
        // An image copied in another app can only be seen by reading the OS clipboard, which happens
        // on an explicit paste: with a clipboard service, Paste stays enabled whenever a document is open.
        "edit.paste" | "edit.pasteSpecial.pasteInPlace" => {
            app.session.is_enabled(id) || (app.services.clipboard_get_image.is_some() && app.session.active().is_some())
        }
        "select.selectAndMask" => app.session.is_enabled("select.refineEdge"),
        "select.transformSelection" => app.ui.transform.is_none() && app.session.is_enabled("select.transformSelection"),
        i if (i.starts_with("view.zoom") || i == "view.fitOnScreen" || i == "view.actualPixels") || i == "window.newWindowForDocument" => {
            app.session.active().is_some()
        }
        "edit.freeTransform"
        | "edit.transform.scale"
        | "edit.transform.rotate"
        | "edit.transform.skew"
        | "edit.transform.distort"
        | "edit.transform.perspective" => app.ui.transform.is_none() && app.session.active().and_then(|s| s.active_layer).is_some(),
        i => app.session.is_enabled(i),
    }
}

/// Is a UI-level panel toggle currently on (for checkmarks)?
fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    use photocraft_doc::{ColorMode, SampleType};
    if let Some(c) = crate::view_cmds::checked(app, id) {
        return Some(c);
    }
    if let Some(c) = crate::analysis_ui::checked(app, id).or_else(|| crate::workspace_ui::checked(app, id)).or_else(|| crate::file_ui::checked(app, id)) {
        return Some(c);
    }
    if let Some(c) = crate::preset_panels::checked(app, id) {
        return Some(c);
    }
    if let Some(c) = crate::timeline_ui::checked(app, id) {
        return Some(c);
    }
    if let Some(c) = crate::type_panels_ui::checked(app, id) {
        return Some(c);
    }
    if let Some(alias) = panel_alias(id) {
        return checked(app, alias);
    }
    if id == "select.isolateLayers" {
        return Some(!app.session.active()?.isolated_layers.is_empty());
    }
    if id == "view.proofColors" || id == "view.gamutWarning" {
        let d = app.session.active()?;
        let pv = app.session.color.proof(d.doc.id);
        return Some(if id == "view.proofColors" { pv.enabled } else { pv.gamut_warning });
    }
    if id.starts_with("window.workspace.") && id != "window.workspace.resetWorkspace" {
        let want = workspace_name(id)?;
        return Some(app.ui.workspace == if want.is_empty() { "Essentials" } else { want });
    }
    if let Some(m) = id.strip_prefix("image.mode.") {
        let d = &app.session.active()?.doc;
        return match m {
            "rgb" => Some(d.mode == ColorMode::Rgb),
            "grayscale" => Some(d.mode == ColorMode::Grayscale),
            "cmyk" => Some(d.mode == ColorMode::Cmyk),
            "lab" => Some(d.mode == ColorMode::Lab),
            "multichannel" => Some(d.mode == ColorMode::Multichannel),
            "indexedColor" => Some(d.mode == ColorMode::Indexed),
            "bitmap" => Some(d.mode == ColorMode::Bitmap),
            "duotone" => Some(d.mode == ColorMode::Duotone),
            "bits8" => Some(d.depth == SampleType::U8),
            "bits16" => Some(d.depth == SampleType::U16),
            "bits32" => Some(d.depth == SampleType::F32),
            _ => None,
        };
    }
    let e = &app.ui.extras;
    match id {
        "view.rulers" => return Some(e.rulers),
        "view.show.grid" => return Some(e.grid),
        "view.show.guides" => return Some(e.guides),
        "view.snap" => return Some(e.snap),
        "view.lockGuides" => return Some(e.lock_guides),
        _ => {}
    }
    let p = &app.ui.panels;
    Some(match id {
        "window.toggle.layers" => p.layers,
        "window.toggle.history" => p.history,
        "window.toggle.properties" => p.properties,
        "window.toggle.color" => p.color,
        "window.toggle.navigator" => p.navigator,
        "window.toggle.toolbar" => p.toolbar,
        "window.toggle.options" => p.options_bar,
        "window.toggle.brushSettings" => p.brush_settings,
        _ => return None,
    })
}

/// Menu tree entry for rendering and for `ui.inspect`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub path: Vec<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
    pub checked: Option<bool>,
    /// Edit › Menus colour (red, orange, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Is `id` implemented by the engine or the shell (a live menu item)? Shared by the menus and
/// the parity report ([`crate::parity`]).
pub fn is_live(id: &str) -> bool {
    photocraft_engine::commands::find(id).is_some()
        || UI_COMMANDS.iter().any(|c| c.0 == id)
        || panel_alias(id).is_some()
        || workspace_name(id).is_some()
        || proof_preset(id).is_some()
        || id == "view.proofSetup.custom"
        || crate::view_cmds::handles(id)
        || crate::analysis_ui::handles(id)
        || crate::workspace_ui::handles(id)
        || crate::preset_panels::handles(id)
        || crate::type_panels_ui::handles(id)
        || crate::timeline_ui::handles(id)
}

pub fn menu_items(app: &PhotocraftApp) -> Vec<MenuItem> {
    // 1) Photoshop's full menu tree, in Photoshop order; live where we implement the command.
    let known = is_live;
    let mut items: Vec<MenuItem> = crate::menu_catalog::CATALOG
        .iter()
        .map(|&(path, label, sc, id)| MenuItem {
            id: id.to_string(),
            label: label.to_string(),
            path: path.iter().map(|s| s.to_string()).collect(),
            shortcut: sc.map(Into::into),
            enabled: known(id) && is_enabled(app, id),
            checked: checked(app, id),
            color: None,
        })
        .collect();
    // 2) Our commands that Photoshop's tree doesn't list (or lists under another id).
    let mut extra: Vec<MenuItem> = Vec::new();
    for &(id, label, path, sc) in UI_COMMANDS {
        extra.push(MenuItem {
            id: id.into(),
            label: label.into(),
            path: path.iter().map(|s| s.to_string()).collect(),
            shortcut: sc.map(Into::into),
            enabled: is_enabled(app, id),
            checked: checked(app, id),
            color: None,
        });
    }
    for c in photocraft_engine::command_specs().iter().filter(|c| !c.menu.is_empty()) {
        extra.push(MenuItem {
            id: c.id.into(),
            label: c.label.into(),
            path: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: c.shortcut.map(Into::into),
            enabled: is_enabled(app, c.id),
            checked: None,
            color: None,
        });
    }
    for e in extra {
        let dup = items.iter().any(|i| i.id == e.id || (i.path == e.path && i.label.trim_end_matches('…') == e.label.trim_end_matches('…')));
        if !dup {
            // Insert after the last item of the same top-level menu, keeping menus contiguous.
            let top = e.path.first().cloned();
            let at = items.iter().rposition(|i| i.path.first() == top.as_ref()).map_or(items.len(), |p| p + 1);
            items.insert(at, e);
        }
    }
    crate::plugin_ui::insert_menu_items(app, &mut items);
    // File › Open Recent: a dynamic submenu of recently opened files (inserted after "Open As…").
    if let Some(after) = items.iter().position(|i| i.id == "file.openAs") {
        let rp: Vec<String> = vec!["File".into(), "Open Recent".into()];
        let mut recent: Vec<MenuItem> = app
            .ui
            .recent_files
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let label = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
                MenuItem { id: format!("file.openRecent.{i}"), label, path: rp.clone(), shortcut: None, enabled: true, checked: None, color: None }
            })
            .collect();
        if !recent.is_empty() {
            recent.push(MenuItem { id: "---".into(), label: "---".into(), path: rp.clone(), shortcut: None, enabled: false, checked: None, color: None });
        }
        recent.push(MenuItem {
            id: "file.clearRecent".into(),
            label: "Clear Recent Files".into(),
            path: rp.clone(),
            shortcut: None,
            enabled: !app.ui.recent_files.is_empty(),
            checked: None,
            color: None,
        });
        for (k, it) in recent.into_iter().enumerate() {
            items.insert(after + 1 + k, it);
        }
    }
    // Help: the link items, a separator, then System Info and About.
    if let Some(at) = items.iter().position(|i| i.id == "help.systemInfo" || i.id == "help.about") {
        items.insert(
            at,
            MenuItem { id: "---".into(), label: "---".into(), path: vec!["Help".into()], shortcut: None, enabled: false, checked: None, color: None },
        );
    }
    // Edit › Keyboard Shortcuts overrides, Edit › Menus hidden items and colours.
    let prefs = app.session.prefs();
    if !prefs.shortcuts.is_empty() {
        for it in &mut items {
            if prefs.shortcuts.contains_key(&it.id) {
                it.shortcut = prefs.shortcut(&it.id, None).map(str::to_string);
            }
        }
    }
    if !prefs.menus.hidden.is_empty() {
        items.retain(|i| !prefs.menus.hidden.contains(&i.id));
    }
    if prefs.interface.show_menu_colors {
        for it in &mut items {
            it.color = prefs.menus.colors.get(&it.id).cloned();
        }
    }
    items
}

/// Background tint of an Edit › Menus colour name.
fn menu_tint(name: &str) -> Option<egui::Color32> {
    Some(match name {
        "red" => egui::Color32::from_rgb(190, 60, 60),
        "orange" => egui::Color32::from_rgb(200, 120, 40),
        "yellow" => egui::Color32::from_rgb(190, 170, 40),
        "green" => egui::Color32::from_rgb(60, 150, 70),
        "blue" => egui::Color32::from_rgb(50, 110, 200),
        "violet" => egui::Color32::from_rgb(130, 80, 190),
        "gray" => egui::Color32::from_rgb(120, 120, 120),
        _ => return None,
    })
}

/// Draws the menu bar; returns the right edge of the last menu title (the bar itself fills the row).
pub fn menu_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> f32 {
    // Built only while a menu is open: every item's enabled/checked state scales with the
    // document (layer lookups), which cost milliseconds per frame on large layouts (#125).
    let items: std::cell::OnceCell<Vec<MenuItem>> = std::cell::OnceCell::new();
    let app_ref: &PhotocraftApp = app;
    let mut right = ui.cursor().left();
    let lang = crate::i18n::current();
    let mut clicked: Option<String> = None;
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut nav = crate::menu_nav::Nav::load(ui.ctx());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let v = &mut ui.style_mut().visuals;
        v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, t.text_dim);
        v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
            nav.bar_bottom = Some(ui.max_rect().bottom());
            let mut buttons = Vec::with_capacity(TOP_MENUS.len());
            for top in TOP_MENUS {
                let r = ui.menu_button(egui::RichText::new(crate::i18n::tr(lang, top)).color(t.text_dim), |ui| {
                    let items = items.get_or_init(|| menu_items(app_ref));
                    let mine: Vec<&MenuItem> = items.iter().filter(|i| i.path.first().map(String::as_str) == Some(top)).collect();
                    ui.set_min_width(220.0);
                    if mine.is_empty() {
                        ui.weak(crate::i18n::tr(lang, "(coming soon)"));
                    }
                    render_level(ui, &mine, 1, &mut clicked, &mut nav);
                });
                buttons.push(r.response);
            }
            right = buttons.iter().map(|b| b.rect.right()).fold(right, f32::max);
            switch_on_hover(ui.ctx(), &buttons);
            let tops: Vec<egui::Id> = buttons.iter().map(egui::Popup::default_response_id).collect();
            nav.keys(ui.ctx(), &tops, &mut clicked);
        });
    });
    nav.store(ui.ctx());
    if let Some(id) = clicked {
        let ctx = ui.ctx().clone();
        let id = alt_click(id, ctx.input(|i| i.modifiers.alt));
        if let Err(e) = invoke(app, &ctx, &id, json!({})) {
            app.ui.status = e;
        }
    }
    right
}

/// True only when the pointer can actually reach a menu title. A tall submenu can be
/// repositioned upward by egui and overlap the menu bar; in that case the popup's layer is
/// top-most and hovering it must not switch the open top-level menu.
fn pointer_reaches_title(ctx: &egui::Context, response: &egui::Response, p: egui::Pos2) -> bool {
    response.interact_rect.contains(p) && ctx.layer_id_at(p) == Some(response.layer_id)
}

/// Like a native menu bar (Windows, macOS): while one top-level menu is open, hovering another
/// top-level title opens that menu instead.
fn switch_on_hover(ctx: &egui::Context, buttons: &[egui::Response]) {
    let ids: Vec<egui::Id> = buttons.iter().map(egui::Popup::default_response_id).collect();
    let Some(open) = ids.iter().position(|id| egui::Popup::is_id_open(ctx, *id)) else { return };
    // `Response::hovered` is false while the menu's popup layer is open. Hit-test the title
    // rect manually, but only when its own layer is top-most at the pointer.
    let Some(p) = ctx.pointer_hover_pos() else { return };
    // Only a moving pointer switches: a pointer resting on a title must not undo ← / → .
    if ctx.input(|i| i.pointer.delta() == egui::Vec2::ZERO) {
        return;
    }
    if let Some(i) = buttons.iter().position(|b| pointer_reaches_title(ctx, b, p))
        && i != open
    {
        egui::Popup::open_id(ctx, ids[i]);
        ctx.request_repaint();
    }
}

fn render_level(ui: &mut egui::Ui, items: &[&MenuItem], depth: usize, clicked: &mut Option<String>, nav: &mut crate::menu_nav::Nav) {
    // Menu popups can be taller than the window: each level stays on screen and scrolls (wheel,
    // scroll arrows, keyboard), like a native menu on a small display.
    crate::menu_nav::level(ui, depth, nav, |ui, nav| render_level_rows(ui, items, depth, clicked, nav));
}

fn render_level_rows(ui: &mut egui::Ui, items: &[&MenuItem], depth: usize, clicked: &mut Option<String>, nav: &mut crate::menu_nav::Nav) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let lang = crate::i18n::current();
    // Items never wrap: the menu widens to its longest label plus shortcut (translations can be
    // longer than the English).
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    if t.pro {
        // Spectrum/macOS menus: blue highlight row with white text.
        let v = &mut ui.style_mut().visuals;
        v.widgets.hovered.weak_bg_fill = t.accent;
        v.widgets.hovered.bg_fill = t.accent;
        v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(3);
        ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
    }
    // Walk in Photoshop order: leaves and separators at this depth; a submenu appears at the position
    // of its first child.
    let mut shown_subs: Vec<&str> = Vec::new();
    let mut last_was_sep = true;
    for (i, it) in items.iter().enumerate() {
        if it.path.len() == depth {
            if it.label == "---" {
                if !last_was_sep && i + 1 < items.len() {
                    ui.separator();
                    last_was_sep = true;
                }
                continue;
            }
            let mut text = crate::i18n::tr_id(lang, &it.id, &it.label).to_string();
            if let Some(c) = it.checked {
                text = format!("{} {}", if c { "✔" } else { "  " }, text);
            }
            let mut b = egui::Button::new(text);
            if let Some(tint) = it.color.as_deref().and_then(menu_tint) {
                b = b.fill(tint.gamma_multiply(0.55));
            }
            if let Some(sc) = &it.shortcut {
                b = b.shortcut_text(crate::shortcuts::pretty(sc));
            }
            let hit = nav.row(ui, depth - 1, it.enabled, Some(&it.id), |ui, _| {
                let r = ui.add_enabled(it.enabled, b);
                let hit = r.clicked();
                (r, hit)
            });
            if hit {
                *clicked = Some(it.id.clone());
                ui.close();
            }
            last_was_sep = false;
        } else if it.path.len() > depth {
            let name = it.path[depth].as_str();
            if shown_subs.contains(&name) {
                continue;
            }
            shown_subs.push(name);
            let child: Vec<&MenuItem> = items.iter().copied().filter(|c| c.path.len() > depth && c.path[depth] == name).collect();
            let any_enabled = child.iter().any(|c| c.enabled && c.label != "---");
            let enabled = any_enabled || !child.is_empty();
            ui.add_enabled_ui(enabled, |ui| {
                nav.row(ui, depth - 1, enabled, None, |ui, nav| {
                    let r = ui.menu_button(crate::i18n::tr(lang, name), |ui| render_level(ui, &child, depth + 1, clicked, nav));
                    if r.inner.is_some() {
                        // Where the submenu hangs from, to keep it below the menu bar (#319).
                        nav.set_anchor(depth, r.response.rect);
                    }
                    (r.response, ())
                });
            });
            last_was_sep = false;
        }
    }
}

/// The command a menu click runs: ⌥ + Merge Down / Merge Layers / Merge Visible keep the
/// originals, running Stamp Down / Stamp Visible instead (#217).
pub fn alt_click(id: String, alt: bool) -> String {
    match photocraft_engine::stamp_cmds::alt_variant(&id) {
        Some(stamp) if alt => stamp.to_string(),
        _ => id,
    }
}

/// Workspace presets (Window → Workspace): which panels are visible.
pub fn apply_workspace(app: &mut PhotocraftApp) {
    // Saved workspaces (Window › Workspace › New Workspace…) restore their own layout.
    if crate::workspace_ui::apply_custom(app) {
        return;
    }
    // Presets use the default group order, heights and tabs (Reset brings everything back).
    app.ui.dock = Default::default();
    app.ui.dock_tabs = Default::default();
    let p = &mut app.ui.panels;
    let (nav, color, layers, history, props) = match app.ui.workspace.as_str() {
        "Photography" => (true, false, true, true, true),
        "Painting" => (false, true, true, false, false),
        "Graphic and Web" => (false, true, true, false, true),
        _ => (false, true, true, false, true),
    };
    p.navigator = nav;
    p.color = color;
    p.layers = layers;
    p.history = history;
    p.properties = props;
    p.character = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_bar_labels_have_horizontal_padding_and_open_menus() {
        use egui_kittest::{Harness, kittest::Queryable};

        for theme in crate::theme::ThemeKind::ALL {
            for width in [640.0, 1440.0] {
                let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                let mut harness = Harness::builder().with_size(egui::vec2(width, 200.0)).build_ui_state(
                    |ui, app| {
                        menu_bar(app, ui);
                    },
                    app,
                );
                PhotocraftApp::setup_context(&harness.ctx, theme);
                harness.run_steps(3);

                let font = egui::TextStyle::Button.resolve(&harness.ctx.global_style());
                let mut right = 0.0;
                for label in TOP_MENUS {
                    let rect = harness.get_by_label(label).rect();
                    let text_width = harness.ctx.fonts_mut(|fonts| fonts.layout_no_wrap(label.into(), font.clone(), egui::Color32::WHITE).size().x);
                    assert!(rect.width() >= text_width + 11.5, "{theme:?}: {label} lacks horizontal padding");
                    assert!(rect.left() >= right && rect.right() <= width, "{theme:?}: {label} overlaps or overflows at width {width}");
                    right = rect.right();
                }

                harness.get_by_label("File").click();
                harness.run_steps(3);
                assert!(harness.query_by_label_contains("Open…").is_some(), "{theme:?}: File menu did not open");
            }
        }
    }

    #[test]
    fn overlapped_menu_title_does_not_receive_hover_switching() {
        let ctx = egui::Context::default();
        let mut title: Option<egui::Response> = None;
        let p = egui::pos2(90.0, 12.0);
        let mut reaches = true;
        // The popup area becomes hit-testable once egui has laid it out, so draw a few frames.
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let ctx = &ui.ctx().clone();
                egui::CentralPanel::default().show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 24.0), egui::Sense::hover());
                    let shifted = rect.translate(egui::vec2(p.x - rect.center().x, p.y - rect.center().y));
                    let response = ui.interact(shifted, egui::Id::new("menu-title-test"), egui::Sense::hover());
                    title = Some(response);
                });
                egui::Area::new(egui::Id::new("overlapping-popup-test")).order(egui::Order::Foreground).fixed_pos(p - egui::vec2(10.0, 10.0)).show(ctx, |ui| {
                    ui.allocate_space(egui::vec2(20.0, 20.0));
                });
                let response = title.as_ref().unwrap();
                reaches = pointer_reaches_title(ctx, response, p);
            });
            out.textures_delta.clear();
        }
        assert!(!reaches);
    }

    #[test]
    fn hovering_another_title_switches_the_open_menu() {
        use egui_kittest::{Harness, kittest::Queryable};

        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let mut harness = Harness::builder().with_size(egui::vec2(1200.0, 700.0)).build_ui_state(
            |ui, app| {
                menu_bar(app, ui);
            },
            app,
        );
        PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        harness.run_steps(3);
        // Nothing open: hovering a title does not open it.
        harness.get_by_label("Image").hover();
        harness.run_steps(3);
        assert!(harness.query_by_label_contains("Open…").is_none() && harness.query_by_label_contains("Duplicate…").is_none());
        harness.get_by_label("File").click();
        harness.run_steps(3);
        assert!(harness.query_by_label_contains("Open…").is_some());
        harness.get_by_label("Image").hover();
        harness.run_steps(4);
        assert!(harness.query_by_label_contains("Open…").is_none(), "File menu should close");
        assert!(harness.query_by_label_contains("Duplicate…").is_some(), "Image menu should open on hover");
    }

    #[test]
    fn dropdown_items_have_room() {
        use egui_kittest::{Harness, kittest::Queryable};

        for theme in crate::theme::ThemeKind::ALL {
            let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            let mut harness = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui_state(
                |ui, app| {
                    menu_bar(app, ui);
                },
                app,
            );
            PhotocraftApp::setup_context(&harness.ctx, theme);
            harness.run_steps(3);
            harness.get_by_label("File").click();
            harness.run_steps(3);
            let a = harness.get_by_label_contains("New…").rect();
            let b = harness.get_by_label_contains("Open…").rect();
            let font = egui::TextStyle::Button.resolve(&harness.ctx.global_style()).size;
            assert!(a.height() >= font + 8.0, "{theme:?}: item height {} for a {font} pt font", a.height());
            assert!(b.top() - a.top() >= font + 10.0, "{theme:?}: rows {} apart", b.top() - a.top());
        }
    }

    #[test]
    fn menu_level_height_is_bounded_by_viewport() {
        use egui_kittest::Harness;

        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let mut harness = Harness::builder().with_size(egui::vec2(900.0, 240.0)).build_ui_state(
            |ui, app| {
                menu_bar(app, ui);
            },
            app,
        );
        PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        harness.run_steps(3);
        // Opening a top-level menu with many entries must not grow its popup past the viewport.
        use egui_kittest::kittest::Queryable;
        harness.get_by_label("Filter").click();
        harness.run_steps(3);
        let viewport = harness.ctx.content_rect();
        let max_bottom = harness
            .ctx
            .memory(|m| m.areas().visible_layer_ids())
            .into_iter()
            .filter_map(|layer| harness.ctx.memory(|m| m.area_rect(layer.id)))
            .map(|r| r.bottom())
            .fold(viewport.top(), f32::max);
        assert!(max_bottom <= viewport.bottom() + 1.0, "menu popup overflowed viewport: {max_bottom} > {}", viewport.bottom());
    }

    #[test]
    fn tall_menu_scrolls_with_mouse_wheel_on_short_display() {
        use egui_kittest::{Harness, kittest::Queryable};

        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let mut harness = Harness::builder().with_size(egui::vec2(900.0, 220.0)).build_ui_state(
            |ui, app| {
                menu_bar(app, ui);
            },
            app,
        );
        PhotocraftApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        harness.run_steps(3);
        harness.get_by_label("Filter").click();
        harness.run_steps(3);

        let item = harness.get_by_label_contains("Filter Gallery…");
        let before = item.rect().top();
        harness.hover_at(item.rect().center());
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -80.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(4);

        let after = harness.get_by_label_contains("Filter Gallery…").rect().top();
        assert!(after < before - 1.0, "mouse wheel should move tall menu content: {before} -> {after}");
    }

    #[test]
    fn window_panel_and_workspace_ids_drive_the_shell() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        assert!(!app.ui.panels.history);
        invoke(&mut app, &ctx, "window.panel.history", Value::Null).unwrap();
        assert!(app.ui.panels.history);
        assert_eq!(checked(&app, "window.panel.history"), Some(true));
        invoke(&mut app, &ctx, "window.workspace.painting", Value::Null).unwrap();
        assert_eq!(app.ui.workspace, "Painting");
        assert!(!app.ui.panels.properties);
        assert_eq!(checked(&app, "window.workspace.painting"), Some(true));
        invoke(&mut app, &ctx, "window.workspace.essentials", Value::Null).unwrap();
        assert!(app.ui.panels.properties);
        // Live in the menu, and no duplicate "Layers" entry from the window.toggle.* commands.
        let items = menu_items(&app);
        assert!(items.iter().any(|i| i.id == "window.panel.layers" && i.enabled));
        assert_eq!(items.iter().filter(|i| i.path == ["Window"] && i.label == "Layers").count(), 1);
        assert!(items.iter().any(|i| i.id == "edit.stroke"));
    }

    #[test]
    fn proof_setup_presets_and_checkmarks() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        app.sync_views();
        assert_eq!(checked(&app, "view.proofColors"), Some(false));
        invoke(&mut app, &ctx, "view.proofSetup.workingCmyk", Value::Null).unwrap();
        assert_eq!(checked(&app, "view.proofColors"), Some(true));
        invoke(&mut app, &ctx, "view.gamutWarning", json!({"on": true})).unwrap();
        assert_eq!(checked(&app, "view.gamutWarning"), Some(true));
        let doc = app.session.active().unwrap().doc.clone();
        let lut = app.session.color.canvas_lut(&doc, 9).unwrap().unwrap();
        assert_eq!(lut.len(), 9 * 9 * 9 * 4);
        // Pure sRGB green is outside coated CMYK; mid grey is inside.
        let at = |r: usize, g: usize, b: usize| lut[(r + g * 9 + b * 81) * 4 + 3];
        assert_eq!((at(0, 8, 0), at(4, 4, 4)), (255, 0));
        // Custom… opens the generated Proof Setup dialog with a profile choice.
        assert!(invoke(&mut app, &ctx, "view.proofSetup.custom", Value::Null).unwrap()["dialog"].is_u64());
        let spec = photocraft_engine::commands::find("edit.convertToProfile").unwrap();
        let params = crate::filter_dialog::parse_spec(spec.params);
        assert!(matches!(&params[0].kind, crate::filter_dialog::Kind::Choice(c) if c[0] == "srgb" && !c.iter().any(|v| v.contains('/'))));
    }
}

#[cfg(test)]
mod open_recent_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracks_dedupes_caps_and_lists() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.push_recent("/tmp/a.png");
        app.push_recent("/tmp/b.psd");
        app.push_recent("/tmp/a.png"); // de-dupe → moves to front
        assert_eq!(app.ui.recent_files, vec!["/tmp/a.png".to_string(), "/tmp/b.psd".to_string()]);
        for i in 0..15 {
            app.push_recent(&format!("/tmp/f{i}.png"));
        }
        assert_eq!(app.ui.recent_files.len(), 17, "all kept below the default cap of 20");
        for i in 15..30 {
            app.push_recent(&format!("/tmp/f{i}.png"));
        }
        assert_eq!(app.ui.recent_files.len(), 20, "capped at Recent File List Contains (20)");
        assert_eq!(app.session.prefs().file_handling.recent_files, app.ui.recent_files, "stored in the preferences");

        // Menu lists them under File › Open Recent, with basenames, plus Clear Recent Files.
        app.ui.recent_files = vec!["/tmp/a.png".into(), "/dir/b.psd".into()];
        let items = menu_items(&app);
        let rp = vec!["File".to_string(), "Open Recent".to_string()];
        let labels: Vec<&str> = items.iter().filter(|i| i.id.starts_with("file.openRecent.")).map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["a.png", "b.psd"]);
        assert!(items.iter().any(|i| i.id == "file.clearRecent" && i.path == rp));

        // Clear Recent Files empties the list.
        let ctx = egui::Context::default();
        invoke(&mut app, &ctx, "file.clearRecent", json!({})).unwrap();
        assert!(app.ui.recent_files.is_empty());
        assert!(app.session.prefs().file_handling.recent_files.is_empty(), "cleared in the preferences too");
        // No recent items in the menu once cleared (only the disabled "Clear Recent Files").
        assert!(!menu_items(&app).iter().any(|i| i.id.starts_with("file.openRecent.")));
        // A bad recent index errors gracefully (no panic).
        assert!(invoke(&mut app, &ctx, "file.openRecent.5", json!({})).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn opening_a_file_records_it_as_recent() {
        use photocraft_color::{ColorMode, SampleType};
        use photocraft_geom::Size;
        let services = crate::Services {
            import: Some(Box::new(|_n: &str, _b: &[u8]| Ok((photocraft_doc::Document::new("t", Size::new(4, 4), ColorMode::Rgb, SampleType::U8), Vec::new())))),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let path = std::env::temp_dir().join("photocraft_recent_test.pcraft");
        std::fs::write(&path, b"x").unwrap();
        let p = path.to_string_lossy().to_string();
        let ctx = egui::Context::default();
        invoke(&mut app, &ctx, "file.open", json!({ "path": p })).unwrap();
        assert_eq!(app.ui.recent_files.first().map(String::as_str), Some(p.as_str()));
        // Re-open via the recent entry.
        invoke(&mut app, &ctx, "file.openRecent.0", json!({})).unwrap();
        assert_eq!(app.session.documents().len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}

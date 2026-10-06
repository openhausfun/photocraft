# UI design system

## Themes

| Theme | Intent |
|---|---|
| **Pro** (default) | Photoshop-style Spectrum dark: flat charcoal panels (#323232), dark tab strips, Spectrum blue accent (#378ef0), pill buttons, checkboxes, compact 12 px type |
| Studio | Dark studio style: near-black, rounded cards, pill tabs, violet accent, toggles |
| Studio Light | Studio on light surfaces |
| Classic | Windows-2000 bevels, square corners, navy selection |

Switch themes with the sun icon, Window → Theme, or `ui.set {"theme":"classic"}` over the control channel.

## Rules

- **Colours and radii come from tokens.** Read them with `Tokens::get(ctx)`; never hard-code a colour in a widget.
- **Shared widgets live in `widgets.rs`:** `card` (panel group; Pro renders a Photoshop tab strip), `value_field`, `slider`/`slider_row`, `toggle`/`checkbox`, `primary_button`/`secondary_button`, `dropdown` (with a chevron icon), `hairline`/`vline`.
- **Icons:** Lucide SVGs in `assets/icons` (ISC licence), embedded via `icon_data.rs`. Regenerate that file when you add icons.
- **Fonts:** Inter (UI) and JetBrains Mono (numbers), both OFL. Named families `medium` and `semibold` are available via `theme::medium()` and `theme::semibold()`.
- **Photoshop layout grammar (Pro):**
  - Essentials dock order: Color | Swatches, then Properties | Adjustments, then Layers | Channels | Paths (Layers fills the remaining height).
  - Options-bar labels end with a colon ("Size:").
  - Document tabs read "name @ 12.5% (RGB/8)".
  - Toolbar tool groups carry a corner triangle.
- **Verify every visual change** with `ui.screenshot`, at several window sizes and in every theme.

## Adding icons

```sh
curl -sfL -o assets/icons/<name>.svg https://raw.githubusercontent.com/lucide-icons/lucide/main/icons/<name>.svg
# regenerate the embedded table
{ printf '%s\n' '//! Lucide icons (ISC licence), embedded and tinted at runtime.' '' 'pub static ICONS: &[(&str, &[u8])] = &['; \
  for f in assets/icons/*.svg; do n=$(basename $f .svg); echo "    (\"$n\", include_bytes!(\"../../../assets/icons/$n.svg\")),"; done; echo '];'; } \
  > crates/ui-egui/src/icon_data.rs
```

## Interaction models (match Photoshop CC)

| Feature | Module | Behaviour |
|---|---|---|
| Type tool | `type_tool.rs` | Click: point text with the placeholder "Lorem Ipsum" selected. Drag: paragraph box. Inline caret and selection drawn from the text engine layout. ⌥/⌘ word and line navigation, ↩ newline, ⌘↩ or Esc commits, a click outside commits. One history step per session (`coalesce`). A new layer is named after its text; an empty one is deleted. |
| Free Transform | `transform_tool.rs` | ⌘T. Corners scale proportionally (⇧ frees them); edges scale one axis; ⌥ scales about the reference point; ⌘-corner distorts; dragging outside rotates (⇧ snaps to 15°); dragging inside moves. Preview = document without the moving pixels + a textured 24×24 mesh. ↩ or a double-click commits via `edit.transform {rect, quad}`. |
| Layer masks | `panels.rs` | Clicking the mask thumbnail targets the mask (corner-bracket frame; the tab reads "Layer, Layer Mask/8"). Brush, eraser (paints background colour), gradient and bucket then send `"target": "mask"`. Adjustment and fill layers target their mask automatically. |
| Levels / Curves | `tone.rs` | Histogram of the image *below* the adjustment. Curves: click to add a point, drag out to delete. Every change is a coalesced `layer.setAdjustment`, so one drag = one undo step and the canvas updates at full resolution on the GPU. |

Where the font lacks a symbol (e.g. ∠ ↦ ▔), draw it with the painter or use a Lucide icon; never ship
missing-glyph boxes. Check every new panel with the offscreen snapshot tool (`docs/development.md`).

## Menus

`menu_catalog.rs` holds Photoshop's menu tree (standard command names, order, separators, default shortcuts). Items whose id matches an engine or UI command are live; others render disabled until implemented. Give new commands the catalogue's id (for example `image.imageSize`) and they light up in the right place automatically.

## Automation for visual checks

Use `ui.click {x,y}`, `ui.move`, `ui.key` and `ui.type` (synthetic input in screen points) to open menus, popups and context menus, then `ui.screenshot`.

## Preferences

Preferences has **Apply**, **OK** and **Cancel**. Apply saves the edited sections and keeps the
dialog open; it is disabled when the values match the saved preferences. OK saves and closes.
Cancel discards only changes made since the last successful Apply. A failed Apply leaves the
draft open for correction. Settings marked for the next launch still require a restart.

## High DPI and 4K displays

Edit → Preferences → Interface → UI Scale applies immediately. Auto follows the operating
system's display scale (including fractional scales). For a 4K or larger monitor,
Auto uses at least 200% so text and controls remain readable. Detection uses the current monitor,
including portrait displays, and updates when the window moves between monitors. If monitor
size is unavailable, Auto follows system DPI. The 100% and 200% choices set an absolute UI
scale, allowing large 4K displays to use smaller controls when desired. Canvas zoom shortcuts
continue to control the document independently of UI scaling.

## Localisation

Strings in code stay English and are the default lookup keys. `crates/ui-egui/src/i18n` maps them
to display text at render time from one catalog per language (`i18n/<code>.tsv`; the format is
documented in the header of `ja.tsv`). Command ids, menu paths used for logic, the control channel,
the CLI and MCP always use the English ids and labels.

Languages shipped (`complete_menus` marks a catalog that covers every menu string and `tl!` literal;
the tests enforce it):

- English (`en`), the source language.
- Japanese (`ja`), complete.
- Simplified Chinese (`zh-hans`; see [`localization-zh-hans.md`](localization-zh-hans.md)), partial.
- Traditional Chinese (`zh-hant`), complete, in the vocabulary used in Taiwan; `zh-TW`, `zh-HK`,
  `zh-MO` and `zh-Hant-*` locales all resolve to it. The resolver distinguishes the two Chinese
  scripts, so neither catalog is shown to the other script's locales.
- Spanish (`es`), complete.
- Russian (`ru`), complete, with three plural forms (`one|few|many`, see `plural_russian`).

- `tr(lang, s)` plain strings; `tr_ctx` when one English word needs different translations;
  `tr_id(lang, command_id, label)` for menu items (keyed by command id, English label as the
  fallback); `trn(lang, n, one, other)` for plurals; `fmt` fills `{name}` placeholders, which
  translators may reorder.
- The language is Preferences › Interface › Language (`interface.language`: `auto` or a language
  code; `auto` follows the system locale, an unknown code falls back to `auto`).
- To add a language: add `<code>.tsv` and one row in `i18n::LANGUAGES` (code, native name, catalog,
  plural rule). The dropdown, locale matching and the catalog tests (well-formed, no duplicates,
  placeholders and ellipses agree, command ids exist) pick it up. Set `complete_menus` once every
  menu string is translated; a test then enforces it. A language with more plural forms lists them
  all in `@plural` entries (Russian: `one|few|many`); the row's plural rule picks the form.
- Translations are clean-room: written from the meaning of the English text in ordinary vocabulary,
  never from another product's localisation resources.

Localised so far: menus, the command palette, dialogs and panels (literals wrapped in `tl!("…")`;
widgets such as `checkbox`, `slider_row`, `dropdown` and the buttons translate their labels
themselves). A test fails when a `tl!` literal, a menu string, a blend mode name or a generated
preference label has no entry in a language marked `complete_menus`. Not translated: status-bar
messages and errors (they stay English, also for agents), names that are user data (layers, styles,
documents), strings assembled with `format!` that were not converted to `fmt`/`trn`. Not done yet:
per-language font fallback (the CJK fallback prefers Japanese forms), right-to-left layout,
locale-aware number and date formats, automatic language detection on the web build (native
builds read `LANG`/`LC_*`, the macOS preferred languages and the Windows user locale).

# Control protocol

The desktop app listens on `127.0.0.1:<port>` (loopback only). Start it with a token file so the credential is not exposed in the process command line:

```sh
photocraft --control 7878 --control-token-file /private/path/photocraft-control.token \
  --automation-read-root /work/project --automation-write-root /work/project
```

If the file does not exist, PhotoCraft creates it with a fresh 256-bit token. On Unix the new file is mode `0600`; on Windows, protect it with an appropriate user-only ACL. An existing file is reused. If neither a token nor token file is configured, PhotoCraft generates a token for that launch and writes it to standard error. `PHOTOCRAFT_CONTROL_TOKEN` and `PHOTOCRAFT_CONTROL_TOKEN_FILE` are the environment-variable equivalents.

The first request on every TCP connection must authenticate. No other method is dispatched before this succeeds:

```json
{"id": "auth", "method": "auth", "params": {"token": "<64 hexadecimal characters>"}}
```

After the successful authentication reply, each request is one JSON line:

```json
{"id": 1, "method": "ui.inspect", "params": {}}
```

Each reply is one JSON line with the same `id`:

```json
{"id": 1, "ok": true, "result": { ... }}
{"id": 2, "ok": false, "error": "unknown tool `foo`"}
```

The transport is `apps/photocraft/src/control_server.rs`, and the handlers are in `crates/ui-egui/src/control.rs`. The MCP server (`photocraft-cli mcp --bridge 127.0.0.1:<port>`, crate `photocraft-automation`) wraps this same protocol. See [MCP bridge](#mcp-bridge) below.

## Methods

- `engine.execute {command, params}`: run any engine or UI command by id. Engine commands run directly with their default params and never open a dialog. Use `ui.menu.invoke` for menu-click behaviour, which opens a command's dialog when no params are given
- `engine.commands`: list commands with enablement
- `ui.inspect`: full UI state (tool, panels, views, dialogs, windows, menu tree, window size). `view` holds the View/Window/Type preferences: `screen_mode`, `extras`, `show` and `snap_to` flags, `flip_horizontal`, `arrange` (Window › Arrange layout), pixel aspect, font preview size, language options. `perf.timings.gpuInfo` holds the graphics adapter, backend, driver, the backend chosen at launch and why, the canvas renderer (`gpu`/`cpu`) and, after a device loss, `lost` (`help.systemInfo` returns the same as `info`)
- `ui.set {tool?, panels?, dockTabs?, dock?, dockWidth?, maskTarget?, selectionMode?, zoom?, center?, fit?, theme?, brushSize?, brushSection?, brushTab?}`: change UI state (`selectionMode` is the selection tools' options-bar mode, 0 New, 1 Add, 2 Subtract, 3 Intersect; `brushSection` indexes the Brush Settings sections, `brushTab` 0 = Brush Settings, 1 = Brushes; `dock` is `{order: ["layers", …], heights: {"properties": 180}, collapsed: ["color"]}`, the right-dock groups top to bottom, their heights in points and the groups collapsed to their tab strip; `dockWidth` sets the right dock width in points, clamped to 250..520)
- `ui.menu.invoke {id}` / `ui.menu.list`: activate a menu item by id; list the menu tree
- `ui.dialog.open {kind, fields?}` (kinds `newDocument`, `about`, `layerStyle {effect?}`, `colorPicker {target: foreground|background}`, `command {command}`) / `ui.dialog.set {dialog, field, value}` / `ui.dialog.confirm {dialog}` / `ui.dialog.cancel {dialog}`
  - Preferences (`ui.menu.invoke {id: "edit.preferences.interface"}`) edits the sections in its `values` field. `ui.dialog.apply {dialog}` saves those values through `prefs.set` and keeps the same dialog and section open. `ui.dialog.confirm` saves and closes; `ui.dialog.cancel` discards only edits made since the last successful Apply. Invalid values return an error without closing the Apply dialog or changing the saved preferences. Settings marked for the next launch still require a restart.
  - Edit › Fill… (`ui.menu.invoke {id: "edit.fill"}`, Shift+F5, Shift+Backspace) opens the Fill dialog; its fields are `edit.fill`'s params (`contents`, `color`, `pattern`, `colorAdaptation`, `mode`, `opacity`, `preserveTransparency`), and OK remembers them in the preferences (`dialogs["edit.fill"]`).
  - A pixel tool pressed on a type, shape, Smart Object or fill layer (e.g. through `ui.pointer`) opens the "Rasterize?" prompt instead of painting: a dialog with `__rasterize` (`type|shape|smartObject|fill`), `message`, `layer`, `tool` and `at`. `ui.dialog.confirm` runs the `layer.rasterize.*` command and then paints at `at` (two history states, `{"rasterized", "painted"}`); `ui.dialog.cancel` does nothing.
- `ui.window.open {document?}` / `ui.window.close {window}`: extra document windows
- `ui.pointer {events: [{kind: down|move|up, x, y, pressure?, tiltX?, tiltY?, rotation?}], modifiers?}`: drive the active tool in document coordinates (pressure 0..1, tilt in degrees -90..90, barrel rotation 0..360: a simulated pen); `space: true` holds Space (the Crop frame, marquee, lasso or shape being drawn then moves instead of growing); `button: "right"` with the Move tool (or `command: true` with any tool) opens the canvas layer menu at that point instead (`layerMenu` in `ui.inspect` lists the layers there, topmost first; select one with `layer.select`)
- `ui.key {key, command?, shift?, alt?, ctrl?}` (flags may also be grouped under `modifiers`): press and release a key, e.g. `{"key": "ArrowLeft", "shift": true}`
- `ui.type {text}`: type text (goes to the focused widget, or to the canvas while the Type tool is editing)
- `ui.resize {width, height}`: resize the main window
- `ui.gpu.simulateLoss {error?}`: act as if the wgpu device was lost (or reported an error with `error: true`). The app switches to the CPU renderer for the rest of the session and shows the same notice as on a real loss. Returns `wasActive` and `gpuInfo`. For testing the fallback
- `ui.screenshot {path?, focus?}`: capture the main window (PNG). With no path the reply contains
  base64 PNG data; a path is relative to the automation write root. Raises the window first
  (default) because occluded macOS windows stop rendering
- `ui.focus`: bring the main window to the front
- `app.open {path}` / `app.save {path}`: relative file I/O through the configured automation roots (`app.open` reads under the read root, `app.save` writes under the write root; absolute paths, `..` and paths escaping the root are refused, and both fail closed when no root was granted). Both reply with `warnings` (import/export notes such as "adjustment layer flattened"; `[]` when none), also shown to the user in the status bar and as a notice (`notices` in `ui.inspect`); `app.open` also returns the `path` and document `name`, `app.save` the `path` written. Automation opens and saves never fire script events. `file.open`, `file.save`, `file.saveAs` and `file.saveACopy` reply with `warnings` the same way
- `app.quit`

## Engine commands

`engine.execute` runs any command by id. `engine.commands` (or the engine command `command.list`) lists them all, with labels, menu paths, shortcuts, a parameter description, and whether each is currently enabled. Examples:

| Command | Params |
|---|---|
| `file.new` | `{"width":1920,"height":1080,"mode":"rgb","depth":8,"background":"white"}` |
| `layer.new.layer` | `{"name":"Ink"}` |
| `layer.select` | `{"layer":id,"mode":"replace\|toggle\|range\|add"}`: ⌘-click = toggle, ⇧-click = range; `document.inspect` reports `selectedLayers` and a per-layer `selected` flag |
| `channel.target` / `channel.setVisible` | Channels panel target and eyes (`"composite"`, colour name, alpha index or name, `"quickMask"`). `document.inspect` (and `channel.list`) report `channels`: composite/colour/alpha rows with visibility, channel options, `quickMask`, `target`. Pixel commands without a `target` param follow the targeted channel |
| `layer.setProps` | `{"layer":id?,"name":…,"visible":…,"opacity":0..1,"blend":"Multiply"}` |
| `layer.setExpanded` | `{"layer":id?,"expanded":bool?,"all":bool?}`: open/close a group in the Layers panel (no `expanded`: toggle; `all`: every group). A view change saved with the document (PSD open/closed folder), not an undo step; `document.inspect` reports `expanded` on groups |
| `layer.newAdjustmentLayer.hueSaturation` | `{"hue":30,"saturation":10}` |
| `paint.stroke` | `{"points":[[x,y,pressure],…],"size":20,"color":"#ff0000"}` |
| `select.rect` | `{"x":0,"y":0,"width":100,"height":50,"mode":"add","ellipse":false}` |
| `document.inspect` | `{}`: layer tree, history, selection bounds |
| `document.pixel` | `{"x":10,"y":10}`: composite RGBA |

UI-level commands (`file.open`, `file.save`, `view.zoomIn`, `window.theme.pro`, `edit.search`, …) are also accepted by `engine.execute` and `ui.menu.invoke`.

### Background jobs (#210)

Long commands (every `filter.*`, `edit.contentAwareFill`, `edit.contentAwareScale`, `file.automate.photomerge`, `brush.presets.importAbr`) and file opens run as background jobs in the desktop app: the window keeps drawing, the status bar shows progress with a Cancel button, and jobs that lock the active document show a modal progress dialog (Esc cancels). `engine.execute` still waits for the result by default; pass `"wait": false` to get `{job, pending: true}` at once, then poll `jobs.list` (`state`: running, done, failed, cancelled; `progress` 0–1; the result or error) and stop it with `jobs.cancel {job}`. A cancelled or failed job leaves the document unchanged. While a job runs, commands that would edit its document fail with "… is still running on this document". `ui.inspect` reports `jobs` (running jobs, opening files). Set `PHOTOCRAFT_INLINE_JOBS=1` to run everything inline.

## Preferences

Preferences (Edit › Preferences, grouped like Photoshop's dialog sections) live in the engine, so
the same commands work in the app, the CLI and headless MCP. Paths are dotted camelCase keys:
`<section>.<key>`, with sections `general`, `interface`, `workspace`, `tools`, `historyLog`,
`fileHandling`, `export`, `performance`, `scratchDisks`, `cursors`, `transparencyAndGamut`,
`unitsAndRulers`, `guidesGridAndSlices`, `plugIns`, `type`, `enhancedControls`, `rawDefaults`,
`integrations`, plus `shortcuts.<command id>`, `menus` (`hidden`, `colors.<id>`), `toolbar`
(`hidden`, `order`) and `colorSettings` (Edit › Color Settings).

| Command | Params |
|---|---|
| `prefs.get` | `{"path":"performance.historyStates"}`; no path returns everything |
| `prefs.set` | `{"path":"cursors.painting","value":"precise"}` or `{"values":{"unitsAndRulers.rulers":"cm","guidesGridAndSlices.gridColor":"#ff8800"}}`. Values are validated (choices, ranges, `#rrggbb` colours, shortcut syntax); a batch applies all or nothing. A section path takes an object and merges it key by key |
| `prefs.reset` | `{"path":"performance"}` (a section or key); no path resets everything |
| `edit.preferences.<section>` | the section's values; in the app (no params) it opens the Preferences dialog on that section |
| `edit.keyboardShortcuts` | `{"set":{"edit.fill":"Cmd+Shift+F"},"reset":true\|["id",…],"removeConflicts":true,"filter":"blur","list":false}`: returns overrides, matching commands and conflicts. A shortcut moved to another command is removed from its old owner unless `removeConflicts` is false. `""` removes a shortcut, `null` restores the default. The held temporary tools are bindable too: `tools.temporary.hand` (Space; also repositions a selection being drawn), `tools.temporary.zoomIn` (Cmd+Space), `tools.temporary.zoomOut` (Cmd+Alt+Space); they list with `"hold": true`. In the app it opens Keyboard Shortcuts and Menus |
| `edit.menus` / `edit.toolbar` | `{"hide":["edit.fade"],"show":[…],"color":{"edit.fill":"red"},"reset":false}` / `{"hidden":["Sponge"],"order":[…]}` |
| `edit.colorSettings` | `{"workingRgb":"display-p3","workingCmyk":"coated-cmyk","workingGray":"sgray","policyRgb":"preserve\|convert\|off",…,"askOnMismatch":true,"intent":"perceptual","bpc":true}`; honoured when opening files (`file.openAs`, the app's File › Open) and by Image › Mode and "working" profile specs. `color.profileMismatch {"action":"preserve\|convert\|discard\|assignWorking"}` answers the mismatch prompt |

Values the app honours live: history states (every open document), effect-cache budget, interface
theme, canvas colour and border, checkerboard size and colours (CPU and GPU canvas), gamut warning
colour and opacity, guide/grid/smart-guide colours and styles, grid spacing and subdivisions, ruler
units (rulers, Info panel, Image Size default unit), image interpolation (Image Size default),
painting and other cursors, zoom with scroll wheel, Use Shift Key for Tool Switch, keyboard
shortcuts, hidden and coloured menu items, autosave interval and crash recovery, and the history
log text file. GPU on/off and the GPU tile size apply at the next launch.

The desktop app stores them in `preferences.json` in the platform config directory (macOS
`~/Library/Application Support/Photocraft`, Windows `%APPDATA%\Photocraft`, Linux
`$XDG_CONFIG_HOME/photocraft`; override with `PHOTOCRAFT_CONFIG_DIR`); autosaves go to its
`Recovery` folder. In portable mode (a `portable.txt` or `PhotoCraft.portable` file beside the
executable, as in the Windows portable zip) that directory is `PhotoCraftData` next to the
executable instead. The web build keeps them in `localStorage`.

User and imported (`.abr`) brush presets live in the config directory's `Presets` folder: one
`.pcbrushes` JSON file per preset group, content-addressed tip bitmaps under `tips/`, and an
`index.json` with the group order and deleted built-ins (see `photocraft_engine::preset_store`).
The store loads in the background at launch and syncs after every brush preset change; built-ins
are never written. Headless CLI/MCP sessions and the web build keep brush presets for the session
only. Gradient presets (including imported `.grd` groups) persist with the preferences.

## Snapping

With View › Snap on, tool gestures snap to the View › Snap To targets (guides and grid while they
are shown, layer edges and centres, document bounds and centre, selection edges) within 8 screen
pixels: Move tool drags (the moved layers' bounds), marquee, crop, shape, type-box and pen points,
Free Transform handles and drags inside the box, and guides. Holding Ctrl disables snapping for
the drag. With View › Show › Smart Guides on, the Move tool also snaps to other layers and draws
magenta alignment lines. `ui.pointer` drives the same code, so agents get identical results.

## MCP bridge

`photocraft-automation` provides an MCP server built on the official Rust SDK (`rmcp`). It runs in one of two modes:

- **Headless** (`photocraft-cli mcp`): an in-process `photocraft_engine::Session`. There is no window.
- **Bridge** (`photocraft-cli mcp --bridge 127.0.0.1:7878 --control-token-file <path>`): every tool is forwarded to a running `photocraft --control 7878 --control-token-file <path>` over this protocol, so agents see and drive the live app.

The bridge keeps one authenticated TCP connection open. It reconnects and authenticates once if a request fails, and it skips reply lines whose `id` doesn't match the request (for example, stale replies to requests that timed out). It only accepts loopback addresses, because the app only listens on loopback. Supply its bearer token with `--control-token-file`, `--control-token`, `PHOTOCRAFT_CONTROL_TOKEN_FILE`, or `PHOTOCRAFT_CONTROL_TOKEN`:

```sh
photocraft-cli mcp --bridge 127.0.0.1:7878 \
  --control-token-file /private/path/photocraft-control.token
```

How each MCP tool maps onto control methods in bridge mode:

| MCP tool | Control method |
|---|---|
| `command_run {id, params, wait?}` | `engine.execute {command: id, params, wait}` |
| `jobs_list` / `jobs_cancel {job?}` | `jobs.list` / `jobs.cancel {job?}` |
| `command_list {filter?, enabled_only?}` | `engine.commands` (filtered by the MCP server) |
| `doc_new {…}` | `engine.execute {command: "file.new", params}` |
| `doc_inspect` | `engine.execute {command: "document.inspect"}` |
| `doc_open {path}` | `app.open {path}` |
| `doc_save {path}` / `doc_export {path}` | `app.save {path}` |
| `doc_render_preview {max_side?}` | `ui.screenshot`, returned directly as PNG image content |
| `session_list`, `ui_inspect` | `ui.inspect` |
| `ui_screenshot {max_side?}` | `ui.screenshot`, returned as PNG image content |
| `ui_pointer {events, modifiers?}` | `ui.pointer` |
| `ui_menu_invoke {id}` | `ui.menu.invoke` |
| `ui_set {fields}` | `ui.set` |
| `control_call {method, params}` | any method, passed through unchanged |

`doc_select` and `doc_close` work only in headless mode. The `ui_*` tools and `control_call` work only in bridge mode; in headless mode they return a tool error that explains how to start bridge mode.

**Security note:** TCP control uses a bearer token, not client identity or general per-method
authorization. A client that possesses the token receives the non-filesystem control surface,
including UI input, command execution, and application control. Keep token files private, do not
commit or log tokens, and do not pass a token directly on a shared system where process command
lines are visible. The protocol is unencrypted and must remain on loopback; do not tunnel or proxy
it to an untrusted host.

Filesystem access fails closed unless launch-time read and/or write roots are granted with
`--automation-read-root` and `--automation-write-root` (or
`PHOTOCRAFT_AUTOMATION_READ_ROOT` / `PHOTOCRAFT_AUTOMATION_WRITE_ROOT`). Request paths must be
non-empty, forward-slash relative paths beneath the applicable root. Absolute paths, parent
traversal, alternate separators, drive/device/stream prefixes, malformed components, and link
escapes are rejected before file effects. Read and write authority are independent; the parent of
a new output file must already exist. Engine commands that still use ambient filesystem paths are
disabled for automation until they are migrated to the same capability interface. Interactive
desktop file pickers retain normal user-selected access.

## Headless server

`photocraft-cli serve` keeps one headless engine session (no window, no GPU) and answers the same
JSON-lines envelope on stdio, or on `127.0.0.1:<port>` with `--port <port>` (loopback only; each
authenticated connection shares the session). TCP uses the same first-frame `auth` exchange and
token options as desktop control. Stdio does not require this TCP handshake because access is
inherited from the process pipe. It is the fastest way for a script or agent to make many edits:
no MCP framing, no app start-up per command. Configure its file access with the same
`--automation-read-root` and `--automation-write-root` flags. Implementation:
`crates/automation/src/rpc.rs`.

| Method | Params |
|---|---|
| `engine.execute` | `{command, params?, wait?}`: any engine command (`wait: false` starts a long one as a background job: `{job, pending}`) |
| `jobs.list` / `jobs.cancel` | `{}` / `{job?}`: background jobs (applying finished ones); cancel one or all |
| `engine.commands` | `{filter?}`: registry with params docs and enablement |
| `session.list` | open documents and the active index |
| `doc.open` / `doc.new` | `{path}` / `file.new` params |
| `doc.save` | `{path?, format?, quality?, index?}` (`.pcraft` native, else export by extension) |
| `doc.inspect` | `{index?}`: same JSON as `document.inspect` |
| `doc.render` | `{index?, maxSide? (1024; 0 = full), path?}`: PNG to `path`, else `{mime, base64}` |
| `doc.select` / `doc.close` | `{index}` / `{index?}` |
| `batch` | `{steps: [{command, params?} \| {method, params?}], stopOnError? (true)}` → `{completed, failed, results}` |
| `methods` | the list above |

```sh
printf '%s\n' \
  '{"id":1,"method":"doc.open","params":{"path":"in.jpg"}}' \
  '{"id":2,"method":"batch","params":{"steps":[{"command":"image.adjustments.invert"},{"command":"filter.blur.gaussianBlur","params":{"radius":3}}]}}' \
  '{"id":3,"method":"doc.save","params":{"path":"out.png"}}' | \
  photocraft-cli serve --automation-read-root /work/project --automation-write-root /work/project
```

The MCP server has the same batching as the `command_batch` tool (`{steps:[{id, params}], stop_on_error}`),
in headless and bridge mode.

## Transport limits

The desktop and headless TCP listeners currently enforce:

- a 1 MiB maximum encoded request line;
- an 8 MiB maximum encoded JSON reply, including the newline;
- at most 16 simultaneously serviced connections per listener;
- a 30-second socket read/write timeout;
- at most 256 steps in a headless `batch` or MCP `command_batch` request.

An oversized line, excess connection, unauthenticated request, or unauthorized filesystem path is
rejected before command dispatch or file effects. The headless JSON-lines stdio server also
enforces the request and reply byte ceilings. MCP tool results are checked as encoded JSON,
including the text/image content envelope, and the MCP bridge bounds incoming desktop replies.

Headless automation previews (`doc.render` / MCP `doc_render_preview`) allow a maximum requested
edge of 2048 pixels and a source document of at most 67,108,864 pixels. `maxSide: 0` (MCP
`max_side: 0`) still means full size, but fails if the document's longest edge exceeds 2048.
PNG results are capped at 5 MiB before base64 encoding or writing a rendered file. Preview
dimension/source checks run before compositing; the encoded-PNG check runs after encoding.
Explicit trusted-local CLI rendering keeps its existing behavior.

Batch replies have an aggregate byte budget with space reserved for the outer reply and ID.
Exhausting it stops later steps even when `stopOnError` is false. A result can exceed the reply
budget after an edit has run: the error says the operation may have completed. Earlier steps
are not rolled back; inspect state before retrying. The MCP bridge drops an oversized incoming
reply without automatically retrying the operation. Bridged screenshots are decoded with
8192-pixel edge, 16,777,216-pixel, and 64 MiB allocation ceilings, even without downscaling.

These ceilings do not implement explicit JSON-depth policy, total session/document-memory
accounting, compositor scratch-space accounting, command cancellation/duration limits, or
general per-method capabilities. Desktop screenshot capture/encoding and document import/export
still need their own operation budgets; the desktop reply ceiling applies after the UI creates
its response. A bounded output does not imply bounded command cost.

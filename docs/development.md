# Development guide

## Prerequisites

- Rust stable (1.90+). Add the web target with `rustup target add wasm32-unknown-unknown`.
- macOS, Windows or Linux. Linux needs `libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libgtk-3-dev`.

## Build and run

```sh
cargo run --release -p photocraft -- path/to/image.psd      # desktop app
cargo run --release -p photocraft -- --control 7878 --control-token-file .private/control.token \
  --automation-read-root work --automation-write-root work
cargo test --workspace                                     # everything
cargo xtask ci                                             # fmt + clippy + tests + layers + wasm
cargo xtask stats                                          # tests and lines per crate
cargo xtask parity                                         # Photoshop menu coverage -> docs/parity.md
```

Image code is slow at `opt-level 0`, so the workspace profile builds dependencies at `opt-level 2`. Use `--release` for anything interactive.

## Fonts (craft-fonts)

Font assets shared by the Crafting Apps live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), never in this repo: don't commit font files here (Inter and JetBrains Mono in `assets/fonts/` are the only exceptions; new fonts go to craft-fonts). The rules are in [`craftrules/standards/fonts.md`](../../craftrules/standards/fonts.md) ([on GitHub](https://github.com/storytold/craftrules/blob/main/standards/fonts.md)).

craft-fonts is an **optional build input**, never a Cargo dependency:

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR="$PWD/../craft-fonts" cargo run --release -p photocraft
CRAFT_FONTS_DIR="$PWD/../craft-fonts" cargo test --workspace     # runs the Japanese font tests too
```

- Use an absolute path (`$PWD/...`): `build.rs` runs in `crates/text`, so a relative `CRAFT_FONTS_DIR` would resolve from there.
- `crates/text/build.rs` reads `$CRAFT_FONTS_DIR/fonts/manifest.txt` and embeds the fonts as `photocraft_text::CRAFT_FONTS` (`crates/text/src/craft_fonts.rs`). Unset, `CRAFT_FONTS` is empty and the build is unchanged. A bad path is a build warning, or an error with `CRAFT_FONTS_REQUIRED=1` (release builds set both).
- **UI:** the Japanese fonts (BIZ UDPGothic Regular first) are the first Japanese fallback in the lazy CJK loader (`crates/ui-egui/src/cjk_fonts.rs`), ahead of the system Japanese fonts and in the same locale script order, appended last to every egui family with the usual baseline alignment.
- **Type tool:** the text engine registers them in `FontDb::new` (so also with no system fonts) and puts them first in the Japanese slot of the locale-ordered fallback list: BIZ UDPGothic for sans runs, Shippori Mincho / BIZ UDMincho for serif runs.
- **Web:** the wasm32 build never embeds craft-fonts, even with `CRAFT_FONTS_DIR` set: measured on 2026-10-06, the UI face alone took the release wasm from 24.19 MB to 28.87 MB, over the 24 MiB gate in `packaging/web/package.sh`. The web build therefore has no Japanese font yet (loading craft-fonts next to the wasm at run time would be the way to add one).
- Tests that need the fonts skip with a message when `CRAFT_FONTS` is empty; CI's Linux job runs the tests a second time with `CRAFT_FONTS_DIR` set. Desktop releases check out craft-fonts at the commit pinned in `.github/workflows/release.yml` (`CRAFT_FONTS_REF`; ci.yml pins the same commit) and ship each font's `OFL.txt` as `OFL-<family>.txt`.

## Graphics startup and device loss

`--safe-gpu` starts with the CPU renderer for one launch (no GPU canvas; a software adapter for the window where the platform has one: WARP on Windows, llvmpipe over GL on Linux). Before creating the wgpu device the app writes and locks `gpu-starting.json` in the config directory; it clears it once the first frames have rendered. A launch that finds an unlocked marker knows the previous start died inside the graphics driver (#4) and uses the next safer backend (Windows: Vulkan → DX12 → CPU; Linux: Vulkan → GL → CPU; macOS: Metal → CPU), remembering it in `performance.gpuBackend` (Preferences › Performance › GPU Backend, with **Reset GPU Backend**). With `auto`, Intel adapters on Windows use DX12. Help › System Info shows the adapter, backend, driver and fallback state.

If the device is lost while running (#243), every GPU entry point checks the device's health flag first, the canvas switches to the CPU compositor for the rest of the session and a notice says "GPU device was lost; using the CPU renderer." `ui.gpu.simulateLoss` triggers this path from the control channel.

## Environment variables

| Variable | Effect |
|---|---|
| `PHOTOCRAFT_CONTROL_PORT` | Same as `--control <port>` |
| `PHOTOCRAFT_CONTROL_TOKEN` | 64-hex bearer token for control TCP (avoid on shared systems where environment inspection is possible) |
| `PHOTOCRAFT_CONTROL_TOKEN_FILE` | Read, or create for a server, the control bearer-token file |
| `PHOTOCRAFT_AUTOMATION_READ_ROOT` | Directory capability for automation reads; requests use relative paths |
| `PHOTOCRAFT_AUTOMATION_WRITE_ROOT` | Separate directory capability for automation writes; requests use relative paths |
| `PHOTOCRAFT_CPU_CANVAS=1` | Force the CPU canvas path instead of the wgpu shader canvas |
| `WGPU_BACKEND=dx12` | Pick the wgpu backend(s) (`vulkan`, `dx12`, `metal`, `gl`); overrides `performance.gpuBackend` and the startup fallback |
| `PHOTOCRAFT_GPU_TILE=2048` | Force GPU canvas tiling (tests tile seams) |
| `PHOTOCRAFT_FX_NOCACHE=1` | Bypass the CPU layer-effect map cache (`compose::effect_maps`) |
| `PHOTOCRAFT_CPU_COMPOSE=1` | Keep the wgpu canvas but composite on the CPU (compare GPU vs CPU renders, e.g. with the snapshot example) |
| `PHOTOCRAFT_FX_TRACE=1` | Print the CPU time spent on GPU effect shapes and distance fields per rebuild |
| `PHOTOCRAFT_THEME_FILE=tokens.json` | **Debug builds only:** live design-token overrides, re-read on change |

### Live design tokens

```json
{ "card": "#323232", "tab_strip": "#262626", "accent": "#378ef0", "radius": 4 }
```

Keys are the field names of `theme::Tokens` (`crates/ui-egui/src/theme.rs`). Edit and save the file while the app runs to see changes immediately, with no recompile. This is compiled out of release builds.

## Driving the app programmatically

Start the app with a private token file. It then accepts authenticated JSON lines on `127.0.0.1:7878`:

```sh
TOKEN=$(tr -d '\r\n' < .private/control.token)
printf '%s\n' "{\"id\":\"auth\",\"method\":\"auth\",\"params\":{\"token\":\"$TOKEN\"}}" \
              '{"id":1,"method":"engine.execute","params":{"command":"layer.newAdjustmentLayer.hueSaturation","params":{"hue":30}}}' \
              '{"id":2,"method":"ui.screenshot","params":{"path":"evidence/shot.png"}}' | nc 127.0.0.1 7878
```

See `docs/control-protocol.md` for every method. Tips:

- **macOS does not render occluded windows.** `ui.screenshot` raises the window first (`focus: true` by default). Use `ui.focus` before other visual checks.
- **Capture only our own window.** For native captures (to see the real title bar and traffic lights), capture by window id (`screencapture -l <CGWindowID>`), never a screen region, which can grab other apps.
- **Pointer gestures:** `ui.pointer` takes events in *document* coordinates and runs them through the same tool state machine as the mouse.

## Headless CLI

`apps/photocraft-cli` builds the binary `photocraft-cli`:

```sh
cargo run -p photocraft-cli -- convert in.psd out.pcraft               # any supported format -> any
cargo run -p photocraft-cli -- info out.pcraft                          # JSON: size, mode, depth, layer tree
cargo run -p photocraft-cli -- run in.png --cmd layer.new.layer --params '{"name":"Ink"}' \
                                          --cmd filter.blur.gaussian --params '{"radius":3}' --out out.psd
cargo run -p photocraft-cli -- run --new '{"width":800,"height":600}' --cmd document.inspect
cargo run -p photocraft-cli -- batch --actions actions.json --in photos/ --out done/ --format jpg
cargo run -p photocraft-cli -- commands --filter blur                    # the command registry
```

`actions.json` is `[{"command": "<id>", "params": {…}}, …]`, which is the same shape as a recorded action. `run` prints one JSON line per command result.

## Native format (.pcraft)

`crates/format` defines the lossless native bundle. It is either a ZIP of STORE entries or a directory with the same layout:

- `manifest.json`: the versioned document tree, with migrations in `format/src/migrate.rs`.
- `tiles/<blake3>.zst` and `blobs/<blake3>.zst`: zstd-compressed objects, content-addressed by the BLAKE3 hash of their uncompressed bytes.
- `thumb.png` and `composite/preview.png`: previews.

Keep one `PcraftWriter` per open document: re-saving then only compresses and writes tiles that changed, and directory bundles garbage-collect unreferenced objects. `format::Autosaver` writes snapshots into a recovery directory on a background thread. `list_recovery` / `recover` / `discard_recovery` implement crash recovery.

`photocraft-io` routes `.pcraft` through this crate in `import`/`export`, detecting it by magic or by extension.

## MCP (agents)

`photocraft-cli mcp` serves MCP on stdio using `crates/automation`, which is built on `rmcp`:

- **Headless:** `photocraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>`. It drives an in-process engine session and has no file authority when a root is omitted.
- **Live app:** start `photocraft --control 7878 --control-token-file <private-path> --automation-read-root <dir> --automation-write-root <dir>`, then run `photocraft-cli mcp --bridge 127.0.0.1:7878 --control-token-file <private-path>`. The desktop process owns the roots. See `docs/control-protocol.md#mcp-bridge`.

Tools:

- `session_list`
- `doc_open`, `doc_new`, `doc_save`, `doc_export`, `doc_inspect`, `doc_render_preview` (returns a PNG image), `doc_select`, `doc_close`
- `command_list`, `command_run`, `command_batch` (several commands per call)
- bridge only: `ui_inspect`, `ui_screenshot`, `ui_pointer`, `ui_menu_invoke`, `ui_set`, `control_call`

Claude Code (`.mcp.json` in the repo root, or `claude mcp add`):

```json
{
  "mcpServers": {
    "photocraft": {
      "command": "/path/to/photocraft/target/release/photocraft-cli",
      "args": ["mcp"]
    },
    "photocraft-live": {
      "command": "/path/to/photocraft/target/release/photocraft-cli",
      "args": ["mcp", "--bridge", "127.0.0.1:7878", "--control-token-file", "/private/path/photocraft-control.token"]
    }
  }
}
```

```sh
cargo build --release -p photocraft-cli
claude mcp add photocraft -- "$PWD/target/release/photocraft-cli" mcp
```

`doc_inspect` (and the engine command `document.inspect`) reports the layer tree with kinds,
bounds, masks, selection, effects (`effects.items[].kind`), smart filters (`smartFilters[]`), type
text, adjustment settings, channels and history, so agents can verify what they did without a
screenshot. `crates/automation/tests/agent_tasks.rs` is the reference: ten realistic edit tasks
(title card, colour grade, undo/redo, editable smart blur, masks, saved selections, align,
capability-scoped export, resize/crop, CMYK + native save) driven purely over MCP.

Without MCP, `photocraft-cli serve [--port N]` keeps a headless session open and answers JSON lines
(see `docs/control-protocol.md#headless-server`).

A typical agent loop:

1. `doc_open {path}`
2. `command_list {filter:"blur"}`
3. `command_run {id:"filter.blur.gaussian", params:{radius:4}}`
4. `doc_render_preview` to check the result
5. `doc_save {path:"out.pcraft"}`

## Colour management

`crates/cms` is our own pure-Rust ICC engine (v2/v4 parsing, matrix/TRC and LUT profiles, all four
intents, black point compensation). It ships CC0 built-in profiles, including a synthetic
"Photocraft Coated CMYK", because Adobe's CMYK profiles are proprietary (see `crates/cms/README.md`).

- Documents carry an optional embedded ICC profile (`Document::icc_profile`); `edit.assignProfile`
  and `edit.convertToProfile` change it. Mode changes (`image.mode.*`) convert through cms.
- **Proof Colors** (⌘Y), **Proof Setup** and **Gamut Warning** (⇧⌘Y) bake a 3D LUT
  (`cms::Lut3d`) that the canvas shader applies; the document pixels never change.
- Convert colours with `photocraft_cms::transform::cached(src, dst, opts)`: transforms are cached
  process-wide and integer buffers use precomputed tables or a device link.

## Menu parity

`cargo xtask parity` compares Photoshop's menu tree (`crates/ui-egui/src/menu_catalog.rs`) with
the live command registry (`menus::is_live`) and rewrites [`docs/parity.md`](parity.md). The test
`parity::tests::parity_does_not_regress` fails if the live count drops below `parity::FLOOR`.

## Testing strategy

- **Unit and property tests** in every crate. proptest is used for tile COW, regions and codecs.
- **Format crates:**
  - synthetic generators (`psd::testgen`)
  - byte-exact round trips
  - malformed-input sweeps (truncate at every offset)
  - fuzz targets (`crates/*/fuzz`)
- **Real-file corpora** in `corpus/` (gitignored, fetched at pinned commits, sha256-verified): opt-in locally through the `corpus` cargo feature, always run in CI. See [Test corpora](#test-corpora).
- **Composite oracle:** a PSD's embedded merged image is compared with our compositor's output. The pass rate is tracked in the roadmap.
- **UI:** unit tests for widgets and state, plus screenshot checks through the control channel.

## Performance notes

- The canvas is presented by a custom WGSL shader (`ui-egui/src/gpu_canvas.rs`): mip-mapped/nearest sampling, procedural checkerboard, pixel grid, tiling. Brush strokes upload only their damage rect.
- **The canvas composites on the GPU** (`photocraft-gpu`, driven from `gpu_canvas.rs`), layer effects included. What the planner can't express returns `Unsupported` and the canvas falls back to the CPU compositor (`photocraft-compose`, also the reference for export and tests): Multichannel documents, and documents or effect regions over the texture limit. Pieces the GPU can't derive itself are rasterised once on the CPU and cached (`compose::masks` for vector masks, `compose::shape_split` for stroked shapes with clipped layers, effect distance fields). Timings of the interactive paths: `cargo run --release -p photocraft-ui-egui --example interactive_bench`; effects: `--example fx_bench` (`--compare files…` for GPU vs CPU); large documents (open, refresh, thumbnails, a filter, a stroke, saves; one operation per run so `/usr/bin/time -l` gives its peak memory): `--example large_image_bench -- --size 14000x14000 --op psd`. The app requests the adapter's own texture limit (egui's default is 8192 px; see `gpu_canvas::use_adapter_limits`); beyond it the CPU fallback composites and uploads in bands (`compose::render_bands`), and exports, thumbnails and flattening stream bands too, so no full-size float composite is ever held. Rendering fidelity: `cargo run --release -p photocraft-io --example oracle_diff -- corpus/psd` (the whole PSD oracle table in seconds). `ui.inspect` → `perf.timings.gpuFallback` names the reason (`null` on the GPU path).
- **Layer effects on the GPU** (`gpu/src/fx.rs`, kernels in `gpu/src/compose.wgsl`). Every enabled effect becomes a *map program* over the layer's effect region (shift, dilate, Gaussian blur, glow ramp, bevel height and shading, contour, stroke band), mirroring `compose::effects` step by step; the chunked composite then paints through the maps, clipped to that region, and copies the result back into the backdrop in place, so a small text layer costs only its own pixels. The layer's shape (`compose::layer_shape`) and its distance fields (`compose::effects::distance_field`: a sequential transform whose tie-breaking a parallel GPU pass can't reproduce bit for bit) come from compose on the CPU, computed in parallel bands. Everything is cached per layer state: an unrelated edit, or an effect's colour or opacity, rebuilds nothing; a brush dab recomputes the touched 256² tiles grown by the effect reach; changing one effect's geometry rebuilds that effect only. Cache budget `gpu::FX_BUDGET` (1.5 GB, least recently drawn layers evicted first). `PHOTOCRAFT_FX_TRACE=1` prints the CPU time of each rebuild.
- The canvas grows a stroke's damage rect by the effect reach of the layers around it (`canvas::effect_reach`), so effects beyond the dab refresh too (on both paths).
- **Numbers** (7360 × 4912, 8 text layers + one painted layer with drop shadow + stroke + bevel, Hue/Saturation on top; M4 Pro; `cargo run --release -p photocraft-ui-egui --example fx_bench`), CPU fallback → GPU: full refresh with warm effect maps 5.7 s → 47 ms; Hue/Saturation tweak above the effects 4.7 s → 31 ms; brush dab on a plain layer 32 → 1.7 ms; brush dab on the effect layer (its maps rebuilt around the dab) 659 → 3.7 ms; first refresh (all maps built) 6.4 s → 0.39 s. With the CPU ~14× oversubscribed by parallel builds (min of 7 runs): 8.1 s → 0.33 s, 9.9 s → 0.28 s, 28 → 2.2 ms, 1.7 s → 89 ms; moving a text layer 7 px 336 → 13 ms. `fx_bench --compare corpus/psd/…/*.psd` reports the GPU vs CPU difference on real files (30 of the 31 corpus files with effects render on the GPU, worst 0.12/255).
- On the CPU path `compose::effect_maps` caches shadow, glow, bevel and satin maps per layer state (LRU, 768 MB budget).
- Live adjustment previews on large documents use a downsampled proxy (`ui-egui/src/proxy.rs`).
- `ui.inspect` returns `perf` timings (UI ms per frame, composite ms, upload ms).
- Never scan full surfaces per frame. Cache per document revision (`PhotocraftApp::cached_bounds`). An uncached `content_bounds()` on a 36 MP layer once cost 77 ms per frame.

## Scorecard and performance budgets

[`docs/scorecard.md`](scorecard.md) says, with numbers, where PhotoCraft stands per area. It is
generated; never edit it by hand.

```sh
cargo xtask scorecard            # regenerate docs/scorecard.md
cargo xtask scorecard --check    # what CI runs: fails if the committed file is stale
cargo xtask perf                 # full benchmark run (tens of minutes, release build)
cargo xtask perf --quick         # small synthetic documents (about a minute once built)
cargo xtask perf --update-baseline   # also write perf/baseline.json from this run
```

**Sources.** The scorecard reads only committed files, so it is deterministic:
- `scorecard/*.toml`: one checklist per area (tools, file compatibility, UI, type, automation,
  reliability, distribution, open bugs). Each item has an `id`, a short `target`, a `status`
  (`done`, `partial` or `missing`), the `issue` and a `note` with the evidence (file:line).
  Statuses must be true on `main`: verify against the code, and say `partial` with a note when
  unsure. Flip a row in the PR that changes it.
- `perf/budgets.toml`: the benches `xtask perf` runs and the scenarios P1…Pn, each mapped to one
  bench row, with a budget (targets from #209, #210, #211). `enforce = false` marks a target the
  code doesn't meet yet (reported as over budget; only regressions fail); flip it to `true` in the
  PR that meets it. Scenarios the code can't support yet carry `not_measurable` and a reason, never
  a number.
- `perf/baseline.json`: the numbers the scorecard shows, written only by
  `cargo xtask perf --update-baseline`, with the machine (CPU, RAM, GPU adapter, OS), its machine
  class, commit, date and load average.
- `crates/io/tests/corpus.rs`: the corpus floors (every `Source { .. }` constant).
- The prefs audit: `Preferences` fields that no code reads (target 0, #204), plus counts taken
  from the tree (never-crash attribute coverage, `docs/parity.md`).

**Perf runs.** `xtask perf` builds the benches in release (`perf_scenarios`, `interactive_bench`,
`fx_bench`, `type_bench`, `large_image_bench`, and `layout_bench` once it exists), runs each with
`--json`, and merges the reports into `target/perf/results.json` keyed by scenario id, with p50,
p95 and max (nearest rank over the samples), peak RSS measured in-process (`photocraft-testkit`'s
`perf::RssSampler`), GPU bytes held by the canvas, and the load average before and after each
bench. `target/perf/summary.md` is the Markdown table. It exits non-zero when an enforced budget
breaks, when a scenario's p50 regresses more than 15 % (`--threshold`, or `regression_pct`)
against a baseline from the same machine class and mode, or when a bench fails. Runs from
different machine classes are never compared, and `--quick` numbers are only compared with a
quick baseline. Machines that run other work give noisy numbers: check the load average in the
summary before trusting a regression, and record it next to any number you quote.

**Adding a scenario.** Add a row to a bench (keep its name stable: it is the key), give the
bench `--json` support through `photocraft_testkit::perf::{row, report, write_report}`, then add a
`[[scenario]]` to `perf/budgets.toml` and run `cargo xtask scorecard`.

**CI.** `ci.yml` runs `cargo xtask scorecard --check`. `perf-nightly.yml` runs `cargo xtask perf`
on a fixed macOS runner every night (and on demand), posts the table as the job summary, uploads
`results.json`, and fails on a broken budget or regression. It never runs on pull requests: the
release build of the benches alone takes longer than PR CI should. To give the nightly runner
its own baseline, run the workflow by hand with `update_baseline` and commit the `perf-baseline`
artifact as `perf/baseline.json`.

## Web build

`apps/photocraft-web` runs the same `PhotocraftApp` in the browser through eframe's web runner. The renderer is wgpu: WebGPU where the browser has it, WebGL2 otherwise. It is Rust only. The only JavaScript is the glue that wasm-bindgen generates.

```sh
brew install trunk                 # or: cargo install trunk --locked
cd apps/photocraft-web
trunk build --release              # writes ../../dist/web (index.html, .js glue, .wasm)
trunk serve --release              # dev server on http://127.0.0.1:8765
```

Any static file server works for `dist/web`, for example `python3 -m http.server 8765` run inside that directory. Trunk downloads the matching `wasm-bindgen` and `wasm-opt` itself. `trunk build --release` uses the `wasm-release` Cargo profile (`data-cargo-profile` in `index.html`: fat LTO, opt-level "s" except the pixel crates). The `.wasm` is about 18.8 MiB raw, 7.8 MiB gzipped and 5.6 MiB with Brotli; serve it with compression. Keep it under 24 MiB (`packaging/web/package.sh` enforces this; Cloudflare's per-file cap is 25 MiB). The web build never embeds craft-fonts (see Fonts above). To see where the bytes go, run `twiggy top -n 40` on `target/wasm32-unknown-unknown/wasm-release/photocraft-web.wasm` (before wasm-opt strips the names).

URL flags: `?webgl` forces the WebGL2 backend, and `?cpu` forces the CPU canvas path.

How the web shell (`apps/photocraft-web/src/web.rs`) differs from desktop:

- **Open** uses `rfd::AsyncFileDialog`. The bytes arrive asynchronously in `Services::inbox`, which the app drains every frame.
- **Save / Save As / Export** trigger a browser download of the encoded bytes. The shell does this with a Blob, an object URL and a temporary `<a download>`, all created from Rust. There is no save dialog, so the suggested name becomes the download name.
- **Drag-and-drop:** `WebShell` takes the frame's `dropped_files` before the app sees them. It reads each file with `DroppedFile::bytes_async` and pushes the bytes into the inbox.
- **No control server:** browsers can't listen on TCP. To automate the web build, drive headless Chrome with `--remote-debugging-port`. `Page.setInterceptFileChooserDialog` plus `DOM.setFileInputFiles` covers Open, `Input.dispatchDragEvent` with `files` covers drops, and `Browser.setDownloadBehavior` captures downloads.
- Headless Chrome on macOS (`--headless=new --enable-unsafe-webgpu`) gets a real WebGPU adapter.


## Offscreen UI snapshots (no window)

Render the full UI headlessly with the real wgpu canvas, e.g. for design reviews or when the app
window would be occluded (macOS doesn't render occluded windows, so live `ui.screenshot` must raise
the window and steal focus):

```sh
cargo run --release -p photocraft-ui-egui --example snapshot -- \
    --out ui.png --size 1440x900 --scale 2 --open photo.jpg \
    --script '[["ui.set", {"tool": "type"}], ["ui.menu.invoke", {"id": "image.canvasSize"}]]'
```

`--script` is a list of control-protocol calls (`[method, params]`), applied in order.
Input calls (`ui.key`, `ui.type`, `ui.click`) now reply only after the app has processed the events,
so a following `ui.inspect` observes their effect.


## Test corpora

Real files are our best oracles, but they are large binaries, so they never go into this
repository. They live in `corpus/` (gitignored) and are fetched at **pinned commits** and checked
against committed **sha256 manifests**. All pins are in one place:
[`xtask/src/corpus_pins.rs`](../xtask/src/corpus_pins.rs).

| Directory | What | Source | Manifest |
|---|---|---|---|
| `corpus/photoshop/` | 256 PSDs we authored with Photoshop: smart filters, layer-style effect shapes, the text engine, adjustments in every mode and depth | https://github.com/storytold/photocraft-corpus (ours, MIT OR Apache-2.0) | `xtask/photoshop-corpus.sha256` |
| `corpus/psd/` | 170 small psd-tools and ag-psd files, the mix most PSD tests use | psd-tools and ag-psd upstreams (MIT) | `xtask/psd-corpus.sha256` |
| `corpus/psd-tools/` | the complete psd-tools test set (309 files) | psd-tools upstream (MIT) | `xtask/psd-tools-corpus.sha256` |
| `corpus/pngsuite/` | PngSuite | schaik.com release archive (public domain) | (fixed archive) |

```sh
cargo xtask corpus                 # where each corpus lives, its pin, present or missing
cargo xtask corpus --all           # fetch everything missing or stale (cold: about 15 s; verified copies are left alone)
cargo xtask test-corpus            # fetch, then cargo test --release --features corpus on psd, codecs, io, engine
cargo xtask test-corpus -p io      # narrow to one crate (repeat -p for more)
cargo xtask test-corpus --changed  # only if psd, io, codecs, compose, gpu, text or format changed vs origin/main
cargo xtask test-corpus -- --nocapture   # pass arguments to the test binaries (per-file tables)
scripts/fetch-corpus.sh            # the same as cargo xtask corpus --all
```

**Opt-in locally, strict when opted in, always in CI.**

- The corpus tests sit behind the `corpus` cargo feature of `photocraft-psd`, `photocraft-codecs`,
  `photocraft-io` and `photocraft-engine`, so plain `cargo test` neither compiles nor needs them.
- With the feature on, a missing corpus is a failure ("run `cargo xtask corpus --all`"), never a
  silent skip, and every floor is enforced.
- If you touch psd, io, codecs, compose, gpu, text or format, run `cargo xtask test-corpus` before
  you finish.
- The CI `corpus` job runs `cargo xtask corpus --all` and then `cargo xtask test-corpus` on every PR
  and on main. `corpus/` is cached with `actions/cache`, keyed on the hash of `corpus_pins.rs` and
  the manifests. A download happens only when a pin changes; otherwise the cache restores in seconds
  and is re-verified (sha256) before the tests run. Fork PRs read the base branch's cache.

**Our own corpus: [photocraft-corpus](https://github.com/storytold/photocraft-corpus).**

- **Authoring clone:** a normal git clone, `git clone git@github.com:storytold/photocraft-corpus.git`,
  placed next to this checkout (`../photocraft-corpus`). Its generators write there, and you commit
  and push there.
- **Consuming copy:** `corpus/photoshop/` here. It is a plain directory: the pinned snapshot
  (downloaded as the codeload tarball of the pinned commit, then verified).
- **Local mode:** `cargo xtask corpus --photoshop --local` copies `photoshop/` from
  `../photocraft-corpus` (or `PHOTOCRAFT_CORPUS_REPO=<path>`) instead of downloading. It warns when
  the clone's HEAD isn't the pin or its files differ from the manifest. Use it to test regenerated
  files before pushing: `cargo xtask test-corpus --local` copies them and runs every corpus test.
- **Bumping the pin:** commit and push in photocraft-corpus, then in a PR here set
  `PHOTOCRAFT_CORPUS_COMMIT` in `corpus_pins.rs`, run
  `cargo xtask corpus --photoshop --update-manifest`, commit the manifest diff, run
  `cargo xtask test-corpus`, and raise floors that improved.
- **Not a submodule or a subtree:** a subtree would put the binaries back into this repository's
  history, and submodules cause init and detached-HEAD friction for every contributor. A pin plus a
  manifest gives the same reproducibility.
- The third-party sets (psd-tools, ag-psd, PngSuite) are fetched from their upstreams the same way,
  and are never copied into photocraft-corpus.

## Rendering fidelity (PSD oracle)

`cargo xtask test-corpus -p io -- --nocapture` compares our composite of every corpus PSD with
Photoshop's own merged image (PASS ≤ 2/255) and checks that export → re-import renders the same,
per source with its own floors: `corpus/psd`, the psd-tools set (also a truncation/corruption sweep
over every file that must never panic) and our Photoshop set `corpus/photoshop` (per-feature-group
totals: `smart-filters`, `effects`, `text`, `adjustments/<mode><bits>`). Smart objects and type
layers composite Photoshop's cached pixels there; `cargo xtask test-corpus -p engine -- --nocapture`
(`crates/engine/tests/photoshop_oracles.rs`) re-renders them with our smart-filter stack and text
engine and is the failure map for both. A panic is reported as `CRASH` and fails the run. Files without a real merged image (Maximize Compatibility off)
are judged against their embedded thumbnail instead (`PASS (thumbnail)`, a strict low-resolution
check); a file only SKIPs when it has no oracle at all. Raise the floors in `crates/io/tests/corpus.rs` when they
improve; never lower them. Synthetic reproductions of corpus findings live in
`crates/io/tests/corpus_regressions.rs` (corpus files are never committed). To dig into one file:

```sh
cargo run --release -p photocraft-io --example oracle_diff -- corpus/psd/<file>.psd 0 png /tmp/diff.png
```

writes ours | Photoshop | a diff heatmap side by side; `col`, `row`, `worst [n]`, `grid x0 y0 x1 y1 [ch]`,
`layerpx x y` and `DUMP_FX=1` print samples, the worst pixels, value grids, per-layer pixels and raw
effect descriptors. Findings so far: fill-layer gradients are framed by the layer's mask bounds;
Photoshop's gradient Smoothness is a Catmull-Rom blend and "Perceptual" interpolation is Oklab
(baked into dense stops on import, `crates/io/src/gradient_bake.rs`); a shape layer's vector
stroke is drawn above its clipped layers; linked effect patterns tile from the layer's `fxrp`
reference point; stroke distances follow a 5 × 5 chamfer metric (1, √2, √5) seeded at sub-pixel
edge offsets; interior effects keep the layer's alpha; outside strokes blend onto the backdrop with
their own modes, an upper stroke covering lower ones.

# Roadmap

Status legend: ✅ done · 🟡 in progress · ⬜ not started. Updated 2026-10-05.

**Parity metrics.** `cargo xtask parity` measures how much of Photoshop's menu tree is *wired to a
command* and writes [`parity.md`](parity.md). It does **not** measure whether those commands behave
like Photoshop, feel right, or survive real files. Read it next to the PSD composite oracle, the
test count, and the [honest parity assessment](#honest-parity-assessment-2026-10-05) below, which
is the reference answer to "how close are we really". The weighted feature estimate in
[`parity-estimate.md`](parity-estimate.md) (~82%) counts feature surface and overstates user-facing
readiness; treat it as an upper bound.

| M | Status | Where we are |
|---|---|---|
| M0 Skeleton | ✅ | workspace, xtask (layers / wasm / ci / stats / corpus / parity), CI workflow |
| M1 Foundation | ✅ | geom, color (27 blend modes), raster (COW tiles, any depth), doc, ops, cms (ICC) |
| M2 PSD v1 | ✅ | photocraft-psd: 134/135 real files byte-exact round trip |
| M3 Viewer app | ✅ | egui shell (Pro / Studio / Classic themes), 13+ codecs, native and web (trunk) builds |
| M4 Native format + engine | ✅ | 500+ commands, `.pcraft` (incremental, autosave, crash recovery), CLI, persistent preferences |
| M5 GPU compositor | 🟡 | wgpu compositor drives the canvas, layer effects, vector masks, artboards, pattern fills and every clip case included (≤1/255 vs CPU); Multichannel documents fall back to the CPU |
| M6 Paint + select | 🟡 | brush engine, all selection tools, multi-layer selection, snapping + smart guides, free transform + warp; stylus pressure on Windows (WM_POINTER via winit), the web (Pointer Events, with tilt/twist), macOS (AppKit event monitor: pressure, tilt, rotation, eraser end) and Linux X11 (XInput2 raw valuators); Linux Wayland (tablet-v2) still open (#79) |
| M7 Adjust + filters | 🟡 | 16 adjustment layers + destructive-only adjustments, 70+ filters incl. Blur Gallery, Actions record/replay, Fade |
| M8 PSD v2 | 🟡 | adjustments (incl. Selective Color, Color Lookup), fills, effects, patterns, text, shapes, smart objects, alpha channels; oracle 111/170 |
| M9 Text, vector, styles | 🟡 | type engine + Warp Text, shapes / pen / paths, all 10 effects on CPU and GPU (parity ≤1/255, 30/31 corpus effect files on the GPU) |
| M10 Smart features | 🟡 | classical Select Subject / Object, content-aware fill and scale, healing, auto-align / auto-blend; ML backend not started |
| M11 Automation + formats | 🟡 | MCP (headless + live bridge), batch, Image Processor, prefs over MCP; DoD test passes (10 agent tasks over MCP, `automation/tests/agent_tasks.rs`); JP2 / DICOM / DPX / C2PA pending |
| M12 Pro parity | 🟡 | CMYK / Lab / Indexed / Bitmap / Duotone, ICC + soft proofing, channels + Quick Mask, smart-object stack modes, artboards, layer comps; print, HDR, photomerge, timeline pending |

**Menu parity: 532 / 625 (85.1%)** on 2026-10-01, up from 224 (35.8%) the day before. See [`parity.md`](parity.md).

## Honest parity assessment (2026-10-05)

This is the reference answer to "how close are we to Photoshop parity, really". Agents: read it
before picking work. Update it (with dated measurements) when the numbers move; don't restate the
menu-parity number in its place.

**Measured numbers live in the [scorecard](scorecard.md)** (`cargo xtask scorecard`, kept current
by CI): performance scenarios against their budgets, corpus floors, per-area checklists with
done / partial / missing counts, and the number of settings that do nothing. Where the scorecard
and the estimates below disagree, the scorecard's numbers supersede them. As of 2026-10-05 it
counts 67 of 129 preferences that nothing reads; 3 of the 25 budgeted performance scenarios meet
their #209–#211 targets (first baseline, taken on a heavily loaded machine), and the 150-layer
nudge scenario crashes the GPU compositor; the open area issues' checklists (#203–#220) are
almost entirely missing or partial.

**Bottom line.** Two days after 0.2.0 we had merged ~96 PRs and closed ~48 issues, but **real
Photoshop parity is still well below 50%**. The biggest gaps are AI, missing tools, professional
workflow depth and the plug-in ecosystem. Most fixes since 0.2.0 have passed our tests but have
**not yet been validated by users**, and this week proved our tests miss what users hit:
on first public use they found broken basics (text selection offset, 214 shortcut failures, panels
resizing themselves, an immovable crop frame, folders that wouldn't collapse, lag on layout PSDs)
that all counted as "live" in `parity.md`.

**Overall.** Feature surface: roughly **60–70%** of what a typical Photoshop user touches exists in
some form. "A professional could switch for daily work": roughly **25–35%**. True 1:1 parity is
**many months** of focused work, and some areas need product decisions rather than effort.
Confidence: moderate — the next users of 0.2.x will move these numbers either way.

### By dimension

| Dimension | Measured / evidence (2026-10-05) | Grade | Notes |
|---|---|---|---|
| Menu wiring | 626/626 menu items dispatch a command (`parity.md`) | high but shallow | Says nothing about behaviour. |
| PSD fidelity (rendering) | Corpus oracle 115/170 (68%): 30 differ, 26 have no usable reference, 1 import error | medium | Push to 170/170 under way (effects/strokes, multi-instance effects, 16/32-bit and colour modes, references for skipped files). |
| PSD round trip | 169/169 re-import identically; every adjustment layer and blend mode round-trips | high (within corpus) | Floors in `crates/io/tests/corpus.rs`; raise, never lower. Corpora: `cargo xtask corpus --all` (ours: https://github.com/storytold/photocraft-corpus). |
| Smart filters / text / effect shapes in PSDs | Measured on our Photoshop-authored set (https://github.com/storytold/photocraft-corpus, `corpus/photoshop`, 258 files): see the per-group floors in `crates/io/tests/corpus.rs` and `crates/engine/tests/photoshop_oracles.rs`. Smart objects and smart filters now survive PSD save and open (41 corpus files, 206 smart objects, round trip strict; Photoshop opens our exports with live filters); re-rendering Photoshop's smart filters with ours matches 5/30 (was 1/30) | low–medium | Remaining re-render gaps are filter maths (Gaussian/Motion Blur, Unsharp Mask, Emboss, Add Noise RNG) and bicubic placement. |
| Core editing (layers, masks, selections, adjustments, filters, transforms) | Broad engine coverage; many interaction bugs fixed after 0.2.0 (adjustment dialogs, Curves, crop, Move/Transform modifiers, gesture origin) | medium | Fixes not yet user-validated. |
| UI / UX polish | Shortcut audit 214 → 0 failures; dock, Layers rows and menus reworked; first visual-QA sweep found 14 defects (#147–#157) | low–medium | Needs recurring visual QA with realistic documents. |
| Tools | ~20 Photoshop tools missing: Pencil, Mixer Brush (tool), Patch, Content-Aware Move, Red Eye, Pattern Stamp, Art History Brush, Freeform/Curvature Pen, anchor tools, Direct Selection, Magnetic Lasso, single row/column marquee, Color Sampler, Perspective Crop, Rotate View, the Vertical Type tool (vertical layout itself landed, #199: toggle via Type › Orientation) and type masks, Frame | low–medium | Magic/Background Eraser added; live gradients in progress (#180). |
| Painting | Brush model and Brush Settings panel near Photoshop; .abr/.grd import; persistent presets; pen pressure/tilt on Windows, web, macOS and X11 | medium | Wayland pen input open (#79); macOS/X11 pressure not yet verified on tablet hardware. |
| Text / typography | Engine works; caret placement and size editing fixed; OpenType features, text-on-path editing, composer parity partial | medium-low | Measure with the Photoshop-authored set. |
| Colour management | Colour-managed canvas (document → monitor), embedded CMYK profiles, linear EXR/HDR, 16-bit float canvas | medium-high | Monitor profile follows only at launch. |
| Performance | 14k+ px on the GPU at ~⅓ the memory; adjustment preview 285 ms → 4–9 ms; font-size edits 297 ms → 4.6 ms | medium-high on rasters | Complex layout documents still laggy (#125/#128); >16384 px GPU tiling in progress (#49). |
| Stability | Never-crash lint series, crash guard, `panic_hunt` fuzzing in the gate | medium-high | No field crash data yet. |
| Camera RAW | DNG, CR2, Sony ARW (lossless + compressed), RW2, uncompressed ORF | medium | Nikon compressed NEF, CR3, RAF blocked by clean-room limits (#50). |
| AI / generative | none | ~0% | Deferred by decision (#41). |
| Ecosystem | Sandboxed WebAssembly plug-ins instead of .8BF; no ExtendScript/UXP/.atn; no Adobe Fonts/Libraries/cloud docs | low | By design for 8BF; scripting compatibility open. |
| Platforms | macOS (notarized), Windows, Linux (AppImage/deb/rpm/Flatpak bundle), web | medium-high | Flathub later (#173); Windows signing material pending. |

### Where we're going (priority order)

1. **Ship the fixes:** cut 0.2.1 once the current batch lands, so users validate them.
2. **PSD fidelity to 170/170** with round trips, plus the Photoshop-authored reference set for smart
   filters, the text engine and effect shapes (https://github.com/storytold/photocraft-corpus, fetched by
   `cargo xtask corpus --all`); enforce floors in `corpus.rs`.
3. **Workflow acceptance tests:** 30–50 real tasks (retouch a portrait, social post with text and
   effects, composite with masks and adjustment layers, CMYK print prep…) scripted end to end and
   checked against Photoshop's output on every build. Their pass rate becomes the headline parity
   number.
4. **Missing tools,** starting with the Pen variants and Direct Selection, Patch / Content-Aware
   Move, Pencil, Rotate View, Perspective Crop.
5. **Complex-document performance** (#125/#128) and GPU tiling beyond the texture limit (#49).
6. **Recurring visual QA** (`cargo run -p photocraft-engine --example designer_psd`) and fast
   turnaround on user reports (OS, document size, layer count, screenshot).
7. Later / needs decisions: generative AI backend (#41), scripting compatibility (ExtendScript /
   UXP / .atn), Flathub (#173), Wayland pen pressure (#79).

## Current focus (infrastructure before the long tail)

Landed on 2026-10-01:
- multi-layer selection, live smart objects + smart filters, alpha channels + Quick Mask;
- patterns, Warp, preferences, snapping, ~33 filters, the remaining core adjustments;
- Layer Comps and Artboards, GPU layer effects, Liquify / Puppet Warp / Perspective Warp;
- the PSD fidelity pass (oracle 102 → 111), the M11 MCP acceptance test, and the release pipeline
  (`docs/releasing.md`).

Next:
1. **Follow "Where we're going" in the assessment above.** 0.2.0 shipped signed and notarized on
   2026-10-05 (`docs/releasing.md`); Windows code-signing material still needs to be obtained.
2. **Fidelity**: the PSD oracle (113/170). Modern Brightness/Contrast and grayscale Levels
   curves; chisel-soft / stroke-emboss bevel shapes; Photoshop's 8-bit blend rounding; non-Normal
   modes in Lab documents; smart-filter maths against Photoshop's (re-render oracle 5/30).
3. **GPU**: only Multichannel documents and regions over the texture limit fall back to the CPU.
4. **Vanishing Point, Camera Raw / Lens Correction, Face-Aware Liquify** (needs a landmark model).
5. **Panels**: Patterns, Styles, Glyphs, Character/Paragraph Styles, Timeline; Custom Shape tool.
6. Print, Photomerge, Merge to HDR, video layers.

## Milestone definitions

Each milestone has a **definition of done (DoD)** and must leave `main` green on all Tier-1 platforms plus a wasm build.

| M | Name | Scope (key items) | DoD / acceptance |
|---|---|---|---|
| **M0** | Skeleton | Workspace, all crate stubs, lints, `xtask` (layers check, ci), CI matrix, testkit | `cargo test --workspace` green; `cargo check --target wasm32-unknown-unknown -p photocraft-engine -p photocraft-ui-egui` green; layering check passes |
| **M1** | Foundation | geom, color (formats, blend math for all 27 modes, sRGB/linear), raster (sparse COW tiles, U8/U16/F32), doc (full type model incl. CMYK/Lab/adjust/smart), ops (history) | Property tests (proptest) on tile COW and history; blend-mode reference tests against published formulas |
| **M2** | PSD v1 | `photocraft-psd`: header, resources, layer records, channel data (raw/RLE/ZIP/ZIP+pred), masks, groups (lsct), unicode names, unknown-block passthrough, merged image, PSB; writer | Round-trip byte-stability tests; synthetic PSD generator tests; fuzz target; ≥150 unit tests |
| **M3** | Viewer app | codecs (png/jpeg/tiff/webp/gif/bmp/tga/pnm/qoi/exr/hdr, all read+write), CPU compositor, egui shell (menu from registry, canvas pan/zoom, layers panel, history), open/save, native + web build | Opens PNG/JPEG/PSD, shows layers, toggles visibility, undo/redo, saves PNG/PSD; web build loads a file from the browser |
| **M4** | Native format + engine | `.pcraft` bundle, command registry with schemas, jobs/cancellation, snapshots via arc-swap, CLI (`convert`, `run`, `inspect`) | CLI parity tests: every GUI command also runs headless |
| **M5** | GPU compositor | wgpu planner backend, tile residency, parity tests vs CPU (all blend modes), viewport on GPU, mips | GPU vs CPU ≤1/255; pan/zoom 60 fps on a 100 MP / 20-layer document |
| **M6** | Paint + select | platform input (pen: macOS/Windows/Linux/web), brush engine v1, eraser, marquee/lasso/wand, move, free transform, crop | Brush latency <1 frame; selection-restricted paint tests |
| **M7** | Adjust + filters | 16 adjustment layers, first 30 filters, schema-generated dialogs + live preview, actions record/replay | Golden tests at 8/16/32f; action replay determinism |
| **M8** | PSD v2 | adjustment layers, fill layers, lfx2 effects (descriptor parser), text (EngineData) preserve+render, smart objects, vector masks | Composite-oracle pass rate ≥90% on corpus |
| **M9** | Text, vector, styles | parley text layers, shapes/pen/paths, all 10 layer effects on GPU | Visual goldens; PSD text round-trip |
| **M10** | Smart features | `ml` (ort native / ort-web on the web), Select Subject/Object/Sky, Remove BG, Remove tool, content-aware fill, healing, AI denoise, RAW develop | Quality benchmarks on a public dataset; timing budgets |
| **M11** | Automation + formats | MCP server, batch, scripting, remaining formats (JP2, DICOM, DPX…), C2PA | An agent completes 10 scripted edit tasks via MCP |
| **M12** | Pro parity | CMYK/Lab UI, print, HDR display, photomerge/HDR merge, timeline, layer comps, artboards, symmetry, neural filters | `xtask parity` ≥ 90% of Photoshop menu checklist |


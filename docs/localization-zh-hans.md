# Simplified Chinese catalog

The `zh-hans` catalog targets the i18n interface proposed in
[PhotoCraft PR #169](https://github.com/storytold/photocraft/pull/169), commit
`c1fb8909c4b3b176eead12d66b04338a103cf63d`. It translates the English source keys
from that revision's catalog. The Chinese wording is original and uses ordinary
image-editing terminology; no proprietary translation resources were extracted
or copied. Contributions use the repository's MIT OR Apache-2.0 license.

## Draft status

This contribution contains catalog data and documentation for translation review.
The catalog is not loaded by current main, so it does not yet enable a Chinese UI.
The integration below has been validated locally against the referenced #169
revision. Language registration and its tests will follow the shared i18n
foundation adopted upstream; the translations can be retained if that design
changes.

## Files and integration

- `crates/ui-egui/src/i18n/zh-hans.tsv` contains the translations, independent of
  the lookup implementation. Its UTF-8 columns are `context<TAB>source<TAB>translation`.
- With #169's foundation, register `zh-hans` in `crates/ui-egui/src/i18n/mod.rs`,
  displayed as `简体中文`, with one plural form and the complete-catalog checks.
- Once registered, select **Preferences > Interface > Language > 简体中文**, or set
  `interface.language` to `zh-hans` through the existing `prefs.set` command.

Keep English source keys, contexts, command IDs, placeholders and escapes intact.
Retain the trailing `…` on commands that open a dialog. Missing translations use
the framework's English fallback. User-supplied names and document data are not
translated. Product names and technology names such as PhotoCraft, ArtCraft,
OpenType, RGB, CMYK and Lab retain their spelling.

The #169 locale resolver recognizes `zh`, `zh-CN`, `zh-SG` and `zh-Hans`
variants. Traditional Chinese locales (`zh-TW`, `zh-HK`, `zh-MO`, `zh-Hant`) do
not select this catalog. Automatic OS/browser detection remains the responsibility
of the i18n foundation; manual language selection works independently of it.

## Terminology

| English | Simplified Chinese |
| --- | --- |
| Layer / Layer Comp | 图层 / 图层复合 |
| Mask / Clipping Mask | 蒙版 / 剪贴蒙版 |
| Selection / Feather | 选区 / 羽化 |
| Blend Mode / Opacity | 混合模式 / 不透明度 |
| Adjustment Layer | 调整图层 |
| Smart Object / Smart Filter | 智能对象 / 智能滤镜 |
| Canvas / Artboard | 画布 / 画板 |
| Brush / Stroke | 画笔 / 描边 |
| Fill / Gradient | 填充 / 渐变 |
| Path / Rasterize | 路径 / 栅格化 |
| Preset / Swatch | 预设 / 色板 |
| Export / Preferences | 导出 / 首选项 |

## Validation and maintenance

Run `cargo test -p photocraft-ui-egui`, the touched-crate all-target clippy check,
`cargo xtask layers` and `cargo xtask wasm`. The shared catalog tests validate
duplicate keys, placeholders, ellipses, menu coverage, `tl!` literals and blend
modes. Chinese-specific tests cover locale selection, fallback, plural messages
and formatted labels. Render and inspect the menus, Preferences and representative
dialogs with the existing offscreen `snapshot` example.

Font delivery is separate from the translation data. This contribution adds no
font assets. The PR #169 revision predates the newer system CJK fallback changes;
test against the current upstream font implementation before publishing a
release, and verify Web glyph coverage separately.

Offscreen review on Windows confirmed that the PR #169 baseline's Japanese-first
font fallback leaves some Simplified Chinese glyphs missing. New Document and
Interface Preferences were also reviewed using the locally installed Microsoft
YaHei font in a temporary harness; their Chinese labels fit at 1280 × 800. That
font override is only a review aid and is not part of the production change.

If #169's implementation changes, retain the catalog's English source and
context keys, adapt the registration/loader, and rerun the catalog checks. If a
different resource format is adopted, convert these three columns mechanically
instead of retranslating the text. Review additions and changed source meanings
against the new English UI.

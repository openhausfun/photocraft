//! Lazy CJK fallback fonts for the UI.
//!
//! The bundled Inter / JetBrains Mono have no Japanese, Chinese or Korean glyphs, and the OS
//! fonts that do are large (Hiragino ~10 MB, Apple SD Gothic Neo 28 MB, PingFang 78 MB, Noto
//! Sans CJK ~20 MB). Instead of reading them at startup, an egui plugin scans each frame's text
//! for CJK characters no registered font covers, and registers the next system font for that
//! character's script (`ctx.add_font`, active from the next frame, which it requests). Fonts are
//! appended at the lowest priority to every family, so Latin text keeps Inter.
//!
//! Script order follows the UI locale ([`photocraft_text::cjk::script_order`]): Kana prefers a
//! Japanese font, Hangul a Korean one, Bopomofo a Traditional Chinese one, and Han the
//! locale's script (Japanese forms only for a Japanese locale). At most one font is read per
//! frame, each file at most once, and once every script has been tried the scan stops.
//!
//! Builds made with the optional craft-fonts input (`CRAFT_FONTS_DIR`,
//! [`photocraft_text::craft_fonts`]) carry Japanese fonts (BIZ UDPGothic first): they are tried
//! before the system Japanese fonts, in the Japanese slot of the same locale order. Without
//! craft-fonts (and on the web, which never embeds them) nothing changes.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily, FontId, Shape};
use photocraft_text::cjk::{self, CjkChar, CjkScript, FontFile};
use photocraft_text::craft_fonts::{self, CraftFont};
use std::path::PathBuf;

/// Skip absurdly large files (a corrupt or non-font path must not eat memory).
pub const MAX_FONT_BYTES: u64 = 128 << 20;
/// Name prefix of the registered fallback fonts.
pub const FONT_PREFIX: &str = "system-cjk";

/// Where fonts come from; swapped out in tests.
#[derive(Clone)]
pub struct Sources {
    pub locale: fn() -> Option<String>,
    pub files: fn(CjkScript) -> Vec<FontFile>,
    pub last_resort: fn() -> Vec<FontFile>,
    /// Embedded fonts tried before `files` for a script (craft-fonts' Japanese fonts).
    pub embedded: fn(CjkScript) -> Vec<&'static CraftFont>,
}

/// craft-fonts' Japanese fonts for the Japanese script, UI face first (empty without craft-fonts).
pub fn craft_embedded(script: CjkScript) -> Vec<&'static CraftFont> {
    if script == CjkScript::Japanese { craft_fonts::japanese_for_ui() } else { Vec::new() }
}

/// No embedded fonts (tests that pin the system-font behaviour).
pub fn no_embedded(_: CjkScript) -> Vec<&'static CraftFont> {
    Vec::new()
}

impl Sources {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn system() -> Self {
        Self { locale: || cjk::ui_locale().map(str::to_string), files: cjk::font_files, last_resort: cjk::last_resort_files, embedded: craft_embedded }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn system() -> Self {
        Self { locale: || None, files: |_| Vec::new(), last_resort: Vec::new, embedded: craft_embedded }
    }
}

/// The lazy loader's state: which scripts were tried and which files were read.
pub struct CjkFallback {
    sources: Sources,
    order: Option<[CjkScript; 4]>,
    tried: Vec<CjkScript>,
    last_resort_tried: bool,
    loaded: Vec<PathBuf>,
    /// Fonts registered so far: (name, path, face index).
    pub registered: Vec<(String, PathBuf, u32)>,
}

impl CjkFallback {
    pub fn new(sources: Sources) -> Self {
        Self { sources, order: None, tried: Vec::new(), last_resort_tried: false, loaded: Vec::new(), registered: Vec::new() }
    }

    /// Script order for the UI locale (resolved on first use, not at startup).
    pub fn order(&mut self) -> [CjkScript; 4] {
        *self.order.get_or_insert_with(|| cjk::script_order((self.sources.locale)().as_deref()))
    }

    /// Every script and the last-resort fonts have been tried: nothing more to load.
    pub fn exhausted(&self) -> bool {
        self.tried.len() >= 4 && self.last_resort_tried
    }

    /// The next font to register for a character of kind `kind` that no current font covers:
    /// the first readable file of the first untried script (the character's preferred script,
    /// then the locale order), then the last-resort fonts. `None` when nothing is left.
    pub fn next_font(&mut self, kind: CjkChar) -> Option<(String, FontData)> {
        let order = self.order();
        let scripts: Vec<CjkScript> = std::iter::once(kind.preferred(&order)).chain(order).collect();
        for s in scripts {
            if self.tried.contains(&s) {
                continue;
            }
            self.tried.push(s);
            if let Some(f) = self.load_embedded(s) {
                return Some(f);
            }
            let files = (self.sources.files)(s);
            if let Some(f) = self.load_first(&files) {
                return Some(f);
            }
        }
        if !self.last_resort_tried {
            self.last_resort_tried = true;
            let files = (self.sources.last_resort)();
            return self.load_first(&files);
        }
        None
    }

    /// The first embedded (craft-fonts) font for `s`, registered from its static bytes.
    fn load_embedded(&mut self, s: CjkScript) -> Option<(String, FontData)> {
        let f = (self.sources.embedded)(s).into_iter().find(|f| !f.bytes.is_empty())?;
        let path = PathBuf::from(format!("craft-fonts/{} {}", f.family, f.style));
        if self.loaded.contains(&path) {
            return None;
        }
        self.loaded.push(path.clone());
        let name = format!("{FONT_PREFIX}-{}", self.registered.len());
        log::info!("UI font fallback: registered embedded {} as {name}", path.display());
        self.registered.push((name.clone(), path, 0));
        Some((name, FontData::from_static(f.bytes)))
    }

    fn load_first(&mut self, files: &[FontFile]) -> Option<(String, FontData)> {
        for f in files {
            if self.loaded.contains(&f.path) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&f.path) else { continue };
            if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FONT_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f.path) else { continue };
            let index = cjk::face_index_for_family(&bytes, f.family);
            self.loaded.push(f.path.clone());
            let name = format!("{FONT_PREFIX}-{}", self.registered.len());
            log::info!("UI font fallback: registered {} (face {index}) as {name}", f.path.display());
            self.registered.push((name.clone(), f.path.clone(), index));
            let mut data = FontData::from_owned(bytes);
            data.index = index;
            return Some((name, data));
        }
        None
    }
}

/// First CJK character in the frame's text that the UI fonts can't draw.
fn missing_char(ctx: &egui::Context, shapes: &[egui::epaint::ClippedShape]) -> Option<char> {
    fn collect(shape: &Shape, out: &mut Vec<char>) {
        match shape {
            Shape::Text(t) => {
                let text = &t.galley.job.text;
                if !text.is_ascii() {
                    for c in text.chars().filter(|c| cjk::classify(*c).is_some()) {
                        if out.len() >= 256 {
                            return;
                        }
                        if !out.contains(&c) {
                            out.push(c);
                        }
                    }
                }
            }
            Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut chars = Vec::new();
    for s in shapes {
        collect(&s.shape, &mut chars);
    }
    if chars.is_empty() {
        return None;
    }
    let font = FontId::proportional(12.0);
    ctx.fonts_mut(|f| chars.into_iter().find(|c| !f.has_glyph(&font, *c)))
}

/// (ascent, descent, line gap) of a face in em, from its `hhea` table; `descent` is negative.
fn vertical_metrics(font: &[u8], index: u32) -> Option<(f32, f32, f32)> {
    let u16_at = |o: usize| font.get(o..o.checked_add(2)?).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |o: usize| font.get(o..o.checked_add(4)?).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let i16_at = |o: usize| u16_at(o).map(|v| v as i16 as f32);
    let dir = if font.get(..4)? == b"ttcf" {
        // Collection: the offset table of face `index` follows a 12-byte header.
        u32_at(12usize.checked_add((index as usize).checked_mul(4)?)?)? as usize
    } else {
        0
    };
    let tables = u16_at(dir.checked_add(4)?)? as usize;
    let (mut hhea, mut head) = (None, None);
    for i in 0..tables {
        let rec = dir.checked_add(12)?.checked_add(i.checked_mul(16)?)?;
        let tag = font.get(rec..rec.checked_add(4)?)?;
        let off = u32_at(rec.checked_add(8)?)? as usize;
        match tag {
            b"hhea" => hhea = Some(off),
            b"head" => head = Some(off),
            _ => {}
        }
    }
    let (hhea, head) = (hhea?, head?);
    let upm = u16_at(head.checked_add(18)?)? as f32;
    if upm <= 0.0 {
        return None;
    }
    Some((i16_at(hhea.checked_add(4)?)? / upm, i16_at(hhea.checked_add(6)?)? / upm, i16_at(hhea.checked_add(8)?)? / upm))
}

/// `y_offset_factor` that puts `face`'s baseline where `primary`'s would be. egui centres a
/// fallback face's row in the primary font's row, so a face with a big line gap (Hiragino: 0.5em)
/// otherwise rides about 0.23em high next to Latin text.
fn baseline_offset(face: (f32, f32, f32), primary: (f32, f32, f32)) -> f32 {
    let height = |m: (f32, f32, f32)| m.0 - m.1 + m.2;
    let off = primary.0 - face.0 - 0.5 * (height(primary) - height(face));
    if off.is_finite() { off.clamp(-0.5, 0.5) } else { 0.0 }
}

/// Registers `name` at the lowest priority in every font family.
fn add_to_all_families(ctx: &egui::Context, name: String, mut data: FontData) {
    // Align to Inter, the primary of every UI family; JetBrains Mono differs by under 0.005em.
    let primary = ctx.fonts(|f| f.definitions().font_data.get("Inter").and_then(|p| vertical_metrics(&p.font, p.index)));
    if let (Some(face), Some(primary)) = (vertical_metrics(&data.font, data.index), primary) {
        data.tweak.y_offset_factor = baseline_offset(face, primary);
    }
    let mut families: Vec<FontFamily> = ctx.fonts(|f| f.definitions().families.keys().cloned().collect());
    for f in [FontFamily::Proportional, FontFamily::Monospace] {
        if !families.contains(&f) {
            families.push(f);
        }
    }
    let families = families.into_iter().map(|family| InsertFontFamily { family, priority: FontPriority::Lowest }).collect();
    ctx.add_font(FontInsert { name, data, families });
}

/// The egui plugin that watches drawn text and loads fallback fonts on demand.
pub struct CjkFontPlugin(pub CjkFallback);

impl egui::Plugin for CjkFontPlugin {
    fn debug_name(&self) -> &'static str {
        "photocraft-cjk-fonts"
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        if self.0.exhausted() {
            return;
        }
        let Some(c) = missing_char(ctx, &output.shapes) else { return };
        let Some(kind) = cjk::classify(c) else { return };
        if let Some((name, data)) = self.0.next_font(kind) {
            add_to_all_families(ctx, name, data);
            ctx.request_repaint();
        } else if !self.0.exhausted() {
            // Nothing readable for this script; try the next one on the next frame.
            ctx.request_repaint();
        }
    }
}

/// Installs the lazy CJK fallback (system fonts; nothing on the web).
pub fn install(ctx: &egui::Context) {
    install_with(ctx, Sources::system());
}

pub fn install_with(ctx: &egui::Context, sources: Sources) {
    ctx.add_plugin(CjkFontPlugin(CjkFallback::new(sources)));
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn fake_dir() -> PathBuf {
        let mut g = DIR.lock().unwrap();
        g.get_or_insert_with(|| {
            let dir = std::env::temp_dir().join(format!("photocraft-cjk-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("font.ttf"), include_bytes!("../../../assets/fonts/Inter-Regular.ttf")).unwrap();
            std::fs::write(dir.join("empty.ttf"), b"").unwrap();
            dir
        })
        .clone()
    }

    fn fake(name: &str) -> FontFile {
        FontFile { path: fake_dir().join(name), family: "" }
    }

    fn fake_sources(locale: fn() -> Option<String>) -> Sources {
        Sources {
            locale,
            // Korean has a readable "font", Japanese only broken files, Chinese none.
            files: |s| match s {
                CjkScript::Korean => vec![fake("missing.ttc"), fake("empty.ttf"), fake("font.ttf")],
                CjkScript::Japanese => vec![fake("empty.ttf")],
                _ => vec![],
            },
            last_resort: || vec![fake("font.ttf")],
            embedded: no_embedded,
        }
    }

    #[test]
    fn picks_preferred_script_skips_unreadable_and_exhausts() {
        let mut fb = CjkFallback::new(fake_sources(|| Some("en_US".into())));
        assert!(!fb.exhausted());
        // Hangul: Korean first; missing and empty files are skipped.
        let (name, data) = fb.next_font(CjkChar::Hangul).expect("korean font");
        assert!(name.starts_with(FONT_PREFIX));
        assert_eq!(data.index, 0);
        assert_eq!(fb.tried, vec![CjkScript::Korean]);
        // Han on an English locale: SC, TC, JA have nothing readable, and the last-resort file was
        // already loaded, so nothing new; everything is now tried.
        assert!(fb.next_font(CjkChar::Han).is_none());
        assert!(fb.exhausted());
        assert_eq!(fb.tried, vec![CjkScript::Korean, CjkScript::SimplifiedChinese, CjkScript::TraditionalChinese, CjkScript::Japanese]);
        assert_eq!(fb.registered.len(), 1, "a file is read at most once");
        assert!(fb.next_font(CjkChar::Kana).is_none());
    }

    #[test]
    fn han_follows_locale() {
        for (loc, first) in [
            ("ja_JP.UTF-8", CjkScript::Japanese),
            ("zh-Hans-CN", CjkScript::SimplifiedChinese),
            ("zh-Hant-TW", CjkScript::TraditionalChinese),
            ("ko_KR", CjkScript::Korean),
        ] {
            let mut fb = CjkFallback::new(Sources { locale: || None, files: |_| vec![], last_resort: Vec::new, embedded: no_embedded });
            fb.order = Some(cjk::script_order(Some(loc)));
            assert!(fb.next_font(CjkChar::Han).is_none());
            assert_eq!(fb.tried[0], first, "{loc}");
        }
    }

    /// Runs frames showing `text` until fonts settle; returns whether every glyph is covered.
    fn render(ctx: &egui::Context, text: &str) -> bool {
        for _ in 0..12 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                ui.label(text);
            });
            out.textures_delta.clear();
        }
        ctx.fonts_mut(|f| f.has_glyphs(&FontId::proportional(12.0), &text.replace(' ', "")))
    }

    #[test]
    fn vertical_metrics_parse_and_reject_garbage() {
        let (asc, desc, gap) = vertical_metrics(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
        assert!((asc - 1984.0 / 2048.0).abs() < 1e-4 && (desc + 494.0 / 2048.0).abs() < 1e-4 && gap == 0.0);
        for bad in [&b""[..], b"ttcf", b"ttcf\0\0\0\0\0\0\0\0\xff\xff\xff\xff", b"\0\x01\0\0\0\xff\0\0\0\0\0\0"] {
            assert!(vertical_metrics(bad, 0).is_none());
            assert!(vertical_metrics(bad, u32::MAX).is_none());
        }
    }

    #[test]
    fn a_face_with_a_big_line_gap_is_shifted_down_onto_the_primary_baseline() {
        let inter = (1984.0 / 2048.0, -494.0 / 2048.0, 0.0);
        let hiragino = (0.88, -0.12, 0.5);
        assert!((baseline_offset(hiragino, inter) - 0.2338).abs() < 1e-3);
        assert_eq!(baseline_offset(inter, inter), 0.0);
        assert_eq!(baseline_offset((f32::NAN, 0.0, 0.0), inter), 0.0);
        let jb = (1.02, -0.3, 0.0);
        assert!((baseline_offset(hiragino, jb) - baseline_offset(hiragino, inter)).abs() < 0.005, "one offset serves the monospace family too");
    }

    #[test]
    fn lazy_registration_triggers_only_for_cjk() {
        let ctx = egui::Context::default();
        install_with(&ctx, fake_sources(|| Some("ko".into())));
        let _ = render(&ctx, "Layer 1 – café");
        let n = ctx.fonts(|f| f.definitions().font_data.keys().filter(|k| k.starts_with(FONT_PREFIX)).count());
        assert_eq!(n, 0, "Latin text loads nothing");
        // Hangul isn't in the stand-in font, but the Korean candidate must have been registered.
        let _ = render(&ctx, "카드 배경");
        let fonts = ctx.fonts(|f| f.definitions().clone());
        let name = format!("{FONT_PREFIX}-0");
        assert!(fonts.font_data.contains_key(&name));
        for (fam, stack) in &fonts.families {
            assert_eq!(stack.last(), Some(&name), "{fam:?}: appended last so Latin keeps Inter");
        }
    }

    /// No system fonts at all: craft-fonts alone draws Japanese.
    fn craft_only(locale: fn() -> Option<String>) -> Sources {
        Sources { locale, files: |_| vec![], last_resort: Vec::new, embedded: craft_embedded }
    }

    #[test]
    fn craft_fonts_render_japanese_without_system_fonts() {
        let Some(ui_font) = craft_fonts::japanese_for_ui().first().copied() else {
            eprintln!("skipping: built without craft-fonts (set CRAFT_FONTS_DIR to a craft-fonts checkout)");
            return;
        };
        // An English locale tries Chinese first (none here), then Japanese: the craft font.
        for locale in [(|| Some("en_US".into())) as fn() -> Option<String>, || Some("ja_JP".into()), || None] {
            let ctx = egui::Context::default();
            crate::theme::install_fonts_with(&ctx, craft_only(locale));
            assert!(render(&ctx, "日本語の文字"), "tofu in 日本語の文字");
            assert!(render(&ctx, "レイヤー 1"), "tofu in kana");
            let fonts = ctx.fonts(|f| f.definitions().clone());
            let name = format!("{FONT_PREFIX}-0");
            assert!(fonts.font_data.get(&name).is_some_and(|d| std::ptr::eq(d.font.as_ref().as_ptr(), ui_font.bytes.as_ptr())), "BIZ UDPGothic Regular first");
            for (fam, stack) in &fonts.families {
                assert_eq!(stack.last(), Some(&name), "{fam:?}: appended last so Latin keeps Inter");
                assert!(stack.first().is_some_and(|f| f != &name), "{fam:?}");
            }
        }
    }

    #[test]
    fn ui_works_without_craft_fonts() {
        // The pre-craft-fonts behaviour: no embedded fonts, no system fonts. Latin renders,
        // Japanese is tried (and stays missing) without panicking, and the loader exhausts.
        let ctx = egui::Context::default();
        crate::theme::install_fonts_with(&ctx, Sources { locale: || None, files: |_| vec![], last_resort: Vec::new, embedded: no_embedded });
        assert!(render(&ctx, "Layer 1 – café"));
        assert!(!render(&ctx, "日本語の文字"));
        let n = ctx.fonts(|f| f.definitions().font_data.keys().filter(|k| k.starts_with(FONT_PREFIX)).count());
        assert_eq!(n, 0);
        if craft_fonts::CRAFT_FONTS.is_empty() {
            assert!(craft_embedded(CjkScript::Japanese).is_empty());
        }
    }

    /// With the real system fonts, every CJK sample renders (skipped per script when the
    /// machine has no font for it, e.g. CI Linux without Noto CJK).
    #[test]
    fn system_fonts_cover_cjk_samples() {
        let available = |s: CjkScript| cjk::font_files(s).iter().any(|f| f.path.is_file());
        let samples = [
            ("图层", CjkScript::SimplifiedChinese),
            ("圖層", CjkScript::TraditionalChinese),
            ("카드 배경", CjkScript::Korean),
            ("レイヤー", CjkScript::Japanese),
        ];
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        for (text, script) in samples {
            if !available(script) {
                eprintln!("skipping {text}: no {script:?} system font");
                continue;
            }
            assert!(render(&ctx, text), "{text} still has missing glyphs");
        }
        // All at once in a fresh context too (several scripts in one frame).
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let all: String = samples.iter().filter(|(_, s)| available(*s)).map(|(t, _)| *t).collect::<Vec<_>>().join(" ");
        assert!(render(&ctx, &all), "{all}");
    }
}

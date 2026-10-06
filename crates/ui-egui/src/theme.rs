//! Design system: themes, colour tokens, radii, typography.
//!
//! - **Studio** (default): near-black surfaces, rounded cards, Inter + JetBrains Mono, soft violet
//!   accent. Modelled on the look of modern pro editors (such as Photoshop 2025).
//! - **Studio Light**: the same system on light surfaces.
//! - **Classic**: a deliberately Windows-2000-era look (grey bevels, square corners, navy selection)
//!   for people who prefer it.
//!
//! Widgets read [`Tokens::get`] instead of hard-coding colours, so every theme applies everywhere.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// Photoshop-style Spectrum dark: flat charcoal panels, tab strips, blue accents (darkest brightness).
    Pro,
    /// Photoshop's default (second) interface brightness: #535353 panels, #282828 canvas (default).
    #[default]
    ProMedium,
    Studio,
    StudioLight,
    Classic,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 5] = [ThemeKind::Pro, ThemeKind::ProMedium, ThemeKind::Studio, ThemeKind::StudioLight, ThemeKind::Classic];
    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Pro => "Pro (Dark)",
            ThemeKind::ProMedium => "Pro (Medium Gray)",
            ThemeKind::Studio => "Studio (Dark)",
            ThemeKind::StudioLight => "Studio (Light)",
            ThemeKind::Classic => "Classic",
        }
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-', '(', ')'], "").as_str() {
            "pro" | "prodark" | "photoshop" | "dark" => Some(ThemeKind::Pro),
            "promedium" | "promediumgray" | "medium" | "mediumgray" => Some(ThemeKind::ProMedium),
            "studio" | "studiodark" => Some(ThemeKind::Studio),
            "studiolight" | "light" => Some(ThemeKind::StudioLight),
            "classic" | "win2000" | "retro" => Some(ThemeKind::Classic),
            _ => None,
        }
    }
}

/// Colour and shape tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Window chrome (title bar, toolbars).
    pub chrome: Color32,
    /// Canvas surround.
    pub canvas: Color32,
    /// Dot colour of the canvas grid pattern.
    pub canvas_dot: Color32,
    /// Dock background (behind cards).
    pub dock: Color32,
    /// Cards / panels.
    pub card: Color32,
    pub card_border: Color32,
    /// Inputs, fields, dropdowns.
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_border: Color32,
    pub accent_text: Color32,
    pub separator: Color32,
    pub shadow: Color32,
    pub primary_bg: Color32,
    pub primary_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
    /// Classic theme draws 3D bevels instead of flat fills.
    pub bevel: bool,
    /// Pro (Photoshop-grammar) layout: tab strips, flat panels, checkboxes, pill buttons.
    pub pro: bool,
    /// Panel tab-strip background (Pro).
    pub tab_strip: Color32,
    /// Selected list row (layers, history).
    pub row_selected: Color32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            // Sampled from Photoshop 2026's default brightness (raw display values).
            ThemeKind::ProMedium => Tokens {
                kind,
                chrome: Color32::from_rgb(83, 83, 83),
                dock: Color32::from_rgb(66, 66, 66),
                card: Color32::from_rgb(83, 83, 83),
                card_border: Color32::from_rgb(66, 66, 66),
                field: Color32::from_rgb(69, 69, 69),
                field_border: Color32::from_rgb(104, 104, 104),
                hover: Color32::from_rgb(98, 98, 98),
                pressed: Color32::from_rgb(112, 112, 112),
                text: Color32::from_rgb(238, 238, 238),
                text_dim: Color32::from_rgb(212, 212, 212),
                text_faint: Color32::from_rgb(160, 160, 160),
                icon: Color32::from_rgb(226, 226, 226),
                accent_soft: Color32::from_rgb(110, 110, 110),
                separator: Color32::from_rgb(62, 62, 62),
                tab_strip: Color32::from_rgb(74, 74, 74),
                row_selected: Color32::from_rgb(107, 107, 107),
                ..Tokens::for_kind(ThemeKind::Pro)
            },
            ThemeKind::Pro => Tokens {
                kind,
                chrome: Color32::from_rgb(50, 50, 50),
                canvas: Color32::from_rgb(40, 40, 40),
                canvas_dot: Color32::from_rgb(40, 40, 40),
                dock: Color32::from_rgb(30, 30, 30),
                card: Color32::from_rgb(50, 50, 50),
                card_border: Color32::from_rgb(30, 30, 30),
                field: Color32::from_rgb(36, 36, 36),
                field_border: Color32::from_rgb(74, 74, 74),
                hover: Color32::from_rgb(66, 66, 66),
                pressed: Color32::from_rgb(78, 78, 78),
                text: Color32::from_rgb(222, 222, 222),
                text_dim: Color32::from_rgb(178, 178, 178),
                text_faint: Color32::from_rgb(128, 128, 128),
                icon: Color32::from_rgb(200, 200, 200),
                accent: Color32::from_rgb(55, 142, 240),
                accent_soft: Color32::from_rgb(78, 78, 78),
                accent_border: Color32::from_rgb(55, 142, 240),
                accent_text: Color32::WHITE,
                separator: Color32::from_rgb(30, 30, 30),
                shadow: Color32::from_black_alpha(150),
                primary_bg: Color32::from_rgb(55, 142, 240),
                primary_text: Color32::WHITE,
                danger: Color32::from_rgb(236, 91, 98),
                warning: Color32::from_rgb(232, 176, 70),
                radius_sm: 3.0,
                radius: 4.0,
                radius_lg: 6.0,
                bevel: false,
                pro: true,
                tab_strip: Color32::from_rgb(38, 38, 38),
                row_selected: Color32::from_rgb(82, 82, 82),
            },
            ThemeKind::Studio => Tokens {
                kind,
                chrome: Color32::from_rgb(20, 20, 21),
                canvas: Color32::from_rgb(14, 14, 15),
                canvas_dot: Color32::from_rgb(46, 46, 50),
                dock: Color32::from_rgb(17, 17, 18),
                card: Color32::from_rgb(26, 26, 28),
                card_border: Color32::from_rgb(40, 40, 44),
                field: Color32::from_rgb(35, 35, 38),
                field_border: Color32::from_rgb(52, 52, 57),
                hover: Color32::from_rgb(44, 44, 48),
                pressed: Color32::from_rgb(56, 56, 62),
                text: Color32::from_rgb(236, 236, 240),
                text_dim: Color32::from_rgb(150, 150, 158),
                text_faint: Color32::from_rgb(96, 96, 104),
                icon: Color32::from_rgb(196, 196, 204),
                accent: Color32::from_rgb(139, 124, 246),
                accent_soft: Color32::from_rgba_unmultiplied(139, 124, 246, 46),
                accent_border: Color32::from_rgba_unmultiplied(160, 148, 255, 110),
                accent_text: Color32::from_rgb(214, 208, 255),
                separator: Color32::from_rgb(38, 38, 42),
                shadow: Color32::from_black_alpha(140),
                primary_bg: Color32::from_rgb(246, 246, 248),
                primary_text: Color32::from_rgb(12, 12, 14),
                danger: Color32::from_rgb(240, 96, 96),
                warning: Color32::from_rgb(240, 190, 90),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                bevel: false,
                pro: false,
                tab_strip: Color32::TRANSPARENT,
                row_selected: Color32::TRANSPARENT,
            },
            ThemeKind::StudioLight => Tokens {
                kind,
                chrome: Color32::from_rgb(246, 246, 248),
                canvas: Color32::from_rgb(226, 226, 230),
                canvas_dot: Color32::from_rgb(200, 200, 206),
                dock: Color32::from_rgb(240, 240, 243),
                card: Color32::from_rgb(252, 252, 253),
                card_border: Color32::from_rgb(222, 222, 228),
                field: Color32::from_rgb(242, 242, 245),
                field_border: Color32::from_rgb(214, 214, 220),
                hover: Color32::from_rgb(232, 232, 237),
                pressed: Color32::from_rgb(220, 220, 226),
                text: Color32::from_rgb(24, 24, 28),
                text_dim: Color32::from_rgb(96, 96, 106),
                text_faint: Color32::from_rgb(150, 150, 160),
                icon: Color32::from_rgb(60, 60, 68),
                accent: Color32::from_rgb(108, 92, 231),
                accent_soft: Color32::from_rgba_unmultiplied(108, 92, 231, 36),
                accent_border: Color32::from_rgba_unmultiplied(108, 92, 231, 120),
                accent_text: Color32::from_rgb(80, 64, 200),
                separator: Color32::from_rgb(226, 226, 232),
                shadow: Color32::from_black_alpha(50),
                primary_bg: Color32::from_rgb(20, 20, 24),
                primary_text: Color32::from_rgb(250, 250, 252),
                danger: Color32::from_rgb(210, 60, 60),
                warning: Color32::from_rgb(190, 130, 20),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                bevel: false,
                pro: false,
                tab_strip: Color32::TRANSPARENT,
                row_selected: Color32::TRANSPARENT,
            },
            ThemeKind::Classic => Tokens {
                kind,
                chrome: Color32::from_rgb(212, 208, 200),
                canvas: Color32::from_rgb(128, 128, 128),
                canvas_dot: Color32::from_rgb(128, 128, 128),
                dock: Color32::from_rgb(212, 208, 200),
                card: Color32::from_rgb(212, 208, 200),
                card_border: Color32::from_rgb(128, 128, 128),
                field: Color32::WHITE,
                field_border: Color32::from_rgb(128, 128, 128),
                hover: Color32::from_rgb(226, 222, 214),
                pressed: Color32::from_rgb(190, 186, 178),
                text: Color32::BLACK,
                text_dim: Color32::from_rgb(64, 64, 64),
                text_faint: Color32::from_rgb(128, 128, 128),
                icon: Color32::BLACK,
                accent: Color32::from_rgb(10, 36, 106),
                accent_soft: Color32::from_rgb(10, 36, 106),
                accent_border: Color32::from_rgb(10, 36, 106),
                accent_text: Color32::WHITE,
                separator: Color32::from_rgb(128, 128, 128),
                shadow: Color32::from_black_alpha(0),
                primary_bg: Color32::from_rgb(212, 208, 200),
                primary_text: Color32::BLACK,
                danger: Color32::from_rgb(160, 0, 0),
                warning: Color32::from_rgb(128, 96, 0),
                radius_sm: 0.0,
                radius: 0.0,
                radius_lg: 0.0,
                bevel: true,
                pro: false,
                tab_strip: Color32::from_rgb(212, 208, 200),
                row_selected: Color32::from_rgb(10, 36, 106),
            },
        }
    }

    /// Tokens for the active theme (stored in egui's context data by [`apply`]).
    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::new("photocraft-theme"))).unwrap_or_else(|| Tokens::for_kind(ThemeKind::Studio))
    }

    pub fn dark(&self) -> bool {
        matches!(self.kind, ThemeKind::Studio | ThemeKind::Pro | ThemeKind::ProMedium)
    }
}

/// Register Inter (UI) and JetBrains Mono (numbers) plus named weights.
pub fn install_fonts(ctx: &egui::Context) {
    install_fonts_with(ctx, crate::cjk_fonts::Sources::system());
}

/// [`install_fonts`] with the CJK fallback fonts taken from `cjk` (tests swap the sources).
pub fn install_fonts_with(ctx: &egui::Context, cjk: crate::cjk_fonts::Sources) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, "Inter", photocraft_text::fonts::INTER_REGULAR);
    add(&mut fonts, "Inter-Medium", photocraft_text::fonts::INTER_MEDIUM);
    add(&mut fonts, "Inter-SemiBold", photocraft_text::fonts::INTER_SEMIBOLD);
    add(&mut fonts, "JetBrainsMono", photocraft_text::fonts::JETBRAINS_MONO_REGULAR);
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".to_owned());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "JetBrainsMono".to_owned());
    // Named weights fall back to the default stack for missing glyphs.
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    for (fam, primary) in [("medium", "Inter-Medium"), ("semibold", "Inter-SemiBold")] {
        let mut stack = vec![primary.to_owned()];
        stack.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(fam.into()), stack);
    }
    ctx.set_fonts(fonts);
    // Japanese / Chinese / Korean fallback fonts (craft-fonts' Japanese ones if built in, then
    // the system's) are registered on demand (cjk_fonts.rs).
    crate::cjk_fonts::install_with(ctx, cjk);
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Apply a theme to egui's global style and publish its tokens.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let t = Tokens::for_kind(kind);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("photocraft-theme"), t));
    let mut v = if t.dark() { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.chrome;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.card_border);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.card;
    v.code_bg_color = t.field;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.danger;
    v.window_corner_radius = CornerRadius::same(t.radius_lg as u8);
    v.menu_corner_radius = CornerRadius::same(t.radius as u8);
    v.window_shadow = egui::Shadow { offset: [0, 10], blur: 32, spread: 0, color: t.shadow };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: t.shadow };
    v.selection.bg_fill = if t.bevel || t.pro { t.accent } else { t.accent_soft };
    v.selection.stroke = Stroke::new(1.0, t.accent_text);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    let r = CornerRadius::same(t.radius_sm as u8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_dim);
    w.noninteractive.corner_radius = r;
    for (wv, bg, stroke) in [
        (&mut w.inactive, t.field, t.field_border),
        (&mut w.hovered, t.hover, t.field_border),
        (&mut w.active, t.pressed, t.accent_border),
        (&mut w.open, t.hover, t.field_border),
    ] {
        wv.bg_fill = bg;
        wv.weak_bg_fill = bg;
        wv.bg_stroke = if t.bevel { Stroke::new(1.0, Color32::from_gray(64)) } else { Stroke::new(1.0, stroke) };
        wv.fg_stroke = Stroke::new(1.0, t.text);
        wv.corner_radius = r;
        wv.expansion = 0.0;
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.5)),
            (TextStyle::Body, FontId::proportional(if t.pro { 12.0 } else { 12.5 })),
            (TextStyle::Button, FontId::proportional(if t.pro { 12.0 } else { 12.5 })),
            (TextStyle::Heading, semibold(15.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size = egui::vec2(24.0, 24.0);
        s.spacing.slider_width = 150.0;
        s.spacing.combo_width = 120.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(16);
        s.spacing.icon_width = 14.0;
        s.visuals.indent_has_left_vline = false;
        s.interaction.tooltip_delay = TOOLTIP_DELAY;
        // Thin overlay scrollbars that appear on hover (Photoshop/macOS style).
        s.spacing.scroll = if t.bevel { egui::style::ScrollStyle::solid() } else { egui::style::ScrollStyle::thin() };
        s.spacing.tooltip_width = 280.0;
    });
}

/// Seconds the pointer rests on a control before its tooltip shows.
pub const TOOLTIP_DELAY: f32 = 0.35;

/// Vertical gap between stacked control rows in panels (Properties fields, the Layers panel's
/// Opacity and Fill rows); docks zero egui's item spacing, so rows add this themselves.
pub const ROW_GAP: f32 = 4.0;

pub fn canvas_bg(t: &Tokens) -> Color32 {
    t.canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_parse() {
        assert_eq!(ThemeKind::from_name("Classic"), Some(ThemeKind::Classic));
        assert_eq!(ThemeKind::from_name("studio (light)"), Some(ThemeKind::StudioLight));
        assert_eq!(ThemeKind::from_name("dark"), Some(ThemeKind::Pro));
        assert_eq!(ThemeKind::from_name("studio"), Some(ThemeKind::Studio));
        assert_eq!(ThemeKind::from_name("Pro (Medium Gray)"), Some(ThemeKind::ProMedium));
        let m = Tokens::for_kind(ThemeKind::ProMedium);
        assert!(m.pro && m.dark() && m.kind == ThemeKind::ProMedium && m.card == Color32::from_rgb(83, 83, 83));
        assert_eq!(ThemeKind::from_name("neon"), None);
    }

    #[test]
    fn studio_text_contrast_is_high() {
        let t = Tokens::for_kind(ThemeKind::Studio);
        let p = Tokens::for_kind(ThemeKind::Pro);
        assert!(p.pro && !t.pro);
        let lum = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        assert!(lum(t.text) - lum(t.card) > 180.0);
        assert!(lum(t.text_dim) - lum(t.card) > 90.0);
    }
}

/// Development feature: live design-token overrides.
///
/// Set `PHOTOCRAFT_THEME_FILE=/path/tokens.json` in a debug build; the file is polled and applied
/// on change, so colours, radii and sizes can be tuned without recompiling. Keys are `Tokens` field
/// names; values are `"#rrggbb"`, `"#rrggbbaa"` or numbers. Compiled out of release builds.
#[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
pub mod live {
    use super::Tokens;
    use egui::Color32;
    use std::time::SystemTime;

    #[derive(Default)]
    pub struct LiveTokens {
        path: Option<std::path::PathBuf>,
        stamp: Option<SystemTime>,
        last_check: f64,
    }

    impl LiveTokens {
        pub fn from_env() -> Self {
            Self { path: std::env::var_os("PHOTOCRAFT_THEME_FILE").map(Into::into), ..Default::default() }
        }

        /// Re-apply overrides if the file changed. Returns true when tokens were updated.
        pub fn poll(&mut self, ctx: &egui::Context, kind: super::ThemeKind) -> bool {
            let Some(path) = &self.path else { return false };
            let now = ctx.input(|i| i.time);
            if now - self.last_check < 0.4 {
                ctx.request_repaint_after(std::time::Duration::from_millis(400));
                return false;
            }
            self.last_check = now;
            ctx.request_repaint_after(std::time::Duration::from_millis(400));
            let Ok(meta) = std::fs::metadata(path) else { return false };
            let stamp = meta.modified().ok();
            if stamp == self.stamp {
                return false;
            }
            self.stamp = stamp;
            let Ok(text) = std::fs::read_to_string(path) else { return false };
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => {
                    super::apply(ctx, kind);
                    let mut t = Tokens::get(ctx);
                    let unknown = apply_overrides(&mut t, &v);
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("photocraft-theme"), t));
                    if !unknown.is_empty() {
                        log::warn!("unknown token keys: {unknown:?}");
                    }
                    true
                }
                Err(e) => {
                    log::warn!("theme file: {e}");
                    false
                }
            }
        }
    }

    fn color(v: &serde_json::Value) -> Option<Color32> {
        let s = v.as_str()?.trim_start_matches('#');
        let b = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        match s.len() {
            6 => Some(Color32::from_rgb(b(0)?, b(2)?, b(4)?)),
            8 => Some(Color32::from_rgba_unmultiplied(b(0)?, b(2)?, b(4)?, b(6)?)),
            _ => None,
        }
    }

    /// Apply JSON overrides onto tokens; returns unknown keys.
    pub fn apply_overrides(t: &mut Tokens, v: &serde_json::Value) -> Vec<String> {
        let mut unknown = Vec::new();
        let Some(obj) = v.as_object() else { return unknown };
        for (k, val) in obj {
            macro_rules! c {
                ($($f:ident),*) => {
                    match k.as_str() {
                        $(stringify!($f) => { if let Some(c) = color(val) { t.$f = c; } })*
                        "radius_sm" => { if let Some(x) = val.as_f64() { t.radius_sm = x as f32; } }
                        "radius" => { if let Some(x) = val.as_f64() { t.radius = x as f32; } }
                        "radius_lg" => { if let Some(x) = val.as_f64() { t.radius_lg = x as f32; } }
                        _ => unknown.push(k.clone()),
                    }
                };
            }
            c!(
                chrome,
                canvas,
                canvas_dot,
                dock,
                card,
                card_border,
                field,
                field_border,
                hover,
                pressed,
                text,
                text_dim,
                text_faint,
                icon,
                accent,
                accent_soft,
                accent_border,
                accent_text,
                separator,
                shadow,
                primary_bg,
                primary_text,
                danger,
                warning,
                tab_strip,
                row_selected
            );
        }
        unknown
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn overrides_apply_and_report_unknown() {
            let mut t = Tokens::for_kind(super::super::ThemeKind::Pro);
            let unknown = apply_overrides(&mut t, &serde_json::json!({"card": "#102030", "radius": 9, "nope": 1}));
            assert_eq!(t.card, Color32::from_rgb(16, 32, 48));
            assert_eq!(t.radius, 9.0);
            assert_eq!(unknown, vec!["nope".to_string()]);
        }
    }
}

//! UI localisation. Strings in code stay English and are the default lookup keys; a per-language
//! catalog (`*.tsv`, see `ja.tsv` for the format) maps them to display text at render time. Command
//! ids, menu paths used for logic, the control channel, the CLI and MCP never see translated text,
//! so agents and scripts are unaffected. A string without a translation is shown in English.
//!
//! # Adding a language
//! 1. Add `xx.tsv` next to `ja.tsv` (copy its header; translate from the *meaning* of the English
//!    text, clean-room, see `ja.tsv`).
//! 2. Add one row to [`LANGUAGES`] (code, native name, catalog, plural rule).
//!
//! That is all: the Preferences dropdown, the system-locale match and the catalog tests (parse,
//! placeholders, plural forms) pick it up from the registry.
//!
//! # Looking strings up
//! - [`tr`]: a plain string. [`tr_ctx`]: when one English word needs different translations.
//! - [`tr_id`]: a command-id keyed string with the English label as fallback (menu items), so a
//!   translation survives rewording of the English text and can differ per command.
//! - [`trn`]: plural-aware (`{n}` is filled in). [`fmt`]: fill `{name}` placeholders after [`tr`];
//!   translators may reorder placeholders freely.

mod catalog;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use catalog::Catalog;

/// One supported UI language.
pub struct LangInfo {
    /// BCP 47 code, lowercase (`ja`, `zh-hans`, `pt-br`). Also the `interface.language` value.
    pub code: &'static str,
    /// The language's name in itself, shown in the Preferences dropdown.
    pub name: &'static str,
    /// Catalog file contents (empty for the built-in English).
    pub source: &'static str,
    /// Plural form index for a count (English: 0 = one, 1 = other; Japanese and Chinese: always 0;
    /// Czech: 0 = one, 1 = few (2–4), 2 = other). A catalog's `@plural` entries list one form per
    /// index.
    pub plural: fn(u64) -> usize,
    /// Must the catalog cover every menu string? (checked by the tests)
    pub complete_menus: bool,
    catalog: OnceLock<Catalog>,
}

fn plural_one_other(n: u64) -> usize {
    usize::from(n != 1)
}

fn plural_none(_: u64) -> usize {
    0
}

fn plural_russian(n: u64) -> usize {
    match (n % 10, n % 100) {
        (1, 11..=19) => 2,
        (1, _) => 0,
        (2..=4, 11..=19) => 2,
        (2..=4, _) => 1,
        _ => 2,
    }
}

/// Czech: 1 → one, 2–4 → few, everything else (0, 5+) → other.
fn plural_cs(n: u64) -> usize {
    match n {
        1 => 0,
        2..=4 => 1,
        _ => 2,
    }
}

/// The registry. English first: it is the fallback and the source language.
pub static LANGUAGES: [LangInfo; 7] = [
    LangInfo { code: "en", name: "English", source: "", plural: plural_one_other, complete_menus: false, catalog: OnceLock::new() },
    LangInfo { code: "ja", name: "日本語", source: include_str!("ja.tsv"), plural: plural_none, complete_menus: true, catalog: OnceLock::new() },
    LangInfo {
        code: "zh-hans", name: "简体中文", source: include_str!("zh-hans.tsv"), plural: plural_none, complete_menus: false, catalog: OnceLock::new()
    },
    // Traditional Chinese in the vocabulary used in Taiwan; `zh-TW`, `zh-HK`, `zh-MO` and `zh-Hant-*`
    // locales all resolve here (see `candidates`).
    LangInfo {
        code: "zh-hant", name: "繁體中文", source: include_str!("zh-hant.tsv"), plural: plural_none, complete_menus: true, catalog: OnceLock::new()
    },
    LangInfo { code: "es", name: "Español", source: include_str!("es.tsv"), plural: plural_one_other, complete_menus: true, catalog: OnceLock::new() },
    LangInfo { code: "ru", name: "Русский", source: include_str!("ru.tsv"), plural: plural_russian, complete_menus: true, catalog: OnceLock::new() },
    LangInfo { code: "cs", name: "Čeština", source: include_str!("cs.tsv"), plural: plural_cs, complete_menus: true, catalog: OnceLock::new() },
];

impl LangInfo {
    fn catalog(&self) -> &Catalog {
        self.catalog.get_or_init(|| Catalog::parse(self.source))
    }
}

/// A language the UI can be shown in (a handle into [`LANGUAGES`]).
#[derive(Clone, Copy)]
pub struct Lang(&'static LangInfo);

impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lang({})", self.0.code)
    }
}

impl PartialEq for Lang {
    fn eq(&self, other: &Self) -> bool {
        self.0.code == other.0.code
    }
}

impl Eq for Lang {}

impl Lang {
    pub const EN: Lang = Lang(&LANGUAGES[0]);

    pub fn code(self) -> &'static str {
        self.0.code
    }

    /// A language by its exact code.
    pub fn from_code(code: &str) -> Option<Lang> {
        LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(code)).map(Lang)
    }

    /// Resolve the `interface.language` preference: a language code, or `auto` (and anything
    /// unknown, e.g. a code from a newer version) to follow the system locale.
    pub fn from_pref(pref: &str) -> Lang {
        Lang::from_code(pref).unwrap_or_else(system_lang)
    }

    /// Every registered language.
    pub fn all() -> impl Iterator<Item = Lang> {
        LANGUAGES.iter().map(Lang)
    }

    pub fn name(self) -> &'static str {
        self.0.name
    }

    /// Does this language's catalog claim to cover every menu string and `tl!` literal?
    pub fn complete_menus(self) -> bool {
        self.0.complete_menus
    }

    fn catalog(self) -> &'static Catalog {
        self.0.catalog()
    }
}

/// Candidate language codes for a locale tag, most specific first: `zh_TW.UTF-8` →
/// `zh-tw`, `zh-hant`, `zh`.
fn candidates(tag: &str) -> Vec<String> {
    let base = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
    let parts: Vec<&str> = base.split('-').filter(|p| !p.is_empty()).collect();
    let Some(&primary) = parts.first() else { return Vec::new() };
    let mut out = Vec::new();
    for n in (1..=parts.len()).rev() {
        out.push(parts[..n].join("-"));
    }
    if primary == "zh" && !parts.iter().any(|p| matches!(*p, "hans" | "hant")) {
        // Chinese by region when no script is given.
        let script = if parts.iter().any(|p| matches!(*p, "tw" | "hk" | "mo")) { "zh-hant" } else { "zh-hans" };
        out.insert(out.len() - 1, script.to_string());
    }
    out
}

/// The registered language for a locale tag such as `ja_JP.UTF-8`, `ja-JP`, `zh-TW`; `None` if
/// the language isn't supported. `C`/`POSIX` mean English.
pub fn lang_from_tag(tag: &str) -> Option<Lang> {
    let cands = candidates(tag);
    if matches!(cands.first().map(String::as_str), Some("c" | "posix")) {
        return Some(Lang::EN);
    }
    cands.iter().find_map(|c| Lang::from_code(c))
}

/// The system language (cached). English when it can't be determined.
pub fn system_lang() -> Lang {
    // Tests drive the UI by its English labels whatever the developer's locale is.
    if cfg!(test) {
        return Lang::EN;
    }
    static SYSTEM: OnceLock<Lang> = OnceLock::new();
    *SYSTEM.get_or_init(detect_system_lang)
}

#[cfg(not(target_arch = "wasm32"))]
fn detect_system_lang() -> Lang {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(l) = std::env::var(var).ok().filter(|v| !v.is_empty()).and_then(|v| lang_from_tag(&v)) {
            return l;
        }
    }
    // Apps started from the Finder don't inherit LANG: use the macOS preferred-languages list.
    // The absolute path keeps a `defaults` earlier on PATH from running; any failure means English.
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output()
        && out.status.success()
        && let Some(l) = first_supported(&String::from_utf8_lossy(&out.stdout))
    {
        return l;
    }
    // Windows sets no LANG: fall back to the OS locale the text engine already reads for its CJK
    // font order (`HKCU\Control Panel\International` › `LocaleName`, e.g. `zh-TW`; on macOS the
    // preferences plist). `PHOTOCRAFT_LOCALE` overrides it there too.
    if let Some(l) = photocraft_text::cjk::ui_locale().and_then(lang_from_tag) {
        return l;
    }
    Lang::EN
}

/// The first supported language in a `defaults read` list like `(\n    "ja-JP",\n    "en-US"\n)`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn first_supported(list: &str) -> Option<Lang> {
    list.split(['(', ')', ',', '"', '\n']).map(str::trim).filter(|s| !s.is_empty()).find_map(lang_from_tag)
}

#[cfg(target_arch = "wasm32")]
fn detect_system_lang() -> Lang {
    Lang::EN
}

/// Index into [`LANGUAGES`] of the language the UI is drawn in this frame.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Set the UI language for drawing (the shell calls this once per frame from the preference), so
/// widgets can translate without every call site carrying a language around.
pub fn set_current(lang: Lang) {
    let i = LANGUAGES.iter().position(|l| l.code == lang.code()).unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
}

/// The language the UI is drawn in.
pub fn current() -> Lang {
    Lang(LANGUAGES.get(CURRENT.load(Ordering::Relaxed)).unwrap_or(&LANGUAGES[0]))
}

/// Does `lang` have a catalog entry for this plain string? (English never does: it is the source.)
pub fn has(lang: Lang, s: &str) -> bool {
    lang.catalog().plain(s).is_some()
}

/// Translate an English UI string into the current language ([`tr`] with [`current`]).
pub fn t(s: &str) -> &str {
    tr(current(), s)
}

/// Translate an English UI string; unknown strings come back unchanged.
pub fn tr(lang: Lang, s: &str) -> &str {
    lang.catalog().plain(s).unwrap_or(s)
}

/// Like [`tr`], for an English string that needs a disambiguating `context`.
pub fn tr_ctx<'a>(lang: Lang, context: &str, s: &'a str) -> &'a str {
    lang.catalog().contextual(context, s).unwrap_or_else(|| tr(lang, s))
}

/// A string keyed by its command id, falling back to the translation of the English `label`.
pub fn tr_id<'a>(lang: Lang, id: &str, label: &'a str) -> &'a str {
    lang.catalog().id(id).unwrap_or_else(|| tr(lang, label))
}

/// Fill `{name}` placeholders. Unknown placeholders are left as written.
pub fn fmt(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// A plural-aware message: `one`/`other` are the English forms (with `{n}` where the count goes).
pub fn trn(lang: Lang, n: u64, one: &str, other: &str) -> String {
    let idx = (lang.0.plural)(n);
    let text = lang.catalog().plural(one, other, idx).unwrap_or(if n == 1 { one } else { other });
    fmt(text, &[("n", &n.to_string())])
}

#[cfg(test)]
mod tests {
    use super::catalog::{parse_entries, placeholders};
    use super::*;

    const JA: fn() -> Lang = || Lang::from_code("ja").expect("ja registered");
    const ZH: fn() -> Lang = || Lang::from_code("zh-hant").expect("zh-hant registered");
    const CS: fn() -> Lang = || Lang::from_code("cs").expect("cs registered");

    #[test]
    fn tags_map_to_languages() {
        assert_eq!(lang_from_tag("ja_JP.UTF-8"), Some(JA()));
        assert_eq!(lang_from_tag("ja-JP"), Some(JA()));
        assert_eq!(lang_from_tag("en_US.UTF-8"), Some(Lang::EN));
        assert_eq!(lang_from_tag("C"), Some(Lang::EN));
        assert_eq!(lang_from_tag("POSIX"), Some(Lang::EN));
        assert_eq!(lang_from_tag("cs_CZ.UTF-8"), Some(CS()));
        assert_eq!(lang_from_tag("cs-CZ"), Some(CS()));
        assert_eq!(lang_from_tag("fr_FR"), None);
        // Traditional Chinese: by region, by script, and with a region after the script.
        assert_eq!(lang_from_tag("zh_TW.UTF-8"), Some(ZH()));
        assert_eq!(lang_from_tag("zh-TW"), Some(ZH()));
        assert_eq!(lang_from_tag("zh-HK"), Some(ZH()));
        assert_eq!(lang_from_tag("zh_MO"), Some(ZH()));
        assert_eq!(lang_from_tag("zh-Hant"), Some(ZH()));
        assert_eq!(lang_from_tag("zh-Hant-TW"), Some(ZH()));
        assert_eq!(lang_from_tag("zh-Hant-HK"), Some(ZH()));
        // Simplified Chinese locales never pick up the Traditional catalog: they resolve to `zh-hans`.
        for tag in ["zh-CN", "zh_CN.UTF-8", "zh_SG", "zh-Hans", "zh-Hans-CN", "zh"] {
            assert_ne!(lang_from_tag(tag), Some(ZH()), "{tag}");
            assert_eq!(lang_from_tag(tag), Lang::from_code("zh-hans"), "{tag}");
        }
        assert_eq!(lang_from_tag(""), None);
        assert_eq!(lang_from_tag("_"), None);
    }

    #[test]
    fn candidates_walk_from_specific_to_general() {
        assert_eq!(candidates("pt_BR.UTF-8"), ["pt-br", "pt"]);
        assert_eq!(candidates("zh_TW"), ["zh-tw", "zh-hant", "zh"]);
        assert_eq!(candidates("zh-CN"), ["zh-cn", "zh-hans", "zh"]);
        assert_eq!(candidates("zh-Hant-HK"), ["zh-hant-hk", "zh-hant", "zh"]);
    }

    #[test]
    fn macos_language_list_is_parsed() {
        assert_eq!(first_supported("(\n    \"ja-JP\",\n    \"en-US\"\n)\n"), Some(JA()));
        assert_eq!(first_supported("(\n    \"fr-FR\",\n    \"en-US\"\n)\n"), Some(Lang::EN));
        assert_eq!(first_supported("(\n    \"zh-Hant-TW\",\n    \"en-US\"\n)\n"), Some(ZH()));
        assert_eq!(first_supported("("), None);
    }

    #[test]
    fn preferences_resolve_with_fallback() {
        assert_eq!(Lang::from_pref("ja"), JA());
        assert_eq!(Lang::from_pref("JA"), JA());
        assert_eq!(Lang::from_pref("zh-hant"), ZH());
        assert_eq!(Lang::from_pref("ZH-Hant"), ZH());
        assert_eq!(Lang::from_pref("en"), Lang::EN);
        // `auto` and unknown codes follow the system (English under test).
        assert_eq!(Lang::from_pref("auto"), Lang::EN);
        assert_eq!(Lang::from_pref("xx-unknown"), Lang::EN);
    }

    #[test]
    fn spanish_resolves_and_pluralises() {
        let es = Lang::from_code("es").expect("es registered");
        for tag in ["es", "es_ES.UTF-8", "es-MX", "es-419"] {
            assert_eq!(lang_from_tag(tag), Some(es), "{tag}");
        }
        assert_eq!(tr(es, "Layer"), "Capa");
        assert_eq!(trn(es, 1, "{n} item", "{n} items"), "1 elemento");
        assert_eq!(trn(es, 3, "{n} item", "{n} items"), "3 elementos");
    }

    #[test]
    fn lookups_fall_back_to_english() {
        assert_eq!(tr(JA(), "no such label"), "no such label");
        assert_eq!(tr(Lang::EN, "Layer"), "Layer");
        assert_eq!(tr(JA(), "Layer"), "レイヤー");
        assert_eq!(tr(ZH(), "Layer"), "圖層");
        assert_eq!(tr(ZH(), "no such label"), "no such label");
        assert_eq!(tr_id(ZH(), "no.such.id", "Layer"), "圖層");
        assert_eq!(tr_id(JA(), "no.such.id", "Layer"), "レイヤー");
        assert_eq!(tr_ctx(JA(), "no such context", "Layer"), "レイヤー");
    }

    #[test]
    fn russian_plural_rules() {
        let ru = || Lang::from_code("ru").expect("ru registered");
        assert_eq!(trn(ru(), 1, "{n} item", "{n} items"), "1 элемент");
        assert_eq!(trn(ru(), 2, "{n} item", "{n} items"), "2 элемента");
        assert_eq!(trn(ru(), 5, "{n} item", "{n} items"), "5 элементов");
        assert_eq!(trn(ru(), 11, "{n} item", "{n} items"), "11 элементов");
        assert_eq!(trn(ru(), 21, "{n} item", "{n} items"), "21 элемент");
        assert_eq!(trn(ru(), 22, "{n} item", "{n} items"), "22 элемента");
        assert_eq!(trn(ru(), 101, "{n} item", "{n} items"), "101 элемент");
        assert_eq!(trn(ru(), 111, "{n} item", "{n} items"), "111 элементов");
    }

    #[test]
    fn catalog_kinds_are_parsed_and_looked_up() {
        let c = Catalog::parse("# c\n\tHello\tこんにちは\n@id\tfile.save\t保存する\nmenu\tWindows\tウィンドウ群\n@plural\t{n} file|{n} files\t{n} 個\n\n");
        assert_eq!(c.plain("Hello"), Some("こんにちは"));
        assert_eq!(c.id("file.save"), Some("保存する"));
        assert_eq!(c.contextual("menu", "Windows"), Some("ウィンドウ群"));
        assert_eq!(c.contextual("other", "Windows"), None);
        assert_eq!(c.plural("{n} file", "{n} files", 0), Some("{n} 個"));
        assert_eq!(c.plural("{n} file", "{n} files", 5), Some("{n} 個"), "an index past the forms clamps");
    }

    #[test]
    fn malformed_lines_are_reported_not_fatal() {
        let (entries, errors) = parse_entries("\tok\tはい\nno tabs here\n\tonly\n\ta\tb\tc\textra\n\t\tempty source\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert_eq!(parse_entries("\ta\\tb\tx\\ny\\\\z\n").0[0], (String::new(), "a\tb".into(), "x\ny\\z".into()));
    }

    #[test]
    fn plurals_and_placeholders() {
        assert_eq!(trn(Lang::EN, 1, "{n} item", "{n} items"), "1 item");
        assert_eq!(trn(Lang::EN, 0, "{n} item", "{n} items"), "0 items");
        assert_eq!(trn(Lang::EN, 7, "{n} item", "{n} items"), "7 items");
        assert_eq!(trn(JA(), 1, "{n} item", "{n} items"), "1 件");
        assert_eq!(trn(JA(), 7, "{n} item", "{n} items"), "7 件");
        assert_eq!(trn(ZH(), 1, "{n} item", "{n} items"), "1 個項目");
        assert_eq!(trn(ZH(), 7, "{n} item", "{n} items"), "7 個項目");
        assert_eq!(trn(CS(), 1, "{n} item", "{n} items"), "1 položka");
        assert_eq!(trn(CS(), 3, "{n} item", "{n} items"), "3 položky");
        assert_eq!(trn(CS(), 5, "{n} item", "{n} items"), "5 položek");
        assert_eq!(trn(CS(), 0, "{n} item", "{n} items"), "0 položek");
        assert_eq!(fmt("{b} before {a}", &[("a", "x"), ("b", "y"), ("c", "z")]), "y before x");
        assert_eq!(fmt("{missing}", &[]), "{missing}");
        assert_eq!(placeholders("a {x} b {y} {"), ["x", "y"]);
    }

    #[test]
    fn czech_plural_rule() {
        let forms: Vec<usize> = [0, 1, 2, 3, 4, 5, 11, 12, 21, 22, 100, u64::MAX].into_iter().map(plural_cs).collect();
        assert_eq!(forms, [2, 0, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2]);
        assert_eq!(tr(CS(), "Layer"), "Vrstva");
        assert_eq!(tr_id(CS(), "select.all", "All"), "Vybrat vše", "an id override wins over the plain label");
        assert_eq!(tr(CS(), "All"), "Vše");
    }

    /// Every bundled catalog is well-formed and consistent with its sources.
    #[test]
    fn bundled_catalogs_are_consistent() {
        for l in &LANGUAGES {
            assert!(l.code == l.code.to_ascii_lowercase() && !l.name.is_empty(), "{}", l.code);
            let (entries, errors) = parse_entries(l.source);
            assert!(errors.is_empty(), "{}: {errors:?}", l.code);
            let mut seen = std::collections::HashSet::new();
            for (ctx, src, tr) in &entries {
                assert!(seen.insert((ctx.clone(), src.clone())), "{}: duplicate {ctx:?} {src:?}", l.code);
                if ctx == "@plural" {
                    let one_other: Vec<&str> = src.split('|').collect();
                    assert_eq!(one_other.len(), 2, "{}: plural source must be `one|other`: {src:?}", l.code);
                    let forms = (0..=1000).map(l.plural).max().unwrap_or(0) + 1;
                    assert_eq!(tr.split('|').count(), forms, "{}: {forms} plural forms expected in {src:?}", l.code);
                    for form in tr.split('|') {
                        let mut want = placeholders(one_other[1]);
                        let mut got = placeholders(form);
                        want.sort_unstable();
                        got.sort_unstable();
                        assert_eq!(want, got, "{}: placeholders differ in {src:?}", l.code);
                    }
                    continue;
                }
                let mut want = placeholders(src);
                let mut got = placeholders(tr);
                want.sort_unstable();
                got.sort_unstable();
                assert_eq!(want, got, "{}: placeholders differ in {src:?}", l.code);
                if ctx.is_empty() {
                    assert_eq!(src.ends_with('…'), tr.ends_with('…'), "{}: ellipsis mismatch: {src:?}", l.code);
                }
                if ctx == "@id" {
                    assert!(crate::menus::is_live(src) || crate::menu_catalog::CATALOG.iter().any(|m| m.3 == src), "{}: unknown command id {src:?}", l.code);
                }
            }
        }
    }

    /// Languages that claim complete menus have an entry for every label and path segment.
    #[test]
    fn complete_languages_translate_every_menu_string() {
        let mut strings = std::collections::BTreeSet::new();
        for &(path, label, _, _) in crate::menu_catalog::CATALOG {
            strings.extend(path.iter().copied());
            strings.insert(label);
        }
        for &(_, label, path, _) in crate::menus::UI_COMMANDS {
            strings.extend(path.iter().copied());
            strings.insert(label);
        }
        for c in photocraft_engine::command_specs().iter().filter(|c| !c.menu.is_empty()) {
            strings.extend(c.menu.iter().copied());
            strings.insert(c.label);
        }
        strings.remove("---");
        for l in LANGUAGES.iter().filter(|l| l.complete_menus) {
            let cat = l.catalog();
            let missing: Vec<_> = strings.iter().filter(|s| cat.plain(s).is_none()).collect();
            assert!(missing.is_empty(), "{}: untranslated menu strings: {missing:#?}", l.code);
        }
    }

    /// Every `tl!("literal")` in the shell has an entry in each language that claims complete menus
    /// (so a new label can't ship untranslated by accident). Literals that are deliberately shown as
    /// they are (names, units) are listed in `KEEP_AS_IS`.
    #[test]
    fn every_tl_literal_is_translated() {
        const KEEP_AS_IS: &[&str] = &[];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut literals = std::collections::BTreeSet::new();
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("lib.rs") {
                    let text = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
                    // Test modules aside, scan every `tl!("…")`. Cut at the test *module*: a
                    // `#[cfg(test)]` on a single item earlier in the file must not hide the rest.
                    let code = text.split("#[cfg(test)]\nmod ").next().unwrap_or("");
                    let mut rest = code;
                    while let Some(at) = rest.find("tl!(\"") {
                        rest = &rest[at + 5..];
                        let mut end = 0;
                        let bytes = rest.as_bytes();
                        while end < bytes.len() && !(bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\')) {
                            end += 1;
                        }
                        let lit = rest.get(..end).unwrap_or("").replace("\\\"", "\"");
                        if rest.get(end + 1..end + 2) == Some(")") {
                            literals.insert(lit);
                        }
                    }
                }
            }
        }
        assert!(literals.len() > 300, "scan found only {} literals", literals.len());
        for l in LANGUAGES.iter().filter(|l| l.complete_menus) {
            let cat = l.catalog();
            let missing: Vec<_> = literals.iter().filter(|s| !KEEP_AS_IS.contains(&s.as_str()) && cat.plain(s).is_none()).collect();
            assert!(missing.is_empty(), "{}: untranslated tl! strings: {missing:#?}", l.code);
        }
    }

    /// Blend mode names come from the colour crate; each must be translated.
    #[test]
    fn blend_mode_names_are_translated() {
        for l in LANGUAGES.iter().filter(|l| l.complete_menus) {
            for m in std::iter::once(photocraft_color::BlendMode::PassThrough).chain(photocraft_color::BlendMode::LAYER_MODES) {
                assert!(l.catalog().plain(m.label()).is_some(), "{}: blend mode {:?}", l.code, m.label());
            }
        }
    }
}

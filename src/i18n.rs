//! UI translations. One TOML file per language in `locales/`, embedded into the binary.
//! English is the source and fallback language: a key missing in the active language is
//! taken from English. Plural forms are picked by a small rule per language.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

type PluralRule = fn(u64) -> &'static str;

/// Available languages: code, translation file, plural rule. English must stay first.
const LANGUAGES: &[(&str, &str, PluralRule)] =
    &[("en", include_str!("../locales/en.toml"), plural_en), ("pl", include_str!("../locales/pl.toml"), plural_pl)];

fn plural_en(n: u64) -> &'static str {
    if n == 1 { "one" } else { "other" }
}

fn plural_pl(n: u64) -> &'static str {
    if n == 1 {
        "one"
    } else if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {
        "few"
    } else {
        "many"
    }
}

enum Entry {
    Text(String),
    Plural(HashMap<String, String>),
}

struct Catalog {
    code: &'static str,
    name: String,
    entries: HashMap<String, Entry>,
    plural: PluralRule,
}

static CATALOGS: OnceLock<Vec<Catalog>> = OnceLock::new();
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

fn catalogs() -> &'static [Catalog] {
    CATALOGS.get_or_init(|| LANGUAGES.iter().map(|&(code, src, plural)| parse(code, src, plural)).collect())
}

/// Flattens `[section] key = ...` into `section.key`. A broken file yields an empty
/// catalog (everything falls back to English); the tests make sure that never ships.
fn parse(code: &'static str, src: &str, plural: PluralRule) -> Catalog {
    let table: toml::Table = toml::from_str(src).unwrap_or_default();
    let mut entries = HashMap::new();
    let mut name = code.to_string();
    for (section, value) in table {
        let Some(keys) = value.as_table() else { continue };
        for (key, v) in keys {
            if section == "meta" {
                if key == "name" {
                    name = v.as_str().unwrap_or(code).to_string();
                }
                continue;
            }
            let entry = match v {
                toml::Value::String(s) => Entry::Text(s.clone()),
                toml::Value::Table(forms) => Entry::Plural(
                    forms.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect(),
                ),
                _ => continue,
            };
            entries.insert(format!("{section}.{key}"), entry);
        }
    }
    Catalog { code, name, entries, plural }
}

/// Maps a locale such as `pl-PL`, `pl_PL.UTF-8` or `PL` to an available language, or English.
fn resolve(locale: &str) -> usize {
    let primary = locale.split(['-', '_', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
    LANGUAGES.iter().position(|(code, _, _)| *code == primary).unwrap_or(0)
}

/// The language to start with: `NOPEPDF_LANG` (for testing translations), then the saved
/// setting (empty means "follow the system"), then the system UI language.
pub fn startup_language(setting: &str) -> String {
    match std::env::var("NOPEPDF_LANG") {
        Ok(l) if !l.trim().is_empty() => l,
        _ if !setting.is_empty() => setting.to_string(),
        _ => system_language(),
    }
}

pub fn system_language() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "en".into())
}

/// Switches the UI language; takes effect on the next frame.
pub fn set_language(locale: &str) {
    ACTIVE.store(resolve(locale), Ordering::Relaxed);
}

/// (code, native name) of every available language.
pub fn languages() -> impl Iterator<Item = (&'static str, &'static str)> {
    catalogs().iter().map(|c| (c.code, c.name.as_str()))
}

fn active() -> usize {
    ACTIVE.load(Ordering::Relaxed).min(LANGUAGES.len() - 1)
}

fn text_in(lang: usize, key: &'static str) -> &'static str {
    let cats = catalogs();
    for cat in [&cats[lang], &cats[0]] {
        if let Some(Entry::Text(s)) = cat.entries.get(key) {
            return s;
        }
    }
    key
}

fn count_in(lang: usize, key: &'static str, n: u64) -> &'static str {
    let cats = catalogs();
    for cat in [&cats[lang], &cats[0]] {
        if let Some(Entry::Plural(forms)) = cat.entries.get(key)
            && let Some(s) = forms.get((cat.plural)(n)).or_else(|| forms.get("other"))
        {
            return s;
        }
    }
    key
}

fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut s = template.to_string();
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), &value.to_string());
    }
    s
}

/// Translated text for `key`, e.g. `tr("toolbar.open")`.
pub fn tr(key: &'static str) -> &'static str {
    text_in(active(), key)
}

/// Translated text with named placeholders filled in.
pub fn tr_args(key: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(tr(key), args)
}

/// Plural-aware text: picks the form for `n` and fills `{n}` plus `args`.
pub fn tr_count(key: &'static str, n: usize, args: &[(&str, &dyn Display)]) -> String {
    let s = fill(count_in(active(), key, n as u64), args);
    fill(&s, &[("n", &n)])
}

/// A label with its keyboard shortcut, e.g. "Open (Ctrl+O)". Shortcuts are kept out of the
/// translation files so they cannot drift from the real key bindings.
pub fn with_keys(label: &str, keys: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{label} ({})", keys.replace("Ctrl+", "⌘"))
    } else {
        format!("{label} ({keys})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(name, _)| name.to_string())).collect()
    }

    fn entry_placeholders(e: &Entry) -> BTreeSet<String> {
        match e {
            Entry::Text(s) => placeholders(s),
            Entry::Plural(forms) => forms.values().flat_map(|s| placeholders(s)).collect(),
        }
    }

    #[test]
    fn files_parse() {
        for (code, src, _) in LANGUAGES {
            let t: Result<toml::Table, _> = toml::from_str(src);
            assert!(t.is_ok(), "locales/{code}.toml: {:?}", t.err());
        }
        assert_eq!(catalogs()[0].code, "en");
        assert!(catalogs().iter().all(|c| c.name != c.code), "every file needs [meta] name");
    }

    #[test]
    fn languages_match_english() {
        let en = &catalogs()[0];
        for cat in &catalogs()[1..] {
            let mut missing: Vec<_> = en.entries.keys().filter(|k| !cat.entries.contains_key(*k)).collect();
            let mut extra: Vec<_> = cat.entries.keys().filter(|k| !en.entries.contains_key(*k)).collect();
            missing.sort();
            extra.sort();
            assert!(missing.is_empty() && extra.is_empty(), "{}: missing {missing:?}, extra {extra:?}", cat.code);
            // Every plural category the language's rule can produce must be present.
            let needed: BTreeSet<&str> = (0..1000).map(cat.plural).collect();
            for (key, e) in &cat.entries {
                let en_e = &en.entries[key];
                assert_eq!(
                    entry_placeholders(e),
                    entry_placeholders(en_e),
                    "{}: placeholders of {key} differ from English",
                    cat.code
                );
                match (e, en_e) {
                    (Entry::Text(_), Entry::Text(_)) => {}
                    (Entry::Plural(forms), Entry::Plural(_)) => {
                        for c in &needed {
                            assert!(forms.contains_key(*c), "{}: {key} lacks plural form '{c}'", cat.code);
                        }
                    }
                    _ => panic!("{}: {key} is plural in one file and plain text in the other", cat.code),
                }
            }
        }
    }

    #[test]
    fn english_plurals_complete() {
        for (key, e) in &catalogs()[0].entries {
            if let Entry::Plural(forms) = e {
                assert!(forms.contains_key("one") && forms.contains_key("other"), "en: {key}");
            }
        }
    }

    #[test]
    fn polish_plural_rule() {
        let cases = [
            (0, "many"),
            (1, "one"),
            (2, "few"),
            (4, "few"),
            (5, "many"),
            (12, "many"),
            (14, "many"),
            (21, "many"),
            (22, "few"),
            (25, "many"),
            (104, "few"),
            (112, "many"),
        ];
        for (n, want) in cases {
            assert_eq!(plural_pl(n), want, "n = {n}");
        }
        assert_eq!(plural_en(1), "one");
        assert_eq!(plural_en(0), "other");
        assert_eq!(plural_en(2), "other");
    }

    #[test]
    fn search_counter_forms() {
        let pl = resolve("pl");
        let f = |n: u64| fill(count_in(pl, "search.results", n), &[("n", &n)]);
        assert_eq!(f(1), "1 wynik");
        assert_eq!(f(3), "3 wyniki");
        assert_eq!(f(5), "5 wyników");
        assert_eq!(f(22), "22 wyniki");
        let f = |n: u64| fill(count_in(0, "search.results", n), &[("n", &n)]);
        assert_eq!(f(1), "1 result");
        assert_eq!(f(2), "2 results");
        let s = fill(count_in(pl, "search.searching", 2), &[("n", &2), ("page", &7)]);
        assert_eq!(s, "2 wyniki… (str. 7)");
    }

    #[test]
    fn resolve_locales() {
        assert_eq!(LANGUAGES[resolve("pl-PL")].0, "pl");
        assert_eq!(LANGUAGES[resolve("pl_PL.UTF-8")].0, "pl");
        assert_eq!(LANGUAGES[resolve("PL")].0, "pl");
        assert_eq!(LANGUAGES[resolve("en-US")].0, "en");
        assert_eq!(LANGUAGES[resolve("de-DE")].0, "en");
        assert_eq!(LANGUAGES[resolve("")].0, "en");
    }

    #[test]
    fn missing_key_falls_back() {
        assert_eq!(text_in(resolve("pl"), "no.such_key"), "no.such_key");
        assert_eq!(text_in(resolve("pl"), "toolbar.open"), "Otwórz");
        assert_eq!(text_in(0, "toolbar.open"), "Open");
    }

    /// Catches UI text hard-coded in Polish instead of going through the translation files.
    #[test]
    fn no_polish_literals_in_code() {
        let sources = [
            ("app.rs", include_str!("app.rs")),
            ("view.rs", include_str!("view.rs")),
            ("worker.rs", include_str!("worker.rs")),
            ("print.rs", include_str!("print.rs")),
            ("main.rs", include_str!("main.rs")),
            ("settings.rs", include_str!("settings.rs")),
            ("instance.rs", include_str!("instance.rs")),
        ];
        let polish = ['\u{105}', '\u{107}', '\u{119}', '\u{142}', '\u{144}', '\u{f3}', '\u{15b}', '\u{17a}', '\u{17c}'];
        for (file, src) in sources {
            for (i, line) in src.lines().enumerate() {
                let lower = line.to_lowercase();
                assert!(!lower.chars().any(|c| polish.contains(&c)), "{file}:{}: {line}", i + 1);
            }
        }
    }
}

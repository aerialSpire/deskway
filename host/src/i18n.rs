//! Text shown to people, in English, Simplified Chinese and Traditional Chinese (Taiwan).
//!
//! The catalogs in `host/i18n/` are flat JSON objects with dotted keys and `{name}`
//! placeholders. The language comes from the host's own setting (`lang` in its config), else
//! the system UI language. It is the host's choice, not the controlling browser's: notices at
//! this computer are for the person sitting here. A missing text falls back to English, then
//! to the key itself.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    ZhHans,
    ZhHant,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::En, Lang::ZhHans, Lang::ZhHant];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhHans => "zh-Hans",
            Lang::ZhHant => "zh-Hant",
        }
    }

    /// One of our own codes, as stored in the config. Anything else is no choice at all.
    pub fn parse(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code().eq_ignore_ascii_case(code.trim()))
    }

    /// A system locale tag (`zh-TW`, `zh_CN.UTF-8`, `zh-Hant-HK`, `en-US` …) to the nearest
    /// catalog. Taiwan, Hong Kong and Macau write Traditional; other Chinese, Simplified.
    pub fn from_locale(tag: &str) -> Lang {
        let tag = tag.split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
        let mut parts = tag.split('-');
        if parts.next() != Some("zh") {
            return Lang::En;
        }
        let rest: Vec<&str> = parts.collect();
        if rest.contains(&"hans") {
            Lang::ZhHans
        } else if rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) {
            Lang::ZhHant
        } else {
            Lang::ZhHans
        }
    }

    fn source(self) -> &'static str {
        match self {
            Lang::En => include_str!("../i18n/en.json"),
            Lang::ZhHans => include_str!("../i18n/zh-Hans.json"),
            Lang::ZhHant => include_str!("../i18n/zh-Hant.json"),
        }
    }
}

type Catalog = HashMap<String, String>;

fn catalog(lang: Lang) -> &'static Catalog {
    static CATALOGS: [OnceLock<Catalog>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let i = Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0);
    CATALOGS[i].get_or_init(|| serde_json::from_str(lang.source()).expect("valid catalog"))
}

/// The setting from the config; None follows the system.
static CHOSEN: RwLock<Option<Lang>> = RwLock::new(None);

/// Uses this language from now on: a code from the config, or None to follow the system.
pub fn set(code: Option<&str>) {
    *CHOSEN.write().unwrap() = code.and_then(Lang::parse);
}

/// Takes the language from this program's config, if it has one.
pub fn from_config() {
    if let Ok(c) = crate::config::Config::load() {
        set(c.lang.as_deref());
    }
}

pub fn current() -> Lang {
    CHOSEN.read().unwrap().unwrap_or_else(system)
}

/// The system UI language, read once.
pub fn system() -> Lang {
    static SYSTEM: OnceLock<Lang> = OnceLock::new();
    *SYSTEM.get_or_init(|| crate::platform::system_language().map(|t| Lang::from_locale(&t)).unwrap_or(Lang::En))
}

fn lookup(lang: Lang, key: &str) -> String {
    pick(catalog(lang), catalog(Lang::En), key)
}

fn pick(primary: &Catalog, english: &Catalog, key: &str) -> String {
    primary.get(key).or_else(|| english.get(key)).cloned().unwrap_or_else(|| key.to_string())
}

/// The text for `key` in the current language.
pub fn t(key: &str) -> String {
    lookup(current(), key)
}

fn fill(mut text: String, args: &[(&str, String)]) -> String {
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// The text for `key` with its `{name}` placeholders filled in.
pub fn tf(key: &str, args: &[(&str, String)]) -> String {
    fill(t(key), args)
}

/// A count: `key.one` for exactly one, `key.other` otherwise. `{n}` is the count.
pub fn tn(key: &str, n: usize, args: &[(&str, String)]) -> String {
    let key = format!("{key}.{}", if n == 1 { "one" } else { "other" });
    let mut all = vec![("n", n.to_string())];
    all.extend(args.iter().cloned());
    fill(t(&key), &all)
}

/// The current language for a page: its code and every text, English filling any gaps.
pub fn page_data() -> Value {
    let lang = current();
    let mut dict: serde_json::Map<String, Value> = catalog(Lang::En).iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    for (k, v) in catalog(lang) {
        dict.insert(k.clone(), json!(v));
    }
    json!({ "lang": lang.code(), "dict": dict })
}

/// Runs before a page's own script, so its first paint is already in the right language.
pub fn page_script() -> String {
    format!("window.I18N = {};", page_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back() {
        let primary: Catalog = [("a".to_string(), "A-zh".to_string())].into();
        let english: Catalog = [("a".to_string(), "A".to_string()), ("b".to_string(), "B".to_string())].into();
        assert_eq!(pick(&primary, &english, "a"), "A-zh");
        assert_eq!(pick(&primary, &english, "b"), "B");
        assert_eq!(pick(&primary, &english, "c"), "c");
        assert_eq!(lookup(Lang::ZhHant, "no.such.key"), "no.such.key");
        assert_eq!(lookup(Lang::En, "overlay.open_folder"), "Open folder");
        assert_eq!(fill(lookup(Lang::En, "panel.disconnect_name"), &[("name", "Ann".into())]), "Disconnect Ann");
    }

    #[test]
    fn plurals() {
        let one = fill(lookup(Lang::En, "note.received.one"), &[("n", "1".into())]);
        let many = fill(lookup(Lang::En, "note.received.other"), &[("n", "3".into())]);
        assert_eq!(one, "Received 1 file");
        assert_eq!(many, "Received 3 files");
    }

    #[test]
    fn every_catalog_has_the_same_texts() {
        let en: std::collections::BTreeSet<&String> = catalog(Lang::En).keys().collect();
        for lang in [Lang::ZhHans, Lang::ZhHant] {
            let other: std::collections::BTreeSet<&String> = catalog(lang).keys().collect();
            assert_eq!(en.symmetric_difference(&other).collect::<Vec<_>>(), Vec::<&&String>::new(), "{} and en differ", lang.code());
        }
    }

    /// Every key named in the source exists: `t("…")`, `tf("…")` and `tn("…")` (its `.one` and
    /// `.other`) in the program, `t('…')`, `data-i18n` and `data-i18n-attr` in the pages. Keys
    /// made at run time are not seen.
    #[test]
    fn every_key_the_source_names_exists() {
        let is_key = |k: &str| k.contains('.') && k.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        let mut used = std::collections::BTreeSet::new();
        let mut dirs = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for (call, quote, plural) in [("t(\"", '"', false), ("tf(\"", '"', false), ("tn(\"", '"', true), ("t('", '\'', false)] {
                    for (i, _) in text.match_indices(call) {
                        if text[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                            continue;
                        }
                        let rest = &text[i + call.len()..];
                        let key = &rest[..rest.find(quote).unwrap_or(0)];
                        if is_key(key) {
                            if plural {
                                used.extend([format!("{key}.one"), format!("{key}.other")]);
                            } else {
                                used.insert(key.to_string());
                            }
                        }
                    }
                }
                for (attr, pairs) in [("data-i18n=\"", false), ("data-i18n-attr=\"", true)] {
                    for (i, _) in text.match_indices(attr) {
                        let rest = &text[i + attr.len()..];
                        let value = &rest[..rest.find('"').unwrap_or(0)];
                        for part in value.split(',') {
                            let key = if pairs { part.split('=').nth(1).unwrap_or_default() } else { part };
                            used.insert(key.trim().to_string());
                        }
                    }
                }
            }
        }
        assert!(used.len() > 100, "the source names its texts: {}", used.len());
        let missing: Vec<&String> = used.iter().filter(|k| !catalog(Lang::En).contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "no text for {missing:?}");
    }

    #[test]
    fn detects_language() {
        for tag in ["zh-TW", "zh-HK", "zh-MO", "zh-Hant-TW", "zh-Hant", "zh_TW.UTF-8"] {
            assert_eq!(Lang::from_locale(tag), Lang::ZhHant, "{tag}");
        }
        for tag in ["zh-CN", "zh-SG", "zh", "zh-Hans", "zh-Hans-HK", "zh_CN.UTF-8"] {
            assert_eq!(Lang::from_locale(tag), Lang::ZhHans, "{tag}");
        }
        for tag in ["en-US", "ja-JP", "fr", "", "C"] {
            assert_eq!(Lang::from_locale(tag), Lang::En, "{tag}");
        }
        assert_eq!(Lang::parse("zh-hant"), Some(Lang::ZhHant));
        assert_eq!(Lang::parse("zh-TW"), None);
    }
}

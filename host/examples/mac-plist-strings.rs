//! Writes the app bundle's product name and localized permission texts from the host sources.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[path = "../src/product.rs"]
mod product;

/// Info.plist key, catalog key.
const TEXTS: [(&str, &str); 2] = [
    ("NSScreenCaptureUsageDescription", "mac.plist_screen"),
    ("NSDownloadsFolderUsageDescription", "mac.plist_downloads"),
];

const CATALOGS: [(&str, &str); 3] = [
    ("en", include_str!("../i18n/en.json")),
    ("zh-Hans", include_str!("../i18n/zh-Hans.json")),
    ("zh-Hant", include_str!("../i18n/zh-Hant.json")),
];

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

fn main() -> Result<()> {
    let contents = std::env::args_os().nth(1).map(PathBuf::from).context("specify the bundle's Contents directory")?;
    let name = product::NAME.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let plist = include_str!("../macos/Info.plist").replace("@PRODUCT_NAME@", &name);
    std::fs::write(contents.join("Info.plist"), plist)?;
    let resources = contents.join("Resources");
    for (lang, source) in CATALOGS {
        let catalog: serde_json::Map<String, serde_json::Value> = serde_json::from_str(source)?;
        let mut text = String::from("/* Generated from host/i18n by examples/mac-plist-strings.rs. */\n");
        for (plist_key, key) in TEXTS {
            let value = catalog.get(key).and_then(|v| v.as_str()).with_context(|| format!("{lang} lacks {key}"))?;
            text.push_str(&format!("{} = {};\n", quoted(plist_key), quoted(value)));
        }
        let dir = resources.join(format!("{lang}.lproj"));
        std::fs::create_dir_all(&dir)?;
        // UTF-16 with a byte order mark, the encoding .strings files are documented in.
        let bytes: Vec<u8> = [0xFEFFu16].into_iter().chain(text.encode_utf16()).flat_map(|u| u.to_le_bytes()).collect();
        std::fs::write(dir.join("InfoPlist.strings"), bytes)?;
    }
    Ok(())
}

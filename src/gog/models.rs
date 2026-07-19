#![allow(dead_code)]

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;

const EMBED_BASE: &str = "https://embed.gog.com";

#[derive(Debug, Clone)]
pub struct LibraryGame {
    pub id: u64,
    pub title: String,
    pub image: Option<String>,
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FilteredProducts {
    pub products: Vec<Product>,
    #[serde(rename = "totalPages", default)]
    pub total_pages: u32,
}

#[derive(Debug, Deserialize)]
pub struct Product {
    pub id: u64,
    pub title: String,
    pub image: Option<String>,
    pub slug: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GameDetails {
    pub id: u64,
    pub title: String,
    pub image: Option<String>,
    pub installers: Vec<DownloadFile>,
    pub extras: Vec<DownloadFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub os: Option<String>,
    pub language: Option<String>,
    pub downlink: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Installer,
    Extra,
    Patch,
    LanguagePack,
}

impl GameDetails {
    pub fn from_json(id: u64, value: Value) -> Result<Self> {
        let title = value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let image = value
            .get("image")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                value
                    .pointer("/images/logo")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                value
                    .pointer("/images/icon")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });

        let mut installers = Vec::new();
        let mut extras = Vec::new();

        // Typical shape:
        // "downloads": [ [ "English", { "windows": [ {...} ], "linux": [ {...} ] } ], ... ]
        if let Some(downloads) = value.get("downloads").and_then(|v| v.as_array()) {
            for (block_idx, block) in downloads.iter().enumerate() {
                parse_download_block(block, block_idx, &mut installers);
            }
        }

        if let Some(extra_list) = value.get("extras").and_then(|v| v.as_array()) {
            for (idx, extra) in extra_list.iter().enumerate() {
                if let Some(file) =
                    parse_file_node(extra, None, None, FileKind::Extra, idx)
                {
                    extras.push(file);
                }
            }
        }

        let mut details = Self {
            id,
            title,
            image,
            installers,
            extras,
        };
        ensure_unique_ids(&mut details);
        Ok(details)
    }
}

fn parse_download_block(block: &Value, block_idx: usize, out: &mut Vec<DownloadFile>) {
    // Preferred: [ language, { os: [files...] } ]
    if let Some(arr) = block.as_array() {
        let language = arr.first().and_then(|v| v.as_str()).map(|s| s.to_string());
        if let Some(oses) = arr.get(1).and_then(|v| v.as_object()) {
            for (os, entries) in oses {
                collect_os_file_list(os, entries, language.as_deref(), block_idx, out);
            }
            return;
        }
    }

    // Fallback: { windows: [...], mac: [...] }
    if let Some(obj) = block.as_object() {
        for (os, entries) in obj {
            collect_os_file_list(os, entries, None, block_idx, out);
        }
    }
}

fn collect_os_file_list(
    os: &str,
    entries: &Value,
    language: Option<&str>,
    block_idx: usize,
    out: &mut Vec<DownloadFile>,
) {
    let os_norm = normalize_os(os);
    let Some(list) = entries.as_array() else {
        return;
    };

    for (idx, item) in list.iter().enumerate() {
        // Nested language groups: [ { language, files: [...] }, ... ]
        if let Some(files) = item.get("files").and_then(|v| v.as_array()) {
            let lang = item
                .get("language")
                .and_then(|v| v.as_str())
                .or(language)
                .map(|s| s.to_string());
            for (j, file) in files.iter().enumerate() {
                if let Some(parsed) = parse_file_node(
                    file,
                    Some(&os_norm),
                    lang.as_deref(),
                    FileKind::Installer,
                    block_idx * 10_000 + idx * 100 + j,
                ) {
                    out.push(parsed);
                }
            }
            continue;
        }

        if let Some(parsed) = parse_file_node(
            item,
            Some(&os_norm),
            language,
            FileKind::Installer,
            block_idx * 10_000 + idx,
        ) {
            out.push(parsed);
        }
    }
}

fn parse_file_node(
    node: &Value,
    os: Option<&str>,
    language: Option<&str>,
    kind: FileKind,
    idx: usize,
) -> Option<DownloadFile> {
    let downlink = resolve_downlink_field(node)?;
    let name = node
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| node.get("manualUrl").and_then(|v| v.as_str()))
        .map(|s| s.rsplit('/').next().unwrap_or(s).to_string())
        .unwrap_or_else(|| "download.bin".into());
    let size = parse_size(node.get("size"));
    let id = node
        .get("id")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => format!("{name}-{idx}"),
        })
        .unwrap_or_else(|| format!("{name}-{idx}-{}", os.unwrap_or("any")));

    Some(DownloadFile {
        id,
        name,
        size,
        os: os.map(|s| s.to_string()),
        language: language.map(|s| s.to_string()),
        downlink,
        kind,
    })
}

fn resolve_downlink_field(node: &Value) -> Option<String> {
    if let Some(url) = node.get("downlink").and_then(|v| v.as_str()) {
        return Some(absolutize_gog_url(url));
    }
    if let Some(url) = node.get("manualUrl").and_then(|v| v.as_str()) {
        return Some(absolutize_gog_url(url));
    }
    None
}

fn absolutize_gog_url(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else if url.starts_with('/') {
        format!("{EMBED_BASE}{url}")
    } else {
        format!("{EMBED_BASE}/{url}")
    }
}

fn parse_size(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => parse_human_size(s),
        _ => 0,
    }
}

fn parse_human_size(raw: &str) -> u64 {
    let cleaned = raw.replace(',', "").trim().to_string();
    if let Ok(n) = cleaned.parse::<u64>() {
        return n;
    }

    let lower = cleaned.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    if parts.len() >= 2 {
        if let Ok(value) = parts[0].parse::<f64>() {
            let mult = match parts[1] {
                "b" | "byte" | "bytes" => 1.0,
                "kb" | "kib" => 1024.0,
                "mb" | "mib" => 1024.0 * 1024.0,
                "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
                "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
                _ => 1.0,
            };
            return (value * mult) as u64;
        }
    }

    // e.g. "1.5GB" without space
    let mut num = String::new();
    let mut unit = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if unit.is_empty() {
                num.push(ch);
            }
        } else if !ch.is_whitespace() {
            unit.push(ch);
        }
    }
    if let Ok(value) = num.parse::<f64>() {
        let mult = match unit.as_str() {
            "b" => 1.0,
            "kb" | "kib" => 1024.0,
            "mb" | "mib" => 1024.0 * 1024.0,
            "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
            "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            _ => 0.0,
        };
        if mult > 0.0 {
            return (value * mult) as u64;
        }
    }
    0
}

fn normalize_os(os: &str) -> String {
    match os.to_ascii_lowercase().as_str() {
        "osx" | "mac" | "macos" => "mac".into(),
        "windows" | "win" => "windows".into(),
        "linux" => "linux".into(),
        other => other.into(),
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn file_label(file: &DownloadFile) -> String {
    let mut parts = Vec::new();
    if let Some(os) = &file.os {
        parts.push(os.clone());
    }
    if let Some(lang) = &file.language {
        parts.push(lang.clone());
    }
    parts.push(file.name.clone());
    if file.size > 0 {
        parts.push(format_bytes(file.size));
    }
    parts.join(" · ")
}

pub fn ensure_unique_ids(details: &mut GameDetails) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for file in details
        .installers
        .iter_mut()
        .chain(details.extras.iter_mut())
    {
        let base = file.id.clone();
        let mut id = base.clone();
        let mut n = 1;
        while !seen.insert(id.clone()) {
            n += 1;
            id = format!("{base}-{n}");
        }
        file.id = id;
    }
}

pub fn parse_owned_ids(value: Value) -> Result<Vec<u64>> {
    value
        .get("owned")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing owned array"))
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_language_os_download_blocks() {
        let value = json!({
            "title": "Example Game",
            "downloads": [[
                "English",
                {
                    "windows": [{
                        "name": "setup_example.exe",
                        "size": "1.5 GB",
                        "manualUrl": "/downlink/example/setup_example.exe"
                    }],
                    "linux": [{
                        "name": "example.sh",
                        "size": "800 MB",
                        "downlink": "https://embed.gog.com/downlink/example/example.sh"
                    }]
                }
            ]],
            "extras": [{
                "name": "manual.pdf",
                "size": "12 MB",
                "manualUrl": "/downlink/example/manual.pdf"
            }]
        });

        let details = GameDetails::from_json(42, value).unwrap();
        assert_eq!(details.title, "Example Game");
        assert_eq!(details.installers.len(), 2);
        assert_eq!(details.extras.len(), 1);
        assert!(details.installers.iter().any(|f| f.os.as_deref() == Some("windows")));
        assert!(details.installers.iter().any(|f| f.os.as_deref() == Some("linux")));
        assert!(details.installers[0].downlink.starts_with("https://embed.gog.com/"));
        assert!(details.installers.iter().any(|f| f.size > 1_000_000_000));
    }

    #[test]
    fn parse_human_sizes() {
        assert_eq!(parse_human_size("1024"), 1024);
        assert_eq!(parse_human_size("1.5 GB"), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_human_size("800 MB"), 800 * 1024 * 1024);
    }

    #[test]
    fn parses_multiple_language_download_blocks() {
        let value = json!({
            "title": "Multi Lang Game",
            "downloads": [
                ["English", { "windows": [{
                    "name": "setup_en.exe", "size": "100 MB",
                    "manualUrl": "/downlink/game/setup_en.exe"
                }]}],
                ["Deutsch", { "windows": [{
                    "name": "setup_de.exe", "size": "100 MB",
                    "manualUrl": "/downlink/game/setup_de.exe"
                }], "linux": [{
                    "name": "setup_de.sh", "size": "90 MB",
                    "manualUrl": "/downlink/game/setup_de.sh"
                }]}],
                ["français", { "windows": [{
                    "name": "setup_fr.exe", "size": "100 MB",
                    "manualUrl": "/downlink/game/setup_fr.exe"
                }]}]
            ],
            "extras": []
        });
        let details = GameDetails::from_json(1, value).unwrap();
        let langs: std::collections::BTreeSet<_> = details
            .installers
            .iter()
            .filter_map(|f| f.language.as_deref())
            .collect();
        assert_eq!(
            langs,
            ["Deutsch", "English", "français"]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(details.installers.len(), 4);
    }
}

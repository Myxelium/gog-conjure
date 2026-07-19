//! Future automatic disc burning support.
//!
//! This module defines disc media capacities and a packer that suggests which
//! downloaded games fit on a DVD or Blu-ray. Burning itself is intentionally
//! not implemented yet — only planning / suggestion APIs for a later feature.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::gog::format_bytes;

/// Optical media profiles we plan to support for burning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscMedia {
    Dvd5,
    Dvd9,
    Bd25,
    Bd50,
    Bd100,
}

impl DiscMedia {
    pub fn all() -> &'static [DiscMedia] {
        &[
            DiscMedia::Dvd5,
            DiscMedia::Dvd9,
            DiscMedia::Bd25,
            DiscMedia::Bd50,
            DiscMedia::Bd100,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DiscMedia::Dvd5 => "DVD-5 (~4.7 GB)",
            DiscMedia::Dvd9 => "DVD-9 (~8.5 GB)",
            DiscMedia::Bd25 => "Blu-ray 25 GB",
            DiscMedia::Bd50 => "Blu-ray 50 GB",
            DiscMedia::Bd100 => "Blu-ray 100 GB",
        }
    }

    /// Usable capacity in bytes (conservative ISO-ish limits with a small safety margin).
    pub fn capacity_bytes(self) -> u64 {
        match self {
            // 4,700,000,000 decimal manufacturer rating ≈ 4.37 GiB; leave ~2% margin.
            DiscMedia::Dvd5 => 4_377_000_000,
            DiscMedia::Dvd9 => 7_925_000_000,
            DiscMedia::Bd25 => 25_025_000_000,
            DiscMedia::Bd50 => 50_050_000_000,
            DiscMedia::Bd100 => 100_100_000_000,
        }
    }
}

/// A game folder candidate for packing onto a disc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnCandidate {
    pub game_id: u64,
    pub title: String,
    /// Total bytes on disk for this game's folder (installers/extras already downloaded).
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscPack {
    pub media: DiscMedia,
    pub selected: Vec<BurnCandidate>,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
}

impl DiscPack {
    pub fn used_label(&self) -> String {
        format!(
            "{} / {} ({} free)",
            format_bytes(self.used_bytes),
            format_bytes(self.media.capacity_bytes()),
            format_bytes(self.remaining_bytes)
        )
    }
}

/// Trait for a future burner backend (cdrtools, libburn, macOS Disk Utility, etc.).
pub trait DiscBurner: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    /// Placeholder — will write an ISO / burn `pack` to the chosen drive.
    fn burn(&self, _pack: &DiscPack, _drive: &str) -> Result<(), BurnError>;
}

#[derive(Debug, thiserror::Error)]
pub enum BurnError {
    #[error("burning is not implemented yet")]
    NotImplemented,
    #[error("no optical drive found")]
    NoDrive,
    #[error("{0}")]
    Other(String),
}

/// Stub burner kept so UI / CI can wire the feature without real hardware APIs.
#[derive(Debug, Default)]
pub struct StubBurner;

impl DiscBurner for StubBurner {
    fn name(&self) -> &str {
        "stub (not implemented)"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn burn(&self, _pack: &DiscPack, _drive: &str) -> Result<(), BurnError> {
        Err(BurnError::NotImplemented)
    }
}

/// Suggest a pack of games that fit on `media`.
///
/// Strategy: largest-first greedy fill — good enough for MVP suggestions and
/// easy to replace with a true knapsack later.
pub fn suggest_pack(media: DiscMedia, mut candidates: Vec<BurnCandidate>) -> DiscPack {
    let capacity = media.capacity_bytes();
    candidates.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    let mut selected = Vec::new();
    let mut used = 0u64;

    for game in candidates {
        if game.size_bytes == 0 {
            continue;
        }
        if used.saturating_add(game.size_bytes) <= capacity {
            used += game.size_bytes;
            selected.push(game);
        }
    }

    // Prefer a denser fill: try adding any leftovers that now fit (small games).
    // (Already handled by continuing the loop after large items.)

    DiscPack {
        media,
        selected,
        used_bytes: used,
        remaining_bytes: capacity.saturating_sub(used),
    }
}

/// Scan a download root for per-game folders and estimate sizes.
pub fn scan_download_root(root: &std::path::Path) -> std::io::Result<Vec<BurnCandidate>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let title = entry.file_name().to_string_lossy().to_string();
        let size = dir_size(&entry.path()).unwrap_or(0);
        out.push(BurnCandidate {
            game_id: 0, // unknown until linked back to library
            title,
            size_bytes: size,
        });
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(out)
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += dir_size(&entry.path())?;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dvd_pack_respects_capacity() {
        let games = vec![
            BurnCandidate {
                game_id: 1,
                title: "Big".into(),
                size_bytes: 3_000_000_000,
            },
            BurnCandidate {
                game_id: 2,
                title: "Medium".into(),
                size_bytes: 1_200_000_000,
            },
            BurnCandidate {
                game_id: 3,
                title: "Tiny".into(),
                size_bytes: 100_000_000,
            },
            BurnCandidate {
                game_id: 4,
                title: "TooBigAlone".into(),
                size_bytes: 5_000_000_000,
            },
        ];
        let pack = suggest_pack(DiscMedia::Dvd5, games);
        assert!(pack.used_bytes <= DiscMedia::Dvd5.capacity_bytes());
        assert!(pack.selected.iter().all(|g| g.title != "TooBigAlone"));
        assert!(pack.selected.iter().any(|g| g.title == "Big"));
    }
}

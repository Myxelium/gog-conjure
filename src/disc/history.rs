use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{load_json, save_json};

/// Persisted burn / download memory across sessions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BurnHistory {
    /// Games successfully burned at least once.
    pub burned_game_ids: Vec<u64>,
    /// Games known to have been downloaded (folder may still exist).
    pub known_downloads: Vec<KnownDownload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownDownload {
    pub game_id: u64,
    pub title: String,
}

impl BurnHistory {
    pub fn load() -> Self {
        load_json("burn_history.json").unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        save_json("burn_history.json", self)
    }

    pub fn burned_set(&self) -> HashSet<u64> {
        self.burned_game_ids.iter().copied().collect()
    }

    pub fn mark_burned(&mut self, game_ids: impl IntoIterator<Item = u64>) {
        let mut set = self.burned_set();
        for id in game_ids {
            if id != 0 {
                set.insert(id);
            }
        }
        self.burned_game_ids = set.into_iter().collect();
        self.burned_game_ids.sort_unstable();
    }

    pub fn remember_download(&mut self, game_id: u64, title: String) {
        if game_id == 0 {
            return;
        }
        if let Some(existing) = self.known_downloads.iter_mut().find(|d| d.game_id == game_id) {
            existing.title = title;
        } else {
            self.known_downloads.push(KnownDownload { game_id, title });
        }
    }

    pub fn is_burned(&self, game_id: u64) -> bool {
        game_id != 0 && self.burned_game_ids.contains(&game_id)
    }
}

/// A game folder found under the download root (and/or remembered).
#[derive(Debug, Clone)]
pub struct AvailableDownload {
    pub game_id: u64,
    pub title: String,
    pub folder: PathBuf,
    pub size_bytes: u64,
    pub burned: bool,
    pub on_burn_list: bool,
}

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub download_root: Option<PathBuf>,
    pub max_concurrent_downloads: usize,
}

impl AppConfig {
    pub fn load() -> Self {
        load_json("config.json").unwrap_or(Self {
            download_root: None,
            max_concurrent_downloads: 2,
        })
    }

    pub fn save(&self) -> Result<()> {
        save_json("config.json", self)
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "gog-conjure", "gog-conjure")
        .context("could not resolve config directory")?;
    let path = dirs.config_dir().to_path_buf();
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn load_json<T: for<'de> Deserialize<'de>>(name: &str) -> Result<T> {
    let path = config_dir()?.join(name);
    let data = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save_json<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let path = config_dir()?.join(name);
    let data = serde_json::to_string_pretty(value)?;
    fs::write(path, data)?;
    Ok(())
}

//! Stage a disc layout on disk for backends that burn a directory or ISO.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::burner::BurnError;
use super::models::DiscLayout;
use super::pack::iso_path_for;

/// Temporary on-disk layout mirroring ISO paths (`/{game}/{relative}`).
pub struct StagedDisc {
    pub root: PathBuf,
}

impl Drop for StagedDisc {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Build a staging directory with hardlinks (or copies) for every mapped unit file/folder.
pub fn stage_disc_layout(
    disc: &DiscLayout,
    game_folders: &[(u64, PathBuf)],
) -> Result<StagedDisc, BurnError> {
    let root = std::env::temp_dir().join(format!(
        "gog-conjure-stage-disc{:02}-{}",
        disc.index + 1,
        std::process::id()
    ));
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).map_err(|e| {
        BurnError::Other(format!("failed to create staging dir {}: {e}", root.display()))
    })?;

    // Include GOG and synthetic local ids; only the invalid 0 sentinel is skipped.
    let folder_by_id: HashMap<u64, &PathBuf> = game_folders
        .iter()
        .filter(|(id, _)| *id != 0)
        .map(|(id, p)| (*id, p))
        .collect();

    let mut mapped_folders = HashSet::new();
    for unit in &disc.units {
        let folder = folder_by_id
            .get(&unit.game_id)
            .copied()
            .or_else(|| find_folder_by_title(game_folders, &unit.game_title))
            .ok_or_else(|| {
                BurnError::Other(format!("missing folder for game '{}'", unit.game_title))
            })?;
        if !folder.is_dir() {
            return Err(BurnError::Other(format!(
                "folder not found for '{}': {}",
                unit.game_title,
                folder.display()
            )));
        }

        if unit.part_count == 1 && unit.files.is_empty() {
            let dest = root.join(sanitize_filename::sanitize(&unit.game_title));
            link_or_copy_tree(folder, &dest)?;
            continue;
        }

        if unit.part_count == 1 {
            let key = unit.game_id;
            if mapped_folders.insert(key) {
                let dest = root.join(sanitize_filename::sanitize(&unit.game_title));
                link_or_copy_tree(folder, &dest)?;
            }
            continue;
        }

        for file in &unit.files {
            if !file.path.is_file() {
                return Err(BurnError::Other(format!(
                    "missing file for '{}': {}",
                    unit.game_title,
                    file.path.display()
                )));
            }
            let iso = iso_path_for(&unit.game_title, &file.relative_name);
            let rel = iso.trim_start_matches('/');
            let dest = root.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    BurnError::Other(format!("failed to create {}: {e}", parent.display()))
                })?;
            }
            link_or_copy_file(&file.path, &dest)?;
        }
    }

    Ok(StagedDisc { root })
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn simulate_iso_path(disc_index: usize) -> PathBuf {
    std::env::temp_dir().join(format!("gog-conjure-simulate-disc{:02}.iso", disc_index + 1))
}

fn find_folder_by_title<'a>(
    game_folders: &'a [(u64, PathBuf)],
    title: &str,
) -> Option<&'a PathBuf> {
    let sanitized = sanitize_filename::sanitize(title);
    game_folders.iter().find_map(|(_, p)| {
        let name = p.file_name()?.to_string_lossy();
        if name == sanitized || name.eq_ignore_ascii_case(title) {
            Some(p)
        } else {
            None
        }
    })
}

fn link_or_copy_tree(src: &Path, dest: &Path) -> Result<(), BurnError> {
    if dest.exists() {
        return Ok(());
    }
    fs::create_dir_all(dest).map_err(|e| {
        BurnError::Other(format!("failed to create {}: {e}", dest.display()))
    })?;
    for entry in fs::read_dir(src).map_err(|e| {
        BurnError::Other(format!("failed to read {}: {e}", src.display()))
    })? {
        let entry = entry.map_err(|e| BurnError::Other(e.to_string()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = entry
            .metadata()
            .map_err(|e| BurnError::Other(format!("{}: {e}", from.display())))?;
        if meta.is_dir() {
            link_or_copy_tree(&from, &to)?;
        } else if meta.is_file() {
            link_or_copy_file(&from, &to)?;
        }
    }
    Ok(())
}

fn link_or_copy_file(src: &Path, dest: &Path) -> Result<(), BurnError> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            BurnError::Other(format!("failed to create {}: {e}", parent.display()))
        })?;
    }
    match fs::hard_link(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device or unsupported — fall back to symlink, then copy.
            if symlink_file(src, dest).is_ok() {
                return Ok(());
            }
            fs::copy(src, dest).map_err(|e| {
                BurnError::Other(format!(
                    "failed to stage {} → {}: {e}",
                    src.display(),
                    dest.display()
                ))
            })?;
            Ok(())
        }
    }
}

fn symlink_file(src: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dest)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dest)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (src, dest);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlink not supported",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::media::DiscMedia;
    use crate::disc::models::{BurnOptions, BurnUnit, DiscBurnStatus};

    #[test]
    fn stages_whole_game_folder() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gog-conjure-stage-test-{nanos}"));
        let game = dir.join("My Game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("setup.exe"), b"x").unwrap();

        let disc = DiscLayout {
            index: 0,
            media: DiscMedia::Dvd5,
            volid: "MY_GAME".into(),
            volid_manual: false,
            units: vec![BurnUnit {
                game_id: 1,
                game_title: "My Game".into(),
                size_bytes: 1,
                files: vec![],
                part_index: 0,
                part_count: 1,
            }],
            used_bytes: 1,
            remaining_bytes: 0,
            status: DiscBurnStatus::Planned,
            last_error: None,
            options: BurnOptions::default(),
        };
        let staged = stage_disc_layout(&disc, &[(1, game.clone())]).unwrap();
        assert!(staged.root.join("My Game").join("setup.exe").is_file());
        drop(staged);
        let _ = fs::remove_dir_all(&dir);
    }
}

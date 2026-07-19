use std::path::{Path, PathBuf};

pub fn game_folder(download_root: &Path, title: &str) -> PathBuf {
    download_root.join(sanitize_filename::sanitize(title))
}

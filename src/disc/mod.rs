//! Disc planning and optical burning (Linux xorriso, Windows IMAPI2, macOS drutil).

mod burner;
mod history;
mod install;
mod media;
mod models;
mod pack;
mod stage;
mod volid;
mod xorriso;

// Always compiled so unit tests run on Linux CI; wired into create_burner only on macOS.
mod macos_drutil;
#[cfg(target_os = "windows")]
mod windows_imapi;

#[cfg(target_os = "windows")]
pub use windows_imapi::{run_list_drives_helper, LIST_DRIVES_FLAG};

pub use burner::{create_burner, BurnEvent, DiscBurner};
pub use history::{AvailableDownload, BurnHistory};
pub use install::{install_xorriso, PackageManager};
pub use media::DiscMedia;
pub use models::{
    BurnListEntry, BurnOptions, BurnPlan, DiscBurnStatus, DiscLayout, DownloadReadiness,
    OpticalDrive, SplitPolicy,
};
pub use pack::{
    folder_size, list_available_downloads, plan_homogeneous_discs, plan_into_discs,
    planned_files_from_download_files, resolve_disc_file_paths,
};
pub use volid::{sanitize_volid, VOLID_MAX_LEN};

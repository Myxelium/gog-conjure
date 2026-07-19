use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::models::{BurnOptions, DiscLayout, OpticalDrive};

#[cfg(target_os = "linux")]
use super::xorriso::XorrisoBurner;

#[derive(Debug, thiserror::Error)]
pub enum BurnError {
    #[error("burning is not available on this platform")]
    #[allow(dead_code)]
    UnsupportedPlatform,
    #[error("burn backend not found")]
    BackendMissing,
    #[error("no optical drive found")]
    NoDrive,
    #[error("{0}")]
    Other(String),
}

/// Progress / lifecycle events from a burn job.
#[derive(Debug, Clone)]
pub enum BurnEvent {
    Log(String),
    Progress { fraction: f32, message: String },
    Finished(Result<(), String>),
}

pub trait DiscBurner: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn unavailable_reason(&self) -> Option<String>;
    fn list_drives(&self) -> Result<Vec<OpticalDrive>, BurnError>;
    /// Build argv for inspection/tests / log preview.
    fn build_burn_command(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
    ) -> Result<Vec<String>, BurnError>;
    /// Run the burn asynchronously; emits [`BurnEvent`]s on `tx`.
    fn start_burn_job(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
        tx: mpsc::UnboundedSender<BurnEvent>,
        cancel: Arc<AtomicBool>,
    );
}

/// Platform burner: xorriso on Linux, IMAPI2 on Windows, drutil on macOS.
pub fn create_burner() -> Box<dyn DiscBurner> {
    #[cfg(target_os = "linux")]
    {
        Box::new(XorrisoBurner::detect())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(super::windows_imapi::WindowsBurner::detect())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(super::macos_drutil::MacosBurner::detect())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Box::new(UnsupportedBurner)
    }
}

#[derive(Debug, Default)]
#[cfg_attr(
    any(target_os = "linux", target_os = "windows", target_os = "macos"),
    allow(dead_code)
)]
pub struct UnsupportedBurner;

impl DiscBurner for UnsupportedBurner {
    fn name(&self) -> &str {
        "unsupported"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some("Disc burning is not supported on this platform.".into())
    }

    fn list_drives(&self) -> Result<Vec<OpticalDrive>, BurnError> {
        Err(BurnError::UnsupportedPlatform)
    }

    fn build_burn_command(
        &self,
        _disc: &DiscLayout,
        _options: &BurnOptions,
        _game_folders: &[(u64, PathBuf)],
    ) -> Result<Vec<String>, BurnError> {
        Err(BurnError::UnsupportedPlatform)
    }

    fn start_burn_job(
        &self,
        _disc: &DiscLayout,
        _options: &BurnOptions,
        _game_folders: &[(u64, PathBuf)],
        tx: mpsc::UnboundedSender<BurnEvent>,
        _cancel: Arc<AtomicBool>,
    ) {
        let _ = tx.send(BurnEvent::Finished(Err(
            "Disc burning is not supported on this platform.".into(),
        )));
    }
}

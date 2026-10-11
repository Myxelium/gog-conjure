use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::media::DiscMedia;
use crate::gog::format_bytes;

/// Filesystem written for one disc. Joliet matches the historical burn path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DiscFilesystem {
    #[default]
    Joliet,
    Udf,
}

impl DiscFilesystem {
    pub fn label(self) -> &'static str {
        match self {
            DiscFilesystem::Joliet => "Joliet",
            DiscFilesystem::Udf => "UDF",
        }
    }

    /// `SetFileSystemsToCreate` choice. Joliet is ISO 9660 + Joliet, not a hybrid with UDF.
    #[cfg(any(windows, test))]
    pub fn imapi_filesystems(self) -> ImapiFileSystems {
        match self {
            DiscFilesystem::Joliet => ImapiFileSystems::Iso9660AndJoliet,
            DiscFilesystem::Udf => ImapiFileSystems::Udf,
        }
    }
}

/// IMAPI filesystem set. Plain values so the choice can be tested without COM.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImapiFileSystems {
    /// `FsiFileSystemISO9660 | FsiFileSystemJoliet`
    Iso9660AndJoliet,
    /// `FsiFileSystemUDF`
    Udf,
}

/// Windows IMAPI rejects ISO 9660 / Joliet files at or above this size.
pub const JOLIET_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// `man xorriso`: it does not produce UDF filesystems, and 1.5.6 has no `-udf` command.
/// `-udf on` is reported as `Not a known command: '-udf'` and then `'on'`.
pub fn xorriso_udf_error() -> &'static str {
    "Cannot burn with UDF: xorriso does not produce UDF filesystems. Choose Joliet. On Linux, Joliet with ISO 9660 level 3 stores files larger than 2 GiB."
}

/// True when any planned file is too large for Joliet, so the disc must use UDF.
pub fn disc_needs_udf(disc: &DiscLayout) -> bool {
    // Planned sizes only. Statting every file here runs on each UI frame.
    disc.units.iter().any(|unit| {
        unit.files
            .iter()
            .any(|file| file.size_bytes >= JOLIET_MAX_FILE_BYTES)
    })
}

/// Error for a Joliet burn that contains a file ≥ 2 GiB. `None` when UDF is selected
/// or every file is under the limit.
pub fn joliet_file_limit_error(disc: &DiscLayout) -> Option<String> {
    if disc.options.filesystem != DiscFilesystem::Joliet {
        return None;
    }
    let file = disc
        .units
        .iter()
        .flat_map(|unit| &unit.files)
        .find(|file| effective_file_bytes(file) >= JOLIET_MAX_FILE_BYTES)?;
    let name = if file.relative_name.is_empty() {
        file.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
    } else {
        file.relative_name.clone()
    };
    Some(format!(
        "Cannot burn with Joliet: '{name}' is {} (≥ 2 GiB). Choose UDF.",
        format_bytes(effective_file_bytes(file))
    ))
}

fn effective_file_bytes(file: &BurnFile) -> u64 {
    let on_disk = std::fs::metadata(&file.path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .unwrap_or(0);
    file.size_bytes.max(on_disk)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SplitPolicy {
    /// Never split; oversized games block the plan.
    Never,
    /// Split only when the whole game exceeds media capacity.
    #[default]
    WhenOversized,
    /// Split into ordered installer pieces so packing can fill discs denser.
    AllowToPack,
}

impl SplitPolicy {
    pub fn all() -> &'static [SplitPolicy] {
        &[
            SplitPolicy::Never,
            SplitPolicy::WhenOversized,
            SplitPolicy::AllowToPack,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            SplitPolicy::Never => "Never split",
            SplitPolicy::WhenOversized => "Split if oversized",
            SplitPolicy::AllowToPack => "Allow split to pack denser",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadReadiness {
    Pending,
    Downloading,
    Ready,
    Failed,
}

impl DownloadReadiness {
    pub fn label(self) -> &'static str {
        match self {
            DownloadReadiness::Pending => "Pending",
            DownloadReadiness::Downloading => "Downloading…",
            DownloadReadiness::Ready => "Ready",
            DownloadReadiness::Failed => "Failed",
        }
    }
}

/// File size/name known from GOG (or prior queue) before the download finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub relative_name: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct BurnListEntry {
    pub game_id: u64,
    pub title: String,
    pub folder: PathBuf,
    pub size_bytes: u64,
    pub readiness: DownloadReadiness,
    pub split_override: Option<SplitPolicy>,
    /// Include when planning into discs.
    pub included: bool,
    /// Expected files for size-first planning (GOG API / queue selection).
    pub planned_files: Vec<PlannedFile>,
}

impl BurnListEntry {
    pub fn effective_split(&self, global: SplitPolicy) -> SplitPolicy {
        self.split_override.unwrap_or(global)
    }

    pub fn planned_size(&self) -> u64 {
        self.planned_files.iter().map(|f| f.size_bytes).sum()
    }
}

#[derive(Debug, Clone)]
pub struct BurnFile {
    pub path: PathBuf,
    /// Path relative to the game folder (ISO path under `/Title/...`).
    pub relative_name: String,
    pub size_bytes: u64,
}

/// Atomic placeable chunk for packing (whole game or ordered installer piece).
#[derive(Debug, Clone)]
pub struct BurnUnit {
    pub game_id: u64,
    pub game_title: String,
    pub size_bytes: u64,
    pub files: Vec<BurnFile>,
    /// 0-based part index within a split game.
    pub part_index: u32,
    pub part_count: u32,
}

impl BurnUnit {
    pub fn is_split(&self) -> bool {
        self.part_count > 1
    }

    pub fn summary_label(&self) -> String {
        if self.is_split() {
            format!(
                "{} (part {}/{})",
                self.game_title,
                self.part_index + 1,
                self.part_count
            )
        } else {
            self.game_title.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscBurnStatus {
    Empty,
    Planned,
    Burning,
    Done,
    Failed,
}

impl DiscBurnStatus {
    pub fn label(self) -> &'static str {
        match self {
            DiscBurnStatus::Empty => "Empty",
            DiscBurnStatus::Planned => "Ready to burn",
            DiscBurnStatus::Burning => "Burning…",
            DiscBurnStatus::Done => "Burned",
            DiscBurnStatus::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscLayout {
    /// 0-based disc index.
    pub index: usize,
    pub media: DiscMedia,
    pub volid: String,
    pub volid_manual: bool,
    pub units: Vec<BurnUnit>,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
    pub status: DiscBurnStatus,
    pub last_error: Option<String>,
    /// Per-disc burn settings (drive, speed, verify, …).
    pub options: BurnOptions,
}

impl DiscLayout {
    pub fn new_empty(index: usize, media: DiscMedia, options: BurnOptions) -> Self {
        let capacity = media.capacity_for_burn(options.defect_management);
        Self {
            index,
            media,
            volid: String::new(),
            volid_manual: false,
            units: Vec::new(),
            used_bytes: 0,
            remaining_bytes: capacity,
            status: DiscBurnStatus::Empty,
            last_error: None,
            options,
        }
    }

    /// Usable bytes for planning/burning, honoring BD defect-management spare.
    pub fn usable_capacity_bytes(&self) -> u64 {
        self.media.capacity_for_burn(self.options.defect_management)
    }

    pub fn used_label(&self) -> String {
        format!(
            "{} / {} ({} free)",
            format_bytes(self.used_bytes),
            format_bytes(self.usable_capacity_bytes()),
            format_bytes(self.remaining_bytes)
        )
    }

    pub fn fill_fraction(&self) -> f32 {
        let cap = self.usable_capacity_bytes().max(1) as f64;
        let frac = self.used_bytes as f64 / cap;
        if frac.is_finite() {
            (frac as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub fn game_titles(&self) -> Vec<String> {
        let mut titles = Vec::new();
        for unit in &self.units {
            if !titles.iter().any(|t| t == &unit.game_title) {
                titles.push(unit.game_title.clone());
            }
        }
        titles
    }

    /// 1-based part number when this disc holds a continuation of a single split game.
    pub fn split_part_suffix(&self) -> Option<u32> {
        if self.units.is_empty() {
            return None;
        }
        if self.units.len() == 1 && self.units[0].is_split() {
            Some(self.units[0].part_index + 1)
        } else if self
            .units
            .iter()
            .all(|u| u.game_id == self.units[0].game_id)
            && self.units.iter().any(|u| u.is_split())
        {
            self.units.first().map(|u| u.part_index + 1)
        } else {
            None
        }
    }

    pub fn recompute_usage(&mut self) {
        let capacity = self.usable_capacity_bytes();
        self.used_bytes = self.units.iter().map(|u| u.size_bytes).sum();
        self.remaining_bytes = capacity.saturating_sub(self.used_bytes);
        if self.units.is_empty()
            && !matches!(
                self.status,
                DiscBurnStatus::Burning | DiscBurnStatus::Done | DiscBurnStatus::Failed
            )
        {
            self.status = DiscBurnStatus::Empty;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BurnPlan {
    pub discs: Vec<DiscLayout>,
    /// Hard errors that disable burning affected content.
    pub blockers: Vec<String>,
    /// Soft notices (skipped / unassigned games).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnOptions {
    pub drive: String,
    /// `None` = auto / drive default.
    pub speed: Option<u32>,
    pub verify: bool,
    pub simulate: bool,
    pub blank: bool,
    pub eject: bool,
    /// BD-R/RE: format with Defect Management spare area (slower, less capacity).
    #[serde(default)]
    pub defect_management: bool,
    /// Missing on older saved options, which stay Joliet.
    #[serde(default)]
    pub filesystem: DiscFilesystem,
}

impl Default for BurnOptions {
    fn default() -> Self {
        Self {
            drive: String::new(),
            speed: None,
            verify: false,
            simulate: false,
            // Safer default for DVD+RW / BD-RE that still have old sessions.
            blank: true,
            eject: true,
            // Off by default so nearly-full BD plans keep full media capacity.
            defect_management: false,
            filesystem: DiscFilesystem::Joliet,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpticalDrive {
    pub path: String,
    pub vendor: String,
    pub model: String,
}

impl OpticalDrive {
    pub fn label(&self) -> String {
        let name = format!("{} {}", self.vendor, self.model).trim().to_string();
        if name.is_empty() {
            self.path.clone()
        } else if self.path.len() > 28 {
            // Unique IMAPI recorder ids are long; prefer the human-readable name.
            name
        } else {
            format!("{} — {}", self.path, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_burn_options_json_defaults_to_joliet() {
        let opts: BurnOptions = serde_json::from_str(
            r#"{"drive":"/dev/sr0","speed":null,"verify":false,"simulate":false,"blank":true,"eject":true}"#,
        )
        .unwrap();
        assert_eq!(opts.filesystem, DiscFilesystem::Joliet);
        assert!(!opts.defect_management);
        assert_eq!(opts.drive, "/dev/sr0");
    }

    #[test]
    fn windows_joliet_is_not_a_udf_hybrid() {
        assert_eq!(
            DiscFilesystem::Joliet.imapi_filesystems(),
            ImapiFileSystems::Iso9660AndJoliet
        );
        assert_eq!(
            DiscFilesystem::Udf.imapi_filesystems(),
            ImapiFileSystems::Udf
        );
    }

    #[test]
    fn joliet_rejects_file_at_two_gib_and_names_udf() {
        let disc = DiscLayout {
            index: 0,
            media: DiscMedia::Dvd5,
            volid: "X".into(),
            volid_manual: false,
            units: vec![BurnUnit {
                game_id: 1,
                game_title: "Big".into(),
                size_bytes: JOLIET_MAX_FILE_BYTES,
                files: vec![BurnFile {
                    path: PathBuf::from("missing-setup.bin"),
                    relative_name: "setup.bin".into(),
                    size_bytes: JOLIET_MAX_FILE_BYTES,
                }],
                part_index: 0,
                part_count: 1,
            }],
            used_bytes: JOLIET_MAX_FILE_BYTES,
            remaining_bytes: 0,
            status: DiscBurnStatus::Planned,
            last_error: None,
            options: BurnOptions::default(),
        };
        let err = joliet_file_limit_error(&disc).unwrap();
        assert!(err.contains("UDF"), "{err}");
        assert!(err.contains("setup.bin"), "{err}");
        assert!(disc_needs_udf(&disc));

        let mut udf = disc;
        udf.options.filesystem = DiscFilesystem::Udf;
        assert!(joliet_file_limit_error(&udf).is_none());
    }
}

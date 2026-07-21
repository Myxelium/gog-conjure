use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::burner::{BurnError, BurnEvent, DiscBurner};
use super::models::{BurnOptions, DiscLayout, OpticalDrive};
use super::pack::iso_path_for;
use super::volid::{sanitize_volid, VOLID_MAX_LEN};

#[derive(Debug, Clone)]
pub struct XorrisoBurner {
    binary: Option<PathBuf>,
    /// True when resolved from a bundled path (next to the app / `vendor/`).
    bundled: bool,
}

impl XorrisoBurner {
    pub fn detect() -> Self {
        match find_xorriso() {
            Some((binary, bundled)) => Self {
                binary: Some(binary),
                bundled,
            },
            None => Self {
                binary: None,
                bundled: false,
            },
        }
    }

    fn bin(&self) -> Result<&Path, BurnError> {
        self.binary
            .as_deref()
            .ok_or(BurnError::BackendMissing)
    }
}

impl DiscBurner for XorrisoBurner {
    fn name(&self) -> &str {
        if self.bundled {
            "xorriso (bundled)"
        } else if self.binary.is_some() {
            "xorriso (system)"
        } else {
            "xorriso"
        }
    }

    fn is_available(&self) -> bool {
        self.binary.is_some()
    }

    fn unavailable_reason(&self) -> Option<String> {
        if self.binary.is_none() {
            let hint = super::install::PackageManager::detect()
                .map(|m| format!("Click “Install xorriso” (runs `{}`),", m.short_command()))
                .unwrap_or_else(|| {
                    "Install the xorriso package with your distro’s package manager,".into()
                });
            Some(format!(
                "xorriso not found. {hint} or place a binary at vendor/xorriso next to the app."
            ))
        } else {
            None
        }
    }

    fn list_drives(&self) -> Result<Vec<OpticalDrive>, BurnError> {
        let bin = self.bin()?;
        let output = Command::new(bin)
            .arg("-devices")
            .output()
            .map_err(|e| BurnError::Other(format!("failed to run xorriso: {e}")))?;

        // xorriso returns non-zero when no drives; still parse stdout.
        let text = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{text}\n{stderr}");
        Ok(parse_devices(&combined))
    }

    fn build_burn_command(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
    ) -> Result<Vec<String>, BurnError> {
        let bin = self
            .binary
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "xorriso".into());
        Ok(build_argv(&bin, disc, options, game_folders)?)
    }

    fn start_burn_job(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
        tx: mpsc::UnboundedSender<BurnEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        match self.build_burn_command(disc, options, game_folders) {
            Ok(argv) => start_burn(argv, tx, cancel),
            Err(err) => {
                let _ = tx.send(BurnEvent::Finished(Err(err.to_string())));
            }
        }
    }
}

/// Resolve `xorriso`: bundled locations first, then `PATH`.
///
/// Search order:
/// 1. `{exe_dir}/vendor/xorriso`
/// 2. `{exe_dir}/xorriso`
/// 3. `./vendor/xorriso` (current working directory)
/// 4. `./xorriso`
/// 5. first `xorriso` on `PATH`
///
/// Returns `(path, bundled)` where `bundled` is true for (1)–(4).
pub fn find_xorriso() -> Option<(PathBuf, bool)> {
    let mut bundled_candidates = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            bundled_candidates.push(dir.join("vendor").join("xorriso"));
            bundled_candidates.push(dir.join("xorriso"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        bundled_candidates.push(cwd.join("vendor").join("xorriso"));
        bundled_candidates.push(cwd.join("xorriso"));
    }

    for candidate in bundled_candidates {
        if is_usable_binary(&candidate) {
            return Some((candidate, true));
        }
    }

    which("xorriso").map(|p| (p, false))
}

fn is_usable_binary(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_usable_binary(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Parse `xorriso -devices` output.
pub fn parse_devices(text: &str) -> Vec<OpticalDrive> {
    let mut drives = Vec::new();
    for line in text.lines() {
        // Example: 0  -dev '/dev/sr0' rwrw-- :  'ASUS' 'BW-16D1HT'
        let trimmed = line.trim();
        if !trimmed.contains("-dev") {
            continue;
        }
        let Some(path) = extract_quoted_after(trimmed, "-dev") else {
            continue;
        };
        let mut vendor = String::new();
        let mut model = String::new();
        if let Some(rest) = trimmed.split(':').nth(1) {
            let quotes: Vec<&str> = rest
                .split('\'')
                .enumerate()
                .filter_map(|(i, s)| if i % 2 == 1 { Some(s.trim()) } else { None })
                .collect();
            if !quotes.is_empty() {
                vendor = quotes[0].to_string();
            }
            if quotes.len() > 1 {
                model = quotes[1].to_string();
            }
        }
        drives.push(OpticalDrive { path, vendor, model });
    }
    drives
}

fn extract_quoted_after(line: &str, marker: &str) -> Option<String> {
    let idx = line.find(marker)?;
    let after = &line[idx + marker.len()..];
    let after = after.trim_start();
    if let Some(rest) = after.strip_prefix('\'') {
        let end = rest.find('\'')?;
        return Some(rest[..end].to_string());
    }
    after
        .split_whitespace()
        .next()
        .map(|s| s.trim_matches('\'').to_string())
}

pub fn build_argv(
    binary: &str,
    disc: &DiscLayout,
    options: &BurnOptions,
    game_folders: &[(u64, PathBuf)],
) -> Result<Vec<String>, BurnError> {
    // Simulate writes a temp ISO instead of using MMC -dummy (many DVD/BD drives
    // cannot simulate and fail with "libburn indicates failure with writing").
    let outdev = if options.simulate {
        let path = std::env::temp_dir().join(format!(
            "gog-conjure-simulate-disc{:02}.iso",
            disc.index + 1
        ));
        format!("stdio:{}", path.display())
    } else {
        if options.drive.trim().is_empty() {
            return Err(BurnError::NoDrive);
        }
        options.drive.clone()
    };

    // Include GOG and synthetic local ids; only the invalid 0 sentinel is skipped.
    let folder_by_id: HashMap<u64, &PathBuf> = game_folders
        .iter()
        .filter(|(id, _)| *id != 0)
        .map(|(id, p)| (*id, p))
        .collect();
    let volid = {
        let s = sanitize_volid(&disc.volid);
        if s.is_empty() {
            format!("GOG_DISC_{:02}", disc.index + 1)
        } else {
            s.chars().take(VOLID_MAX_LEN).collect()
        }
    };

    let mut args = vec![
        binary.to_string(),
        "-outdev".into(),
        outdev,
        "-volid".into(),
        volid,
        "-joliet".into(),
        "on".into(),
        "-rockridge".into(),
        "on".into(),
        "-charset".into(),
        "UTF-8".into(),
        "-compliance".into(),
        "iso_9660_level=3".into(),
    ];

    // MD5 tags enable post-burn verify via -check_media.
    if options.verify {
        args.extend(["-md5".into(), "on".into()]);
    }

    // Blank only applies to real optical media.
    if options.blank && !options.simulate {
        args.extend(["-blank".into(), "as_needed".into()]);
    }
    if let Some(speed) = options.speed {
        if !options.simulate {
            args.extend(["-speed".into(), speed.to_string()]);
        }
    }
    // BD-R: pre-format before writing (helps OPC on some drives; matches K3B/growisofs).
    if !options.simulate && disc.media.is_bluray() {
        let mode = if options.defect_management {
            "as_needed"
        } else {
            "without_spare"
        };
        args.extend(["-format".into(), mode.into()]);
    }

    // Map files onto the ISO. Whole-game (unsplit) units map the folder once.
    let mut mapped_folders = std::collections::HashSet::new();
    for unit in &disc.units {
        // Prefer exact game_id; fall back to folder-name match when ids don't line up.
        let folder = folder_by_id
            .get(&unit.game_id)
            .copied()
            .or_else(|| find_folder_by_title(game_folders, &unit.game_title));
        let Some(folder) = folder else {
            return Err(BurnError::Other(format!(
                "missing folder for game '{}'",
                unit.game_title
            )));
        };
        if !folder.is_dir() {
            return Err(BurnError::Other(format!(
                "folder not found for '{}': {}",
                unit.game_title,
                folder.display()
            )));
        }

        if unit.part_count == 1 && unit.files.is_empty() {
            let iso = format!("/{}", sanitize_filename::sanitize(&unit.game_title));
            args.push("-map".into());
            args.push(folder.display().to_string());
            args.push(iso);
            continue;
        }

        if unit.part_count == 1 {
            let key = unit.game_id;
            if mapped_folders.insert(key) {
                let iso = format!("/{}", sanitize_filename::sanitize(&unit.game_title));
                args.push("-map".into());
                args.push(folder.display().to_string());
                args.push(iso);
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
            args.push("-map".into());
            args.push(file.path.display().to_string());
            args.push(iso);
        }
    }

    args.push("-commit".into());

    // Verify after write (works for real media and simulate ISO). Trailing `--` is required.
    if options.verify {
        args.extend(["-check_media".into(), "--".into()]);
    }

    // Eject only after a real optical burn (not simulate-to-file).
    if options.eject && !options.simulate {
        args.extend(["-eject".into(), "on".into()]);
    }

    Ok(args)
}

/// Run a burn asynchronously; emits [`BurnEvent`]s on `tx`.
pub fn start_burn(
    argv: Vec<String>,
    tx: mpsc::UnboundedSender<BurnEvent>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        if argv.is_empty() {
            let _ = tx.send(BurnEvent::Finished(Err("empty burn command".into())));
            return;
        }
        let (prog, args) = argv.split_first().unwrap();
        let _ = tx.send(BurnEvent::Log(format!("Running: {}", argv.join(" "))));

        let mut child = match Command::new(prog)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(BurnEvent::Finished(Err(format!("spawn failed: {e}"))));
                return;
            }
        };

        let stderr = child.stderr.take();
        let stdout = child.stdout.take();
        let problems: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let write_ok: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));

        let tx2 = tx.clone();
        let cancel2 = cancel.clone();
        let problems2 = problems.clone();
        let write_ok2 = write_ok.clone();
        let reader_thread = std::thread::spawn(move || {
            let handle_line = |line: String| {
                if line.to_lowercase().contains("completed successfully") {
                    write_ok2.store(true, Ordering::Relaxed);
                }
                if let Some(msg) = extract_xorriso_problem(&line) {
                    if let Ok(mut p) = problems2.lock() {
                        p.push(msg);
                    }
                }
                if let Some((fraction, message)) = parse_progress_update(&line) {
                    let _ = tx2.send(BurnEvent::Progress { fraction, message });
                }
                let _ = tx2.send(BurnEvent::Log(line));
            };

            if let Some(err) = stderr {
                let reader = BufReader::new(err);
                for line in reader.lines().flatten() {
                    if cancel2.load(Ordering::Relaxed) {
                        break;
                    }
                    handle_line(line);
                }
            }
            if let Some(out) = stdout {
                let reader = BufReader::new(out);
                for line in reader.lines().flatten() {
                    handle_line(line);
                }
            }
        });

        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = reader_thread.join();
                let _ = tx.send(BurnEvent::Finished(Err("burn cancelled".into())));
                return;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let _ = reader_thread.join();
                    if status.success() {
                        let _ = tx.send(BurnEvent::Finished(Ok(())));
                    } else {
                        let detail = problems
                            .lock()
                            .ok()
                            .map(|p| p.last().cloned().unwrap_or_default())
                            .unwrap_or_default();
                        // Write+eject can succeed, then a bad verify option fails the process.
                        // Don't report that as a total burn failure.
                        if write_ok.load(Ordering::Relaxed)
                            && is_post_write_only_failure(&detail)
                        {
                            let _ = tx.send(BurnEvent::Finished(Ok(())));
                            let _ = tx.send(BurnEvent::Log(format!(
                                "NOTE: disc write succeeded; post-write step failed: {detail}"
                            )));
                        } else {
                            let hint = hint_for_xorriso_failure(&detail);
                            let msg = if detail.is_empty() {
                                format!("xorriso failed ({status}).{hint}")
                            } else {
                                format!("{detail} ({status}).{hint}")
                            };
                            let _ = tx.send(BurnEvent::Finished(Err(msg)));
                        }
                    }
                    return;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(e) => {
                    let _ = reader_thread.join();
                    let _ = tx.send(BurnEvent::Finished(Err(format!("wait failed: {e}"))));
                    return;
                }
            }
        }
    });
}

fn is_post_write_only_failure(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("check_media")
        || lower.contains("unknown option")
        || lower.contains("md5")
        || lower.contains("eject")
}

fn extract_xorriso_problem(line: &str) -> Option<String> {
    for marker in [": FAILURE : ", ": FATAL : ", ": SORRY : "] {
        if let Some(idx) = line.find(marker) {
            let msg = line[idx + marker.len()..].trim();
            if !msg.is_empty() {
                return Some(msg.to_string());
            }
        }
    }
    None
}

fn hint_for_xorriso_failure(detail: &str) -> String {
    let lower = detail.to_lowercase();
    if lower.contains("acquire drive") || lower.contains("permission") {
        return " Check that no other program is using the drive, and that your user can access it (often membership in the `cdrom` group)."
            .into();
    }
    if lower.contains("not blank")
        || lower.contains("already contains")
        || lower.contains("closed")
        || lower.contains("no suitable")
    {
        return " Try enabling “Blank RW” for rewriteable media, or insert a blank disc.".into();
    }
    if lower.contains("no such file") || lower.contains("cannot determine attributes") {
        return " A source file/folder is missing — refresh downloads and Plan again.".into();
    }
    if lower.contains("power calibration") || lower.contains("73 03") {
        return " The drive rejected laser calibration on this disc — try a fresh blank and a different BD-R brand if it persists.".into();
    }
    if lower.contains("exceeds free space") {
        return " The image is larger than usable space on this disc — Plan again (BD capacities leave headroom), and use a fresh blank (already-formatted BD-R cannot regain spare area).".into();
    }
    if lower.contains("invalid command operation code")
        || lower.contains("cannot reserve track")
    {
        return " The drive rejected a write command — confirm it can write BD-R (not read-only), then try a fresh blank disc.".into();
    }
    if detail.is_empty() {
        return " See Burn progress log for details.".into();
    }
    String::new()
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

fn parse_progress_fraction(line: &str) -> Option<f32> {
    // xorriso often prints percentages like "40.2%" or "Done: 40.2%"
    let idx = line.find('%')?;
    let start = line[..idx]
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let num: f32 = line[start..idx].trim().parse().ok()?;
    Some((num / 100.0).clamp(0.0, 1.0))
}

/// Map xorriso log lines to a progress fraction + short status for the UI.
fn parse_progress_update(line: &str) -> Option<(f32, String)> {
    let lower = line.to_lowercase();
    if let Some(frac) = parse_progress_fraction(line) {
        // Writing percents usually sit in the mid/late phase.
        let adjusted = if lower.contains("write") || lower.contains("done") {
            0.15 + frac * 0.75
        } else {
            frac
        };
        return Some((adjusted.clamp(0.0, 0.99), tidy_progress_message(line)));
    }
    if lower.contains("blank") && (lower.contains("as_needed") || lower.contains("blanking")) {
        return Some((0.05, "Blanking media…".into()));
    }
    if lower.contains("files added") || lower.contains("added to iso") {
        return Some((0.12, "Building ISO image…".into()));
    }
    if lower.contains("writing to") && lower.contains("completed successfully") {
        return Some((0.88, "Write finished — finalizing…".into()));
    }
    if lower.contains("check_media") || lower.contains("md5") || lower.contains("media checks") {
        return Some((0.93, "Verifying media…".into()));
    }
    if lower.contains("eject") {
        return Some((0.98, "Ejecting…".into()));
    }
    if lower.contains("written to medium") {
        return Some((0.55, "Writing to disc…".into()));
    }
    None
}

fn tidy_progress_message(line: &str) -> String {
    let trimmed = line.trim();
    if let Some(idx) = trimmed.find("UPDATE : ") {
        return trimmed[idx + "UPDATE : ".len()..].trim().to_string();
    }
    if trimmed.len() > 120 {
        format!("{}…", &trimmed[..117])
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::media::DiscMedia;
    use crate::disc::models::{BurnOptions, BurnUnit, DiscBurnStatus};

    #[cfg(unix)]
    #[test]
    fn accepts_executable_vendor_binary() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gog-conjure-xorriso-{nanos}"));
        let vendor = dir.join("vendor");
        fs::create_dir_all(&vendor).unwrap();
        let fake = vendor.join("xorriso");
        fs::write(&fake, b"#!/bin/true\n").unwrap();
        let mut perms = fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake, perms).unwrap();

        assert!(is_usable_binary(&fake));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_device_line() {
        let text = "0  -dev '/dev/sr0' rwrw-- :  'ASUS' 'BW-16D1HT'\n";
        let drives = parse_devices(text);
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].path, "/dev/sr0");
        assert_eq!(drives[0].vendor, "ASUS");
        assert_eq!(drives[0].model, "BW-16D1HT");
    }

    #[test]
    fn argv_includes_options_and_maps() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gog-conjure-burn-argv-{nanos}"));
        let game_dir = dir.join("My Game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("setup.exe"), b"x").unwrap();

        let disc = DiscLayout {
            index: 0,
            media: DiscMedia::Dvd5,
            volid: "My Game!".into(),
            volid_manual: false,
            units: vec![BurnUnit {
                game_id: 1,
                game_title: "My Game".into(),
                size_bytes: 100,
                files: vec![],
                part_index: 0,
                part_count: 1,
            }],
            used_bytes: 100,
            remaining_bytes: 0,
            status: DiscBurnStatus::Planned,
            last_error: None,
            options: BurnOptions::default(),
        };
        // Real burn options
        let opts = BurnOptions {
            drive: "/dev/sr0".into(),
            speed: Some(4),
            verify: true,
            simulate: false,
            blank: true,
            eject: true,
            defect_management: false,
        };
        let folders = vec![(1u64, game_dir.clone())];
        let argv = build_argv("xorriso", &disc, &opts, &folders).unwrap();
        assert!(argv.contains(&"-outdev".into()));
        assert!(argv.contains(&"/dev/sr0".into()));
        assert!(argv.contains(&"-speed".into()));
        assert!(argv.contains(&"4".into()));
        assert!(argv.contains(&"-blank".into()));
        assert!(argv.contains(&"-eject".into()));
        assert!(argv.contains(&"-rockridge".into()));
        assert!(argv.contains(&"-md5".into()));
        assert!(!argv.iter().any(|a| a == "-write_type"));
        assert!(!argv.iter().any(|a| a == "-stream_recording"));
        assert!(!argv.iter().any(|a| a == "-format")); // DVD, not BD
        assert!(argv.windows(2).any(|w| w[0] == "-check_media" && w[1] == "--"));
        // Regression: old builds passed the invalid token "default".
        assert!(!argv.iter().any(|a| a == "default"));
        assert!(argv.contains(&"MY_GAME".into()));
        assert!(argv.windows(3).any(|w| {
            w[0] == "-map" && w[1] == game_dir.display().to_string() && w[2].contains("My Game")
        }));

        let mut bd_disc = disc.clone();
        bd_disc.media = DiscMedia::Bd50;
        let argv_bd = build_argv("xorriso", &bd_disc, &opts, &folders).unwrap();
        assert!(!argv_bd.iter().any(|a| a == "-write_type"));
        assert!(!argv_bd.iter().any(|a| a == "-stream_recording"));
        assert!(argv_bd
            .windows(2)
            .any(|w| w[0] == "-format" && w[1] == "without_spare"));

        let mut opts_spare = opts.clone();
        opts_spare.defect_management = true;
        let argv_spare = build_argv("xorriso", &bd_disc, &opts_spare, &folders).unwrap();
        assert!(argv_spare
            .windows(2)
            .any(|w| w[0] == "-format" && w[1] == "as_needed"));

        // Simulate must not touch the optical drive (avoids MMC dummy failures).
        let sim = BurnOptions {
            drive: "/dev/sr0".into(),
            speed: Some(4),
            verify: true,
            simulate: true,
            blank: true,
            eject: true,
            defect_management: false,
        };
        let argv_sim = build_argv("xorriso", &disc, &sim, &folders).unwrap();
        assert!(argv_sim.iter().any(|a| a.starts_with("stdio:")));
        assert!(!argv_sim.iter().any(|a| a == "/dev/sr0"));
        assert!(!argv_sim.iter().any(|a| a == "-dummy"));
        assert!(!argv_sim.iter().any(|a| a == "-eject"));
        assert!(!argv_sim.iter().any(|a| a == "-write_type"));
        assert!(!argv_sim.iter().any(|a| a == "-format"));
        assert!(!argv_sim.iter().any(|a| a == "default"));
        assert!(argv_sim.windows(2).any(|w| w[0] == "-check_media" && w[1] == "--"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_write_verify_glitch_is_not_fatal() {
        assert!(is_post_write_only_failure(
            "-check_media: Unknown option 'default'"
        ));
        assert!(!is_post_write_only_failure(
            "libburn indicates failure with writing"
        ));
    }

    #[test]
    fn extracts_failure_message() {
        let line = "xorriso : FAILURE : Cannot acquire drive '/dev/sr0'";
        assert_eq!(
            extract_xorriso_problem(line).as_deref(),
            Some("Cannot acquire drive '/dev/sr0'")
        );
    }
}

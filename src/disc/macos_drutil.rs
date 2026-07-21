//! macOS optical burning via built-in `drutil` (DiscRecording).
//!
//! Stages a directory layout on disk, then burns it with ISO9660 + Joliet
//! (no intermediate ISO file). Simulate uses `drutil burn -test`.
//!
//! Docs: `man drutil` / https://keith.github.io/xcode-man-pages/drutil.1.html

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::burner::{BurnError, BurnEvent, DiscBurner};
use super::models::{BurnOptions, DiscLayout, OpticalDrive};
use super::stage::stage_disc_layout;
use super::volid::{sanitize_volid, VOLID_MAX_LEN};

const DRUTIL: &str = "/usr/bin/drutil";

#[derive(Debug, Clone)]
pub struct MacosBurner {
    binary: Option<PathBuf>,
}

impl MacosBurner {
    pub fn detect() -> Self {
        let binary = find_drutil();
        Self { binary }
    }

    fn bin(&self) -> Result<&Path, BurnError> {
        self.binary
            .as_deref()
            .ok_or(BurnError::BackendMissing)
    }
}

impl DiscBurner for MacosBurner {
    fn name(&self) -> &str {
        "drutil (macOS)"
    }

    fn is_available(&self) -> bool {
        self.binary.is_some()
    }

    fn unavailable_reason(&self) -> Option<String> {
        if self.binary.is_none() {
            Some(
                "drutil not found. Optical burning requires Apple’s DiscRecording tools \
                 (/usr/bin/drutil), which ship with macOS."
                    .into(),
            )
        } else {
            None
        }
    }

    fn list_drives(&self) -> Result<Vec<OpticalDrive>, BurnError> {
        let bin = self.bin()?;
        let output = Command::new(bin)
            .arg("list")
            .output()
            .map_err(|e| BurnError::Other(format!("failed to run drutil: {e}")))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(parse_drutil_list(&format!("{text}\n{stderr}")))
    }

    fn build_burn_command(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        _game_folders: &[(u64, PathBuf)],
    ) -> Result<Vec<String>, BurnError> {
        let bin = self
            .binary
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "drutil".into());
        Ok(build_argv(&bin, disc, options, Path::new("<staged>")))
    }

    fn start_burn_job(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
        tx: mpsc::UnboundedSender<BurnEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let bin = match self.bin() {
            Ok(b) => b.to_path_buf(),
            Err(err) => {
                let _ = tx.send(BurnEvent::Finished(Err(err.to_string())));
                return;
            }
        };
        let disc = disc.clone();
        let options = options.clone();
        let game_folders = game_folders.to_vec();
        std::thread::spawn(move || {
            let result = run_burn_job(&bin, &disc, &options, &game_folders, &tx, &cancel);
            match result {
                Ok(()) => {
                    let _ = tx.send(BurnEvent::Finished(Ok(())));
                }
                Err(err) => {
                    let _ = tx.send(BurnEvent::Finished(Err(err)));
                }
            }
        });
    }
}

fn find_drutil() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(DRUTIL),
        PathBuf::from("/usr/sbin/drutil"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    which("drutil")
}

fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn run_burn_job(
    bin: &Path,
    disc: &DiscLayout,
    options: &BurnOptions,
    game_folders: &[(u64, PathBuf)],
    tx: &mpsc::UnboundedSender<BurnEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("burn cancelled".into());
    }
    if !options.simulate && options.drive.trim().is_empty() {
        return Err("No optical drive selected.".into());
    }

    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.08,
        message: "Staging disc layout…".into(),
    });
    let _ = tx.send(BurnEvent::Log("Staging disc layout on disk…".into()));
    let staged = stage_disc_layout(disc, game_folders).map_err(|e| e.to_string())?;

    let argv = build_argv(
        &bin.display().to_string(),
        disc,
        options,
        &staged.root,
    );
    let _ = tx.send(BurnEvent::Log(format!("Running: {}", argv.join(" "))));
    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.20,
        message: if options.simulate {
            "Simulating burn…".into()
        } else {
            "Writing to disc…".into()
        },
    });

    let (prog, args) = argv.split_first().unwrap();
    let mut child = Command::new(prog)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn drutil failed: {e}"))?;

    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let tx2 = tx.clone();
    let cancel2 = cancel.clone();
    let reader = std::thread::spawn(move || {
        let handle = |line: String| {
            if let Some((frac, msg)) = parse_drutil_progress(&line) {
                let _ = tx2.send(BurnEvent::Progress {
                    fraction: frac,
                    message: msg,
                });
            }
            let _ = tx2.send(BurnEvent::Log(line));
        };
        if let Some(err) = stderr {
            for line in BufReader::new(err).lines().flatten() {
                if cancel2.load(Ordering::Relaxed) {
                    break;
                }
                handle(line);
            }
        }
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().flatten() {
                handle(line);
            }
        }
    });

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = reader.join();
            return Err("burn cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = reader.join();
                // Drop staging after burn.
                drop(staged);
                if status.success() {
                    let _ = tx.send(BurnEvent::Progress {
                        fraction: 1.0,
                        message: "Burn complete".into(),
                    });
                    return Ok(());
                }
                return Err(format!("drutil failed ({status})"));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => {
                let _ = reader.join();
                return Err(format!("wait failed: {e}"));
            }
        }
    }
}

fn disc_volid(disc: &DiscLayout) -> String {
    let s = sanitize_volid(&disc.volid);
    if s.is_empty() {
        format!("GOG_DISC_{:02}", disc.index + 1)
    } else {
        s.chars().take(VOLID_MAX_LEN).collect()
    }
}

pub fn build_argv(
    binary: &str,
    disc: &DiscLayout,
    options: &BurnOptions,
    stage_path: &Path,
) -> Vec<String> {
    let mut args = vec![binary.to_string()];

    // Drive selection must appear before the verb (see drutil(1)).
    if !options.drive.trim().is_empty() {
        args.extend(["-drive".into(), options.drive.clone()]);
    }

    args.push("burn".into());

    // Filesystem: ISO9660 + Joliet (close to Linux xorriso joliet+iso9660).
    args.extend([
        "-iso9660".into(),
        "-joliet".into(),
        "-nohfsplus".into(),
        "-noudf".into(),
    ]);

    if options.simulate {
        args.push("-test".into());
    }
    if options.blank && !options.simulate {
        args.push("-erase".into());
    }
    if options.verify {
        args.push("-verify".into());
    } else {
        args.push("-noverify".into());
    }
    if options.eject && !options.simulate {
        args.push("-eject".into());
    }
    if let Some(speed) = options.speed {
        if !options.simulate {
            args.extend(["-speed".into(), speed.to_string()]);
        }
    }

    // Optional disc title (supported by DiscRecording / older drutil docs).
    let volid = disc_volid(disc);
    args.extend(["-disctitle".into(), volid]);

    args.push(stage_path.display().to_string());
    args
}

/// Parse `drutil list` into drives. Paths are 1-based indexes for `-drive N`.
pub fn parse_drutil_list(text: &str) -> Vec<OpticalDrive> {
    let mut drives = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if lower.contains("vendor") && lower.contains("product") {
            continue;
        }
        if lower.starts_with("drutil") || lower.starts_with("usage") {
            continue;
        }
        // Typical: "MATSHITA DVD-R   UJ-85J  HA09"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        // Heuristic: vendor is first token; rest is model (drop trailing rev if short).
        let vendor = parts[0].to_string();
        let model = parts[1..].join(" ");
        let index = drives.len() + 1;
        drives.push(OpticalDrive {
            path: index.to_string(),
            vendor,
            model,
        });
    }
    drives
}

fn parse_drutil_progress(line: &str) -> Option<(f32, String)> {
    let lower = line.to_lowercase();
    if let Some(idx) = line.find('%') {
        let start = line[..idx]
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        if let Ok(num) = line[start..idx].trim().parse::<f32>() {
            let frac = (num / 100.0).clamp(0.0, 1.0);
            return Some((0.20 + frac * 0.75, tidy_line(line)));
        }
    }
    if lower.contains("eras") {
        return Some((0.15, "Blanking media…".into()));
    }
    if lower.contains("verif") {
        return Some((0.92, "Verifying media…".into()));
    }
    if lower.contains("eject") {
        return Some((0.98, "Ejecting…".into()));
    }
    if lower.contains("burn") || lower.contains("writ") {
        return Some((0.55, "Writing to disc…".into()));
    }
    None
}

fn tidy_line(line: &str) -> String {
    let t = line.trim();
    if t.len() > 120 {
        format!("{}…", &t[..117])
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::media::DiscMedia;
    use crate::disc::models::{BurnOptions, BurnUnit, DiscBurnStatus};

    #[test]
    fn parse_list_lines() {
        let text = "\
 Vendor Product           Rev
 MATSHITA DVD-R   UJ-85J  HA09
 ASUS      BW-16D1HT       1.01
";
        let drives = parse_drutil_list(text);
        assert_eq!(drives.len(), 2);
        assert_eq!(drives[0].path, "1");
        assert_eq!(drives[0].vendor, "MATSHITA");
        assert_eq!(drives[1].path, "2");
        assert!(drives[1].model.contains("BW-16D1HT"));
    }

    #[test]
    fn argv_maps_options() {
        let disc = DiscLayout {
            index: 0,
            media: DiscMedia::Dvd5,
            volid: "My Game".into(),
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
        let opts = BurnOptions {
            drive: "1".into(),
            speed: Some(4),
            verify: true,
            simulate: false,
            blank: true,
            eject: true,
            defect_management: false,
        };
        let argv = build_argv("drutil", &disc, &opts, Path::new("/tmp/stage"));
        assert!(argv.windows(2).any(|w| w[0] == "-drive" && w[1] == "1"));
        assert!(argv.contains(&"burn".into()));
        assert!(argv.contains(&"-iso9660".into()));
        assert!(argv.contains(&"-joliet".into()));
        assert!(argv.contains(&"-erase".into()));
        assert!(argv.contains(&"-verify".into()));
        assert!(argv.contains(&"-eject".into()));
        assert!(argv.windows(2).any(|w| w[0] == "-speed" && w[1] == "4"));
        assert!(argv.windows(2).any(|w| w[0] == "-disctitle" && w[1] == "MY_GAME"));
        assert!(argv.iter().any(|a| a.ends_with("stage")));

        let sim = BurnOptions {
            simulate: true,
            ..opts
        };
        let argv_sim = build_argv("drutil", &disc, &sim, Path::new("/tmp/stage"));
        assert!(argv_sim.contains(&"-test".into()));
        assert!(!argv_sim.contains(&"-eject".into()));
    }
}

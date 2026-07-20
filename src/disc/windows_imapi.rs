//! Windows optical burning via IMAPI2 (built into Windows).
//!
//! Flow (disk-backed, not RAM-buffered):
//! 1. Stage ISO layout on disk (hardlinks/copies)
//! 2. Stream an ISO file via [`IFileSystemImage::CreateResultImage`] + chunked `IStream` reads
//! 3. Burn that ISO with [`IDiscFormat2Data::Write`]
//!
//! Drive enumeration follows Microsoft's *Checking Drive Support* sample:
//! <https://learn.microsoft.com/en-us/windows/win32/imapi/checking-drive-support>
//! Burn flow: <https://learn.microsoft.com/en-us/windows/win32/imapi/burning-a-disc>
//! Overview: <https://learn.microsoft.com/en-us/windows/win32/imapi/using-imapi>

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use windows::core::{BSTR, Interface, PCWSTR};
use windows::Win32::Foundation::VARIANT_BOOL;
use windows::Win32::Storage::Imapi::{
    IBurnVerification, IDiscFormat2Data, IDiscFormat2Erase, IDiscMaster2, IDiscRecorder2,
    IFileSystemImage, IFsiDirectoryItem, FsiFileSystemISO9660, FsiFileSystemJoliet, FsiFileSystems,
    IMAPI_BURN_VERIFICATION_FULL, IMAPI_BURN_VERIFICATION_NONE, IMAPI_MEDIA_PHYSICAL_TYPE,
    IMAPI_MEDIA_TYPE_BDR, IMAPI_MEDIA_TYPE_DVDPLUSR, IMAPI_MEDIA_TYPE_DVDPLUSR_DUALLAYER,
    MsftDiscFormat2Data, MsftDiscFormat2Erase, MsftDiscMaster2, MsftDiscRecorder2,
    MsftFileSystemImage,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IStream, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ, STGM_SHARE_DENY_WRITE, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateStreamOnFileW;

/// CLI flag for the isolated drive-enumeration helper process.
pub const LIST_DRIVES_FLAG: &str = "--list-optical-drives";
/// CLI flag for the isolated IMAPI burn helper process.
pub const BURN_JOB_FLAG: &str = "--imapi-burn-job";

use super::burner::{BurnError, BurnEvent, DiscBurner};
use super::media::DiscMedia;
use super::models::{BurnOptions, DiscLayout, OpticalDrive};
use super::stage::{simulate_iso_path, stage_disc_layout};
use super::volid::{sanitize_volid, VOLID_MAX_LEN};

#[derive(Debug, Serialize, Deserialize)]
struct BurnJobFile {
    stage_root: PathBuf,
    iso_path: PathBuf,
    options: BurnOptions,
    media: DiscMedia,
    volid: String,
    disc_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum HelperEvent {
    Progress { fraction: f32, message: String },
    Log { line: String },
    Finished { ok: bool, #[serde(default)] error: Option<String> },
}

const CLIENT_NAME: &str = "gog-conjure";
const STREAM_CHUNK: usize = 1024 * 1024; // 1 MiB — keeps peak RAM bounded
const VARIANT_FALSE: VARIANT_BOOL = VARIANT_BOOL(0);
const VARIANT_TRUE: VARIANT_BOOL = VARIANT_BOOL(-1);

/// `windows` 0.61 COM interfaces are `!Send`; IMAPI write cancellation is safe across
/// threads for a single writer, so mark this wrapper explicitly.
struct SendDiscFormat2Data(IDiscFormat2Data);
// SAFETY: CancelWrite is documented as callable while Write runs on another thread.
unsafe impl Send for SendDiscFormat2Data {}
impl SendDiscFormat2Data {
    unsafe fn cancel_write(&self) {
        let _ = self.0.CancelWrite();
    }
}

#[derive(Debug, Default)]
pub struct WindowsBurner;

impl WindowsBurner {
    pub fn detect() -> Self {
        Self
    }
}

impl DiscBurner for WindowsBurner {
    fn name(&self) -> &str {
        "IMAPI2 (Windows)"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    fn list_drives(&self) -> Result<Vec<OpticalDrive>, BurnError> {
        // IMAPI can hard-crash (AV) inside imapi2.dll. Run enumeration in a child process
        // so a bad drive stack cannot tear down the egui UI.
        list_drives_subprocess()
    }

    fn build_burn_command(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        _game_folders: &[(u64, PathBuf)],
    ) -> Result<Vec<String>, BurnError> {
        let volid = disc_volid(disc);
        let iso = if options.simulate {
            simulate_iso_path(disc.index)
        } else {
            PathBuf::from(format!("%TEMP%\\gog-conjure-disc{:02}.iso", disc.index + 1))
        };
        let mut argv = vec![
            "imapi2".into(),
            "stage".into(),
            "build-iso".into(),
            iso.display().to_string(),
            "-volid".into(),
            volid,
        ];
        if options.simulate {
            argv.push("-simulate".into());
        } else {
            if options.drive.trim().is_empty() {
                return Err(BurnError::NoDrive);
            }
            argv.extend(["burn".into(), options.drive.clone()]);
            if options.blank {
                argv.push("-blank".into());
            }
            if options.verify {
                argv.push("-verify".into());
            }
            if options.eject {
                argv.push("-eject".into());
            }
            if let Some(speed) = options.speed {
                argv.extend(["-speed".into(), speed.to_string()]);
            }
        }
        Ok(argv)
    }

    fn start_burn_job(
        &self,
        disc: &DiscLayout,
        options: &BurnOptions,
        game_folders: &[(u64, PathBuf)],
        tx: mpsc::UnboundedSender<BurnEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let disc = disc.clone();
        let options = options.clone();
        let game_folders = game_folders.to_vec();
        std::thread::spawn(move || {
            let result = run_burn_job(&disc, &options, &game_folders, &tx, &cancel);
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

fn run_burn_job(
    disc: &DiscLayout,
    options: &BurnOptions,
    game_folders: &[(u64, PathBuf)],
    tx: &mpsc::UnboundedSender<BurnEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Staging is pure filesystem work — keep it in-process.
    // All IMAPI COM runs in a child process so an AV cannot kill the UI.
    check_cancel(cancel)?;
    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.05,
        message: "Staging disc layout…".into(),
    });
    let _ = tx.send(BurnEvent::Log("Staging disc layout on disk…".into()));
    let staged = stage_disc_layout(disc, game_folders).map_err(|e| e.to_string())?;

    check_cancel(cancel)?;
    if !options.simulate && options.drive.trim().is_empty() {
        return Err("No optical drive selected.".into());
    }

    let iso_path = if options.simulate {
        simulate_iso_path(disc.index)
    } else {
        std::env::temp_dir().join(format!(
            "gog-conjure-burn-disc{:02}-{}.iso",
            disc.index + 1,
            std::process::id()
        ))
    };
    if iso_path.exists() {
        let _ = std::fs::remove_file(&iso_path);
    }

    let job = BurnJobFile {
        stage_root: staged.root.clone(),
        iso_path: iso_path.clone(),
        options: options.clone(),
        media: disc.media,
        volid: disc_volid(disc),
        disc_index: disc.index,
    };
    let job_path = std::env::temp_dir().join(format!(
        "gog-conjure-burn-job-{}-{}.json",
        disc.index + 1,
        std::process::id()
    ));
    let job_json = serde_json::to_string_pretty(&job).map_err(|e| e.to_string())?;
    std::fs::write(&job_path, job_json).map_err(|e| e.to_string())?;

    let result = run_burn_helper_subprocess(&job_path, tx, cancel);

    // Keep staging dir alive until the helper exits (it reads files during ISO build).
    drop(staged);
    let _ = std::fs::remove_file(&job_path);
    if !options.simulate {
        let _ = std::fs::remove_file(&iso_path);
    }
    result
}

fn run_burn_helper_subprocess(
    job_path: &Path,
    tx: &mpsc::UnboundedSender<BurnEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("resolve executable: {e}"))?;
    let mut child = Command::new(&exe)
        .arg(BURN_JOB_FLAG)
        .arg(job_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn burn helper: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "burn helper missing stdout".to_string())?;
    let stderr = child.stderr.take();
    let mut finished_ok: Option<Result<(), String>> = None;
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("burn cancelled".into());
        }
        let line = match line {
            Ok(l) => l,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("read burn helper stdout: {err}"));
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: HelperEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => {
                let _ = tx.send(BurnEvent::Log(line.to_string()));
                continue;
            }
        };
        match event {
            HelperEvent::Progress { fraction, message } => {
                let _ = tx.send(BurnEvent::Progress { fraction, message });
            }
            HelperEvent::Log { line } => {
                let _ = tx.send(BurnEvent::Log(line));
            }
            HelperEvent::Finished { ok, error } => {
                finished_ok = Some(if ok {
                    Ok(())
                } else {
                    Err(error.unwrap_or_else(|| "burn helper failed".into()))
                });
            }
        }
    }

    let status = child.wait().map_err(|e| format!("wait burn helper: {e}"))?;
    if let Some(result) = finished_ok {
        return result;
    }
    if status.success() {
        Ok(())
    } else {
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "crash/signal".into());
        let stderr = stderr
            .map(|s| {
                let mut buf = String::new();
                let _ = BufReader::new(s).read_to_string(&mut buf);
                buf
            })
            .unwrap_or_default();
        Err(format!(
            "IMAPI burn helper exited unexpectedly ({code}). Optical COM faults are isolated from the UI. {}",
            stderr.trim()
        ))
    }
}

/// Entry point for `gog-conjure --imapi-burn-job <job.json>`.
pub fn run_burn_job_helper(job_path: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(job_path).map_err(|e| format!("read burn job: {e}"))?;
    let job: BurnJobFile =
        serde_json::from_str(&raw).map_err(|e| format!("parse burn job: {e}"))?;

    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<BurnEvent>();
    let cancel = Arc::new(AtomicBool::new(false));

    // Pump helper-local BurnEvents onto stdout while COM work runs on this thread.
    let pump = std::thread::spawn(move || {
        while let Ok(ev) = progress_rx.recv() {
            match ev {
                BurnEvent::Progress { fraction, message } => {
                    emit_helper(HelperEvent::Progress { fraction, message });
                }
                BurnEvent::Log(line) => emit_helper(HelperEvent::Log { line }),
                BurnEvent::Finished(result) => {
                    match result {
                        Ok(()) => emit_helper(HelperEvent::Finished {
                            ok: true,
                            error: None,
                        }),
                        Err(error) => emit_helper(HelperEvent::Finished {
                            ok: false,
                            error: Some(error),
                        }),
                    }
                    break;
                }
            }
        }
    });

    let bridge = HelperProgress(progress_tx.clone());
    let com_result = with_com(|| {
        unsafe {
            build_iso_file(
                &job.stage_root,
                job.media,
                &job.volid,
                &job.iso_path,
                &bridge,
                &cancel,
            )?;
        }
        if job.options.simulate {
            bridge.send(BurnEvent::Progress {
                fraction: 1.0,
                message: "Simulate complete".into(),
            });
            bridge.send(BurnEvent::Log(format!(
                "Simulate complete — ISO left at {}",
                job.iso_path.display()
            )));
            return Ok(());
        }
        unsafe { burn_iso_file(&job.iso_path, &job.options, &bridge, &cancel) }
    });

    match &com_result {
        Ok(()) => bridge.send(BurnEvent::Finished(Ok(()))),
        Err(err) => bridge.send(BurnEvent::Finished(Err(err.to_string()))),
    }
    drop(progress_tx);
    drop(bridge);
    let _ = pump.join();

    com_result.map_err(|e| e.to_string())?;
    Ok(())
}

fn emit_helper(event: HelperEvent) {
    if let Ok(line) = serde_json::to_string(&event) {
        println!("{line}");
        let _ = std::io::stdout().flush();
    }
}

/// Thin adapter so COM helpers can emit progress without depending on tokio.
struct HelperProgress(std::sync::mpsc::Sender<BurnEvent>);
impl HelperProgress {
    fn send(&self, event: BurnEvent) {
        let _ = self.0.send(event);
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

fn check_cancel(cancel: &Arc<AtomicBool>) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("burn cancelled".into())
    } else {
        Ok(())
    }
}

/// Entry point for `gog-conjure --list-optical-drives` (no UI, own process).
pub fn run_list_drives_helper() -> Result<(), String> {
    let drives = with_com(|| unsafe { list_drives_com() }).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(&drives).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

fn list_drives_subprocess() -> Result<Vec<OpticalDrive>, BurnError> {
    let exe = std::env::current_exe()
        .map_err(|e| BurnError::Other(format!("resolve executable: {e}")))?;
    let output = Command::new(&exe)
        .arg(LIST_DRIVES_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| BurnError::Other(format!("spawn drive helper: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "crash/signal".into());
        return Err(BurnError::Other(format!(
            "optical drive helper failed ({code}): {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(trimmed)
        .map_err(|e| BurnError::Other(format!("parse drive helper JSON: {e}")))
}

fn with_com<T>(f: impl FnOnce() -> Result<T, BurnError>) -> Result<T, BurnError> {
    // Prefer STA — IMAPI samples and shell-adjacent COM expect apartment threading.
    // S_OK / S_FALSE → we own a matching CoUninitialize. RPC_E_CHANGED_MODE means this
    // thread already has another apartment; COM is usable, but we must not uninitialize it.
    let should_uninit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = f();
    if should_uninit {
        unsafe {
            CoUninitialize();
        }
    }
    result
}

/// Enumerate recorders like the Microsoft *Checking Drive Support* sample:
/// `MsftDiscMaster2` → `Item(i)` → `MsftDiscRecorder2.InitializeDiscRecorder` →
/// read `ActiveDiscRecorder` / `VendorId` / `ProductId` / `VolumeName`.
///
/// Intentionally avoids `VolumePathNames` (SAFEARRAY of VARIANTs) — that property has
/// hard-crashed this app; the unique recorder id is what burn re-opens with anyway.
unsafe fn list_drives_com() -> Result<Vec<OpticalDrive>, BurnError> {
    let master: IDiscMaster2 =
        CoCreateInstance(&MsftDiscMaster2, None, CLSCTX_INPROC_SERVER).map_err(|e| {
            BurnError::Other(format!("IMAPI DiscMaster2 unavailable: {e}"))
        })?;

    // Same gate the platform exposes before IMAPI is usable on this machine.
    if let Ok(supported) = master.IsSupportedEnvironment() {
        if supported == VARIANT_FALSE {
            return Err(BurnError::Other(
                "IMAPI reports this environment does not support optical recording".into(),
            ));
        }
    }

    let count = master
        .Count()
        .map_err(|e| BurnError::Other(format!("DiscMaster2.Count: {e}")))?;
    let mut drives = Vec::new();
    for i in 0..count {
        let unique_id = match master.get_Item(i) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let recorder: IDiscRecorder2 =
            match CoCreateInstance(&MsftDiscRecorder2, None, CLSCTX_INPROC_SERVER) {
                Ok(r) => r,
                Err(_) => continue,
            };
        if recorder.InitializeDiscRecorder(&unique_id).is_err() {
            continue;
        }

        // Canonical id from Burning a Disc — used later by open_recorder / SetRecorder.
        let path = recorder
            .ActiveDiscRecorder()
            .map(bstr_to_string)
            .unwrap_or_else(|_| bstr_to_string(unique_id));
        let vendor = bstr_to_string(recorder.VendorId().unwrap_or_default());
        let product = bstr_to_string(recorder.ProductId().unwrap_or_default());
        let revision = bstr_to_string(recorder.ProductRevision().unwrap_or_default());
        let volume = recorder
            .VolumeName()
            .ok()
            .map(bstr_to_string)
            .filter(|s| !s.trim().is_empty());
        let model = match (revision.is_empty(), volume) {
            (true, Some(v)) => format!("{product} · {v}"),
            (false, Some(v)) => format!("{product} {revision} · {v}"),
            (true, None) => product,
            (false, None) => format!("{product} {revision}"),
        };

        drives.push(OpticalDrive { path, vendor, model });
    }
    Ok(drives)
}

fn bstr_to_string(b: BSTR) -> String {
    b.to_string()
}

/// Map our disc profiles to IMAPI physical media types for
/// `ChooseImageDefaultsForMediaType` (see IMAPI_MEDIA_PHYSICAL_TYPE).
fn imapi_media_type(media: DiscMedia) -> IMAPI_MEDIA_PHYSICAL_TYPE {
    match media {
        DiscMedia::Dvd5 => IMAPI_MEDIA_TYPE_DVDPLUSR,
        DiscMedia::Dvd9 => IMAPI_MEDIA_TYPE_DVDPLUSR_DUALLAYER,
        // IMAPI exposes BDR/BDRE only; capacity is set via FreeMediaBlocks.
        DiscMedia::Bd25 | DiscMedia::Bd50 | DiscMedia::Bd100 => IMAPI_MEDIA_TYPE_BDR,
    }
}

unsafe fn build_iso_file(
    stage_root: &Path,
    media: DiscMedia,
    volid: &str,
    iso_path: &Path,
    tx: &HelperProgress,
    cancel: &Arc<AtomicBool>,
) -> Result<(), BurnError> {
    tx.send(BurnEvent::Progress {
        fraction: 0.12,
        message: "Building ISO image…".into(),
    });
    tx.send(BurnEvent::Log(format!(
        "Building ISO (streamed to disk): {}",
        iso_path.display()
    )));

    // IMAPI2FS.MsftFileSystemImage — same object the Burning a Disc sample uses.
    let fsi: IFileSystemImage =
        CoCreateInstance(&MsftFileSystemImage, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| BurnError::Other(format!("MsftFileSystemImage: {e}")))?;

    fsi.ChooseImageDefaultsForMediaType(imapi_media_type(media))
        .map_err(|e| BurnError::Other(format!("ChooseImageDefaultsForMediaType: {e}")))?;

    // ISO9660 + Joliet (Rock Ridge is Linux-specific; Windows IMAPI has no RR).
    fsi.SetFileSystemsToCreate(FsiFileSystems(
        FsiFileSystemISO9660.0 | FsiFileSystemJoliet.0,
    ))
    .map_err(|e| BurnError::Other(format!("SetFileSystemsToCreate: {e}")))?;

    // Level 3 — long file names / large files (matches Linux xorriso iso_9660_level=3).
    let _ = fsi.SetISO9660InterchangeLevel(3);

    let vol = if volid.trim().is_empty() {
        "GOG_DISC".to_string()
    } else {
        volid.chars().take(VOLID_MAX_LEN).collect()
    };
    fsi.SetVolumeName(&BSTR::from(vol.as_str()))
        .map_err(|e| BurnError::Other(format!("SetVolumeName: {e}")))?;

    // Ensure capacity headroom for large BD layouts when creating a disc-file image.
    let blocks = (media.capacity_bytes() / 2048).max(1) as i32;
    let _ = fsi.SetFreeMediaBlocks(blocks);

    // Stream from source paths; do not stage a second full copy of every file.
    let _ = fsi.SetStageFiles(VARIANT_FALSE);

    let root: IFsiDirectoryItem = fsi
        .Root()
        .map_err(|e| BurnError::Other(format!("FileSystemImage.Root: {e}")))?;
    let stage = stage_root
        .to_str()
        .ok_or_else(|| BurnError::Other("staging path is not valid UTF-8".into()))?;
    // includeBaseDirectory = FALSE → contents of stage land at ISO root.
    root.AddTree(&BSTR::from(stage), VARIANT_FALSE)
        .map_err(|e| BurnError::Other(format!("AddTree: {e}")))?;

    let result = fsi
        .CreateResultImage()
        .map_err(|e| BurnError::Other(format!("CreateResultImage: {e}")))?;
    let stream = result
        .ImageStream()
        .map_err(|e| BurnError::Other(format!("ImageStream: {e}")))?;
    let total_blocks = result.TotalBlocks().unwrap_or(0).max(0) as u64;
    let block_size = result.BlockSize().unwrap_or(2048).max(1) as u64;
    let total_bytes = total_blocks.saturating_mul(block_size);

    stream_to_file(&stream, iso_path, total_bytes, tx, cancel)?;
    tx.send(BurnEvent::Log(format!(
        "ISO ready ({:.1} MiB)",
        total_bytes as f64 / (1024.0 * 1024.0)
    )));
    Ok(())
}

unsafe fn stream_to_file(
    stream: &IStream,
    iso_path: &Path,
    total_bytes: u64,
    tx: &HelperProgress,
    cancel: &Arc<AtomicBool>,
) -> Result<(), BurnError> {
    let mut pos = 0u64;
    let _ = stream.Seek(0, STREAM_SEEK_SET, Some(&mut pos as *mut u64));

    let mut file = File::create(iso_path).map_err(|e| {
        BurnError::Other(format!(
            "cannot create ISO {}: {e} (need free disk space ≈ disc size)",
            iso_path.display()
        ))
    })?;

    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut written = 0u64;
    loop {
        check_cancel(cancel).map_err(BurnError::Other)?;
        let mut read = 0u32;
        let hr = stream.Read(
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            Some(&mut read as *mut u32),
        );
        if read == 0 {
            if hr.is_err() {
                return Err(BurnError::Other(format!("IStream::Read failed: {hr:?}")));
            }
            break;
        }
        file.write_all(&buf[..read as usize])
            .map_err(|e| BurnError::Other(format!("writing ISO: {e}")))?;
        written += u64::from(read);
        if total_bytes > 0 {
            let frac = (written as f32 / total_bytes as f32).clamp(0.0, 1.0);
            tx.send(BurnEvent::Progress {
                fraction: 0.12 + frac * 0.48,
                message: format!("Building ISO… {:.0}%", frac * 100.0),
            });
        }
    }
    file.flush()
        .map_err(|e| BurnError::Other(format!("flush ISO: {e}")))?;

    // IMAPI requires stream size multiple of 2048 for burn; pad if needed.
    if written % 2048 != 0 {
        let pad = 2048 - (written % 2048);
        file.write_all(&vec![0u8; pad as usize])
            .map_err(|e| BurnError::Other(format!("padding ISO: {e}")))?;
    }
    Ok(())
}

unsafe fn burn_iso_file(
    iso_path: &Path,
    options: &BurnOptions,
    tx: &HelperProgress,
    cancel: &Arc<AtomicBool>,
) -> Result<(), BurnError> {
    // MsftDiscMaster2 → Item → MsftDiscRecorder2 → MsftDiscFormat2Data::Write
    // https://learn.microsoft.com/en-us/windows/win32/imapi/burning-a-disc
    let recorder = open_recorder(&options.drive)?;

    if options.blank {
        check_cancel(cancel).map_err(BurnError::Other)?;
        tx.send(BurnEvent::Progress {
            fraction: 0.62,
            message: "Blanking media…".into(),
        });
        tx.send(BurnEvent::Log("Blanking RW media (quick erase)…".into()));
        // Best-effort: erase may fail on write-once blanks — ignore that.
        if let Err(err) = erase_media(&recorder) {
            tx.send(BurnEvent::Log(format!(
                "NOTE: blank/erase skipped or failed ({err}); continuing write"
            )));
        }
    }

    check_cancel(cancel).map_err(BurnError::Other)?;
    tx.send(BurnEvent::Progress {
        fraction: 0.70,
        message: "Writing to disc…".into(),
    });
    tx.send(BurnEvent::Log(format!(
        "Burning ISO to {}…",
        options.drive
    )));

    let data: IDiscFormat2Data =
        CoCreateInstance(&MsftDiscFormat2Data, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| BurnError::Other(format!("MsftDiscFormat2Data: {e}")))?;
    data.SetRecorder(&recorder)
        .map_err(|e| BurnError::Other(format!("SetRecorder: {e}")))?;
    data.SetClientName(&BSTR::from(CLIENT_NAME))
        .map_err(|e| BurnError::Other(format!("SetClientName: {e}")))?;
    let _ = data.SetForceMediaToBeClosed(VARIANT_TRUE);
    if options.blank {
        let _ = data.SetForceOverwrite(VARIANT_TRUE);
    }

    if let Ok(verifier) = data.cast::<IBurnVerification>() {
        let level = if options.verify {
            IMAPI_BURN_VERIFICATION_FULL
        } else {
            IMAPI_BURN_VERIFICATION_NONE
        };
        let _ = verifier.SetBurnVerificationLevel(level);
    }

    if let Some(speed) = options.speed {
        // DVD 1x ≈ 1385 KiB/s ≈ 692 sectors/s (2048-byte sectors). Drive may adjust.
        let sectors = (speed as i32).saturating_mul(692);
        let _ = data.SetWriteSpeed(sectors, VARIANT_FALSE);
    }

    let iso_w: Vec<u16> = iso_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let grf_mode = STGM_READ.0 | STGM_SHARE_DENY_WRITE.0;
    let stream = SHCreateStreamOnFileW(PCWSTR(iso_w.as_ptr()), grf_mode)
        .map_err(|e| BurnError::Other(format!("SHCreateStreamOnFileW: {e}")))?;

    // Cancel watcher — Write is synchronous on this thread.
    let cancel_flag = cancel.clone();
    let writing = Arc::new(AtomicBool::new(true));
    let writing_watch = writing.clone();
    let data_cancel = SendDiscFormat2Data(data.clone());
    let watch = std::thread::spawn(move || {
        while writing_watch.load(Ordering::Relaxed) {
            if cancel_flag.load(Ordering::Relaxed) {
                unsafe { data_cancel.cancel_write() };
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });

    let write_result = data.Write(&stream);
    writing.store(false, Ordering::Relaxed);
    let _ = watch.join();

    write_result.map_err(|e| BurnError::Other(format!("IDiscFormat2Data::Write failed: {e}")))?;

    tx.send(BurnEvent::Log("Write completed successfully.".into()));
    tx.send(BurnEvent::Progress {
        fraction: 0.95,
        message: if options.verify {
            "Verifying media…".into()
        } else {
            "Finalizing…".into()
        },
    });

    if options.eject {
        tx.send(BurnEvent::Progress {
            fraction: 0.98,
            message: "Ejecting…".into(),
        });
        if let Err(err) = recorder.EjectMedia() {
            tx.send(BurnEvent::Log(format!("NOTE: eject failed: {err}")));
        }
    }

    tx.send(BurnEvent::Progress {
        fraction: 1.0,
        message: "Burn complete".into(),
    });
    Ok(())
}

unsafe fn erase_media(recorder: &IDiscRecorder2) -> Result<(), BurnError> {
    let eraser: IDiscFormat2Erase =
        CoCreateInstance(&MsftDiscFormat2Erase, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| BurnError::Other(format!("MsftDiscFormat2Erase: {e}")))?;
    eraser
        .SetRecorder(recorder)
        .map_err(|e| BurnError::Other(format!("erase SetRecorder: {e}")))?;
    eraser
        .SetClientName(&BSTR::from(CLIENT_NAME))
        .map_err(|e| BurnError::Other(format!("erase SetClientName: {e}")))?;
    // Quick erase (FullErase = FALSE).
    let _ = eraser.SetFullErase(VARIANT_FALSE);
    eraser
        .EraseMedia()
        .map_err(|e| BurnError::Other(format!("EraseMedia: {e}")))?;
    Ok(())
}

unsafe fn open_recorder(drive: &str) -> Result<IDiscRecorder2, BurnError> {
    // Same bind sequence as https://learn.microsoft.com/en-us/windows/win32/imapi/burning-a-disc
    let master: IDiscMaster2 = CoCreateInstance(&MsftDiscMaster2, None, CLSCTX_INPROC_SERVER)
        .map_err(|e| BurnError::Other(format!("DiscMaster2: {e}")))?;
    let count = master
        .Count()
        .map_err(|e| BurnError::Other(format!("Count: {e}")))?;
    let needle = drive.trim();
    for i in 0..count {
        let unique_id = match master.get_Item(i) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let recorder: IDiscRecorder2 =
            match CoCreateInstance(&MsftDiscRecorder2, None, CLSCTX_INPROC_SERVER) {
                Ok(r) => r,
                Err(_) => continue,
            };
        if recorder.InitializeDiscRecorder(&unique_id).is_err() {
            continue;
        }
        let active = recorder
            .ActiveDiscRecorder()
            .map(bstr_to_string)
            .unwrap_or_else(|_| bstr_to_string(unique_id));
        let vol_name = recorder
            .VolumeName()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default();
        // Match unique recorder id (preferred) or VolumeName — not VolumePathNames.
        if active.eq_ignore_ascii_case(needle)
            || (!vol_name.is_empty() && vol_name.eq_ignore_ascii_case(needle))
        {
            return Ok(recorder);
        }
    }
    Err(BurnError::Other(format!(
        "optical drive not found: {needle}"
    )))
}

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
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use windows::core::{BSTR, Interface, PCWSTR};
use windows::Win32::Foundation::VARIANT_BOOL;
use windows::Win32::Storage::Imapi::{
    IBurnVerification, IDiscFormat2Data, IDiscFormat2Erase, IDiscMaster2, IDiscRecorder2,
    IFileSystemImage, IFsiDirectoryItem, FsiFileSystemISO9660, FsiFileSystemJoliet, FsiFileSystems,
    IMAPI_BURN_VERIFICATION_FULL, IMAPI_BURN_VERIFICATION_NONE, IMAPI_MEDIA_TYPE_DISK,
    MsftDiscFormat2Data, MsftDiscFormat2Erase, MsftDiscMaster2, MsftDiscRecorder2,
    MsftFileSystemImage,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IStream, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, SAFEARRAY, STGM_READ, STGM_SHARE_DENY_WRITE, STREAM_SEEK_SET,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR};
use windows::Win32::UI::Shell::SHCreateStreamOnFileW;

/// CLI flag for the isolated drive-enumeration helper process.
pub const LIST_DRIVES_FLAG: &str = "--list-optical-drives";

use super::burner::{BurnError, BurnEvent, DiscBurner};
use super::models::{BurnOptions, DiscLayout, OpticalDrive};
use super::stage::{simulate_iso_path, stage_disc_layout};
use super::volid::{sanitize_volid, VOLID_MAX_LEN};

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
    check_cancel(cancel)?;
    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.05,
        message: "Staging disc layout…".into(),
    });
    let _ = tx.send(BurnEvent::Log("Staging disc layout on disk…".into()));
    let staged = stage_disc_layout(disc, game_folders).map_err(|e| e.to_string())?;

    check_cancel(cancel)?;
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

    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.12,
        message: "Building ISO image…".into(),
    });
    let _ = tx.send(BurnEvent::Log(format!(
        "Building ISO (streamed to disk): {}",
        iso_path.display()
    )));

    with_com(|| unsafe {
        build_iso_file(&staged.root, disc, &iso_path, tx, cancel)?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    // Staging dir can go away once the ISO exists.
    drop(staged);

    if options.simulate {
        let _ = tx.send(BurnEvent::Progress {
            fraction: 1.0,
            message: "Simulate complete".into(),
        });
        let _ = tx.send(BurnEvent::Log(format!(
            "Simulate complete — ISO left at {}",
            iso_path.display()
        )));
        return Ok(());
    }

    check_cancel(cancel)?;
    if options.drive.trim().is_empty() {
        let _ = std::fs::remove_file(&iso_path);
        return Err("No optical drive selected.".into());
    }

    let burn_result = with_com(|| unsafe {
        burn_iso_file(&iso_path, options, tx, cancel)?;
        Ok(())
    });

    let _ = std::fs::remove_file(&iso_path);
    burn_result.map_err(|e| e.to_string())
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
/// read `VendorId` / `ProductId` / `VolumeName` / `VolumePathNames`.
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

        // Canonical recorder id (what Burning a Disc uses to re-open the device).
        let active_id = recorder
            .ActiveDiscRecorder()
            .map(bstr_to_string)
            .unwrap_or_else(|_| bstr_to_string(unique_id));
        let vendor = bstr_to_string(recorder.VendorId().unwrap_or_default());
        let product = bstr_to_string(recorder.ProductId().unwrap_or_default());
        let revision = bstr_to_string(recorder.ProductRevision().unwrap_or_default());
        let model = if revision.is_empty() {
            product
        } else {
            format!("{product} {revision}")
        };

        // UI path: mount point (e.g. "D:\") when available, else VolumeName, else active id.
        // VolumePathNames is best-effort — a bad SAFEARRAY parse must not abort enumeration.
        let path = first_volume_path(&recorder)
            .or_else(|| {
                recorder
                    .VolumeName()
                    .ok()
                    .map(bstr_to_string)
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or_else(|| active_id.clone());

        drives.push(OpticalDrive { path, vendor, model });
    }
    Ok(drives)
}

unsafe fn first_volume_path(recorder: &IDiscRecorder2) -> Option<String> {
    let psa = recorder.VolumePathNames().ok()?;
    let path = first_path_from_volumepaths(psa);
    let _ = SafeArrayDestroy(psa);
    path
}

/// `get_VolumePathNames` returns `SAFEARRAY**` of **VARIANT** (`VT_BSTR`) elements
/// ([MSDN](https://learn.microsoft.com/en-us/windows/win32/api/imapi2/nf-imapi2-idiscrecorder2-get_volumepathnames)).
/// The VB sample's `For Each mountPoint In recorder.VolumePathNames` unwraps those
/// variants automatically; raw C/Rust must not treat the elements as bare BSTRs.
unsafe fn first_path_from_volumepaths(psa: *mut SAFEARRAY) -> Option<String> {
    if psa.is_null() {
        return None;
    }
    let lbound = SafeArrayGetLBound(psa, 1).ok()?;
    let ubound = SafeArrayGetUBound(psa, 1).ok()?;
    if ubound < lbound {
        return None;
    }

    let vt = SafeArrayGetVartype(psa).ok()?;
    let s = if vt == VT_BSTR {
        let mut bstr = BSTR::new();
        SafeArrayGetElement(psa, &lbound, &mut bstr as *mut BSTR as *mut _).ok()?;
        bstr.to_string()
    } else {
        let mut var = VARIANT::default();
        if SafeArrayGetElement(psa, &lbound, &mut var as *mut VARIANT as *mut _).is_err() {
            return None;
        }
        let text = BSTR::try_from(&var).ok().map(|b| b.to_string());
        drop(var);
        text?
    };

    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

fn bstr_to_string(b: BSTR) -> String {
    b.to_string()
}

unsafe fn build_iso_file(
    stage_root: &Path,
    disc: &DiscLayout,
    iso_path: &Path,
    tx: &mpsc::UnboundedSender<BurnEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), BurnError> {
    let fsi: IFileSystemImage = CoCreateInstance(&MsftFileSystemImage, None, CLSCTX_ALL)
        .map_err(|e| BurnError::Other(format!("MsftFileSystemImage: {e}")))?;

    fsi.ChooseImageDefaultsForMediaType(IMAPI_MEDIA_TYPE_DISK)
        .map_err(|e| BurnError::Other(format!("ChooseImageDefaultsForMediaType: {e}")))?;

    // ISO9660 + Joliet (Rock Ridge is Linux-specific; Windows IMAPI has no RR).
    fsi.SetFileSystemsToCreate(FsiFileSystems(
        FsiFileSystemISO9660.0 | FsiFileSystemJoliet.0,
    ))
    .map_err(|e| BurnError::Other(format!("SetFileSystemsToCreate: {e}")))?;

    // Level 3 — long file names / large files (matches Linux xorriso iso_9660_level=3).
    let _ = fsi.SetISO9660InterchangeLevel(3);

    let volid = disc_volid(disc);
    fsi.SetVolumeName(&BSTR::from(volid.as_str()))
        .map_err(|e| BurnError::Other(format!("SetVolumeName: {e}")))?;

    // Ensure capacity headroom for large BD layouts when creating a disc-file image.
    let blocks = (disc.media.capacity_bytes() / 2048).max(1) as i32;
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
    let _ = tx.send(BurnEvent::Log(format!(
        "ISO ready ({:.1} MiB)",
        total_bytes as f64 / (1024.0 * 1024.0)
    )));
    Ok(())
}

unsafe fn stream_to_file(
    stream: &IStream,
    iso_path: &Path,
    total_bytes: u64,
    tx: &mpsc::UnboundedSender<BurnEvent>,
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
            let _ = tx.send(BurnEvent::Progress {
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
    tx: &mpsc::UnboundedSender<BurnEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), BurnError> {
    let recorder = open_recorder(&options.drive)?;

    if options.blank {
        check_cancel(cancel).map_err(BurnError::Other)?;
        let _ = tx.send(BurnEvent::Progress {
            fraction: 0.62,
            message: "Blanking media…".into(),
        });
        let _ = tx.send(BurnEvent::Log("Blanking RW media (quick erase)…".into()));
        // Best-effort: erase may fail on write-once blanks — ignore that.
        if let Err(err) = erase_media(&recorder) {
            let _ = tx.send(BurnEvent::Log(format!(
                "NOTE: blank/erase skipped or failed ({err}); continuing write"
            )));
        }
    }

    check_cancel(cancel).map_err(BurnError::Other)?;
    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.70,
        message: "Writing to disc…".into(),
    });
    let _ = tx.send(BurnEvent::Log(format!(
        "Burning ISO to {}…",
        options.drive
    )));

    let data: IDiscFormat2Data = CoCreateInstance(&MsftDiscFormat2Data, None, CLSCTX_ALL)
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

    let _ = tx.send(BurnEvent::Log("Write completed successfully.".into()));
    let _ = tx.send(BurnEvent::Progress {
        fraction: 0.95,
        message: if options.verify {
            "Verifying media…".into()
        } else {
            "Finalizing…".into()
        },
    });

    if options.eject {
        let _ = tx.send(BurnEvent::Progress {
            fraction: 0.98,
            message: "Ejecting…".into(),
        });
        if let Err(err) = recorder.EjectMedia() {
            let _ = tx.send(BurnEvent::Log(format!("NOTE: eject failed: {err}")));
        }
    }

    let _ = tx.send(BurnEvent::Progress {
        fraction: 1.0,
        message: "Burn complete".into(),
    });
    Ok(())
}

unsafe fn erase_media(recorder: &IDiscRecorder2) -> Result<(), BurnError> {
    let eraser: IDiscFormat2Erase = CoCreateInstance(&MsftDiscFormat2Erase, None, CLSCTX_ALL)
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
    let needle_trim = needle.trim_end_matches(['\\', '/']);
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
        let vol = first_volume_path(&recorder).unwrap_or_default();
        let vol_name = recorder
            .VolumeName()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default();
        let vol_trim = vol.trim_end_matches(['\\', '/']);
        if active.eq_ignore_ascii_case(needle)
            || vol.eq_ignore_ascii_case(needle)
            || vol_trim.eq_ignore_ascii_case(needle_trim)
            || (!vol_name.is_empty() && vol_name.eq_ignore_ascii_case(needle))
        {
            return Ok(recorder);
        }
    }
    Err(BurnError::Other(format!(
        "optical drive not found: {needle}"
    )))
}

# Handoff: GitHub issue 2

Issue: https://github.com/Myxelium/gog-conjure/issues/2

A fresh session can implement one task from this file. Read this page, then only the task you were assigned. Tasks 1 and 2 are independent. Task 3 starts after both are merged, because a remembered disc stores the filesystem choice and must refuse M-DISC.

Do not replace `xorriso` with `mkudffs`. Do not make the burned disc mount read-write. A finished burn stays a closed image. Rewritable discs change when the app burns a new image over them.

## Locked decisions

- Filesystem choice is **Joliet** or **UDF**. No hybrid ISO+Joliet+UDF image.
- Default is **Joliet**, so existing Linux and macOS burns stay the same.
- A disc with any file ≥ 2 GiB cannot use Joliet. Windows IMAPI rejects those files (`IMAPI_ISO_MAX_FILE_BYTES` in `src/disc/windows_imapi.rs`). The burn stops with an error that names UDF. The UI selects UDF and disables Joliet while such a file is on the disc.
- New media profiles are **CD (~700 MB)** and **M-DISC DVD (~4.7 GB)**.
- M-DISC Blu-ray is already covered by BD-25 / BD-50 / BD-100. Do not add those profiles.
- Overwrite means blank the rewritable disc and write a full new image. It does not mean editing files on a mounted disc.

## What the app does now

| Platform | Code | Filesystem | Write |
|---|---|---|---|
| Linux | `src/disc/xorriso.rs` `build_argv` | `-joliet on`, `-rockridge on`, `-compliance iso_9660_level=3` | Direct to the drive, then `-commit`. Blank RW sends `-blank as_needed`. Blu-ray also sends `-format`. |
| macOS | `src/disc/macos_drutil.rs` `build_argv` | `-iso9660 -joliet -nohfsplus -noudf` | `drutil burn`. Blank RW sends `-erase`. |
| Windows | `src/disc/windows_imapi.rs` `build_iso_file` | UDF only if any file is ≥ 2 GiB. Otherwise ISO 9660 + Joliet + UDF. | Mastered IMAPI burn. Blank RW erases first. |

Disc sizes live in `DiscMedia` (`src/disc/media.rs`). The plan dialog (`src/ui/plan_modal.rs`) and the Burn tab (`src/ui/burn.rs`) both list `DiscMedia::all()`. Burn checkboxes are on `BurnOptions` (`src/disc/models.rs`). `BurnHistory` (`src/disc/history.rs`) stores burned game ids only.

Volume labels stay within `VOLID_MAX_LEN` (32) in `src/disc/volid.rs`.

---

## Task 1 — Choose Joliet or UDF per disc

### Done when

- Each disc has a Joliet / UDF control on the Burn tab, next to the existing Blank RW checkbox.
- Joliet uses the current Linux and macOS flags.
- UDF uses UDF only. Joliet is off on that path.
- A Joliet burn that contains a file ≥ 2 GiB fails with a clear error, on every platform, before the writer runs.
- Argv tests cover both choices.

### Files

- `src/disc/models.rs` — add the choice on `BurnOptions`. Serde default: Joliet. Old saved plans must still load.
- `src/ui/burn.rs` — the control. When any planned file is ≥ 2 GiB, force UDF and disable Joliet, and show why.
- `src/disc/xorriso.rs` — `build_argv`.
- `src/disc/macos_drutil.rs` — `build_argv`. Confirm flags with `man drutil` on a Mac. This repo is developed on Linux, where `drutil` is absent. Likely UDF spelling is `-udf` plus `-nojoliet`. Keep `-nohfsplus`.
- `src/disc/windows_imapi.rs` — `SetFileSystemsToCreate`. Joliet: `FsiFileSystemISO9660 | FsiFileSystemJoliet`. UDF: `FsiFileSystemUDF`. Remove the automatic hybrid for the Joliet choice.
- Tests in `src/disc/xorriso.rs` and `src/disc/macos_drutil.rs`. Add a Windows selection test if it can run without COM.

### Linux UDF argv

Joliet (current, keep it):

```text
-joliet on -rockridge on -compliance iso_9660_level=3
```

UDF:

```text
-udf on -joliet off
```

Rock Ridge stays off for UDF. Check these spellings against the `xorriso` man page and lock them in the argv test.

### Out of scope

`mkudffs`, mount-rw, multi-session, CD, M-DISC, reburn history.

---

## Task 2 — CD and M-DISC DVD

### Done when

- The plan dialog and the Burn tab offer CD and M-DISC DVD.
- CD plans at about 2% under the 80-minute data capacity of 737,280,000 bytes (360,000 × 2048), same margin style as DVD-5’s `4_377_000_000`.
- M-DISC DVD uses the same usable bytes as `DiscMedia::Dvd5`.
- CD burn argv does not include Blu-ray `-format`.
- M-DISC DVD uses the DVD write path. Its blank default is off. The tooltip says the disc is write-once and cannot be erased for reuse.
- `is_bluray()` is false for both, so Defect spare stays hidden.

### Files

- `src/disc/media.rs` — new `DiscMedia` variants, labels, capacities. Adding variants is compatible with existing saved plans.
- `src/ui/plan_modal.rs` and `src/ui/burn.rs` — they already iterate `DiscMedia::all()`.
- `src/disc/windows_imapi.rs` — `imapi_media_type`. CD → `IMAPI_MEDIA_TYPE_CDR`. M-DISC DVD → the same type already used for `Dvd5`.
- `src/disc/xorriso.rs` — a test that CD does not receive `-format`.
- `src/disc/models.rs` — new M-DISC discs start with `blank: false`. CD still starts with `blank: true`, because CD-RW uses that checkbox. CD-R ignores a blank that the drive cannot do; keep the current “blank failed, continue write” behavior on Windows.

### Do not add

M-DISC BD-25 / BD-50 / BD-100. Those burns are the existing Blu-ray profiles.

### Out of scope

Filesystem picker, reburn history, `mkudffs`.

---

## Task 3 — Reburn updated downloads onto a rewritable disc

Start this only after Tasks 1 and 2 are on the branch you are editing.

### Done when

- A successful burn saves a disc record: volume label, `DiscMedia`, filesystem choice, game ids, titles.
- The Burn tab lists those records with a Reburn action.
- Reburn reloads the current files in those game folders, packs them onto the same media type and filesystem, forces Blank RW on, and burns a full new image.
- M-DISC DVD has no Reburn action. The UI says that disc is write-once.
- CD, DVD-5, DVD-9, and Blu-ray can reburn. The user is confirming the inserted disc is rewritable (CD-RW, DVD±RW, or BD-RE).

### Files

- `src/disc/history.rs` — extend `BurnHistory`. Keep loading old `burn_history.json` files that only have `burned_game_ids`.
- `src/app.rs` — write the record when a burn finishes successfully. Wire the Reburn action into the existing burn start path.
- `src/ui/burn.rs` — list and button.
- Packing already lives in `src/disc/pack.rs`. Reuse it with the remembered media type.

### Behavior to keep

Blank plus burn is the overwrite. Linux already sends `-blank as_needed`. Windows and macOS already erase when Blank RW is on. Reburn turns that on and runs the normal writer. Afterward the disc still mounts read-only until the next reburn.

### Out of scope

Mounted read-write UDF, `mkudffs`, appending a session, detecting from the drive whether the inserted disc is actually RW.

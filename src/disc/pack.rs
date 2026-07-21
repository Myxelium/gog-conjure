use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::history::{AvailableDownload, BurnHistory};
use super::local_id::{is_local_game_id, local_game_id};
use super::media::DiscMedia;
use super::models::{
    BurnFile, BurnListEntry, BurnOptions, BurnPlan, BurnUnit, DiscBurnStatus, DiscLayout,
    DownloadReadiness, PlannedFile, SplitPolicy,
};
use super::volid::auto_volid;

/// Organize included burn-list games into the user-provided discs (heterogeneous sizes).
///
/// Existing disc media/options/manual volids are preserved. Contents are redistributed.
/// Games that do not fit are reported in `warnings` / `blockers`.
/// Planning uses GOG/planned sizes when downloads are not finished yet.
pub fn plan_into_discs(
    mut discs: Vec<DiscLayout>,
    entries: &[BurnListEntry],
    global_split: SplitPolicy,
) -> BurnPlan {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if discs.is_empty() {
        warnings.push("Add at least one disc, then click Plan.".into());
        return BurnPlan {
            discs,
            blockers,
            warnings,
        };
    }

    let max_cap = discs
        .iter()
        .map(|d| d.usable_capacity_bytes())
        .max()
        .unwrap_or(0);

    let (chains, mut loose) =
        collect_units(entries, global_split, max_cap, &mut blockers, &mut warnings);

    // Clear contents; keep media, options, manual volid.
    for disc in &mut discs {
        disc.units.clear();
        disc.used_bytes = 0;
        disc.remaining_bytes = disc.usable_capacity_bytes();
        disc.last_error = None;
        if !matches!(disc.status, DiscBurnStatus::Burning) {
            disc.status = DiscBurnStatus::Empty;
        }
        if !disc.volid_manual {
            disc.volid.clear();
        }
    }

    let capacities: Vec<u64> = discs.iter().map(|d| d.usable_capacity_bytes()).collect();
    let mut bins: Vec<Vec<BurnUnit>> = discs.iter().map(|_| Vec::new()).collect();
    let mut used: Vec<u64> = vec![0; discs.len()];

    for chain in chains {
        let game_title = chain
            .first()
            .map(|u| u.game_title.clone())
            .unwrap_or_else(|| "game".into());
        let mut min_disc = 0usize;
        let mut chain_ok = true;
        let mut placed: Vec<(usize, u64, u32, u64)> = Vec::new();
        for unit in chain {
            let game_id = unit.game_id;
            let part = unit.part_index;
            let size = unit.size_bytes;
            match place_on_fixed(&capacities, &mut bins, &mut used, unit, min_disc) {
                Some(idx) => {
                    min_disc = idx;
                    placed.push((idx, game_id, part, size));
                }
                None => {
                    chain_ok = false;
                    break;
                }
            }
        }
        if !chain_ok {
            for (idx, game_id, part, size) in placed.into_iter().rev() {
                if let Some(pos) = bins[idx]
                    .iter()
                    .rposition(|u| u.game_id == game_id && u.part_index == part)
                {
                    bins[idx].remove(pos);
                    used[idx] = used[idx].saturating_sub(size);
                }
            }
            warnings.push(format!(
                "{game_title}: split parts did not fit across current discs — add a disc or enlarge media"
            ));
        }
    }

    loose.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    for unit in loose {
        let title = unit.game_title.clone();
        if place_on_fixed(&capacities, &mut bins, &mut used, unit, 0).is_none() {
            warnings.push(format!(
                "{title}: does not fit on any current disc — add a disc or change media sizes"
            ));
        }
    }

    for (i, disc) in discs.iter_mut().enumerate() {
        disc.units = std::mem::take(&mut bins[i]);
        disc.recompute_usage();
        if !disc.units.is_empty() && !matches!(disc.status, DiscBurnStatus::Burning) {
            disc.status = DiscBurnStatus::Planned;
        }
    }

    assign_volids(&mut discs);

    BurnPlan {
        discs,
        blockers,
        warnings,
    }
}

/// Pack included games onto as many same-size discs as needed.
pub fn plan_homogeneous_discs(
    media: DiscMedia,
    entries: &[BurnListEntry],
    global_split: SplitPolicy,
    options: BurnOptions,
) -> BurnPlan {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let capacity = media.capacity_bytes();
    let (chains, loose) = collect_units(entries, global_split, capacity, &mut blockers, &mut warnings);

    if chains.is_empty() && loose.is_empty() {
        return BurnPlan {
            discs: Vec::new(),
            blockers,
            warnings,
        };
    }

    let mut discs = pack_units(media, capacity, chains, loose);
    for disc in &mut discs {
        disc.options = options.clone();
    }
    assign_volids(&mut discs);

    BurnPlan {
        discs,
        blockers,
        warnings,
    }
}

fn collect_units(
    entries: &[BurnListEntry],
    global_split: SplitPolicy,
    split_cap: u64,
    blockers: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> (Vec<Vec<BurnUnit>>, Vec<BurnUnit>) {
    let mut chains: Vec<Vec<BurnUnit>> = Vec::new();
    let mut loose: Vec<BurnUnit> = Vec::new();

    for entry in entries.iter().filter(|e| e.included) {
        if entry.readiness != DownloadReadiness::Ready {
            warnings.push(format!(
                "{}: planned from GOG sizes — burn waits until download is complete ({})",
                entry.title,
                entry.readiness.label()
            ));
        }

        let policy = entry.effective_split(global_split);
        match resolve_game(entry, policy, split_cap) {
            Ok(units) if units.len() == 1 && !units[0].is_split() => {
                loose.push(units.into_iter().next().unwrap());
            }
            Ok(units) => chains.push(units),
            Err(msg) => blockers.push(msg),
        }
    }

    (chains, loose)
}

fn place_on_fixed(
    capacities: &[u64],
    bins: &mut [Vec<BurnUnit>],
    used: &mut [u64],
    unit: BurnUnit,
    min_disc: usize,
) -> Option<usize> {
    for i in min_disc..bins.len() {
        if used[i] + unit.size_bytes <= capacities[i] {
            used[i] += unit.size_bytes;
            bins[i].push(unit);
            return Some(i);
        }
    }
    None
}

fn resolve_game(
    entry: &BurnListEntry,
    policy: SplitPolicy,
    capacity: u64,
) -> Result<Vec<BurnUnit>, String> {
    let files = game_files_for_planning(entry)?;
    if files.is_empty() {
        return Err(format!("{}: no files to plan", entry.title));
    }

    let total: u64 = files.iter().map(|f| f.size_bytes).sum();
    let needs_split = match policy {
        SplitPolicy::Never => false,
        SplitPolicy::WhenOversized => total > capacity,
        SplitPolicy::AllowToPack => true,
    };

    if !needs_split {
        if total > capacity {
            return Err(format!(
                "{}: {} exceeds {} (enable splitting or choose larger media)",
                entry.title,
                crate::gog::format_bytes(total),
                entry_media_hint(capacity)
            ));
        }
        return Ok(vec![BurnUnit {
            game_id: entry.game_id,
            game_title: entry.title.clone(),
            size_bytes: total,
            files,
            part_index: 0,
            part_count: 1,
        }]);
    }

    let ordered = order_for_split(files);
    if !can_split(&ordered) && total > capacity {
        return Err(format!(
            "{}: larger than media and no GOG-style installer bins to split",
            entry.title
        ));
    }

    // AllowToPack with total fitting: still emit per-piece units when multipart exists.
    if policy == SplitPolicy::AllowToPack && total <= capacity && !can_split(&ordered) {
        return Ok(vec![BurnUnit {
            game_id: entry.game_id,
            game_title: entry.title.clone(),
            size_bytes: total,
            files: ordered,
            part_index: 0,
            part_count: 1,
        }]);
    }

    let units = chunk_ordered_files(entry, &ordered, capacity)?;
    Ok(units)
}

/// Prefer on-disk files when Ready; otherwise use the GOG/planned manifest.
fn game_files_for_planning(entry: &BurnListEntry) -> Result<Vec<BurnFile>, String> {
    if entry.readiness == DownloadReadiness::Ready && entry.folder.is_dir() {
        match list_game_files(&entry.folder) {
            Ok(files) if !files.is_empty() => return Ok(files),
            Ok(_) if !entry.planned_files.is_empty() => {
                return Ok(burn_files_from_planned(&entry.folder, &entry.planned_files));
            }
            Ok(_) => return Err(format!("{}: no files in folder", entry.title)),
            Err(_) if !entry.planned_files.is_empty() => {
                return Ok(burn_files_from_planned(&entry.folder, &entry.planned_files));
            }
            Err(e) => return Err(format!("{}: {e}", entry.title)),
        }
    }

    if !entry.planned_files.is_empty() {
        return Ok(burn_files_from_planned(&entry.folder, &entry.planned_files));
    }

    Err(format!(
        "{}: no size data — plan from Library or download first",
        entry.title
    ))
}

pub fn burn_files_from_planned(folder: &Path, planned: &[PlannedFile]) -> Vec<BurnFile> {
    planned
        .iter()
        .map(|f| BurnFile {
            path: folder.join(&f.relative_name),
            relative_name: f.relative_name.clone(),
            size_bytes: f.size_bytes,
        })
        .collect()
}

/// Fill in on-disk paths for planned/split units before burning.
pub fn resolve_disc_file_paths(
    disc: &mut DiscLayout,
    folders: &[(u64, PathBuf)],
) -> Result<(), String> {
    for unit in &mut disc.units {
        let folder = folders
            .iter()
            .find(|(id, _)| *id == unit.game_id)
            .map(|(_, p)| p.clone())
            .or_else(|| {
                let sanitized = sanitize_filename::sanitize(&unit.game_title);
                folders.iter().find_map(|(_, p)| {
                    let name = p.file_name()?.to_string_lossy();
                    if name == sanitized || name.eq_ignore_ascii_case(&unit.game_title) {
                        Some(p.clone())
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| format!("missing folder for '{}'", unit.game_title))?;

        let is_split = unit.is_split();
        let game_title = unit.game_title.clone();
        for file in &mut unit.files {
            let path = if file.relative_name.is_empty() {
                folder.clone()
            } else {
                folder.join(&file.relative_name)
            };
            if is_split && !path.is_file() {
                return Err(format!(
                    "missing file for '{game_title}': {}",
                    path.display()
                ));
            }
            file.path = path;
        }
    }
    Ok(())
}

pub fn planned_files_from_download_files(
    files: &[crate::gog::DownloadFile],
) -> Vec<PlannedFile> {
    files
        .iter()
        .map(|f| PlannedFile {
            relative_name: f.name.clone(),
            size_bytes: f.size,
        })
        .collect()
}

fn entry_media_hint(capacity: u64) -> String {
    crate::gog::format_bytes(capacity)
}

/// List all files under a game folder with paths relative to the folder.
pub fn list_game_files(folder: &Path) -> std::io::Result<Vec<BurnFile>> {
    let mut out = Vec::new();
    list_game_files_inner(folder, folder, &mut out)?;
    out.sort_by(|a, b| a.relative_name.cmp(&b.relative_name));
    Ok(out)
}

fn list_game_files_inner(
    root: &Path,
    dir: &Path,
    out: &mut Vec<BurnFile>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            list_game_files_inner(root, &path, out)?;
        } else if meta.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(BurnFile {
                path,
                relative_name: relative,
                size_bytes: meta.len(),
            });
        }
    }
    Ok(())
}

fn is_installer_exe(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".exe")
        && (lower.contains("setup") || lower.starts_with("setup") || lower.contains("install"))
}

fn is_bin_part(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".bin")
}

fn bin_sort_key(name: &str) -> (i32, String) {
    let lower = name.to_lowercase();
    // setup_game-1.bin / setup_game.bin
    let stem = lower.trim_end_matches(".bin");
    if let Some(idx) = stem.rfind('-') {
        if let Ok(n) = stem[idx + 1..].parse::<i32>() {
            return (n, lower);
        }
    }
    // bare .bin without number sorts before numbered ones
    (0, lower)
}

/// Order files for split: installer exes first, then bins numeric, then extras.
fn order_for_split(files: Vec<BurnFile>) -> Vec<BurnFile> {
    let mut exes = Vec::new();
    let mut bins = Vec::new();
    let mut other = Vec::new();

    for f in files {
        let base = Path::new(&f.relative_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&f.relative_name);
        if is_installer_exe(base) {
            exes.push(f);
        } else if is_bin_part(base) {
            bins.push(f);
        } else {
            other.push(f);
        }
    }

    exes.sort_by(|a, b| a.relative_name.to_lowercase().cmp(&b.relative_name.to_lowercase()));
    bins.sort_by(|a, b| {
        let ka = bin_sort_key(
            Path::new(&a.relative_name)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&a.relative_name),
        );
        let kb = bin_sort_key(
            Path::new(&b.relative_name)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&b.relative_name),
        );
        ka.cmp(&kb)
    });
    other.sort_by(|a, b| a.relative_name.cmp(&b.relative_name));

    // Disc 1: exes + extras, then bins in order across discs.
    let mut ordered = Vec::with_capacity(exes.len() + other.len() + bins.len());
    ordered.extend(exes);
    ordered.extend(other);
    ordered.extend(bins);
    ordered
}

fn can_split(ordered: &[BurnFile]) -> bool {
    let has_exe = ordered.iter().any(|f| {
        let base = Path::new(&f.relative_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        is_installer_exe(base)
    });
    let has_bin = ordered.iter().any(|f| {
        let base = Path::new(&f.relative_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        is_bin_part(base)
    });
    has_exe && has_bin
}

fn chunk_ordered_files(
    entry: &BurnListEntry,
    ordered: &[BurnFile],
    capacity: u64,
) -> Result<Vec<BurnUnit>, String> {
    for f in ordered {
        if f.size_bytes > capacity {
            return Err(format!(
                "{}: file '{}' alone exceeds media capacity",
                entry.title, f.relative_name
            ));
        }
    }

    let mut chunks: Vec<Vec<BurnFile>> = Vec::new();
    let mut current: Vec<BurnFile> = Vec::new();
    let mut used = 0u64;

    for f in ordered {
        if !current.is_empty() && used + f.size_bytes > capacity {
            chunks.push(std::mem::take(&mut current));
            used = 0;
        }
        used += f.size_bytes;
        current.push(f.clone());
    }
    if !current.is_empty() {
        chunks.push(current);
    }

    let part_count = chunks.len() as u32;
    let units = chunks
        .into_iter()
        .enumerate()
        .map(|(i, files)| {
            let size_bytes = files.iter().map(|f| f.size_bytes).sum();
            BurnUnit {
                game_id: entry.game_id,
                game_title: entry.title.clone(),
                size_bytes,
                files,
                part_index: i as u32,
                part_count,
            }
        })
        .collect();
    Ok(units)
}

fn pack_units(
    media: DiscMedia,
    capacity: u64,
    chains: Vec<Vec<BurnUnit>>,
    mut loose: Vec<BurnUnit>,
) -> Vec<DiscLayout> {
    let mut bins: Vec<Vec<BurnUnit>> = Vec::new();
    let mut used: Vec<u64> = Vec::new();

    let place_on = |bins: &mut Vec<Vec<BurnUnit>>,
                    used: &mut Vec<u64>,
                    unit: BurnUnit,
                    min_disc: usize|
     -> usize {
        for i in min_disc..bins.len() {
            if used[i] + unit.size_bytes <= capacity {
                used[i] += unit.size_bytes;
                bins[i].push(unit);
                return i;
            }
        }
        let size = unit.size_bytes;
        bins.push(vec![unit]);
        used.push(size);
        bins.len() - 1
    };

    // Place ordered chains first (install order across discs).
    for chain in chains {
        let mut min_disc = 0usize;
        for unit in chain {
            min_disc = place_on(&mut bins, &mut used, unit, min_disc);
        }
    }

    // Largest-first for whole games, then fill residuals.
    loose.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    for unit in loose {
        let _ = place_on(&mut bins, &mut used, unit, 0);
    }

    // Residual fill pass: try moving nothing — FFD already fills first fit.
    // Optional: second pass re-check any empty — not needed.

    bins.into_iter()
        .enumerate()
        .map(|(index, units)| {
            let used_bytes = units.iter().map(|u| u.size_bytes).sum();
            DiscLayout {
                index,
                media,
                volid: String::new(),
                volid_manual: false,
                units,
                used_bytes,
                remaining_bytes: capacity.saturating_sub(used_bytes),
                status: DiscBurnStatus::Planned,
                last_error: None,
                options: BurnOptions::default(),
            }
        })
        .collect()
}

/// Scan download root + history for games the user can add/re-add to the burn list.
pub fn list_available_downloads(
    root: &Path,
    library: &[(u64, String)],
    history: &BurnHistory,
    burn_list_ids: &std::collections::HashSet<u64>,
) -> Vec<AvailableDownload> {
    let burned = history.burned_set();
    let mut by_folder: BTreeMap<String, AvailableDownload> = BTreeMap::new();

    // Index library by sanitized folder name.
    let mut title_to_id: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for (id, title) in library {
        let key = sanitize_filename::sanitize(title).to_lowercase();
        title_to_id.insert(key, (*id, title.clone()));
    }
    for known in &history.known_downloads {
        if known.game_id == 0 {
            continue;
        }
        let key = sanitize_filename::sanitize(&known.title).to_lowercase();
        title_to_id
            .entry(key)
            .or_insert((known.game_id, known.title.clone()));
    }

    if root.is_dir() {
        if let Ok(rd) = std::fs::read_dir(root) {
            for entry in rd.flatten() {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let folder = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let key = name.to_lowercase();
                let size = folder_size(&folder);
                if size == 0 {
                    continue;
                }
                let (game_id, title) = title_to_id.get(&key).cloned().unwrap_or_else(|| {
                    // Unmatched folder: stable synthetic id + folder name as title.
                    (local_game_id(&name), name.clone())
                });
                // Ignore stale history entries that still used the old id-0 sentinel.
                let game_id = if game_id == 0 {
                    local_game_id(&name)
                } else {
                    game_id
                };
                let title = if is_local_game_id(game_id) {
                    name.clone()
                } else {
                    title
                };
                by_folder.insert(
                    key,
                    AvailableDownload {
                        game_id,
                        title,
                        folder,
                        size_bytes: size,
                        burned: burned.contains(&game_id),
                        on_burn_list: burn_list_ids.contains(&game_id),
                    },
                );
            }
        }
    }

    // Remembered downloads whose folders still exist but weren't scanned (edge cases).
    for known in &history.known_downloads {
        if known.game_id == 0 {
            continue;
        }
        let folder = root.join(sanitize_filename::sanitize(&known.title));
        if !folder.is_dir() {
            continue;
        }
        let key = sanitize_filename::sanitize(&known.title).to_lowercase();
        by_folder.entry(key).or_insert_with(|| AvailableDownload {
            game_id: known.game_id,
            title: known.title.clone(),
            folder: folder.clone(),
            size_bytes: folder_size(&folder),
            burned: burned.contains(&known.game_id),
            on_burn_list: burn_list_ids.contains(&known.game_id),
        });
    }

    let mut out: Vec<_> = by_folder.into_values().collect();
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

fn assign_volids(discs: &mut [DiscLayout]) {
    // Track how many discs each split game appears on for suffix logic.
    let mut game_disc_counts: BTreeMap<u64, usize> = BTreeMap::new();
    for disc in discs.iter() {
        let mut seen = std::collections::HashSet::new();
        for u in &disc.units {
            if u.is_split() && seen.insert(u.game_id) {
                *game_disc_counts.entry(u.game_id).or_default() += 1;
            }
        }
    }

    for disc in discs.iter_mut() {
        if disc.volid_manual {
            continue;
        }
        if disc.units.is_empty() {
            disc.volid.clear();
            continue;
        }
        let titles = disc.game_titles();
        let suffix = if titles.len() == 1 {
            let gid = disc.units.first().map(|u| u.game_id);
            let multi_disc_split = gid
                .and_then(|id| game_disc_counts.get(&id).copied())
                .unwrap_or(0)
                > 1;
            if multi_disc_split {
                disc.split_part_suffix()
            } else {
                None
            }
        } else {
            None
        };
        disc.volid = auto_volid(&titles, disc.index + 1, suffix);
    }
}

/// Recalculate folder size for a burn list entry.
pub fn folder_size(path: &Path) -> u64 {
    dir_size(path).unwrap_or(0)
}

fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += dir_size(&entry.path())?;
        }
    }
    Ok(total)
}

/// ISO path for a unit file: `/{sanitized_title}/{relative}`.
pub fn iso_path_for(game_title: &str, relative_name: &str) -> String {
    let folder = sanitize_filename::sanitize(game_title);
    format!("/{folder}/{relative_name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gog-conjure-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: u64, title: &str, folder: PathBuf, size: u64, policy: SplitPolicy) -> BurnListEntry {
        BurnListEntry {
            game_id: id,
            title: title.into(),
            folder,
            size_bytes: size,
            readiness: DownloadReadiness::Ready,
            split_override: Some(policy),
            included: true,
            planned_files: Vec::new(),
        }
    }

    fn entry_planned(
        id: u64,
        title: &str,
        files: Vec<PlannedFile>,
        policy: SplitPolicy,
    ) -> BurnListEntry {
        let size: u64 = files.iter().map(|f| f.size_bytes).sum();
        BurnListEntry {
            game_id: id,
            title: title.into(),
            folder: PathBuf::from("/tmp/not-downloaded"),
            size_bytes: size,
            readiness: DownloadReadiness::Pending,
            split_override: Some(policy),
            included: true,
            planned_files: files,
        }
    }

    #[test]
    fn plan_from_planned_files_without_download() {
        let entries = vec![entry_planned(
            1,
            "SmallGame",
            vec![PlannedFile {
                relative_name: "setup.exe".into(),
                size_bytes: 1_000,
            }],
            SplitPolicy::Never,
        )];
        let plan = plan_homogeneous_discs(
            DiscMedia::Dvd5,
            &entries,
            SplitPolicy::Never,
            BurnOptions::default(),
        );
        assert!(plan.blockers.is_empty());
        assert_eq!(plan.discs.len(), 1);
        assert_eq!(plan.discs[0].units.len(), 1);
        assert_eq!(plan.discs[0].used_bytes, 1_000);
    }

    #[test]
    fn homogeneous_counts_discs_from_sizes() {
        let capacity = DiscMedia::Dvd5.capacity_bytes();
        let entries = vec![
            entry_planned(
                1,
                "A",
                vec![PlannedFile {
                    relative_name: "a.bin".into(),
                    size_bytes: capacity * 3 / 4,
                }],
                SplitPolicy::Never,
            ),
            entry_planned(
                2,
                "B",
                vec![PlannedFile {
                    relative_name: "b.bin".into(),
                    size_bytes: capacity * 3 / 4,
                }],
                SplitPolicy::Never,
            ),
        ];
        let plan = plan_homogeneous_discs(
            DiscMedia::Dvd5,
            &entries,
            SplitPolicy::Never,
            BurnOptions::default(),
        );
        assert_eq!(plan.discs.len(), 2);
    }

    #[test]
    fn split_from_planned_gog_bins() {
        let e = entry_planned(
            9,
            "HugeGame",
            vec![
                PlannedFile {
                    relative_name: "setup_huge.exe".into(),
                    size_bytes: 500,
                },
                PlannedFile {
                    relative_name: "setup_huge-1.bin".into(),
                    size_bytes: 900,
                },
                PlannedFile {
                    relative_name: "setup_huge-2.bin".into(),
                    size_bytes: 900,
                },
                PlannedFile {
                    relative_name: "setup_huge-3.bin".into(),
                    size_bytes: 900,
                },
            ],
            SplitPolicy::WhenOversized,
        );
        let units = resolve_game(&e, SplitPolicy::WhenOversized, 2000).unwrap();
        assert!(units.len() >= 2);
        assert!(units[0]
            .files
            .iter()
            .any(|f| f.relative_name.ends_with(".exe")));
    }

    #[test]
    fn pack_respects_capacity_min_discs() {
        let media = DiscMedia::Dvd5;
        let loose = vec![
            BurnUnit {
                game_id: 1,
                game_title: "Big".into(),
                size_bytes: 3_000,
                files: vec![],
                part_index: 0,
                part_count: 1,
            },
            BurnUnit {
                game_id: 2,
                game_title: "Med".into(),
                size_bytes: 1_200,
                files: vec![],
                part_index: 0,
                part_count: 1,
            },
            BurnUnit {
                game_id: 3,
                game_title: "Tiny".into(),
                size_bytes: 100,
                files: vec![],
                part_index: 0,
                part_count: 1,
            },
        ];
        let discs = pack_units(media, 4_000, vec![], loose);
        assert_eq!(discs.len(), 2);
        assert!(discs.iter().all(|d| d.used_bytes <= 4_000));
        let d0_titles: Vec<_> = discs[0].units.iter().map(|u| u.game_title.as_str()).collect();
        assert!(d0_titles.contains(&"Big"));
        assert!(d0_titles.contains(&"Tiny"));
    }

    #[test]
    fn plan_into_heterogeneous_discs() {
        let shells = vec![
            DiscLayout::new_empty(0, DiscMedia::Dvd5, BurnOptions::default()),
            DiscLayout::new_empty(1, DiscMedia::Dvd9, BurnOptions::default()),
        ];
        // Force tiny capacities via direct place logic already covered; here ensure API keeps media.
        let plan = plan_into_discs(shells, &[], SplitPolicy::Never);
        assert_eq!(plan.discs.len(), 2);
        assert_eq!(plan.discs[0].media, DiscMedia::Dvd5);
        assert_eq!(plan.discs[1].media, DiscMedia::Dvd9);
    }

    #[test]
    fn split_bins_ordered_across_discs() {
        let dir = test_dir("split");
        let g = dir.join("HugeGame");
        fs::create_dir_all(&g).unwrap();
        fs::write(g.join("setup_huge.exe"), vec![0u8; 500]).unwrap();
        fs::write(g.join("setup_huge-1.bin"), vec![0u8; 900]).unwrap();
        fs::write(g.join("setup_huge-2.bin"), vec![0u8; 900]).unwrap();
        fs::write(g.join("setup_huge-3.bin"), vec![0u8; 900]).unwrap();

        let e = entry(9, "HugeGame", g.clone(), 3200, SplitPolicy::WhenOversized);
        let units = resolve_game(&e, SplitPolicy::WhenOversized, 2000).unwrap();
        assert!(units.len() >= 2);
        assert_eq!(units[0].part_index, 0);
        assert!(units[0]
            .files
            .iter()
            .any(|f| f.relative_name.ends_with(".exe")));
        let all_bins: Vec<_> = units
            .iter()
            .flat_map(|u| u.files.iter())
            .filter(|f| f.relative_name.ends_with(".bin"))
            .map(|f| f.relative_name.clone())
            .collect();
        assert_eq!(
            all_bins,
            vec![
                "setup_huge-1.bin".to_string(),
                "setup_huge-2.bin".to_string(),
                "setup_huge-3.bin".to_string()
            ]
        );

        let discs = pack_units(DiscMedia::Dvd5, 2000, vec![units], vec![]);
        assert!(discs.len() >= 2);
        let mut last_part = 0u32;
        for d in &discs {
            for u in &d.units {
                assert!(u.part_index >= last_part);
                last_part = u.part_index;
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_split_blocks_oversized() {
        let dir = test_dir("never");
        let g = dir.join("TooBig");
        fs::create_dir_all(&g).unwrap();
        fs::write(g.join("setup.exe"), vec![0u8; 100]).unwrap();
        fs::write(g.join("setup-1.bin"), vec![0u8; 5000]).unwrap();

        let err = resolve_game(
            &entry(1, "TooBig", g, 5100, SplitPolicy::Never),
            SplitPolicy::Never,
            1000,
        );
        assert!(err.is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmatched_folders_get_distinct_local_ids() {
        let root = test_dir("locals");
        for name in ["LocalAlpha", "LocalBeta"] {
            let g = root.join(name);
            fs::create_dir_all(&g).unwrap();
            fs::write(g.join("data.bin"), b"hello").unwrap();
        }
        let history = BurnHistory::default();
        let on_list = std::collections::HashSet::new();
        let available = list_available_downloads(&root, &[], &history, &on_list);
        assert_eq!(available.len(), 2);
        assert!(available.iter().all(|a| is_local_game_id(a.game_id)));
        assert_ne!(available[0].game_id, available[1].game_id);
        assert!(available.iter().any(|a| a.title == "LocalAlpha"));
        assert!(available.iter().any(|a| a.title == "LocalBeta"));

        let entries: Vec<_> = available
            .iter()
            .map(|a| {
                entry(
                    a.game_id,
                    &a.title,
                    a.folder.clone(),
                    a.size_bytes,
                    SplitPolicy::Never,
                )
            })
            .collect();
        let plan = plan_homogeneous_discs(
            DiscMedia::Dvd5,
            &entries,
            SplitPolicy::Never,
            BurnOptions::default(),
        );
        assert!(plan.blockers.is_empty());
        assert_eq!(plan.discs[0].units.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}

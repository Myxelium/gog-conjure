use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use eframe::egui;
use tokio::sync::mpsc as tokio_mpsc;

use crate::auth::{self, AuthState, LoginOutcome};
use crate::config::AppConfig;
use crate::disc::{
    create_burner, folder_size, install_xorriso, is_local_game_id, list_available_downloads,
    plan_homogeneous_discs, plan_into_discs, planned_files_from_download_files,
    resolve_disc_file_paths, AvailableDownload, BurnEvent, BurnHistory, BurnListEntry, BurnOptions,
    BurnPlan, DiscBurner, DiscBurnStatus, DiscLayout, DiscMedia, DownloadReadiness, OpticalDrive,
    PackageManager, SplitPolicy,
};
use crate::download::{game_folder, DownloadQueue, JobStatus, QueueEvent, QueueItem};
use crate::gog::{DownloadFile, GameDetails, GogClient, LibraryGame};
use crate::images::ImageCache;
use crate::theme;
use crate::ui::{
    collect_filter_options, filter_details_files, BurnPanel, DownloadWhen, GameDetailPanel,
    LibraryPanel, LibraryPlanState, PlanModal, QueuePanel,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Queue,
    Burn,
}

enum AsyncMsg {
    Library(Result<Vec<LibraryGame>, String>),
    Details(Result<GameDetails, String>),
    LoginDone(Result<(), String>),
    Image {
        url: String,
        bytes: Result<Vec<u8>, String>,
    },
    BatchQueued {
        ok: usize,
        failed: usize,
        errors: Vec<String>,
        add_to_burn: bool,
        /// Files queued per game (for burn-list planned sizes when add_to_burn).
        burn_files: Vec<(u64, String, Vec<DownloadFile>)>,
        /// When false, leave the current tab (e.g. disc download from Burn).
        switch_tab: bool,
    },
    LibraryPlanDetails {
        details: Vec<GameDetails>,
        errors: Vec<String>,
    },
    XorrisoInstall(Result<String, String>),
}

pub struct ConjureApp {
    runtime: tokio::runtime::Runtime,
    config: AppConfig,
    auth: AuthState,
    client: GogClient,
    tab: Tab,
    library: Vec<LibraryGame>,
    library_filter: String,
    selected_game: Option<u64>,
    checked_games: HashSet<u64>,
    details: Option<GameDetails>,
    selected_files: HashSet<String>,
    images: ImageCache,
    loading_library: bool,
    loading_details: bool,
    batch_queueing: bool,
    status: String,
    logging_in: bool,
    login_rx: Option<mpsc::Receiver<LoginOutcome>>,
    async_rx: tokio_mpsc::UnboundedReceiver<AsyncMsg>,
    async_tx: tokio_mpsc::UnboundedSender<AsyncMsg>,
    queue: DownloadQueue,
    queue_items: Vec<QueueItem>,
    queue_rx: tokio_mpsc::UnboundedReceiver<QueueEvent>,
    burn_split: SplitPolicy,
    burn_list: Vec<BurnListEntry>,
    burn_plan: BurnPlan,
    burn_new_media: DiscMedia,
    burn_default_options: BurnOptions,
    burn_history: BurnHistory,
    burn_available: Vec<AvailableDownload>,
    burn_available_filter: String,
    burner: Box<dyn DiscBurner>,
    burn_drives: Vec<OpticalDrive>,
    burn_rx: Option<tokio_mpsc::UnboundedReceiver<BurnEvent>>,
    burn_cancel: Option<Arc<AtomicBool>>,
    burn_active_disc: Option<usize>,
    burn_log: String,
    burn_progress: Option<f32>,
    burn_progress_text: String,
    burn_was_simulate: bool,
    installing_xorriso: bool,
    library_plan: LibraryPlanState,
    /// Defer IMAPI drive scan until after the window exists (Windows COM safety).
    burn_drives_pending: bool,
}

impl ConjureApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        let config = AppConfig::load();
        let auth = AuthState::load();
        let client = GogClient::new(auth.clone());

        let (async_tx, async_rx) = tokio_mpsc::unbounded_channel();
        let (queue_tx, queue_rx) = tokio_mpsc::unbounded_channel();
        let queue = DownloadQueue::new(config.max_concurrent_downloads, queue_tx);

        let burner = create_burner();
        // Do not call list_drives() here. On Windows, IMAPI/COM during eframe creation has
        // aborted the process before the GUI appears; scan once the first frame runs.
        let burn_default_options = BurnOptions::default();

        let mut app = Self {
            runtime,
            config,
            auth,
            client,
            tab: Tab::Library,
            library: Vec::new(),
            library_filter: String::new(),
            selected_game: None,
            checked_games: HashSet::new(),
            details: None,
            selected_files: HashSet::new(),
            images: ImageCache::default(),
            loading_library: false,
            loading_details: false,
            batch_queueing: false,
            status: String::new(),
            logging_in: false,
            login_rx: None,
            async_rx,
            async_tx,
            queue,
            queue_items: Vec::new(),
            queue_rx,
            burn_split: SplitPolicy::WhenOversized,
            burn_list: Vec::new(),
            burn_plan: BurnPlan::default(),
            burn_new_media: DiscMedia::default_for_new(),
            burn_default_options,
            burn_history: BurnHistory::load(),
            burn_available: Vec::new(),
            burn_available_filter: String::new(),
            burner,
            burn_drives: Vec::new(),
            burn_rx: None,
            burn_cancel: None,
            burn_active_disc: None,
            burn_log: String::new(),
            burn_progress: None,
            burn_progress_text: String::new(),
            burn_was_simulate: false,
            installing_xorriso: false,
            library_plan: LibraryPlanState::default(),
            burn_drives_pending: true,
        };

        app.refresh_available_downloads();

        if app.auth.is_logged_in() {
            app.refresh_library();
        }

        app
    }

    fn refresh_library(&mut self) {
        self.loading_library = true;
        self.status = "Loading library…".into();
        let client = self.client.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let result = client
                .list_owned_games()
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AsyncMsg::Library(result));
        });
    }

    fn load_details(&mut self, id: u64) {
        self.loading_details = true;
        self.details = None;
        self.selected_files.clear();
        let client = self.client.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let result = client.game_details(id).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AsyncMsg::Details(result));
        });
    }

    fn request_image(&mut self, url: String) {
        if !self.images.request(url.clone()) {
            return;
        }
        let client = self.client.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let bytes = async {
                let resp = client
                    .http()
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
                resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
            }
            .await;
            let _ = tx.send(AsyncMsg::Image { url, bytes });
        });
    }

    fn start_login(&mut self) {
        self.logging_in = true;
        self.status = "Opening GOG login…".into();

        let (tx, rx) = mpsc::channel();
        self.login_rx = Some(rx);
        auth::begin_login(tx);
    }

    fn exchange_code(&mut self, code: String) {
        self.status = "Finishing login…".into();
        let client = self.client.clone();
        let auth = self.auth.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let result = async {
                let tokens = auth::exchange_code(client.http(), &code).await?;
                auth.set_tokens(tokens)?;
                Ok::<(), anyhow::Error>(())
            }
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AsyncMsg::LoginDone(result));
        });
    }

    fn pick_download_root(&mut self) {
        let folder = self.runtime.block_on(async {
            rfd::AsyncFileDialog::new()
                .set_title("Choose download folder")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf())
        });

        if let Some(path) = folder {
            self.config.download_root = Some(path);
            let _ = self.config.save();
        }
    }

    fn ensure_download_root(&mut self) -> Option<PathBuf> {
        if self.config.download_root.is_none() {
            self.pick_download_root();
        }
        self.config.download_root.clone()
    }

    fn enqueue_game_files(&mut self, game_id: u64, title: String, files: Vec<DownloadFile>) {
        let Some(root) = self.ensure_download_root() else {
            self.status = "Download folder required.".into();
            return;
        };

        self.queue.enqueue(
            self.client.clone(),
            game_id,
            title,
            files,
            root,
            self.runtime.handle(),
        );
        self.tab = Tab::Queue;
        self.status = "Added downloads to queue.".into();
    }

    fn enqueue_selected(&mut self, files: Vec<DownloadFile>) {
        let Some(game_id) = self.selected_game else {
            return;
        };
        let title = self
            .details
            .as_ref()
            .map(|d| d.title.clone())
            .or_else(|| {
                self.library
                    .iter()
                    .find(|g| g.id == game_id)
                    .map(|g| g.title.clone())
            })
            .unwrap_or_else(|| format!("game-{game_id}"));

        self.enqueue_game_files(game_id, title, files);
    }

    fn add_to_burn_list(&mut self, game_id: u64, title: String) {
        self.add_to_burn_list_with_files(game_id, title, Vec::new());
    }

    fn add_to_burn_list_with_files(
        &mut self,
        game_id: u64,
        title: String,
        files: Vec<DownloadFile>,
    ) {
        let Some(root) = self.config.download_root.clone() else {
            return;
        };
        let folder = game_folder(&root, &title);
        let planned = planned_files_from_download_files(&files);
        let planned_size: u64 = planned.iter().map(|f| f.size_bytes).sum();
        let disk_size = if folder.is_dir() {
            folder_size(&folder)
        } else {
            0
        };
        let size = if disk_size > 0 {
            disk_size
        } else {
            planned_size
        };
        if game_id != 0 {
            self.burn_history.remember_download(game_id, title.clone());
            let _ = self.burn_history.save();
        }

        if let Some(existing) = self.burn_list.iter_mut().find(|e| e.game_id == game_id) {
            // Re-add: refresh size/folder/files in place; no duplicates.
            existing.title = title;
            existing.folder = folder;
            existing.size_bytes = size;
            existing.included = true;
            if !planned.is_empty() {
                existing.planned_files = planned;
            }
        } else {
            self.burn_list.push(BurnListEntry {
                game_id,
                title,
                folder,
                size_bytes: size,
                readiness: DownloadReadiness::Pending,
                split_override: None,
                included: true,
                planned_files: planned,
            });
        }
        self.refresh_burn_readiness();
        self.refresh_available_downloads();
    }

    fn add_available_to_burn_list(&mut self, index: usize) {
        let Some(game) = self.burn_available.get(index).cloned() else {
            return;
        };
        self.add_available_download(game);
        self.status = "Added to burn list.".into();
    }

    fn add_available_download(&mut self, game: AvailableDownload) {
        self.add_to_burn_list(game.game_id, game.title);
        // Already-downloaded folders should show Ready immediately.
        if let Some(entry) = self
            .burn_list
            .iter_mut()
            .find(|e| e.game_id == game.game_id)
        {
            entry.folder = game.folder;
            entry.size_bytes = game.size_bytes;
            if game.size_bytes > 0 {
                entry.readiness = DownloadReadiness::Ready;
            }
        }
    }

    fn add_all_available_to_burn_list(&mut self, indices: &[usize]) {
        let games: Vec<AvailableDownload> = indices
            .iter()
            .filter_map(|idx| self.burn_available.get(*idx).cloned())
            .collect();
        let n = games.len();
        for game in games {
            self.add_available_download(game);
        }
        if n > 0 {
            self.status = format!("Added {n} download(s) to the burn list.");
        }
    }

    fn refresh_available_downloads(&mut self) {
        let root = self
            .config
            .download_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let library: Vec<(u64, String)> = self
            .library
            .iter()
            .map(|g| (g.id, g.title.clone()))
            .collect();
        let on_list: HashSet<u64> = self.burn_list.iter().map(|e| e.game_id).collect();
        self.burn_available =
            list_available_downloads(&root, &library, &self.burn_history, &on_list);
        // Folder equality as a safety net if ids ever diverge mid-session.
        for avail in &mut self.burn_available {
            if !avail.on_burn_list {
                avail.on_burn_list = self
                    .burn_list
                    .iter()
                    .any(|e| e.folder == avail.folder);
            }
            avail.burned = self.burn_history.is_burned(avail.game_id);
        }
    }

    fn refresh_burn_readiness(&mut self) {
        let mut history_dirty = false;
        for entry in &mut self.burn_list {
            let jobs: Vec<_> = self
                .queue_items
                .iter()
                .filter(|j| j.game_id == entry.game_id)
                .collect();

            let readiness = if jobs
                .iter()
                .any(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
            {
                DownloadReadiness::Downloading
            } else if jobs.iter().any(|j| j.status == JobStatus::Failed)
                && !jobs.iter().any(|j| j.status == JobStatus::Completed)
            {
                DownloadReadiness::Failed
            } else if entry.folder.is_dir() && folder_size(&entry.folder) > 0 {
                DownloadReadiness::Ready
            } else if jobs.iter().all(|j| j.status == JobStatus::Completed) && !jobs.is_empty() {
                DownloadReadiness::Ready
            } else if jobs.is_empty() {
                DownloadReadiness::Pending
            } else {
                DownloadReadiness::Pending
            };

            entry.readiness = readiness;
            if entry.folder.is_dir() {
                let disk = folder_size(&entry.folder);
                if disk > 0 {
                    entry.size_bytes = disk;
                } else if !entry.planned_files.is_empty() {
                    entry.size_bytes = entry.planned_size();
                }
            } else if !entry.planned_files.is_empty() {
                entry.size_bytes = entry.planned_size();
            }
            if readiness == DownloadReadiness::Ready {
                self.burn_history
                    .remember_download(entry.game_id, entry.title.clone());
                history_dirty = true;
            }
        }
        if history_dirty {
            let _ = self.burn_history.save();
        }
    }

    /// Fetch details for every checked game and queue all installers + extras.
    fn queue_checked_games(&mut self, add_to_burn: bool) {
        if self.checked_games.is_empty() || self.batch_queueing {
            return;
        }
        let Some(root) = self.ensure_download_root() else {
            self.status = "Download folder required.".into();
            return;
        };

        let jobs: Vec<(u64, String)> = self
            .library
            .iter()
            .filter(|g| self.checked_games.contains(&g.id))
            .map(|g| (g.id, g.title.clone()))
            .collect();

        if jobs.is_empty() {
            return;
        }

        self.batch_queueing = true;
        self.status = format!("Queueing all files for {} game(s)…", jobs.len());

        let client = self.client.clone();
        let queue = self.queue.clone();
        let handle = self.runtime.handle().clone();
        let tx = self.async_tx.clone();

        self.runtime.spawn(async move {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut errors = Vec::new();
            let mut burn_files: Vec<(u64, String, Vec<DownloadFile>)> = Vec::new();

            for (game_id, title) in jobs {
                match client.game_details(game_id).await {
                    Ok(details) => {
                        let files: Vec<DownloadFile> = details
                            .installers
                            .into_iter()
                            .chain(details.extras.into_iter())
                            .collect();
                        if files.is_empty() {
                            failed += 1;
                            errors.push(format!("{title}: no downloadable files"));
                            continue;
                        }
                        if add_to_burn {
                            burn_files.push((game_id, title.clone(), files.clone()));
                        }
                        queue.enqueue(
                            client.clone(),
                            game_id,
                            title,
                            files,
                            root.clone(),
                            &handle,
                        );
                        ok += 1;
                    }
                    Err(err) => {
                        failed += 1;
                        errors.push(format!("{title}: {err:#}"));
                    }
                }
            }

            let _ = tx.send(AsyncMsg::BatchQueued {
                ok,
                failed,
                errors,
                add_to_burn,
                burn_files,
                switch_tab: true,
            });
        });
    }

    fn prefer_host_os() -> &'static str {
        match std::env::consts::OS {
            "macos" => "mac",
            other => other,
        }
    }

    fn open_library_plan(&mut self) {
        if self.checked_games.is_empty() || self.library_plan.loading {
            return;
        }
        let jobs: Vec<(u64, String)> = self
            .library
            .iter()
            .filter(|g| self.checked_games.contains(&g.id))
            .map(|g| (g.id, g.title.clone()))
            .collect();
        if jobs.is_empty() {
            return;
        }

        self.library_plan
            .reset_for_open(self.burn_new_media, Self::prefer_host_os(), jobs.len());
        self.status = format!("Loading sizes for {} game(s)…", jobs.len());

        let client = self.client.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let mut details = Vec::new();
            let mut errors = Vec::new();
            for (game_id, title) in jobs {
                match client.game_details(game_id).await {
                    Ok(d) => details.push(d),
                    Err(err) => errors.push(format!("{title}: {err:#}")),
                }
            }
            let _ = tx.send(AsyncMsg::LibraryPlanDetails { details, errors });
        });
    }

    fn rebuild_library_plan_preview(&mut self) {
        if self.library_plan.details.is_empty() {
            self.library_plan.preview = None;
            self.library_plan.preview_total_bytes = 0;
            return;
        }

        let root = self
            .config
            .download_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        let mut entries = Vec::new();
        let mut total = 0u64;
        let mut empty = 0usize;

        for details in &self.library_plan.details {
            let files = filter_details_files(
                details,
                &self.library_plan.os,
                &self.library_plan.language,
                self.library_plan.extras,
            );
            if files.is_empty() {
                empty += 1;
                continue;
            }
            let planned = planned_files_from_download_files(&files);
            let size: u64 = planned.iter().map(|f| f.size_bytes).sum();
            total += size;
            entries.push(BurnListEntry {
                game_id: details.id,
                title: details.title.clone(),
                folder: game_folder(&root, &details.title),
                size_bytes: size,
                readiness: DownloadReadiness::Pending,
                split_override: None,
                included: true,
                planned_files: planned,
            });
        }

        if empty > 0 && entries.is_empty() {
            self.library_plan.error = Some(format!(
                "No files match the current OS/language/extras filters ({empty} game(s))."
            ));
            self.library_plan.preview = None;
            self.library_plan.preview_total_bytes = 0;
            return;
        }

        self.library_plan.error = None;

        let mut options = self.burn_default_options.clone();
        if options.drive.is_empty() {
            if let Some(first) = self.burn_drives.first() {
                options.drive = first.path.clone();
            }
        }

        let mut plan =
            plan_homogeneous_discs(self.library_plan.media, &entries, self.burn_split, options);
        if empty > 0 {
            plan.warnings.insert(
                0,
                format!("{empty} game(s) have no files matching the current filters."),
            );
        }
        self.library_plan.preview_total_bytes = total;
        self.library_plan.preview = Some(plan);
    }

    fn confirm_library_plan(&mut self) {
        let Some(preview) = self.library_plan.preview.clone() else {
            return;
        };
        if preview.discs.is_empty() || !preview.blockers.is_empty() {
            self.status = "Cannot add plan — fix blockers first.".into();
            return;
        }

        if self.library_plan.download_when == DownloadWhen::Now
            && self.ensure_download_root().is_none()
        {
            return;
        }
        if self.config.download_root.is_none() {
            // Still need a folder path for burn-list entries.
            if self.ensure_download_root().is_none() {
                return;
            }
        }

        let download_now = self.library_plan.download_when == DownloadWhen::Now;
        let mut queued = 0usize;

        for details in self.library_plan.details.clone() {
            let files = filter_details_files(
                &details,
                &self.library_plan.os,
                &self.library_plan.language,
                self.library_plan.extras,
            );
            if files.is_empty() {
                continue;
            }
            self.add_to_burn_list_with_files(details.id, details.title.clone(), files.clone());
            if download_now {
                let Some(root) = self.config.download_root.clone() else {
                    break;
                };
                self.queue.enqueue(
                    self.client.clone(),
                    details.id,
                    details.title.clone(),
                    files,
                    root,
                    self.runtime.handle(),
                );
                queued += 1;
            }
        }

        let start_index = self.burn_plan.discs.len();
        for mut disc in preview.discs {
            disc.index = start_index + disc.index;
            if disc.options.drive.is_empty() {
                disc.options = self.burn_default_options.clone();
                if disc.options.drive.is_empty() {
                    if let Some(first) = self.burn_drives.first() {
                        disc.options.drive = first.path.clone();
                    }
                }
            }
            self.burn_plan.discs.push(disc);
        }

        let n = self.burn_plan.discs.len().saturating_sub(start_index);
        self.tab = Tab::Burn;
        self.status = if download_now {
            format!("Added {n} disc(s) to Burn and queued {queued} game(s).")
        } else {
            format!("Added {n} disc(s) to Burn (download later).")
        };
        self.library_plan.close();
        self.refresh_burn_readiness();
    }

    fn plan_burn_discs(&mut self) {
        self.refresh_burn_readiness();
        if self.burn_plan.discs.is_empty() {
            self.status = "Add at least one disc before planning.".into();
            return;
        }
        // Preserve disc shells (media + options + manual volids).
        let shells = self.burn_plan.discs.clone();
        let plan = plan_into_discs(shells, &self.burn_list, self.burn_split);
        let filled = plan.discs.iter().filter(|d| !d.units.is_empty()).count();
        let warnings = plan.warnings.len();
        let blockers = plan.blockers.len();
        self.burn_plan = plan;
        self.status = format!(
            "Planned {filled} filled disc(s){}{}",
            if warnings > 0 {
                format!(" · {warnings} notice(s)")
            } else {
                String::new()
            },
            if blockers > 0 {
                format!(" · {blockers} blocker(s)")
            } else {
                String::new()
            }
        );
    }

    fn add_burn_disc(&mut self) {
        let index = self.burn_plan.discs.len();
        let mut options = self.burn_default_options.clone();
        if options.drive.is_empty() {
            if let Some(first) = self.burn_drives.first() {
                options.drive = first.path.clone();
            }
        }
        self.burn_plan
            .discs
            .push(DiscLayout::new_empty(index, self.burn_new_media, options));
        self.status = format!(
            "Added disc {} ({})",
            index + 1,
            self.burn_new_media.short_label()
        );
    }

    fn remove_burn_disc(&mut self, index: usize) {
        if self.burn_rx.is_some() {
            self.status = "Cannot remove a disc while burning.".into();
            return;
        }
        if index >= self.burn_plan.discs.len() {
            return;
        }
        self.burn_plan.discs.remove(index);
        for (i, disc) in self.burn_plan.discs.iter_mut().enumerate() {
            disc.index = i;
        }
        self.status = "Disc removed.".into();
    }

    fn start_xorriso_install(&mut self) {
        if self.installing_xorriso {
            return;
        }
        self.installing_xorriso = true;
        self.status =
            "Installing xorriso… approve the system password prompt if one appears.".into();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            // Package managers + pkexec are blocking; keep the UI responsive.
            let result = tokio::task::spawn_blocking(install_xorriso)
                .await
                .unwrap_or_else(|e| Err(format!("install task failed: {e}")));
            let _ = tx.send(AsyncMsg::XorrisoInstall(result));
        });
    }

    fn refresh_burn_drives(&mut self) {
        match self.burner.list_drives() {
            Ok(drives) => {
                self.burn_drives = drives;
                if self.burn_default_options.drive.is_empty()
                    || !self
                        .burn_drives
                        .iter()
                        .any(|d| d.path == self.burn_default_options.drive)
                {
                    if let Some(first) = self.burn_drives.first() {
                        self.burn_default_options.drive = first.path.clone();
                    }
                }
                for disc in &mut self.burn_plan.discs {
                    if disc.options.drive.is_empty()
                        || !self
                            .burn_drives
                            .iter()
                            .any(|d| d.path == disc.options.drive)
                    {
                        if let Some(first) = self.burn_drives.first() {
                            disc.options.drive = first.path.clone();
                        }
                    }
                }
                self.status = format!("Found {} optical drive(s).", self.burn_drives.len());
            }
            Err(err) => {
                self.burn_drives.clear();
                self.status = format!(
                    "Could not list optical drives (IMAPI). App will keep running. {err}"
                );
            }
        }
    }

    /// Queue downloads for games on a disc that are not Ready or already Downloading.
    fn download_disc_games(&mut self, disc_index: usize) {
        if self.batch_queueing {
            return;
        }
        let Some(root) = self.ensure_download_root() else {
            return;
        };
        let Some(disc) = self.burn_plan.discs.get(disc_index) else {
            return;
        };

        let mut jobs: Vec<(u64, String, Vec<String>)> = Vec::new();
        let mut seen = HashSet::new();
        for unit in &disc.units {
            let entry = self
                .burn_list
                .iter()
                .find(|e| e.game_id == unit.game_id);
            let Some(entry) = entry else {
                continue;
            };
            // Local folders are on-disk only — never call the GOG API for them.
            if is_local_game_id(entry.game_id) || entry.game_id == 0 {
                continue;
            }
            if matches!(
                entry.readiness,
                DownloadReadiness::Ready | DownloadReadiness::Downloading
            ) {
                continue;
            }
            if !seen.insert(entry.game_id) {
                continue;
            }
            let planned_names: Vec<String> = entry
                .planned_files
                .iter()
                .map(|f| f.relative_name.clone())
                .collect();
            jobs.push((entry.game_id, entry.title.clone(), planned_names));
        }

        if jobs.is_empty() {
            self.status = "Nothing to download — games are ready or already downloading.".into();
            return;
        }

        // Skip files already queued/running for these games.
        let active_keys: HashSet<(u64, String)> = self
            .queue_items
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
            .map(|j| (j.game_id, j.file.name.to_lowercase()))
            .collect();

        self.batch_queueing = true;
        self.status = format!(
            "Queueing downloads for {} game(s) on disc {}…",
            jobs.len(),
            disc_index + 1
        );

        let client = self.client.clone();
        let queue = self.queue.clone();
        let handle = self.runtime.handle().clone();
        let tx = self.async_tx.clone();

        self.runtime.spawn(async move {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut errors = Vec::new();

            for (game_id, title, planned_names) in jobs {
                match client.game_details(game_id).await {
                    Ok(details) => {
                        let mut files: Vec<DownloadFile> = details
                            .installers
                            .into_iter()
                            .chain(details.extras.into_iter())
                            .collect();
                        if !planned_names.is_empty() {
                            files.retain(|f| {
                                planned_names
                                    .iter()
                                    .any(|n| n.eq_ignore_ascii_case(&f.name))
                            });
                        }
                        files.retain(|f| {
                            !active_keys.contains(&(game_id, f.name.to_lowercase()))
                        });
                        if files.is_empty() {
                            failed += 1;
                            errors.push(format!("{title}: no matching files to download"));
                            continue;
                        }
                        queue.enqueue(
                            client.clone(),
                            game_id,
                            title,
                            files,
                            root.clone(),
                            &handle,
                        );
                        ok += 1;
                    }
                    Err(err) => {
                        failed += 1;
                        errors.push(format!("{title}: {err:#}"));
                    }
                }
            }

            let _ = tx.send(AsyncMsg::BatchQueued {
                ok,
                failed,
                errors,
                add_to_burn: false,
                burn_files: Vec::new(),
                switch_tab: false,
            });
        });
    }

    fn start_disc_burn(&mut self, disc_index: usize) {
        if self.burn_rx.is_some() {
            self.status = "A burn is already in progress.".into();
            return;
        }
        if !self.burn_plan.blockers.is_empty() {
            self.status = "Cannot burn while the plan has blockers.".into();
            return;
        }
        let Some(disc) = self.burn_plan.discs.get(disc_index).cloned() else {
            return;
        };
        if disc.units.is_empty() {
            self.status = "Disc is empty — Plan first.".into();
            return;
        }

        for unit in &disc.units {
            let ready = self
                .burn_list
                .iter()
                .find(|e| e.game_id == unit.game_id)
                .map(|e| e.readiness == DownloadReadiness::Ready)
                .unwrap_or_else(|| {
                    // Available download only — treat existing folder as ready.
                    true
                });
            if !ready {
                self.status = format!(
                    "Cannot burn: '{}' download is not complete.",
                    unit.game_title
                );
                return;
            }
        }

        let mut folders: Vec<(u64, PathBuf)> = self
            .burn_list
            .iter()
            .map(|e| (e.game_id, e.folder.clone()))
            .collect();
        // Include available download folders for games not on list matching.
        for avail in &self.burn_available {
            if !folders.iter().any(|(id, p)| *id == avail.game_id || p == &avail.folder) {
                folders.push((avail.game_id, avail.folder.clone()));
            }
        }

        let mut disc = disc;
        if let Err(err) = resolve_disc_file_paths(&mut disc, &folders) {
            self.status = format!("Cannot start burn: {err}");
            return;
        }

        let options = disc.options.clone();
        if let Err(err) = self.burner.build_burn_command(&disc, &options, &folders) {
            self.status = format!("Cannot start burn: {err}");
            return;
        }

        if let Some(d) = self.burn_plan.discs.get_mut(disc_index) {
            d.status = DiscBurnStatus::Burning;
            d.last_error = None;
            // Keep resolved paths on the plan for the active burn.
            d.units = disc.units.clone();
        }

        let (tx, rx) = tokio_mpsc::unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.burn_rx = Some(rx);
        self.burn_cancel = Some(cancel.clone());
        self.burn_active_disc = Some(disc_index);
        self.burn_log.clear();
        self.burn_progress = Some(0.02);
        self.burn_progress_text = if options.simulate {
            "Simulating…".into()
        } else {
            "Starting burn…".into()
        };
        self.burn_was_simulate = options.simulate;
        self.status = if options.simulate {
            format!("Simulating disc {}…", disc_index + 1)
        } else {
            format!("Burning disc {}…", disc_index + 1)
        };

        self.burner
            .start_burn_job(&disc, &options, &folders, tx, cancel);
    }

    fn poll_channels(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.async_rx.try_recv() {
            match msg {
                AsyncMsg::Library(Ok(games)) => {
                    self.library = games;
                    self.loading_library = false;
                    self.status = format!("Loaded {} games.", self.library.len());
                    self.refresh_available_downloads();
                }
                AsyncMsg::Library(Err(err)) => {
                    self.loading_library = false;
                    self.status = format!("Library error: {err}");
                }
                AsyncMsg::Details(Ok(details)) => {
                    let host_os = std::env::consts::OS;
                    let prefer = match host_os {
                        "macos" => "mac",
                        other => other,
                    };
                    self.selected_files.clear();
                    for file in &details.installers {
                        if file.os.as_deref() == Some(prefer) {
                            self.selected_files.insert(file.id.clone());
                        }
                    }
                    self.details = Some(details);
                    self.loading_details = false;
                }
                AsyncMsg::Details(Err(err)) => {
                    self.loading_details = false;
                    self.status = format!("Details error: {err}");
                }
                AsyncMsg::LoginDone(Ok(())) => {
                    self.logging_in = false;
                    self.status = "Logged in.".into();
                    self.refresh_library();
                }
                AsyncMsg::LoginDone(Err(err)) => {
                    self.logging_in = false;
                    self.status = format!("Login failed: {err}");
                }
                AsyncMsg::Image { url, bytes } => match bytes {
                    Ok(data) => {
                        let _ = self.images.insert_bytes(ctx, url, &data);
                    }
                    Err(_) => self.images.mark_failed(&url),
                },
                AsyncMsg::XorrisoInstall(result) => {
                    self.installing_xorriso = false;
                    match result {
                        Ok(msg) => {
                            self.burner = create_burner();
                            self.refresh_burn_drives();
                            self.status = msg;
                        }
                        Err(err) => {
                            self.status = format!("xorriso install failed: {err}");
                        }
                    }
                }
                AsyncMsg::BatchQueued {
                    ok,
                    failed,
                    errors,
                    add_to_burn,
                    burn_files,
                    switch_tab,
                } => {
                    self.batch_queueing = false;
                    if add_to_burn {
                        for (game_id, title, files) in burn_files {
                            self.add_to_burn_list_with_files(game_id, title, files);
                        }
                    }
                    self.refresh_burn_readiness();
                    if failed == 0 {
                        self.status = format!("Queued all files for {ok} game(s).");
                    } else {
                        let detail = errors.into_iter().take(3).collect::<Vec<_>>().join(" · ");
                        self.status = format!(
                            "Queued {ok} game(s), {failed} failed{}",
                            if detail.is_empty() {
                                String::new()
                            } else {
                                format!(": {detail}")
                            }
                        );
                    }
                    if ok > 0 && switch_tab {
                        self.tab = if add_to_burn { Tab::Burn } else { Tab::Queue };
                    }
                }
                AsyncMsg::LibraryPlanDetails { details, errors } => {
                    self.library_plan.loading = false;
                    if details.is_empty() {
                        self.library_plan.error = Some(if errors.is_empty() {
                            "No game details returned.".into()
                        } else {
                            errors.into_iter().take(3).collect::<Vec<_>>().join(" · ")
                        });
                        self.library_plan.preview = None;
                    } else {
                        if !errors.is_empty() {
                            self.status = format!(
                                "Plan: loaded {} game(s), {} failed",
                                details.len(),
                                errors.len()
                            );
                        }
                        self.library_plan.details = details;
                        // Keep preferred OS if present; otherwise All.
                        let (oses, _) =
                            collect_filter_options(&self.library_plan.details, None);
                        self.library_plan.available_os = oses;
                        if self.library_plan.os != "All"
                            && !self
                                .library_plan
                                .available_os
                                .iter()
                                .any(|o| o == &self.library_plan.os)
                        {
                            self.library_plan.os = "All".into();
                        }
                        self.library_plan.refresh_filter_options();
                        self.rebuild_library_plan_preview();
                    }
                }
            }
        }

        if let Some(rx) = &self.login_rx {
            match rx.try_recv() {
                Ok(LoginOutcome::Code(code)) => {
                    self.login_rx = None;
                    self.status = "Authorization received — finishing login…".into();
                    self.exchange_code(code);
                }
                Ok(LoginOutcome::Error(err)) => {
                    self.login_rx = None;
                    self.logging_in = false;
                    self.status = format!("Login failed: {err}");
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.login_rx = None;
                    self.logging_in = false;
                }
            }
        }

        while let Ok(QueueEvent::Updated(item)) = self.queue_rx.try_recv() {
            if let Some(existing) = self.queue_items.iter_mut().find(|i| i.id == item.id) {
                *existing = item;
            } else {
                self.queue_items.push(item);
            }
        }
        let live = self.queue.items();
        if !live.is_empty()
            || self.queue_items.iter().any(|i| {
                matches!(i.status, JobStatus::Queued | JobStatus::Running)
            })
        {
            self.queue_items = live;
        }
        self.refresh_burn_readiness();

        let mut burn_finished: Option<Result<(), String>> = None;
        if let Some(rx) = &mut self.burn_rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    BurnEvent::Log(line) => {
                        if !self.burn_log.is_empty() {
                            self.burn_log.push('\n');
                        }
                        self.burn_log.push_str(&line);
                        if self.burn_log.len() > 32_000 {
                            let keep = self.burn_log.split_off(self.burn_log.len() - 24_000);
                            self.burn_log = keep;
                        }
                    }
                    BurnEvent::Progress { fraction, message } => {
                        self.burn_progress = Some(fraction);
                        self.burn_progress_text = message.clone();
                        if !self.burn_log.is_empty() {
                            self.burn_log.push('\n');
                        }
                        self.burn_log.push_str(&message);
                    }
                    BurnEvent::Finished(result) => {
                        burn_finished = Some(result);
                    }
                }
            }
        }

        if let Some(result) = burn_finished {
            let disc_index = self.burn_active_disc;
            let was_simulate = self.burn_was_simulate;
            self.burn_rx = None;
            self.burn_cancel = None;
            self.burn_active_disc = None;
            self.burn_was_simulate = false;
            if let Some(idx) = disc_index {
                if let Some(disc) = self.burn_plan.discs.get_mut(idx) {
                    match &result {
                        Ok(()) => {
                            disc.status = DiscBurnStatus::Done;
                            disc.last_error = None;
                            self.burn_progress = Some(1.0);
                            self.burn_progress_text = if was_simulate {
                                "Simulate finished.".into()
                            } else {
                                "Burn finished.".into()
                            };
                            if was_simulate {
                                self.status = format!(
                                    "Disc {} simulate OK (no disc was written).",
                                    idx + 1
                                );
                            } else {
                                let game_ids: Vec<u64> =
                                    disc.units.iter().map(|u| u.game_id).collect();
                                self.status = format!("Disc {} burned successfully.", idx + 1);
                                self.burn_history.mark_burned(game_ids);
                                let _ = self.burn_history.save();
                                self.refresh_available_downloads();
                            }
                        }
                        Err(err) => {
                            disc.status = DiscBurnStatus::Failed;
                            disc.last_error = Some(err.clone());
                            self.burn_progress_text = "Failed.".into();
                            self.status = format!("Disc {} burn failed: {err}", idx + 1);
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for ConjureApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Scan optical drives only once the Burn tab is used — keeps a broken IMAPI stack
        // from taking down library/download flows at startup.
        if self.burn_drives_pending && self.tab == Tab::Burn {
            self.burn_drives_pending = false;
            self.refresh_burn_drives();
        }
        self.poll_channels(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(theme::BRAND);
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Library, "Library");
                ui.selectable_value(&mut self.tab, Tab::Queue, "Queue");
                ui.selectable_value(&mut self.tab, Tab::Burn, "Burn");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.auth.is_logged_in() {
                        if ui.button("Log out").clicked() {
                            let _ = self.auth.clear();
                            self.library.clear();
                            self.details = None;
                            self.checked_games.clear();
                            self.status = "Logged out.".into();
                        }
                        if ui.button("Refresh").clicked() {
                            self.refresh_library();
                        }
                    } else if ui.button("Login with GOG").clicked() {
                        self.start_login();
                    }

                    let folder_label = self
                        .config
                        .download_root
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Download folder…".into());
                    if ui.button(folder_label).clicked() {
                        self.pick_download_root();
                    }
                });
            });
            if !self.status.is_empty() {
                ui.small(&self.status);
            }
            ui.add_space(4.0);
        });

        if !self.auth.is_logged_in() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.heading(theme::BRAND);
                    ui.label(theme::TAGLINE);
                    ui.add_space(20.0);
                    let login_enabled = !self.logging_in;
                    if ui
                        .add_enabled(
                            login_enabled,
                            egui::Button::new(if self.logging_in {
                                "Waiting for login…"
                            } else {
                                "Login with GOG"
                            })
                            .min_size(theme::HERO_BTN),
                        )
                        .clicked()
                    {
                        self.start_login();
                    }

                    if self.logging_in {
                        ui.add_space(16.0);
                        ui.spinner();
                        ui.label("Sign in inside the GOG login window.");
                        ui.small("When authorization finishes, this app continues automatically.");
                    }
                });
            });
            return;
        }

        match self.tab {
            Tab::Library => {
                let mut image_requests = Vec::new();
                let mut download_selected = false;
                let mut plan_selected = false;
                let mut select_all = false;
                let mut clear_checks = false;

                egui::SidePanel::left("library_panel")
                    .resizable(true)
                    .default_width(260.0)
                    .width_range(200.0..=320.0)
                    .show(ctx, |ui| {
                        if self.loading_library {
                            ui.spinner();
                        }
                        if self.batch_queueing {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Queueing selected games…");
                            });
                        }

                        let prev = self.selected_game;
                        let actions = LibraryPanel::show(
                            ui,
                            &self.library,
                            &mut self.library_filter,
                            &mut self.selected_game,
                            &mut self.checked_games,
                            &mut self.images,
                            &mut |url| image_requests.push(url),
                        );
                        select_all = actions.select_all_filtered;
                        clear_checks = actions.clear_checks;
                        download_selected = actions.download_selected;
                        plan_selected = actions.plan_selected;

                        if self.selected_game != prev {
                            if let Some(id) = self.selected_game {
                                self.load_details(id);
                            }
                        }
                    });

                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut queued: Option<Vec<DownloadFile>> = None;
                    let library_game = self
                        .selected_game
                        .and_then(|id| self.library.iter().find(|g| g.id == id));
                    GameDetailPanel::show(
                        ui,
                        self.details.as_ref(),
                        library_game,
                        &mut self.selected_files,
                        self.loading_details,
                        &mut self.images,
                        &mut |url| image_requests.push(url),
                        &mut |files| queued = Some(files),
                    );
                    if let Some(files) = queued {
                        self.enqueue_selected(files);
                    }
                });

                for url in image_requests {
                    self.request_image(url);
                }

                if select_all {
                    let filter_lower = self.library_filter.to_lowercase();
                    for game in &self.library {
                        if filter_lower.is_empty()
                            || game.title.to_lowercase().contains(&filter_lower)
                        {
                            self.checked_games.insert(game.id);
                        }
                    }
                }
                if clear_checks {
                    self.checked_games.clear();
                }
                if download_selected {
                    self.queue_checked_games(false);
                }
                if plan_selected {
                    self.open_library_plan();
                }
            }
            Tab::Queue => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut cancel_id = None;
                    let mut clear = false;
                    QueuePanel::show(
                        ui,
                        &self.queue_items,
                        &mut |id| cancel_id = Some(id),
                        &mut || clear = true,
                    );
                    if let Some(id) = cancel_id {
                        self.queue.cancel(id);
                        self.queue_items = self.queue.items();
                    }
                    if clear {
                        self.queue.clear_finished();
                        self.queue_items = self.queue.items();
                    }
                });
            }
            Tab::Burn => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let unavailable = self.burner.unavailable_reason();
                    let burning = self.burn_rx.is_some();
                    let pkg = PackageManager::detect();
                    let can_install = cfg!(target_os = "linux")
                        && !self.burner.is_available()
                        && pkg.is_some()
                        && !self.installing_xorriso;
                    let install_hint = pkg.as_ref().map(|m| m.short_command());
                    let actions = BurnPanel::show(
                        ui,
                        &self.burn_available,
                        &mut self.burn_available_filter,
                        &mut self.burn_new_media,
                        &mut self.burn_split,
                        &mut self.burn_list,
                        &mut self.burn_plan,
                        &self.burn_drives,
                        self.burner.name(),
                        self.burner.is_available(),
                        unavailable.as_deref(),
                        burning,
                        self.burn_active_disc,
                        &self.burn_log,
                        self.burn_progress,
                        &self.burn_progress_text,
                        self.installing_xorriso,
                        can_install,
                        install_hint.as_deref(),
                    );

                    if actions.install_xorriso {
                        self.start_xorriso_install();
                    }
                    if actions.refresh_available {
                        self.refresh_available_downloads();
                    }
                    if actions.add_disc {
                        self.add_burn_disc();
                    }
                    if actions.plan {
                        self.plan_burn_discs();
                    }
                    if actions.clear_list {
                        self.burn_list.clear();
                        self.refresh_available_downloads();
                        self.status = "Burn list cleared.".into();
                    }
                    if actions.refresh_drives {
                        self.refresh_burn_drives();
                    }
                    if let Some(game_id) = actions.remove_from_list {
                        self.burn_list.retain(|e| e.game_id != game_id);
                        self.refresh_available_downloads();
                    }
                    if let Some(idx) = actions.add_available {
                        self.add_available_to_burn_list(idx);
                    }
                    if !actions.add_all_available.is_empty() {
                        self.add_all_available_to_burn_list(&actions.add_all_available);
                    }
                    if let Some(idx) = actions.remove_disc {
                        self.remove_burn_disc(idx);
                    }
                    if let Some(idx) = actions.download_disc {
                        self.download_disc_games(idx);
                    }
                    if let Some(idx) = actions.burn_disc {
                        self.start_disc_burn(idx);
                    }
                });
            }
        }

        if self.library_plan.open {
            let actions = PlanModal::show(ctx, &mut self.library_plan);
            if actions.filters_changed && !self.library_plan.loading {
                self.rebuild_library_plan_preview();
            }
            if actions.cancel {
                self.library_plan.close();
            }
            if actions.add_to_burn {
                self.confirm_library_plan();
            }
        }
    }
}

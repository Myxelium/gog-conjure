use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use eframe::egui;
use tokio::sync::mpsc as tokio_mpsc;

use crate::auth::{self, AuthState, LoginOutcome};
use crate::config::AppConfig;
use crate::disc::{
    create_burner, folder_size, install_xorriso, is_local_game_id, joliet_file_limit_error,
    list_available_downloads, plan_homogeneous_discs, plan_into_discs,
    planned_files_from_download_files, resolve_disc_file_paths, xorriso_udf_error,
    AvailableDownload, BurnEvent, BurnHistory, BurnListEntry, BurnOptions, BurnPlan,
    DiscBurnStatus, DiscBurner, DiscLayout, DiscMedia, DownloadReadiness, OpticalDrive,
    PackageManager, SplitPolicy,
};
use crate::download::{game_folder, DownloadQueue, JobStatus, QueueEvent, QueueItem};
use crate::gog::{DownloadFile, GameDetails, GogClient, LibraryGame};
use crate::images::{cover_url_candidates, direct_image_url, CoverSize, ImageCache};
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
        image: Result<egui::ColorImage, String>,
    },
    /// Galaxy backdrop URL resolved off the UI thread. `Err` means the details fetch failed.
    BackdropMeta {
        id: u64,
        result: Result<Option<String>, String>,
    },
    /// One finished blurred window texture, already decoded and blurred.
    BackdropReady {
        key: theme::BlurKey,
        image: Result<egui::ColorImage, String>,
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
    /// Covers decoded off the UI thread, uploaded a few at a time.
    cover_ready: VecDeque<(String, egui::ColorImage)>,
    /// Galaxy backdrop URL per game, once gameDetails has been read. `None` means no backdrop field.
    backdrop_urls: HashMap<u64, Option<String>>,
    backdrop_fetching: HashSet<u64>,
    backdrop_failed: HashSet<u64>,
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
            cover_ready: VecDeque::new(),
            backdrop_urls: HashMap::new(),
            backdrop_fetching: HashSet::new(),
            backdrop_failed: HashSet::new(),
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
        self.backdrop_failed.remove(&id);
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
            let fetched = async {
                let resp = client
                    .http()
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
                resp.bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| e.to_string())
            }
            .await;
            // JPEG decode is slow enough to freeze the window if it runs on the UI thread.
            let image = match fetched {
                Ok(bytes) => {
                    tokio::task::spawn_blocking(move || crate::images::decode_cover(&bytes))
                        .await
                        .unwrap_or_else(|err| Err(err.to_string()))
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(AsyncMsg::Image { url, image });
        });
    }

    fn store_background_url(&mut self, id: u64, raw: Option<String>) {
        let url = raw.as_deref().and_then(direct_image_url);
        self.backdrop_fetching.remove(&id);
        self.backdrop_failed.remove(&id);
        self.backdrop_urls.insert(id, url);
    }

    fn remember_background(&mut self, id: u64, raw: Option<String>) {
        self.store_background_url(id, raw);
        if let Some(url) = self.backdrop_urls.get(&id).cloned().flatten() {
            self.request_image(url);
        }
    }

    /// Queue Galaxy backdrop fetches for the selected game, then visible sidebar rows.
    fn prefetch_backdrops(&mut self, visible: &[u64]) {
        let mut order = Vec::new();
        if let Some(id) = self.selected_game {
            order.push(id);
        }
        for id in visible {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        for id in order {
            self.ensure_backdrop(id);
        }
    }

    fn ensure_backdrop(&mut self, id: u64) {
        if let Some(url) = self.backdrop_urls.get(&id).cloned() {
            if let Some(url) = url {
                self.request_image(url);
            }
            return;
        }
        if self.backdrop_fetching.contains(&id) || self.backdrop_failed.contains(&id) {
            return;
        }
        self.backdrop_fetching.insert(id);
        let client = self.client.clone();
        let tx = self.async_tx.clone();
        self.runtime.spawn(async move {
            let result = client
                .galaxy_background_url(id)
                .await
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(AsyncMsg::BackdropMeta { id, result });
        });
    }

    fn enqueue_blur(&mut self, job: theme::BlurJob) {
        let tx = self.async_tx.clone();
        let key = job.key;
        self.runtime.spawn(async move {
            let rendered = tokio::task::spawn_blocking(move || job.render()).await;
            let msg = match rendered {
                Ok(image) => AsyncMsg::BackdropReady {
                    key,
                    image: Ok(image),
                },
                Err(_) => AsyncMsg::BackdropReady {
                    key,
                    image: Err("blur worker stopped".into()),
                },
            };
            let _ = tx.send(msg);
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
                avail.on_burn_list = self.burn_list.iter().any(|e| e.folder == avail.folder);
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
                        queue.enqueue(client.clone(), game_id, title, files, root.clone(), &handle);
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
                let blank = disc.options.blank;
                let filesystem = disc.options.filesystem;
                disc.options = self.burn_default_options.clone();
                disc.options.blank = blank;
                disc.options.filesystem = filesystem;
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
        options.blank = self.burn_new_media.blank_default();
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
                self.status =
                    format!("Could not list optical drives. App will keep running. {err}");
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
            let entry = self.burn_list.iter().find(|e| e.game_id == unit.game_id);
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
                        files.retain(|f| !active_keys.contains(&(game_id, f.name.to_lowercase())));
                        if files.is_empty() {
                            failed += 1;
                            errors.push(format!("{title}: no matching files to download"));
                            continue;
                        }
                        queue.enqueue(client.clone(), game_id, title, files, root.clone(), &handle);
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

    fn reburn_recorded_disc(&mut self, record_index: usize) {
        if self.burn_rx.is_some() {
            self.status = "A burn is already in progress.".into();
            return;
        }
        let Some(record) = self.burn_history.discs.get(record_index).cloned() else {
            return;
        };
        if !record.media.allows_reburn() {
            self.status = "M-DISC DVD is write-once and cannot be reburned.".into();
            return;
        }

        let mut entries = Vec::new();
        for (i, game_id) in record.game_ids.iter().copied().enumerate() {
            let title = record
                .titles
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("game-{game_id}"));
            let folder = self
                .burn_list
                .iter()
                .find(|entry| entry.game_id == game_id)
                .map(|entry| entry.folder.clone())
                .or_else(|| {
                    self.burn_available
                        .iter()
                        .find(|avail| avail.game_id == game_id)
                        .map(|avail| avail.folder.clone())
                });
            let Some(folder) = folder else {
                self.status = format!("Cannot reburn: missing folder for '{title}'.");
                return;
            };
            if !folder.is_dir() {
                self.status = format!(
                    "Cannot reburn: folder not found for '{title}' ({}).",
                    folder.display()
                );
                return;
            }
            entries.push(BurnListEntry {
                game_id,
                title,
                folder: folder.clone(),
                size_bytes: folder_size(&folder),
                readiness: DownloadReadiness::Ready,
                split_override: None,
                included: true,
                planned_files: Vec::new(),
            });
        }
        if entries.is_empty() {
            self.status = "Cannot reburn: this disc record has no games.".into();
            return;
        }

        let mut options = self.burn_default_options.clone();
        if options.drive.is_empty() {
            if let Some(first) = self.burn_drives.first() {
                options.drive = first.path.clone();
            }
        }
        options.filesystem = record.filesystem;
        options.blank = true;
        options.simulate = false;
        let plan =
            plan_homogeneous_discs(record.media, &entries, SplitPolicy::WhenOversized, options);
        if !plan.blockers.is_empty() {
            self.status = format!("Cannot reburn: {}", plan.blockers.join(" · "));
            return;
        }
        if plan.discs.len() != 1 {
            self.status = format!(
                "Cannot reburn: the current files no longer fit on one {} disc.",
                record.media.short_label()
            );
            return;
        }

        let mut disc = plan.discs.into_iter().next().unwrap();
        disc.volid = record.volid.clone();
        disc.volid_manual = !record.volid.is_empty();
        disc.options.filesystem = record.filesystem;
        disc.options.blank = true;
        let index = self.burn_plan.discs.len();
        disc.index = index;
        self.burn_plan.discs.push(disc);
        self.start_disc_burn(index);
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
            if !folders
                .iter()
                .any(|(id, p)| *id == avail.game_id || p == &avail.folder)
            {
                folders.push((avail.game_id, avail.folder.clone()));
            }
        }

        let mut disc = disc;
        if let Err(err) = resolve_disc_file_paths(&mut disc, &folders) {
            self.status = format!("Cannot start burn: {err}");
            return;
        }
        if disc.options.filesystem == crate::disc::DiscFilesystem::Udf && cfg!(target_os = "linux")
        {
            self.status = xorriso_udf_error().into();
            return;
        }
        if cfg!(not(target_os = "linux")) {
            if let Some(err) = joliet_file_limit_error(&disc) {
                self.status = err;
                return;
            }
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

    fn upload_ready_covers(&mut self, ctx: &egui::Context) {
        const PER_FRAME: usize = 4;
        for _ in 0..PER_FRAME {
            let Some((url, image)) = self.cover_ready.pop_front() else {
                break;
            };
            self.images.insert_decoded(ctx, url, image);
        }
        if !self.cover_ready.is_empty() {
            ctx.request_repaint();
        }
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
                    if let Some(id) = self.selected_game {
                        self.backdrop_fetching.remove(&id);
                        self.backdrop_failed.insert(id);
                    }
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
                AsyncMsg::Image { url, image } => match image {
                    Ok(image) => self.cover_ready.push_back((url, image)),
                    Err(_) => self.images.mark_failed(&url),
                },
                AsyncMsg::BackdropMeta { id, result } => match result {
                    Ok(url) => self.remember_background(id, url),
                    Err(_) => {
                        self.backdrop_fetching.remove(&id);
                        self.backdrop_failed.insert(id);
                    }
                },
                AsyncMsg::BackdropReady { key, image } => match image {
                    Ok(image) => theme::install_backdrop(ctx, key, image),
                    Err(_) => theme::cancel_backdrop(ctx, key),
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
                        let (oses, _) = collect_filter_options(&self.library_plan.details, None);
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
            || self
                .queue_items
                .iter()
                .any(|i| matches!(i.status, JobStatus::Queued | JobStatus::Running))
        {
            self.queue_items = live;
        }
        self.refresh_burn_readiness();

        let mut burn_finished: Option<Result<(), String>> = None;
        if let Some(rx) = &mut self.burn_rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    BurnEvent::Log(line) => {
                        append_burn_log(&mut self.burn_log, &line);
                    }
                    BurnEvent::Progress { fraction, message } => {
                        self.burn_progress = Some(fraction);
                        self.burn_progress_text = message.clone();
                        append_burn_log(&mut self.burn_log, &message);
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
                                self.status =
                                    format!("Disc {} simulate OK (no disc was written).", idx + 1);
                            } else {
                                let mut game_ids = Vec::new();
                                for unit in &disc.units {
                                    if unit.game_id != 0 && !game_ids.contains(&unit.game_id) {
                                        game_ids.push(unit.game_id);
                                    }
                                }
                                self.status = format!("Disc {} burned successfully.", idx + 1);
                                self.burn_history.record_disc(crate::disc::BurnedDisc {
                                    volid: disc.volid.clone(),
                                    media: disc.media,
                                    filesystem: disc.options.filesystem,
                                    game_ids,
                                    titles: disc.game_titles(),
                                });
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

        self.upload_ready_covers(ctx);
    }
}

fn show_tab(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str) {
    let selected = *current == tab;
    let id = theme::hover_id(ui, label);
    let hovered = theme::was_hovered(ui, id);
    let response = ui.add(theme::nav_button(label, selected, hovered));
    theme::store_hover_rect(ui, id, response.rect);
    if response.clicked() {
        *current = tab;
    }
}

/// Leftover width in the top bar, after the title and nav tabs, before the folder
/// control switches from its path text to an icon. The sidebar is already outside
/// this panel on the Library tab, so this is the free space in the chrome itself.
const PATH_BUTTON_TEXT_MIN: f32 = 160.0;

#[derive(Default)]
struct ChromeClicks {
    logout: bool,
    refresh: bool,
    login: bool,
    pick_folder: bool,
}

fn chrome_button_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::FontId::new(15.0, egui::FontFamily::Proportional);
    let galley =
        ui.fonts(|fonts| fonts.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE));
    galley.size().x + theme::BUTTON_PADDING.x * 2.0 + 4.0
}

/// Width of Log out / Refresh (or Login) plus a folder icon. The path text is extra.
fn chrome_actions_min_width(ui: &egui::Ui, logged_in: bool) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    let folder = chrome_button_width(ui, "📁");
    if logged_in {
        chrome_button_width(ui, "Log out") + gap + chrome_button_width(ui, "Refresh") + gap + folder
    } else {
        chrome_button_width(ui, "Login with GOG") + gap + folder
    }
}

fn place_chrome_actions(ui: &mut egui::Ui, logged_in: bool, path: &str) -> ChromeClicks {
    let mut clicks = ChromeClicks::default();
    let clip = ui.available_rect_before_wrap();
    ui.set_clip_rect(clip);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if logged_in {
            if ui.button("Log out").clicked() {
                clicks.logout = true;
            }
            if ui.button("Refresh").clicked() {
                clicks.refresh = true;
            }
        } else if ui.button("Login with GOG").clicked() {
            clicks.login = true;
        }

        let response = if ui.available_width() < PATH_BUTTON_TEXT_MIN {
            ui.button("📁")
        } else {
            ui.add(egui::Button::new(path).truncate())
        };
        if response.on_hover_text(path).clicked() {
            clicks.pick_folder = true;
        }
    });
    clicks
}

enum BackdropChoice {
    /// Blurred texture is ready to become the window background.
    Image(std::sync::Arc<egui::ColorImage>),
    /// No game selected, or nothing to hold onto yet.
    Gradient,
    /// Keep the previous blurred frame until the next one is ready.
    Hold,
}

fn backdrop_choice(app: &ConjureApp) -> BackdropChoice {
    let Some(id) = app.selected_game else {
        return BackdropChoice::Gradient;
    };
    match app.backdrop_urls.get(&id) {
        Some(Some(url)) => {
            if let Some(pixels) = app.images.pixels(url) {
                BackdropChoice::Image(std::sync::Arc::clone(pixels))
            } else if app.images.has_failed(url) {
                cover_backdrop(app, id)
            } else {
                BackdropChoice::Hold
            }
        }
        Some(None) => cover_backdrop(app, id),
        None => {
            let waiting = app.backdrop_fetching.contains(&id)
                || (app.loading_details && app.selected_game == Some(id));
            if waiting {
                BackdropChoice::Hold
            } else {
                cover_backdrop(app, id)
            }
        }
    }
}

fn cover_backdrop(app: &ConjureApp, id: u64) -> BackdropChoice {
    if let Some((_, pixels)) = selected_cover(app, id) {
        BackdropChoice::Image(pixels)
    } else {
        BackdropChoice::Hold
    }
}

fn selected_cover(
    app: &ConjureApp,
    id: u64,
) -> Option<(egui::TextureHandle, std::sync::Arc<egui::ColorImage>)> {
    let mut sources = Vec::new();
    if let Some(game) = app.library.iter().find(|g| g.id == id) {
        if let Some(image) = game.image.as_deref() {
            sources.push(image.to_string());
        }
    }
    if let Some(details) = app.details.as_ref().filter(|d| d.id == id) {
        if let Some(image) = details.image.as_deref() {
            if !sources.iter().any(|s| s == image) {
                sources.push(image.to_string());
            }
        }
    }
    for image in &sources {
        for size in [CoverSize::Large, CoverSize::Thumb] {
            for url in cover_url_candidates(image, size) {
                if let (Some(tex), Some(pixels)) =
                    (app.images.texture(&url), app.images.pixels(&url))
                {
                    return Some((tex.clone(), std::sync::Arc::clone(pixels)));
                }
            }
        }
    }
    None
}

impl eframe::App for ConjureApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::AMBIENT_TOP.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Scan optical drives only once the Burn tab is used — keeps a broken IMAPI stack
        // from taking down library/download flows at startup.
        if self.burn_drives_pending && self.tab == Tab::Burn {
            self.burn_drives_pending = false;
            self.refresh_burn_drives();
        }
        self.poll_channels(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        match backdrop_choice(self) {
            BackdropChoice::Hold if theme::backdrop_is_showing(ctx) => {}
            BackdropChoice::Image(pixels) => {
                if let Some(job) = theme::sync_backdrop(ctx, theme::BackdropSource::Cover(&pixels))
                {
                    self.enqueue_blur(job);
                }
            }
            BackdropChoice::Gradient | BackdropChoice::Hold => {
                if let Some(job) = theme::sync_backdrop(ctx, theme::BackdropSource::Gradient) {
                    self.enqueue_blur(job);
                }
            }
        }
        theme::paint_window_backdrop(ctx);

        // Snapshot the tab so the sidebar and the page drawn this frame agree.
        // Clicks in the top chrome apply on the next frame.
        let frame_tab = self.tab;
        let mut image_requests = Vec::new();
        let mut visible_ids = Vec::new();
        let mut download_selected = false;
        let mut plan_selected = false;
        let mut select_all = false;
        let mut clear_checks = false;

        if self.auth.is_logged_in() && frame_tab == Tab::Library {
            egui::SidePanel::left("library_sidebar")
                .resizable(true)
                .default_width(440.0)
                .width_range(340.0..=640.0)
                .show_separator_line(false)
                .frame(theme::sidebar_frame())
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
                    visible_ids = actions.visible_ids;

                    if self.selected_game != prev {
                        if let Some(id) = self.selected_game {
                            self.load_details(id);
                        }
                    }
                });
        }

        egui::TopBottomPanel::top("top")
            .show_separator_line(false)
            .frame(theme::chrome_frame())
            .show(ctx, |ui| {
                let path_label = self
                    .config
                    .download_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Download folder…".into());
                let logged_in = self.auth.is_logged_in();
                let mut placed = false;
                let mut clicks = ChromeClicks::default();
                ui.horizontal(|ui| {
                    ui.heading(theme::header(theme::BRAND));
                    ui.separator();
                    show_tab(ui, &mut self.tab, Tab::Library, "Library");
                    show_tab(ui, &mut self.tab, Tab::Queue, "Queue");
                    show_tab(ui, &mut self.tab, Tab::Burn, "Burn");

                    if chrome_actions_min_width(ui, logged_in) <= ui.available_width() {
                        clicks = place_chrome_actions(ui, logged_in, &path_label);
                        placed = true;
                    }
                });
                if !placed {
                    ui.horizontal(|ui| {
                        clicks = place_chrome_actions(ui, logged_in, &path_label);
                    });
                }
                if clicks.logout {
                    let _ = self.auth.clear();
                    self.library.clear();
                    self.details = None;
                    self.checked_games.clear();
                    self.status = "Logged out.".into();
                }
                if clicks.refresh {
                    self.refresh_library();
                }
                if clicks.login {
                    self.start_login();
                }
                if clicks.pick_folder {
                    self.pick_download_root();
                }
                if !self.status.is_empty() {
                    ui.label(theme::meta(&self.status));
                }
            });

        if !self.auth.is_logged_in() {
            egui::CentralPanel::default()
                .frame(theme::content_frame())
                .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(48.0);
                    theme::glass_card(false, false).show(ui, |ui| {
                        ui.set_min_width(380.0);
                        ui.vertical_centered(|ui| {
                            ui.heading(theme::header(theme::BRAND));
                            ui.label(theme::meta(theme::TAGLINE));
                            ui.add_space(8.0);
                            let login_enabled = !self.logging_in;
                            let label = if self.logging_in {
                                "Waiting for login…"
                            } else {
                                "Login with GOG"
                            };
                            let id = theme::hover_id(ui, "hero_login");
                            let hovered = login_enabled && theme::was_hovered(ui, id);
                            let response = ui.add_enabled(
                                login_enabled,
                                theme::primary_button(label, hovered).min_size(theme::HERO_BTN),
                            );
                            theme::store_hover_rect(ui, id, response.rect);
                            if response.clicked() {
                                self.start_login();
                            }

                            if self.logging_in {
                                ui.add_space(8.0);
                                ui.spinner();
                                ui.label("Sign in inside the GOG login window.");
                                ui.label(theme::meta(
                                    "When authorization finishes, this app continues automatically.",
                                ));
                            }
                        });
                    });
                });
            });
            return;
        }

        match frame_tab {
            Tab::Library => {
                egui::CentralPanel::default()
                    .frame(theme::content_frame())
                    .show(ctx, |ui| {
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

                self.prefetch_backdrops(&visible_ids);
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
                egui::CentralPanel::default()
                    .frame(theme::content_frame())
                    .show(ctx, |ui| {
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
                egui::CentralPanel::default()
                    .frame(theme::content_frame())
                    .show(ctx, |ui| {
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
                            &self.burn_history.discs,
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
                        if let Some(idx) = actions.reburn {
                            self.reburn_recorded_disc(idx);
                        }
                    });
            }
        }

        if self.auth.is_logged_in() && frame_tab != Tab::Library {
            self.prefetch_backdrops(&[]);
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

const BURN_LOG_MAX_BYTES: usize = 32_000;
const BURN_LOG_KEEP_BYTES: usize = 24_000;

fn append_burn_log(log: &mut String, line: &str) {
    if !log.is_empty() {
        log.push('\n');
    }
    log.push_str(line);
    trim_burn_log_tail(log, BURN_LOG_MAX_BYTES, BURN_LOG_KEEP_BYTES);
}

/// Keep the newest tail once `log` exceeds `max_bytes`.
///
/// `String::split_off` panics unless the index is a UTF-8 character boundary.
/// Burn output includes multi-byte characters (the ellipsis in status lines,
/// non-ASCII game titles), so a raw `len - keep` cut crashes the UI on the
/// next disc once the log crosses 32KB. The burner thread keeps running.
fn trim_burn_log_tail(log: &mut String, max_bytes: usize, keep_bytes: usize) {
    if log.len() <= max_bytes {
        return;
    }
    let mut cut = log.len().saturating_sub(keep_bytes);
    while cut > 0 && !log.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 || cut >= log.len() {
        return;
    }
    let keep = log.split_off(cut);
    *log = keep;
}

#[cfg(test)]
mod tests {
    use super::trim_burn_log_tail;

    #[test]
    fn trim_burn_log_cuts_on_char_boundary() {
        // 8000 ASCII bytes, a 3-byte ellipsis, then enough tail that
        // `len - 24_000` lands on the ellipsis's middle byte.
        let mut log = "a".repeat(8_000);
        log.push('…');
        log.push_str(&"b".repeat(23_998));
        let naive = log.len() - 24_000;
        assert!(log.len() > 32_000);
        assert!(
            !log.is_char_boundary(naive),
            "fixture must reproduce the panic offset"
        );

        trim_burn_log_tail(&mut log, 32_000, 24_000);

        assert!(log.is_char_boundary(0));
        assert!(log.starts_with('…'));
        assert!(log.len() <= 24_000 + '…'.len_utf8());
        assert!(log.chars().all(|c| c == '…' || c == 'b'));
    }

    #[test]
    fn trim_burn_log_leaves_short_logs_alone() {
        let mut log = "disc 1 ok\n".to_string();
        trim_burn_log_tail(&mut log, 32_000, 24_000);
        assert_eq!(log, "disc 1 ok\n");
    }
}

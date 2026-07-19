use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;
use tokio::sync::mpsc as tokio_mpsc;

use crate::auth::{self, AuthState, LoginOutcome};
use crate::config::AppConfig;
use crate::disc::{scan_download_root, suggest_pack, DiscMedia, DiscPack};
use crate::download::{DownloadQueue, QueueEvent, QueueItem};
use crate::gog::{DownloadFile, GameDetails, GogClient, LibraryGame};
use crate::images::ImageCache;
use crate::theme;
use crate::ui::{BurnPanel, GameDetailPanel, LibraryPanel, QueuePanel};

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
    },
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
    burn_media: DiscMedia,
    burn_pack: Option<DiscPack>,
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
            burn_media: DiscMedia::Bd25,
            burn_pack: None,
        };

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

    /// Fetch details for every checked game and queue all installers + extras.
    fn queue_checked_games(&mut self) {
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

            let _ = tx.send(AsyncMsg::BatchQueued { ok, failed, errors });
        });
    }

    fn poll_channels(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.async_rx.try_recv() {
            match msg {
                AsyncMsg::Library(Ok(games)) => {
                    self.library = games;
                    self.loading_library = false;
                    self.status = format!("Loaded {} games.", self.library.len());
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
                AsyncMsg::BatchQueued { ok, failed, errors } => {
                    self.batch_queueing = false;
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
                    if ok > 0 {
                        self.tab = Tab::Queue;
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
        if !live.is_empty() || self.queue_items.iter().any(|i| {
            matches!(
                i.status,
                crate::download::JobStatus::Queued | crate::download::JobStatus::Running
            )
        }) {
            self.queue_items = live;
        }
    }

    fn suggest_burn_pack(&mut self) {
        let Some(root) = self.config.download_root.clone() else {
            self.status = "Choose a download folder first.".into();
            return;
        };
        match scan_download_root(&root) {
            Ok(candidates) => {
                if candidates.is_empty() {
                    self.status = "No game folders found under the download root yet.".into();
                    self.burn_pack = None;
                } else {
                    self.burn_pack = Some(suggest_pack(self.burn_media, candidates));
                    self.status = "Suggested a disc pack from downloaded games.".into();
                }
            }
            Err(err) => self.status = format!("Could not scan download root: {err}"),
        }
    }
}

impl eframe::App for ConjureApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
                            .min_size(egui::vec2(220.0, 40.0)),
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
                let mut queue_checked = false;
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
                        queue_checked = actions.queue_checked;

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
                if queue_checked {
                    self.queue_checked_games();
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
                    let mut suggest = false;
                    BurnPanel::show(
                        ui,
                        &mut self.burn_media,
                        &self.burn_pack,
                        &mut || suggest = true,
                    );
                    if suggest {
                        self.suggest_burn_pack();
                    }
                });
            }
        }
    }
}

mod path;

pub use path::game_folder;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::gog::{DownloadFile, GogClient};

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: u64,
    pub game_id: u64,
    pub game_title: String,
    pub file: DownloadFile,
    pub dest_dir: PathBuf,
    pub status: JobStatus,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum QueueEvent {
    Updated(QueueItem),
}

#[derive(Clone)]
pub struct DownloadQueue {
    inner: Arc<Mutex<QueueInner>>,
    events: mpsc::UnboundedSender<QueueEvent>,
}

struct QueueInner {
    next_id: u64,
    items: Vec<QueueItem>,
    max_concurrent: usize,
    running: usize,
}

impl DownloadQueue {
    pub fn new(max_concurrent: usize, events: mpsc::UnboundedSender<QueueEvent>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueInner {
                next_id: 1,
                items: Vec::new(),
                max_concurrent: max_concurrent.max(1),
                running: 0,
            })),
            events,
        }
    }

    pub fn items(&self) -> Vec<QueueItem> {
        self.inner.lock().items.clone()
    }

    pub fn enqueue(
        &self,
        client: GogClient,
        game_id: u64,
        game_title: String,
        files: Vec<DownloadFile>,
        download_root: PathBuf,
        runtime: &tokio::runtime::Handle,
    ) {
        let dest_dir = game_folder(&download_root, &game_title);

        {
            let mut inner = self.inner.lock();
            for file in files {
                let id = inner.next_id;
                inner.next_id += 1;
                let total = file.size;
                let item = QueueItem {
                    id,
                    game_id,
                    game_title: game_title.clone(),
                    file,
                    dest_dir: dest_dir.clone(),
                    status: JobStatus::Queued,
                    downloaded: 0,
                    total,
                    error: None,
                };
                let _ = self.events.send(QueueEvent::Updated(item.clone()));
                inner.items.push(item);
            }
        }

        self.pump(client, runtime);
    }

    pub fn cancel(&self, id: u64) {
        let mut inner = self.inner.lock();
        if let Some(item) = inner.items.iter_mut().find(|i| i.id == id) {
            if matches!(item.status, JobStatus::Queued | JobStatus::Running) {
                item.status = JobStatus::Cancelled;
                let _ = self.events.send(QueueEvent::Updated(item.clone()));
            }
        }
    }

    pub fn clear_finished(&self) {
        let mut inner = self.inner.lock();
        inner.items.retain(|i| {
            !matches!(
                i.status,
                JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
            )
        });
    }

    fn pump(&self, client: GogClient, runtime: &tokio::runtime::Handle) {
        loop {
            let next_id = {
                let mut inner = self.inner.lock();
                if inner.running >= inner.max_concurrent {
                    return;
                }
                let Some(idx) = inner
                    .items
                    .iter()
                    .position(|i| i.status == JobStatus::Queued)
                else {
                    return;
                };
                inner.items[idx].status = JobStatus::Running;
                inner.running += 1;
                let item = inner.items[idx].clone();
                let id = item.id;
                let _ = self.events.send(QueueEvent::Updated(item));
                id
            };

            let queue = self.clone();
            let client = client.clone();
            runtime.spawn(async move {
                let result = queue.run_job(&client, next_id).await;
                {
                    let mut inner = queue.inner.lock();
                    inner.running = inner.running.saturating_sub(1);
                    if let Some(item) = inner.items.iter_mut().find(|i| i.id == next_id) {
                        match result {
                            Ok(()) if item.status != JobStatus::Cancelled => {
                                item.status = JobStatus::Completed;
                                item.downloaded = item.total.max(item.downloaded);
                            }
                            Err(err) if item.status != JobStatus::Cancelled => {
                                item.status = JobStatus::Failed;
                                item.error = Some(format!("{err:#}"));
                            }
                            _ => {}
                        }
                        let _ = queue.events.send(QueueEvent::Updated(item.clone()));
                    }
                }
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    queue.pump(client, &handle);
                }
            });
        }
    }

    async fn run_job(&self, client: &GogClient, id: u64) -> Result<()> {
        let (file, dest_dir) = {
            let inner = self.inner.lock();
            let item = inner
                .items
                .iter()
                .find(|i| i.id == id)
                .context("missing queue item")?;
            (item.file.clone(), item.dest_dir.clone())
        };

        tokio::fs::create_dir_all(&dest_dir).await?;
        let dest_path = dest_dir.join(sanitize_filename::sanitize(&file.name));

        let existing = tokio::fs::metadata(&dest_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        // Already complete according to GOG metadata.
        if file.size > 0 && existing == file.size {
            let mut inner = self.inner.lock();
            if let Some(item) = inner.items.iter_mut().find(|i| i.id == id) {
                item.downloaded = existing;
                item.total = file.size;
                let _ = self.events.send(QueueEvent::Updated(item.clone()));
            }
            return Ok(());
        }

        let cdn_url = client.resolve_downlink(&file.downlink).await?;

        // Prefer a clean full download when the partial looks unusable.
        // GOG CDN often answers 416 for bad Range offsets (common on Windows .exe retries).
        let mut resume_from = existing;
        if file.size > 0 && existing > file.size {
            let _ = tokio::fs::remove_file(&dest_path).await;
            resume_from = 0;
        }

        let resp = match fetch_cdn(client, &cdn_url, resume_from).await? {
            FetchResult::Full(resp) => {
                if resume_from > 0 {
                    let _ = tokio::fs::remove_file(&dest_path).await;
                    resume_from = 0;
                }
                resp
            }
            FetchResult::Partial(resp) => resp,
            FetchResult::Restart => {
                let _ = tokio::fs::remove_file(&dest_path).await;
                resume_from = 0;
                match fetch_cdn(client, &cdn_url, 0).await? {
                    FetchResult::Full(resp) | FetchResult::Partial(resp) => resp,
                    FetchResult::Restart => bail!("CDN refused download after restart"),
                }
            }
        };

        let content_len = resp.content_length().unwrap_or(0);
        let total = if resume_from > 0 && content_len > 0 {
            resume_from + content_len
        } else {
            content_len.max(file.size)
        };

        {
            let mut inner = self.inner.lock();
            if let Some(item) = inner.items.iter_mut().find(|i| i.id == id) {
                item.total = total;
                item.downloaded = resume_from;
                let _ = self.events.send(QueueEvent::Updated(item.clone()));
            }
        }

        let mut file_out = if resume_from > 0 {
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&dest_path)
                .await?
        } else {
            tokio::fs::File::create(&dest_path).await?
        };

        let mut stream = resp.bytes_stream();
        let mut downloaded = resume_from;
        // Throttle UI events — per-chunk clones on an unbounded channel could
        // balloon RAM while downloading multi‑GB installers.
        let mut last_report = Instant::now();
        let mut last_reported = resume_from;
        let mut since_flush = 0u64;
        const REPORT_EVERY: Duration = Duration::from_millis(250);
        const REPORT_BYTES: u64 = 4 * 1024 * 1024;
        const FLUSH_BYTES: u64 = 8 * 1024 * 1024;

        while let Some(chunk) = stream.next().await {
            {
                let inner = self.inner.lock();
                if let Some(item) = inner.items.iter().find(|i| i.id == id) {
                    if item.status == JobStatus::Cancelled {
                        bail!("cancelled");
                    }
                }
            }
            let chunk = chunk?;
            file_out.write_all(&chunk).await?;
            let n = chunk.len() as u64;
            downloaded += n;
            since_flush += n;

            if since_flush >= FLUSH_BYTES {
                file_out.flush().await?;
                since_flush = 0;
            }

            let should_report = last_report.elapsed() >= REPORT_EVERY
                || downloaded.saturating_sub(last_reported) >= REPORT_BYTES;
            if should_report {
                let mut inner = self.inner.lock();
                if let Some(item) = inner.items.iter_mut().find(|i| i.id == id) {
                    item.downloaded = downloaded;
                    item.total = total.max(downloaded);
                    let _ = self.events.send(QueueEvent::Updated(item.clone()));
                }
                last_report = Instant::now();
                last_reported = downloaded;
            }
        }
        file_out.flush().await?;
        {
            let mut inner = self.inner.lock();
            if let Some(item) = inner.items.iter_mut().find(|i| i.id == id) {
                item.downloaded = downloaded;
                item.total = total.max(downloaded);
                let _ = self.events.send(QueueEvent::Updated(item.clone()));
            }
        }
        Ok(())
    }
}

enum FetchResult {
    Full(reqwest::Response),
    Partial(reqwest::Response),
    Restart,
}

async fn fetch_cdn(client: &GogClient, cdn_url: &str, resume_from: u64) -> Result<FetchResult> {
    // CDN signed URLs should not get the GOG Bearer token — some edges mis-handle it.
    let mut request = client.http().get(cdn_url);
    if resume_from > 0 {
        request = request.header("Range", format!("bytes={resume_from}-"));
    }

    let resp = request.send().await.context("CDN request failed")?;
    let status = resp.status();

    if resume_from > 0 {
        if status == StatusCode::PARTIAL_CONTENT {
            return Ok(FetchResult::Partial(resp));
        }
        if status == StatusCode::OK {
            // Server ignored Range and sent the whole file.
            return Ok(FetchResult::Full(resp));
        }
        if status == StatusCode::RANGE_NOT_SATISFIABLE || status.is_client_error() {
            return Ok(FetchResult::Restart);
        }
    }

    Ok(FetchResult::Full(resp.error_for_status()?))
}

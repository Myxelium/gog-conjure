mod models;

pub use models::*;

use anyhow::{bail, Context, Result};
use reqwest::header::{CONTENT_LENGTH, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use url::Url;

use crate::auth::AuthState;

/// Refuse to buffer more than this when resolving a downlink JSON/metadata response.
const MAX_DOWNLINK_BODY: u64 = 1_048_576; // 1 MiB

#[derive(Clone)]
pub struct GogClient {
    http: Client,
    /// Never follows redirects — prevents pulling multi‑GB CDN bodies into RAM.
    http_no_redirect: Client,
    auth: AuthState,
}

impl GogClient {
    pub fn new(auth: AuthState) -> Self {
        let http = Client::builder()
            .user_agent("gog-conjure/0.1")
            .build()
            .expect("http client");
        let http_no_redirect = Client::builder()
            .user_agent("gog-conjure/0.1")
            .redirect(Policy::none())
            .build()
            .expect("http client (no redirect)");
        Self {
            http,
            http_no_redirect,
            auth,
        }
    }

    pub fn http(&self) -> &Client {
        &self.http
    }

    pub fn auth(&self) -> &AuthState {
        &self.auth
    }

    async fn bearer_get(&self, url: &str) -> Result<reqwest::Response> {
        let token = self.auth.access_token(&self.http).await?;
        let resp = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        Ok(resp.error_for_status()?)
    }

    pub async fn list_owned_games(&self) -> Result<Vec<LibraryGame>> {
        let mut page = 1u32;
        let mut games = Vec::new();

        loop {
            let url = format!(
                "https://embed.gog.com/account/getFilteredProducts?mediaType=1&page={page}"
            );
            let resp = self.bearer_get(&url).await?;
            let body: FilteredProducts = resp.json().await?;
            let total_pages = body.total_pages.max(1);
            games.extend(body.products.into_iter().map(|p| LibraryGame {
                id: p.id,
                title: p.title,
                image: p.image,
                slug: p.slug,
            }));
            if page >= total_pages {
                break;
            }
            page += 1;
        }

        games.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        Ok(games)
    }

    pub async fn game_details(&self, id: u64) -> Result<GameDetails> {
        let url = format!("https://embed.gog.com/account/gameDetails/{id}.json");
        let resp = self.bearer_get(&url).await?;
        let value: Value = resp.json().await?;
        Ok(GameDetails::from_json(id, value)?)
    }

    /// Resolve a GOG `downlink` endpoint into a direct CDN URL.
    ///
    /// Critical: must not follow redirects into the CDN and must not buffer the
    /// installer body — that previously loaded entire multi‑GB files into RAM.
    pub async fn resolve_downlink(&self, downlink: &str) -> Result<String> {
        let token = self.auth.access_token(&self.http).await?;
        let request_url = Url::parse(downlink).context("invalid downlink URL")?;

        let resp = self
            .http_no_redirect
            .get(downlink)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json,text/plain,*/*")
            .send()
            .await
            .context("resolve downlink")?;

        let status = resp.status();

        // 3xx → use Location (signed CDN URL) without downloading the file.
        if status.is_redirection() {
            if let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()) {
                let resolved = request_url
                    .join(loc)
                    .with_context(|| format!("join redirect Location '{loc}'"))?;
                return Ok(resolved.to_string());
            }
            bail!("downlink returned {status} without Location header");
        }

        if status == StatusCode::OK {
            let len = resp
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());

            if let Some(len) = len {
                if len > MAX_DOWNLINK_BODY {
                    bail!(
                        "downlink response is {len} bytes — refusing to buffer in memory \
                         (expected a small JSON redirect payload)"
                    );
                }
            }

            let bytes = read_body_limited(resp, MAX_DOWNLINK_BODY).await?;

            // Typical GOG response: { "downlink": "https://gog-cdn-.../..." }
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(url) = v.get("downlink").and_then(|x| x.as_str()) {
                    return Ok(url.to_string());
                }
                if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
                    return Ok(url.to_string());
                }
            }

            // Plain-text URL body
            if let Ok(text) = std::str::from_utf8(&bytes) {
                let trimmed = text.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    return Ok(trimmed.to_string());
                }
            }

            bail!("could not parse downlink JSON for a CDN URL");
        }

        bail!("could not resolve downlink ({status})")
    }
}

async fn read_body_limited(resp: reqwest::Response, max: u64) -> Result<bytes::Bytes> {
    use futures_util::StreamExt;

    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read downlink body")?;
        if (out.len() as u64) + (chunk.len() as u64) > max {
            bail!(
                "downlink response exceeded {max} bytes — refusing to buffer in memory"
            );
        }
        out.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(out))
}

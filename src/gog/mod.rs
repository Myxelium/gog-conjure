mod models;

pub use models::*;

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

use crate::auth::AuthState;

#[derive(Clone)]
pub struct GogClient {
    http: Client,
    auth: AuthState,
}

impl GogClient {
    pub fn new(auth: AuthState) -> Self {
        let http = Client::builder()
            .user_agent("gog-conjure/0.1")
            .build()
            .expect("http client");
        Self { http, auth }
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
    pub async fn resolve_downlink(&self, downlink: &str) -> Result<String> {
        let token = self.auth.access_token(&self.http).await?;

        // Prefer JSON body from embed.gog.com; do not follow straight to a binary CDN body.
        let resp = self
            .http
            .get(downlink)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json,text/plain,*/*")
            .send()
            .await
            .context("resolve downlink")?;

        let status = resp.status();
        let final_url = resp.url().clone();
        let bytes = resp.bytes().await?;

        // Typical GOG response: { "downlink": "https://gog-cdn-.../..." }
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(url) = v.get("downlink").and_then(|x| x.as_str()) {
                return Ok(url.to_string());
            }
            if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
                return Ok(url.to_string());
            }
        }

        // Fallback: redirect already landed on a signed CDN URL.
        let final_s = final_url.as_str();
        if final_s.contains("gog-cdn") || final_s.contains("cdn.gog.com") {
            return Ok(final_s.to_string());
        }

        if status.is_success() {
            return Ok(final_s.to_string());
        }

        anyhow::bail!("could not resolve downlink ({status})")
    }
}

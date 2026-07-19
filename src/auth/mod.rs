mod login;
mod login_helper;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config;

pub use login::{begin_login, LoginOutcome};
pub use login_helper::run_login_window;

pub const CLIENT_ID: &str = "46899977096215655";
pub const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
/// Only redirect URI registered for the public Galaxy client — localhost is rejected.
pub const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub user_id: Option<String>,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now + 60 >= self.expires_at
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    user_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct AuthState {
    inner: Arc<RwLock<Option<TokenSet>>>,
}

impl AuthState {
    pub fn load() -> Self {
        let tokens = config::load_json::<TokenSet>("tokens.json").ok();
        Self {
            inner: Arc::new(RwLock::new(tokens)),
        }
    }

    pub fn tokens(&self) -> Option<TokenSet> {
        self.inner.read().clone()
    }

    pub fn is_logged_in(&self) -> bool {
        self.inner.read().is_some()
    }

    pub fn set_tokens(&self, tokens: TokenSet) -> Result<()> {
        config::save_json("tokens.json", &tokens)?;
        *self.inner.write() = Some(tokens);
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let path = config::config_dir()?.join("tokens.json");
        let _ = std::fs::remove_file(path);
        *self.inner.write() = None;
        Ok(())
    }

    pub async fn access_token(&self, client: &reqwest::Client) -> Result<String> {
        let current = self.tokens().ok_or_else(|| anyhow!("not logged in"))?;
        if !current.is_expired() {
            return Ok(current.access_token);
        }
        let refreshed = refresh_token(client, &current.refresh_token).await?;
        self.set_tokens(refreshed.clone())?;
        Ok(refreshed.access_token)
    }
}

pub fn auth_url() -> String {
    format!(
        "https://auth.gog.com/auth?client_id={CLIENT_ID}&redirect_uri={}&response_type=code&layout=client2",
        urlencoding(REDIRECT_URI)
    )
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

pub async fn exchange_code(client: &reqwest::Client, code: &str) -> Result<TokenSet> {
    let url = format!(
        "https://auth.gog.com/token?client_id={CLIENT_ID}&client_secret={CLIENT_SECRET}&grant_type=authorization_code&code={code}&redirect_uri={}",
        urlencoding(REDIRECT_URI)
    );
    let resp = client
        .get(url)
        .send()
        .await
        .context("token exchange request failed")?
        .error_for_status()
        .context("token exchange rejected")?;
    let body: TokenResponse = resp.json().await?;
    Ok(token_set_from_response(body))
}

pub async fn refresh_token(client: &reqwest::Client, refresh: &str) -> Result<TokenSet> {
    let url = format!(
        "https://auth.gog.com/token?client_id={CLIENT_ID}&client_secret={CLIENT_SECRET}&grant_type=refresh_token&refresh_token={refresh}"
    );
    let resp = client
        .get(url)
        .send()
        .await
        .context("token refresh request failed")?
        .error_for_status()
        .context("token refresh rejected")?;
    let body: TokenResponse = resp.json().await?;
    Ok(token_set_from_response(body))
}

fn token_set_from_response(body: TokenResponse) -> TokenSet {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    TokenSet {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_at: now + body.expires_in.saturating_sub(30),
        user_id: body.user_id,
    }
}

/// Extract an OAuth code from a GOG redirect URL, callback path, or stored raw code.
///
/// Deliberately strict about junk like `about:blank`, but accepts a bare code
/// token (the login helper writes that to disk).
pub fn extract_code(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty login response"));
    }
    if trimmed.starts_with("about:") {
        return Err(anyhow!("ignored about: URL"));
    }

    if let Ok(parsed) = url::Url::parse(trimmed) {
        if let Some(code) = parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
        {
            if is_plausible_oauth_code(&code) {
                return Ok(code);
            }
        }
    }

    // Path or query: /on_login_success?origin=client&code=...
    if let Some(query) = trimmed.split_once('?').map(|(_, q)| q) {
        for pair in query.split('&') {
            if let Some(code) = pair.strip_prefix("code=") {
                if is_plausible_oauth_code(code) {
                    return Ok(code.to_string());
                }
            }
        }
    }

    // Raw code written by the login helper.
    if is_plausible_oauth_code(trimmed) {
        return Ok(trimmed.to_string());
    }

    Err(anyhow!("could not find OAuth code in callback"))
}

pub fn is_plausible_oauth_code(code: &str) -> bool {
    let code = code.trim();
    // GOG codes are long opaque tokens; reject junk like "about:blank".
    code.len() >= 16
        && !code.contains(':')
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// True when a navigation/load URL is the Galaxy OAuth success redirect.
pub fn is_login_success_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("code=")
        && (lower.contains("on_login_success")
            || lower.contains("embed.gog.com")
            || lower.contains("www.gog.com"))
}

#[cfg(test)]
mod tests {
    use super::extract_code;

    #[test]
    fn extracts_code_from_galaxy_redirect() {
        let code = extract_code(
            "https://embed.gog.com/on_login_success?origin=client&code=oF8OSgZVMFb7a8Y3Dolrz4YPqDUnG7TC",
        )
        .unwrap();
        assert_eq!(code, "oF8OSgZVMFb7a8Y3Dolrz4YPqDUnG7TC");
    }

    #[test]
    fn ignores_about_blank() {
        assert!(extract_code("about:blank").is_err());
    }

    #[test]
    fn accepts_raw_helper_code() {
        let code = "oF8OSgZVMFb7a8Y3Dolrz4YPqDUnG7TC";
        assert_eq!(extract_code(code).unwrap(), code);
    }
}

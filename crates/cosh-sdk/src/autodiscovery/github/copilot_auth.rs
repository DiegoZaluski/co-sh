#![allow(dead_code)]

//! GitHub Copilot authentication utilities.
//!
//! Implements the OAuth device code flow used by the Copilot CLI and handles
//! token validation/exchange for the Copilot API.
//!
//! Token type support (per GitHub docs):
//!   gho_          OAuth token           ✓  (default via `gh auth login`)
//!   `github_pat_` Fine-grained PAT      ✓  (needs Copilot Requests permission)
//!   ghu_          GitHub App token      ✓  (via environment variable)
//!   ghp_          Classic PAT           ✗  NOT SUPPORTED
//!
//! Credential search order (matching Copilot CLI behaviour):
//!   1. `COPILOT_GITHUB_TOKEN` env var
//!   2. `GH_TOKEN` env var
//!   3. `GITHUB_TOKEN` env var
//!   4. `gh auth token` CLI fallback
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use std::fmt::Write as _;

/// GitHub Copilot authentication client.
///
/// Handles token resolution (env vars → `gh auth token`), OAuth device code
/// login, token exchange for the Copilot API, and standard request headers.
///
/// ```no_run
/// use cosh_sdk::autodiscovery::github::copilot_auth::CopilotAuth;
///
/// let auth = CopilotAuth::new()
///     .with_host("github.com")
///     .with_timeout(120.0);
///
/// let (token, source) = auth.resolve_token().unwrap();
/// let api_token = tokio::runtime::Runtime::new()
///     .unwrap()
///     .block_on(auth.get_api_token(&token));
/// ```
#[derive(Debug)]
pub struct CopilotAuth {
    client: reqwest::Client,
    pub(crate) host: String,
    pub(crate) timeout_seconds: f64,
    pub(crate) gh_host: Option<String>,
}

/// Response from initiating the OAuth device code flow.
///
/// Returned by [`CopilotAuth::initiate_device_code`]. The caller should display
/// `verification_uri` and `user_code` to the user, then call
/// [`CopilotAuth::poll_for_token`] with `device_code` and `interval`.
#[derive(Debug, Clone)]
pub struct DeviceCodeResponse {
    /// URL the user must open in their browser.
    pub verification_uri: String,
    /// One-time code the user enters on the verification page.
    pub user_code: String,
    /// Opaque handle passed to `poll_for_token`.
    pub device_code: String,
    /// Recommended poll interval in seconds (from server, min 1).
    pub interval: u64,
}

impl CopilotAuth {
    const DEFAULT_HOST: &'static str = "github.com";
    const DEFAULT_TIMEOUT: f64 = 30.0;

    /// Create a new client with default configuration.
    ///
    /// Defaults: host=`github.com`, timeout=30s, no GH host override.
    /// Call `.with_*` to customise.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            host: Self::DEFAULT_HOST.into(),
            timeout_seconds: Self::DEFAULT_TIMEOUT,
            gh_host: None,
        }
    }

    /// Override the GitHub host (default: `github.com`).
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Override the request timeout in seconds (default: 30).
    pub fn with_timeout(mut self, seconds: f64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    /// Set `--hostname` for `gh auth token` (via `COPILOT_GH_HOST` behaviour).
    pub fn with_gh_host(mut self, host: impl Into<String>) -> Self {
        self.gh_host = Some(host.into());
        self
    }

    // Token validation

    /// Validate that a token is usable with the Copilot API.
    ///
    /// Returns `Ok(())` on success, or an error message explaining why the
    /// token is not suitable.
    #[allow(clippy::unused_self)]
    pub fn validate_token(&self, token: &str) -> Result<(), String> {
        const CLASSIC_PAT_PREFIX: &str = "ghp_";

        let token = token.trim();
        if token.is_empty() {
            return Err("Empty token".into());
        }

        if token.starts_with(CLASSIC_PAT_PREFIX) {
            return Err(
                "Classic Personal Access Tokens (ghp_*) are not supported by the \
                 Copilot API. Use one of:\n  \
                 → `copilot login` or `gh auth login` to authenticate via OAuth\n  \
                 → A fine-grained PAT (github_pat_*) with Copilot Requests permission\n  \
                 → `gh auth login` with the default device code flow (produces gho_* tokens)"
                    .into(),
            );
        }

        Ok(())
    }

    // Token resolution

    /// Resolve a GitHub token suitable for Copilot API use.
    ///
    /// Returns `(token, source)` where source describes where the token came from.
    /// Returns `Err` if no compatible token was found.
    pub fn resolve_token(&self) -> Result<(String, String), String> {
        const COPILOT_ENV_VARS: &[&str] = &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];

        for env_var in COPILOT_ENV_VARS {
            let val = std::env::var(env_var).unwrap_or_default();
            let val = val.trim().to_string();
            if val.is_empty() {
                continue;
            }
            if let Err(msg) = self.validate_token(&val) {
                log::warn!("Token from {env_var} is not supported: {msg}");
                continue;
            }
            return Ok((val, env_var.to_string()));
        }

        if let Some(token) = try_gh_cli_token(self.gh_host.as_deref()) {
            match self.validate_token(&token) {
                Err(msg) => {
                    return Err(format!(
                        "Token from `gh auth token` is a classic PAT (ghp_*). {msg}"
                    ));
                }
                Ok(()) => return Ok((token, "gh auth token".to_string())),
            }
        }

        Err("No Copilot-compatible token found".into())
    }

    // OAuth Device Code Flow

    /// Initiate the GitHub OAuth device code flow.
    ///
    /// Sends a POST to `https://<host>/login/device/code` and returns the
    /// verification URI, user code, and polling parameters. The caller should
    /// display `verification_uri` and `user_code` to the user, then call
    /// [`Self::poll_for_token`] with the returned `device_code`.
    ///
    /// This replicates the flow used by opencode and the Copilot CLI.
    pub async fn initiate_device_code(&self) -> Result<DeviceCodeResponse, String> {
        const DEVICE_CODE_POLL_INTERVAL: u64 = 5;

        let domain = self.host.trim_end_matches('/');
        let device_code_url = format!("https://{domain}/login/device/code");

        let body = format!(
            "client_id={}&scope={}",
            url_encode(COPILOT_OAUTH_CLIENT_ID),
            url_encode("read:user"),
        );

        let resp = self
            .client
            .post(&device_code_url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", "cosh/0.1.0")
            .body(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("Failed to initiate device authorization: {e}"))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse device authorization response: {e}"))?;

        let verification_uri = data
            .get("verification_uri")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("https://github.com/login/device")
            .to_string();
        let user_code = data
            .get("user_code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let device_code = data
            .get("device_code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let interval = data
            .get("interval")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEVICE_CODE_POLL_INTERVAL)
            .max(1);

        if device_code.is_empty() || user_code.is_empty() {
            return Err("GitHub did not return a device code".into());
        }

        Ok(DeviceCodeResponse {
            verification_uri,
            user_code,
            device_code,
            interval,
        })
    }

    /// Poll the OAuth token endpoint until the user authorizes the device.
    ///
    /// Call this after [`Self::initiate_device_code`] with the `device_code`
    /// and `interval` from the response. The `deadline` controls how long to
    /// keep polling before giving up.
    ///
    /// Returns the OAuth access token on success.
    pub async fn poll_for_token(
        &self,
        device_code: &str,
        interval: u64,
        deadline: Instant,
    ) -> Result<String, String> {
        const DEVICE_CODE_POLL_SAFETY_MARGIN: u64 = 3;

        let domain = self.host.trim_end_matches('/');
        let access_token_url = format!("https://{domain}/login/oauth/access_token");

        let mut interval = interval;
        let safety = DEVICE_CODE_POLL_SAFETY_MARGIN;

        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(interval + safety)).await;

            let poll_body = format!(
                "client_id={}&device_code={}&grant_type={}",
                url_encode(COPILOT_OAUTH_CLIENT_ID),
                url_encode(device_code),
                url_encode("urn:ietf:params:oauth:grant-type:device_code"),
            );

            let Ok(poll_resp) = self
                .client
                .post(&access_token_url)
                .header("Accept", "application/json")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("User-Agent", "cosh/0.1.0")
                .body(poll_body)
                .timeout(Duration::from_secs(10))
                .send()
                .await
            else {
                continue;
            };

            let Ok(result) = poll_resp.json::<serde_json::Value>().await else {
                continue;
            };

            if let Some(token) = result
                .get("access_token")
                .and_then(serde_json::Value::as_str)
            {
                return Ok(token.to_string());
            }

            let error = result
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            match error {
                "authorization_pending" => {}
                "slow_down" => {
                    if let Some(si) = result.get("interval").and_then(serde_json::Value::as_u64) {
                        interval = si;
                    } else {
                        interval += 5;
                    }
                }
                "expired_token" => return Err("Device code expired. Please try again.".into()),
                "access_denied" => return Err("Authorization was denied.".into()),
                _ if !error.is_empty() => {
                    return Err(format!("Authorization failed: {error}"));
                }
                _ => {}
            }
        }

        Err("Timed out waiting for authorization.".into())
    }

    // Token Exchange

    /// Exchange a raw GitHub token for a short-lived Copilot API token.
    ///
    /// Calls `GET https://api.github.com/copilot_internal/v2/token` with
    /// the raw GitHub token and returns `(api_token, expires_at)`.
    ///
    /// The returned token is a semicolon-separated string (not a standard JWT)
    /// used as `Authorization: Bearer <token>` for Copilot API requests.
    ///
    /// Results are cached in-process and reused until close to expiry.
    /// Returns `Err` on failure.
    pub async fn exchange_token(&self, raw_token: &str) -> Result<(String, f64), String> {
        const JWT_REFRESH_MARGIN_SECONDS: f64 = 120.0;
        const TOKEN_EXCHANGE_URL: &str = "https://api.github.com/copilot_internal/v2/token";
        const EDITOR_VERSION: &str = "vscode/1.104.1";
        const EXCHANGE_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";

        let fp = token_fingerprint(raw_token);

        {
            let cache = JWT_CACHE.lock().map_err(|e| e.to_string())?;
            if let Some((api_token, expires_at)) = cache.get(&fp)
                && std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
                    < *expires_at - JWT_REFRESH_MARGIN_SECONDS
            {
                return Ok((api_token.clone(), *expires_at));
            }
        }

        let resp = self
            .client
            .get(TOKEN_EXCHANGE_URL)
            .header("Authorization", format!("token {raw_token}"))
            .header("User-Agent", EXCHANGE_USER_AGENT)
            .header("Accept", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .timeout(Duration::from_secs_f64(self.timeout_seconds))
            .send()
            .await
            .map_err(|e| format!("Copilot token exchange failed: {e}"))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Copilot token exchange failed: {e}"))?;

        let api_token = data
            .get("token")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let expires_at = data
            .get("expires_at")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
                    + 1800.0
            });

        if api_token.is_empty() {
            return Err("Copilot token exchange returned empty token".into());
        }

        {
            let mut cache = JWT_CACHE.lock().map_err(|e| e.to_string())?;
            cache.insert(fp, (api_token.clone(), expires_at));
        }

        log::debug!("Copilot token exchanged, expires_at={expires_at}");
        Ok((api_token, expires_at))
    }

    /// Exchange a raw GitHub token for a Copilot API token, with fallback.
    ///
    /// Convenience wrapper: returns the exchanged token on success, or the
    /// raw token unchanged if the exchange fails (e.g. network error, unsupported
    /// account type). This preserves existing behaviour for accounts that don't
    /// need exchange while enabling access to internal-only models for those that do.
    pub async fn get_api_token(&self, raw_token: &str) -> String {
        if raw_token.is_empty() {
            return raw_token.to_string();
        }
        match self.exchange_token(raw_token).await {
            Ok((api_token, _)) => api_token,
            Err(e) => {
                log::debug!("Copilot token exchange failed, using raw token: {e}");
                raw_token.to_string()
            }
        }
    }

    // Request Headers

    /// Build the standard headers for Copilot API requests.
    ///
    /// Replicates the header set used by opencode and the Copilot CLI.
    #[allow(clippy::unused_self)]
    pub fn request_headers(&self, is_agent_turn: bool, is_vision: bool) -> HashMap<String, String> {
        const EDITOR_VERSION: &str = "vscode/1.104.1";

        let mut headers = HashMap::new();
        headers.insert("Editor-Version".into(), EDITOR_VERSION.into());
        headers.insert("User-Agent".into(), "GitHubCopilotChat/0.26.7".into());
        headers.insert("Copilot-Integration-Id".into(), "vscode-chat".into());
        headers.insert("Openai-Intent".into(), "conversation-edits".into());
        headers.insert(
            "x-initiator".into(),
            if is_agent_turn { "agent" } else { "user" }.into(),
        );
        if is_vision {
            headers.insert("Copilot-Vision-Request".into(), "true".into());
        }
        headers
    }
}

// Private helpers

// OAuth device code flow client ID (same as opencode/Copilot CLI)
const COPILOT_OAUTH_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

/// Minimal percent-encoding for application/x-www-form-urlencoded values.
pub(crate) fn url_encode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => {
                let _ = write!(result, "%{byte:02X}");
            }
        }
    }
    result
}

/// Return candidate `gh` binary paths, including common Homebrew installs.
pub(crate) fn gh_cli_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(resolved) = which_gh() {
        candidates.push(resolved);
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let extra_candidates = [
        PathBuf::from("/opt/homebrew/bin/gh"),
        PathBuf::from("/usr/local/bin/gh"),
        PathBuf::from(&home).join(".local").join("bin").join("gh"),
    ];

    for candidate in &extra_candidates {
        if candidates.contains(candidate) {
            continue;
        }
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = candidate.metadata()
                    && meta.permissions().mode() & 0o111 != 0
                {
                    candidates.push(candidate.clone());
                }
            }
            #[cfg(not(unix))]
            {
                candidates.push(candidate.clone());
            }
        }
    }

    candidates
}

/// Cross-platform `which` for the `gh` binary.
fn which_gh() -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("gh");
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = candidate.metadata()
                    && meta.permissions().mode() & 0o111 != 0
                {
                    return Some(candidate);
                }
            }
            #[cfg(not(unix))]
            {
                return Some(candidate);
            }
        }

        #[cfg(windows)]
        {
            let candidate_exe = dir.join("gh.exe");
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
    }
    None
}

/// Return a token from `gh auth token` when the GitHub CLI is available.
///
/// When `gh_host` is `Some`, passes `--hostname` so gh returns the correct
/// host's token. Also strips `GITHUB_TOKEN` / `GH_TOKEN` from the subprocess
/// environment so `gh` reads from its own credential store (hosts.yml) instead
/// of just echoing the env var back.
pub(crate) fn try_gh_cli_token(gh_host: Option<&str>) -> Option<String> {
    let clean_env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| k != "GITHUB_TOKEN" && k != "GH_TOKEN")
        .collect();

    for gh_path in gh_cli_candidates() {
        let mut cmd = std::process::Command::new(&gh_path);
        cmd.args(["auth", "token"]);
        if let Some(host) = gh_host {
            cmd.arg("--hostname");
            cmd.arg(host);
        }
        cmd.env_clear();
        for (k, v) in &clean_env {
            cmd.env(k, v);
        }

        let output = cmd.output().ok()?;
        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    None
}

/// Short fingerprint of a raw token for cache keying (avoids storing full token).
pub(crate) fn token_fingerprint(raw_token: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw_token.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// Module-level cache for exchanged Copilot API tokens.
// Maps raw_token_fingerprint -> (api_token, expires_at_epoch).
static JWT_CACHE: LazyLock<Mutex<HashMap<String, (String, f64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

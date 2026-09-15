//! Provider-native cost reporting for the usage dashboard.
//!
//! Spend is never estimated locally: each supported provider is asked
//! directly and is the single source of truth for its own billing.
//!
//! Supported providers: `openrouter`, `zen`, `opencode-go`, `charm`
//! (see [`supports_cost_reporting`]). Others return
//! [`CostError::CostNotSupported`] — support for more providers is planned.
//!
//! Units: Charm bills in Hypercredits; its per-request usage reports BOTH
//! `usd` and `hypercredits`. USD is preferred for the dashboard; the credit
//! balance is surfaced as [`ProviderCost::credit_balance`].

use super::error::ConnectorError;
use super::params::Parameters;
use super::provider::{ProviderConfig, get_api_key};

/// Spend information as reported by the provider itself.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProviderCost {
    /// REAL spend (USD) attributable to this session's requests so far —
    /// the sum of every per-request cost the provider reported (or the
    /// provider-side usage figure for account-spend providers).
    pub session_cost_usd: f64,
    /// Provider-side balance (USD) when the provider exposes one
    /// (OpenRouter `limit_remaining`).
    pub balance_usd: Option<f64>,
    /// Provider-side balance in NON-USD units when that is its native unit
    /// (Charm Hypercredits from `GET /v1/credits`). The dashboard only
    /// renders `session_cost_usd` — this field is informational.
    pub credit_balance: Option<f64>,
}

/// Structured rejection for providers with no spend-reporting capability.
///
/// Returned instead of a guessed number so callers can `match`/`if` and
/// simply hide the price rows when the provider cannot vouch for them.
#[derive(Debug, Clone, PartialEq)]
pub enum CostError {
    /// The provider has no API for reporting spend (see the support matrix
    /// in the module docs). Nothing was requested over the network.
    CostNotSupported,
    /// The provider supports cost reporting but no API key could be resolved.
    MissingApiKey(String),
    /// The provider's cost endpoint rejected the request or answered
    /// something unusable.
    CostUnavailable(String),
}

impl std::fmt::Display for CostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CostNotSupported => write!(
                f,
                "provider does not report costs; token/cost display is disabled for it"
            ),
            Self::MissingApiKey(p) => {
                write!(f, "API key not set for provider: {p}")
            }
            Self::CostUnavailable(detail) => write!(f, "cost unavailable: {detail}"),
        }
    }
}

impl std::error::Error for CostError {}

impl From<ConnectorError> for CostError {
    fn from(e: ConnectorError) -> Self {
        match e {
            ConnectorError::MissingApiKey(p) => Self::MissingApiKey(p),
            other => Self::CostUnavailable(other.to_string()),
        }
    }
}

/// Whether the provider can report spend at all — a cheap, OFFLINE check the
/// TUI runs BEFORE deciding to render any cost row.
///
/// Supported: `openrouter`, `zen`, `opencode-go`, `charm`. More providers
/// coming in a future release.
#[must_use]
pub fn supports_cost_reporting(provider: &str) -> bool {
    matches!(
        provider,
        "openrouter" | "zen" | "opencode-go" | "charm"
    )
}

/// Report the REAL session spend using the provider as the source of truth.
///
/// `session_reported_costs` are the per-request costs the provider already
/// reported in each response ([`super::TokenUsage::reported_cost`]); their
/// sum is the session spend. For `openrouter` the figure comes from the
/// provider's account endpoint instead.
///
/// # Errors
///
/// - [`CostError::CostNotSupported`] when the provider has no spend API
///   (offline check, no network).
/// - [`CostError::MissingApiKey`] when the provider reports spend but no
///   key could be resolved.
/// - [`CostError::CostUnavailable`] when the provider's endpoint fails.
pub async fn provider_session_cost(
    config: &ProviderConfig,
    params: &Parameters,
    service: Option<&str>,
    session_reported_costs: &[f64],
) -> Result<ProviderCost, CostError> {
    if !supports_cost_reporting(config.name) {
        return Err(CostError::CostNotSupported);
    }
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name, service))
        .ok_or_else(|| CostError::MissingApiKey(config.name.to_string()))?;

    match config.name {
        "openrouter" => openrouter_cost(config, params, &api_key).await,
        "zen" | "opencode-go" => {
            opencode_cost(config, params, &api_key, session_reported_costs).await
        }
        "charm" => charm_cost(config, params, &api_key, session_reported_costs).await,
        // `supports_cost_reporting` guarantees this arm is unreachable.
        _ => Err(CostError::CostNotSupported),
    }
}

/// Shared GET against a provider endpoint with a Bearer key attached.
async fn get_json(
    config: &ProviderConfig,
    params: &Parameters,
    url: &str,
    api_key: &str,
) -> Result<serde_json::Value, CostError> {
    let mut headers: Vec<(&str, String)> =
        vec![("Authorization", format!("Bearer {api_key}"))];
    if let Some(sid) = params.session_id.as_deref() {
        headers.push(("x-session-id", sid.to_string()));
        headers.push(("x-session-affinity", sid.to_string()));
    }
    let text = super::common::send_get_request(config, url, &headers).await?;
    serde_json::from_str(&text).map_err(|e| CostError::CostUnavailable(e.to_string()))
}

/// OpenRouter: `GET /api/v1/key` reports the key's lifetime `usage` (USD)
/// and, when a limit is configured, `limit_remaining`. Both are the
/// PROVIDER's own numbers — used verbatim.
async fn openrouter_cost(
    config: &ProviderConfig,
    params: &Parameters,
    api_key: &str,
) -> Result<ProviderCost, CostError> {
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let body = get_json(config, params, &format!("{base_url}/key"), api_key).await?;
    let data = body.get("data").unwrap_or(&body);
    let usage = data
        .get("usage")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| CostError::CostUnavailable("missing `data.usage`".to_string()))?;
    let balance_usd = data
        .get("limit_remaining")
        .and_then(serde_json::Value::as_f64);
    Ok(ProviderCost {
        session_cost_usd: usage,
        balance_usd,
        credit_balance: None,
    })
}

/// OpenCode Zen / Go: the account endpoint does not expose spend
/// (anomalyco/opencode#10448), so the source of truth is the per-request
/// REAL `cost` the gateway already reported in its responses — summed here.
async fn opencode_cost(
    config: &ProviderConfig,
    params: &Parameters,
    api_key: &str,
    session_reported_costs: &[f64],
) -> Result<ProviderCost, CostError> {
    // Verify the key still works against the models endpoint (the only
    // authenticated, documented GET these gateways serve). A failure here
    // surfaces as CostUnavailable instead of a stale silent number.
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    get_json(config, params, &format!("{base_url}/models"), api_key).await?;
    Ok(ProviderCost {
        session_cost_usd: session_reported_costs.iter().sum(),
        balance_usd: None,
        credit_balance: None,
    })
}

/// Charm Hyper: per-request `usage.cost.usd` (summed, like Zen) plus the
/// team's remaining Hypercredit balance from `GET /v1/credits`.
async fn charm_cost(
    config: &ProviderConfig,
    params: &Parameters,
    api_key: &str,
    session_reported_costs: &[f64],
) -> Result<ProviderCost, CostError> {
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let credits = get_json(config, params, &format!("{base_url}/credits"), api_key).await?;
    Ok(ProviderCost {
        session_cost_usd: session_reported_costs.iter().sum(),
        balance_usd: None,
        credit_balance: credits.get("balance").and_then(serde_json::Value::as_f64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::test::common::EnvGuard;

    fn cfg(name: &str) -> ProviderConfig {
        *crate::connector::get_provider(name).unwrap()
    }

    #[test]
    fn support_matrix_matches_module_docs() {
        for provider in ["openrouter", "zen", "opencode-go", "charm"] {
            assert!(
                supports_cost_reporting(provider),
                "{provider} must report costs"
            );
        }
        for provider in [
            "openai", "claude", "gemini", "groq", "mistral", "deepseek", "xai", "together",
            "ollama",
        ] {
            assert!(
                !supports_cost_reporting(provider),
                "{provider} must NOT report costs"
            );
        }
    }

    #[tokio::test]
    async fn unsupported_provider_fails_fast_without_network() {
        let err = provider_session_cost(&cfg("openai"), &Parameters::default(), None, &[])
            .await
            .unwrap_err();
        assert_eq!(err, CostError::CostNotSupported);
    }

    #[tokio::test]
    async fn local_provider_is_never_considered_a_cost_source() {
        // Local servers have no billing at all — same structured rejection.
        let err = provider_session_cost(&cfg("ollama"), &Parameters::default(), None, &[])
            .await
            .unwrap_err();
        assert_eq!(err, CostError::CostNotSupported);
    }

    #[tokio::test]
    async fn supported_provider_without_key_reports_missing_api_key() {
        let _guard = EnvGuard::remove("OPENROUTER_API_KEY");
        let err = provider_session_cost(&cfg("openrouter"), &Parameters::default(), None, &[])
            .await
            .unwrap_err();
        assert_eq!(err, CostError::MissingApiKey("openrouter".to_string()));
    }
}

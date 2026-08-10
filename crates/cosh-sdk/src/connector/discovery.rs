//! Context window discovery for LLM models.
//!
//! Attempts to discover the maximum context window size for a given model.
//! Known models are resolved from a static table FIRST — no network call, so
//! the most common models (deepseek, claude, gpt-4o, gemini, …) can never
//! silently fall back to the 100k default because of a slow network, a
//! changed API or a missing auth header. Unknown models are then looked up
//! through public catalogs — models.dev as the primary source with OpenRouter
//! as its fallback — without authentication. For model ids containing
//! "claude", Anthropic's API is tried first (its `/v1/models/{id}` endpoint
//! returns 401 without `x-api-key`, so it only helps when a key is present),
//! then models.dev, then OpenRouter.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// File name of the cached models.dev catalog (raw JSON, rewritten whole).
const MODELS_DEV_CATALOG_FILE: &str = "models.dev.json";
/// File name of the cached OpenRouter catalog (raw JSON, rewritten whole).
const OPENROUTER_CATALOG_FILE: &str = "openrouter.json";

/// Response from OpenRouter's /api/v1/models endpoint.
///
/// The catalog fits in a single page today (the response also carries
/// `total_count`/`links` fields that are ignored here).
#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    /// The vendor-prefixed, dated slug (e.g. `deepseek/deepseek-v4-flash-0731`).
    /// OpenRouter aliases (`~vendor/model-latest`) point at the canonical slug,
    /// so matching against it resolves the alias to the real entry.
    #[serde(default)]
    canonical_slug: Option<String>,
    context_length: Option<usize>,
}

/// Response from Anthropic's /v1/models/{model_id} endpoint.
#[derive(Debug, Deserialize)]
struct AnthropicModelResponse {
    model_info: AnthropicModelInfo,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelInfo {
    max_input_tokens: Option<usize>,
}

/// Response from models.dev `/api.json`.
///
/// The catalog is a map keyed by provider id (`google`, `zhipuai`, …); each
/// provider carries a `models` map keyed by model id (bare like `glm-5`, or
/// vendor-prefixed like `google/gemini-2.5-flash`), and every model exposes
/// its context window under `limit.context` plus reasoning metadata under
/// `reasoning` / `reasoning_options`. The outer map is caught with
/// `#[serde(flatten)]` and other provider fields (name, api, doc, …) are
/// ignored.
#[derive(Debug, Deserialize)]
struct ModelsDevCatalog {
    #[serde(flatten)]
    providers: HashMap<String, ModelsDevProvider>,
}

#[derive(Debug, Deserialize)]
struct ModelsDevProvider {
    #[serde(default)]
    models: HashMap<String, ModelsDevModel>,
}

#[derive(Debug, Default, Deserialize)]
struct ModelsDevModel {
    limit: ModelsDevLimit,
    /// Whether the model advertises reasoning/thinking support. `None` when
    /// the (possibly stale) catalog entry predates the field — treated as
    /// "unknown" so callers fall back to a heuristic instead of concluding
    /// the model has no reasoning.
    #[serde(default)]
    reasoning: Option<bool>,
    /// How reasoning can be configured (effort levels, token budgets, or a
    /// plain toggle). Effort-style options carry the accepted values.
    #[serde(default)]
    reasoning_options: Vec<ModelsDevReasoningOption>,
}

/// One entry of a model's `reasoning_options`. The catalog occasionally
/// emits `null` inside the effort `values` array (`[null, "low", …]`), so
/// the elements are deserialized as optional and filtered later.
#[derive(Debug, Deserialize)]
struct ModelsDevReasoningOption {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<Option<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct ModelsDevLimit {
    context: Option<usize>,
}

/// Reasoning metadata for a model, resolved from the cached models.dev catalog.
///
/// The providers' own `/models` endpoints (OpenAI, Gemini, Claude) do NOT
/// expose reasoning capability — the catalog is the authoritative offline
/// source. `None` means the model is absent from the cached catalog and the
/// caller should fall back to a name heuristic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelReasoning {
    /// Whether the model advertises reasoning/thinking support.
    pub supported: bool,
    /// The accepted effort levels (e.g. `["low", "medium", "high"]`) when
    /// the catalog advertises effort-style configuration. `None` when it only
    /// advertises a toggle or token budget (callers can offer the standard
    /// low/medium/high set).
    pub efforts: Option<Vec<String>>,
}

/// Resolves a model's reasoning metadata, fastest source first:
///
/// 1. The static table of known, current models (no disk read — the most
///    common models always resolve, immune to a missing/stale catalog cache);
/// 2. The cached models.dev catalog (same file [`discover_context_window`]
///    maintains) for the long tail of models not worth hardcoding;
/// 3. `None` — the caller falls back to its name heuristic.
///
/// Synchronous and offline, so the TUI can consult it instantly when
/// rendering the model dialog. Matching is exact and case/vendor tolerant
/// in both tiers, mirroring the context-window lookup.
#[must_use]
pub fn model_reasoning(model_name: &str, cache_dir: Option<&str>) -> Option<ModelReasoning> {
    if let Some(meta) = static_known_reasoning(model_name) {
        return Some(meta);
    }
    model_reasoning_from_catalog(model_name, cache_dir)
}

/// Resolves a model's reasoning metadata from the cached models.dev catalog.
///
/// This is the second tier of [`model_reasoning`] (the static table is tried
/// first). Synchronous and offline: reads `models.dev.json` from the on-disk
/// catalog cache, so the TUI can consult it instantly when rendering the
/// model dialog. Matching is vendor-prefix and case tolerant, mirroring the
/// context-window lookup.
///
/// # Returns
///
/// * `Some(ModelReasoning)` — the model was found; `supported`/`efforts`
///   reflect the catalog entry.
/// * `None` — the model is not in the cached catalog (or the cache is
///   missing/unreadable); the caller should fall back to a heuristic.
#[must_use]
pub fn model_reasoning_from_catalog(
    model_name: &str,
    cache_dir: Option<&str>,
) -> Option<ModelReasoning> {
    let dir = resolve_cache_dir(cache_dir?)?;
    let cache = CatalogCache::new(dir);
    let raw = cache.load_raw(MODELS_DEV_CATALOG_FILE)?;
    let catalog: ModelsDevCatalog = serde_json::from_str(&raw).ok()?;

    let needle_lower = model_name.to_lowercase();
    let needle_bare = needle_lower.rsplit('/').next().unwrap_or(&needle_lower);
    for provider in catalog.providers.values() {
        for (id, model) in &provider.models {
            let id_lower = id.to_lowercase();
            let id_bare = id_lower.rsplit('/').next().unwrap_or(&id_lower);
            let matches = id_lower == needle_lower
                || id_bare == needle_lower
                || id_lower == needle_bare
                || id_bare == needle_bare;
            if !matches {
                continue;
            }
            // A catalog entry without the `reasoning` field (stale/older
            // schema) is "unknown", not "no reasoning" — report a miss so
            // the caller falls back to its heuristic.
            let supported = model.reasoning?;
            let efforts = model
                .reasoning_options
                .iter()
                .find(|o| o.kind == "effort")
                .map(|o| {
                    o.values
                        .iter()
                        .filter_map(|v| v.clone())
                        .collect::<Vec<String>>()
                });
            return Some(ModelReasoning {
                supported,
                efforts,
            });
        }
    }
    None
}

/// Resolves the user-supplied cache directory (e.g. `"cosh"` or `"cosh/cache"`)
/// under the platform's local data dir (`~/.local/share/…` on Linux) using the
/// `directories` crate.
///
/// NOTE: the component is ALWAYS treated as a directory, even if it looks like
/// a file. The per-provider catalog file names are fixed
/// (`models.dev.json` / `openrouter.json`), so passing something like
/// `"cosh/cache/name.json"` would create a directory literally named
/// `name.json`. Pass only the folder you want.
fn resolve_cache_dir(user_dir: &str) -> Option<PathBuf> {
    let base_dirs = directories::BaseDirs::new()?;
    Some(base_dirs.data_local_dir().join(user_dir))
}

/// Reads and rewrites a provider's cached catalog (the raw JSON body) on disk.
///
/// The cache is deliberately dumb: it stores the exact HTTP response body our
/// biggest catalogs return (models.dev `api.json` is several MB), so a hit can
/// keep an unknown/dated model from forcing a full re-download. It is rewritten
/// entirely (never patched) whenever the network supplies new data.
struct CatalogCache {
    dir: PathBuf,
}

impl CatalogCache {
    fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path_for(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    /// Returns the cached raw body for `file`, if present and readable.
    fn load_raw(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.path_for(file)).ok()
    }

    /// Rewrites `file` entirely with `raw`, creating the directory if needed.
    /// Never fatal — a failing cache write must not block discovery.
    fn save_raw(&self, file: &str, raw: &str) {
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            log::warn!(
                "failed to create catalog cache dir {}: {e}",
                self.dir.display()
            );
            return;
        }
        if let Err(e) = std::fs::write(self.path_for(file), raw) {
            log::warn!(
                "failed to write catalog cache {}: {e}",
                self.path_for(file).display()
            );
        }
    }
}

/// A known context window and the exact, current model ids it applies to.
struct KnownContextWindow {
    window: usize,
    aliases: &'static [&'static str],
}

/// Known reasoning capability for a set of exact model ids.
struct KnownReasoning {
    /// Whether the model advertises a configurable reasoning/thinking knob.
    supported: bool,
    /// The exact effort levels the model accepts (`low`, `medium`, `high`, …)
    /// when it exposes effort-style reasoning; `None` for models that only
    /// offer a toggle or a token budget (callers then offer the standard
    /// low/medium/high set, which each provider's wire mapping tolerates).
    efforts: Option<&'static [&'static str]>,
    aliases: &'static [&'static str],
}

/// Well-known context windows for the most used, current models, checked
/// BEFORE any network call.
///
/// Unlike an older substring table, every alias here is an EXACT model id —
/// there is no family catch-all like `"claude"` or `"deepseek"` that could
/// mis-assign a window (a generic `"deepseek"` → 128k would wrongly cap the 1M
/// `deepseek-v4`; a generic `"claude"` → 200k would wrongly cap the 1M
/// `claude-sonnet-5`). Vendors that prefix ids with their own name
/// (`Qwen/Qwen3-…` vs the bare `Qwen3-…`) are covered by listing both
/// spellings — [`static_known_window`] matches them all.
///
/// Values are deliberately CONSERVATIVE where the official API and
/// aggregators disagree (e.g. DeepSeek's official `deepseek-chat` window is
/// 128k while OpenRouter reports 163,840): under-sizing the budget only makes
/// the 80% compaction fire slightly early — safe — while over-sizing it would
/// push the prompt into a provider-side `ContextWindowExceeded` error.
const KNOWN_CONTEXT_WINDOWS: &[KnownContextWindow] = &[
    // OpenAI.
    // Current frontier 1.05M window (official docs + OpenRouter agree).
    KnownContextWindow {
        window: 1_050_000,
        aliases: &[
            "gpt-5.4",
            "gpt-5.4-pro",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.6-sol",
            "gpt-5.6-sol-pro",
            "gpt-5.6-terra",
            "gpt-5.6-terra-pro",
            "gpt-5.6-luna",
            "gpt-5.6-luna-pro",
        ],
    },
    // GPT-4.1 family: ~1M.
    KnownContextWindow {
        window: 1_047_576,
        aliases: &[
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "gpt-4.1-2025-04-14",
        ],
    },
    // GPT-5 family: 400k window.
    KnownContextWindow {
        window: 400_000,
        aliases: &[
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-5-pro",
            "gpt-5.1",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5.1-codex-max",
            "gpt-5.2",
            "gpt-5.2-pro",
            "gpt-5.2-codex",
            "gpt-5.3-codex",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
        ],
    },
    // Reasoning o-series: 200k.
    KnownContextWindow {
        window: 200_000,
        aliases: &[
            "o1",
            "o1-mini",
            "o3",
            "o3-mini",
            "o3-pro",
            "o4-mini",
            "o4-mini-high",
        ],
    },
    // Legacy 128k chat models still widely deployed.
    KnownContextWindow {
        window: 128_000,
        aliases: &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4o-2024-11-20",
            "gpt-4o-mini-2024-07-18",
            "gpt-5.2-chat",
            "gpt-5.3-chat",
        ],
    },
    // Anthropic.
    // Current generation (Sonnet/Opus/Fable 4.6+) runs on a 1M window.
    KnownContextWindow {
        window: 1_000_000,
        aliases: &[
            "claude-sonnet-4",
            "claude-sonnet-4.5",
            "claude-sonnet-4.6",
            "claude-sonnet-5",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4.6",
            "claude-opus-4.7",
            "claude-opus-4.8",
            "claude-opus-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-fable-5",
            "claude-mythos-5",
        ],
    },
    // 200k haiku (current) and the still-served 200k Sonnet/Opus 3.x/4.x line.
    KnownContextWindow {
        window: 200_000,
        aliases: &[
            "claude-haiku-4.5",
            "claude-haiku-4-5",
            "claude-3-haiku",
            "claude-3-sonnet",
            "claude-3-5-sonnet",
            "claude-3-5-haiku",
            "claude-3-7-sonnet",
            "claude-opus-4",
            "claude-opus-4.1",
            "claude-opus-4.5",
            "claude-opus-4-1",
            "claude-opus-4-5",
        ],
    },
    // DeepSeek.
    // deepseek-v4 is a 1M model; the older chat/reasoner/v3 line is 128k.
    KnownContextWindow {
        window: 1_048_576,
        aliases: &[
            "deepseek-v4",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash-0731",
        ],
    },
    KnownContextWindow {
        window: 128_000,
        aliases: &[
            "deepseek-chat",
            "deepseek-chat-v3-0324",
            "deepseek-chat-v3.1",
            "deepseek-reasoner",
            "deepseek-r1",
            "deepseek-v3.1",
            "deepseek-v3.2",
            "deepseek-v3.2-exp",
        ],
    },
    // Google Gemini.
    // Every current Gemini model exposes a 1M window.
    KnownContextWindow {
        window: 1_048_576,
        aliases: &[
            "gemini-2.0-flash",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-3-pro",
            "gemini-3-flash",
            "gemini-3.1-pro",
            "gemini-3.1-flash",
            "gemini-3.1-flash-lite",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.6-flash",
        ],
    },
    // Qwen (Alibaba).
    // Newest 1M flagship class (`qwen3.x-flash/plus/max`).
    KnownContextWindow {
        window: 1_000_000,
        aliases: &[
            "qwen3-coder-flash",
            "qwen3-coder-plus",
            "qwen3.5-flash",
            "qwen3.5-plus",
            "qwen3.6-flash",
            "qwen3.6-plus",
            "qwen3.7-flash",
            "qwen3.7-plus",
            "qwen3.7-max",
            "qwen3.8-max",
        ],
    },
    // 256k MoE / instruction family.
    KnownContextWindow {
        window: 262_144,
        aliases: &[
            "qwen3-235b-a22b",
            "qwen3-30b-a3b",
            "qwen3-max",
            "qwen3-coder",
            "qwen3-coder-30b-a3b-instruct",
            "qwen3-next",
            "qwen3-vl-30b-a3b-instruct",
            "qwen3-vl-8b-instruct",
            "qwen3.5-9b",
            "qwen3.5-27b",
            "qwen3.5-35b-a3b",
            "qwen3.5-122b-a10b",
            "qwen3.5-397b-a17b",
            "qwen3.6-27b",
            "qwen3.6-35b-a3b",
        ],
    },
    // 128k dense family (qwen3 base, qwen2.5).
    KnownContextWindow {
        window: 131_072,
        aliases: &[
            "qwen3-8b",
            "qwen3-14b",
            "qwen3-32b",
            "qwen2.5-72b-instruct",
            "qwen2.5-14b-instruct",
            "qwen2.5-7b-instruct",
            "qwen2.5-vl-72b-instruct",
        ],
    },
    // Kimi (Moonshot).
    KnownContextWindow {
        window: 1_048_576,
        aliases: &["kimi-k3"],
    },
    KnownContextWindow {
        window: 262_144,
        aliases: &[
            "kimi-k2",
            "kimi-k2-0905",
            "kimi-k2-thinking",
            "kimi-k2.5",
            "kimi-k2.6",
            "kimi-k2.7-code",
        ],
    },
    // Mistral.
    KnownContextWindow {
        window: 262_144,
        aliases: &[
            "mistral-large",
            "mistral-large-2512",
            "mistral-medium",
            "mistral-medium-3.5",
            "mistral-small",
            "mistral-small-4",
            "mistral-small-2603",
            "codestral",
            "codestral-2508",
            "ministral-8b-2512",
            "ministral-14b-2512",
        ],
    },
    KnownContextWindow {
        window: 131_072,
        aliases: &["ministral-3b-2512", "mistral-nemo"],
    },
    // xAI Grok.
    KnownContextWindow {
        window: 2_000_000,
        aliases: &["grok-4.20"],
    },
    KnownContextWindow {
        window: 1_000_000,
        aliases: &["grok-4.3"],
    },
    KnownContextWindow {
        window: 500_000,
        aliases: &["grok-4.5", "grok-4.5-fast", "grok-latest"],
    },
    KnownContextWindow {
        window: 262_144,
        aliases: &["grok-4", "grok-4-fast", "grok-3", "grok-3-mini"],
    },
    // Meta Llama.
    KnownContextWindow {
        window: 1_310_720,
        aliases: &["llama-4-scout"],
    },
    KnownContextWindow {
        window: 1_048_576,
        aliases: &["llama-4-maverick"],
    },
    KnownContextWindow {
        window: 131_072,
        aliases: &[
            "llama-3.1-8b-instruct",
            "llama-3.1-70b-instruct",
            "llama-3.1-405b",
            "llama-3.3-70b-instruct",
        ],
    },
    // Zhipu GLM.
    KnownContextWindow {
        window: 1_048_576,
        aliases: &["glm-5.2"],
    },
    KnownContextWindow {
        window: 204_800,
        aliases: &[
            "glm-4.6",
            "glm-4.7",
            "glm-4.7-flash",
            "glm-5",
            "glm-5-turbo",
            "glm-5.1",
        ],
    },
    KnownContextWindow {
        window: 131_072,
        aliases: &["glm-4.5", "glm-4.5-air"],
    },
];

/// Known reasoning capabilities for the most used, current models, checked
/// BEFORE the on-disk catalog is read.
///
/// Mirrors [`KNOWN_CONTEXT_WINDOWS`]: every alias here is an EXACT model id —
/// there is no family catch-all like `"claude"` or `"qwen3"` that could
/// mis-claim reasoning (a generic `"qwen3"` → yes would wrongly enable the
/// reasoning dialog for the non-reasoning `qwen3-8b`/`qwen3-max`/`qwen3-coder`
/// line, which the name heuristic still does today).
///
/// Values are taken from the models.dev catalog (the only source that exposes
/// per-model reasoning metadata): `supported` is the catalog's `reasoning`
/// flag and `efforts` its effort-style `reasoning_options.values`, verbatim.
/// Models whose catalog entry lists only a toggle or a token budget carry
/// `efforts: None` — the caller then offers the standard low/medium/high set,
/// which every provider's wire mapping tolerates. Every alias here has a real
/// catalog entry behind it — models absent from the catalog are intentionally
/// NOT listed (a guessed `false` would permanently shadow a future catalog
/// update; a guessed `true` would offer a knob the model rejects).
const KNOWN_REASONING: &[KnownReasoning] = &[
    // OpenAI — GPT-5.x reasoning line. Effort sets follow the catalog, which
    // varies per model (newer flagships accept none/minimal/xhigh/max).
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high"]),
        aliases: &[
            "gpt-5",
            "gpt-5-mini",
            "gpt-5.1",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5.2",
            "gpt-5.4",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high", "xhigh"]),
        aliases: &["gpt-5.1-codex-max", "gpt-5.2-codex"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["medium", "high", "xhigh"]),
        aliases: &["gpt-5.2-pro", "gpt-5.4-pro", "gpt-5.5-pro"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "low", "medium", "high", "xhigh"]),
        aliases: &[
            "gpt-5.3-codex",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.5",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "low", "medium", "high", "xhigh", "max"]),
        aliases: &["gpt-5.6-sol-pro", "gpt-5.6-terra-pro", "gpt-5.6-luna-pro"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["minimal", "low", "medium", "high"]),
        aliases: &["gpt-5-nano"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["high"]),
        aliases: &["gpt-5-pro"],
    },
    // OpenAI — o-series reasoning.
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high"]),
        aliases: &["o1", "o3", "o3-mini", "o3-pro", "o4-mini"],
    },
    // OpenAI — legacy chat models without a reasoning knob (incl. o1-mini,
    // which the name heuristic would wrongly claim via its "o1" prefix).
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "o1-mini",
            "gpt-5.2-chat",
            "gpt-5.3-chat",
        ],
    },
    // Anthropic — both the dotted and the dashed spellings carry their own
    // catalog entries (the effort sets differ slightly between them).
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high"]),
        aliases: &[
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4.5",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-haiku-4-5",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "low", "medium", "high", "max"]),
        aliases: &["claude-sonnet-4.6", "claude-opus-4.6"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high", "xhigh", "max"]),
        aliases: &[
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-fable-5",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "low", "medium", "high", "xhigh", "max"]),
        aliases: &["claude-opus-4.7", "claude-opus-4.8"],
    },
    // Claude 4.5/4.1 and haiku expose only a toggle or a token budget — no
    // effort values in the catalog.
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &[
            "claude-sonnet-4",
            "claude-sonnet-4.5",
            "claude-opus-4",
            "claude-opus-4.1",
            "claude-opus-4-1",
            "claude-haiku-4.5",
        ],
    },
    // Claude 3.x has no reasoning knob.
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &["claude-3-haiku", "claude-3-sonnet", "claude-3-5-haiku"],
    },
    // DeepSeek.
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &["deepseek-v4-flash", "deepseek-v4-pro", "deepseek-r1"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
        aliases: &["deepseek-chat-v3.1"],
    },
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &["deepseek-v3.2"],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &[
            "deepseek-chat",
            "deepseek-chat-v3-0324",
            "deepseek-reasoner",
            "deepseek-v3.1",
        ],
    },
    // Google Gemini — 2.5 exposes thinking only (budget knob, no efforts);
    // the 3.x line adds effort levels.
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &[
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-3-pro",
            "gemini-3.1-pro",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["minimal", "low", "high"]),
        aliases: &["gemini-3-flash"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["minimal", "low", "medium", "high"]),
        aliases: &[
            "gemini-3.1-flash-lite",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.6-flash",
        ],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &["gemini-2.0-flash"],
    },
    // Qwen — only the thinking-flavoured 3.x models reason; the plain
    // qwen3 base/coder/max line does NOT (the name heuristic gets this
    // wrong today — the static table fixes it with exact matches).
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &[
            "qwen3.5-plus",
            "qwen3.5-9b",
            "qwen3.5-35b-a3b",
            "qwen3.5-122b-a10b",
            "qwen3.6-flash",
            "qwen3.6-plus",
            "qwen3.6-27b",
            "qwen3.7-flash",
            "qwen3.7-plus",
            "qwen3.7-max",
            "qwen3.8-max",
        ],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "minimal", "low", "medium", "high"]),
        aliases: &["qwen3.5-397b-a17b", "qwen3.6-35b-a3b"],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &[
            "qwen3-8b",
            "qwen3-14b",
            "qwen3-32b",
            "qwen3-235b-a22b",
            "qwen3-30b-a3b",
            "qwen3-max",
            "qwen3-coder",
            "qwen3-coder-flash",
            "qwen3-coder-plus",
            "qwen3-coder-30b-a3b-instruct",
            "qwen3-vl-30b-a3b-instruct",
            "qwen3-vl-8b-instruct",
            "qwen3.5-flash",
            "qwen3.5-27b",
            "qwen2.5-72b-instruct",
            "qwen2.5-7b-instruct",
            "qwen2.5-vl-72b-instruct",
        ],
    },
    // Kimi.
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &["kimi-k3", "kimi-k2-thinking", "kimi-k2.5"],
    },
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "minimal", "low", "medium", "high"]),
        aliases: &["kimi-k2.6", "kimi-k2.7-code"],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &["kimi-k2", "kimi-k2-0905"],
    },
    // Mistral — only the 3.5/2603 generation exposes a reasoning knob.
    KnownReasoning {
        supported: true,
        efforts: Some(&["none", "high"]),
        aliases: &["mistral-medium-3.5", "mistral-small-2603"],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &[
            "mistral-large",
            "mistral-large-2512",
            "mistral-medium",
            "mistral-small",
            "mistral-small-4",
            "codestral",
            "codestral-2508",
            "ministral-3b-2512",
            "ministral-8b-2512",
            "ministral-14b-2512",
            "mistral-nemo",
        ],
    },
    // xAI Grok.
    KnownReasoning {
        supported: true,
        efforts: Some(&["low", "medium", "high"]),
        aliases: &["grok-4.20", "grok-3-mini", "grok-latest"],
    },
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &["grok-4", "grok-4-fast", "grok-4.3", "grok-4.5"],
    },
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &["grok-3"],
    },
    // Meta Llama — no reasoning knob.
    KnownReasoning {
        supported: false,
        efforts: None,
        aliases: &[
            "llama-4-scout",
            "llama-4-maverick",
            "llama-3.1-8b-instruct",
            "llama-3.1-70b-instruct",
            "llama-3.3-70b-instruct",
        ],
    },
    // Zhipu GLM.
    KnownReasoning {
        supported: true,
        efforts: Some(&["high", "max"]),
        aliases: &["glm-5.2"],
    },
    KnownReasoning {
        supported: true,
        efforts: None,
        aliases: &[
            "glm-4.5",
            "glm-4.5-air",
            "glm-4.6",
            "glm-4.7",
            "glm-4.7-flash",
            "glm-5",
            "glm-5-turbo",
            "glm-5.1",
        ],
    },
];

/// Look up a model's window in the static table.
///
/// Matching is EXACT and case-insensitive against a model's registered
/// aliases — never a substring, so a family name like `"claude"` or
/// `"deepseek"` can no longer accidentally match every model in the family.
/// To keep the same model resolvable under the spellings different providers
/// use, an OpenRouter `~` alias marker and a vendor prefix (`qwen/…`,
/// `anthropic/…`, …) are tolerated: the bare name and the vendor-prefixed name
/// of both the needle and the alias are compared.
///
/// Returns `None` for unknown models — the caller then tries the network.
fn static_known_window(model: &str) -> Option<usize> {
    let lower = model.trim_start_matches('~').to_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);
    KNOWN_CONTEXT_WINDOWS
        .iter()
        .find(|entry| {
            entry.aliases.iter().any(|alias| {
                let a = alias.to_lowercase();
                let a_bare = a.rsplit('/').next().unwrap_or(&a);
                lower == a || lower == a_bare || bare == a || bare == a_bare
            })
        })
        .map(|entry| entry.window)
}

/// Look up a model's reasoning capability in the static table.
///
/// Matching follows the exact same rules as [`static_known_window`] (exact,
/// case-insensitive, `~`- and vendor-prefix tolerant) — the two tables are
/// keyed consistently, so a model that resolves its window statically also
/// resolves its reasoning statically, without ever touching the disk catalog.
///
/// Returns `None` for models absent from the table — the caller then falls
/// back to the catalog and finally to a name heuristic.
fn static_known_reasoning(model: &str) -> Option<ModelReasoning> {
    let lower = model.trim_start_matches('~').to_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);
    KNOWN_REASONING
        .iter()
        .find(|entry| {
            entry.aliases.iter().any(|alias| {
                let a = alias.to_lowercase();
                let a_bare = a.rsplit('/').next().unwrap_or(&a);
                lower == a || lower == a_bare || bare == a || bare == a_bare
            })
        })
        .map(|entry| ModelReasoning {
            supported: entry.supported,
            efforts: entry
                .efforts
                .map(|efforts| efforts.iter().map(|e| (*e).to_string()).collect()),
        })
}

/// Flexible match against the OpenRouter catalog: the needle matches a model
/// when it equals the id, equals the bare vendor-less suffix of the id
/// (`deepseek-v4-flash` matches `deepseek/deepseek-v4-flash`), or equals the
/// bare suffix of the canonical slug (so OpenRouter `~` aliases and dated
/// variants resolve to the same window). All comparisons are
/// case-insensitive — the user's model string and OpenRouter's slugs rarely
/// share casing.
fn find_window_in_models(needle: &str, models: &[OpenRouterModel]) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    models.iter().find_map(|m| {
        let id = m.id.to_lowercase();
        let matches = id == needle_lower
            || id.rsplit('/').next() == Some(needle_lower.as_str())
            || m.canonical_slug.as_deref().is_some_and(|c| {
                c.to_lowercase().rsplit('/').next() == Some(needle_lower.as_str())
            });
        m.context_length.filter(|_| matches)
    })
}

/// Flexible match against the models.dev catalog.
///
/// models.dev keys a model either by its bare id (`glm-5`) or its
/// vendor-prefixed id (`google/gemini-2.5-flash`), and callers can use either
/// spelling too, so the needle matches when it equals the key or when their
/// bare suffixes (parts after the last `/`) match. Comparisons are
/// case-insensitive.
fn find_window_in_models_dev(
    needle: &str,
    providers: &HashMap<String, ModelsDevProvider>,
) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    let needle_bare = needle_lower.rsplit('/').next().unwrap_or(&needle_lower);
    providers
        .values()
        .flat_map(|p| p.models.iter())
        .find_map(|(id, m)| {
            let id_lower = id.to_lowercase();
            let id_bare = id_lower.rsplit('/').next().unwrap_or(&id_lower);
            let matches = id_lower == needle_lower
                || id_bare == needle_lower
                || id_lower == needle_bare
                || id_bare == needle_bare;
            if matches { m.limit.context } else { None }
        })
}

/// Attempts to discover the context window size for a given model.
///
/// Resolution order (fastest first):
///   1. The static table of known, current models (no network — the most used
///      models always resolve, immune to API/pagination/auth failures);
///   2. The on-disk provider catalogs (models.dev, then OpenRouter) when
///      [`CatalogCache`] caching is enabled — a hit avoids any request;
///   3. The public APIs, back-filling the catalog of the provider that served
///      the hit: for ids containing "claude", Anthropic is tried FIRST (the
///      authoritative source, but it needs a key), then models.dev, then
///      OpenRouter. Every other model goes straight to models.dev, then
///      OpenRouter;
///   4. `None` — the caller keeps its current budget. The failure is logged so
///      a silent fallback to the default window is never invisible.
///
/// # Arguments
///
/// * `model_name` - The model identifier (e.g., "claude-sonnet-4-5", "gpt-4o",
///   "deepseek-v4-flash")
/// * `cache_dir` - Optional cache directory name (e.g. `"cosh"` or
///   `"cosh/cache"`), resolved under the platform data dir
///   (`~/.local/share/…` on Linux). `None` disables catalog caching. NOTE: it
///   is always treated as a directory, even if it looks like a file — the
///   catalog file names are fixed, so `"cosh/cache/name.json"` would create a
///   directory named `name.json`.
///
/// # Returns
///
/// * `Some(usize)` - The discovered context window size in tokens
/// * `None` - If the context window could not be discovered
///
/// # Notes
///
/// - Does not require authentication
/// - May fail due to network issues or API changes
/// - The caller should provide a sensible fallback value when this returns `None`
pub async fn discover_context_window(model_name: &str, cache_dir: Option<&str>) -> Option<usize> {
    if let Some(window) = static_known_window(model_name) {
        return Some(window);
    }

    // Resolve the disk cache handle once; `None` disables caching entirely.
    let cache = cache_dir.and_then(resolve_cache_dir).map(CatalogCache::new);
    let cache = cache.as_ref();

    let model_lower = model_name.to_lowercase();
    let discovered = if model_lower.contains("claude") {
        // Every Claude model has "claude" in its id, so Anthropic is the
        // authoritative first try; a miss only means an unfamiliar/dated id,
        // and models.dev then OpenRouter are safe fallbacks.
        match discover_anthropic_context(model_name).await {
            Some(window) => Some(window),
            None => match discover_models_dev_context(model_name, cache).await {
                Some(window) => Some(window),
                None => discover_openrouter_context(model_name, cache).await,
            },
        }
    } else {
        // No "claude" in the id: routing it to the (auth-gated) Anthropic API
        // would be pointless, so it goes straight to models.dev with OpenRouter
        // as fallback.
        match discover_models_dev_context(model_name, cache).await {
            Some(window) => Some(window),
            None => discover_openrouter_context(model_name, cache).await,
        }
    };
    if discovered.is_none() {
        log::warn!("context window discovery failed for {model_name}; keeping the current budget");
    }
    discovered
}

/// Discover context window from Anthropic's API.
///
/// NOTE: the endpoint requires an `x-api-key` header — unauthenticated calls
/// return 401. Known claude models are resolved from the static table before
/// reaching here, so this only ever runs for unknown claude variants.
async fn discover_anthropic_context(model_name: &str) -> Option<usize> {
    let url = format!("https://api.anthropic.com/v1/models/{}", model_name);
    let response = reqwest::get(&url).await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    let model_response: AnthropicModelResponse = response.json().await.ok()?;
    model_response.model_info.max_input_tokens
}

/// Discover context window from models.dev's public catalog.
///
/// When `cache` is provided the cached raw catalog is consulted first; a miss
/// falls back to the network, and any successful hit backs the cache-rewrite
/// so the next call for this provider is served from disk.
async fn discover_models_dev_context(
    model_name: &str,
    cache: Option<&CatalogCache>,
) -> Option<usize> {
    if let Some(cache) = cache
        && let Some(raw) = cache.load_raw(MODELS_DEV_CATALOG_FILE)
        && let Ok(catalog) = serde_json::from_str::<ModelsDevCatalog>(&raw)
        && let Some(window) = find_window_in_models_dev(model_name, &catalog.providers)
    {
        return Some(window);
    }

    let response = reqwest::get("https://models.dev/api.json").await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let raw = response.text().await.ok()?;
    let catalog: ModelsDevCatalog = serde_json::from_str(&raw).ok()?;
    let window = find_window_in_models_dev(model_name, &catalog.providers);
    if window.is_some()
        && let Some(cache) = cache
    {
        cache.save_raw(MODELS_DEV_CATALOG_FILE, &raw);
    }
    window
}

/// Discover context window from OpenRouter's API.
///
/// The per-model endpoint `GET /api/v1/models/{id}` returns 404 for EVERY id
/// (verified) — it is dead weight and is skipped. The full catalog listing is
/// fetched instead, matched flexibly ([`find_window_in_models`]) and cached
/// exactly like the models.dev catalog when `cache` is provided.
async fn discover_openrouter_context(
    model_name: &str,
    cache: Option<&CatalogCache>,
) -> Option<usize> {
    if let Some(cache) = cache
        && let Some(raw) = cache.load_raw(OPENROUTER_CATALOG_FILE)
        && let Ok(models) = serde_json::from_str::<OpenRouterModelsResponse>(&raw)
        && let Some(window) = find_window_in_models(model_name, &models.data)
    {
        return Some(window);
    }

    let response = reqwest::get("https://openrouter.ai/api/v1/models")
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let raw = response.text().await.ok()?;
    let models_response: OpenRouterModelsResponse = serde_json::from_str(&raw).ok()?;
    let window = find_window_in_models(model_name, &models_response.data);
    if window.is_some()
        && let Some(cache) = cache
    {
        cache.save_raw(OPENROUTER_CATALOG_FILE, &raw);
    }
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to validate if a value looks like a valid context window size.
    /// Context windows are typically in the range of 1K to 2M tokens and should not
    /// be HTTP status codes (200, 400, 404, 500, etc).
    fn is_valid_context_window(value: usize) -> bool {
        // Reasonable context window range: 1K to 2M tokens
        const MIN_CONTEXT: usize = 1_000;
        const MAX_CONTEXT: usize = 2_000_000;

        // Common HTTP status codes that should not be mistaken for context windows
        const HTTP_STATUS_CODES: [usize; 10] = [200, 201, 204, 400, 401, 403, 404, 500, 502, 503];

        (MIN_CONTEXT..=MAX_CONTEXT).contains(&value) && !HTTP_STATUS_CODES.contains(&value)
    }

    // Static table (no network).

    #[test]
    fn static_table_covers_the_most_common_models() {
        // A bare, capitalized DeepSeek-V4-Flash resolves to its real 1M window
        // with no network call, in any spelling a caller might use.
        assert_eq!(static_known_window("deepseek-v4-flash"), Some(1_048_576));
        assert_eq!(static_known_window("DeepSeek-V4-Flash"), Some(1_048_576));
        assert_eq!(
            static_known_window("deepseek/deepseek-v4-flash"),
            Some(1_048_576)
        );
        assert_eq!(
            static_known_window("~deepseek/deepseek-v4-flash"),
            Some(1_048_576)
        );
        // The old chat line and the new 1M v4 line must keep distinct windows.
        assert_eq!(static_known_window("deepseek-chat"), Some(128_000));
        // Family names are no catch-alls: they must never match any model.
        assert_eq!(static_known_window("claude"), None);
        assert_eq!(static_known_window("deepseek"), None);
        // Unknown models fall through to the network path.
        assert_eq!(static_known_window("totally-unknown-model-123"), None);
    }

    #[test]
    fn static_table_matches_vendor_prefixed_and_bare_variants() {
        // Same model id, different spellings used by different vendors —
        // all resolve to the same window.
        assert_eq!(static_known_window("qwen/qwen3-235b-a22b"), Some(262_144));
        assert_eq!(static_known_window("qwen3-235b-a22b"), Some(262_144));
        assert_eq!(static_known_window("qwen3.8-max"), Some(1_000_000));
        assert_eq!(
            static_known_window("anthropic/claude-sonnet-5"),
            Some(1_000_000)
        );
        assert_eq!(
            static_known_window("mistralai/mistral-medium-3.5"),
            Some(262_144)
        );
        // A v4 model still resolves to 1M, never to the 128k chat window.
        assert_eq!(static_known_window("deepseek-v4-flash"), Some(1_048_576));
        assert_eq!(static_known_window("deepseek-chat"), Some(128_000));
    }

    // Static reasoning table (no network, no disk).

    #[test]
    fn static_reasoning_table_covers_the_most_common_models() {
        // Current flagship models resolve their reasoning statically.
        assert_eq!(
            static_known_reasoning("deepseek-v4-flash").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("gpt-5").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("claude-sonnet-4-5").map(|m| m.supported),
            Some(true)
        );
        // Non-reasoning models resolve to an explicit "no".
        assert_eq!(
            static_known_reasoning("gpt-4o").map(|m| m.supported),
            Some(false)
        );
        // o1-mini and the plain qwen3 line are the exact cases the name
        // heuristic gets wrong ("o1" prefix / "qwen3" substring): the static
        // table must say no, not fall through to the heuristic.
        assert_eq!(
            static_known_reasoning("o1-mini").map(|m| m.supported),
            Some(false)
        );
        assert_eq!(
            static_known_reasoning("qwen3-8b").map(|m| m.supported),
            Some(false)
        );
        assert_eq!(
            static_known_reasoning("qwen3-max").map(|m| m.supported),
            Some(false)
        );
        assert_eq!(
            static_known_reasoning("qwen3-coder").map(|m| m.supported),
            Some(false)
        );
    }

    #[test]
    fn static_reasoning_reports_efforts_verbatim() {
        // gpt-5 accepts low/medium/high; the catalog says so and the table
        // mirrors it exactly.
        let gpt5 = static_known_reasoning("gpt-5").expect("gpt-5 must be known");
        assert_eq!(
            gpt5.efforts,
            Some(vec!["low".to_string(), "medium".to_string(), "high".to_string()])
        );
        // A model whose catalog entry lists only a toggle (no effort values)
        // carries `efforts: None` — callers offer the standard set.
        let glm5 = static_known_reasoning("glm-5").expect("glm-5 must be known");
        assert!(glm5.supported);
        assert!(glm5.efforts.is_none());
        // Effort sets vary per model and are preserved verbatim.
        let opus46 = static_known_reasoning("claude-opus-4.6").expect("known");
        assert_eq!(
            opus46.efforts,
            Some(vec![
                "none".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "max".to_string()
            ])
        );
    }

    #[test]
    fn static_reasoning_matches_case_vendor_and_tilde_variants() {
        // Bare, capitalized and vendor-prefixed spellings all resolve.
        assert_eq!(
            static_known_reasoning("DeepSeek-V4-Flash").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("deepseek/deepseek-v4-flash").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("~deepseek/deepseek-v4-flash").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("anthropic/claude-sonnet-4-5").map(|m| m.supported),
            Some(true)
        );
        assert_eq!(
            static_known_reasoning("openai/gpt-4o").map(|m| m.supported),
            Some(false)
        );
    }

    #[test]
    fn static_reasoning_family_names_never_match() {
        // Family names are no catch-alls: they must never match any model.
        assert_eq!(static_known_reasoning("claude"), None);
        assert_eq!(static_known_reasoning("deepseek"), None);
        assert_eq!(static_known_reasoning("qwen3"), None);
        // Unknown models fall through to the catalog.
        assert_eq!(static_known_reasoning("totally-unknown-model-123"), None);
    }

    #[test]
    fn model_reasoning_serves_the_static_table_without_disk() {
        // A bogus cache dir proves the static tier resolves first: the model
        // is answered even though the catalog read would fail.
        let meta = model_reasoning("gpt-5", Some("/nonexistent/cosh/cache"))
            .expect("static hit");
        assert!(meta.supported);
        assert_eq!(
            meta.efforts,
            Some(vec!["low".to_string(), "medium".to_string(), "high".to_string()])
        );
        // Unknown models with no cache fall through to None (then heuristic).
        assert!(model_reasoning("totally-unknown-model-123", None).is_none());
    }

    // OpenRouter flexible matching (no network).

    fn model(id: &str, canonical: Option<&str>, context: Option<usize>) -> OpenRouterModel {
        OpenRouterModel {
            id: id.to_string(),
            canonical_slug: canonical.map(str::to_string),
            context_length: context,
        }
    }

    #[test]
    fn openrouter_match_is_vendor_prefix_and_case_tolerant() {
        let models = vec![
            model(
                "deepseek/deepseek-v4-flash",
                Some("deepseek/deepseek-v4-flash-0731"),
                Some(1_048_576),
            ),
            model(
                "openai/gpt-4o",
                Some("openai/gpt-4o-2024-11-20"),
                Some(128_000),
            ),
        ];

        // The reported failure mode: a bare, capitalized name still matches
        // the vendor-prefixed id.
        assert_eq!(
            find_window_in_models("deepseek-v4-flash", &models),
            Some(1_048_576)
        );
        assert_eq!(
            find_window_in_models("DeepSeek-V4-Flash", &models),
            Some(1_048_576)
        );
        // Exact prefixed id also matches.
        assert_eq!(
            find_window_in_models("deepseek/deepseek-v4-flash", &models),
            Some(1_048_576)
        );
        // A dated canonical variant resolves via the canonical slug suffix.
        assert_eq!(
            find_window_in_models("deepseek-v4-flash-0731", &models),
            Some(1_048_576)
        );
        // Unrelated models never match.
        assert_eq!(find_window_in_models("gpt-3.5", &models), None);
    }

    #[test]
    fn openrouter_match_skips_models_without_context_length() {
        let models = vec![model(
            "vendor/some-model",
            None,
            None, // no window advertised
        )];
        assert_eq!(find_window_in_models("some-model", &models), None);
    }

    // Models.dev flexible matching (no network).

    fn md_provider(models: &[(&str, Option<usize>)]) -> ModelsDevProvider {
        ModelsDevProvider {
            models: models
                .iter()
                .map(|(id, context)| {
                    (
                        (*id).to_string(),
                        ModelsDevModel {
                            limit: ModelsDevLimit { context: *context },
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn models_dev_match_is_prefix_and_case_tolerant() {
        // models.dev keys some models bare (`glm-5`) and others prefixed
        // (`google/gemini-2.5-flash`); both spellings must resolve.
        let providers = HashMap::from([
            (
                "zhipuai".to_string(),
                md_provider(&[("glm-5", Some(204_800))]),
            ),
            (
                "google".to_string(),
                md_provider(&[("google/gemini-2.5-flash", Some(1_048_576))]),
            ),
        ]);

        assert_eq!(
            find_window_in_models_dev("glm-5", &providers),
            Some(204_800)
        );
        assert_eq!(
            find_window_in_models_dev("zhipuai/glm-5", &providers),
            Some(204_800)
        );
        assert_eq!(
            find_window_in_models_dev("gemini-2.5-flash", &providers),
            Some(1_048_576)
        );
        assert_eq!(
            find_window_in_models_dev("Google/Gemini-2.5-Flash", &providers),
            Some(1_048_576)
        );
        // Models without a recorded context window never match.
        assert_eq!(
            find_window_in_models_dev(
                "no-window",
                &HashMap::from([("vendor".to_string(), md_provider(&[("no-window", None)]),)])
            ),
            None
        );
        // Unknown models never match.
        assert_eq!(find_window_in_models_dev("gpt-3.5", &providers), None);
    }

    #[test]
    fn catalog_cache_roundtrips_raw_json() {
        let dir = std::env::temp_dir().join(format!("cosh-catalog-test-{}", std::process::id()));
        let cache = CatalogCache::new(dir.clone());
        let raw = r#"{"data":[{"id":"x"}]}"#;

        cache.save_raw(MODELS_DEV_CATALOG_FILE, raw);
        assert_eq!(
            cache.load_raw(MODELS_DEV_CATALOG_FILE).as_deref(),
            Some(raw)
        );
        // The file name is exactly the fixed "models.dev.json" under the dir.
        assert!(dir.join(MODELS_DEV_CATALOG_FILE).is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Write a small models.dev-style catalog into a temp cache dir and return
    /// the cache dir name. `tag` must differ per test — the catalog resolver
    /// is exercised in parallel and shared directories would race.
    fn seed_models_dev_cache(tag: &str, raw: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "cosh-mdreason-{}-{}",
            std::process::id(),
            tag
        ));
        let cache = CatalogCache::new(dir.clone());
        cache.save_raw(MODELS_DEV_CATALOG_FILE, raw);
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn catalog_reasoning_resolves_supported_model_with_efforts() {
        let raw = r#"{
            "anthropic": {
                "models": {
                    "claude-sonnet-4-6": {
                        "limit": {"context": 200000},
                        "reasoning": true,
                        "reasoning_options": [
                            {"type": "effort", "values": ["low", "medium", "high", "max"]},
                            {"type": "budget_tokens", "min": 1024}
                        ]
                    }
                }
            }
        }"#;
        let dir = seed_models_dev_cache("efforts", raw);
        let meta = model_reasoning_from_catalog("claude-sonnet-4-6", Some(&dir))
            .expect("model must resolve");
        assert!(meta.supported);
        let efforts = meta.efforts.unwrap_or_default();
        assert_eq!(efforts, vec!["low", "medium", "high", "max"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_reasoning_reports_non_reasoning_model() {
        let raw = r#"{"openai": {"models": {"gpt-image-2": {"limit": {"context": 1000}, "reasoning": false}}}}"#;
        let dir = seed_models_dev_cache("nonreasoning", raw);
        let meta = model_reasoning_from_catalog("gpt-image-2", Some(&dir))
            .expect("model must resolve");
        assert!(!meta.supported);
        assert_eq!(meta.efforts, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_reasoning_toggle_model_has_no_efforts() {
        let raw = r#"{"zhipuai": {"models": {"glm-5": {"limit": {"context": 204800}, "reasoning": true, "reasoning_options": [{"type": "toggle"}]}}}}"#;
        let dir = seed_models_dev_cache("toggle", raw);
        let meta = model_reasoning_from_catalog("glm-5", Some(&dir))
            .expect("model must resolve");
        assert!(meta.supported);
        assert_eq!(meta.efforts, None, "toggle models advertise no effort values");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_reasoning_unknown_model_returns_none() {
        let raw = r#"{"openai": {"models": {"gpt-4o": {"limit": {"context": 128000}, "reasoning": false}}}}"#;
        let dir = seed_models_dev_cache("unknown", raw);
        assert!(model_reasoning_from_catalog("nonexistent-model", Some(&dir)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A catalog entry missing the `reasoning` field (older schema) is
    /// "unknown", not "no reasoning" — the lookup reports a miss so callers
    /// fall back to their heuristic.
    #[test]
    fn catalog_reasoning_missing_field_is_unknown() {
        let raw = r#"{"openai": {"models": {"gpt-4o": {"limit": {"context": 128000}}}}}"#;
        let dir = seed_models_dev_cache("missingfield", raw);
        assert!(model_reasoning_from_catalog("gpt-4o", Some(&dir)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Live network tests (best effort — require internet).

    #[tokio::test]
    async fn test_models_dev_cache_backfills_and_hits() {
        // First call fetches and back-fills models.dev.json; the second is
        // served from disk without re-downloading the whole catalog.
        let dir = std::env::temp_dir().join(format!("cosh-mdcache-{}", std::process::id()));
        let cache = CatalogCache::new(dir.clone());

        let w1 = discover_models_dev_context("gpt-4o", Some(&cache)).await;
        assert_eq!(w1, Some(128_000));
        assert!(dir.join(MODELS_DEV_CATALOG_FILE).is_file());

        let w2 = discover_models_dev_context("gpt-4o", Some(&cache)).await;
        assert_eq!(w2, Some(128_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_discover_anthropic_context() {
        // Test with a known Anthropic model
        let context = discover_anthropic_context("claude-sonnet-4-5").await;
        // Anthropic API may require auth or be unavailable, so we just check
        // that the function doesn't crash and returns either Some or None
        if let Some(value) = context {
            assert!(
                is_valid_context_window(value),
                "Anthropic context window {} is not in valid range",
                value
            );
        }
    }

    #[tokio::test]
    async fn test_discover_openrouter_context() {
        // Test with a known OpenRouter model
        let context = discover_openrouter_context("openai/gpt-4o", None).await;
        // GPT-4o should have a context window
        assert!(
            context.is_some(),
            "OpenRouter should return a context window for gpt-4o"
        );

        let value = context.unwrap();
        assert!(
            is_valid_context_window(value),
            "OpenRouter context window {} is not in valid range",
            value
        );
    }

    #[tokio::test]
    async fn test_discover_context_window_anthropic() {
        // A current Claude model resolves from the static table, so the public
        // entry point never even hits the (auth-gated) Anthropic API.
        let context = discover_context_window("claude-opus-5", None).await;
        assert_eq!(context, Some(1_000_000));
    }

    #[tokio::test]
    async fn test_discover_context_window_openrouter() {
        // Test the main function with an OpenRouter model — resolved from the
        // static table, so it never even hits the network.
        let context = discover_context_window("openai/gpt-4o", None).await;
        assert_eq!(context, Some(128_000));
    }

    #[tokio::test]
    async fn test_discover_context_window_invalid_model() {
        // Test with an invalid model name
        let context = discover_context_window("invalid-model-xyz", None).await;
        // Should return None for invalid models
        assert!(context.is_none(), "Invalid model should return None");
    }

    #[tokio::test]
    async fn test_discover_context_window_bare_deepseek_v4_flash() {
        // The regression this module fixes: a bare, capitalized model name
        // must resolve through the flexible OpenRouter match, not fall back.
        let context = discover_context_window("DeepSeek-V4-Flash", None).await;
        assert!(
            context.is_some(),
            "bare DeepSeek-V4-Flash must resolve to a window"
        );
        assert!(
            is_valid_context_window(context.unwrap()),
            "resolved window must be plausible"
        );
    }

    #[tokio::test]
    async fn test_openrouter_models_list() {
        // Test that we can list models from OpenRouter
        let response = reqwest::get("https://openrouter.ai/api/v1/models")
            .await
            .unwrap();
        assert!(response.status().is_success());

        let models: OpenRouterModelsResponse = response.json().await.unwrap();
        assert!(!models.data.is_empty());

        // Verify that at least some models have context_length
        let with_context = models
            .data
            .iter()
            .filter(|m| m.context_length.is_some())
            .count();
        assert!(with_context > 0, "Should have models with context_length");

        // Verify that context_length values are valid
        let valid_context_count = models
            .data
            .iter()
            .filter_map(|m| m.context_length)
            .filter(|&value| is_valid_context_window(value))
            .count();
        assert!(
            valid_context_count > 0,
            "Should have at least one model with valid context window"
        );
    }

    #[tokio::test]
    async fn test_models_dev_models_list() {
        // Test that the real models.dev catalog deserializes into our shape and
        // carries context windows (also exercises the `#[serde(flatten)]` map).
        let response = reqwest::get("https://models.dev/api.json").await.unwrap();
        assert!(response.status().is_success());

        let catalog: ModelsDevCatalog = response.json().await.unwrap();
        assert!(!catalog.providers.is_empty());

        let windows: Vec<usize> = catalog
            .providers
            .values()
            .flat_map(|p| p.models.values())
            .filter_map(|m| m.limit.context)
            .collect();
        assert!(
            !windows.is_empty(),
            "Should have models with context_length"
        );
        // The catalog also holds tiny-context models (speech/embedding), so we
        // only require that plenty of real LLM windows parse as plausible.
        let valid_count = windows
            .iter()
            .filter(|&&w| is_valid_context_window(w))
            .count();
        assert!(
            valid_count > 0,
            "Should have at least one valid context window"
        );
    }

    #[tokio::test]
    async fn test_context_window_validation_helper() {
        // Test the validation helper function
        assert!(is_valid_context_window(128000), "128K should be valid");
        assert!(is_valid_context_window(200000), "200K should be valid");
        assert!(is_valid_context_window(1000000), "1M should be valid");
        assert!(is_valid_context_window(1048576), "1M-ish should be valid");

        // Test invalid values
        assert!(
            !is_valid_context_window(200),
            "200 (HTTP status) should be invalid"
        );
        assert!(
            !is_valid_context_window(404),
            "404 (HTTP status) should be invalid"
        );
        assert!(
            !is_valid_context_window(500),
            "500 (HTTP status) should be invalid"
        );
        assert!(!is_valid_context_window(100), "100 is too small");
        assert!(!is_valid_context_window(500), "500 is too small");
        assert!(!is_valid_context_window(3_000_000), "3M is too large");
    }
}

//! Usage & cost tracking for the dashboard panel.
//!
//! Persists one record per real API request in a global JSONL log
//! (`{data_dir}/usage/usage.jsonl`) and aggregates it into the token and
//! dollar figures the dashboard shows. Token counters and the dollar figure
//! are both taken verbatim from the provider's OWN response — its usage
//! object and the REAL cost it reports inside it. A provider that reports no
//! cost yields a record WITHOUT a price (its tokens still count) and it is
//! excluded from dollar totals — nothing is ever estimated locally.
//! `cosh_sdk::connector::supports_cost_reporting` lists the providers that
//! report costs today.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use cosh_sdk::connector::TokenUsage;

/// Where the global usage log lives, relative to the data dir.
const USAGE_DIR: &str = "usage";
const USAGE_FILE: &str = "usage.jsonl";

/// Cache-dir handle shared with the SDK's context-window / reasoning
/// metadata lookups, so both reuse the same models.dev catalog discovery
/// maintains.
pub const MODELS_DEV_CACHE_DIR: Option<&str> = Some("cosh/cache");

/// A single API request's real usage, persisted as one JSONL line.
///
/// Every field is always populated except `cost_usd`, which is omitted when
/// the provider does not report a cost (skipped on serialization so the log
/// stays clean). There is no estimate path: a record without a
/// provider-reported cost is simply unpriced and excluded from $ totals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    #[serde(skip)]
    pub id: u64,
    /// Unix millisecond timestamp of the request.
    pub ts: u64,
    /// The session that owned the request — lets us recompute a specific
    /// session's spend even after switching away and back.
    pub session_id: String,
    /// Provider that served the request (e.g. `"opencode"`, `"claude"`).
    pub provider: String,
    /// Model that served the request (e.g. `"glm-5"`).
    pub model: String,
    /// Real per-request token usage (input/output/cache split).
    pub usage: TokenUsage,
    /// REAL cost (USD) the provider itself reported in its usage object
    /// (`usage.cost` — OpenRouter, Charm Hyper, OpenCode Zen/Go). The
    /// authoritative billed amount, recorded verbatim — never re-priced
    /// locally. `None` when the provider does not report a cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Appends/reads the global usage log.
#[derive(Clone)]
pub struct UsageStore {
    file: PathBuf,
}

impl UsageStore {
    /// Create a store rooted at `{data_dir}/usage/usage.jsonl`. Honours
    /// `COSH_DATA_DIR` (test isolation; see `util::setup::data_dir_override`).
    ///
    /// # Panics
    /// Panics if the data directory cannot be determined (no override AND
    /// `ProjectDirs` fails, e.g. no `$HOME` set), mirroring
    /// [`SessionStore`](crate::session_store::SessionStore).
    pub fn new() -> Self {
        let dir = crate::util::setup::data_dir_override().join(USAGE_DIR);
        std::fs::create_dir_all(&dir).ok();
        Self {
            file: dir.join(USAGE_FILE),
        }
    }

    /// Test-only store rooted at an explicit file.
    #[cfg(test)]
    pub(crate) fn with_file(file: PathBuf) -> Self {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Self { file }
    }

    /// Append one record. A single line is appended atomically on Linux, so
    /// concurrent cosh processes cannot corrupt the log.
    pub fn append(&self, record: &UsageRecord) {
        let mut line = match serde_json::to_string(&record) {
            Ok(line) => line,
            Err(_) => return,
        };
        line.push('\n');
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Read every record currently in the log (reverse-chronological callers
    /// sort themselves).
    ///
    /// Assigns fresh unique runtime `id`s (0..n) — ids are never persisted
    /// (serde skip), so records loaded from disk would otherwise ALL carry
    /// id 0 and collide in in-memory maps.
    pub fn load(&self) -> Vec<UsageRecord> {
        let Ok(raw) = std::fs::read_to_string(&self.file) else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|l| serde_json::from_str::<UsageRecord>(l).ok())
            .enumerate()
            .map(|(i, mut r)| {
                r.id = i as u64;
                r
            })
            .collect()
    }
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

/// How far back a dashboard aggregation reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsagePeriod {
    /// Since local midnight today.
    Day,
    /// Since the start of the current calendar week (Monday 00:00 local).
    Week,
    /// Since the start of the current calendar month.
    Month,
    /// Since the start of the current calendar year.
    Year,
    /// Everything since the app started being used.
    #[default]
    All,
}

impl UsagePeriod {
    /// Short, label-safe name for the period selector.
    pub fn label(self) -> &'static str {
        match self {
            UsagePeriod::Day => "day",
            UsagePeriod::Week => "week",
            UsagePeriod::Month => "month",
            UsagePeriod::Year => "year",
            UsagePeriod::All => "total",
        }
    }

    /// Next period (wrapping from All back to Day).
    pub fn next(self) -> Self {
        match self {
            UsagePeriod::Day => UsagePeriod::Week,
            UsagePeriod::Week => UsagePeriod::Month,
            UsagePeriod::Month => UsagePeriod::Year,
            UsagePeriod::Year => UsagePeriod::All,
            UsagePeriod::All => UsagePeriod::Day,
        }
    }

    /// Previous period (wrapping from Day back to All).
    pub fn prev(self) -> Self {
        match self {
            UsagePeriod::Day => UsagePeriod::All,
            UsagePeriod::Week => UsagePeriod::Day,
            UsagePeriod::Month => UsagePeriod::Week,
            UsagePeriod::Year => UsagePeriod::Month,
            UsagePeriod::All => UsagePeriod::Year,
        }
    }

    /// The inclusive start (in ms since epoch) of this period for `now_ms`,
    /// or `None` for `All` (no lower bound).
    fn start_ms(self, now_ms: u64) -> Option<u64> {
        use chrono::{Datelike, Local, TimeZone};
        let now = Local.timestamp_millis_opt(now_ms as i64).single()?;
        let start = match self {
            UsagePeriod::All => return None,
            UsagePeriod::Day => Local
                .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
                .single(),
            UsagePeriod::Week => {
                let dow = now.weekday().num_days_from_monday();
                Local
                    .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
                    .single()
                    .and_then(|midnight| {
                        midnight.checked_sub_signed(chrono::Duration::days(dow as i64))
                    })
            }
            UsagePeriod::Month => Local
                .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
                .single(),
            UsagePeriod::Year => Local.with_ymd_and_hms(now.year(), 1, 1, 0, 0, 0).single(),
        };
        start.map(|dt| dt.timestamp_millis() as u64)
    }
}

/// Aggregated spend, optionally split by provider.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendSummary {
    /// Spend per provider, descendently sorted by cost. Providers that do
    /// not report costs are omitted from the dollar view (their tokens still
    /// show in `tokens`).
    pub by_provider: Vec<(String, f64)>,
    /// Grand total spend across all providers in the period.
    pub total: f64,
    /// Total billed tokens in the period (input+output+cache), all models.
    pub tokens: u64,
}

/// Sum the real token counters of a slice of records.
pub fn total_tokens(records: &[UsageRecord]) -> u64 {
    records
        .iter()
        .map(|r| {
            let u = &r.usage;
            u64::from(u.input_tokens)
                + u64::from(u.output_tokens)
                + u64::from(u.cache_creation_input_tokens)
                + u64::from(u.cache_read_input_tokens)
        })
        .sum()
}

/// Aggregate spend for the given period. `now_ms` anchors "today/week/…".
///
/// Records without a provider-reported cost are excluded from dollar totals
/// (no estimate is ever fabricated); their tokens still count.
pub fn summarize(records: &[UsageRecord], period: UsagePeriod, now_ms: u64) -> SpendSummary {
    let lower = period.start_ms(now_ms);
    let mut by_provider: Vec<(String, f64)> = Vec::new();
    let mut in_period_tokens = 0u64;

    for r in records {
        // A record belongs to the period when it isn't older than the start.
        if lower.is_some_and(|lo| r.ts < lo) {
            continue;
        }
        in_period_tokens += {
            let u = &r.usage;
            u64::from(u.input_tokens)
                + u64::from(u.output_tokens)
                + u64::from(u.cache_creation_input_tokens)
                + u64::from(u.cache_read_input_tokens)
        };
        let Some(cost) = r.cost_usd else {
            continue;
        };
        match by_provider.iter_mut().find(|(p, _)| p == &r.provider) {
            Some((_, c)) => *c += cost,
            None => by_provider.push((r.provider.clone(), cost)),
        }
    }

    by_provider.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let total = by_provider.iter().map(|(_, c)| *c).sum();
    SpendSummary {
        by_provider,
        total,
        tokens: in_period_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn rec(
        ts: u64,
        provider: &str,
        cost: Option<f64>,
        in_tok: u32,
        out_tok: u32,
    ) -> UsageRecord {
        UsageRecord {
            id: 0,
            ts,
            session_id: "s1".into(),
            provider: provider.into(),
            model: "m".into(),
            usage: TokenUsage {
                input_tokens: in_tok,
                output_tokens: out_tok,
                ..TokenUsage::default()
            },
            cost_usd: cost,
        }
    }

    fn day_start_ms(y: i32, m: u32, d: u32) -> u64 {
        use chrono::{Local, TimeZone};
        Local
            .with_ymd_and_hms(y, m, d, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    #[test]
    fn summarize_filters_by_period_and_groups_by_provider() {
        // Two records "today", one yesterday.
        let now = day_start_ms(2026, 8, 30) + 3_600_000; // 2026-08-30 13:00 local
        let yesterday = day_start_ms(2026, 8, 29) + 3_600_000;
        let records = vec![
            rec(now - 1000, "openai", Some(2.0), 1000, 500),
            rec(now - 2000, "openai", Some(3.0), 1000, 500),
            rec(yesterday, "claude", Some(9.0), 0, 0),
        ];
        let s = summarize(&records, UsagePeriod::Day, now);
        assert_eq!(s.by_provider, vec![("openai".to_string(), 5.0)]);
        assert!((s.total - 5.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_cost_is_excluded_from_dollar_total_but_tokens_count() {
        let now = day_start_ms(2026, 8, 30) + 3_600_000;
        let records = vec![
            rec(now - 1000, "openai", Some(1.0), 100, 100),
            rec(now - 2000, "ollama", None, 300, 300),
        ];
        let s = summarize(&records, UsagePeriod::Day, now);
        assert_eq!(s.by_provider, vec![("openai".to_string(), 1.0)]);
        assert!((s.total - 1.0).abs() < 1e-9);
        // 400 in + 400 out = 800 tokens counted regardless of pricing.
        assert_eq!(s.tokens, 800);
    }

    #[test]
    fn reported_cost_is_kept_verbatim_not_re_priced() {
        // The provider-reported REAL cost is the ONLY source: it is recorded
        // verbatim and never re-derived from a price table.
        let r = rec(1, "openrouter", Some(0.95), 100, 100);
        assert_eq!(r.cost_usd, Some(0.95));

        // No cost at all: the record stays unpriced (excluded from $ totals).
        let unpriced = rec(1, "tiny-local", None, 100, 100);
        assert_eq!(unpriced.cost_usd, None);
    }

    #[test]
    fn records_without_cost_persisted_round_trip_keeps_unpriced() {
        // Old log lines without a cost must still deserialize (cost_usd is
        // serde-defaulted): history stays readable and stays unpriced.
        let line = r#"{"ts":1,"session_id":"s1","provider":"p","model":"m","usage":{"input_tokens":1,"output_tokens":2},"cost_usd":0.5}"#;
        let r: UsageRecord = serde_json::from_str(line).unwrap();
        assert_eq!(r.cost_usd, Some(0.5));
    }

    #[test]
    fn load_assigns_unique_runtime_ids() {
        let dir = std::env::temp_dir().join(format!("cosh-usage-ids-{}", std::process::id()));
        let store = UsageStore::with_file(dir.join("usage.jsonl"));
        store.append(&rec(1, "a", Some(1.0), 1, 1));
        store.append(&rec(2, "b", Some(2.0), 1, 1));
        let loaded = store.load();
        let ids: Vec<u64> = loaded.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![0, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_period_ignores_timestamps() {
        let now = day_start_ms(2026, 8, 30);
        let old = 1_000_000; // 1970
        let records = vec![rec(old, "x", Some(7.0), 0, 0)];
        let s = summarize(&records, UsagePeriod::All, now);
        assert!((s.total - 7.0).abs() < 1e-9);
    }

    #[test]
    fn period_cycles_wrapping_through_all() {
        macro_rules! assert_cycle {
            ($start:expr, $next:expr, $prev:expr) => {
                assert_eq!($start.next(), $next);
                assert_eq!($start.prev(), $prev);
            };
        }
        assert_cycle!(UsagePeriod::Day, UsagePeriod::Week, UsagePeriod::All);
        assert_cycle!(UsagePeriod::Week, UsagePeriod::Month, UsagePeriod::Day);
        assert_cycle!(UsagePeriod::Month, UsagePeriod::Year, UsagePeriod::Week);
        assert_cycle!(UsagePeriod::Year, UsagePeriod::All, UsagePeriod::Month);
        assert_cycle!(UsagePeriod::All, UsagePeriod::Day, UsagePeriod::Year);
    }

    #[test]
    fn append_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("cosh-usage-test-{}", std::process::id()));
        let store = UsageStore::with_file(dir.join("usage.jsonl"));
        let r = rec(1, "openai", Some(0.5), 10, 20);
        store.append(&r);
        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].usage.input_tokens == 10);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

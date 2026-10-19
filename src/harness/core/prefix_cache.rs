//! Exact-prefix cache diagnostics probe (zero behavioral effect).

use cosh_sdk::connector::ChatMessage;

/// Fingerprint of ONE wire request: the model it was sent to, the system
/// prompt hash and the per-message hash chain — the exact material the
/// provider's exact-prefix cache keys on. See [`PrefixCacheProbe`].
pub(super) struct RequestFingerprint {
    model: String,
    system_hash: u64,
    message_hashes: Vec<u64>,
}

/// Exact-prefix cache probe (diagnostics, zero behavioral effect).
///
/// Providers cache the request prefix byte-exactly: valid up to the FIRST
/// divergent byte, re-billed after it. This probe fingerprints every agent
/// request and compares it with the previous one from the SAME model; the
/// first divergent position classifies the cache outcome:
///
/// - system prompt diverged (position 0) → the whole conversation prefix
///   is re-billed — counted in `full_breaks` (the pathological case this
///   probe exists to catch: dynamic content interpolated into the header);
/// - the message HISTORY diverged before its last message → mid-history
///   mutation, cache lost from there on — counted in `mid_history_mutations`
///   (also pathological: history items are append-only by design);
/// - the history SHRANK → compaction/revert rewrote it legitimately —
///   counted separately in `history_shrinks`, never conflated with the
///   pathological counters;
/// - divergence at the last message (steering/tail blocks churn per
///   request) or a pure append → the cached prefix stays valid, not
///   counted;
/// - zero divergence → the same request re-sent (mid-stream retry), the
///   cache is fully valid, not counted.
///
/// Everything is MEASURED, never inferred: a counter only moves when the
/// previous fingerprint actually diverges at that position, so heuristic
/// false positives ("the TODO block changed, therefore the cache broke")
/// are impossible by construction. Two caveats to keep the counters honest:
/// the probe is keyed by model — a model switch resets the snapshot instead
/// of comparing incompatible prefixes — and `MID_HISTORY_MUTATION` also
/// counts USER-driven history rewrites (revert/fork/session restore), which
/// the probe cannot distinguish from a model-side mutation; read that
/// counter with the session's user actions in mind.
///
/// Hashes chain `xxh3_64_with_seed` over the fields every provider sees
/// (role, content, tool-call identity, tool-result id, image payloads).
/// `thinking_blocks`/`thought_signature` are deliberately excluded: they are
/// immutable per item (write-once at the item's birth, replayed verbatim),
/// so a divergence confined to them cannot occur in a live session — and
/// including them would couple the probe to per-provider wire quirks.
#[derive(Default)]
pub(super) struct PrefixCacheProbe {
    previous: Option<RequestFingerprint>,
    requests_compared: u64,
    full_breaks: u64,
    mid_history_mutations: u64,
    history_shrinks: u64,
}

impl PrefixCacheProbe {
    /// Hash one wire message: role → content → tool-call identity → tool
    /// result id → image payloads, chained seeds (wire-visible fields only).
    fn message_hash(message: &ChatMessage) -> u64 {
        use xxhash_rust::xxh3::xxh3_64_with_seed;
        let mut h = xxh3_64_with_seed(message.role.as_bytes(), 0);
        h = xxh3_64_with_seed(message.content.as_deref().unwrap_or("").as_bytes(), h);
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                h = xxh3_64_with_seed(call.id.as_bytes(), h);
                h = xxh3_64_with_seed(call.function.name.as_bytes(), h);
                h = xxh3_64_with_seed(call.function.arguments.as_bytes(), h);
            }
        }
        if let Some(id) = &message.tool_call_id {
            h = xxh3_64_with_seed(id.as_bytes(), h);
        }
        if let Some(images) = &message.images {
            for image in images {
                h = xxh3_64_with_seed(image.media_type.as_bytes(), h);
                h = xxh3_64_with_seed(image.data.as_bytes(), h);
            }
        }
        h
    }

    /// Fingerprint one request.
    pub(super) fn fingerprint(model: &str, system: &str, messages: &[ChatMessage]) -> RequestFingerprint {
        use xxhash_rust::xxh3::xxh3_64;
        RequestFingerprint {
            model: model.to_string(),
            system_hash: xxh3_64(system.as_bytes()),
            message_hashes: messages.iter().map(Self::message_hash).collect(),
        }
    }

    /// Compare the new fingerprint with the previous one, classify the
    /// cache outcome, advance the counters and log one line. The new
    /// fingerprint always becomes the previous one afterwards.
    pub(super) fn record(&mut self, fingerprint: RequestFingerprint) {
        let Some(previous) = self.previous.take() else {
            // First request of this probe: nothing to compare, just seed.
            log::info!(
                target: "cosh::harness",
                "prefix cache probe: seeded (model={}, messages={})",
                fingerprint.model,
                fingerprint.message_hashes.len(),
            );
            self.previous = Some(fingerprint);
            return;
        };
        self.requests_compared += 1;
        let verdict;
        if fingerprint.model != previous.model {
            // Incompatible prefix: reset the snapshot, classify nothing.
            verdict = "model_switch";
        } else if fingerprint.system_hash != previous.system_hash {
            verdict = "FULL_BREAK";
            self.full_breaks += 1;
        } else {
            let common = previous.message_hashes.len().min(fingerprint.message_hashes.len());
            let divergence = (0..common)
                .find(|&i| previous.message_hashes[i] != fingerprint.message_hashes[i]);
            verdict = match divergence {
                None if fingerprint.message_hashes.len() == previous.message_hashes.len() => {
                    "identical_retry"
                }
                None => "append",
                Some(_i) if fingerprint.message_hashes.len() < previous.message_hashes.len() => {
                    self.history_shrinks += 1;
                    "history_shrunk"
                }
                Some(i) if i + 1 == common => "tail_change",
                Some(_) => {
                    self.mid_history_mutations += 1;
                    "MID_HISTORY_MUTATION"
                }
            };
        }
        log::info!(
            target: "cosh::harness",
            "prefix cache probe: verdict={verdict} msgs {}→{} system_changed={} | \
             totals: compared={} full_breaks={} mid_history={} shrinks={}",
            previous.message_hashes.len(),
            fingerprint.message_hashes.len(),
            fingerprint.system_hash != previous.system_hash,
            self.requests_compared,
            self.full_breaks,
            self.mid_history_mutations,
            self.history_shrinks,
        );
        self.previous = Some(fingerprint);
    }
}

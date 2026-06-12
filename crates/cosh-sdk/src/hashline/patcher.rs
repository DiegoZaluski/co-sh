//! High-level patch orchestrator. Reads each section's target file via the
//! configured [`Filesystem`], strips BOM and normalizes line endings,
//! validates the section snapshot tag (with [`Recovery`]), applies the
//! result back through the same [`Filesystem`].
//!
//! Two layers:
//!
//! - [`Patcher::apply`] — high-level, all-or-nothing. Preflights every
//!   section in memory before any write hits disk, then commits in order.
//! - [`Patcher::prepare`] / [`Patcher::commit`] — granular primitives
//!   for callers that need per-section control (e.g. batched LSP flush,
//!   custom interleaving). `prepare` performs all the read-side work,
//!   validates the section snapshot tag (with recovery), and applies the
//!   edits in memory. `commit` writes the prepared result and records a
//!   fresh snapshot.
//!
//! Because `prepare` already runs the full apply, a multi-section batch is
//! naturally all-or-nothing: by the time any `commit` runs, every section
//! has been validated.
//!
//! The patcher itself is stateless across calls; reuse one instance per
//! filesystem configuration.
use std::sync::{Arc, Mutex};

use super::apply::apply_edits;
use super::block::{has_block_edit, resolve_block_edits, ResolveBlockEditsOptions};
use super::format::{compute_file_hash, format_hashline_header};
use super::fs::{is_not_found, Filesystem, WriteResult};
use super::input::{Patch, PatchSection};
use super::messages::{missing_snapshot_tag_message, HEADTAIL_DRIFT_WARNING};
use super::mismatch::{MismatchDetails, MismatchError};
use super::normalize::{
    detect_line_ending, normalize_to_lf, restore_line_endings, strip_bom, BomResult, LineEnding,
};
use super::recovery::{Recovery, RecoveryArgs, RecoveryResult};
use super::snapshots::SnapshotStore;
use super::types::{ApplyResult, BlockResolver, Edit, ResolveAction};

/// Per-section result returned by [`Patcher::apply`] / [`Patcher::commit`].
#[derive(Debug, Clone)]
pub struct PatchSectionResult {
    /// Section path (as authored, after cwd-resolution at parse time).
    pub path: String,
    /// Filesystem-canonical key for this section (e.g. absolute path).
    pub canonical_path: String,
    /// `"noop"` when the apply produced no change; otherwise `"create"` / `"update"`.
    pub op: PatchOp,
    /// Pre-edit text (LF-normalized, BOM-stripped).
    pub before: String,
    /// Post-edit text (LF-normalized, BOM-stripped). For `"noop"` equals `before`.
    pub after: String,
    /// Same text as `after` but with the original BOM and line ending restored.
    pub persisted: String,
    /// Final text that the [`Filesystem`] actually wrote (may differ if the FS transformed it).
    pub written: String,
    /// 4-hex opaque snapshot tag for `after`. Use to anchor follow-up edits.
    pub file_hash: String,
    /// Hashline section header (`¶path#tag`) of the post-edit content.
    pub header: String,
    /// 1-indexed first changed line in `after`, or `None` for noops.
    pub first_changed_line: Option<u32>,
    /// Warnings collected by the parser, applier, and (optionally) recovery.
    pub warnings: Vec<String>,
}

/// Operation type for a committed section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOp {
    /// File was created by this patch.
    Create,
    /// Existing file was modified by this patch.
    Update,
    /// The apply produced no change.
    Noop,
}

/// Result of applying a full patch via [`Patcher::apply`].
#[derive(Debug, Clone)]
pub struct PatcherApplyResult {
    /// Results for each section, in original patch order.
    pub sections: Vec<PatchSectionResult>,
}

/// Opaque token returned by [`Patcher::prepare`]. Carries the section, the
/// raw file content read off disk, and the in-memory apply result.
/// [`Patcher::commit`] just writes the [`PreparedSection::apply_result`].
#[derive(Debug, Clone)]
pub struct PreparedSection {
    /// Section path (as authored, after cwd-resolution at parse time).
    pub path: String,
    /// Filesystem-canonical key for this section (e.g. absolute path).
    pub canonical_path: String,
    /// Whether the target file existed before this apply.
    pub exists: bool,
    /// Raw content read off the filesystem (with BOM, original line endings).
    pub raw_content: String,
    /// BOM sequence stripped from raw content (empty string if none).
    pub bom: String,
    /// Line ending style detected in the original content.
    pub line_ending: LineEnding,
    /// LF-normalized, BOM-stripped pre-edit text.
    pub normalized: String,
    /// In-memory apply result (post-edit text, first changed line, warnings).
    pub apply_result: ApplyResult,
    /// Warnings collected during section parsing.
    pub parse_warnings: Vec<String>,
}

impl PreparedSection {
    /// Convenience: returns true when the apply produced no change.
    pub fn is_noop(&self) -> bool {
        self.apply_result.text == self.normalized
    }
}

fn has_anchor_scoped_edit(edits: &[Edit]) -> bool {
    edits.iter().any(|edit| match edit {
        Edit::Delete { .. } => true,
        Edit::Block { .. } => true,
        Edit::Insert { cursor, .. } => {
            matches!(
                cursor,
                super::types::Cursor::BeforeAnchor(_) | super::types::Cursor::AfterAnchor(_)
            )
        }
    })
}

fn assert_section_hash_present(
    section_path: &str,
    file_hash: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if file_hash.is_some() {
        return Ok(());
    }
    Err(missing_snapshot_tag_message(section_path).into())
}

fn recovery_to_apply_result(result: RecoveryResult) -> ApplyResult {
    ApplyResult {
        text: result.text,
        first_changed_line: result.first_changed_line.map(|l| l as u32),
        warnings: result.warnings,
    }
}

fn merge_warnings(sources: &[Option<&[String]>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for warnings in sources.iter().flatten() {
        out.extend(warnings.iter().cloned());
    }
    out
}

fn assert_unique_canonical_paths(prepared: &[PreparedSection]) -> Result<(), String> {
    let mut seen = std::collections::HashMap::<String, String>::new();
    for entry in prepared {
        if let Some(previous) = seen.get(&entry.canonical_path) {
            return Err(format!(
                "Multiple hashline sections resolve to the same file ({} and {}). \
                 Merge their ops under one header before applying.",
                previous, entry.path
            ));
        }
        seen.insert(entry.canonical_path.clone(), entry.path.clone());
    }
    Ok(())
}

/// High-level patcher. Wires a [`Filesystem`] and a required
/// [`SnapshotStore`] together with the parsing + applying core.
///
/// Construct once per FS configuration; reuse across patches.
pub struct Patcher<F: Filesystem, S: SnapshotStore> {
    fs: F,
    snapshots: Arc<Mutex<S>>,
    recovery: Recovery<Arc<Mutex<S>>>,
    block_resolver: Option<BlockResolver>,
}

impl<F: Filesystem, S: SnapshotStore> Patcher<F, S> {
    /// Create a new patcher.
    ///
    /// `snapshots` is required — section tags are opaque store pointers and
    /// without a store the patcher cannot validate or recover them.
    pub fn new(fs: F, snapshots: S, block_resolver: Option<BlockResolver>) -> Self {
        let snapshots = Arc::new(Mutex::new(snapshots));
        let recovery = Recovery::new(Arc::clone(&snapshots));
        Self {
            fs,
            snapshots,
            recovery,
            block_resolver,
        }
    }

    /// Apply every section in `patch`. `prepare` runs the full apply for each
    /// section in memory before any write hits the filesystem, so a
    /// multi-section batch is naturally all-or-nothing. Returns one
    /// [`PatchSectionResult`] per section in the original patch order.
    pub async fn apply(
        &mut self,
        patch: &Patch,
    ) -> Result<PatcherApplyResult, Box<dyn std::error::Error + Send + Sync>> {
        // Single-section fast path.
        if patch.sections.len() == 1 {
            let prepared = self.prepare(&patch.sections[0]).await?;
            return Ok(PatcherApplyResult {
                sections: vec![self.commit(prepared).await?],
            });
        }

        // Prepare every section first so any failure (stale hash, missing
        // file, parse error, in-memory no-op) surfaces before any write.
        let mut prepared: Vec<PreparedSection> = Vec::new();
        for section in &patch.sections {
            prepared.push(self.prepare(section).await?);
        }
        assert_unique_canonical_paths(&prepared)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
        for entry in &prepared {
            if entry.is_noop() {
                return Err(
                    format!("Edits to {} resulted in no changes being made.", entry.path).into(),
                );
            }
        }

        let mut results: Vec<PatchSectionResult> = Vec::new();
        for entry in prepared {
            results.push(self.commit(entry).await?);
        }
        Ok(PatcherApplyResult { sections: results })
    }

    /// Run the preflight pass only: read, parse, validate, apply-in-memory.
    /// No writes hit the filesystem. Use for CI checks and dry runs.
    pub async fn preflight(
        &mut self,
        patch: &Patch,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut prepared: Vec<PreparedSection> = Vec::new();
        for section in &patch.sections {
            prepared.push(self.prepare(section).await?);
        }
        assert_unique_canonical_paths(&prepared)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
        for entry in &prepared {
            if entry.is_noop() {
                return Err(
                    format!("Edits to {} resulted in no changes being made.", entry.path).into(),
                );
            }
        }
        Ok(())
    }

    /// Read a section's target file, parse the section, validate the snapshot
    /// tag (with recovery), and apply the edits in memory. Returns a
    /// [`PreparedSection`] which can be fed to [`commit`](Self::commit) to land
    /// the result on the filesystem.
    ///
    /// Returns an error on parse error, missing-file-for-anchored-edit, or
    /// unrecovered tag mismatch ([`MismatchError`]).
    pub async fn prepare(
        &mut self,
        section: &PatchSection,
    ) -> Result<PreparedSection, Box<dyn std::error::Error + Send + Sync>> {
        let parsed = section.parse();
        let edits = &parsed.0;
        let parse_warnings = parsed.1.clone();
        assert_section_hash_present(&section.path, section.file_hash.as_deref())?;

        let canonical_path = self.fs.canonical_path(&section.path).await;
        self.fs.preflight_write(&section.path).await?;
        let (exists, raw_content) = self.try_read(&section.path).await?;
        if !exists {
            return Err(format!(
                "File not found: {}. Use the write tool to create new files.",
                section.path
            )
            .into());
        }

        let BomResult { bom, text } = strip_bom(&raw_content);
        let line_ending = detect_line_ending(&text);
        let normalized = normalize_to_lf(&text);

        let apply_result = self.apply_with_recovery(ApplyWithRecoveryArgs {
            section,
            canonical_path: &canonical_path,
            exists,
            normalized: &normalized,
            edits,
        })?;

        Ok(PreparedSection {
            path: section.path.clone(),
            canonical_path,
            exists,
            raw_content,
            bom,
            line_ending,
            normalized,
            apply_result,
            parse_warnings,
        })
    }

    /// Commit a previously [`prepare`](Self::prepare)d section to the
    /// filesystem. Restores line endings and BOM, writes via the
    /// [`Filesystem`], and records a fresh snapshot in the
    /// [`SnapshotStore`] keyed by the filesystem-canonical path.
    pub async fn commit(
        &mut self,
        prepared: PreparedSection,
    ) -> Result<PatchSectionResult, Box<dyn std::error::Error + Send + Sync>> {
        let PreparedSection {
            path,
            canonical_path,
            exists,
            raw_content,
            bom,
            line_ending,
            normalized,
            apply_result,
            parse_warnings,
        } = prepared;

        let after = apply_result.text;
        let warnings = merge_warnings(&[Some(&parse_warnings), Some(&apply_result.warnings)]);

        if after == normalized {
            let hash = self.record_full_snapshot(&canonical_path, &normalized);
            return Ok(PatchSectionResult {
                path: path.clone(),
                canonical_path,
                op: PatchOp::Noop,
                before: normalized.clone(),
                after: normalized,
                persisted: raw_content.clone(),
                written: raw_content,
                file_hash: hash.clone(),
                header: format_hashline_header(&path, &hash),
                first_changed_line: None,
                warnings,
            });
        }

        let persisted = bom + &restore_line_endings(&after, line_ending);
        let write: WriteResult = self.fs.write_text(&path, &persisted).await?;
        let file_hash = self.record_full_snapshot(&canonical_path, &after);
        let op = if exists {
            PatchOp::Update
        } else {
            PatchOp::Create
        };

        Ok(PatchSectionResult {
            path: path.clone(),
            canonical_path,
            op,
            before: normalized,
            after,
            persisted,
            written: write.text,
            file_hash: file_hash.clone(),
            header: format_hashline_header(&path, &file_hash),
            first_changed_line: apply_result.first_changed_line,
            warnings,
        })
    }

    async fn try_read(
        &self,
        path: &str,
    ) -> Result<(bool, String), Box<dyn std::error::Error + Send + Sync>> {
        match self.fs.read_text(path).await {
            Ok(content) => Ok((true, content)),
            Err(err) => {
                if is_not_found(err.as_ref()) {
                    Ok((false, String::new()))
                } else {
                    Err(err)
                }
            }
        }
    }

    fn record_full_snapshot(&mut self, canonical_path: &str, normalized: &str) -> String {
        self.snapshots
            .lock()
            .unwrap()
            .record(canonical_path, normalized)
    }
}

struct ApplyWithRecoveryArgs<'a> {
    section: &'a PatchSection,
    canonical_path: &'a str,
    exists: bool,
    normalized: &'a str,
    edits: &'a [Edit],
}

impl<F: Filesystem, S: SnapshotStore> Patcher<F, S> {
    fn apply_with_recovery(
        &mut self,
        args: ApplyWithRecoveryArgs,
    ) -> Result<ApplyResult, Box<dyn std::error::Error + Send + Sync>> {
        let ApplyWithRecoveryArgs {
            section,
            canonical_path,
            exists,
            normalized,
            edits,
        } = args;
        let expected: Option<&str> = if exists {
            section.file_hash.as_deref()
        } else {
            None
        };
        let live_matches = expected
            .map(|exp| compute_file_hash(normalized) == exp)
            .unwrap_or(true);

        // Resolve `replace block N:` edits to concrete ranges before recovery
        // runs. Block anchors are expressed against the snapshot the section tag
        // names, so resolve against that exact text:
        //   - live content matches the tag (or there is no tag) → resolve against
        //     the live, normalized content;
        //   - the file drifted → resolve against the tagged snapshot's text so the
        //     resulting ranges flow through the 3-way-merge recovery below.
        // When a block edit needs the tagged snapshot but it is unavailable, the
        // range cannot be placed safely — reject with a MismatchError (re-read).
        let resolved: Vec<Edit> = if has_block_edit(edits) {
            let base = match expected {
                None => normalized.to_string(),
                Some(_) if live_matches => normalized.to_string(),
                Some(exp) => {
                    // Pre-fetch snapshot outside the block so the Mutex lock is
                    // released before we call any &mut self methods (e.g. mismatch_error).
                    let snapshot = self.snapshots.lock().unwrap().by_hash(canonical_path, exp);
                    match snapshot {
                        Some(s) => s.text,
                        None => {
                            let actual_file_hash =
                                self.record_full_snapshot(canonical_path, normalized);
                            return Err(Box::new(MismatchError::new(MismatchDetails {
                                path: Some(section.path.clone()),
                                expected_file_hash: exp.to_string(),
                                actual_file_hash,
                                file_lines: normalized.split('\n').map(|s| s.to_string()).collect(),
                                anchor_lines: section.collect_anchor_lines(),
                                hash_recognized: false,
                            })));
                        }
                    }
                }
            };
            resolve_block_edits(
                edits,
                &base,
                &section.path,
                self.block_resolver,
                Some(ResolveBlockEditsOptions {
                    on_unresolved: ResolveAction::Throw,
                }),
            )
        } else {
            edits.to_vec()
        };

        if expected.is_none() {
            return Ok(apply_edits(normalized, &resolved));
        }
        // Whole-file unchanged → the tag still names the live content, so an
        // edit anchored at ANY line (displayed or not) is safe to apply.
        if live_matches {
            return Ok(apply_edits(normalized, &resolved));
        }
        // Head/tail-only inserts are position-stable: "start"/"end" cannot move
        // with content drift, so a stale tag is non-fatal. Apply onto the live
        // content and warn instead of hard-failing — unlike an anchored
        // mismatch, which cannot be safely relocated and must reject.
        if !has_anchor_scoped_edit(&resolved) {
            let mut result = apply_edits(normalized, &resolved);
            let mut warnings = vec![HEADTAIL_DRIFT_WARNING.to_string()];
            warnings.extend(result.warnings);
            result.warnings = warnings;
            return Ok(result);
        }
        // File drifted: try to replay the edit against the version the tag
        // names and 3-way-merge it onto the live content.
        let recovered = self.recovery.try_recover(RecoveryArgs {
            path: canonical_path.to_string(),
            current_text: normalized.to_string(),
            file_hash: expected.unwrap().to_string(),
            edits: resolved,
        });
        if let Some(result) = recovered {
            return Ok(recovery_to_apply_result(result));
        }
        let hash_recognized = self
            .snapshots
            .lock()
            .unwrap()
            .by_hash(canonical_path, expected.unwrap())
            .is_some();
        Err(Box::new(self.mismatch_error(
            section,
            canonical_path,
            normalized,
            expected.unwrap(),
            hash_recognized,
        )))
    }

    fn mismatch_error(
        &mut self,
        section: &PatchSection,
        canonical_path: &str,
        normalized: &str,
        expected: &str,
        hash_recognized: bool,
    ) -> MismatchError {
        let actual_file_hash = self.record_full_snapshot(canonical_path, normalized);
        MismatchError::new(MismatchDetails {
            path: Some(section.path.clone()),
            expected_file_hash: expected.to_string(),
            actual_file_hash,
            file_lines: normalized.split('\n').map(|s| s.to_string()).collect(),
            anchor_lines: section.collect_anchor_lines(),
            hash_recognized,
        })
    }
}

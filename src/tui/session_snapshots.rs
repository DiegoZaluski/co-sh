//! Rebuildable, append-only view snapshots in the authoritative JSONL.
//!
//! A commit points at a tree of immutable JSON pages. Unchanged pages are
//! referenced by offset, length and checksum, including across snapshots.
//! Publication is the final newline-terminated commit, after all of its pages.
//! An incomplete or invalid snapshot is ignored in favour of event replay.
//! No sidecar, rewritten header, or replacement transcript is required.

use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use lru::LruCache;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xxhash_rust::xxh64::xxh64;

use crate::session_history::{HistoryProjection, parse_jsonl};

const FORMAT: u32 = 1;
const LEAF_BYTES: usize = 16 * 1024;
const FANOUT: usize = 64;
const MIN_CHECKPOINT_BYTES: u64 = 256 * 1024;
const COMMIT_SEARCH_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct PageRef {
    offset: u64,
    length: u64,
    checksum: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Page {
    Leaf(Value),
    Array(Vec<PageRef>),
    Object(Vec<(String, PageRef)>),
    /// Bounded-fanout directory pages, not a chain of previous snapshots.
    Join {
        object: bool,
        parts: Vec<PageRef>,
    },
}

#[derive(Serialize, Deserialize)]
struct PageRecord {
    view_page: Page,
}

#[derive(Serialize, Deserialize)]
struct Commit {
    version: u32,
    covered_bytes: u64,
    last_event_id: u64,
    root: PageRef,
    /// Amortize snapshot construction against new event bytes. Large views
    /// are not reserialized after a fixed small number of tiny deltas.
    checkpoint_weight: u64,
}

#[derive(Serialize, Deserialize)]
struct CommitRecord {
    view_commit: Commit,
}

type Catalog = HashMap<(u64, u64), Vec<PageRef>>;

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    /// File identity, when the platform exposes one. A replacement file at
    /// the same path must never extend the previous file's cached view.
    identity: Option<(u64, u64)>,
}

impl Stamp {
    fn of(metadata: &Metadata, _file: &File) -> Self {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(windows)]
        let identity = windows_file_identity(_file);
        #[cfg(not(any(unix, windows)))]
        let identity: Option<(u64, u64)> = None;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            identity,
        }
    }

    fn appended_to(&self, previous: &Self) -> bool {
        self.length > previous.length
            && match (self.identity, previous.identity) {
                (Some(current), Some(before)) => current == before,
                // Without a platform identity (e.g. a network drive that
                // reports none) fall back to the append-only size check.
                _ => true,
            }
    }
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Option<(u64, u64)> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `file` owns a valid Windows file handle for the duration of this
    // call, and `information` points to writable storage of the required type.
    let success =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if success == 0 {
        return None;
    }
    // SAFETY: A successful call initializes the output structure.
    let information = unsafe { information.assume_init() };
    let volume = u64::from(information.dwVolumeSerialNumber);
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Some((volume, index))
}

struct Cached {
    stamp: Stamp,
    projection: HistoryProjection,
    catalog: Catalog,
    checkpoint_end: u64,
    checkpoint_weight: u64,
    /// Only the final byte is needed by the append writer, not the full log.
    tail: String,
}

fn cache() -> &'static Mutex<LruCache<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<LruCache<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(4).unwrap())))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Readers never append. The returned string is just an append delimiter
/// sentinel; callers must not use it as a copy of the history.
pub(crate) fn load(path: &Path) -> io::Result<(String, HistoryProjection)> {
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    let cached = load_cached(path, cache.pop(path))?;
    let result = (cached.tail.clone(), cached.projection.clone());
    cache.put(path.to_path_buf(), cached);
    Ok(result)
}

fn load_cached(path: &Path, previous: Option<Cached>) -> io::Result<Cached> {
    let mut file = File::open(path)?;
    let stamp = Stamp::of(&file.metadata()?, &file);
    if let Some(mut previous) = previous {
        if stamp == previous.stamp {
            return Ok(previous);
        }
        // Only complete-line cached tails can be extended safely. A partial
        // last record may have been completed by another writer since reading.
        if stamp.appended_to(&previous.stamp) && previous.tail == "\n" {
            file.seek(SeekFrom::Start(previous.stamp.length))?;
            let mut suffix = String::new();
            file.read_to_string(&mut suffix)?;
            let parsed = parse_jsonl("", &suffix);
            let mut projection = previous.projection.clone();
            if parsed.legacy.is_none() && projection.extend(&parsed.events).is_ok() {
                previous.projection = projection;
                previous.tail = tail(&suffix);
                previous.stamp = stamp;
                return Ok(previous);
            }
        }
    }
    if let Some(mut restored) = restore_snapshot(&mut file, &stamp)? {
        file.seek(SeekFrom::Start(restored.checkpoint_end))?;
        let mut suffix = String::new();
        file.read_to_string(&mut suffix)?;
        let parsed = parse_jsonl("", &suffix);
        if restored.projection.extend(&parsed.events).is_ok() {
            restored.tail = if suffix.is_empty() {
                "\n".into()
            } else {
                tail(&suffix)
            };
            return Ok(restored);
        }
        // A legacy reference may address an event not retained by the snapshot.
        // Full replay can resolve it; do not substitute the nearest view.
    }
    file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let root_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("session-"))
        .ok_or_else(|| invalid("invalid session filename"))?;
    let parsed = parse_jsonl(root_id, &contents);
    if !parsed.corrupt_lines.is_empty() {
        log::warn!(
            "session {} has corrupt lines {:?}",
            path.display(),
            parsed.corrupt_lines
        );
    }
    let projection = HistoryProjection::replay(parsed.legacy, &parsed.events)
        .map_err(|e| invalid(format!("history replay failed: {e:?}")))?;
    Ok(Cached {
        stamp,
        projection,
        catalog: Catalog::new(),
        checkpoint_end: 0,
        checkpoint_weight: 0,
        tail: tail(&contents),
    })
}

fn tail(contents: &str) -> String {
    if contents.is_empty() {
        String::new()
    } else if contents.ends_with('\n') {
        "\n".into()
    } else {
        "x".into()
    }
}

/// Called only by the store's serialized, cross-process locked append writer.
pub(crate) fn checkpoint(path: &Path) -> io::Result<()> {
    checkpoint_impl(path, false)
}

fn checkpoint_impl(path: &Path, force: bool) -> io::Result<()> {
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    let mut state = load_cached(path, cache.pop(path))?;
    let threshold = MIN_CHECKPOINT_BYTES.max(state.checkpoint_weight / 4);
    if !force && state.stamp.length.saturating_sub(state.checkpoint_end) < threshold {
        cache.put(path.to_path_buf(), state);
        return Ok(());
    }
    let covered_bytes = state.stamp.length;
    let value = serde_json::to_value(&state.projection).map_err(io::Error::other)?;
    let weight = serde_json::to_vec(&value).map_err(io::Error::other)?.len() as u64;
    let mut file = OpenOptions::new().read(true).append(true).open(path)?;
    if state.tail != "\n" && covered_bytes > 0 {
        file.write_all(b"\n")?;
    }
    let root = write_value(&mut file, &mut state.catalog, value)?;
    // Data pages must precede publication even under an OS crash. Readers
    // also check every referenced page, so partial publication is harmless.
    file.sync_data()?;
    let commit = CommitRecord {
        view_commit: Commit {
            version: FORMAT,
            covered_bytes,
            last_event_id: state.projection.last_event_id,
            root,
            checkpoint_weight: weight,
        },
    };
    let mut bytes = serde_json::to_vec(&commit).map_err(io::Error::other)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()?;
    state.stamp = Stamp::of(&file.metadata()?, &file);
    state.checkpoint_end = state.stamp.length;
    state.checkpoint_weight = weight;
    state.tail = "\n".into();
    cache.put(path.to_path_buf(), state);
    Ok(())
}

fn write_value(file: &mut File, catalog: &mut Catalog, value: Value) -> io::Result<PageRef> {
    let size = serde_json::to_vec(&value).map_err(io::Error::other)?.len();
    if size <= LEAF_BYTES {
        return write_page(file, catalog, Page::Leaf(value));
    }
    match value {
        Value::Array(values) => {
            let mut parts = Vec::new();
            for chunk in values.chunks(FANOUT) {
                let mut refs = Vec::new();
                for value in chunk {
                    refs.push(write_value(file, catalog, value.clone())?);
                }
                parts.push(write_page(file, catalog, Page::Array(refs))?);
            }
            write_directory(file, catalog, false, parts)
        }
        Value::Object(values) => {
            let entries: Vec<_> = values.into_iter().collect();
            let mut parts = Vec::new();
            for chunk in entries.chunks(FANOUT) {
                let mut refs = Vec::new();
                for (key, value) in chunk {
                    refs.push((key.clone(), write_value(file, catalog, value.clone())?));
                }
                parts.push(write_page(file, catalog, Page::Object(refs))?);
            }
            write_directory(file, catalog, true, parts)
        }
        // Large scalar payloads remain one immutable page. They are shared by
        // offset on later snapshots rather than copied on every save.
        scalar => write_page(file, catalog, Page::Leaf(scalar)),
    }
}

fn write_directory(
    file: &mut File,
    catalog: &mut Catalog,
    object: bool,
    mut parts: Vec<PageRef>,
) -> io::Result<PageRef> {
    while parts.len() > 1 {
        let mut parents = Vec::new();
        for chunk in parts.chunks(FANOUT) {
            parents.push(write_page(
                file,
                catalog,
                Page::Join {
                    object,
                    parts: chunk.to_vec(),
                },
            )?);
        }
        parts = parents;
    }
    parts
        .pop()
        .ok_or_else(|| invalid("empty snapshot directory"))
}

fn write_page(file: &mut File, catalog: &mut Catalog, page: Page) -> io::Result<PageRef> {
    let mut bytes =
        serde_json::to_vec(&PageRecord { view_page: page }).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let checksum = xxh64(&bytes, 0);
    let length = bytes.len() as u64;
    let key = (checksum, length);
    if let Some(candidates) = catalog.get(&key) {
        // Hashes are lookup accelerators, never identity: compare bytes before
        // sharing a page, so even a checksum collision cannot change a view.
        for candidate in candidates {
            if read_bytes(file, *candidate, file.metadata()?.len())? == bytes {
                return Ok(*candidate);
            }
        }
    }
    let offset = file.seek(SeekFrom::End(0))?;
    file.write_all(&bytes)?;
    let reference = PageRef {
        offset,
        length,
        checksum,
    };
    catalog.entry(key).or_default().push(reference);
    Ok(reference)
}

fn read_bytes(file: &mut File, reference: PageRef, before: u64) -> io::Result<Vec<u8>> {
    let end = reference
        .offset
        .checked_add(reference.length)
        .ok_or_else(|| invalid("snapshot offset overflow"))?;
    if reference.length == 0 || end > before {
        return Err(invalid(
            "snapshot reference points outside its committed prefix",
        ));
    }
    let length =
        usize::try_from(reference.length).map_err(|_| invalid("snapshot page too large"))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(io::Error::other)?;
    bytes.resize(length, 0);
    file.seek(SeekFrom::Start(reference.offset))?;
    file.read_exact(&mut bytes)?;
    if !bytes.ends_with(b"\n") || xxh64(&bytes, 0) != reference.checksum {
        return Err(invalid("snapshot page checksum mismatch"));
    }
    Ok(bytes)
}

fn read_value(
    file: &mut File,
    catalog: &mut Catalog,
    reference: PageRef,
    before: u64,
    depth: usize,
) -> io::Result<Value> {
    if depth > 128 {
        return Err(invalid("snapshot directory too deep"));
    }
    let bytes = read_bytes(file, reference, before)?;
    let record: PageRecord = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let refs = catalog
        .entry((reference.checksum, reference.length))
        .or_default();
    if !refs.contains(&reference) {
        refs.push(reference);
    }
    let mut child =
        |reference_child| read_value(file, catalog, reference_child, reference.offset, depth + 1);
    match record.view_page {
        Page::Leaf(value) => Ok(value),
        Page::Array(parts) => Ok(Value::Array(
            parts
                .into_iter()
                .map(&mut child)
                .collect::<io::Result<_>>()?,
        )),
        Page::Object(parts) => {
            let mut values = serde_json::Map::new();
            for (key, part) in parts {
                if values.insert(key, child(part)?).is_some() {
                    return Err(invalid("duplicate snapshot object key"));
                }
            }
            Ok(Value::Object(values))
        }
        Page::Join { object, parts } => {
            if object {
                let mut values = serde_json::Map::new();
                for part in parts {
                    let Value::Object(chunk) = child(part)? else {
                        return Err(invalid("invalid snapshot object directory"));
                    };
                    for (key, value) in chunk {
                        if values.insert(key, value).is_some() {
                            return Err(invalid("duplicate snapshot directory key"));
                        }
                    }
                }
                Ok(Value::Object(values))
            } else {
                let mut values = Vec::new();
                for part in parts {
                    let Value::Array(chunk) = child(part)? else {
                        return Err(invalid("invalid snapshot array directory"));
                    };
                    values.extend(chunk);
                }
                Ok(Value::Array(values))
            }
        }
    }
}

fn restore_snapshot(file: &mut File, stamp: &Stamp) -> io::Result<Option<Cached>> {
    let start = stamp.length.saturating_sub(COMMIT_SEARCH_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    // A final unterminated commit was not published. Work backwards through
    // complete lines, including earlier valid commits after a torn write.
    let mut end = tail.len();
    if tail.last() != Some(&b'\n') {
        end = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    }
    while end > 0 {
        let begin = tail[..end - 1]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        let line = &tail[begin..end];
        if line.starts_with(b"{\"view_commit\":") {
            let locator_start = start + begin as u64;
            let commit_ref = PageRef {
                offset: locator_start,
                length: line.len() as u64,
                checksum: xxh64(line, 0),
            };
            if let Ok(record) = serde_json::from_slice::<CommitRecord>(line) {
                let checkpoint_end = commit_ref.offset + commit_ref.length;
                let commit_start = commit_ref.offset;
                let commit = record.view_commit;
                if commit.version == FORMAT && commit.covered_bytes <= commit_start {
                    let mut catalog = Catalog::new();
                    if let Ok(value) = read_value(file, &mut catalog, commit.root, commit_start, 0)
                        && let Ok(projection) = serde_json::from_value::<HistoryProjection>(value)
                        && projection.last_event_id == commit.last_event_id
                    {
                        return Ok(Some(Cached {
                            stamp: stamp.clone(),
                            projection,
                            catalog,
                            checkpoint_end,
                            checkpoint_weight: commit.checkpoint_weight,
                            tail: "\n".into(),
                        }));
                    }
                }
            }
        }
        end = begin;
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) fn evict(path: &Path) {
    cache().lock().unwrap_or_else(|p| p.into_inner()).pop(path);
}

#[cfg(test)]
pub(crate) fn force_checkpoint(path: &Path) -> io::Result<()> {
    checkpoint_impl(path, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::{
        BranchMetadata, Delta, HistoryEvent, MessageDelta, MetadataDelta, Reference,
    };
    use crate::types::{Message, MessageRole, Part, TextPart};

    fn metadata(id: &str) -> BranchMetadata {
        BranchMetadata {
            session_id: id.into(),
            title: "Original".into(),
            title_generated: false,
            created_at: 1,
            cwd: "/test".into(),
            provider: None,
            model: None,
            reasoning: None,
        }
    }

    fn append(path: &Path, event: HistoryEvent) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        serde_json::to_writer(&mut file, &event).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn fixture(path: &Path, count: u64) {
        append(
            path,
            HistoryEvent::new(
                1,
                "root",
                Delta::Genesis {
                    metadata: metadata("root"),
                },
            ),
        );
        for i in 0..count {
            append(
                path,
                HistoryEvent::new(
                    i + 2,
                    "root",
                    Delta::Message {
                        change: MessageDelta::Upsert {
                            message: Message {
                                id: format!("m{i}"),
                                role: MessageRole::User,
                                parts: vec![Part::Text(TextPart {
                                    text: format!("payload {i} {}", "x".repeat(2_000)),
                                    synthetic: false,
                                })],
                                created_at: 1,
                                agent: None,
                                model: None,
                            },
                            context_item_ids: Vec::new(),
                        },
                    },
                ),
            );
        }
    }

    fn assert_matches_replay(path: &Path, actual: &HistoryProjection) {
        let raw = std::fs::read_to_string(path).unwrap();
        let parsed = parse_jsonl("root", &raw);
        let expected = HistoryProjection::replay(parsed.legacy, &parsed.events).unwrap();
        assert_eq!(actual.last_event_id, expected.last_event_id);
        assert_eq!(
            serde_json::to_value(&actual.branches).unwrap(),
            serde_json::to_value(&expected.branches).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&actual.reverts).unwrap(),
            serde_json::to_value(&expected.reverts).unwrap()
        );
    }

    #[test]
    fn snapshot_pages_share_payload_and_cold_load_matches_full_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 256);
        force_checkpoint(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        append(
            &path,
            HistoryEvent::new(
                258,
                "root",
                Delta::Metadata {
                    change: MetadataDelta::Title {
                        title: "Renamed".into(),
                        title_generated: true,
                    },
                },
            ),
        );
        force_checkpoint(&path).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert!(after.starts_with(&before));
        assert!(
            after.len() - before.len() < LEAF_BYTES * 4,
            "unchanged transcript was copied into the new snapshot"
        );
        let mut file = File::open(&path).unwrap();
        let stamp = Stamp::of(&file.metadata().unwrap(), &file);
        let restored = restore_snapshot(&mut file, &stamp).unwrap().unwrap();
        assert_eq!(
            restored.projection.branches["root"].session.title,
            "Renamed"
        );
        assert_matches_replay(&path, &restored.projection);
        evict(&path);
        assert_matches_replay(&path, &load(&path).unwrap().1);
    }

    #[test]
    fn torn_snapshot_commit_falls_back_to_durable_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 20);
        force_checkpoint(&path).unwrap();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 7).unwrap();
        evict(&path);
        assert_matches_replay(&path, &load(&path).unwrap().1);
    }

    #[test]
    fn invalid_snapshot_page_is_discarded_without_losing_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 20);
        force_checkpoint(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let commit: CommitRecord = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(commit.view_commit.root.offset))
            .unwrap();
        file.write_all(b"!").unwrap();
        evict(&path);
        assert_matches_replay(&path, &load(&path).unwrap().1);
    }

    #[test]
    fn snapshot_tail_can_resolve_an_older_unretained_reference_by_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 20);
        force_checkpoint(&path).unwrap();
        append(
            &path,
            HistoryEvent::new(
                22,
                "child",
                Delta::Fork {
                    parent: Reference::head("root", 5),
                    metadata: metadata("child"),
                },
            ),
        );
        evict(&path);
        let actual = load(&path).unwrap().1;
        assert_eq!(actual.branches["child"].session.messages.len(), 4);
        assert_matches_replay(&path, &actual);
    }

    #[test]
    fn replacing_a_cached_file_does_not_return_its_previous_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 20);
        assert_eq!(
            load(&path).unwrap().1.branches["root"]
                .session
                .messages
                .len(),
            20
        );
        let replacement = dir.path().join("replacement.jsonl");
        fixture(&replacement, 2);
        std::fs::rename(replacement, &path).unwrap();
        assert_eq!(
            load(&path).unwrap().1.branches["root"]
                .session
                .messages
                .len(),
            2
        );
    }

    #[test]
    fn unchanged_snapshot_only_appends_a_new_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-root.jsonl");
        fixture(&path, 128);
        force_checkpoint(&path).unwrap();
        let previous_len = std::fs::metadata(&path).unwrap().len();
        force_checkpoint(&path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() - previous_len < 1_024);
    }
}

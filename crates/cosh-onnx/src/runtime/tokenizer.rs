//! Tokenizers: the encoding surface the sequence builders use, and the
//! HuggingFace fast tokenizer behind it.
//!
//! `fix_tokenizer_config` patches `tokenizer_config.json` so a checkpoint
//! produced by newer Transformers versions loads across tokenizer-library
//! versions — rewritten through a temporary file and an atomic replace,
//! because HuggingFace snapshots are links into a shared blob store and
//! writing through the link would truncate a file shared with other
//! revisions and processes.

use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

/// The tokenizer surface `build_sequence` uses — the HuggingFace fast
/// tokenizer's call shape, restricted to what the sequence builder passes.
///
/// Upstream wraps every call in `_TOKENIZE_LOCK` (`encode_text`) because a
/// shared fast tokenizer is not read-only: `truncation=True` mutates the
/// shared Rust object, so concurrent `predict()` calls raced on one parse.
/// The `tokenizers` crate here encodes through `&self`, so the lock is an
/// implementation detail of the concrete tokenizer wrapper, not of
/// this trait.
/// `Send` is a supertrait so a boxed tokenizer can sit inside an agent that
/// the Router shares across threads (upstream's thread-safety guarantees,
/// #95): `Arc<Mutex<OnnxAgent>>` is `Send + Sync` exactly when the agent is
/// `Send`.
pub trait Tokenizer: Send {
    /// `tok(text, add_special_tokens=False, truncation=..., max_length=...)["input_ids"]`.
    ///
    /// `max_length` is only honoured when `truncation` is set, matching the
    /// upstream call sites (truncation keeps the *first* `max_length` tokens).
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32>;
    fn mask_token(&self) -> &str;
    fn mask_token_id(&self) -> u32;
    fn cls_token_id(&self) -> u32;
    fn sep_token_id(&self) -> u32;
    fn pad_token_id(&self) -> u32;
}

/// The HuggingFace fast tokenizer behind the [`Tokenizer`] trait.
///
/// Upstream serialises every tokenizer call behind `_TOKENIZE_LOCK` because a
/// shared fast tokenizer is not read-only: `truncation=True` mutates the
/// shared Rust object and concurrent `predict()` calls raced on one parse.
/// The same lock wraps the same object here.
pub struct HfTokenizer {
    inner: Mutex<tokenizers::Tokenizer>,
    cls: u32,
    sep: u32,
    mask: u32,
    pad: u32,
    mask_token: String,
}

impl HfTokenizer {
    /// Load from a `tokenizer/` directory (`tokenizer.json` plus
    /// `tokenizer_config.json` for the special-token names), as
    /// `AutoTokenizer.from_pretrained` does.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| Error::Runtime(format!("cosh-onnx: could not load tokenizer: {}", e)))?;
        let cfg: Value = std::fs::read_to_string(dir.join("tokenizer_config.json"))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(json!({}));
        let token_str = |key: &str| -> Option<String> {
            match cfg.get(key) {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Object(m)) => m.get("content").and_then(Value::as_str).map(str::to_string),
                _ => None,
            }
        };
        // Fall back to the standard special-token names when the config does
        // not name them, mirroring AutoTokenizer's defaults.
        let resolve = |key: &str, fallback: &str| -> Result<(String, u32)> {
            let name = token_str(key).unwrap_or_else(|| fallback.to_string());
            let id = tok.token_to_id(&name).ok_or_else(|| {
                Error::Runtime(format!(
                    "cosh-onnx: tokenizer is missing the {} special token {:?}",
                    key, name
                ))
            })?;
            Ok((name, id))
        };
        let (cls_name, cls) = resolve("cls_token", "[CLS]")?;
        let (sep_name, sep) = resolve("sep_token", "[SEP]")?;
        let (mask_name, mask) = resolve("mask_token", "[MASK]")?;
        let (pad_name, pad) = resolve("pad_token", "[PAD]")?;
        let _ = (cls_name, sep_name, pad_name);
        Ok(Self {
            inner: Mutex::new(tok),
            cls,
            sep,
            mask,
            pad,
            mask_token: mask_name,
        })
    }
}

impl Tokenizer for HfTokenizer {
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32> {
        let tok = self.inner.lock().unwrap();
        // `truncation=True, max_length=N` on a single sequence keeps the first
        // N tokens of the full encoding; truncating the produced ids is the
        // same tokens at the same cost, and leaves the shared tokenizer
        // unmutated.
        let encoding = tok.encode(text, false).ok();
        let mut ids = encoding.map(|e| e.get_ids().to_vec()).unwrap_or_default();
        if truncation && let Some(max_length) = max_length {
            ids.truncate(max_length);
        }
        ids
    }

    fn mask_token(&self) -> &str {
        &self.mask_token
    }
    fn mask_token_id(&self) -> u32 {
        self.mask
    }
    fn cls_token_id(&self) -> u32 {
        self.cls
    }
    fn sep_token_id(&self) -> u32 {
        self.sep
    }
    fn pad_token_id(&self) -> u32 {
        self.pad
    }
}


/// `Agent._fix_tokenizer_config`: ensure `tokenizer_config.json` can be loaded
/// across tokenizer-library versions.
///
/// Checkpoints produced by newer Transformers versions can contain a
/// `TokenizersBackend` class or a list-valued `extra_special_tokens` field
/// that older loaders cannot parse. When a change is needed the file is
/// rewritten through a temporary file and an atomic replace — HuggingFace
/// snapshots are links into a shared blob store, so writing through the link
/// would truncate a file shared with other revisions and processes.
pub fn fix_tokenizer_config(path: &std::path::Path) {
    let cfg_file = path.join("tokenizer").join("tokenizer_config.json");
    if !cfg_file.exists() {
        return;
    }
    if let Err(e) = fix_tokenizer_config_inner(&cfg_file) {
        // Do not swallow this silently: if the patch did not apply, the
        // tokenizer may fail later with a confusing error and no hint that
        // the config was the cause.
        log::warn!(
            "could not patch {} ({}); the tokenizer may fail to load with this \
             transformers version.",
            cfg_file.display(),
            e
        );
    }
}

pub(crate) fn fix_tokenizer_config_inner(cfg_file: &std::path::Path) -> std::result::Result<(), String> {
    let raw = std::fs::read_to_string(cfg_file).map_err(|e| e.to_string())?;
    let mut tcfg: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let Value::Object(map) = &mut tcfg else {
        return Err("tokenizer_config.json is not a JSON object".to_string());
    };
    let mut changed = false;
    let class_is_default = match map.get("tokenizer_class") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s == "TokenizersBackend",
        Some(_) => false,
    };
    if class_is_default {
        map.insert("tokenizer_class".into(), Value::String("PreTrainedTokenizerFast".into()));
        map.remove("backend");
        map.remove("is_local");
        changed = true;
    }
    // Checkpoints built on the mmBERT/Gemma tokenizer store
    // extra_special_tokens as a list; the loader expects a mapping and raises
    // "'list' object has no attribute 'keys'", which makes the whole model
    // fail to load.
    if let Some(Value::Array(extra)) = map.get("extra_special_tokens") {
        let mapped: Map<String, Value> = extra
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("extra_{}", i), t.clone()))
            .collect();
        map.insert("extra_special_tokens".into(), Value::Object(mapped));
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    // Write to a temporary file in the same directory and replace the
    // original atomically, so a concurrent load never sees a missing or
    // half-written config. mkstemp creates the file 0600; keep the mode the
    // cache file had so a shared cache stays readable to the same users.
    let cfg_dir = cfg_file.parent().unwrap_or(std::path::Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".tokenizer_config.")
        .suffix(".tmp")
        .tempfile_in(cfg_dir)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    let mut out = tmp.reopen().map_err(|e| e.to_string())?;
    // `json.dump(tcfg, f, indent=2)`; Python adds no trailing newline.
    let body = serde_json::to_string_pretty(&tcfg).map_err(|e| e.to_string())?;
    out.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(cfg_file) {
            let _ = tmp.as_file().set_permissions(std::fs::Permissions::from_mode(
                meta.permissions().mode() & 0o777,
            ));
        }
    }
    // Dropping the PersistError removes the temporary file, leaving the
    // original intact — the cleanup path of the upstream `except` block.
    tmp.persist(cfg_file).map_err(|e| e.error.to_string())?;
    Ok(())
}

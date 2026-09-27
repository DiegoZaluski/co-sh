//! Token sequence construction for the question protocol: format
//! `[CLS] <type> instructions [SEP] [MASK] opt0 [MASK] opt1 ... [SEP] state [SEP]`.

use serde_json::Value;

use crate::decision::question::{render_options, serialize_state};
use crate::error::{Error, Result};
use crate::pycompat::py_str;
use crate::runtime::tokenizer::Tokenizer;

/// Options for [`build_sequence`], mirroring the upstream keyword arguments.
#[derive(Debug, Clone)]
pub struct BuildOptions<'a> {
    pub max_len: usize,
    pub head_max_len: usize,
    /// Render option texts in this label-index order instead of natural order.
    pub option_order: Option<&'a [usize]>,
    /// Truncate the state from the left (keep the tail) instead of the head.
    pub truncate_left: bool,
    /// Pre-tokenized state, shared across every question of one call.
    pub state_ids: Option<&'a [u32]>,
}
/// `build_sequence`: format
/// `[CLS] <type> instructions [SEP] [MASK] opt0 [MASK] opt1 ... [SEP] state [SEP]`.
///
/// `state_ids` lets a caller tokenize the shared state once and reuse it
/// across every question, instead of re-serializing and re-tokenizing the
/// same document per question.
pub fn build_sequence(
    tok: &dyn Tokenizer,
    state: &Value,
    q: &Value,
    opts: &BuildOptions<'_>,
) -> Result<(Vec<u32>, Vec<usize>)> {
    let rendered = render_options(q)?;
    build_sequence_with_options(tok, state, q, opts, &rendered)
}

/// The [`build_sequence`] core for callers that already rendered the
/// question's options — the agent path renders once and reuses both the
/// sequence and the option count, instead of re-rendering per use.
pub(crate) fn build_sequence_with_options(
    tok: &dyn Tokenizer,
    state: &Value,
    q: &Value,
    opts: &BuildOptions<'_>,
    rendered: &[String],
) -> Result<(Vec<u32>, Vec<usize>)> {
    let mask_tok = tok.mask_token();
    let order: Vec<usize> = match opts.option_order {
        Some(order) => order.to_vec(),
        None => (0..rendered.len()).collect(),
    };
    // `str(q["ins"])` upstream; the internal shape always carries a string, but
    // the conversion is applied unconditionally, so mirror it.
    let ins = py_str(&q["ins"]).replace(mask_tok, " ");
    let head_text = format!("{} question: {}", q["t"].as_str().unwrap_or_default(), ins);
    let mut head_ids = tok.encode(&head_text, false, None);
    let mut opt_ids: Vec<Vec<u32>> = Vec::with_capacity(order.len());
    for &i in &order {
        // Python raises a catchable IndexError for an out-of-range order
        // entry; mirror that as the same text instead of panicking.
        let Some(text) = rendered.get(i) else {
            return Err(Error::Value("list index out of range".to_string()));
        };
        // Cap at the tokenizer, not after the fact: `[:48]` still makes the
        // tokenizer process the whole (possibly long) description.
        // truncation=True, max_length=48 keeps the first 48 tokens.
        let opt_text = format!(" {}", text.replace(mask_tok, " "));
        let tokens = tok.encode(&opt_text, true, Some(48));
        let mut one = vec![tok.mask_token_id()];
        one.extend(tokens);
        opt_ids.push(one);
    }
    let mut opt_budget = opts.head_max_len as isize - total_len(&opt_ids) as isize;
    if opt_budget < 16 {
        let per = 4.max((opts.head_max_len as isize - 16) / order.len().max(1) as isize);
        opt_ids = opt_ids
            .into_iter()
            .map(|o| o[..(per.max(0) as usize).min(o.len())].to_vec())
            .collect();
        opt_budget = opts.head_max_len as isize - total_len(&opt_ids) as isize;
    }
    head_ids.truncate(8.max(opt_budget) as usize);
    let mut ids = vec![tok.cls_token_id()];
    ids.extend(head_ids);
    ids.push(tok.sep_token_id());
    let mut markers = Vec::with_capacity(opt_ids.len());
    for o in &opt_ids {
        markers.push(ids.len());
        ids.extend_from_slice(o);
    }
    ids.push(tok.sep_token_id());
    let room = opts.max_len.saturating_sub(ids.len() + 1);
    let state_ids: Vec<u32> = match opts.state_ids {
        Some(ids) => ids.to_vec(),
        None => {
            let text = serialize_state(state).replace(mask_tok, " ");
            tok.encode(&text, false, None)
        }
    };
    // not state_ids[-room:]: with no room left, state_ids[-0:] is the whole
    // state rather than none of it.
    let st: &[u32] = if opts.truncate_left {
        let skip = state_ids.len().saturating_sub(room);
        &state_ids[skip..]
    } else {
        &state_ids[..room.min(state_ids.len())]
    };
    ids.extend_from_slice(st);
    ids.push(tok.sep_token_id());
    ids.truncate(opts.max_len);
    let markers: Vec<usize> = markers.into_iter().filter(|m| *m < opts.max_len).collect();
    Ok((ids, markers))
}
fn total_len(opt_ids: &[Vec<u32>]) -> usize {
    opt_ids.iter().map(|o| o.len()).sum()
}

use crate::extract_action::jsonish::Value;
use crate::extract_action::jsonish::value::CompletionState;

#[derive(Debug)]
pub enum JsonCollection {
    // Key, Value
    Object(Vec<String>, Vec<Value>, CompletionState),
    Array(Vec<Value>, CompletionState),
    QuotedString(String, CompletionState),
    TripleQuotedString(String, CompletionState),
    SingleQuotedString(String, CompletionState),
    // edge cases that need handling:
    // - triple backticks in a triple backtick string
    // - will the LLM terminate a triple backtick with a single backtick? probably not
    // - do we give the language specifier out? no
    // - what if the triple backtick block contains both a lang and path specifier? e.g. ```tsx path/to/file.tsx
    //   should we hand back the path?
    // - do we dedent the output?
    // - is it an acceptable heuristic to discard the first line of a triple backtick block?
    TripleBacktickString {
        lang: Option<(String, CompletionState)>,
        path: Option<(String, CompletionState)>,
        content: (String, CompletionState),
    },
    BacktickString(String, CompletionState),
    // Handles numbers, booleans, null, and unquoted strings
    UnquotedString(String, CompletionState),
    // Starting with // or #
    TrailingComment(String, CompletionState),
    // Content between /* and */
    BlockComment(String, CompletionState),
}

impl JsonCollection {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Object(_, _, _) => "Object",
            Self::Array(_, _) => "Array",
            Self::QuotedString(_, _)
            | Self::SingleQuotedString(_, _)
            | Self::BacktickString(_, _) => "String",
            Self::TripleBacktickString { .. } => "TripleBacktickString",
            Self::TripleQuotedString(_, _) => "TripleQuotedString",
            Self::UnquotedString(_, _) => "UnquotedString",
            Self::TrailingComment(_, _) | Self::BlockComment(_, _) => "Comment",
        }
    }

    pub const fn completion_state(&self) -> &CompletionState {
        match self {
            Self::Object(_, _, s)
            | Self::Array(_, s)
            | Self::QuotedString(_, s)
            | Self::SingleQuotedString(_, s)
            | Self::BacktickString(_, s)
            | Self::TripleQuotedString(_, s)
            | Self::UnquotedString(_, s)
            | Self::TrailingComment(_, s)
            | Self::BlockComment(_, s) => s,
            Self::TripleBacktickString { content, .. } => &content.1, // TODO: correct?
        }
    }
}

impl From<JsonCollection> for Option<Value> {
    fn from(collection: JsonCollection) -> Self {
        Some(match collection {
            JsonCollection::TrailingComment(_, _) | JsonCollection::BlockComment(_, _) => {
                return None;
            }
            JsonCollection::Object(keys, values, object_completion) => {
                // log::debug!("keys: {:?}", keys);
                let mut object: Vec<_> = Vec::new();
                for (key, value) in keys.into_iter().zip(values) {
                    object.push((key, value));
                }
                Value::Object(object, object_completion)
            }
            JsonCollection::Array(values, completion_state) => {
                Value::Array(values, completion_state)
            }
            JsonCollection::QuotedString(s, completion_state) => Value::String(s, completion_state),
            JsonCollection::TripleQuotedString(s, completion_state) => {
                Value::String(dedent(s.as_str()).content, completion_state)
            }
            JsonCollection::SingleQuotedString(s, completion_state)
            | JsonCollection::BacktickString(s, completion_state) => {
                Value::String(s, completion_state)
            }
            JsonCollection::TripleBacktickString { content, .. } => {
                let Some((_fenced_codeblock_info, codeblock_contents)) = content.0.split_once('\n')
                else {
                    return Some(Value::String(content.0, content.1));
                };

                Value::String(dedent(codeblock_contents).content, content.1)
            }
            JsonCollection::UnquotedString(s, completion_state) => {
                let s = s.trim();
                if s == "true" {
                    Value::Boolean(true)
                } else if s == "false" {
                    Value::Boolean(false)
                } else if s == "null" {
                    Value::Null
                } else if let Ok(n) = s.parse::<i64>() {
                    Value::Number(n.into(), completion_state)
                } else if let Ok(n) = s.parse::<u64>() {
                    Value::Number(n.into(), completion_state)
                } else if let Ok(n) = s.parse::<f64>() {
                    serde_json::Number::from_f64(n).map_or_else(
                        || Value::String(s.into(), completion_state),
                        |n| Value::Number(n, completion_state),
                    )
                } else {
                    Value::String(s.into(), completion_state)
                }
            }
        })
    }
}

struct DedentResult {
    content: String,
}

fn dedent(s: &str) -> DedentResult {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 1 {
        return DedentResult {
            content: s.to_string(),
        };
    }
    let min_indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let content = if min_indent > 0 {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i == 0 {
                    (*l).to_string()
                } else if l.len() >= min_indent {
                    l[min_indent..].to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        s.to_string()
    };
    DedentResult { content }
}

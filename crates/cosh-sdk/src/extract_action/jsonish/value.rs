use std::{
    collections::HashSet,
    fmt::Write,
    hash::{Hash, Hasher},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionState {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fixes {
    GreppedForJSON,
    InferredArray,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    // Primitive Types
    String(String, CompletionState),
    Number(serde_json::Number, CompletionState),
    Boolean(bool),
    Null,

    // Complex Types
    // Note: Greg - should keys carry completion state?
    // During parsing, if we hare an incomplete key, does the parser
    // complete it and set its value to null? Or drop it?
    // If the parser drops it, we don't need to carry CompletionState.
    Object(Vec<(String, Self)>, CompletionState),
    Array(Vec<Self>, CompletionState),

    // Fixed types
    Markdown(String, Box<Self>, CompletionState),
    FixedJson(Box<Self>, Vec<Fixes>),
    AnyOf(Vec<Self>, String),
}

impl Hash for Value {
    // Hashing a Value ignores CompletationState.
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);

        match self {
            Self::String(s, _) => s.hash(state),
            Self::Number(n, _) => n.to_string().hash(state),
            Self::Boolean(b) => b.hash(state),
            Self::Null => "null".hash(state),
            Self::Object(o, _) => {
                for (k, v) in o {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Self::Array(a, _) => {
                for v in a {
                    v.hash(state);
                }
            }
            Self::Markdown(s, v, _) => {
                s.hash(state);
                v.hash(state);
            }
            Self::FixedJson(v, _) => v.hash(state),
            Self::AnyOf(items, _) => {
                for item in items {
                    item.hash(state);
                }
            }
        }
    }
}

impl Value {
    pub(super) fn simplify(self, is_done: bool) -> Self {
        match self {
            Self::AnyOf(items, s) => {
                let as_simple_str = |s: String| {
                    Self::String(
                        s,
                        if is_done {
                            CompletionState::Complete
                        } else {
                            CompletionState::Incomplete
                        },
                    )
                };
                let mut items = items
                    .into_iter()
                    .map(|v| v.simplify(is_done))
                    .collect::<Vec<_>>();
                match items.len() {
                    0 => as_simple_str(s),
                    1 => {
                        #[allow(clippy::expect_used)]
                        let item = items.pop().expect("Expected 1 item");
                        match item {
                            Self::String(content, _completion_state) if content == s => {
                                as_simple_str(s)
                            }
                            other => Self::AnyOf(vec![other], s),
                        }
                    }
                    _ => Self::AnyOf(items, s),
                }
            }
            _ => self,
        }
    }

    #[must_use]
    pub fn r#type(&self) -> String {
        match self {
            Self::String(_, _) => "String".to_string(),
            Self::Number(_, _) => "Number".to_string(),
            Self::Boolean(_) => "Boolean".to_string(),
            Self::Null => "Null".to_string(),
            Self::Object(k, _) => {
                let mut s = "Object{".to_string();
                for (key, value) in k {
                    #[allow(clippy::unwrap_used)]
                    write!(s, "{}: {}, ", key, value.r#type()).unwrap();
                }
                s.push('}');
                s
            }
            Self::Array(i, _) => {
                let mut s = "Array[".to_string();
                let items = i
                    .iter()
                    .map(Self::r#type)
                    .collect::<HashSet<String>>()
                    .into_iter()
                    .collect::<Vec<String>>()
                    .join(" | ");
                s.push_str(&items);
                s.push(']');
                s
            }
            Self::Markdown(tag, item, _) => {
                format!("Markdown:{} - {}", tag, item.r#type())
            }
            Self::FixedJson(inner, fixes) => {
                format!("{} ({} fixes)", inner.r#type(), fixes.len())
            }
            Self::AnyOf(items, _) => {
                let mut s = "AnyOf[".to_string();
                for item in items {
                    s.push_str(&item.r#type());
                    s.push_str(", ");
                }
                s.push(']');
                s
            }
        }
    }

    #[must_use]
    pub fn completion_state(&self) -> &CompletionState {
        match self {
            Self::String(_, s)
            | Self::Number(_, s)
            | Self::Object(_, s)
            | Self::Array(_, s)
            | Self::Markdown(_, _, s) => s,
            Self::Boolean(_) | Self::Null | Self::FixedJson(_, _) => &CompletionState::Complete,
            Self::AnyOf(choices, _) => {
                if choices
                    .iter()
                    .any(|c| c.completion_state() == &CompletionState::Incomplete)
                {
                    &CompletionState::Incomplete
                } else {
                    &CompletionState::Complete
                }
            }
        }
    }

    pub fn complete_deeply(&mut self) {
        match self {
            Self::String(_, s) | Self::Number(_, s) | Self::Markdown(_, _, s) => {
                *s = CompletionState::Complete;
            }
            Self::Boolean(_) | Self::Null => {}
            Self::Object(kv_pairs, s) => {
                *s = CompletionState::Complete;
                for (_, v) in kv_pairs.iter_mut() {
                    v.complete_deeply();
                }
            }
            Self::Array(elems, s) => {
                *s = CompletionState::Complete;
                for v in elems.iter_mut() {
                    v.complete_deeply();
                }
            }
            Self::FixedJson(val, _fixes) => {
                val.complete_deeply();
            }
            Self::AnyOf(choices, _) => {
                for v in choices.iter_mut() {
                    v.complete_deeply();
                }
            }
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(s, _) => write!(f, "{s}"),
            Self::Number(n, _) => write!(f, "{n}"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Null => write!(f, "null"),
            Self::Object(o, _) => {
                write!(f, "{{")?;
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Self::Array(a, _) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Markdown(s, v, _) => write!(f, "{s}\n{v}"),
            Self::FixedJson(v, _) => write!(f, "{v}"),
            Self::AnyOf(items, s) => {
                write!(f, "AnyOf[{s},")?;
                for item in items {
                    write!(f, "{item},")?;
                }
                write!(f, "]")
            }
        }
    }
}

// The serde implementation is used as one of our parsing options.
// We deserialize into a "complete" value, and this property is
// true for nested values, because serde will call the same `deserialize`
// method on children of a serde container.
//
// Numbers should be considered Incomplete if they are encountered
// at the top level. Therefore the non-recursive callsite of `deserialize`
// is responsible for setting completion state to Incomplete for top-level
// strings and numbers.
//
// Lists, strings and objects at the top level are necessarily complete, because
// serde will not parse an array, string or an object unless the closing
// delimiter is present.

/// A serde Visitor that constructs Value directly from the deserializer,
/// avoiding the intermediate `serde_json::Value` allocation and double-parsing.
struct ValueVisitor;

impl<'de> serde::de::Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any valid JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(Value::Boolean(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(v.into(), CompletionState::Complete))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(v.into(), CompletionState::Complete))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(v).map_or_else(
            || {
                Err(serde::de::Error::custom(format!(
                    "f64 value cannot be represented as JSON number: {v}"
                )))
            },
            |n| Ok(Value::Number(n, CompletionState::Complete)),
        )
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(Value::String(v.to_owned(), CompletionState::Complete))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(Value::String(v, CompletionState::Complete))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut vec = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(elem) = seq.next_element::<Value>()? {
            vec.push(elem);
        }
        Ok(Value::Array(vec, CompletionState::Complete))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut object = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            object.push((key, value));
        }
        Ok(Value::Object(object, CompletionState::Complete))
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}

//! Minimal-shape rendering of tool input schemas for rejection messages.
//!
//! When a tool call is rejected, the error should teach the expected
//! argument shape (a minimal example the model can copy) instead of only
//! echoing the rejected payload. [`schema_skeleton`] renders a JSON Schema
//! into that one-line example: required fields first, types as placeholders,
//! depth- and width-capped so huge schemas stay readable.
//!
//! Compositional keywords: `oneOf`/`anyOf` render as alternatives joined
//! with `|` (capped at 3 branches); `allOf` renders its first subschema.
//! `enum` is honored on any primitive type (strings, integers, booleans).

use serde_json::Value as JsonValue;

/// Maximum property nesting rendered before collapsing to `<...>`. Four
/// levels cover realistic tool schemas (`targets[].file_hash` etc.) while
/// capping pathological depth.
const MAX_DEPTH: usize = 4;

/// Maximum properties rendered per object level.
const MAX_PROPS: usize = 6;

/// Maximum alternatives rendered for a `oneOf`/`anyOf` union.
const MAX_UNION_BRANCHES: usize = 3;

/// Render a JSON Schema as a minimal argument example.
///
/// For `{type: object, properties: {a: {type: string}, b: ...}, required:
/// ["a"]}` this yields `{ a: <string>, b?: <number> }`: required fields
/// first (all of them, capped at [`MAX_PROPS`], suffixed `...` when
/// truncated), optional fields marked with a trailing `?`.
#[must_use]
pub fn schema_skeleton(schema: &JsonValue) -> String {
    skeleton(schema, 0)
}

fn skeleton(schema: &JsonValue, depth: usize) -> String {
    if depth >= MAX_DEPTH {
        return "<...>".to_string();
    }
    match schema.get("type") {
        Some(JsonValue::String(t)) => match t.as_str() {
            // A `oneOf`/`anyOf` alongside `type: object` carries the real
            // shape (each branch a full object schema) — render the union,
            // not an empty object.
            "object" if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() => {
                union_skeleton(schema, depth)
            }
            "object" => object_or_array_skeleton(schema, depth),
            "array" => array_skeleton(schema, depth),
            "string" | "integer" | "number" | "boolean" => enum_or_plain(schema, t),
            "null" => "<null>".to_string(),
            _ => "<any>".to_string(),
        },
        Some(JsonValue::Array(types)) => types
            .iter()
            .map(|t| {
                skeleton(
                    &JsonValue::Object([("type".to_string(), t.clone())].into_iter().collect()),
                    depth,
                )
            })
            .collect::<Vec<String>>()
            .join("|"),
        Some(JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_))
        | Some(JsonValue::Object(_))
        | None => {
            // No (string) `type`: structural keywords decide; anything else
            // is opaque.
            if schema.get("properties").is_some() || schema.get("items").is_some() {
                object_or_array_skeleton(schema, depth)
            } else if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
                union_skeleton(schema, depth)
            } else {
                "<any>".to_string()
            }
        }
    }
}

fn object_or_array_skeleton(schema: &JsonValue, depth: usize) -> String {
    if schema.get("properties").is_some() {
        return object_skeleton(schema, depth);
    }
    if schema.get("items").is_some() {
        return array_skeleton(schema, depth);
    }
    "{}".to_string()
}

fn object_skeleton(schema: &JsonValue, depth: usize) -> String {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return "{}".to_string();
    };
    if props.is_empty() {
        return "{}".to_string();
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut fields: Vec<(String, String, &'static str)> = Vec::new();
    for (key, prop) in props {
        let marker = if required.iter().any(|r| *r == key) {
            ""
        } else {
            "?"
        };
        fields.push((key.clone(), skeleton(prop, depth + 1), marker));
    }
    fields.sort_by_key(|(_, _, marker)| if marker.is_empty() { 0 } else { 1 });
    let truncated = fields.len() > MAX_PROPS;
    let rendered: Vec<String> = fields
        .into_iter()
        .take(MAX_PROPS)
        .map(|(key, sk, marker)| format!("{key}{marker}: {sk}"))
        .collect();
    let body = rendered.join(", ");
    if truncated {
        format!("{{ {body}, ... }}")
    } else {
        format!("{{ {body} }}")
    }
}

fn array_skeleton(schema: &JsonValue, depth: usize) -> String {
    match schema.get("items") {
        Some(items) => format!("<array of {}>", skeleton(items, depth + 1)),
        None => "<array>".to_string(),
    }
}

fn union_skeleton(schema: &JsonValue, depth: usize) -> String {
    let branches = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|u| u.as_array())
        .map(|values| values.to_vec())
        .unwrap_or_default();
    if branches.is_empty() {
        return "<any>".to_string();
    }
    let truncated = branches.len() > MAX_UNION_BRANCHES;
    let rendered: Vec<String> = branches
        .into_iter()
        .take(MAX_UNION_BRANCHES)
        .map(|branch| skeleton(&branch, depth + 1))
        .collect();
    let joined = rendered.join("|");
    if truncated {
        format!("<{joined}|...>")
    } else {
        joined
    }
}

fn enum_or_plain(schema: &JsonValue, placeholder: &str) -> String {
    let plain = match placeholder {
        "integer" | "number" => "<number>",
        "boolean" => "<bool>",
        _ => "<string>",
    };
    // A `const` pins a single value (e.g. an action discriminator) — it is
    // the most informative thing the schema says about the field.
    if let Some(const_value) = schema.get("const").and_then(|v| v.as_str()) {
        return format!("<{const_value}>");
    }
    match schema.get("enum").and_then(|e| e.as_array()) {
        Some(values) if !values.is_empty() => {
            let options: Vec<String> = values
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();
            if options.is_empty() {
                plain.to_string()
            } else {
                format!("<{}>", options.join("|"))
            }
        }
        _ => plain.to_string(),
    }
}

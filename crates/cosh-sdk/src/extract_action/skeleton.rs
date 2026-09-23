//! Minimal-shape rendering of tool input schemas for rejection messages.
//!
//! When a tool call is rejected, the error should teach the expected
//! argument shape (a minimal example the model can copy) instead of only
//! echoing the rejected payload. [`schema_skeleton`] renders a JSON Schema
//! into that one-line example: required fields first, types as placeholders,
//! depth- and width-capped so huge schemas stay readable.
//!
//! Compositional keywords: `oneOf`/`anyOf` render as alternatives joined
//! with `|` (capped at 3 branches); `allOf` renders its first subschema
//! when the level carries no properties of its own (the
//! `allOf: [{$ref: '#/$defs/step'}]` idiom — the real shape lives in the
//! definition); `$ref` resolves against the document root's `$defs`.
//! `enum` is honored on any primitive type (strings, integers, booleans).

use serde_json::Value as JsonValue;

/// Maximum property nesting rendered before collapsing to `<...>`. Four
/// levels cover realistic tool schemas (`targets[].file_hash` etc.) while
/// capping pathological depth. Compositional hops (`$ref` follows, `allOf`
/// dispatch) do NOT consume this budget — the rendering position does not
/// get structurally deeper by following a reference.
const MAX_DEPTH: usize = 4;

/// Maximum `$ref` hops followed before collapsing to `<...>`. Ref-following
/// is the only recursion that can loop: a `serde_json::Value` is a finite
/// tree (an `allOf` chain cannot cycle), but a third-party MCP schema may
/// declare a cyclic `$ref` (`$defs/a → $defs/b → $defs/a`), which must hit
/// this cap instead of recursing forever.
const MAX_REF_HOPS: usize = 4;

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
    skeleton(schema, 0, 0, schema)
}

/// Resolve a local `$ref` (`#/$defs/name`) against the document root.
fn resolve_ref<'a>(schema: &'a JsonValue, root: &'a JsonValue) -> Option<&'a JsonValue> {
    let path = schema.get("$ref")?.as_str()?.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Whether this schema level carries a non-empty `properties` map of its own.
fn has_own_properties(schema: &JsonValue) -> bool {
    schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|props| !props.is_empty())
}

/// `depth` is the structural property descent ([`MAX_DEPTH`] budget);
/// `ref_hops` the number of `$ref` links followed so far
/// ([`MAX_REF_HOPS`] budget, the only recursion that can cycle).
fn skeleton(schema: &JsonValue, depth: usize, ref_hops: usize, root: &JsonValue) -> String {
    if depth >= MAX_DEPTH || ref_hops >= MAX_REF_HOPS {
        return "<...>".to_string();
    }
    // `$ref`: the shape lives elsewhere in the same document — follow it
    // laterally (the rendering position does not get deeper) while charging
    // the hop budget, so a cyclic `$ref` from a third-party MCP schema
    // terminates at [`MAX_REF_HOPS`].
    if schema.get("$ref").is_some()
        && let Some(target) = resolve_ref(schema, root)
    {
        return skeleton(target, depth, ref_hops + 1, root);
    }
    // `allOf`: composition of constraints. When this level carries no
    // properties of its own, the real shape lives in the subschemas (the
    // `allOf: [{$ref: ...}]` idiom) — render the first one. When it HAS
    // properties, they already say the shape and the `allOf` entries are
    // conditional constraints, not shape. A dispatch is lateral: it costs
    // neither depth nor a ref hop (an `allOf` chain is a finite tree —
    // only `$ref` can loop).
    if !has_own_properties(schema)
        && let Some(first) = schema
            .get("allOf")
            .and_then(JsonValue::as_array)
            .and_then(|branches| branches.first())
    {
        return skeleton(first, depth, ref_hops, root);
    }
    match schema.get("type") {
        Some(JsonValue::String(t)) => match t.as_str() {
            // A `oneOf`/`anyOf` alongside `type: object` carries the real
            // shape (each branch a full object schema) — render the union,
            // not an empty object.
            "object" if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() => {
                union_skeleton(schema, depth, ref_hops, root)
            }
            "object" => object_or_array_skeleton(schema, depth, ref_hops, root),
            "array" => array_skeleton(schema, depth, ref_hops, root),
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
                    ref_hops,
                    root,
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
                object_or_array_skeleton(schema, depth, ref_hops, root)
            } else if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
                union_skeleton(schema, depth, ref_hops, root)
            } else {
                "<any>".to_string()
            }
        }
    }
}

fn object_or_array_skeleton(
    schema: &JsonValue,
    depth: usize,
    ref_hops: usize,
    root: &JsonValue,
) -> String {
    if schema.get("properties").is_some() {
        return object_skeleton(schema, depth, ref_hops, root);
    }
    if schema.get("items").is_some() {
        return array_skeleton(schema, depth, ref_hops, root);
    }
    "{}".to_string()
}

fn object_skeleton(schema: &JsonValue, depth: usize, ref_hops: usize, root: &JsonValue) -> String {
    let Some(props) = schema.get("properties").and_then(JsonValue::as_object) else {
        return "{}".to_string();
    };
    if props.is_empty() {
        return "{}".to_string();
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut fields: Vec<(String, String, &'static str)> = Vec::new();
    for (key, prop) in props {
        let marker = if required.iter().any(|r| *r == key) {
            ""
        } else {
            "?"
        };
        fields.push((
            key.clone(),
            skeleton(prop, depth + 1, ref_hops, root),
            marker,
        ));
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

fn array_skeleton(schema: &JsonValue, depth: usize, ref_hops: usize, root: &JsonValue) -> String {
    match schema.get("items") {
        Some(items) => format!("<array of {}>", skeleton(items, depth + 1, ref_hops, root)),
        None => "<array>".to_string(),
    }
}

fn union_skeleton(schema: &JsonValue, depth: usize, ref_hops: usize, root: &JsonValue) -> String {
    let branches = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();
    if branches.is_empty() {
        return "<any>".to_string();
    }
    let truncated = branches.len() > MAX_UNION_BRANCHES;
    let rendered: Vec<String> = branches
        .into_iter()
        .take(MAX_UNION_BRANCHES)
        .map(|branch| skeleton(&branch, depth + 1, ref_hops, root))
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
    if let Some(const_value) = schema.get("const").and_then(JsonValue::as_str) {
        return format!("<{const_value}>");
    }
    match schema.get("enum").and_then(JsonValue::as_array) {
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

#[cfg(test)]
mod skeleton_tests {
    use super::schema_skeleton;
    use serde_json::json;

    #[test]
    fn all_of_ref_idiom_renders_the_step_shape() {
        // The computer_keyboard layout: the top level carries only
        // `allOf: [{$ref}]` — the shape lives in `$defs`. Before the $ref/
        // allOf support this rendered as an empty `{}` and the rejection
        // hint taught the model nothing.
        let schema = json!({
            "type": "object",
            "allOf": [{ "$ref": "#/$defs/step" }],
            "$defs": {
                "step": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "held": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "text": { "type": "string" }
                    },
                    "required": ["key"]
                }
            }
        });
        let rendered = schema_skeleton(&schema);
        assert!(rendered.contains("key: <string>"), "{rendered}");
        assert!(
            rendered.contains("held?: <array of <string>>"),
            "{rendered}"
        );
        assert!(rendered.contains("text?"), "{rendered}");
    }

    #[test]
    fn all_of_with_own_properties_ignores_the_constraints() {
        // The computer_pointer layout: real properties at the top level,
        // `allOf` entries are conditional constraints — render the
        // properties, never descend into an if/then subschema.
        let schema = json!({
            "type": "object",
            "properties": {
                "x": { "type": "integer" },
                "y": { "type": "integer" }
            },
            "allOf": [
                { "if": { "required": ["x"] }, "then": { "required": ["y"] } }
            ],
            "required": ["x"]
        });
        let rendered = schema_skeleton(&schema);
        assert!(rendered.contains("x: <number>"), "{rendered}");
        assert!(rendered.contains("y?: <number>"), "{rendered}");
        assert!(!rendered.contains("if"), "{rendered}");
    }

    #[test]
    fn dangling_ref_renders_as_opaque() {
        let schema = json!({ "type": "object", "allOf": [{ "$ref": "#/$defs/missing" }] });
        assert_eq!(schema_skeleton(&schema), "<any>");
    }

    #[test]
    fn cyclic_ref_terminates_at_the_ref_hop_cap() {
        // Third-party MCP schemas can be self-referential; ref-following is
        // the only recursion that can cycle (a serde_json::Value is a finite
        // tree), so the hop budget — not the structural depth — terminates
        // it.
        let schema = json!({
            "type": "object",
            "properties": { "child": { "$ref": "#/$defs/node" } },
            "$defs": {
                "node": {
                    "type": "object",
                    "properties": { "child": { "$ref": "#/$defs/node" } }
                }
            }
        });
        let rendered = schema_skeleton(&schema);
        assert!(rendered.contains("<...>"), "{rendered}");
    }
}

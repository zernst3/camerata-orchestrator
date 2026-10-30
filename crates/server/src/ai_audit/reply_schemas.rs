//! The JSON Schemas of the five JSON stages' replies (scan, alternatives, calibration, detail,
//! fix), sent as `output_config.format` on the API backend to a model that accepts it
//! ([`camerata_llm::model_registry::accepts_structured_output`]).
//!
//! # Why
//!
//! A schema makes the reply's shape and field types structurally right: no prose-only reply, no
//! fenced JSON, a number where a number belongs, an id that is one of the batch's. Each stage
//! still parses tolerantly ([`super::extract_json_object`] and the per-stage readers), because
//! the CLI backend, OpenRouter and a model without structured output never see the schema and
//! reply as they always did; the schema path and the tolerant path read the same shape.
//!
//! A schema constrains the SHAPE, never the content: whether a detail keeps the names the
//! statement made, or a fix names only what the evidence shows, is still checked after the
//! reply, by the same checks.
//!
//! # The API's limits these schemas respect
//!
//! Every object is closed (`additionalProperties: false`), no string or number constraints, no
//! `maxItems`, `minItems` only 0 or 1, no recursion, at most 24 optional properties and 16
//! union types per request. `schema_limit_violations` (tests) checks every schema against
//! them.
//!
//! # Cache
//!
//! The API caches a prompt per schema: a change of `output_config.format` invalidates the cached
//! prefix. The scan and the alternatives stage send a cached digest with every request of a
//! run, so each has ONE schema for all of its requests (no id enum that differs per batch). The
//! calibration, detail and fix stages send no cached prefix, so their schemas may name the
//! ids of the batch they belong to.

use serde_json::{json, Value};

/// A string property.
fn string() -> Value {
    json!({ "type": "string" })
}

/// An integer property.
fn integer() -> Value {
    json!({ "type": "integer" })
}

/// A boolean property.
fn boolean() -> Value {
    json!({ "type": "boolean" })
}

/// A string restricted to `values`.
fn one_of(values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values })
}

/// A closed object with the given properties; `required` names the ones that must be present,
/// every other listed property is optional.
fn object(properties: &[(&str, Value)], required: &[&str]) -> Value {
    let mut props = serde_json::Map::new();
    for (name, schema) in properties {
        props.insert((*name).to_string(), schema.clone());
    }
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

/// An array of `items`.
fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

const SEVERITIES: &[&str] = &["critical", "high", "medium", "low"];

/// The categories the calibration prompt lists (a closed set).
pub(crate) const CALIBRATION_CATEGORIES: &[&str] = &[
    "authorization",
    "authentication",
    "secret-exposure",
    "injection",
    "transport-security",
    "rls-policy",
    "resource-exposure",
    "input-validation",
    "error-handling",
    "arch-conformance",
    "testing-style",
    "performance",
];

/// The scan's reply: `findings`, `needs_files` and, in the one pass that may propose rules,
/// `proposed_rules`. ONE schema for every pass of a run, terse or not, advisory or not, so the
/// cached digest prefix is shared (see the module docs). `captures` lists the object tokens the
/// parser accepts (`KNOWN_AI_CAPTURE_TOKENS`) as optional strings, because a closed object
/// cannot be a free map.
pub(crate) fn scan_reply_schema() -> Value {
    let capture_props: Vec<(&str, Value)> = super::KNOWN_AI_CAPTURE_TOKENS
        .iter()
        .map(|token| (*token, string()))
        .collect();
    let finding = object(
        &[
            ("path", string()),
            ("line", integer()),
            ("severity", one_of(SEVERITIES)),
            ("rule", string()),
            ("title", string()),
            ("code", string()),
            ("detail", string()),
            ("captures", object(&capture_props, &[])),
        ],
        &[
            "path", "line", "severity", "rule", "title", "code", "detail",
        ],
    );
    let proposed = object(
        &[
            ("name", string()),
            ("title", string()),
            ("rationale", string()),
            ("severity", one_of(SEVERITIES)),
            ("enforcement", one_of(&["mechanical", "review"])),
        ],
        &["name", "title", "rationale", "severity", "enforcement"],
    );
    object(
        &[
            ("findings", array(finding)),
            ("proposed_rules", array(proposed)),
            ("needs_files", array(string())),
        ],
        &["findings", "needs_files"],
    )
}

/// The alternatives stage's reply: one recommendation per rule. ONE schema for every batch of a
/// run (the rule id is a plain string, not an enum of the batch's ids), so the digest the stage
/// re-sends with every batch is one cached prefix. A rule the reply leaves out is handled as it
/// always was (it keeps its currently selected option).
pub(crate) fn alternatives_reply_schema() -> Value {
    let observed = object(
        &[("option_id", string()), ("sites", integer())],
        &["option_id"],
    );
    let recommendation = object(
        &[
            ("rule_id", string()),
            ("applicable", boolean()),
            ("recommended_option_id", string()),
            ("evidence", string()),
            ("recommendation_reasoning", string()),
            ("sites_total", integer()),
            ("currently_followed", boolean()),
            ("no_convention", boolean()),
            ("observed_options", array(observed)),
        ],
        &["rule_id", "applicable", "recommendation_reasoning"],
    );
    object(
        &[("recommendations", array(recommendation))],
        &["recommendations"],
    )
}

/// The calibration reply for a batch of `n` findings: a verdict per finding, addressed by
/// `index` (an integer from 0 to `n - 1`, so no verdict can point outside the batch).
/// `category` is optional ("omit the field if none clearly applies").
pub(crate) fn calibration_reply_schema(n: usize) -> Value {
    let indices: Vec<usize> = (0..n.max(1)).collect();
    let verdict = object(
        &[
            ("index", json!({ "type": "integer", "enum": indices })),
            ("severity", one_of(SEVERITIES)),
            ("confidence", one_of(&["high", "low"])),
            ("effort", one_of(&["low", "medium", "high"])),
            ("category", one_of(CALIBRATION_CATEGORIES)),
            ("reason", string()),
        ],
        &["index", "severity", "confidence", "effort", "reason"],
    );
    object(&[("verdicts", array(verdict))], &["verdicts"])
}

/// The detail regeneration reply for a batch whose findings carry the opaque `ids`: an entry per
/// finding, its `id` one of the batch's. The finding's path and line are not echoed: Camerata
/// knows them and attaches them.
pub(crate) fn detail_reply_schema(ids: &[String]) -> Value {
    let entry = object(
        &[
            ("id", json!({ "type": "string", "enum": ids })),
            ("detail", string()),
        ],
        &["id", "detail"],
    );
    object(&[("details", array(entry))], &["details"])
}

/// The fix reply for a batch whose findings carry the opaque `ids`: a fix per finding it could
/// ground, addressed by `id` (one of the batch's). An omitted id is allowed: the prompt asks the
/// model to leave out a fix it cannot ground rather than invent one.
pub(crate) fn fix_reply_schema(ids: &[String]) -> Value {
    let entry = object(
        &[
            ("id", json!({ "type": "string", "enum": ids })),
            ("fix", string()),
        ],
        &["id", "fix"],
    );
    object(&[("fixes", array(entry))], &["fixes"])
}

#[cfg(test)]
pub(crate) mod check {
    //! A small validator for the subset of JSON Schema these schemas use, and the API's limits
    //! on that subset. Tests use it to prove that a reply a model could produce under a schema
    //! is the shape the stage's reader expects, and that every schema is one the API accepts.

    use serde_json::Value;

    /// The opaque ids a detail or fix prompt addresses its findings by, in order: the id in the
    /// square brackets that opens each finding's block (`[a1b2c3] rule ...`).
    pub(crate) fn prompt_ids(prompt: &str) -> Vec<String> {
        prompt
            .lines()
            .filter(|l| l.starts_with('[') && l.contains("] rule "))
            .filter_map(|l| l.find(']').map(|end| l[1..end].to_string()))
            .collect()
    }

    /// A scripted fix reply written by batch POSITION (`{"fixes":[{"index":0,"fix":"..."}]}`)
    /// addressed by the ids `prompt` actually carries, as the model is asked to. Scripted stubs
    /// know positions, not the opaque ids Camerata draws per batch. A reply that is not a fix
    /// reply (a verdicts reply keeps its `index`) is returned as it was, and so is one whose
    /// position the prompt does not have (it matches no finding, as a stray id would).
    pub(crate) fn address_by_id(prompt: &str, reply: &str) -> String {
        if !reply.contains("\"fixes\"") {
            return reply.to_string();
        }
        let ids = prompt_ids(prompt);
        let re = regex::Regex::new(r#""index"\s*:\s*(\d+)"#).expect("static regex");
        re.replace_all(reply, |caps: &regex::Captures| {
            let pos: usize = caps[1].parse().unwrap_or(usize::MAX);
            format!(
                r#""id":"{}""#,
                ids.get(pos).map_or("ffffff", String::as_str)
            )
        })
        .into_owned()
    }

    /// Why `value` does not satisfy `schema`, or `None` when it does.
    pub(crate) fn violation(schema: &Value, value: &Value) -> Option<String> {
        match schema["type"].as_str() {
            Some("object") => {
                let obj = value.as_object().ok_or("not an object").ok();
                let Some(obj) = obj else {
                    return Some(format!("{value} is not an object"));
                };
                let props = schema["properties"].as_object();
                for name in schema["required"].as_array().into_iter().flatten() {
                    let name = name.as_str().unwrap_or_default();
                    if !obj.contains_key(name) {
                        return Some(format!("missing required `{name}`"));
                    }
                }
                for (key, v) in obj {
                    let Some(sub) = props.and_then(|p| p.get(key)) else {
                        return Some(format!("unexpected property `{key}`"));
                    };
                    if let Some(why) = violation(sub, v) {
                        return Some(format!("`{key}`: {why}"));
                    }
                }
                None
            }
            Some("array") => {
                let Some(items) = value.as_array() else {
                    return Some(format!("{value} is not an array"));
                };
                if schema["minItems"]
                    .as_u64()
                    .is_some_and(|m| (items.len() as u64) < m)
                {
                    return Some("too few items".to_string());
                }
                items.iter().find_map(|i| violation(&schema["items"], i))
            }
            Some("string") => {
                if !value.is_string() {
                    return Some(format!("{value} is not a string"));
                }
                enum_violation(schema, value)
            }
            Some("integer") => {
                if !(value.is_i64() || value.is_u64()) {
                    return Some(format!("{value} is not an integer"));
                }
                enum_violation(schema, value)
            }
            Some("boolean") => (!value.is_boolean()).then(|| format!("{value} is not a boolean")),
            other => Some(format!("unsupported type {other:?}")),
        }
    }

    fn enum_violation(schema: &Value, value: &Value) -> Option<String> {
        let allowed = schema["enum"].as_array()?;
        (!allowed.contains(value)).then(|| format!("{value} is not one of {allowed:?}"))
    }

    /// Every way `schema` breaks the structured-output limits of the API (see the module docs of
    /// [`super`]): an object not closed, a forbidden keyword, more than 24 optional properties.
    pub(crate) fn limit_violations(schema: &Value) -> Vec<String> {
        let mut out = Vec::new();
        let mut optional = 0usize;
        walk(schema, "$", &mut out, &mut optional);
        if optional > 24 {
            out.push(format!("{optional} optional properties (the limit is 24)"));
        }
        out
    }

    fn walk(schema: &Value, at: &str, out: &mut Vec<String>, optional: &mut usize) {
        let Some(obj) = schema.as_object() else {
            return;
        };
        for banned in [
            "minLength",
            "maxLength",
            "minimum",
            "maximum",
            "multipleOf",
            "maxItems",
            "pattern",
            "$ref",
            "oneOf",
        ] {
            if obj.contains_key(banned) {
                out.push(format!("{at}: `{banned}` is not supported"));
            }
        }
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if min > 1 {
                out.push(format!("{at}: minItems {min} (only 0 or 1)"));
            }
        }
        if obj.get("type").and_then(Value::as_str) == Some("object") {
            if obj.get("additionalProperties") != Some(&Value::Bool(false)) {
                out.push(format!(
                    "{at}: an object must set additionalProperties: false"
                ));
            }
            let required: Vec<&str> = obj
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(props) = obj.get("properties").and_then(Value::as_object) {
                for (name, sub) in props {
                    if !required.contains(&name.as_str()) {
                        *optional += 1;
                    }
                    walk(sub, &format!("{at}.{name}"), out, optional);
                }
            }
        }
        if let Some(items) = obj.get("items") {
            walk(items, &format!("{at}[]"), out, optional);
        }
    }
}

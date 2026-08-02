//! Lenient handling of tool arguments whose JSON types don't match the tool
//! schema: a value may arrive as a JSON string (`"true"`, `"3"`, `"[...]"`)
//! or mistyped number (`1` for a bool) when a client doesn't coerce args
//! against the tool schema.
//!
//! Two mechanisms live here:
//!
//! 1. **Per-field lenient bool deserializers** (`deserialize_lenient_bool`,
//!    `deserialize_lenient_option_bool`) applied via `#[serde(deserialize_with)]`.
//!    Accepted forms (strings case-insensitive, trimmed; `null` is `false`):
//!
//!    | Truthy                                | Falsy                                          |
//!    |---------------------------------------|------------------------------------------------|
//!    | `true`, `"true"`, `"yes"`, `"1"`, `1` | `false`, `"false"`, `"no"`, `"0"`, `0`, `null` |
//!
//! 2. **Schema-driven argument coercion** ([`coerce_args_against_schema`]),
//!    applied once at the dispatch choke point before typed parsing. It
//!    repairs the whole argument object against the tool's JSON Schema —
//!    string-encoded numbers, whole floats for integer fields, and
//!    string-encoded arrays/objects — without requiring every input struct
//!    to annotate every field.

use serde::Deserialize;

const TRUE_LITERALS: [&str; 3] = ["true", "yes", "1"];
const FALSE_LITERALS: [&str; 3] = ["false", "no", "0"];

/// Parse a JSON value into a `bool` per the accepted forms; `None` otherwise.
pub fn lenient_bool_from_json(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::Null => Some(false),
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if TRUE_LITERALS
                .iter()
                .any(|lit| trimmed.eq_ignore_ascii_case(lit))
            {
                Some(true)
            } else if FALSE_LITERALS
                .iter()
                .any(|lit| trimmed.eq_ignore_ascii_case(lit))
            {
                Some(false)
            } else {
                None
            }
        }
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn invalid_bool_message(value: &serde_json::Value) -> String {
    format!(
        "expected a boolean (true/false, \"true\"/\"false\", \"yes\"/\"no\", \"1\"/\"0\", 1/0), got {value}"
    )
}

/// Deserialize a required `bool`; pair with `#[serde(default)]` so an absent key
/// uses the field default.
pub fn deserialize_lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    lenient_bool_from_json(&value)
        .ok_or_else(|| serde::de::Error::custom(invalid_bool_message(&value)))
}

/// Deserialize `Option<bool>`: absent key → `None` (via `#[serde(default)]`),
/// explicit `null` → `Some(false)`.
pub fn deserialize_lenient_option_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    lenient_bool_from_json(&value)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom(invalid_bool_message(&value)))
}

// ---------------------------------------------------------------------------
// Schema-driven argument coercion
// ---------------------------------------------------------------------------

/// Largest whole value exactly representable as `f64` (2^53). Whole floats
/// above this cannot be converted to integers without rounding ambiguity.
const F64_EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// Repair mistyped tool arguments in `params` against the tool's JSON
/// Schema, in place.
///
/// Providers and models sometimes deliver every argument value JSON-encoded
/// as a string (`"limit": "3"`, `"todos": "[{...}]"`) or deliver integers as
/// whole floats (`"limit": 3.0`). Strict `serde` parsing then rejects the
/// call (`invalid type: string "3", expected u8`) even though the intent is
/// unambiguous. This walks the schema's `properties` and applies **lossless**
/// repairs only:
///
/// - string → integer/number, when the schema types include a numeric type
///   but not `string`, and the trimmed string parses cleanly
/// - whole float → integer (`3.0` → `3`), when the schema types include
///   `integer` but not `number`
/// - string → array/object, when the schema types include a composite type
///   but not `string`, and the string parses as JSON of exactly that shape
///
/// Anything ambiguous (schema allows `string`, lossy fraction, unparseable
/// text, unknown property) is left untouched so the normal typed-parse error
/// path still reports it. Nested objects and array items are repaired
/// recursively via the schema's `properties` / `items`.
pub fn coerce_args_against_schema(params: &mut serde_json::Value, schema: &serde_json::Value) {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    for (key, value) in obj.iter_mut() {
        if let Some(prop_schema) = props.get(key) {
            coerce_value(value, prop_schema);
        }
    }
}

/// Primitive JSON Schema types a property schema declares, collected from
/// `type` (string or array form) and one level of `anyOf` / `oneOf`
/// branches (the shapes `schemars` emits for `Option<T>` and unions).
fn collect_schema_types<'s>(schema: &'s serde_json::Value, out: &mut Vec<&'s str>) {
    match schema.get("type") {
        Some(serde_json::Value::String(t)) => out.push(t),
        Some(serde_json::Value::Array(ts)) => {
            out.extend(ts.iter().filter_map(|t| t.as_str()));
        }
        _ => {}
    }
    for branch_key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(branch_key).and_then(|v| v.as_array()) {
            for branch in branches {
                match branch.get("type") {
                    Some(serde_json::Value::String(t)) => out.push(t),
                    Some(serde_json::Value::Array(ts)) => {
                        out.extend(ts.iter().filter_map(|t| t.as_str()));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn coerce_value(value: &mut serde_json::Value, prop_schema: &serde_json::Value) {
    let mut types = Vec::new();
    collect_schema_types(prop_schema, &mut types);
    let allows = |t: &str| types.contains(&t);

    match value {
        serde_json::Value::String(s) => {
            // A string is already valid for this property — never rewrite.
            if allows("string") {
                return;
            }
            let trimmed = s.trim();
            if allows("integer")
                && let Some(n) = parse_lossless_integer(trimmed)
            {
                *value = serde_json::Value::Number(n);
                return;
            }
            if allows("number")
                && let Some(n) = parse_lossless_number(trimmed)
            {
                *value = serde_json::Value::Number(n);
                return;
            }
            if (allows("array") || allows("object"))
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
            {
                let shape_matches = (parsed.is_array() && allows("array"))
                    || (parsed.is_object() && allows("object"));
                if shape_matches {
                    *value = parsed;
                    coerce_value(value, prop_schema);
                }
            }
        }
        serde_json::Value::Number(n) => {
            // Whole float for an integer-typed property: `3.0` → `3`.
            // Skip when the schema also allows `number` — the float is valid.
            if allows("integer")
                && !allows("number")
                && n.as_i64().is_none()
                && n.as_u64().is_none()
                && let Some(f) = n.as_f64()
                && f.is_finite()
                && f.fract() == 0.0
                && f.abs() <= F64_EXACT_INTEGER_LIMIT
            {
                *value = serde_json::Value::Number(serde_json::Number::from(f as i64));
            }
        }
        serde_json::Value::Array(items) => {
            if let Some(item_schema) = prop_schema.get("items") {
                for item in items.iter_mut() {
                    coerce_value(item, item_schema);
                }
            }
        }
        serde_json::Value::Object(_) => {
            coerce_args_against_schema(value, prop_schema);
        }
        _ => {}
    }
}

/// Parse a trimmed string as an integer `Number`, losslessly.
///
/// Accepts plain integer literals (`"300"`, `"-7"`) and whole-float
/// renderings (`"3.0"`) within the exact-`f64` range. Fractions,
/// out-of-range magnitudes, and non-numeric text return `None`.
fn parse_lossless_integer(s: &str) -> Option<serde_json::Number> {
    if s.is_empty() {
        return None;
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(serde_json::Number::from(i));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Some(serde_json::Number::from(u));
    }
    // Whole-float renderings like "3.0".
    let f = s.parse::<f64>().ok()?;
    if f.is_finite() && f.fract() == 0.0 && f.abs() <= F64_EXACT_INTEGER_LIMIT {
        return Some(serde_json::Number::from(f as i64));
    }
    None
}

/// Parse a trimmed string as a JSON `Number` (integer when exact, float
/// otherwise). Non-finite and non-numeric inputs return `None`.
fn parse_lossless_number(s: &str) -> Option<serde_json::Number> {
    if s.is_empty() {
        return None;
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(serde_json::Number::from(i));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Some(serde_json::Number::from(u));
    }
    let f = s.parse::<f64>().ok()?;
    serde_json::Number::from_f64(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_native_bools() {
        assert_eq!(lenient_bool_from_json(&json!(true)), Some(true));
        assert_eq!(lenient_bool_from_json(&json!(false)), Some(false));
    }

    #[test]
    fn parses_string_true_false() {
        assert_eq!(lenient_bool_from_json(&json!("true")), Some(true));
        assert_eq!(lenient_bool_from_json(&json!("false")), Some(false));
    }

    #[test]
    fn parses_yes_no() {
        assert_eq!(lenient_bool_from_json(&json!("yes")), Some(true));
        assert_eq!(lenient_bool_from_json(&json!("no")), Some(false));
    }

    #[test]
    fn parses_string_one_zero() {
        assert_eq!(lenient_bool_from_json(&json!("1")), Some(true));
        assert_eq!(lenient_bool_from_json(&json!("0")), Some(false));
    }

    #[test]
    fn parses_numeric_one_zero() {
        assert_eq!(lenient_bool_from_json(&json!(1)), Some(true));
        assert_eq!(lenient_bool_from_json(&json!(0)), Some(false));
    }

    #[test]
    fn is_case_insensitive_and_trims() {
        assert_eq!(lenient_bool_from_json(&json!("TRUE")), Some(true));
        assert_eq!(lenient_bool_from_json(&json!("False")), Some(false));
        assert_eq!(lenient_bool_from_json(&json!("  yes  ")), Some(true));
        assert_eq!(lenient_bool_from_json(&json!("No")), Some(false));
    }

    #[test]
    fn parses_null_as_false() {
        assert_eq!(lenient_bool_from_json(&json!(null)), Some(false));
    }

    #[test]
    fn rejects_unknown_forms() {
        for v in [
            json!("maybe"),
            json!(""),
            json!(2),
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!([]),
            json!({}),
        ] {
            assert_eq!(lenient_bool_from_json(&v), None, "should reject {v}");
        }
    }

    fn deser_bool(json_str: &str) -> Result<bool, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_bool")]
            value: bool,
        }
        Ok(serde_json::from_str::<Wrapper>(json_str)?.value)
    }

    fn deser_opt_bool(json_str: &str) -> Result<Option<bool>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_option_bool")]
            value: Option<bool>,
        }
        Ok(serde_json::from_str::<Wrapper>(json_str)?.value)
    }

    #[test]
    fn required_accepts_all_forms() {
        assert!(deser_bool(r#"{"value":true}"#).unwrap());
        assert!(deser_bool(r#"{"value":"true"}"#).unwrap());
        assert!(deser_bool(r#"{"value":"yes"}"#).unwrap());
        assert!(deser_bool(r#"{"value":"1"}"#).unwrap());
        assert!(deser_bool(r#"{"value":1}"#).unwrap());
        assert!(!deser_bool(r#"{"value":"no"}"#).unwrap());
        assert!(!deser_bool(r#"{"value":0}"#).unwrap());
    }

    #[test]
    fn required_missing_uses_default() {
        assert!(!deser_bool(r#"{}"#).unwrap());
    }

    #[test]
    fn required_null_is_false() {
        assert!(!deser_bool(r#"{"value":null}"#).unwrap());
    }

    #[test]
    fn required_rejects_unknown() {
        let err = deser_bool(r#"{"value":"maybe"}"#).unwrap_err();
        assert!(err.to_string().contains("expected a boolean"));
    }

    #[test]
    fn optional_missing_is_none_but_null_is_false() {
        assert_eq!(deser_opt_bool(r#"{}"#).unwrap(), None);
        assert_eq!(deser_opt_bool(r#"{"value":null}"#).unwrap(), Some(false));
    }

    #[test]
    fn optional_parses_and_rejects() {
        assert_eq!(deser_opt_bool(r#"{"value":"yes"}"#).unwrap(), Some(true));
        assert_eq!(deser_opt_bool(r#"{"value":0}"#).unwrap(), Some(false));
        assert!(deser_opt_bool(r#"{"value":"nope"}"#).is_err());
    }

    // -- schema-driven coercion ---------------------------------------------

    fn coerced(mut params: serde_json::Value, schema: serde_json::Value) -> serde_json::Value {
        coerce_args_against_schema(&mut params, &schema);
        params
    }

    #[test]
    fn coerces_string_ints_against_integer_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "offset": {"type": "integer"},
                "limit": {"type": ["integer", "null"]},
            }
        });
        let out = coerced(json!({"offset": "300", "limit": "140"}), schema);
        assert_eq!(out, json!({"offset": 300, "limit": 140}));
    }

    #[test]
    fn coerces_negative_and_u64_range_strings() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"},
            }
        });
        let out = coerced(json!({"a": "-7", "b": "18446744073709551615"}), schema);
        assert_eq!(out["a"], json!(-7));
        assert_eq!(out["b"], json!(18_446_744_073_709_551_615_u64));
    }

    #[test]
    fn coerces_string_floats_against_number_schema() {
        let schema = json!({
            "type": "object",
            "properties": {"timeout": {"type": ["number", "null"]}}
        });
        let out = coerced(json!({"timeout": "1.5"}), schema);
        assert_eq!(out, json!({"timeout": 1.5}));
    }

    #[test]
    fn coerces_whole_float_to_integer() {
        let schema = json!({
            "type": "object",
            "properties": {"limit": {"type": "integer"}}
        });
        let out = coerced(json!({"limit": 3.0}), schema);
        assert_eq!(out, json!({"limit": 3}));
        assert!(out["limit"].is_i64());
    }

    #[test]
    fn leaves_lossy_and_invalid_values_untouched() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "integer"},
                "b": {"type": "integer"},
                "c": {"type": "integer"},
            }
        });
        let out = coerced(json!({"a": "1.5", "b": "abc", "c": 2.5}), schema.clone());
        assert_eq!(out, json!({"a": "1.5", "b": "abc", "c": 2.5}));
    }

    #[test]
    fn leaves_strings_alone_when_schema_allows_string() {
        let schema = json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "either": {"type": ["string", "integer"]},
            }
        });
        let out = coerced(json!({"id": "3", "either": "4"}), schema);
        assert_eq!(out, json!({"id": "3", "either": "4"}));
    }

    #[test]
    fn coerces_against_any_of_option_shape() {
        // schemars emits anyOf branches for Option<T> in some configurations.
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": {"anyOf": [{"type": "integer"}, {"type": "null"}]}
            }
        });
        let out = coerced(json!({"limit": "5"}), schema);
        assert_eq!(out, json!({"limit": 5}));
    }

    #[test]
    fn unwraps_string_encoded_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"id": {"type": "string"}}
                    }
                }
            }
        });
        let out = coerced(
            json!({"todos": "[{\"id\": \"1\"}, {\"id\": \"2\"}]"}),
            schema,
        );
        assert_eq!(out, json!({"todos": [{"id": "1"}, {"id": "2"}]}));
    }

    #[test]
    fn unwraps_string_encoded_objects() {
        let schema = json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": {"line": {"type": "integer"}}
                }
            }
        });
        // Nested repair applies after unwrapping: "line" is also stringified.
        let out = coerced(json!({"opts": "{\"line\": \"7\"}"}), schema);
        assert_eq!(out, json!({"opts": {"line": 7}}));
    }

    #[test]
    fn string_stays_when_json_shape_mismatches_schema() {
        let schema = json!({
            "type": "object",
            "properties": {"tags": {"type": "array"}}
        });
        // Parses as an object, but schema wants an array — leave it alone.
        let out = coerced(json!({"tags": "{\"a\": 1}"}), schema);
        assert_eq!(out, json!({"tags": "{\"a\": 1}"}));
    }

    #[test]
    fn coerces_nested_object_and_array_items() {
        let schema = json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": {"depth": {"type": "integer"}}
                },
                "ports": {
                    "type": "array",
                    "items": {"type": "integer"}
                }
            }
        });
        let out = coerced(
            json!({"opts": {"depth": "9"}, "ports": ["80", "443"]}),
            schema,
        );
        assert_eq!(out, json!({"opts": {"depth": 9}, "ports": [80, 443]}));
    }

    #[test]
    fn unknown_properties_and_schemaless_params_pass_through() {
        let schema = json!({
            "type": "object",
            "properties": {"known": {"type": "integer"}}
        });
        let out = coerced(json!({"unknown": "3"}), schema);
        assert_eq!(out, json!({"unknown": "3"}));

        let out = coerced(json!({"x": "3"}), json!({"type": "object"}));
        assert_eq!(out, json!({"x": "3"}));

        // Non-object params (e.g. raw MCP passthrough) are untouched.
        let mut arr = json!(["3"]);
        coerce_args_against_schema(&mut arr, &json!({"properties": {"a": {"type": "integer"}}}));
        assert_eq!(arr, json!(["3"]));
    }

    #[test]
    fn trims_whitespace_before_numeric_parse() {
        let schema = json!({
            "type": "object",
            "properties": {"n": {"type": "integer"}}
        });
        let out = coerced(json!({"n": "  42  "}), schema);
        assert_eq!(out, json!({"n": 42}));
    }
}

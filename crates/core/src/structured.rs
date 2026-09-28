//! Checking that an answer has the shape a schema asks for.
//!
//! A workflow asks a helper for JSON in a given shape, and the answer is only
//! any use to the script if it has that shape. This checks the part of JSON
//! Schema such shapes are written in -- `type`, `properties`, `required`,
//! `items`, `enum` and `additionalProperties` -- in a page of code, instead of
//! a validator crate that would bring the whole standard along for six words.
//!
//! Every problem comes back, each with the path to where it is
//! (`items[2].line: expected integer`), because the list goes back to the
//! model as a correction and one pass should be enough to fix all of it. A
//! keyword outside the subset is reported too rather than passed over: a
//! schema asking for `minimum` that quietly let anything through would accept
//! answers its author meant to refuse. Keywords that only describe, such as
//! `description`, pass as they are.

use serde_json::Value;

/// Keywords that describe a value without constraining it, so passing over
/// them never lets through an answer the schema meant to refuse.
const NOTES: [&str; 6] = [
    "title",
    "description",
    "default",
    "examples",
    "$schema",
    "$comment",
];

/// The names `type` can take.
const TYPES: [&str; 7] = [
    "string", "number", "integer", "boolean", "object", "array", "null",
];

/// Whether `schema` keeps to the part of JSON Schema `check` understands, and
/// if not, each place it strays.
pub fn check_schema(schema: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    schema_errors(schema, "", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Whether `value` has the shape `schema` describes, and if not, every place
/// it differs, or every place the schema itself strays.
pub fn check(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    check_schema(schema)?;
    let mut errors = Vec::new();
    value_errors(schema, value, "", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Every place in `schema` that strays from the subset, pushed onto `errors`.
fn schema_errors(schema: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        errors.push(at(path, "a schema must be an object"));
        return;
    };
    for (keyword, rule) in schema {
        match (keyword.as_str(), rule) {
            ("type", _)
                if names(rule)
                    .is_some_and(|n| !n.is_empty() && n.iter().all(|t| TYPES.contains(t))) => {}
            ("required", Value::Array(list)) if list.iter().all(Value::is_string) => {}
            ("enum", Value::Array(choices)) if !choices.is_empty() => {}
            ("additionalProperties", Value::Bool(_)) => {}
            ("properties", Value::Object(fields)) => {
                for (name, field) in fields {
                    schema_errors(field, &field_path(path, name), errors);
                }
            }
            ("items", _) => schema_errors(rule, &format!("{path}[]"), errors),
            ("additionalProperties", _) => schema_errors(rule, &field_path(path, "*"), errors),
            ("type", _) => errors.push(at(path, "type must be a JSON type name or a list of them")),
            ("required", _) => errors.push(at(path, "required must be a list of field names")),
            ("enum", _) => errors.push(at(path, "enum must be a list of at least one value")),
            ("properties", _) => errors.push(at(path, "properties must be an object of schemas")),
            (note, _) if NOTES.contains(&note) => {}
            (other, _) => errors.push(at(path, &format!("unsupported keyword \"{other}\""))),
        }
    }
}

/// Every place `value` differs from `schema`, pushed onto `errors`.
fn value_errors(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(allowed) = schema.get("type").and_then(names)
        && !allowed.iter().any(|name| is(value, name))
    {
        // Past a value of the wrong kind, its fields and items only add noise.
        errors.push(at(path, &format!("expected {}", allowed.join(" or "))));
        return;
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        let choices: Vec<String> = choices.iter().map(Value::to_string).collect();
        errors.push(at(path, &format!("expected one of {}", choices.join(", "))));
    }
    if let Some(object) = value.as_object() {
        let required = schema.get("required").and_then(Value::as_array);
        for name in required.into_iter().flatten().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                errors.push(at(&field_path(path, name), "missing"));
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (name, field) in object {
            let path = field_path(path, name);
            let rule = properties.and_then(|p| p.get(name));
            match (rule, schema.get("additionalProperties")) {
                (Some(rule), _) => value_errors(rule, field, &path, errors),
                (None, Some(Value::Bool(false))) => errors.push(at(&path, "unexpected field")),
                (None, Some(rule)) if rule.is_object() => value_errors(rule, field, &path, errors),
                _ => {}
            }
        }
    }
    if let (Some(items), Some(rule)) = (value.as_array(), schema.get("items")) {
        for (index, item) in items.iter().enumerate() {
            value_errors(rule, item, &format!("{path}[{index}]"), errors);
        }
    }
}

/// The type names a `type` keyword allows, written as one name or a list.
fn names(rule: &Value) -> Option<Vec<&str>> {
    match rule {
        Value::String(name) => Some(vec![name.as_str()]),
        Value::Array(list) => list.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

/// Whether `value` is of the type `name`, counting 3.0 as an integer as JSON
/// Schema does.
fn is(value: &Value, name: &str) -> bool {
    match name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    }
}

/// `problem`, said of the place `path` names.
fn at(path: &str, problem: &str) -> String {
    if path.is_empty() {
        problem.to_owned()
    } else {
        format!("{path}: {problem}")
    }
}

/// The path to the field `name` inside the place `path` names.
fn field_path(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_fields_and_enums_are_checked() {
        let schema = json!({
            "type": "object",
            "required": ["real", "reason"],
            "properties": {
                "real": { "type": "boolean" },
                "reason": { "type": "string" },
                "severity": { "enum": ["low", "high"] },
            },
        });
        let good = json!({ "real": true, "reason": "the caller checks first", "severity": "low" });
        assert_eq!(check(&schema, &good), Ok(()));
        assert_eq!(
            check(&schema, &json!({ "severity": "Low" })),
            Err(vec![
                "real: missing".to_owned(),
                "reason: missing".to_owned(),
                "severity: expected one of \"low\", \"high\"".to_owned(),
            ])
        );
    }

    #[test]
    fn an_unsupported_keyword_is_reported_not_ignored() {
        let schema = json!({
            "type": "array",
            "description": "Lines worth a look.",
            "items": {
                "type": "object",
                "properties": { "line": { "type": "integer", "minimum": 1 } },
            },
        });
        // Reported even though no item ever reaches the schema that uses it.
        let reported = Err(vec!["[].line: unsupported keyword \"minimum\"".to_owned()]);
        assert_eq!(check(&schema, &json!([])), reported);
        assert_eq!(check_schema(&schema), reported);
        assert_eq!(
            check_schema(&json!({ "title": "Vote", "type": "text" })),
            Err(vec![
                "type must be a JSON type name or a list of them".to_owned()
            ])
        );
    }

    #[test]
    fn nested_problems_name_the_path_to_them() {
        let findings = json!({
            "type": "object",
            "required": ["items"],
            "properties": { "items": { "type": "array", "items": {
                "type": "object",
                "required": ["line", "why"],
                "additionalProperties": false,
                "properties": { "line": { "type": "integer" }, "why": { "type": "string" } },
            } } },
        });
        let answer = json!({ "items": [
            { "line": 3, "why": "a flag is read" },
            { "line": 9, "why": "a path is split" },
            { "line": "40", "why": "a number is parsed" },
            { "extra": true, "line": 7 },
        ] });
        assert_eq!(
            check(&findings, &answer),
            Err(vec![
                "items[2].line: expected integer".to_owned(),
                "items[3].why: missing".to_owned(),
                "items[3].extra: unexpected field".to_owned(),
            ])
        );
        let row = json!({ "type": "array", "items": { "type": "integer" } });
        let grid = json!({ "type": "array", "items": row });
        assert_eq!(check(&grid, &json!([[1, 2], [3, 4]])), Ok(()));
        assert_eq!(
            check(&grid, &json!([[1, 2], [3, "4"]])),
            Err(vec!["[1][1]: expected integer".to_owned()])
        );
    }

    #[test]
    fn integers_are_whole_numbers_and_null_is_a_type_of_its_own() {
        let integer = json!({ "type": "integer" });
        assert_eq!(check(&integer, &json!(3)), Ok(()));
        assert_eq!(check(&integer, &json!(3.0)), Ok(()));
        assert_eq!(
            check(&integer, &json!(3.5)),
            Err(vec!["expected integer".to_owned()])
        );
        assert_eq!(check(&json!({ "type": "number" }), &json!(3.5)), Ok(()));
        assert_eq!(check(&json!({ "type": "number" }), &json!(3)), Ok(()));

        let maybe = json!({ "type": ["string", "null"] });
        assert_eq!(check(&maybe, &json!(null)), Ok(()));
        assert_eq!(check(&maybe, &json!("text")), Ok(()));
        assert_eq!(
            check(&maybe, &json!(1)),
            Err(vec!["expected string or null".to_owned()])
        );
        assert_eq!(
            check(&json!({ "type": "string" }), &json!(null)),
            Err(vec!["expected string".to_owned()])
        );
    }
}

//! The route input schema of one generated CRUD operation.
//!
//! A route states what a caller may send before the operation reads it. For a
//! generated operation that statement is its input contract, so the schema is
//! derived here from the contract and written under `generated/routes/`. A
//! package's `publication/attachments.json` and its component declaration name
//! that file in place of a schema (wamn-4omo), so no hand-written copy can
//! disagree with the contract.

use serde_json::{Map, Value, json};

use super::wit::{CRUD_ITEMS_MAXIMUM, CRUD_ITEMS_MINIMUM};

/// A canonical hyphenated UUID, the form every generated client sends.
const UUID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// The route input schema of one generated operation, from its input contract.
pub(super) fn route_input_schema(contract: &Value) -> Value {
    let mut item = ObjectSchema::default();
    // Every top-level member that states a type is one input: the request
    // identity, the key, the idempotency key and the revision.
    for (name, member) in contract.as_object().into_iter().flatten() {
        let Some(ty) = member.get("type").and_then(Value::as_str) else {
            continue;
        };
        let required = member.get("required").and_then(Value::as_bool) == Some(true);
        item.insert(name, present(scalar(ty), ty, required), required);
    }
    for writable in members(contract, "writable_fields") {
        let (Some(path), Some(ty)) = (
            writable.get("path").and_then(Value::as_str),
            writable.get("type").and_then(Value::as_str),
        ) else {
            continue;
        };
        let required = writable.get("omitted").and_then(Value::as_str) == Some("invalid_input");
        let schema = present(nullable(writable, ty), ty, required);
        match path.split_once('.') {
            // A group, such as an update's `change`, is always sent.
            Some((group, name)) => item.group(group).insert(name, schema, required),
            None => item.insert(path, schema, required),
        }
    }
    let mut filter = ObjectSchema::default();
    for declared in members(contract, "filters") {
        let (Some(name), Some(ty)) = (
            declared.get("field").and_then(Value::as_str),
            declared.get("type").and_then(Value::as_str),
        ) else {
            continue;
        };
        let mut value = scalar(ty);
        if let Some(values) = declared.get("values") {
            value["enum"] = values.clone();
        }
        filter.insert(name, json!({"type": "array", "items": value}), false);
    }
    if !filter.properties.is_empty() {
        item.insert("filter", filter.into_value(), false);
    }
    if let Some(sort) = contract.get("sort").filter(|sort| sort.is_object()) {
        item.insert(
            "sort",
            json!({
                "type": "object",
                "required": ["field", "direction"],
                "additionalProperties": false,
                "properties": {
                    "field": {"enum": sort["fields"]},
                    "direction": {"enum": sort["directions"]},
                },
            }),
            false,
        );
    }
    // The cursor is an opaque token the previous page returned.
    if contract.get("pagination").is_some_and(Value::is_object) {
        item.insert("cursor", json!({"type": "string", "minLength": 1}), false);
    }
    if let Some(limit) = contract.get("limit").filter(|limit| limit.is_object()) {
        item.insert(
            "limit",
            json!({"type": "integer", "minimum": limit["minimum"], "maximum": limit["maximum"]}),
            false,
        );
    }
    json!({
        "type": "array",
        "minItems": CRUD_ITEMS_MINIMUM,
        "maxItems": CRUD_ITEMS_MAXIMUM,
        "items": item.into_value(),
    })
}

/// One closed object: its properties and the ones a caller must send.
#[derive(Default)]
struct ObjectSchema {
    properties: Map<String, Value>,
    required: Vec<String>,
    groups: Vec<(String, Self)>,
}

impl ObjectSchema {
    fn insert(&mut self, name: &str, schema: Value, required: bool) {
        self.properties.insert(name.to_owned(), schema);
        if required {
            self.required.push(name.to_owned());
        }
    }

    fn group(&mut self, name: &str) -> &mut Self {
        let index = self
            .groups
            .iter()
            .position(|(group, _)| group == name)
            .unwrap_or_else(|| {
                self.groups.push((name.to_owned(), Self::default()));
                self.groups.len() - 1
            });
        &mut self.groups[index].1
    }

    fn into_value(mut self) -> Value {
        for (name, group) in std::mem::take(&mut self.groups) {
            self.insert(&name, group.into_value(), true);
        }
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": self.properties,
        });
        if !self.required.is_empty() {
            schema["required"] = json!(self.required);
        }
        schema
    }
}

fn members<'a>(contract: &'a Value, name: &str) -> impl Iterator<Item = &'a Value> {
    contract
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The JSON form of one contract type, as the generated codec reads it.
fn scalar(ty: &str) -> Value {
    match ty {
        "uuid" => json!({"type": "string", "pattern": UUID_PATTERN}),
        "int32" => json!({"type": "integer"}),
        "float64" => json!({"type": "number"}),
        "boolean" => json!({"type": "boolean"}),
        "timestamptz" => json!({"type": "string", "format": "date-time"}),
        "bytes" => {
            json!({"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}})
        }
        "json" => json!({}),
        // text, string, numeric, and int64, which a decimal string carries.
        _ => json!({"type": "string"}),
    }
}

/// A required free-text input is not empty.
fn present(mut schema: Value, ty: &str, required: bool) -> Value {
    if required && matches!(ty, "text" | "string") && schema.get("enum").is_none() {
        schema["minLength"] = json!(1);
    }
    schema
}

/// A writable field admits null at the route, and its contract states what an
/// explicit null means to the operation.
fn nullable(writable: &Value, ty: &str) -> Value {
    let mut schema = scalar(ty);
    if let Some(single) = schema.get("type").cloned() {
        schema["type"] = json!([single, "null"]);
    }
    if let Some(values) = writable.get("values").and_then(Value::as_array) {
        let mut values = values.clone();
        values.push(Value::Null);
        schema["enum"] = Value::Array(values);
    }
    if let Some(explicit_null) = writable.get("explicit_null") {
        schema["x-wamn-explicit-null"] = explicit_null.clone();
    }
    schema
}

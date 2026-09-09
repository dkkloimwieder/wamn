//! Projects declared field presence, null values and nested shapes.

use serde_json::Value;

use crate::client_ir::{FieldIr, leaf_fields};

fn field(path: String, type_name: String, required: bool, nullable: bool) -> FieldIr {
    FieldIr {
        path,
        type_name,
        required,
        nullable,
        values: Vec::new(),
        children: Vec::new(),
        minimum: None,
        maximum: None,
    }
}

fn values(value: Option<&Value>) -> Vec<String> {
    let mut values: Vec<_> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    values.sort();
    values.dedup();
    values
}

fn insert(tree: &mut Vec<FieldIr>, leaf: FieldIr, segments: &[&str], prefix: &str) {
    let Some((segment, rest)) = segments.split_first() else {
        return;
    };
    if rest.is_empty() {
        tree.push(leaf);
        tree.sort_by(|a, b| a.path.cmp(&b.path));
        tree.dedup();
        return;
    }
    let path = if prefix.is_empty() {
        (*segment).to_owned()
    } else {
        format!("{prefix}.{segment}")
    };
    let index = tree
        .iter()
        .position(|item| item.path == path)
        .unwrap_or_else(|| {
            tree.push(field(
                path.clone(),
                if segment.ends_with("[]") {
                    "array"
                } else {
                    "object"
                }
                .into(),
                true,
                false,
            ));
            tree.len() - 1
        });
    insert(&mut tree[index].children, leaf, rest, &path);
    tree.sort_by(|a, b| a.path.cmp(&b.path));
}

pub(super) fn fields_of(contract: &Value) -> Vec<FieldIr> {
    let mut tree = Vec::new();
    for declared in contract
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = declared.get("path").and_then(Value::as_str) else {
            continue;
        };
        let mut leaf = field(
            path.to_owned(),
            declared
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("opaque")
                .to_owned(),
            declared
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            declared
                .get("nullable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        leaf.values = values(declared.get("values"));
        insert(&mut tree, leaf, &path.split('.').collect::<Vec<_>>(), "");
    }
    tree
}

pub(super) fn input_fields_of(contract: &Value) -> Vec<FieldIr> {
    let mut tree = fields_of(contract);
    if !tree.is_empty() {
        // The custom input contract's line bound owns value.line[].
        if let Some(value) = tree.iter_mut().find(|field| field.path == "value")
            && let Some(line) = value
                .children
                .iter_mut()
                .find(|field| field.path == "value.line[]")
            && let Some(bounds) = contract.get("line")
        {
            line.minimum = bounds.get("minimum").and_then(Value::as_u64);
            line.maximum = bounds.get("maximum").and_then(Value::as_u64);
        }
        return tree;
    }
    if let Some(members) = contract.as_object() {
        for (name, member) in members {
            let Some(type_name) = member.get("type").and_then(Value::as_str) else {
                continue;
            };
            let mut declared = field(
                name.clone(),
                type_name.to_owned(),
                member
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                member
                    .get("nullable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            declared.values = values(member.get("values"));
            tree.push(declared);
        }
    }
    for writable in contract
        .get("writable_fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = writable
            .get("path")
            .or_else(|| writable.get("field"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let mut declared = field(
            name.to_owned(),
            writable
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("opaque")
                .to_owned(),
            writable
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            writable.get("explicit_null").and_then(Value::as_str) == Some("accepted"),
        );
        declared.values = values(writable.get("values"));
        insert(
            &mut tree,
            declared,
            &name.split('.').collect::<Vec<_>>(),
            "",
        );
    }
    for filter in contract
        .get("filters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(type_name)) = (
            filter.get("field").and_then(Value::as_str),
            filter.get("type").and_then(Value::as_str),
        ) else {
            continue;
        };
        if filter.get("binding").and_then(Value::as_str) != Some("json_array") {
            continue;
        }
        let path = format!("filter.{name}[]");
        let mut repeated = field(path.clone(), "array".into(), false, false);
        repeated
            .children
            .push(field(path.clone(), type_name.into(), true, false));
        insert(
            &mut tree,
            repeated,
            &path.split('.').collect::<Vec<_>>(),
            "",
        );
        if let Some(filter) = tree.iter_mut().find(|item| item.path == "filter") {
            filter.required = false;
        }
    }
    tree.sort();
    tree.dedup();
    tree
}

/// A served input schema states property presence and repeated bounds.
/// Descriptor hints retain UUID, timestamp and numeric types that JSON calls strings.
pub(super) fn schema_fields(schema: &Value, hints: &[FieldIr]) -> Vec<FieldIr> {
    let hints = leaf_fields(hints);
    let unknown = Value::Null;
    let item = if schema.get("type").and_then(Value::as_str) == Some("array") {
        schema.get("items").unwrap_or(&unknown)
    } else {
        schema
    };
    if item.get("properties").is_some() {
        object_fields(item, "", &hints)
    } else {
        vec![schema_field(item, String::new(), true, &hints)]
    }
}

fn object_fields(schema: &Value, prefix: &str, hints: &[&FieldIr]) -> Vec<FieldIr> {
    let required = values(schema.get("required"));
    schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, child)| {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            schema_field(child, path, required.contains(name), hints)
        })
        .collect()
}

fn schema_field(schema: &Value, mut path: String, required: bool, hints: &[&FieldIr]) -> FieldIr {
    let types = schema.get("type");
    let nullable = types
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|ty| ty == "null"))
        && schema.get("x-wamn-explicit-null").and_then(Value::as_str) != Some("invalid_input");
    let ty = types
        .and_then(Value::as_str)
        .or_else(|| {
            let mut alternatives = types
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .filter(|ty| *ty != "null");
            let first = alternatives.next()?;
            alternatives.next().is_none().then_some(first)
        })
        .or_else(|| {
            let domain = schema.get("enum")?.as_array()?;
            (!domain.is_empty() && domain.iter().all(Value::is_string)).then_some("string")
        });
    let mut result = field(path.clone(), "opaque".into(), required, nullable);
    if schema.get("oneOf").is_some()
        || schema.get("anyOf").is_some()
        || schema.get("$ref").is_some()
        || schema.get("allOf").is_some()
    {
        return result;
    }
    match ty {
        Some("object") => {
            result.type_name = "object".into();
            result.children = object_fields(schema, &path, hints);
        }
        Some("array") => {
            path.push_str("[]");
            result.path.clone_from(&path);
            result.type_name = "array".into();
            result.minimum = schema.get("minItems").and_then(Value::as_u64);
            result.maximum = schema.get("maxItems").and_then(Value::as_u64);
            if let Some(item) = schema.get("items") {
                result.children = if item.get("type").and_then(Value::as_str) == Some("object") {
                    object_fields(item, &path, hints)
                } else {
                    vec![schema_field(item, path, true, hints)]
                };
            }
        }
        Some("string" | "integer" | "number" | "boolean") => {
            result.type_name = hints.iter().find(|hint| hint.path == path).map_or_else(
                || {
                    match ty {
                        Some("integer") => "int64",
                        Some("number") => "float64",
                        Some("boolean") => "boolean",
                        _ => match schema.get("format").and_then(Value::as_str) {
                            Some("uuid") => "uuid",
                            Some("date-time") => "timestamptz",
                            _ => "text",
                        },
                    }
                    .to_owned()
                },
                |hint| hint.type_name.clone(),
            );
            result.values = values(schema.get("enum"));
            if let Some(hint) = hints.iter().find(|hint| hint.path == path) {
                result.nullable &= hint.nullable;
                if !hint.values.is_empty() {
                    if result.values.is_empty() {
                        result.values.clone_from(&hint.values);
                    } else {
                        result.values.retain(|value| hint.values.contains(value));
                        if result.values.is_empty() {
                            result.type_name = "opaque".into();
                        }
                    }
                }
            }
        }
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn presence_and_null_are_independent_in_the_served_schema() {
        let fields = schema_fields(
            &json!({
                "type":"object", "required":["a","b"], "properties":{
                    "a":{"type":"string"}, "b":{"type":["string","null"]},
                    "c":{"type":"string"}, "d":{"type":["string","null"]}
                }
            }),
            &[],
        );
        assert_eq!(
            fields
                .iter()
                .map(|f| (f.required, f.nullable))
                .collect::<Vec<_>>(),
            [(true, false), (true, true), (false, false), (false, true)]
        );
    }

    #[test]
    fn repeated_inputs_keep_children_bounds_and_typed_hints() {
        let hints = fields_of(
            &json!({"fields":[{"path":"value.line[].quantity","type":"numeric","nullable":false}]}),
        );
        let fields = schema_fields(
            &json!({"type":"array","items":{
                "type":"object","required":["value"],"properties":{"value":{
                    "type":"object","required":["line"],"properties":{"line":{
                        "type":"array","minItems":1,"maxItems":100,"items":{
                            "type":"object","required":["quantity"],"properties":{"quantity":{"type":"string"}}
                        }
                    }}
                }}
            }}),
            &hints,
        );
        let line = &fields[0].children[0];
        assert_eq!(
            (&line.path, line.minimum, line.maximum),
            (&"value.line[]".to_owned(), Some(1), Some(100))
        );
        assert_eq!(line.children[0].type_name, "numeric");
        assert!(line.children[0].required);
    }

    #[test]
    fn omission_does_not_permit_a_null_writable_value() {
        let fields = input_fields_of(&json!({"writable_fields":[{
            "field":"supplier_id","type":"uuid","omitted":"unchanged","explicit_null":"invalid_input"
        }]}));
        assert!(!fields[0].required);
        assert!(!fields[0].nullable);
    }
    #[test]
    fn unsupported_unions_do_not_silently_select_a_text_editor() {
        for schema in [
            json!({"type":["string","integer"]}),
            json!({"type":"string","allOf":[{"minLength":2}]}),
            json!({"$ref":"https://example.test/schema"}),
        ] {
            assert_eq!(schema_fields(&schema, &[])[0].type_name, "opaque");
        }
    }
    #[test]
    fn typed_contract_constraints_remain_stricter_than_the_transport_schema() {
        let hints = fields_of(&json!({"fields":[{
            "path":"status", "type":"text", "nullable":false, "values":["open","closed"]
        }]}));
        let fields = schema_fields(
            &json!({"type":"object","properties":{
                "status":{"type":["string","null"]}
            }}),
            &hints,
        );
        assert!(!fields[0].required);
        assert!(!fields[0].nullable);
        assert_eq!(fields[0].values, ["closed", "open"]);
    }
}

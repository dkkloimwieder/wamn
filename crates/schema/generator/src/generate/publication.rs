//! Inline publication schemas and OpenAPI from compiled operations.
use super::{OperationDescription, insert_json_line};
use crate::ManifestError as GenerateError;
use crate::client_fields::{fields_of, input_fields_of};
use crate::client_ir::FieldIr;
use crate::manifest::PackageManifest;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const ATTACHMENTS: &str = "generated/publication/attachments.json";
pub const COMPONENTS: &str = "generated/publication/components.json";
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

pub(super) fn emit_package_publication(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    operations: &BTreeMap<String, OperationDescription>,
) -> Result<(), GenerateError> {
    let mut attachments = BTreeMap::new();
    let mut components = BTreeMap::<String, Value>::new();
    for (component, declaration) in &manifest.components {
        for (export, operation) in &declaration.exports {
            if operation.registered_operation.is_some() {
                return Err(crate::ManifestError::new(
                    crate::ManifestErrorType::InvalidManifest,
                    format!("component export {export} cannot declare a registered operation"),
                ));
            }
        }
        components.insert(component.clone(), json!({
            "target": {"package-id": manifest.package.id, "package-version": manifest.package.version},
            "component": component, "interface-version": "0.1.0", "operations": declaration.exports,
        }));
    }
    let routes = &manifest.routes;
    let modes = routes.auth_modes.as_deref().unwrap_or(&[]);
    let modes = if modes.is_empty() {
        json!(["pat", "session"])
    } else {
        json!(modes)
    };
    let single = (manifest.components.len() == 1)
        .then(|| manifest.components.keys().next())
        .flatten();
    for (local, description) in operations {
        let (module, action) = local.split_once('.').expect("validated operation name");
        let model_operation = manifest.models.get(module).and_then(|model| {
            model
                .operations
                .iter()
                .find(|(operation, _)| operation.as_str() == action)
                .map(|(_, declaration)| declaration)
        });
        let component = model_operation
            .and_then(|operation| operation.component.as_ref())
            .or_else(|| {
                manifest
                    .custom_operations
                    .get(local)
                    .and_then(|operation| operation.component.as_ref())
            })
            .or(single)
            .expect("validated component owner");
        let identity = description.operation.operation.as_str();
        let input = input_schema(description);
        let result = result_schema(description);
        let declaration = components
            .get_mut(component)
            .expect("validated component owner");
        if declaration["operations"].get(identity).is_some() {
            return Err(crate::ManifestError::new(
                crate::ManifestErrorType::InvalidManifest,
                format!("component export {identity} is already generated"),
            ));
        }
        let mut published = json!({
            "input-ports": [{"name": "input", "schema": input}],
            "output-ports": [{"name": "main", "schema": result}],
            "parameters": manifest.components[component].parameters.get(local).map_or(&[][..], Vec::as_slice),
        });
        let connections = manifest.operation_connections(local)?;
        if !connections.is_empty() {
            published["connections"] = json!(connections);
        }
        if description.operation.visibility.as_deref() != Some("private") {
            published["registered-operation"] = json!(identity);
        }
        if !description.operation.type_.is_read() && !description.result.is_null() {
            published["committed-result-schema"] =
                json!({"type": "array", "items": result["items"]["oneOf"][0]});
        }
        declaration["operations"][identity] = published;
        let exposure = manifest.routes.operations.get(local);
        if model_operation.is_none() && exposure.is_none() {
            continue;
        }
        let id = wamn_catalog::route_attachment_id(
            routes.id_prefix.as_deref().unwrap_or(""),
            module,
            action,
        );
        let id = exposure.map_or(id, |exposure| exposure.id.clone());
        let path = exposure.map_or_else(
            || {
                wamn_catalog::route_path(
                    routes.path_prefix.as_deref().unwrap_or(""),
                    module,
                    action,
                )
            },
            |exposure| exposure.path.clone(),
        );
        let path = wamn_catalog::exposure::normalize_http_route(
            &wamn_catalog::exposure::HttpRoute {
                path,
                method: "POST".to_owned(),
            },
            &id,
        )
        .map_err(|source| {
            crate::ManifestError::new(
                crate::ManifestErrorType::InvalidManifest,
                source.to_string(),
            )
        })?
        .path;
        let definition = json!({
            "id": id, "type": "http",
            "route": {"path": path},
            "input-schema": input,
            "raw-body-bytes": {"maximum": RAW_BODY_MAXIMUM},
        });
        if attachments.contains_key(&id) {
            return Err(crate::ManifestError::new(
                crate::ManifestErrorType::InvalidManifest,
                format!("duplicate HTTP attachment id {id}"),
            ));
        }
        attachments.insert(
            id,
            json!({
                "type": "http", "package-id": manifest.package.id, "component": component,
                "operation": identity, "registered-operation": identity,
                "definition-hash": wamn_digest::canonical_json_sha256(&definition),
                "definition": definition, "auth-policy": {"modes": modes},
            }),
        );
    }
    insert_json_line(files, ATTACHMENTS, &attachments)?;
    insert_json_line(files, COMPONENTS, &components)?;
    let attachments = serde_json::to_value(attachments).expect("publication serializes");
    insert_json_line(
        files,
        "generated/openapi.json",
        &openapi(
            manifest,
            operations,
            &attachments,
            &serde_json::to_value(components).expect("components serialize"),
        ),
    )
}

pub(crate) fn input_schema(description: &OperationDescription) -> Value {
    if description.input.get("writable_fields").is_some() {
        return super::routes::route_input_schema(&description.input);
    }
    let mut object = object_schema(&input_fields_of(&description.input));
    if description.operation.lists.is_some()
        && let Some(limit) = object.pointer_mut("/properties/limit")
    {
        limit["minimum"] = json!(1);
        limit["maximum"] = json!(super::QUERY_LIMIT);
        if limit["type"] == "string" {
            limit["pattern"] = json!("^(?:[1-9]|[1-9][0-9]|100)$");
        }
    }
    if let Some(feed) = description.input.get("feed") {
        object["properties"]["feed"] = json!({"type": "object", "additionalProperties": false, "properties": {
            "cursor": {"type": "string", "minLength": 1},
            "limit": {"type": "integer", "minimum": feed["limit"]["minimum"], "maximum": feed["limit"]["maximum"]}
        }});
    }
    if description.result.is_null() {
        return object;
    }
    let envelope = description.input.get("envelope");
    json!({"type": "array", "items": object,
        "minItems": envelope.map_or(json!(1), |envelope| envelope["minimum"].clone()),
        "maxItems": envelope.map_or(json!(100), |envelope| envelope["maximum"].clone())})
}

fn field_schema(field: &FieldIr) -> Value {
    let mut schema = super::routes::scalar(&field.type_name);
    if !field.values.is_empty() {
        schema["enum"] = json!(field.values);
    }
    nullable_schema(schema, field.nullable)
}

fn nullable_schema(mut schema: Value, nullable: bool) -> Value {
    if nullable {
        if let Some(ty) = schema.get("type").cloned() {
            schema["type"] = json!([ty, "null"]);
        }
        if let Some(values) = schema.get_mut("enum").and_then(Value::as_array_mut) {
            values.push(Value::Null);
        }
    }
    schema
}

fn object_schema(fields: &[FieldIr]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for field in fields {
        let name = field
            .path
            .rsplit('.')
            .next()
            .unwrap_or(&field.path)
            .trim_end_matches("[]");
        let mut schema = match field.type_name.as_str() {
            "object" => object_schema(&field.children),
            "array" | "list" => {
                let mut schema = json!({"type": "array", "items": if let [item] = field.children.as_slice() && item.path == field.path { field_schema(item) } else { object_schema(&field.children) }});
                if let Some(minimum) = field.minimum {
                    schema["minItems"] = json!(minimum);
                }
                if let Some(maximum) = field.maximum {
                    schema["maxItems"] = json!(maximum);
                }
                schema
            }
            ty => super::routes::scalar(ty),
        };
        if !field.values.is_empty() {
            schema["enum"] = json!(field.values);
        }
        schema = nullable_schema(schema, field.nullable);
        properties.insert(name.to_owned(), schema);
        if field.required {
            required.push(name);
        }
    }
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

pub(crate) fn result_schema(description: &OperationDescription) -> Value {
    if description.result.is_null() {
        return input_schema(description);
    }
    let row = object_schema(&fields_of(&description.result));
    let class = description.operation.result.as_deref().unwrap_or("none");
    let mut value = match class {
        "one" => json!(row),
        "bounded_list" => {
            json!({"type": "object", "properties": {"rows": {"type": "array", "items": row, "maxItems": super::QUERY_LIMIT}}, "required": ["rows"], "additionalProperties": false})
        }
        "optional_one" => json!({"anyOf": [row, {"type": "null"}]}),
        "page" => json!({"type": "object", "properties": {
            "item": {"type": "array", "items": row, "maxItems": super::QUERY_LIMIT},
            "next_cursor": {"type": ["string", "null"]},
            "cursor": {"type": "string"}
        }, "required": ["item", "next_cursor"], "additionalProperties": false}),
        _ => json!({"type": "null"}),
    };
    if description.input.get("feed").is_some() {
        value["required"]
            .as_array_mut()
            .expect("page members")
            .push(json!("cursor"));
        let key = description
            .operation
            .lists
            .as_ref()
            .and_then(|lists| lists["key_field"].as_str())
            .unwrap_or("id");
        let id = row["properties"]
            .get(key)
            .cloned()
            .unwrap_or_else(|| json!({"type": "string"}));
        let changes = json!({"type": "object", "required": ["rows", "next_cursor", "complete"], "additionalProperties": false, "properties": {
            "rows": {"type": "array", "maxItems": super::QUERY_LIMIT, "items": {"type": "object", "required": ["id", "row_version", "deleted"], "additionalProperties": false, "properties": {
                "id": id, "row_version": {"type": "string"}, "deleted": {"type": "boolean"}, "data": row
            }}},
            "next_cursor": {"type": "string"}, "complete": {"type": "boolean"}
        }});
        value = json!({"oneOf": [value, changes]});
    }
    // Several constraints can report one refusal. Its operation has one variant,
    // so publish one alternative with the shared required detail members.
    let mut refusals = BTreeMap::<&str, (BTreeSet<&str>, serde_json::Map<String, Value>)>::new();
    for case in description.errors["cases"].as_array().into_iter().flatten() {
        let literal = case["literal"].as_str().expect("compiled refusal literal");
        let required = case
            .pointer("/detail/required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        let properties = required
            .iter()
            .copied()
            .chain(
                case.pointer("/detail/optional")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
            .map(|name| {
                let ty = case["detail"]["types"][name]
                    .as_str()
                    .expect("compiled detail type");
                (name.to_owned(), super::routes::scalar(ty))
            })
            .collect::<serde_json::Map<_, _>>();
        match refusals.entry(literal) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((required, properties));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (shared, members) = entry.get_mut();
                shared.retain(|name| required.contains(name));
                members.extend(properties);
            }
        }
    }
    let cases = refusals.into_iter().map(|(literal, (required, properties))| {
        json!({"type": "object", "properties": {
            "code": {"const": literal},
            "detail": {"type": "object", "properties": properties, "required": required, "additionalProperties": false}
        }, "required": ["code", "detail"], "additionalProperties": false})
    }).collect::<Vec<_>>();
    let error = json!({"oneOf": cases});
    let keyed = description.input.get("request_id").is_some()
        || input_fields_of(&description.input)
            .iter()
            .any(|field| field.path == "request_id");
    let mut item = json!({"oneOf": [
        {"type": "object", "properties": {"value": value}, "required": ["value"], "additionalProperties": false},
        {"type": "object", "properties": {"error": error}, "required": ["error"], "additionalProperties": false}
    ]});
    if keyed {
        for outcome in item["oneOf"].as_array_mut().expect("outcome alternatives") {
            outcome["properties"]["request_id"] = json!({"type": "string"});
            outcome["required"]
                .as_array_mut()
                .expect("outcome members")
                .push(json!("request_id"));
        }
    }
    json!({"type": "array", "items": item})
}

pub(crate) fn openapi(
    manifest: &PackageManifest,
    operations: &BTreeMap<String, OperationDescription>,
    attachments: &Value,
    components: &Value,
) -> Value {
    let mut paths = serde_json::Map::<String, Value>::new();
    for attachment in attachments
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
    {
        let Some(identity) = attachment["registered-operation"].as_str() else {
            continue;
        };
        let Some(operation) = operations
            .values()
            .find(|operation| operation.operation.operation == identity)
        else {
            continue;
        };
        let Some(path) = attachment
            .pointer("/definition/route/path")
            .or_else(|| attachment.pointer("/definition/route/template"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let method = operation.operation.type_.http_method();
        let input = attachment
            .pointer("/definition/input-schema")
            .cloned()
            .unwrap_or_else(|| input_schema(operation));
        let output = components
            .get(attachment["component"].as_str().unwrap_or_default())
            .and_then(|component| component["operations"].get(identity))
            .and_then(|operation| operation["output-ports"].as_array())
            .and_then(|ports| ports.iter().find(|port| port["name"] == "main"))
            .and_then(|port| port.get("schema"))
            .cloned()
            .unwrap_or_else(|| result_schema(operation));
        let mut entry = json!({"operationId": identity, "responses": {
            "200": {"description": "Operation outcomes", "content": {"application/json": {"schema": output}}}
        }});
        let request = input.get("items").unwrap_or(&input);
        let path_names = path
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect::<BTreeSet<_>>();
        let mut parameters = path_names
            .iter()
            .map(|name| {
                let schema = request["properties"]
                    .get(*name)
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "string"}));
                json!({"name": name, "in": "path", "required": true, "schema": schema})
            })
            .collect::<Vec<_>>();
        if method == "GET" {
            let required = request.get("required").and_then(Value::as_array);
            parameters.extend(request["properties"].as_object().into_iter().flatten().filter(|(name, _)| !path_names.contains(name.as_str())).map(|(name, schema)| {
                json!({"name": name, "in": "query", "required": required.is_some_and(|required| required.iter().any(|value| value == name)), "content": {"application/json": {"schema": schema}}})
            }));
        } else {
            entry["requestBody"] =
                json!({"required": true, "content": {"application/json": {"schema": input}}});
        }
        if !parameters.is_empty() {
            entry["parameters"] = json!(parameters);
        }
        paths.entry(path.to_owned()).or_insert_with(|| json!({}))[method.to_ascii_lowercase()] =
            entry;
    }
    json!({"openapi": "3.1.0", "info": {"title": manifest.package.id, "version": manifest.package.version}, "paths": paths})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GenerationInput, StatementTransactionality, generate};
    use wamn_scalar::SqlValueType;
    use wamn_schema_introspection::ir::{CatalogIr, Column, ColumnDefault, Constraint, Table};

    #[test]
    fn published_schemas_accept_outcomes_and_refuse_unbounded_reads() {
        let catalog = CatalogIr::new(vec![Table::new(
            "app",
            "record",
            vec![
                Column::new("id", SqlValueType::Uuid, false, None, None),
                Column::new("code", SqlValueType::Text, false, None, None),
                Column::new("other", SqlValueType::Text, false, None, None),
                Column::new(
                    "revision",
                    SqlValueType::Int32,
                    false,
                    Some(ColumnDefault::int32(1)),
                    None,
                ),
                Column::new(
                    "created_at",
                    SqlValueType::Timestamptz,
                    false,
                    Some(ColumnDefault::CurrentTimestamp),
                    None,
                ),
            ],
            vec![
                Constraint::primary_key("record_pkey", ["id"]).unwrap(),
                Constraint::unique("record_code_key", ["code"]).unwrap(),
                Constraint::unique("record_other_key", ["other"]).unwrap(),
            ],
            Vec::new(),
        )]);
        let authored = serde_json::to_vec(&json!({
            "package": {"id": "example", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "example_access", "state": "satisfied"},
            "models": {"record": {
                "schema": "app", "table": "record", "owner": "example",
                "audit_log": {"columns": ["created_at"], "retention": "unlimited"},
                "server_owned_fields": ["revision"],
                "operations": {"query": {
                    "permission": "record.query", "result": "page", "filters": [{"field": "code"}],
                    "pagination": {"default_sort": {"field": "created_at", "direction": "ascending"}, "tie_breaker": {"field": "id"}},
                    "limit": {"default": 100, "minimum": 1, "maximum": 100}
                }, "update": {"permission": "record.update", "writable_fields": ["code", "other"], "revision_field": "revision", "result": "one"}}
            }},
            "connections": {}, "components": {"data": {}}
        })).unwrap();
        let manifest = crate::derive_manifest(&catalog, &authored, &[], &[]).unwrap();
        let facts = StatementTransactionality::from_paths(BTreeMap::new());
        let package = generate(&GenerationInput::new(&catalog, &manifest, &[], &facts)).unwrap();
        let mut duplicate: Value = serde_json::from_slice(&manifest).unwrap();
        duplicate["routes"]["operations"] = json!({
            "record.query": {"id": "record-http", "path": "/record/query"},
            "record.update": {"id": "record-http", "path": "/record/update"}
        });
        assert!(
            generate(&GenerationInput::new(
                &catalog,
                &serde_json::to_vec(&duplicate).unwrap(),
                &[],
                &facts
            ))
            .is_err()
        );

        let attachments: BTreeMap<String, wamn_catalog::ServingAttachment> =
            serde_json::from_slice(package.file(ATTACHMENTS).unwrap().bytes()).unwrap();
        for attachment in attachments.values().filter(|attachment| {
            attachment.registered_operation.as_deref()
                == Some(
                    package.operations()["record.query"]
                        .operation
                        .operation
                        .as_str(),
                )
        }) {
            let schema = attachment.definition["input-schema"].clone();
            let mut compiler = boon::Compiler::new();
            compiler.set_default_draft(boon::Draft::V2020_12);
            compiler.add_resource("mem://input.json", schema).unwrap();
            let mut schemas = boon::Schemas::new();
            let input = compiler.compile("mem://input.json", &mut schemas).unwrap();
            assert!(schemas.validate(&json!([{"limit": 100}]), input).is_ok());
            for invalid in [json!([{"limit": 101}]), json!([{"feed": {"limit": 101}}])] {
                assert!(schemas.validate(&invalid, input).is_err());
            }
        }
        let declarations: BTreeMap<String, crate::publication::CompiledComponentDeclaration> =
            serde_json::from_slice(package.file(COMPONENTS).unwrap().bytes()).unwrap();
        for declaration in declarations.values() {
            for (identity, operation) in &declaration.operations {
                for port in &operation.output_ports {
                    let mut compiler = boon::Compiler::new();
                    compiler.set_default_draft(boon::Draft::V2020_12);
                    compiler
                        .add_resource("mem://output.json", port.schema.clone())
                        .unwrap();
                    let mut schemas = boon::Schemas::new();
                    let output = compiler.compile("mem://output.json", &mut schemas).unwrap();
                    if identity == &package.operations()["record.update"].operation.operation {
                        assert!(schemas.validate(&json!([{"request_id": "r", "error": {"code": "concurrency_conflict", "detail": {"expected_row_version": 1, "observed_row_version": 2}}}]), output).is_ok());
                        assert!(schemas.validate(&json!([{"request_id": "r", "error": {"code": "concurrency_conflict", "detail": {"expected_row_version": "1", "observed_row_version": "2"}}}]), output).is_err());
                        for constraint in ["record_code_key", "record_other_key"] {
                            assert!(schemas.validate(&json!([{"request_id": "r", "error": {"code": "unique_violation", "detail": {"constraint": constraint, "field": "code"}}}]), output).is_ok());
                        }
                    } else {
                        assert!(
                        schemas
                            .validate(
                                &json!([{"value": {"item": [], "next_cursor": null, "cursor": "start"}}]),
                                output
                            )
                            .is_ok()
                    );
                        assert!(schemas.validate(&json!([{"error": {"code": "invalid_input", "detail": {"field": "limit"}}}]), output).is_ok());
                        assert!(
                            schemas
                                .validate(
                                    &json!([{"error": {"code": "invalid_input", "detail": {}}}]),
                                    output
                                )
                                .is_err()
                        );
                    }
                }
            }
        }
        let mut routed = serde_json::to_value(&attachments).unwrap();
        for attachment in routed.as_object_mut().unwrap().values_mut() {
            let request = &attachment["definition"]["input-schema"]["items"];
            let name = request["properties"]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap();
            attachment["definition"]["route"]["path"] = json!(format!("/record/{{{name}}}"));
        }
        let api = openapi(
            &PackageManifest::from_slice(&manifest).unwrap(),
            package.operations(),
            &routed,
            &serde_json::to_value(declarations).unwrap(),
        );
        for (path, methods) in api["paths"].as_object().unwrap() {
            for method in methods.as_object().unwrap().values() {
                let parameters = method["parameters"].as_array().unwrap();
                for name in path
                    .split('{')
                    .skip(1)
                    .map(|part| part.split_once('}').unwrap().0)
                {
                    let parameter = parameters
                        .iter()
                        .find(|parameter| parameter["name"] == name)
                        .unwrap();
                    assert!(parameter["required"].as_bool().unwrap());
                    assert_eq!(parameter["in"], "path");
                    assert!(parameter.get("schema").is_some());
                    assert!(
                        !parameters.iter().any(
                            |parameter| parameter["name"] == name && parameter["in"] == "query"
                        )
                    );
                }
            }
        }
    }
}

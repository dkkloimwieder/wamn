//! OpenAPI documents from published route schemas.

use oas3::spec::{
    Info, MediaType, ObjectOrReference, ObjectSchema, Operation, Parameter, ParameterIn, PathItem,
    RequestBody, Response, Schema, SchemaType, SchemaTypeSet,
};
use oas3::{Map, Spec};
use serde_json::{Value, json};

pub(crate) fn document(title: String, version: String, paths: Map<String, PathItem>) -> Spec {
    Spec {
        openapi: "3.1.0".to_owned(),
        info: Info {
            title,
            version,
            summary: None,
            description: None,
            terms_of_service: None,
            contact: None,
            license: None,
            extensions: Map::new(),
        },
        servers: Vec::new(),
        paths: Some(paths),
        components: None,
        security: Vec::new(),
        tags: Vec::new(),
        webhooks: Map::new(),
        external_docs: None,
        extensions: Map::new(),
    }
}

fn json_content(schema: Schema) -> Map<String, MediaType> {
    Map::from([(
        "application/json".to_owned(),
        MediaType {
            schema: Some(schema),
            ..MediaType::default()
        },
    )])
}

pub(crate) fn add_route<'a>(
    paths: &'a mut Map<String, PathItem>,
    path: &str,
    method: &str,
    operation_id: String,
    input: Schema,
    output: Schema,
) -> &'a mut Operation {
    let mut entry = Operation {
        operation_id: Some(operation_id),
        responses: Some(Map::from([(
            "200".to_owned(),
            ObjectOrReference::Object(Response {
                description: Some("Operation outcomes".to_owned()),
                content: json_content(output),
                ..Response::default()
            }),
        )])),
        ..Operation::default()
    };
    let request =
        object(&input).and_then(|schema| schema.items.as_deref().map_or(Some(schema), object));
    let path_names = path
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        .collect::<std::collections::BTreeSet<_>>();
    for name in &path_names {
        let schema = request
            .and_then(|request| request.properties.get(*name))
            .cloned()
            .unwrap_or_else(|| {
                Schema::Object(
                    ObjectOrReference::Object(ObjectSchema {
                        schema_type: Some(SchemaTypeSet::Single(SchemaType::String)),
                        ..ObjectSchema::default()
                    })
                    .into(),
                )
            });
        entry
            .parameters
            .push(parameter(name, ParameterIn::Path, true, schema));
    }
    if method == "GET" {
        if let Some(request) = request {
            for (name, schema) in &request.properties {
                if !path_names.contains(name.as_str()) {
                    entry.parameters.push(parameter(
                        name,
                        ParameterIn::Query,
                        request.required.contains(name),
                        schema.clone(),
                    ));
                }
            }
        }
    } else {
        entry.request_body = Some(ObjectOrReference::Object(RequestBody {
            required: Some(true),
            content: json_content(input),
            ..RequestBody::default()
        }));
    }
    if !paths.contains_key(path) {
        paths.insert(path.to_owned(), PathItem::default());
    }
    let item = &mut paths[path];
    match method {
        "GET" => item.get.insert(entry),
        "POST" => item.post.insert(entry),
        _ => unreachable!("unsupported published HTTP method: {method}"),
    }
}

fn object(schema: &Schema) -> Option<&ObjectSchema> {
    match schema {
        Schema::Object(schema) => match schema.as_ref() {
            ObjectOrReference::Object(schema) => Some(schema),
            ObjectOrReference::Ref { .. } => None,
        },
        Schema::Boolean(_) => None,
    }
}

fn parameter(
    name: &str,
    location: ParameterIn,
    required: bool,
    schema: Schema,
) -> ObjectOrReference<Parameter> {
    let (schema, content) = match location {
        ParameterIn::Query => (None, Some(json_content(schema))),
        _ => (Some(schema), None),
    };
    ObjectOrReference::Object(Parameter {
        name: name.to_owned(),
        location,
        required: Some(required),
        schema,
        content,
        description: None,
        deprecated: None,
        allow_empty_value: None,
        style: None,
        explode: None,
        allow_reserved: None,
        example: None,
        examples: Map::new(),
        extensions: Map::new(),
    })
}

/// A canonical hyphenated UUID.
const UUID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

pub(crate) fn scalar(ty: &str) -> Value {
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

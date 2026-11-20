//! OpenAPI for the two sets of host routes.

use anyhow::{Context as _, Result};
use oas3::Map;
use oas3::spec::{ObjectOrReference, ObjectSchema, Schema, SchemaType, SchemaTypeSet};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_routes::{HostRouteSet, host_route_package, host_route_version};

use crate::openapi::{add_route, document, scalar};

/// Build the OpenAPI document for one host route set.
pub fn host_openapi(set: HostRouteSet) -> Result<Vec<u8>> {
    let contracts = set.contract_files()?;
    let mut paths = Map::new();
    for (_, attachment) in set.attachments() {
        let route = attachment.route;
        let name = route.operation.replace('.', "/");
        let contract = |part| {
            let file = format!("{name}.{part}.json");
            contracts.get(&file).with_context(|| format!("read {file}"))
        };
        let input = fields(contract("input")?).with_context(|| format!("read {name} input"))?;
        let keyed = input.properties.contains_key("request_id");
        let input = inline(input);
        let method = route.type_.http_method();
        let input = if method == "GET" {
            input
        } else {
            envelope(input)
        };
        let result = fields(contract("result")?).with_context(|| format!("read {name} result"))?;
        let refusals: Value = serde_json::from_slice(contract("errors")?)
            .with_context(|| format!("read {name} refusals"))?;
        let descriptor: Value = serde_json::from_slice(contract("operation")?)
            .with_context(|| format!("read {name} descriptor"))?;
        let response_contract = response_contract(&descriptor, &refusals);
        let refusals: Refusals = serde_json::from_value(refusals)?;
        let outcome = |name, value| {
            let mut item = record([(name, value)]);
            if keyed {
                item.required.push("request_id".to_owned());
                item.properties.insert(
                    "request_id".to_owned(),
                    inline(ObjectSchema {
                        schema_type: Some(SchemaTypeSet::Single(SchemaType::String)),
                        ..ObjectSchema::default()
                    }),
                );
            }
            inline(item)
        };
        let output_schema = envelope(inline(ObjectSchema {
            one_of: vec![
                outcome("value", inline(result)),
                outcome(
                    "error",
                    inline(ObjectSchema {
                        one_of: refusals.cases.into_iter().map(refusal_schema).collect(),
                        ..ObjectSchema::default()
                    }),
                ),
            ],
            ..ObjectSchema::default()
        }));
        let path = attachment.definition["route"]["path"]
            .as_str()
            .context("read host route path")?;
        let operation = add_route(
            &mut paths,
            path,
            method,
            attachment.operation.as_str().to_owned(),
            input,
            output_schema,
        );
        operation.extensions.insert("wamn-contract".to_owned(), response_contract);
    }
    let spec = document(
        host_route_package().as_str().to_owned(),
        host_route_version().as_str().to_owned(),
        paths,
    );
    let mut bytes = serde_json::to_vec(&spec)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn response_contract(descriptor: &Value, refusals: &Value) -> Value {
    let errors = crate::client_ir::errors_of(Some(refusals))
        .into_iter()
        .map(|case| json!({
            "literal": case.literal,
            "required": case.detail_required,
            "sources": case.sources,
            "text": case.text,
        }))
        .collect::<Vec<_>>();
    json!({
        "resultClass": descriptor["result"],
        "partialSchema": null,
        "errors": errors,
        "replay": null,
        "direct": true,
        "type": descriptor["type"],
        "transaction": descriptor["transaction"],
        "reads": [],
        "writes": null,
    })
}

#[derive(Deserialize)]
struct Contract {
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Field {
    path: String,
    #[serde(rename = "type")]
    type_name: String,
    nullable: bool,
    #[serde(default)]
    values: Vec<Value>,
    #[serde(default = "required")]
    required: bool,
}

#[derive(Deserialize)]
struct Refusals {
    cases: Vec<Refusal>,
}

#[derive(Deserialize)]
struct Refusal {
    literal: String,
    detail: RefusalDetail,
}

#[derive(Deserialize)]
struct RefusalDetail {
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

fn refusal_schema(case: Refusal) -> Schema {
    let detail = ObjectSchema {
        schema_type: Some(SchemaTypeSet::Single(SchemaType::Object)),
        properties: case
            .detail
            .required
            .iter()
            .cloned()
            .chain(case.detail.optional)
            .map(|name| (name, inline(ObjectSchema::default())))
            .collect(),
        required: case.detail.required,
        ..ObjectSchema::default()
    };
    let code = ObjectSchema {
        const_value: Some(Value::String(case.literal)),
        ..ObjectSchema::default()
    };
    inline(record([("code", inline(code)), ("detail", inline(detail))]))
}

fn required() -> bool {
    true
}

fn fields(bytes: &[u8]) -> Result<ObjectSchema> {
    let contract: Contract = serde_json::from_slice(bytes)?;
    let mut object = record([]);
    for field in contract.fields {
        let mut schema = scalar(match field.type_name.as_str() {
            "bool" => "boolean",
            name => name,
        });
        if field.nullable
            && let Some(ty) = schema.get_mut("type")
        {
            *ty = json!([ty.take(), "null"]);
        }
        if !field.values.is_empty() {
            let mut values = field.values;
            if field.nullable {
                values.push(Value::Null);
            }
            schema["enum"] = Value::Array(values);
        }
        insert_field(
            &mut object,
            &field.path,
            field.required,
            serde_json::from_value(schema)?,
        );
    }
    Ok(object)
}

fn insert_field(object: &mut ObjectSchema, path: &str, required: bool, schema: Schema) {
    let (segment, rest) = path
        .split_once('.')
        .map_or((path, None), |(head, tail)| (head, Some(tail)));
    let name = segment.strip_suffix("[]").unwrap_or(segment);
    let repeated = name != segment;
    if let Some(rest) = rest {
        if !object.properties.contains_key(name) {
            let inner = inline(record([]));
            object
                .properties
                .insert(name.to_owned(), if repeated { array(inner) } else { inner });
            object.required.push(name.to_owned());
        }
        let mut child = object
            .properties
            .get_mut(name)
            .and_then(object_mut)
            .expect("a parent is a schema object");
        if repeated {
            child = child
                .items
                .as_deref_mut()
                .and_then(object_mut)
                .expect("array items are schema objects");
        }
        insert_field(child, rest, required, schema);
    } else {
        object.properties.insert(
            name.to_owned(),
            if repeated { array(schema) } else { schema },
        );
        if required {
            object.required.push(name.to_owned());
        }
    }
}

fn inline(schema: ObjectSchema) -> Schema {
    Schema::Object(ObjectOrReference::Object(schema).into())
}

fn object_mut(schema: &mut Schema) -> Option<&mut ObjectSchema> {
    let Schema::Object(schema) = schema else {
        return None;
    };
    let ObjectOrReference::Object(schema) = schema.as_mut() else {
        return None;
    };
    Some(schema)
}

fn array(items: Schema) -> Schema {
    inline(ObjectSchema {
        schema_type: Some(SchemaTypeSet::Single(SchemaType::Array)),
        items: Some(items.into()),
        ..ObjectSchema::default()
    })
}

fn envelope(items: Schema) -> Schema {
    inline(ObjectSchema {
        schema_type: Some(SchemaTypeSet::Single(SchemaType::Array)),
        items: Some(items.into()),
        min_items: Some(1),
        max_items: Some(1),
        ..ObjectSchema::default()
    })
}

fn record<const N: usize>(properties: [(&str, Schema); N]) -> ObjectSchema {
    ObjectSchema {
        schema_type: Some(SchemaTypeSet::Single(SchemaType::Object)),
        required: properties
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect(),
        properties: properties
            .into_iter()
            .map(|(name, schema)| (name.to_owned(), schema))
            .collect(),
        additional_properties: Some(Schema::Boolean(oas3::spec::BooleanSchema(false))),
        ..ObjectSchema::default()
    }
}

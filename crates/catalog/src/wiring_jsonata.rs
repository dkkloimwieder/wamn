//! Node-specific JSONata port contracts, derived from wiring configuration.

use crate::canonical_name::CanonicalName;

use crate::{
    AdmittedComponent, AdmittedComponentOperation, WiringCompatibilityError,
    WiringCompatibilityErrorType, WiringNode,
};
use std::borrow::Cow;

/// Specialize palette JSONata ports from the node's required schema declarations.
///
/// Other operations are borrowed unchanged. JSONata operations are owned and
/// carry the declared input schema on every input and the declared output schema
/// on each selectable output.
///
/// # Errors
/// Returns a refusal for a missing or invalid schema declaration.
pub fn specialize_wiring_operation<'a>(
    node_id: &CanonicalName,
    node: &WiringNode,
    component: &AdmittedComponent,
    operation: &'a AdmittedComponentOperation,
) -> Result<Cow<'a, AdmittedComponentOperation>, WiringCompatibilityError> {
    if component.target.package_id.as_str() != crate::PALETTE_PACKAGE_ID
        || node.component != "jsonata"
        || node.operation != "wamn:node/handler@0.1.0"
    {
        return Ok(Cow::Borrowed(operation));
    }
    let declared_schema = |parameter: &str| {
        let schema = node.params.get(parameter).ok_or_else(|| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorType::MissingRequiredParameter,
                format!("JSONata node {node_id:?} must declare {parameter}"),
            )
        })?;
        crate::component_facts::normalize_schema(schema.clone(), parameter).map_err(|error| {
            WiringCompatibilityError::new(
                WiringCompatibilityErrorType::InvalidParameter,
                format!("JSONata node {node_id:?} has an invalid {parameter}: {error}"),
            )
        })
    };
    let input_schema = declared_schema("input-schema")?;
    let output_schema = declared_schema("output-schema")?;
    let mut operation = operation.clone();
    for port in &mut operation.input_ports {
        port.schema = input_schema.clone();
    }
    let selected = node
        .params
        .get("port")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(crate::MAIN_PORT);
    for port in &mut operation.output_ports {
        if node.params.contains_key("port-input-pointer") || port.name == selected {
            port.schema = output_schema.clone();
        }
    }
    Ok(Cow::Owned(operation))
}

/// Three deterministic probes keep publication work bounded. They exercise
/// alternate scalar values, optional members and array sizes; they are not a proof.
const SAMPLE_COUNT: usize = 3;
/// Bound the size and nesting of generated publication inputs.
const SAMPLE_VALUE_BUDGET: usize = 256;
const SAMPLE_DEPTH: usize = 16;

pub(super) fn validate_samples(
    node_id: &CanonicalName,
    node: &WiringNode,
) -> Result<(), WiringCompatibilityError> {
    let expression = node.params["expression"]
        .as_str()
        .expect("validated expression parameter");
    let parameters = node
        .params
        .get("parameters")
        .unwrap_or(&serde_json::Value::Null);
    let output = &node.params["output-schema"];
    let mut output_schemas = boon::Schemas::new();
    let mut compiler = boon::Compiler::new();
    compiler.set_default_draft(boon::Draft::V2020_12);
    compiler
        .add_resource("mem://output.json", output.clone())
        .expect("validated output schema");
    let output_index = compiler
        .compile("mem://output.json", &mut output_schemas)
        .expect("validated output schema");
    let input = &node.params["input-schema"];
    let mut input_schemas = boon::Schemas::new();
    let uri = "mem://input.json";
    let mut compiler = boon::Compiler::new();
    compiler.set_default_draft(boon::Draft::V2020_12);
    compiler
        .add_resource(uri, input.clone())
        .expect("admitted input schema");
    let input_index = compiler
        .compile(uri, &mut input_schemas)
        .expect("admitted input schema");
    let mut checked = false;
    for number in 0..SAMPLE_COUNT {
        let mut budget = SAMPLE_VALUE_BUDGET;
        let sample = sample(input, input, number, 0, &mut budget);
        if input_schemas.validate(&sample, input_index).is_err() {
            continue;
        }
        let output = wamn_jsonata::evaluate_sample(expression, &sample, parameters)
            .map_err(|error| refusal(node_id, number, &sample, &error))?;
        let output: serde_json::Value = serde_json::from_str(&output).expect("JSONata emits JSON");
        output_schemas
            .validate(&output, output_index)
            .map_err(|error| refusal(node_id, number, &sample, &error))?;
        checked = true;
    }
    if !checked {
        return Err(WiringCompatibilityError::new(
            WiringCompatibilityErrorType::JsonataSample,
            format!("JSONata node {node_id:?}: no valid input schema sample could be built"),
        ));
    }
    Ok(())
}

fn refusal(
    node: &CanonicalName,
    number: usize,
    sample: &serde_json::Value,
    error: &dyn std::fmt::Display,
) -> WiringCompatibilityError {
    WiringCompatibilityError::new(
        WiringCompatibilityErrorType::JsonataSample,
        format!("JSONata node {node:?} input sample {number} ({sample}) failed: {error}"),
    )
}

fn sample(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    number: usize,
    depth: usize,
    budget: &mut usize,
) -> serde_json::Value {
    use serde_json::{Value, json};
    if depth >= SAMPLE_DEPTH || *budget == 0 {
        return Value::Null;
    }
    *budget -= 1;
    if let Some(examples) = schema
        .get("examples")
        .and_then(Value::as_array)
        .filter(|examples| !examples.is_empty())
    {
        return examples[number % examples.len()].clone();
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(values) = schema
        .get("enum")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
    {
        return values[number % values.len()].clone();
    }
    if let Some(reference) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        && let Some(target) = root.pointer(reference)
    {
        return sample(target, root, number, depth + 1, budget);
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(choices) = schema
            .get(keyword)
            .and_then(Value::as_array)
            .filter(|choices| !choices.is_empty())
        {
            return sample(
                &choices[number % choices.len()],
                root,
                number,
                depth + 1,
                budget,
            );
        }
    }
    let type_ = schema
        .get("type")
        .and_then(|value| {
            value.as_str().or_else(|| {
                let values = value.as_array()?;
                values.get(number % values.len()).and_then(Value::as_str)
            })
        })
        .unwrap_or_else(|| {
            if schema.get("properties").is_some() {
                "object"
            } else {
                "null"
            }
        });
    match type_ {
        "object" => {
            let required = schema.get("required").and_then(Value::as_array);
            Value::Object(
                schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter(|(name, _)| {
                        number != 0
                            || required.is_some_and(|required| {
                                required.iter().any(|value| value.as_str() == Some(name))
                            })
                    })
                    .map(|(name, schema)| {
                        (
                            name.clone(),
                            sample(schema, root, number, depth + 1, budget),
                        )
                    })
                    .collect(),
            )
        }
        "array" => {
            let minimum = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            let maximum = schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| minimum.saturating_add(2));
            let size = minimum
                .saturating_add(u64::try_from(number).expect("bounded sample number"))
                .min(maximum)
                .min(100);
            Value::Array(
                (0..size)
                    .map(|index| {
                        let item = schema
                            .get("prefixItems")
                            .and_then(Value::as_array)
                            .and_then(|items| {
                                usize::try_from(index)
                                    .ok()
                                    .and_then(|index| items.get(index))
                            })
                            .unwrap_or(&schema["items"]);
                        sample(item, root, number, depth + 1, budget)
                    })
                    .collect(),
            )
        }
        "boolean" => json!(number != 0),
        "integer" | "number" => {
            let minimum = schema
                .get("minimum")
                .and_then(Value::as_f64)
                .or_else(|| {
                    schema
                        .get("exclusiveMinimum")
                        .and_then(Value::as_f64)
                        .map(|value| value + 1.0)
                })
                .unwrap_or(0.0);
            let maximum = schema
                .get("maximum")
                .and_then(Value::as_f64)
                .or_else(|| {
                    schema
                        .get("exclusiveMaximum")
                        .and_then(Value::as_f64)
                        .map(|value| value - 1.0)
                })
                .unwrap_or(minimum + 2.0);
            let value = (minimum
                + f64::from(u32::try_from(number).expect("bounded sample number")))
            .min(maximum);
            let value = if type_ == "integer" {
                value.ceil()
            } else {
                value
            };
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
        }
        "string" => {
            let value = match schema.get("format").and_then(Value::as_str) {
                Some("uuid") => "00000000-0000-4000-8000-000000000001".to_owned(),
                Some("date-time") => format!("2026-01-0{}T00:00:00Z", number + 1),
                Some("date") => format!("2026-01-0{}", number + 1),
                _ if schema
                    .get("pattern")
                    .and_then(Value::as_str)
                    .is_some_and(|pattern| pattern.contains("[0-9a-f]")) =>
                {
                    "00000000-0000-4000-8000-000000000001".to_owned()
                }
                _ => {
                    let length = schema
                        .get("minLength")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        .saturating_add(u64::try_from(number).expect("bounded sample number"))
                        .min(
                            schema
                                .get("maxLength")
                                .and_then(Value::as_u64)
                                .unwrap_or(1024),
                        )
                        .min(1024);
                    "x".repeat(usize::try_from(length).expect("bounded sample length"))
                }
            };
            Value::String(value)
        }
        _ => Value::Null,
    }
}

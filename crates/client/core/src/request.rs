//! Typed request construction and validation before a submission captures its bytes.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use boon::{Compiler, Draft, Schemas, UrlLoader};
use chrono::{DateTime, SecondsFormat};
use serde_json::{Map, Number, Value};

use crate::descriptor::FieldSchema;

/// One valid canonical input and its single-item HTTP body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltRequest {
    item: Value,
    body: Box<[u8]>,
}

impl BuiltRequest {
    /// The canonical input object without the outer envelope.
    pub const fn item(&self) -> &Value {
        &self.item
    }

    /// The captured outer-array bytes to send unchanged on a permitted retry.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Why a request cannot be built from the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErrorKind {
    RequiredField,
    NullNotAllowed,
    UnknownField,
    UnsupportedType,
    InvalidValue,
    Bounds,
    ClosedValue,
    InvalidSchema,
    SchemaViolation,
}

/// A request refusal with its field path and original parsing cause.
#[derive(Debug, Clone)]
pub struct RequestError {
    kind: RequestErrorKind,
    path: String,
    detail: String,
    source: Option<Arc<dyn Error + Send + Sync>>,
}

impl RequestError {
    pub const fn kind(&self) -> RequestErrorKind {
        self.kind
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.detail)
    }
}

impl Error for RequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Build one canonical outer-array request from declared fields and draft values.
///
/// # Errors
///
/// Returns the field that is missing, unsupported, malformed, or outside its bounds.
/// The final canonical body must also satisfy the served input schema.
pub fn build_request(
    fields: &[FieldSchema],
    item: &Value,
    input_schema: Option<&Value>,
) -> Result<BuiltRequest, RequestError> {
    supported_fields(fields)?;
    let item_schema = input_schema.and_then(|schema| schema.get("items"));
    let item = canonical_object(fields, item, item_schema, "$")?;
    let envelope = Value::Array(vec![item.clone()]);
    if let Some(schema) = input_schema {
        validate_schema(schema, &envelope)?;
    }
    Ok(BuiltRequest {
        item,
        body: wamn_execution_contract::canonical_json_bytes(&envelope).into_boxed_slice(),
    })
}

/// Validate a JSON value against a served schema without loading external resources.
///
/// # Errors
///
/// Returns a schema error or the instance path that violates the schema.
pub fn validate_schema(schema: &Value, value: &Value) -> Result<(), RequestError> {
    const SCHEMA_URI: &str = "mem://wamn-client-schema.json";
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler.use_loader(Box::new(LocalSchemaOnly));
    let mut schemas = Schemas::new();
    let invalid_schema = |error: boon::CompileError| {
        request_error(
            RequestErrorKind::InvalidSchema,
            "$schema",
            error.to_string(),
        )
    };
    compiler
        .add_resource(SCHEMA_URI, schema.clone())
        .map_err(invalid_schema)?;
    let index = compiler
        .compile(SCHEMA_URI, &mut schemas)
        .map_err(invalid_schema)?;
    schemas.validate(value, index).map_err(|error| {
        request_error(
            RequestErrorKind::SchemaViolation,
            format!("${}", error.instance_location),
            format!("{error:#}"),
        )
    })
}

struct LocalSchemaOnly;

impl UrlLoader for LocalSchemaOnly {
    fn load(&self, url: &str) -> Result<Value, Box<dyn Error>> {
        Err(Box::new(request_error(
            RequestErrorKind::InvalidSchema,
            "$schema",
            format!("external schema resource is not available: {url}"),
        )))
    }
}

/// Validate known result fields without rewriting their values.
///
/// Returns true when additive or unsupported fields require opaque display.
///
/// # Errors
///
/// Returns the known field that violates its declared presence, type, or bounds.
pub fn validate_result(fields: &[FieldSchema], value: &Value) -> Result<bool, RequestError> {
    validate_result_object(fields, value, "$")
}

fn validate_result_object(
    fields: &[FieldSchema],
    value: &Value,
    path: &str,
) -> Result<bool, RequestError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid(path, "expected a result object"))?;
    let mut opaque = object
        .keys()
        .any(|key| !fields.iter().any(|field| member_name(field) == key));
    for field in fields {
        let name = member_name(field);
        let member_path = format!("{path}.{name}");
        match object.get(name) {
            Some(value) => opaque |= validate_result_field(field, value, &member_path)?,
            None if field.required => {
                return Err(request_error(
                    RequestErrorKind::RequiredField,
                    member_path,
                    "the required result field is absent",
                ));
            }
            None => {}
        }
    }
    Ok(opaque)
}

fn validate_result_field(
    field: &FieldSchema,
    value: &Value,
    path: &str,
) -> Result<bool, RequestError> {
    if value.is_null() {
        return if field.field.nullable {
            Ok(!primitive(field.field.type_name) && field.children.is_empty())
        } else {
            Err(request_error(
                RequestErrorKind::NullNotAllowed,
                path,
                "the result field does not admit null",
            ))
        };
    }
    match field.field.type_name {
        "object" => {
            let opaque = validate_result_object(field.children, value, path)?;
            Ok(opaque || field.children.is_empty())
        }
        "array" => {
            let items = bounded_array(field, value, path)?;
            if field.children.is_empty() {
                return Ok(true);
            }
            let mut opaque = false;
            for (index, value) in items.iter().enumerate() {
                let path = format!("{path}[{index}]");
                opaque |= match array_element(field) {
                    Some(element) => validate_result_field(element, value, &path)?,
                    None => validate_result_object(field.children, value, &path)?,
                };
            }
            Ok(opaque)
        }
        name if primitive(name) => {
            let matches_wire = match name {
                "boolean" => value.is_boolean(),
                "int32" | "float64" => value.is_number(),
                _ => value.is_string(),
            };
            if !matches_wire {
                return Err(invalid(path, "the result field has the wrong JSON type"));
            }
            canonical_field(field, value, None, path)?;
            Ok(false)
        }
        _ => Ok(true),
    }
}

fn primitive(name: &str) -> bool {
    matches!(
        name,
        "text"
            | "string"
            | "uuid"
            | "timestamptz"
            | "numeric"
            | "int32"
            | "int64"
            | "float64"
            | "boolean"
    )
}

fn supported_fields(fields: &[FieldSchema]) -> Result<(), RequestError> {
    for field in fields {
        let supported = match field.field.type_name {
            "object" | "array" => !field.children.is_empty(),
            name => primitive(name),
        };
        if !supported {
            return Err(request_error(
                RequestErrorKind::UnsupportedType,
                field.field.path,
                format!(
                    "input type {:?} requires composition",
                    field.field.type_name
                ),
            ));
        }
        supported_fields(field.children)?;
    }
    Ok(())
}

fn canonical_object(
    fields: &[FieldSchema],
    input: &Value,
    schema: Option<&Value>,
    path: &str,
) -> Result<Value, RequestError> {
    let input = input
        .as_object()
        .ok_or_else(|| invalid(path, "expected an object"))?;
    for name in input.keys() {
        if !fields.iter().any(|field| member_name(field) == name) {
            return Err(request_error(
                RequestErrorKind::UnknownField,
                format!("{path}.{name}"),
                "the input field is not declared",
            ));
        }
    }
    let mut output = Map::new();
    for field in fields {
        let name = member_name(field);
        let member_path = format!("{path}.{name}");
        let Some(value) = input.get(name) else {
            if field.required {
                return Err(request_error(
                    RequestErrorKind::RequiredField,
                    member_path,
                    "the required input field is absent",
                ));
            }
            continue;
        };
        let property = schema
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.get(name));
        output.insert(
            name.to_owned(),
            canonical_field(field, value, property, &member_path)?,
        );
    }
    Ok(Value::Object(output))
}

fn canonical_field(
    field: &FieldSchema,
    value: &Value,
    schema: Option<&Value>,
    path: &str,
) -> Result<Value, RequestError> {
    if value.is_null() {
        return if field.field.nullable {
            Ok(Value::Null)
        } else {
            Err(request_error(
                RequestErrorKind::NullNotAllowed,
                path,
                "the input field does not admit null",
            ))
        };
    }
    let result = match field.field.type_name {
        "object" => canonical_object(field.children, value, schema, path)?,
        "array" => canonical_array(field, value, schema, path)?,
        "text" | "string" => Value::String(string(value, path)?.to_owned()),
        "uuid" => Value::String(
            uuid::Uuid::parse_str(string(value, path)?)
                .map_err(|source| caused(path, "expected a UUID", source))?
                .hyphenated()
                .to_string(),
        ),
        "timestamptz" => Value::String(
            DateTime::parse_from_rfc3339(string(value, path)?)
                .map_err(|source| caused(path, "expected an RFC3339 timestamp", source))?
                .to_utc()
                .to_rfc3339_opts(SecondsFormat::Micros, true),
        ),
        "numeric" => Value::String(canonical_numeric(string(value, path)?, path)?),
        "int32" | "int64" => canonical_integer(field.field.type_name, value, schema, path)?,
        "float64" => {
            let number = match value {
                Value::String(text) => text
                    .parse::<f64>()
                    .map_err(|source| caused(path, "expected a finite number", source))?,
                Value::Number(number) => number
                    .as_f64()
                    .ok_or_else(|| invalid(path, "expected a finite number"))?,
                _ => return Err(invalid(path, "expected a finite number")),
            };
            Value::Number(
                Number::from_f64(number)
                    .ok_or_else(|| invalid(path, "expected a finite number"))?,
            )
        }
        "boolean" => match value {
            Value::Bool(value) => Value::Bool(*value),
            Value::String(value) if value == "true" => Value::Bool(true),
            Value::String(value) if value == "false" => Value::Bool(false),
            _ => return Err(invalid(path, "expected true or false")),
        },
        _ => {
            return Err(request_error(
                RequestErrorKind::UnsupportedType,
                path,
                "input type requires composition",
            ));
        }
    };
    if !field.field.values.is_empty() {
        let literal = result
            .as_str()
            .map_or_else(|| result.to_string(), str::to_owned);
        if !field.field.values.contains(&literal.as_str()) {
            return Err(request_error(
                RequestErrorKind::ClosedValue,
                path,
                "the value is outside the declared choices",
            ));
        }
    }
    Ok(result)
}

fn canonical_array(
    field: &FieldSchema,
    value: &Value,
    schema: Option<&Value>,
    path: &str,
) -> Result<Value, RequestError> {
    let items = bounded_array(field, value, path)?;
    let item_schema = schema.and_then(|schema| schema.get("items"));
    let element = array_element(field);
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let path = format!("{path}[{index}]");
            match element {
                Some(child) => canonical_field(child, item, item_schema, &path),
                None => canonical_object(field.children, item, item_schema, &path),
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn bounded_array<'a>(
    field: &FieldSchema,
    value: &'a Value,
    path: &str,
) -> Result<&'a [Value], RequestError> {
    let items = value
        .as_array()
        .ok_or_else(|| invalid(path, "expected an array"))?;
    let count = u64::try_from(items.len())
        .map_err(|source| caused(path, "array length is too large", source))?;
    if field.minimum.is_some_and(|minimum| count < minimum)
        || field.maximum.is_some_and(|maximum| count > maximum)
    {
        return Err(request_error(
            RequestErrorKind::Bounds,
            path,
            format!("array length {count} is outside the declared bounds"),
        ));
    }
    Ok(items)
}

fn array_element(field: &FieldSchema) -> Option<&FieldSchema> {
    match field.children {
        [child]
            if child.field.path == field.field.path
                || child.field.path.strip_suffix("[]") == Some(field.field.path) =>
        {
            Some(child)
        }
        _ => None,
    }
}

fn canonical_integer(
    type_name: &str,
    value: &Value,
    schema: Option<&Value>,
    path: &str,
) -> Result<Value, RequestError> {
    let integer = match value {
        Value::String(value) => value
            .parse::<i64>()
            .map_err(|source| caused(path, "expected an integer", source))?,
        Value::Number(value) => value
            .as_i64()
            .ok_or_else(|| invalid(path, "expected an integer"))?,
        _ => return Err(invalid(path, "expected an integer")),
    };
    if type_name == "int32" {
        i32::try_from(integer)
            .map_err(|source| caused(path, "integer exceeds int32 bounds", source))?;
    }
    let wire_type = schema.and_then(|schema| schema.get("type"));
    let has_type = |expected: &str| {
        wire_type.is_some_and(|value| {
            value.as_str() == Some(expected)
                || value
                    .as_array()
                    .is_some_and(|types| types.iter().any(|value| value == expected))
        })
    };
    let string_wire = has_type("string") || (wire_type.is_none() && type_name == "int64");
    Ok(if string_wire {
        Value::String(integer.to_string())
    } else {
        Value::Number(integer.into())
    })
}

fn canonical_numeric(value: &str, path: &str) -> Result<String, RequestError> {
    let value = value.trim();
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let (integer, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(integer, fraction)| {
            (integer, Some(fraction))
        });
    if (integer.is_empty() && fraction.is_none_or(str::is_empty))
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|fraction| !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid(path, "expected decimal text without an exponent"));
    }
    let integer = integer.trim_start_matches('0');
    let nonzero = !integer.is_empty()
        || fraction.is_some_and(|fraction| fraction.bytes().any(|byte| byte != b'0'));
    let mut canonical = String::new();
    if negative && nonzero {
        canonical.push('-');
    }
    canonical.push_str(if integer.is_empty() { "0" } else { integer });
    if let Some(fraction) = fraction.filter(|fraction| !fraction.is_empty()) {
        canonical.push('.');
        canonical.push_str(fraction);
    }
    Ok(canonical)
}

fn member_name(field: &FieldSchema) -> &str {
    field.field.leaf().trim_end_matches("[]")
}

fn string<'a>(value: &'a Value, path: &str) -> Result<&'a str, RequestError> {
    value.as_str().ok_or_else(|| invalid(path, "expected text"))
}

fn invalid(path: &str, detail: &str) -> RequestError {
    request_error(RequestErrorKind::InvalidValue, path, detail)
}

fn caused(path: &str, detail: &str, source: impl Error + Send + Sync + 'static) -> RequestError {
    RequestError {
        source: Some(Arc::new(source)),
        ..invalid(path, detail)
    }
}

fn request_error(
    kind: RequestErrorKind,
    path: impl Into<String>,
    detail: impl Into<String>,
) -> RequestError {
    RequestError {
        kind,
        path: path.into(),
        detail: detail.into(),
        source: None,
    }
}

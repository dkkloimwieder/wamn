//! Convert the HTTP JSON envelope using the called export's WIT types.

use anyhow::{Context as _, bail, ensure};
use serde_json::{Map, Value, json};
use wash_runtime::wasmtime::component::{Type, Val};

/// Convert the published request envelope into the typed operation argument.
pub fn input(ty: &Type, value: &Value) -> anyhow::Result<Val> {
    if matches!(ty, Type::Record(_))
        && !is_json(ty)
        && let Value::Array(items) = value
    {
        let [item] = items.as_slice() else {
            bail!("a read takes one request element")
        };
        return decode(ty, Some(item));
    }
    decode(ty, Some(value))
}

fn is_json(ty: &Type) -> bool {
    matches!(ty, Type::Record(record) if record.fields().map(|field| (field.name, field.ty)).eq([("encoded-json", Type::String)]))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "WIT float32 rounds a JSON number to its declared width"
)]
fn decode(ty: &Type, value: Option<&Value>) -> anyhow::Result<Val> {
    if let Type::Option(option) = ty {
        return Ok(Val::Option(match value {
            None => None,
            Some(Value::Null) if !matches!(option.ty(), Type::Option(_)) => None,
            Some(value) => Some(Box::new(decode(&option.ty(), Some(value))?)),
        }));
    }
    let value = value.context("required member is absent")?;
    let invalid = || anyhow::anyhow!("JSON value does not fit {ty:?}");
    Ok(match ty {
        Type::Bool => Val::Bool(value.as_bool().context("expected boolean")?),
        Type::S8 => Val::S8(i8::try_from(value.as_i64().ok_or_else(invalid)?)?),
        Type::U8 => Val::U8(u8::try_from(value.as_u64().ok_or_else(invalid)?)?),
        Type::S16 => Val::S16(i16::try_from(value.as_i64().ok_or_else(invalid)?)?),
        Type::U16 => Val::U16(u16::try_from(value.as_u64().ok_or_else(invalid)?)?),
        Type::S32 => Val::S32(i32::try_from(value.as_i64().ok_or_else(invalid)?)?),
        Type::U32 => Val::U32(u32::try_from(value.as_u64().ok_or_else(invalid)?)?),
        Type::S64 => Val::S64(
            value
                .as_str()
                .context("s64 requires decimal text")?
                .parse()?,
        ),
        Type::U64 => Val::U64(
            value
                .as_str()
                .context("u64 requires decimal text")?
                .parse()?,
        ),
        Type::Float32 => Val::Float32(value.as_f64().ok_or_else(invalid)? as f32),
        Type::Float64 => Val::Float64(value.as_f64().ok_or_else(invalid)?),
        Type::Char => {
            let mut chars = value.as_str().ok_or_else(invalid)?.chars();
            let ch = chars.next().ok_or_else(invalid)?;
            ensure!(chars.next().is_none(), "char requires one character");
            Val::Char(ch)
        }
        Type::String => Val::String(value.as_str().context("expected string")?.to_owned()),
        Type::List(list) => Val::List(
            value
                .as_array()
                .context("expected list")?
                .iter()
                .map(|value| decode(&list.ty(), Some(value)))
                .collect::<anyhow::Result<_>>()?,
        ),
        Type::Record(_) if is_json(ty) => Val::Record(vec![(
            "encoded-json".into(),
            Val::String(value.to_string()),
        )]),
        Type::Record(record) => record_value(record, value, false, false)?,
        Type::Result(result) => result_value(result, value, false)?,
        Type::Tuple(tuple) => {
            let items = value.as_array().context("expected tuple")?;
            ensure!(items.len() == tuple.types().len(), "tuple length differs");
            Val::Tuple(
                tuple
                    .types()
                    .zip(items)
                    .map(|(ty, v)| decode(&ty, Some(v)))
                    .collect::<anyhow::Result<_>>()?,
            )
        }
        Type::Enum(enumeration) => {
            let name = value.as_str().ok_or_else(invalid)?;
            ensure!(
                enumeration.names().any(|candidate| candidate == name),
                "unknown enum value"
            );
            Val::Enum(name.to_owned())
        }
        Type::Variant(variant) => {
            let name = value
                .get("type")
                .and_then(Value::as_str)
                .context("variant requires type")?;
            let case = variant
                .cases()
                .find(|case| case.name == name)
                .context("unknown variant")?;
            Val::Variant(
                name.to_owned(),
                case.ty
                    .map(|ty| decode(&ty, value.get("value")).map(Box::new))
                    .transpose()?,
            )
        }
        Type::Flags(flags) => {
            let names = value
                .as_array()
                .context("flags require a list")?
                .iter()
                .map(|v| {
                    let name = v.as_str().context("flag requires string")?;
                    ensure!(
                        flags.names().any(|candidate| candidate == name),
                        "unknown flag"
                    );
                    Ok(name.to_owned())
                })
                .collect::<anyhow::Result<_>>()?;
            Val::Flags(names)
        }
        _ => bail!("WIT type cannot be carried by HTTP JSON: {ty:?}"),
    })
}

fn result_value(
    result: &wash_runtime::wasmtime::component::types::ResultType,
    value: &Value,
    correlation: bool,
) -> anyhow::Result<Val> {
    let decoded = result
        .ok()
        .map(|ty| match ty {
            Type::Record(record) => record_value(&record, value, correlation, true),
            Type::Enum(enumeration)
                if enumeration.names().eq(["request"])
                    && value.as_object().is_some_and(|object| {
                        object
                            .keys()
                            .all(|name| correlation && name == "request_id")
                    }) =>
            {
                Ok(Val::Enum("request".into()))
            }
            _ => decode(&ty, Some(value)),
        })
        .transpose();
    Ok(match decoded {
        Ok(value) => Val::Result(Ok(value.map(Box::new))),
        Err(error) => {
            let detail = result.err().context("request has no input refusal type")?;
            let invalid = json!({"field": "input"});
            let detail = decode(&detail, Some(&invalid)).with_context(|| error.to_string())?;
            Val::Result(Err(Some(Box::new(detail))))
        }
    })
}

fn record_value(
    record: &wash_runtime::wasmtime::component::types::Record,
    value: &Value,
    correlation: bool,
    envelope: bool,
) -> anyhow::Result<Val> {
    let object = value.as_object().context("expected object")?;
    let fields: Vec<_> = record.fields().collect();
    let item = fields
        .iter()
        .any(|field| field.name == "input" && matches!(field.ty, Type::Result(_)));
    let wrapped = envelope
        && object.contains_key("value")
        && !fields.iter().any(|field| field.name == "value");
    let body = if wrapped {
        object["value"]
            .as_object()
            .context("value must be an object")?
    } else {
        object
    };
    if !item {
        if wrapped {
            ensure!(
                object.keys().all(|key| key == "value"
                    || (correlation && key == "request_id")
                    || (key == "feed" && fields.iter().any(|field| field.name == "feed"))),
                "unknown request envelope member"
            );
        }
        for key in body.keys() {
            let known = fields
                .iter()
                .any(|field| field.name.replace('-', "_") == *key)
                || (correlation && key == "request_id")
                || (key == "filter"
                    && fields
                        .iter()
                        .any(|field| body["filter"].get(field.name.replace('-', "_")).is_some()))
                || (key == "sort" && fields.iter().any(|field| field.name == "sort-field"));
            ensure!(known, "unknown input member {key}");
        }
        if let Some(filter) = body.get("filter").and_then(Value::as_object) {
            ensure!(
                filter.keys().all(|name| fields
                    .iter()
                    .any(|field| field.name.replace('-', "_") == *name)),
                "unknown filter member"
            );
        }
    }
    let mut values = Vec::with_capacity(fields.len());
    for field in fields {
        let name = field.name.replace('-', "_");
        if item
            && field.name == "input"
            && let Type::Result(result) = &field.ty
        {
            values.push((
                field.name.to_owned(),
                result_value(
                    result,
                    value,
                    record.fields().any(|field| field.name == "request-id"),
                )?,
            ));
            continue;
        }
        let member = body
            .get(&name)
            .or_else(|| {
                (wrapped && field.name == "feed")
                    .then(|| object.get("feed"))
                    .flatten()
            })
            .or_else(|| body.get("filter").and_then(|filter| filter.get(&name)))
            .or_else(|| match field.name {
                "sort-field" => body.get("sort").and_then(|sort| sort.get("field")),
                "sort-direction" => body.get("sort").and_then(|sort| sort.get("direction")),
                _ => None,
            });
        values.push((
            field.name.to_owned(),
            decode(&field.ty, member).with_context(|| format!("input member {name}"))?,
        ));
    }
    Ok(Val::Record(values))
}

/// Convert a typed result into the public outcome envelope.
pub(super) fn output(value: &Val) -> anyhow::Result<Value> {
    match value {
        Val::Bool(v) => Ok(json!(v)),
        Val::S8(v) => Ok(json!(v)),
        Val::U8(v) => Ok(json!(v)),
        Val::S16(v) => Ok(json!(v)),
        Val::U16(v) => Ok(json!(v)),
        Val::S32(v) => Ok(json!(v)),
        Val::U32(v) => Ok(json!(v)),
        Val::S64(v) => Ok(json!(v.to_string())),
        Val::U64(v) => Ok(json!(v.to_string())),
        Val::Float32(v) => Ok(json!(v)),
        Val::Float64(v) => Ok(json!(v)),
        Val::Char(v) => Ok(json!(v.to_string())),
        Val::String(v) => Ok(json!(v)),
        Val::List(items) | Val::Tuple(items) => items.iter().map(output).collect(),
        Val::Option(value) => value.as_deref().map_or(Ok(Value::Null), output),
        Val::Record(fields) => {
            if let [(name, Val::String(encoded))] = fields.as_slice()
                && name == "encoded-json"
            {
                return serde_json::from_str(encoded).context("invalid JSON field result");
            }
            let change =
                fields
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .eq(["id", "row-version", "data"]);
            let mut object = Map::new();
            for (name, value) in fields {
                if name == "outcome" {
                    let value = output(value)?;
                    let Value::Object(value) = value else {
                        bail!("outcome must be a result")
                    };
                    object.extend(value);
                } else {
                    object.insert(name.replace('-', "_"), output(value)?);
                }
            }
            if change {
                let deleted = matches!(&fields[2].1, Val::Option(None));
                object.insert("deleted".into(), Value::Bool(deleted));
                if deleted {
                    object.remove("data");
                }
            }
            Ok(Value::Object(object))
        }
        Val::Result(Ok(value)) => {
            let value = match value.as_deref() {
                Some(Val::Record(fields)) if fields.len() == 1 && fields[0].0 == "value" => {
                    output(&fields[0].1)?
                }
                Some(value) => output(value)?,
                None => Value::Null,
            };
            Ok(json!({"value": value}))
        }
        Val::Result(Err(value)) => Ok(json!({"error": value.as_deref().map(error).transpose()?})),
        Val::Variant(name, value) if name == "page" || name == "changes" => {
            value.as_deref().map_or(Ok(Value::Null), output)
        }
        Val::Variant(name, value) => {
            Ok(json!({"type": name, "value": value.as_deref().map(output).transpose()?}))
        }
        Val::Enum(name) => Ok(json!(name)),
        Val::Flags(names) => Ok(json!(names)),
        _ => bail!("WIT value cannot be carried by HTTP JSON"),
    }
}

fn error(value: &Val) -> anyhow::Result<Value> {
    let Val::Variant(name, detail) = value else {
        bail!("error must be a WIT variant")
    };
    let mut encoded = Map::new();
    if let Some(detail) = detail.as_deref() {
        let Val::Record(fields) = detail else {
            bail!("error detail must be a record")
        };
        for (name, value) in fields {
            if !matches!(value, Val::Option(None)) {
                encoded.insert(name.replace('-', "_"), output(value)?);
            }
        }
    }
    Ok(json!({"code": name.replace('-', "_"), "detail": encoded}))
}

/// Convert the typed operation result to its HTTP outcome envelope.
pub fn emission(
    value: &Val,
) -> anyhow::Result<Result<super::node_types::Emission, super::node_types::NodeError>> {
    let Val::Result(result) = value else {
        bail!("operation must return result")
    };
    match result {
        Ok(Some(value)) => {
            if let Val::Record(fields) = value.as_ref()
                && let [
                    (payload_name, Val::String(payload)),
                    (port_name, Val::Option(port)),
                ] = fields.as_slice()
                && payload_name == "payload"
                && port_name == "port"
            {
                let port = match port.as_deref() {
                    Some(Val::String(port)) => Some(port.clone()),
                    None => None,
                    _ => bail!("emission port is not a string"),
                };
                return Ok(Ok(super::node_types::Emission {
                    payload: payload.clone(),
                    port,
                }));
            }
            let mut payload = output(value)?;
            if !payload.is_array() {
                payload = json!([payload]);
            }
            Ok(Ok(super::node_types::Emission {
                payload: payload.to_string(),
                port: None,
            }))
        }
        Ok(None) => bail!("operation returned no outcome"),
        Err(Some(value)) => {
            let Val::Variant(name, detail) = value.as_ref() else {
                bail!("operation error is not a variant")
            };
            let detail = detail
                .as_deref()
                .map(output)
                .transpose()?
                .unwrap_or(Value::Null);
            let error_detail = if name == "rate-limited" {
                &detail["detail"]
            } else {
                &detail
            };
            let refusal = super::node_types::ErrorDetail {
                code: error_detail
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: error_detail
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("operation refused")
                    .to_owned(),
            };
            Ok(Err(match name.as_str() {
                "invalid-input" => super::node_types::NodeError::InvalidInput(refusal),
                "retryable" => super::node_types::NodeError::Retryable(refusal),
                "rate-limited" => {
                    super::node_types::NodeError::RateLimited(super::node_types::RateLimitDetail {
                        detail: refusal,
                        retry_after_ms: detail
                            .get("retry_after_ms")
                            .filter(|value| !value.is_null())
                            .map(|value| {
                                value
                                    .as_str()
                                    .context("retry delay must be decimal text")?
                                    .parse::<u64>()
                                    .map_err(anyhow::Error::from)
                            })
                            .transpose()?,
                    })
                }
                "cancelled" => super::node_types::NodeError::Cancelled,
                _ => super::node_types::NodeError::Terminal(refusal),
            }))
        }
        Err(None) => bail!("operation returned an empty error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wash_runtime::wasmtime::component::{Component, types::ComponentItem};

    fn argument(definition: &str) -> Type {
        let engine = crate::build_engine(&[]).expect("build converter test engine");
        let mut resolve = wit_parser::Resolve::new();
        resolve
            .push_str(
                "node.wit",
                include_str!("../../../../../wit/wamn-node/package.wit"),
            )
            .expect("shared node types");
        let package = resolve.push_str("conversion.wit", &format!("package converter:tests; interface conversion {{ {definition} convert: func(input: input); }} world fixture {{ import conversion; }}")).expect("parse conversion contract");
        let world = resolve.select_world(&[package], None).unwrap();
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let bytes = wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        let component = Component::new(engine.inner(), bytes).expect("compile conversion contract");
        let ComponentItem::ComponentInstance(instance) = component
            .component_type()
            .imports(component.engine())
            .find(|(name, _)| name.starts_with("converter:tests/conversion"))
            .expect("conversion import")
            .1
            .ty
        else {
            panic!("conversion interface")
        };
        let ComponentItem::ComponentFunc(function) = instance
            .get_export(component.engine(), "convert")
            .expect("conversion function")
            .ty
        else {
            panic!("conversion function")
        };
        function.params().next().expect("one argument").1
    }

    #[test]
    fn scalars_and_omitted_null_and_value_follow_the_export_type() {
        let ty = argument(
            r"record input { revision: s32, quantity: s64, change: option<option<string>>, limit: option<u32> }",
        );
        for (member, expected) in [
            (None, Val::Option(None)),
            (
                Some(Value::Null),
                Val::Option(Some(Box::new(Val::Option(None)))),
            ),
            (
                Some(json!("approved")),
                Val::Option(Some(Box::new(Val::Option(Some(Box::new(Val::String(
                    "approved".into(),
                ))))))),
            ),
        ] {
            let mut value =
                json!({"revision": i32::MAX, "quantity": i64::MAX.to_string(), "limit": 100});
            if let Some(member) = member {
                value["change"] = member;
            }
            let Val::Record(fields) = input(&ty, &value).unwrap() else {
                panic!("record argument")
            };
            assert_eq!(fields[0].1, Val::S32(i32::MAX));
            assert_eq!(fields[1].1, Val::S64(i64::MAX));
            assert_eq!(fields[2].1, expected);
            assert_eq!(fields[3].1, Val::Option(Some(Box::new(Val::U32(100)))));
        }
        assert!(input(&ty, &json!({"revision": 2147483648_i64, "quantity":"1"})).is_err());
        assert!(input(&ty, &json!({"revision": "1", "quantity":"1"})).is_err());
        assert!(input(&ty, &json!({"revision": 1, "quantity":1})).is_err());
        assert_eq!(output(&Val::S32(i32::MAX)).unwrap(), json!(i32::MAX));
        assert_eq!(
            output(&Val::S64(i64::MAX)).unwrap(),
            json!(i64::MAX.to_string())
        );
    }

    #[test]
    fn invalid_elements_keep_their_correlation_and_refuse_before_execution() {
        let ty = argument(
            "record request { name: string } record invalid { field: string } record item { request-id: string, input: result<request, invalid> } type input = list<item>;",
        );
        let Val::List(items) = input(
            &ty,
            &json!([
                {"request_id":"ok","name":"value"},
                {"request_id":"unknown","name":"value","other":1},
                {"request_id":"null","name":null},
                {"request_id":"missing"},
                {"request_id":"wrapped","value":{"name":"value"}},
                {"request_id":"outer","value":{"name":"value"},"other":1}
            ]),
        )
        .unwrap() else {
            panic!("list argument")
        };
        for (item, succeeds) in items.iter().zip([true, false, false, false, true, false]) {
            let Val::Record(fields) = item else {
                panic!("request element")
            };
            assert!(matches!(fields[0].1, Val::String(_)));
            assert_eq!(matches!(fields[1].1, Val::Result(Ok(_))), succeeds);
        }
    }
    #[test]
    fn json_values_and_text_keep_their_distinct_http_shapes() {
        let ty = argument("use wamn:node/types@0.1.0.{json-value}; type input = json-value;");
        for value in [
            Value::Null,
            json!(true),
            json!(3),
            json!("text"),
            json!([null, 1]),
            json!({"nested":[true]}),
        ] {
            let encoded = input(&ty, &value).unwrap();
            let result = Val::Result(Ok(Some(Box::new(Val::List(vec![encoded])))));
            let emission = emission(&result).unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&emission.payload).unwrap(),
                json!([value])
            );
        }
        let text = argument("type input = string;");
        assert!(input(&text, &json!({"object": true})).is_err());
        assert_eq!(
            output(&input(&text, &json!("null")).unwrap()).unwrap(),
            json!("null")
        );
    }

    #[test]
    fn feed_changes_and_refusals_preserve_the_published_envelope() {
        let ty = argument(
            "record row { name: string } record input { id: string, row-version: s64, data: option<row> }",
        );
        for (data, deleted) in [(Value::Null, true), (json!({"name":"present"}), false)] {
            let change =
                output(&input(&ty, &json!({"id":"1", "row_version":"2", "data":data})).unwrap())
                    .unwrap();
            assert_eq!(change["deleted"], json!(deleted));
            assert_eq!(change.get("data").is_none(), deleted);
            if !deleted {
                assert_eq!(change["data"], data);
            }
        }
        let result = Val::Result(Err(Some(Box::new(Val::Variant("not-found".into(), None)))));
        assert_eq!(
            output(&result).unwrap(),
            json!({"error":{"code":"not_found","detail":{}}})
        );
        let detail = Val::Record(vec![
            ("constraint".into(), Val::String("unique_name".into())),
            ("field".into(), Val::Option(None)),
        ]);
        let refusal = Val::Variant("unique-violation".into(), Some(Box::new(detail)));
        let result = Val::Result(Err(Some(Box::new(refusal))));
        assert_eq!(
            output(&result).unwrap(),
            json!({"error":{"code":"unique_violation","detail":{"constraint":"unique_name"}}})
        );
    }
}

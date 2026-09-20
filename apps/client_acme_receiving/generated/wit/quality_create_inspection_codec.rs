// @generated from wamn.json and schema IR; do not edit.

use serde::Deserialize;
#[allow(unused_imports)]
use serde_json::{Map, Value, json};

#[allow(dead_code)]
fn canonical_uuid(value: &mut String) -> bool {
    let Ok(parsed) = uuid::Uuid::parse_str(value) else {
        return false;
    };
    *value = parsed.hyphenated().to_string();
    true
}

#[allow(dead_code)]
struct JsonInt64(i64);

impl<'de> Deserialize<'de> for JsonInt64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map(Self).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for JsonInt64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

#[derive(Debug)]
pub(crate) struct CodecError(&'static str);

impl CodecError {
    pub(crate) const fn context(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CodecError {}

fn invalid(field: &'static str) -> CodecError {
    CodecError(field)
}

#[derive(Deserialize)]
struct JsonNew {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    event: String,
    new: JsonNew,
}

#[allow(dead_code)]
pub(crate) fn decode(input: &str) -> Result<contract::CreateInspectionRequest, CodecError> {
    let request: JsonRequest = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input does not match its declared object"))?;
    Ok(contract::CreateInspectionRequest {
        event: request.event,
        new: contract::CreateInspectionNew { id: request.new.id },
    })
}

pub(crate) fn normalize(request: &mut contract::CreateInspectionRequest) -> Result<(), CodecError> {
    {
        let value = &mut request.event;
        if !["insert"].contains(&value.as_str()) {
            return Err(invalid("event"));
        }
    }
    {
        let value = &mut request.new.id;
        if !canonical_uuid(value) {
            return Err(invalid("new.id"));
        }
    }
    Ok(())
}

#[allow(unused_macros)]
macro_rules! export_operation {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $codec:ident) => {
        const _: () = {
            use $codec as __codec;
            use $contract as __contract;
            use $node as __node;

            fn invalid(error: __codec::CodecError) -> __node::NodeError {
                __node::NodeError::InvalidInput(__node::ErrorDetail {
                    message: error.context().to_owned(),
                    code: Some("invalid_input".to_owned()),
                })
            }

            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: __contract::CreateInspectionRequest,
                ) -> Result<__contract::CreateInspectionRequest, __node::NodeError> {
                    let mut state = $state;
                    let mut input = input;
                    __codec::normalize(&mut input).map_err(invalid)?;
                    $handler(&mut state, input).await
                }

                async fn run_json(
                    context: __node::NodeContext,
                    input: String,
                ) -> Result<__node::Emission, __node::NodeError> {
                    let raw = input;
                    let input = __codec::decode(&raw).map_err(invalid)?;
                    <Self as __contract::Guest>::run(context, input).await?;
                    Ok(__node::Emission {
                        payload: raw,
                        port: None,
                    })
                }
            }
        };
    };
}
#[allow(unused_imports)]
pub(crate) use export_operation;

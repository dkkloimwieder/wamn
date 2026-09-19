// @generated from operation declarations; do not edit.

use serde::Deserialize;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonInput {
    event: String,
    new: JsonNew,
}
#[derive(Deserialize)]
struct JsonNew {
    id: String,
}

#[allow(dead_code)]
pub(crate) fn decode(input: &str) -> Result<contract::CreateInspectionRequest, CodecError> {
    let value: JsonInput = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input does not match its declared object"))?;
    Ok(contract::CreateInspectionRequest {
        event: value.event,
        new: contract::CreateInspectionNew { id: value.new.id },
    })
}

#[allow(dead_code)]
pub(crate) fn encode(value: &contract::CreateInspectionRequest) -> String {
    serde_json::json!({"event": value.event, "new": {"id": value.new.id}}).to_string()
}
pub(crate) fn normalize(value: &mut contract::CreateInspectionRequest) -> Result<(), CodecError> {
    if value.event != "insert" {
        return Err(CodecError("event is outside its declared value domain"));
    }
    let parsed =
        uuid::Uuid::parse_str(&value.new.id).map_err(|_| CodecError("new.id is not a UUID"))?;
    value.new.id = parsed.hyphenated().to_string();
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

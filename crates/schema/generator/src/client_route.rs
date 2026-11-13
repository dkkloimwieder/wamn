//! Schemas of the final published operation.
use crate::client_ir::{ClientIrError, ClientIrErrorType};
use serde_json::Value;
use wamn_catalog::ServingAttachment;

#[derive(Debug, Default)]
pub(super) struct RouteEvidence {
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub partial_schema: Option<Value>,
    pub terminal_operation: Option<String>,
    pub direct: bool,
}

pub(super) fn evidence(
    attachment: &ServingAttachment,
    declarations: &Value,
) -> Result<RouteEvidence, ClientIrError> {
    let wamn_catalog::AttachmentTarget::Route {
        component,
        operation,
    } = &attachment.target;
    let declaration = declarations
        .get(component)
        .and_then(|component| component.get("operations"))
        .and_then(|operations| operations.get(operation));
    let output_schema = declaration
        .and_then(|operation| operation.get("output-ports"))
        .and_then(Value::as_array)
        .and_then(|ports| ports.iter().find(|port| port["name"] == "main"))
        .and_then(|port| port.get("schema"))
        .cloned();
    let input_schema = attachment.definition.get("input-schema").cloned();
    if input_schema
        .as_ref()
        .is_some_and(|schema| schema.get("$ref").is_some())
    {
        return Err(ClientIrError::new(
            ClientIrErrorType::MalformedContract,
            "publication must contain inline schemas",
        ));
    }
    Ok(RouteEvidence {
        input_schema,
        output_schema,
        partial_schema: declaration
            .and_then(|operation| operation.get("committed-result-schema"))
            .cloned(),
        terminal_operation: Some(operation.clone()),
        direct: attachment.registered_operation.as_deref() == Some(operation),
    })
}

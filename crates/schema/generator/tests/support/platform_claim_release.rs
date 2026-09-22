use std::collections::BTreeMap;

use serde_json::json;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr, ReplayIr, ResponseIr, RouteIr};

use super::{fixture, platform_claim};

pub(super) fn release(composed: bool) -> ClientContractIr {
    let package = fixture::generate_with(&fixture::catalog(), &platform_claim::manifest());
    let identity = "platform-fixture:widget/archive@1.0.0".to_owned();
    let response = if composed {
        ResponseIr {
            schema: Some(json!({"type": "object"})),
            partial_schema: Some(json!({
                "type": "object",
                "properties": {"committed_result": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"value": {
                        "type": "object",
                        "required": ["id"],
                        "properties": {"id": {"type": "string", "format": "uuid"}}
                    }}}
                }}
            })),
            result_class: Some("one".to_owned()),
            fields: ["stored.container", "stored.key"]
                .into_iter()
                .map(|path| FieldIr {
                    path: path.to_owned(),
                    type_name: "text".to_owned(),
                    nullable: false,
                    required: true,
                    revision: false,
                    children: Vec::new(),
                    minimum: None,
                    maximum: None,
                    values: Vec::new(),
                    label: None,
                    description: None,
                    references: None,
                })
                .collect(),
            errors: Vec::new(),
        }
    } else {
        ResponseIr::default()
    };
    let route = RouteIr {
        method: "POST".to_owned(),
        template: "/widget/archive".to_owned(),
        input_schema: None,
        terminal_operation: Some(if composed {
            "wamn:node/async-handler@0.1.0".to_owned()
        } else {
            identity.clone()
        }),
        direct: !composed,
        response,
        replay: (!composed).then_some(ReplayIr::Claim),
    };
    ClientContractIr::from_release_contracts(
        "platform_fixture",
        &fixture::contracts(&package),
        &BTreeMap::from([(identity, route)]),
    )
    .expect("claim release projects")
}

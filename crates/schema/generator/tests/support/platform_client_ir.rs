use super::fixture;
use std::collections::BTreeMap;
use wamn_compiler::client_ir::{ClientContractIr, ClientIrErrorType};

#[test]
fn private_operations_are_excluded_but_malformed_public_operations_refuse() {
    let mut operations = fixture::generate_fixture().operations().clone();
    let public = operations
        .values()
        .find(|description| description.operation.grant.is_some())
        .unwrap()
        .operation
        .operation
        .clone();
    let private = operations
        .values_mut()
        .find(|description| description.operation.operation == public)
        .unwrap();
    private.operation.visibility = Some("private".to_owned());
    private.operation.grant = None;
    let ir = ClientContractIr::from_operations("platform_fixture", &operations, &BTreeMap::new())
        .unwrap();
    assert!(
        ir.models
            .iter()
            .flat_map(|model| &model.operations)
            .all(|operation| operation.operation != public)
    );
    operations
        .values_mut()
        .find(|description| description.operation.operation == public)
        .unwrap()
        .operation
        .visibility = Some("public".to_owned());
    let refusal =
        ClientContractIr::from_operations("platform_fixture", &operations, &BTreeMap::new())
            .unwrap_err();
    assert_eq!(refusal.error_type(), ClientIrErrorType::MissingMember);
}

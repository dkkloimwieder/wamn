
use wamn_engine::native_routes::{OperationRefusalType, authorize_registered_operation};

#[test]
fn a_custom_read_refuses_a_caller_without_its_grant() {
    let operation = "grant-fixture:widget/list@1.0.0".parse().unwrap();
    let denial = authorize_registered_operation(None, Some(&operation))
        .expect_err("a caller without the grant is refused");
    assert_eq!(
        (denial.refusal_type(), denial.operation()),
        (OperationRefusalType::PermissionDenied, operation.as_str())
    );
}

use std::collections::HashSet;
use std::time::Duration;

use wamn_application::ingress::CredentialType;
use wamn_application::native_routes::{OperationRefusal, OperationRefusalType};
use wamn_component_policy::{AdmittedComponentOperation, ComponentPackageCoordinate};
use wamn_digest::{bytes_sha256, canonical_json_sha256};

use super::*;

const OPERATION: &str = "scale:device/record@1.0.0";

fn authority() -> EdgeAuthority {
    let fact = AdmittedComponent {
        data_change: false,
        target: ComponentPackageCoordinate {
            project_env_key: "org-a--dev--project".parse().unwrap(),
            package_id: "scale".parse().unwrap(),
            package_version: "1.0.0".parse().unwrap(),
        },
        component: "device".parse().unwrap(),
        interface_version: "0.1".parse().unwrap(),
        component_digest: bytes_sha256(b"device"),
        operations: BTreeMap::from([(
            OPERATION.parse().unwrap(),
            AdmittedComponentOperation {
                registered_operation: Some(OPERATION.parse().unwrap()),
                committed_result_schema: None,
                host_transaction: false,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
            },
        )]),
        imports: Vec::new(),
        imports_fingerprint: canonical_json_sha256(&serde_json::json!([])),
        effects: Vec::new(),
    };
    let authority = EdgeAuthority::default();
    authority.bind("bound".into(), Arc::new(fact));
    authority
}

fn caller(grants: HashSet<String>, role: &str) -> AuthenticatedCaller {
    AuthenticatedCaller::new(
        "record-http".parse().unwrap(),
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        CredentialType::Session,
        false,
        grants,
        Some(role.into()),
    )
}

fn operator() -> AuthenticatedCaller {
    caller(
        [wamn_package::sealed_operation_reference(OPERATION).to_owned()].into(),
        "operator",
    )
}

#[test]
fn an_expired_deadline_refuses() {
    let authority = authority();
    let caller = operator();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(60);
    authority
        .authorize("bound", OPERATION, Some(&caller), now, deadline)
        .unwrap();
    assert!(
        authority
            .authorize(
                "bound",
                OPERATION,
                Some(&caller),
                deadline + Duration::from_secs(1),
                deadline
            )
            .is_err()
    );
}

#[test]
fn an_unbound_component_refuses() {
    let authority = authority();
    let caller = operator();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(60);
    authority
        .authorize("bound", OPERATION, Some(&caller), now, deadline)
        .unwrap();
    assert!(
        authority
            .authorize("unbound", OPERATION, Some(&caller), now, deadline)
            .is_err()
    );
}

#[test]
fn a_caller_without_the_grant_refuses() {
    let authority = authority();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(60);
    authority
        .authorize("bound", OPERATION, Some(&operator()), now, deadline)
        .unwrap();
    let error = authority
        .authorize(
            "bound",
            OPERATION,
            Some(&caller(HashSet::new(), "viewer")),
            now,
            deadline,
        )
        .unwrap_err();
    let refusal = error.downcast_ref::<OperationRefusal>().unwrap();
    assert_eq!(
        refusal.refusal_type(),
        OperationRefusalType::PermissionDenied
    );
    assert_eq!(refusal.operation(), OPERATION);
}

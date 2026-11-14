use wamn_capability_authority::connection::WiringPosition;

use super::*;
use wamn_engine::native_routes::{OperationRefusalType, authorize_registered_operation};

#[test]
fn a_custom_read_refuses_a_caller_without_its_grant() {
    let tokens = wamn_control_provision::operation_grants::operation_grant_tokens(
        &wamn_fixture_package::manifest_bytes(),
    )
    .expect("parse the fixture manifest");
    let operation = tokens
        .get("platform-fixture:widget/list@2.1.1")
        .expect("the custom list read is its own operation grant");
    let denial = authorize_registered_operation(None, Some(operation))
        .expect_err("a caller without the grant is refused");
    assert_eq!(
        (denial.refusal_type(), denial.operation()),
        (OperationRefusalType::PermissionDenied, operation.as_str())
    );
}

#[test]
fn acquisition_binds_its_executing_principal_operation_and_application() {
    let causation = Causation {
        run: "registration:delivery:9".to_owned(),
        root: "attachment:delivery:1".to_owned(),
        depth: 2,
    };
    let acquisition = NodeAcquisition {
        identity: SessionIdentity {
            project_env_key: "org-a".to_owned(),
            project: Some("project-a".to_owned()),
            schema: Some("app".to_owned()),
            runner: Some("executor-a".to_owned()),
            role: Some("operator".to_owned()),
            user_id: Some("user-a".to_owned()),
            operation: None,
            application: None,
            release: Some(ReleaseIdentity::for_test(
                wamn_catalog::ManifestDigest::parse(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid manifest digest"),
            )),
        },
        invocation: ConnectionInvocation {
            origin: ConnectionOrigin {
                package_id: "platform_fixture_overlay".to_owned(),
                component_digest: "sha256:overlay".to_owned(),
                component: "overlay".to_owned(),
                interface_version: "1.0.0".to_owned(),
                operation: "platform-fixture-overlay:widget/record-batch@2.1.0".to_owned(),
            },
            entry: InvocationEntry::Wiring(WiringPosition {
                configuration: Default::default(),
                package_id: "org_workflow".to_owned(),
                wiring_hash: format!("sha256:{}", "a".repeat(64)),
                wiring_id: "record-batch".to_owned(),
                wiring_version: 1,
                node_id: "base-command".to_owned(),
                occurrence: 0,
            }),
            package_id: "platform_fixture_overlay".to_owned(),
            component_digest: "sha256:overlay".to_owned(),
            component: "overlay".to_owned(),
            operation: "platform-fixture-overlay:widget/record-batch@2.1.0".to_owned(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
        },
        causation: Some(causation),
        platform: Some(PlatformComponent::ApplyPackage),
    };

    let original = acquisition.clone();
    let executor = NodeAcquisition {
        platform: Some(PlatformComponent::Executor),
        ..acquisition
    };
    // A callerless call binds its platform principal.
    let platform_user = PlatformComponent::ApplyPackage.principal_id().to_string();
    assert_eq!(
        original.executing_principal(None).as_deref(),
        Some(platform_user.as_str())
    );
    assert_eq!(
        executor.executing_principal(None),
        Some(PlatformComponent::Executor.principal_id().to_string())
    );
    // The call binds the token of the operation that it executes and the
    // declared application that its policy names.
    assert_eq!(
        original.executing_identity(None, "receiving"),
        SessionIdentity {
            user_id: Some(platform_user),
            operation: Some("platform-fixture-overlay:widget/record-batch@2.1.0".to_owned()),
            application: Some("receiving".to_owned()),
            ..original.identity
        }
    );
}

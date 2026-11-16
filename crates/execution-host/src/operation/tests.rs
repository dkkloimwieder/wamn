use wamn_authority::connection::WiringPosition;

use super::*;
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

#[test]
fn acquisition_binds_its_executing_principal_operation_and_application() {
    let causation = Causation {
        run: "registration:delivery:9".to_owned(),
        root: "attachment:delivery:1".to_owned(),
        depth: 2,
    };
    let acquisition = NodeAcquisition {
        identity: SessionIdentity {
            project_env_key: Some("org-a--dev--project".parse().unwrap()),
            project: Some("project-a".parse().unwrap()),
            schema: Some("app".to_owned()),
            runner: Some("executor-a".to_owned()),
            role: Some("operator".to_owned()),
            user_id: Some("00000000-0000-4000-8000-000000000001".parse().unwrap()),
            operation: None,
            application: None,
            release: Some(ReleaseIdentity::for_test(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .parse::<wamn_digest::Digest>()
                    .unwrap(),
            )),
        },
        invocation: ConnectionInvocation {
            origin: ConnectionOrigin {
                package_id: "platform_fixture_overlay".parse().unwrap(),
                component_digest: wamn_digest::bytes_sha256(b"overlay"),
                component: "overlay".parse().unwrap(),
                interface_version: "1.0.0".parse().unwrap(),
                operation: "platform-fixture-overlay:widget/record-batch@2.1.0"
                    .parse()
                    .unwrap(),
            },
            entry: InvocationEntry::Wiring(WiringPosition {
                configuration: Default::default(),
                application: "org_workflow".parse().unwrap(),
                wiring_hash: (format!("sha256:{}", "a".repeat(64)))
                    .parse::<wamn_digest::Digest>()
                    .unwrap(),
                wiring_id: "record-batch".parse().unwrap(),
                wiring_version: 1,
                node_id: "base-command".parse().unwrap(),
                occurrence: 0,
            }),
            package_id: "platform_fixture_overlay".parse().unwrap(),
            component_digest: wamn_digest::bytes_sha256(b"overlay"),
            component: "overlay".parse().unwrap(),
            operation: "platform-fixture-overlay:widget/record-batch@2.1.0"
                .parse()
                .unwrap(),
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
    let platform_user = PlatformComponent::ApplyPackage.principal_id();
    assert_eq!(original.executing_principal(None), Some(platform_user));
    assert_eq!(
        executor.executing_principal(None),
        Some(PlatformComponent::Executor.principal_id())
    );
    // The call binds the token of the operation that it executes and the
    // declared application that its policy names.
    assert_eq!(
        original.executing_identity(None, &"receiving".parse().expect("valid name")),
        SessionIdentity {
            user_id: Some(platform_user),
            operation: Some(
                "platform-fixture-overlay:widget/record-batch@2.1.0"
                    .parse()
                    .unwrap()
            ),
            application: Some("receiving".parse().expect("valid name")),
            ..original.identity
        }
    );
}

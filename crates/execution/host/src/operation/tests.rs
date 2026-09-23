use wamn_catalog::{AdmittedComponentEffect, AdmittedComponentOperation, ComponentPackageScope};
use wamn_runtime::plugins::connection_http::WiringPosition;

use super::*;

fn component_with_operations(
    operations: BTreeMap<String, AdmittedComponentOperation>,
) -> AdmittedComponent {
    AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "orders".to_owned(),
            package_version: "1.0.0".to_owned(),
        },
        component: "orders".to_owned(),
        interface_version: "0.1.0".to_owned(),
        operations,
        component_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        imports: Vec::new(),
        imports_fingerprint:
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        effects: Vec::new(),
    }
}

fn operation_with_statements(
    statements: BTreeMap<String, wamn_catalog::ComponentSqlStatement>,
) -> AdmittedComponentOperation {
    AdmittedComponentOperation {
        pre_commit: None,
        registered_operation: None,
        fresh_only: false,
        committed_result_schema: None,
        dependencies: Vec::new(),
        input_ports: Vec::new(),
        output_ports: Vec::new(),
        parameters: Vec::new(),
        statements,
    }
}

fn statement_plugin() -> Arc<WamnPostgres> {
    Arc::new(WamnPostgres::with_provider(Arc::new(
        wamn_runtime::plugins::wamn_postgres::StaticCredentialProvider::new(HashMap::new(), None),
    )))
}

#[test]
fn node_deadline_is_nonzero_and_host_bounded() {
    let ceiling = max_host_call_ms();

    assert_eq!(bounded_node_deadline_ms(None), ceiling);
    assert_eq!(bounded_node_deadline_ms(Some(0)), 1);
    assert_eq!(bounded_node_deadline_ms(Some(ceiling + 1)), ceiling);
    assert_eq!(bounded_node_deadline_ms(Some(17)), 17);
}

#[test]
fn admitted_statement_types_lower_exhaustively_to_the_runtime_vocabulary() {
    let cases = [
        (ComponentSqlValueType::Boolean, StatementValueType::Boolean),
        (ComponentSqlValueType::Int32, StatementValueType::Int32),
        (ComponentSqlValueType::Int64, StatementValueType::Int64),
        (ComponentSqlValueType::Float64, StatementValueType::Float64),
        (ComponentSqlValueType::Text, StatementValueType::Text),
        (ComponentSqlValueType::Bytes, StatementValueType::Bytes),
        (ComponentSqlValueType::Numeric, StatementValueType::Numeric),
        (
            ComponentSqlValueType::Timestamptz,
            StatementValueType::Timestamptz,
        ),
        (ComponentSqlValueType::Json, StatementValueType::Json),
        (ComponentSqlValueType::Uuid, StatementValueType::Uuid),
    ];

    for (index, (admitted, runtime)) in cases.into_iter().enumerate() {
        let field = ComponentSqlField {
            name: format!("field-{index}"),
            value_type: admitted,
            nullable: index % 2 == 0,
        };
        assert_eq!(
            lower_statement_field(&field),
            StatementField {
                value_type: runtime,
                nullable: field.nullable,
            }
        );
    }
}

#[test]
fn partial_statement_binding_failure_cleans_earlier_operations() {
    let postgres = statement_plugin();
    let invalid_statement = wamn_catalog::ComponentSqlStatement {
        name: "lookup".to_owned(),
        path: "sql/lookup.sql".to_owned(),
        sql: "SELECT 1".to_owned(),
        binds: Vec::new(),
        columns: Vec::new(),
        transactional: false,
    };
    let component = component_with_operations(BTreeMap::from([
        (
            "a-empty".to_owned(),
            operation_with_statements(BTreeMap::new()),
        ),
        (
            "b-invalid".to_owned(),
            operation_with_statements(BTreeMap::from([(
                "sha256:not-the-statement-digest".to_owned(),
                invalid_statement,
            )])),
        ),
    ]));

    // The refusal moved to preparation: a digest that does not name its
    // SQL is refused before any scope exists to bind it under, so nothing
    // partial can ever have been bound.
    let error = prepare_statement_sets(&component)
        .expect_err("a digest that does not name its SQL is refused at preparation");
    assert!(
        format!("{error:#}").contains("statement-digest-mismatch"),
        "the refusal names the mismatch: {error:#}"
    );
    assert!(
        postgres
            .activate_statement_operation("scope-partial", "a-empty")
            .is_err(),
        "no operation of a refused digest is ever bound"
    );
}

#[test]
fn every_registered_invocation_requires_the_exact_operation_grant() {
    let operation = "orders:widget/get@7.0.0";

    assert!(authorize_registered_operation(None, None, false).is_ok());
    let denial = authorize_registered_operation(None, Some(operation), false)
        .expect_err("a registered invocation without an originating caller is denied");
    assert_eq!(denial.operation(), operation);
}

/// Spec test 9. The host refuses the fixture's custom list read to a caller
/// without its operation grant.
#[test]
fn a_custom_read_refuses_a_caller_without_its_grant() {
    let tokens = wamn_control_provision::operation_grants::operation_grant_tokens(
        &wamn_fixture_package::manifest_bytes(),
    )
    .expect("parse the fixture manifest");
    let operation = tokens
        .get("platform-fixture:widget/list@1.0.0")
        .expect("the custom list read is its own operation grant");
    let denial = authorize_registered_operation(None, Some(operation), false)
        .expect_err("a caller without the grant is refused");
    assert_eq!(
        (denial.kind(), denial.operation()),
        (OperationRefusalKind::PermissionDenied, operation.as_str())
    );
}

#[test]
fn nested_acquisition_preserves_causation_and_root_origin() {
    let causation = Causation {
        run: "registration:delivery:9".to_owned(),
        root: "attachment:delivery:1".to_owned(),
        depth: 2,
    };
    let acquisition = NodeAcquisition {
        claims: SessionClaims {
            tenant: "tenant-a".to_owned(),
            project: Some("project-a".to_owned()),
            schema: Some("app".to_owned()),
            runner: Some("executor-a".to_owned()),
            role: Some("operator".to_owned()),
            user_id: Some("user-a".to_owned()),
            operation: None,
            release: Some(ReleaseIdentity {
                effective_release_id: 7,
                manifest_digest: wamn_catalog::ManifestDigest::parse(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid manifest digest"),
            }),
        },
        invocation: ConnectionInvocation {
            origin: ConnectionOrigin {
                package_id: "platform_fixture_overlay".to_owned(),
                component_digest: "sha256:overlay".to_owned(),
                component: "overlay".to_owned(),
                interface_version: "1.0.0".to_owned(),
                operation: "platform-fixture-overlay:widget/record-batch@1.0.0".to_owned(),
            },
            entry: InvocationEntry::Wiring(WiringPosition {
                package_id: "org_workflow".to_owned(),
                wiring_id: "record-batch".to_owned(),
                wiring_version: 1,
                node_id: "base-command".to_owned(),
                occurrence: 0,
            }),
            package_id: "platform_fixture_overlay".to_owned(),
            component_digest: "sha256:overlay".to_owned(),
            component: "overlay".to_owned(),
            operation: "platform-fixture-overlay:widget/record-batch@1.0.0".to_owned(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
        },
        causation: Some(causation.clone()),
        platform: Some(PlatformComponent::Materializer),
    };

    let original = acquisition.clone();
    let mut target = component_with_operations(BTreeMap::new());
    target.scope.package_id = "platform_fixture".to_owned();
    target.component = "fixture".to_owned();
    target.component_digest = "sha256:base".to_owned();
    let executor = NodeAcquisition {
        platform: Some(PlatformComponent::Executor),
        ..acquisition.clone()
    }
    .retarget(&target, "platform-fixture:widget/record-batch@1.0.0");
    let child = acquisition.retarget(&target, "platform-fixture:widget/record-batch@1.0.0");
    // A callerless parent and its nested call bind the same platform principal.
    let materializer = PlatformComponent::Materializer.principal_id().to_string();
    assert_eq!(
        original.executing_principal(None).as_deref(),
        Some(materializer.as_str())
    );
    assert_eq!(
        child.executing_principal(None).as_deref(),
        Some(materializer.as_str())
    );
    assert_eq!(
        executor.executing_principal(None),
        Some(PlatformComponent::Executor.principal_id().to_string())
    );
    // The parent binds its own operation token and the nested call binds
    // the token of the operation that it executes.
    assert_eq!(
        original.executing_claims(None).operation.as_deref(),
        Some("platform-fixture-overlay:widget/record-batch@1.0.0")
    );
    assert_eq!(
        child.executing_claims(None),
        SessionClaims {
            user_id: Some(materializer.clone()),
            operation: Some("platform-fixture:widget/record-batch@1.0.0".to_owned()),
            ..original.claims.clone()
        }
    );
    assert_eq!(child.causation.as_ref(), Some(&causation));
    assert_eq!(child.invocation.package_id, "platform_fixture");
    assert_eq!(child.invocation.component_digest, "sha256:base");
    assert_eq!(
        child
            .invocation
            .entry
            .wiring()
            .expect("a wiring entry")
            .wiring_id,
        "record-batch"
    );
    // The child raises its effects under ITS OWN component and operation.
    // The overlay's pair belongs to the caller (`wamn-b2m6.7`).
    assert_eq!(child.invocation.component, "fixture");
    assert_eq!(
        child.invocation.operation,
        "platform-fixture:widget/record-batch@1.0.0"
    );
    assert_eq!(child.claims, original.claims);
    assert_eq!(
        child.invocation,
        ConnectionInvocation {
            package_id: "platform_fixture".to_owned(),
            component_digest: "sha256:base".to_owned(),
            component: "fixture".to_owned(),
            operation: "platform-fixture:widget/record-batch@1.0.0".to_owned(),
            ..original.invocation.clone()
        }
    );

    target.scope.package_id = "wamn_inventory".to_owned();
    target.component = "inventory".to_owned();
    target.component_digest = "sha256:inventory".to_owned();
    let grandchild = child.retarget(&target, "wamn-inventory:inventory/receive@1.0.0");
    assert_eq!(grandchild.claims, original.claims);
    assert_eq!(grandchild.causation, original.causation);
    assert_eq!(
        grandchild.invocation,
        ConnectionInvocation {
            package_id: "wamn_inventory".to_owned(),
            component_digest: "sha256:inventory".to_owned(),
            component: "inventory".to_owned(),
            operation: "wamn-inventory:inventory/receive@1.0.0".to_owned(),
            ..original.invocation
        }
    );
}

#[test]
fn shared_nested_import_retains_each_declaring_export() {
    let operation = "platform-fixture:widget/record-batch@1.0.0";
    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let dependency = ComponentOperationDependency {
        participant: None,
        package: "platform_fixture".to_owned(),
        version: "1.0.0".to_owned(),
        digest: digest.to_owned(),
        operation: operation.to_owned(),
    };
    let target = AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "platform_fixture".to_owned(),
            package_version: "1.0.0".to_owned(),
        },
        component: "fixture".to_owned(),
        interface_version: "0.1.0".to_owned(),
        operations: BTreeMap::from([(
            operation.to_owned(),
            AdmittedComponentOperation {
                pre_commit: None,
                registered_operation: Some(operation.to_owned()),
                fresh_only: false,
                committed_result_schema: None,
                dependencies: Vec::new(),
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: digest.to_owned(),
        imports: Vec::new(),
        imports_fingerprint:
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        effects: Vec::<AdmittedComponentEffect>::new(),
    };

    let declaration = AdmittedComponentOperation {
        pre_commit: None,
        registered_operation: None,
        fresh_only: false,
        committed_result_schema: None,
        dependencies: vec![dependency],
        input_ports: Vec::new(),
        output_ports: Vec::new(),
        parameters: Vec::new(),
        statements: BTreeMap::new(),
    };
    let mut caller = target;
    caller.operations = BTreeMap::from([
        (
            "platform-fixture-overlay:one/run@3.0.0".to_owned(),
            declaration.clone(),
        ),
        (
            "platform-fixture-overlay:two/run@3.0.0".to_owned(),
            declaration,
        ),
    ]);
    let links = nested_operation_links(&caller).expect("matching pins may share one import");
    let (_, owners) = links
        .get(operation)
        .expect("the exact dependency import is present once");
    assert_eq!(links.len(), 1);
    assert_eq!(owners.len(), 2);
}

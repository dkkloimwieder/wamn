use wamn_catalog::{AdmittedComponentOperation, ComponentPackageScope};
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
    let served = wamn_catalog::ServingComponent::project(&component, &|_| None)
        .expect("the fixture projects");
    let error = prepare_statement_sets(&served.operations)
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
fn acquisition_binds_its_executing_principal_and_operation() {
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
    let executor = NodeAcquisition {
        platform: Some(PlatformComponent::Executor),
        ..acquisition
    };
    // A callerless call binds its platform principal.
    let materializer = PlatformComponent::Materializer.principal_id().to_string();
    assert_eq!(
        original.executing_principal(None).as_deref(),
        Some(materializer.as_str())
    );
    assert_eq!(
        executor.executing_principal(None),
        Some(PlatformComponent::Executor.principal_id().to_string())
    );
    // The call binds the token of the operation that it executes.
    assert_eq!(
        original.executing_claims(None),
        SessionClaims {
            user_id: Some(materializer),
            operation: Some("platform-fixture-overlay:widget/record-batch@1.0.0".to_owned()),
            ..original.claims.clone()
        }
    );
}

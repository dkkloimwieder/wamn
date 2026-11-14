//! Run the pull's checkpoint node through the native host and its application login.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use sqlx::Row as _;
use wamn_engine::artifact_source::LocalComponentSource;
use wamn_engine::operation::native_workload::NativeComponent;
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_execution_host::{NativeFacts, NodeAcquisition, OperationHost, OperationHostConfig};
use wamn_package_control::component_admission::ComponentAdmission;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, InvocationEntry, WiringPosition,
};
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres,
};

const PROJECT_ENV_KEY: &str = "rest--test--publication";

pub async fn invoke(
    roots: &[PathBuf],
    manifest: &wamn_catalog::ServingManifest,
    admissions: &[ComponentAdmission],
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut admin = wamn_postgres::connect(database.url()).await?;

    let guest_url = super::consumer_live::install(&mut admin, &database, roots, manifest).await?;
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "NetSuite checkpoint live",
        ReleaseTarget::new(PROJECT_ENV_KEY, "test")
            .with_applications(super::consumer_live::declared("erp", manifest)),
    )?);
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            HashMap::from([(
                "test".into(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::GuestSql, &guest_url)
                        .with_class(AuthorityClass::ExecutorPlatform, database.url()),
                    guest_pool_max_size: 2,
                    platform_pool_max_size: 2,
                    wait_timeout_ms: 5000,
                    statement_timeout_ms: 10000,
                    row_limit: 1000,
                },
            )]),
            None,
        ),
    )));
    let host = OperationHost::new(
        Arc::new(wamn_engine::build_engine(&[])?),
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
            Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
            wamn_broker::Publisher::default(),
            Arc::new(ConnectionCredentials::empty()),
            postgres,
            PROJECT_ENV_KEY,
            "test",
            "test",
        )),
        Arc::new(WamnLogging::new(&WamnLoggingConfig::default())?),
        Arc::from([]),
        release,
        Arc::new(LocalComponentSource::new(PathBuf::new())),
        OperationHostConfig {
            project: "test".into(),
            schema: Some("wamn_integration".into()),
            owner_prefix: "netsuite-checkpoint-live".into(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )?;
    let component = admissions
        .iter()
        .find(|admission| admission.component() == "netsuite")
        .unwrap()
        .facts();
    let application = host
        .load_application(vec![NativeComponent {
            fact: component.clone(),
            bytes: std::fs::read(
                wamn_compiler::output_root(&super::support::repository().join("apps/netsuite"))
                    .join("components/netsuite.wasm"),
            )?,
        }])
        .await?;
    let wiring: wamn_catalog::WiringDocument = serde_json::from_slice(&std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/netsuite/tests/workflows/netsuite_pull.json"),
    )?)?;
    // The NetSuite pull is an environment workflow
    // (docs/architecture/execution.md#workflows), authored, enabled and
    // admitted at one due moment. Its run executes as the `workflow` user with
    // its execution role, which holds exactly the operation of the graph that
    // needs a grant: the consumer command of another package of the release.
    let workflow_host =
        super::consumer_live::workflow_host(&mut admin, &database, manifest).await?;
    // The application `erp` holds the three packages of the release. Its name
    // is no package (docs/architecture/execution.md#workflows).
    let run = super::consumer_live::schedule_run(
        &database,
        &workflow_host,
        manifest,
        "erp",
        &wiring,
        "netsuite-pull",
        &["rest-consumer-fixture:item/record"],
        "2026-10-22T12:00:00Z",
    )
    .await?;
    // A second role the `workflow` user holds, without the consumer command.
    // It is granted before the first grant read, which the host's auth memory
    // keeps.
    super::consumer_live::execution_role(&database, "netsuite-pull-without-record", &[]).await?;
    let caller =
        super::consumer_live::run_caller(&mut admin, &workflow_host, &run, "netsuite-pull").await?;
    // The router's test before the `record` node: the node's registered
    // operation against the grants the run's role holds now. A role without
    // the consumer command refuses the node.
    let record = &wiring.nodes["record"];
    let registered = wamn_catalog::resolve_workflow_components(
        PROJECT_ENV_KEY,
        &super::consumer_live::declared("erp", manifest)["erp"].packages,
        manifest,
        &wiring,
    )?["record"]
        .operations[&record.operation]
        .registered_operation
        .clone();
    assert_eq!(
        registered.as_deref(),
        Some("rest-consumer-fixture:item/record@0.1.1")
    );
    wamn_engine::router_delivery::authorize_registered_operation(
        Some(&caller),
        registered.as_deref(),
    )?;
    let without = workflow_host
        .workflow_node_caller(
            "test",
            PROJECT_ENV_KEY,
            &wamn_identity_protocol::workflow_user_id().to_string(),
            "netsuite-pull-without-record",
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("the workflow user holds the role without the command"))?;
    let refusal = wamn_engine::router_delivery::authorize_registered_operation(
        Some(&without),
        registered.as_deref(),
    )
    .expect_err("a role without the consumer command refuses the record node");
    assert_eq!(
        refusal.refusal_type(),
        wamn_engine::router_delivery::OperationRefusalType::PermissionDenied
    );
    let write = &wiring.nodes["write"];
    let intents = host.call_intents(component, &write.operation)?;
    // The command stores the due time in its normalized timestamptz form.
    for (due, stored) in [
        ("2026-10-22T12:00:00Z", "2026-10-22T12:00:00.000000Z"),
        ("2026-10-22T12:01:00Z", "2026-10-22T12:01:00.000000Z"),
    ] {
        let input = json!([{"request_id": due, "value": {"due": due}}]);
        let result = invoke_operation(
            &host,
            OperationCall {
                closure: OperationClosure::Candidate(&application),
                component,
                operation: &write.operation,
                context: node_types::NodeContext {
                    wiring_id: wiring.wiring_id.clone(),
                    wiring_version: wiring.version,
                    node_id: "write".into(),
                    delivery_id: due.into(),
                    input_port: None,
                    occurrence: 0,
                    traceparent: None,
                    tracestate: None,
                    deadline_ms: Some(10000),
                    config: serde_json::to_string(&write.params)?,
                },
                input: &input,
                deadline_ms: 10000,
                facts: NativeFacts::entry(
                    NodeAcquisition {
                        identity: host.identity(PROJECT_ENV_KEY, Some(host.release_identity())),
                        invocation: wamn_execution_host::component_invocation(
                            component,
                            &write.operation,
                            InvocationEntry::Wiring(WiringPosition {
                                package_id: "netsuite".into(),
                                wiring_hash: wiring.wiring_hash().to_string(),
                                wiring_id: wiring.wiring_id.clone(),
                                wiring_version: wiring.version,
                                node_id: "write".into(),
                                occurrence: 0,
                            }),
                            ConnectionExecutionClosure::Released,
                            None,
                        ),
                        causation: None,
                        platform: None,
                    },
                    Some(caller.clone()),
                ),
            },
            intents
                .as_ref()
                .map(wamn_execution_host::CallIntents::context),
        )
        .await?;
        let emission =
            result.map_err(|error| anyhow::anyhow!("checkpoint write failed: {error:?}"))?;
        let payload: serde_json::Value = serde_json::from_str(&emission.payload)?;
        assert_eq!(payload[0]["value"]["due"], stored, "{payload}");
        let checkpoint: serde_json::Value =
            sqlx::query("SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'")
                .fetch_one(&mut admin)
                .await?
                .get(0);
        assert_eq!(checkpoint, json!(stored));
    }
    application.workload.unbind_all_plugins().await?;
    Ok(())
}

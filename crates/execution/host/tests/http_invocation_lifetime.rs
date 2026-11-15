//! Claimed calls stop at their invocation deadline or cancellation.
#![cfg(feature = "test-util")]
use sqlx::Connection as _;
use std::sync::Arc;
use std::time::Duration;
use wamn_authority::connection::InvocationEntry;
use wamn_http::test_util::{Fixture, invocation};
#[tokio::test]
async fn claimed_requests_stop_at_the_invocation_deadline_or_cancellation() {
    use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
    use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation};
    use wamn_execution_host::{NativeFacts, NodeAcquisition, OperationHost, OperationHostConfig};
    use wamn_sql::write_log::{Claim, WriteLogStore};
    use wash_runtime::plugin::wasi_logging::TracingLogger;

    let _lock = wamn_test_postgres::lock();
    for cancel in [false, true] {
        let mut endpoint = invocation::Endpoint::start().await;
        let mut fixture = Fixture::start(Vec::new(), vec![]).await;
        fixture.base = endpoint.base.clone();
        let operation = invocation().operation;
        let bytes = invocation::guest(&operation);
        let plugin = fixture
            .plugin_with_component(
                &serde_json::json!({}),
                "http_basic",
                r#"{"username":"fixture","password":"old"}"#,
                Some(&bytes),
            )
            .await;
        let mut observer = wamn_postgres::connect(plugin.database.as_str())
            .await
            .unwrap();
        let mut transaction = observer.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::platform_principals_sql("platform.example.test").unwrap(),
        ))
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let component = plugin.release.components().first().unwrap();
        std::fs::write(
            local_component_path(directory.path(), &component.component_digest).unwrap(),
            bytes,
        )
        .unwrap();
        let host = Arc::new(
            OperationHost::new(
                Arc::new(wamn_engine::build_engine(&[]).unwrap()),
                Arc::clone(&plugin.postgres),
                Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
                    &plugin.postgres,
                ))),
                Arc::clone(&endpoint.native_transport),
                Arc::clone(&plugin.facts),
                Arc::new(TracingLogger::default()),
                Arc::clone(&plugin.allowed_hosts),
                Arc::clone(&plugin.release),
                Arc::new(LocalComponentSource::new(directory.path().into())),
                OperationHostConfig {
                    project: "project-a".into(),
                    schema: None,
                    owner_prefix: "request-lifetime".into(),
                    warm_reuse: Default::default(),
                },
            )
            .unwrap(),
        );
        let components = Arc::clone(plugin.release.components());
        host.prepare_released(&components).await.unwrap();
        let mut connection = invocation();
        if let InvocationEntry::Wiring(position) = &mut connection.entry {
            position.package_id = "app".into();
        }
        connection.component_digest = component.component_digest.clone();
        connection.origin.component_digest = component.component_digest.clone();
        let facts = NativeFacts::entry(
            NodeAcquisition {
                identity: host.identity("acme--dev--project-a", Some(host.release_identity())),
                invocation: connection,
                causation: None,
                platform: Some(wamn_control_provision::PlatformComponent::Executor),
            },
            None,
        );
        let store = WriteLogStore::new(
            Arc::clone(&plugin.postgres),
            "project-a".into(),
            "acme--dev--project-a".into(),
        );
        let called = Arc::clone(&host);
        let mut call = tokio::spawn(async move {
            let component = components.first().unwrap();
            invoke_operation(
                called.as_ref(),
                OperationCall {
                    closure: OperationClosure::Released(&components),
                    component,
                    operation: &operation,
                    context: wamn_engine::operation::node_types::NodeContext {
                        wiring_id: "orders".into(),
                        wiring_version: 3,
                        node_id: "notify".into(),
                        delivery_id: "bounded".into(),
                        input_port: None,
                        occurrence: 2,
                        traceparent: None,
                        tracestate: None,
                        deadline_ms: None,
                        config: "{}".into(),
                    },
                    input: &serde_json::json!([{"request_id":"bounded","value":{}}]),
                    deadline_ms: if cancel { 30_000 } else { 5_000 },
                    facts,
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut call => panic!("operation ended before HTTP arrived: {result:?}"),
                arrived = &mut endpoint.arrived => arrived.unwrap(),
            }
        })
        .await
        .unwrap();
        let claimed = tokio::time::Instant::now();
        let request: Vec<u8> = sqlx::query_scalar("SELECT request FROM platform.write_log")
            .fetch_one(&mut observer)
            .await
            .unwrap();
        let operation = invocation().operation;
        assert!(matches!(
            store
                .claim(&operation, "bounded", request.clone())
                .await
                .unwrap(),
            Claim::InProgress
        ));
        if cancel {
            call.abort();
            assert!(call.await.unwrap_err().is_cancelled());
        } else {
            assert!(
                tokio::time::timeout(Duration::from_secs(6), call)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
        }
        // A stream reset does not close an HTTP/2 connection. Observe the driver ending.
        tokio::time::timeout(Duration::from_secs(2), &mut endpoint.closed)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            store
                .claim(&operation, "bounded", request.clone())
                .await
                .unwrap(),
            Claim::InProgress
        ));
        tokio::time::sleep_until(claimed + Duration::from_secs(36)).await;
        assert!(matches!(
            store.claim(&operation, "bounded", request).await.unwrap(),
            Claim::OutcomeUnknown
        ));
    }
}

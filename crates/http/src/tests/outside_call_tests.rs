//! The outside call of a local-target connection reaches a local server.

use std::convert::Infallible;

use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use super::*;
use crate::test_util::Observed;
use wamn_connection::connection_credentials::{ConnectionCredentials, ConnectionGeneration};
use wamn_connection::connection_facts::ConnectionAnswer;
use wamn_connection::connection_memory::{ConnectionFact, ConnectionStatus};
use wamn_connection::database::ProjectDatabase;
use wamn_plugin::host_policy::CONNECTION_HOST_POLICY;

/// The project database is never read: a call without OAuth reads no connection row.
fn facts() -> ConnectionFacts {
    let pools = wamn_postgres::WamnPostgres::new(wamn_postgres::WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
    })
    .expect("offline project pools build");
    let database: Arc<dyn ProjectDatabase> =
        Arc::new(wamn_plugin::OperationDatabase::new(Arc::new(pools)));
    let org = "acme".parse().unwrap();
    ConnectionFacts::new(
        Arc::default(),
        Arc::new(ConnectionCredentials::for_org(&org, None)),
        &database,
        &"acme--dev--erp".parse().unwrap(),
        &"erp".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
}

#[tokio::test]
async fn a_local_target_call_reaches_a_private_server_with_its_credential() {
    // Loopback is refused on every target, so the server listens on the address
    // of the interface on the default route.
    let listener = TcpListener::bind((interface_address(), 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&observed);
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(tcp),
                service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let recorded = Arc::clone(&recorded);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes().to_vec();
                        recorded.lock().unwrap().push(Observed {
                            path: parts.uri.to_string(),
                            method: parts.method.to_string(),
                            headers: parts.headers,
                            body,
                        });
                        Ok::<_, Infallible>(
                            hyper::Response::builder()
                                .header("connection", "close")
                                .body(Full::new(bytes::Bytes::new()))
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
    });

    let transport = HttpTransport::new().unwrap();
    let execution: Arc<dyn wamn_application::InvocationLifetime> =
        Arc::new(wamn_engine::invocation_scope::InvocationExecution::new(
            tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        ));
    let answer = ConnectionAnswer {
        fact: ConnectionFact {
            active_generation: 1,
            status: ConnectionStatus::Active,
            credential_id: Some(uuid::Uuid::new_v4()),
            service_principal_id: uuid::Uuid::new_v4().to_string().parse().unwrap(),
            target_id: uuid::Uuid::new_v4(),
            resource_key: None,
            authority: None,
        },
        generation: Arc::new(ConnectionGeneration {
            name: "manager".into(),
            integration_type: "http".parse().unwrap(),
            auth_type: "http_basic".into(),
            external_system: None,
            parameters: serde_json::json!({
                "primary-authority": format!("http://{address}/"),
                "tls-verification": "disabled",
            }),
        }),
    };

    let (response, token) = transport
        .attempt(
            &Call {
                facts: &facts(),
                connection: uuid::Uuid::new_v4(),
                target: Target::Local,
                execution: &execution,
                host_policy: &CONNECTION_HOST_POLICY,
                request: &effect_request(Vec::new()),
                principal: &|| None,
            },
            Attempt {
                answer: &answer,
                material: Some(r#"{"username":"owner","password":"secret"}"#),
                rejected: None,
            },
        )
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(response.status, 200);
    assert!(token.is_none());
    let observed = observed.lock().unwrap();
    let [request] = observed.as_slice() else {
        panic!("one request reaches the server, got {}", observed.len());
    };
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/notify");
    assert_eq!(request.headers["authorization"], "Basic b3duZXI6c2VjcmV0");
    assert!(request.body.is_empty());
}

//! The org routes of the control serving root (docs/plan/platform-ui.md §4.4,
//! wamn-a40n.3) on the org's real `control` login over the system schema.
//!
//! The test starts its own PostgreSQL 18 server and a fake identity service
//! that requires the operator certificate. It calls the host route delivery
//! with the caller that the control route authenticator admits; the
//! admission itself is the test of `control.mine` in `host_route_live.rs`.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_postgres::{Client, NoTls};
use tokio_rustls::TlsAcceptor;
use wamn_catalog::HostRouteSet;
use wamn_control_provision::{
    CredentialGeneration, PlatformComponent, WorkloadRoleFamily, WorkloadRoleScope, sql,
    workload_generation_role,
};
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryError, DeliveryOutcome, DeliveryRequest, FailureType, RouteDelivery, Source,
};
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_identity_client::PatIssuerConfig;
use wamn_platform_identity::org::{MemberGrants, invite_member};
use wamn_platform_identity::{PrincipalId, create_or_reuse_user};

const ORG: &str = "org-a";
const PASSWORD: &str = "control-route-test-only";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

/// The system schema as `wamn_system`, one org with two projects and one
/// environment each, and the org's `control` login.
async fn install(admin: &Client, admin_url: &str) -> anyhow::Result<String> {
    admin
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    admin
        .batch_execute(&format!(
            "CREATE ROLE wamn_system NOLOGIN; GRANT CREATE ON DATABASE \"{database}\" TO wamn_system; \
             SET ROLE wamn_system"
        ))
        .await?;
    admin
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
               VALUES ('org-a', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
               VALUES ('org-a', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
             INSERT INTO registry.projects (org, id) VALUES ('org-a', 'billing'), ('org-a', 'shop'); \
             INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) \
               VALUES ('org-a', 'billing', 'dev', 's1', 'aaaaaaa1'), \
                      ('org-a', 'shop', 'dev', 's2', 'aaaaaaa2'); \
             RESET ROLE",
        )
        .await?;
    let role = workload_generation_role(
        WorkloadRoleFamily::Control,
        WorkloadRoleScope::Org {
            org: ORG,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Control,
            &database,
            &role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    Ok(url.into())
}

/// A fake identity service that requires the operator certificate. `/users`
/// answers from `users` by email, or refuses an unknown email as identity
/// refuses one. `/invitations` accepts every request. Each request path and
/// body is sent on the returned channel.
struct FakeIdentity {
    config: PatIssuerConfig,
    requests: mpsc::UnboundedReceiver<(String, Value)>,
    task: tokio::task::JoinHandle<()>,
    directory: PathBuf,
}

impl Drop for FakeIdentity {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

async fn fake_identity(users: Vec<(&'static str, String, bool)>) -> FakeIdentity {
    let directory = std::env::temp_dir().join(format!(
        "wamn-control-route-identity-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).expect("fixture directory");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let server_key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let client_key = KeyPair::generate().unwrap();
    let client = CertificateParams::new(vec!["operator.example".to_owned()])
        .unwrap()
        .signed_by(&client_key, &issuer)
        .unwrap();
    let write = |name: &str, pem: &str| {
        let path = directory.join(name);
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(pem.as_bytes()))
            .expect("fixture credential file");
        path
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::clone(&provider),
    )
    .build()
    .unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![server.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
        )
        .unwrap();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = PatIssuerConfig {
        endpoint: Some(format!(
            "https://{}/identity",
            listener.local_addr().unwrap()
        )),
        client_cert: Some(write("operator.pem", &client.pem())),
        client_key: Some(write("operator.key", &client_key.serialize_pem())),
        server_ca: Some(write("ca.pem", &ca.pem())),
    };
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(mut stream) = acceptor.accept(stream).await else {
                continue;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            let headers_end = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(offset) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let headers = String::from_utf8(request[..headers_end].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while request.len() < headers_end + length {
                let read = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
            }
            let path = headers.split(' ').nth(1).unwrap().to_owned();
            let body: Value = serde_json::from_slice(&request[headers_end..]).unwrap();
            let (status, reply) = if path.ends_with("/users") {
                match users.iter().find(|(email, _, _)| body["email"] == *email) {
                    Some((_, id, enrolled)) => {
                        ("200 OK", json!({"principal_id": id, "enrolled": enrolled}))
                    }
                    None => (
                        "400 Bad Request",
                        json!({"error": "email must be a local part, an @, and a dotted domain, in at most 254 bytes"}),
                    ),
                }
            } else {
                ("201 Created", json!({"status": "accepted_for_delivery"}))
            };
            sender.send((path, body)).unwrap();
            let reply = reply.to_string();
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.shutdown().await;
        }
    });
    FakeIdentity {
        config,
        requests,
        task,
        directory,
    }
}

/// One org route call: the answer, or the refusal.
async fn call(
    delivery: &HostRouteDelivery,
    operation: &str,
    caller: &str,
    payload: Value,
) -> Result<Value, String> {
    let (id, _) = HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.reference == format!("wamn-control:{operation}"))
        .expect("the control set serves the route");
    let report = delivery
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(id.to_owned()),
                delivery_id: "control-route-test".to_owned(),
                payload: payload.to_string(),
                caller: None,
                trace: None,
                parent_causation: None,
                if_none_match: None,
            },
            Some(AuthenticatedCaller::new(
                id,
                caller,
                CredentialType::Session,
                false,
                HashSet::new(),
            )),
        )
        .await;
    match report.outcome {
        Ok(DeliveryOutcome::Respond(body)) => Ok(serde_json::from_str(&body).expect("JSON")),
        Ok(DeliveryOutcome::Failed(failure)) => {
            assert!(matches!(failure.failure_type, FailureType::InvalidInput));
            Err(failure.message)
        }
        Err(DeliveryError::PermissionDenied(denial)) => {
            Err(format!("permission denied {}", denial.operation))
        }
        other => panic!("an org route answers or refuses: {other:?}"),
    }
}

/// The org roles, project roles and memberships of one principal.
async fn grants(admin: &Client, principal: &str) -> Vec<String> {
    admin
        .query(
            "SELECT 'org ' || role FROM identity.org_roles WHERE principal_id = $1::text::uuid \
             UNION ALL SELECT 'project ' || project || ' ' || role \
               FROM identity.project_roles WHERE principal_id = $1::text::uuid \
             UNION ALL SELECT 'env ' || project || '/' || env \
               FROM identity.project_env_memberships WHERE principal_id = $1::text::uuid \
             UNION ALL SELECT 'member ' || status || ' ' || updated_by::text \
               FROM identity.org_memberships WHERE principal_id = $1::text::uuid \
             ORDER BY 1",
            &[&principal],
        )
        .await
        .expect("read the grants")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_routes_write_through_the_control_login_and_refuse_a_non_admin() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_org_routes")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let control_url = install(&admin, admin_url).await?;

    // The org owner, an enrolled user and a new user, written as provisioning.
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let mut ids = Vec::new();
    for (email, name) in [
        ("boss@example.test", "Boss"),
        ("cat@example.test", "Cat"),
        ("ann@example.test", "Ann"),
    ] {
        ids.push(
            create_or_reuse_user(&admin, email, name)
                .await?
                .principal_id,
        );
    }
    let [boss, cat, ann]: [PrincipalId; 3] = ids.try_into().expect("three users");
    invite_member(
        &admin,
        &boss,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    let boss = boss.as_str();

    let mut identity = fake_identity(vec![
        ("ann@example.test", ann.as_str().to_owned(), false),
        ("cat@example.test", cat.as_str().to_owned(), true),
    ])
    .await;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: Some(identity.config.clone()),
            org: ORG.to_owned(),
        },
        None,
    );

    // Every org route refuses a caller without org-admin.
    for operation in [
        "user/list",
        "user/invite",
        "user/activate",
        "user/deactivate",
        "project/list",
        "org-admin/grant",
        "org-admin/revoke",
    ] {
        assert_eq!(
            call(&delivery, operation, ann.as_str(), json!({})).await,
            Err(format!("permission denied wamn-control:{operation}@0.1.0"))
        );
    }

    assert_eq!(
        call(&delivery, "project/list", boss, json!({})).await,
        Ok(json!({"projects": ["billing", "shop"]}))
    );

    // A new user gets the membership and the mail; an enrolled user gets no
    // mail; identity's refusal reaches the caller.
    assert_eq!(
        call(
            &delivery,
            "user/invite",
            boss,
            json!({"email": "ann@example.test", "display_name": "Ann",
                   "memberships": [{"project": "billing", "env": "dev"}]})
        )
        .await,
        Ok(json!({"principal_id": ann.as_str(), "enrolled": false, "invited": true}))
    );
    assert_eq!(identity.requests.recv().await.unwrap().0, "/identity/users");
    assert_eq!(
        identity.requests.recv().await.unwrap(),
        (
            "/identity/invitations".to_owned(),
            json!({"principal_id": ann.as_str()})
        )
    );
    assert_eq!(
        grants(&admin, ann.as_str()).await,
        [
            "env billing/dev".to_owned(),
            format!("member active {boss}")
        ]
    );
    assert_eq!(
        call(
            &delivery,
            "user/invite",
            boss,
            json!({"email": "cat@example.test", "display_name": "Cat"})
        )
        .await,
        Ok(json!({"principal_id": cat.as_str(), "enrolled": true, "invited": false}))
    );
    assert_eq!(identity.requests.recv().await.unwrap().0, "/identity/users");
    assert!(
        identity.requests.try_recv().is_err(),
        "an enrolled user gets no invitation"
    );
    assert_eq!(
        call(
            &delivery,
            "user/invite",
            boss,
            json!({"email": "not-an-email", "display_name": "Nobody"})
        )
        .await,
        Err(
            "email must be a local part, an @, and a dotted domain, in at most 254 bytes"
                .to_owned()
        )
    );
    assert_eq!(
        call(&delivery, "user/list", boss, json!({})).await,
        Ok(json!({"users": [
            {"principal_id": ann.as_str(), "email": "ann@example.test", "display_name": "Ann", "status": "active"},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active"},
            {"principal_id": cat.as_str(), "email": "cat@example.test", "display_name": "Cat", "status": "active"},
        ]}))
    );

    // org-admin materializes over every project and environment, and its
    // revoke leaves the ordinary memberships.
    let ann_payload = json!({"principal_id": ann.as_str()});
    assert_eq!(
        call(&delivery, "org-admin/grant", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann.as_str(), "org_admin": true}))
    );
    assert_eq!(
        grants(&admin, ann.as_str()).await,
        [
            "env billing/dev".to_owned(),
            "env shop/dev".to_owned(),
            format!("member active {boss}"),
            "org org-admin".to_owned(),
            "project billing project-admin".to_owned(),
            "project shop project-admin".to_owned(),
        ]
    );
    assert_eq!(
        call(&delivery, "org-admin/revoke", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann.as_str(), "org_admin": false}))
    );
    assert_eq!(
        grants(&admin, ann.as_str()).await,
        [
            "env billing/dev".to_owned(),
            "env shop/dev".to_owned(),
            format!("member active {boss}"),
        ]
    );

    // Deactivation revokes from the leaves upward, and activation restores
    // no access.
    assert_eq!(
        call(&delivery, "user/deactivate", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann.as_str(), "status": "inactive"}))
    );
    assert_eq!(
        grants(&admin, ann.as_str()).await,
        [format!("member inactive {boss}")]
    );
    assert_eq!(
        call(&delivery, "org-admin/grant", boss, ann_payload.clone()).await,
        Err(format!(
            "principal {} is not an active member of org {ORG}",
            ann.as_str()
        ))
    );
    assert_eq!(
        call(&delivery, "user/activate", boss, ann_payload).await,
        Ok(json!({"principal_id": ann.as_str(), "status": "active"}))
    );
    assert_eq!(
        grants(&admin, ann.as_str()).await,
        [format!("member active {boss}")]
    );

    // A revoked org-admin is refused on the next request.
    admin
        .execute(
            "DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid",
            &[&boss],
        )
        .await?;
    assert_eq!(
        call(&delivery, "user/list", boss, json!({})).await,
        Err("permission denied wamn-control:user/list@0.1.0".to_owned())
    );
    Ok(())
}

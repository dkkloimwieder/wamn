//! The org and project routes of the control serving root
//! (docs/plan/platform-ui.md §4.4 and §4.5, wamn-a40n.3 and wamn-a40n.6) on
//! the org's real `control` login over the system schema.
//!
//! The application rows of those routes are written in project databases of
//! the same server, with administration logins that the production prepare
//! mints, read from a directory laid out as the mounted Secret.
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
use tokio_postgres::Client;
use tokio_rustls::TlsAcceptor;
use wamn_catalog::HostRouteSet;
use wamn_control_provision::PlatformComponent;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryError, DeliveryOutcome, DeliveryRequest, RouteDelivery, Source,
};
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_identity_client::PatIssuerConfig;
use wamn_platform_identity::org::{MemberGrants, invite_member};
use wamn_platform_identity::{PrincipalId, create_or_reuse_user};

#[path = "support/control_fixture.rs"]
mod control_fixture;
use control_fixture::{ORG, connect, environments, install};

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

/// One org route call: the answer, or the refusal. A declared refusal reads
/// as its code and its detail.
async fn call(
    delivery: &HostRouteDelivery,
    operation: &str,
    caller: &str,
    payload: Value,
) -> Result<Value, String> {
    let (id, attachment) = HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.reference == format!("wamn-control:{operation}"))
        .expect("the control set serves the route");
    // One request item, as a generated client sends it: a read carries its
    // input, and a write carries a request id and its input under `value`.
    let item = if attachment.route.type_.is_read() {
        payload
    } else {
        json!({"request_id": "control-route-test", "value": payload})
    };
    let report = delivery
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(id.to_owned()),
                delivery_id: "control-route-test".to_owned(),
                payload: json!([item]).to_string(),
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
        Ok(DeliveryOutcome::Respond(body)) => {
            let [item]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
            match item.get("error") {
                Some(error) => Err(refused(
                    error["code"].as_str().expect("a refusal has a code"),
                    &error["detail"],
                )),
                None => Ok(item["value"].clone()),
            }
        }
        Err(DeliveryError::PermissionDenied(denial)) => {
            Err(format!("permission denied {}", denial.operation))
        }
        other => panic!("an org route answers or refuses: {other:?}"),
    }
}

/// How [`call`] reads a declared refusal.
fn refused(code: &str, detail: &Value) -> String {
    format!("{code} {detail}")
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
    let (logins, _billing, _shop) = environments(&admin, admin_url, "org").await?;

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
            administration: Some(logins.clone()),
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
            Err(format!("permission denied wamn-control:{operation}@0.3.0"))
        );
    }

    assert_eq!(
        call(&delivery, "project/list", boss, json!({})).await,
        Ok(json!({"projects": ["billing", "shop"]}))
    );

    // A new user gets the membership and the mail; an enrolled user gets no
    // mail. The host refuses an email that the identity rules refuse, and
    // identity refuses a user it does not create.
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
        Err(refused("invalid_input", &json!({"field": "email"})))
    );
    assert_eq!(
        call(
            &delivery,
            "user/invite",
            boss,
            json!({"email": "dan@example.test", "display_name": "Dan"})
        )
        .await,
        Err(refused("user_refused", &json!({"field": "email"})))
    );
    assert_eq!(identity.requests.recv().await.unwrap().0, "/identity/users");
    assert_eq!(
        call(&delivery, "user/list", boss, json!({})).await,
        Ok(json!({"users": [
            {"principal_id": ann.as_str(), "email": "ann@example.test", "display_name": "Ann", "status": "active", "org_admin": false},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active", "org_admin": true},
            {"principal_id": cat.as_str(), "email": "cat@example.test", "display_name": "Cat", "status": "active", "org_admin": false},
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
        Err(refused(
            "user_not_active",
            &json!({"field": "principal_id"})
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

    // A revoked org-admin is refused on the next request. Boss keeps the
    // project-admin rows that org-admin materialized, so an org route that
    // only org-admin holds is the one that refuses.
    admin
        .execute(
            "DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid",
            &[&boss],
        )
        .await?;
    assert_eq!(
        call(&delivery, "project/list", boss, json!({})).await,
        Err("permission denied wamn-control:project/list@0.3.0".to_owned())
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_routes_admit_a_project_admin_and_refuse_a_covered_revoke() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_project_routes")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let control_url = install(&admin, admin_url).await?;
    let (logins, _billing, _shop) = environments(&admin, admin_url, "project").await?;

    // An org admin, a project admin of billing and a plain member.
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&provisioning],
        )
        .await?;
    let mut ids = Vec::new();
    for (email, name, grants) in [
        (
            "boss@example.test",
            "Boss",
            MemberGrants {
                org_admin: true,
                ..MemberGrants::default()
            },
        ),
        (
            "cat@example.test",
            "Cat",
            MemberGrants {
                project_admins: vec!["billing".to_owned()],
                ..MemberGrants::default()
            },
        ),
        ("ann@example.test", "Ann", MemberGrants::default()),
    ] {
        let user = create_or_reuse_user(&admin, email, name)
            .await?
            .principal_id;
        invite_member(&admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, cat, ann]: [PrincipalId; 3] = ids.try_into().expect("three users");
    let (boss, cat, ann) = (boss.as_str(), cat.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
        },
        None,
    );
    let member = |principal: &str, env: &str| json!({"project": "billing", "env": env, "principal_id": principal});
    let project_admin = |principal: &str| json!({"project": "billing", "principal_id": principal});

    // Every project route refuses a caller without org-admin or project-admin
    // in the named project.
    for (operation, payload) in [
        ("environment/list", json!({"project": "billing"})),
        ("member/list", json!({"project": "billing"})),
        ("member/grant", member(ann, "dev")),
        ("member/revoke", member(ann, "dev")),
        ("project-admin/grant", project_admin(ann)),
        ("project-admin/revoke", project_admin(ann)),
    ] {
        assert_eq!(
            call(&delivery, operation, ann, payload).await,
            Err(format!("permission denied wamn-control:{operation}@0.3.0"))
        );
    }
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "shop"})
        )
        .await,
        Err("permission denied wamn-control:environment/list@0.3.0".to_owned())
    );
    // A project admin reads the org's users, and holds no other org route.
    assert_eq!(
        call(&delivery, "user/list", cat, json!({})).await,
        Ok(json!({"users": [
            {"principal_id": ann, "email": "ann@example.test", "display_name": "Ann", "status": "active", "org_admin": false},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active", "org_admin": true},
            {"principal_id": cat, "email": "cat@example.test", "display_name": "Cat", "status": "active", "org_admin": false},
        ]}))
    );
    assert_eq!(
        call(&delivery, "project/list", cat, json!({})).await,
        Err("permission denied wamn-control:project/list@0.3.0".to_owned())
    );
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "billing"})
        )
        .await,
        Ok(json!({"environments": ["dev"]}))
    );

    // A membership goes to an active org member in an environment of the
    // project.
    assert_eq!(
        call(&delivery, "member/grant", cat, member(ann, "dev")).await,
        Ok(json!({"principal_id": ann, "project": "billing", "env": "dev", "member": true}))
    );
    assert_eq!(
        grants(&admin, ann).await,
        [
            "env billing/dev".to_owned(),
            format!("member active {provisioning}")
        ]
    );
    assert_eq!(
        call(&delivery, "member/grant", cat, member(ann, "prod")).await,
        Err(refused("environment_not_found", &json!({"field": "env"})))
    );
    assert_eq!(
        call(&delivery, "member/list", cat, json!({"project": "billing"})).await,
        Ok(json!({"members": [
            {"principal_id": ann, "email": "ann@example.test", "display_name": "Ann",
             "org_admin": false, "project_admin": false, "environments": ["dev"]},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss",
             "org_admin": true, "project_admin": true, "environments": ["dev"]},
            {"principal_id": cat, "email": "cat@example.test", "display_name": "Cat",
             "org_admin": false, "project_admin": true, "environments": ["dev"]},
        ]}))
    );

    // A covering grant refuses the revoke below it.
    assert_eq!(
        call(&delivery, "member/revoke", boss, member(cat, "dev")).await,
        Err(refused("admin_covered", &json!({"field": "principal_id"})))
    );
    assert_eq!(
        call(&delivery, "member/revoke", cat, member(boss, "dev")).await,
        Err(refused("admin_covered", &json!({"field": "principal_id"})))
    );
    assert_eq!(
        call(&delivery, "project-admin/revoke", cat, project_admin(boss)).await,
        Err(refused("admin_covered", &json!({"field": "principal_id"})))
    );

    // project-admin covers every environment of the project, and its revoke
    // leaves the memberships.
    assert_eq!(
        call(&delivery, "project-admin/grant", cat, project_admin(ann)).await,
        Ok(json!({"principal_id": ann, "project": "billing", "project_admin": true}))
    );
    assert_eq!(
        grants(&admin, ann).await,
        [
            "env billing/dev".to_owned(),
            format!("member active {provisioning}"),
            "project billing project-admin".to_owned(),
        ]
    );
    assert_eq!(
        call(&delivery, "project-admin/revoke", cat, project_admin(ann)).await,
        Ok(json!({"principal_id": ann, "project": "billing", "project_admin": false}))
    );
    assert_eq!(
        call(&delivery, "member/revoke", cat, member(ann, "dev")).await,
        Ok(json!({"principal_id": ann, "project": "billing", "env": "dev", "member": false}))
    );
    assert_eq!(
        grants(&admin, ann).await,
        [format!("member active {provisioning}")]
    );

    // A revoked project admin is refused on the next request.
    admin
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid",
            &[&cat],
        )
        .await?;
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "billing"})
        )
        .await,
        Err("permission denied wamn-control:environment/list@0.3.0".to_owned())
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

/// The application rows of one principal in one project database.
async fn application_rows(target: &Client, principal: &str) -> Vec<String> {
    target
        .query(
            "SELECT 'user ' || tenant_id FROM app_system.users WHERE id = $1::text::uuid \
             UNION ALL SELECT 'role ' || role_name FROM app_system.user_roles \
               WHERE user_id = $1::text::uuid \
             ORDER BY 1",
            &[&principal],
        )
        .await
        .expect("read the application rows")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_writes_reach_the_application_rows_of_every_environment() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_application_rows")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let control_url = install(&admin, admin_url).await?;
    let (logins, billing, shop) = environments(&admin, admin_url, "application").await?;
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let mut ids = Vec::new();
    for (email, name, org_admin) in [
        ("boss@example.test", "Boss", true),
        ("ann@example.test", "Ann", false),
    ] {
        let user = create_or_reuse_user(&admin, email, name)
            .await?
            .principal_id;
        let grants = MemberGrants {
            org_admin,
            ..MemberGrants::default()
        };
        invite_member(&admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, ann]: [PrincipalId; 2] = ids.try_into().expect("two users");
    let (boss, ann) = (boss.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
        },
        None,
    );
    let ann_payload = json!({"principal_id": ann});
    let billing_admin = json!({"project": "billing", "principal_id": ann});
    let billing_member = json!({"project": "billing", "env": "dev", "principal_id": ann});

    // org-admin writes the user row and admin in every environment, after its
    // system rows; its revoke removes admin first and keeps the user row.
    assert_eq!(
        call(&delivery, "org-admin/grant", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann, "org_admin": true}))
    );
    assert_eq!(
        application_rows(&billing, ann).await,
        ["role admin", "user t-billing"]
    );
    assert_eq!(
        application_rows(&shop, ann).await,
        ["role admin", "user t-shop"]
    );
    assert_eq!(
        call(
            &delivery,
            "project-admin/revoke",
            boss,
            billing_admin.clone()
        )
        .await,
        Err(refused("admin_covered", &json!({"field": "principal_id"})))
    );
    assert_eq!(
        application_rows(&billing, ann).await,
        ["role admin", "user t-billing"],
        "a refused revoke changes no application row"
    );
    assert_eq!(
        call(&delivery, "org-admin/revoke", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann, "org_admin": false}))
    );
    assert_eq!(application_rows(&billing, ann).await, ["user t-billing"]);
    assert_eq!(application_rows(&shop, ann).await, ["user t-shop"]);

    // project-admin covers its project's environments only.
    assert_eq!(
        call(
            &delivery,
            "project-admin/grant",
            boss,
            billing_admin.clone()
        )
        .await,
        Ok(json!({"principal_id": ann, "project": "billing", "project_admin": true}))
    );
    assert_eq!(
        application_rows(&billing, ann).await,
        ["role admin", "user t-billing"]
    );
    assert_eq!(application_rows(&shop, ann).await, ["user t-shop"]);
    assert_eq!(
        call(&delivery, "project-admin/revoke", boss, billing_admin).await,
        Ok(json!({"principal_id": ann, "project": "billing", "project_admin": false}))
    );
    assert_eq!(application_rows(&billing, ann).await, ["user t-billing"]);

    // A membership revoke removes the user row, and a grant writes it again.
    assert_eq!(
        call(&delivery, "member/revoke", boss, billing_member.clone()).await,
        Ok(json!({"principal_id": ann, "project": "billing", "env": "dev", "member": false}))
    );
    assert!(application_rows(&billing, ann).await.is_empty());
    assert_eq!(
        call(&delivery, "member/grant", boss, billing_member).await,
        Ok(json!({"principal_id": ann, "project": "billing", "env": "dev", "member": true}))
    );
    assert_eq!(application_rows(&billing, ann).await, ["user t-billing"]);

    // A failed environment refuses with the completed ones named, and the
    // system rows of the deactivation stay uncommitted.
    let shop_key = logins.join("shop--dev");
    let shop_login = std::fs::read_to_string(&shop_key)?;
    std::fs::write(&shop_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    assert_eq!(
        call(&delivery, "user/deactivate", boss, ann_payload.clone()).await,
        Err(refused(
            "application_write_incomplete",
            &json!({"environment": "shop/dev", "completed": ["billing/dev"]})
        ))
    );
    assert!(application_rows(&billing, ann).await.is_empty());
    assert_eq!(application_rows(&shop, ann).await, ["user t-shop"]);
    assert_eq!(
        grants(&admin, ann).await,
        [
            "env billing/dev".to_owned(),
            "env shop/dev".to_owned(),
            format!(
                "member active {}",
                PlatformComponent::Provisioning.principal_id()
            ),
        ],
        "the system rows stay until every environment is done"
    );

    // With the login back, the deactivation completes.
    std::fs::write(&shop_key, shop_login)?;
    assert_eq!(
        call(&delivery, "user/deactivate", boss, ann_payload).await,
        Ok(json!({"principal_id": ann, "status": "inactive"}))
    );
    assert!(application_rows(&shop, ann).await.is_empty());
    assert_eq!(
        grants(&admin, ann).await,
        [format!("member inactive {boss}")]
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

/// The system status of each environment of the org, and the status row of
/// each project database, or `none` without one.
async fn statuses(admin: &Client, billing: &Client, shop: &Client) -> Vec<String> {
    let mut statuses: Vec<String> = admin
        .query(
            "SELECT 'system ' || project || '/' || env || ' ' || status \
               FROM registry.project_envs WHERE org = $1 ORDER BY project, env",
            &[&ORG],
        )
        .await
        .expect("read the system statuses")
        .iter()
        .map(|row| row.get(0))
        .collect();
    for (name, target) in [("billing", billing), ("shop", shop)] {
        let row = target
            .query_opt("SELECT status FROM app_system.environment", &[])
            .await
            .expect("read the status row");
        let status: String = row.map_or_else(|| "none".to_owned(), |row| row.get(0));
        statuses.push(format!("row {name} {status}"));
    }
    statuses
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_routes_mirror_the_status_into_each_environment_leaves_first() -> anyhow::Result<()>
{
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_status")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let control_url = install(&admin, admin_url).await?;
    let (logins, billing, shop) = environments(&admin, admin_url, "status").await?;
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let mut ids = Vec::new();
    for (email, name, org_admin) in [
        ("boss@example.test", "Boss", true),
        ("ann@example.test", "Ann", false),
    ] {
        let user = create_or_reuse_user(&admin, email, name)
            .await?
            .principal_id;
        let grants = MemberGrants {
            org_admin,
            project_admins: if org_admin {
                Vec::new()
            } else {
                vec!["billing".to_owned()]
            },
            ..MemberGrants::default()
        };
        invite_member(&admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, ann]: [PrincipalId; 2] = ids.try_into().expect("two users");
    let (boss, ann) = (boss.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
        },
        None,
    );
    let billing_dev = json!({"project": "billing", "env": "dev"});
    let shop_project = json!({"project": "shop"});
    assert_eq!(
        statuses(&admin, &billing, &shop).await,
        [
            "system billing/dev active",
            "system shop/dev active",
            "row billing none",
            "row shop none"
        ],
        "an environment without a status row is active"
    );

    // A project-admin of billing is not an org-admin.
    assert_eq!(
        call(
            &delivery,
            "environment/inactivate",
            ann,
            billing_dev.clone()
        )
        .await,
        Err("permission denied wamn-control:environment/inactivate@0.3.0".to_owned())
    );
    assert_eq!(
        call(
            &delivery,
            "environment/inactivate",
            boss,
            json!({"project": "billing", "env": "prod"})
        )
        .await,
        Err(refused("environment_not_found", &json!({"field": "env"})))
    );
    assert_eq!(
        call(
            &delivery,
            "project/inactivate",
            boss,
            json!({"project": "nope"})
        )
        .await,
        Err(refused("project_not_found", &json!({"field": "project"})))
    );

    // One environment, then a whole project, and back.
    assert_eq!(
        call(
            &delivery,
            "environment/inactivate",
            boss,
            billing_dev.clone()
        )
        .await,
        Ok(json!({"project": "billing", "env": "dev", "status": "inactive"}))
    );
    assert_eq!(
        call(&delivery, "project/inactivate", boss, shop_project.clone()).await,
        Ok(json!({"project": "shop", "status": "inactive"}))
    );
    assert_eq!(
        statuses(&admin, &billing, &shop).await,
        [
            "system billing/dev inactive",
            "system shop/dev inactive",
            "row billing inactive",
            "row shop inactive"
        ]
    );
    assert_eq!(
        call(&delivery, "environment/activate", boss, billing_dev.clone()).await,
        Ok(json!({"project": "billing", "env": "dev", "status": "active"}))
    );
    assert_eq!(
        statuses(&admin, &billing, &shop).await,
        [
            "system billing/dev active",
            "system shop/dev inactive",
            "row billing active",
            "row shop inactive"
        ]
    );

    // An activation commits the system row first, so a failed environment
    // leaves it active and its row inactive.
    let shop_key = logins.join("shop--dev");
    let shop_login = std::fs::read_to_string(&shop_key)?;
    std::fs::write(&shop_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    assert_eq!(
        call(&delivery, "project/activate", boss, shop_project.clone()).await,
        Err(refused(
            "application_write_incomplete",
            &json!({"environment": "shop/dev", "completed": []})
        ))
    );
    assert_eq!(
        statuses(&admin, &billing, &shop).await,
        [
            "system billing/dev active",
            "system shop/dev active",
            "row billing active",
            "row shop inactive"
        ],
        "an activation commits the system row before the environment"
    );
    std::fs::write(&shop_key, &shop_login)?;
    assert_eq!(
        call(&delivery, "project/activate", boss, shop_project).await,
        Ok(json!({"project": "shop", "status": "active"}))
    );

    // An inactivation writes the environment first, so a failed environment
    // leaves the system row active.
    let billing_key = logins.join("billing--dev");
    std::fs::write(&billing_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    assert_eq!(
        call(&delivery, "environment/inactivate", boss, billing_dev).await,
        Err(refused(
            "application_write_incomplete",
            &json!({"environment": "billing/dev", "completed": []})
        ))
    );
    assert_eq!(
        statuses(&admin, &billing, &shop).await,
        [
            "system billing/dev active",
            "system shop/dev active",
            "row billing active",
            "row shop active"
        ],
        "an inactivation commits the system row after every environment"
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

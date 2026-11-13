//! The org and project routes of the control serving root
//! (docs/architecture/execution.md#host-run-routes, wamn-a40n.3, wamn-a40n.6)
//! on the org's real `control` login over the system schema.
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
use wamn_broker::Publisher;

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
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::create_or_reuse_user;
use wamn_platform_identity::org::{MemberGrants, invite_member};

#[path = "support/control_fixture.rs"]
mod control_fixture;
use control_fixture::{ORG, connect, environments, install};

/// A fake identity service that requires the operator certificate. `/users`
/// answers from `user` by email, or refuses an unknown email as identity
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
    // One request element, as a generated client sends it: a read carries its
    // input, and a write carries a request id and its input under `value`.
    let request_element = if attachment.route.type_.is_read() {
        payload
    } else {
        json!({"request_id": "control-route-test", "value": payload})
    };
    let report = delivery
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(id.to_owned()),
                delivery_id: "control-route-test".to_owned(),
                payload: json!([request_element]).to_string(),
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
                None,
            )),
        )
        .await;
    match report.outcome {
        Ok(DeliveryOutcome::Respond(body)) => {
            let [request_element]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
            match request_element.get("error") {
                Some(error) => Err(refused(
                    error["code"].as_str().expect("a refusal has a code"),
                    &error["detail"],
                )),
                None => Ok(request_element["value"].clone()),
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
    let boss_text = boss.to_string();
    let boss = boss_text.as_str();

    let mut identity = fake_identity(vec![
        ("ann@example.test", ann.to_string(), false),
        ("cat@example.test", cat.to_string(), true),
    ])
    .await;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: Some(identity.config.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
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
            call(&delivery, operation, ann.to_string().as_str(), json!({})).await,
            Err(format!("permission denied wamn-control:{operation}@0.9.0"))
        );
    }

    assert_eq!(
        call(&delivery, "project/list", boss, json!({})).await,
        Ok(
            json!({"next_cursor": null, "total": 2, "projects": [{"name": "billing", "state": "active"}, {"name": "shop", "state": "active"}]})
        )
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
        Ok(json!({"principal_id": ann.to_string().as_str(), "enrolled": false, "invited": true}))
    );
    assert_eq!(identity.requests.recv().await.unwrap().0, "/identity/users");
    assert_eq!(
        identity.requests.recv().await.unwrap(),
        (
            "/identity/invitations".to_owned(),
            json!({"principal_id": ann.to_string().as_str()})
        )
    );
    assert_eq!(
        grants(&admin, ann.to_string().as_str()).await,
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
        Ok(json!({"principal_id": cat.to_string().as_str(), "enrolled": true, "invited": false}))
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
        Ok(json!({"next_cursor": null, "total": 3, "users": [
            {"principal_id": ann.to_string().as_str(), "email": "ann@example.test", "display_name": "Ann", "status": "active", "org_admin": false},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active", "org_admin": true},
            {"principal_id": cat.to_string().as_str(), "email": "cat@example.test", "display_name": "Cat", "status": "active", "org_admin": false},
        ]}))
    );
    // A paged read follows next_cursor to the end. Only a complete first page carries a total.
    let mut paged = Vec::new();
    let mut input = json!({"limit": 1});
    loop {
        let page = call(&delivery, "user/list", boss, input)
            .await
            .expect("a page");
        assert!(page["total"].is_null());
        assert_eq!(page["users"].as_array().expect("users").len(), 1);
        paged.push(page["users"][0]["principal_id"].clone());
        if page["next_cursor"].is_null() {
            break;
        }
        input = json!({"limit": 1, "cursor": page["next_cursor"]});
    }
    assert_eq!(
        paged,
        [
            json!(ann.to_string().as_str()),
            json!(boss),
            json!(cat.to_string().as_str())
        ]
    );
    assert_eq!(
        call(&delivery, "user/list", boss, json!({"limit": 3}))
            .await
            .expect("one page")["total"],
        3
    );

    // org-admin materializes over every project and environment, and its
    // revoke leaves the ordinary memberships.
    let ann_payload = json!({"principal_id": ann.to_string().as_str()});
    assert_eq!(
        call(&delivery, "org-admin/grant", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann.to_string().as_str(), "org_admin": true}))
    );
    assert_eq!(
        grants(&admin, ann.to_string().as_str()).await,
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
        Ok(json!({"principal_id": ann.to_string().as_str(), "org_admin": false}))
    );
    assert_eq!(
        grants(&admin, ann.to_string().as_str()).await,
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
        Ok(json!({"principal_id": ann.to_string().as_str(), "status": "inactive"}))
    );
    assert_eq!(
        grants(&admin, ann.to_string().as_str()).await,
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
        Ok(json!({"principal_id": ann.to_string().as_str(), "status": "active"}))
    );
    assert_eq!(
        grants(&admin, ann.to_string().as_str()).await,
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
        Err("permission denied wamn-control:project/list@0.9.0".to_owned())
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
    let (boss_text, cat_text, ann_text) = (boss.to_string(), cat.to_string(), ann.to_string());
    let (boss, cat, ann) = (boss_text.as_str(), cat_text.as_str(), ann_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
        None,
    );
    let member = |principal: &str, env: &str| json!({"project": "billing", "env": env, "principal_id": principal});
    let project_admin = |principal: &str| json!({"project": "billing", "principal_id": principal});

    // Every project route refuses a caller without org-admin or project-admin
    // in the named project.
    for (operation, payload) in [
        ("environment/list", json!({"project": "billing"})),
        (
            "environment/status",
            json!({"project": "billing", "env": "dev"}),
        ),
        ("member/list", json!({"project": "billing"})),
        ("member/grant", member(ann, "dev")),
        ("member/revoke", member(ann, "dev")),
        ("project-admin/grant", project_admin(ann)),
        ("project-admin/revoke", project_admin(ann)),
    ] {
        assert_eq!(
            call(&delivery, operation, ann, payload).await,
            Err(format!("permission denied wamn-control:{operation}@0.9.0"))
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
        Err("permission denied wamn-control:environment/list@0.9.0".to_owned())
    );
    // A project admin reads the org's users, and holds no other org route.
    assert_eq!(
        call(&delivery, "user/list", cat, json!({})).await,
        Ok(json!({"next_cursor": null, "total": 3, "users": [
            {"principal_id": ann, "email": "ann@example.test", "display_name": "Ann", "status": "active", "org_admin": false},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active", "org_admin": true},
            {"principal_id": cat, "email": "cat@example.test", "display_name": "Cat", "status": "active", "org_admin": false},
        ]}))
    );
    assert_eq!(
        call(&delivery, "project/list", cat, json!({})).await,
        Err("permission denied wamn-control:project/list@0.9.0".to_owned())
    );
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "billing"})
        )
        .await,
        Ok(
            json!({"next_cursor": null, "total": 1, "environments": [{"name": "dev", "state": "active"}]})
        )
    );
    // A status read names an environment of the project before it reads
    // Kubernetes.
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            cat,
            json!({"project": "billing", "env": "prod"})
        )
        .await,
        Err(refused("environment_not_found", &json!({"field": "env"})))
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
        Ok(json!({"next_cursor": null, "total": 3, "members": [
            {"principal_id": ann, "email": "ann@example.test", "display_name": "Ann",
             "org_admin": false, "project_admin": false, "environments": ["dev"]},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss",
             "org_admin": true, "project_admin": true, "environments": ["dev"]},
            {"principal_id": cat, "email": "cat@example.test", "display_name": "Cat",
             "org_admin": false, "project_admin": true, "environments": ["dev"]},
        ]}))
    );

    // An environment filters in the query, before the limit.
    let page = call(
        &delivery,
        "member/list",
        cat,
        json!({"project": "billing", "environment": "dev", "limit": 2}),
    )
    .await
    .expect("a filtered page");
    assert_eq!(
        page["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row["email"].clone(), row["environments"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("ann@example.test"), json!(["dev"])),
            (json!("boss@example.test"), json!(["dev"])),
        ]
    );
    assert!(page["next_cursor"].is_string());
    assert_eq!(
        call(
            &delivery,
            "member/list",
            cat,
            json!({"project": "billing", "cursor": page["next_cursor"]}),
        )
        .await,
        Err(refused("invalid_input", &json!({"field": "cursor"})))
    );
    assert_eq!(
        call(
            &delivery,
            "member/list",
            cat,
            json!({"project": "billing", "environment": "prod"}),
        )
        .await,
        Ok(json!({"next_cursor": null, "total": 0, "members": []}))
    );
    assert_eq!(
        call(
            &delivery,
            "member/list",
            cat,
            json!({"project": "billing", "environment": null}),
        )
        .await,
        Err(refused("invalid_input", &json!({"field": "environment"})))
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
        Err("permission denied wamn-control:environment/list@0.9.0".to_owned())
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

/// The application rows of one principal in one project database.
async fn application_rows(target: &Client, principal: &str) -> Vec<String> {
    target
        .query(
            "SELECT 'user' FROM platform.\"user\" WHERE id = $1::text::uuid \
             UNION ALL SELECT 'role ' || role_name FROM platform.user_role \
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
    let (boss_text, ann_text) = (boss.to_string(), ann.to_string());
    let (boss, ann) = (boss_text.as_str(), ann_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
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
        ["role admin", "user"]
    );
    assert_eq!(application_rows(&shop, ann).await, ["role admin", "user"]);
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
        ["role admin", "user"],
        "a refused revoke changes no application row"
    );
    assert_eq!(
        call(&delivery, "org-admin/revoke", boss, ann_payload.clone()).await,
        Ok(json!({"principal_id": ann, "org_admin": false}))
    );
    assert_eq!(application_rows(&billing, ann).await, ["user"]);
    assert_eq!(application_rows(&shop, ann).await, ["user"]);

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
        ["role admin", "user"]
    );
    assert_eq!(application_rows(&shop, ann).await, ["user"]);
    assert_eq!(
        call(&delivery, "project-admin/revoke", boss, billing_admin).await,
        Ok(json!({"principal_id": ann, "project": "billing", "project_admin": false}))
    );
    assert_eq!(application_rows(&billing, ann).await, ["user"]);

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
    assert_eq!(application_rows(&billing, ann).await, ["user"]);

    // A failed projection names completed environments after system authority commits.
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
    assert_eq!(application_rows(&shop, ann).await, ["user"]);
    assert_eq!(
        grants(&admin, ann).await,
        [format!("member inactive {boss}")],
        "committed deactivation remains authoritative after a projection failure"
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
               FROM registry.environment_projects WHERE org = $1 ORDER BY project, env",
            &[&ORG],
        )
        .await
        .expect("read the system statuses")
        .iter()
        .map(|row| row.get(0))
        .collect();
    for (name, target) in [("billing", billing), ("shop", shop)] {
        let row = target
            .query_opt("SELECT status FROM platform.environment", &[])
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
    let (boss_text, ann_text) = (boss.to_string(), ann.to_string());
    let (boss, ann) = (boss_text.as_str(), ann_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
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
        Err("permission denied wamn-control:environment/inactivate@0.9.0".to_owned())
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
        call(&delivery, "project/list", boss, json!({})).await,
        Ok(
            json!({"next_cursor": null, "total": 2, "projects": [{"name": "billing", "state": "inactive"}, {"name": "shop", "state": "inactive"}]})
        )
    );
    assert_eq!(
        call(&delivery, "environment/list", boss, shop_project.clone()).await,
        Ok(
            json!({"next_cursor": null, "total": 1, "environments": [{"name": "dev", "state": "inactive"}]})
        )
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
    admin.execute(
        "WITH parent_environment AS ( \
             INSERT INTO registry.environments (org, env, policy_name, status) \
             SELECT org, 'other', policy_name, 'inactive' FROM registry.environments \
             WHERE org=$1 AND env='dev' \
         ) INSERT INTO registry.environment_projects (org, env, project, secret_name, instance_suffix) \
         SELECT org, 'other', project, secret_name, instance_suffix \
         FROM registry.environment_projects WHERE org=$1 AND project='billing' AND env='dev'",
        &[&ORG],
    ).await?;
    assert_eq!(
        call(&delivery, "project/list", boss, json!({})).await,
        Ok(
            json!({"next_cursor": null, "total": 2, "projects": [{"name": "billing", "state": "active"}, {"name": "shop", "state": "active"}]})
        ),
        "an inactive environment does not hide an active environment"
    );
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            boss,
            json!({"project": "billing"})
        )
        .await,
        Ok(
            json!({"next_cursor": null, "total": 2, "environments": [{"name": "dev", "state": "active"}, {"name": "other", "state": "inactive"}]})
        )
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_create_writes_a_project_that_every_org_admin_administers() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_project_create")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let control_url = install(&admin, admin_url).await?;
    let (logins, _billing, _shop) = environments(&admin, admin_url, "ledger").await?;

    // Two org admins and a project admin of billing.
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
            "dee@example.test",
            "Dee",
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
    ] {
        let user = create_or_reuse_user(&admin, email, name)
            .await?
            .principal_id;
        invite_member(&admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, dee, cat]: [PrincipalId; 3] = ids.try_into().expect("three users");
    let (boss_text, dee_text, cat_text) = (boss.to_string(), dee.to_string(), cat.to_string());
    let (boss, dee, cat) = (boss_text.as_str(), dee_text.as_str(), cat_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
        None,
    );

    // Only an org admin creates a project, and the id is a project slug.
    assert_eq!(
        call(
            &delivery,
            "project/create",
            cat,
            json!({"project": "ledger"})
        )
        .await,
        Err("permission denied wamn-control:project/create@0.9.0".to_owned())
    );
    for project in ["Ledger", "wamn-ledger", "led--ger"] {
        assert_eq!(
            call(
                &delivery,
                "project/create",
                boss,
                json!({"project": project})
            )
            .await,
            Err(refused("invalid_input", &json!({"field": "project"}))),
            "{project} is not a project id"
        );
    }
    assert_eq!(
        call(
            &delivery,
            "project/create",
            boss,
            json!({"project": "billing"})
        )
        .await,
        Err(refused("project_exists", &json!({"field": "project"})))
    );
    assert_eq!(
        call(
            &delivery,
            "project/create",
            boss,
            json!({"project": "ledger"})
        )
        .await,
        Ok(json!({"project": "ledger"}))
    );
    assert_eq!(
        call(
            &delivery,
            "project/create",
            dee,
            json!({"project": "ledger"})
        )
        .await,
        Err(refused("project_exists", &json!({"field": "project"}))),
        "ledger exists"
    );

    // The project has no environment, and each org admin, and only they,
    // holds project-admin in it.
    let projects = call(&delivery, "project/list", dee, json!({}))
        .await
        .expect("an org admin lists the projects");
    assert_eq!(
        projects["projects"],
        json!([{"name": "billing", "state": "active"}, {"name": "ledger", "state": "inactive"}, {"name": "shop", "state": "active"}])
    );
    let listed = call(
        &delivery,
        "environment/list",
        dee,
        json!({"project": "ledger"}),
    )
    .await
    .expect("an org admin lists the environments of ledger");
    assert_eq!(
        listed,
        json!({"next_cursor": null, "total": 0, "environments": []})
    );
    let mut admins: Vec<String> = admin
        .query(
            "SELECT principal_id::text FROM identity.project_roles \
              WHERE org = $1 AND project = 'ledger' AND role = 'project-admin'",
            &[&ORG],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    admins.sort();
    let mut expected = vec![boss.to_owned(), dee.to_owned()];
    expected.sort();
    assert_eq!(admins, expected);

    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

#[derive(Debug)]
struct FixedStatus;

#[async_trait::async_trait]
impl wamn_execution_host::environment_status::StatusSource for FixedStatus {
    async fn read(&self, _: &str, _: &str, _: &str) -> anyhow::Result<Value> {
        Ok(
            json!({"digest": "sha256:live", "rollout": {"replicas": 1}, "workloads": [], "live": []}),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_status_counts_are_targeted_and_optional() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("control_status_counts")?;
    let admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let (logins, billing, shop) = environments(&admin, database.url(), "counts").await?;
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let principal = create_or_reuse_user(&admin, "counts@example.test", "Counts")
        .await?
        .principal_id;
    invite_member(
        &admin,
        &principal,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    billing
        .batch_execute(
            "INSERT INTO platform.role (name) VALUES ('first'), ('second');
         INSERT INTO platform.\"user\" (id, type, email) VALUES
           ('00000000-0000-0000-0000-000000000001', 'user', 'one@example.test'),
           ('00000000-0000-0000-0000-000000000002', 'user', 'two@example.test'),
           ('00000000-0000-0000-0000-000000000003', 'service', 'service@example.test');
         INSERT INTO platform.user_role (user_id, role_name) VALUES
           ('00000000-0000-0000-0000-000000000001', 'first'),
           ('00000000-0000-0000-0000-000000000001', 'second'),
           ('00000000-0000-0000-0000-000000000003', 'first');",
        )
        .await?;
    shop.batch_execute(
        "INSERT INTO platform.role (name) VALUES ('ignored');
        INSERT INTO platform.\"user\" (id, type, email) VALUES
        ('00000000-0000-0000-0000-000000000004', 'user', 'other@example.test');
        INSERT INTO platform.user_role (user_id, role_name) VALUES
        ('00000000-0000-0000-0000-000000000004', 'ignored');",
    )
    .await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(FixedStatus),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
        None,
    );
    let payload = json!({"project": "billing", "env": "dev"});
    let expected = json!({"digest": "sha256:live", "rollout": {"replicas": 1}, "workloads": [], "live": [],
        "users_with_role": 1, "roles": 2, "memberships": 1});
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            payload.clone()
        )
        .await,
        Ok(expected.clone())
    );
    let mut other = expected.clone();
    other["roles"] = json!(1);
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            json!({"project": "shop", "env": "dev"})
        )
        .await,
        Ok(other)
    );
    shop.batch_execute("DELETE FROM platform.role").await?;
    let mut empty = expected.clone();
    empty["users_with_role"] = json!(0);
    empty["roles"] = json!(0);
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            json!({"project": "shop", "env": "dev"})
        )
        .await,
        Ok(empty)
    );
    // A failed application query cannot turn runtime status into a refusal.
    billing
        .batch_execute("ALTER TABLE platform.role RENAME TO unavailable_roles")
        .await?;
    let mut unavailable = expected;
    unavailable["users_with_role"] = Value::Null;
    unavailable["roles"] = Value::Null;
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            payload.clone()
        )
        .await,
        Ok(unavailable.clone())
    );
    // A missing mounted credential has the same optional-count behavior.
    let login = logins.join(wamn_control_provision::control_administration_key(
        "billing", "dev",
    ));
    std::fs::remove_file(login)?;
    assert_eq!(
        call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            payload
        )
        .await,
        Ok(unavailable)
    );
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

async fn assert_definer_grantees(client: &Client) -> anyhow::Result<()> {
    const CONNECTION: &[&str] = &["wamn_control", "wamn_administration"];
    const ALLOWED: &[(&str, &[&str])] = &[
        ("identity.revoke_disabled_password_logins()", &[]),
        (
            "identity.lock_password_principal(uuid)",
            &["wamn_identity_issuer"],
        ),
        ("platform.bump_counter(uuid, text, text)", &["wamn_app"]),
        ("platform.assign_counter(uuid, text, text)", &["wamn_app"]),
        (
            "platform.set_counter(uuid, text, text, bigint)",
            &["wamn_app"],
        ),
        (
            "registry.read_file_connection(text, text, text, uuid)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.lock_oauth_connection(text, text, text, uuid)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.read_connection_fact(text, text, text, uuid)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.read_connection_generation(text, text, text, uuid, bigint)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.read_connection_secret(text, text, text, uuid)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.read_env_connections(text, text, text)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.read_integration_version(text, integer)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.resolve_environment_connection(text, text, text, text, text, integer)",
            &["wamn_apply", "wamn_db_owner"],
        ),
        (
            "registry.connection_projection(text, text, text)",
            &["wamn_apply", "wamn_control", "wamn_db_owner"],
        ),
        (
            "registry.publish_integration(text, integer, text, jsonb, text, text[], text[])",
            &["wamn_control"],
        ),
        (
            "registry.configure_connection(text, text, jsonb)",
            CONNECTION,
        ),
        (
            "registry.configure_external_system(text, text, jsonb)",
            CONNECTION,
        ),
        (
            "registry.configure_integration(text, text, jsonb)",
            CONNECTION,
        ),
        (
            "registry.configure_integration_type(text, text, jsonb)",
            CONNECTION,
        ),
        (
            "registry.retired_connection_credential(text)",
            &["wamn_control"],
        ),
    ];
    let functions = client.query("SELECT n.nspname || '.' || p.proname || '(' || oidvectortypes(p.proargtypes) || ')', p.proconfig, pg_get_userbyid(p.proowner), ARRAY(SELECT CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE pg_get_userbyid(a.grantee)::text END FROM aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) a WHERE a.privilege_type='EXECUTE' AND a.grantee<>p.proowner ORDER BY 1) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE p.prosecdef ORDER BY 1", &[]).await?;
    assert!(!functions.is_empty());
    for function in functions {
        let name: String = function.get(0);
        let allowed = ALLOWED
            .iter()
            .find(|(signature, _)| *signature == name)
            .unwrap_or_else(|| panic!("SECURITY DEFINER function absent from allow list: {name}"));
        assert_eq!(function.get::<_, String>(2), "wamn_system", "{name} owner");
        let configuration: Vec<String> = function.get(1);
        let expected_path = "search_path=pg_catalog";
        assert_eq!(configuration, [expected_path], "{name}");
        for grantee in function.get::<_, Vec<String>>(3) {
            assert!(
                allowed.1.contains(&grantee.as_str()),
                "unexpected EXECUTE grantee {grantee} on {name}"
            );
        }
        let effective = client.query("SELECT r.rolname::text FROM pg_roles r WHERE NOT r.rolsuper AND has_function_privilege(r.oid,$1::text::regprocedure,'EXECUTE') AND NOT pg_has_role(r.oid,(SELECT proowner FROM pg_proc WHERE oid=$1::text::regprocedure),'USAGE') AND NOT EXISTS (SELECT FROM pg_roles a WHERE a.rolname=ANY($2::text[]) AND pg_has_role(r.oid,a.oid,'USAGE'))", &[&name, &allowed.1]).await?;
        assert!(
            effective.is_empty(),
            "unexpected inherited EXECUTE privilege on {name}: {effective:?}"
        );
        let grantable: bool = client.query_one("SELECT EXISTS (SELECT FROM pg_proc p CROSS JOIN LATERAL aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) a WHERE p.oid=$1::text::regprocedure AND a.grantee<>p.proowner AND a.is_grantable)", &[&name]).await?.get(0);
        assert!(!grantable, "non-owner grant option on {name}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn connection_routes_refuse_mismatches_and_project_only_enabled_rows() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("connection_routes")?;
    let admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let (logins, billing, shop) = environments(&admin, database.url(), "connections").await?;
    let writer = connect(&control_url).await?;
    writer
        .batch_execute("SET TIME ZONE 'America/New_York'")
        .await?;
    assert!(
        writer.batch_execute("SET ROLE wamn_control").await.is_err(),
        "the control generation inherits its grants without permission to assume the ACL role"
    );
    admin
        .batch_execute(&format!(
            "SET app.user_id='{}'; UPDATE registry.meta SET platform_domain='example.test'",
            PlatformComponent::Provisioning.principal_id()
        ))
        .await?;
    assert_definer_grantees(&admin).await?;
    assert_definer_grantees(&billing).await?;
    assert_definer_grantees(&shop).await?;
    assert!(
        billing
            .query_one(
                "SELECT to_regprocedure('catalog.connection_is_referenced(text)') IS NULL",
                &[]
            )
            .await?
            .get::<_, bool>(0)
    );
    let boss = create_or_reuse_user(&admin, "connection-admin@example.test", "Admin")
        .await?
        .principal_id;
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
    let (_nats_server, nats) = wamn_runtime::auth_cache::test_util::nats().await;
    let bucket = wamn_broker::auth_changes::auth_bucket(&nats).await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::new(nats),
        },
        None,
    );
    admin.batch_execute(&format!("BEGIN; {} UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning))).await?;
    let external = json!({"project":"billing","name":"erp"});
    assert!(
        call(
            &delivery,
            "external-system/write",
            "00000000-0000-4000-8000-000000000099",
            external.clone()
        )
        .await
        .unwrap_err()
        .starts_with("permission denied ")
    );
    call(
        &delivery,
        "external-system/write",
        boss.to_string().as_str(),
        external,
    )
    .await
    .unwrap();
    let integration = json!({"project":"billing","name":"erp","integration_type":"http","auth_type":"none","external_system":"erp"});
    call(
        &delivery,
        "integration/write",
        boss.to_string().as_str(),
        integration.clone(),
    )
    .await
    .unwrap();
    let mut wrong_type = integration;
    wrong_type["integration_type"] = "s3".into();
    wrong_type["auth_type"] = "custom".into();
    assert!(
        call(
            &delivery,
            "integration/write",
            boss.to_string().as_str(),
            wrong_type
        )
        .await
        .is_err()
    );
    let connection = json!({"project":"billing","env":"dev","name":"erp","parameters":{"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}});
    call(
        &delivery,
        "connection/write",
        boss.to_string().as_str(),
        connection.clone(),
    )
    .await
    .unwrap();
    for (client, count) in [(&billing, 1_i64), (&shop, 0)] {
        assert_eq!(
            client
                .query_one("SELECT count(*) FROM platform.connection", &[])
                .await?
                .get::<_, i64>(0),
            count
        );
        for table in ["external_system", "integration", "connection"] {
            assert!(!client.query_one("SELECT has_table_privilege('wamn_app',$1,'INSERT,UPDATE,DELETE,TRUNCATE') OR has_any_column_privilege('wamn_app',$1,'INSERT,UPDATE')",&[&format!("platform.{table}")]).await?.get::<_,bool>(0));
        }
        for table in ["catalog.release_connection", "catalog.connection_binding"] {
            assert!(!client.query_one("SELECT has_table_privilege('wamn_administration',$1,'INSERT,UPDATE,DELETE,TRUNCATE') OR has_any_column_privilege('wamn_administration',$1,'INSERT,UPDATE')",&[&table]).await?.get::<_,bool>(0));
        }
    }
    let identity: String = billing
        .query_one("SELECT id::text FROM platform.connection", &[])
        .await?
        .get(0);
    admin.execute("UPDATE registry.integration SET auth_type='oauth2_client_credentials' WHERE name='erp' AND project='billing'", &[]).await?;
    let selection = json!({"project":"billing","env":"dev","name":"erp"});
    let empty = call(
        &delivery,
        "connection/get",
        boss.to_string().as_str(),
        selection.clone(),
    )
    .await
    .unwrap();
    assert_eq!(empty["refresh_status"], Value::Null);
    assert_eq!(empty["expires_at"], Value::Null);
    admin.execute("INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) VALUES($1::text::uuid,$2,$3,$4,'file:fixture','2099-01-01T00:00:00Z','failing',clock_timestamp())",
        &[&identity, &b"private-token-bytes".as_slice(), &b"private-nonce".as_slice(), &b"private-wrapped-key".as_slice()]).await?;
    for status in ["failing", "ok"] {
        admin.execute("UPDATE identity.oauth_token SET refresh_status=$1 WHERE connection_id=$2::text::uuid", &[&status,&identity]).await?;
        let answer = call(
            &delivery,
            "connection/get",
            boss.to_string().as_str(),
            selection.clone(),
        )
        .await
        .unwrap();
        assert_eq!(answer["refresh_status"], status);
        assert_eq!(answer["expires_at"], "2099-01-01T00:00:00+00:00");
        for private in [
            "ciphertext",
            "nonce",
            "wrapped_key",
            "key_id",
            "access_token",
            "refresh_token",
            "private-token",
        ] {
            assert!(!answer.to_string().contains(private), "{answer}");
        }
    }
    admin
        .execute(
            "DELETE FROM identity.oauth_token WHERE connection_id=$1::text::uuid",
            &[&identity],
        )
        .await?;
    admin.execute("UPDATE registry.integration SET auth_type='none' WHERE name='erp' AND project='billing'", &[]).await?;
    billing.execute("CREATE TABLE public.connection_reference(connection_id uuid REFERENCES platform.connection(id))",&[]).await?;
    billing
        .execute(
            "INSERT INTO public.connection_reference VALUES($1::text::uuid)",
            &[&identity],
        )
        .await?;
    let selection = json!({"project":"billing","env":"dev","name":"erp"});
    call(
        &delivery,
        "connection/disable",
        boss.to_string().as_str(),
        selection.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        billing
            .query_one("SELECT status FROM platform.connection", &[])
            .await?
            .get::<_, String>(0),
        "disabled"
    );
    billing
        .batch_execute(&format!(
            "BEGIN; {}",
            wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning)
        ))
        .await?;
    // The sha256 of the canonical bytes `{}`, which catalog.releases tests.
    let digest = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
    billing.execute("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)",&[&wamn_registry::project_env_key(ORG,"billing","dev"),&digest,&b"{}".as_slice()]).await?;
    billing.execute("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3::text::uuid,1)",&[&wamn_registry::project_env_key(ORG,"billing","dev"),&digest,&identity]).await?;
    billing.batch_execute("COMMIT").await?;
    assert!(
        call(
            &delivery,
            "connection/delete",
            boss.to_string().as_str(),
            selection.clone()
        )
        .await
        .unwrap_err()
        .starts_with("referenced ")
    );
    call(
        &delivery,
        "connection/enable",
        boss.to_string().as_str(),
        selection,
    )
    .await
    .unwrap();
    let mut changed = connection.clone();
    changed["parameters"]["primary-authority"] = "https://changed.invalid".into();
    assert!(
        call(
            &delivery,
            "connection/write",
            boss.to_string().as_str(),
            changed
        )
        .await
        .unwrap_err()
        .starts_with("immutable_field ")
    );
    billing
        .batch_execute("ALTER TABLE platform.integration RENAME TO unavailable_integration")
        .await?;
    let mut changed = connection;
    changed["parameters"]["tls-verification"] = "disabled".into();
    assert!(
        call(
            &delivery,
            "connection/write",
            boss.to_string().as_str(),
            changed
        )
        .await
        .unwrap_err()
        .starts_with("application_write_incomplete ")
    );
    assert_eq!(admin.query_one("SELECT g.parameters->>'tls-verification' FROM registry.connection c JOIN registry.connection_generation g ON(g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.id=$1::text::uuid",&[&identity]).await?.get::<_,String>(0),"disabled");
    billing
        .batch_execute("ALTER TABLE platform.unavailable_integration RENAME TO integration")
        .await?;
    let document = wamn_environment_control::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(
            &json!({"org":ORG,"project":"billing","env":"dev","target":"cloud","policy":"dev","applications":{}}),
        )?,
    )?;
    // The projection runs as the database owner, as env apply runs it.
    let mut billing_url = url::Url::parse(database.url())?;
    billing_url.set_path(
        url::Url::parse(
            std::fs::read_to_string(logins.join(
                wamn_control_provision::control_administration_key("billing", "dev"),
            ))?
            .trim(),
        )?
        .path(),
    );
    let _ = wamn_environment_control::environment::bindings::project_authority(
        database.url(),
        billing_url.as_str(),
        &document,
    )
    .await?;
    let before: String = billing
        .query_one("SELECT xmin::text FROM platform.connection", &[])
        .await?
        .get(0);
    let _ = wamn_environment_control::environment::bindings::project_authority(
        database.url(),
        billing_url.as_str(),
        &document,
    )
    .await?;
    assert_eq!(
        billing
            .query_one("SELECT xmin::text FROM platform.connection", &[])
            .await?
            .get::<_, String>(0),
        before
    );
    drop(bucket);
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

#[path = "support/connection_test.rs"]
mod connection_test;

#[path = "support/oauth_authorization.rs"]
mod oauth_authorization;

#[cfg(feature = "test-util")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn delayed_grant_cannot_restore_revoked_admin() -> anyhow::Result<()> {
    use wamn_execution_host::ProjectionPause;
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("grant_projection_order")?;
    let admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let (logins, billing, _shop) =
        environments(&admin, database.url(), "grant_projection_order").await?;
    admin.execute("SELECT set_config('app.user_id',$1,false), set_config('app.operation','wamn:provisioning',false)", &[&PlatformComponent::Provisioning.principal_id().to_string()]).await?;
    let boss = create_or_reuse_user(&admin, "boss-order@example.test", "Boss")
        .await?
        .principal_id;
    let user = create_or_reuse_user(&admin, "member-order@example.test", "Member")
        .await?
        .principal_id;
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
    invite_member(&admin, &user, ORG, &MemberGrants::default()).await?;
    let (_server, broker) = wamn_runtime::auth_cache::test_util::nats().await;
    let changes = Publisher::new(broker.clone());
    let bucket = wamn_broker::auth_changes::auth_bucket(&broker).await?;
    let delivery = async || -> anyhow::Result<HostRouteDelivery> {
        Ok(HostRouteDelivery::new(
            Arc::new(LoadedRelease::control_root()),
            HostRouteHandlers::Control {
                status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
                administration: Some(logins.clone()),
                control: Arc::new(connect(&control_url).await?),
                writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
                identity: None,
                org: ORG.to_owned(),
                changes: changes.clone(),
            },
            None,
        ))
    };
    let second = delivery().await?;
    for after_lock in [false, true] {
        let pause = Arc::new(ProjectionPause::new(user.clone(), after_lock));
        let first = delivery().await?.with_projection_pause(Arc::clone(&pause));
        let caller = boss.as_str().to_owned();
        let principal = user.as_str().to_owned();
        let grant = tokio::spawn(async move {
            call(
                &first,
                "project-admin/grant",
                &caller,
                json!({"project":"billing","principal_id":principal}),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered()).await?;
        let project_env_key = wamn_registry::project_env_key(ORG, "billing", "dev");
        bucket
            .put(
                wamn_broker::auth_changes::principal_key(&project_env_key, user.as_str()),
                "stale".into(),
            )
            .await?;
        let payload = json!({"project":"billing","principal_id":user.as_str()});
        let revoke = call(&second, "project-admin/revoke", boss.as_str(), payload);
        tokio::pin!(revoke);
        if after_lock {
            let blocked = async {
                loop {
                    let waiting: bool = admin.query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND cardinality(pg_blocking_pids(pid))>0)", &[],
                    ).await?.get(0);
                    if waiting {
                        return anyhow::Ok(());
                    }
                    tokio::task::yield_now().await;
                }
            };
            tokio::select! {
                result = &mut revoke => panic!("revocation bypassed the held system lock: {result:?}"),
                result = tokio::time::timeout(std::time::Duration::from_secs(10), blocked) => result??,
            }
            pause.resume();
            revoke.await.map_err(anyhow::Error::msg)?;
        } else {
            revoke.await.map_err(anyhow::Error::msg)?;
            let count: i64 = billing.query_one("SELECT count(*) FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin'", &[&user.as_str()]).await?.get(0);
            assert_eq!(
                count, 0,
                "the second host completes revocation while the grant waits"
            );
            pause.resume();
        }
        grant.await?.map_err(anyhow::Error::msg)?;
        let rows = application_rows(&billing, user.as_str()).await;
        assert!(rows.iter().all(|row| !row.contains("admin")), "{rows:?}");
        let count: i64 = admin.query_one("SELECT count(*) FROM identity.project_roles WHERE principal_id=$1::text::uuid AND role='project-admin'", &[&user.as_str()]).await?.get(0);
        assert_eq!(count, 0);
        let project_env_key = wamn_registry::project_env_key(ORG, "billing", "dev");
        assert!(
            bucket
                .get(wamn_broker::auth_changes::principal_key(
                    &project_env_key,
                    user.as_str()
                ))
                .await?
                .is_none()
        );
    }
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

#[path = "support/operation_jobs.rs"]
mod operation_jobs;

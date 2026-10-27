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
use wamn_platform_identity::auth_changes::AuthChanges;

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
                None,
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
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: Some(identity.config.clone()),
            org: ORG.to_owned(),
            changes: AuthChanges::default(),
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
        Ok(json!({"next_cursor": null, "total": 3, "users": [
            {"principal_id": ann.as_str(), "email": "ann@example.test", "display_name": "Ann", "status": "active", "org_admin": false},
            {"principal_id": boss, "email": "boss@example.test", "display_name": "Boss", "status": "active", "org_admin": true},
            {"principal_id": cat.as_str(), "email": "cat@example.test", "display_name": "Cat", "status": "active", "org_admin": false},
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
        [json!(ann.as_str()), json!(boss), json!(cat.as_str())]
    );
    assert_eq!(
        call(&delivery, "user/list", boss, json!({"limit": 3}))
            .await
            .expect("one page")["total"],
        3
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
    let (boss, cat, ann) = (boss.as_str(), cat.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: AuthChanges::default(),
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
    let (boss, ann) = (boss.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: AuthChanges::default(),
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
    assert_eq!(application_rows(&shop, ann).await, ["user"]);
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
    let (boss, ann) = (boss.as_str(), ann.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: AuthChanges::default(),
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
        "INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix, policy_name, status) \
         SELECT org, project, 'other', secret_name, instance_suffix, policy_name, 'inactive' \
         FROM registry.project_envs WHERE org = $1 AND project = 'billing' AND env = 'dev'",
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
    let (boss, dee, cat) = (boss.as_str(), dee.as_str(), cat.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: AuthChanges::default(),
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
async fn environment_status_counts_are_scoped_and_optional() -> anyhow::Result<()> {
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
            changes: AuthChanges::default(),
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
            principal.as_str(),
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
            principal.as_str(),
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
            principal.as_str(),
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
            principal.as_str(),
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
        call(&delivery, "environment/status", principal.as_str(), payload).await,
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
        (
            "wamn_authority.session_author_env_key()",
            &["wamn_control_author"],
        ),
        ("platform.bump_counter(uuid, text, text)", &["wamn_app"]),
        ("platform.assign_counter(uuid, text, text)", &["wamn_app"]),
        (
            "platform.set_counter(uuid, text, text, bigint)",
            &["wamn_app"],
        ),
        (
            "registry.lookup_connection_credential(text, text, text, bigint)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.lookup_connection_binding_mode(text, text, text, text, text)",
            &["wamn_conn_reader"],
        ),
        (
            "registry.lookup_file_connection(text, text, text, text, text, text, bigint)",
            &["wamn_conn_reader"],
        ),
        (
            "catalog.connection_upgrade_bindings(text, text)",
            &["wamn_administration"],
        ),
        ("registry.connection_org_exists(text)", CONNECTION),
        ("registry.connection_integration_exists(text)", CONNECTION),
        ("registry.connection_system_rows(text)", CONNECTION),
        ("registry.connection_rows(text)", CONNECTION),
        ("registry.connection_project_rows(text)", CONNECTION),
        ("registry.connection_allow_rows(text)", CONNECTION),
        ("registry.connection_principal_rows(text, text)", CONNECTION),
        (
            "registry.configure_connection(text, text, jsonb)",
            CONNECTION,
        ),
        ("registry.rotate_connection(text, text, text)", CONNECTION),
        (
            "registry.import_connection_credentials(text, jsonb)",
            CONNECTION,
        ),
        ("registry.connection_integrations()", CONNECTION),
        (
            "registry.connection_binding_rows(text, text, text)",
            CONNECTION,
        ),
        (
            "registry.analyse_connection_bindings(text, text, text, text, jsonb)",
            CONNECTION,
        ),
        (
            "registry.reserve_connection_bindings(text, text, text, text, jsonb)",
            CONNECTION,
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
        let expected_path = if name == "wamn_authority.session_author_env_key()" {
            "search_path=pg_catalog, wamn_authority"
        } else {
            "search_path=pg_catalog"
        };
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
        if name == "catalog.connection_upgrade_bindings(text, text)" {
            let policies = client.query("SELECT c.relname::text, p.polcmd::text, p.polroles=ARRAY[(SELECT proowner FROM pg_proc WHERE oid=$1::text::regprocedure)], pg_get_expr(p.polqual,p.polrelid), p.polwithcheck IS NULL FROM pg_policy p JOIN pg_class c ON c.oid=p.polrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='catalog' AND p.polname='connection_upgrade_reader_owner' ORDER BY c.relname", &[&name]).await?;
            assert_eq!(
                policies
                    .iter()
                    .map(|row| row.get::<_, String>(0))
                    .collect::<Vec<_>>(),
                [
                    "connection_bindings",
                    "connection_generations",
                    "connection_instances",
                    "connection_requirements",
                    "releases"
                ]
            );
            for policy in policies {
                assert_eq!(policy.get::<_, String>(1), "r");
                assert!(
                    policy.get::<_, bool>(2),
                    "only the function owner reads through this policy"
                );
                assert_eq!(policy.get::<_, String>(3), "true");
                assert!(
                    policy.get::<_, bool>(4),
                    "the reader policy has no write condition"
                );
            }
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_routes_refuse_mismatches_and_project_only_enabled_rows() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("connection_routes")?;
    let admin = connect(database.url()).await?;
    let control_url = install(&admin, database.url()).await?;
    let (logins, billing, shop) = environments(&admin, database.url(), "connections").await?;
    let writer = connect(&control_url).await?;
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
    let bucket = wamn_platform_identity::auth_changes::auth_bucket(&nats).await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: Arc::new(connect(&control_url).await?),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            identity: None,
            org: ORG.to_owned(),
            changes: AuthChanges::new(nats),
        },
        None,
    );
    let system = json!({"integration":"http","name":"erp","parameters":{},"status":"active"});
    assert!(
        call(
            &delivery,
            "external-system/write",
            "00000000-0000-4000-8000-000000000099",
            system.clone()
        )
        .await
        .unwrap_err()
        .starts_with("permission denied ")
    );
    let system_id = call(
        &delivery,
        "external-system/write",
        boss.as_str(),
        system.clone(),
    )
    .await
    .unwrap()["name"]
        .clone();
    let connection = json!({"integration":"http","external_system":system_id,"name":"erp","transport":"http","exchange":"request","parameters":{"primary-authority":"https://example.invalid","tls-verification":"verify-authority"},"status":"active"});
    let id = call(
        &delivery,
        "connection/write",
        boss.as_str(),
        connection.clone(),
    )
    .await
    .unwrap()["name"]
        .clone();
    let first_uuid: String = admin
        .query_one(
            "SELECT id::text FROM registry.connection WHERE org=$1 AND name='erp'",
            &[&ORG],
        )
        .await?
        .get(0);
    call(
        &delivery,
        "connection/write",
        boss.as_str(),
        connection.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        admin
            .query_one(
                "SELECT id::text FROM registry.connection WHERE org=$1 AND name='erp'",
                &[&ORG]
            )
            .await?
            .get::<_, String>(0),
        first_uuid
    );
    let mut new_name = connection.clone();
    new_name["name"] = "separate".into();
    assert_eq!(
        call(&delivery, "connection/write", boss.as_str(), new_name)
            .await
            .unwrap()["name"],
        "separate"
    );
    assert_ne!(
        admin
            .query_one(
                "SELECT id::text FROM registry.connection WHERE org=$1 AND name='separate'",
                &[&ORG]
            )
            .await?
            .get::<_, String>(0),
        first_uuid
    );
    let principal: String = admin
        .query_one(
            "SELECT service_principal_id::text FROM registry.connection WHERE name=$1",
            &[&id.as_str().unwrap()],
        )
        .await?
        .get(0);
    let key =
        wamn_platform_identity::auth_changes::principal_key("org-a--billing--dev", &principal);
    bucket.put(&key, "cached".into()).await?;
    call(
        &delivery,
        "project-connection/enable",
        boss.as_str(),
        json!({"project":"billing","connection":id}),
    )
    .await
    .unwrap();
    assert!(
        bucket.get(&key).await?.is_none(),
        "enabling the connection deletes its projected auth key"
    );
    let mut rename = connection.clone();
    rename["id"] = id.clone();
    rename["name"] = "renamed".into();
    assert!(
        call(&delivery, "connection/write", boss.as_str(), rename)
            .await
            .unwrap_err()
            .starts_with("invalid_input ")
    );
    let mut rename = system;
    rename["id"] = system_id.clone();
    rename["name"] = "renamed".into();
    assert!(
        call(
            &delivery,
            "external-system/write",
            boss.as_str(),
            rename.clone()
        )
        .await
        .unwrap_err()
        .starts_with("invalid_input ")
    );
    rename.as_object_mut().unwrap().remove("id");
    rename["name"] = "erp".into();
    rename["integration"] = "gcs".into();
    assert!(
        call(&delivery, "external-system/write", boss.as_str(), rename)
            .await
            .unwrap_err()
            .starts_with("integration_mismatch ")
    );
    let mismatch = json!({"integration":"gcs","external_system":system_id,"name":"blob","transport":"http","exchange":"request","parameters":{"provider":"gcs","container":"bucket","prefix":"test"},"status":"active"});
    assert!(
        call(&delivery, "connection/write", boss.as_str(), mismatch)
            .await
            .unwrap_err()
            .starts_with("integration_mismatch ")
    );
    assert!(
        call(
            &delivery,
            "project-connection/enable",
            boss.as_str(),
            json!({"project":"missing","connection":id})
        )
        .await
        .unwrap_err()
        .starts_with("not_found ")
    );
    assert!(
        call(
            &delivery,
            "integration-allow/allow",
            boss.as_str(),
            json!({"scope_type":"project","scope_id":"billing","integration":"http"})
        )
        .await
        .unwrap_err()
        .starts_with("integration_not_allowed ")
    );
    call(
        &delivery,
        "integration-allow/allow",
        boss.as_str(),
        json!({"scope_type":"org","scope_id":ORG,"integration":"http"}),
    )
    .await
    .unwrap();
    for (scope, id) in [("project", "billing"), ("environment", "billing/dev")] {
        call(
            &delivery,
            "integration-allow/allow",
            boss.as_str(),
            json!({"scope_type":scope,"scope_id":id,"integration":"http"}),
        )
        .await
        .unwrap();
    }
    let applied_document = wamn_control::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(&json!({
            "org": ORG,"project":"billing","env":"dev","target":"cloud","release":"none","route_host":"billing.example","policy":"dev",
            "applications":{"fixture":{"packages":["fixture"],"connections":{"upstream":"erp"}}}
        }))?,
    )?;
    let package = wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(
        &json!({
            "package":{"id":"fixture","version":"0.1.0"},"required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "models":{},"components":{},"connections":{"upstream":{"integration":"http","mode":"read"}}
        }),
    )?)?;
    let mut authority = connect(&control_url).await?;
    let binding_tx =
        wamn_control::provision_project_env::provisioning_transaction(&mut authority).await?;
    wamn_control::environment::bindings::reserve(
        &binding_tx,
        &applied_document,
        std::slice::from_ref(&package),
    )
    .await?;
    binding_tx.commit().await?;
    let replacement = call(
        &delivery,
        "external-system/write",
        boss.as_str(),
        json!({"integration":"http","name":"replacement","parameters":{},"status":"active"}),
    )
    .await
    .unwrap()["name"]
        .clone();
    let mut moved = connection;
    moved["external_system"] = replacement.clone();
    call(&delivery, "connection/write", boss.as_str(), moved)
        .await
        .unwrap();
    assert_eq!(
        billing
            .query_one(
                "SELECT name FROM platform.external_system WHERE status='active'",
                &[]
            )
            .await?
            .get::<_, String>(0),
        replacement.as_str().unwrap()
    );
    for (client, expected) in [(&billing, 1_i64), (&shop, 0)] {
        assert_eq!(
            client
                .query_one("SELECT count(*) FROM platform.connection", &[])
                .await?
                .get::<_, i64>(0),
            expected
        );
        for table in [
            "connection",
            "external_system",
            "project_connection",
            "integration_allow",
        ] {
            let privilege: bool = client
                .query_one(
                    "SELECT has_table_privilege('wamn_app', $1, 'INSERT,UPDATE,DELETE,TRUNCATE') OR has_any_column_privilege('wamn_app', $1, 'INSERT,UPDATE')",
                    &[&format!("platform.{table}")],
                )
                .await?
                .get(0);
            assert!(!privilege, "wamn_app writes {table}");
        }
    }
    call(
        &delivery,
        "project-connection/enable",
        boss.as_str(),
        json!({"project":"shop","connection":"erp"}),
    )
    .await
    .unwrap();
    shop.batch_execute(
        "ALTER TABLE platform.integration_allow RENAME TO integration_allow_unavailable",
    )
    .await?;
    let mut changed = json!({"integration":"http","external_system":replacement,"name":"erp","transport":"http","exchange":"request","parameters":{"primary-authority":"https://changed.invalid","tls-verification":"verify-authority"},"status":"active"});
    assert!(
        call(
            &delivery,
            "connection/write",
            boss.as_str(),
            changed.clone()
        )
        .await
        .unwrap_err()
        .starts_with("application_write_incomplete ")
    );
    assert_eq!(
        admin
            .query_one(
                "SELECT parameters->>'primary-authority' FROM registry.connection WHERE name='erp'",
                &[]
            )
            .await?
            .get::<_, String>(0),
        "https://changed.invalid"
    );
    assert_eq!(
        billing
            .query_one(
                "SELECT parameters->>'primary-authority' FROM platform.connection WHERE name='erp'",
                &[]
            )
            .await?
            .get::<_, String>(0),
        "https://changed.invalid"
    );
    shop.batch_execute(
        "ALTER TABLE platform.integration_allow_unavailable RENAME TO integration_allow",
    )
    .await?;
    let document = wamn_control::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(&json!({
            "org": ORG, "project":"shop", "env":"dev", "target":"cloud", "release":"none", "route_host":"shop.example", "policy":"dev", "applications":{}
        }))?,
    )?;
    let shop_url = std::fs::read_to_string(logins.join(
        wamn_control_provision::control_administration_key("shop", "dev"),
    ))?;
    let _ = wamn_control::environment::bindings::project_authority(
        database.url(),
        shop_url.trim(),
        &document,
    )
    .await?;
    assert_eq!(
        shop.query_one(
            "SELECT parameters->>'primary-authority' FROM platform.connection WHERE name='erp'",
            &[]
        )
        .await?
        .get::<_, String>(0),
        "https://changed.invalid"
    );
    let before: String = shop
        .query_one(
            "SELECT xmin::text FROM platform.connection WHERE name='erp'",
            &[],
        )
        .await?
        .get(0);
    let _ = wamn_control::environment::bindings::project_authority(
        database.url(),
        shop_url.trim(),
        &document,
    )
    .await?;
    assert_eq!(
        shop.query_one(
            "SELECT xmin::text FROM platform.connection WHERE name='erp'",
            &[]
        )
        .await?
        .get::<_, String>(0),
        before
    );
    let history_sql = "SELECT (SELECT count(*) FROM platform.connection_history)+(SELECT count(*) FROM platform.external_system_history)+(SELECT count(*) FROM platform.project_connection_history)";
    let projection_history: i64 = shop.query_one(history_sql, &[]).await?.get(0);
    let _ = wamn_control::environment::bindings::project_authority(
        database.url(),
        shop_url.trim(),
        &document,
    )
    .await?;
    assert_eq!(
        shop.query_one(history_sql, &[]).await?.get::<_, i64>(0),
        projection_history
    );
    // A project disable retains provenance references and does not disable another project.
    billing.batch_execute("CREATE TABLE public.connection_reference (connection_id uuid REFERENCES platform.connection(id), external_system_id uuid REFERENCES platform.external_system(id)); INSERT INTO public.connection_reference SELECT id,external_system_id FROM platform.connection WHERE name='erp'").await?;
    let shop_principal_key =
        wamn_platform_identity::auth_changes::principal_key("org-a--shop--dev", &principal);
    bucket.put(&key, "cached".into()).await?;
    bucket.put(&shop_principal_key, "cached".into()).await?;
    call(
        &delivery,
        "project-connection/disable",
        boss.as_str(),
        json!({"project":"billing","connection":"erp"}),
    )
    .await
    .unwrap();
    assert_eq!(billing.query_one("SELECT c.status FROM platform.connection c JOIN public.connection_reference r ON r.connection_id=c.id", &[]).await?.get::<_,String>(0), "disabled");
    assert_eq!(
        shop.query_one(
            "SELECT status FROM platform.connection WHERE name='erp'",
            &[]
        )
        .await?
        .get::<_, String>(0),
        "active"
    );
    assert_eq!(
        wamn_control::environment::bindings::analyse(&authority, &applied_document, &[package])
            .await
            .unwrap_err()
            .code,
        "connection_not_enabled"
    );
    assert!(
        bucket.get(&key).await?.is_none(),
        "disablement deletes the affected project's auth key"
    );
    assert!(
        bucket.get(&shop_principal_key).await?.is_some(),
        "disablement retains the other project's auth key"
    );
    let shop_key = logins.join(wamn_control_provision::control_administration_key(
        "shop", "dev",
    ));
    let stalled = TcpListener::bind("127.0.0.1:0").await?;
    let stalled_port = stalled.local_addr()?.port();
    let stalled_connection = tokio::spawn(async move {
        let (_stream, _) = stalled.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    std::fs::write(
        &shop_key,
        format!("host=127.0.0.1 port={stalled_port} user=unreachable dbname=unreachable"),
    )?;
    // Only shop enables erp, so its unavailable environment reports a bounded incomplete write.
    let failed = tokio::time::timeout(
        std::time::Duration::from_secs(7),
        call(
            &delivery,
            "connection/write",
            boss.as_str(),
            changed.clone(),
        ),
    )
    .await?;
    assert!(
        failed
            .unwrap_err()
            .starts_with("application_write_incomplete ")
    );
    stalled_connection.abort();
    std::fs::write(&shop_key, &shop_url)?;
    // A disabled project's unavailable environment does not block a connection write.
    std::fs::write(
        logins.join(wamn_control_provision::control_administration_key(
            "billing", "dev",
        )),
        "host=127.0.0.1 port=1 user=unreachable dbname=unreachable",
    )?;
    call(
        &delivery,
        "connection/write",
        boss.as_str(),
        changed.clone(),
    )
    .await
    .unwrap();
    changed["integration"] = "wamn".into();
    assert!(
        call(&delivery, "connection/write", boss.as_str(), changed)
            .await
            .unwrap_err()
            .starts_with("integration_not_found ")
    );
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

//! The org and project routes of the control serving root
//! (docs/architecture/execution.md#host-run-routes, wamn-a40n.3, wamn-a40n.6)
//! on the org's real `control` login over the system schema.
//!
//! The application rows of those routes are written in project databases of
//! the same server, with administration logins that the production prepare
//! mints, read from a directory laid out as the mounted Secret.
//!
//! The test starts its own PostgreSQL 18 server and the identity service
//! that requires the operator certificate. It calls the host route delivery
//! with the caller that the control route authenticator admits; the
//! admission itself is the test of `control.mine` in `host_route_live.rs`.

use sqlx::{Connection as _, Row as _};
use std::collections::HashSet;
use std::sync::Arc;
use wamn_broker::Publisher;

use rcgen::{CertificateParams, KeyPair};
use serde_json::{Value, json};
use sqlx::PgConnection as Client;
use tokio::net::TcpListener;
use wamn_catalog::{HostRouteAuthority, HostRouteSet};
use wamn_control_provision::PlatformComponent;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
use wamn_engine::native_routes::{DeliveryError, DeliveryRequest, Source};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_identity_client::PatIssuerConfig;
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::create_or_reuse_user;
use wamn_platform_identity::org::{MemberGrants, invite_member};

#[path = "support/control_fixture.rs"]
mod control_fixture;
use control_fixture::{ORG, connect, environments, install};

/// Run the identity service with its issuer login and the shared TLS fixture.
struct IdentityServer {
    config: PatIssuerConfig,
    task: tokio::task::JoinHandle<Result<(), wamn_identity::IdentityServiceError>>,
    directory: wamn_test_infrastructure::scratch::ScratchRoot,
}

impl Drop for IdentityServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn identity_server(admin: &mut Client, admin_url: &str) -> anyhow::Result<IdentityServer> {
    let directory = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let authority = wamn_test_infrastructure::nats_tls::Authority::new(directory.path())?;
    let operator = authority.client("operator.example")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("https://{}", listener.local_addr()?);
    let password = "control-route-issuer-test-only";
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::issuer_login_sql(&endpoint, password)?,
    ))
    .execute(admin)
    .await?;
    let mut connection = url::Url::parse(admin_url)?;
    connection
        .set_username(&wamn_control_provision::identity_role::identity_issuer_login(&endpoint)?)
        .map_err(|()| anyhow::anyhow!("issuer username"))?;
    connection
        .set_password(Some(password))
        .map_err(|()| anyhow::anyhow!("issuer password"))?;
    let service = wamn_identity::IdentityService::connect(
        wamn_identity::IdentityConfig::new(&endpoint, connection.as_str())?
            .with_local_mail(directory.path().join("mail.jsonl")),
    )
    .await?;
    let tls = wamn_identity::tls_config_with_operator_ca(
        &std::fs::read(&authority.server_cert)?,
        &std::fs::read(&authority.server_key)?,
        &std::fs::read(&authority.ca)?,
    )?;
    Ok(IdentityServer {
        config: PatIssuerConfig {
            endpoint: Some(endpoint),
            client_cert: Some(operator.cert),
            client_key: Some(operator.key),
            server_ca: Some(authority.ca),
        },
        task: tokio::spawn(wamn_identity::serve(listener, service, tls)),
        directory,
    })
}

/// One org route call: the answer, or the refusal. A declared refusal reads
/// as its code and its detail.
async fn call(
    delivery: &HostRouteDelivery,
    operation: &str,
    caller: &str,
    payload: Value,
) -> Result<Value, Value> {
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
        Ok(body) => {
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
            Err(json!({"code":"permission-denied","operation":denial.operation}))
        }
        other => panic!("an org route answers or refuses: {other:?}"),
    }
}

/// How [`call`] reads a declared refusal.
fn refused(code: &str, detail: &Value) -> Value {
    json!({"code":code,"detail":detail})
}

/// The org roles, project roles and memberships of one principal.
async fn grants(admin: &mut Client, principal: &str) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT 'org', role, NULL::text FROM identity.org_roles WHERE principal_id=$1::text::uuid
         UNION ALL SELECT 'project', project, role FROM identity.project_roles WHERE principal_id=$1::text::uuid
         UNION ALL SELECT 'env', project, env FROM identity.project_env_memberships WHERE principal_id=$1::text::uuid
         UNION ALL SELECT 'member', status, updated_by::text FROM identity.org_memberships WHERE principal_id=$1::text::uuid
         ORDER BY 1,2,3"
    ).bind(principal).fetch_all(admin).await.expect("read the grants")
}

/// Compare a route page with the current registry and identity state.
async fn assert_project_list(response: &Value, admin: &mut Client) -> anyhow::Result<()> {
    let expected_projects: Vec<(String, bool)> = sqlx::query_as(
        "SELECT p.id, EXISTS(SELECT FROM registry.environment_projects ep
         JOIN registry.environments e USING(org,env)
         WHERE ep.org=p.org AND ep.project=p.id AND e.status='active')
         FROM registry.projects p WHERE org=$1 ORDER BY id",
    )
    .bind(ORG)
    .fetch_all(&mut *admin)
    .await?;
    let returned_projects = response["projects"].as_array().expect("projects");
    assert_eq!(returned_projects.len(), expected_projects.len());
    assert_eq!(
        response["total"].as_u64(),
        Some(expected_projects.len() as u64)
    );
    assert!(response["next_cursor"].is_null());
    for (returned, (name, active)) in returned_projects.iter().zip(expected_projects) {
        assert_eq!(returned["name"].as_str(), Some(name.as_str()));
        assert_eq!(
            returned["state"],
            if active { "active" } else { "inactive" }
        );
    }

    Ok(())
}

async fn assert_org_users(response: &Value, admin: &mut Client) -> anyhow::Result<()> {
    let expected_users: Vec<(String,String,String,String,bool)> = sqlx::query_as(
        "SELECT p.id::text,p.email,p.display_name,m.status,
         EXISTS(SELECT FROM identity.org_roles r WHERE r.org=m.org AND r.principal_id=p.id AND r.role='org-admin')
         FROM identity.principals p JOIN identity.org_memberships m ON m.principal_id=p.id
         WHERE m.org=$1 ORDER BY p.email"
    ).bind(ORG).fetch_all(&mut *admin).await?;
    let returned_users = response["users"].as_array().expect("users");
    assert_eq!(returned_users.len(), expected_users.len());
    assert_eq!(
        response["total"].as_u64(),
        Some(expected_users.len() as u64)
    );
    assert!(response["next_cursor"].is_null());
    for (returned, (id, email, name, status, org_admin)) in
        returned_users.iter().zip(&expected_users)
    {
        assert_eq!(returned["principal_id"].as_str(), Some(id.as_str()));
        assert_eq!(returned["email"].as_str(), Some(email.as_str()));
        assert_eq!(returned["display_name"].as_str(), Some(name.as_str()));
        assert_eq!(returned["status"].as_str(), Some(status.as_str()));
        assert_eq!(returned["org_admin"].as_bool(), Some(*org_admin));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn org_routes_write_through_the_control_login_and_refuse_a_non_admin() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("wamn_system").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let control_url = install(&mut admin, admin_url).await?;
    let (logins, _billing, _shop) = environments(&mut admin, admin_url, "org").await?;

    // The org owner, an enrolled user and a new user, written as provisioning.
    let mut ids = Vec::new();
    for (email, name) in [
        ("boss@example.test", "Boss"),
        ("cat@example.test", "Cat"),
        ("ann@example.test", "Ann"),
    ] {
        ids.push(
            create_or_reuse_user(&mut admin, email, name)
                .await?
                .principal_id,
        );
    }
    let [boss, cat, ann]: [PrincipalId; 3] = ids.try_into().expect("three users");
    invite_member(
        &mut admin,
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

    let invitation =
        wamn_platform_identity::password::issue_invitation(&mut admin, &boss_text.parse()?, &cat)
            .await?;
    wamn_platform_identity::password::enroll_password(
        &mut admin,
        &wamn_platform_identity::password::password_work(),
        &cat,
        invitation.secret(),
        wamn_platform_identity::password::Password::new("an enrolled user's password".to_owned())?,
    )
    .await?;
    let disabled = create_or_reuse_user(&mut admin, "dan@example.test", "Dan")
        .await?
        .principal_id;
    wamn_platform_identity::disable_principal(
        &mut admin,
        &disabled,
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    let identity = identity_server(&mut admin, admin_url).await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: Some(identity.config.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );

    // Every route whose declared authority is org administration refuses a non-admin.
    for (_, attachment) in HostRouteSet::Control
        .attachments()
        .filter(|(_, attachment)| attachment.route.authority == HostRouteAuthority::OrgAdmin)
    {
        let operation = attachment
            .reference
            .strip_prefix("wamn-control:")
            .expect("control reference");
        assert_eq!(
            call(&delivery, operation, ann.to_string().as_str(), json!({}))
                .await
                .unwrap_err()["code"],
            "permission-denied"
        );
    }
    let projects = call(&delivery, "project/list", boss, json!({}))
        .await
        .expect("projects");
    assert_project_list(&projects, &mut admin).await?;

    // A new user gets the membership and the mail; an enrolled user gets no
    // mail. The host refuses an email that the identity rules refuse, and
    // identity refuses a disabled user.
    let invited = call(
        &delivery,
        "user/invite",
        boss,
        json!({"email": "ann@example.test", "display_name": "Ann",
               "memberships": [{"project": "billing", "env": "dev"}]}),
    )
    .await
    .expect("invite new user");
    assert_eq!(
        invited["principal_id"].as_str(),
        Some(ann.to_string().as_str())
    );
    assert_eq!(invited["enrolled"].as_bool(), Some(false));
    assert_eq!(invited["invited"].as_bool(), Some(true));
    let mail: Vec<Value> = std::fs::read_to_string(identity.directory.path().join("mail.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        mail.iter()
            .map(|message| message["to"].as_str().expect("recipient"))
            .collect::<Vec<_>>(),
        ["ann@example.test"]
    );
    assert_eq!(
        grants(&mut admin, ann.to_string().as_str()).await,
        [
            (
                "env".to_owned(),
                "billing".to_owned(),
                Some("dev".to_owned())
            ),
            (
                "member".to_owned(),
                "active".to_owned(),
                Some(boss.to_owned())
            )
        ]
    );
    let enrolled = call(
        &delivery,
        "user/invite",
        boss,
        json!({"email": "cat@example.test", "display_name": "Cat"}),
    )
    .await
    .expect("invite enrolled user");
    assert_eq!(
        enrolled["principal_id"].as_str(),
        Some(cat.to_string().as_str())
    );
    assert_eq!(enrolled["enrolled"].as_bool(), Some(true));
    assert_eq!(enrolled["invited"].as_bool(), Some(false));
    let recipients: Vec<String> =
        std::fs::read_to_string(identity.directory.path().join("mail.jsonl"))?
            .lines()
            .map(|line| -> anyhow::Result<String> {
                let message: Value = serde_json::from_str(line)?;
                Ok(message["to"].as_str().expect("recipient").to_owned())
            })
            .collect::<Result<_, _>>()?;
    assert_eq!(recipients, ["ann@example.test"]);
    for (email, code) in [
        ("not-an-email", "invalid_input"),
        ("dan@example.test", "user_refused"),
    ] {
        let refusal = call(
            &delivery,
            "user/invite",
            boss,
            json!({"email": email, "display_name": "Nobody"}),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal["code"], code);
        assert_eq!(refusal["detail"]["field"], "email");
    }
    let users = call(&delivery, "user/list", boss, json!({}))
        .await
        .expect("users");
    assert_org_users(&users, &mut admin).await?;
    let expected_users = users["users"].as_array().expect("users");
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
        paged
            .iter()
            .map(|id| id.as_str().expect("principal"))
            .collect::<Vec<_>>(),
        expected_users
            .iter()
            .map(|user| user["principal_id"].as_str().expect("principal"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        call(
            &delivery,
            "user/list",
            boss,
            json!({"limit": expected_users.len()})
        )
        .await
        .expect("one page")["total"]
            .as_u64(),
        Some(expected_users.len() as u64)
    );

    // Org administration derives authority over every registry project and
    // environment. Revocation preserves the explicit memberships.
    let ordinary_grants = grants(&mut admin, ann.to_string().as_str()).await;
    let ann_payload = json!({"principal_id": ann.to_string().as_str()});
    let granted = call(&delivery, "org-admin/grant", boss, ann_payload.clone())
        .await
        .expect("grant");
    assert_eq!(
        granted["principal_id"].as_str(),
        Some(ann.to_string().as_str())
    );
    assert_eq!(granted["org_admin"].as_bool(), Some(true));
    let projects: Vec<(String, Option<wamn_registry::EnvName>)> = sqlx::query_as(
        "SELECT p.id,ep.env FROM registry.projects p
         LEFT JOIN registry.environment_projects ep ON ep.org=p.org AND ep.project=p.id
         WHERE p.org=$1 ORDER BY p.id,ep.env",
    )
    .bind(ORG)
    .fetch_all(&mut admin)
    .await?;
    for (project, env) in &projects {
        assert!(
            wamn_platform_identity::control::is_project_admin(
                &mut admin,
                &ann,
                ORG,
                project,
                env.as_ref()
            )
            .await?
        );
    }
    let revoked = call(&delivery, "org-admin/revoke", boss, ann_payload.clone())
        .await
        .expect("revoke");
    assert_eq!(
        revoked["principal_id"].as_str(),
        Some(ann.to_string().as_str())
    );
    assert_eq!(revoked["org_admin"].as_bool(), Some(false));
    assert_eq!(
        grants(&mut admin, ann.to_string().as_str()).await,
        ordinary_grants
    );
    for (project, env) in &projects {
        assert!(
            !wamn_platform_identity::control::is_project_admin(
                &mut admin,
                &ann,
                ORG,
                project,
                env.as_ref()
            )
            .await?
        );
    }

    // Deactivation revokes from the leaves upward, and activation restores
    // no access.
    let deactivated = call(&delivery, "user/deactivate", boss, ann_payload.clone())
        .await
        .expect("deactivate");
    assert_eq!(
        deactivated["principal_id"].as_str(),
        Some(ann.to_string().as_str())
    );
    assert_eq!(deactivated["status"], "inactive");
    assert_eq!(
        grants(&mut admin, ann.to_string().as_str()).await,
        [(
            "member".to_owned(),
            "inactive".to_owned(),
            Some(boss.to_owned())
        )]
    );
    let refusal = call(&delivery, "org-admin/grant", boss, ann_payload.clone())
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], "user_not_active");
    assert_eq!(refusal["detail"]["field"], "principal_id");
    let activated = call(&delivery, "user/activate", boss, ann_payload)
        .await
        .expect("activate");
    assert_eq!(
        activated["principal_id"].as_str(),
        Some(ann.to_string().as_str())
    );
    assert_eq!(activated["status"], "active");
    assert_eq!(
        grants(&mut admin, ann.to_string().as_str()).await,
        [(
            "member".to_owned(),
            "active".to_owned(),
            Some(boss.to_owned())
        )]
    );

    // A revoked org administrator is refused on the next request.
    sqlx::query("DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid")
        .bind(&boss)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        call(&delivery, "project/list", boss, json!({}))
            .await
            .unwrap_err()["code"],
        "permission-denied"
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn project_routes_admit_a_project_admin_and_refuse_a_covered_revoke() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_project_routes").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let control_url = install(&mut admin, admin_url).await?;
    let (logins, _billing, _shop) = environments(&mut admin, admin_url, "project").await?;

    // An org admin, a project admin of billing and a plain member.
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();
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
        let user = create_or_reuse_user(&mut admin, email, name)
            .await?
            .principal_id;
        invite_member(&mut admin, &user, ORG, &grants).await?;
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
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );
    let member = |principal: &str, env: &wamn_registry::EnvName| json!({"project": "billing", "env": env, "principal_id": principal});
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
        ("member/grant", member(ann, &"dev".parse().unwrap())),
        ("member/revoke", member(ann, &"dev".parse().unwrap())),
        ("project-admin/grant", project_admin(ann)),
        ("project-admin/revoke", project_admin(ann)),
    ] {
        assert_eq!(
            call(&delivery, operation, ann, payload).await.unwrap_err()["code"],
            "permission-denied"
        );
    }
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "shop"})
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
    );
    // A project admin reads the org's users, and holds no other org route.
    let users = call(&delivery, "user/list", cat, json!({}))
        .await
        .expect("org users");
    assert_org_users(&users, &mut admin).await?;
    assert_eq!(
        call(&delivery, "project/list", cat, json!({}))
            .await
            .unwrap_err()["code"],
        "permission-denied"
    );
    let environments = call(
        &delivery,
        "environment/list",
        cat,
        json!({"project": "billing"}),
    )
    .await
    .expect("project environments");
    let expected_environments: Vec<(String, wamn_registry::EnvName)> = sqlx::query_as(
        "SELECT ep.env,e.status FROM registry.environment_projects ep
         JOIN registry.environments e USING(org,env)
         WHERE ep.org=$1 AND ep.project=$2 ORDER BY ep.env",
    )
    .bind(ORG)
    .bind("billing")
    .fetch_all(&mut admin)
    .await?;
    let returned_environments = environments["environments"]
        .as_array()
        .expect("environments");
    assert_eq!(returned_environments.len(), expected_environments.len());
    assert_eq!(
        environments["total"].as_u64(),
        Some(expected_environments.len() as u64)
    );
    assert!(environments["next_cursor"].is_null());
    for (returned, (name, status)) in returned_environments.iter().zip(expected_environments) {
        assert_eq!(returned["name"].as_str(), Some(name.as_str()));
        assert_eq!(returned["state"].as_str(), Some(status.as_str()));
    }
    // A status read names an environment of the project before it reads
    // Kubernetes.
    let refusal = call(
        &delivery,
        "environment/status",
        cat,
        json!({"project": "billing", "env": "prod"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "environment_not_found");
    assert_eq!(refusal["detail"]["field"], "env");

    // A membership goes to an active org member in an environment of the
    // project.
    let response = call(
        &delivery,
        "member/grant",
        cat,
        member(ann, &"dev".parse().unwrap()),
    )
    .await
    .expect("route response");
    assert_eq!(response["principal_id"].as_str(), Some(ann));
    assert_eq!(response["project"], "billing");
    assert_eq!(response["env"], "dev");
    assert_eq!(response["member"].as_bool(), Some(true));
    assert_eq!(
        grants(&mut admin, ann).await,
        [
            (
                "env".to_owned(),
                "billing".to_owned(),
                Some("dev".to_owned())
            ),
            (
                "member".to_owned(),
                "active".to_owned(),
                Some(provisioning.to_owned())
            )
        ]
    );
    let refusal = call(
        &delivery,
        "member/grant",
        cat,
        member(ann, &"prod".parse().unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "environment_not_found");
    assert_eq!(refusal["detail"]["field"], "env");
    let members = call(&delivery, "member/list", cat, json!({"project": "billing"}))
        .await
        .expect("project members");
    let expected_members =
        wamn_platform_identity::org::project_members(&mut admin, ORG, "billing").await?;
    let returned_members = members["members"].as_array().expect("members");
    assert_eq!(returned_members.len(), expected_members.len());
    assert_eq!(
        members["total"].as_u64(),
        Some(expected_members.len() as u64)
    );
    assert!(members["next_cursor"].is_null());
    for (returned, expected) in returned_members.iter().zip(&expected_members) {
        assert_eq!(
            returned["principal_id"].as_str(),
            Some(expected.principal_id.as_str())
        );
        assert_eq!(returned["email"].as_str(), Some(expected.email.as_str()));
        assert_eq!(
            returned["display_name"].as_str(),
            Some(expected.display_name.as_str())
        );
        assert_eq!(returned["org_admin"].as_bool(), Some(expected.org_admin));
        assert_eq!(
            returned["project_admin"].as_bool(),
            Some(expected.project_admin)
        );
        assert_eq!(
            returned["environments"]
                .as_array()
                .expect("environments")
                .iter()
                .map(|env| env.as_str().expect("environment"))
                .collect::<Vec<_>>(),
            expected
                .environments
                .iter()
                .map(wamn_registry::EnvName::as_str)
                .collect::<Vec<_>>()
        );
    }

    // An environment filters in the query, before the limit.
    let page = call(
        &delivery,
        "member/list",
        cat,
        json!({"project": "billing", "environment": "dev", "limit": 2}),
    )
    .await
    .expect("a filtered page");
    let expected_page: Vec<_> = expected_members
        .iter()
        .filter(|member| {
            member
                .environments
                .iter()
                .any(|env| env == &"dev".parse().unwrap())
        })
        .take(2)
        .collect();
    let returned_page = page["members"].as_array().expect("members");
    assert_eq!(returned_page.len(), expected_page.len());
    for (returned, expected) in returned_page.iter().zip(expected_page) {
        assert_eq!(
            returned["principal_id"].as_str(),
            Some(expected.principal_id.as_str())
        );
        assert_eq!(returned["email"].as_str(), Some(expected.email.as_str()));
        assert_eq!(
            returned["environments"]
                .as_array()
                .expect("environments")
                .iter()
                .map(|env| env.as_str().expect("environment"))
                .collect::<Vec<_>>(),
            expected
                .environments
                .iter()
                .map(wamn_registry::EnvName::as_str)
                .collect::<Vec<_>>()
        );
    }
    assert!(page["next_cursor"].is_string());
    let refusal = call(
        &delivery,
        "member/list",
        cat,
        json!({"project": "billing", "cursor": page["next_cursor"]}),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "invalid_input");
    assert_eq!(refusal["detail"]["field"], "cursor");
    let empty = call(
        &delivery,
        "member/list",
        cat,
        json!({"project": "billing", "environment": "prod"}),
    )
    .await
    .expect("members of an absent environment");
    assert!(empty["members"].as_array().expect("members").is_empty());
    assert_eq!(empty["total"].as_u64(), Some(0));
    assert!(empty["next_cursor"].is_null());
    let refusal = call(
        &delivery,
        "member/list",
        cat,
        json!({"project": "billing", "environment": null}),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "invalid_input");
    assert_eq!(refusal["detail"]["field"], "environment");

    // A covering grant refuses the revoke below it.
    let refusal = call(
        &delivery,
        "member/revoke",
        boss,
        member(cat, &"dev".parse().unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "admin_covered");
    assert_eq!(refusal["detail"]["field"], "principal_id");
    let refusal = call(
        &delivery,
        "member/revoke",
        cat,
        member(boss, &"dev".parse().unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "admin_covered");
    assert_eq!(refusal["detail"]["field"], "principal_id");
    let refusal = call(&delivery, "project-admin/revoke", cat, project_admin(boss))
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], "admin_covered");
    assert_eq!(refusal["detail"]["field"], "principal_id");

    // project-admin covers every environment of the project, and its revoke
    // leaves the memberships.
    let response = call(&delivery, "project-admin/grant", cat, project_admin(ann))
        .await
        .expect("route response");
    assert_eq!(response["principal_id"].as_str(), Some(ann));
    assert_eq!(response["project"], "billing");
    assert_eq!(response["project_admin"].as_bool(), Some(true));
    assert_eq!(
        grants(&mut admin, ann).await,
        [
            (
                "env".to_owned(),
                "billing".to_owned(),
                Some("dev".to_owned())
            ),
            (
                "member".to_owned(),
                "active".to_owned(),
                Some(provisioning.to_owned())
            ),
            (
                "project".to_owned(),
                "billing".to_owned(),
                Some("project-admin".to_owned())
            ),
        ]
    );
    let response = call(&delivery, "project-admin/revoke", cat, project_admin(ann))
        .await
        .expect("route response");
    assert_eq!(response["principal_id"].as_str(), Some(ann));
    assert_eq!(response["project"], "billing");
    assert_eq!(response["project_admin"].as_bool(), Some(false));
    let response = call(
        &delivery,
        "member/revoke",
        cat,
        member(ann, &"dev".parse().unwrap()),
    )
    .await
    .expect("route response");
    assert_eq!(response["principal_id"].as_str(), Some(ann));
    assert_eq!(response["project"], "billing");
    assert_eq!(response["env"], "dev");
    assert_eq!(response["member"].as_bool(), Some(false));
    assert_eq!(
        grants(&mut admin, ann).await,
        [(
            "member".to_owned(),
            "active".to_owned(),
            Some(provisioning.to_owned())
        )]
    );

    // A revoked project admin is refused on the next request.
    sqlx::query("DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid")
        .bind(&cat)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project": "billing"})
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

/// The application rows of one principal in one project database.
async fn application_rows(target: &mut Client, principal: &str) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT 'user', NULL::text FROM platform.\"user\" WHERE id=$1::text::uuid
         UNION ALL SELECT 'role', role_name FROM platform.user_role WHERE user_id=$1::text::uuid ORDER BY 1,2"
    ).bind(principal).fetch_all(target).await.expect("read application rows")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn admin_writes_reach_the_application_rows_of_every_environment() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_application_rows").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let control_url = install(&mut admin, admin_url).await?;
    let (logins, mut billing, mut shop) =
        environments(&mut admin, admin_url, "application").await?;
    let mut ids = Vec::new();
    for (email, name, org_admin) in [
        ("boss@example.test", "Boss", true),
        ("ann@example.test", "Ann", false),
    ] {
        let user = create_or_reuse_user(&mut admin, email, name)
            .await?
            .principal_id;
        let grants = MemberGrants {
            org_admin,
            ..MemberGrants::default()
        };
        invite_member(&mut admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, ann]: [PrincipalId; 2] = ids.try_into().expect("two users");
    let (boss_text, ann_text) = (boss.to_string(), ann.to_string());
    let (boss, ann) = (boss_text.as_str(), ann_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );
    let ann_payload = json!({"principal_id": ann});
    let billing_admin = json!({"project": "billing", "principal_id": ann});
    let billing_member = json!({"project": "billing", "env": "dev", "principal_id": ann});

    // org-admin writes the user row and admin in every environment, after its
    // system rows; its revoke removes admin first and keeps the user row.
    {
        let observed = &(call(&delivery, "org-admin/grant", boss, ann_payload.clone()).await)
            .expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["org_admin"], serde_json::json!(true));
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [
            ("role".to_owned(), Some("admin".to_owned())),
            ("user".to_owned(), None)
        ]
    );
    assert_eq!(
        application_rows(&mut shop, ann).await,
        [
            ("role".to_owned(), Some("admin".to_owned())),
            ("user".to_owned(), None)
        ]
    );
    {
        let observed = &(call(
            &delivery,
            "project-admin/revoke",
            boss,
            billing_admin.clone(),
        )
        .await)
            .expect_err("route refuses");
        assert_eq!(observed["code"], serde_json::json!("admin_covered"));
        assert_eq!(
            observed["detail"]["field"],
            serde_json::json!("principal_id")
        );
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [
            ("role".to_owned(), Some("admin".to_owned())),
            ("user".to_owned(), None)
        ],
        "a refused revoke changes no application row"
    );
    {
        let observed = &(call(&delivery, "org-admin/revoke", boss, ann_payload.clone()).await)
            .expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["org_admin"], serde_json::json!(false));
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [("user".to_owned(), None)]
    );
    assert_eq!(
        application_rows(&mut shop, ann).await,
        [("user".to_owned(), None)]
    );

    // project-admin covers its project's environments only.
    {
        let observed = &(call(
            &delivery,
            "project-admin/grant",
            boss,
            billing_admin.clone(),
        )
        .await)
            .expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["project"], serde_json::json!("billing"));
        assert_eq!(observed["project_admin"], serde_json::json!(true));
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [
            ("role".to_owned(), Some("admin".to_owned())),
            ("user".to_owned(), None)
        ]
    );
    assert_eq!(
        application_rows(&mut shop, ann).await,
        [("user".to_owned(), None)]
    );
    {
        let observed = &(call(&delivery, "project-admin/revoke", boss, billing_admin).await)
            .expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["project"], serde_json::json!("billing"));
        assert_eq!(observed["project_admin"], serde_json::json!(false));
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [("user".to_owned(), None)]
    );

    // A membership revoke removes the user row, and a grant writes it again.
    {
        let observed = &(call(&delivery, "member/revoke", boss, billing_member.clone()).await)
            .expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["project"], serde_json::json!("billing"));
        assert_eq!(observed["env"], serde_json::json!("dev"));
        assert_eq!(observed["member"], serde_json::json!(false));
    };
    assert!(application_rows(&mut billing, ann).await.is_empty());
    {
        let observed =
            &(call(&delivery, "member/grant", boss, billing_member).await).expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["project"], serde_json::json!("billing"));
        assert_eq!(observed["env"], serde_json::json!("dev"));
        assert_eq!(observed["member"], serde_json::json!(true));
    };
    assert_eq!(
        application_rows(&mut billing, ann).await,
        [("user".to_owned(), None)]
    );

    // A failed projection names completed environments after system authority commits.
    let shop_key = logins.join(wamn_control_provision::control_administration_key(
        "shop",
        &"dev".parse().unwrap(),
    ));
    let shop_login = std::fs::read_to_string(&shop_key)?;
    std::fs::write(&shop_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    {
        let observed = &(call(&delivery, "user/deactivate", boss, ann_payload.clone()).await)
            .expect_err("route refuses");
        assert_eq!(
            observed["code"],
            serde_json::json!("application_write_incomplete")
        );
        assert_eq!(
            observed["detail"]["environment"],
            serde_json::json!("shop/dev")
        );
        assert_eq!(
            observed["detail"]["completed"]
                .as_array()
                .expect("array")
                .len(),
            1
        );
        assert_eq!(
            observed["detail"]["completed"][0],
            serde_json::json!("billing/dev")
        );
    };
    assert!(application_rows(&mut billing, ann).await.is_empty());
    assert_eq!(
        application_rows(&mut shop, ann).await,
        [("user".to_owned(), None)]
    );
    assert_eq!(
        grants(&mut admin, ann).await,
        [(
            "member".to_owned(),
            "inactive".to_owned(),
            Some(boss.to_owned())
        )],
        "committed deactivation remains authoritative after a projection failure"
    );

    // With the login back, the deactivation completes.
    std::fs::write(&shop_key, shop_login)?;
    {
        let observed =
            &(call(&delivery, "user/deactivate", boss, ann_payload).await).expect("route succeeds");
        assert_eq!(observed["principal_id"], serde_json::json!(ann));
        assert_eq!(observed["status"], serde_json::json!("inactive"));
    };
    assert!(application_rows(&mut shop, ann).await.is_empty());
    assert_eq!(
        grants(&mut admin, ann).await,
        [(
            "member".to_owned(),
            "inactive".to_owned(),
            Some(boss.to_owned())
        )]
    );
    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

/// The system status of each environment of the org, and the status row of
/// each project database, or `none` without one.
async fn statuses(
    admin: &mut Client,
    billing: &mut Client,
    shop: &mut Client,
) -> (String, Option<String>, Option<String>) {
    let status =
        sqlx::query_scalar("SELECT status FROM registry.environments WHERE org=$1 AND env='dev'")
            .bind(ORG)
            .fetch_one(admin)
            .await
            .unwrap();
    let billing = sqlx::query_scalar("SELECT status FROM platform.environment")
        .fetch_optional(billing)
        .await
        .unwrap();
    let shop = sqlx::query_scalar("SELECT status FROM platform.environment")
        .fetch_optional(shop)
        .await
        .unwrap();
    (status, billing, shop)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn status_routes_mirror_the_status_into_each_environment_leaves_first() -> anyhow::Result<()>
{
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_status").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let control_url = install(&mut admin, admin_url).await?;
    let (logins, mut billing, mut shop) = environments(&mut admin, admin_url, "status").await?;
    let mut ids = Vec::new();
    for (email, name, org_admin) in [
        ("boss@example.test", "Boss", true),
        ("ann@example.test", "Ann", false),
    ] {
        let user = create_or_reuse_user(&mut admin, email, name)
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
        invite_member(&mut admin, &user, ORG, &grants).await?;
        ids.push(user);
    }
    let [boss, ann]: [PrincipalId; 2] = ids.try_into().expect("two users");
    let (boss_text, ann_text) = (boss.to_string(), ann.to_string());
    let (boss, ann) = (boss_text.as_str(), ann_text.as_str());
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            administration: Some(logins.clone()),
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );
    let billing_dev = json!({"project": "billing", "env": "dev"});
    let shop_dev = json!({"project": "shop", "env": "dev"});
    assert_eq!(
        statuses(&mut admin, &mut billing, &mut shop).await,
        ("active".into(), None, None)
    );
    assert_eq!(
        call(
            &delivery,
            "environment/inactivate",
            ann,
            billing_dev.clone()
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
    );
    assert_eq!(
        call(
            &delivery,
            "environment/inactivate",
            boss,
            json!({"project":"billing","env":"prod"})
        )
        .await
        .unwrap_err()["code"],
        "environment_not_found"
    );
    assert_eq!(
        call(
            &delivery,
            "project/inactivate",
            boss,
            json!({"project":"nope"})
        )
        .await
        .unwrap_err()["code"],
        "project_not_found"
    );

    let expected_projects =
        sqlx::query_scalar::<_, String>("SELECT id FROM registry.projects WHERE org=$1")
            .bind(ORG)
            .fetch_all(&mut admin)
            .await?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
    let expected_environments = sqlx::query_scalar::<_, String>(
        "SELECT env FROM registry.environment_projects WHERE org=$1 AND project='shop'",
    )
    .bind(ORG)
    .fetch_all(&mut admin)
    .await?
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    // Lifecycle belongs to the environment. Every attached project receives the same status.
    for (route, payload, expected) in [
        ("environment/inactivate", billing_dev.clone(), "inactive"),
        ("project/inactivate", json!({"project":"shop"}), "inactive"),
        ("environment/activate", billing_dev.clone(), "active"),
    ] {
        let reply = call(&delivery, route, boss, payload).await.unwrap();
        assert_eq!(reply["status"], expected);
        assert_eq!(
            statuses(&mut admin, &mut billing, &mut shop).await,
            (
                expected.into(),
                Some(expected.into()),
                Some(expected.into())
            )
        );
        let projects = call(&delivery, "project/list", boss, json!({}))
            .await
            .unwrap();
        assert_eq!(
            projects["projects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|project| project["name"].as_str().unwrap().to_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            expected_projects
        );
        assert!(
            projects["projects"]
                .as_array()
                .unwrap()
                .iter()
                .all(|project| project["state"] == expected)
        );
        let environments = call(
            &delivery,
            "environment/list",
            boss,
            json!({"project":"shop"}),
        )
        .await
        .unwrap();
        assert_eq!(
            environments["environments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|environment| environment["name"].as_str().unwrap().to_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            expected_environments
        );
        assert!(
            environments["environments"]
                .as_array()
                .unwrap()
                .iter()
                .all(|environment| environment["state"] == expected)
        );
    }
    call(
        &delivery,
        "environment/inactivate",
        boss,
        billing_dev.clone(),
    )
    .await
    .unwrap();

    // Activation commits the registry first. A later projection can fail independently.
    let shop_key = logins.join(wamn_control_provision::control_administration_key(
        "shop",
        &"dev".parse().unwrap(),
    ));
    let shop_login = std::fs::read_to_string(&shop_key)?;
    std::fs::write(&shop_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    let refusal = call(&delivery, "environment/activate", boss, shop_dev.clone())
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], "application_write_incomplete");
    assert_eq!(
        statuses(&mut admin, &mut billing, &mut shop).await,
        (
            "active".into(),
            Some("active".into()),
            Some("inactive".into())
        )
    );
    std::fs::write(&shop_key, &shop_login)?;
    call(&delivery, "environment/activate", boss, shop_dev)
        .await
        .unwrap();

    // Inactivation commits the registry only after all project writes succeed.
    let billing_key = logins.join(wamn_control_provision::control_administration_key(
        "billing",
        &"dev".parse().unwrap(),
    ));
    std::fs::write(&billing_key, "postgres://nobody:wrong@127.0.0.1:1/none")?;
    let refusal = call(&delivery, "environment/inactivate", boss, billing_dev)
        .await
        .unwrap_err();
    assert_eq!(refusal["code"], "application_write_incomplete");
    assert_eq!(
        statuses(&mut admin, &mut billing, &mut shop).await,
        (
            "active".into(),
            Some("active".into()),
            Some("active".into())
        )
    );

    sqlx::query("WITH parent_environment AS ( \
             INSERT INTO registry.environments (org, env, policy_name, status) \
             SELECT org, 'other', policy_name, 'inactive' FROM registry.environments \
             WHERE org=$1 AND env='dev' \
         ) INSERT INTO registry.environment_projects (org, env, project, secret_name, instance_suffix) \
         SELECT org, 'other', project, secret_name, instance_suffix \
         FROM registry.environment_projects WHERE org=$1 AND project='billing' AND env='dev'")
        .bind(ORG).execute(&mut admin).await?;
    let projects = call(&delivery, "project/list", boss, json!({}))
        .await
        .unwrap();
    assert!(
        projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|project| project["state"] == "active")
    );
    let environments = call(
        &delivery,
        "environment/list",
        boss,
        json!({"project":"billing"}),
    )
    .await
    .unwrap();
    for environment in environments["environments"].as_array().unwrap() {
        let expected: String =
            sqlx::query_scalar("SELECT status FROM registry.environments WHERE org=$1 AND env=$2")
                .bind(ORG)
                .bind(environment["name"].as_str().unwrap())
                .fetch_one(&mut admin)
                .await?;
        assert_eq!(environment["state"], expected);
    }
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn project_create_writes_a_project_that_every_org_admin_administers() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_project_create").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let control_url = install(&mut admin, admin_url).await?;
    let (logins, _billing, _shop) = environments(&mut admin, admin_url, "ledger").await?;

    // Two org admins and a project admin of billing.
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
        let user = create_or_reuse_user(&mut admin, email, name)
            .await?
            .principal_id;
        invite_member(&mut admin, &user, ORG, &grants).await?;
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
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );

    // Only an org admin creates a project, and the id is a project slug.
    assert_eq!(
        call(
            &delivery,
            "project/create",
            cat,
            json!({"project": "ledger"})
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
    );
    for project in ["Ledger", "wamn-ledger", "led--ger"] {
        let refusal = call(
            &delivery,
            "project/create",
            boss,
            json!({"project": project}),
        )
        .await
        .unwrap_err();
        assert_eq!(
            refusal["code"], "invalid_input",
            "{project} is not a project id"
        );
        assert_eq!(refusal["detail"]["field"], "project");
    }
    let refusal = call(
        &delivery,
        "project/create",
        boss,
        json!({"project": "billing"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "project_exists");
    assert_eq!(refusal["detail"]["field"], "project");
    let response = call(
        &delivery,
        "project/create",
        boss,
        json!({"project": "ledger"}),
    )
    .await
    .expect("route response");
    assert_eq!(response["project"], "ledger");
    let refusal = call(
        &delivery,
        "project/create",
        dee,
        json!({"project": "ledger"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal["code"], "project_exists", "ledger exists");
    assert_eq!(refusal["detail"]["field"], "project");

    // The project has no environment, and each org admin, and only they,
    // holds project-admin in it.
    let projects = call(&delivery, "project/list", dee, json!({}))
        .await
        .expect("an org admin lists the projects");
    assert_project_list(&projects, &mut admin).await?;
    let listed = call(
        &delivery,
        "environment/list",
        dee,
        json!({"project": "ledger"}),
    )
    .await
    .expect("an org admin lists the environments of ledger");
    let expected_environments: Vec<wamn_registry::EnvName> = sqlx::query_scalar(
        "SELECT env FROM registry.environment_projects WHERE org=$1 AND project=$2 ORDER BY env",
    )
    .bind(ORG)
    .bind("ledger")
    .fetch_all(&mut admin)
    .await?;
    assert!(
        expected_environments.is_empty(),
        "a new project has no environment"
    );
    let returned_environments = listed["environments"].as_array().expect("environments");
    assert_eq!(
        returned_environments
            .iter()
            .map(|row| row["name"].as_str().expect("name"))
            .collect::<Vec<_>>(),
        expected_environments
            .iter()
            .map(wamn_registry::EnvName::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        listed["total"].as_u64(),
        Some(expected_environments.len() as u64)
    );
    assert!(listed["next_cursor"].is_null());
    let admins: Vec<String> = sqlx::query_scalar(
        "SELECT principal_id::text FROM identity.org_roles WHERE org=$1 AND role='org-admin' ORDER BY principal_id"
    ).bind(ORG).fetch_all(&mut admin).await?;
    for principal in admins {
        assert!(
            wamn_platform_identity::control::is_project_admin(
                &mut admin,
                &principal.parse()?,
                ORG,
                "ledger",
                None
            )
            .await?
        );
    }
    assert!(
        !wamn_platform_identity::control::is_project_admin(
            &mut admin,
            &cat.parse()?,
            ORG,
            "ledger",
            None
        )
        .await?
    );
    assert_eq!(
        call(
            &delivery,
            "environment/list",
            cat,
            json!({"project":"ledger"})
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
    );

    let _ = std::fs::remove_dir_all(&logins);
    Ok(())
}

#[derive(Debug)]
struct FixedStatus;

#[async_trait::async_trait]
impl wamn_execution_host::environment_status::StatusSource for FixedStatus {
    async fn read(&self, _: &str, _: &str, _: &wamn_registry::EnvName) -> anyhow::Result<Value> {
        Ok(
            json!({"digest": "sha256:live", "rollout": {"replicas": 1}, "workloads": [], "live": []}),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn environment_status_counts_are_targeted_and_optional() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("control_status_counts").await?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&mut admin, database.url()).await?;
    let (logins, mut billing, mut shop) =
        environments(&mut admin, database.url(), "counts").await?;
    let principal = create_or_reuse_user(&mut admin, "counts@example.test", "Counts")
        .await?
        .principal_id;
    invite_member(
        &mut admin,
        &principal,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO platform.role (name) VALUES ('first'), ('second');
         INSERT INTO platform.\"user\" (id, type, email) VALUES
           ('00000000-0000-0000-0000-000000000001', 'user', 'one@example.test'),
           ('00000000-0000-0000-0000-000000000002', 'user', 'two@example.test'),
           ('00000000-0000-0000-0000-000000000003', 'service', 'service@example.test');
         INSERT INTO platform.user_role (user_id, role_name) VALUES
           ('00000000-0000-0000-0000-000000000001', 'first'),
           ('00000000-0000-0000-0000-000000000001', 'second'),
           ('00000000-0000-0000-0000-000000000003', 'first');")
            .to_string(),
    ))
    .execute(&mut billing)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO platform.role (name) VALUES ('ignored');
        INSERT INTO platform.\"user\" (id, type, email) VALUES
        ('00000000-0000-0000-0000-000000000004', 'user', 'other@example.test');
        INSERT INTO platform.user_role (user_id, role_name) VALUES
        ('00000000-0000-0000-0000-000000000004', 'ignored');")
            .to_string(),
    ))
    .execute(&mut shop)
    .await
    .map(drop)?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(FixedStatus),
            administration: Some(logins.clone()),
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    );
    let payload = json!({"project": "billing", "env": "dev"});
    let runtime = |answer: &Value| {
        assert_eq!(answer["digest"].as_str(), Some("sha256:live"));
        assert_eq!(answer["rollout"]["replicas"].as_u64(), Some(1));
        assert!(
            answer["workloads"]
                .as_array()
                .expect("workloads")
                .is_empty()
        );
        assert!(answer["live"].as_array().expect("live runs").is_empty());
    };
    for (project, connection) in [("billing", &mut billing), ("shop", &mut shop)] {
        let (users, roles): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(DISTINCT r.user_id) FROM platform.user_role r
                     JOIN platform.\"user\" u ON u.id=r.user_id WHERE u.type='user'),
                    (SELECT count(*) FROM platform.role)",
        )
        .fetch_one(connection)
        .await?;
        let memberships = wamn_platform_identity::org::project_members(&mut admin, ORG, project)
            .await?
            .iter()
            .filter(|member| {
                member
                    .environments
                    .iter()
                    .any(|env| env == &"dev".parse().unwrap())
            })
            .count();
        let answer = call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            json!({"project":project,"env":"dev"}),
        )
        .await
        .expect("status");
        runtime(&answer);
        assert_eq!(answer["users_with_role"].as_i64(), Some(users));
        assert_eq!(answer["roles"].as_i64(), Some(roles));
        assert_eq!(answer["memberships"].as_u64(), Some(memberships as u64));
    }
    sqlx::raw_sql("DELETE FROM platform.role")
        .execute(&mut shop)
        .await
        .map(drop)?;
    let answer = call(
        &delivery,
        "environment/status",
        principal.to_string().as_str(),
        json!({"project":"shop","env":"dev"}),
    )
    .await
    .expect("empty counts");
    runtime(&answer);
    assert_eq!(answer["users_with_role"].as_u64(), Some(0));
    assert_eq!(answer["roles"].as_u64(), Some(0));
    // Count queries can fail without refusing independent runtime state.
    sqlx::raw_sql("ALTER TABLE platform.role RENAME TO unavailable_roles")
        .execute(&mut billing)
        .await
        .map(drop)?;
    let memberships = wamn_platform_identity::org::project_members(&mut admin, ORG, "billing")
        .await?
        .iter()
        .filter(|member| {
            member
                .environments
                .iter()
                .any(|env| env == &"dev".parse().unwrap())
        })
        .count();
    for missing_credential in [false, true] {
        if missing_credential {
            std::fs::remove_file(
                logins.join(wamn_control_provision::control_administration_key(
                    "billing",
                    &"dev".parse().unwrap(),
                )),
            )?;
        }
        let answer = call(
            &delivery,
            "environment/status",
            principal.to_string().as_str(),
            payload.clone(),
        )
        .await
        .expect("runtime status survives unavailable counts");
        runtime(&answer);
        assert!(answer["users_with_role"].is_null());
        assert!(answer["roles"].is_null());
        assert_eq!(answer["memberships"].as_u64(), Some(memberships as u64));
    }
    std::fs::remove_dir_all(logins)?;
    Ok(())
}

async fn assert_definer_grantees(client: &mut Client) -> anyhow::Result<()> {
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
    let functions = sqlx::query("SELECT p.oid, n.nspname || '.' || p.proname || '(' || oidvectortypes(p.proargtypes) || ')' AS name, p.proconfig, pg_get_userbyid(p.proowner) AS owner, EXISTS (SELECT FROM aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) a WHERE a.grantee=0 AND a.privilege_type='EXECUTE') AS public_execute, EXISTS (SELECT FROM aclexplode(coalesce(p.proacl,acldefault('f',p.proowner))) a WHERE a.grantee<>p.proowner AND a.is_grantable) AS grantable FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE p.prosecdef")
        .fetch_all(&mut *client).await?;
    for function in functions {
        let name: String = function.get("name");
        let allowed = ALLOWED
            .iter()
            .find(|(signature, _)| *signature == name)
            .unwrap_or_else(|| panic!("unreviewed privileged function: {name}"));
        let effective = sqlx::query("SELECT r.rolname::text FROM pg_roles r WHERE NOT r.rolsuper AND has_function_privilege(r.oid,$1::text::regprocedure,'EXECUTE') AND NOT pg_has_role(r.oid,(SELECT proowner FROM pg_proc WHERE oid=$1::text::regprocedure),'USAGE') AND NOT EXISTS (SELECT FROM pg_roles a WHERE a.rolname=ANY($2::text[]) AND pg_has_role(r.oid,a.oid,'USAGE'))")
            .bind(&name).bind(&allowed.1).fetch_all(&mut *client).await?;
        assert!(
            effective.is_empty(),
            "unexpected effective privilege on {name}: {effective:?}"
        );
        assert_eq!(function.get::<String, _>("owner"), "wamn_system", "{name}");
        let configuration: Vec<String> = function.get("proconfig");
        let search_path = configuration
            .iter()
            .filter_map(|setting| setting.split_once('='))
            .find(|(key, _)| *key == "search_path")
            .map(|(_, value)| value);
        assert_eq!(search_path, Some("pg_catalog"), "{name}");
        assert!(!function.get::<bool, _>("public_execute"), "{name}");
        assert!(!function.get::<bool, _>("grantable"), "{name}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres, nats, env:WAMN_NATIVE_C_NATS_BIN"]
async fn connection_routes_refuse_mismatches_and_project_only_enabled_rows() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("connection_routes").await?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&mut admin, database.url()).await?;
    let (logins, mut billing, mut shop) =
        environments(&mut admin, database.url(), "connections").await?;
    let mut writer = connect(&control_url).await?;
    sqlx::raw_sql("SET TIME ZONE 'America/New_York'")
        .execute(&mut writer)
        .await
        .map(drop)?;
    assert!(
        sqlx::raw_sql("SET ROLE wamn_control")
            .execute(&mut writer)
            .await
            .map(drop)
            .is_err(),
        "the control generation inherits its grants without permission to assume the ACL role"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{}'::text::uuid,'','fixture','admin:fixture'); UPDATE registry.meta SET platform_domain='example.test'",
            PlatformComponent::Provisioning.principal_id()
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    assert_definer_grantees(&mut admin).await?;
    assert_definer_grantees(&mut billing).await?;
    assert_definer_grantees(&mut shop).await?;
    assert!(
        sqlx::query("SELECT to_regprocedure('catalog.connection_is_referenced(text)') IS NULL")
            .fetch_one(&mut billing)
            .await?
            .get::<bool, _>(0)
    );
    let boss = create_or_reuse_user(&mut admin, "connection-admin@example.test", "Admin")
        .await?
        .principal_id;
    invite_member(
        &mut admin,
        &boss,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    let (_nats_server, nats) = wamn_capability_authority::auth_cache::test_util::nats().await;
    let bucket = wamn_broker::auth_changes::auth_bucket(&nats).await?;
    let delivery = HostRouteDelivery::new(
        Arc::new(LoadedRelease::control_root()),
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: Some(logins.clone()),
            control: wamn_postgres::pool(&control_url, 4).await?,
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::new(nats),
        },
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {} UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning)))).execute(&mut admin).await.map(drop)?;
    let external = json!({"project":"billing","name":"erp"});
    assert_eq!(
        call(
            &delivery,
            "external-system/write",
            "00000000-0000-4000-8000-000000000099",
            external.clone()
        )
        .await
        .unwrap_err()["code"],
        "permission-denied"
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
    for (client, count) in [(&mut billing, 1_i64), (&mut shop, 0)] {
        assert_eq!(
            sqlx::query("SELECT count(*) FROM platform.connection")
                .fetch_one(&mut *client)
                .await?
                .get::<i64, _>(0),
            count
        );
        for table in ["external_system", "integration", "connection"] {
            assert!(!sqlx::query("SELECT has_table_privilege('wamn_app',$1,'INSERT,UPDATE,DELETE,TRUNCATE') OR has_any_column_privilege('wamn_app',$1,'INSERT,UPDATE')").bind(&format!("platform.{table}")).fetch_one(&mut *client).await?.get::<bool, _>(0));
        }
        for table in ["catalog.release_connection", "catalog.connection_binding"] {
            assert!(!sqlx::query("SELECT has_table_privilege('wamn_administration',$1,'INSERT,UPDATE,DELETE,TRUNCATE') OR has_any_column_privilege('wamn_administration',$1,'INSERT,UPDATE')").bind(&table).fetch_one(&mut *client).await?.get::<bool, _>(0));
        }
    }
    let identity: String = sqlx::query("SELECT id::text FROM platform.connection")
        .fetch_one(&mut billing)
        .await?
        .get(0);
    sqlx::query("UPDATE registry.integration SET auth_type='oauth2_client_credentials' WHERE name='erp' AND project='billing'").execute(&mut admin).await.map(|result| result.rows_affected())?;
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
    sqlx::query("INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) VALUES($1::text::uuid,$2,$3,$4,'file:fixture','2099-01-01T00:00:00Z','failing',clock_timestamp())").bind(&identity).bind(&b"private-token-bytes".as_slice()).bind(&b"private-nonce".as_slice()).bind(&b"private-wrapped-key".as_slice()).execute(&mut admin).await.map(|result| result.rows_affected())?;
    for status in ["failing", "ok"] {
        sqlx::query(
            "UPDATE identity.oauth_token SET refresh_status=$1 WHERE connection_id=$2::text::uuid",
        )
        .bind(&status)
        .bind(&identity)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
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
    sqlx::query("DELETE FROM identity.oauth_token WHERE connection_id=$1::text::uuid")
        .bind(&identity)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::query(
        "UPDATE registry.integration SET auth_type='none' WHERE name='erp' AND project='billing'",
    )
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("CREATE TABLE public.connection_reference(connection_id uuid REFERENCES platform.connection(id))").execute(&mut billing).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO public.connection_reference VALUES($1::text::uuid)")
        .bind(&identity)
        .execute(&mut billing)
        .await
        .map(|result| result.rows_affected())?;
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
        sqlx::query("SELECT status FROM platform.connection")
            .fetch_one(&mut billing)
            .await?
            .get::<String, _>(0),
        "disabled"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "BEGIN; {}",
            wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning)
        ))
            .to_string(),
    ))
    .execute(&mut billing)
    .await
    .map(drop)?;
    // The sha256 of the canonical bytes `{}`, which catalog.releases tests.
    let digest = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        .parse::<wamn_digest::Digest>()
        .unwrap();
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)").bind(&wamn_registry::project_env_key(ORG,"billing", &"dev".parse().unwrap())).bind(&digest).bind(&b"{}".as_slice()).execute(&mut billing).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3::text::uuid,1)").bind(&wamn_registry::project_env_key(ORG,"billing", &"dev".parse().unwrap())).bind(&digest).bind(&identity).execute(&mut billing).await.map(|result| result.rows_affected())?;
    sqlx::raw_sql("COMMIT")
        .execute(&mut billing)
        .await
        .map(drop)?;
    assert_eq!(
        call(
            &delivery,
            "connection/delete",
            boss.to_string().as_str(),
            selection.clone()
        )
        .await
        .unwrap_err()["code"],
        "referenced"
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
    assert_eq!(
        call(
            &delivery,
            "connection/write",
            boss.to_string().as_str(),
            changed
        )
        .await
        .unwrap_err()["code"],
        "immutable_field"
    );
    sqlx::raw_sql("ALTER TABLE platform.integration RENAME TO unavailable_integration")
        .execute(&mut billing)
        .await
        .map(drop)?;
    let mut changed = connection;
    changed["parameters"]["tls-verification"] = "disabled".into();
    assert_eq!(
        call(
            &delivery,
            "connection/write",
            boss.to_string().as_str(),
            changed
        )
        .await
        .unwrap_err()["code"],
        "application_write_incomplete"
    );
    assert_eq!(sqlx::query("SELECT g.parameters->>'tls-verification' FROM registry.connection c JOIN registry.connection_generation g ON(g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.id=$1::text::uuid").bind(&identity).fetch_one(&mut admin).await?.get::<String, _>(0),"disabled");
    sqlx::raw_sql("ALTER TABLE platform.unavailable_integration RENAME TO integration")
        .execute(&mut billing)
        .await
        .map(drop)?;
    let document = wamn_environment::environment::document::EnvironmentDocument::from_json(
        &serde_json::to_vec(
            &json!({"org":ORG,"project":"billing","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"applications":{}}),
        )?,
    )?;
    // The projection runs as the database owner, as env apply runs it.
    let mut billing_url = url::Url::parse(database.url())?;
    billing_url.set_path(
        url::Url::parse(
            std::fs::read_to_string(logins.join(
                wamn_control_provision::control_administration_key(
                    "billing",
                    &"dev".parse().unwrap(),
                ),
            ))?
            .trim(),
        )?
        .path(),
    );
    let _ = wamn_environment::environment::bindings::project_authority(
        database.url(),
        billing_url.as_str(),
        &document,
    )
    .await?;
    let before: String = sqlx::query("SELECT xmin::text FROM platform.connection")
        .fetch_one(&mut billing)
        .await?
        .get(0);
    let _ = wamn_environment::environment::bindings::project_authority(
        database.url(),
        billing_url.as_str(),
        &document,
    )
    .await?;
    assert_eq!(
        sqlx::query("SELECT xmin::text FROM platform.connection")
            .fetch_one(&mut billing)
            .await?
            .get::<String, _>(0),
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
#[ignore = "requires: postgres, nats, env:WAMN_NATIVE_C_NATS_BIN"]
async fn delayed_grant_cannot_restore_revoked_admin() -> anyhow::Result<()> {
    use wamn_execution_host::ProjectionPause;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let mut database = postgres.create_database("grant_projection_order").await?;
    let mut admin = connect(database.url()).await?;
    let control_url = install(&mut admin, database.url()).await?;
    let (logins, billing, _shop) =
        environments(&mut admin, database.url(), "grant_projection_order").await?;
    let boss = create_or_reuse_user(&mut admin, "boss-order@example.test", "Boss")
        .await?
        .principal_id;
    let user = create_or_reuse_user(&mut admin, "member-order@example.test", "Member")
        .await?
        .principal_id;
    invite_member(
        &mut admin,
        &boss,
        ORG,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    invite_member(&mut admin, &user, ORG, &MemberGrants::default()).await?;
    let (_server, broker) = wamn_capability_authority::auth_cache::test_util::nats().await;
    let changes = Publisher::new(broker.clone());
    let bucket = wamn_broker::auth_changes::auth_bucket(&broker).await?;
    let delivery = async || -> anyhow::Result<HostRouteDelivery> {
        Ok(HostRouteDelivery::new(
            Arc::new(LoadedRelease::control_root()),
            HostRouteHandlers::Control {
                status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
                administration: Some(logins.clone()),
                control: wamn_postgres::pool(&control_url, 4).await?,
                writer: Arc::new(tokio::sync::Mutex::new(connect(&control_url).await?)),
                identity: None,
                org: ORG.to_owned(),
                changes: changes.clone(),
            },
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
        let project_env_key =
            wamn_registry::project_env_key(ORG, "billing", &"dev".parse().unwrap());
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
                    let waiting: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND cardinality(pg_blocking_pids(pid))>0)").fetch_one(&mut admin).await?.get(0);
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
            let count: i64 = sqlx::query("SELECT count(*) FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin'").bind(&user.as_str()).fetch_one(&mut billing).await?.get(0);
            assert_eq!(
                count, 0,
                "the second host completes revocation while the grant waits"
            );
            pause.resume();
        }
        grant.await?.map_err(anyhow::Error::msg)?;
        let rows = application_rows(&billing, user.as_str()).await;
        assert!(
            rows.iter().all(|row| row.1.as_deref() != Some("admin")),
            "{rows:?}"
        );
        let count: i64 = sqlx::query("SELECT count(*) FROM identity.project_roles WHERE principal_id=$1::text::uuid AND role='project-admin'").bind(&user.as_str()).fetch_one(&mut admin).await?.get(0);
        assert_eq!(count, 0);
        let project_env_key =
            wamn_registry::project_env_key(ORG, "billing", &"dev".parse().unwrap());
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

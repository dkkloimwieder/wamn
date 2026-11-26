use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use serde_json::Value;
use sqlx::{Connection as _, postgres::PgConnectOptions};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::task::JoinSet;
use wamn_authentication::access_token::{
    AccessTokenError, AccessTokenVerifier, FetchedKeys, KeySource,
};
use wamn_authentication::route_authentication::ControlRouteAuthenticator;
use wamn_identity::identity::session_keys::{initialize_session_key, session_jwks};
use wamn_identity::identity::session_token::sign_session_token_in_transaction;
use wamn_identity_protocol::{PrincipalId, records::AccessTokenClaims};
use wamn_registry::{OrgId, Triple};

use super::{Arc, HostRouteDelivery, HostRouteHandlers, Writer};

const ISSUER: &str = "https://identity.example.invalid";

#[derive(Debug)]
struct PublicKeys(Value);

impl KeySource for PublicKeys {
    fn fetch<'a, 'future>(
        &'a self,
        started: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<FetchedKeys, AccessTokenError>> + Send + 'future>>
    where
        'a: 'future,
        Self: 'future,
    {
        Box::pin(async move {
            Ok(FetchedKeys {
                document: serde_json::from_value(self.0.clone()).expect("issuer JWKS"),
                fresh_until: started + Duration::from_secs(60),
            })
        })
    }
}

async fn request(
    address: std::net::SocketAddr,
    token: &str,
    line: &str,
    body: &str,
) -> anyhow::Result<(u16, Value)> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream.write_all(format!(
        "{line} HTTP/1.0\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    ).as_bytes()).await?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await?;
    let (headers, body) = raw.split_once("\r\n\r\n").expect("HTTP response");
    let status = headers
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()?;
    Ok((status, serde_json::from_str(body)?))
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn literal_control_http_wire() -> anyhow::Result<()> {
    let system = wamn_test_infrastructure::database::system().await?;
    let mut admin = system.admin().await?;
    let org: wamn_registry::OrgId = "acme".parse()?;
    let login = system.org_login(wamn_role::CONTROL_ROLE)?;
    let database = system
        .login_url(login.login.as_str(), &login.password)?
        .to_string();
    let mut seed =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::raw_sql(
        "INSERT INTO registry.project (org, id) VALUES ('acme', 'alpha'), ('acme', 'beta');
         SELECT registry.provision_env('acme', 'dev');
         SELECT registry.provision_env('acme', 'paused');
         UPDATE registry.env SET active = false WHERE org = 'acme' AND env = 'paused';
         INSERT INTO registry.project_env (org, env, project, secret_name, instance_suffix)
           SELECT org, env, 'alpha', 'fixture-only', 'a1b2c3d4'
           FROM registry.env WHERE org = 'acme';
         INSERT INTO identity.principal (id, type, subject, email, display_name) VALUES
           ('00000000-0000-0000-0000-000000000001', 'user', 'admin@example.invalid', 'admin@example.invalid', 'Admin'),
           ('00000000-0000-0000-0000-000000000002', 'user', 'member@example.invalid', 'member@example.invalid', 'Member'),
           ('00000000-0000-0000-0000-000000000003', 'user', 'project@example.invalid', 'project@example.invalid', 'Project');
         INSERT INTO identity.principal (type, subject, email, display_name, active) VALUES
           ('user', 'disabled@example.invalid', 'disabled@example.invalid', 'Disabled', false);
         INSERT INTO identity.org_membership (principal_id, org, active)
           SELECT id, 'acme', active FROM identity.principal WHERE email LIKE '%@example.invalid';
         INSERT INTO identity.org_role (principal_id, org, role) VALUES
           ('00000000-0000-0000-0000-000000000001', 'acme', 'org-admin');
         INSERT INTO identity.project_role (principal_id, org, project, role) VALUES
           ('00000000-0000-0000-0000-000000000003', 'acme', 'alpha', 'project-admin');",
    ).execute(&mut *seed).await?;
    seed.commit().await?;

    let principals: [PrincipalId; 3] = [
        "00000000-0000-0000-0000-000000000001".parse()?,
        "00000000-0000-0000-0000-000000000002".parse()?,
        "00000000-0000-0000-0000-000000000003".parse()?,
    ];
    let (authenticator, tokens) = control_sessions(&mut admin, &org, &principals).await?;
    let (address, _server) = serve_control_http(database, org, authenticator).await?;

    let cases = [
        (
            0,
            "GET /wamn_control/control/mine",
            "",
            200,
            r#"[{"value":{"org_admin":true,"projects":[{"project":"alpha","project_admin":true},{"project":"beta","project_admin":true}],"next_cursor":null,"total":2}}]"#,
        ),
        (
            0,
            "POST /wamn_control/project/create",
            r#"[{"request_id":"write-1","value":{"project":"gamma"}}]"#,
            200,
            r#"[{"request_id":"write-1","value":{"project":"gamma"}}]"#,
        ),
        (
            0,
            "POST /wamn_control/project/create",
            r#"[{"request_id":"write-2","value":{"project":"gamma"}}]"#,
            200,
            r#"[{"request_id":"write-2","error":{"code":"project_exists","detail":{"field":"project"}}}]"#,
        ),
        (
            1,
            "POST /wamn_control/project/create",
            r#"[{"request_id":"write-3","value":{"project":"delta"}}]"#,
            403,
            r#"{"error":{"code":"permission-denied","operation":"wamn-control:project/create@0.10.0"}}"#,
        ),
        (
            2,
            "GET /wamn_control/env/list?project=%22alpha%22",
            "",
            200,
            r#"[{"value":{"envs":[{"name":"dev","state":"active"},{"name":"paused","state":"inactive"}],"next_cursor":null,"total":2}}]"#,
        ),
        (
            2,
            "GET /wamn_control/env/list?project=%22beta%22",
            "",
            403,
            r#"{"error":{"code":"permission-denied","operation":"wamn-control:env/list@0.10.0"}}"#,
        ),
    ];
    for (actor, line, body, status, expected) in cases {
        let observed = request(address, &tokens[actor], line, body).await?;
        assert_eq!(
            observed,
            (status, serde_json::from_str(expected)?),
            "{line}"
        );
    }
    let (status, body) = request(address, &tokens[0], "GET /wamn_control/user/list", "").await?;
    assert_eq!(status, 200);
    let users = body[0]["value"]["users"].as_array().expect("user list");
    for (email, membership, principal) in [
        ("admin@example.invalid", "active", "active"),
        ("disabled@example.invalid", "inactive", "disabled"),
    ] {
        let user = users
            .iter()
            .find(|user| user["email"] == email)
            .expect(email);
        assert_eq!(user["status"], membership);
        assert_eq!(user["principal_status"], principal);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, input:web-install, bin:pnpm"]
async fn generated_org_client_live() -> anyhow::Result<()> {
    use wamn_identity::identity::{create_or_reuse_user, org::invite_member};
    use wamn_identity_protocol::records::MemberGrants;
    use wamn_platform_install::provision_org::{control_login, create_org_logins};
    use wamn_platform_install::provision_system::SYSTEM_DATABASE;

    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let mut cluster = wamn_postgres::connect(postgres.admin_url()).await?;
    // Supply the apply group that CloudNativePG creates before org provisioning.
    sqlx::raw_sql("CREATE ROLE wamn_apply NOLOGIN NOINHERIT")
        .execute(&mut cluster)
        .await?;
    let url = wamn_test_infrastructure::identity::system(&postgres).await;
    let mut admin = wamn_postgres::connect(url.as_str()).await?;
    let org: wamn_registry::OrgId = "acme".parse()?;
    let logins = create_org_logins(&mut admin, &org, SYSTEM_DATABASE).await?;
    let control = control_login(&org, SYSTEM_DATABASE);
    let login = logins
        .iter()
        .find(|login| login.login == control)
        .context("provisioned control login")?;
    let database = postgres.database_url(SYSTEM_DATABASE, login.login.as_str(), &login.password)?;

    let mut seed =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("INSERT INTO registry.project (org, id) VALUES ($1, 'billing')")
        .bind(org.as_str())
        .execute(&mut *seed)
        .await?;
    let boss = create_or_reuse_user(&mut seed, "boss@example.test", "Boss")
        .await?
        .principal_id;
    let cat = create_or_reuse_user(&mut seed, "cat@example.test", "Cat")
        .await?
        .principal_id;
    let ann = create_or_reuse_user(&mut seed, "ann@example.test", "Ann")
        .await?
        .principal_id;
    let dan = create_or_reuse_user(&mut seed, "dan@example.test", "Dan")
        .await?
        .principal_id;
    invite_member(
        &mut seed,
        &boss,
        &org,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    invite_member(
        &mut seed,
        &cat,
        &org,
        &MemberGrants {
            project_admins: vec!["billing".parse()?],
            ..MemberGrants::default()
        },
    )
    .await?;
    invite_member(&mut seed, &ann, &org, &MemberGrants::default()).await?;
    seed.commit().await?;

    let (authenticator, tokens) = control_sessions(&mut admin, &org, &[boss, cat]).await?;
    let (address, _server) = serve_control_http(database.into(), org, authenticator).await?;
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/platform_ui");
    let status = tokio::task::spawn_blocking(move || {
        Command::new("pnpm")
            .current_dir(directory)
            .args([
                "exec",
                "vitest",
                "run",
                "test/control-org-client.live.test.ts",
            ])
            .env(
                "WAMN_CONTROL_ORG_CLIENT_BASE_URL",
                format!("http://{address}"),
            )
            .env("WAMN_CONTROL_ORG_CLIENT_BOSS_TOKEN", &tokens[0])
            .env("WAMN_CONTROL_ORG_CLIENT_CAT_TOKEN", &tokens[1])
            .env("WAMN_CONTROL_ORG_CLIENT_BOSS_ID", boss.to_string())
            .env("WAMN_CONTROL_ORG_CLIENT_ANN_ID", ann.to_string())
            .env("WAMN_CONTROL_ORG_CLIENT_DAN_ID", dan.to_string())
            .status()
    })
    .await?
    .context("run the generated org client test")?;
    anyhow::ensure!(status.success(), "Vitest exited with {status}");
    Ok(())
}

async fn activity_test_org(
    postgres: &wamn_test_infrastructure::containers::Postgres,
    org: &wamn_registry::OrgId,
) -> anyhow::Result<(url::Url, String, PrincipalId)> {
    use wamn_identity::identity::{create_user, org::invite_member};
    use wamn_identity_protocol::records::MemberGrants;
    use wamn_platform_install::provision_org::{control_login, create_org_logins};
    use wamn_platform_install::provision_system::SYSTEM_DATABASE;

    let mut cluster = wamn_postgres::connect(postgres.admin_url()).await?;
    sqlx::raw_sql("CREATE ROLE wamn_apply NOLOGIN NOINHERIT")
        .execute(&mut cluster)
        .await?;
    let database = wamn_test_infrastructure::identity::system(postgres).await;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let logins = create_org_logins(&mut admin, org, SYSTEM_DATABASE).await?;
    let control = control_login(org, SYSTEM_DATABASE);
    let login = logins
        .iter()
        .find(|login| login.login == control)
        .context("control login")?;
    let control_url =
        postgres.database_url(SYSTEM_DATABASE, login.login.as_str(), &login.password)?;
    let mut seed =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    let email = "owner@example.invalid";
    let owner = create_user(&mut seed, email, email, "Owner").await?;
    let principal = *owner.id();
    invite_member(
        &mut seed,
        &principal,
        org,
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await?;
    seed.commit().await?;
    Ok((database, control_url.to_string(), principal))
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn project_admin_grant_distinguishes_missing_and_inactive_projects() -> anyhow::Result<()> {
    use crate::control_route::Refusal;
    use wamn_app::ingress::{AuthenticatedCaller, CredentialType};
    use wamn_app::native_routes::DeliveryError;
    use wamn_platform_install::provision_org::{OrgActivity, set_activity};
    use wamn_routes::{HostHandler, HostRouteSet};

    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let org: OrgId = "grant-projects".parse()?;
    let (database, control_url, principal) = activity_test_org(&postgres, &org).await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let mut seed =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("INSERT INTO registry.project (org, id, active) VALUES ($1, 'paused', false)")
        .bind(org.as_str())
        .execute(&mut *seed)
        .await?;
    seed.commit().await?;

    let delivery = control_delivery(control_url, org.clone(), HashMap::new()).await?;
    let (id, attachment) = HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.route.handler == HostHandler::ProjectAdminGrant)
        .context("project admin grant route")?;
    let caller = AuthenticatedCaller::new(
        id.clone(),
        principal,
        CredentialType::Session,
        false,
        HashSet::new(),
        None,
    );
    let grant = async |project: &str| {
        let payload = serde_json::json!({"project": project, "principal_id": principal});
        delivery
            .handle(attachment, &caller, &payload.to_string())
            .await
    };
    let missing = grant("nowhere").await;
    assert!(
        matches!(&missing, Err(Refusal::Declared { code: "project_not_found", detail })
            if detail["field"] == "project"),
        "{missing:?}"
    );
    let inactive = grant("paused").await;
    assert!(
        matches!(
            &inactive,
            Err(Refusal::Delivery(DeliveryError::PermissionDenied(_)))
        ),
        "{inactive:?}"
    );
    set_activity(database.as_str(), &org, OrgActivity::Inactive).await?;
    let inactive_org = grant("nowhere").await;
    assert!(
        matches!(
            &inactive_org,
            Err(Refusal::Delivery(DeliveryError::PermissionDenied(_)))
        ),
        "{inactive_org:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn org_activity_controls_new_commands_and_password_logins() -> anyhow::Result<()> {
    use wamn_identity::identity::{issuer::Issuer, password};
    use wamn_identity::password::{Password, password_work};
    use wamn_identity::session_exchange::{self, Exchange, ExchangeFailure};
    use wamn_platform_install::identity_role::grant_identity_issuer_surface_sql;
    use wamn_platform_install::provision_org::{OrgActivity, set_activity};

    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let org = "activity-org".parse()?;
    let (database, control_url, principal) = activity_test_org(&postgres, &org).await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let (authenticator, tokens) = control_sessions(&mut admin, &org, &[principal]).await?;
    let (address, _server) = serve_control_http(control_url, org.clone(), authenticator).await?;

    let email = "owner@example.invalid";
    let work = password_work();
    let passphrase = || {
        Password::new("warehouse activity fixture password".to_owned())
            .expect("valid fixture password")
    };
    let provisioning = wamn_identity_protocol::PlatformComponent::Provisioning.principal_id();
    let invitation = password::issue_invitation(&mut admin, &provisioning, &principal).await?;
    password::enroll_password(
        &mut admin,
        &work,
        &principal,
        invitation.secret(),
        passphrase(),
    )
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(grant_identity_issuer_surface_sql()))
        .execute(&mut admin)
        .await?;
    let mut issuer = wamn_postgres::connect(database.as_str()).await?;
    sqlx::raw_sql("SET ROLE wamn_identity_issuer")
        .execute(&mut issuer)
        .await?;
    let envelope = control_session_envelope()?;
    let audience = wamn_registry::name::org_audience(&org);
    let exchange = || Exchange {
        issuer: ISSUER,
        audience: &audience,
        started_at: session_exchange::unix_seconds().expect("current Unix time"),
        envelope: &envelope,
    };

    set_activity(database.as_str(), &org, OrgActivity::Inactive).await?;
    let command = "POST /wamn_control/project/create";
    let input = r#"[{"request_id":"create","value":{"project":"widgets"}}]"#;
    let (status, _) = request(address, &tokens[0], command, input).await?;
    assert_eq!(status, 403);
    assert!(matches!(
        session_exchange::login(
            &mut Issuer(&mut issuer),
            exchange(),
            &work,
            email,
            passphrase(),
            None,
        )
        .await,
        Err(ExchangeFailure::Unauthorized)
    ));

    set_activity(database.as_str(), &org, OrgActivity::Active).await?;
    let session = session_exchange::login(
        &mut Issuer(&mut issuer),
        exchange(),
        &work,
        email,
        passphrase(),
        None,
    )
    .await
    .map_err(|error| anyhow::anyhow!("password login after activation failed: {error:?}"))?;
    let (status, result) = request(address, session.token.token(), command, input).await?;
    assert_eq!(status, 200);
    assert_eq!(result[0]["value"]["project"], "widgets");
    Ok(())
}

async fn mirror_databases(
    postgres: &wamn_test_infrastructure::containers::Postgres,
) -> anyhow::Result<[PgConnectOptions; 2]> {
    let alpha = postgres.create_project_database("activity_alpha").await?;
    let mut cluster = wamn_postgres::connect(postgres.admin_url()).await?;
    sqlx::raw_sql("CREATE DATABASE activity_beta OWNER wamn_migration")
        .execute(&mut cluster)
        .await?;
    sqlx::raw_sql("CREATE ROLE activity_platform LOGIN PASSWORD 'fixture' IN ROLE wamn_platform")
        .execute(&mut cluster)
        .await?;
    let alpha: PgConnectOptions = alpha.as_str().parse()?;
    let beta = alpha.clone().database("activity_beta");
    for options in [alpha, beta] {
        let mut migration = wamn_postgres::connect(options).await?;
        wamn_env::platform_migrations::install_project(&mut migration).await?;
    }
    Ok([
        postgres
            .database_url("activity_alpha", "activity_platform", "fixture")?
            .as_str()
            .parse()?,
        postgres
            .database_url("activity_beta", "activity_platform", "fixture")?
            .as_str()
            .parse()?,
    ])
}

async fn command_status(
    address: SocketAddr,
    token: &str,
    route: &str,
    input: &str,
    expected: &str,
) -> anyhow::Result<()> {
    let (status, result) = request(address, token, route, input).await?;
    assert_eq!(status, 200, "{route}: {result}");
    assert_eq!(result[0]["value"]["status"], expected, "{route}: {result}");
    Ok(())
}

async fn assert_mirrors(
    databases: &mut [sqlx::PgConnection; 2],
    expected: [bool; 2],
) -> anyhow::Result<()> {
    for (database, expected) in databases.iter_mut().zip(expected) {
        let active: bool = sqlx::query_scalar("SELECT active FROM platform.env")
            .fetch_one(database)
            .await?;
        assert_eq!(active, expected);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn project_deactivation_refuses_access_and_preserves_its_sibling() -> anyhow::Result<()> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let org = "activity-projects".parse::<OrgId>()?;
    let (database, control_url, principal) = activity_test_org(&postgres, &org).await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let mut seed =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("INSERT INTO registry.project (org, id) VALUES ($1, 'alpha'), ($1, 'beta')")
        .bind(org.as_str())
        .execute(&mut *seed)
        .await?;
    sqlx::query("SELECT registry.provision_env($1, 'dev')")
        .bind(org.as_str())
        .execute(&mut *seed)
        .await?;
    sqlx::query(
        "INSERT INTO registry.project_env (org, project, env, secret_name, instance_suffix)
         SELECT org, id, 'dev', 'fixture-only', 'a1b2c3d4' FROM registry.project WHERE org=$1",
    )
    .bind(org.as_str())
    .execute(&mut *seed)
    .await?;
    seed.commit().await?;

    let (authenticator, tokens) = control_sessions(&mut admin, &org, &[principal]).await?;
    let mut projection = wamn_postgres::connect(&control_url).await?;
    let alpha_target = Triple::new(org.clone(), "dev".parse()?, "alpha".parse()?);
    let beta_target = Triple::new(org.clone(), "dev".parse()?, "beta".parse()?);
    let [alpha_db, beta_db] = mirror_databases(&postgres).await?;
    let mut mirrors = [
        wamn_postgres::connect(alpha_db.clone()).await?,
        wamn_postgres::connect(beta_db.clone()).await?,
    ];
    let logins = HashMap::from([(alpha_target.clone(), alpha_db), (beta_target, beta_db)]);
    let (address, _server) =
        serve_control_http_with_projects(control_url, org, authenticator, logins).await?;
    let token = &tokens[0];
    let project_input = r#"[{"request_id":"project-status","value":{"project":"alpha"}}]"#;
    let env_input = r#"[{"request_id":"env-status","value":{"project":"beta","env":"dev"}}]"#;
    let project_deactivate = "POST /wamn_control/project/deactivate";
    let project_activate = "POST /wamn_control/project/activate";
    let env_activate = "POST /wamn_control/env/activate";
    let env_deactivate = "POST /wamn_control/env/deactivate";

    command_status(address, token, env_activate, env_input, "active").await?;
    command_status(
        address,
        token,
        project_deactivate,
        project_input,
        "inactive",
    )
    .await?;
    assert_mirrors(&mut mirrors, [false, true]).await?;

    let alpha = "GET /wamn_control/env/list?project=%22alpha%22";
    let beta = "GET /wamn_control/env/list?project=%22beta%22";
    assert_eq!(request(address, token, alpha, "").await?.0, 403);
    let (status, result) = request(address, token, beta, "").await?;
    assert_eq!(status, 200);
    let envs = result[0]["value"]["envs"]
        .as_array()
        .context("sibling envs")?;
    let dev = envs
        .iter()
        .find(|env| env["name"] == "dev")
        .context("sibling dev env")?;
    assert_eq!(dev["state"], "active");

    let authority = wamn_authentication::administration::read_project_authority(
        &mut projection,
        wamn_authentication::administration::AppTarget {
            org: &alpha_target.org,
            project: &alpha_target.project,
            env: &alpha_target.env,
        },
        &principal,
    )
    .await?
    .context("retained project authority")?;
    assert!(authority.admin);

    command_status(address, token, env_deactivate, env_input, "inactive").await?;
    command_status(address, token, env_activate, env_input, "active").await?;
    assert_mirrors(&mut mirrors, [false, true]).await?;

    command_status(address, token, env_deactivate, env_input, "inactive").await?;
    command_status(address, token, project_activate, project_input, "active").await?;
    assert_mirrors(&mut mirrors, [false, false]).await?;
    assert_eq!(request(address, token, alpha, "").await?.0, 200);
    command_status(address, token, env_activate, env_input, "active").await?;
    assert_mirrors(&mut mirrors, [true, true]).await?;
    Ok(())
}

fn control_session_envelope() -> anyhow::Result<wamn_envelope::encryption::Envelope> {
    let key_path = std::env::temp_dir().join(format!("wamn-wire-{}", uuid::Uuid::new_v4()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)?
        .write_all(&[7; 32])?;
    let envelope = wamn_key::from_file(&key_path);
    std::fs::remove_file(key_path)?;
    envelope.map_err(Into::into)
}

async fn control_sessions(
    admin: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    principals: &[PrincipalId],
) -> anyhow::Result<(ControlRouteAuthenticator, Vec<String>)> {
    let envelope = control_session_envelope()?;
    let audience = wamn_registry::name::org_audience(org);
    initialize_session_key(admin, ISSUER, &envelope).await?;
    let jwks = serde_json::to_value(session_jwks(admin, ISSUER).await?)?;
    let verifier = AccessTokenVerifier::new(
        ISSUER,
        &format!("{ISSUER}/.well-known/jwks.json"),
        &audience,
        &["EdDSA".into()],
        |_| Ok(PublicKeys(jwks)),
    )?;
    let mut tokens = Vec::with_capacity(principals.len());
    for principal in principals {
        let mut transaction = admin.begin().await?;
        let signed = sign_session_token_in_transaction(
            &mut transaction,
            AccessTokenClaims {
                iss: ISSUER.into(),
                sub: *principal,
                aud: audience.clone(),
                exp: 0,
                iat: 0,
                jti: uuid::Uuid::new_v4().to_string(),
                client_id: "wamn-web".into(),
            },
            SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs()
                .try_into()?,
            None,
            &envelope,
        )
        .await?;
        transaction.commit().await?;
        tokens.push(signed.token().to_owned());
    }
    Ok((ControlRouteAuthenticator::new(verifier), tokens))
}

async fn serve_control_http(
    database: String,
    org: wamn_registry::OrgId,
    authenticator: ControlRouteAuthenticator,
) -> anyhow::Result<(SocketAddr, JoinSet<anyhow::Result<()>>)> {
    serve_control_http_with_projects(database, org, authenticator, HashMap::new()).await
}

async fn serve_control_http_with_projects(
    database: String,
    org: OrgId,
    authenticator: ControlRouteAuthenticator,
    logins: HashMap<Triple, PgConnectOptions>,
) -> anyhow::Result<(SocketAddr, JoinSet<anyhow::Result<()>>)> {
    let delivery = Arc::new(
        control_delivery(database, org, logins)
            .await?
            .with_control_authenticator(Arc::new(authenticator)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut server = JoinSet::new();
    server.spawn(async move {
        loop {
            let (stream, _) = listener.accept().await?;
            let service = hyper::service::service_fn(|incoming| async {
                Ok::<_, std::convert::Infallible>(
                    delivery.http(wamn_http::body::incoming(incoming)).await,
                )
            });
            hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await?;
        }
    });
    Ok((address, server))
}

async fn control_delivery(
    database: String,
    org: OrgId,
    logins: HashMap<Triple, PgConnectOptions>,
) -> anyhow::Result<HostRouteDelivery> {
    let control = wamn_postgres::pool(&database, 3).await?;
    let kubernetes = wamn_kubernetes::client::KubernetesApi::from_config(kube::Config::new(
        "http://127.0.0.1:1".parse()?,
    ))?;
    let mut project_logins =
        crate::project_login::ProjectLogins::new(kubernetes, control.clone(), org.clone());
    project_logins.test_logins = logins;
    let writer = wamn_postgres::connect(&database).await?;
    Ok(HostRouteDelivery::new(HostRouteHandlers {
        logins: Arc::new(project_logins),
        org,
        control,
        writer: Writer::new(database, writer),
        identity: None,
        issuer: ISSUER.into(),
    }))
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn oauth_refresh_records_attempts_and_serializes_hosts() -> anyhow::Result<()> {
    use wamn_connection::{
        connection_credentials::ConnectionCredentials,
        connection_facts::ConnectionFacts,
        connection_memory::ConnectionMemory,
        oauth_token::{self, Fetched, Token},
    };
    use wamn_identity_protocol::AuthorityClass;
    use wamn_postgres::{ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres};

    fn request(
        facts: &ConnectionFacts,
        principal: PrincipalId,
        connection: uuid::Uuid,
        generation: i64,
    ) -> oauth_token::Request<'_> {
        oauth_token::Request {
            reader: facts.reader(),
            principal,
            project: facts.project(),
            env: facts.env(),
            connection,
            generation,
            authorization_code: false,
            rejected: None,
        }
    }

    let fixture = wamn_test_infrastructure::database::project().await?;
    let mut admin = fixture.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    let connection: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ('billing','http','oauth2_client_credentials',gen_random_uuid(),1,gen_random_uuid())
         RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.connection_generation(connection_id,generation) VALUES ($1,1),($1,2)",
    )
    .bind(connection)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let project: wamn_registry::ProjectId = "billing".parse()?;
    let env = "dev".parse()?;
    let org = "acme".parse()?;
    let key = wamn_registry::name::project_env_key(&org, &project, &env);
    let url = fixture.login_url(wamn_role::PLATFORM_LOGIN)?;
    let database = || {
        let mut provider = StaticCredentialProvider::default();
        provider.insert(
            project.clone(),
            ProjectConfig {
                credentials: ClassCredentials::default()
                    .with_class(AuthorityClass::Platform, url.as_str()),
                guest_pool_max_size: 1,
                platform_pool_max_size: 1,
                wait_timeout_ms: 5_000,
                statement_timeout_ms: 5_000,
            },
        );
        Arc::new(WamnPostgres::with_provider(Arc::new(provider)))
    };
    let first_db = database();
    let second_db = database();
    let facts = |db| {
        ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            Arc::new(ConnectionCredentials::for_org(&org, None)),
            db,
            &key,
            &project,
            &env,
        )
    };
    let first = facts(&first_db);
    let second = facts(&second_db);
    let principal = PrincipalId::from(uuid::Uuid::new_v4());
    let first_key = control_session_envelope()?;
    let second_key = control_session_envelope()?;

    let failed = oauth_token::get(
        request(&first, principal, connection, 1),
        &first_key,
        |_| async {
            anyhow::bail!("provider refused the fixture request");
        },
    )
    .await;
    assert!(failed.is_err());
    let cooling = oauth_token::get(
        request(&second, principal, connection, 1),
        &second_key,
        |_| async {
            panic!("the second host called the provider during back-off");
        },
    )
    .await;
    assert!(cooling.is_err());

    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("UPDATE platform.connection SET active_generation=2 WHERE id=$1")
        .bind(connection)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let (entered, waiting) = tokio::sync::oneshot::channel();
    let first_refresh = oauth_token::get(
        request(&first, principal, connection, 2),
        &first_key,
        |_| async {
            // Administration can lock the row while the provider work is in flight.
            sqlx::query("SELECT id FROM platform.connection WHERE id=$1 FOR NO KEY UPDATE NOWAIT")
                .bind(connection)
                .fetch_one(&mut admin)
                .await?;
            entered.send(()).expect("second host is waiting to start");
            tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT FROM pg_locks
                     WHERE locktype='advisory' AND NOT granted
                       AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))",
                )
                .fetch_one(&mut admin)
                .await?;
                if blocked {
                    break Ok::<_, sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        }).await??;
            Ok(Fetched {
                token: Token {
                    access_token: Some("fixture-access-token".into()),
                    refresh_token: None,
                },
                expires_in: Some(600),
            })
        },
    );
    let second_refresh = async {
        waiting.await.context("first host reached provider work")?;
        Ok::<_, anyhow::Error>(
            oauth_token::get(
                request(&second, principal, connection, 2),
                &second_key,
                |_| async {
                    panic!("the second host missed the first host's token");
                },
            )
            .await?,
        )
    };
    let (first_token, second_token) = tokio::join!(first_refresh, second_refresh);
    assert_eq!(first_token?.token, second_token?.token);
    let events: Vec<(i64, String)> = sqlx::query_as(
        "SELECT generation,outcome FROM platform.connection_event
         WHERE connection_id=$1 AND event_type='token_refresh' ORDER BY id",
    )
    .bind(connection)
    .fetch_all(&mut admin)
    .await?;
    assert_eq!(events, [(1, "failed".into()), (2, "succeeded".into())]);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn oauth_callback_records_a_provider_refusal_without_consuming_state() -> anyhow::Result<()> {
    use crate::control_route::oauth_authorization::Authorization;

    let fixture = wamn_test_infrastructure::database::project().await?;
    let mut admin = fixture.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    let connection: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ('orders','http','oauth2_authorization_code',gen_random_uuid(),1,gen_random_uuid())
         RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.connection_generation(connection_id,generation) VALUES ($1,1)",
    )
    .bind(connection)
    .execute(&mut *tx)
    .await?;
    let state = "billing/dev/fixture-state";
    let principal = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO platform.oauth_authorization
             (state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at)
         VALUES ($1,$2,1,'fixture','fixture','fixture','fixture',$3,clock_timestamp()+interval '10 minutes')",
    )
    .bind(state)
    .bind(connection)
    .bind(principal)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let authorization = Authorization::new("https://control.example.invalid", None)?;
    let envelope = control_session_envelope()?;
    let login = fixture.login_url(wamn_role::PLATFORM_LOGIN)?;
    let mut platform = wamn_postgres::connect(login.as_str()).await?;
    let reply = authorization
        .callback(
            &mut platform,
            &envelope,
            "acme--dev",
            &format!("state={state}&error=access_denied&error_description=provider+details"),
        )
        .await;
    assert!(reply.is_err());

    let row: (String, uuid::Uuid) = sqlx::query_as(
        "SELECT outcome,principal_id FROM platform.connection_event
         WHERE connection_id=$1 AND event_type='oauth_callback'",
    )
    .bind(connection)
    .fetch_one(&mut platform)
    .await?;
    assert_eq!(row, ("refused".into(), principal));
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM platform.oauth_authorization WHERE state=$1)",
    )
    .bind(state)
    .fetch_one(&mut platform)
    .await?;
    assert!(pending);
    Ok(())
}

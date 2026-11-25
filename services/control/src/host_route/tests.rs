use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use serde_json::Value;
use sqlx::Connection as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::task::JoinSet;
use wamn_authentication::access_token::{
    AccessTokenError, AccessTokenVerifier, FetchedKeys, KeySource,
};
use wamn_authentication::route_authentication::ControlRouteAuthenticator;
use wamn_identity::identity::session_keys::{initialize_session_key, session_jwks};
use wamn_identity::identity::session_token::sign_session_token_in_transaction;
use wamn_identity_protocol::{PrincipalId, records::AccessTokenClaims};

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
        "INSERT INTO registry.projects (org, id) VALUES ('acme', 'alpha'), ('acme', 'beta');
         INSERT INTO identity.principals (id, type, subject, email, display_name) VALUES
           ('00000000-0000-0000-0000-000000000001', 'user', 'admin@example.invalid', 'admin@example.invalid', 'Admin'),
           ('00000000-0000-0000-0000-000000000002', 'user', 'member@example.invalid', 'member@example.invalid', 'Member'),
           ('00000000-0000-0000-0000-000000000003', 'user', 'project@example.invalid', 'project@example.invalid', 'Project');
         INSERT INTO identity.org_memberships (principal_id, org, status)
           SELECT id, 'acme', 'active' FROM identity.principals WHERE email LIKE '%@example.invalid';
         INSERT INTO identity.org_roles (principal_id, org, role) VALUES
           ('00000000-0000-0000-0000-000000000001', 'acme', 'org-admin');
         INSERT INTO identity.project_roles (principal_id, org, project, role) VALUES
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
            r#"{"error":{"code":"permission-denied","operation":"wamn-control:project/create@0.9.0"}}"#,
        ),
        (
            2,
            "GET /wamn_control/environment/list?project=%22alpha%22",
            "",
            200,
            r#"[{"value":{"environments":[],"next_cursor":null,"total":0}}]"#,
        ),
        (
            2,
            "GET /wamn_control/environment/list?project=%22beta%22",
            "",
            403,
            r#"{"error":{"code":"permission-denied","operation":"wamn-control:environment/list@0.9.0"}}"#,
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
    sqlx::query("INSERT INTO registry.projects (org, id) VALUES ($1, 'billing')")
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

async fn control_sessions(
    admin: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    principals: &[PrincipalId],
) -> anyhow::Result<(ControlRouteAuthenticator, Vec<String>)> {
    let key_path = std::env::temp_dir().join(format!("wamn-wire-{}", uuid::Uuid::new_v4()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)?
        .write_all(&[7; 32])?;
    let envelope = wamn_key::from_file(&key_path);
    std::fs::remove_file(key_path)?;
    let envelope = envelope?;
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
    let control = wamn_postgres::pool(&database, 3).await?;
    let kubernetes = wamn_kubernetes::client::KubernetesApi::from_config(kube::Config::new(
        "http://127.0.0.1:1".parse()?,
    ))?;
    let delivery = Arc::new(
        HostRouteDelivery::new(HostRouteHandlers {
            logins: Arc::new(crate::project_login::ProjectLogins::new(
                kubernetes,
                control.clone(),
                org.clone(),
            )),
            org,
            control,
            writer: {
                let writer = wamn_postgres::connect(&database).await?;
                Writer::new(database, writer)
            },
            identity: None,
            issuer: ISSUER.into(),
        })
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

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sqlx::Connection as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use wamn_authentication::access_token::{
    AccessTokenError, AccessTokenVerifier, FetchedKeys, KeySource,
};
use wamn_authentication::route_authentication::ControlRouteAuthenticator;
use wamn_identity::identity::session_keys::{initialize_session_key, session_jwks};
use wamn_identity::identity::session_token::sign_session_token_in_transaction;
use wamn_identity_protocol::records::AccessTokenClaims;

use super::{Arc, HostRouteDelivery, HostRouteHandlers, Writer};

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
    let issuer = "https://identity.example.invalid";
    let audience = wamn_registry::name::org_audience(&org);
    initialize_session_key(&mut admin, issuer, &envelope).await?;
    let jwks = serde_json::to_value(session_jwks(&mut admin, issuer).await?)?;
    let verifier = AccessTokenVerifier::new(
        issuer,
        "https://identity.example.invalid/.well-known/jwks.json",
        &audience,
        &["EdDSA".into()],
        |_| Ok(PublicKeys(jwks)),
    )?;
    let mut tokens = Vec::new();
    for id in [
        "00000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000002",
        "00000000-0000-0000-0000-000000000003",
    ] {
        let mut transaction = admin.begin().await?;
        let signed = sign_session_token_in_transaction(
            &mut transaction,
            AccessTokenClaims {
                iss: issuer.into(),
                sub: id.parse()?,
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
            writer: Writer::new(database.clone(), wamn_postgres::connect(&database).await?),
            identity: None,
            issuer: issuer.into(),
        })
        .with_control_authenticator(Arc::new(ControlRouteAuthenticator::new(verifier))),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
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
        Ok::<_, hyper::Error>(())
    });

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
    let result = async {
        for (actor, line, body, status, expected) in cases {
            let observed = request(address, &tokens[actor], line, body).await?;
            assert_eq!(
                observed,
                (status, serde_json::from_str(expected)?),
                "{line}"
            );
        }
        anyhow::Ok(())
    }
    .await;
    server.abort();
    result
}

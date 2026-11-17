//! Actual identity CLI and HTTPS routes on a disposable test DB.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use std::collections::BTreeSet;
use std::time::Duration;

use clap::Parser as _;
use postgres_protocol::escape::escape_identifier;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;
use wamn_provision::identity_role::identity_issuer_login;
use wamn_provision::sql::revoke_public_connect_base_grants_sql;
use wamn_role::IDENTITY_ISSUER_ROLE;
use wamn_identity_service::cli::Cli;
use wamn_identity_service::{IdentityConfig, IdentityService, serve, tls_config};
use wamn_identity_protocol::keys::{PublicSessionKey, SessionJwks};
use wamn_oci::package_artifact::http_client::{HttpClient, Redirects};
use wamn_identity::session_keys::publish_session_key;

const ISSUER: &str = "https://identity.service.internal";
const PASSWORD: &str = "identity-surface-fixture-secret";

#[test]
fn cli_and_validated_configuration_debug_do_not_disclose_database_credentials() {
    let role = identity_issuer_login(ISSUER).expect("targeted role");
    let url = format!("postgres://{role}:{PASSWORD}@127.0.0.1/wamn_system");
    for args in [
        vec![
            "wamn-identity",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
            "publish",
        ],
        vec![
            "wamn-identity",
            "publish",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
        ],
        vec![
            "wamn-identity",
            "serve",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
            "--tls-cert",
            "/mounted/tls.crt",
            "--tls-key",
            "/mounted/tls.key",
        ],
    ] {
        let cli = Cli::try_parse_from(args).expect("CLI contract");
        assert!(!format!("{cli:?}").contains(PASSWORD));
        assert!(!format!("{cli:?}").contains(&url));
    }
    let config = IdentityConfig::new(ISSUER, &url).expect("targeted configuration");
    assert!(!format!("{config:?}").contains(PASSWORD));
    assert!(!format!("{config:?}").contains(&url));
    let refused = format!("postgres://wamn_system:{PASSWORD}@unreachable.invalid/wamn_system");
    let error = IdentityConfig::new(ISSUER, &refused).expect_err("owner refused before any socket");
    assert!(!format!("{error:?} {error}").contains(PASSWORD));
    assert!(!format!("{error:?} {error}").contains(&refused));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires: postgres"]
async fn identity_https_has_only_public_jwks_and_health() {
    let mut postgres = wamn_test_postgres::start(&[]).expect("start the test PostgreSQL server");
    let system = postgres
        .create_database("wamn_system")
        .await
        .expect("create the wamn_system test database");
    let admin_url = system.url();
    let mut admin = connect(admin_url).await;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await
        .expect("fixture database name")
        .get(0);
    assert_eq!(database, "wamn_system", "never reset another database");
    let role = identity_issuer_login(ISSUER).expect("targeted role");
    let existing: bool =
        sqlx::query("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = ANY($1::text[]))")
            .bind(&vec![role.clone(), IDENTITY_ISSUER_ROLE.parse().unwrap()])
            .fetch_one(&mut admin)
            .await
            .expect("fixture role absence")
            .get(0);
    assert!(
        !existing,
        "fixture requires absent identity authority roles"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_provision::sql::ensure_project_roles_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("install shared project roles");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; \
         DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
         THEN CREATE ROLE wamn_system; END IF; END $$; \
         GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("prepare disposable system schema owner");
    sqlx::raw_sql(wamn_provision::SYSTEM_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)
        .expect("production system schema");
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut admin)
        .await
        .map(drop)
        .expect("restore fixture admin");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (revoke_public_connect_base_grants_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("production CONNECT minimum");
    sqlx::raw_sql("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC")
        .execute(&mut admin)
        .await
        .map(drop)
        .expect("production TEMPORARY minimum");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_provision::test_database::issuer_login_sql(ISSUER, PASSWORD)
            .expect("targeted credential SQL"))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("provision narrow identity credential");
    let mut targeted = url::Url::parse(admin_url).expect("fixture URL shape");
    targeted
        .set_username(role.as_str())
        .expect("targeted login");
    targeted
        .set_password(Some(PASSWORD))
        .expect("fixture password");
    targeted.set_fragment(None);
    let targeted = targeted.to_string();

    let published = cli(&targeted, &["publish"]).await;
    let public = public_key(&published);
    assert_eq!(
        cli(&targeted, &["activate", "--kid", &public.kid]).await["activated"],
        true
    );
    let other = publish_session_key(&mut admin, "https://other.identity.service.internal")
        .await
        .expect("other issuer control");
    let config = IdentityConfig::new(ISSUER, &targeted).expect("validated service configuration");
    let service = IdentityService::connect(config)
        .await
        .expect("targeted service connection");
    assert!(!format!("{service:?}").contains(PASSWORD));
    assert!(!format!("{service:?}").contains(&targeted));
    let (certificate, private_key, ca) = certificates();
    let tls = tls_config(&certificate, &private_key).expect("production TLS configuration");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTPS fixture listener");
    let address = listener.local_addr().expect("bound address");
    let serving = tokio::spawn(serve(listener, service, tls));
    let endpoint = format!("https://{address}");
    let trusted = https_client(&ca);
    assert_eq!(
        trusted
            .get(format!("{endpoint}/healthz"))
            .send()
            .await
            .expect("HTTPS health")
            .status(),
        200
    );
    let plaintext = format!("http://{address}/healthz");
    assert!(
        HttpClient::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("plaintext control")
            .get(plaintext)
            .send()
            .await
            .is_err(),
        "plaintext cannot reach an HTTP route"
    );
    let (_, _, wrong_ca) = certificates();
    assert!(
        https_client(&wrong_ca)
            .get(format!("{endpoint}/healthz"))
            .send()
            .await
            .is_err(),
        "untrusted TLS peer is refused"
    );

    let response = trusted
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .expect("JWKS HTTPS");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "public, max-age=300");
    assert_eq!(response.headers()["age"], "0");
    let bytes = response.bytes().await.expect("public body");
    let value: Value = serde_json::from_slice(&bytes).expect("public JSON");
    assert_eq!(
        value
            .as_object()
            .expect("JWKS object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["keys"]
    );
    assert_eq!(value["keys"].as_array().expect("JWKS keys").len(), 1);
    assert_eq!(public_key(&value["keys"][0]), public);
    assert!(
        value["keys"]
            .as_array()
            .unwrap()
            .iter()
            .all(|key| key["kid"] != other.kid)
    );
    assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    for path in [
        "/session",
        "/authoring",
        "/authoring/effective-release",
        "/publish",
        "/activate",
    ] {
        assert_eq!(
            trusted
                .get(format!("{endpoint}{path}"))
                .send()
                .await
                .expect("absent GET route")
                .status(),
            404
        );
        assert_eq!(
            trusted
                .post(format!("{endpoint}{path}"))
                .send()
                .await
                .expect("absent POST route")
                .status(),
            404
        );
    }
    assert_eq!(
        trusted
            .post(format!("{endpoint}/.well-known/jwks.json"))
            .send()
            .await
            .expect("GET-only JWKS")
            .status(),
        404
    );

    assert_eq!(
        cli(&targeted, &["remove", "--kid", &public.kid]).await["removed"],
        true
    );
    assert_eq!(
        cli(&targeted, &["remove", "--kid", &public.kid]).await["removed"],
        false
    );
    assert_eq!(cli(&targeted, &["retire"]).await["retired"], 0);
    let bytes = trusted
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .expect("JWKS after removal")
        .bytes()
        .await
        .expect("fresh public body");
    assert!(
        serde_json::from_slice::<SessionJwks>(&bytes)
            .expect("fresh public set")
            .keys
            .is_empty(),
        "JWKS must read committed state on every request"
    );
    // A restarted server ends every session. Identity opens its connection
    // again and serves without a restart of its own (wamn-fwzc).
    let ended: i64 = sqlx::query(
        "SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE usename = $1) ended",
    )
    .bind(&role)
    .fetch_one(&mut admin)
    .await
    .expect("end the service's sessions")
    .get(0);
    assert!(ended > 0, "the service holds sessions to end");
    let mut status = 0;
    for _ in 0..50 {
        status = trusted
            .get(format!("{endpoint}/.well-known/jwks.json"))
            .send()
            .await
            .expect("JWKS after the sessions ended")
            .status()
            .as_u16();
        if status == 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(status, 200, "identity opens its connection again");
    assert_eq!(
        trusted
            .get(format!("{endpoint}/healthz"))
            .send()
            .await
            .expect("health after the sessions ended")
            .status(),
        200
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "REVOKE SELECT ON identity.session_keys FROM {}",
            escape_identifier(IDENTITY_ISSUER_ROLE)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("authoritative read refusal control");
    let failure = trusted
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .expect("fixed service refusal");
    assert_eq!(failure.status(), 503);
    let failure = failure
        .json::<serde_json::Value>()
        .await
        .expect("refusal body");
    assert!(failure["error"].is_string());
    serving.abort();
    assert!(
        serving
            .await
            .expect_err("stopped owned listener")
            .is_cancelled()
    );
    drop(trusted);
    sqlx::raw_sql("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;").execute(&mut admin).await.map(drop).expect("remove owned disposable schemas");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP OWNED BY {role}; DROP ROLE {role}; DROP OWNED BY {stable}; DROP ROLE {stable};",
            role = escape_identifier(role.as_str()),
            stable = escape_identifier(IDENTITY_ISSUER_ROLE)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("remove exact fixture authority roles");
    drop(admin);
}

async fn connect(url: &str) -> Client {
    wamn_postgres::connect(url)
        .await
        .map_err(|_| ())
        .expect("connect disposable PostgreSQL")
}

async fn cli(url: &str, args: &[&str]) -> Value {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_wamn-identity"))
            .env("WAMN_IDENTITY_ISSUER", ISSUER)
            .env("WAMN_IDENTITY_DATABASE_URL", url)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("bounded identity command")
    .expect("run actual identity binary");
    assert!(
        output.status.success(),
        "identity lifecycle command succeeded"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(PASSWORD));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(PASSWORD));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(url));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(url));
    serde_json::from_slice(&output.stdout).expect("public command metadata")
}

fn public_key(value: &Value) -> PublicSessionKey {
    let fields = value
        .as_object()
        .expect("public key object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fields,
        BTreeSet::from(["kid", "kty", "crv", "alg", "use", "x"])
    );
    serde_json::from_value(value.clone()).expect("strict public JWK shape")
}

fn https_client(ca: &[u8]) -> HttpClient {
    HttpClient::builder()
        .https_only(true)
        .redirects(Redirects::None)
        .timeout(Duration::from_secs(5))
        .roots(ca)
        .expect("fixture CA")
        .build()
        .expect("trusted HTTPS client")
}

fn certificates() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().expect("CA key");
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().expect("TLS key");
    let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
        .expect("TLS params")
        .signed_by(&key, &issuer)
        .expect("TLS certificate");
    (
        certificate.pem().into_bytes(),
        key.serialize_pem().into_bytes(),
        ca.pem().into_bytes(),
    )
}

#[tokio::test]
async fn dotenv_loading_precedence_and_secret_redaction() {
    use wamn_test_infrastructure::{process_input, scratch::ScratchRoot};
    let directory = ScratchRoot::create().unwrap();
    let input = directory.path().join("certificate.pem");
    process_input::create(&input).unwrap();
    let role = identity_issuer_login(ISSUER).unwrap();
    let url = format!("postgres://{role}:fixture@127.0.0.1/wamn_system");
    std::fs::write(
        directory.path().join(".env"),
        "RESEND_API_KEY='fixture secret'\nRESEND_FROM='WAMN <fixture@example.invalid>'\n",
    )
    .unwrap();
    for (key, flag, malformed, reads_certificate) in [
        (None, None, false, false),
        (Some("fixture-key"), None, false, true),
        (Some("fixture-key"), Some("fixture secret"), false, false),
        (None, None, true, false),
    ] {
        if malformed {
            std::fs::write(
                directory.path().join(".env"),
                "RESEND_API_KEY='unterminated-secret",
            )
            .unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn-identity"));
        command
            .current_dir(directory.path())
            .env_clear()
            .env("TOKIO_WORKER_THREADS", "2")
            .env("TMPDIR", directory.path())
            .args([
                "--issuer",
                ISSUER,
                "--database-url",
                &url,
                "serve",
                "--tls-cert",
            ])
            .arg(&input)
            .args(["--tls-key", "missing.key"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(key) = key {
            command.env("RESEND_API_KEY", key);
        }
        if let Some(flag) = flag {
            command.args(["--resend-api-key", flag]);
        }
        let mut child = command.spawn().unwrap();
        let writer = process_input::reader(&input, &mut child).await.unwrap();
        assert_eq!(writer.is_some(), reads_certificate);
        drop(writer);
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        for secret in ["fixture secret", "fixture-key", "unterminated-secret"] {
            assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        }
    }
}

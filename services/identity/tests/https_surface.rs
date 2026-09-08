//! Actual identity CLI and HTTPS routes on an explicitly armed disposable DB.

use std::collections::BTreeSet;
use std::time::Duration;

use clap::Parser as _;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, identity_issuer_generation_role, prepare_identity_issuer_generation_sql,
};
use wamn_control_provision::sql::revoke_public_connect_floor_sql;
use wamn_identity::cli::Cli;
use wamn_identity::{IdentityConfig, IdentityService, serve, tls_config};
use wamn_pg_core::quote_ident;
use wamn_platform_identity::session_keys::{PublicSessionKey, SessionJwks, publish_session_key};

const ISSUER: &str = "https://identity.service.internal";
const PASSWORD: &str = "identity-surface-fixture-secret";
const SYSTEM_SCHEMA: &str = include_str!("../../../deploy/sql/system-schema.sql");

#[test]
fn cli_and_validated_configuration_debug_do_not_disclose_database_credentials() {
    let role =
        identity_issuer_generation_role(ISSUER, CredentialGeneration::A).expect("scoped role");
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
    let config = IdentityConfig::new(ISSUER, &url).expect("scoped configuration");
    assert!(!format!("{config:?}").contains(PASSWORD));
    assert!(!format!("{config:?}").contains(&url));
    let refused = format!("postgres://wamn_system:{PASSWORD}@unreachable.invalid/wamn_system");
    let error = IdentityConfig::new(ISSUER, &refused).expect_err("owner refused before any socket");
    assert!(!format!("{error:?} {error}").contains(PASSWORD));
    assert!(!format!("{error:?} {error}").contains(&refused));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an explicitly armed disposable wamn_system PostgreSQL database"]
async fn identity_https_has_only_public_jwks_and_health() {
    assert_eq!(
        std::env::var("WAMN_IDENTITY_SERVICE_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "arm only an owned disposable cluster: this proof replaces platform schemas"
    );
    let admin_url = std::env::var("WAMN_IDENTITY_SERVICE_PG_URL")
        .expect("provide the disposable wamn_system URL");
    let (mut admin, admin_driver) = connect(&admin_url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("fixture database name")
        .get(0);
    assert_eq!(database, "wamn_system", "never reset another database");
    let role =
        identity_issuer_generation_role(ISSUER, CredentialGeneration::A).expect("scoped role");
    let existing: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = ANY($1::text[]))",
            &[&vec![role.clone(), IDENTITY_ISSUER_ROLE.to_owned()]],
        )
        .await
        .expect("fixture role absence")
        .get(0);
    assert!(
        !existing,
        "fixture requires absent identity authority roles"
    );
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; \
         DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
         THEN CREATE ROLE wamn_system; END IF; END $$; \
         GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system;",
        )
        .await
        .expect("prepare disposable system schema owner");
    admin
        .batch_execute(SYSTEM_SCHEMA)
        .await
        .expect("production system schema");
    admin
        .batch_execute("RESET ROLE")
        .await
        .expect("restore fixture admin");
    admin
        .batch_execute(revoke_public_connect_floor_sql())
        .await
        .expect("production CONNECT floor");
    admin
        .batch_execute("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC")
        .await
        .expect("production TEMPORARY floor");
    admin
        .batch_execute(
            &prepare_identity_issuer_generation_sql(
                ISSUER,
                CredentialGeneration::A,
                PASSWORD,
                "2100-01-01T00:00:00Z",
            )
            .expect("scoped credential SQL"),
        )
        .await
        .expect("provision narrow identity credential");
    let mut scoped = url::Url::parse(&admin_url).expect("fixture URL shape");
    scoped.set_username(&role).expect("scoped login");
    scoped
        .set_password(Some(PASSWORD))
        .expect("fixture password");
    scoped.set_query(None);
    scoped.set_fragment(None);
    let scoped = scoped.to_string();

    let published = cli(&scoped, &["publish"]).await;
    let public = public_key(&published);
    assert_eq!(
        cli(&scoped, &["activate", "--kid", &public.kid]).await["activated"],
        true
    );
    let other = publish_session_key(&mut admin, "https://other.identity.service.internal")
        .await
        .expect("other issuer control");
    let config = IdentityConfig::new(ISSUER, &scoped).expect("validated service configuration");
    let service = IdentityService::connect(config)
        .await
        .expect("scoped service connection");
    assert!(!format!("{service:?}").contains(PASSWORD));
    assert!(!format!("{service:?}").contains(&scoped));
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
        reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
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
        !String::from_utf8_lossy(&bytes).contains(&other.kid),
        "another issuer is not projected"
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
        cli(&scoped, &["remove", "--kid", &public.kid]).await["removed"],
        true
    );
    assert_eq!(
        cli(&scoped, &["remove", "--kid", &public.kid]).await["removed"],
        false
    );
    assert_eq!(cli(&scoped, &["retire"]).await["retired"], 0);
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
    admin
        .batch_execute(&format!(
            "REVOKE SELECT ON identity.session_keys FROM {}",
            quote_ident(IDENTITY_ISSUER_ROLE)
        ))
        .await
        .expect("authoritative read refusal control");
    let failure = trusted
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .expect("fixed service refusal");
    assert_eq!(failure.status(), 503);
    assert_eq!(
        failure.text().await.expect("fixed refusal body"),
        "{\"error\":\"identity unavailable\"}"
    );
    serving.abort();
    assert!(
        serving
            .await
            .expect_err("stopped owned listener")
            .is_cancelled()
    );
    drop(trusted);
    admin.batch_execute("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;")
        .await.expect("remove owned disposable schemas");
    admin
        .batch_execute(&format!(
            "DROP OWNED BY {role}; DROP ROLE {role}; DROP OWNED BY {stable}; DROP ROLE {stable};",
            role = quote_ident(&role),
            stable = quote_ident(IDENTITY_ISSUER_ROLE)
        ))
        .await
        .expect("remove exact fixture authority roles");
    drop(admin);
    admin_driver.abort();
}

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .map_err(|_| ())
        .expect("connect disposable PostgreSQL without disclosing URL");
    (
        client,
        tokio::spawn(async move {
            let _ = connection.await;
        }),
    )
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

fn https_client(ca: &[u8]) -> reqwest::Client {
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .tls_backend_rustls()
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(ca).expect("fixture CA"))
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

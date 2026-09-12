//! The native identity process issues PATs only to verified TLS operators.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, grant_identity_issuer_surface_sql, identity_issuer_generation_role,
    prepare_identity_issuer_generation_sql,
};
use wamn_control_provision::session_target::SessionTarget;
use wamn_control_provision::sql::revoke_public_connect_floor_sql;
use wamn_control_provision::workload_role::{WorkloadRoleScope, workload_generation_role};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, project_env_database_name};
use wamn_control_registry::Triple;
use wamn_identity::tls_config_with_operator_ca;
use wamn_pg_core::quote_ident;
use wamn_platform_identity::session_keys::{activate_session_key, publish_session_key};
use wamn_platform_identity::session_token::{SessionClaims, sign_session_token};
use wamn_platform_identity::{
    PrincipalId, authenticate_pat, create_human, create_service, disable_principal, revoke_pat,
};

const ISSUER: &str = "https://identity.pat-test.internal";
const PASSWORD: &str = "operator-pat-disposable-fixture-password";
const SYSTEM_SCHEMA: &str = include_str!("../../../deploy/sql/system-schema.sql");
const FORBIDDEN: &str = "{\"error\":\"operator certificate required\"}";
const INVALID: &str = "{\"error\":\"invalid PAT request\"}";
const UNAVAILABLE: &str = "{\"error\":\"identity unavailable\"}";

// Upstream errors and response bodies can contain secrets. Never format them.
trait ExpectRedacted<T> {
    fn expect_redacted(self, context: &'static str) -> T;
}

impl<T, E> ExpectRedacted<T> for Result<T, E> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.ok().expect(context)
    }
}

struct Material {
    certificate: Vec<u8>,
    key: Vec<u8>,
}

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect_redacted("fixture clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("wamn-pat-issuance-{}-{nonce}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect_redacted("create private fixture directory");
        Self(path)
    }

    fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect_redacted("create private fixture file")
            .write_all(contents)
            .expect_redacted("write private fixture file");
        path
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process {
    child: Child,
    endpoint: String,
    stdout: BufReader<ChildStdout>,
}

#[test]
fn operator_ca_configuration_refuses_missing_or_malformed_roots() {
    let (issuer, _) = authority();
    let server = certificate(&issuer, false, false);
    for roots in [b"".as_slice(), b"not a certificate".as_slice()] {
        let error = tls_config_with_operator_ca(&server.certificate, &server.key, roots)
            .expect_err("invalid operator CA refuses listener startup");
        let detail = format!("{error:?} {error}");
        assert!(!detail.contains(&String::from_utf8_lossy(&server.key).to_string()));
        assert!(detail.contains("identity operator CA refused"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires armed disposable PostgreSQL 18; replaces identity, registry, and provisioning schemas"]
async fn operator_pat_issuance_over_https() {
    assert!(
        std::env::var("WAMN_PAT_ISSUANCE_ALLOW_SCHEMA_RESET").as_deref() == Ok("1"),
        "arm only an owned disposable cluster"
    );
    let raw = std::env::var("WAMN_PAT_ISSUANCE_PG_URL")
        .expect_redacted("provide disposable wamn_system URL");
    let parsed = url::Url::parse(&raw).expect_redacted("armed URL shape");
    assert!(
        parsed.path() == "/wamn_system",
        "never reset another database"
    );
    let (mut system, connection) = tokio_postgres::connect(&raw, NoTls)
        .await
        .expect_redacted("connect disposable PostgreSQL");
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let safe: bool = system.query_one("SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user", &[])
        .await.expect_redacted("disposable PG18 preflight").get(0);
    assert!(safe, "test needs dedicated PostgreSQL 18 superuser setup");
    let role = identity_issuer_generation_role(ISSUER, CredentialGeneration::A)
        .expect_redacted("issuer role");
    let exists: bool = system
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_roles WHERE rolname=ANY($1::text[]))",
            &[&vec![role.clone(), IDENTITY_ISSUER_ROLE.to_owned()]],
        )
        .await
        .expect_redacted("fixture role absence")
        .get(0);
    assert!(
        !exists,
        "refuse to replace existing identity authority roles"
    );
    system.batch_execute("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system; END IF; END $$; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system;")
        .await.expect_redacted("prepare disposable system schema owner");
    system
        .batch_execute(SYSTEM_SCHEMA)
        .await
        .expect_redacted("production system schema");
    system
        .batch_execute("RESET ROLE")
        .await
        .expect_redacted("restore fixture admin");
    system
        .batch_execute(revoke_public_connect_floor_sql())
        .await
        .expect_redacted("production CONNECT floor");
    system
        .batch_execute("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC")
        .await
        .expect_redacted("production TEMPORARY floor");
    system
        .batch_execute(
            &prepare_identity_issuer_generation_sql(
                ISSUER,
                CredentialGeneration::A,
                PASSWORD,
                "2100-01-01T00:00:00Z",
            )
            .expect_redacted("scoped credential SQL"),
        )
        .await
        .expect_redacted("provision issuer credential");
    let mut scoped = parsed.clone();
    scoped.set_username(&role).expect_redacted("scoped login");
    scoped
        .set_password(Some(PASSWORD))
        .expect_redacted("scoped password");
    scoped.set_query(None);
    scoped.set_fragment(None);
    let scoped = scoped.to_string();

    let human = create_human(&system, "pat-human", "PAT Human")
        .await
        .expect_redacted("human fixture");
    let machine = create_service(&system, "pat-machine", "PAT Machine")
        .await
        .expect_redacted("service fixture");
    let disabled = create_human(&system, "pat-disabled", "PAT Disabled")
        .await
        .expect_redacted("disabled fixture");
    disable_principal(&system, disabled.id())
        .await
        .expect_redacted("disable fixture principal");
    let key = publish_session_key(&mut system, ISSUER)
        .await
        .expect_redacted("publish fixture signing key");
    activate_session_key(&mut system, ISSUER, &key.kid)
        .await
        .expect_redacted("activate fixture signing key");
    let target = unused_target(&parsed);
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect_redacted("fixture clock")
        .as_secs() as i64;
    let jwt = sign_session_token(
        &mut system,
        SessionClaims {
            iss: ISSUER.to_owned(),
            sub: human.id().to_string(),
            org: "acme".to_owned(),
            aud: target.audience().to_owned(),
            roles: vec!["admin".to_owned()],
            exp: 0,
            iat: 0,
            jti: "operator-denial-control".to_owned(),
        },
        started,
    )
    .await
    .expect_redacted("genuine signed session denial control");

    let files = Files::new();
    let (server_authority, server_ca) = authority();
    let server = certificate(&server_authority, false, false);
    let (operator_authority, operator_ca) = authority();
    let operator = certificate(&operator_authority, true, false);
    let expired = certificate(&operator_authority, true, true);
    let wrong_usage = certificate(&operator_authority, false, false);
    let (foreign_authority, _) = authority();
    let foreign = certificate(&foreign_authority, true, false);
    let cert_path = files.write("server.crt", &server.certificate);
    let key_path = files.write("server.key", &server.key);
    let ca_path = files.write("operators.crt", &operator_ca);
    let target_path = files.write(
        "target.json",
        target
            .to_json()
            .expect_redacted("target document")
            .as_bytes(),
    );
    let anonymous = https_client(&server_ca, None);
    let authorized = https_client(&server_ca, Some(&operator));
    let before = snapshots(&system, false).await;
    let process = start(&scoped, &cert_path, &key_path, Some(&ca_path), &target_path).await;

    public_routes(&process.endpoint, &anonymous).await;
    for material in [&foreign, &expired, &wrong_usage] {
        let client = https_client(&server_ca, Some(material));
        assert!(
            request(&client, &process.endpoint, human.id(), "refused", 3600)
                .send()
                .await
                .is_err(),
            "untrusted, expired, or wrong-purpose certificate cannot reach a handler"
        );
    }
    assert_failure(
        request(
            &anonymous,
            &process.endpoint,
            human.id(),
            "absent certificate",
            3600,
        )
        .send()
        .await
        .expect_redacted("anonymous refusal"),
        403,
        FORBIDDEN,
    )
    .await;
    assert_failure(
        request(
            &anonymous,
            &process.endpoint,
            human.id(),
            "header spoof",
            3600,
        )
        .header("x-forwarded-client-cert", "verified operator")
        .header("x-ssl-client-verify", "SUCCESS")
        .header("x-operator", "true")
        .bearer_auth(jwt.token())
        .send()
        .await
        .expect_redacted("JWT and header refusal"),
        403,
        FORBIDDEN,
    )
    .await;
    assert!(
        before == snapshots(&system, false).await,
        "refused callers cannot change any system row"
    );

    let human_pat = success(
        &authorized,
        &process.endpoint,
        human.id(),
        "  operator human  ",
        &system,
    )
    .await;
    let machine_pat = success(
        &authorized,
        &process.endpoint,
        machine.id(),
        "operator service",
        &system,
    )
    .await;
    let unrelated = snapshots(&system, true).await;
    assert_failure(
        request(
            &anonymous,
            &process.endpoint,
            human.id(),
            "PAT refusal",
            3600,
        )
        .bearer_auth(human_pat["token"].as_str().expect("PAT string"))
        .send()
        .await
        .expect_redacted("PAT cannot authenticate an operator"),
        403,
        FORBIDDEN,
    )
    .await;
    let before_refusals = snapshots(&system, false).await;
    malformed_requests(&authorized, &process.endpoint, human.id(), disabled.id()).await;
    assert!(
        before_refusals == snapshots(&system, false).await,
        "all request refusals preserve every system row"
    );
    assert!(
        unrelated == snapshots(&system, true).await,
        "PAT issuance cannot create principals, roles, or membership grants"
    );

    let token = human_pat["token"].as_str().expect("PAT string");
    let mut forged = token.to_owned();
    let end = forged.pop().expect("token secret");
    forged.push(if end == 'a' { 'b' } else { 'a' });
    assert!(
        authenticate_pat(&system, &forged)
            .await
            .expect_redacted("full secret test")
            .is_none(),
        "known prefix alone is insufficient"
    );
    let prefix = human_pat["token_prefix"].as_str().expect("PAT prefix");
    revoke_pat(&system, prefix)
        .await
        .expect_redacted("revoke service-issued PAT");
    assert!(
        authenticate_pat(&system, token)
            .await
            .expect_redacted("revocation test")
            .is_none()
    );
    let machine_token = machine_pat["token"].as_str().expect("machine PAT string");
    disable_principal(&system, machine.id())
        .await
        .expect_redacted("disable service-issued principal");
    assert!(
        authenticate_pat(&system, machine_token)
            .await
            .expect_redacted("principal disable test")
            .is_none()
    );
    let expired_pat = success(
        &authorized,
        &process.endpoint,
        human.id(),
        "expiry control",
        &system,
    )
    .await;
    system.execute("UPDATE identity.pats SET created_at=clock_timestamp()-interval '2 hours', expires_at=clock_timestamp()-interval '1 hour' WHERE token_prefix=$1", &[&expired_pat["token_prefix"].as_str().expect("expiry prefix")])
        .await.expect_redacted("deterministic PAT expiry fixture");
    assert!(
        authenticate_pat(
            &system,
            expired_pat["token"].as_str().expect("expiry token")
        )
        .await
        .expect_redacted("expiry test")
        .is_none()
    );

    system
        .batch_execute(&format!(
            "REVOKE INSERT (principal_id, token_prefix, token_hash, label, expires_at) ON identity.pats FROM {}",
            quote_ident(IDENTITY_ISSUER_ROLE)
        ))
        .await
        .expect_redacted("database authority failure control");
    let before_failure = snapshots(&system, false).await;
    assert_failure(
        request(
            &authorized,
            &process.endpoint,
            human.id(),
            "database secret control",
            3600,
        )
        .send()
        .await
        .expect_redacted("database refusal"),
        503,
        UNAVAILABLE,
    )
    .await;
    assert!(
        before_failure == snapshots(&system, false).await,
        "database refusal cannot write a PAT"
    );
    system
        .batch_execute(&grant_identity_issuer_surface_sql())
        .await
        .expect_redacted("restore issuer fixture grants");

    stop(
        process,
        &[token, machine_token, jwt.token(), PASSWORD, &scoped],
    )
    .await;
    let no_ca = start(&scoped, &cert_path, &key_path, None, &target_path).await;
    public_routes(&no_ca.endpoint, &anonymous).await;
    assert_failure(
        request(
            &authorized,
            &no_ca.endpoint,
            human.id(),
            "no configured CA",
            3600,
        )
        .send()
        .await
        .expect_redacted("unconfigured operator authority refusal"),
        403,
        FORBIDDEN,
    )
    .await;
    stop(
        no_ca,
        &[token, machine_token, jwt.token(), PASSWORD, &scoped],
    )
    .await;

    system.batch_execute("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE;")
        .await.expect_redacted("remove owned fixture schemas");
    system
        .batch_execute(&format!(
            "DROP OWNED BY {role}; DROP ROLE {role}; DROP OWNED BY {stable}; DROP ROLE {stable};",
            role = quote_ident(&role),
            stable = quote_ident(IDENTITY_ISSUER_ROLE)
        ))
        .await
        .expect_redacted("remove exact fixture authority roles");
    drop(system);
    driver.abort();
    println!(
        "OPERATOR_PAT_ISSUANCE result=pass native_process=pass mtls=pass anonymous_surfaces=pass bearer_refused=pass token_predicates=pass no_side_effects=pass redaction=pass"
    );
}

fn request(
    client: &reqwest::Client,
    endpoint: &str,
    principal: &PrincipalId,
    label: &str,
    lifetime: u64,
) -> reqwest::RequestBuilder {
    client
        .post(format!("{endpoint}/pats"))
        .header("content-type", "application/json")
        .body(
            json!({"principal_id":principal.as_str(),"label":label,"lifetime_seconds":lifetime})
                .to_string(),
        )
}

async fn success(
    client: &reqwest::Client,
    endpoint: &str,
    principal: &PrincipalId,
    label: &str,
    system: &Client,
) -> Value {
    let before = snapshots(system, true).await;
    let previous_pats = pat_versions(system, "").await;
    let response = request(client, endpoint, principal, label, 3600)
        .send()
        .await
        .expect_redacted("operator issuance response");
    assert_eq!(response.status(), 201);
    assert!(response.headers()["cache-control"] == "no-store");
    assert!(response.headers()["content-type"] == "application/json");
    let bytes = response
        .bytes()
        .await
        .expect_redacted("one-time PAT response body");
    let value: Value = serde_json::from_slice(&bytes).expect_redacted("PAT response JSON");
    let fields: BTreeSet<_> = value
        .as_object()
        .expect("PAT object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        fields
            == BTreeSet::from([
                "token",
                "token_prefix",
                "principal_id",
                "created_at",
                "expires_at"
            ])
    );
    assert!(value["principal_id"] == principal.as_str());
    let token = value["token"].as_str().expect("PAT string");
    let authenticated = authenticate_pat(system, token)
        .await
        .expect_redacted("real PAT verification")
        .expect("issued token authenticates");
    assert!(authenticated.principal().id() == principal);
    let record = system.query_one("SELECT principal_id::text, token_hash, label, extract(epoch FROM expires_at-created_at)::bigint FROM identity.pats WHERE token_prefix=$1", &[&value["token_prefix"].as_str().expect("token prefix")])
        .await.expect_redacted("persisted PAT record");
    assert!(record.get::<_, String>(0) == principal.as_str());
    assert!(
        record.get::<_, String>(1) != token,
        "storage retains no raw PAT"
    );
    assert!(record.get::<_, String>(2) == label.trim());
    assert_eq!(record.get::<_, i64>(3), 3600);
    for field in ["created_at", "expires_at"] {
        let instant = value[field].as_str().expect("RFC 3339 instant");
        assert!(instant.len() == 20 && instant.ends_with('Z') && instant.contains('T'));
    }
    assert!(
        before == snapshots(system, true).await,
        "successful issuance changes only PAT storage"
    );
    assert!(
        previous_pats
            == pat_versions(
                system,
                value["token_prefix"].as_str().expect("new PAT prefix")
            )
            .await,
        "issuance adds exactly one PAT and leaves every older token unchanged"
    );
    value
}

async fn pat_versions(system: &Client, exclude_prefix: &str) -> Vec<String> {
    system.query_one("SELECT coalesce(array_agg(md5(row_to_json(t)::text)||':'||t.xmin::text||':'||t.ctid::text ORDER BY t.ctid),ARRAY[]::text[]) FROM identity.pats t WHERE token_prefix <> $1", &[&exclude_prefix])
        .await.expect_redacted("opaque PAT row-version snapshot").get(0)
}

async fn malformed_requests(
    client: &reqwest::Client,
    endpoint: &str,
    principal: &PrincipalId,
    disabled: &PrincipalId,
) {
    for (id, label, ttl) in [
        (principal.as_str(), "", 3600),
        (principal.as_str(), "   ", 3600),
        (principal.as_str(), "fixture", 0),
        (principal.as_str(), "fixture", 31_536_001),
        (disabled.as_str(), "fixture", 3600),
        ("00000000-0000-0000-0000-000000000001", "fixture", 3600),
        ("not-a-principal", "fixture", 3600),
    ] {
        let body = json!({"principal_id":id,"label":label,"lifetime_seconds":ttl}).to_string();
        assert_failure(
            request(client, endpoint, principal, "fixture", 3600)
                .body(body)
                .send()
                .await
                .expect_redacted("invalid request response"),
            400,
            INVALID,
        )
        .await;
    }
    let valid =
        json!({"principal_id":principal.as_str(),"label":"fixture","lifetime_seconds":3600});
    let mut extra = valid.clone();
    extra["roles"] = json!(["admin"]);
    let mut oversized = valid.clone();
    oversized["label"] = json!("x".repeat(1024));
    let mut long_label = valid.clone();
    long_label["label"] = json!("x".repeat(201));
    for body in [
        "{}".to_owned(),
        "[]".to_owned(),
        "not-json".to_owned(),
        extra.to_string(),
        oversized.to_string(),
        long_label.to_string(),
        format!(
            "{{\"principal_id\":\"{principal}\",\"label\":\"fixture\",\"lifetime_seconds\":1,\"lifetime_seconds\":1}}"
        ),
    ] {
        assert_failure(
            request(client, endpoint, principal, "fixture", 3600)
                .body(body)
                .send()
                .await
                .expect_redacted("malformed request response"),
            400,
            INVALID,
        )
        .await;
    }
    for request in [
        client
            .post(format!("{endpoint}/pats"))
            .body(valid.to_string()),
        client
            .post(format!("{endpoint}/pats"))
            .header("content-type", "text/plain")
            .body(valid.to_string()),
        request(client, endpoint, principal, "fixture", 3600)
            .header("content-type", "application/json"),
    ] {
        assert_failure(
            request.send().await.expect_redacted("content type refusal"),
            400,
            INVALID,
        )
        .await;
    }
    for request in [
        client.get(format!("{endpoint}/pats")),
        client.post(format!("{endpoint}/pats?principal_id={principal}")),
    ] {
        assert_eq!(
            request
                .send()
                .await
                .expect_redacted("route contract refusal")
                .status(),
            404
        );
    }
}

async fn assert_failure(response: reqwest::Response, status: u16, body: &str) {
    assert_eq!(response.status().as_u16(), status);
    assert!(response.headers()["cache-control"] == "no-store");
    assert!(response.headers()["content-type"] == "application/json");
    assert!(
        response.text().await.expect_redacted("fixed refusal body") == body,
        "fixed non-secret refusal"
    );
}

async fn public_routes(endpoint: &str, client: &reqwest::Client) {
    assert_eq!(
        client
            .get(format!("{endpoint}/healthz"))
            .send()
            .await
            .expect_redacted("anonymous health")
            .status(),
        200
    );
    let response = client
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .expect_redacted("anonymous JWKS");
    assert_eq!(response.status(), 200);
    assert!(response.headers()["cache-control"] == "public, max-age=300");
    let value: Value = serde_json::from_slice(&response.bytes().await.expect_redacted("JWKS body"))
        .expect_redacted("public JWKS JSON");
    assert_eq!(value["keys"].as_array().expect("public keys").len(), 1);
    assert_failure(
        client
            .post(format!("{endpoint}/session"))
            .send()
            .await
            .expect_redacted("anonymous exchange reaches bearer authentication"),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
}

async fn snapshots(system: &Client, exclude_pats: bool) -> Vec<Vec<String>> {
    let tables = system.query("SELECT n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('identity','registry','provisioning') AND c.relkind='r' AND NOT ($1 AND n.nspname='identity' AND c.relname='pats') ORDER BY n.nspname,c.relname", &[&exclude_pats])
        .await.expect_redacted("fixture relation inventory");
    let mut snapshots = Vec::new();
    for table in tables {
        let schema: String = table.get(0);
        let name: String = table.get(1);
        snapshots.push(system.query_one(&format!("SELECT coalesce(array_agg(md5(row_to_json(t)::text)||':'||t.xmin::text||':'||t.ctid::text ORDER BY t.ctid),ARRAY[]::text[]) FROM {}.{} t", quote_ident(&schema), quote_ident(&name)), &[])
            .await.expect_redacted("opaque row-version snapshot").get(0));
    }
    snapshots
}

fn unused_target(admin: &url::Url) -> SessionTarget {
    let triple = Triple {
        org: "acme".into(),
        project: "pattest".into(),
        env: "dev".into(),
    };
    let database = project_env_database_name("acme", "pattest", "dev", "pattest");
    let role = workload_generation_role(
        WorkloadRoleFamily::SessionRoleReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: "acme",
            project: "pattest",
            environment: "dev",
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect_redacted("unused reader scope");
    let mut url = admin.clone();
    url.set_username(&role)
        .expect_redacted("unused reader login");
    url.set_password(Some(PASSWORD))
        .expect_redacted("unused reader password");
    url.set_path(&database);
    url.set_query(None);
    url.set_fragment(None);
    SessionTarget::new(&triple, "pattest", "pat-tenant", url.as_str())
        .expect_redacted("unused configured audience")
}

async fn start(
    url: &str,
    certificate: &Path,
    key: &Path,
    ca: Option<&Path>,
    target: &Path,
) -> Process {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wamn-identity"));
    command
        .env("WAMN_IDENTITY_ISSUER", ISSUER)
        .env("WAMN_IDENTITY_DATABASE_URL", url)
        .env_remove("WAMN_IDENTITY_OPERATOR_CA")
        .args(["serve", "--bind", "127.0.0.1:0", "--tls-cert"])
        .arg(certificate)
        .arg("--tls-key")
        .arg(key)
        .arg("--session-target")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(ca) = ca {
        command.arg("--operator-ca").arg(ca);
    }
    let mut child = command
        .spawn()
        .expect_redacted("start native identity process");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped identity output"));
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
        .await
        .expect_redacted("bounded identity readiness event")
        .expect_redacted("identity readiness output");
    let address: SocketAddr = line
        .trim_end()
        .strip_prefix("identity listening on ")
        .expect("fixed identity readiness event")
        .parse()
        .expect_redacted("identity listen address");
    assert!(
        address.ip().is_loopback() && address.port() != 0,
        "owned loopback listener"
    );
    Process {
        child,
        endpoint: format!("https://{address}"),
        stdout,
    }
}

async fn stop(mut process: Process, secrets: &[&str]) {
    process
        .child
        .start_kill()
        .expect_redacted("stop owned identity process");
    let mut stdout = Vec::new();
    let (read, output) = tokio::join!(
        process.stdout.read_to_end(&mut stdout),
        process.child.wait_with_output()
    );
    read.expect_redacted("collect identity standard output");
    let output = output.expect_redacted("collect identity process output");
    for stream in [&stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        for secret in secrets {
            assert!(
                !text.contains(secret),
                "identity process output cannot disclose secrets"
            );
        }
        assert!(!text.contains("PRIVATE KEY"));
    }
}

fn authority() -> (Issuer<'static, KeyPair>, Vec<u8>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect_redacted("CA parameters");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().expect_redacted("CA key");
    let certificate = params.self_signed(&key).expect_redacted("CA certificate");
    (Issuer::new(params, key), certificate.pem().into_bytes())
}

fn certificate(issuer: &Issuer<'_, KeyPair>, operator: bool, expired: bool) -> Material {
    let mut params =
        CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect_redacted("TLS parameters");
    params.extended_key_usages = vec![if operator {
        ExtendedKeyUsagePurpose::ClientAuth
    } else {
        ExtendedKeyUsagePurpose::ServerAuth
    }];
    if expired {
        params.not_before = date_time_ymd(2000, 1, 1);
        params.not_after = date_time_ymd(2001, 1, 1);
    }
    let key = KeyPair::generate().expect_redacted("TLS key");
    let certificate = params
        .signed_by(&key, issuer)
        .expect_redacted("TLS certificate");
    Material {
        certificate: certificate.pem().into_bytes(),
        key: key.serialize_pem().into_bytes(),
    }
}

fn https_client(ca: &[u8], material: Option<&Material>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .tls_backend_rustls()
        .tls_certs_only(
            reqwest::Certificate::from_pem_bundle(ca).expect_redacted("trusted server CA"),
        );
    if let Some(material) = material {
        let pem = [material.certificate.as_slice(), material.key.as_slice()].concat();
        builder = builder.identity(
            reqwest::Identity::from_pem(&pem).expect_redacted("operator certificate identity"),
        );
    }
    builder.build().expect_redacted("HTTPS fixture client")
}

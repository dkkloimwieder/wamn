//! Real HTTPS exchanges against a disposable test PostgreSQL 18 server.
//!
//! This suite replaces the system schemas and creates three exact fixture
//! databases. It uses the production issuer and role-reader credential builders.
//! Tokens and database credentials must never appear in assertion diagnostics.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wamn_broker::auth_changes::AuthDrops;

use postgres_protocol::escape::escape_identifier;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use wamn_control_provision::identity_role::{IDENTITY_ISSUER_ROLE, identity_issuer_login};
use wamn_control_provision::project_env_database_name;
use wamn_control_provision::session_target::SessionTarget;
use wamn_control_provision::sql::{
    ensure_db_owner_role_sql, revoke_public_connect_base_grants_sql,
};
use wamn_control_provision::workload_role::{WorkloadRoleIdentity, workload_login};
use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL, WorkloadRoleFamily};
use wamn_identity::{IdentityConfig, IdentityService, serve, tls_config};
use wamn_identity_protocol::keys::SessionJwks;
use wamn_identity_protocol::test_util::{Audience, verify};
use wamn_identity_protocol::token::SessionClaims;
use wamn_platform_identity::session_keys::{activate_session_key, publish_session_key};
use wamn_platform_identity::{
    Principal, create_user, disable_principal, grant_project_env_membership,
    revoke_project_env_membership,
};
use wamn_registry::Triple;

#[path = "session_exchange/password_lifecycle.rs"]
mod password_lifecycle;

const ISSUER: &str = "https://identity.session-test.internal";
const PASSWORD: &str = "session-exchange-disposable-fixture-password";
const SUFFIX: &str = "s3ss10n2";
/// The test principal that each environment fixture writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
const TARGETS: [(&str, &str); 3] = [("demo", "dev"), ("demo", "prod"), ("other", "dev")];
/// Bounds a wait for an observed backend state. A wait ends when PostgreSQL
/// shows that state, never after a fixed delay. This bound only stops a test
/// that would otherwise hang because of a defect, so machine load cannot reach it.
const HANG_GUARD: Duration = Duration::from_secs(120);

// A failing fixture or mutated response must not append upstream error Debug:
// PostgreSQL DETAIL and deserialization errors can contain secret input values.
trait ExpectRedacted<T> {
    fn expect_redacted(self, context: &'static str) -> T;
}

impl<T, E> ExpectRedacted<T> for Result<T, E> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.ok().expect(context)
    }
}

impl<T> ExpectRedacted<T> for Option<T> {
    fn expect_redacted(self, context: &'static str) -> T {
        self.expect(context)
    }
}

struct Database {
    client: Client,
}

struct Fixture {
    observations: sqlx::PgPool,
    system: Database,
    environments: Vec<Database>,
    targets: Vec<SessionTarget>,
    issuer_url: String,
    issuer_role: String,
    roles: Vec<String>,
}

struct Https {
    endpoint: String,
    client: reqwest::Client,
    serving: JoinHandle<Result<(), wamn_identity::IdentityServiceError>>,
}

impl Drop for Https {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

async fn assert_failure(response: reqwest::Response, status: u16, body: &str) {
    assert_eq!(response.status().as_u16(), status);
    assert!(
        response.headers()["cache-control"] == "no-store",
        "refusal must not be cached"
    );
    assert!(
        response.headers()["content-type"] == "application/json",
        "refusal must be JSON"
    );
    if status == 401 {
        assert!(
            response.headers()["www-authenticate"] == "Bearer",
            "fixed bearer challenge"
        );
    }
    // Boolean equality avoids printing a token or key if a refusal regresses.
    assert!(
        response.text().await.expect_redacted("refusal body") == body,
        "fixed indistinguishable refusal body"
    );
}

async fn verify_response(
    response: reqwest::Response,
    target: &SessionTarget,
    principal: &Principal,
    roles: &[&str],
    jwks: &SessionJwks,
) -> SessionClaims {
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["cache-control"] == "no-store",
        "login must not be cached"
    );
    assert!(
        response.headers()["content-type"] == "application/json",
        "login must be JSON"
    );
    let bytes = response.bytes().await.expect_redacted("login body");
    assert!(!String::from_utf8_lossy(&bytes).contains(PASSWORD));
    let body: Value = serde_json::from_slice(&bytes).expect_redacted("login JSON");
    assert_fields(
        &body,
        &[
            "access_token",
            "expires_at",
            "token_type",
            "renewal_token",
            "login_expires_at",
            "sub",
        ],
    );
    assert_eq!(body["token_type"], "Bearer");
    let token = body["access_token"]
        .as_str()
        .expect_redacted("bearer string");
    let kid = jsonwebtoken::decode_header(token)
        .expect_redacted("fixed signature profile")
        .kid
        .expect("key ID");
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .expect_redacted("key from actual public JWKS");
    let coordinate = Audience {
        issuer: ISSUER,
        org: &target.triple().org,
        audience: target.audience(),
    };
    let claims = verify(token, key, coordinate, unix_seconds())
        .await
        .expect_redacted("public signature and exact coordinate");
    assert!(claims.sub == *principal.id(), "exact authenticated subject");
    assert!(
        claims.roles
            == roles
                .iter()
                .map(|role| role.parse::<wamn_identity_protocol::RoleName>().unwrap())
                .collect::<Vec<_>>(),
        "exact current project roles"
    );
    assert_eq!(body["expires_at"].as_i64(), Some(claims.exp));
    assert_eq!(body["sub"], serde_json::to_value(claims.sub).unwrap());
    for wrong in [
        Audience {
            issuer: "https://wrong-issuer.invalid",
            ..coordinate
        },
        Audience {
            org: "wrong-org",
            ..coordinate
        },
        Audience {
            audience: "wrong-audience",
            ..coordinate
        },
    ] {
        assert!(
            verify(token, key, wrong, unix_seconds()).await.is_err(),
            "signed JWT claims cannot change configured coordinate"
        );
    }
    claims
}

async fn wait_for_blocked_issuer(system: &mut Client, issuer_role: &str) {
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            sqlx::query("SELECT pg_stat_clear_snapshot()").fetch_one(&mut *system).await.expect_redacted("refresh backend observation");
            let blocked: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename=$1 AND pg_backend_pid()=ANY(pg_blocking_pids(pid)))").bind(&issuer_role).fetch_one(&mut *system)
                .await.expect_redacted("observe actual issuer wait").get(0);
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect_redacted("issuer must reach the held PostgreSQL lock");
}

async fn seed_user(client: &mut Client, principal: &Principal, role: &str) {
    sqlx::query("INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'user',$2) ON CONFLICT (id) DO UPDATE SET status='active'").bind(&principal.id().to_string().as_str()).bind(&format!("{}@fixture.invalid", principal.id())).execute(&mut *client).await.map(|result| result.rows_affected()).expect_redacted("project user fixture");
    sqlx::query("INSERT INTO platform.role (name) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(&role)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("project role fixture");
    sqlx::query("INSERT INTO platform.user_role (user_id,role_name) VALUES ($1::text::uuid,$2) ON CONFLICT DO NOTHING").bind(&principal.id().to_string().as_str()).bind(&role).execute(&mut *client).await.map(|result| result.rows_affected()).expect_redacted("exact project/principal role fixture");
}

async fn grant(system: &mut Client, principal: &Principal, org: &str, env: &str) {
    wamn_platform_identity::org::activate_org_membership(system, principal.id(), org)
        .await
        .expect_redacted("active org membership fixture");
    grant_project_env_membership(system, principal.id(), org, "widgets", env)
        .await
        .expect_redacted("explicit environment grant");
}

async fn setup(raw: &str) -> Fixture {
    let mut admin = url::Url::parse(raw).expect_redacted("test database URL shape");
    assert!(
        admin.path() == "/wamn_system",
        "never reset another database"
    );
    let mut system = connect(raw).await;
    let safe: bool = sqlx::query("SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user").fetch_one(&mut system.client)
        .await.expect_redacted("disposable PG18 preflight").get(0);
    assert!(safe, "test needs dedicated PostgreSQL 18 superuser setup");
    let issuer_role = identity_issuer_login(ISSUER).expect_redacted("issuer role");
    let mut roles = vec![
        issuer_role.clone(),
        IDENTITY_ISSUER_ROLE.to_owned(),
        WorkloadRoleFamily::SessionRoleReader.acl_role().to_owned(),
    ];
    roles.extend(["wamn_app", "wamn_platform", "wamn_run_retention"].map(str::to_owned));
    let mut targets = Vec::new();
    for (org, env) in TARGETS {
        let triple = Triple {
            org: org.into(),
            project: "widgets".into(),
            env: env.into(),
        };
        let name = project_env_database_name(org, "widgets", env, SUFFIX);
        let role = workload_login(
            WorkloadRoleFamily::SessionRoleReader,
            WorkloadRoleIdentity::ProjectEnvironment {
                org,
                project: "widgets",
                environment: env,
                database: &name,
            },
        )
        .expect_redacted("reader role");
        let target = SessionTarget::new(
            &triple,
            SUFFIX,
            &wamn_registry::project_env_key(org, "widgets", env),
            &login_url(&mut admin, &name, &role),
        )
        .expect_redacted("production target capability");
        assert!(!format!("{target:?}").contains(PASSWORD));
        roles.push(role);
        targets.push(target);
    }
    let databases: Vec<String> = targets
        .iter()
        .map(|t| t.connection().database.as_str().to_owned())
        .collect();
    let exists: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname=ANY($1::text[])) OR EXISTS (SELECT FROM pg_database WHERE datname=ANY($2::text[]))").bind(&roles).bind(&databases).fetch_one(&mut system.client)
        .await.expect_redacted("exact fixture names must be absent").get(0);
    assert!(
        !exists,
        "refuse to overwrite existing fixture databases or authority roles"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut system.client)
    .await
    .map(drop)
    .expect_redacted("record history grants name wamn_db_owner");
    sqlx::raw_sql("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system; END IF; END $$; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system;").execute(&mut system.client).await.map(drop).expect_redacted("prepare armed disposable system owner");
    sqlx::raw_sql(SYSTEM_SCHEMA_SQL)
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("production system schema");
    sqlx::raw_sql("RESET ROLE; CREATE ROLE wamn_app NOLOGIN NOINHERIT;  ")
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("schema prerequisite roles");
    // The system fixture is platform setup, so it writes as wamn:provisioning.
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&PlatformComponent::Provisioning.principal_id().to_string())
        .execute(&mut system.client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("bind wamn:provisioning for the system fixture session");
    for name in &databases {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {}",
            escape_identifier(name)
        )))
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("create exact disposable environment");
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (revoke_public_connect_base_grants_sql()).to_string(),
    ))
    .execute(&mut system.client)
    .await
    .map(drop)
    .expect_redacted("production CONNECT minimum on disposable cluster");
    sqlx::raw_sql("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC")
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("system TEMP minimum");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::issuer_login_sql(ISSUER, PASSWORD)
            .expect_redacted("issuer credential SQL"))
            .to_string(),
    ))
    .execute(&mut system.client)
    .await
    .map(drop)
    .expect_redacted("actual issuer provisioning");
    let issuer_url = login_url(&mut admin, "wamn_system", &issuer_role);
    let mut key_authority = connect(&issuer_url).await;
    let public = publish_session_key(&mut key_authority.client, ISSUER)
        .await
        .expect_redacted("publish through provisioned issuer");
    activate_session_key(&mut key_authority.client, ISSUER, &public.kid)
        .await
        .expect_redacted("activate through provisioned issuer");
    drop(key_authority);
    let mut environments = Vec::new();
    for target in &targets {
        let mut url = admin.clone();
        url.set_path(&format!("/{}", target.connection().database.as_str()));
        let mut environment = connect(url.as_str()).await;
        for schema in [
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../deploy/sql/run-state.sql"),
            include_str!("../../../deploy/sql/run-queue.sql"),
            include_str!("../../../deploy/sql/app-schema.sql"),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe((schema).to_string()))
                .execute(&mut environment.client)
                .await
                .map(drop)
                .expect_redacted("production environment schema");
        }
        // The fixture writes as its test principal, whose row stamps itself.
        // Both bindings stay for the later fixture writes of this session.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "SET app.user_id = '{FIXTURE_PRINCIPAL}'; \
                 SET app.operation = 'admin:session-exchange-fixture'; \
                 INSERT INTO platform.\"user\" (id,type,email) \
                 VALUES ('{FIXTURE_PRINCIPAL}','user','fixture@example.invalid');"
            ))
                .to_string(),
        ))
        .execute(&mut environment.client)
        .await
        .map(drop)
        .expect_redacted("environment test principal");
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "REVOKE TEMPORARY ON DATABASE {} FROM PUBLIC",
                escape_identifier(target.connection().database.as_str())
            ))
                .to_string(),
        ))
        .execute(&mut environment.client)
        .await
        .map(drop)
        .expect_redacted("environment TEMP minimum");
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                WorkloadRoleFamily::SessionRoleReader,
                target.connection().database.as_str(),
                target.connection().username.as_str(),
                PASSWORD,
            ))
                .to_string(),
        ))
        .execute(&mut environment.client)
        .await
        .map(drop)
        .expect_redacted("actual dedicated role-reader provisioning");
        let triple = target.triple();
        sqlx::query("INSERT INTO registry.orgs (id,placement_type) VALUES ($1,'dedicated') ON CONFLICT DO NOTHING").bind(&triple.org).execute(&mut system.client).await.map(|result| result.rows_affected()).expect_redacted("fixture org");
        sqlx::query(
            "INSERT INTO registry.projects (org,id) VALUES ($1,'widgets') ON CONFLICT DO NOTHING",
        )
        .bind(&triple.org)
        .execute(&mut system.client)
        .await
        .map(|result| result.rows_affected())
        .expect_redacted("fixture project");
        sqlx::query("INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ($1,$2,'\"own\"',0,1,'1Gi','1','1Gi','fixture')").bind(&triple.org).bind(&triple.env.as_str()).execute(&mut system.client).await.map(|result| result.rows_affected()).expect_redacted("fixture environment policy");
        sqlx::query("WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ($1,$2,$2) ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ($1,'widgets',$2,'fixture-reference',$3)").bind(&triple.org).bind(&triple.env.as_str()).bind(&SUFFIX).execute(&mut system.client).await.map(|result| result.rows_affected()).expect_redacted("current environment registry entry");
        environments.push(environment);
    }
    Fixture {
        observations: wamn_postgres::pool(raw, 4).await.unwrap(),
        system,
        environments,
        targets,
        issuer_url,
        issuer_role,
        roles,
    }
}

async fn cleanup(fixture: Fixture) {
    let Fixture {
        mut system,
        environments,
        targets,
        roles,
        ..
    } = fixture;
    drop(environments);
    for target in targets {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "DROP DATABASE {} WITH (FORCE)",
                escape_identifier(target.connection().database.as_str())
            ))
                .to_string(),
        ))
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("remove only owned disposable environment");
    }
    sqlx::raw_sql("DROP SCHEMA identity CASCADE; DROP SCHEMA provisioning CASCADE; DROP SCHEMA registry CASCADE").execute(&mut system.client).await.map(drop).expect_redacted("remove owned system fixture schemas");
    for role in roles {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "DROP OWNED BY {role}; DROP ROLE {role}",
                role = escape_identifier(&role)
            ))
                .to_string(),
        ))
        .execute(&mut system.client)
        .await
        .map(drop)
        .expect_redacted("remove exact fixture authority role");
    }
}

async fn start_config(config: IdentityConfig) -> Https {
    assert!(!format!("{config:?}").contains(PASSWORD));
    let service = IdentityService::connect(config)
        .await
        .expect_redacted("production targeted service");
    assert!(!format!("{service:?}").contains(PASSWORD));
    let (certificate, private, ca) = certificates();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect_redacted("HTTPS listener");
    let endpoint = format!(
        "https://{}",
        listener.local_addr().expect_redacted("listener address")
    );
    let serving = tokio::spawn(serve(
        listener,
        service,
        tls_config(&certificate, &private).expect_redacted("production TLS"),
    ));
    let mut client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .tls_backend_rustls()
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca).expect_redacted("fixture CA"))
        .build()
        .expect_redacted("trusted HTTPS client");
    Https {
        endpoint,
        client,
        serving,
    }
}

async fn connect(url: &str) -> Database {
    Database {
        client: wamn_postgres::connect(url)
            .await
            .expect_redacted("database connection"),
    }
}

fn login_url(admin: &url::Url, database: &str, role: &str) -> String {
    let mut url = admin.clone();
    url.set_path(&format!("/{database}"));
    url.set_username(role).expect_redacted("targeted role");
    url.set_password(Some(PASSWORD))
        .expect_redacted("fixture password");
    url.set_fragment(None);
    url.into()
}

fn assert_fields(value: &Value, fields: &[&str]) {
    assert!(
        value
            .as_object()
            .expect_redacted("public JSON object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == fields.iter().copied().collect::<BTreeSet<_>>(),
        "exact public JSON field names"
    );
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect_redacted("clock after epoch")
        .as_secs()
        .try_into()
        .expect_redacted("Unix seconds fit")
}

fn certificates() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut ca_params =
        CertificateParams::new(Vec::<String>::new()).expect_redacted("CA parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().expect_redacted("CA key");
    let ca = ca_params
        .self_signed(&ca_key)
        .expect_redacted("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().expect_redacted("TLS key");
    let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
        .expect_redacted("TLS parameters")
        .signed_by(&key, &issuer)
        .expect_redacted("TLS certificate");
    (
        certificate.pem().into_bytes(),
        key.serialize_pem().into_bytes(),
        ca.pem().into_bytes(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_enrollment_and_sessions_preserve_current_authority() {
    use wamn_platform_identity::password::issue_invitation;
    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("fresh password fixture");
    let db = postgres
        .create_database("wamn_system")
        .expect_redacted("password system database");
    let mut fixture = setup(db.url()).await;
    let alice = create_user(
        &mut fixture.system.client,
        "password-alice",
        "password-alice@example.invalid",
        "Alice",
    )
    .await
    .expect_redacted("password user");
    seed_user(&mut fixture.environments[0].client, &alice, "receiver").await;
    grant(&mut fixture.system.client, &alice, "demo", "dev").await;
    let actor = PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap();
    let mut issuer = connect(&fixture.issuer_url).await;
    let invitation = issue_invitation(&mut issuer.client, &actor, alice.id())
        .await
        .expect_redacted("invitation through actual issuer grants");
    let config = IdentityConfig::new(ISSUER, &fixture.issuer_url)
        .unwrap()
        .with_session_targets(fixture.targets.clone())
        .unwrap()
        .with_resend(
            wamn_identity::mail::ResendConfig::new(
                "fixture-key".into(),
                "WAMN <fixture@example.invalid>".into(),
            )
            .unwrap(),
        );
    let https = start_config(config.clone()).await;
    let replica = start_config(config).await;
    let post = |server: &Https, path: &str, body: Value| {
        server
            .client
            .post(format!("{}{path}", server.endpoint))
            .json(&body)
    };
    // Never contact Resend: operator authentication refuses before mail work.
    let denied = post(
        &https,
        "/invitations",
        json!({"principal_id":alice.id().to_string().as_str()}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(denied.status(), 403);
    let enroll = json!({"principal_id":alice.id().to_string().as_str(),"invitation":invitation.secret(),"password":PASSWORD});
    assert_eq!(
        post(&https, "/password/enroll", enroll.clone())
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        post(&https, "/password/enroll", enroll)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let login = json!({"email":" PASSWORD-ALICE@EXAMPLE.INVALID ","password":PASSWORD,"aud":fixture.targets[0].audience()});
    let jwks = wamn_platform_identity::session_keys::session_jwks(&mut issuer.client, ISSUER)
        .await
        .unwrap();
    verify_response(
        post(&https, "/password/session", login.clone())
            .send()
            .await
            .unwrap(),
        &fixture.targets[0],
        &alice,
        &["receiver"],
        &jwks,
    )
    .await;
    for (email, password) in [
        ("absent@example.invalid", PASSWORD),
        ("password-alice@example.invalid", "incorrect password"),
    ] {
        let reply = post(
            &replica,
            "/password/session",
            json!({"email":email,"password":password,"aud":fixture.targets[0].audience()}),
        )
        .send()
        .await
        .unwrap();
        assert_failure(reply, 401, "{\"error\":\"unauthorized\"}").await;
    }
    revoke_project_env_membership(
        &mut fixture.system.client,
        alice.id(),
        "demo",
        "widgets",
        "dev",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        post(&https, "/password/session", login.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    grant(&mut fixture.system.client, &alice, "demo", "dev").await;
    sqlx::query("UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid")
        .bind(&alice.id().to_string().as_str())
        .execute(&mut fixture.environments[0].client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    assert_eq!(
        post(&replica, "/password/session", login.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    sqlx::query("UPDATE platform.\"user\" SET status='active' WHERE id=$1::text::uuid")
        .bind(&alice.id().to_string().as_str())
        .execute(&mut fixture.environments[0].client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    disable_principal(
        &mut fixture.system.client,
        alice.id(),
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        post(&https, "/password/session", login.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let throttled = post(&replica, "/password/session", login.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(throttled.status(), 429);
    assert_eq!(throttled.headers()["retry-after"], "60");
    // Advance only the owned counters, without sleeping, to test recovery.
    sqlx::raw_sql("UPDATE identity.password_attempts SET started_at=clock_timestamp()-interval '61 seconds'; UPDATE identity.principals SET status='active', disabled_at=NULL WHERE subject='password-alice'").execute(&mut fixture.system.client).await.map(drop).unwrap();
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='replaced' WHERE org='demo' AND env='dev'").execute(&mut fixture.system.client).await.map(drop).unwrap();
    assert_eq!(
        post(&replica, "/password/session", login.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='s3ss10n2' WHERE org='demo' AND env='dev'").execute(&mut fixture.system.client).await.map(drop).unwrap();
    verify_response(
        post(&replica, "/password/session", login)
            .send()
            .await
            .unwrap(),
        &fixture.targets[0],
        &alice,
        &["receiver"],
        &jwks,
    )
    .await;
    drop(issuer);
    drop(https);
    drop(replica);
    cleanup(fixture).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_environment_discovery_requires_current_authority() {
    use wamn_platform_identity::password::issue_invitation;

    let mut postgres = wamn_test_postgres::start(&[]).expect_redacted("fresh discovery fixture");
    let db = postgres
        .create_database("wamn_system")
        .expect_redacted("discovery database");
    let mut fixture = setup(db.url()).await;
    let alice = create_user(
        &mut fixture.system.client,
        "discovery-alice",
        "discovery@example.invalid",
        "Alice",
    )
    .await
    .expect_redacted("discovery user");
    for database in &mut fixture.environments {
        seed_user(&mut database.client, &alice, "receiver").await;
    }
    let actor = PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap();
    let mut issuer = connect(&fixture.issuer_url).await;
    let invitation = issue_invitation(&mut issuer.client, &actor, alice.id())
        .await
        .expect_redacted("discovery invitation");
    let config = IdentityConfig::new(ISSUER, &fixture.issuer_url)
        .unwrap()
        .with_session_targets(fixture.targets.clone())
        .unwrap()
        .with_resend(
            wamn_identity::mail::ResendConfig::new(
                "unused".into(),
                "WAMN <fixture@example.invalid>".into(),
            )
            .unwrap(),
        );
    let https = start_config(config).await;
    let response = https.client.post(format!("{}/password/enroll", https.endpoint))
        .json(&json!({"principal_id":alice.id().to_string().as_str(), "invitation":invitation.secret(), "password":PASSWORD}))
        .send().await.expect_redacted("enroll discovery account");
    assert_eq!(response.status(), 204);
    let credentials = json!({"email":" DISCOVERY@EXAMPLE.INVALID ", "password":PASSWORD});
    let discover = || {
        https
            .client
            .post(format!("{}/password/environments", https.endpoint))
            .json(&credentials)
    };
    let expected = |indices: &[usize]| {
        json!({"next_cursor": null, "total": indices.len(), "environments": indices.iter().map(|index| {
        let target = &fixture.targets[*index];
        json!({"aud":target.audience(), "org":target.triple().org, "project":"widgets", "env":target.triple().env.as_str()})
    }).collect::<Vec<_>>()})
    };

    // A project role alone never supplies membership, including across organizations.
    assert_environments(discover().send().await.unwrap(), expected(&[])).await;
    grant(&mut fixture.system.client, &alice, "demo", "dev").await;
    assert_environments(discover().send().await.unwrap(), expected(&[0])).await;
    grant(&mut fixture.system.client, &alice, "demo", "prod").await;
    assert_environments(discover().send().await.unwrap(), expected(&[0, 1])).await;
    grant(&mut fixture.system.client, &alice, "other", "dev").await;
    assert_environments(discover().send().await.unwrap(), expected(&[0, 1, 2])).await;
    revoke_project_env_membership(
        &mut fixture.system.client,
        alice.id(),
        "other",
        "widgets",
        "dev",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[0, 1])).await;
    // Discovery and issuance consume the same account budget; selecting a new route
    // cannot bypass it. No token is minted by the five preceding discoveries.
    let login = json!({"email":"discovery@example.invalid", "password":PASSWORD, "aud":fixture.targets[0].audience()});
    assert_eq!(
        https
            .client
            .post(format!("{}/password/session", https.endpoint))
            .json(&login)
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    discovery_window(&fixture.observations).await;

    sqlx::query("DELETE FROM platform.user_role WHERE user_id=$1::text::uuid")
        .bind(&alice.id().to_string().as_str())
        .execute(&mut fixture.environments[1].client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[0])).await;
    sqlx::query("UPDATE platform.\"user\" SET status='disabled' WHERE id=$1::text::uuid")
        .bind(&alice.id().to_string().as_str())
        .execute(&mut fixture.environments[0].client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[])).await;
    seed_user(&mut fixture.environments[0].client, &alice, "receiver").await;
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='replaced' WHERE org='demo' AND env='dev'").execute(&mut fixture.system.client).await.map(drop).unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[])).await;
    sqlx::raw_sql("UPDATE registry.environment_projects SET instance_suffix='s3ss10n2' WHERE org='demo' AND env='dev'").execute(&mut fixture.system.client).await.map(drop).unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[0])).await;
    // An inactive environment is not offered.
    discovery_window(&fixture.observations).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("UPDATE registry.environments SET status='inactive' WHERE org='demo' AND env='dev'")
            .to_string(),
    ))
    .execute(&mut fixture.system.client)
    .await
    .map(drop)
    .unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[])).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("UPDATE registry.environments SET status='active' WHERE org='demo' AND env='dev'")
            .to_string(),
    ))
    .execute(&mut fixture.system.client)
    .await
    .map(drop)
    .unwrap();
    assert_environments(discover().send().await.unwrap(), expected(&[0])).await;
    // A prior discovery cannot authorize issuance after membership changes.
    revoke_project_env_membership(
        &mut fixture.system.client,
        alice.id(),
        "demo",
        "widgets",
        "dev",
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    assert_failure(
        https
            .client
            .post(format!("{}/password/session", https.endpoint))
            .json(&login)
            .send()
            .await
            .unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    discovery_window(&fixture.observations).await;

    for (email, password) in [
        ("absent@example.invalid", PASSWORD),
        ("discovery@example.invalid", "incorrect password"),
    ] {
        let response = https
            .client
            .post(format!("{}/password/environments", https.endpoint))
            .json(&json!({"email":email,"password":password}))
            .send()
            .await
            .unwrap();
        assert_failure(response, 401, "{\"error\":\"unauthorized\"}").await;
    }
    disable_principal(
        &mut fixture.system.client,
        alice.id(),
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    assert_failure(
        discover().send().await.unwrap(),
        401,
        "{\"error\":\"unauthorized\"}",
    )
    .await;
    // Discovery accepts no caller-provided authority or database addresses.
    for extra in ["aud", "roles", "database_url"] {
        let mut invalid = credentials.clone();
        invalid[extra] = json!("untrusted");
        assert_eq!(
            https
                .client
                .post(format!("{}/password/environments", https.endpoint))
                .json(&invalid)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let pats: i64 =
        sqlx::query("SELECT count(*) FROM identity.pats WHERE principal_id=$1::text::uuid")
            .bind(&alice.id().to_string().as_str())
            .fetch_one(&mut fixture.system.client)
            .await
            .unwrap()
            .get(0);
    assert_eq!(pats, 0, "discovery must not create a hidden PAT");
    drop(issuer);
    drop(https);
    cleanup(fixture).await;
}

async fn assert_environments(response: reqwest::Response, expected: Value) {
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let actual = response
        .json::<Value>()
        .await
        .expect_redacted("public environment response");
    // Exact public fields also exclude session tokens and database credentials.
    assert!(
        actual == expected,
        "authorized public environment list differs"
    );
}

async fn discovery_window(observations: &sqlx::PgPool) {
    sqlx::raw_sql(
        "UPDATE identity.password_attempts SET started_at=clock_timestamp()-interval '61 seconds'",
    )
    .execute(observations)
    .await
    .map(drop)
    .expect_redacted("advance only the owned throttle window");
}

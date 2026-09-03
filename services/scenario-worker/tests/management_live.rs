//! Real-PostgreSQL gate for the authenticated management authoring surface.
//!
//! Proves the whole bridge end to end over HTTP: a valid personal access token
//! reaches a landed authoring command with trusted principal and role context;
//! absent, forged, expired, revoked, cross-project, and client-injected identity
//! all refuse before the command runs and leave no side effect; and two
//! principals running the same command stay distinguishable in the append-only
//! ledger.
//!
//! Both surviving commands are mounted. An untrusted presenter naming either
//! receives the same refusal document, so the surface is no route-existence
//! oracle; admitted authors exercise the gate-then-publish transition below.
//!
//! Proving that judgment takes a SECOND database. The project-environment
//! database this gate creates alongside the control one is where the candidate
//! wirings and the component library's effect posture live; the surface reaches
//! it through the scoped `wamn_management_admitter` generation.
//!
//! # What wamn-0h0g.8.5.5 took out of this gate
//!
//! The sequential per-ordinal composition — reserve, admit, poll, evaluate,
//! finalize — and the three control-plane relations it wrote to are DELETED. A
//! gate is a judgment about a document, not an execution of it, so what remains
//! to prove is that the verb judges, refuses typed, attributes exactly once, and
//! **executes nothing**: no run, no queue row, no stored report. The run and
//! queue reads below are kept precisely to prove that emptiness, so a
//! resurrection of the composition would fail here rather than pass silently.
//!
//! It NO LONGER owns `wamn-ftfc.2`'s S1 write path. That half proved a checkout
//! client's submitted definition reached a server-side draft store, its exact
//! stored revision read back, and a stale working copy refusing — every one of
//! which is a claim about `catalog.flow_drafts`, deleted by wamn-0h0g.8.5.5. A
//! draft is a CLIENT-SIDE FILE now, so the platform has nothing to prove about
//! storing one. The authentication, attribution and append-only-ledger
//! properties those sections carried survive on the mounted gate below, which
//! reaches the ledger under two distinct principals.
//!
//! The recipe in `docs/operations/build-and-test.md` supplies one disposable database.

use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_postgres::{Client, NoTls};

use wamn_catalog::ConnectionTypeDescriptor;
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader,
    WorkloadRoleFamily, control_author_generation_role, management_admitter_generation_role, sql,
    system_reader_generation_role,
};
use wamn_platform_identity::{
    IssuedPat, PAT_TOKEN_PREFIX, assign_project_role, create_human, create_service, issue_pat,
    resolve_subject, revoke_pat,
};
use wamn_schema_control::connections::ComponentConnectionRequirement;

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

const TENANT: &str = "management-live-tenant";
/// Fixed by the control store (wamn-0h0g.8.18): both the identity registry this
/// gate authenticates against and the authoring store it writes to live in the
/// one control database, so there is no schema to rewrite.
const SOURCE_SCHEMA: &str = "wamn_run";
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const OTHER_PROJECT: &str = "shipping";
const ENVIRONMENT: &str = "dev";
const AUTHOR_PASSWORD: &str = "wamn-management-live";
/// Password for the identity-reader generation this gate mints (`wamn-0h0g.12.67`).
const IDENTITY_READER_PASSWORD: &str = "wamn-management-identity-read-live";
/// The project-environment database the admission credential is scoped to. The
/// generation role name binds it, so the gate cannot rename one without the
/// other.
const PROJECT_DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";
const ADMITTER_PASSWORD: &str = "wamn-management-admission-live";
/// One wiring document, exactly as `catalog.wirings.graph_json` stores it.
///
/// Both seeded candidates and the `publish` input differ only in which component
/// operation the single node reaches, so they share one builder — a document
/// written twice is a document that can drift on one of them.
fn candidate_graph(wiring_id: &str, component: &str, operation: &str) -> serde_json::Value {
    serde_json::json!({
        "format-version": "0.1",
        "wiring-id": wiring_id,
        "version": 1,
        "entry": "node",
        "nodes": {
            "node": {
                "component": component,
                "interface-version": "0.1",
                "operation": operation,
            },
        },
        "cases": [
            {
                "case-id": "creates",
                "input": {"name": "first"},
                "expect": {"outcome": "responded", "status": 201},
            },
            {
                "case-id": "rejects",
                "input": {"name": "second"},
                "expect": {"outcome": "responded", "status": 200},
            },
        ],
    })
}

/// The exact package version every seeded candidate is gated against.
const CANDIDATE_PACKAGE_VERSION: &str = "1.0.0";
/// The active effective release exercised later by publication and release minting.
const CANDIDATE_EFFECTIVE_RELEASE_ID: i32 = 1;
/// The identity the SERVER will derive for one submitted document.
///
/// It is derived here the same way and by the same reader (wamn-0h0g.8.28), not
/// written down. A literal would be a second copy of a derived value, and the
/// old one was a fiction: the gate resolved its candidate by a hash the fixture
/// INVENTED and stored, so the identity the report was keyed under had never
/// been computed from the bytes it named.
fn derived_hash(document: &serde_json::Value) -> String {
    wamn_catalog::WiringDocument::parse(document)
        .expect("the fixture document is a valid wiring document")
        .wiring_hash()
        .as_str()
        .to_owned()
}

/// The one candidate wiring `gate` is exercised against.
///
/// The wiring hash is the WHOLE identity (wamn-0h0g.8.5.6): it is the key the
/// accepted gate's report is stored under, the report id the receipt hands back,
/// and what `get-report` resolves. There is no second identifier to derive,
/// choose, or mismatch -- the column that used to carry one is gone.
const CANDIDATE_PACKAGE: &str = "candidate_package";
const CANDIDATE_MANIFEST_SHA256: &str =
    "sha256:1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c";
const CANDIDATE_WIRING: &str = "orders-create";
const CANDIDATE_COMPONENT_DIGEST: &str =
    "sha256:2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d";
const CANDIDATE_PROJECTION_HASH: &str =
    "sha256:4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f";
const CANDIDATE_IMPORTS_FINGERPRINT: &str =
    "sha256:3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e";
/// An admitted package that deliberately belongs to no effective release.
/// Its component carries a real HTTP connection requirement, so the same
/// document can prove both sides of the case-scoped effect judgment without a
/// fabricated release or binding world (`wamn-10yt.10.14`).
const EFFECTFUL_PACKAGE: &str = "effectful_package";
const EFFECTFUL_MANIFEST_SHA256: &str =
    "sha256:9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d";
const EFFECTFUL_WIRING: &str = "erp-call";
const EFFECTFUL_COMPONENT: &str = "http-client";
const EFFECTFUL_OPERATION: &str = "send";
const EFFECTFUL_COMPONENT_DIGEST: &str =
    "sha256:5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const EFFECTFUL_PROJECTION_HASH: &str =
    "sha256:7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c";
const EFFECTFUL_STORE_ALIAS: &str = "receipts";
const HTTP_IMPORT: &str = "wamn:connection/http@0.1.0";
/// The blobstore capability's first consumer, refused by the same clause.
///
/// The HTTP candidate proves the clause fires for one registered effect. This
/// proves it fires for `wasmcloud:blobstore` — the capability 2b added — so the
/// gate's effect law is shown to key on POSTURE rather than on one known
/// package. If the registry ever classified a new capability as ambient by
/// accident, this is where it would surface.
const BLOBSTORE_WIRING: &str = "labels-write";
const BLOBSTORE_COMPONENT: &str = "blob-put";
const BLOBSTORE_COMPONENT_DIGEST: &str =
    "sha256:3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d";
const BLOBSTORE_PROJECTION_HASH: &str =
    "sha256:4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e";
const BLOBSTORE_IMPORTS_FINGERPRINT: &str =
    "sha256:5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f";
/// Fixed loopback port for the gate. The gate is serial and env-gated, so a
/// fixed port is simpler than plumbing an ephemeral one out of the listener.
const BIND: &str = "127.0.0.1:18088";
/// Cluster-global roles, one project database, and one fixed listener make this
/// live gate deliberately serial even when the Rust test harness is parallel.
static LIVE_GATE_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const TTL: Duration = Duration::from_secs(3600);

/// The one refusal every authentication and authorization failure must return.
const AUTHORIZATION_DENIED: &str = r#"{"kind":"authorization-denied"}"#;

struct Response {
    status: u16,
    body: String,
}

/// Post one document and read the whole response. `Connection: close` lets the
/// reply be read to EOF without parsing a framing header.
async fn post(path: &str, bearer: Option<&str>, extra: &[(&str, &str)], body: &str) -> Response {
    send("POST", path, bearer, extra, body).await
}

/// Send one request with an explicit method.
async fn send(
    method: &str,
    path: &str,
    bearer: Option<&str>,
    extra: &[(&str, &str)],
    body: &str,
) -> Response {
    let mut stream = TcpStream::connect(BIND).await.expect("reach the surface");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {BIND}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(bearer) = bearer {
        request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    }
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let text = String::from_utf8(raw).expect("responses are UTF-8");
    let (head, body) = text.split_once("\r\n\r\n").expect("a complete response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    Response {
        status,
        body: body.to_owned(),
    }
}

fn get_report_document(query_id: &str, project: &str, report_id: &str) -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "query-id": query_id,
            "query": {
                "kind": "get-report",
                "input": {
                    "scope": {"project-id": project, "environment": ENVIRONMENT},
                    "report-id": report_id,
                },
            },
        },
    })
    .to_string()
}

fn legacy_get_run_query() -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "query-id": "retired-get-run",
            "query": {
                "kind": "get-run",
                "input": {
                    "scope": {"project-id": PROJECT, "environment": "dev"},
                    "run-id": "run-retired",
                },
            },
        },
    })
    .to_string()
}

fn outcome(body: &str) -> serde_json::Value {
    let document: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|_| panic!("a response document: {body}"));
    document["body"]["outcome"].clone()
}

fn as_json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| panic!("valid JSON: {text}"))
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

/// The database the admin URL names — half of the author generation's scope.
fn database_of(url: &str) -> String {
    let parsed = url::Url::parse(url).expect("the admin PG URL parses");
    let database = parsed.path().trim_start_matches('/').to_owned();
    assert!(!database.is_empty(), "the admin PG URL names no database");
    database
}

/// The exact scoped generation the credential contract mints for this gate.
///
/// Derived rather than hand-written: the role name binds `(org, project,
/// environment, database)`, so a constant would silently drift out of scope.
fn author_role(admin_url: &str) -> String {
    control_author_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database_of(admin_url),
        CredentialGeneration::A,
    )
}

fn author_url(admin_url: &str) -> String {
    // Same server and database, a different LOGIN: the adapter refuses a
    // credential it shares with the runtime, and the tenant mapping resolves on
    // `session_user`, so the gate must authenticate as the author.
    let mut parsed = url::Url::parse(admin_url).expect("the admin PG URL parses");
    parsed
        .set_username(&author_role(admin_url))
        .expect("a postgres URL carries a username");
    parsed
        .set_password(Some(AUTHOR_PASSWORD))
        .expect("a postgres URL carries a password");
    parsed.to_string()
}

/// The exact scoped identity-READ generation `serve` will accept
/// (`wamn-0h0g.12.67`).
///
/// Derived for the same reason `author_role` is, and from the same database:
/// the identity registry lives in the one control database this gate's admin URL
/// names, so that name is inside the scope digest and no other login satisfies
/// the pre-I/O gate.
fn identity_reader_role(admin_url: &str) -> String {
    system_reader_generation_role(
        SystemReader::Identity,
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database_of(admin_url),
        CredentialGeneration::A,
    )
}

/// The T1 identity read input `serve` refuses to start without.
///
/// NOT the admin URL. `serve` settles this input purely before any I/O and
/// refuses anything that is not this scope's identity-reader generation — the
/// schema owner and the superuser included — because this surface's whole
/// authorization model is rows in `identity.*` under no row-level security, so a
/// connection that could write them would let its own reader forge the answers
/// it then trusts.
fn identity_reader_url(admin_url: &str) -> String {
    let mut parsed = url::Url::parse(admin_url).expect("the admin PG URL parses");
    parsed
        .set_username(&identity_reader_role(admin_url))
        .expect("a postgres URL carries a username");
    parsed
        .set_password(Some(IDENTITY_READER_PASSWORD))
        .expect("a postgres URL carries a password");
    parsed.to_string()
}

/// The exact scoped generation the credential contract mints for the project
/// plane. Derived, not hand-written: the role name binds `(org, project,
/// environment, database)`.
fn admitter_role() -> String {
    management_admitter_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        PROJECT_DATABASE,
        CredentialGeneration::A,
    )
}

/// The gate admin's URL, repointed at the project-environment database.
///
/// Same server, so both planes share one clock — which is what lets this gate
/// compare a control-plane `finalized_at` against a project-plane `created_at`
/// and read the comparison as an ordering rather than as skew.
fn project_admin_url(admin_url: &str) -> String {
    let mut parsed = url::Url::parse(admin_url).expect("the admin PG URL parses");
    parsed.set_path(&format!("/{PROJECT_DATABASE}"));
    parsed.to_string()
}

/// The project-database admission input `serve` refuses to start without
/// (wamn-0h0g.8.5.3), and now OPENS (wamn-0h0g.8.5.4).
///
/// Until the composition landed this named an unresolvable host, because `serve`
/// only parsed the value. It is a real connection now: the surface admits runs
/// through it, so a URL nothing can reach would fail the process at startup.
fn admission_url(admin_url: &str) -> String {
    let mut parsed =
        url::Url::parse(&project_admin_url(admin_url)).expect("the project URL parses");
    parsed
        .set_username(&admitter_role())
        .expect("a postgres URL carries a username");
    parsed
        .set_password(Some(ADMITTER_PASSWORD))
        .expect("a postgres URL carries a password");
    parsed.to_string()
}

async fn start_management_surface(admin_url: &str) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (readiness_tx, readiness_rx) = tokio::sync::oneshot::channel();
    let mut surface = tokio::spawn(wamn_scenario_worker::management::serve_with_readiness(
        wamn_scenario_worker::management::ManagementServeArgs {
            bind: BIND.to_owned(),
            system_url: identity_reader_url(admin_url),
            control_authoring_database_url: author_url(admin_url),
            management_admission_database_url: admission_url(admin_url),
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            tenant: TENANT.to_owned(),
            source_schema: SOURCE_SCHEMA.to_owned(),
        },
        readiness_tx,
    ));
    let bound = tokio::select! {
        ready = readiness_rx => ready.expect("the management surface dropped readiness"),
        stopped = &mut surface => {
            let refused = stopped.expect("the surface task did not panic");
            panic!("the management surface never listened: {refused:?}");
        }
    };
    assert_eq!(bound.to_string(), BIND);
    surface
}

/// Bring up the project-environment plane this gate admits runs into.
///
/// A SEPARATE DATABASE, not a schema: residency is what distinguishes the two
/// stores, and a gate that put them in one database would prove nothing about a
/// composition whose whole difficulty is that it cannot be one transaction.
async fn provision_project(
    admin: &Client,
    admin_url: &str,
) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    admin
        .simple_query(&format!(
            "DROP DATABASE IF EXISTS \"{PROJECT_DATABASE}\" WITH (FORCE)"
        ))
        .await
        .context("drop any previous project database")?;
    admin
        .simple_query(&format!("CREATE DATABASE \"{PROJECT_DATABASE}\""))
        .await
        .context("create the project-environment database")?;
    let (project, task) = connect(&project_admin_url(admin_url)).await?;

    // The plane DDL grants to cluster-global roles, some of which the control
    // half of this gate already minted. Create only what is missing: dropping
    // and recreating one would revoke the other half's grants.
    project
        .batch_execute(
            "DO $roles$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author', \
                 'wamn_control_author', 'wamn_effect_writer', 'wamn_executor_platform'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB \
                     NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $roles$;",
        )
        .await
        .context("ensure the cluster-global plane roles")?;
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .context("read the catalog DDL")?;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .context("read the run-state DDL")?;
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .context("read the run-queue DDL")?;
    project
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             BEGIN; {catalog} {run_state} {run_queue} COMMIT; \
             {access_floor} {generation}",
            access_floor = sql::grant_connect_on_database_sql(PROJECT_DATABASE),
            // One call mints the stable admitter ACL role, applies the exact
            // column-level surface `ctl` applies, creates this generation, and
            // grants it CONNECT. The gate does not hand-write any of it.
            generation = sql::prepare_workload_generation_sql(
                WorkloadRoleFamily::ManagementAdmitter,
                PROJECT_DATABASE,
                &admitter_role(),
                ADMITTER_PASSWORD,
                "2099-01-01T00:00:00Z",
            ),
        ))
        .await
        .context("apply the project plane DDL and admitter credential")?;
    seed_candidate(&project).await?;
    Ok((project, task))
}

/// Seed one applied package and one unreleased, connection-bearing package.
///
/// The primary candidate's component declares no connection requirements. The
/// second package stores the production-normalized HTTP import/effect/
/// requirement shape, but receives no release membership or binding facts.
async fn seed_candidate(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(&format!(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('{TENANT}', '{CANDIDATE_PACKAGE}', '{CANDIDATE_PACKAGE_VERSION}', \
                     '{CANDIDATE_MANIFEST_SHA256}'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment) \
             VALUES ('{TENANT}', {CANDIDATE_EFFECTIVE_RELEASE_ID}, '{ENVIRONMENT}'); \
             INSERT INTO catalog.effective_release_packages \
               (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ('{TENANT}', {CANDIDATE_EFFECTIVE_RELEASE_ID}, '{CANDIDATE_PACKAGE}', \
                     '{CANDIDATE_PACKAGE_VERSION}'); \
             INSERT INTO catalog.effective_release_heads \
               (tenant_id, environment, effective_release_id) \
             VALUES ('{TENANT}', '{ENVIRONMENT}', {CANDIDATE_EFFECTIVE_RELEASE_ID}); \
             INSERT INTO catalog.component_library \
               (tenant_id, package_id, package_version, component, interface_version, operations, \
                component_digest, projection_hash, imports, imports_fingerprint, effects) \
             VALUES ('{TENANT}', '{CANDIDATE_PACKAGE}', '{CANDIDATE_PACKAGE_VERSION}', \
                     'entity', '0.1', \
                     '{{\"create\":{{\"input-ports\":[],\"output-ports\":[],\"parameters\":[]}}}}', \
                     '{CANDIDATE_COMPONENT_DIGEST}', '{CANDIDATE_PROJECTION_HASH}', '[]', \
                     '{CANDIDATE_IMPORTS_FINGERPRINT}', '[]'); \
             INSERT INTO catalog.component_library \
               (tenant_id, package_id, package_version, component, interface_version, operations, \
                component_digest, projection_hash, imports, imports_fingerprint, effects) \
             VALUES ('{TENANT}', '{CANDIDATE_PACKAGE}', '{CANDIDATE_PACKAGE_VERSION}', \
                     '{BLOBSTORE_COMPONENT}', '0.1', \
                     '{{\"put\":{{\"input-ports\":[],\"output-ports\":[],\"parameters\":[]}}}}', \
                     '{BLOBSTORE_COMPONENT_DIGEST}', '{BLOBSTORE_PROJECTION_HASH}', \
                     '[\"wasmcloud:blobstore/blobstore@0.1.0\"]', \
                     '{BLOBSTORE_IMPORTS_FINGERPRINT}', \
                     '[{{\"package\":\"wasmcloud:blobstore\",\"interfaces\":\
[\"wasmcloud:blobstore/blobstore@0.1.0\"]}}]'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             VALUES ('{TENANT}', '{ENVIRONMENT}', 'standard');"
        ))
        .await
        .context("seed the applied package, effective release, and environment policy")?;
    seed_effectful_unreleased_candidate(project).await?;
    // AND NOTHING INTO `catalog.wirings` (wamn-0h0g.8.28).
    //
    // Two rows used to be inserted here by direct admin SQL so the gate could
    // resolve a candidate out of them. THAT WAS THE FALSE GREEN. Authorship
    // refuses to write a wiring row without a green report for its own hash, and
    // the gate was the only producer of that report -- so on the real ordering
    // the row could never exist, and this fixture manufactured the steady state
    // the gate never reaches on its own. The gate now judges the DOCUMENT the
    // command carries, and this relation stays EMPTY for the whole run, which
    // `stored_wiring_count` asserts.
    //
    // What IS seeded above stays seeded: the applied package, effective release,
    // component library, and environment policy are facts their owning verbs
    // legitimately write. Gate judges only the component facts.
    Ok(())
}

/// Persist one production-valid connection/effect pair without release state.
///
/// Production admission refuses an HTTP requirement unless its audited
/// import is present in the nonempty effect posture. This fixture preserves that
/// exact pair; separating them would fabricate a state push-component cannot
/// persist.
async fn seed_effectful_unreleased_candidate(project: &Client) -> anyhow::Result<()> {
    let requirement = ComponentConnectionRequirement::new(
        EFFECTFUL_COMPONENT_DIGEST,
        EFFECTFUL_STORE_ALIAS,
        ConnectionTypeDescriptor::http_v1(),
    );
    let operations = serde_json::Value::Object(
        [(
            EFFECTFUL_OPERATION.to_owned(),
            serde_json::json!({"input-ports": [], "output-ports": [], "parameters": []}),
        )]
        .into_iter()
        .collect(),
    )
    .to_string();
    let imports = serde_json::json!([HTTP_IMPORT]);
    let imports_fingerprint = wamn_execution_contract::canonical_json_sha256(&imports);
    let imports = imports.to_string();
    let effects = serde_json::json!([{
        "package": "wamn:connection",
        "interfaces": [HTTP_IMPORT],
    }])
    .to_string();

    project
        .execute(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ($1, $2, $3, $4)",
            &[
                &TENANT,
                &EFFECTFUL_PACKAGE,
                &CANDIDATE_PACKAGE_VERSION,
                &EFFECTFUL_MANIFEST_SHA256,
            ],
        )
        .await
        .context("seed the unreleased package root")?;
    project
        .execute(
            "INSERT INTO catalog.component_library \
               (tenant_id, package_id, package_version, component, interface_version, operations, \
                component_digest, projection_hash, imports, imports_fingerprint, effects) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9::text::jsonb, $10, \
                     $11::text::jsonb)",
            &[
                &TENANT,
                &EFFECTFUL_PACKAGE,
                &CANDIDATE_PACKAGE_VERSION,
                &EFFECTFUL_COMPONENT,
                &"0.1",
                &operations,
                &EFFECTFUL_COMPONENT_DIGEST,
                &EFFECTFUL_PROJECTION_HASH,
                &imports,
                &imports_fingerprint,
                &effects,
            ],
        )
        .await
        .context("seed the normalized connection-bearing component")?;
    let requirement_json = String::from_utf8(requirement.canonical_bytes())
        .context("connection requirement is canonical UTF-8")?;
    let requirement_hash = requirement.requirement_hash();
    project
        .execute(
            wamn_schema_control::connections::insert_component_connection_requirement_sql(),
            &[
                &TENANT,
                &requirement.component_digest(),
                &requirement.store_alias(),
                &requirement_json,
                &requirement_hash,
            ],
        )
        .await
        .context("seed the generated HTTP connection requirement")?;
    Ok(())
}

/// How many wiring rows the project plane holds.
///
/// The first-transition proof reads this: an EMPTY `catalog.wirings` must still
/// reach a green report, which is precisely what the deadlock made impossible.
async fn stored_wiring_count(project: &Client) -> i64 {
    project
        .query_one(
            "SELECT count(*) FROM catalog.wirings WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await
        .expect("count the stored wiring rows")
        .get(0)
}

async fn stored_wiring(project: &Client) -> Option<(String, u32, String, serde_json::Value)> {
    project
        .query_opt(
            "SELECT wiring_id, version, wiring_hash, graph_json \
               FROM catalog.wirings \
              WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3",
            &[&TENANT, &CANDIDATE_PACKAGE, &CANDIDATE_PACKAGE_VERSION],
        )
        .await
        .expect("read the published wiring")
        .map(|row| {
            let version: i32 = row.get(1);
            (
                row.get(0),
                u32::try_from(version).expect("the stored wiring version is non-negative"),
                row.get(2),
                row.get(3),
            )
        })
}

async fn minted_release_snapshot_count(project: &Client) -> i64 {
    project
        .query_one(
            "SELECT count(*) FROM catalog.release_manifest_v3_snapshots WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await
        .expect("count minted release snapshots")
        .get(0)
}

/// One `gate` request carrying one document, in one project.
///
/// The command carries the DOCUMENT and its package placement (wamn-0h0g.8.28).
/// Nothing here names a stored row, which is why an empty `catalog.wirings` is
/// no longer an obstacle to being gated.
fn gate_document_for_package_in(
    command_id: &str,
    project: &str,
    package: &str,
    document: &serde_json::Value,
) -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "gate",
                "input": {
                    "scope": {"project-id": project, "environment": ENVIRONMENT},
                    "package-id": package,
                    "package-version": CANDIDATE_PACKAGE_VERSION,
                    "document": document,
                },
            },
        },
    })
    .to_string()
}

fn gate_document_in(command_id: &str, project: &str, document: &serde_json::Value) -> String {
    gate_document_for_package_in(command_id, project, CANDIDATE_PACKAGE, document)
}

/// One `gate` request document for one project, gating the pure candidate.
fn gate_document_for(command_id: &str, project: &str) -> String {
    gate_document_in(
        command_id,
        project,
        &candidate_graph(CANDIDATE_WIRING, "entity", "create"),
    )
}

/// One `gate` request document gating the pure candidate.
fn gate_document(command_id: &str) -> String {
    gate_document_for(command_id, PROJECT)
}

/// One `publish` request carrying the same document and placement as `gate`.
fn publish_document_in(command_id: &str, project: &str, document: &serde_json::Value) -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "publish",
                "input": {
                    "scope": {"project-id": project, "environment": ENVIRONMENT},
                    "package-id": CANDIDATE_PACKAGE,
                    "package-version": CANDIDATE_PACKAGE_VERSION,
                    "document": document,
                },
            },
        },
    })
    .to_string()
}

fn publish_document(command_id: &str) -> String {
    publish_document_in(
        command_id,
        PROJECT,
        &candidate_graph(CANDIDATE_WIRING, "entity", "create"),
    )
}

/// Every test-case run present in the project plane.
///
/// wamn-0h0g.8.5.5: the gate admits none, so this must stay EMPTY for the whole
/// gate. It is read rather than assumed, because "the composition came back" is
/// exactly the regression this file has to be able to see.
async fn project_case_runs(project: &Client) -> Vec<(String, String, String, SystemTime)> {
    project
        .query(
            "SELECT run_id, idempotency_key, status, created_at FROM wamn_run.runs \
              WHERE tenant_id = $1 AND trigger_source = 'test-case' ORDER BY created_at, run_id",
            &[&TENANT],
        )
        .await
        .expect("read the admitted test-case runs")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}

async fn project_queue_count(project: &Client) -> i64 {
    project
        .query_one(
            "SELECT count(*) FROM wamn_run.run_queue AS queue \
               JOIN wamn_run.runs AS run USING (tenant_id, run_id) \
              WHERE run.tenant_id = $1 AND run.trigger_source = 'test-case'",
            &[&TENANT],
        )
        .await
        .expect("count the admitted queue rows")
        .get(0)
}

async fn provision(admin: &mut Client, admin_url: &str) -> anyhow::Result<()> {
    let database = database_of(admin_url);
    let role = author_role(admin_url);
    // Roles are CLUSTER-wide, so dropping this database's schemas does not reach
    // them, and a leftover HEALTHY reader would satisfy the builder's
    // `IF NOT EXISTS` and mask a mutated one. `DROP OWNED BY` precedes
    // `DROP ROLE` so a privilege left behind cannot refuse the drop.
    let drop_identity_reader_roles: String = [
        identity_reader_role(admin_url),
        system_reader_generation_role(
            SystemReader::Identity,
            ORG,
            PROJECT,
            ENVIRONMENT,
            &database,
            CredentialGeneration::B,
        ),
        WorkloadRoleFamily::IdentityReader.acl_role().to_owned(),
    ]
    .iter()
    .map(|role| {
        format!(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
               EXECUTE 'DROP OWNED BY \"{role}\"'; \
               EXECUTE 'DROP ROLE \"{role}\"'; \
             END IF; END $$; "
        )
    })
    .collect();
    admin
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS {SOURCE_SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             {drop_identity_reader_roles} \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; END $$; \
             {prepare_generation} \
             DO $$ BEGIN EXECUTE format( \
               'GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;",
            // Mints the stable NOLOGIN ACL role AND this scope's generation, using
            // exactly the text ctl applies.
            prepare_generation = wamn_control_provision::sql::prepare_control_author_generation_sql(
                &database,
                &role,
                AUTHOR_PASSWORD,
                "2099-01-01T00:00:00Z",
            ),
        ))
        .await
        .context("reset management-live schemas and mint the control-author generation")?;
    // The fresh-control bootstrap record: identity registry AND portable store in
    // the one control database, applied as its documented owner.
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system;\n{SYSTEM_SCHEMA_SQL}\n{CONTROL_PORTABLE_STORE_SQL}\nRESET ROLE;"
        ))
        .await
        .context("apply the control system schema and portable store")?;
    // The identity read's OWN credential, minted through the real builders now
    // that `identity.*` exists (`wamn-0h0g.12.67`). The surface authenticates as
    // this generation, never as the superuser this gate provisions with: `serve`
    // settles that input purely and refuses the wide credential before it opens
    // a socket. Applying the stable surface on its own first proves the
    // convergent step is a no-op on replay rather than a one-shot —
    // `prepare_workload_generation_sql` runs the very same text again.
    admin
        .batch_execute(
            &sql::stable_surface_sql(WorkloadRoleFamily::IdentityReader)
                .context("the identity reader family carries a stable grant set")?,
        )
        .await
        .context("converge the identity reader's grant set")?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::IdentityReader,
            &database,
            &identity_reader_role(admin_url),
            IDENTITY_READER_PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .context("mint the identity-reader generation the surface authenticates as")?;
    admin
        .execute(
            "INSERT INTO wamn_authority.author_login_tenants \
               (login_identity, tenant_id, org_id, project_id, environment) \
             VALUES ($1, $2, $3, $4, $5)",
            &[&role, &TENANT, &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("map the control-author login to its one tenant")?;
    admin
        .batch_execute(&format!(
            "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
               VALUES ('{ORG}', 'pooled', 'pool-a'); \
             INSERT INTO registry.projects (org, id) VALUES ('{ORG}', '{PROJECT}'); \
             INSERT INTO registry.projects (org, id) VALUES ('{ORG}', '{OTHER_PROJECT}');"
        ))
        .await
        .context("seed the registry org and projects the role FK needs")?;
    Ok(())
}

/// Create one human with a project role and mint a token for it.
async fn admitted_human(
    admin: &Client,
    subject: &str,
    project: &str,
    role: &str,
) -> anyhow::Result<IssuedPat> {
    let principal = create_human(admin, subject, subject).await?;
    assign_project_role(admin, principal.id(), ORG, project, role).await?;
    issue_pat(admin, principal.id(), "gate", TTL)
        .await
        .map_err(Into::into)
}

async fn ledger_rows(admin: &Client) -> Vec<(String, String, String, String)> {
    admin
        .query(
            "SELECT principal_subject, principal_id, command_kind, effective_role \
               FROM catalog.authoring_command_audit ORDER BY recorded_at, principal_subject",
            &[],
        )
        .await
        .expect("read the command ledger")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}

/// Every durable authoring row the control store still holds.
///
/// TWO relations: the command ledger, and the gate report keyed by wiring hash
/// that wamn-0h0g.8.5.6 built. The three reservation-era relations this used to
/// count are deleted, so naming them here would query relations that do not
/// exist.
async fn authoring_durable_counts(admin: &Client) -> Vec<i64> {
    let row = admin
        .query_one(
            "SELECT (SELECT count(*) FROM catalog.authoring_command_audit), \
                    (SELECT count(*) FROM wamn_run.gate_reports)",
            &[],
        )
        .await
        .expect("count every authoring durable relation");
    (0..row.len()).map(|index| row.get(index)).collect()
}

/// The stored report for one wiring hash, read straight out of the control
/// store rather than through the query surface.
///
/// The two proofs it separates: that the gate WROTE a row, and that `get-report`
/// can RESOLVE it. A test that only read the query surface could not tell a
/// write that never happened from a read path that cannot find it.
async fn stored_gate_report(
    admin: &Client,
    wiring_hash: &str,
) -> Option<(bool, serde_json::Value)> {
    admin
        .query_opt(
            "SELECT passed, summary FROM wamn_run.gate_reports \
              WHERE tenant_id = $1 AND wiring_hash = $2",
            &[&TENANT, &wiring_hash],
        )
        .await
        .expect("read the stored gate report")
        .map(|row| (row.get(0), row.get(1)))
}

async fn effectful_candidate_runtime_state(project: &Client) -> (bool, bool, bool) {
    let row = project
        .query_one(
            "SELECT \
               EXISTS (SELECT 1 FROM catalog.connection_requirements \
                        WHERE tenant_id = $1 AND component_digest = $2) AS has_requirement, \
               EXISTS (SELECT 1 FROM catalog.effective_release_heads AS head \
                         JOIN catalog.effective_release_packages AS member \
                           ON member.tenant_id = head.tenant_id \
                          AND member.effective_release_id = head.effective_release_id \
                        WHERE head.tenant_id = $1 AND head.environment = $3 \
                          AND member.package_id = $4 AND member.package_version = $5) \
                 AS has_release_scope, \
               EXISTS (SELECT 1 FROM catalog.connection_bindings \
                        WHERE tenant_id = $1 AND component_digest = $2) AS has_binding",
            &[
                &TENANT,
                &EFFECTFUL_COMPONENT_DIGEST,
                &ENVIRONMENT,
                &EFFECTFUL_PACKAGE,
                &CANDIDATE_PACKAGE_VERSION,
            ],
        )
        .await
        .expect("read the effectful candidate's runtime state");
    (row.get(0), row.get(1), row.get(2))
}

/// Empty cases judge only compatibility, even for an unbound effectful component.
async fn empty_case_store_requirement_needs_no_runtime_binding_world(
    admin: &Client,
    project: &Client,
    token: &str,
) {
    assert_eq!(
        effectful_candidate_runtime_state(project).await,
        (true, false, false),
        "the proof fixture gained a fabricated release or binding world"
    );
    let mut document = candidate_graph(EFFECTFUL_WIRING, EFFECTFUL_COMPONENT, EFFECTFUL_OPERATION);
    document["cases"] = serde_json::json!([]);
    let wiring_hash = derived_hash(&document);
    let response = post(
        "/authoring",
        Some(token),
        &[],
        &gate_document_for_package_in(
            "gate-effectful-no-cases",
            PROJECT,
            EFFECTFUL_PACKAGE,
            &document,
        ),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        outcome(&response.body)["status"],
        serde_json::json!("completed"),
        "an empty-case wiring consulted a runtime binding world: {}",
        response.body
    );
    assert_eq!(
        stored_gate_report(admin, &wiring_hash).await,
        Some((true, serde_json::json!({"cases": 0}))),
        "the empty-case judgment did not persist its exact case count"
    );
}

/// Nonempty cases stop at the admitted effect posture before runtime bindings.
///
/// This is partial coverage only for wamn-61d0: it proves the common normalized
/// connection posture participates in Gate. Blobstore normalization and
/// blob-put behavior remain owned by that bead.
async fn nonempty_case_store_requirement_refuses_effect_posture(
    admin: &Client,
    project: &Client,
    token: &str,
) {
    assert_eq!(
        effectful_candidate_runtime_state(project).await,
        (true, false, false),
        "the proof fixture gained a fabricated release or binding world"
    );
    let document = candidate_graph(EFFECTFUL_WIRING, EFFECTFUL_COMPONENT, EFFECTFUL_OPERATION);
    let response = post(
        "/authoring",
        Some(token),
        &[],
        &gate_document_for_package_in("gate-effectful", PROJECT, EFFECTFUL_PACKAGE, &document),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        outcome(&response.body)["value"],
        serde_json::json!({
            "command": "gate",
            "reason": {
                "kind": "effectful-component-reached",
                "components": [EFFECTFUL_COMPONENT],
            },
        }),
        "a nonempty case bypassed the admitted effect posture: {}",
        response.body
    );
    assert!(
        stored_gate_report(admin, &derived_hash(&document))
            .await
            .is_none(),
        "an effectful nonempty candidate was given a gate report"
    );
}

#[derive(Clone, Copy)]
enum ConnectionGateProof {
    EmptyCases,
    NonemptyCases,
}

async fn run_connection_gate_proof(proof: ConnectionGateProof) {
    let Ok(url) = std::env::var("WAMN_PLATFORM_IDENTITY_PG_URL") else {
        eprintln!("skipping live connection Gate proof (set WAMN_PLATFORM_IDENTITY_PG_URL to run)");
        return;
    };
    let _serial = LIVE_GATE_SERIAL.lock().await;
    let (mut admin, admin_task) = connect(&url).await.expect("connect as the gate admin");
    provision(&mut admin, &url)
        .await
        .expect("provision the gate");
    let (project, project_task) = provision_project(&admin, &url)
        .await
        .expect("provision the project plane");
    let principal = admitted_human(
        &admin,
        "connection-gate@example.com",
        PROJECT,
        "project-author",
    )
    .await
    .expect("admit the connection Gate principal");
    let surface = start_management_surface(&url).await;

    match proof {
        ConnectionGateProof::EmptyCases => {
            empty_case_store_requirement_needs_no_runtime_binding_world(
                &admin,
                &project,
                principal.token(),
            )
            .await;
        }
        ConnectionGateProof::NonemptyCases => {
            nonempty_case_store_requirement_refuses_effect_posture(
                &admin,
                &project,
                principal.token(),
            )
            .await;
        }
    }

    surface.abort();
    project_task.abort();
    admin_task.abort();
}

#[tokio::test]
async fn empty_case_connection_component_without_release_or_binding_reports_zero_cases() {
    run_connection_gate_proof(ConnectionGateProof::EmptyCases).await;
}

#[tokio::test]
async fn nonempty_case_connection_component_without_release_or_binding_refuses_effect_posture() {
    run_connection_gate_proof(ConnectionGateProof::NonemptyCases).await;
}

#[tokio::test]
// LOUD, not silent (wamn-61d0). This returned early when the variable was
// unset, so a default `cargo test` reported `ok. 1 passed ... 0.00s` for a
// test that executed nothing — the self-skipping false green
// `docs/operations/build-and-test.md` names. `#[ignore]` makes its absence
// VISIBLE in the default run, and the `expect` below makes an explicit run
// without the database fail loudly instead of passing vacuously. A test that
// cannot tell "passed" from "never ran" is not evidence.
#[ignore = "requires disposable PostgreSQL via WAMN_PLATFORM_IDENTITY_PG_URL"]
async fn management_surface_authenticates_and_attributes_authoring_commands() {
    let url = std::env::var("WAMN_PLATFORM_IDENTITY_PG_URL")
        .expect("set WAMN_PLATFORM_IDENTITY_PG_URL to a disposable PostgreSQL superuser URL");
    let _serial = LIVE_GATE_SERIAL.lock().await;

    let (mut admin, admin_task) = connect(&url).await.expect("connect as the gate admin");
    provision(&mut admin, &url)
        .await
        .expect("provision the gate");
    // The SECOND plane. It is a different database on the same server: the
    // composition under test spans both, and could not be proved against one.
    let (project, project_task) = provision_project(&admin, &url)
        .await
        .expect("provision the project plane");

    // Two admitted principals for the same project, one service principal, one
    // principal admitted only for a different project, and one with no role.
    let alice = admitted_human(&admin, "alice@example.com", PROJECT, "project-author")
        .await
        .expect("admit alice");
    let bob = admitted_human(&admin, "bob@example.com", PROJECT, "project-admin")
        .await
        .expect("admit bob");
    let service = create_service(&admin, "ci-runner", "CI runner")
        .await
        .expect("create the service principal");
    assign_project_role(&admin, service.id(), ORG, PROJECT, "project-author")
        .await
        .expect("admit the service principal");
    let service_token = issue_pat(&admin, service.id(), "gate", TTL)
        .await
        .expect("mint a service token");
    let stranger = admitted_human(
        &admin,
        "stranger@example.com",
        OTHER_PROJECT,
        "project-author",
    )
    .await
    .expect("admit the stranger elsewhere");
    let roleless = create_human(&admin, "roleless@example.com", "Roleless")
        .await
        .expect("create a roleless principal");
    let roleless_token = issue_pat(&admin, roleless.id(), "gate", TTL)
        .await
        .expect("mint a roleless token");

    // A revoked token and an expired token, both otherwise well formed.
    let revoked = admitted_human(&admin, "revoked@example.com", PROJECT, "project-author")
        .await
        .expect("admit the revoked principal");
    revoke_pat(&admin, revoked.record().prefix())
        .await
        .expect("revoke a token");
    let expired = admitted_human(&admin, "expired@example.com", PROJECT, "project-author")
        .await
        .expect("admit the expiring principal");
    admin
        .execute(
            // `expires_at > created_at` is a stored invariant, so an aged token
            // moves both instants rather than only its expiry.
            "UPDATE identity.pats \
                SET created_at = now() - interval '2 hours', \
                    expires_at = now() - interval '1 hour' \
              WHERE token_prefix = $1",
            &[&expired.record().prefix()],
        )
        .await
        .expect("age one token past its expiry");

    let surface = start_management_surface(&url).await;

    // ---- every untrusted presenter refuses, identically, before any command --
    let forged = format!("{PAT_TOKEN_PREFIX}0123456789abcdef_{}", "0".repeat(64));
    let refusals: [(&str, Option<String>); 7] = [
        ("absent", None),
        ("malformed", Some("not-a-token".to_owned())),
        ("forged", Some(forged)),
        ("expired", Some(expired.token().to_owned())),
        ("revoked", Some(revoked.token().to_owned())),
        ("cross-project", Some(stranger.token().to_owned())),
        ("unroled", Some(roleless_token.token().to_owned())),
    ];
    for (name, token) in &refusals {
        for document in [
            gate_document_for("refused-gate", PROJECT),
            publish_document("refused-publish"),
        ] {
            let response = post("/authoring", token.as_deref(), &[], &document).await;
            assert_eq!(response.status, 403, "{name} was not refused");
            assert_eq!(
                response.body, AUTHORIZATION_DENIED,
                "{name} leaked a route distinction"
            );
        }
    }
    // A valid token that names another project refuses the same way.
    let elsewhere = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document_for("refused", OTHER_PROJECT),
    )
    .await;
    assert_eq!(elsewhere.status, 403);
    assert_eq!(elsewhere.body, AUTHORIZATION_DENIED);

    // `get-run` is no longer part of the Rust contract. Even an admitted author
    // receives the empty transport-level decode refusal, before route dispatch
    // can write any durable authoring state.
    let durable_before_retired = authoring_durable_counts(&admin).await;
    let retired = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &legacy_get_run_query(),
    )
    .await;
    assert_eq!(retired.status, 400, "retired get-run reached dispatch");
    assert!(retired.body.is_empty(), "decode refusal carried a document");
    assert_eq!(
        authoring_durable_counts(&admin).await,
        durable_before_retired,
        "retired get-run wrote durable authoring state"
    );
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a pre-dispatch refusal was audited"
    );

    // ---- get-report reads only the scoped control report store ---------------
    let get_missing = get_report_document("report-missing-query", PROJECT, "report-missing");
    for (name, token) in &refusals {
        let response = post("/authoring", token.as_deref(), &[], &get_missing).await;
        assert_eq!(response.status, 403, "{name} probed get-report");
        assert_eq!(response.body, AUTHORIZATION_DENIED);
    }
    let wrong_scope = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &get_report_document("wrong-scope-report", OTHER_PROJECT, "report-missing"),
    )
    .await;
    assert_eq!(wrong_scope.status, 403);
    assert_eq!(wrong_scope.body, AUTHORIZATION_DENIED);

    let missing = post("/authoring", Some(alice.token()), &[], &get_missing).await;
    assert_eq!(missing.status, 200, "{}", missing.body);
    let missing_document: serde_json::Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(
        missing_document["body"]["outcome"],
        serde_json::json!({
            "status": "refused",
            "value": {
                "query": "get-report",
                "reason": {"kind": "report-not-found", "report-id": "report-missing"},
            },
        })
    );

    // wamn-0h0g.8.5.5: the control store holds NO reports. The reservation
    // relation this section used to seed, the immutable report relation it used
    // to finalize, and the `pending` projection between them are all deleted --
    // a relation whose only writer and only reader die with the same change does
    // not survive it (owner ruling, 2026-08-25). So `report-not-found` is the
    // one truthful answer for every report id, and the query is still proved to
    // be non-mutating and non-ledgered, which is what it always owned here.
    let before_reads = authoring_durable_counts(&admin).await;
    for (query_id, principal, report_id) in [
        ("foreign-report", alice.token(), "report-foreign"),
        ("absent-report", alice.token(), "report-a"),
        ("absent-report-by-bob", bob.token(), "report-a"),
    ] {
        let response = post(
            "/authoring",
            Some(principal),
            &[],
            &get_report_document(query_id, PROJECT, report_id),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(document["body"]["query-id"], query_id);
        assert_eq!(
            outcome(&response.body)["value"]["reason"],
            serde_json::json!({"kind": "report-not-found", "report-id": report_id}),
            "get-report answered from a store that no longer exists: {}",
            response.body
        );
        // `pending` is off the wire entirely, not merely unreached.
        assert!(
            !response.body.contains("pending"),
            "get-report answered a pending projection: {}",
            response.body
        );
    }
    assert_eq!(
        authoring_durable_counts(&admin).await,
        before_reads,
        "get-report mutated control authoring state"
    );
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a non-ledgered query appended a command audit row"
    );

    // ---- a smuggled principal is refused by the contract before dispatch ---
    // wamn-0h0g.8.5.5: the sections that used to live here rode `save-draft` --
    // trusted-context attribution, the S1 checkout write path, stale-revision
    // refusal, handler parity, concurrent-retry serialization, exact stored
    // bytes and recorded provenance. Every one of them was a claim about
    // `catalog.flow_drafts`, or used the one command that wrote it as its
    // vehicle. The relation is deleted, so the claims about it are deleted too;
    // the claims about IDENTITY that used it as a vehicle move to the mounted
    // gate below, which reaches the ledger under two distinct principals.
    //
    // What survives here is the pair that never reaches a handler at all, so
    // neither needs a mounted command to be meaningful: a body that asserts a
    // principal, and a document with no usable contract version. Both are
    // refused at DECODE, which is why they can be asserted before the
    // composition runs without disturbing the "nothing has reached a command
    // yet" invariant above.
    let before = ledger_rows(&admin).await.len();
    let injected_body = post(
        "/authoring",
        Some(alice.token()),
        &[],
        // Spliced onto a token the gate input still carries. wamn-0h0g.8.28
        // retired `validated-draft` from this command, and a replace matching
        // NOTHING smuggles nothing — the document would stay valid, answer 200,
        // and this assertion would quietly stop testing anything.
        &{
            let document = gate_document_for("gate-injected-body", PROJECT);
            let smuggled = document.replace(
                r#""package-id""#,
                r#""principal":"bob@example.com","package-id""#,
            );
            assert_ne!(smuggled, document, "the injection matched nothing");
            smuggled
        },
    )
    .await;
    assert_eq!(injected_body.status, 400, "{}", injected_body.body);
    assert_eq!(
        ledger_rows(&admin).await.len(),
        before,
        "a smuggled principal reached a command"
    );

    // ---- an unversioned or unsupported document refuses before a command ---
    let versioned = |change: fn(&mut serde_json::Value)| {
        let mut document = as_json(&gate_document_for("gate-version", PROJECT));
        change(&mut document);
        document.to_string()
    };
    let unversioned = versioned(|document| {
        document["body"]
            .as_object_mut()
            .expect("a request body")
            .remove("schema-version");
    });
    let unsupported = versioned(|document| {
        document["body"]["schema-version"] = serde_json::json!("0.2");
    });
    for (name, document) in [("unversioned", &unversioned), ("unsupported", &unsupported)] {
        let ledger_before = ledger_rows(&admin).await.len();
        let refused = post("/authoring", Some(alice.token()), &[], document).await;
        assert_eq!(refused.status, 400, "{name}: {}", refused.body);
        assert_eq!(
            ledger_rows(&admin).await.len(),
            ledger_before,
            "{name} was audited"
        );
    }
    // The unsupported version is a typed refusal naming both versions; the
    // unversioned one cannot be, because a document with no version has no
    // contract to answer on.
    let refused = post("/authoring", Some(alice.token()), &[], &unsupported).await;
    assert_eq!(
        as_json(&refused.body),
        serde_json::json!({
            "kind": "unsupported-contract-version",
            "requested": "0.2",
            "supported": "0.1",
        })
    );

    let alice_principal = resolve_subject(
        &admin,
        wamn_platform_identity::PrincipalKind::Human,
        "alice@example.com",
    )
    .await
    .expect("resolve alice")
    .expect("alice exists");
    let bob_principal = resolve_subject(
        &admin,
        wamn_platform_identity::PrincipalKind::Human,
        "bob@example.com",
    )
    .await
    .expect("resolve bob")
    .expect("bob exists");
    // Nothing above reached a handler, so the ledger is still empty and the
    // composition below starts from a clean attribution slate.
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a pre-dispatch refusal was audited"
    );

    // ---- publish is mounted, but its green guard precedes the append -------
    // The document is valid and compatible with the effective release, so the
    // missing report is the only refusing predicate. The server derives the
    // report key from these bytes; the command carries no hash to forge.
    let candidate_document = candidate_graph(CANDIDATE_WIRING, "entity", "create");
    let candidate_hash = derived_hash(&candidate_document);
    let ungated_publish = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-before-gate", PROJECT, &candidate_document),
    )
    .await;
    assert_eq!(ungated_publish.status, 200, "{}", ungated_publish.body);
    assert_eq!(
        outcome(&ungated_publish.body)["value"],
        serde_json::json!({
            "command": "publish",
            "reason": {"kind": "report-not-found", "report-id": candidate_hash},
        })
    );
    assert_eq!(stored_wiring_count(&project).await, 0);
    assert_eq!(
        ledger_rows(&admin)
            .await
            .iter()
            .map(|row| row.2.as_str())
            .collect::<Vec<_>>(),
        ["publish"]
    );

    // ---- gate IS mounted, and judges a real candidate ----------------------
    // An untrusted presenter learns nothing from the mount: both command kinds
    // already returned the same frozen refusal above.
    let ledger_before_composition = ledger_rows(&admin).await.len();
    for (name, token) in &refusals {
        let response = post(
            "/authoring",
            token.as_deref(),
            &[],
            &gate_document("probe-gate"),
        )
        .await;
        assert_eq!(response.status, 403, "{name} probed gate for a route");
        assert_eq!(
            response.body, AUTHORIZATION_DENIED,
            "{name} learned that gate is mounted"
        );
    }
    assert_eq!(
        ledger_rows(&admin).await.len(),
        ledger_before_composition,
        "an untrusted gate probe was attributed"
    );

    // The headers assert another principal and a wider role. Neither is read:
    // identity is settled from the bearer token alone, before the body.
    let accepted = post(
        "/authoring",
        Some(alice.token()),
        &[
            ("X-Wamn-Principal", "bob@example.com"),
            ("X-Wamn-Role", "project-admin"),
        ],
        &gate_document("test-set-1"),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "gate did not reach its handler: {}",
        accepted.body
    );
    // ---- THE FIRST TRANSITION (wamn-0h0g.8.28) -----------------------------
    // This is the assertion the old fixture could not make. `catalog.wirings` is
    // EMPTY and has been for the whole run -- nothing seeded a row, and the gate
    // writes none -- yet a document just reached an accepted judgment. Under the
    // stored-row resolution this command could only ever have been refused,
    // because the row it would have resolved cannot be authored without the
    // green report this command produces.
    assert_eq!(
        stored_wiring_count(&project).await,
        0,
        "a wiring row existed, so this proves the steady state and not the first transition"
    );

    // The receipt names the report identity the judgment DERIVED from the
    // submitted bytes, not one the caller chose -- the command carries no hash
    // at all now. Report id and validated-draft id are the SAME string, which is
    // the whole of wamn-0h0g.8.5.6 visible on the wire.
    assert_eq!(
        outcome(&accepted.body),
        serde_json::json!({
            "status": "completed",
            "value": {
                "command": "gate",
                "result": {
                    "report-id": candidate_hash,
                    "validated-draft": {"validated-draft-id": candidate_hash},
                },
            },
        }),
        "the test-set receipt drifted: {}",
        accepted.body
    );

    // The header-asserted principal reached nothing: the one new ledger row is
    // attributed to the TOKEN principal, under the role that principal actually
    // holds rather than the one the header asked for.
    let attributed = ledger_rows(&admin).await;
    assert_eq!(
        attributed.len(),
        ledger_before_composition + 1,
        "the gate was not attributed exactly once: {attributed:?}"
    );
    let alice_row = attributed
        .iter()
        .find(|row| row.0 == "alice@example.com" && row.2 == "gate")
        .expect("the gate is attributed to the token principal");
    assert_eq!(alice_row.1, alice_principal.id().as_str());
    assert_eq!(alice_row.2, "gate");
    assert_eq!(alice_row.3, "project-author", "a header widened a role");
    assert!(
        !attributed
            .iter()
            .any(|row| row.1 == bob_principal.id().as_str()),
        "a header attributed a command to the wrong principal: {attributed:?}"
    );

    // NOTHING WAS EXECUTED. A gate is a judgment about a document (ratified
    // spec §5.1), so an accepted candidate admits no run and enqueues nothing.
    // These are the reads the deleted composition used to make non-empty;
    // keeping them is what makes its resurrection visible here.
    let runs = project_case_runs(&project).await;
    assert!(
        runs.is_empty(),
        "an accepted gate admitted a test-case run: {runs:?}"
    );
    assert_eq!(
        project_queue_count(&project).await,
        0,
        "an accepted gate enqueued a run"
    );

    // ---- THE GATE'S ONE DURABLE FACT, WRITTEN AND THEN READ ----------------
    // wamn-0h0g.8.5.6. Judging writes nothing to the PROJECT plane, but an
    // accepted judgment is not free of consequence: the control store gains one
    // immutable report keyed by the candidate's wiring hash.
    //
    // Asserted at the STORE first, so a receipt that merely echoes a hash back
    // cannot pass for a persisted report.
    let stored = stored_gate_report(&admin, &candidate_hash)
        .await
        .expect("the accepted gate wrote its report row");
    assert!(stored.0, "an accepted gate stored a failing report");
    assert_eq!(
        stored.1,
        serde_json::json!({"cases": 2}),
        "the stored summary does not count the judged document's cases"
    );
    // The REFUSED candidate below writes none, and the un-gated one never did:
    // one row exists, not one per candidate.
    assert_eq!(
        authoring_durable_counts(&admin).await[1],
        1,
        "the gate wrote a report for a document it did not accept"
    );

    // And the mounted query RESOLVES it. This is the half that closes the hole
    // wamn-0h0g.8.5.5 opened: before this bead `get-report` answered
    // `report-not-found` unconditionally, so the gate handed back a report id no
    // query could resolve. The id it hands back is the one asked for here.
    let projected = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &get_report_document("judged-report", PROJECT, &candidate_hash),
    )
    .await;
    assert_eq!(projected.status, 200, "{}", projected.body);
    assert_eq!(
        outcome(&projected.body),
        serde_json::json!({
            "status": "completed",
            "value": {
                "query": "get-report",
                "result": {
                    "state": "finalized",
                    "report-id": candidate_hash,
                    "validated-draft": {"validated-draft-id": candidate_hash},
                    "passed": true,
                    "summary": {"cases": 2},
                },
            },
        }),
        "the gate's report is not retrievable under the id the gate returned: {}",
        projected.body
    );
    assert_eq!(
        ledger_rows(&admin).await[ledger_before_composition..]
            .iter()
            .map(|(_, _, kind, _)| kind.clone())
            .collect::<Vec<_>>(),
        ["gate".to_owned()],
        "the judged command was not attributed exactly once"
    );

    // ---- an exact retry converges on the same judgment ---------------------
    // The same command id replays the stored receipt.
    let replay = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document("test-set-1"),
    )
    .await;
    assert_eq!(replay.status, 200, "{}", replay.body);
    assert_eq!(outcome(&replay.body), outcome(&accepted.body));

    // A DIFFERENT command id for the same candidate re-derives the judgment
    // from scratch. It converges trivially now that the judgment reads and
    // writes nothing: the candidate row is immutable and the postures are the
    // same, so the same document yields the same answer.
    let rerun = post(
        "/authoring",
        Some(bob.token()),
        &[],
        &gate_document("test-set-2"),
    )
    .await;
    assert_eq!(rerun.status, 200, "{}", rerun.body);
    assert_eq!(outcome(&rerun.body), outcome(&accepted.body));
    assert_eq!(
        project_case_runs(&project).await,
        runs,
        "a re-derived judgment admitted a run"
    );
    assert_eq!(project_queue_count(&project).await, 0);
    // Two principals ran the same command against the same candidate. Both are
    // attributed, and they stay distinguishable in the append-only ledger --
    // each under the role it actually holds.
    let rows = ledger_rows(&admin).await;
    assert_eq!(
        rows.len(),
        ledger_before_composition + 2,
        "the second command id was not attributed exactly once"
    );
    let bob_row = rows
        .iter()
        .find(|row| row.0 == "bob@example.com")
        .expect("bob is attributed");
    assert_ne!(
        alice_row.1, bob_row.1,
        "two principals ran the same command and are not distinguishable"
    );
    assert_eq!(bob_row.1, bob_principal.id().as_str());
    assert_eq!(bob_row.2, "gate");
    assert_eq!(bob_row.3, "project-admin");

    // A SERVICE token reaches the same command on the same terms as a human
    // one, and is attributed as itself.
    let by_service = post(
        "/authoring",
        Some(service_token.token()),
        &[],
        &gate_document("test-set-3"),
    )
    .await;
    assert_eq!(by_service.status, 200, "{}", by_service.body);
    assert_eq!(outcome(&by_service.body), outcome(&accepted.body));
    assert!(
        ledger_rows(&admin)
            .await
            .iter()
            .any(|row| row.0 == "ci-runner"),
        "the service principal is not attributed"
    );
    assert_eq!(
        project_case_runs(&project).await,
        runs,
        "a service-token re-drive admitted a run"
    );

    empty_case_store_requirement_needs_no_runtime_binding_world(&admin, &project, alice.token())
        .await;
    assert_eq!(
        project_case_runs(&project).await,
        runs,
        "a zero-case judgment admitted a run"
    );
    assert_eq!(project_queue_count(&project).await, 0);

    // Zero cases remove only execution proof. The Gate still validates the
    // document against the exact package's admitted component facts; a missing
    // operation cannot disappear through the posture queries' joins and mint a
    // green report.
    let mut incompatible_without_cases = candidate_graph(
        "orders-missing-operation-no-cases",
        "entity",
        "missing-operation",
    );
    incompatible_without_cases["cases"] = serde_json::json!([]);
    let incompatible_without_cases_hash = derived_hash(&incompatible_without_cases);
    let incompatible = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document_in(
            "gate-incompatible-no-cases",
            PROJECT,
            &incompatible_without_cases,
        ),
    )
    .await;
    assert_eq!(incompatible.status, 200, "{}", incompatible.body);
    let refusal = outcome(&incompatible.body)["value"].clone();
    assert_eq!(refusal["command"], serde_json::json!("gate"));
    assert_eq!(
        refusal["reason"]["kind"],
        serde_json::json!("invalid-document"),
        "an incompatible zero-case wiring was not refused: {}",
        incompatible.body
    );
    assert!(
        refusal["reason"]["detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "the compatibility refusal carried no actionable detail: {}",
        incompatible.body
    );
    assert!(
        stored_gate_report(&admin, &incompatible_without_cases_hash)
            .await
            .is_none(),
        "an incompatible zero-case wiring was given a green report"
    );
    assert_eq!(project_case_runs(&project).await, runs);
    assert_eq!(project_queue_count(&project).await, 0);

    // ---- THE CONSTITUTIONAL CLAUSE FIRES ------------------------------------
    // wamn-0h0g.8.5.5: gate cases are EFFECT-FREE BY CONTRACT. A gate is a
    // judgment about a document, not an execution of it, so a candidate that
    // reaches a component whose admitted effects projection is non-empty is
    // refused TYPED and executes nothing.
    //
    // This is the behavioural proof, not a source scan. The SAME unreleased
    // connection-bearing component accepted above now carries cases, so its
    // admitted effect posture becomes the exact refusing predicate.
    let runs_before_effectful = project_case_runs(&project).await;
    let ledger_before_effectful = ledger_rows(&admin).await.len();
    nonempty_case_store_requirement_refuses_effect_posture(&admin, &project, alice.token()).await;
    // Nothing executed. The refusal precedes everything the verb could do with
    // the candidate, so there is no run and no queue row.
    assert_eq!(
        project_case_runs(&project).await,
        runs_before_effectful,
        "an effectful candidate admitted a run before being refused"
    );
    assert_eq!(project_queue_count(&project).await, 0);
    // A refusal is still an authorized command, so it IS attributed.
    assert_eq!(
        ledger_rows(&admin).await.len(),
        ledger_before_effectful + 1,
        "a typed gate refusal was not attributed"
    );
    // THE SAME CLAUSE, AGAINST THE BLOBSTORE CAPABILITY (wamn-61d0). `ledger`
    // above proves it fires for another registered effect. This proves it fires
    // for `wasmcloud:blobstore`, the capability 2b added — so the effect law
    // keys on POSTURE, not on one known package, and the registry's first new
    // consumer is refused by it like any other effect.
    let blobstore = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document_in(
            "gate-blobstore",
            PROJECT,
            &candidate_graph(BLOBSTORE_WIRING, BLOBSTORE_COMPONENT, "put"),
        ),
    )
    .await;
    assert_eq!(blobstore.status, 200, "{}", blobstore.body);
    assert_eq!(
        outcome(&blobstore.body)["value"],
        serde_json::json!({
            "command": "gate",
            "reason": {
                "kind": "effectful-component-reached",
                "components": [BLOBSTORE_COMPONENT],
            },
        }),
        "a blobstore-effect candidate was not refused by its effect posture: {}",
        blobstore.body
    );

    // The PURE candidate was ACCEPTED a few lines above, against the very same
    // effective release and the very same posture read. That is what makes this a
    // predicate rather than a blanket refusal: one document passes the clause
    // and one does not, and the effects projection is the only difference.
    assert_eq!(
        outcome(&accepted.body)["status"],
        serde_json::json!("completed"),
        "the effect-free predicate refused the pure candidate too"
    );
    // Bytes that are not a wiring document are a typed product refusal, not a
    // 501 and not a fabricated report. This REPLACES the unknown-candidate arm
    // (wamn-0h0g.8.28): a document the command carries cannot be "not found", so
    // what the gate can still refuse on identity is bytes it cannot read. The
    // entry names no node, which `WiringDocument::parse` refuses -- a shape that
    // is well-formed JSON and a well-formed command, so only the document
    // validator can catch it.
    let mut malformed = candidate_graph(CANDIDATE_WIRING, "entity", "create");
    malformed["entry"] = serde_json::json!("no-such-node");
    let unknown = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document_in("test-set-unknown", PROJECT, &malformed),
    )
    .await;
    assert_eq!(unknown.status, 200, "{}", unknown.body);
    let refusal = outcome(&unknown.body)["value"].clone();
    assert_eq!(refusal["command"], serde_json::json!("gate"));
    assert_eq!(
        refusal["reason"]["kind"],
        serde_json::json!("invalid-document"),
        "an unreadable document was not refused as one: {}",
        unknown.body
    );
    // The refusal names WHY, and the detail is the validator's own, so a client
    // can act on it rather than guessing.
    assert!(
        refusal["reason"]["detail"]
            .as_str()
            .expect("the refusal carries a detail")
            .contains("no-such-node"),
        "the document refusal did not name the offending entry: {}",
        unknown.body
    );
    assert_eq!(
        project_case_runs(&project).await,
        runs,
        "a refused test-set command admitted a run"
    );
    assert_eq!(project_queue_count(&project).await, 0);

    // A green report for the candidate cannot authorize different bytes, and a
    // report under the exact second hash must itself be green. Seeding B red
    // kills both an any-green lookup and an implementation that ignores passed.
    let other_document = candidate_graph("orders-red-report", "entity", "create");
    let other_hash = derived_hash(&other_document);
    admin
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, false, '{}'::jsonb)",
            &[&TENANT, &other_hash],
        )
        .await
        .expect("seed the exact red report counterfactual");
    let wrong_hash_guard = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-other-hash", PROJECT, &other_document),
    )
    .await;
    assert_eq!(
        outcome(&wrong_hash_guard.body)["value"],
        serde_json::json!({
            "command": "publish",
            "reason": {"kind": "report-not-successful"},
        }),
        "{}",
        wrong_hash_guard.body
    );
    assert_eq!(stored_wiring_count(&project).await, 0);

    // A green report is necessary but not sufficient: Publish re-validates the
    // submitted document against the CURRENT component facts before it writes.
    // The document is structurally valid but names no admitted operation, so
    // removing that compatibility check would append it under this green hash.
    let incompatible_document =
        candidate_graph("orders-incompatible", "entity", "missing-operation");
    let incompatible_hash = derived_hash(&incompatible_document);
    admin
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, true, '{}'::jsonb)",
            &[&TENANT, &incompatible_hash],
        )
        .await
        .expect("seed the incompatible document's green report counterfactual");
    let incompatible = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-incompatible", PROJECT, &incompatible_document),
    )
    .await;
    assert_eq!(
        outcome(&incompatible.body)["value"],
        serde_json::json!({
            "command": "publish",
            "reason": {"kind": "publish-executable-drift"},
        }),
        "{}",
        incompatible.body
    );
    assert_eq!(stored_wiring_count(&project).await, 0);

    // ---- Publish is act 1: exact wiring append, never release mint ----------
    assert_eq!(minted_release_snapshot_count(&project).await, 0);
    let published = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-gated", PROJECT, &candidate_document),
    )
    .await;
    assert_eq!(published.status, 200, "{}", published.body);
    assert_eq!(
        outcome(&published.body),
        serde_json::json!({
            "status": "completed",
            "value": {
                "command": "publish",
                "result": {
                    "wiring-id": CANDIDATE_WIRING,
                    "version": 1,
                    "artifact-hash": candidate_hash,
                },
            },
        })
    );
    let normalized_document = serde_json::to_value(
        wamn_catalog::WiringDocument::parse(&candidate_document)
            .expect("the published fixture is a wiring document"),
    )
    .expect("the parsed wiring serializes");
    assert_eq!(
        stored_wiring(&project).await,
        Some((
            CANDIDATE_WIRING.to_owned(),
            1,
            candidate_hash.clone(),
            normalized_document,
        ))
    );
    assert_eq!(
        minted_release_snapshot_count(&project).await,
        0,
        "act-1 Publish minted an act-2 release snapshot"
    );

    // An exact retry replays its stored receipt and converges on the one row.
    let publish_ledger_count = ledger_rows(&admin).await.len();
    let replayed_publish = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-gated", PROJECT, &candidate_document),
    )
    .await;
    assert_eq!(replayed_publish.body, published.body);
    assert_eq!(stored_wiring_count(&project).await, 1);
    assert_eq!(ledger_rows(&admin).await.len(), publish_ledger_count);

    // A second compatible, green document reusing that command id is refused
    // before its project append. This is the side-effecting retry invariant:
    // the control lock chooses one payload, not merely one ledger answer after
    // two project writes have escaped.
    let divergent_document = candidate_graph("orders-create-copy", "entity", "create");
    let divergent_gate = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &gate_document_in("gate-divergent-publish", PROJECT, &divergent_document),
    )
    .await;
    assert_eq!(
        outcome(&divergent_gate.body)["status"],
        serde_json::json!("completed"),
        "{}",
        divergent_gate.body
    );
    let reused = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-gated", PROJECT, &divergent_document),
    )
    .await;
    assert_eq!(
        outcome(&reused.body)["value"],
        serde_json::json!({
            "command": "publish",
            "reason": {"kind": "command-id-reuse"},
        }),
        "{}",
        reused.body
    );
    assert_eq!(stored_wiring_count(&project).await, 1);
    assert_eq!(minted_release_snapshot_count(&project).await, 0);

    // Retry identity is a CONTROL-store fact and wins before project-side
    // validation. Reusing the completed id with the incompatible document
    // above must therefore remain command-id-reuse, not executable drift.
    let reused_incompatible = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &publish_document_in("publish-gated", PROJECT, &incompatible_document),
    )
    .await;
    assert_eq!(
        outcome(&reused_incompatible.body)["value"],
        serde_json::json!({
            "command": "publish",
            "reason": {"kind": "command-id-reuse"},
        }),
        "{}",
        reused_incompatible.body
    );
    assert_eq!(stored_wiring_count(&project).await, 1);

    // ---- the ledger is append-only -----------------------------------------
    let rewrite = admin
        .execute(
            "UPDATE catalog.authoring_command_audit SET principal_subject = 'rewritten'",
            &[],
        )
        .await;
    assert!(rewrite.is_err(), "the ledger accepted a rewrite");
    let erase = admin
        .execute("DELETE FROM catalog.authoring_command_audit", &[])
        .await;
    assert!(erase.is_err(), "the ledger accepted a delete");

    surface.abort();
    project_task.abort();
    admin_task.abort();
}

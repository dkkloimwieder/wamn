//! Real-PostgreSQL gate for the authenticated management authoring surface.
//!
//! Proves the whole bridge end to end over HTTP: a valid personal access token
//! reaches a landed authoring command with trusted principal and role context;
//! absent, forged, expired, revoked, cross-project, and client-injected identity
//! all refuse before the command runs and leave no side effect; and two
//! principals running the same command stay distinguishable in the append-only
//! ledger.
//!
//! It also owns `wamn-ftfc.22`'s posture for the contract kinds this transport
//! does not mount: an untrusted presenter naming one gets the same refusal
//! document as any other, so the surface is no route-existence oracle, and an
//! admitted author gets a bare `501` that audits nothing and mutates nothing.
//! `test-set-run` LEFT that set in wamn-0h0g.8.5.4 — it is mounted, so this gate
//! now proves the judgment it reaches instead of the 501 it used to answer.
//! wamn-0h0g.8.5.5 then collapsed `save-draft`, `validate`, `draft-run` and
//! `read-draft` OUT OF THE CONTRACT, so `publish` is the whole unmounted
//! inventory and there is no unmounted query at all.
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

use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, WorkloadRoleFamily,
    control_author_generation_role, management_admitter_generation_role, sql,
};
use wamn_platform_identity::{
    IssuedPat, PAT_TOKEN_PREFIX, assign_project_role, create_human, create_service, issue_pat,
    resolve_subject, revoke_pat,
};

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
/// The project-environment database the admission credential is scoped to. The
/// generation role name binds it, so the gate cannot rename one without the
/// other.
const PROJECT_DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";
const ADMITTER_PASSWORD: &str = "wamn-management-admission-live";
/// The one candidate wiring `test-set-run` is exercised against.
///
/// `CANDIDATE_WIRING_HASH` is the WHOLE identity (wamn-0h0g.8.5.6). It is what
/// the command carries as its `validated-draft-id`, it is the key the accepted
/// gate's report is stored under, and it is therefore the report id the receipt
/// hands back and `get-report` resolves. There is no second identifier to
/// derive, chose, or mismatch -- the column that used to carry one is gone.
const CANDIDATE_CATALOG: &str = "catalog-candidate";
const CANDIDATE_WIRING: &str = "orders-create";
const CANDIDATE_WIRING_HASH: &str =
    "sha256:1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c";
const CANDIDATE_COMPONENT_DIGEST: &str =
    "sha256:2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d";
const CANDIDATE_IMPORTS_FINGERPRINT: &str =
    "sha256:3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e";
/// A SECOND candidate, identical in every way that matters to admission except
/// that the component its one node reaches carries a NON-EMPTY effects
/// projection (wamn-0h0g.21.9). It exists to prove the constitutional clause
/// FIRES rather than merely being written: gate cases are effect-free by
/// contract, so this candidate must be refused, typed, with nothing admitted.
const EFFECTFUL_WIRING: &str = "orders-charge";
const EFFECTFUL_WIRING_HASH: &str =
    "sha256:4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f";
const EFFECTFUL_COMPONENT: &str = "ledger";
const EFFECTFUL_COMPONENT_DIGEST: &str =
    "sha256:5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const EFFECTFUL_IMPORTS_FINGERPRINT: &str =
    "sha256:6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b";
/// Fixed loopback port for the gate. The gate is serial and env-gated, so a
/// fixed port is simpler than plumbing an ephemeral one out of the listener.
const BIND: &str = "127.0.0.1:18088";
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

/// `test-set-run` was here until wamn-0h0g.8.5.4 mounted it. Its assertions moved
/// to the composition block; what stays here is the property that outlives it —
/// a kind with no route answers a bare `501`, and the answer carries no
/// document.
///
/// wamn-0h0g.8.5.5 removed `validate` and `draft-run` from the CONTRACT, so they
/// are no longer unmounted kinds — they are not kinds at all, and a document
/// naming one is refused at decode rather than answered with a 501. `publish` is
/// the whole remaining inventory.
fn unmounted_commands() -> Vec<(&'static str, String)> {
    let scope = serde_json::json!({"project-id": PROJECT, "environment": "dev"});
    let validated = serde_json::json!({"validated-draft-id": "validated-draft-1"});
    [(
        "publish",
        serde_json::json!({
            "scope": scope,
            "validated-draft": validated,
            "successful-report-id": "report-1",
        }),
    )]
    .into_iter()
    .map(|(kind, input)| {
        let document = serde_json::json!({
            "document": "request",
            "body": {
                "schema-version": "0.1",
                "command-id": format!("unmounted-{kind}"),
                "command": {"kind": kind, "input": input},
            }
        });
        (kind, document.to_string())
    })
    .collect()
}

/// There is NO unmounted query (wamn-0h0g.8.5.5): `get-report` is the whole
/// query inventory and it is mounted, and `read-draft` left the contract with
/// the draft concept. Returned as an empty inventory rather than deleted so the
/// bare-501 loop below still states, by running over nothing, that no query
/// kind answers a 501.
fn unmounted_queries() -> Vec<(&'static str, String)> {
    Vec::new()
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

/// Seed the one applied catalog and the one gated candidate wiring.
///
/// The candidate's component declares NO connection requirements, so its binding
/// world is the empty array. That is deliberate: a stable empty world still
/// proves the world is FROZEN at ordinal zero and re-presented by every later
/// ordinal, without making the assertion depend on connection lifecycle the
/// composition does not own.
async fn seed_candidate(project: &Client) -> anyhow::Result<()> {
    let graph = serde_json::json!({
        "format-version": "0.1",
        "wiring-id": CANDIDATE_WIRING,
        "version": 1,
        "entry": "node",
        "nodes": {
            "node": {
                "component": "entity",
                "interface-version": "0.1",
                "operation": "create",
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
    });
    project
        .batch_execute(&format!(
            "INSERT INTO catalog.catalogs \
               (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ('{TENANT}', '{CANDIDATE_CATALOG}', 1, '{ENVIRONMENT}', '0.1', 'applied'); \
             INSERT INTO catalog.releases (tenant_id, catalog_id, catalog_version) \
             VALUES ('{TENANT}', '{CANDIDATE_CATALOG}', 1); \
             INSERT INTO catalog.component_library \
               (tenant_id, catalog_id, catalog_version, component, interface_version, operation, \
                component_digest, imports, imports_fingerprint, effects, input_ports, \
                output_ports, parameters) \
             VALUES ('{TENANT}', '{CANDIDATE_CATALOG}', 1, 'entity', '0.1', 'create', \
                     '{CANDIDATE_COMPONENT_DIGEST}', '[]', '{CANDIDATE_IMPORTS_FINGERPRINT}', \
                     '[]', '[]', '[]', '[]'); \
             INSERT INTO catalog.component_library \
               (tenant_id, catalog_id, catalog_version, component, interface_version, operation, \
                component_digest, imports, imports_fingerprint, effects, input_ports, \
                output_ports, parameters) \
             VALUES ('{TENANT}', '{CANDIDATE_CATALOG}', 1, '{EFFECTFUL_COMPONENT}', '0.1', \
                     'charge', '{EFFECTFUL_COMPONENT_DIGEST}', \
                     '[\"wamn:postgres/client@0.1.0\"]', '{EFFECTFUL_IMPORTS_FINGERPRINT}', \
                     '[{{\"package\":\"wamn:postgres\",\"interfaces\":\
[\"wamn:postgres/client@0.1.0\"]}}]', '[]', '[]', '[]'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             VALUES ('{TENANT}', '{ENVIRONMENT}', 'standard');"
        ))
        .await
        .context("seed the applied catalog and environment policy")?;
    project
        .execute(
            "INSERT INTO catalog.wirings \
               (tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
                graph_json, wiring_hash) \
             VALUES ($1, $2, $3, 1, 1, $4::text::jsonb, $5)",
            &[
                &TENANT,
                &CANDIDATE_CATALOG,
                &CANDIDATE_WIRING,
                &graph.to_string(),
                &CANDIDATE_WIRING_HASH,
            ],
        )
        .await
        .context("seed the gated candidate wiring")?;
    let effectful_graph = serde_json::json!({
        "format-version": "0.1",
        "wiring-id": EFFECTFUL_WIRING,
        "version": 1,
        "entry": "node",
        "nodes": {
            "node": {
                "component": EFFECTFUL_COMPONENT,
                "interface-version": "0.1",
                "operation": "charge",
            },
        },
        // A well-formed, otherwise gateable case set. The refusal must come from
        // the effect posture alone, not from an invalid test set.
        "cases": [
            {
                "case-id": "charges",
                "input": {"amount": 1},
                "expect": {"outcome": "responded", "status": 201},
            },
        ],
    });
    project
        .execute(
            "INSERT INTO catalog.wirings \
               (tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
                graph_json, wiring_hash) \
             VALUES ($1, $2, $3, 1, 1, $4::text::jsonb, $5)",
            &[
                &TENANT,
                &CANDIDATE_CATALOG,
                &EFFECTFUL_WIRING,
                &effectful_graph.to_string(),
                &EFFECTFUL_WIRING_HASH,
            ],
        )
        .await
        .context("seed the effectful candidate wiring")?;
    Ok(())
}

/// One `test-set-run` request document for the seeded candidate, in one project.
fn gate_document_for(command_id: &str, project: &str) -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "test-set-run",
                "input": {
                    "scope": {"project-id": project, "environment": ENVIRONMENT},
                    "validated-draft": {"validated-draft-id": CANDIDATE_WIRING_HASH},
                },
            },
        },
    })
    .to_string()
}

/// One `test-set-run` request document for the seeded candidate.
///
/// The `validated-draft-id` is the WIRING HASH. There is no draft to resolve it
/// through: the wiring document is the validated artifact and its hash is the
/// identity, so the command carries the whole coordinate.
fn test_set_run_document(command_id: &str) -> String {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "test-set-run",
                "input": {
                    "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                    "validated-draft": {"validated-draft-id": CANDIDATE_WIRING_HASH},
                },
            },
        },
    })
    .to_string()
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
    admin
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS {SOURCE_SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
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

#[tokio::test]
async fn management_surface_authenticates_and_attributes_authoring_commands() {
    let Ok(url) = std::env::var("WAMN_PLATFORM_IDENTITY_PG_URL") else {
        eprintln!(
            "skipping management_surface_authenticates_and_attributes_authoring_commands \
             (set WAMN_PLATFORM_IDENTITY_PG_URL to run)"
        );
        return;
    };

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

    let surface = tokio::spawn(wamn_scenario_worker::management::serve(
        wamn_scenario_worker::management::ManagementServeArgs {
            bind: BIND.to_owned(),
            system_url: url.clone(),
            control_authoring_database_url: author_url(&url),
            management_admission_database_url: admission_url(&url),
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            tenant: TENANT.to_owned(),
            source_schema: SOURCE_SCHEMA.to_owned(),
        },
    ));
    // The listener binds inside the spawned task; give it the connection.
    for _ in 0..50 {
        if TcpStream::connect(BIND).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

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
        let response = post(
            "/authoring",
            token.as_deref(),
            &[],
            &gate_document_for("refused", PROJECT),
        )
        .await;
        assert_eq!(response.status, 403, "{name} was not refused");
        assert_eq!(
            response.body, AUTHORIZATION_DENIED,
            "{name} leaked a reason"
        );
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

    // ---- the unmounted kinds are an absent route, not a product refusal -----
    // The operation-integration owner mounts the remaining four commands and
    // `read-draft`. Each either has no backend or has one whose trusted inputs
    // no in-process producer supplies. Two properties have to hold while that
    // is true.
    //
    // First, route selection happens after authorization, so naming an
    // unmounted kind is not a way to ask whether a route exists. Every
    // untrusted presenter gets the one refusal document for every kind.
    let unmounted = unmounted_commands();
    let unmounted_queries = unmounted_queries();
    let durable_before_unmounted = authoring_durable_counts(&admin).await;

    // `get-run` is no longer part of the Rust contract. Even an admitted author
    // receives the empty transport-level decode refusal, before route dispatch
    // can answer `501` or write any durable authoring state.
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
        durable_before_unmounted,
        "retired get-run wrote durable authoring state"
    );

    for (name, token) in &refusals {
        for (kind, document) in unmounted.iter().chain(&unmounted_queries) {
            let response = post("/authoring", token.as_deref(), &[], document).await;
            assert_eq!(response.status, 403, "{name} probed {kind} for a route");
            assert_eq!(
                response.body, AUTHORIZATION_DENIED,
                "{name} learned something about {kind}"
            );
        }
    }
    // Second, an admitted author gets a bare `501`: the absence of a route
    // carries no document, so no client can read it as a typed product refusal
    // and no client can read a fabricated result out of it.
    for (kind, document) in &unmounted {
        let response = post("/authoring", Some(alice.token()), &[], document).await;
        assert_eq!(
            response.status, 501,
            "{kind} answered as though it were mounted: {}",
            response.body
        );
        assert!(
            response.body.is_empty(),
            "{kind} answered 501 carrying a document: {}",
            response.body
        );
    }
    for (kind, document) in &unmounted_queries {
        let response = post("/authoring", Some(alice.token()), &[], document).await;
        assert_eq!(
            response.status, 501,
            "query {kind} answered as though it were mounted: {}",
            response.body
        );
        assert!(
            response.body.is_empty(),
            "query {kind} answered 501 carrying a document: {}",
            response.body
        );
    }
    // The ledger retains authorized command attempts. An absent route is not
    // one, so an unmounted kind leaves it untouched — and mutates nothing.
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "an unmounted kind was attributed on the command ledger"
    );
    assert_eq!(
        authoring_durable_counts(&admin).await,
        durable_before_unmounted,
        "an unmounted command or query wrote durable authoring state"
    );

    // Nothing above reached a command: no ledger row. `test-set-run` is no
    // longer among the kinds above — wamn-0h0g.8.5.4 mounted it, and the
    // assertions that replaced its bare 501 run at the end of this gate, where
    // they cannot disturb the "nothing has reached a command yet" invariants
    // this section and the query section below both depend on.
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a refusal was audited"
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
        &gate_document_for("gate-injected-body", PROJECT).replace(
            r#""validated-draft""#,
            r#""principal":"bob@example.com","validated-draft""#,
        ),
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

    // ---- test-set-run IS mounted, and judges a real candidate --------------
    // wamn-0h0g.8.5.4. This is the assertion that replaced the bare 501: the
    // very document that used to be in `unmounted` above now reaches the
    // composition. `validate`, `draft-run`, `publish`, and `read-draft` kept
    // the bare-501 property, asserted immediately above and unchanged.
    //
    // An untrusted presenter learns nothing from the mount either. A MOUNTED
    // kind has to answer the same refusal document as an unmounted one, or the
    // surface would become the route-existence oracle the 501 was shaped to
    // avoid.
    let ledger_before_composition = ledger_rows(&admin).await.len();
    for (name, token) in &refusals {
        let response = post(
            "/authoring",
            token.as_deref(),
            &[],
            &test_set_run_document("probe-test-set-run"),
        )
        .await;
        assert_eq!(
            response.status, 403,
            "{name} probed test-set-run for a route"
        );
        assert_eq!(
            response.body, AUTHORIZATION_DENIED,
            "{name} learned that test-set-run is mounted"
        );
    }
    assert_eq!(
        ledger_rows(&admin).await.len(),
        ledger_before_composition,
        "an untrusted test-set-run probe was attributed"
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
        &test_set_run_document("test-set-1"),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "test-set-run did not reach its handler: {}",
        accepted.body
    );
    // The receipt names the report identity the judgment DERIVED — the
    // candidate's own content hash — not one the caller chose. Report id and
    // validated-draft id are now the SAME string, which is the whole of
    // wamn-0h0g.8.5.6 visible on the wire.
    assert_eq!(
        outcome(&accepted.body),
        serde_json::json!({
            "status": "completed",
            "value": {
                "command": "test-set-run",
                "result": {
                    "report-id": CANDIDATE_WIRING_HASH,
                    "validated-draft": {"validated-draft-id": CANDIDATE_WIRING_HASH},
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
        .find(|row| row.0 == "alice@example.com")
        .expect("the gate is attributed to the token principal");
    assert_eq!(alice_row.1, alice_principal.id().as_str());
    assert_eq!(alice_row.2, "test-set-run");
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
    let stored = stored_gate_report(&admin, CANDIDATE_WIRING_HASH)
        .await
        .expect("the accepted gate wrote its report row");
    assert!(stored.0, "an accepted gate stored a failing report");
    assert_eq!(
        stored.1,
        serde_json::json!({"cases": ["creates", "rejects"]}),
        "the stored summary is not the judged document's own case set"
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
        &get_report_document("judged-report", PROJECT, CANDIDATE_WIRING_HASH),
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
                    "report-id": CANDIDATE_WIRING_HASH,
                    "validated-draft": {"validated-draft-id": CANDIDATE_WIRING_HASH},
                    "passed": true,
                    "summary": {"cases": ["creates", "rejects"]},
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
        ["test-set-run".to_owned()],
        "the judged command was not attributed exactly once"
    );

    // ---- an exact retry converges on the same judgment ---------------------
    // The same command id replays the stored receipt.
    let replay = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &test_set_run_document("test-set-1"),
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
        &test_set_run_document("test-set-2"),
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
    assert_eq!(bob_row.2, "test-set-run");
    assert_eq!(bob_row.3, "project-admin");

    // A SERVICE token reaches the same command on the same terms as a human
    // one, and is attributed as itself.
    let by_service = post(
        "/authoring",
        Some(service_token.token()),
        &[],
        &test_set_run_document("test-set-3"),
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

    // ---- THE CONSTITUTIONAL CLAUSE FIRES ------------------------------------
    // wamn-0h0g.8.5.5: gate cases are EFFECT-FREE BY CONTRACT. A gate is a
    // judgment about a document, not an execution of it, so a candidate that
    // reaches a component whose admitted effects projection is non-empty is
    // refused TYPED and executes nothing.
    //
    // This is the behavioural proof, not a source scan. The effectful candidate
    // is identical to the gated one in every way admission cares about -- same
    // tenant, same applied catalog version, same well-formed single case, its
    // own wiring hash and its own gate report -- and differs ONLY in the
    // `effects` value of the component its node resolves to. It therefore
    // cannot be refused for any other reason, and if the posture read were
    // deleted this candidate would be ACCEPTED instead -- which is exactly what
    // the mandatory mutation of this bead neuters the check to prove.
    let runs_before_effectful = project_case_runs(&project).await;
    let ledger_before_effectful = ledger_rows(&admin).await.len();
    let effectful = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &serde_json::json!({
            "document": "request",
            "body": {
                "schema-version": "0.1",
                "command-id": "gate-effectful",
                "command": {
                    "kind": "test-set-run",
                    "input": {
                        "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                        "validated-draft": {"validated-draft-id": EFFECTFUL_WIRING_HASH},
                    },
                },
            },
        })
        .to_string(),
    )
    .await;
    assert_eq!(effectful.status, 200, "{}", effectful.body);
    assert_eq!(
        outcome(&effectful.body)["value"],
        serde_json::json!({
            "command": "test-set-run",
            "reason": {
                "kind": "effectful-component-reached",
                "components": [EFFECTFUL_COMPONENT],
            },
        }),
        "an effectful candidate was not refused by its effect posture: {}",
        effectful.body
    );
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
    // But a refusal is NOT a report (wamn-0h0g.8.5.6). The effectful candidate
    // has a wiring hash of its own, so a writer that persisted before consulting
    // the judgment would leave a row under it -- and `get-report` would then
    // certify a document the gate refused.
    assert!(
        stored_gate_report(&admin, EFFECTFUL_WIRING_HASH)
            .await
            .is_none(),
        "a refused candidate was given a gate report"
    );

    // The PURE candidate was ACCEPTED a few lines above, against the very same
    // catalog version and the very same posture read. That is what makes this a
    // predicate rather than a blanket refusal: one document passes the clause
    // and one does not, and the effects projection is the only difference.
    assert_eq!(
        outcome(&accepted.body)["status"],
        serde_json::json!("completed"),
        "the effect-free predicate refused the pure candidate too"
    );

    // A candidate this plane does not hold is a typed product refusal, not a
    // 501 and not a fabricated report.
    let unknown = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &serde_json::json!({
            "document": "request",
            "body": {
                "schema-version": "0.1",
                "command-id": "test-set-unknown",
                "command": {
                    "kind": "test-set-run",
                    "input": {
                        "scope": {"project-id": PROJECT, "environment": ENVIRONMENT},
                        "validated-draft": {
                            "validated-draft-id": "sha256:".to_owned() + &"9".repeat(64),
                        },
                    },
                },
            },
        })
        .to_string(),
    )
    .await;
    assert_eq!(unknown.status, 200, "{}", unknown.body);
    assert_eq!(
        outcome(&unknown.body)["value"],
        serde_json::json!({
            "command": "test-set-run",
            "reason": {
                "kind": "validated-draft-not-found",
                "validated-draft-id": "sha256:".to_owned() + &"9".repeat(64),
            },
        }),
        "an unknown candidate was not refused by identity"
    );
    assert_eq!(
        project_case_runs(&project).await,
        runs,
        "a refused test-set command admitted a run"
    );
    assert_eq!(project_queue_count(&project).await, 0);

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

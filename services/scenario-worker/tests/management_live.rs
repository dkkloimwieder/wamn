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
//! now proves the composition it reaches instead of the 501 it used to answer,
//! while `validate`, `draft-run`, `publish`, and the query-side `read-draft`
//! keep the bare-501 property unchanged.
//!
//! Proving that composition takes a SECOND database. The project-environment
//! database this gate creates alongside the control one is where runs, the run
//! queue, and the catalog live; the surface reaches it through the scoped
//! `wamn_management_admitter` generation, and a transaction never spans the two.
//! There is no executor in this gate, so a stand-in completes each admitted run
//! in place — it cannot complete one before the composition admits it, which is
//! what makes the sequencing assertions meaningful.
//!
//! It also owns `wamn-ftfc.2`'s S1 write path: a checkout client reads
//! working-tree definition files and submits their content with the revision it
//! last saw. That half proves the submitted document reaches the canonical
//! store, that the exact stored revision is the one the canonical store read
//! returns, that a stale working copy refuses before mutating anything, that
//! the public HTTP path and the canonical audited handler path agree, and that
//! attribution a client attaches is inert.
//!
//! The recipe in `docs/archive/build-and-test.md` supplies one disposable database.

use std::path::{Path, PathBuf};
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
use wamn_scenario_worker::authoring::{ControlAuthoringScope, InternalAuthoringBackend, SaveDraft};
use wamn_scenario_worker::management::{CommandScope, authorize, save_draft};

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
/// `CANDIDATE_WIRING_HASH` is what the command carries as its
/// `validated-draft-id` (the owner ruled the two are the same identity), and
/// `CANDIDATE_GATE_REPORT` is what the composition MUST use as the report id:
/// management admission classifies a test case whose `report_id` differs from
/// the candidate row's `gate_report_id` as `gate-report-mismatch`, so the report
/// identity is derived from the candidate rather than chosen by the caller.
const CANDIDATE_CATALOG: &str = "catalog-candidate";
const CANDIDATE_WIRING: &str = "orders-create";
const CANDIDATE_GATE_REPORT: &str = "gate-report-candidate";
const CANDIDATE_WIRING_HASH: &str =
    "sha256:1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c";
const CANDIDATE_COMPONENT_DIGEST: &str =
    "sha256:2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d";
const CANDIDATE_IMPORTS_FINGERPRINT: &str =
    "sha256:3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e";
/// Fixed loopback port for the gate. The gate is serial and env-gated, so a
/// fixed port is simpler than plumbing an ephemeral one out of the listener.
const BIND: &str = "127.0.0.1:18088";
const TTL: Duration = Duration::from_secs(3600);

const DRAFT_GRAPH: &str = r#"{"schema-version":"0.1","flow-id":"receive-material","version":1,
  "nodes":[{"id":"request","type":"request","config":{"input-schema":true}},
           {"id":"respond","type":"respond","config":{"status":200}}],
  "edges":[{"from":"request","to":"respond"}]}"#;

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

fn save_document(command_id: &str, project: &str, revision: u64, draft: &str) -> String {
    save_definition(command_id, project, revision, draft, DRAFT_GRAPH)
}

/// The same closed save document with every object member deliberately emitted
/// in a different order. Canonical retry identity must ignore this transport
/// formatting while retaining the exact typed content.
fn reordered_save_document(
    command_id: &str,
    project: &str,
    revision: u64,
    draft: &str,
    definition: &str,
) -> String {
    format!(
        r#"{{"body":{{"command":{{"input":{{"provenance":null,"definition":{},"expected-revision":{},"wiring-id":"receive-material","draft-id":{},"scope":{{"environment":"dev","project-id":{}}}}},"kind":"save-draft"}},"command-id":{},"schema-version":"0.1"}},"document":"request"}}"#,
        serde_json::to_string(definition).unwrap(),
        revision,
        serde_json::to_string(draft).unwrap(),
        serde_json::to_string(project).unwrap(),
        serde_json::to_string(command_id).unwrap(),
    )
}

/// The same command with a caller-supplied definition body.
fn save_definition(
    command_id: &str,
    project: &str,
    revision: u64,
    draft: &str,
    definition: &str,
) -> String {
    save_attributed(command_id, project, revision, draft, definition, None)
}

/// The same command carrying optional client-supplied source attribution.
fn save_attributed(
    command_id: &str,
    project: &str,
    revision: u64,
    draft: &str,
    definition: &str,
    provenance: Option<serde_json::Value>,
) -> String {
    let mut document = save_command(command_id, project, revision, draft, definition);
    if let Some(provenance) = provenance {
        document["body"]["command"]["input"]["provenance"] = provenance;
    }
    document.to_string()
}

fn save_command(
    command_id: &str,
    project: &str,
    revision: u64,
    draft: &str,
    definition: &str,
) -> serde_json::Value {
    serde_json::json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": command_id,
            "command": {
                "kind": "save-draft",
                "input": {
                    "scope": {"project-id": project, "environment": "dev"},
                    "draft-id": draft,
                    "wiring-id": "receive-material",
                    "expected-revision": revision,
                    "definition": definition,
                }
            }
        }
    })
}

/// One well-formed request document for every contract kind this transport does
/// not mount, paired with its kind for assertion messages.
///
/// Each document has to decode: the contract boundary answers `400` for a
/// document it rejects, so a `501` for one of these is evidence about the route
/// and nothing else.
/// The contract command kinds this transport still answers `501` for.
///
/// `test-set-run` was here until wamn-0h0g.8.5.4 mounted it. Its assertions moved
/// to the composition block; what stays here is the property that outlives it —
/// a kind with no route answers a bare `501`, and the answer carries no
/// document.
fn unmounted_commands() -> Vec<(&'static str, String)> {
    let scope = serde_json::json!({"project-id": PROJECT, "environment": "dev"});
    let validated = serde_json::json!({"validated-draft-id": "validated-draft-1"});
    [
        (
            "validate",
            serde_json::json!({
                "scope": scope.clone(),
                "draft": {"draft-id": "draft-unmounted", "revision": 1},
            }),
        ),
        (
            "draft-run",
            serde_json::json!({
                "scope": scope.clone(),
                "validated-draft": validated.clone(),
                "input": {},
            }),
        ),
        (
            "publish",
            serde_json::json!({
                "scope": scope.clone(),
                "validated-draft": validated,
                "successful-report-id": "report-1",
            }),
        ),
    ]
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

fn unmounted_queries() -> Vec<(&'static str, String)> {
    let scope = serde_json::json!({"project-id": PROJECT, "environment": "dev"});
    [(
        "read-draft",
        serde_json::json!({
            "scope": scope,
            "draft": {"draft-id": "draft-unmounted", "revision": 1},
        }),
    )]
    .into_iter()
    .map(|(kind, input)| {
        let document = serde_json::json!({
            "document": "request",
            "body": {
                "schema-version": "0.1",
                "query-id": format!("unmounted-{kind}"),
                "query": {"kind": kind, "input": input},
            }
        });
        (kind, document.to_string())
    })
    .collect()
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

/// Create one disposable working-tree checkout. There is no repository in it:
/// the platform never sees a checkout, only the content a client sends.
fn checkout() -> PathBuf {
    let root = std::env::temp_dir().join("wamn-ftfc2-checkout");
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("flows")).expect("create the checkout");
    root
}

/// Write one flow definition file, as an editor or an agent would.
fn edit(root: &Path, file: &str, contents: &str) -> PathBuf {
    let path = root.join("flows").join(file);
    std::fs::write(&path, contents).expect("write the definition file");
    path
}

/// The whole S1 client: read the working-tree file and submit its content with
/// the revision the client last saw, presenting its own bearer token.
///
/// No repository is opened, no Git process runs, and no database URL or
/// platform credential is involved on either side of this call.
async fn submit(
    token: &str,
    command_id: &str,
    path: &Path,
    draft: &str,
    expected_revision: u64,
) -> Response {
    submit_with(token, command_id, path, draft, expected_revision, &[]).await
}

/// Submit exactly as [`submit`], with extra transport headers attached.
async fn submit_with(
    token: &str,
    command_id: &str,
    path: &Path,
    draft: &str,
    expected_revision: u64,
    extra: &[(&str, &str)],
) -> Response {
    let bytes = std::fs::read(path).expect("read the working-tree definition file");
    let definition = String::from_utf8(bytes).expect("a definition file is UTF-8");
    let document = save_definition(command_id, PROJECT, expected_revision, draft, &definition);
    post("/authoring", Some(token), extra, &document).await
}

/// Submit as [`submit`], attaching the commit the working tree was read at.
async fn submit_attributed(
    token: &str,
    command_id: &str,
    path: &Path,
    draft: &str,
    expected_revision: u64,
    provenance: serde_json::Value,
) -> Response {
    let bytes = std::fs::read(path).expect("read the working-tree definition file");
    let definition = String::from_utf8(bytes).expect("a definition file is UTF-8");
    let document = save_attributed(
        command_id,
        PROJECT,
        expected_revision,
        draft,
        &definition,
        Some(provenance),
    );
    post("/authoring", Some(token), &[], &document).await
}

/// The response envelope's outcome, which is where every typed result and
/// product refusal lives.
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
                graph_json, wiring_hash, gate_report_id) \
             VALUES ($1, $2, $3, 1, 1, $4::text::jsonb, $5, $6)",
            &[
                &TENANT,
                &CANDIDATE_CATALOG,
                &CANDIDATE_WIRING,
                &graph.to_string(),
                &CANDIDATE_WIRING_HASH,
                &CANDIDATE_GATE_REPORT,
            ],
        )
        .await
        .context("seed the gated candidate wiring")?;
    Ok(())
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

/// Stand in for the executor: release one caller outcome per admitted ordinal.
///
/// It waits for each ordinal's run to EXIST before completing it, and completes
/// them strictly in ordinal order. That is the whole point: the composition
/// admits ordinal `n + 1` only after ordinal `n` has a stored verdict, so if it
/// ever admitted them together this task would complete them out of the order
/// the composition observes and the sequencing assertions would fail.
async fn complete_admitted_case_runs(url: String, outcomes: Vec<(i32, serde_json::Value)>) {
    let (project, task) = connect(&url)
        .await
        .expect("connect the project plane as the gate admin");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    for (ordinal, (status, body)) in outcomes.iter().enumerate() {
        let key = format!("case:{CANDIDATE_GATE_REPORT}:{ordinal}");
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "ordinal {ordinal} was never admitted"
            );
            let updated = project
                .execute(
                    "UPDATE wamn_run.runs \
                        SET status = 'completed', caller_outcome_kind = 'responded', \
                            caller_http_status = $1, caller_outcome_json = $2::text::jsonb, \
                            caller_release_node_id = 'node', \
                            caller_released_at = clock_timestamp(), \
                            updated_at = clock_timestamp() \
                      WHERE tenant_id = $3 AND idempotency_key = $4 AND status = 'dispatched'",
                    &[status, &body.to_string(), &TENANT, &key],
                )
                .await
                .expect("complete one admitted case run");
            if updated == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    task.abort();
}

/// Every stored case verdict of the composed report, in ordinal order.
type CaseVerdict = (
    i32,
    String,
    String,
    bool,
    Option<String>,
    serde_json::Value,
    SystemTime,
);

async fn control_case_verdicts(admin: &Client) -> Vec<CaseVerdict> {
    admin
        .query(
            &format!(
                "SELECT ordinal, case_id, run_id, passed, failure_kind, summary, finalized_at \
                   FROM {SOURCE_SCHEMA}.authoring_test_case_runs \
                  WHERE tenant_id = $1 AND report_id = $2 ORDER BY ordinal"
            ),
            &[&TENANT, &CANDIDATE_GATE_REPORT],
        )
        .await
        .expect("read the composed case verdicts")
        .iter()
        .map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get(2),
                row.get::<_, Option<bool>>(3).unwrap_or(false),
                row.get(4),
                row.get(5),
                row.get(6),
            )
        })
        .collect()
}

/// Every run this composition admitted into the project plane, in admission
/// order.
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

fn authoring_scope() -> ControlAuthoringScope {
    ControlAuthoringScope {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        environment: ENVIRONMENT.to_owned(),
        tenant_id: TENANT.to_owned(),
        source_schema: SOURCE_SCHEMA.to_owned(),
    }
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

async fn retry_ledger_count(admin: &Client, principal_id: &str, command_id: &str) -> i64 {
    admin
        .query_one(
            "SELECT count(*) FROM catalog.authoring_command_audit \
              WHERE tenant_id = $1 AND principal_id = $2 AND command_id = $3",
            &[&TENANT, &principal_id, &command_id],
        )
        .await
        .expect("count one retry identity")
        .get(0)
}

async fn draft_count(admin: &Client) -> i64 {
    admin
        .query_one("SELECT count(*) FROM catalog.flow_drafts", &[])
        .await
        .expect("count drafts")
        .get(0)
}

async fn authoring_durable_counts(admin: &Client) -> Vec<i64> {
    let row = admin
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM catalog.flow_drafts), \
                        (SELECT count(*) FROM catalog.authoring_command_audit), \
                        (SELECT count(*) FROM {SOURCE_SCHEMA}.authoring_test_run_reservations), \
                        (SELECT count(*) FROM {SOURCE_SCHEMA}.authoring_test_case_runs), \
                        (SELECT count(*) FROM {SOURCE_SCHEMA}.authoring_test_reports)"
            ),
            &[],
        )
        .await
        .expect("count every authoring durable relation");
    (0..row.len()).map(|index| row.get(index)).collect()
}

async fn insert_pending_report(
    admin: &Client,
    tenant_id: &str,
    report_id: &str,
    validated_draft_id: &str,
) {
    admin
        .execute(
            &format!(
                "INSERT INTO {SOURCE_SCHEMA}.authoring_test_run_reservations \
                    (tenant_id, report_id, command_hash, validated_draft_id, catalog_id, \
                     catalog_version, case_count, whole_deadline_at) \
                 VALUES ($1, $2, 'sha256:' || repeat('0', 64), $3, 'catalog-a', 1, 1, \
                         clock_timestamp() + interval '1 hour')"
            ),
            &[&tenant_id, &report_id, &validated_draft_id],
        )
        .await
        .expect("insert one pending control-store report reservation");
}

async fn finalize_report(admin: &Client, report_id: &str, validated_draft_id: &str) {
    admin
        .execute(
            &format!(
                "INSERT INTO {SOURCE_SCHEMA}.authoring_test_reports \
                    (tenant_id, report_id, validated_draft_id, catalog_id, catalog_version, \
                     passed, summary) \
                 VALUES ($1, $2, $3, 'catalog-a', 1, false, \
                         '{{\"cases\":[{{\"case-id\":\"case-a\",\"passed\":false}}]}}'::jsonb)"
            ),
            &[&TENANT, &report_id, &validated_draft_id],
        )
        .await
        .expect("insert one immutable control-store report");
    admin
        .execute(
            &format!(
                "UPDATE {SOURCE_SCHEMA}.authoring_test_run_reservations \
                    SET state = 'finalized', finalized_at = clock_timestamp() \
                  WHERE tenant_id = $1 AND report_id = $2"
            ),
            &[&TENANT, &report_id],
        )
        .await
        .expect("finalize the report reservation");
}

/// Read one exact draft revision through the canonical mutable-document store
/// statement, so this gate cannot agree with the production read by accident.
async fn draft_at_revision(admin: &Client, draft: &str, revision: i64) -> Option<(String, String)> {
    admin
        .query_opt(
            wamn_scenario_worker::store::drafts::select_flow_draft_sql(),
            &[&TENANT, &draft, &revision],
        )
        .await
        .expect("read one exact draft revision")
        .map(|row| (row.get(0), row.get(1)))
}

async fn stored_revision(admin: &Client, draft: &str) -> Option<i64> {
    admin
        .query_opt(
            "SELECT revision FROM catalog.flow_drafts WHERE tenant_id = $1 AND draft_id = $2",
            &[&TENANT, &draft],
        )
        .await
        .expect("read the stored draft revision")
        .map(|row| row.get(0))
}

/// The attribution recorded for one command target: subject and effective role.
async fn attribution(admin: &Client, target: &str) -> Vec<(String, String)> {
    admin
        .query(
            "SELECT principal_subject, effective_role \
               FROM catalog.authoring_command_audit \
              WHERE tenant_id = $1 AND target_ref = $2 ORDER BY recorded_at",
            &[&TENANT, &target],
        )
        .await
        .expect("read the attribution for one target")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

/// The client's recorded source claim for one command target.
type RecordedProvenance = (Option<String>, Option<String>, Option<bool>);

async fn recorded_provenance(admin: &Client, target: &str) -> Vec<RecordedProvenance> {
    admin
        .query(
            "SELECT provenance_commit, provenance_ref, provenance_dirty \
               FROM catalog.authoring_command_audit \
              WHERE tenant_id = $1 AND target_ref = $2 ORDER BY recorded_at",
            &[&TENANT, &target],
        )
        .await
        .expect("read the recorded provenance for one target")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
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
            &save_document("refused", PROJECT, 0, "draft-refused"),
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
        &save_document("refused", OTHER_PROJECT, 0, "draft-refused"),
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
        draft_count(&admin).await,
        0,
        "an unmounted kind reached a command"
    );
    assert_eq!(
        authoring_durable_counts(&admin).await,
        durable_before_unmounted,
        "an unmounted command or query wrote durable authoring state"
    );

    // Nothing above reached a command: no draft, no ledger row. `test-set-run`
    // is no longer among the kinds above — wamn-0h0g.8.5.4 mounted it, and the
    // assertions that replaced its bare 501 run at the end of this gate, where
    // they cannot disturb the "nothing has reached a command yet" invariants
    // this section and the query section below both depend on.
    assert_eq!(draft_count(&admin).await, 0, "a refusal ran a command");
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

    insert_pending_report(
        &admin,
        "foreign-management-tenant",
        "report-foreign",
        "validated-foreign",
    )
    .await;
    let before_foreign_read = authoring_durable_counts(&admin).await;
    let foreign = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &get_report_document("foreign-report", PROJECT, "report-foreign"),
    )
    .await;
    assert_eq!(foreign.status, 200, "{}", foreign.body);
    assert_eq!(
        outcome(&foreign.body)["value"]["reason"],
        serde_json::json!({"kind": "report-not-found", "report-id": "report-foreign"})
    );
    assert_eq!(
        authoring_durable_counts(&admin).await,
        before_foreign_read,
        "cross-tenant get-report mutated control authoring state"
    );

    insert_pending_report(&admin, TENANT, "report-a", "validated-a").await;
    let before_pending_read = authoring_durable_counts(&admin).await;
    let pending = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &get_report_document("pending-report", PROJECT, "report-a"),
    )
    .await;
    assert_eq!(pending.status, 200, "{}", pending.body);
    let pending_document: serde_json::Value = serde_json::from_str(&pending.body).unwrap();
    assert_eq!(pending_document["body"]["query-id"], "pending-report");
    assert_eq!(
        pending_document["body"]["outcome"]["value"]["result"],
        serde_json::json!({
            "state": "pending",
            "report-id": "report-a",
            "validated-draft": {"validated-draft-id": "validated-a"},
        })
    );
    assert_eq!(
        authoring_durable_counts(&admin).await,
        before_pending_read,
        "pending get-report mutated control authoring state"
    );

    finalize_report(&admin, "report-a", "validated-a").await;
    let before_finalized_read = authoring_durable_counts(&admin).await;
    let finalized = post(
        "/authoring",
        Some(bob.token()),
        &[],
        &get_report_document("finalized-report", PROJECT, "report-a"),
    )
    .await;
    assert_eq!(finalized.status, 200, "{}", finalized.body);
    let finalized_document: serde_json::Value = serde_json::from_str(&finalized.body).unwrap();
    assert_eq!(
        finalized_document["body"]["outcome"]["value"]["result"],
        serde_json::json!({
            "state": "finalized",
            "report-id": "report-a",
            "validated-draft": {"validated-draft-id": "validated-a"},
            "passed": false,
            "summary": {"cases": [{"case-id": "case-a", "passed": false}]},
        })
    );
    assert!(
        finalized_document
            .to_string()
            .find("resolution-map")
            .is_none(),
        "get-report fabricated the retired resolution map"
    );
    assert_eq!(
        authoring_durable_counts(&admin).await,
        before_finalized_read,
        "finalized get-report mutated control authoring state"
    );
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a non-ledgered query appended a command audit row"
    );

    // ---- a valid human token reaches the command with trusted context --------
    let saved = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &save_document("save-1", PROJECT, 0, "draft-alice"),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert!(
        saved.body.contains(r#""command-id":"save-1""#),
        "{}",
        saved.body
    );
    assert!(
        saved.body.contains(r#""status":"completed""#),
        "{}",
        saved.body
    );

    // Same principal + command ID + canonical content replays the exact stored
    // full envelope even when raw JSON object order and explicit null/omission
    // differ. It executes no second draft write and adds no ledger row.
    let exact_retry = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &reordered_save_document("save-1", PROJECT, 0, "draft-alice", DRAFT_GRAPH),
    )
    .await;
    assert_eq!(exact_retry.status, 200);
    assert_eq!(exact_retry.body.as_bytes(), saved.body.as_bytes());
    assert_eq!(ledger_rows(&admin).await.len(), 1);
    assert_eq!(stored_revision(&admin, "draft-alice").await, Some(1));

    // Same retry identity with changed canonical content is the typed refusal;
    // it neither discloses the stored completion nor mutates draft or ledger.
    let divergent = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &save_definition(
            "save-1",
            PROJECT,
            0,
            "draft-alice",
            &DRAFT_GRAPH.replace(r#""status":200"#, r#""status":299"#),
        ),
    )
    .await;
    assert_eq!(divergent.status, 200);
    assert_eq!(
        outcome(&divergent.body)["value"]["reason"]["kind"],
        "command-id-reuse"
    );
    assert_ne!(divergent.body, saved.body);
    assert_eq!(ledger_rows(&admin).await.len(), 1);
    assert_eq!(stored_revision(&admin, "draft-alice").await, Some(1));

    // ---- a service token reaches the same command ---------------------------
    let by_service = post(
        "/authoring",
        Some(service_token.token()),
        &[],
        &save_document("save-2", PROJECT, 0, "draft-service"),
    )
    .await;
    assert_eq!(by_service.status, 200, "{}", by_service.body);

    // ---- two principals, the same command, distinguishable evidence ---------
    let by_bob = post(
        "/authoring",
        Some(bob.token()),
        &[],
        &save_document("save-1", PROJECT, 0, "draft-alice"),
    )
    .await;
    // Same draft at revision 0 again: the command runs and refuses on revision,
    // which is a product refusal — it still attributes. In particular, Bob
    // does not learn or replay Alice's stored completion for the same command
    // ID.
    assert_eq!(by_bob.status, 200, "{}", by_bob.body);
    assert_eq!(
        outcome(&by_bob.body)["value"]["reason"]["kind"],
        "revision-conflict"
    );
    assert_ne!(by_bob.body, saved.body);

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

    let rows = ledger_rows(&admin).await;
    assert_eq!(
        rows.len(),
        3,
        "one ledger row per authorized command: {rows:?}"
    );
    let alice_row = rows
        .iter()
        .find(|row| row.0 == "alice@example.com")
        .expect("alice is attributed");
    let bob_row = rows
        .iter()
        .find(|row| row.0 == "bob@example.com")
        .expect("bob is attributed");
    assert_ne!(
        alice_row.1, bob_row.1,
        "two principals ran the same command and are not distinguishable"
    );
    assert_eq!(alice_row.1, alice_principal.id().as_str());
    assert_eq!(bob_row.1, bob_principal.id().as_str());
    assert_eq!(alice_row.2, "save-draft");
    assert_eq!(bob_row.2, "save-draft");
    // The role is the one the caller actually holds, not one it asked for.
    assert_eq!(alice_row.3, "project-author");
    assert_eq!(bob_row.3, "project-admin");
    assert!(
        rows.iter().any(|row| row.0 == "ci-runner"),
        "the service principal is attributed: {rows:?}"
    );

    // ---- client-injected identity never overrides the token -----------------
    // A header asserting another principal is simply never read.
    let injected_header = post(
        "/authoring",
        Some(alice.token()),
        &[
            ("X-Wamn-Principal", "bob@example.com"),
            ("X-Wamn-Role", "project-admin"),
        ],
        &save_document("save-3", PROJECT, 0, "draft-injected"),
    )
    .await;
    assert_eq!(injected_header.status, 200, "{}", injected_header.body);
    let rows = ledger_rows(&admin).await;
    let injected_row = rows
        .iter()
        .find(|row| row.2 == "save-draft" && row.1 == alice_principal.id().as_str())
        .expect("the header request is attributed to the token principal");
    assert_eq!(injected_row.3, "project-author", "a header widened a role");
    assert_eq!(
        rows.iter()
            .filter(|row| row.1 == bob_principal.id().as_str())
            .count(),
        1,
        "a header attributed a command to the wrong principal: {rows:?}"
    );

    // A body asserting a principal is refused by the contract before dispatch.
    let before = ledger_rows(&admin).await.len();
    let injected_body = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &save_document("save-4", PROJECT, 0, "draft-body").replace(
            r#""draft-id""#,
            r#""principal":"bob@example.com","draft-id""#,
        ),
    )
    .await;
    assert_eq!(injected_body.status, 400, "{}", injected_body.body);
    assert_eq!(
        ledger_rows(&admin).await.len(),
        before,
        "a smuggled principal reached a command"
    );

    // ---- S1: a checkout client submits working-tree file content -----------
    let root = checkout();
    let file = edit(&root, "receive-material.flow.json", DRAFT_GRAPH);
    let created = submit(alice.token(), "checkout-1", &file, "draft-checkout", 0).await;
    assert_eq!(created.status, 200, "{}", created.body);
    let result = outcome(&created.body);
    assert_eq!(result["status"], "completed", "{}", created.body);
    assert_eq!(result["value"]["command"], "save-draft");
    assert_eq!(result["value"]["result"]["draft-id"], "draft-checkout");
    assert_eq!(result["value"]["result"]["wiring-id"], "receive-material");
    assert_eq!(result["value"]["result"]["revision"], 1);

    // The editor changes one literal in the file; the client submits the new
    // content at exactly the revision it last saw.
    let edited_text = DRAFT_GRAPH.replace(r#""status":200"#, r#""status":201"#);
    assert_ne!(edited_text, DRAFT_GRAPH, "the fixture edit changed nothing");
    let file = edit(&root, "receive-material.flow.json", &edited_text);
    let saved = submit(alice.token(), "checkout-2", &file, "draft-checkout", 1).await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(outcome(&saved.body)["value"]["result"]["revision"], 2);

    // The revision the client was handed is the revision the canonical read
    // returns, carrying the document the client submitted.
    let (flow_id, stored) = draft_at_revision(&admin, "draft-checkout", 2)
        .await
        .expect("the canonical store read finds the revision the client was handed");
    assert_eq!(flow_id, "receive-material");
    assert_eq!(
        as_json(&stored),
        as_json(&edited_text),
        "the stored revision is not the document the client submitted"
    );
    // A superseded revision is not separately addressable: the draft is one
    // mutable document, so the store exposes only its current revision.
    assert!(
        draft_at_revision(&admin, "draft-checkout", 1)
            .await
            .is_none()
    );

    // ---- a stale working copy refuses before it mutates anything -----------
    let stale_text = DRAFT_GRAPH.replace(r#""status":200"#, r#""status":500"#);
    let stale_file = edit(&root, "receive-material.flow.json", &stale_text);
    let before = draft_at_revision(&admin, "draft-checkout", 2)
        .await
        .expect("the draft is stored");
    // The client still believes it holds revision 1.
    let stale = submit(
        alice.token(),
        "checkout-stale",
        &stale_file,
        "draft-checkout",
        1,
    )
    .await;
    assert_eq!(stale.status, 200, "{}", stale.body);
    let refused = outcome(&stale.body);
    assert_eq!(refused["status"], "refused", "{}", stale.body);
    assert_eq!(refused["value"]["command"], "save-draft");
    assert_eq!(refused["value"]["reason"]["kind"], "revision-conflict");
    assert_eq!(refused["value"]["reason"]["expected-revision"], 1);
    // Refused before mutation: neither the revision nor the stored document
    // moved, so the concurrent editor's work is intact.
    assert_eq!(
        draft_at_revision(&admin, "draft-checkout", 2).await,
        Some(before),
        "a stale write overwrote the stored document"
    );
    assert_eq!(
        stored_revision(&admin, "draft-checkout").await,
        Some(2),
        "a stale write advanced the draft revision"
    );

    // ---- an unversioned or unsupported document refuses before a command ---
    let versioned = |change: fn(&mut serde_json::Value)| {
        let mut document = as_json(&save_definition(
            "checkout-version",
            PROJECT,
            0,
            "draft-version",
            DRAFT_GRAPH,
        ));
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
        assert!(
            stored_revision(&admin, "draft-version").await.is_none(),
            "{name} reached a command"
        );
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

    // ---- attribution a client attaches is inert ----------------------------
    // A checkout client legitimately knows the commit it edited from. It may
    // send that; the platform runs no Git, reads no such header, and must
    // produce the identical outcome and the identical attribution either way.
    let provenance = [
        ("X-Wamn-Commit", "0123456789abcdef0123456789abcdef01234567"),
        ("X-Git-Author", "bob@example.com"),
        ("X-Wamn-Signed-Off-By", "project-admin"),
        ("X-Wamn-Repository", "git@example.invalid:acme/flows.git"),
    ];
    let plain = submit(alice.token(), "prov-plain", &file, "draft-prov-plain", 0).await;
    let signed = submit_with(
        alice.token(),
        "prov-signed",
        &file,
        "draft-prov-signed",
        0,
        &provenance,
    )
    .await;
    assert_eq!(plain.status, signed.status, "{}", signed.body);
    assert_eq!(outcome(&plain.body)["status"], "completed");
    assert_eq!(
        outcome(&plain.body)["value"]["result"]["revision"],
        outcome(&signed.body)["value"]["result"]["revision"],
        "attached provenance changed the command outcome"
    );
    assert_eq!(
        as_json(
            &draft_at_revision(&admin, "draft-prov-plain", 1)
                .await
                .expect("the plain draft is stored")
                .1
        ),
        as_json(
            &draft_at_revision(&admin, "draft-prov-signed", 1)
                .await
                .expect("the signed draft is stored")
                .1
        ),
        "attached provenance changed what was stored"
    );
    // Attribution stays the verified presenter, never the attached author or
    // the attached role.
    assert_eq!(
        attribution(&admin, "draft-prov-signed").await,
        vec![("alice@example.com".to_owned(), "project-author".to_owned())],
        "attached provenance became identity or authority"
    );

    // ---- handler parity: the HTTP path is the canonical handler path -------
    // The same author saves the same content at the same expected revision,
    // once through the public versioned API and once through the canonical
    // audited command boundary that API itself calls.
    let parity_text = DRAFT_GRAPH.replace(r#""status":200"#, r#""status":202"#);
    let parity_file = edit(&root, "parity.flow.json", &parity_text);
    let (identity, identity_task) = connect(&url).await.expect("connect for authorization");
    let author = authorize(&identity, alice.token(), ORG, PROJECT)
        .await
        .expect("authorize the parity author")
        .expect("alice is admitted");
    let mut backend = InternalAuthoringBackend::connect(&author_url(&url), &authoring_scope())
        .await
        .expect("connect the canonical authoring backend");
    let scope = CommandScope::new(TENANT, ORG, PROJECT, ENVIRONMENT);
    let request = |draft: &str| SaveDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: draft.to_owned(),
        wiring_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: parity_text.clone(),
    };
    // Both paths carry the same source claim, so parity covers attribution too.
    let claim = wamn_authoring_model::CommitProvenance {
        commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        r#ref: Some("refs/heads/main".to_owned()),
        dirty: false,
    };
    let direct_document = save_attributed(
        "parity-direct",
        PROJECT,
        0,
        "draft-parity-direct",
        &parity_text,
        Some(serde_json::to_value(&claim).unwrap()),
    );
    let direct_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&direct_document)["body"].clone()).unwrap();
    let direct = save_draft(
        &mut backend,
        &author,
        &scope,
        &direct_command,
        &request("draft-parity-direct"),
    )
    .await
    .expect("the canonical handler runs");
    assert_eq!(
        outcome(std::str::from_utf8(&direct).unwrap())["value"]["result"]["revision"],
        1
    );
    let over_http = submit_attributed(
        alice.token(),
        "parity-http",
        &parity_file,
        "draft-parity-http",
        0,
        serde_json::json!({
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "ref": "refs/heads/main",
            "dirty": false,
        }),
    )
    .await;
    assert_eq!(over_http.status, 200, "{}", over_http.body);
    assert_eq!(outcome(&over_http.body)["value"]["result"]["revision"], 1);
    assert_eq!(
        draft_at_revision(&admin, "draft-parity-direct", 1)
            .await
            .map(|row| row.1),
        draft_at_revision(&admin, "draft-parity-http", 1)
            .await
            .map(|row| row.1),
        "the two paths stored different documents"
    );
    assert_eq!(
        attribution(&admin, "draft-parity-direct").await,
        attribution(&admin, "draft-parity-http").await,
        "the two paths attributed differently"
    );
    assert_eq!(
        recorded_provenance(&admin, "draft-parity-direct").await,
        recorded_provenance(&admin, "draft-parity-http").await,
        "the two paths recorded the source claim differently"
    );

    // ---- concurrent retries serialize on the complete retry identity ------
    // Separate database connections eliminate the HTTP surface's process-local
    // mutex from this proof. Exact retries produce one mutation and one ledger
    // row, then both callers receive the exact same stored envelope bytes.
    let mut concurrent_backend =
        InternalAuthoringBackend::connect(&author_url(&url), &authoring_scope())
            .await
            .expect("connect the concurrent authoring backend");
    let exact_document = save_document("concurrent-exact", PROJECT, 0, "draft-concurrent-exact");
    let exact_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&exact_document)["body"].clone()).unwrap();
    let exact_request = SaveDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-exact".to_owned(),
        wiring_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: DRAFT_GRAPH.to_owned(),
    };
    let (exact_left, exact_right) = tokio::join!(
        save_draft(
            &mut backend,
            &author,
            &scope,
            &exact_command,
            &exact_request,
        ),
        save_draft(
            &mut concurrent_backend,
            &author,
            &scope,
            &exact_command,
            &exact_request,
        ),
    );
    let exact_left = exact_left.expect("the first exact retry completes");
    let exact_right = exact_right.expect("the second exact retry completes");
    assert_eq!(
        exact_left, exact_right,
        "exact retry envelope bytes drifted"
    );
    assert_eq!(
        stored_revision(&admin, "draft-concurrent-exact").await,
        Some(1)
    );
    assert_eq!(
        retry_ledger_count(&admin, author.principal_id(), "concurrent-exact").await,
        1
    );

    // Divergent canonical requests sharing the same retry identity also
    // serialize: exactly one request executes and the other gets the typed
    // reuse refusal without a second draft or ledger mutation.
    let divergent_definition = DRAFT_GRAPH.replace(r#""status":200"#, r#""status":299"#);
    let divergent_left_document = save_definition(
        "concurrent-divergent",
        PROJECT,
        0,
        "draft-concurrent-left",
        DRAFT_GRAPH,
    );
    let divergent_right_document = save_definition(
        "concurrent-divergent",
        PROJECT,
        0,
        "draft-concurrent-right",
        &divergent_definition,
    );
    let divergent_left_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&divergent_left_document)["body"].clone()).unwrap();
    let divergent_right_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&divergent_right_document)["body"].clone()).unwrap();
    let divergent_left_request = SaveDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-left".to_owned(),
        wiring_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: DRAFT_GRAPH.to_owned(),
    };
    let divergent_right_request = SaveDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-right".to_owned(),
        wiring_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: divergent_definition,
    };
    let (divergent_left, divergent_right) = tokio::join!(
        save_draft(
            &mut backend,
            &author,
            &scope,
            &divergent_left_command,
            &divergent_left_request,
        ),
        save_draft(
            &mut concurrent_backend,
            &author,
            &scope,
            &divergent_right_command,
            &divergent_right_request,
        ),
    );
    let divergent_left = divergent_left.expect("the first divergent retry answers");
    let divergent_right = divergent_right.expect("the second divergent retry answers");
    let mut kinds = [
        outcome(std::str::from_utf8(&divergent_left).unwrap())["status"]
            .as_str()
            .unwrap()
            .to_owned(),
        outcome(std::str::from_utf8(&divergent_right).unwrap())["status"]
            .as_str()
            .unwrap()
            .to_owned(),
    ];
    kinds.sort();
    assert_eq!(kinds, ["completed", "refused"]);
    let refusal = [&divergent_left, &divergent_right]
        .into_iter()
        .map(|bytes| outcome(std::str::from_utf8(bytes).unwrap()))
        .find(|value| value["status"] == "refused")
        .expect("one divergent request refuses");
    assert_eq!(refusal["value"]["reason"]["kind"], "command-id-reuse");
    let created = usize::from(
        stored_revision(&admin, "draft-concurrent-left")
            .await
            .is_some(),
    ) + usize::from(
        stored_revision(&admin, "draft-concurrent-right")
            .await
            .is_some(),
    );
    assert_eq!(created, 1, "a divergent retry executed both requests");
    assert_eq!(
        retry_ledger_count(&admin, author.principal_id(), "concurrent-divergent").await,
        1
    );
    drop(concurrent_backend);

    // Both paths refuse a stale expected revision the same way.
    let stale_direct_document = save_attributed(
        "parity-direct-stale",
        PROJECT,
        9,
        "draft-parity-direct",
        &parity_text,
        Some(serde_json::to_value(&claim).unwrap()),
    );
    let stale_direct_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&stale_direct_document)["body"].clone()).unwrap();
    let stale_direct = save_draft(
        &mut backend,
        &author,
        &scope,
        &stale_direct_command,
        &SaveDraft {
            expected_revision: 9,
            ..request("draft-parity-direct")
        },
    )
    .await
    .expect("the canonical handler runs");
    assert_eq!(
        outcome(std::str::from_utf8(&stale_direct).unwrap())["value"]["reason"]["kind"],
        "revision-conflict"
    );
    let stale_http = submit(
        alice.token(),
        "parity-http-stale",
        &parity_file,
        "draft-parity-http",
        9,
    )
    .await;
    assert_eq!(
        outcome(&stale_http.body)["value"]["reason"]["kind"],
        "revision-conflict"
    );
    // Neither refusal advanced its draft.
    for draft in ["draft-parity-direct", "draft-parity-http"] {
        assert_eq!(stored_revision(&admin, draft).await, Some(1), "{draft}");
    }
    drop(backend);
    identity_task.abort();

    // ---- the stored draft is the exact submitted bytes ---------------------
    // `definition` is `text`, so what comes back is what went in: whitespace,
    // key order, trailing newline and all. Nothing on the save path parses it.
    let exact = "{  \"schema-version\":\"0.1\",\n\n  \"flow-id\":\"receive-material\",\n  \
                 \"version\":1,\n\t\"nodes\":[],\n  \"edges\":[]  }\n";
    let exact_file = edit(&root, "exact.flow.json", exact);
    let saved = submit(alice.token(), "exact-1", &exact_file, "draft-exact", 0).await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let (_, stored) = draft_at_revision(&admin, "draft-exact", 1)
        .await
        .expect("the exact draft is stored");
    assert_eq!(
        stored, exact,
        "the stored revision is not the bytes the client submitted"
    );
    // And it is still exactly the file on disk, which is the whole point: the
    // client can diff its working tree against the stored revision.
    assert_eq!(
        stored,
        std::fs::read_to_string(&exact_file).expect("read the working-tree file"),
        "the stored revision drifted from the working-tree file"
    );

    // ---- a half-finished edit is a preserved draft, not a failure ----------
    // This is the normal state of a file between two keystrokes. Save stores it
    // without parsing.
    let ledger_before = ledger_rows(&admin).await.len();
    let broken = "{\"schema-version\":\"0.1\",\n  \"nodes\": [";
    let broken_file = edit(&root, "broken.flow.json", broken);
    let preserved = submit(alice.token(), "broken-1", &broken_file, "draft-broken", 0).await;
    assert_eq!(preserved.status, 200, "{}", preserved.body);
    assert_eq!(
        outcome(&preserved.body)["status"],
        "completed",
        "{}",
        preserved.body
    );
    assert_eq!(outcome(&preserved.body)["value"]["result"]["revision"], 1);
    let (_, stored) = draft_at_revision(&admin, "draft-broken", 1)
        .await
        .expect("the half-finished draft is preserved");
    assert_eq!(
        stored, broken,
        "invalid intermediate text was not preserved"
    );
    assert_eq!(
        ledger_rows(&admin).await.len(),
        ledger_before + 1,
        "the authorized save was not attributed"
    );
    // An emptied file is equally legitimate, and equally preserved.
    let emptied_file = edit(&root, "emptied.flow.json", "");
    let emptied = submit(
        alice.token(),
        "emptied-1",
        &emptied_file,
        "draft-emptied",
        0,
    )
    .await;
    assert_eq!(emptied.status, 200, "{}", emptied.body);
    assert_eq!(
        draft_at_revision(&admin, "draft-emptied", 1)
            .await
            .expect("the emptied draft is preserved")
            .1,
        ""
    );

    // ---- provenance is recorded verbatim, and only as attribution ----------
    let attributed = serde_json::json!({
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "ref": "refs/heads/main",
        "dirty": false,
    });
    let with_source = submit_attributed(
        alice.token(),
        "prov-recorded",
        &file,
        "draft-prov-recorded",
        0,
        attributed.clone(),
    )
    .await;
    assert_eq!(with_source.status, 200, "{}", with_source.body);
    assert_eq!(
        recorded_provenance(&admin, "draft-prov-recorded").await,
        vec![(
            Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            Some("refs/heads/main".to_owned()),
            Some(false),
        )],
        "the client's source claim was not recorded verbatim"
    );
    // A detached checkout has a commit and no ref; a dirty tree says so.
    let detached = submit_attributed(
        alice.token(),
        "prov-detached",
        &file,
        "draft-prov-detached",
        0,
        serde_json::json!({"commit": "feedface", "ref": null, "dirty": true}),
    )
    .await;
    assert_eq!(detached.status, 200, "{}", detached.body);
    assert_eq!(
        recorded_provenance(&admin, "draft-prov-detached").await,
        vec![(Some("feedface".to_owned()), None, Some(true))]
    );
    // Omitting it records nothing rather than inventing a claim.
    assert_eq!(
        recorded_provenance(&admin, "draft-prov-plain").await,
        vec![(None, None, None)],
        "an absent claim was fabricated"
    );

    // Two commands differing ONLY in provenance are indistinguishable in every
    // respect a client can observe, and in what they stored.
    let twin = edit(&root, "twin.flow.json", &edited_text);
    let bare = submit(alice.token(), "twin-bare", &twin, "draft-twin-bare", 0).await;
    let claimed = submit_attributed(
        alice.token(),
        "twin-claimed",
        &twin,
        "draft-twin-claimed",
        0,
        serde_json::json!({"commit": "deadbeef", "ref": "refs/heads/other", "dirty": true}),
    )
    .await;
    assert_eq!(bare.status, claimed.status);
    assert_eq!(
        outcome(&bare.body).to_string().replace("bare", "claimed"),
        outcome(&claimed.body).to_string(),
        "attribution changed the command outcome"
    );
    assert_eq!(
        draft_at_revision(&admin, "draft-twin-bare", 1)
            .await
            .map(|row| row.1),
        draft_at_revision(&admin, "draft-twin-claimed", 1)
            .await
            .map(|row| row.1),
        "attribution changed what was stored"
    );
    // Even a claim that names a role or another principal is inert: the row is
    // attributed to the verified presenter, and the claim stays a claim.
    let hostile = submit_attributed(
        alice.token(),
        "prov-hostile",
        &twin,
        "draft-prov-hostile",
        0,
        serde_json::json!({"commit": "project-admin", "ref": "bob@example.com", "dirty": false}),
    )
    .await;
    assert_eq!(hostile.status, 200, "{}", hostile.body);
    assert_eq!(
        attribution(&admin, "draft-prov-hostile").await,
        vec![("alice@example.com".to_owned(), "project-author".to_owned())],
        "a source claim became identity or authority"
    );

    // ---- test-set-run IS mounted, and composes a real report ---------------
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

    // Ordinal 0 gets the status its case expects; ordinal 1 deliberately does
    // not, so the report's verdict comes from real per-case evaluation rather
    // than from everything trivially passing.
    let stand_in = tokio::spawn(complete_admitted_case_runs(
        project_admin_url(&url),
        vec![
            (201, serde_json::json!({"id": "first"})),
            (500, serde_json::json!({"error": "second"})),
        ],
    ));
    let accepted = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &test_set_run_document("test-set-1"),
    )
    .await;
    stand_in
        .await
        .expect("the stand-in executor completed every admitted run");
    assert_eq!(
        accepted.status, 200,
        "test-set-run did not reach its handler: {}",
        accepted.body
    );
    // The receipt names the report the composition DERIVED — the candidate's
    // gate report — not one the caller chose.
    assert_eq!(
        outcome(&accepted.body),
        serde_json::json!({
            "status": "completed",
            "value": {
                "command": "test-set-run",
                "result": {
                    "report-id": CANDIDATE_GATE_REPORT,
                    "validated-draft": {"validated-draft-id": CANDIDATE_WIRING_HASH},
                },
            },
        }),
        "the test-set receipt drifted: {}",
        accepted.body
    );

    // Exactly one run per ordinal, each under its derived producer key, each
    // with its queue row.
    let runs = project_case_runs(&project).await;
    assert_eq!(
        runs.iter()
            .map(|(_, key, status, _)| (key.as_str(), status.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                format!("case:{CANDIDATE_GATE_REPORT}:0").as_str(),
                "completed"
            ),
            (
                format!("case:{CANDIDATE_GATE_REPORT}:1").as_str(),
                "completed"
            ),
        ],
        "the admitted runs are not one per ordinal"
    );
    assert_eq!(project_queue_count(&project).await, 2);

    // Each case's asserted facts AND its frozen binding world reached the
    // immutable control summary, and the verdicts are the ones evaluation
    // produced rather than a blanket pass.
    let verdicts = control_case_verdicts(&admin).await;
    assert_eq!(
        verdicts.len(),
        2,
        "the report did not finalize both ordinals"
    );
    assert_eq!(verdicts[0].1, "creates");
    assert_eq!(verdicts[1].1, "rejects");
    assert!(
        verdicts[0].3,
        "the passing case was not recorded as passing"
    );
    assert!(!verdicts[1].3, "the failing case was recorded as passing");
    assert_eq!(verdicts[0].4, None);
    assert_eq!(verdicts[1].4, Some("assertion-failed".to_owned()));
    for (ordinal, verdict) in verdicts.iter().enumerate() {
        assert_eq!(
            verdict.5["case"]["actual"]["response"]["status"],
            serde_json::json!(if ordinal == 0 { 201 } else { 500 }),
            "ordinal {ordinal} recorded facts that are not its run's"
        );
        assert_eq!(
            verdict.5["binding-world"],
            serde_json::json!([]),
            "ordinal {ordinal} recorded no frozen binding world"
        );
    }
    assert!(
        verdicts[1].5["case"]["detail"].is_string(),
        "the failing case recorded no diff"
    );

    // SEQUENTIAL, and it is the ordering that proves it: ordinal 1's run did
    // not exist in the project plane until ordinal 0's verdict was committed to
    // the control plane. Both instants come from the one server clock.
    assert!(
        runs[1].3 >= verdicts[0].6,
        "ordinal 1 was admitted before ordinal 0's summary was stored"
    );
    assert!(
        verdicts[1].6 >= verdicts[0].6,
        "the case verdicts were not stored in ordinal order"
    );

    // The report is the conjunction of the cases, and it was attributed once.
    let report = admin
        .query_one(
            &format!(
                "SELECT passed, validated_draft_id FROM {SOURCE_SCHEMA}.authoring_test_reports \
                  WHERE tenant_id = $1 AND report_id = $2"
            ),
            &[&TENANT, &CANDIDATE_GATE_REPORT],
        )
        .await
        .expect("the composed report finalized");
    assert!(
        !report.get::<_, bool>(0),
        "a failing case passed the report"
    );
    assert_eq!(report.get::<_, String>(1), CANDIDATE_WIRING_HASH);
    assert_eq!(
        ledger_rows(&admin).await[ledger_before_composition..]
            .iter()
            .map(|(_, _, kind, _)| kind.clone())
            .collect::<Vec<_>>(),
        ["test-set-run".to_owned()],
        "the composed command was not attributed exactly once"
    );

    // The mounted query reads the report the mounted command produced.
    let projected = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &get_report_document("composed-report", PROJECT, CANDIDATE_GATE_REPORT),
    )
    .await;
    assert_eq!(projected.status, 200, "{}", projected.body);
    assert_eq!(
        outcome(&projected.body)["value"]["result"]["state"],
        serde_json::json!("finalized")
    );

    // ---- an exact retry converges rather than double-admitting -------------
    // The same command id replays the stored receipt. Nothing is admitted.
    let replay = post(
        "/authoring",
        Some(alice.token()),
        &[],
        &test_set_run_document("test-set-1"),
    )
    .await;
    assert_eq!(replay.status, 200, "{}", replay.body);
    assert_eq!(outcome(&replay.body), outcome(&accepted.body));

    // A DIFFERENT command id for the same candidate re-drives the whole
    // composition against durable state that already exists. Every step
    // converges — the reservation is its own, every ordinal admits `duplicate`,
    // every case already has its immutable verdict — so it reaches the same
    // report without a second run, a second queue row, or a changed verdict.
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
        "a re-driven composition admitted a second run"
    );
    assert_eq!(project_queue_count(&project).await, 2);
    assert_eq!(
        control_case_verdicts(&admin).await,
        verdicts,
        "a re-driven composition rewrote an immutable case verdict"
    );
    assert_eq!(
        ledger_rows(&admin).await.len(),
        ledger_before_composition + 2,
        "the second command id was not attributed exactly once"
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

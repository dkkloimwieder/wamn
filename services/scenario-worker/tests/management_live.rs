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
//!
//! It also owns `wamn-ftfc.2`'s S1 write path: a checkout client reads
//! working-tree definition files and submits their content with the revision it
//! last saw. That half proves the submitted document reaches the canonical
//! store, that the exact stored revision is the one the canonical validate read
//! returns, that a stale working copy refuses before mutating anything, that
//! the public HTTP path and the canonical audited handler path agree, and that
//! attribution a client attaches is inert.
//!
//! The recipe in `docs/archive/build-and-test.md` supplies one disposable database.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_postgres::{Client, NoTls};

use wamn_platform_identity::{
    IssuedPat, PAT_TOKEN_PREFIX, assign_project_role, create_human, create_service, issue_pat,
    resolve_subject, revoke_pat,
};
use wamn_scenario_worker::authoring::{InternalAuthoringBackend, SaveFlowDraft};
use wamn_scenario_worker::management::{CommandScope, authorize, save_flow_draft};
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const AUTHORING_TESTS_SQL: &str = include_str!("../../../deploy/sql/authoring-tests.sql");

const TENANT: &str = "management-live-tenant";
const SOURCE_SCHEMA: &str = "management_live_source";
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const OTHER_PROJECT: &str = "shipping";
const AUTHOR_LOGIN: &str = "wamn_management_live_author";
const AUTHOR_PASSWORD: &str = "wamn-management-live";
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
        r#"{{"body":{{"command":{{"input":{{"provenance":null,"definition":{},"expected-revision":{},"flow-id":"receive-material","draft-id":{},"scope":{{"environment":"dev","project-id":{}}}}},"kind":"save-flow-draft"}},"command-id":{},"schema-version":"0.1"}},"document":"request"}}"#,
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
                "kind": "save-flow-draft",
                "input": {
                    "scope": {"project-id": project, "environment": "dev"},
                    "draft-id": draft,
                    "flow-id": "receive-material",
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
            "test-set-run",
            serde_json::json!({
                "scope": scope.clone(),
                "validated-draft": validated.clone(),
                "test-set": {"definition": "{\"schema-version\":\"0.1\",\"cases\":[]}"},
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
    [
        (
            "read-draft",
            serde_json::json!({
                "scope": scope.clone(),
                "draft": {"draft-id": "draft-unmounted", "revision": 1},
            }),
        ),
        (
            "get-run",
            serde_json::json!({"scope": scope.clone(), "run-id": "run-unmounted"}),
        ),
        (
            "get-report",
            serde_json::json!({"scope": scope, "report-id": "report-unmounted"}),
        ),
    ]
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

fn author_url(admin_url: &str) -> String {
    // Same server and database, a different login: the adapter refuses a
    // credential it shares with the runtime.
    let (scheme, rest) = admin_url.split_once("://").expect("a postgres url");
    let tail = rest.split_once('@').expect("a credentialed url").1;
    format!("{scheme}://{AUTHOR_LOGIN}:{AUTHOR_PASSWORD}@{tail}")
}

async fn provision(admin: &mut Client) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SOURCE_SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP ROLE IF EXISTS {AUTHOR_LOGIN}; \
             DROP ROLE IF EXISTS wamn_scenario_author; \
             DROP ROLE IF EXISTS wamn_app; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system; END IF; END $$; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') \
             THEN CREATE ROLE wamn_effect_writer NOLOGIN; END IF; END $$; \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn-app-live' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE {AUTHOR_LOGIN} LOGIN PASSWORD '{AUTHOR_PASSWORD}' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_scenario_author TO {AUTHOR_LOGIN};"
        ))
        .await
        .context("reset management-live schemas and roles")?;
    admin
        .batch_execute(SYSTEM_SQL)
        .await
        .context("apply deploy/sql/system-schema.sql")?;
    admin
        .batch_execute(CATALOG_SQL)
        .await
        .context("apply deploy/sql/catalog-schema.sql")?;
    let schema = BareSchemaName::new(SOURCE_SCHEMA).expect("a valid bare schema");
    for record in [RUN_STATE_SQL, FLOWS_SQL, AUTHORING_TESTS_SQL] {
        admin
            .batch_execute(&rewrite_schema(record, &schema))
            .await
            .context("apply the run-plane records the authority probe reads")?;
    }
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
                        (SELECT count(*) FROM {SOURCE_SCHEMA}.runs), \
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

/// Read one exact draft revision through the very statement
/// `validate_flow_draft` selects with, so this gate cannot agree with the
/// canonical read by accident.
///
/// `validate` is the only command that resolves a mutable revision; `draft-run`
/// and `promote` consume the immutable pin it produces, so they inherit exactly
/// whatever this returns.
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
    provision(&mut admin).await.expect("provision the gate");

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
            authoring_database_url: author_url(&url),
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
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
    // three queries. This contract cut keeps each one absent.
    // tree and mounted none of them: each either has no backend or has one whose
    // trusted inputs no in-process producer supplies. Two properties have to
    // hold while that is true.
    //
    // First, route selection happens after authorization, so naming an
    // unmounted kind is not a way to ask whether a route exists. Every
    // untrusted presenter gets the one refusal document for every kind.
    let unmounted = unmounted_commands();
    let unmounted_queries = unmounted_queries();
    let durable_before_unmounted = authoring_durable_counts(&admin).await;
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

    // Nothing above reached a command: no draft, no ledger row.
    assert_eq!(draft_count(&admin).await, 0, "a refusal ran a command");
    assert!(
        ledger_rows(&admin).await.is_empty(),
        "a refusal was audited"
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
    assert_eq!(alice_row.2, "save-flow-draft");
    assert_eq!(bob_row.2, "save-flow-draft");
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
        .find(|row| row.2 == "save-flow-draft" && row.1 == alice_principal.id().as_str())
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
    assert_eq!(result["value"]["command"], "save-flow-draft");
    assert_eq!(result["value"]["result"]["draft-id"], "draft-checkout");
    assert_eq!(result["value"]["result"]["flow-id"], "receive-material");
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
        .expect("the canonical validate read finds the revision the client was handed");
    assert_eq!(flow_id, "receive-material");
    assert_eq!(
        as_json(&stored),
        as_json(&edited_text),
        "the stored revision is not the document the client submitted"
    );
    // A superseded revision is not separately addressable: the draft is one
    // mutable document, so `validate` can only ever pin the current revision.
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
    assert_eq!(refused["value"]["command"], "save-flow-draft");
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
    let mut backend = InternalAuthoringBackend::connect(
        &author_url(&url),
        TENANT.to_owned(),
        SOURCE_SCHEMA.to_owned(),
    )
    .await
    .expect("connect the canonical authoring backend");
    let scope = CommandScope::new(TENANT, ORG, PROJECT, "dev");
    let request = |draft: &str| SaveFlowDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: draft.to_owned(),
        flow_id: "receive-material".to_owned(),
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
    let direct = save_flow_draft(
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
    let mut concurrent_backend = InternalAuthoringBackend::connect(
        &author_url(&url),
        TENANT.to_owned(),
        SOURCE_SCHEMA.to_owned(),
    )
    .await
    .expect("connect the concurrent authoring backend");
    let exact_document = save_document("concurrent-exact", PROJECT, 0, "draft-concurrent-exact");
    let exact_command: wamn_authoring_model::AuthoringRequest =
        serde_json::from_value(as_json(&exact_document)["body"].clone()).unwrap();
    let exact_request = SaveFlowDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-exact".to_owned(),
        flow_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: DRAFT_GRAPH.to_owned(),
    };
    let (exact_left, exact_right) = tokio::join!(
        save_flow_draft(
            &mut backend,
            &author,
            &scope,
            &exact_command,
            &exact_request,
        ),
        save_flow_draft(
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
    let divergent_left_request = SaveFlowDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-left".to_owned(),
        flow_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: DRAFT_GRAPH.to_owned(),
    };
    let divergent_right_request = SaveFlowDraft {
        tenant_id: TENANT.to_owned(),
        draft_id: "draft-concurrent-right".to_owned(),
        flow_id: "receive-material".to_owned(),
        expected_revision: 0,
        definition: divergent_definition,
    };
    let (divergent_left, divergent_right) = tokio::join!(
        save_flow_draft(
            &mut backend,
            &author,
            &scope,
            &divergent_left_command,
            &divergent_left_request,
        ),
        save_flow_draft(
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
    let stale_direct = save_flow_draft(
        &mut backend,
        &author,
        &scope,
        &stale_direct_command,
        &SaveFlowDraft {
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
    // This is the normal state of a file between two keystrokes. Save stores
    // it; `validate` is where unparseable text becomes a typed refusal.
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
    admin_task.abort();
}

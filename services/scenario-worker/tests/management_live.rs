//! Real-PostgreSQL gate for the authenticated management authoring surface.
//!
//! Proves the whole bridge end to end over HTTP: a valid personal access token
//! reaches a landed authoring command with trusted principal and role context;
//! absent, forged, expired, revoked, cross-project, and client-injected identity
//! all refuse before the command runs and leave no side effect; and two
//! principals running the same command stay distinguishable in the append-only
//! ledger.
//!
//! The recipe in `docs/build-and-test.md` supplies one disposable database.

use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_postgres::{Client, NoTls};

use wamn_platform_identity::{
    IssuedPat, PAT_TOKEN_PREFIX, assign_project_role, create_human, create_service, issue_pat,
    resolve_subject, revoke_pat,
};
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");

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
const SECRET: &[u8] = b"correct horse battery staple";
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
    let mut stream = TcpStream::connect(BIND).await.expect("reach the surface");
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {BIND}\r\nConnection: close\r\n\
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
                    "definition": DRAFT_GRAPH,
                }
            }
        }
    })
    .to_string()
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
    for record in [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL] {
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
    let principal = create_human(admin, subject, subject, SECRET).await?;
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

async fn draft_count(admin: &Client) -> i64 {
    admin
        .query_one("SELECT count(*) FROM catalog.flow_drafts", &[])
        .await
        .expect("count drafts")
        .get(0)
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
    let roleless = create_human(&admin, "roleless@example.com", "Roleless", SECRET)
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
            login_token_ttl_secs: 3600,
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
    // which is a product refusal — it still attributes.
    assert_eq!(by_bob.status, 200, "{}", by_bob.body);

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

    // ---- the reserved login route implements the ctc8.7 wire contract -------
    let logged_in = post(
        "/login",
        None,
        &[],
        r#"{"subject":"alice@example.com","secret":"correct horse battery staple","label":"laptop"}"#,
    )
    .await;
    assert_eq!(logged_in.status, 200, "{}", logged_in.body);
    let minted: serde_json::Value =
        serde_json::from_str(&logged_in.body).expect("login returns a document");
    let token = minted["token"].as_str().expect("a token field");
    assert!(token.starts_with(PAT_TOKEN_PREFIX));
    assert!(
        minted["expires_at"]
            .as_str()
            .expect("an expires_at field")
            .ends_with('Z')
    );
    assert_eq!(minted.as_object().expect("an object").len(), 2);

    // The freshly minted token works, proving login and authorization agree.
    let with_minted = post(
        "/authoring",
        Some(token),
        &[],
        &save_document("save-5", PROJECT, 0, "draft-minted"),
    )
    .await;
    assert_eq!(with_minted.status, 200, "{}", with_minted.body);

    // Every login refusal is the same document as every token refusal.
    for bad in [
        r#"{"subject":"alice@example.com","secret":"wrong","label":"laptop"}"#,
        r#"{"subject":"nobody@example.com","secret":"whatever","label":"laptop"}"#,
        r#"{"subject":"ci-runner","secret":"whatever","label":"laptop"}"#,
        r#"{"subject":"not a subject","secret":"whatever","label":"laptop"}"#,
    ] {
        let refused = post("/login", None, &[], bad).await;
        assert_eq!(refused.status, 403, "{bad}");
        assert_eq!(refused.body, AUTHORIZATION_DENIED, "{bad}");
    }

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

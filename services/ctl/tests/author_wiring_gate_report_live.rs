//! Live proof that authorship needs a GREEN gate report for its own hash.
//!
//! Two disposable PostgreSQL 18 databases are required, because the fact under
//! test is not co-resident with the row it guards: `catalog.wirings` is a
//! PROJECT-plane relation and `wamn_run.gate_reports` a CONTROL-plane one
//! (wamn-0h0g.8.5.6 removed the wiring row's report column). Set
//! `WAMN_AUTHOR_WIRING_PROJECT_PG_URL` and `WAMN_AUTHOR_WIRING_CONTROL_PG_URL`;
//! this test drops and recreates the schemas it owns in each.
//!
//! Both stores are provisioned from the production SQL artifacts —
//! `ensure_catalog_storage` and `CONTROL_BOOTSTRAP_SQL` — so the report row the
//! verb reads is the deployed relation, not a fixture shaped like it.

use std::collections::BTreeMap;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{DefinitionHash, WiringDocument, WiringNode, WiringTerminal};
use wamn_control_provision::CONTROL_BOOTSTRAP_SQL;
use wamn_ctl::author_wiring::{AuthorWiringErrorKind, AuthorWiringRequest, author_wiring};
use wamn_ctl::publish_catalog::ensure_catalog_storage;

const TENANT: &str = "gate-report-tenant";
const CATALOG: &str = "gate-report-catalog";
const CATALOG_VERSION: i32 = 3;
const ENVIRONMENT: &str = "prod";
const COMPONENT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const FACT_FINGERPRINT: &str =
    "sha256:6666666666666666666666666666666666666666666666666666666666666666";

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to a disposable database");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

fn wiring(id: &str, version: u32) -> WiringDocument {
    WiringDocument::new(
        id,
        version,
        "node",
        BTreeMap::from([(
            "node".to_string(),
            WiringNode {
                component: "http-request".to_string(),
                interface_version: "0.1".to_string(),
                operation: "call".to_string(),
                params: BTreeMap::new(),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
        Vec::new(),
    )
    .expect("fixture wiring is structurally valid")
}

/// Install the production catalog schema and the facts one wiring gates against.
async fn provision_project(project: &Client) {
    project
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .expect("reset the project catalog schema and prerequisite role");
    ensure_catalog_storage(project)
        .await
        .expect("install the production catalog schema");
    project
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the project seed session");
    project
        .execute(
            "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ($1, $2, $3, $4, '0.1', 'applied')",
            &[&TENANT, &CATALOG, &CATALOG_VERSION, &ENVIRONMENT],
        )
        .await
        .expect("seed the catalog version");
    project
        .execute(
            "INSERT INTO catalog.component_library \
                   (tenant_id, catalog_id, catalog_version, component, interface_version, \
                    operation, component_digest, imports, imports_fingerprint, effects, \
                    input_ports, output_ports, parameters) \
             VALUES ($1, $2, $3, 'http-request', '0.1', 'call', $4, \
                     '[]'::jsonb, $5, '[]'::jsonb, '[]'::jsonb, '[]'::jsonb, '[]'::jsonb)",
            &[
                &TENANT,
                &CATALOG,
                &CATALOG_VERSION,
                &COMPONENT,
                &FACT_FINGERPRINT,
            ],
        )
        .await
        .expect("seed the admitted component fact every fixture wiring names");
}

/// Install the production control store the gate report is read from.
///
/// `wamn_run.gate_reports` is immutable, so the schemas are dropped rather than
/// emptied. `CONTROL_BOOTSTRAP_SQL`'s author-authority self-check requires that
/// `wamn_scenario_author` cannot reach this database, which is what the CONNECT
/// revoke buys.
async fn provision_control(control: &Client) {
    control
        .batch_execute(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_system', 'wamn_control_author', 'wamn_app'] \
               LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             DO $$ BEGIN \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', \
                              pg_catalog.current_database()); \
             END $$;",
        )
        .await
        .expect("reset the control store and its prerequisite roles");
    for stage in CONTROL_BOOTSTRAP_SQL {
        control
            .batch_execute(stage)
            .await
            .expect("install the production control bootstrap");
    }
    control
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the control seed session");
}

/// Record one gate verdict at exactly the hash the gate judged.
async fn record_report(control: &Client, wiring_hash: &DefinitionHash, passed: bool) {
    control
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, $3, '{}'::jsonb)",
            &[&TENANT, &wiring_hash.as_str(), &passed],
        )
        .await
        .expect("record the gate verdict");
}

/// Submit one document through the production authorship verb.
async fn author(
    control: &Client,
    project: &mut Client,
    document: &WiringDocument,
) -> Result<DefinitionHash, wamn_ctl::author_wiring::AuthorWiringError> {
    let transaction = project
        .transaction()
        .await
        .expect("open the wiring authorship transaction");
    let authored = author_wiring(
        control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            catalog_id: CATALOG,
            gated_catalog_version: CATALOG_VERSION,
            document,
        },
    )
    .await;
    match authored {
        Ok(hash) => {
            transaction
                .commit()
                .await
                .expect("commit the wiring authorship");
            Ok(hash)
        }
        Err(refusal) => {
            transaction
                .rollback()
                .await
                .expect("close the refused authorship");
            Err(refusal)
        }
    }
}

async fn stored_wirings(project: &Client, wiring_id: &str) -> i64 {
    project
        .query_one(
            "SELECT count(*) FROM catalog.wirings \
              WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = $3",
            &[&TENANT, &CATALOG, &wiring_id],
        )
        .await
        .expect("count the authored wiring rows")
        .get(0)
}

/// Four documents, four report states, one admitted row.
///
/// Each document is distinct, so each carries its own hash and its own report
/// key; nothing here depends on rewriting a report, which the relation's
/// immutability trigger forbids anyway.
#[tokio::test]
#[ignore = "requires two disposable PostgreSQL 18 databases in \
            WAMN_AUTHOR_WIRING_PROJECT_PG_URL and WAMN_AUTHOR_WIRING_CONTROL_PG_URL"]
async fn a_wiring_is_authored_only_under_a_green_report_for_its_own_hash() {
    let project_url = std::env::var("WAMN_AUTHOR_WIRING_PROJECT_PG_URL")
        .expect("WAMN_AUTHOR_WIRING_PROJECT_PG_URL names a disposable PostgreSQL 18 database");
    let control_url = std::env::var("WAMN_AUTHOR_WIRING_CONTROL_PG_URL")
        .expect("WAMN_AUTHOR_WIRING_CONTROL_PG_URL names a disposable PostgreSQL 18 database");
    let (mut project, project_task) = connect(&project_url).await;
    let (control, control_task) = connect(&control_url).await;
    provision_project(&project).await;
    provision_control(&control).await;

    // 1. Never gated. The control store holds no row under this hash at all.
    let ungated = wiring("ungated", 1);
    let refusal = author(&control, &mut project, &ungated)
        .await
        .expect_err("a document with no gate report refuses");
    assert_eq!(refusal.kind(), AuthorWiringErrorKind::Report);
    assert_eq!(stored_wirings(&project, "ungated").await, 0);

    // 2. Gated and REFUSED. The row exists under this document's own hash and
    // says the gate rejected it, which is not authority to author it.
    let rejected = wiring("rejected", 1);
    record_report(&control, &rejected.wiring_hash(), false).await;
    let refusal = author(&control, &mut project, &rejected)
        .await
        .expect_err("a document its gate refused refuses");
    assert_eq!(refusal.kind(), AuthorWiringErrorKind::Report);
    assert_eq!(stored_wirings(&project, "rejected").await, 0);

    // 3. A GREEN report exists — for other bytes. This is the arm the hash
    // binding exists for: the store is not empty, it simply holds nothing
    // under THIS document's hash, so the verdict does not transfer.
    let borrowed = wiring("borrowed", 1);
    let elsewhere = wiring("elsewhere", 1);
    assert_ne!(borrowed.wiring_hash(), elsewhere.wiring_hash());
    record_report(&control, &elsewhere.wiring_hash(), true).await;
    let refusal = author(&control, &mut project, &borrowed)
        .await
        .expect_err("another document's green report does not authorize this one");
    assert_eq!(refusal.kind(), AuthorWiringErrorKind::Report);
    assert_eq!(stored_wirings(&project, "borrowed").await, 0);

    // 4. A green report covering this document's own hash. Only now is the
    // definition appended, and the authored hash is the one that was gated.
    let admitted = wiring("admitted", 1);
    record_report(&control, &admitted.wiring_hash(), true).await;
    let authored = author(&control, &mut project, &admitted)
        .await
        .expect("a green report at the document's own hash authorizes authorship");
    assert_eq!(authored, admitted.wiring_hash());
    assert_eq!(stored_wirings(&project, "admitted").await, 1);
    let stored: String = project
        .query_one(
            "SELECT wiring_hash FROM catalog.wirings \
              WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = 'admitted'",
            &[&TENANT, &CATALOG],
        )
        .await
        .expect("read the authored wiring row")
        .get(0);
    assert_eq!(stored, admitted.wiring_hash().as_str());

    drop(project);
    drop(control);
    project_task.await.expect("join the project connection");
    control_task.await.expect("join the control connection");
}

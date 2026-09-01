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
//!
//! # This file SEEDS the gate verdict by admin SQL, and here is what that costs
//!
//! `wamn-0h0g.8.28`'s rider said the live tests stop seeding
//! `wamn_run.gate_reports` directly, because a seeded report is a false green
//! that proves the STEADY STATE and never the FIRST TRANSITION. `record_report`
//! below still seeds. `wamn-0h0g.8.29` measured the alternative and ruled this
//! the honest disposition rather than neglect; the measurement is recorded here
//! so it is not re-derived and the rider is not re-raised as an unfixed defect.
//!
//! **What is NOT wrong with it.** The dangerous half of the pattern — a
//! FABRICATED hash standing in for a derived one — is not present here. Every
//! report is keyed on `document.wiring_hash()`, derived by the same reader the
//! server uses from the same bytes the document carries. The sibling that DID
//! invent its candidate hash (`sha256:1c1c1c…`) was `management_live.rs`, and
//! that is where the false green actually lived; it is gone. This file seeds the
//! VERDICT, not the IDENTITY.
//!
//! **What this file therefore does NOT prove.** That the gate verb itself ever
//! writes a report — the ordering in which a judgment produces the row this test
//! then reads. Every arm below starts from a report row that exists because this
//! file inserted it.
//!
//! **The test that DOES prove it** is
//! `management_surface_authenticates_and_attributes_authoring_commands` in
//! `services/scenario-worker/tests/management_live.rs`. It drives the real gate
//! command over the management surface and asserts, at the moment a document
//! reaches an accepted judgment, that `catalog.wirings` holds ZERO rows — the
//! first transition, which no seeded fixture can reach — then reads the report
//! the gate wrote back out of `wamn_run.gate_reports` at the hash DERIVED from
//! the submitted bytes.
//!
//! **The measured cost of building it here instead.** The gate verb lives in
//! `wamn-scenario-worker`, and its writer `management::insert_gate_report` is
//! `pub(crate)`; the only public entry is `management::serve`, an HTTP server.
//! Taking the crate as a dev-dependency is the cheap part — measured with
//! `cargo tree -e normal`, it adds exactly two crates to `wamn-ctl`'s graph
//! (`wamn-scenario-worker` and `wamn-authoring-model`). The fixture is not. This
//! test would have to stand up `ManagementServeArgs`' four connection inputs,
//! THREE of them scoped A/B generation LOGINs whose digest-derived names are
//! validated before any I/O — a `wamn_identity_reader` generation, a
//! `wamn_control_author` generation and a `wamn_management_admitter` generation
//! — then provision the `identity` schema, admit a principal, mint it a project
//! role and a PAT, bind a listener, and POST the command with that bearer token,
//! across THREE databases rather than two. `management_live.rs` carries exactly
//! that: 812 lines of fixture ahead of its single `#[tokio::test]`, where this
//! whole file — fixture and all four arms — was 292 lines before this comment.
//! The one cheap route, making `insert_gate_report` reachable in-process, is an
//! edit to `services/scenario-worker/src/management.rs`.
//!
//! So the proof of the first transition stays where the gate verb lives, and
//! this file proves the other half: that AUTHORSHIP admits a document only under
//! a green report keyed to that document's own hash. Those are different claims,
//! and this one needs no gate run to make.

use std::collections::BTreeMap;
use std::path::Path;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageScope, DefinitionHash,
    WiringDocument, WiringNode, WiringTerminal,
};
use wamn_control_provision::CONTROL_BOOTSTRAP_SQL;
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{AuthorWiringErrorKind, AuthorWiringRequest, author_wiring};
use wamn_ctl::push_component::admitted_projection_hash;

const TENANT: &str = "gate-report-tenant";
const PACKAGE: &str = "wamn_receiving";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const FACT_FINGERPRINT: &str =
    "sha256:6666666666666666666666666666666666666666666666666666666666666666";
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");

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
                component: "receiving_data".to_string(),
                interface_version: "0.1".to_string(),
                operation: "wamn-receiving:purchase-order/get@1.0.0".to_string(),
                operation_dependency: None,
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
async fn provision_project(project: &Client, project_url: &str) {
    project
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$;",
        )
        .await
        .expect("reset the project catalog schema and prerequisite role");
    project
        .batch_execute(&format!("{CATALOG_SCHEMA_SQL}\n{APP_SCHEMA_SQL}"))
        .await
        .expect("install the production package and application schemas");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ctl crate lives under services/ctl");
    apply_package::run(ApplyPackageArgs {
        package: repository.join("packages/receiving"),
        database_url: project_url.to_string(),
        tenant: TENANT.to_string(),
    })
    .await
    .expect("apply the real Receiving package");
    project
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the project seed session");
    let component = AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: TENANT.to_owned(),
            package_id: PACKAGE.to_owned(),
            package_version: PACKAGE_VERSION.to_owned(),
        },
        component: "receiving_data".to_owned(),
        interface_version: "0.1".to_owned(),
        operations: BTreeMap::from([(
            "wamn-receiving:purchase-order/get@1.0.0".to_owned(),
            AdmittedComponentOperation {
                registered_operation: Some("wamn-receiving:purchase-order/get@1.0.0".to_owned()),
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
            },
        )]),
        component_digest: COMPONENT.to_owned(),
        imports: Vec::new(),
        imports_fingerprint: FACT_FINGERPRINT.to_owned(),
        effects: Vec::new(),
    };
    let projection_hash =
        admitted_projection_hash(&component, &[]).expect("hash admitted fixture projection");
    project
        .execute(
            "INSERT INTO catalog.component_library \
                   (tenant_id, package_id, package_version, component, interface_version, \
                    operations, component_digest, projection_hash, imports, \
                    imports_fingerprint, effects) \
             VALUES ($1, $2, $3, 'receiving_data', '0.1', \
                     '{\"wamn-receiving:purchase-order/get@1.0.0\":{\"registered-operation\":\"wamn-receiving:purchase-order/get@1.0.0\",\"input-ports\":[],\"output-ports\":[],\"parameters\":[]}}'::jsonb, \
                     $4, $5, '[]'::jsonb, $6, '[]'::jsonb)",
            &[
                &TENANT,
                &PACKAGE,
                &PACKAGE_VERSION,
                &COMPONENT,
                &projection_hash,
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
///
/// **THIS IS THE ADMIN-SQL SEEDING `wamn-0h0g.8.28`'s rider named, kept
/// deliberately (`wamn-0h0g.8.29`).** `wiring_hash` is always
/// `document.wiring_hash()` — derived from the bytes, never a literal — so what
/// is seeded is the VERDICT, not the IDENTITY. The consequence is that no arm
/// below proves the gate verb ever WRITES a report; the module header names the
/// test that does and the measured cost of moving that proof here.
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
            package_id: PACKAGE,
            package_version: PACKAGE_VERSION,
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
              WHERE tenant_id = $1 AND package_id = $2 AND wiring_id = $3",
            &[&TENANT, &PACKAGE, &wiring_id],
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
    provision_project(&project, &project_url).await;
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
              WHERE tenant_id = $1 AND package_id = $2 AND wiring_id = 'admitted'",
            &[&TENANT, &PACKAGE],
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

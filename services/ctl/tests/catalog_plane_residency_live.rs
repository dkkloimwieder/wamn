//! Live proof that `ensure_catalog_storage` refuses the CONTROL plane
//! (wamn-0h0g.12.180).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway PostgreSQL 18; skipped cleanly when unset.
//!
//! The hazard is a NAME. `ensure_catalog_storage` decides the baseline is
//! present by reading `to_regclass('catalog.catalogs')`, and that name lives in
//! BOTH planes — `deploy/sql/control-portable-store.sql:38` installs it beside
//! the project copy on purpose ("database residency, not a renamed schema,
//! distinguishes them"). A probe keyed on a shared name therefore cannot tell
//! the planes apart: pointed at a control store it answers "present", takes the
//! converge arm, and installs project-only connection/wiring/release-component
//! storage into the control database.
//!
//! So both stores here are built from the PRODUCTION artifacts — the control
//! plane through `CONTROL_PORTABLE_STORE_SQL`, the project plane through the
//! real `ensure_catalog_storage` — never a fixture shaped like one.
//!
//! Every arm asserts the POST-STATE, not just an exit status. An error string
//! alone cannot tell a refusal from a converge that happened to match nothing,
//! and the pre-refusal code already fails on a control database for an unrelated
//! reason (it reads `catalog.connection_requirements` present and its four
//! siblings absent, and calls that a "partially installed" project database) —
//! after mutating cluster-global role state. The role-membership witness below
//! is what separates "refused before any mutation" from "failed later".

mod support;

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
use wamn_ctl::publish_catalog::{CATALOG_PLANE_RESIDENCY_REFUSAL, ensure_catalog_storage};

const PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

/// The exact `catalog` inventory `control-portable-store.sql:617-627` asserts on
/// apply. Any project-only relation reaching this database changes this list.
const CONTROL_CATALOG_INVENTORY: [&str; 7] = [
    "authoring_command_audit",
    "catalog_heads",
    "catalogs",
    "component_library",
    "connection_requirements",
    "deployment_attestations",
    "releases",
];

/// Project-only catalog storage the converge arm installs. None of it may reach
/// the control plane.
const PROJECT_ONLY: [&str; 6] = [
    "catalog.connection_instances",
    "catalog.connection_generations",
    "catalog.connection_bindings",
    "catalog.connection_generation_retention",
    "catalog.wirings",
    "catalog.release_components",
];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Drop every schema either plane owns and re-harden the roles both need.
///
/// `control-portable-store.sql`'s own authority self-check refuses to apply
/// while `wamn_scenario_author` can CONNECT here, which is what the shared
/// PUBLIC-connect fixture buys.
async fn reset(client: &Client) {
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY \
                 ARRAY['wamn_system', 'wamn_control_author', 'wamn_app', 'wamn_scenario_author'] \
               LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             REVOKE wamn_scenario_author FROM wamn_app; \
             {PUBLIC_CONNECT_SQL} \
             DO $$ BEGIN \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_app', \
                              pg_catalog.current_database()); \
             END $$;"
        ))
        .await
        .expect("reset both planes' schemas and their prerequisite roles");
}

/// Hand the database back in the posture the sibling `WAMN_CTL_PG_URL` gates
/// expect: no control witness, and PUBLIC able to connect again.
async fn restore(client: &Client) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             REVOKE wamn_scenario_author FROM wamn_app; \
             DO $$ BEGIN \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO PUBLIC', \
                              pg_catalog.current_database()); \
             END $$;",
        )
        .await
        .expect("hand the disposable database back");
}

/// The tables a schema actually holds, read off the server.
async fn tables(client: &Client, schema: &str) -> Vec<String> {
    client
        .query(
            "SELECT tablename::text FROM pg_catalog.pg_tables \
             WHERE schemaname = $1 ORDER BY tablename",
            &[&schema],
        )
        .await
        .expect("read the schema inventory")
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

/// Whether `wamn_app` still holds the membership `ensure_wamn_app_role` revokes.
///
/// This is the first side effect of the wrong-plane path and it is
/// CLUSTER-GLOBAL, so a refusal raised after it could not take it back. It is
/// also the one witness a later failure cannot fake.
async fn scenario_author_membership(client: &Client) -> bool {
    client
        .query_one(
            "SELECT EXISTS ( \
               SELECT FROM pg_catalog.pg_auth_members member \
               JOIN pg_catalog.pg_roles granted ON granted.oid = member.roleid \
               JOIN pg_catalog.pg_roles holder ON holder.oid = member.member \
               WHERE granted.rolname = 'wamn_scenario_author' \
                 AND holder.rolname = 'wamn_app')",
            &[],
        )
        .await
        .expect("read the scenario-author membership")
        .get(0)
}

async fn present(client: &Client, relation: &str) -> bool {
    client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&relation])
        .await
        .expect("probe a relation")
        .get(0)
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn ensure_catalog_storage_refuses_control_plane_residency_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the catalog plane-residency gate");
        return;
    };
    let client = connect(&url).await;
    reset(&client).await;

    // ARM 1 — the CONTROL plane, installed from the production artifact.
    client
        .batch_execute(CONTROL_PORTABLE_STORE_SQL)
        .await
        .expect("install the production control portable store");
    client
        .batch_execute(
            "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
             GRANT wamn_scenario_author TO wamn_app",
        )
        .await
        .expect("plant the cluster-global witness the wrong-plane path would revoke");

    let baseline: bool = client
        .query_one("SELECT to_regclass('catalog.catalogs') IS NOT NULL", &[])
        .await
        .expect("read the shared-name baseline probe")
        .get(0);
    assert!(
        baseline,
        "the control store no longer installs catalog.catalogs, so this gate no \
         longer reproduces the shared-name hazard it exists to guard"
    );
    assert!(
        present(&client, "catalog.authoring_command_audit").await,
        "the control store must carry the witness the refusal reads"
    );

    let refusal = ensure_catalog_storage(&client)
        .await
        .expect_err("the control plane must be refused");
    let refusal = format!("{refusal:#}");
    assert!(
        refusal.contains(CATALOG_PLANE_RESIDENCY_REFUSAL),
        "the control plane was rejected for the wrong reason: {refusal}"
    );

    // POST-STATE. A refusal and a converge that matched nothing raise different
    // errors but would leave the same exit status, so assert what the server
    // holds instead.
    assert_eq!(
        tables(&client, "catalog").await,
        CONTROL_CATALOG_INVENTORY,
        "project catalog storage reached the control plane"
    );
    assert_eq!(
        tables(&client, "wamn_run").await,
        ["gate_reports"],
        "project run-plane storage reached the control plane"
    );
    for relation in PROJECT_ONLY {
        assert!(
            !present(&client, relation).await,
            "{relation} was installed into the control plane"
        );
    }
    assert!(
        scenario_author_membership(&client).await,
        "the refusal ran AFTER ensure_wamn_app_role: cluster-global role state \
         was already mutated on a database the verb then rejected"
    );

    // ARM 2 — the PROJECT plane, same connection, same verb. The refusal must
    // not cost the plane it was never meant to guard.
    reset(&client).await;
    ensure_catalog_storage(&client)
        .await
        .expect("a project database still installs");
    let fresh = tables(&client, "catalog").await;
    assert!(
        fresh.contains(&"wirings".to_string())
            && fresh.contains(&"release_components".to_string())
            && !fresh.contains(&"authoring_command_audit".to_string()),
        "the fresh install did not produce a project catalog: {fresh:?}"
    );

    // ARM 3 — the UPGRADE. A fresh install exercises no converge arm at all, so
    // retire the relations one delimited migration owns and prove the verb puts
    // exactly them back; then prove a further pass is a NO-OP rather than a
    // second exit-zero.
    // The block installs a FUNCTION alongside its two tables and creates it with
    // a bare `CREATE FUNCTION`, so retiring only the tables leaves a state the
    // migration cannot re-apply over. Retire the whole block.
    client
        .batch_execute(
            "DROP TABLE catalog.release_manifest_v2_snapshots, catalog.release_components; \
             DROP FUNCTION catalog.guard_release_component_insert()",
        )
        .await
        .expect("retire the release-component membership migration");
    assert_ne!(
        tables(&client, "catalog").await,
        fresh,
        "the retirement did not actually change the database"
    );
    ensure_catalog_storage(&client)
        .await
        .expect("the converge arm reinstalls the retired migration");
    assert_eq!(
        tables(&client, "catalog").await,
        fresh,
        "the converge did not restore exactly the retired relations"
    );
    ensure_catalog_storage(&client)
        .await
        .expect("the converge is idempotent");
    assert_eq!(
        tables(&client, "catalog").await,
        fresh,
        "the second pass over a converged project database was not a no-op"
    );

    restore(&client).await;
}

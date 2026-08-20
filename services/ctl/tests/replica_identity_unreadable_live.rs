//! Live gate for the unreadable-registrations refusal (wamn-0h0g.12.103).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! Postgres; skipped cleanly when unset. No `wal_level=logical` needed — the flip
//! sets a `pg_class` flag, and the non-retroactive WAL truth it protects is proven
//! by the l5i9.31 gate (`replica_identity_live`).
//!
//! **What this proves.** The RI reconciler derives "which entities need REPLICA
//! IDENTITY FULL" from `catalog.event_registrations`. An empty registration set
//! and an UNREADABLE one used to be the same value downstream — an empty slice —
//! so an unreadable set planned a reset to DEFAULT for every entity at FULL. The
//! flip is NON-RETROACTIVE, so that silently and permanently strips the old image
//! from every row event captured until the next repair, while the run reports as a
//! clean, idempotent-looking success. Three states, three behaviours:
//!
//! 1. **absent** (`catalog.event_registrations` dropped — a catalog schema
//!    dropped and partially rebuilt) → refuses `replica-identity-registrations-
//!    absent`, and the table already at FULL is still at FULL.
//! 2. **hidden by row-level security** — the table is `FORCE ROW LEVEL SECURITY`,
//!    so a session that does not BYPASS RLS reads ZERO ROWS WITH NO ERROR. This
//!    gate proves that silence exists live, then proves the reconcile refuses
//!    `replica-identity-registrations-unreadable` instead of consuming it.
//! 3. **present and genuinely empty** — a correctly-provisioned project-env with
//!    no registrations MUST still reconcile, including the legitimate reset
//!    direction. That is the second test, and it is what keeps the refusal precise
//!    rather than blunt.
//!
//! Hermetic: each test drops+recreates the `catalog` metadata schema and the data
//! schema in its preamble, and teardown leaves nothing behind.

use std::sync::LazyLock;

use tokio_postgres::{Client, NoTls};

use wamn_ctl::reconcile_replica_identity::reconcile;
use wamn_schema_compiler::Migration;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const DATA_SCHEMA: &str = "ri_unreadable_data";
const CATALOG_ID: &str = "riunreadable";

/// The registration storage SQL hard-codes the `catalog` schema and the absent
/// arm DROPs `catalog.event_registrations`, so the two gates in this binary must
/// not interleave — cargo runs a binary's tests in parallel threads by default.
static SERIALIZE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// entity `orders` -> table `sales_orders` (the FULL one that must survive every
/// refusal); entity `lines` -> `line_items` (the bystander).
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1", "catalog-id": "riunreadable", "version": 1,
  "entities": [
    { "id": "orders", "name": "sales_orders", "fields": [
      { "id": "status", "name": "status", "type": { "kind": "text" } } ] },
    { "id": "lines", "name": "line_items", "fields": [
      { "id": "qty", "name": "qty", "type": { "kind": "int" } } ] }
  ]
}"#;

fn catalog() -> wamn_schema_model::Catalog {
    wamn_schema_model::Catalog::from_json(CATALOG_JSON).expect("catalog parses")
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// `pg_class.relreplident` for a table in the data schema ('d'/'f'/'n'/'i').
async fn relreplident(su: &Client, table: &str) -> String {
    su.query_one(
        "SELECT c.relreplident::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2",
        &[&DATA_SCHEMA, &table],
    )
    .await
    .expect("read relreplident")
    .get(0)
}

/// Seed the one registration that requires FULL on `orders`: a delete
/// subscription (delete tenant-scoping needs the old image).
async fn seed_delete_registration(su: &Client) {
    let doc = format!(
        r#"{{"schema-version":"0.1","registration-id":"r-del","catalog-id":"{CATALOG_ID}",
           "flow-id":"purge","entity":"orders","ops":["delete"],"condition":null}}"#
    );
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ('t1', $1, 'r-del', 'purge', 'orders', $2::text::jsonb)",
        &[&CATALOG_ID, &doc],
    )
    .await
    .expect("seed the delete registration");
}

/// Drop and rebuild the `catalog` metadata schema from the REAL storage artifact,
/// plus the data schema and its REAL 3.2 floor. `wamn_app` (not owner, no
/// BYPASSRLS) is the non-bypassing identity the hidden arm reads as.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app'; END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') \
           THEN CREATE ROLE wamn_scenario_author NOLOGIN; END IF; END $$;"
    ))
    .await
    .expect("reset schemas + ensure the catalog-schema.sql roles");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply deploy/sql/catalog-schema.sql");
    let floor = Migration::create(&catalog())
        .expect("floor compile")
        .sql()
        .expect("floor sql");
    su.batch_execute(&format!(
        "CREATE SCHEMA {DATA_SCHEMA}; SET search_path TO {DATA_SCHEMA}; {floor}"
    ))
    .await
    .expect("apply the 3.2 floor");
    su.batch_execute("SET search_path TO public")
        .await
        .expect("reset search_path");
}

async fn teardown(su: &Client) {
    su.batch_execute(&format!(
        "RESET ROLE; RESET row_security; \
         DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

/// How many of this catalog's registrations the CURRENT session can see. Read
/// directly (not through the reconciler) so the RLS silence is observable on its
/// own, before any refusal is involved.
async fn visible_registrations(client: &Client) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM catalog.event_registrations WHERE catalog_id = $1",
            &[&CATALOG_ID],
        )
        .await
        .expect("count registrations")
        .get(0)
}

/// The session's `row_security` GUC — the read flips it off and must put it back.
async fn show_row_security(client: &Client) -> String {
    client
        .query_one("SHOW row_security", &[])
        .await
        .expect("show row_security")
        .get(0)
}

/// Bring `sales_orders` to REPLICA IDENTITY FULL through the real reconcile path.
/// Every refusal below is measured against this state: FULL is the thing an
/// unreadable registration set used to silently and permanently destroy.
async fn reconcile_to_full(su: &Client) {
    seed_delete_registration(su).await;
    reconcile(su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect("the provisioned reconcile flips orders to FULL");
    assert_eq!(
        relreplident(su, "sales_orders").await,
        "f",
        "precondition: orders is at FULL"
    );
}

#[tokio::test]
async fn an_absent_or_rls_hidden_registration_set_refuses_by_name_and_flips_nothing() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.103 refusal gate");
        return;
    };
    let _serialized = SERIALIZE.lock().await;
    let su = connect(&url).await;
    reset(&su).await;
    reconcile_to_full(&su).await;

    // --- 1. ABSENT: the catalog schema was dropped and only partially rebuilt. ---
    su.batch_execute("DROP TABLE catalog.event_registrations")
        .await
        .expect("drop the registration table");

    // The DRY RUN refuses too: the point is that the PLAN is never computed from a
    // registration set nobody could read, not merely that the ALTERs are skipped.
    let err = reconcile(&su, &catalog(), DATA_SCHEMA, false)
        .await
        .expect_err("an absent registration table must refuse, not plan a reset");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("replica-identity-registrations-absent"),
        "refuses by name: {msg}"
    );
    let err = reconcile(&su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect_err("an absent registration table must refuse on apply too");
    assert!(
        format!("{err:#}").contains("replica-identity-registrations-absent"),
        "refuses by name on apply: {err:#}"
    );
    assert_eq!(
        relreplident(&su, "sales_orders").await,
        "f",
        "no REPLICA IDENTITY FULL table is flipped as a side effect of an absent registration set"
    );

    // --- 2. HIDDEN BY ROW-LEVEL SECURITY. Rebuild the registration storage from ---
    // the real artifact and re-seed, then read it as a non-bypassing identity.
    su.batch_execute("DROP SCHEMA catalog CASCADE")
        .await
        .expect("drop catalog for the rebuild");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("rebuild deploy/sql/catalog-schema.sql");
    seed_delete_registration(&su).await;

    // THE SILENCE, PROVEN LIVE: the superuser sees the registration; `wamn_app`
    // (not owner, no BYPASSRLS) sees ZERO ROWS and NO ERROR, because the table is
    // FORCE ROW LEVEL SECURITY and no `app.tenant` claim is injected. That zero is
    // what the reconciler used to consume as "no entity needs FULL".
    assert_eq!(
        visible_registrations(&su).await,
        1,
        "the superuser sees the registration"
    );
    su.batch_execute("SET ROLE wamn_app")
        .await
        .expect("assume the non-bypassing identity");
    assert_eq!(
        visible_registrations(&su).await,
        0,
        "FORCE RLS hides the registration from a non-bypassing role with NO error — \
         the silent state this bead closes"
    );

    // The reconcile must refuse that silence rather than plan a reset from it —
    // on the dry run (no plan is computed) and on apply (no effect).
    let err = reconcile(&su, &catalog(), DATA_SCHEMA, false)
        .await
        .expect_err("an RLS-hidden registration set must refuse, not plan a reset");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("replica-identity-registrations-unreadable"),
        "refuses by name: {msg}"
    );
    let err = reconcile(&su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect_err("an RLS-hidden registration set must refuse on apply too");
    assert!(
        format!("{err:#}").contains("replica-identity-registrations-unreadable"),
        "refuses by name on apply: {err:#}"
    );
    su.batch_execute("RESET ROLE")
        .await
        .expect("drop back to the superuser");
    assert_eq!(
        relreplident(&su, "sales_orders").await,
        "f",
        "no REPLICA IDENTITY FULL table is flipped as a side effect of an RLS-hidden \
         registration set"
    );
    // The refusal leaves no session state behind: the read's `row_security = off`
    // is restored even on the failing path, so a shared connection is not silently
    // left with RLS errors armed for whatever the caller runs next.
    assert_eq!(
        show_row_security(&su).await,
        "on",
        "the refusal restores row_security"
    );

    teardown(&su).await;
}

/// The precision arm the acceptance names: the refusal must not swallow the
/// legitimate empty case. A correctly-provisioned project-env whose registration
/// table is PRESENT and genuinely holds no rows still reconciles — including the
/// reset-to-DEFAULT direction, which is exactly the direction the refusal above
/// blocks when the set is unreadable. A reconciler that refused everything would
/// pass the refusal test and fail this one.
#[tokio::test]
async fn a_provisioned_environment_with_no_registrations_still_reconciles() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.103 precision gate");
        return;
    };
    let _serialized = SERIALIZE.lock().await;
    let su = connect(&url).await;
    reset(&su).await;
    reconcile_to_full(&su).await;

    // The registration is removed through the API-shaped path: the table stays
    // provisioned and readable, it just holds nothing for this catalog.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = $1",
        &[&CATALOG_ID],
    )
    .await
    .expect("delete the registration");

    let plan = reconcile(&su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect("a provisioned, genuinely empty registration set still reconciles");
    let back = plan
        .flips
        .iter()
        .find(|f| f.table == "sales_orders")
        .expect("the legitimate reset is still planned");
    assert_eq!(back.to, wamn_schema_control::ReplicaIdentity::Default);
    assert_eq!(
        relreplident(&su, "sales_orders").await,
        "d",
        "the legitimate reset direction still applies"
    );

    // And it is still idempotent at the target state.
    let plan = reconcile(&su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect("reconcile at target still succeeds");
    assert!(plan.is_noop(), "reconcile at target flips nothing");

    // The reusable caller shape (EVT-RI-ORCH): if reconcile runs inside an open
    // operator transaction, the read's `row_security = off` must survive being
    // set and reset there and must not leak past the COMMIT. The live automatic
    // caller, `migrate-catalog`, runs it after commit instead.
    su.batch_execute("BEGIN").await.expect("open a caller tx");
    reconcile(&su, &catalog(), DATA_SCHEMA, true)
        .await
        .expect("the reconcile still works inside a caller's open transaction");
    assert_eq!(
        show_row_security(&su).await,
        "on",
        "row_security is restored inside the caller's transaction"
    );
    su.batch_execute("COMMIT").await.expect("commit");
    assert_eq!(
        show_row_security(&su).await,
        "on",
        "and no session state leaks past the commit"
    );

    teardown(&su).await;
}

//! Live-apply gate for `reconcile-run-plane` (E4/R14-migration, wamn-1wdq): the
//! durable migration path for provisioned run-plane schemas, proven against a
//! REAL Postgres in every starting state the bead's manifestations recorded.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway Postgres (recipe: docs/build-and-test.md [RUN-PLANE-RECONCILE]);
//! skipped cleanly when unset. Four legs, sequential under one test entry
//! (they share the `catalog` schema and the `wamn_app` role):
//!
//! - **v1-era drifted** (manifestations 1 + 4): a queue schema predating E4
//!   `stream_seq` / D20 `partition_policy` / fqg.20 `partition_owner` / v8cv
//!   `run_dead_letters`, with the pre-E4 claimable index, outbox-era tables +
//!   the `wamn_outbox_event` trigger/function, and a stored registration still
//!   carrying the legacy `state` key. The verb (driven through the REAL CLI
//!   `run` path) adds the columns (defaults land on existing rows), recreates
//!   the claimable index WITH `stream_seq`, creates the missing tables, drops
//!   every outbox-era object, strips the `state` key — and a re-run is a no-op.
//! - **queue-missing** (manifestation 2, the live poc_f1 case): run-state +
//!   flows present, queue absent → exactly the three queue tables appear, FKs
//!   resolve, and `run_dead_letters` keeps its append-only grant shape.
//! - **from-zero** (manifestations 3 + 5 + 6, the ephemeral-fixture wipe): a
//!   bare database without even the `wamn_app` role. `--dry-run` first, proven
//!   STRICTLY read-only; then the apply provisions everything — run plane +
//!   `catalog` schema — and a functional smoke as `wamn_app` proves the
//!   sections' grants + RLS isolation end-to-end.
//! - **current = no-op**: a schema at the schema of record plans NOTHING, in
//!   both dry-run and apply mode (the idempotence contract).
//! - **fail_kind CHECK drift** (wamn-fqg.16): a schema whose `runs.fail_kind`
//!   CHECK predates cjv.4's `'runaway-budget'` literal REJECTS a runaway
//!   `mark_failed` UPDATE. The verb drops the observed CHECK and re-adds the
//!   4-literal record form; the runaway UPDATE then succeeds and a re-run is a
//!   no-op (the reconciled CHECK converges with fresh provisioning).

use tokio_postgres::{Client, NoTls};

use wamn_ctl::reconcile_run_plane::{self, ReconcileRunPlaneArgs};
use wamn_migrate::{RunPlaneActionKind, rewrite_schema};

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const SCHEMA: &str = "rp_live";

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// Hermetic reset: drop the target schema + the shared `catalog` schema and
/// ensure the `wamn_app` role, so every leg builds its own starting state.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
         DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOBYPASSRLS; \
         END IF; END $$;"
    ))
    .await
    .expect("hermetic reset");
}

async fn table_exists(su: &Client, schema: &str, table: &str) -> bool {
    su.query_one(
        "SELECT EXISTS ( SELECT FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = $2 )",
        &[&schema, &table],
    )
    .await
    .expect("probe table")
    .get(0)
}

async fn column_exists(su: &Client, table: &str, column: &str) -> bool {
    su.query_one(
        "SELECT EXISTS ( SELECT FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 AND column_name = $3 )",
        &[&SCHEMA, &table, &column],
    )
    .await
    .expect("probe column")
    .get(0)
}

async fn indexdef(su: &Client, name: &str) -> Option<String> {
    su.query_opt(
        "SELECT indexdef FROM pg_indexes WHERE schemaname = $1 AND indexname = $2",
        &[&SCHEMA, &name],
    )
    .await
    .expect("read indexdef")
    .map(|r| r.get(0))
}

#[tokio::test]
async fn run_plane_reconcile_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-1wdq run-plane gate");
        return;
    };
    let su = connect(&url).await;
    v1_era_drifted_leg(&su, &url).await;
    queue_missing_leg(&su).await;
    from_zero_leg(&su).await;
    current_noop_leg(&su).await;
    fail_kind_check_drift_leg(&su).await;
}

/// Manifestations 1 + 4: the 2jkm.41-sweep drift set plus the outbox era.
async fn v1_era_drifted_leg(su: &Client, url: &str) {
    reset(su).await;

    // Current-era runs/node_runs/flows (the drift was queue-side)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, SCHEMA))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, SCHEMA))
        .await
        .expect("apply flows");
    // …and the v1-era queue: no stream_seq / partition_policy, the pre-E4
    // claimable index, no partition_owner / run_dead_letters, plus the
    // outbox-era tables, trigger + function on a floor table, and a stored
    // registration carrying the legacy `state` key.
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.run_queue ( \
             tenant_id text NOT NULL CHECK (tenant_id <> ''), \
             run_id text NOT NULL, \
             partition_key text, \
             priority int NOT NULL DEFAULT 0, \
             available_at timestamptz NOT NULL DEFAULT now(), \
             lease_owner text, \
             lease_expires_at timestamptz, \
             attempts int NOT NULL DEFAULT 0, \
             max_attempts int NOT NULL DEFAULT 20, \
             enqueued_at timestamptz NOT NULL DEFAULT now(), \
             PRIMARY KEY (tenant_id, run_id), \
             FOREIGN KEY (tenant_id, run_id) REFERENCES {SCHEMA}.runs (tenant_id, run_id) ON DELETE CASCADE); \
         CREATE INDEX run_queue_claimable ON {SCHEMA}.run_queue (tenant_id, available_at, lease_expires_at); \
         ALTER TABLE {SCHEMA}.run_queue ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {SCHEMA}.run_queue FORCE ROW LEVEL SECURITY; \
         CREATE POLICY run_queue_tenant ON {SCHEMA}.run_queue \
             USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
             WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), '')); \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {SCHEMA}.run_queue TO wamn_app; \
         CREATE TABLE {SCHEMA}.outbox ( \
             id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
             tenant_id text NOT NULL, event text NOT NULL, payload jsonb, \
             held_since timestamptz); \
         CREATE TABLE {SCHEMA}.evt_shadow ( \
             tenant_id text NOT NULL, registration_id text NOT NULL, \
             stream_seq bigint NOT NULL, \
             PRIMARY KEY (tenant_id, registration_id, stream_seq)); \
         CREATE TABLE {SCHEMA}.receipts ( \
             id uuid PRIMARY KEY DEFAULT gen_random_uuid(), tenant_id text NOT NULL); \
         CREATE FUNCTION {SCHEMA}.wamn_outbox_event() RETURNS trigger \
             LANGUAGE plpgsql AS $f$ BEGIN RETURN NEW; END $f$; \
         CREATE TRIGGER wamn_outbox_event AFTER INSERT OR UPDATE OR DELETE \
             ON {SCHEMA}.receipts FOR EACH ROW EXECUTE FUNCTION {SCHEMA}.wamn_outbox_event();"
    ))
    .await
    .expect("build the v1-era queue + outbox era");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ('t1', 'cat', 'r1', 'f', 'e', \
                 $1::text::jsonb)",
        &[&r#"{"registration-id":"r1","state":"shadow"}"#],
    )
    .await
    .expect("seed a legacy state-carrying registration");
    // A pre-existing queue row: the ADD COLUMN defaults must land on it.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version) \
             VALUES ('t1', 'r-old', 'f', 1); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r-old');"
    ))
    .await
    .expect("seed a pre-drift queue row");

    // The REAL CLI path (arg validation + connect + apply + print).
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        admin_database_url: url.to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane applies");

    // Column drift closed — and the defaults landed on the PRE-EXISTING row.
    assert!(
        column_exists(su, "run_queue", "stream_seq").await,
        "stream_seq added"
    );
    assert!(
        column_exists(su, "run_queue", "partition_policy").await,
        "partition_policy added"
    );
    let row = su
        .query_one(
            &format!(
                "SELECT stream_seq, partition_policy FROM {SCHEMA}.run_queue \
                 WHERE tenant_id = 't1' AND run_id = 'r-old'"
            ),
            &[],
        )
        .await
        .expect("read the pre-drift row");
    assert_eq!(row.get::<_, i64>(0), 0, "stream_seq default backfilled");
    assert_eq!(
        row.get::<_, String>(1),
        "blocking",
        "partition_policy default backfilled"
    );

    // The claimable index was RECREATED with the stream_seq prefix (M2).
    let def = indexdef(su, "run_queue_claimable")
        .await
        .expect("claimable index present");
    assert!(
        def.contains("stream_seq"),
        "claimable index recreated with stream_seq: {def}"
    );
    assert!(
        indexdef(su, "run_queue_partition").await.is_some(),
        "partition index created"
    );

    // Missing tables created.
    assert!(table_exists(su, SCHEMA, "partition_owner").await);
    assert!(table_exists(su, SCHEMA, "run_dead_letters").await);

    // The outbox era is gone: tables, trigger, function.
    assert!(!table_exists(su, SCHEMA, "outbox").await, "outbox dropped");
    assert!(
        !table_exists(su, SCHEMA, "evt_shadow").await,
        "evt_shadow dropped"
    );
    let triggers: i64 = su
        .query_one(
            "SELECT count(*) FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND t.tgname = 'wamn_outbox_event'",
            &[&SCHEMA],
        )
        .await
        .expect("count triggers")
        .get(0);
    assert_eq!(triggers, 0, "legacy trigger dropped");
    let funcs: i64 = su
        .query_one(
            "SELECT count(*) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = $1 AND p.proname = 'wamn_outbox_event'",
            &[&SCHEMA],
        )
        .await
        .expect("count functions")
        .get(0);
    assert_eq!(funcs, 0, "legacy function dropped");
    // The floor table the trigger sat on is untouched.
    assert!(
        table_exists(su, SCHEMA, "receipts").await,
        "floor table left alone"
    );

    // The legacy `state` key is stripped; the registration row survives.
    let (state_rows, reg_id): (i64, String) = {
        let r = su
            .query_one(
                "SELECT (SELECT count(*) FROM catalog.event_registrations \
                          WHERE registration ? 'state'), \
                        (SELECT registration->>'registration-id' \
                          FROM catalog.event_registrations \
                          WHERE registration_id = 'r1')",
                &[],
            )
            .await
            .expect("read registrations");
        (r.get(0), r.get(1))
    };
    assert_eq!(state_rows, 0, "legacy state keys stripped");
    assert_eq!(reg_id, "r1", "registration document otherwise intact");

    // Idempotence: a second reconcile plans nothing.
    let again = reconcile_run_plane::reconcile(su, SCHEMA, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

/// Manifestation 2 (the live poc_f1 case): run-state + flows present, queue
/// wholly absent — the three queue tables appear (M3), FKs resolve, and the
/// dead-letter ledger keeps its append-only grant shape.
async fn queue_missing_leg(su: &Client) {
    reset(su).await;
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, SCHEMA))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, SCHEMA))
        .await
        .expect("apply flows");

    let plan = reconcile_run_plane::reconcile(su, SCHEMA, true)
        .await
        .expect("reconcile applies");
    assert!(!plan.is_noop());

    // Queue tables exist after reconcile (M3).
    for t in ["run_queue", "partition_owner", "run_dead_letters"] {
        assert!(
            table_exists(su, SCHEMA, t).await,
            "queue table {t} exists after reconcile"
        );
    }
    // The FK to runs resolves: a run then its queue row insert cleanly.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version) \
             VALUES ('t1', 'r1', 'f', 1); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("FK insert path");

    // v8cv: run_dead_letters is APPEND-ONLY from the app role.
    let mut privs: Vec<String> = su
        .query(
            "SELECT privilege_type FROM information_schema.role_table_grants \
             WHERE grantee = 'wamn_app' AND table_schema = $1 AND table_name = 'run_dead_letters'",
            &[&SCHEMA],
        )
        .await
        .expect("read grants")
        .iter()
        .map(|r| r.get(0))
        .collect();
    privs.sort();
    assert_eq!(
        privs,
        ["INSERT", "SELECT"],
        "dead-letter ledger append-only grant"
    );
}

/// Manifestations 3 + 5 + 6 (the ephemeral-fixture wipe): a bare database —
/// not even the `wamn_app` role. Dry-run first (strictly read-only), then the
/// apply provisions run plane + `catalog`, and a functional smoke as
/// `wamn_app` proves grants + RLS isolation from the applied sections.
async fn from_zero_leg(su: &Client) {
    reset(su).await;
    su.batch_execute(
        "DROP OWNED BY wamn_app; \
         DROP ROLE wamn_app;",
    )
    .await
    .expect("remove the runtime role (bare database)");

    // --dry-run is STRICTLY read-only: it neither creates the role nor tables.
    let dry = reconcile_run_plane::reconcile(su, SCHEMA, false)
        .await
        .expect("dry-run plans");
    assert!(!dry.is_noop());
    let role_exists: bool = su
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app')",
            &[],
        )
        .await
        .expect("probe role")
        .get(0);
    assert!(!role_exists, "dry-run does not create the role");
    assert!(
        !table_exists(su, SCHEMA, "runs").await,
        "dry-run creates nothing"
    );

    let plan = reconcile_run_plane::reconcile(su, SCHEMA, true)
        .await
        .expect("from-zero reconcile applies");
    assert!(!plan.is_noop());

    for t in [
        "runs",
        "node_runs",
        "flows",
        "run_queue",
        "partition_owner",
        "run_dead_letters",
    ] {
        assert!(
            table_exists(su, SCHEMA, t).await,
            "run-plane table {t} provisioned"
        );
    }
    assert!(
        table_exists(su, "catalog", "event_registrations").await,
        "catalog schema provisioned"
    );

    // Functional smoke as the runtime role: the sections' grants + RLS hold.
    su.batch_execute(&format!(
        "SET ROLE wamn_app; \
         SELECT set_config('app.tenant', 't1', false); \
         INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version) \
             VALUES ('t1', 'r1', 'f', 1); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("wamn_app can write its tenant's run-plane rows");
    let visible: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .expect("tenant read")
        .get(0);
    assert_eq!(visible, 1, "own tenant sees its row");
    su.batch_execute("SELECT set_config('app.tenant', 't2', false)")
        .await
        .expect("switch tenant");
    let foreign: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .expect("foreign read")
        .get(0);
    assert_eq!(foreign, 0, "RLS isolates the foreign tenant");
    su.batch_execute("RESET ROLE; SELECT set_config('app.tenant', '', false)")
        .await
        .expect("drop back to superuser");
}

/// The idempotence contract: a schema AT the schema of record plans nothing —
/// dry-run and apply mode alike.
async fn current_noop_leg(su: &Client) {
    reset(su).await;
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, SCHEMA))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, SCHEMA))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(FLOW_TESTS_SQL, SCHEMA))
        .await
        .expect("apply flow-tests");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, SCHEMA))
        .await
        .expect("apply run-queue");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");

    let dry = reconcile_run_plane::reconcile(su, SCHEMA, false)
        .await
        .expect("dry-run plans");
    assert!(
        dry.is_noop(),
        "current schema dry-run is a no-op: {:#?}",
        dry.actions
    );
    assert_eq!(dry.at_target.len(), 9, "all nine run-plane tables at target");

    let apply = reconcile_run_plane::reconcile(su, SCHEMA, true)
        .await
        .expect("apply-mode reconcile");
    assert!(
        apply.is_noop(),
        "current schema apply is a no-op: {:#?}",
        apply.actions
    );
}

/// wamn-fqg.16: a schema whose `runs.fail_kind` CHECK predates cjv.4's
/// `'runaway-budget'` literal rejects a runaway `mark_failed` UPDATE — the
/// verdict is lost from the audit row. The reconcile drops the observed CHECK
/// and re-adds the 4-literal record form; the runaway UPDATE then succeeds, the
/// canonical def carries `runaway-budget`, and a re-run is a no-op.
async fn fail_kind_check_drift_leg(su: &Client) {
    reset(su).await;
    // Provision the CURRENT run plane (fresh 4-literal fail_kind CHECK)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, SCHEMA))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, SCHEMA))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, SCHEMA))
        .await
        .expect("apply run-queue");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    // …then REGRESS runs.fail_kind to the pre-cjv.4 3-literal CHECK (drop the
    // fresh auto-named one, re-add without 'runaway-budget') — the exact state a
    // schema provisioned from the old run-state.sql carries.
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs DROP CONSTRAINT runs_fail_kind_check; \
         ALTER TABLE {SCHEMA}.runs ADD CONSTRAINT runs_fail_kind_check \
             CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input'));"
    ))
    .await
    .expect("regress fail_kind CHECK to the legacy 3 literals");
    // A run whose runaway verdict we will try to record.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version) \
             VALUES ('t1', 'r-budget', 'f', 1);"
    ))
    .await
    .expect("seed a run");
    // Under the legacy CHECK the runaway verdict is REJECTED (the fqg.16 bug).
    let rejected = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET fail_kind = 'runaway-budget' \
                 WHERE tenant_id = 't1' AND run_id = 'r-budget'"
            ),
            &[],
        )
        .await;
    assert!(
        rejected.is_err(),
        "legacy 3-literal CHECK rejects the runaway verdict"
    );

    // Reconcile: exactly the fail_kind CHECK repair is planned + applied.
    let plan = reconcile_run_plane::reconcile(su, SCHEMA, true)
        .await
        .expect("reconcile applies");
    assert!(
        plan.actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck),
        "the fail_kind CHECK repair is planned: {:#?}",
        plan.actions
    );

    // (i) the canonical constraint def now admits 'runaway-budget'.
    let def: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'runs' \
               AND con.conname = 'runs_fail_kind_check'",
            &[&SCHEMA],
        )
        .await
        .expect("read fail_kind constraintdef")
        .get(0);
    assert!(
        def.contains("runaway-budget"),
        "reconciled CHECK admits runaway-budget: {def}"
    );

    // (ii) the runaway `mark_failed` UPDATE now SUCCEEDS — the verdict lands.
    let updated = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET fail_kind = 'runaway-budget' \
                 WHERE tenant_id = 't1' AND run_id = 'r-budget'"
            ),
            &[],
        )
        .await
        .expect("runaway verdict now accepted");
    assert_eq!(updated, 1, "the runaway verdict lands on the audit row");

    // (iii) a second reconcile plans nothing (idempotence + convergence).
    let again = reconcile_run_plane::reconcile(su, SCHEMA, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

//! Live-apply gate for `reconcile-run-plane` (E4/R14-migration, wamn-1wdq): the
//! durable migration path for provisioned run-plane schemas, proven against a
//! REAL Postgres in every starting state the bead's manifestations recorded.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway Postgres (recipe: docs/archive/build-and-test.md [RUN-PLANE-RECONCILE]);
//! skipped cleanly when unset. The legs run sequentially under the main test
//! entry (they share the `catalog` schema and the `wamn_app` role); the
//! execution-pin cutover has one separate test entry:
//!
//! - **shared-runner legacy** (wamn-l5i9.73): the deployed fixture's old
//!   runs/node_runs/run_queue shape gains canonical admission/causation
//!   columns, CHECKs, helper functions, and lineage trigger without losing its
//!   compatible history. The materializer catalog-head lock and immutable
//!   lineage are exercised, then a second reconcile is a no-op.
//! - **effect frame + writer cutovers** (wamn-0h0g.4.13/.4.9): incompatible
//!   populated immutable ledgers refuse before DDL. Empty ledgers converge to
//!   frame-keyed, coordinate-bound attempt/dispatch/outcome facts while retired
//!   mutable node projection columns are removed without fabricating history.
//! - **forced-RLS owner refusal**: a plain table owner cannot observe hidden
//!   tenant rows, so dry-run and apply both refuse before the pointer or ledger
//!   schema can be activated.
//! - **partition-plane cutover** (wamn-0h0g.4.1): a populated but unleased
//!   legacy queue keeps its retained row while the partition columns, owner
//!   table, dead-letter table, CHECK, and partial index are removed under fixed
//!   locks. Active leases and nonempty dead-letter history refuse before any
//!   schema or unrelated authority mutation. Partial legacy state converges,
//!   the global FIFO claim index lands exactly, and a second pass is a no-op.
//! - **v1-era drifted** (manifestations 1 + 4): a partially converged queue
//!   predating E4 `stream_seq`, with the pre-E4 claimable index, outbox-era
//!   tables + the `wamn_outbox_event` trigger/function, and a stored
//!   registration carrying both retired declaration keys. The real CLI path
//!   adds retained columns, removes remaining partition residue and outbox
//!   objects, strips only the retired registration keys, and is idempotent.
//! - **queue-missing** (manifestation 2, the live poc_f1 case): run-state +
//!   flows present, queue absent → the one global FIFO queue appears and its FK
//!   resolves.
//! - **from-zero** (manifestations 3 + 5 + 6, the ephemeral-fixture wipe): a
//!   bare database without even the `wamn_app` role. `--dry-run` first, proven
//!   STRICTLY read-only; then the apply provisions everything — run plane +
//!   `catalog` schema — and a functional smoke as `wamn_app` proves the
//!   sections' grants + RLS isolation end-to-end.
//! - **invocation retention cutover**: the legacy admission expiry column/index
//!   are removed and the client-key carrier becomes optional; a second pass is
//!   a no-op.
//! - **rerun-lineage cutover**: populated runs retain their payload and trusted
//!   event causation while only `replay_of`, `root_run_id`, and the exact
//!   `runs_root` index disappear. A same-name foreign index refuses atomically.
//! - **stored-test cutover**: all five retired tables and both helper functions
//!   are removed child first while the four authoring-test relations survive;
//!   the obsolete validation dimension and command kinds converge only when
//!   no immutable legacy identity/evidence would be rewritten; a second pass is
//!   a no-op.
//! - **current = no-op**: a schema at the schema of record plans NOTHING, in
//!   both dry-run and apply mode (the idempotence contract).
//! - **authoring additive upgrade + authority repair**: the pre-6A catalog gains
//!   draft/grant storage and the run plane gains authoring-test storage; stale
//!   guest grants and membership are removed; the owner-seeded draft-safe
//!   relation is SELECT-only to management; and guest/release-write refusals
//!   are exercised.
//! - **catalog-head lock concurrency**: both runtime and author admission call
//!   the tenant-checking SECURITY DEFINER bridge; its SHARE lock blocks the
//!   publisher's pointer UPDATE until admission commits.
//! - **retired effect-disposition cutover**: empty parent/child ledgers are
//!   locked and removed child-first; populated history refuses atomically with
//!   the exact archive-or-reprovision diagnostic.
//! - **fail_kind CHECK drift** (wamn-fqg.16): a schema whose `runs.fail_kind`
//!   CHECK predates cjv.4's `'runaway-budget'` literal REJECTS a runaway
//!   `mark_failed` UPDATE. The verb drops the observed CHECK and re-adds the
//!   5-literal record form; the runaway UPDATE then succeeds and a re-run is a
//!   no-op (the reconciled CHECK converges with fresh provisioning).

use tokio_postgres::{Client, NoTls};

use wamn_ctl::reconcile_run_plane::{self, ReconcileRunPlaneArgs};
use wamn_schema_control::{BareSchemaName, RunPlaneActionKind, rewrite_schema};

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const AUTHORING_TESTS_SQL: &str = include_str!("../../../deploy/sql/authoring-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const SCHEMA: &str = "rp_live";
const EMPTY_EXECUTION_BUNDLE_HASH: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn schema() -> BareSchemaName {
    BareSchemaName::new(SCHEMA).expect("live-test schema is valid")
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

async fn connect_as(url: &str, role: &str, password: &str) -> Client {
    let mut config: tokio_postgres::Config = url.parse().expect("parse Postgres URL");
    config.user(role).password(password);
    let (client, conn) = config.connect(NoTls).await.expect("connect as role");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

async fn seed_run_pin_parents(
    su: &Client,
    tenant_id: &str,
    catalog_id: &str,
    catalog_version: i32,
    environment: &str,
) {
    su.execute(
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ($1,$2,$3,$4,'0.1','applied')",
        &[&tenant_id, &catalog_id, &catalog_version, &environment],
    )
    .await
    .expect("seed run-pin catalog");
    su.execute(
        "INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
         VALUES ($1,$2,'0.1',''::bytea,0)",
        &[&tenant_id, &EMPTY_EXECUTION_BUNDLE_HASH],
    )
    .await
    .expect("seed run-pin execution bundle");
    su.execute(
        "INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) \
         VALUES ($1,$2,$3,'[]'::jsonb)",
        &[&tenant_id, &catalog_id, &catalog_version],
    )
    .await
    .expect("seed run-pin release manifest");
}

/// Hermetic reset: drop the target schema + the shared `catalog` schema and
/// ensure the `wamn_app` role, so every leg builds its own starting state.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
         DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOBYPASSRLS; \
         END IF; END $$; \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
             CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_run_projection_writer') THEN \
             CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         REVOKE wamn_scenario_author FROM wamn_app; \
         DO $$ BEGIN \
           EXECUTE format( \
             'REVOKE CONNECT ON DATABASE %I FROM PUBLIC', current_database() \
           ); \
           EXECUTE format( \
             'GRANT CONNECT ON DATABASE %I TO wamn_app', current_database() \
           ); \
           EXECUTE format( \
             'REVOKE CONNECT ON DATABASE %I FROM wamn_effect_writer, wamn_run_projection_writer', current_database() \
           ); \
         END $$;"
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

/// `column_exists` is fixed to the run-plane schema; catalog upgrades need the
/// same probe against `catalog`.
async fn catalog_column_exists(su: &Client, table: &str, column: &str) -> bool {
    su.query_one(
        "SELECT EXISTS ( SELECT FROM information_schema.columns \
         WHERE table_schema = 'catalog' AND table_name = $1 AND column_name = $2 )",
        &[&table, &column],
    )
    .await
    .expect("probe catalog column")
    .get(0)
}

async fn catalog_column_is_nullable(su: &Client, table: &str, column: &str) -> bool {
    su.query_one(
        "SELECT is_nullable = 'YES' FROM information_schema.columns \
          WHERE table_schema = 'catalog' AND table_name = $1 AND column_name = $2",
        &[&table, &column],
    )
    .await
    .expect("probe catalog column nullability")
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

async fn install_current_run_plane(su: &Client) {
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog schema");
    for ddl in [RUN_STATE_SQL, FLOWS_SQL, AUTHORING_TESTS_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane schema");
    }
}

async fn install_legacy_partition_plane(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.run_queue \
           ADD COLUMN partition_key text, \
           ADD COLUMN partition_policy text NOT NULL DEFAULT 'blocking', \
           ADD CONSTRAINT run_queue_partition_policy_check \
             CHECK (partition_policy IN ('blocking','leapfrog')); \
         CREATE INDEX run_queue_partition ON {SCHEMA}.run_queue \
           (tenant_id,partition_key) WHERE partition_key IS NOT NULL; \
         CREATE TABLE {SCHEMA}.partition_owner ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           partition_key text NOT NULL, lease_owner text NOT NULL, \
           lease_expires_at timestamptz NOT NULL, \
           acquired_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,partition_key)); \
         CREATE TABLE {SCHEMA}.run_dead_letters ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL, partition_key text NOT NULL, \
           flow_id text NOT NULL, reason text NOT NULL, \
           failed_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,run_id), \
           FOREIGN KEY (tenant_id,run_id) REFERENCES {SCHEMA}.runs (tenant_id,run_id) \
             ON DELETE CASCADE);"
    ))
    .await
    .expect("install retired partition plane");
}

async fn install_legacy_child_run_state(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN parent_run_id text, \
           ADD COLUMN parent_node_id text, \
           ADD COLUMN parent_occurrence int, \
           ADD COLUMN invoke_depth int NOT NULL DEFAULT 0, \
           ADD COLUMN invoke_root_run_id text, \
           ADD COLUMN waiting_child_run_id text, \
           ADD COLUMN waiting_child_occurrence int, \
           ADD COLUMN wait_generation bigint, \
           ADD CONSTRAINT runs_invoke_depth_check CHECK (invoke_depth >= 0), \
           ADD CONSTRAINT runs_check3 CHECK ( \
             (parent_run_id IS NULL) = (parent_node_id IS NULL) AND \
             (parent_run_id IS NULL) = (parent_occurrence IS NULL)), \
           ADD CONSTRAINT runs_check4 CHECK ( \
             (parent_run_id IS NULL) = (invoke_root_run_id IS NULL)), \
           ADD CONSTRAINT runs_check5 CHECK ( \
             (waiting_child_run_id IS NULL) = (waiting_child_occurrence IS NULL) AND \
             (waiting_child_run_id IS NULL) = (wait_generation IS NULL)); \
         CREATE UNIQUE INDEX runs_parent_occurrence ON {SCHEMA}.runs \
           (tenant_id,parent_run_id,parent_node_id,parent_occurrence) \
           WHERE parent_run_id IS NOT NULL; \
         CREATE INDEX runs_invoke_root ON {SCHEMA}.runs \
           (tenant_id,invoke_root_run_id) WHERE invoke_root_run_id IS NOT NULL; \
         CREATE INDEX runs_waiting_child ON {SCHEMA}.runs \
           (tenant_id,waiting_child_run_id) WHERE waiting_child_run_id IS NOT NULL;"
    ))
    .await
    .expect("install retired child-run state");
}

async fn partition_plane_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'relations', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,c.relkind) ORDER BY c.relname) \
               FROM pg_class c \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb), \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,a.attname,a.attnotnull, \
                                                pg_get_expr(d.adbin,d.adrelid)) \
                              ORDER BY c.relname,a.attnum) \
               FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
               LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters') \
                AND a.attnum > 0 AND NOT a.attisdropped), '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb), \
           'indexes', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(indexname,indexdef) ORDER BY indexname) \
               FROM pg_indexes WHERE schemaname=$1 \
                AND tablename IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("snapshot partition-plane schema")
    .get(0)
}

fn without_marked_section(source: &str, begin: &str, end: &str) -> String {
    let start = source.find(begin).expect("migration start marker");
    let end_start = source.find(end).expect("migration end marker");
    let end = source[end_start..]
        .find('\n')
        .map_or(source.len(), |offset| end_start + offset + 1);
    format!("{}{}", &source[..start], &source[end..])
}

fn assert_db_code(error: tokio_postgres::Error, expected: &str, context: &str) {
    let actual = error
        .as_db_error()
        .map(|database| database.code().code())
        .unwrap_or("non-database-error");
    assert_eq!(actual, expected, "{context}: {error}");
}

#[tokio::test]
async fn run_plane_reconcile_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-1wdq run-plane gate");
        return;
    };
    let su = connect(&url).await;
    shared_runner_legacy_leg(&su).await;
    frame_identity_cutover_leg(&su).await;
    effect_writer_cutover_leg(&su).await;
    effect_writer_populated_refusal_leg(&su).await;
    forced_rls_owner_refusal_leg(&su).await;
    partition_plane_authored_ordering_refusal_leg(&su).await;
    partition_plane_cutover_leg(&su).await;
    partition_plane_active_lease_refusal_leg(&su).await;
    partition_plane_unobservable_lease_refusal_leg(&su).await;
    partition_plane_dead_letter_refusal_leg(&su).await;
    v1_era_drifted_leg(&su, &url).await;
    queue_missing_leg(&su).await;
    from_zero_leg(&su).await;
    child_run_cutover_leg(&su).await;
    rerun_lineage_cutover_leg(&su).await;
    capture_mode_additive_leg(&su, &url).await;
    invocation_admission_retention_leg(&su).await;
    stored_suite_cutover_leg(&su).await;
    current_noop_leg(&su).await;
    authoring_storage_authority_leg(&su, &url).await;
    catalog_head_share_lock_leg(&su, &url).await;
    retired_effect_disposition_cutover_leg(&su).await;
    persisted_literal_check_drift_leg(&su).await;
}

#[tokio::test]
async fn execution_pin_cutover_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the execution-pin cutover gate");
        return;
    };
    let su = connect(&url).await;
    execution_pin_cutover_leg(&su).await;
}

#[tokio::test]
async fn stored_suite_cutover_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the stored-suite cutover gate");
        return;
    };
    let su = connect(&url).await;
    stored_suite_cutover_leg(&su).await;
}

#[tokio::test]
async fn authoring_storage_authority_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the authoring authority gate");
        return;
    };
    let su = connect(&url).await;
    authoring_storage_authority_leg(&su, &url).await;
}

#[tokio::test]
async fn child_run_cutover_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the child-run cutover gate");
        return;
    };
    let su = connect(&url).await;
    child_run_cutover_leg(&su).await;
}

#[tokio::test]
async fn rerun_lineage_cutover_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the rerun-lineage cutover gate");
        return;
    };
    let su = connect(&url).await;
    rerun_lineage_cutover_leg(&su).await;
}

#[tokio::test]
async fn retired_effect_disposition_cutover_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping retired disposition cutover gate");
        return;
    };
    let su = connect(&url).await;
    retired_effect_disposition_cutover_leg(&su).await;
}

async fn regress_execution_pin_contract(su: &Client) {
    su.batch_execute(&format!(
        "DROP TRIGGER runs_admission_pins_immutable ON {SCHEMA}.runs; \
         DROP FUNCTION {SCHEMA}.guard_run_admission_pins_immutable(); \
         DROP INDEX {SCHEMA}.runs_release; \
         DROP INDEX {SCHEMA}.runs_execution_bundle; \
         ALTER TABLE {SCHEMA}.runs \
           DROP CONSTRAINT runs_release_fk, \
           DROP CONSTRAINT runs_execution_bundle_fk, \
           DROP CONSTRAINT runs_check, \
           ALTER COLUMN catalog_id DROP NOT NULL, \
           ALTER COLUMN catalog_version DROP NOT NULL, \
           ALTER COLUMN catalog_version TYPE bigint USING catalog_version::bigint, \
           ALTER COLUMN environment DROP NOT NULL, \
           DROP COLUMN execution_bundle_hash; \
         ALTER TABLE {SCHEMA}.runs \
           ADD CONSTRAINT runs_check CHECK ((catalog_id IS NULL) = (catalog_version IS NULL)), \
           ADD CONSTRAINT runs_environment_check CHECK (environment IS NULL OR environment <> ''); \
         DROP INDEX catalog.release_flows_execution_bundle; \
         ALTER TABLE catalog.release_flows \
           DROP CONSTRAINT release_flows_execution_bundle_fk, \
           DROP CONSTRAINT release_flows_execution_bundle_hash_check, \
           DROP COLUMN execution_bundle_hash;"
    ))
    .await
    .expect("regress execution-pin contract");
}

async fn execution_pin_cutover_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog for pin cutover");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current runs for pin cutover");
    regress_execution_pin_contract(su).await;

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("empty legacy schema accepts execution-pin cutover");
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
            .count(),
        1
    );
    let columns = su
        .query(
            "SELECT table_schema, table_name, column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE (table_schema='catalog' AND table_name='release_flows' \
                    AND column_name='execution_bundle_hash') \
                OR (table_schema=$1 AND table_name='runs' \
                    AND column_name IN ('catalog_id','catalog_version','environment', \
                                        'execution_bundle_hash')) \
             ORDER BY table_schema, table_name, column_name",
            &[&SCHEMA],
        )
        .await
        .expect("read converged execution-pin columns");
    assert_eq!(columns.len(), 5);
    for row in &columns {
        let column: String = row.get(2);
        let data_type: String = row.get(3);
        let nullable: String = row.get(4);
        assert_eq!(nullable, "NO", "{column} is mandatory");
        assert_eq!(
            data_type,
            if column == "catalog_version" {
                "integer"
            } else {
                "text"
            }
        );
    }
    for object in [
        "release_flows_execution_bundle_fk",
        "runs_release_fk",
        "runs_execution_bundle_fk",
        "release_flows_execution_bundle",
        "runs_release",
        "runs_execution_bundle",
        "runs_admission_pins_immutable",
    ] {
        let present: bool = su
            .query_one(
                "SELECT to_regclass($1) IS NOT NULL \
                    OR to_regclass('catalog.' || $2) IS NOT NULL \
                    OR EXISTS (SELECT 1 FROM pg_constraint WHERE conname=$2) \
                    OR EXISTS (SELECT 1 FROM pg_trigger WHERE tgname=$2 AND NOT tgisinternal)",
                &[&format!("{SCHEMA}.{object}"), &object],
            )
            .await
            .expect("observe execution-pin object")
            .get(0);
        assert!(present, "missing execution-pin object {object}");
    }
    su.batch_execute(&format!(
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version) \
           VALUES ('pin','cat',1,'dev','0.1'); \
         INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
           VALUES ('pin','{EMPTY_EXECUTION_BUNDLE_HASH}','0.1',''::bytea,0); \
         INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) \
           VALUES ('pin','cat',1,'[]'::jsonb); \
         INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash) \
           VALUES ('pin','run','flow',1,'cat',1,'dev','{EMPTY_EXECUTION_BUNDLE_HASH}')"
    ))
    .await
    .expect("seed conforming execution pins");
    let update = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET environment='prod' \
                 WHERE tenant_id='pin' AND run_id='run'"
            ),
            &[],
        )
        .await
        .expect_err("run admission pin update must refuse");
    let update_db = update
        .as_db_error()
        .expect("pin update is a database refusal");
    assert_eq!(update_db.code().code(), "55000");
    assert_eq!(update_db.message(), "run-admission-pin-immutable");
    let environment: String = su
        .query_one(
            &format!(
                "SELECT environment FROM {SCHEMA}.runs \
                 WHERE tenant_id='pin' AND run_id='run'"
            ),
            &[],
        )
        .await
        .expect("read unchanged execution pins")
        .get(0);
    assert_eq!(environment, "dev");
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("execution-pin cutover is idempotent");
    assert!(again.is_noop(), "cutover converged: {:#?}", again.actions);

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog for refusal");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current runs for refusal");
    regress_execution_pin_contract(su).await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs (tenant_id,run_id,flow_id,flow_version) \
         VALUES ('legacy','legacy-run','flow',1)"
    ))
    .await
    .expect("seed structurally unpinned legacy run");

    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated legacy execution-pin cutover must refuse");
    assert_db_code(
        error.downcast().expect("postgres refusal"),
        "55000",
        "legacy pin cutover",
    );
    assert!(!catalog_column_exists(su, "release_flows", "execution_bundle_hash").await);
    assert!(!column_exists(su, "runs", "execution_bundle_hash").await);
    let version_type: String = su
        .query_one(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema=$1 AND table_name='runs' AND column_name='catalog_version'",
            &[&SCHEMA],
        )
        .await
        .expect("read rolled-back catalog_version type")
        .get(0);
    assert_eq!(version_type, "bigint");

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog for release-row refusal");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current runs for release-row refusal");
    regress_execution_pin_contract(su).await;
    su.batch_execute(
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version) \
           VALUES ('legacy','cat',1,'dev','0.1'); \
         INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
           VALUES ('legacy','flow',1,'0.1','{}'::jsonb,'graph','artifact'); \
         INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) \
           VALUES ('legacy','cat',1, \
             '[{\"flow-id\":\"flow\",\"flow-version\":1,\"artifact-hash\":\"artifact\"}]'::jsonb); \
         INSERT INTO catalog.release_flows \
           (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
           VALUES ('legacy','cat',1,'flow',1);",
    )
    .await
    .expect("seed structurally unpinned legacy release member");
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated release membership must refuse pin cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("release-row cutover is a database refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "execution-pin-cutover-requires-empty-run-and-release-membership"
    );
    assert!(!catalog_column_exists(su, "release_flows", "execution_bundle_hash").await);
    assert!(!column_exists(su, "runs", "execution_bundle_hash").await);
}

/// Persisted authored ordering bytes have no lossless global-FIFO backfill.
/// Refuse under a flow-table lock before DDL, preserve the bytes, and converge
/// once only default-omitted current graphs remain.
async fn partition_plane_authored_ordering_refusal_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.flows \
           (tenant_id,flow_id,version,graph_json) VALUES \
           ('retired-order','ordered',1, \
            '{{\"schema-version\":\"0.1\",\"ordering\":{{\"key\":\"serial\"}}}}'), \
           ('retired-order','policy',1, \
            '{{\"schema-version\":\"0.1\",\"partition-policy\":\"blocking\"}}'); \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("seed persisted retired flow ordering keys");

    let before: String = su
        .query_one(
            &format!(
                "SELECT jsonb_agg(graph_json ORDER BY flow_id)::text \
                   FROM {SCHEMA}.flows WHERE tenant_id='retired-order'"
            ),
            &[],
        )
        .await
        .expect("snapshot persisted flow bytes")
        .get(0);
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("retired authored ordering plans a guarded cutover");
    let cutover = dry.actions.first().expect("leading partition cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(cutover.sql.contains(&format!(
        "LOCK TABLE \"{SCHEMA}\".\"run_queue\", \"{SCHEMA}\".\"flows\" IN ACCESS EXCLUSIVE MODE"
    )));

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("persisted authored ordering requires reprovision");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("typed authored-order refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "retired-authored-ordering-requires-environment-reprovision"
    );
    let after: String = su
        .query_one(
            &format!(
                "SELECT jsonb_agg(graph_json ORDER BY flow_id)::text \
                   FROM {SCHEMA}.flows WHERE tenant_id='retired-order'"
            ),
            &[],
        )
        .await
        .expect("read refusal-preserved flow bytes")
        .get(0);
    assert_eq!(after, before);
    assert!(!column_exists(su, "run_queue", "partition_key").await);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read authored-ordering refusal mutation sentinel")
        .get(0);
    assert!(membership_retained, "refusal must be the leading action");

    su.batch_execute(&format!(
        "REVOKE wamn_scenario_author FROM wamn_app; \
         DELETE FROM {SCHEMA}.flows WHERE tenant_id='retired-order'; \
         INSERT INTO {SCHEMA}.flows (tenant_id,flow_id,version,graph_json) \
         VALUES ('current-order','default-omitted',1, \
                 '{{\"schema-version\":\"0.1\",\"nodes\":[]}}');"
    ))
    .await
    .expect("replace refusal fixture with current default-omitted graph");
    let converged = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("default-omitted flow graph needs no partition cutover");
    assert!(converged.is_noop(), "actions: {:#?}", converged.actions);
}

/// A table owner remains subject to `FORCE ROW LEVEL SECURITY`. Letting that
/// role reconciliation would hide tenant history, so refuse before even a
/// dry-run observation can claim completeness.
async fn forced_rls_owner_refusal_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "DO $role$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='rp_owner_no_bypass') THEN \
             CREATE ROLE rp_owner_no_bypass NOSUPERUSER NOBYPASSRLS; \
           END IF; \
         END $role$; \
         ALTER ROLE rp_owner_no_bypass NOSUPERUSER NOBYPASSRLS; \
         DO $temporary$ BEGIN EXECUTE format( \
           'GRANT TEMPORARY ON DATABASE %I TO rp_owner_no_bypass', current_database()); \
         END $temporary$; \
         CREATE SCHEMA {SCHEMA} AUTHORIZATION rp_owner_no_bypass; \
         SET ROLE rp_owner_no_bypass; \
         CREATE TABLE {SCHEMA}.node_runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, \
           frame_id bigint NOT NULL DEFAULT 0, parent_frame_id bigint, call_site_id text, \
           current_plan_hash text NOT NULL, local_node_id text NOT NULL, \
           occurrence int NOT NULL, seq int NOT NULL, attempt int NOT NULL, \
           status text NOT NULL, selected_recovery_class text, recovery_class text, \
           generation_fact_kind text, connection_generation text, \
           credential_generation text, attempt_started_at timestamptz, \
           attempt_dispatched_at timestamptz, attempt_deadline_at timestamptz, \
           attempt_input_ref text, attempt_key text, \
           PRIMARY KEY (tenant_id,run_id,frame_id,local_node_id,occurrence)); \
         ALTER TABLE {SCHEMA}.node_runs ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {SCHEMA}.node_runs FORCE ROW LEVEL SECURITY; \
         CREATE POLICY node_runs_tenant ON {SCHEMA}.node_runs \
           USING (tenant_id = NULLIF(current_setting('app.tenant',true),'')) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant',true),'')); \
         RESET ROLE; \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,attempt,status, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_dispatched_at,attempt_deadline_at, \
            attempt_input_ref) VALUES \
           ('hidden','legacy',0,'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','effect',0,0,0,'started','never-replay', \
           'never-replay','not-required',now(),now(),now()+interval '1 minute', \
            'sha256:hidden'); \
         SET ROLE rp_owner_no_bypass; \
         CREATE TEMP TABLE pg_roles \
             (rolname text, rolsuper boolean, rolbypassrls boolean); \
         INSERT INTO pg_temp.pg_roles \
             VALUES ('rp_owner_no_bypass',false,true);"
    ))
    .await
    .expect("seed a forced-RLS legacy row owned by a non-bypass role");

    for apply in [false, true] {
        let error = reconcile_run_plane::reconcile(su, &schema, apply)
            .await
            .expect_err("plain owner must not reconcile forced-RLS history");
        assert!(
            error.to_string().contains("SUPERUSER or BYPASSRLS"),
            "explicit forced-RLS refusal: {error:#}"
        );
    }
    su.batch_execute(
        "RESET ROLE; DROP TABLE pg_temp.pg_roles; \
         DO $temporary$ BEGIN EXECUTE format( \
           'REVOKE TEMPORARY ON DATABASE %I FROM rp_owner_no_bypass', current_database()); \
         END $temporary$;",
    )
    .await
    .expect("restore superuser after owner refusal");

    assert_eq!(
        su.query_one(&format!("SELECT count(*) FROM {SCHEMA}.node_runs"), &[])
            .await
            .expect("hidden legacy row remains")
            .get::<_, i64>(0),
        1
    );
    assert!(
        !column_exists(su, "node_runs", "current_effect_attempt_id").await,
        "refusal performs no schema mutation"
    );
    assert!(
        !table_exists(su, SCHEMA, "effect_attempts").await,
        "refusal occurs before ledger activation"
    );
}

async fn retired_shape_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN ('node_runs','effect_attempts')), '[]'::jsonb), \
           'indexes', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(indexname,indexdef) ORDER BY indexname) \
               FROM pg_indexes WHERE schemaname=$1 \
                AND tablename IN ('node_runs','effect_attempts')), '[]'::jsonb), \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,a.attname,a.attnotnull, \
                                                pg_get_expr(d.adbin,d.adrelid)) \
                              ORDER BY c.relname,a.attnum) \
               FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
               LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname IN ('node_runs','effect_attempts') \
                AND a.attnum > 0 AND NOT a.attisdropped), '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("read retired schema snapshot")
    .get(0)
}

async fn create_old_frame_identity_tables(su: &Client, node: bool, effect: bool, populated: bool) {
    reset(su).await;
    su.batch_execute(&format!("CREATE SCHEMA {SCHEMA};"))
        .await
        .expect("create frame-cutover schema");
    if node {
        su.batch_execute(&format!(
            "CREATE TABLE {SCHEMA}.node_runs ( \
               tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
               occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, status text NOT NULL, \
               PRIMARY KEY (tenant_id,run_id,node_id,occurrence));"
        ))
        .await
        .expect("create old node_runs");
        if populated {
            su.batch_execute(&format!(
                "INSERT INTO {SCHEMA}.node_runs(tenant_id,run_id,node_id,occurrence,seq,status) \
                 VALUES ('t1','r1','n1',0,0,'success');"
            ))
            .await
            .expect("seed old node_runs");
        }
    }
    if effect {
        su.batch_execute(&format!(
            "CREATE TABLE {SCHEMA}.effect_attempts ( \
               tenant_id text NOT NULL, attempt_id uuid NOT NULL, run_id text NOT NULL, \
               node_id text NOT NULL, occurrence int NOT NULL, seq int NOT NULL, \
               generation_fact_kind text NOT NULL, attempt_started_at timestamptz NOT NULL, \
               attempt_deadline_at timestamptz NOT NULL, attempt_input_ref text NOT NULL, \
               PRIMARY KEY (tenant_id,attempt_id), \
               UNIQUE (tenant_id,attempt_id,attempt_started_at), \
               CONSTRAINT effect_attempts_occurrence_key \
                 UNIQUE (tenant_id,run_id,node_id,occurrence));"
        ))
        .await
        .expect("create old effect_attempts");
        if populated {
            su.batch_execute(&format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,attempt_id,run_id,node_id,occurrence,seq,generation_fact_kind, \
                    attempt_started_at,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000413','r1','n1',0,0, \
                         'not-required','2026-01-01 UTC','2026-01-02 UTC','sha256:input');"
            ))
            .await
            .expect("seed old effect_attempts");
        }
    }
}

async fn frame_identity_cutover_leg(su: &Client) {
    for (label, node, effect) in [
        ("both-present", true, true),
        ("node-only", true, false),
        ("effect-only", false, true),
    ] {
        create_old_frame_identity_tables(su, node, effect, true).await;
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("seed role-membership mutation sentinel");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated legacy identity must refuse before DDL");
        let expected_refusal = if effect {
            "effect-writer-cutover-requires-empty-ledger"
        } else {
            "frame-identity-cutover-requires-empty-node-and-effect-facts"
        };
        assert!(
            format!("{error:#}").contains(expected_refusal),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            retired_shape_schema_snapshot(su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app', 'wamn_scenario_author', 'MEMBER')",
                &[],
            )
            .await
            .expect("read role-membership mutation sentinel")
            .get(0);
        assert!(
            membership_retained,
            "{label}: refusal must precede role bootstrap"
        );
    }

    for (label, drift_sql) in [
        (
            "drifted-frame-check",
            format!(
                "ALTER TABLE {SCHEMA}.node_runs \
                   DROP CONSTRAINT node_runs_frame_relation_check, \
                   ADD CONSTRAINT node_runs_frame_relation_check CHECK (frame_id >= 0);"
            ),
        ),
        (
            "drifted-frame-pk",
            format!(
                "ALTER TABLE {SCHEMA}.node_runs DROP CONSTRAINT node_runs_pkey, \
                   ADD PRIMARY KEY (tenant_id,run_id,local_node_id,occurrence);"
            ),
        ),
        (
            "retained-legacy-node-id",
            format!(
                "ALTER TABLE {SCHEMA}.node_runs \
                   ADD COLUMN node_id text NOT NULL DEFAULT 'legacy';"
            ),
        ),
    ] {
        reset(su).await;
        su.batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .expect("apply catalog for populated frame drift refusal");
        su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
            .await
            .expect("apply current run-state for populated frame drift refusal");
        seed_run_pin_parents(su, "t1", "frame-cat", 1, "dev").await;
        su.batch_execute(&format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                execution_bundle_hash,status) \
             VALUES ('t1',$${label}$$,'f',1,'frame-cat',1,'dev',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'running'); \
             INSERT INTO {SCHEMA}.node_runs \
               (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,status) \
             VALUES ('t1',$${label}$$,0,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'n1',0,0,'success'); \
             {drift_sql}"
        ))
        .await
        .expect("seed populated frame drift");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated late frame identity drift must refuse before DDL");
        assert!(
            format!("{error:#}")
                .contains("frame-identity-cutover-requires-empty-node-and-effect-facts"),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            retired_shape_schema_snapshot(su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
    }

    create_old_frame_identity_tables(su, true, true, false).await;
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty old frame identity cutover succeeds");
    assert!(
        plan.actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );
    let old_identity_residue: i64 = su
        .query_one(
            "SELECT count(*) FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name IN ('node_runs','effect_attempts') \
                AND column_name='node_id'",
            &[&SCHEMA],
        )
        .await
        .expect("read upgraded frame identity columns")
        .get(0);
    assert_eq!(
        old_identity_residue, 0,
        "empty cutover retained legacy node_id"
    );
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("frame identity cutover idempotence");
    assert!(
        !again
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for combined frame/writer cutover");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state for combined frame/writer cutover");
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
           DROP CONSTRAINT effect_attempt_dispatches_attempt_fk, \
           DROP CONSTRAINT effect_attempt_dispatches_occurrence_key, \
           DROP COLUMN run_id, DROP COLUMN frame_id, \
           DROP COLUMN local_node_id, DROP COLUMN occurrence; \
         ALTER TABLE {SCHEMA}.effect_attempts \
           DROP CONSTRAINT effect_attempts_current_plan_hash_check, \
           ADD CONSTRAINT effect_attempts_current_plan_hash_check \
             CHECK (current_plan_hash <> '');"
    ))
    .await
    .expect("install combined frame and dispatch-coordinate drift");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("combined frame/writer cutover converges in one pass");
    let frame_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("combined frame cutover action");
    let writer_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("combined writer cutover action");
    assert!(frame_position < writer_position);
    assert!(
        plan.actions[frame_position]
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        !plan.actions[frame_position]
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        plan.actions[writer_position]
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    for column in ["run_id", "frame_id", "local_node_id", "occurrence"] {
        assert!(
            column_exists(su, "effect_attempt_dispatches", column).await,
            "combined cutover did not restore dispatch coordinate {column}"
        );
    }
    let dispatch_fk: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read combined-cutover dispatch FK")
        .get(0);
    assert!(dispatch_fk.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("combined frame/writer cutover reapply");
    assert!(!again.actions.iter().any(|action| matches!(
        action.kind,
        RunPlaneActionKind::FrameIdentityCutover | RunPlaneActionKind::EffectWriterCutover
    )));

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for current effect-frame drift");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state for effect-frame drift");
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempts \
           DROP CONSTRAINT effect_attempts_current_plan_hash_check, \
           ADD CONSTRAINT effect_attempts_current_plan_hash_check CHECK (current_plan_hash <> '');"
    ))
    .await
    .expect("install current-schema effect-frame drift");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty effect-frame drift converges with dispatch FK present");
    let action = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("effect-frame cutover action");
    assert!(action.sql.contains(&format!(
        "LOCK TABLE \"{SCHEMA}\".effect_attempt_dispatches IN ACCESS EXCLUSIVE MODE"
    )));
    assert!(
        action
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        action
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    let dispatch_fk: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read restored dispatch-to-attempt FK")
        .get(0);
    assert!(dispatch_fk.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("effect-frame cutover reapply");
    assert!(
        !again
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for current single-target proof");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state");
    su.batch_execute(&format!("DROP TABLE {SCHEMA}.effect_attempts CASCADE;"))
        .await
        .expect("remove effect peer");
    seed_run_pin_parents(su, "t1", "frame-cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','framed-current','f',1,'frame-cat',1,'dev',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'running'); \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,status) \
         VALUES ('t1','framed-current',0,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'n1',0,0,'success');"
    ))
    .await
    .expect("seed current populated node target");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("current populated node target creates missing effect peer");
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );
    let exists: bool = su
        .query_one(
            "SELECT to_regclass($1::text) IS NOT NULL",
            &[&format!("{SCHEMA}.effect_attempts")],
        )
        .await
        .expect("read recreated effect peer")
        .get(0);
    assert!(exists, "missing current peer was not recreated");
}

async fn effect_writer_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(table_name,column_name,is_nullable,column_default) \
                              ORDER BY table_name,ordinal_position) \
               FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name IN \
                    ('node_runs','effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN \
                    ('node_runs','effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("snapshot effect-writer schema")
    .get(0)
}

async fn install_empty_incompatible_effect_writer_shape(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.node_runs \
             ADD COLUMN current_effect_attempt_id uuid, \
             ADD COLUMN attempt int, \
             ADD COLUMN selected_recovery_class text, \
             ADD COLUMN recovery_class text, \
             ADD COLUMN generation_fact_kind text, \
             ADD COLUMN connection_generation text, \
             ADD COLUMN credential_generation text, \
             ADD COLUMN attempt_started_at timestamptz, \
             ADD COLUMN attempt_dispatched_at timestamptz, \
             ADD COLUMN attempt_deadline_at timestamptz, \
             ADD COLUMN attempt_input_ref text, \
             ADD COLUMN attempt_key text, \
             ADD CONSTRAINT node_runs_current_effect_attempt_fk \
               FOREIGN KEY (tenant_id,current_effect_attempt_id) \
               REFERENCES {SCHEMA}.effect_attempts (tenant_id,attempt_id), \
             ADD CONSTRAINT node_runs_selected_recovery_class_check \
               CHECK (selected_recovery_class IS NULL OR selected_recovery_class <> ''); \
         ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
             DROP CONSTRAINT effect_attempt_dispatches_attempt_fk, \
             DROP CONSTRAINT effect_attempt_dispatches_occurrence_key, \
             DROP COLUMN run_id, DROP COLUMN frame_id, \
             DROP COLUMN local_node_id, DROP COLUMN occurrence; \
         ALTER TABLE {SCHEMA}.effect_attempts \
             DROP CONSTRAINT effect_attempts_dispatch_identity_key, \
             ALTER COLUMN attempt_started_at DROP DEFAULT, \
             ADD COLUMN attempt_key text;"
    ))
    .await
    .expect("install incompatible empty writer-ledger shape");
}

async fn effect_writer_cutover_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for writer cutover");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current run-state for writer cutover");
    seed_run_pin_parents(su, "t1", "writer-cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','writer-projection','f',1,'writer-cat',1,'dev', \
                 $${EMPTY_EXECUTION_BUNDLE_HASH}$$,'running'); \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,status) \
         VALUES ('t1','writer-projection',0,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'n1',0,0,'success');"
    ))
    .await
    .expect("seed mutable projection retained across writer cutover");
    install_empty_incompatible_effect_writer_shape(su).await;

    su.batch_execute("ALTER ROLE wamn_effect_writer LOGIN")
        .await
        .expect("make stable writer role invalid");
    let before_role_refusal = effect_writer_schema_snapshot(su).await;
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("invalid stable role refuses before empty cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("role refusal");
    let database = postgres.as_db_error().expect("typed role refusal");
    assert_eq!(database.code().code(), "42501");
    assert_eq!(database.message(), "effect-writer-role-out-of-bounds");
    assert_eq!(
        effect_writer_schema_snapshot(su).await,
        before_role_refusal,
        "role verification precedes empty structural cutover"
    );
    su.batch_execute("ALTER ROLE wamn_effect_writer NOLOGIN")
        .await
        .expect("restore stable writer role");
    su.batch_execute("ALTER ROLE wamn_run_projection_writer LOGIN")
        .await
        .expect("make stable projection role invalid");
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("invalid stable projection role refuses before empty cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("projection role refusal");
    let database = postgres
        .as_db_error()
        .expect("typed projection role refusal");
    assert_eq!(database.code().code(), "42501");
    assert_eq!(
        database.message(),
        "run-projection-writer-role-out-of-bounds"
    );
    su.batch_execute("ALTER ROLE wamn_run_projection_writer NOLOGIN")
        .await
        .expect("restore stable projection role");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("empty writer-ledger cutover succeeds");
    let action = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("effect writer cutover action");
    assert_eq!(
        action.sql.matches("LOCK TABLE").count(),
        4,
        "the mutable projection and three incompatible ledgers are locked"
    );
    assert!(
        action
            .sql
            .contains(&format!("LOCK TABLE \"{SCHEMA}\".\"node_runs\""))
    );
    let preflight = action
        .sql
        .find("effect-writer-cutover-requires-empty-ledger")
        .expect("frozen cutover preflight");
    let first_ddl = ["ALTER TABLE", "DROP TRIGGER", "DROP FUNCTION"]
        .into_iter()
        .filter_map(|needle| action.sql.find(needle))
        .min()
        .expect("cutover structural DDL");
    assert!(
        preflight < first_ddl,
        "all preflight precedes structural DDL"
    );
    assert!(!action.sql.contains("UPDATE "));
    assert!(!action.sql.contains("INSERT INTO "));

    for column in [
        "current_effect_attempt_id",
        "attempt",
        "selected_recovery_class",
        "recovery_class",
        "generation_fact_kind",
        "connection_generation",
        "credential_generation",
        "attempt_started_at",
        "attempt_dispatched_at",
        "attempt_deadline_at",
        "attempt_input_ref",
        "attempt_key",
    ] {
        assert!(
            !column_exists(su, "node_runs", column).await,
            "retired node projection column {column} survived"
        );
    }
    let retained_projection_rows: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.node_runs"), &[])
        .await
        .expect("read retained mutable projection")
        .get(0);
    assert_eq!(retained_projection_rows, 1);
    assert!(!column_exists(su, "effect_attempts", "attempt_key").await);
    for column in ["run_id", "frame_id", "local_node_id", "occurrence"] {
        assert!(
            column_exists(su, "effect_attempt_dispatches", column).await,
            "missing dispatch coordinate {column}"
        );
    }
    let occurrence: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_occurrence_key'",
            &[&SCHEMA],
        )
        .await
        .expect("read exact dispatch occurrence identity")
        .get(0);
    assert_eq!(
        occurrence,
        "UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)"
    );
    let foreign_key: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read coordinate-bound attempt FK")
        .get(0);
    assert!(foreign_key.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));

    let second = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("writer-ledger cutover reapply");
    assert!(
        !second
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
    );

    su.batch_execute(&format!(
        "GRANT CREATE ON SCHEMA {SCHEMA} TO wamn_effect_writer; \
         GRANT SELECT ON {SCHEMA}.effect_attempts TO wamn_scenario_author; \
         GRANT UPDATE (attempt_input_ref) ON {SCHEMA}.effect_attempts TO wamn_app; \
         GRANT SELECT ON {SCHEMA}.runs TO wamn_effect_writer; \
         GRANT UPDATE (status) ON {SCHEMA}.runs TO wamn_effect_writer; \
         ALTER TABLE {SCHEMA}.run_queue DROP COLUMN lease_owner; \
         REVOKE SELECT (lease_expires_at) ON {SCHEMA}.run_queue FROM wamn_effect_writer; \
         GRANT SELECT (lease_generation) ON {SCHEMA}.run_queue TO wamn_effect_writer; \
         GRANT UPDATE ON {SCHEMA}.node_runs TO wamn_app; \
         REVOKE DELETE ON {SCHEMA}.node_runs FROM wamn_run_projection_writer; \
         GRANT SELECT ON {SCHEMA}.node_runs TO wamn_effect_writer;"
    ))
    .await
    .expect("install schema/table/column ACL drift");
    let repair = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("repair effect-writer ACL drift");
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.usage")
    }));
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
    }));
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.node_runs")
    }));
    for table in ["runs", "run_queue"] {
        assert!(repair.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == format!("{SCHEMA}.{table}.effect-read")
        }));
    }
    let add_lease_owner = repair
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::AddColumn && action.target == "run_queue.lease_owner"
        })
        .expect("partial queue adds the missing writer-read column");
    let repair_queue_read = repair
        .actions
        .iter()
        .position(|action| action.target == format!("{SCHEMA}.run_queue.effect-read"))
        .unwrap();
    assert!(add_lease_owner < repair_queue_read);
    let privileges = su
        .query_one(
            &format!(
                "SELECT has_schema_privilege('wamn_effect_writer','{SCHEMA}','USAGE'), \
                        has_schema_privilege('wamn_effect_writer','{SCHEMA}','CREATE'), \
                        has_column_privilege('wamn_app','{SCHEMA}.effect_attempts', \
                                             'attempt_input_ref','UPDATE'), \
                        has_table_privilege('wamn_scenario_author', \
                                            '{SCHEMA}.effect_attempts','SELECT'), \
                        has_table_privilege('wamn_effect_writer', \
                                            '{SCHEMA}.effect_attempts','INSERT')"
            ),
            &[],
        )
        .await
        .expect("read converged effect-writer ACL boundary");
    assert!(privileges.get::<_, bool>(0));
    assert!(!privileges.get::<_, bool>(1));
    assert!(!privileges.get::<_, bool>(2));
    assert!(!privileges.get::<_, bool>(3));
    assert!(privileges.get::<_, bool>(4));
    let run_reads = su
        .query_one(
            &format!(
                "SELECT \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.runs','SELECT'), \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.runs','UPDATE'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','tenant_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','run_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','status','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','flow_id','SELECT'), \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.run_queue','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','tenant_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','run_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_owner','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_expires_at','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_generation','SELECT'), \
                    has_any_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','INSERT,UPDATE,REFERENCES')"
            ),
            &[],
        )
        .await
        .expect("read exact effect-writer runnable-state privileges");
    assert!(!run_reads.get::<_, bool>(0));
    assert!(!run_reads.get::<_, bool>(1));
    assert!(run_reads.get::<_, bool>(2));
    assert!(!run_reads.get::<_, bool>(3));
    assert!(!run_reads.get::<_, bool>(4));
    assert!(run_reads.get::<_, bool>(5));
    assert!(run_reads.get::<_, bool>(6));
    assert!(!run_reads.get::<_, bool>(7));
    let projection_acl = su
        .query_one(
            &format!(
                "SELECT \
                    has_table_privilege('wamn_app','{SCHEMA}.node_runs','SELECT'), \
                    has_any_column_privilege('wamn_app','{SCHEMA}.node_runs','INSERT,UPDATE,REFERENCES') \
                      OR has_table_privilege('wamn_app','{SCHEMA}.node_runs','DELETE'), \
                    has_table_privilege('wamn_run_projection_writer','{SCHEMA}.node_runs','SELECT,INSERT,UPDATE,DELETE'), \
                    has_table_privilege('wamn_run_projection_writer','{SCHEMA}.node_runs','TRUNCATE,REFERENCES,TRIGGER'), \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.node_runs','SELECT,INSERT,UPDATE,DELETE')"
            ),
            &[],
        )
        .await
        .expect("read exact run-projection ACL boundary");
    assert!(projection_acl.get::<_, bool>(0));
    assert!(!projection_acl.get::<_, bool>(1));
    assert!(projection_acl.get::<_, bool>(2));
    assert!(!projection_acl.get::<_, bool>(3));
    assert!(!projection_acl.get::<_, bool>(4));

    su.batch_execute(&format!(
        "DO $roles$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_projection_rogue_direct') THEN \
             CREATE ROLE wamn_projection_rogue_direct NOLOGIN; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_projection_rogue_column') THEN \
             CREATE ROLE wamn_projection_rogue_column NOLOGIN; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_projection_rogue_member') THEN \
             CREATE ROLE wamn_projection_rogue_member NOLOGIN INHERIT; \
           END IF; \
         END $roles$; \
         GRANT INSERT ON {SCHEMA}.node_runs TO wamn_projection_rogue_direct; \
         GRANT UPDATE (status) ON {SCHEMA}.node_runs TO wamn_projection_rogue_column; \
         GRANT wamn_projection_rogue_column TO wamn_projection_rogue_member;"
    ))
    .await
    .expect("install rogue direct and inherited-column authority");
    let rogue_repair = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("revoke every directly granted rogue projection path");
    assert!(rogue_repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.node_runs")
    }));
    let rogue_direct_closed: bool = su
        .query_one(
            &format!(
                "SELECT NOT has_table_privilege('wamn_projection_rogue_direct', \
                           '{SCHEMA}.node_runs','INSERT') \
                    AND NOT has_column_privilege('wamn_projection_rogue_column', \
                           '{SCHEMA}.node_runs','status','UPDATE') \
                    AND NOT has_column_privilege('wamn_projection_rogue_member', \
                           '{SCHEMA}.node_runs','status','UPDATE')"
            ),
            &[],
        )
        .await
        .expect("read closed rogue direct projection authority")
        .get(0);
    assert!(rogue_direct_closed);

    let generation = "wamn_effect_writer_0000000000000000000000000000000000000000_a";
    su.batch_execute(&format!(
        "DO $generation$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{generation}') THEN \
             CREATE ROLE {generation} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $generation$; \
         GRANT wamn_effect_writer, wamn_run_projection_writer TO {generation}; \
         GRANT {generation} TO wamn_projection_rogue_member;"
    ))
    .await
    .expect("install unexpected transitive generation membership");
    let inherited = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("unexpected stable-role membership must fail closed");
    assert!(
        format!("{inherited:#}").contains("effect-writer-role-out-of-bounds"),
        "wrong inherited-authority refusal: {inherited:#}"
    );
    assert!(
        su.query_one(
            &format!(
                "SELECT has_table_privilege('wamn_projection_rogue_member', \
                                             '{SCHEMA}.node_runs','UPDATE')"
            ),
            &[],
        )
        .await
        .expect("read retained refused membership")
        .get::<_, bool>(0),
        "refusal is atomic and does not silently rewrite role membership"
    );
    su.batch_execute(&format!(
        "REVOKE {generation} FROM wamn_projection_rogue_member; \
         REVOKE wamn_effect_writer, wamn_run_projection_writer FROM {generation}; \
         REVOKE wamn_projection_rogue_column FROM wamn_projection_rogue_member;"
    ))
    .await
    .expect("remove disposable rogue memberships");

    let impostor = "wamn_effect_writer_1111111111111111111111111111111111111111_b";
    su.batch_execute(&format!(
        "DO $impostor$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{impostor}') THEN \
             CREATE ROLE {impostor} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $impostor$; \
         ALTER ROLE {impostor} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           INHERIT NOREPLICATION NOBYPASSRLS; \
         GRANT wamn_run_projection_writer TO {impostor}; \
         DO $connect$ BEGIN EXECUTE format( \
           'GRANT CONNECT ON DATABASE %I TO {impostor}', current_database()); \
         END $connect$;"
    ))
    .await
    .expect("install projection-only connected generation impostor");
    let impostor_refusal = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("projection-only connected generation impostor must fail closed");
    assert!(
        format!("{impostor_refusal:#}").contains("effect-writer-role-out-of-bounds"),
        "wrong connected-generation refusal: {impostor_refusal:#}"
    );
    let impostor_retained: bool = su
        .query_one(
            &format!(
                "SELECT has_database_privilege('{impostor}',current_database(),'CONNECT') \
                    AND pg_has_role('{impostor}','wamn_run_projection_writer','MEMBER') \
                    AND NOT pg_has_role('{impostor}','wamn_effect_writer','MEMBER')"
            ),
            &[],
        )
        .await
        .expect("read atomically retained connected impostor")
        .get(0);
    assert!(impostor_retained);
    su.batch_execute(&format!(
        "DO $disconnect$ BEGIN EXECUTE format( \
           'REVOKE CONNECT ON DATABASE %I FROM {impostor}', current_database()); \
         END $disconnect$; \
         REVOKE wamn_run_projection_writer FROM {impostor}; \
         ALTER ROLE {impostor} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
    ))
    .await
    .expect("remove disposable connected generation impostor authority");
    let clean = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("projection ACL converges after authority removal");
    assert!(!clean.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.node_runs")
    }));
}

async fn effect_writer_populated_refusal_leg(su: &Client) {
    for populated in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        reset(su).await;
        let schema = schema();
        su.batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .expect("apply catalog for writer refusal");
        su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
            .await
            .expect("apply current run-state for writer refusal");
        su.batch_execute("SELECT set_config('app.tenant','t1',false)")
            .await
            .expect("prepare isolated incompatible ledger fact");
        match populated {
            "effect_attempts" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.effect_attempts \
                       (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                        local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                        generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000491','r', \
                        $${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                        'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,1, \
                        'not-required','2099-01-02 UTC','sha256:input');"
                ))
                .await
                .expect("seed incompatible attempt fact");
            }
            "effect_attempt_dispatches" => {
                su.batch_execute(&format!(
                    "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
                       DROP CONSTRAINT effect_attempt_dispatches_attempt_fk; \
                     INSERT INTO {SCHEMA}.effect_attempt_dispatches \
                       (tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                        local_node_id,occurrence,dispatched_at) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000492', \
                        '2026-01-01 UTC','r',0,'n',0,'2026-01-01 00:01 UTC');"
                ))
                .await
                .expect("seed incompatible dispatch fact");
            }
            "effect_attempt_outcomes" => {
                su.batch_execute(&format!(
                    "ALTER TABLE {SCHEMA}.effect_attempt_outcomes \
                       DROP CONSTRAINT effect_attempt_outcomes_dispatch_fk; \
                     INSERT INTO {SCHEMA}.effect_attempt_outcomes \
                       (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000493', \
                        '2026-01-01 UTC','success','2026-01-01 00:01 UTC');"
                ))
                .await
                .expect("seed incompatible outcome fact");
            }
            _ => unreachable!(),
        }
        su.batch_execute(&format!(
            "ALTER TABLE {SCHEMA}.effect_attempts ADD COLUMN attempt_key text;"
        ))
        .await
        .expect("install incompatible accepted-residue column");
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("install unrelated mutation sentinel");
        let before = effect_writer_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .expect_err("populated incompatible writer ledger refuses");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed cutover refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(
            database.message(),
            "effect-writer-cutover-requires-empty-ledger"
        );
        assert_eq!(
            effect_writer_schema_snapshot(su).await,
            before,
            "{populated}: refusal leaves schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
                &[],
            )
            .await
            .expect("read unrelated mutation sentinel")
            .get(0);
        assert!(
            membership_retained,
            "refusal precedes unrelated role repair"
        );
    }
}

async fn shared_runner_legacy_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "CREATE SCHEMA {SCHEMA}; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app; \
         CREATE TABLE {SCHEMA}.runs ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), run_id text NOT NULL, \
           flow_id text NOT NULL, flow_version int NOT NULL, \
           status text NOT NULL DEFAULT 'running' CHECK (status IN \
             ('dispatched','running','completed','failed','infrastructure-failure','effect-uncertain')), \
           trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
           idempotency_key text, replay_of text, root_run_id text, \
           fail_kind text CHECK (fail_kind IN \
             ('terminal','retry-exhausted','invalid-input','runaway-budget')), \
           fail_node text, fail_reason text, created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tenant_id,run_id)); \
         CREATE TABLE {SCHEMA}.node_runs (tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL,node_id text NOT NULL,occurrence int NOT NULL DEFAULT 0, \
           seq int NOT NULL,attempt int NOT NULL DEFAULT 0,status text NOT NULL CHECK \
             (status IN ('running','success','error')),output_port text,output_json jsonb, \
           input_json jsonb,error_kind text CHECK (error_kind IN \
             ('retryable','rate-limited','terminal','invalid-input','cancelled')),error_detail jsonb, \
           input_ref text,output_ref text,preview_head text,payload_size bigint, \
           payload_hash text,capture_mode text,redacted boolean NOT NULL DEFAULT false, \
           started_at timestamptz NOT NULL DEFAULT now(),ended_at timestamptz, \
           PRIMARY KEY(tenant_id,run_id,node_id,occurrence),FOREIGN KEY(tenant_id,run_id) \
             REFERENCES {SCHEMA}.runs(tenant_id,run_id) ON DELETE CASCADE); \
         CREATE INDEX node_runs_seq ON {SCHEMA}.node_runs(tenant_id,run_id,seq); \
         CREATE TABLE {SCHEMA}.run_queue (tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL,partition_key text,partition_policy text NOT NULL DEFAULT 'blocking' \
             CHECK(partition_policy IN ('blocking','leapfrog')),priority int NOT NULL DEFAULT 0, \
           available_at timestamptz NOT NULL DEFAULT now(),stream_seq bigint NOT NULL DEFAULT 0, \
           lease_owner text,lease_expires_at timestamptz,attempts int NOT NULL DEFAULT 0, \
           max_attempts int NOT NULL DEFAULT 20,enqueued_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY(tenant_id,run_id),FOREIGN KEY(tenant_id,run_id) \
             REFERENCES {SCHEMA}.runs(tenant_id,run_id) ON DELETE CASCADE); \
         CREATE INDEX run_queue_claimable ON {SCHEMA}.run_queue \
           (tenant_id,available_at,stream_seq,lease_expires_at); \
         CREATE INDEX run_queue_partition ON {SCHEMA}.run_queue(tenant_id,partition_key) \
           WHERE partition_key IS NOT NULL; \
         CREATE TABLE {SCHEMA}.partition_owner (tenant_id text NOT NULL CHECK(tenant_id <> ''), \
           partition_key text NOT NULL,lease_owner text NOT NULL,lease_expires_at timestamptz NOT NULL, \
           acquired_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(tenant_id,partition_key)); \
         CREATE TABLE {SCHEMA}.run_dead_letters (tenant_id text NOT NULL CHECK(tenant_id <> ''), \
           run_id text NOT NULL,partition_key text NOT NULL,flow_id text NOT NULL,reason text NOT NULL, \
           failed_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(tenant_id,run_id), \
           FOREIGN KEY(tenant_id,run_id) REFERENCES {SCHEMA}.runs(tenant_id,run_id) ON DELETE CASCADE);"
    ))
    .await
    .expect("build shared-runner legacy run plane");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply legacy-compatible flows");
    su.batch_execute(&rewrite_schema(AUTHORING_TESTS_SQL, &schema))
        .await
        .expect("apply authoring tests");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog schema");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs(tenant_id,run_id,flow_id,flow_version,status) \
           VALUES ('t1','history-run','f',1,'completed'); \
         INSERT INTO {SCHEMA}.node_runs(tenant_id,run_id,node_id,occurrence,seq,status) \
           VALUES ('t1','history-run','n1',0,0,'success'), \
                  ('t1','history-run','n2',0,1,'success');"
    ))
    .await
    .expect("seed compatible shared history");

    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated shared-runner legacy fixture must refuse pin cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("shared-runner cutover is a database refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "execution-pin-cutover-requires-empty-run-and-release-membership"
    );

    let counts = su
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM {SCHEMA}.runs), \
                        (SELECT count(*) FROM {SCHEMA}.node_runs)"
            ),
            &[],
        )
        .await
        .expect("read refusal-preserved row counts");
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 2);
    for column in [
        "catalog_id",
        "catalog_version",
        "environment",
        "execution_bundle_hash",
    ] {
        assert!(
            !column_exists(su, "runs", column).await,
            "refusal leaves pin column {column} absent"
        );
    }
    assert!(
        !table_exists(su, SCHEMA, "effect_attempts").await,
        "refusal occurs before unrelated run-plane DDL"
    );
}

/// Retired child/wait state is removed only when every durable row is ordinary.
async fn child_run_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_child_run_state(su).await;
    seed_run_pin_parents(su, "child-cutover", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash,trigger_source, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('child-cutover','retained-run','f',1,'cat',1,'dev', \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}','event','source-run','event-root',3); \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,seq,status) \
         VALUES ('child-cutover','retained-run',0, \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}','root-node',0,'success');"
    ))
    .await
    .expect("seed retained ordinary run and frame facts");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty child state cuts over around retained ordinary facts");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::ChildRunCutover),
        "child deletion is the leading action: {:#?}",
        plan.actions
    );
    for column in [
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            !column_exists(su, "runs", column).await,
            "retired child-run column remains: {column}"
        );
    }
    for index in [
        "runs_parent_occurrence",
        "runs_invoke_root",
        "runs_waiting_child",
    ] {
        assert!(
            indexdef(su, index).await.is_none(),
            "retired index remains: {index}"
        );
    }
    let retained = su
        .query_one(
            &format!(
                "SELECT r.trigger_source, r.event_source_run_id, r.event_root_run_id, r.event_depth, \
                        n.frame_id \
                   FROM {SCHEMA}.runs AS r \
                   JOIN {SCHEMA}.node_runs AS n USING (tenant_id,run_id) \
                  WHERE r.tenant_id='child-cutover' AND r.run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained root facts");
    assert_eq!(
        retained.get::<_, Option<String>>(0).as_deref(),
        Some("event")
    );
    assert_eq!(
        retained.get::<_, Option<String>>(1).as_deref(),
        Some("source-run")
    );
    assert_eq!(
        retained.get::<_, Option<String>>(2).as_deref(),
        Some("event-root")
    );
    assert_eq!(retained.get::<_, Option<i32>>(3), Some(3));
    assert_eq!(retained.get::<_, i64>(4), 0);
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe converged child cutover")
            .actions
            .iter()
            .all(|action| action.kind != RunPlaneActionKind::ChildRunCutover),
        "child cutover is idempotent"
    );

    install_legacy_child_run_state(su).await;
    su.execute(
        &format!(
            "UPDATE {SCHEMA}.runs \
                SET waiting_child_run_id='child',waiting_child_occurrence=2,wait_generation=7 \
              WHERE tenant_id='child-cutover' AND run_id='retained-run'"
        ),
        &[],
    )
    .await
    .expect("restore a populated legacy wait fact");
    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("populated child state must refuse cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    assert_db_code(postgres, "55000", "populated child state refusal");
    for column in [
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            column_exists(su, "runs", column).await,
            "refusal atomically preserves legacy column: {column}"
        );
    }
    for index in [
        "runs_parent_occurrence",
        "runs_invoke_root",
        "runs_waiting_child",
    ] {
        assert!(
            indexdef(su, index).await.is_some(),
            "refusal atomically preserves legacy index: {index}"
        );
    }

    su.execute(
        &format!(
            "UPDATE {SCHEMA}.runs \
                SET waiting_child_run_id=NULL,waiting_child_occurrence=NULL,wait_generation=NULL \
              WHERE tenant_id='child-cutover' AND run_id='retained-run'"
        ),
        &[],
    )
    .await
    .expect("restore the refused mutation to ordinary state");
    let restored = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("restored ordinary state cuts over");
    assert_eq!(
        restored.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::ChildRunCutover)
    );
    assert!(!column_exists(su, "runs", "waiting_child_run_id").await);
}

/// Retired execution-lineage metadata is removable on a populated schema. The
/// exact legacy index is the only same-name object the cutover may destroy.
async fn rerun_lineage_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    seed_run_pin_parents(su, "rerun-cutover", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN replay_of text, ADD COLUMN root_run_id text; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,root_run_id) \
           WHERE root_run_id IS NOT NULL; \
         INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash,trigger_source,input_json,state_json,replay_of,root_run_id, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('rerun-cutover','retained-run','f',1,'cat',1,'dev', \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}','event','{{\"payload\":7}}','{{\"cursor\":9}}', \
                 'legacy-parent','legacy-root','event-source','event-root',4);"
    ))
    .await
    .expect("install populated retired rerun lineage");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("populated rerun lineage cuts over without rewriting the run");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::RerunLineageCutover),
        "rerun lineage deletion is leading: {:#?}",
        plan.actions
    );
    assert!(!column_exists(su, "runs", "replay_of").await);
    assert!(!column_exists(su, "runs", "root_run_id").await);
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(
        indexdef(su, "runs_event_root").await.is_some(),
        "event-causation traversal index survives"
    );
    let retained_row = su
        .query_one(
            &format!(
                "SELECT trigger_source,event_source_run_id,event_root_run_id,event_depth, \
                        input_json::text,state_json::text \
                   FROM {SCHEMA}.runs \
                  WHERE tenant_id='rerun-cutover' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained run after rerun-lineage cutover");
    let retained = (
        retained_row.get::<_, String>(0),
        retained_row.get::<_, String>(1),
        retained_row.get::<_, String>(2),
        retained_row.get::<_, i32>(3),
        retained_row.get::<_, String>(4),
        retained_row.get::<_, String>(5),
    );
    assert_eq!(
        retained,
        (
            "event".to_string(),
            "event-source".to_string(),
            "event-root".to_string(),
            4,
            "{\"payload\": 7}".to_string(),
            "{\"cursor\": 9}".to_string(),
        )
    );
    let retained_event_contract = su
        .query_one(
            &format!(
                "SELECT \
                   EXISTS (SELECT FROM pg_trigger t \
                     JOIN pg_class c ON c.oid=t.tgrelid \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                    WHERE n.nspname='{SCHEMA}' AND c.relname='runs' \
                      AND t.tgname='runs_event_lineage_immutable' AND NOT t.tgisinternal), \
                   EXISTS (SELECT FROM pg_constraint con \
                     JOIN pg_class c ON c.oid=con.conrelid \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                    WHERE n.nspname='{SCHEMA}' AND c.relname='runs' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_source_run_id%' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_root_run_id%' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_depth%'), \
                   has_column_privilege('wamn_app','{SCHEMA}.runs','event_source_run_id','INSERT') \
                     AND has_column_privilege('wamn_app','{SCHEMA}.runs','event_source_run_id','UPDATE') \
                     AND has_column_privilege('wamn_app','{SCHEMA}.runs','event_root_run_id','INSERT') \
                     AND has_column_privilege('wamn_app','{SCHEMA}.runs','event_root_run_id','UPDATE') \
                     AND has_column_privilege('wamn_app','{SCHEMA}.runs','event_depth','INSERT') \
                     AND has_column_privilege('wamn_app','{SCHEMA}.runs','event_depth','UPDATE')"
            ),
            &[],
        )
        .await
        .expect("read retained event-lineage contract");
    assert!(retained_event_contract.get::<_, bool>(0));
    assert!(retained_event_contract.get::<_, bool>(1));
    assert!(retained_event_contract.get::<_, bool>(2));
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe converged rerun-lineage cutover")
            .actions
            .iter()
            .all(|action| action.kind != RunPlaneActionKind::RerunLineageCutover)
    );

    // Restore the retired columns with a foreign same-name index. The action's
    // lock + exact definition guard must roll back before any column or row is
    // changed, then the canonical restored definition must cut over cleanly.
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN replay_of text, ADD COLUMN root_run_id text; \
         UPDATE {SCHEMA}.runs SET replay_of='restored-parent',root_run_id='restored-root' \
          WHERE tenant_id='rerun-cutover' AND run_id='retained-run'; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,flow_id);"
    ))
    .await
    .expect("install unknown same-name runs_root mutant");
    let before = indexdef(su, "runs_root")
        .await
        .expect("mutant index exists");
    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("unknown same-name runs_root must refuse");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    assert_db_code(postgres, "55000", "unknown runs_root refusal");
    assert!(column_exists(su, "runs", "replay_of").await);
    assert!(column_exists(su, "runs", "root_run_id").await);
    assert_eq!(
        indexdef(su, "runs_root").await.as_deref(),
        Some(before.as_str())
    );
    let lineage_row = su
        .query_one(
            &format!(
                "SELECT replay_of,root_run_id,event_root_run_id FROM {SCHEMA}.runs \
                  WHERE tenant_id='rerun-cutover' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("refusal preserved row");
    let lineage = (
        lineage_row.get::<_, Option<String>>(0),
        lineage_row.get::<_, Option<String>>(1),
        lineage_row.get::<_, Option<String>>(2),
    );
    assert_eq!(
        lineage,
        (
            Some("restored-parent".to_string()),
            Some("restored-root".to_string()),
            Some("event-root".to_string()),
        )
    );

    su.batch_execute(&format!(
        "DROP INDEX {SCHEMA}.runs_root; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,root_run_id) \
           WHERE root_run_id IS NOT NULL;"
    ))
    .await
    .expect("restore canonical legacy index definition");
    let restored = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("restored canonical legacy shape cuts over");
    assert_eq!(
        restored.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::RerunLineageCutover)
    );
    assert!(!column_exists(su, "runs", "replay_of").await);
    assert!(!column_exists(su, "runs", "root_run_id").await);
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(indexdef(su, "runs_event_root").await.is_some());
}

/// A populated queue is retained when no worker holds a live lease. Only the
/// retired partition state is removed, and the global FIFO claim index lands
/// in its exact record shape.
async fn partition_plane_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_partition_plane(su).await;
    seed_run_pin_parents(su, "partition", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash) \
         VALUES ('partition','retained-run','f',1,'cat',1,'dev', \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}'); \
         INSERT INTO {SCHEMA}.run_queue \
           (tenant_id,run_id,partition_key,partition_policy,stream_seq) \
         VALUES ('partition','retained-run','serial','blocking',7);"
    ))
    .await
    .expect("seed an unleased legacy queue row");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("drained partition plane converges");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::PartitionPlaneCutover),
        "partition deletion is the leading action: {:#?}",
        plan.actions
    );
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::PartitionPlaneCutover)
            .count(),
        1
    );

    let columns: Vec<String> = su
        .query(
            "SELECT column_name FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name='run_queue' \
              ORDER BY ordinal_position",
            &[&SCHEMA],
        )
        .await
        .expect("read global queue columns")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        columns,
        [
            "tenant_id",
            "run_id",
            "priority",
            "available_at",
            "stream_seq",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
            "attempts",
            "max_attempts",
            "enqueued_at",
        ]
        .map(str::to_string)
    );
    let retained = su
        .query_one(
            &format!(
                "SELECT stream_seq,lease_owner,lease_generation,attempts \
                   FROM {SCHEMA}.run_queue \
                  WHERE tenant_id='partition' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained queue row");
    assert_eq!(retained.get::<_, i64>(0), 7);
    assert_eq!(retained.get::<_, Option<String>>(1), None);
    assert_eq!(retained.get::<_, i64>(2), 0);
    assert_eq!(retained.get::<_, i32>(3), 0);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
    assert!(indexdef(su, "run_queue_partition").await.is_none());
    let claimable = indexdef(su, "run_queue_claimable")
        .await
        .expect("global claim index exists");
    assert!(
        claimable.contains("(tenant_id, available_at, stream_seq, run_id, lease_expires_at)"),
        "global FIFO index shape: {claimable}"
    );
    assert!(
        !claimable.contains("WHERE"),
        "claim index is global: {claimable}"
    );

    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("partition cutover idempotence");
    assert!(
        again.is_noop(),
        "second partition reconcile: {:#?}",
        again.actions
    );
}

/// Either source of live partition ownership refuses before any DDL or later
/// role repair. The fixed-lock cutover is therefore safe against both queue and
/// owner-table workers.
async fn partition_plane_active_lease_refusal_leg(su: &Client) {
    for lease_source in ["run_queue", "partition_owner"] {
        reset(su).await;
        install_current_run_plane(su).await;
        install_legacy_partition_plane(su).await;
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("install later authority-repair sentinel");
        match lease_source {
            "run_queue" => {
                seed_run_pin_parents(su, "leased", "cat", 1, "dev").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                        environment,execution_bundle_hash) \
                     VALUES ('leased','active-run','f',1,'cat',1,'dev', \
                             '{EMPTY_EXECUTION_BUNDLE_HASH}'); \
                     INSERT INTO {SCHEMA}.run_queue \
                       (tenant_id,run_id,lease_owner,lease_expires_at) \
                     VALUES ('leased','active-run','worker','infinity');"
                ))
                .await
                .expect("seed active queue lease");
            }
            "partition_owner" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.partition_owner \
                       (tenant_id,partition_key,lease_owner,lease_expires_at) \
                     VALUES ('leased','serial','worker','infinity');"
                ))
                .await
                .expect("seed active partition-owner lease");
            }
            _ => unreachable!(),
        }

        let before = partition_plane_schema_snapshot(su).await;
        let dry = reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("active lease dry-run plans a refusal guard");
        assert_eq!(
            dry.actions.first().map(|action| action.kind),
            Some(RunPlaneActionKind::PartitionPlaneCutover)
        );
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("active partition lease refuses cutover");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed lease refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(
            database.message(),
            "partition-plane-cutover-requires-drained-workers"
        );
        assert_eq!(
            partition_plane_schema_snapshot(su).await,
            before,
            "{lease_source}: active-lease refusal leaves the schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
                &[],
            )
            .await
            .expect("read authority-repair sentinel")
            .get(0);
        assert!(
            membership_retained,
            "{lease_source}: leading refusal precedes unrelated repair"
        );
    }
}

/// A populated legacy table whose lease state cannot be read is not assumed
/// drained. The cutover refuses byte-for-byte before DDL; once the ambiguous
/// table is empty, the same partial schema converges to the retained record.
async fn partition_plane_unobservable_lease_refusal_leg(su: &Client) {
    for lease_source in ["run_queue", "partition_owner"] {
        reset(su).await;
        install_current_run_plane(su).await;
        install_legacy_partition_plane(su).await;
        let expected_message = match lease_source {
            "run_queue" => {
                seed_run_pin_parents(su, "ambiguous", "cat", 1, "dev").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                        environment,execution_bundle_hash) \
                     VALUES ('ambiguous','queue-run','f',1,'cat',1,'dev', \
                             '{EMPTY_EXECUTION_BUNDLE_HASH}'); \
                     INSERT INTO {SCHEMA}.run_queue (tenant_id,run_id,partition_key) \
                     VALUES ('ambiguous','queue-run','serial'); \
                     ALTER TABLE {SCHEMA}.run_queue DROP COLUMN lease_owner;"
                ))
                .await
                .expect("seed populated queue with unobservable lease shape");
                "partition-plane-cutover-requires-observable-run-queue-leases-or-empty-queue"
            }
            "partition_owner" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.partition_owner \
                       (tenant_id,partition_key,lease_owner,lease_expires_at) \
                     VALUES ('ambiguous','serial','worker','infinity'); \
                     ALTER TABLE {SCHEMA}.partition_owner DROP COLUMN lease_expires_at;"
                ))
                .await
                .expect("seed populated owner table with unobservable lease shape");
                "partition-plane-cutover-requires-observable-partition-leases-or-empty-owner-table"
            }
            _ => unreachable!(),
        };

        let before = partition_plane_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("unobservable populated lease shape refuses cutover");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed lease-shape refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(database.message(), expected_message);
        assert_eq!(
            partition_plane_schema_snapshot(su).await,
            before,
            "{lease_source}: ambiguous lease refusal rolls back every DDL change"
        );

        su.batch_execute(&format!("DELETE FROM {SCHEMA}.{lease_source}"))
            .await
            .expect("drain ambiguous legacy table");
        reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect("empty partial lease shape converges");
        assert!(!table_exists(su, SCHEMA, "partition_owner").await);
        assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
        assert!(!column_exists(su, "run_queue", "partition_key").await);
        assert!(!column_exists(su, "run_queue", "partition_policy").await);
        assert!(column_exists(su, "run_queue", "lease_owner").await);
        assert!(column_exists(su, "run_queue", "lease_expires_at").await);
    }
}

/// Retired dead-letter history has no in-place conversion. Any retained row
/// requires an archive or whole-environment reprovision and rolls the cutover
/// back before a table, column, index, CHECK, or unrelated grant changes.
async fn partition_plane_dead_letter_refusal_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_partition_plane(su).await;
    seed_run_pin_parents(su, "dead-letter", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash) \
         VALUES ('dead-letter','failed-run','f',1,'cat',1,'dev', \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}'); \
         INSERT INTO {SCHEMA}.run_dead_letters \
           (tenant_id,run_id,partition_key,flow_id,reason) \
         VALUES ('dead-letter','failed-run','serial','f','legacy-history'); \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("seed retained dead-letter history");

    let before = partition_plane_schema_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("dead-letter dry-run plans the guarded cutover");
    let cutover = dry.actions.first().expect("leading partition cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    let guard = cutover
        .sql
        .find("retired-run-dead-letter-history-requires-archive-or-environment-reprovision")
        .expect("archive-or-reprovision refusal diagnostic");
    let destructive = cutover
        .sql
        .find("DROP INDEX")
        .expect("guarded destructive statements");
    assert!(
        guard < destructive,
        "history guard precedes destructive DDL"
    );

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("nonempty retired dead-letter history refuses cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres.as_db_error().expect("typed history refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "retired-run-dead-letter-history-requires-archive-or-environment-reprovision"
    );
    assert_eq!(
        partition_plane_schema_snapshot(su).await,
        before,
        "history refusal leaves the partition schema unchanged"
    );
    assert_eq!(
        su.query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.run_dead_letters"),
            &[],
        )
        .await
        .expect("dead-letter history remains")
        .get::<_, i64>(0),
        1
    );
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read authority-repair sentinel")
        .get(0);
    assert!(membership_retained, "history refusal is the leading action");
}

/// Manifestations 1 + 4: the 2jkm.41-sweep drift set plus the outbox era.
async fn v1_era_drifted_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");

    // Current-era runs/node_runs/flows (the drift was queue-side)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    // …and a partially converged v1-era queue: no stream_seq or lease
    // generation, one remaining partition-key column, the pre-E4 claimable
    // index, no retired whole tables, plus the outbox-era objects and a stored
    // registration carrying both retired declaration keys.
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
    seed_run_pin_parents(su, "t1", "cat", 1, "dev").await;
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ('t1', 'cat', 'r1', 'f', 'e', \
                 $1::text::jsonb)",
        &[&r#"{"registration-id":"r1","partition-key":"serial","retained":"yes","state":"shadow"}"#],
    )
    .await
    .expect("seed a retired-key registration");
    // A pre-existing queue row: the ADD COLUMN defaults must land on it.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment,execution_bundle_hash) \
             VALUES ('t1','r-old','f',1,'cat',1,'dev','{EMPTY_EXECUTION_BUNDLE_HASH}'); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r-old');"
    ))
    .await
    .expect("seed a pre-drift queue row");

    let partial = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("partially converged partition plane plans");
    let cutover = partial.actions.first().expect("leading partial cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(cutover.sql.contains("DROP COLUMN IF EXISTS partition_key"));
    assert!(
        !cutover.sql.contains("run_queue_claimable"),
        "cutover defers the index until stream_seq exists"
    );
    assert!(partial.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::AddColumn && action.target == "run_queue.stream_seq"
    }));
    assert!(partial.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RecreateIndex && action.target == "run_queue_claimable"
    }));

    // The REAL CLI path (arg validation + connect + apply + print).
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        admin_database_url: url.to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane applies");

    // Retained column drift closed, partition residue removed, and defaults
    // landed on the pre-existing row.
    assert!(
        column_exists(su, "run_queue", "stream_seq").await,
        "stream_seq added"
    );
    assert!(
        column_exists(su, "run_queue", "lease_generation").await,
        "lease_generation added"
    );
    assert!(!column_exists(su, "run_queue", "partition_key").await);
    assert!(!column_exists(su, "run_queue", "partition_policy").await);
    let row = su
        .query_one(
            &format!(
                "SELECT stream_seq, lease_generation FROM {SCHEMA}.run_queue \
                 WHERE tenant_id = 't1' AND run_id = 'r-old'"
            ),
            &[],
        )
        .await
        .expect("read the pre-drift row");
    assert_eq!(row.get::<_, i64>(0), 0, "stream_seq default backfilled");
    assert_eq!(row.get::<_, i64>(1), 0, "lease generation backfilled");

    // The claimable index was recreated only after the retained columns landed.
    let def = indexdef(su, "run_queue_claimable")
        .await
        .expect("claimable index present");
    assert!(
        def.contains("(tenant_id, available_at, stream_seq, run_id, lease_expires_at)"),
        "global FIFO claimable index: {def}"
    );
    assert!(
        !def.contains("WHERE"),
        "global claim index is not partial: {def}"
    );
    assert!(indexdef(su, "run_queue_partition").await.is_none());

    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);

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

    // Both retired keys are stripped; every retained document key survives.
    let registration: String = su
        .query_one(
            "SELECT registration::text FROM catalog.event_registrations \
              WHERE registration_id = 'r1'",
            &[],
        )
        .await
        .expect("read registration")
        .get(0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&registration).expect("parse registration"),
        serde_json::json!({"registration-id": "r1", "retained": "yes"})
    );

    // Idempotence: a second reconcile plans nothing.
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

/// Manifestation 2 (the live poc_f1 case): run-state + flows present, queue
/// wholly absent — the single global FIFO queue appears and its FK resolves.
async fn queue_missing_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile applies");
    assert!(!plan.is_noop());

    assert!(table_exists(su, SCHEMA, "run_queue").await);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
    // The FK to runs resolves: a run then its queue row insert cleanly.
    seed_run_pin_parents(su, "t1", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment,execution_bundle_hash) \
             VALUES ('t1','r1','f',1,'cat',1,'dev','{EMPTY_EXECUTION_BUNDLE_HASH}'); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("FK insert path");

    let claimable = indexdef(su, "run_queue_claimable")
        .await
        .expect("global FIFO claim index");
    assert!(claimable.contains("(tenant_id, available_at, stream_seq, run_id, lease_expires_at)"));
}

/// Manifestations 3 + 5 + 6 (the ephemeral-fixture wipe): a bare database —
/// neither the `wamn_app` nor host-only author role. Dry-run first (strictly
/// read-only), then the
/// apply provisions run plane + `catalog`, and a functional smoke as
/// `wamn_app` proves grants + RLS isolation from the applied sections.
async fn from_zero_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(
        "DROP OWNED BY wamn_app; \
         DROP ROLE wamn_app; \
         DROP OWNED BY wamn_scenario_author; \
         DROP ROLE wamn_scenario_author;",
    )
    .await
    .expect("remove the runtime role (bare database)");

    // --dry-run is STRICTLY read-only: it neither creates the role nor tables.
    let dry = reconcile_run_plane::reconcile(su, &schema, false)
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
    let author_role_exists: bool = su
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_roles \
             WHERE rolname = 'wamn_scenario_author')",
            &[],
        )
        .await
        .expect("probe author role")
        .get(0);
    assert!(
        !author_role_exists,
        "dry-run does not create the author role"
    );
    assert!(
        !table_exists(su, SCHEMA, "runs").await,
        "dry-run creates nothing"
    );

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("from-zero reconcile applies");
    assert!(!plan.is_noop());

    for t in [
        "runs",
        "node_runs",
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "operator_run_actions",
        "flows",
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
        "run_queue",
    ] {
        assert!(
            table_exists(su, SCHEMA, t).await,
            "run-plane table {t} provisioned"
        );
    }
    for column in [
        "replay_of",
        "root_run_id",
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            !column_exists(su, "runs", column).await,
            "from-zero schema retains retired child-run column: {column}"
        );
    }
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(
        table_exists(su, "catalog", "event_registrations").await,
        "catalog schema provisioned"
    );
    for table in [
        "flow_drafts",
        "validated_flow_drafts",
        "draft_safe_connection_grants",
        "authoring_command_audit",
    ] {
        assert!(
            table_exists(su, "catalog", table).await,
            "catalog authoring table {table} provisioned"
        );
    }

    // Functional smoke as the runtime role: the sections' grants + RLS hold.
    seed_run_pin_parents(su, "t1", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "SET ROLE wamn_app; \
         SELECT set_config('app.tenant', 't1', false); \
         INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment,execution_bundle_hash) \
             VALUES ('t1','r1','f',1,'cat',1,'dev','{EMPTY_EXECUTION_BUNDLE_HASH}'); \
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
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(AUTHORING_TESTS_SQL, &schema))
        .await
        .expect("apply authoring tests");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");

    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("dry-run plans");
    assert!(
        dry.is_noop(),
        "current schema dry-run is a no-op: {:#?}",
        dry.actions
    );
    assert_eq!(
        dry.at_target.len(),
        14,
        "all fourteen run-plane tables at target"
    );

    let apply = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("apply-mode reconcile");
    assert!(
        apply.is_noop(),
        "current schema apply is a no-op: {:#?}",
        apply.actions
    );
}

async fn capture_mode_additive_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(AUTHORING_TESTS_SQL, &schema))
        .await
        .expect("apply authoring tests");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    seed_run_pin_parents(su, "t1", "capture", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','legacy-off','f',1,'capture',1,'dev', \
                 '{EMPTY_EXECUTION_BUNDLE_HASH}','completed'); \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,current_plan_hash,local_node_id,seq,status,output_size) \
         VALUES ('t1','legacy-off','{EMPTY_EXECUTION_BUNDLE_HASH}','legacy-node',0,'success',321); \
         DROP TRIGGER runs_admission_pins_immutable ON {SCHEMA}.runs; \
         ALTER TABLE {SCHEMA}.runs \
           DROP CONSTRAINT runs_capture_mode_source_check, \
           DROP COLUMN capture_mode; \
         ALTER TABLE {SCHEMA}.node_runs RENAME COLUMN output_size TO payload_size; \
         ALTER TABLE {SCHEMA}.node_runs \
           ADD COLUMN preview_head text, \
           ADD COLUMN capture_mode text, \
           ADD COLUMN redacted boolean NOT NULL DEFAULT false;"
    ))
    .await
    .expect("build populated pre-capture carrier schema");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("capture carrier reconcile applies to populated history");
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::AddColumn && action.target == "runs.capture_mode"
    }));
    assert!(
        plan.actions
            .iter()
            .any(|action| { action.kind == RunPlaneActionKind::CaptureProjectionCutover })
    );
    let mode: String = su
        .query_one(
            &format!("SELECT capture_mode FROM {SCHEMA}.runs WHERE run_id='legacy-off'"),
            &[],
        )
        .await
        .expect("read legacy defaulted mode")
        .get(0);
    assert_eq!(mode, "off");
    for column in ["preview_head", "capture_mode", "redacted"] {
        assert!(
            !column_exists(su, "node_runs", column).await,
            "retired node capture column {column} removed"
        );
    }
    assert!(column_exists(su, "node_runs", "output_size").await);
    assert!(!column_exists(su, "node_runs", "payload_size").await);
    let preserved_size: i64 = su
        .query_one(
            &format!(
                "SELECT output_size FROM {SCHEMA}.node_runs \
                 WHERE run_id='legacy-off' AND local_node_id='legacy-node'"
            ),
            &[],
        )
        .await
        .expect("legacy output size was preserved by rename")
        .get(0);
    assert_eq!(preserved_size, 321);

    let immutable = su
        .execute(
            &format!("UPDATE {SCHEMA}.runs SET capture_mode='full' WHERE run_id='legacy-off'"),
            &[],
        )
        .await
        .expect_err("post-admission capture mutation refused");
    assert_db_code(immutable, "55000", "capture mode is admission-immutable");
    let invalid = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                    execution_bundle_hash,status,trigger_source,capture_mode) \
                 VALUES ('t1','published-full','f',1,'capture',1,'dev', \
                         '{EMPTY_EXECUTION_BUNDLE_HASH}','completed','http','full')"
            ),
            &[],
        )
        .await
        .expect_err("published full capture refused");
    assert_db_code(invalid, "23514", "only direct draft rows may capture full");
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                execution_bundle_hash,status,trigger_source,capture_mode) \
             VALUES ('t1','draft-full','f',1,'capture',1,'dev', \
                     '{EMPTY_EXECUTION_BUNDLE_HASH}','completed','scenario-draft','full')"
        ),
        &[],
    )
    .await
    .expect("canonical direct draft may carry full");

    let app = connect_as(url, "wamn_app", "wamn_app").await;
    app.batch_execute("SELECT set_config('app.tenant','t1',false)")
        .await
        .expect("enter application tenant for capture authority probes");
    let default_mode: String = app
        .query_one(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                    execution_bundle_hash,status,trigger_source) \
                 VALUES ('t1','app-default-off','f',1,'capture',1,'dev', \
                         '{EMPTY_EXECUTION_BUNDLE_HASH}','dispatched','test') \
                 RETURNING capture_mode"
            ),
            &[],
        )
        .await
        .expect("application admission may omit capture mode and take the safe default")
        .get(0);
    assert_eq!(default_mode, "off");

    let explicit_capture = app
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                    execution_bundle_hash,status,trigger_source,capture_mode) \
                 VALUES ('t1','app-forged-full','f',1,'capture',1,'dev', \
                         '{EMPTY_EXECUTION_BUNDLE_HASH}','dispatched','scenario-draft','full')"
            ),
            &[],
        )
        .await
        .expect_err("application role cannot name capture mode during admission");
    assert_db_code(
        explicit_capture,
        "42501",
        "application capture-mode insert refusal",
    );

    let capture_update = app
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET capture_mode='off' \
                 WHERE run_id='app-default-off'"
            ),
            &[],
        )
        .await
        .expect_err("application role cannot update capture mode after admission");
    assert_db_code(
        capture_update,
        "42501",
        "application capture-mode update refusal",
    );

    let ordinary_update = app
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET status='running', updated_at=now() \
                 WHERE run_id='app-default-off'"
            ),
            &[],
        )
        .await
        .expect("application role retains ordinary run-state update authority");
    assert_eq!(ordinary_update, 1);

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("capture carrier second reconcile plans");
    assert!(
        again.is_noop(),
        "capture carrier converged: {:#?}",
        again.actions
    );
}

async fn invocation_admission_retention_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(AUTHORING_TESTS_SQL, &schema))
        .await
        .expect("apply authoring tests");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.invocation_admissions \
           ADD COLUMN expires_at timestamptz NOT NULL DEFAULT (now() + interval '1 day'), \
           ALTER COLUMN client_key_digest SET NOT NULL; \
         CREATE INDEX invocation_admissions_expiry \
           ON {SCHEMA}.invocation_admissions (tenant_id, expires_at);"
    ))
    .await
    .expect("build legacy invocation-retention carrier");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("invocation retention cutover applies");
    let cutovers = plan
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::InvocationAdmissionRetentionCutover)
        .collect::<Vec<_>>();
    assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);
    assert!(
        !column_exists(su, "invocation_admissions", "expires_at").await,
        "admission expiry column removed"
    );
    let nullable: bool = su
        .query_one(
            "SELECT is_nullable = 'YES' FROM information_schema.columns \
             WHERE table_schema=$1 AND table_name='invocation_admissions' \
               AND column_name='client_key_digest'",
            &[&SCHEMA],
        )
        .await
        .expect("read invocation client-key nullability")
        .get(0);
    assert!(nullable, "client key is optional at the storage boundary");
    assert!(
        indexdef(su, "invocation_admissions_expiry").await.is_none(),
        "admission expiry index removed"
    );

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("invocation retention second reconcile plans");
    assert!(
        again.is_noop(),
        "invocation retention converged: {:#?}",
        again.actions
    );
}

async fn stored_suite_cutover_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog before stored-suite cutover");
    for ddl in [RUN_STATE_SQL, FLOWS_SQL, AUTHORING_TESTS_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run plane before stored-suite cutover");
    }
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.test_suites (id int PRIMARY KEY); \
         CREATE TABLE {SCHEMA}.test_cases ( \
           id int PRIMARY KEY, suite_id int REFERENCES {SCHEMA}.test_suites(id)); \
         CREATE TABLE {SCHEMA}.authoring_report_reservations (id int PRIMARY KEY); \
         CREATE TABLE {SCHEMA}.authoring_suite_case_facts ( \
           id int PRIMARY KEY, reservation_id int \
             REFERENCES {SCHEMA}.authoring_report_reservations(id)); \
         CREATE TABLE {SCHEMA}.authoring_suite_reports ( \
           id int PRIMARY KEY, reservation_id int \
             REFERENCES {SCHEMA}.authoring_report_reservations(id)); \
         CREATE FUNCTION {SCHEMA}.guard_authoring_report_write() RETURNS trigger \
           LANGUAGE plpgsql AS $guard$ BEGIN RETURN NEW; END $guard$; \
         CREATE FUNCTION {SCHEMA}.reject_immutable_authoring_report_change() RETURNS trigger \
           LANGUAGE plpgsql AS $guard$ BEGIN RETURN NEW; END $guard$; \
         CREATE TRIGGER authoring_report_reservations_guard \
           BEFORE INSERT ON {SCHEMA}.authoring_report_reservations \
           FOR EACH ROW EXECUTE FUNCTION {SCHEMA}.guard_authoring_report_write(); \
         CREATE TRIGGER authoring_suite_reports_immutable \
           BEFORE UPDATE ON {SCHEMA}.authoring_suite_reports \
           FOR EACH ROW EXECUTE FUNCTION \
             {SCHEMA}.reject_immutable_authoring_report_change(); \
         CREATE TABLE catalog.publish_gate_audit (audit_id int PRIMARY KEY); \
         INSERT INTO catalog.publish_gate_audit VALUES (1);"
    ))
    .await
    .expect("install retired stored-suite persistence");
    su.batch_execute(
        "ALTER TABLE catalog.validated_flow_drafts \
           DROP CONSTRAINT validated_flow_drafts_exact_pin, \
           ADD COLUMN suite_flow_version int NOT NULL DEFAULT 1 \
             CHECK (suite_flow_version > 0), \
           ADD CONSTRAINT validated_flow_drafts_exact_pin UNIQUE ( \
             tenant_id,draft_id,draft_revision,draft_content_hash,catalog_id, \
             catalog_version,environment,suite_flow_version,runtime_flow_version, \
             draft_artifact_hash,execution_bundle_hash,binding_base_artifact_hash); \
         ALTER TABLE catalog.authoring_command_audit \
           DROP CONSTRAINT authoring_command_audit_pkey, \
           DROP CONSTRAINT authoring_command_audit_audit_id_key, \
           DROP CONSTRAINT authoring_command_audit_command_kind_check, \
           DROP CONSTRAINT authoring_command_audit_request_hash_check, \
           DROP CONSTRAINT authoring_command_audit_outcome_present, \
           DROP COLUMN request_hash, \
           DROP COLUMN outcome_bytes, \
           ADD CONSTRAINT authoring_command_audit_pkey PRIMARY KEY (tenant_id,audit_id), \
           ADD CONSTRAINT authoring_command_audit_command_kind_check CHECK ( \
             command_kind IN ('save-flow-draft','validate','draft-run','suite-run', \
                              'publish','suite-projection', \
                              'grant-draft-safe-generation', \
                              'revoke-draft-safe-generation'))",
    )
    .await
    .expect("restore retired persisted authoring protocol");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("stored-suite cutover applies");
    let cutovers = plan
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::StoredSuiteCutover)
        .collect::<Vec<_>>();
    assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);

    for table in [
        "authoring_suite_reports",
        "authoring_suite_case_facts",
        "authoring_report_reservations",
        "test_cases",
        "test_suites",
    ] {
        assert!(
            !table_exists(su, SCHEMA, table).await,
            "retired table {table} is absent"
        );
    }
    assert!(
        !table_exists(su, "catalog", "publish_gate_audit").await,
        "populated retired publish-gate audit is absent"
    );
    for table in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(
            table_exists(su, SCHEMA, table).await,
            "retained table {table} survives"
        );
    }
    let retired_functions: i64 = su
        .query_one(
            "SELECT count(*) FROM pg_proc AS proc \
             JOIN pg_namespace AS namespace ON namespace.oid = proc.pronamespace \
             WHERE namespace.nspname = $1 \
               AND proc.proname IN \
                 ('guard_authoring_report_write', \
                  'reject_immutable_authoring_report_change')",
            &[&SCHEMA],
        )
        .await
        .expect("count retired stored-suite functions")
        .get(0);
    assert_eq!(retired_functions, 0);
    assert!(
        !catalog_column_exists(su, "validated_flow_drafts", "suite_flow_version").await,
        "retired validation dimension is absent"
    );
    let command_check: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid, true) \
               FROM pg_constraint AS con \
               JOIN pg_class AS relation ON relation.oid = con.conrelid \
               JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname='catalog' \
                AND relation.relname='authoring_command_audit' \
                AND con.conname='authoring_command_audit_command_kind_check'",
            &[],
        )
        .await
        .expect("read narrowed authoring command CHECK")
        .get(0);
    assert!(!command_check.contains("suite-run"), "{command_check}");
    assert!(
        !command_check.contains("suite-projection"),
        "{command_check}"
    );

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("stored-suite cutover second reconcile plans");
    assert!(
        again.is_noop(),
        "stored-suite cutover converged: {:#?}",
        again.actions
    );

    su.batch_execute(
        "ALTER TABLE catalog.validated_flow_drafts \
           DROP CONSTRAINT validated_flow_drafts_exact_pin, \
           ADD COLUMN suite_flow_version int NOT NULL DEFAULT 1 \
             CHECK (suite_flow_version > 0), \
           ADD CONSTRAINT validated_flow_drafts_exact_pin UNIQUE ( \
             tenant_id,draft_id,draft_revision,draft_content_hash,catalog_id, \
             catalog_version,environment,suite_flow_version,runtime_flow_version, \
             draft_artifact_hash,execution_bundle_hash,binding_base_artifact_hash); \
         INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version) \
           VALUES ('legacy-authoring','cat',1,'dev','0.1'); \
         INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
           VALUES ('legacy-authoring','sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855','0.1',''::bytea,0); \
         INSERT INTO catalog.validated_flow_drafts ( \
           tenant_id,draft_id,draft_revision,draft_edited_at,draft_content_hash, \
           catalog_id,catalog_version,environment,suite_flow_version,flow_id, \
           runtime_flow_version,graph_json,graph_hash,draft_artifact_hash, \
           execution_bundle_hash,binding_base_artifact_hash,validated_draft_hash) \
         VALUES ( \
           'legacy-authoring','draft',1,now(),'content','cat',1,'dev',1,'flow', \
           1,'{}'::jsonb,'graph','artifact', \
           'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
           'base','validated')",
    )
    .await
    .expect("seed populated retired validation identity");
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated retired validation identity must refuse");
    assert_db_code(
        error.downcast().expect("postgres refusal"),
        "55000",
        "retired validation identity cutover",
    );
    assert!(
        catalog_column_exists(su, "validated_flow_drafts", "suite_flow_version").await,
        "refusal rolls back the catalog cutover"
    );

    reset(su).await;
    reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("install canonical state for retired command-history refusal");
    su.batch_execute(
        "ALTER TABLE catalog.validated_flow_drafts \
           DROP CONSTRAINT validated_flow_drafts_exact_pin, \
           ADD COLUMN suite_flow_version int NOT NULL DEFAULT 1 \
             CHECK (suite_flow_version > 0), \
           ADD CONSTRAINT validated_flow_drafts_exact_pin UNIQUE ( \
             tenant_id,draft_id,draft_revision,draft_content_hash,catalog_id, \
             catalog_version,environment,suite_flow_version,runtime_flow_version, \
             draft_artifact_hash,execution_bundle_hash,binding_base_artifact_hash); \
         ALTER TABLE catalog.authoring_command_audit \
           DROP CONSTRAINT authoring_command_audit_pkey, \
           DROP CONSTRAINT authoring_command_audit_audit_id_key, \
           DROP CONSTRAINT authoring_command_audit_command_kind_check, \
           DROP CONSTRAINT authoring_command_audit_request_hash_check, \
           DROP CONSTRAINT authoring_command_audit_outcome_present, \
           DROP COLUMN request_hash, \
           DROP COLUMN outcome_bytes, \
           ADD CONSTRAINT authoring_command_audit_pkey PRIMARY KEY (tenant_id,audit_id), \
           ADD CONSTRAINT authoring_command_audit_command_kind_check CHECK ( \
             command_kind IN ('save-flow-draft','validate','draft-run','suite-run', \
                              'publish','suite-projection', \
                              'grant-draft-safe-generation', \
                              'revoke-draft-safe-generation')); \
         INSERT INTO catalog.authoring_command_audit ( \
           tenant_id,command_id,command_kind,principal_id,principal_kind, \
           principal_subject,effective_role,org,project,environment,target_ref) \
         VALUES \
           ('legacy-authoring','legacy-grant','grant-draft-safe-generation', \
            'principal','human','author@example.com','project-author', \
            'org','project','dev','legacy'), \
           ('legacy-authoring','legacy-revoke','revoke-draft-safe-generation', \
            'principal','human','author@example.com','project-author', \
            'org','project','dev','legacy')",
    )
    .await
    .expect("seed retired immutable authoring command history");
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("retired immutable authoring command history must refuse");
    let database_error: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    assert_eq!(
        database_error.as_db_error().map(|error| error.message()),
        Some(
            "authoring-command-retry-ledger-cutover-requires-empty-audit-or-archive-and-reprovision"
        )
    );
    assert_db_code(
        database_error,
        "55000",
        "populated legacy authoring retry-ledger cutover",
    );
    assert!(
        catalog_column_exists(su, "validated_flow_drafts", "suite_flow_version").await,
        "later history refusal rolls back the earlier validation-column cutover"
    );
    let command_check: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid, true) \
               FROM pg_constraint AS con \
               JOIN pg_class AS relation ON relation.oid = con.conrelid \
               JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname='catalog' \
                AND relation.relname='authoring_command_audit' \
                AND con.conname='authoring_command_audit_command_kind_check'",
            &[],
        )
        .await
        .expect("read rolled-back authoring command CHECK")
        .get(0);
    assert!(command_check.contains("suite-run"), "{command_check}");
}

/// PLAN 6A additive storage and the host/guest authority boundary. This proves
/// both ctl provisioning paths add their retained sections, then exercises
/// adversarial direct, inherited, membership, and ownership authority drift.
async fn authoring_storage_authority_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    let legacy_catalog = without_marked_section(
        CATALOG_SCHEMA_SQL,
        "-- BEGIN AUTHORING DRAFT STORAGE MIGRATION",
        "-- END AUTHORING DRAFT STORAGE MIGRATION",
    );
    let legacy_catalog = without_marked_section(
        &legacy_catalog,
        "-- BEGIN AUTHORING CONNECTION AUTHORITY MIGRATION",
        "-- END AUTHORING CONNECTION AUTHORITY MIGRATION",
    );
    let legacy_catalog = without_marked_section(
        &legacy_catalog,
        "-- BEGIN AUTHORING COMMAND AUDIT MIGRATION",
        "-- END AUTHORING COMMAND AUDIT MIGRATION",
    );
    // Both wamn-ftfc.2 blocks alter tables the three stripped sections own, so
    // they cannot survive into a catalog that predates those tables.
    let legacy_catalog = without_marked_section(
        &legacy_catalog,
        "-- BEGIN AUTHORING DRAFT DEFINITION MIGRATION",
        "-- END AUTHORING DRAFT DEFINITION MIGRATION",
    );
    let legacy_catalog = without_marked_section(
        &legacy_catalog,
        "-- BEGIN AUTHORING COMMAND PROVENANCE MIGRATION",
        "-- END AUTHORING COMMAND PROVENANCE MIGRATION",
    );
    su.batch_execute(&legacy_catalog)
        .await
        .expect("apply pre-authoring catalog storage");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state before authoring additive upgrade");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows before authoring additive upgrade");
    for table in [
        "flow_drafts",
        "validated_flow_drafts",
        "draft_safe_connection_grants",
        "authoring_command_audit",
    ] {
        assert!(
            !table_exists(su, "catalog", table).await,
            "legacy catalog omits {table}"
        );
    }
    for table in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(
            !table_exists(su, SCHEMA, table).await,
            "legacy run plane omits {table}"
        );
    }

    wamn_ctl::publish_catalog::ensure_catalog_storage(su)
        .await
        .expect("additively install catalog authoring storage");
    assert!(
        wamn_ctl::publish_catalog::ensure_flow_tests(su, &schema)
            .await
            .expect("additively install authoring-test storage"),
        "the first additive authoring-test install reports installation"
    );
    assert!(
        !wamn_ctl::publish_catalog::ensure_flow_tests(su, &schema)
            .await
            .expect("reapply authoring-test storage"),
        "the additive authoring-test install is idempotent"
    );
    for table in [
        "flow_drafts",
        "validated_flow_drafts",
        "draft_safe_connection_grants",
        "authoring_command_audit",
    ] {
        assert!(
            table_exists(su, "catalog", table).await,
            "upgrade creates {table}"
        );
    }

    // wamn-ftfc.2's two upgrades add columns to tables that already exist, so
    // the from-zero path above never exercises their probes. Synthesize the
    // pre-upgrade shape on the installed catalog and publish again: that is the
    // exact path an existing project takes.
    su.batch_execute(
        "ALTER TABLE catalog.flow_drafts \
             DROP CONSTRAINT flow_drafts_content_present; \
         ALTER TABLE catalog.flow_drafts DROP COLUMN definition; \
         ALTER TABLE catalog.flow_drafts ALTER COLUMN graph_json SET NOT NULL; \
         ALTER TABLE catalog.authoring_command_audit \
             DROP CONSTRAINT authoring_command_audit_provenance_check; \
         ALTER TABLE catalog.authoring_command_audit \
             DROP COLUMN provenance_commit, \
             DROP COLUMN provenance_ref, \
             DROP COLUMN provenance_dirty;",
    )
    .await
    .expect("synthesize a catalog that predates wamn-ftfc.2");
    for (table, column) in [
        ("flow_drafts", "definition"),
        ("authoring_command_audit", "provenance_commit"),
    ] {
        assert!(
            !catalog_column_exists(su, table, column).await,
            "synthesized legacy catalog still has catalog.{table}.{column}"
        );
    }
    assert!(
        !catalog_column_is_nullable(su, "flow_drafts", "graph_json").await,
        "synthesized legacy catalog must still require a parsed document"
    );

    wamn_ctl::publish_catalog::ensure_catalog_storage(su)
        .await
        .expect("additively install the wamn-ftfc.2 column upgrades");
    for (table, column) in [
        ("flow_drafts", "definition"),
        ("authoring_command_audit", "provenance_commit"),
        ("authoring_command_audit", "provenance_ref"),
        ("authoring_command_audit", "provenance_dirty"),
    ] {
        assert!(
            catalog_column_exists(su, table, column).await,
            "upgrade creates catalog.{table}.{column}"
        );
    }
    // Exact submitted text can only be stored once the retired parsed-document
    // column stops demanding a value.
    assert!(
        catalog_column_is_nullable(su, "flow_drafts", "graph_json").await,
        "upgrade must relax the retired parsed-document column"
    );
    assert!(
        catalog_column_is_nullable(su, "authoring_command_audit", "provenance_commit").await,
        "attribution is optional and must never be demanded"
    );
    wamn_ctl::publish_catalog::ensure_catalog_storage(su)
        .await
        .expect("the wamn-ftfc.2 column upgrades are idempotent");
    for table in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(
            table_exists(su, SCHEMA, table).await,
            "upgrade creates {table}"
        );
    }

    // Inject every repairable stale shape plus an unrepairable inherited path.
    // Reconciliation may revoke known direct grants/membership, but it must not
    // silently alter an unrelated group role; the effective postcondition
    // therefore aborts until the platform administrator removes that source.
    su.batch_execute(&format!(
        "ALTER ROLE wamn_scenario_author LOGIN INHERIT CREATEDB; \
         GRANT wamn_scenario_author TO wamn_app; \
         GRANT INSERT, UPDATE, DELETE ON catalog.validated_flow_drafts TO wamn_app; \
         GRANT INSERT, UPDATE, DELETE ON catalog.release_manifests TO wamn_scenario_author; \
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_test_run_reservations TO wamn_app; \
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_test_case_runs TO PUBLIC; \
         REVOKE EXECUTE ON FUNCTION {SCHEMA}.lock_catalog_head(text,text,text) \
           FROM wamn_scenario_author; \
         DO $role$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rp_guest_writer') THEN \
             CREATE ROLE rp_guest_writer NOLOGIN; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'rp_guest_column_writer') THEN \
             CREATE ROLE rp_guest_column_writer NOLOGIN; \
           END IF; \
         END $role$; \
         GRANT INSERT ON catalog.flow_drafts TO rp_guest_writer; \
         GRANT UPDATE (graph_hash) ON catalog.validated_flow_drafts \
           TO rp_guest_column_writer; \
         GRANT rp_guest_writer, rp_guest_column_writer TO wamn_app;"
    ))
    .await
    .expect("inject stale direct and inherited authoring authority");

    let drift = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("observe stale authoring authority");
    assert!(
        !drift.is_noop(),
        "effective inherited write is never false-clean"
    );
    assert!(drift.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
            && action.target == "catalog.flow_drafts"
            && action.sql.contains("has_table_privilege")
    }));
    let inherited_error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("an unrelated inherited guest writer cannot be auto-repaired safely");
    assert!(
        format!("{inherited_error:#}")
            .contains("authoring-effective-privilege-out-of-bounds:catalog.flow_drafts"),
        "effective privilege refusal is explicit: {inherited_error:#}"
    );
    su.batch_execute(
        "REVOKE rp_guest_writer FROM wamn_app; \
         REVOKE ALL PRIVILEGES ON catalog.flow_drafts FROM rp_guest_writer; \
         DROP ROLE rp_guest_writer;",
    )
    .await
    .expect("platform administrator removes unrelated inherited authority");
    let column_error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("a surviving column grant cannot be repaired as a table ACL");
    assert!(
        format!("{column_error:#}")
            .contains("authoring-effective-privilege-out-of-bounds:catalog.validated_flow_drafts"),
        "column-derived authority is explicit: {column_error:#}"
    );
    su.batch_execute(
        "REVOKE rp_guest_column_writer FROM wamn_app; \
         REVOKE UPDATE (graph_hash) ON catalog.validated_flow_drafts \
           FROM rp_guest_column_writer; \
         DROP ROLE rp_guest_column_writer;",
    )
    .await
    .expect("platform administrator removes the unexpected column grant");
    reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("known direct ACL and membership drift converges");

    // Ownership is another implicit privilege source absent from direct ACL
    // rows. It is surfaced and refused, never reassigned to a guessed owner.
    su.batch_execute("ALTER TABLE catalog.flow_drafts OWNER TO wamn_app")
        .await
        .expect("inject guest ownership mutant");
    let owner_plan = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("observe guest ownership authority");
    assert!(
        !owner_plan.is_noop(),
        "guest table ownership is not false-clean"
    );
    let owner_error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("reconciler must not guess a replacement table owner");
    assert!(
        format!("{owner_error:#}").contains("authoring-effective-privilege-out-of-bounds"),
        "ownership-derived authority is explicit: {owner_error:#}"
    );
    su.batch_execute("ALTER TABLE catalog.flow_drafts OWNER TO SESSION_USER")
        .await
        .expect("platform administrator restores table ownership");
    reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("ownership repair plus exact ACL convergence");

    let role = su
        .query_one(
            "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, \
                    rolreplication, rolbypassrls \
               FROM pg_roles WHERE rolname = 'wamn_scenario_author'",
            &[],
        )
        .await
        .expect("read hardened author role");
    for index in 0..7 {
        assert!(
            !role.get::<_, bool>(index),
            "author role attribute {index} is disabled"
        );
    }
    let app_is_member: bool = su
        .query_one(
            "SELECT pg_has_role('wamn_app', 'wamn_scenario_author', 'MEMBER')",
            &[],
        )
        .await
        .expect("read repaired membership")
        .get(0);
    assert!(
        !app_is_member,
        "guest is not a member of the host author role"
    );
    let author_can_lock: bool = su
        .query_one(
            &format!(
                "SELECT has_function_privilege( \
                   'wamn_scenario_author', \
                   '{SCHEMA}.lock_catalog_head(text,text,text)', 'EXECUTE')"
            ),
            &[],
        )
        .await
        .expect("read repaired catalog-lock grant")
        .get(0);
    assert!(
        author_can_lock,
        "author receives only the narrow lock function"
    );

    // Environment-owned connection generations and draft-safe decisions are
    // provisioned by the platform. Management can inspect an owner-seeded
    // decision, but has no mutation authority over the retained relation.
    su.batch_execute(
        "INSERT INTO catalog.connection_instances \
           (tenant_id,environment,instance_id,requirement_type,contract) \
         VALUES ('t1','dev','erp','http','v1'); \
         INSERT INTO catalog.connection_generations \
           (tenant_id,environment,instance_id,generation,definition_json, \
            definition_hash,credential_set_handle) VALUES \
           ('t1','dev','erp',1,'{}','sha256:g1','cred:g1'), \
           ('t1','dev','erp',2,'{}','sha256:g2','cred:g2');",
    )
    .await
    .expect("seed platform-owned connection generations");

    su.batch_execute(
        "INSERT INTO catalog.draft_safe_connection_grants \
           (tenant_id,environment,instance_id,generation,reason) \
         VALUES ('t1','dev','erp',1,'development-admin seed'); \
         SET ROLE wamn_scenario_author; \
         SELECT set_config('app.tenant','t1',false); \
         INSERT INTO catalog.flow_drafts \
           (tenant_id,draft_id,flow_id,graph_json) \
         VALUES ('t1','draft-a','flow-a','{}');",
    )
    .await
    .expect("platform seeds authority and host author writes the mutable draft surface");
    let visible_grants: i64 = su
        .query_one(
            "SELECT count(*) FROM catalog.draft_safe_connection_grants \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1",
            &[],
        )
        .await
        .expect("management can inspect the owner-seeded decision")
        .get(0);
    assert_eq!(
        visible_grants, 1,
        "the exact owner-seeded decision is visible"
    );
    let privileges: Vec<bool> = su
        .query_one(
            "SELECT ARRAY[ \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'SELECT'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'INSERT'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'UPDATE'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'DELETE'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'TRUNCATE'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'REFERENCES'), \
               has_table_privilege(current_user, \
                 'catalog.draft_safe_connection_grants', 'TRIGGER')]",
            &[],
        )
        .await
        .expect("probe exact management authority")
        .get(0);
    assert_eq!(
        privileges,
        vec![true, false, false, false, false, false, false],
        "management has SELECT and no mutation-adjacent privilege"
    );
    let uncontrolled = su
        .execute(
            "UPDATE catalog.draft_safe_connection_grants SET reason='rewritten' \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1",
            &[],
        )
        .await
        .expect_err("management cannot mutate an owner-seeded decision");
    assert_db_code(uncontrolled, "42501", "management grant mutation privilege");

    // The separate test-set store is gone; a draft carries its own cases, and
    // the immutable report is the only author-visible test artifact left.
    let store_relation: Option<String> = su
        .query_one(
            &format!("SELECT to_regclass('{SCHEMA}.authoring_test_sets')::text"),
            &[],
        )
        .await
        .expect("probe the deleted test-set store")
        .get(0);
    assert_eq!(store_relation, None, "the test-set store must not exist");
    let report_privileges: Vec<bool> = su
        .query_one(
            &format!(
                "SELECT ARRAY[ \
                   has_table_privilege(current_user, \
                     '{SCHEMA}.authoring_test_reports', 'SELECT'), \
                   has_table_privilege(current_user, \
                     '{SCHEMA}.authoring_test_reports', 'INSERT'), \
                   has_table_privilege(current_user, \
                     '{SCHEMA}.authoring_test_reports', 'UPDATE'), \
                   has_table_privilege(current_user, \
                     '{SCHEMA}.authoring_test_reports', 'DELETE')]"
            ),
            &[],
        )
        .await
        .expect("probe immutable report authority")
        .get(0);
    assert_eq!(
        report_privileges,
        vec![true, true, false, false],
        "host author appends reports and never rewrites one"
    );

    // The host author can read release source facts and tenant runs, but has no
    // release publication mutation surface.
    let author_reads: bool = su
        .query_one(
            &format!(
                "SELECT has_table_privilege(current_user,'catalog.release_manifests','SELECT') \
                    AND has_table_privilege(current_user,'catalog.release_flows','SELECT') \
                    AND has_table_privilege(current_user,'catalog.catalog_heads','SELECT') \
                    AND has_table_privilege(current_user,'{SCHEMA}.runs','SELECT')"
            ),
            &[],
        )
        .await
        .expect("probe narrow author reads")
        .get(0);
    assert!(author_reads);
    let release_write = su
        .execute(
            "UPDATE catalog.catalog_heads SET updated_at=clock_timestamp() WHERE false",
            &[],
        )
        .await
        .expect_err("author cannot mutate a release head");
    assert_db_code(release_write, "42501", "author release-write refusal");

    su.batch_execute("RESET ROLE; SELECT set_config('app.tenant','',false)")
        .await
        .expect("leave host-author role");

    // Use a real guest login here: a superuser session that executes
    // `SET ROLE wamn_app` retains its session-user right to assume any role,
    // which cannot prove the membership boundary.
    let guest = connect_as(url, "wamn_app", "wamn_app").await;
    guest
        .batch_execute("SELECT set_config('app.tenant','t1',false)")
        .await
        .expect("enter guest role for negative probes");
    let guest_draft = guest
        .execute(
            "INSERT INTO catalog.flow_drafts \
             (tenant_id,draft_id,flow_id,graph_json) \
             VALUES ('t1','forged','flow-a','{}')",
            &[],
        )
        .await
        .expect_err("guest cannot forge a draft write");
    assert_db_code(guest_draft, "42501", "guest draft forgery");
    let guest_report = guest
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.authoring_test_reports \
                   (tenant_id,report_id,validated_draft_id,catalog_id, \
                    catalog_version,resolution_map,resolution_map_hash,passed,summary) \
                 VALUES ('t1','forged','validated-a','cat',1,'{{}}'::jsonb, \
                         'sha256:' || repeat('0',64), true, '{{}}'::jsonb)"
            ),
            &[],
        )
        .await
        .expect_err("guest cannot forge a report write");
    assert_db_code(guest_report, "42501", "guest report forgery");
    let assume_author = guest
        .batch_execute("SET ROLE wamn_scenario_author")
        .await
        .expect_err("guest cannot assume the host-only author role");
    assert_db_code(assume_author, "42501", "guest author-role assumption");
    drop(guest);

    let final_plan = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan hardened authoring storage");
    assert!(
        final_plan.is_noop(),
        "authoring storage converged: {:#?}",
        final_plan.actions
    );
}

/// SHARE, rather than KEY SHARE, is required because publication advances a
/// non-key column. Both host-author and runtime callers use the same narrow
/// tenant-checking SECURITY DEFINER bridge and hold the lock to transaction end.
async fn catalog_head_share_lock_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog storage for lock probe");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply lock bridge");
    su.batch_execute(
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) VALUES \
           ('t1','cat',1,'dev','1','applied'), \
           ('t1','cat',2,'dev','1','staged'); \
         INSERT INTO catalog.catalog_heads \
           (tenant_id,catalog_id,environment,applied_catalog_version) \
         VALUES ('t1','cat','dev',1);",
    )
    .await
    .expect("seed two catalog versions and stable head");

    for role_name in ["wamn_app", "wamn_scenario_author"] {
        let holder = connect(url).await;
        holder
            .batch_execute(&format!(
                "BEGIN; SET ROLE {role_name}; \
                 SELECT set_config('app.tenant','t1',true); \
                 SELECT {SCHEMA}.lock_catalog_head('t1','cat','dev');"
            ))
            .await
            .expect("admission role acquires catalog-head SHARE lock");
        let contender = connect(url).await;
        contender
            .batch_execute("SET lock_timeout='100ms'")
            .await
            .expect("bound lock probe wait");
        let blocked = contender
            .execute(
                "UPDATE catalog.catalog_heads SET applied_catalog_version=2 \
                 WHERE tenant_id='t1' AND catalog_id='cat' AND environment='dev'",
                &[],
            )
            .await
            .expect_err("publisher pointer update must block behind admission");
        assert_db_code(blocked, "55P03", "catalog-head SHARE lock conflict");
        holder
            .batch_execute("ROLLBACK")
            .await
            .expect("release admission lock");
        contender
            .batch_execute("SET lock_timeout=0")
            .await
            .expect("restore publisher lock timeout");
        contender
            .execute(
                "UPDATE catalog.catalog_heads SET updated_at=clock_timestamp() \
                 WHERE tenant_id='t1' AND catalog_id='cat' AND environment='dev'",
                &[],
            )
            .await
            .expect("publisher update proceeds after admission ends");
    }
}

async fn retired_effect_disposition_cutover_leg(su: &Client) {
    reset(su).await;
    su.batch_execute(&format!(
        "CREATE SCHEMA {SCHEMA}; \
         CREATE TABLE {SCHEMA}.effect_disposition_requests ( \
             tenant_id text NOT NULL, request_id uuid NOT NULL, \
             PRIMARY KEY (tenant_id, request_id)); \
         CREATE TABLE {SCHEMA}.effect_dispositions ( \
             tenant_id text NOT NULL, request_id uuid NOT NULL, \
             PRIMARY KEY (tenant_id, request_id), \
             FOREIGN KEY (tenant_id, request_id) \
               REFERENCES {SCHEMA}.effect_disposition_requests (tenant_id, request_id)); \
         CREATE FUNCTION {SCHEMA}.guard_effect_disposition_append() RETURNS trigger \
             LANGUAGE plpgsql AS $legacy$ BEGIN RETURN NEW; END $legacy$;"
    ))
    .await
    .expect("install empty retired effect-disposition pair");

    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("empty retired pair plans cutover");
    let cutover = dry
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::RetiredEffectDispositionCutover)
        .expect("retired effect-disposition cutover is planned");
    assert!(cutover.sql.contains("IN ACCESS EXCLUSIVE MODE"));
    assert!(cutover.sql.contains(
        "retired-effect-disposition-history-requires-archive-or-environment-reprovision"
    ));
    reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty retired pair is removed");
    assert!(!table_exists(su, SCHEMA, "effect_disposition_requests").await);
    assert!(!table_exists(su, SCHEMA, "effect_dispositions").await);
    assert!(table_exists(su, SCHEMA, "operator_run_actions").await);
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("post-cutover plan")
            .is_noop()
    );

    reset(su).await;
    su.batch_execute(&format!(
        "CREATE SCHEMA {SCHEMA}; \
         CREATE TABLE {SCHEMA}.effect_disposition_requests ( \
             tenant_id text NOT NULL, request_id uuid NOT NULL, \
             PRIMARY KEY (tenant_id, request_id)); \
         CREATE TABLE {SCHEMA}.effect_dispositions ( \
             tenant_id text NOT NULL, request_id uuid NOT NULL, \
             PRIMARY KEY (tenant_id, request_id), \
             FOREIGN KEY (tenant_id, request_id) \
               REFERENCES {SCHEMA}.effect_disposition_requests (tenant_id, request_id)); \
         INSERT INTO {SCHEMA}.effect_disposition_requests \
             VALUES ('t1', '00000000-0000-0000-0000-000000000001');"
    ))
    .await
    .expect("install populated retired effect-disposition pair");
    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("populated retired history refuses");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres.as_db_error().expect("typed cutover refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "retired-effect-disposition-history-requires-archive-or-environment-reprovision"
    );
    assert!(table_exists(su, SCHEMA, "effect_disposition_requests").await);
    assert!(table_exists(su, SCHEMA, "effect_dispositions").await);
    assert!(!table_exists(su, SCHEMA, "operator_run_actions").await);
}

/// Historical pre-cutover disposition hardening proof, retained only as source
/// archaeology while the replacement cutover gate above owns active coverage.
#[allow(dead_code)]
/// wamn-4u7p.42: repair the pre-hardening disposition ledger without replacing
/// its table. The identity column is additive, the old wall-clock history index
/// is recreated, helper authority/search-path definitions converge exactly, and
/// the closed CHECK rejects the two SQL-NULL outcome shapes that previously
/// passed PostgreSQL CHECK semantics.
async fn effect_disposition_security_drift_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    for ddl in [RUN_STATE_SQL, FLOWS_SQL, AUTHORING_TESTS_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane record");
    }

    su.batch_execute(&format!(
        "DROP INDEX {SCHEMA}.effect_dispositions_attempt_history; \
         ALTER TABLE {SCHEMA}.effect_dispositions DROP COLUMN append_ordinal; \
         CREATE INDEX effect_dispositions_attempt_history \
             ON {SCHEMA}.effect_dispositions (tenant_id, attempt_id, created_at DESC); \
         DROP INDEX {SCHEMA}.effect_dispositions_request_ordinal; \
         CREATE INDEX effect_dispositions_request_ordinal \
             ON {SCHEMA}.effect_dispositions \
                (tenant_id, request_id, selection_ordinal); \
         DROP INDEX {SCHEMA}.effect_dispositions_one_resolution; \
         CREATE INDEX effect_dispositions_one_resolution \
             ON {SCHEMA}.effect_dispositions (tenant_id, attempt_id) \
             WHERE action='park'; \
         ALTER TABLE {SCHEMA}.effect_dispositions \
             DROP CONSTRAINT effect_dispositions_outcome_check; \
         ALTER TABLE {SCHEMA}.effect_dispositions \
             ADD CONSTRAINT effect_dispositions_outcome_check CHECK (true); \
         CREATE OR REPLACE FUNCTION {SCHEMA}.guard_effect_disposition_append() \
             RETURNS trigger LANGUAGE plpgsql SET search_path = pg_catalog \
             AS $unsafe$ BEGIN RETURN NEW; END $unsafe$;"
    ))
    .await
    .expect("regress disposition ledger to its pre-hardening shape");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile disposition security drift");
    for (kind, target) in [
        (
            RunPlaneActionKind::AddColumn,
            "effect_dispositions.append_ordinal",
        ),
        (
            RunPlaneActionKind::RecreateIndex,
            "effect_dispositions_attempt_history",
        ),
        (
            RunPlaneActionKind::CreateIndex,
            "effect_dispositions_append_order",
        ),
        (
            RunPlaneActionKind::RecreateIndex,
            "effect_dispositions_request_ordinal",
        ),
        (
            RunPlaneActionKind::RecreateIndex,
            "effect_dispositions_one_resolution",
        ),
        (
            RunPlaneActionKind::RepairConstraint,
            "effect_dispositions.effect_dispositions_outcome_check",
        ),
        (
            RunPlaneActionKind::RepairHelperFunction,
            "guard_effect_disposition_append",
        ),
    ] {
        assert!(
            plan.actions
                .iter()
                .any(|action| action.kind == kind && action.target == target),
            "disposition drift plans {kind:?} for {target}: {:#?}",
            plan.actions
        );
    }

    let identity_row = su
        .query_one(
            "SELECT data_type, is_identity, identity_generation \
             FROM information_schema.columns \
             WHERE table_schema=$1 AND table_name='effect_dispositions' \
               AND column_name='append_ordinal'",
            &[&SCHEMA],
        )
        .await
        .expect("read reconciled append identity");
    let identity: (String, String, String) = (
        identity_row.get(0),
        identity_row.get(1),
        identity_row.get(2),
    );
    assert_eq!(
        identity,
        (
            "bigint".to_string(),
            "YES".to_string(),
            "ALWAYS".to_string()
        )
    );

    let history = indexdef(su, "effect_dispositions_attempt_history")
        .await
        .expect("reconciled disposition history index");
    assert!(history.contains("append_ordinal DESC"), "{history}");
    assert!(!history.contains("created_at"), "{history}");
    let append_order = indexdef(su, "effect_dispositions_append_order")
        .await
        .expect("reconciled unique append-order index");
    assert!(
        append_order.starts_with("CREATE UNIQUE INDEX"),
        "{append_order}"
    );
    assert!(append_order.contains("(append_ordinal)"), "{append_order}");
    let request_order = indexdef(su, "effect_dispositions_request_ordinal")
        .await
        .expect("reconciled request selection-order index");
    assert!(
        request_order.starts_with("CREATE UNIQUE INDEX"),
        "{request_order}"
    );
    let one_resolution = indexdef(su, "effect_dispositions_one_resolution")
        .await
        .expect("reconciled one-resolution index");
    assert!(
        one_resolution.starts_with("CREATE UNIQUE INDEX")
            && one_resolution.contains("WHERE (action = 'resolve'::text)"),
        "{one_resolution}"
    );

    let outcome_check: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid, true) FROM pg_constraint con \
             JOIN pg_class c ON c.oid=con.conrelid \
             JOIN pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname=$1 AND c.relname='effect_dispositions' \
               AND con.conname='effect_dispositions_outcome_check'",
            &[&SCHEMA],
        )
        .await
        .expect("read reconciled outcome CHECK")
        .get(0);
    assert!(outcome_check.contains("IS TRUE"), "{outcome_check}");
    assert!(
        outcome_check.contains("failure_detail ? 'message'::text"),
        "{outcome_check}"
    );

    let guard_definition: String = su
        .query_one(
            "SELECT pg_get_functiondef(p.oid) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid=p.pronamespace \
             WHERE n.nspname=$1 AND p.proname='guard_effect_disposition_append'",
            &[&SCHEMA],
        )
        .await
        .expect("read reconciled disposition guard")
        .get(0);
    assert!(
        guard_definition.contains("SET search_path TO 'pg_catalog', 'pg_temp'"),
        "{guard_definition}"
    );
    assert!(guard_definition.contains("pg_catalog.pg_class"));
    assert!(guard_definition.contains("pg_catalog.pg_roles"));
    assert!(!guard_definition.contains("wamn_platform_admin"));
    assert!(!guard_definition.contains("pg_has_role"));

    // Membership in the legacy platform role does not authorize direct DML.
    // The only non-superuser crossing is a reviewed table-owner definer where
    // CURRENT_USER differs from the authenticated SESSION_USER.
    su.batch_execute(&format!(
        "DO $role$ BEGIN \
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_platform_admin') \
             THEN CREATE ROLE wamn_platform_admin NOLOGIN; END IF; \
         END $role$; \
         GRANT wamn_platform_admin TO wamn_app; \
         GRANT INSERT ON {SCHEMA}.effect_disposition_requests TO wamn_app; \
         SET SESSION AUTHORIZATION wamn_app; \
         SELECT set_config('app.tenant','t1',false); \
         CREATE TEMP TABLE pg_roles \
             (rolname text, rolsuper boolean, rolbypassrls boolean); \
         INSERT INTO pg_temp.pg_roles VALUES ('wamn_app',true,true);"
    ))
    .await
    .expect("enter a platform-member application session");
    let direct_fact_append = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
                    attempt_started_at,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000509', \
                         'forged',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                         'effect',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0,'not-required', \
                         now(),now()+interval '1 minute','sha256:forged')"
            ),
            &[],
        )
        .await;
    let direct_append = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_disposition_requests \
                   (tenant_id,request_id,action,selection_kind,principal,effective_role,correlation_id) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000510', \
                         'park','single','member','project-admin','direct-dml')"
            ),
            &[],
        )
        .await;
    su.batch_execute(&format!(
        "RESET SESSION AUTHORIZATION; \
         SELECT set_config('app.tenant','',false); \
         REVOKE wamn_platform_admin FROM wamn_app; \
         REVOKE INSERT ON {SCHEMA}.effect_disposition_requests FROM wamn_app; \
         DROP TABLE pg_temp.pg_roles;"
    ))
    .await
    .expect("leave the platform-member application session");
    let fact_error = direct_fact_append.expect_err("ordinary app cannot append immutable facts");
    assert_db_code(fact_error, "42501", "effect-ledger table-ACL refusal");
    let error = direct_append.expect_err("platform membership cannot bypass the insert guard");
    assert!(
        error
            .as_db_error()
            .is_some_and(|db| db.message() == "effect-disposition-append-requires-trusted-adapter"),
        "typed direct-DML refusal: {error}"
    );
    for (request_id, effective_role) in [
        ("00000000-0000-0000-0000-000000000520", "system"),
        ("00000000-0000-0000-0000-000000000521", "project-deployer"),
    ] {
        let invalid_resolve = su
            .execute(
                &format!(
                    "INSERT INTO {SCHEMA}.effect_disposition_requests \
                       (tenant_id,request_id,action,selection_kind,principal,effective_role, \
                        basis,evidence_ref,correlation_id) \
                     VALUES ('t1','{request_id}','resolve','single','actor','{effective_role}', \
                             'external-evidence','case:role-floor','role-floor')"
                ),
                &[],
            )
            .await;
        let role_error =
            invalid_resolve.expect_err("storage rejects resolve outside the approved role floor");
        assert!(
            role_error
                .as_db_error()
                .and_then(|db| db.constraint())
                .is_some_and(|constraint| {
                    constraint == "effect_disposition_requests_role_action_check"
                }),
            "typed role/action refusal for {effective_role}: {role_error}"
        );
    }

    // Two formerly nullable-invalid resolution shapes are rejected, while a
    // complete failure lands. Earlier audit timestamps cannot reverse the
    // append identity's history order.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.effect_attempts \
             (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
              local_node_id,source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
              attempt_started_at,attempt_deadline_at,attempt_input_ref) \
             VALUES \
             ('t1','00000000-0000-0000-0000-000000000530', \
                     'audit-run',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                     'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0,'not-required', \
                     now(),now()+interval '1 minute','sha256:audit'), \
             ('t1','00000000-0000-0000-0000-000000000540', \
                     'audit-run',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                     'temporal',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,1,'not-required', \
                     now(),now()+interval '1 minute','sha256:temporal'); \
         INSERT INTO {SCHEMA}.effect_attempt_dispatches \
             (tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
              local_node_id,occurrence,dispatched_at) \
             SELECT tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                    local_node_id,occurrence, \
                    attempt_started_at + interval '1 second' \
               FROM {SCHEMA}.effect_attempts \
              WHERE attempt_id='00000000-0000-0000-0000-000000000530'; \
         INSERT INTO {SCHEMA}.effect_attempt_outcomes \
             (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at) \
             SELECT tenant_id,attempt_id,dispatched_at,'success', \
                    dispatched_at + interval '1 second' \
               FROM {SCHEMA}.effect_attempt_dispatches \
              WHERE attempt_id='00000000-0000-0000-0000-000000000530'; \
         INSERT INTO {SCHEMA}.effect_disposition_requests \
             (tenant_id,request_id,action,selection_kind,principal,effective_role,correlation_id) \
             VALUES \
             ('t1','00000000-0000-0000-0000-000000000531','park','single','operator','project-admin','park'), \
             ('t1','00000000-0000-0000-0000-000000000532','release','single','operator','project-admin','release'); \
         INSERT INTO {SCHEMA}.effect_disposition_requests \
             (tenant_id,request_id,action,selection_kind,principal,effective_role, \
              basis,evidence_ref,correlation_id) \
             VALUES ('t1','00000000-0000-0000-0000-000000000533','resolve','single', \
                     'operator','project-admin','external-evidence','case:1','resolve'); \
         INSERT INTO {SCHEMA}.effect_dispositions \
             (tenant_id,request_id,attempt_id,selection_ordinal,action,created_at) \
             VALUES ('t1','00000000-0000-0000-0000-000000000531', \
                     '00000000-0000-0000-0000-000000000530',0,'park','2099-01-01T00:00:00Z'); \
         INSERT INTO {SCHEMA}.effect_dispositions \
             (tenant_id,request_id,attempt_id,selection_ordinal,action,created_at) \
             VALUES ('t1','00000000-0000-0000-0000-000000000532', \
                     '00000000-0000-0000-0000-000000000530',0,'release','2000-01-01T00:00:00Z');"
    ))
    .await
    .expect("seed append-order and resolution audit");

    let early_dispatch = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempt_dispatches \
                   (tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                    local_node_id,occurrence,dispatched_at) \
                 SELECT tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                        local_node_id,occurrence, \
                        attempt_started_at - interval '1 second' \
                   FROM {SCHEMA}.effect_attempts \
                  WHERE attempt_id='00000000-0000-0000-0000-000000000540'"
            ),
            &[],
        )
        .await;
    let dispatch_time_error =
        early_dispatch.expect_err("dispatch cannot predate immutable attempt start");
    assert!(
        dispatch_time_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "effect_attempt_dispatches_time_check"),
        "typed early-dispatch refusal: {dispatch_time_error}"
    );
    let outcome_without_dispatch = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempt_outcomes \
                   (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at) \
                 SELECT tenant_id,attempt_id,attempt_started_at,'success',attempt_started_at \
                   FROM {SCHEMA}.effect_attempts \
                  WHERE attempt_id='00000000-0000-0000-0000-000000000540'"
            ),
            &[],
        )
        .await;
    let missing_dispatch_error =
        outcome_without_dispatch.expect_err("outcome requires the exact dispatch boundary");
    assert!(
        missing_dispatch_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "effect_attempt_outcomes_dispatch_fk"),
        "typed missing-dispatch refusal: {missing_dispatch_error}"
    );
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.effect_attempt_dispatches \
               (tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                local_node_id,occurrence,dispatched_at) \
             SELECT tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                    local_node_id,occurrence, \
                    attempt_started_at + interval '1 second' \
               FROM {SCHEMA}.effect_attempts \
              WHERE attempt_id='00000000-0000-0000-0000-000000000540'"
        ),
        &[],
    )
    .await
    .expect("seed exact temporal dispatch boundary");
    let early_outcome = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempt_outcomes \
                   (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at) \
                 SELECT tenant_id,attempt_id,dispatched_at,'success', \
                        dispatched_at - interval '1 second' \
                   FROM {SCHEMA}.effect_attempt_dispatches \
                  WHERE attempt_id='00000000-0000-0000-0000-000000000540'"
            ),
            &[],
        )
        .await;
    let outcome_time_error =
        early_outcome.expect_err("outcome cannot predate its exact dispatch boundary");
    assert!(
        outcome_time_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "effect_attempt_outcomes_time_check"),
        "typed early-outcome refusal: {outcome_time_error}"
    );

    let nullable_success = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_dispositions \
                   (tenant_id,request_id,attempt_id,selection_ordinal,action, \
                    resolution_status,success_payload,success_port) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000533', \
                         '00000000-0000-0000-0000-000000000530',0,'resolve', \
                         NULL,'{{}}'::jsonb,'done')"
            ),
            &[],
        )
        .await;
    assert!(
        nullable_success.is_err(),
        "NULL resolution status cannot pass the complete-success branch"
    );
    let missing_message = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_dispositions \
                   (tenant_id,request_id,attempt_id,selection_ordinal,action, \
                    resolution_status,failure_kind,failure_detail) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000533', \
                         '00000000-0000-0000-0000-000000000530',0,'resolve', \
                         'failed','terminal','{{}}'::jsonb)"
            ),
            &[],
        )
        .await;
    assert!(
        missing_message.is_err(),
        "failure detail without a string message cannot pass the failure branch"
    );
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.effect_dispositions \
               (tenant_id,request_id,attempt_id,selection_ordinal,action, \
                resolution_status,failure_kind,failure_detail) \
             VALUES ('t1','00000000-0000-0000-0000-000000000533', \
                     '00000000-0000-0000-0000-000000000530',0,'resolve', \
                     'failed','terminal','{{\"message\":\"confirmed\"}}'::jsonb)"
        ),
        &[],
    )
    .await
    .expect("complete failure outcome is admitted");
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.effect_disposition_requests \
               (tenant_id,request_id,action,selection_kind,principal,effective_role,correlation_id) \
             VALUES ('t1','00000000-0000-0000-0000-000000000534','park','single', \
                     'operator','project-admin','duplicate-append-order')"
        ),
        &[],
    )
    .await
    .expect("seed request for duplicate append-order mutant");
    let duplicate_append_order = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_dispositions \
                   (tenant_id,request_id,attempt_id,append_ordinal,selection_ordinal,action) \
                 OVERRIDING SYSTEM VALUE \
                 SELECT 't1','00000000-0000-0000-0000-000000000534', \
                        '00000000-0000-0000-0000-000000000530', \
                        min(append_ordinal),0,'park' \
                   FROM {SCHEMA}.effect_dispositions \
                  WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await;
    let duplicate_append_error =
        duplicate_append_order.expect_err("append order is globally unique at storage");
    assert!(
        duplicate_append_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "effect_dispositions_append_order"),
        "typed duplicate append-order refusal: {duplicate_append_error}"
    );
    let history_rows = su
        .query(
            &format!(
                "SELECT action,append_ordinal FROM {SCHEMA}.effect_dispositions \
                 WHERE tenant_id='t1' \
                   AND attempt_id='00000000-0000-0000-0000-000000000530' \
                 ORDER BY append_ordinal"
            ),
            &[],
        )
        .await
        .expect("read identity-ordered history");
    assert_eq!(history_rows.len(), 3);
    assert_eq!(history_rows[0].get::<_, String>(0), "park");
    assert_eq!(history_rows[1].get::<_, String>(0), "release");
    assert_eq!(history_rows[2].get::<_, String>(0), "resolve");
    assert!(
        history_rows[0].get::<_, i64>(1) < history_rows[1].get::<_, i64>(1),
        "identity order is monotonic"
    );
    let audit_time_reversed: bool = su
        .query_one(
            &format!(
                "SELECT \
                   (SELECT created_at FROM {SCHEMA}.effect_dispositions \
                     WHERE request_id='00000000-0000-0000-0000-000000000531') \
                   > \
                   (SELECT created_at FROM {SCHEMA}.effect_dispositions \
                     WHERE request_id='00000000-0000-0000-0000-000000000532')"
            ),
            &[],
        )
        .await
        .expect("compare audit time against append order")
        .get(0);
    assert!(
        audit_time_reversed,
        "audit timestamps deliberately disagree with append order"
    );

    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "effect_disposition_requests",
        "effect_dispositions",
    ] {
        for verb in ["UPDATE", "DELETE"] {
            let sql = if verb == "UPDATE" {
                format!("UPDATE {SCHEMA}.{table} SET tenant_id=tenant_id WHERE tenant_id='t1'")
            } else {
                format!("DELETE FROM {SCHEMA}.{table} WHERE tenant_id='t1'")
            };
            let error = su
                .execute(&sql, &[])
                .await
                .expect_err("immutable effect facts reject mutation");
            assert!(
                error
                    .as_db_error()
                    .is_some_and(|db| db.message() == "effect-disposition-immutable"),
                "{verb} on {table} has a typed immutability refusal: {error}"
            );
        }
    }

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan hardened disposition schema");
    assert!(
        again.is_noop(),
        "disposition security drift converged: {:#?}",
        again.actions
    );
}

/// Reconcile repairs drifted persisted vocabularies and converges in one pass.
async fn persisted_literal_check_drift_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    // Provision the CURRENT run plane (fresh 5-literal fail_kind CHECK)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    // Regress the run failure vocabulary and restore the retired node-error
    // cancellation literal to model the two predecessor constraints.
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs DROP CONSTRAINT runs_fail_kind_check; \
         ALTER TABLE {SCHEMA}.runs ADD CONSTRAINT runs_fail_kind_check \
             CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input')); \
         ALTER TABLE {SCHEMA}.node_runs DROP CONSTRAINT node_runs_error_kind_check; \
         ALTER TABLE {SCHEMA}.node_runs ADD CONSTRAINT node_runs_error_kind_check \
             CHECK (error_kind IN \
               ('retryable','rate-limited','terminal','invalid-input','cancelled'));"
    ))
    .await
    .expect("regress persisted literal CHECKs");
    // A run whose runaway verdict we will try to record.
    seed_run_pin_parents(su, "t1", "cat", 1, "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment,execution_bundle_hash) \
             VALUES ('t1','r-budget','f',1,'cat',1,'dev','{EMPTY_EXECUTION_BUNDLE_HASH}');"
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
    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile applies");
    assert!(
        plan.actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::RepairConstraint
                && a.target == "runs.runs_fail_kind_check"),
        "the fail_kind CHECK repair is planned: {:#?}",
        plan.actions
    );
    assert!(
        plan.actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::RepairConstraint
                && a.target == "node_runs.node_runs_error_kind_check"),
        "the node error CHECK repair is planned: {:#?}",
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

    let node_error_def: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'node_runs' \
               AND con.conname = 'node_runs_error_kind_check'",
            &[&SCHEMA],
        )
        .await
        .expect("read node error constraintdef")
        .get(0);
    for literal in ["retryable", "rate-limited", "terminal", "invalid-input"] {
        assert!(
            node_error_def.contains(&format!("'{literal}'::text")),
            "reconciled node error CHECK contains {literal}: {node_error_def}"
        );
    }
    assert!(
        !node_error_def.contains("cancelled"),
        "reconciled node error CHECK removes cancelled: {node_error_def}"
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
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

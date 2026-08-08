//! Live-apply gate for `reconcile-run-plane` (E4/R14-migration, wamn-1wdq): the
//! durable migration path for provisioned run-plane schemas, proven against a
//! REAL Postgres in every starting state the bead's manifestations recorded.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway Postgres (recipe: docs/build-and-test.md [RUN-PLANE-RECONCILE]);
//! skipped cleanly when unset. Eleven legs, sequential under one test entry
//! (they share the `catalog` schema and the `wamn_app` role):
//!
//! - **shared-runner legacy** (wamn-l5i9.73): the deployed fixture's old
//!   runs/node_runs/run_queue shape gains canonical admission/causation
//!   columns, CHECKs, helper functions, and lineage trigger without losing its
//!   compatible history. The materializer catalog-head lock and immutable
//!   lineage are exercised, then a second reconcile is a no-op.
//! - **legacy effect attempts** (wamn-4u7p.42.2): the prior mutable attempt
//!   projection is transactionally copied into immutable attempt, dispatch,
//!   and outcome facts before the composite current-attempt FK activates.
//! - **forced-RLS owner refusal**: a plain table owner cannot observe hidden
//!   tenant rows, so dry-run and apply both refuse before the pointer or ledger
//!   schema can be activated.
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
//! - **authoring additive upgrade + authority repair**: the pre-6A catalog and
//!   suite schemas gain draft/report/grant storage; stale guest grants and
//!   membership are removed; real author-role writes, rapid exact-generation
//!   revoke/re-grant, report finalization, and guest/release-write refusals are
//!   exercised.
//! - **catalog-head lock concurrency**: both runtime and author admission call
//!   the tenant-checking SECURITY DEFINER bridge; its SHARE lock blocks the
//!   publisher's pointer UPDATE until admission commits.
//! - **effect-disposition security drift**: a pre-hardening ledger gains its
//!   identity append order, closed outcome CHECK, trusted-only insert guard,
//!   and pg_temp-last relocated search paths.
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
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const SCHEMA: &str = "rp_live";

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
         END $$; \
         REVOKE wamn_scenario_author FROM wamn_app;"
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
    legacy_effect_attempt_backfill_leg(&su).await;
    forced_rls_owner_refusal_leg(&su).await;
    v1_era_drifted_leg(&su, &url).await;
    queue_missing_leg(&su).await;
    from_zero_leg(&su).await;
    current_noop_leg(&su).await;
    authoring_storage_authority_leg(&su, &url).await;
    catalog_head_share_lock_leg(&su, &url).await;
    effect_disposition_security_drift_leg(&su).await;
    fail_kind_check_drift_leg(&su).await;
}

/// A table owner remains subject to `FORCE ROW LEVEL SECURITY`. Letting that
/// role reconcile would make the hidden legacy set look empty and permit the
/// nullable pointer/FK cutover without immutable facts, so refuse before even
/// a dry-run observation can claim completeness.
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
         CREATE SCHEMA {SCHEMA} AUTHORIZATION rp_owner_no_bypass; \
         SET ROLE rp_owner_no_bypass; \
         CREATE TABLE {SCHEMA}.node_runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
           occurrence int NOT NULL, seq int NOT NULL, attempt int NOT NULL, \
           status text NOT NULL, selected_recovery_class text, recovery_class text, \
           generation_fact_kind text, connection_generation text, \
           credential_generation text, attempt_started_at timestamptz, \
           attempt_dispatched_at timestamptz, attempt_deadline_at timestamptz, \
           attempt_input_ref text, attempt_key text, \
           PRIMARY KEY (tenant_id,run_id,node_id,occurrence)); \
         ALTER TABLE {SCHEMA}.node_runs ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {SCHEMA}.node_runs FORCE ROW LEVEL SECURITY; \
         CREATE POLICY node_runs_tenant ON {SCHEMA}.node_runs \
           USING (tenant_id = NULLIF(current_setting('app.tenant',true),'')) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant',true),'')); \
         RESET ROLE; \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,run_id,node_id,occurrence,seq,attempt,status, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_dispatched_at,attempt_deadline_at, \
            attempt_input_ref) VALUES \
           ('hidden','legacy','effect',0,0,0,'started','never-replay', \
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
    su.batch_execute("RESET ROLE; DROP TABLE pg_temp.pg_roles")
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
        "refusal occurs before pointer activation"
    );
    assert!(
        !table_exists(su, SCHEMA, "effect_attempts").await,
        "refusal occurs before ledger activation"
    );
}

/// wamn-4u7p.42.2: the additive rollout boundary. A database at the prior
/// canonical node-attempt shape receives the new ledgers and pointer, catalog
/// provenance columns, and a transactional backfill before the FK activates.
async fn legacy_effect_attempt_backfill_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    for ddl in [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane record");
    }
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog record");

    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.node_runs \
             DROP CONSTRAINT node_runs_current_effect_attempt_fk; \
         ALTER TABLE {SCHEMA}.node_runs DROP COLUMN current_effect_attempt_id; \
         DROP TABLE {SCHEMA}.effect_dispositions, \
                    {SCHEMA}.effect_disposition_requests, \
                    {SCHEMA}.effect_attempt_outcomes, \
                    {SCHEMA}.effect_attempt_dispatches, \
                    {SCHEMA}.effect_attempts; \
         ALTER TABLE catalog.flow_artifacts DROP COLUMN verified_author_principal; \
         ALTER TABLE catalog.release_manifests \
             DROP CONSTRAINT release_manifests_verified_publisher_principal_check; \
         ALTER TABLE catalog.release_manifests \
             ADD CONSTRAINT release_manifests_verified_publisher_principal_check CHECK (true); \
         INSERT INTO catalog.catalogs \
             (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','legacy-cat',1,'dev','1','applied'); \
         INSERT INTO catalog.flow_artifacts \
             (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
              artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests) \
             VALUES ('t1','legacy-flow',1,'1', \
                     '{{\"nodes\":[{{\"id\":\"http\",\"type\":\"http-request\", \
                                      \"connection\":\"erp\"}}, \
                                    {{\"id\":\"pure\",\"type\":\"map\"}}]}}', \
                     'graph','artifact-legacy','[]','interfaces','[]'); \
         INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment,status,state_json,invocation_context) VALUES \
             ('t1','legacy-live','legacy-flow',1,'legacy-cat',1,'dev','running','{{}}', \
              '{{\"principal\":{{\"artifact-digest\":\"artifact-legacy\"}}}}'), \
             ('t1','legacy-done','legacy-flow',1,'legacy-cat',1,'dev','completed','{{}}', \
              '{{\"principal\":{{\"artifact-digest\":\"artifact-legacy\"}}}}'), \
             ('t1','legacy-incomplete','legacy-flow',1,'legacy-cat',1,'dev','running','{{}}', \
              '{{\"principal\":{{\"artifact-digest\":\"artifact-legacy\"}}}}'), \
             ('t1','legacy-unresolved','legacy-flow',1,'legacy-cat',1,'dev','running','{{}}', \
              '{{\"principal\":{{\"artifact-digest\":\"artifact-legacy\"}}}}'); \
         SET session_replication_role=replica; \
         INSERT INTO {SCHEMA}.node_runs \
             (tenant_id,run_id,node_id,occurrence,seq,attempt,status, \
              selected_recovery_class,recovery_class,generation_fact_kind, \
              connection_generation,credential_generation,attempt_started_at, \
              attempt_dispatched_at,attempt_deadline_at,attempt_input_ref,attempt_key, \
              output_port,output_json,ended_at) VALUES \
             ('t1','legacy-live','http',0,0,3,'started', \
              'never-replay','never-replay','attested','erp-instance:7','credential:7', \
              '2026-08-07 10:00Z','2026-08-07 10:00:01Z','2026-08-07 10:01Z', \
              'sha256:live',NULL,NULL,NULL,NULL), \
             ('t1','legacy-done','pure',0,0,0,'success', \
              'replay','replay','not-required',NULL,NULL, \
              '2026-08-07 09:00Z','2026-08-07 09:00:01Z','2026-08-07 09:01Z', \
              'sha256:done',NULL,'main','{{\"ok\":true}}','2026-08-07 09:00:02Z'), \
             ('t1','legacy-incomplete','pure',0,0,0,'parked', \
              NULL,'replay',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL), \
             ('t1','legacy-unresolved','missing-http',0,0,0,'started', \
              'never-replay','never-replay','attested','erp-instance:8','credential:8', \
              '2026-08-07 08:30Z','2026-08-07 08:30:01Z','2026-08-07 08:31Z', \
              'sha256:unresolved',NULL,NULL,NULL,NULL), \
             ('t1','legacy-orphan','pure',0,0,0,'started', \
              'replay','replay','not-required',NULL,NULL, \
              '2026-08-07 08:00Z','2026-08-07 08:00:01Z','2026-08-07 08:01Z', \
              'sha256:orphan',NULL,NULL,NULL,NULL); \
         SET session_replication_role=origin;"
    ))
    .await
    .expect("seed pre-ledger canonical attempts");

    let plan = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("plan legacy effect-attempt migration");
    for kind in [
        RunPlaneActionKind::CreateTable,
        RunPlaneActionKind::AddColumn,
        RunPlaneActionKind::EnsureCatalogProvenance,
        RunPlaneActionKind::BackfillEffectAttempts,
        RunPlaneActionKind::RepairForeignKey,
    ] {
        assert!(
            plan.actions.iter().any(|action| action.kind == kind),
            "legacy attempt migration plans {kind:?}: {:#?}",
            plan.actions
        );
    }
    let order = |kind| {
        plan.actions
            .iter()
            .position(|action| action.kind == kind)
            .expect("planned action")
    };
    assert!(
        order(RunPlaneActionKind::EnsureCatalogProvenance)
            < order(RunPlaneActionKind::BackfillEffectAttempts)
            && order(RunPlaneActionKind::BackfillEffectAttempts)
                < order(RunPlaneActionKind::RepairForeignKey)
    );

    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("NULL-incomplete legacy authority must abort");
    assert!(
        format!("{error:#}").contains("legacy-effect-attempt-incomplete"),
        "explicit incomplete-authority refusal: {error:#}"
    );
    assert!(
        column_exists(su, "node_runs", "current_effect_attempt_id").await,
        "partial apply may add the nullable pointer before refusal"
    );
    assert!(
        table_exists(su, SCHEMA, "effect_attempts").await,
        "partial apply may create the empty ledger before refusal"
    );
    assert_eq!(
        su.query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.effect_attempts"),
            &[]
        )
        .await
        .expect("failed backfill leaves no partial facts")
        .get::<_, i64>(0),
        0
    );
    let pointer_fk_after_refusal: i64 = su
        .query_one(
            "SELECT count(*) FROM pg_constraint con \
             JOIN pg_class rel ON rel.oid=con.conrelid \
             JOIN pg_namespace ns ON ns.oid=rel.relnamespace \
             WHERE ns.nspname=$1 AND rel.relname='node_runs' \
               AND con.conname='node_runs_current_effect_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("pointer FK is still inactive")
        .get(0);
    assert_eq!(pointer_fk_after_refusal, 0);

    su.batch_execute(&format!(
        "DELETE FROM {SCHEMA}.node_runs \
           WHERE tenant_id='t1' AND run_id='legacy-incomplete'; \
         DELETE FROM {SCHEMA}.runs \
           WHERE tenant_id='t1' AND run_id='legacy-incomplete';"
    ))
    .await
    .expect("remove the explicitly refused malformed fixture row");
    let unresolved_error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("an attested attempt without a pinned connection identity must abort");
    assert!(
        format!("{unresolved_error:#}").contains("legacy-effect-attempt-connection-unresolved"),
        "explicit unresolved-connection refusal: {unresolved_error:#}"
    );
    assert_eq!(
        su.query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.effect_attempts"),
            &[]
        )
        .await
        .expect("unresolved-connection refusal precedes immutable append")
        .get::<_, i64>(0),
        0
    );
    su.batch_execute(&format!(
        "DELETE FROM {SCHEMA}.node_runs \
           WHERE tenant_id='t1' AND run_id='legacy-unresolved'; \
         DELETE FROM {SCHEMA}.runs \
           WHERE tenant_id='t1' AND run_id='legacy-unresolved';"
    ))
    .await
    .expect("remove the explicitly refused unresolved-connection fixture row");
    let orphan_error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("a complete projection lost by the legacy join must abort");
    assert!(
        format!("{orphan_error:#}").contains("legacy-effect-attempt-backfill-incomplete"),
        "explicit post-backfill zero-set refusal: {orphan_error:#}"
    );
    assert_eq!(
        su.query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.effect_attempts"),
            &[]
        )
        .await
        .expect("post-backfill refusal rolls immutable facts back")
        .get::<_, i64>(0),
        0
    );
    su.execute(
        &format!(
            "DELETE FROM {SCHEMA}.node_runs \
             WHERE tenant_id='t1' AND run_id='legacy-orphan'"
        ),
        &[],
    )
    .await
    .expect("remove the explicitly refused orphan fixture row");

    let retry = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("partial migration reapply converges");
    let retry_backfill = retry
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::BackfillEffectAttempts)
        .expect("reapply retains the legacy backfill");
    let retry_fk = retry
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::RepairForeignKey)
        .expect("reapply activates the pointer FK");
    assert!(retry_backfill < retry_fk);

    let facts = su
        .query_one(
            &format!(
                "SELECT \
                   (SELECT count(*) FROM {SCHEMA}.effect_attempts), \
                   (SELECT count(*) FROM {SCHEMA}.effect_attempt_dispatches), \
                   (SELECT count(*) FROM {SCHEMA}.effect_attempt_outcomes), \
                   (SELECT count(*) FROM {SCHEMA}.node_runs \
                     WHERE current_effect_attempt_id IS NOT NULL), \
                   (SELECT connection_name FROM {SCHEMA}.effect_attempts \
                     WHERE run_id='legacy-live'), \
                   (SELECT attempt_index FROM {SCHEMA}.effect_attempts \
                     WHERE run_id='legacy-live'), \
                   (SELECT legacy_imported FROM {SCHEMA}.effect_attempts \
                     WHERE run_id='legacy-live')"
            ),
            &[],
        )
        .await
        .expect("read immutable backfill facts");
    assert_eq!(facts.get::<_, i64>(0), 2);
    assert_eq!(facts.get::<_, i64>(1), 2);
    assert_eq!(facts.get::<_, i64>(2), 1);
    assert_eq!(facts.get::<_, i64>(3), 2);
    assert_eq!(facts.get::<_, Option<String>>(4).as_deref(), Some("erp"));
    assert_eq!(facts.get::<_, i32>(5), 3);
    assert!(
        facts.get::<_, bool>(6),
        "legacy lineage exception is explicit"
    );
    let catalog_columns: i64 = su
        .query_one(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema='catalog' AND \
               ((table_name='flow_artifacts' AND column_name='verified_author_principal') OR \
                (table_name='release_manifests' AND column_name='verified_publisher_principal'))",
            &[],
        )
        .await
        .expect("read catalog provenance columns")
        .get(0);
    assert_eq!(catalog_columns, 2);
    let catalog_checks: i64 = su
        .query_one(
            "SELECT count(*) FROM pg_constraint con \
             JOIN pg_class rel ON rel.oid=con.conrelid \
             JOIN pg_namespace ns ON ns.oid=rel.relnamespace \
             WHERE ns.nspname='catalog' AND \
               ((rel.relname='flow_artifacts' \
                 AND con.conname='flow_artifacts_verified_author_principal_check' \
                 AND pg_get_constraintdef(con.oid,true) = \
                   'CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''''::text)') \
                OR \
                (rel.relname='release_manifests' \
                 AND con.conname='release_manifests_verified_publisher_principal_check' \
                 AND pg_get_constraintdef(con.oid,true) = \
                   'CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''''::text)'))",
            &[],
        )
        .await
        .expect("read catalog provenance CHECKs")
        .get(0);
    assert_eq!(catalog_checks, 2);

    let second = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("re-run immutable attempt migration");
    assert!(
        second.is_noop(),
        "backfill is idempotent: {:#?}",
        second.actions
    );
}

/// The exact durable shape found under the deployed shared runner: legacy
/// runs/node_runs/run_queue tables with compatible history rows, but no
/// admission table, causation columns, helper functions, or lineage trigger.
async fn shared_runner_legacy_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "CREATE SCHEMA {SCHEMA}; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app; \
         CREATE TABLE {SCHEMA}.runs ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), run_id text NOT NULL, \
           flow_id text NOT NULL, flow_version int NOT NULL, \
           status text NOT NULL DEFAULT 'running' CHECK (status IN \
             ('dispatched','running','completed','failed','cancelled','infrastructure-failure')), \
           trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
           idempotency_key text, replay_of text, root_run_id text, \
           fail_kind text CHECK (fail_kind IN \
             ('terminal','retry-exhausted','invalid-input','runaway-budget')), \
           fail_node text, fail_reason text, created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tenant_id,run_id)); \
         CREATE TABLE {SCHEMA}.cron_anchor (tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           flow_id text NOT NULL,last_tick bigint NOT NULL,PRIMARY KEY(tenant_id,flow_id)); \
         CREATE TABLE {SCHEMA}.node_runs (tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL,node_id text NOT NULL,occurrence int NOT NULL DEFAULT 0, \
           seq int NOT NULL,attempt int NOT NULL DEFAULT 0,status text NOT NULL CHECK \
             (status IN ('running','parked','success','error')),output_port text,output_json jsonb, \
           input_json jsonb,error_kind text CHECK (error_kind IN \
             ('retryable','rate-limited','terminal','invalid-input','cancelled')),error_detail jsonb, \
           resume_at timestamptz,input_ref text,output_ref text,preview_head text,payload_size bigint, \
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
    su.batch_execute(&rewrite_schema(FLOW_TESTS_SQL, &schema))
        .await
        .expect("apply legacy-compatible flow tests");
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

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("upgrade shared-runner legacy fixture");
    for kind in [
        RunPlaneActionKind::AddColumn,
        RunPlaneActionKind::CreateTable,
        RunPlaneActionKind::RepairConstraint,
        RunPlaneActionKind::RepairHelperFunction,
        RunPlaneActionKind::RepairTrigger,
    ] {
        assert!(
            plan.actions.iter().any(|action| action.kind == kind),
            "shared fixture plans {kind:?}: {:#?}",
            plan.actions
        );
    }
    let counts = su
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM {SCHEMA}.runs), \
                        (SELECT count(*) FROM {SCHEMA}.node_runs)"
            ),
            &[],
        )
        .await
        .expect("read preserved row counts");
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 2);
    assert!(
        column_exists(su, "node_runs", "current_effect_attempt_id").await,
        "current-attempt pointer added before its composite FK"
    );
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "effect_disposition_requests",
        "effect_dispositions",
    ] {
        assert!(table_exists(su, SCHEMA, table).await, "{table} migrated");
    }
    for trigger in [
        "effect_disposition_requests_insert_guard",
        "effect_dispositions_insert_guard",
    ] {
        let present: bool = su
            .query_one(
                "SELECT EXISTS (SELECT FROM pg_trigger t \
                 JOIN pg_class c ON c.oid=t.tgrelid \
                 JOIN pg_namespace n ON n.oid=c.relnamespace \
                 WHERE n.nspname=$1 AND t.tgname=$2 AND NOT t.tgisinternal)",
                &[&SCHEMA, &trigger],
            )
            .await
            .expect("probe disposition insert guard")
            .get(0);
        assert!(present, "{trigger} migrated");
    }
    let pointer_fk: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid, true) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'node_runs' \
               AND con.conname = 'node_runs_current_effect_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read composite current-attempt FK")
        .get(0);
    for key in ["tenant_id", "run_id", "node_id", "occurrence"] {
        assert!(
            pointer_fk.contains(key),
            "pointer FK covers {key}: {pointer_fk}"
        );
    }

    // Attempt audit outlives prunable run history by design: the pointer is
    // from the cascading projection to the independent ledger, never back.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs(tenant_id,run_id,flow_id,flow_version,status) \
           VALUES ('t1','prunable-run','f',1,'completed'); \
         INSERT INTO {SCHEMA}.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref) \
           VALUES ('t1','00000000-0000-0000-0000-000000000042', \
                   'prunable-run','n',0,0,0,'never-replay','never-replay', \
                   'not-required',now(),now()+interval '1 minute','sha256:input'); \
         INSERT INTO {SCHEMA}.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref) \
           VALUES ('t1','00000000-0000-0000-0000-000000000042', \
                   'prunable-run','n',0,0,'started','never-replay','never-replay', \
                   'not-required',now(),now()+interval '1 minute','sha256:input'); \
         DELETE FROM {SCHEMA}.runs \
           WHERE tenant_id='t1' AND run_id='prunable-run';"
    ))
    .await
    .expect("run pruning does not conflict with independent attempt retention");
    let retained: i64 = su
        .query_one(
            &format!(
                "SELECT count(*) FROM {SCHEMA}.effect_attempts \
                 WHERE tenant_id='t1' AND run_id='prunable-run'"
            ),
            &[],
        )
        .await
        .expect("read retained attempt audit")
        .get(0);
    assert_eq!(
        retained, 1,
        "run pruning cannot erase immutable attempt facts"
    );
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,status) \
             VALUES ('t1','other-run','f',1,'running')"
        ),
        &[],
    )
    .await
    .expect("seed cross-run pointer target");
    let cross_run_pointer = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.node_runs \
                   (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000042', \
                         'other-run','n',0,0,'parked')"
            ),
            &[],
        )
        .await;
    let pointer_error =
        cross_run_pointer.expect_err("current-attempt pointer cannot cross run/node/occurrence");
    assert!(
        pointer_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "node_runs_current_effect_attempt_fk"),
        "typed cross-run current-pointer refusal: {pointer_error}"
    );
    su.execute(
        &format!(
            "DELETE FROM {SCHEMA}.runs \
             WHERE tenant_id='t1' AND run_id='other-run'"
        ),
        &[],
    )
    .await
    .expect("remove cross-run pointer fixture");

    let cross_occurrence_predecessor = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
                    predecessor_attempt_id,selected_recovery_class,recovery_class, \
                    generation_fact_kind,attempt_started_at,attempt_deadline_at, \
                    attempt_input_ref) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000043', \
                         'other-run','other-node',0,0,1, \
                         '00000000-0000-0000-0000-000000000042', \
                         'never-replay','never-replay','not-required', \
                         now(),now()+interval '1 minute','sha256:other')"
            ),
            &[],
        )
        .await;
    let predecessor_error = cross_occurrence_predecessor
        .expect_err("successor lineage cannot cross run/node/occurrence");
    assert!(
        predecessor_error
            .as_db_error()
            .and_then(|error| error.constraint())
            .is_some_and(|constraint| constraint == "effect_attempts_predecessor_fk"),
        "typed cross-occurrence predecessor refusal: {predecessor_error}"
    );

    su.batch_execute(
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
           VALUES ('t1','cat',1,'dev','1','applied'); \
         INSERT INTO catalog.catalog_heads \
           (tenant_id,catalog_id,environment,applied_catalog_version) \
           VALUES ('t1','cat','dev',1);",
    )
    .await
    .expect("seed a stable catalog head");
    su.batch_execute("SET ROLE wamn_app; SELECT set_config('app.tenant','t1',false)")
        .await
        .expect("enter tenant role");
    let head: Option<i32> = su
        .query_one(
            &format!("SELECT {SCHEMA}.lock_catalog_head('t1','cat','dev')"),
            &[],
        )
        .await
        .expect("materializer lock boundary is callable")
        .get(0);
    assert_eq!(head, Some(1));
    su.batch_execute("RESET ROLE; SELECT set_config('app.tenant','',false)")
        .await
        .expect("leave tenant role");

    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,trigger_source,event_source_run_id,event_root_run_id,event_depth) \
           VALUES ('t1','event-run','f',1,'event','event-run','event-run',0);"
    ))
    .await
    .expect("insert canonical event lineage");
    let mutation = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET event_depth=1 \
                 WHERE tenant_id='t1' AND run_id='event-run'"
            ),
            &[],
        )
        .await;
    assert!(
        mutation.is_err(),
        "lineage trigger rejects ancestry mutation"
    );

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan upgraded shared fixture");
    assert!(
        again.is_noop(),
        "shared fixture converged: {:#?}",
        again.actions
    );
}

/// Manifestations 1 + 4: the 2jkm.41-sweep drift set plus the outbox era.
async fn v1_era_drifted_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();

    // Current-era runs/node_runs/flows (the drift was queue-side)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
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
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

/// Manifestation 2 (the live poc_f1 case): run-state + flows present, queue
/// wholly absent — the three queue tables appear (M3), FKs resolve, and the
/// dead-letter ledger keeps its append-only grant shape.
async fn queue_missing_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
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
        "effect_disposition_requests",
        "effect_dispositions",
        "flows",
        "test_suites",
        "test_cases",
        "authoring_report_reservations",
        "authoring_suite_case_facts",
        "authoring_suite_reports",
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
    let schema = schema();
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows");
    su.batch_execute(&rewrite_schema(FLOW_TESTS_SQL, &schema))
        .await
        .expect("apply flow-tests");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");

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
        18,
        "all eighteen run-plane tables at target"
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

/// PLAN 6A additive storage and the host/guest authority boundary. This starts
/// from the immediately preceding catalog/suite record, proves both ctl
/// provisioning paths add only the new sections, then exercises adversarial
/// direct, inherited, membership, and ownership authority drift.
async fn authoring_storage_authority_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state before authoring additive upgrade");
    su.batch_execute(&rewrite_schema(FLOWS_SQL, &schema))
        .await
        .expect("apply flows before authoring additive upgrade");

    let legacy_flow_tests = without_marked_section(
        FLOW_TESTS_SQL,
        "-- BEGIN AUTHORING REPORT STORAGE MIGRATION",
        "-- END AUTHORING REPORT STORAGE MIGRATION",
    );
    su.batch_execute(&rewrite_schema(&legacy_flow_tests, &schema))
        .await
        .expect("apply pre-authoring flow-test storage");
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
        "authoring_report_reservations",
        "authoring_suite_case_facts",
        "authoring_suite_reports",
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
            .expect("additively install authoring report storage"),
        "the first additive flow-test upgrade reports installation"
    );
    assert!(
        !wamn_ctl::publish_catalog::ensure_flow_tests(su, &schema)
            .await
            .expect("reapply authoring report storage"),
        "the additive flow-test upgrade is idempotent"
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
        "authoring_report_reservations",
        "authoring_suite_case_facts",
        "authoring_suite_reports",
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
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_report_reservations TO wamn_app; \
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_suite_case_facts TO PUBLIC; \
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

    // Environment-owned connection generations are provisioned by the
    // platform. The author may grant/revoke only the exact already-provisioned
    // generation, and a successor generation never inherits that authority.
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

    su.batch_execute(&format!(
        "SET ROLE wamn_scenario_author; \
         SELECT set_config('app.tenant','t1',false); \
         INSERT INTO catalog.flow_drafts \
           (tenant_id,draft_id,flow_id,graph_json) \
         VALUES ('t1','draft-a','flow-a','{{}}');"
    ))
    .await
    .expect("host author can write the mutable draft surface");
    su.execute(
        wamn_scenario_catalog::authoring::grant_draft_safe_generation_sql(),
        &[&"t1", &"dev", &"erp", &1_i64, &"initial review"],
    )
    .await
    .expect("real author role grants exact generation");
    let initial_granted: std::time::SystemTime = su
        .query_one(
            "SELECT granted_at FROM catalog.draft_safe_connection_grants \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1",
            &[],
        )
        .await
        .expect("read initial grant time")
        .get(0);
    su.execute(
        wamn_scenario_catalog::authoring::revoke_draft_safe_generation_sql(),
        &[&"t1", &"dev", &"erp", &1_i64],
    )
    .await
    .expect("real author role revokes exact generation");
    let revoked_at: std::time::SystemTime = su
        .query_one(
            "SELECT revoked_at FROM catalog.draft_safe_connection_grants \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1",
            &[],
        )
        .await
        .expect("read rapid revocation time")
        .get(0);
    assert!(revoked_at >= initial_granted);
    su.execute(
        wamn_scenario_catalog::authoring::grant_draft_safe_generation_sql(),
        &[&"t1", &"dev", &"erp", &1_i64, &"review renewed"],
    )
    .await
    .expect("real author role rapidly re-grants the same generation");
    let regranted_at: std::time::SystemTime = su
        .query_one(
            "SELECT granted_at FROM catalog.draft_safe_connection_grants \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1 AND revoked_at IS NULL",
            &[],
        )
        .await
        .expect("read monotonic regrant time")
        .get(0);
    assert!(
        regranted_at > revoked_at,
        "same-generation rapid regrant creates a strictly later grant event"
    );
    let successor_grants: i64 = su
        .query_one(
            "SELECT count(*) FROM catalog.draft_safe_connection_grants \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=2",
            &[],
        )
        .await
        .expect("check successor generation authority")
        .get(0);
    assert_eq!(
        successor_grants, 0,
        "a successor generation never inherits a grant"
    );
    let uncontrolled = su
        .execute(
            "UPDATE catalog.draft_safe_connection_grants SET reason='rewritten' \
             WHERE tenant_id='t1' AND environment='dev' \
               AND instance_id='erp' AND generation=1",
            &[],
        )
        .await
        .expect_err("an active grant cannot be rewritten outside revoke/regrant");
    assert_db_code(uncontrolled, "55000", "uncontrolled grant mutation");

    // Reservation command is the one-snapshot authority for ordered cases.
    // Its array positions are zero-based report ordinals; each fact must match
    // the reserved case/content identity and deterministic run id.
    let command = r#"{"target":{"kind":"draft"},"observation-options":{},"cases":[{"case-id":"case-a","case-content-hash":"sha256:case-a","run-id":"run-a","execution-schema":"rp_live"},{"case-id":"case-b","case-content-hash":"sha256:case-b","run-id":"run-b","execution-schema":"rp_live"}]}"#;
    let lineage = r#"{"kind":"draft","validated-draft-hash":"sha256:validated"}"#;
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.authoring_report_reservations \
               (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                command_json,command_hash,lineage_json,lineage_hash) \
             VALUES ('t1','report-a','execution-a','flow-a',1,'suite-a', \
                     $1::text::jsonb,'sha256:command-a',$2::text::jsonb,'sha256:lineage-a')"
        ),
        &[&command, &lineage],
    )
    .await
    .expect("reserve report before first admission");
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.authoring_suite_case_facts \
               (tenant_id,report_id,ordinal,case_id,run_id,passed,status,outcome) \
             VALUES ('t1','report-a',0,'case-a','run-a',true,'completed','{{}}')"
        ),
        &[],
    )
    .await
    .expect("append first exact case fact");

    for (ordinal, case_id, run_id, label) in [
        (0, "wrong-case", "run-a", "wrong case"),
        (1, "case-a", "run-a", "wrong ordinal"),
        (2, "case-extra", "run-extra", "extra case"),
    ] {
        let error = su
            .execute(
                &format!(
                    "INSERT INTO {SCHEMA}.authoring_suite_case_facts \
                       (tenant_id,report_id,ordinal,case_id,run_id,passed,status,outcome) \
                     VALUES ('t1','report-a',$1,$2,$3,true,'completed','{{}}')"
                ),
                &[&ordinal, &case_id, &run_id],
            )
            .await
            .expect_err("command-mismatched case fact must be refused");
        assert_db_code(error, "23514", label);
    }
    let missing = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.authoring_suite_reports \
                   (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                    passed,lineage_json,lineage_hash) \
                 VALUES ('t1','report-a','execution-a','flow-a',1,'suite-a',true, \
                         $1::text::jsonb,'sha256:lineage-a')"
            ),
            &[&lineage],
        )
        .await
        .expect_err("non-refusal final report requires every reserved case");
    assert_db_code(missing, "23514", "missing case finalization");

    let gap_command = r#"{"cases":[{"case-id":"g0","case-content-hash":"sha256:g0","run-id":"gr0","execution-schema":"rp_live"},{"case-id":"g1","case-content-hash":"sha256:g1","run-id":"gr1","execution-schema":"rp_live"},{"case-id":"g2","case-content-hash":"sha256:g2","run-id":"gr2","execution-schema":"rp_live"}]}"#;
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.authoring_report_reservations \
               (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                command_json,command_hash,lineage_json,lineage_hash) \
             VALUES ('t1','report-gap','execution-gap','flow-a',1,'suite-a', \
                     $1::text::jsonb,'sha256:command-gap',$2::text::jsonb,'sha256:lineage-a')"
        ),
        &[&gap_command, &lineage],
    )
    .await
    .expect("reserve gap mutant report");
    for (ordinal, case_id, run_id) in [(0, "g0", "gr0"), (2, "g2", "gr2")] {
        su.execute(
            &format!(
                "INSERT INTO {SCHEMA}.authoring_suite_case_facts \
                   (tenant_id,report_id,ordinal,case_id,run_id,passed,status,outcome) \
                 VALUES ('t1','report-gap',$1,$2,$3,false,'failed','{{}}')"
            ),
            &[&ordinal, &case_id, &run_id],
        )
        .await
        .expect("individual gap mutant fact still matches its command entry");
    }
    let gap = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.authoring_suite_reports \
                   (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                    passed,lineage_json,lineage_hash,refusal) \
                 VALUES ('t1','report-gap','execution-gap','flow-a',1,'suite-a',false, \
                         $1::text::jsonb,'sha256:lineage-a', \
                         '{{\"kind\":\"capture-interrupted\"}}')"
            ),
            &[&lineage],
        )
        .await
        .expect_err("a refusal may retain only a contiguous observed prefix");
    assert_db_code(gap, "23514", "gapped refusal finalization");

    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.authoring_suite_case_facts \
               (tenant_id,report_id,ordinal,case_id,run_id,passed,status,outcome) \
             VALUES ('t1','report-a',1,'case-b','run-b',true,'completed','{{}}')"
        ),
        &[],
    )
    .await
    .expect("append second exact case fact");
    su.batch_execute("BEGIN")
        .await
        .expect("begin final report transaction");
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.authoring_suite_reports \
               (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                passed,lineage_json,lineage_hash) \
             VALUES ('t1','report-a','execution-a','flow-a',1,'suite-a',true, \
                     $1::text::jsonb,'sha256:lineage-a')"
        ),
        &[&lineage],
    )
    .await
    .expect("insert complete immutable final report");
    su.execute(
        &format!(
            "UPDATE {SCHEMA}.authoring_report_reservations \
             SET state='finalized', finalized_at=clock_timestamp() \
             WHERE tenant_id='t1' AND report_id='report-a'"
        ),
        &[],
    )
    .await
    .expect("finalize reservation after final report exists");
    su.batch_execute("COMMIT")
        .await
        .expect("commit report insert and finalization atomically");
    let late_fact = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.authoring_suite_case_facts \
                   (tenant_id,report_id,ordinal,case_id,run_id,passed,status,outcome) \
                 VALUES ('t1','report-a',1,'case-b','run-b',true,'completed','{{}}')"
            ),
            &[],
        )
        .await
        .expect_err("finalized report facts cannot be appended");
    assert_db_code(late_fact, "23514", "post-finalization fact append");

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
    let immutable = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.authoring_suite_case_facts SET passed=false \
                 WHERE tenant_id='t1' AND report_id='report-a' AND ordinal=0"
            ),
            &[],
        )
        .await
        .expect_err("even migration authority hits immutable report triggers");
    assert_db_code(immutable, "55000", "immutable case fact update");

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
                "INSERT INTO {SCHEMA}.authoring_report_reservations \
                 (tenant_id,report_id,execution_id,flow_id,suite_flow_version,suite_id, \
                  command_json,command_hash,lineage_json,lineage_hash) VALUES \
                 ('t1','forged','forged','flow-a',1,'suite-a', \
                  '{{\"cases\":[]}}','x','{{\"kind\":\"draft\"}}','x')"
            ),
            &[],
        )
        .await
        .expect_err("guest cannot forge a report reservation");
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
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply lock bridge");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog storage for lock probe");
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

/// wamn-4u7p.42: repair the pre-hardening disposition ledger without replacing
/// its table. The identity column is additive, the old wall-clock history index
/// is recreated, helper authority/search-path definitions converge exactly, and
/// the closed CHECK rejects the two SQL-NULL outcome shapes that previously
/// passed PostgreSQL CHECK semantics.
async fn effect_disposition_security_drift_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    for ddl in [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane record");
    }
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");

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
         ALTER TABLE {SCHEMA}.effect_attempts \
             DROP CONSTRAINT effect_attempts_predecessor_fk; \
         ALTER TABLE {SCHEMA}.effect_attempts \
             DISABLE TRIGGER effect_attempts_insert_guard; \
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
        (
            RunPlaneActionKind::RepairForeignKey,
            "effect_attempts.effect_attempts_predecessor_fk",
        ),
        (
            RunPlaneActionKind::RepairTrigger,
            "effect_attempts.effect_attempts_insert_guard",
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
         GRANT INSERT ON {SCHEMA}.effect_attempts TO wamn_app; \
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
                   (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
                    selected_recovery_class,recovery_class,generation_fact_kind, \
                    attempt_started_at,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000509', \
                         'forged','effect',0,0,0,'never-replay','never-replay', \
                         'not-required',now(),now()+interval '1 minute','sha256:forged')"
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
         REVOKE INSERT ON {SCHEMA}.effect_attempts FROM wamn_app; \
         REVOKE INSERT ON {SCHEMA}.effect_disposition_requests FROM wamn_app; \
         DROP TABLE pg_temp.pg_roles;"
    ))
    .await
    .expect("leave the platform-member application session");
    let fact_error =
        direct_fact_append.expect_err("stale table grants cannot authorize immutable fact append");
    assert!(
        fact_error
            .as_db_error()
            .is_some_and(|db| db.message() == "effect-fact-append-requires-migration-authority"),
        "typed immutable-fact refusal: {fact_error}"
    );
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
             (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
              selected_recovery_class,recovery_class,generation_fact_kind, \
              attempt_started_at,attempt_deadline_at,attempt_input_ref) \
             VALUES \
             ('t1','00000000-0000-0000-0000-000000000530', \
                     'audit-run','n',0,0,0,'never-replay','never-replay', \
                     'not-required',now(),now()+interval '1 minute','sha256:audit'), \
             ('t1','00000000-0000-0000-0000-000000000540', \
                     'audit-run','temporal',0,1,0,'never-replay','never-replay', \
                     'not-required',now(),now()+interval '1 minute','sha256:temporal'); \
         INSERT INTO {SCHEMA}.effect_attempt_dispatches \
             (tenant_id,attempt_id,attempt_started_at,dispatched_at) \
             SELECT tenant_id,attempt_id,attempt_started_at, \
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
                   (tenant_id,attempt_id,attempt_started_at,dispatched_at) \
                 SELECT tenant_id,attempt_id,attempt_started_at, \
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
               (tenant_id,attempt_id,attempt_started_at,dispatched_at) \
             SELECT tenant_id,attempt_id,attempt_started_at, \
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

/// wamn-fqg.16: a schema whose `runs.fail_kind` CHECK predates cjv.4's
/// `'runaway-budget'` literal rejects a runaway `mark_failed` UPDATE — the
/// verdict is lost from the audit row. The reconcile drops the observed CHECK
/// and re-adds the 5-literal record form; the runaway UPDATE then succeeds, the
/// canonical def carries `runaway-budget`, and a re-run is a no-op.
async fn fail_kind_check_drift_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
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
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

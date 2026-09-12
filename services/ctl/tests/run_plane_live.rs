//! Live-apply gate for `reconcile-run-plane` (E4/R14-migration, wamn-1wdq): the
//! durable migration path for provisioned run-plane schemas, shown against a
//! REAL Postgres in every starting state the bead's manifestations recorded.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway Postgres (recipe: docs/operations/build-and-test.md [RUN-PLANE-RECONCILE]);
//! skipped cleanly when unset. The legs run sequentially under the main test
//! entry (they share the `catalog` schema and the `wamn_app` role); the
//! execution-pin cutover has one separate test entry:
//!
//! - **shared-runner legacy** (wamn-l5i9.73): the deployed fixture's old
//!   runs/run_queue shape gains canonical admission/causation
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
//!   database without project schemas while the cluster-scoped runtime roles
//!   remain shared. `--dry-run` first, shown STRICTLY read-only; then the apply
//!   provisions everything — run plane + `catalog` schema — and a functional
//!   smoke as a MINTED GUEST GENERATION LOGIN (not the bare `wamn_app` ACL
//!   role, under which `current_tenant_key` derives NULL and every read
//!   matches nothing in silence) shows the sections' grants + RLS isolation
//!   end-to-end.
//! - **invocation retention cutover**: the legacy admission expiry column/index
//!   are removed and the client-key carrier becomes optional; a second pass is
//!   a no-op.
//! - **rerun-lineage cutover**: populated runs retain their payload and trusted
//!   event causation while only `replay_of`, `root_run_id`, and the exact
//!   `runs_root` index disappear. A same-name foreign index refuses atomically.
//! - **failure-detail cutover**: populated runs retain `fail_kind` and their
//!   typed caller outcome while retired per-node detail is deliberately
//!   discarded. A dependent view refuses with `2BP01` before role bootstrap.
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
//! - **retired effect-disposition cutover**: empty parent/child ledgers are
//!   locked and removed child-first; populated history refuses atomically with
//!   the exact archive-or-reprovision diagnostic.
//! - **fail_kind CHECK drift** (wamn-fqg.16): a schema whose `runs.fail_kind`
//!   CHECK predates cjv.4's `'runaway-budget'` literal REJECTS a runaway
//!   `mark_failed` UPDATE. The verb drops the observed CHECK and re-adds the
//!   5-literal record form; the runaway UPDATE then succeeds and a re-run is a
//!   no-op (the reconciled CHECK converges with fresh provisioning).

mod support;

#[path = "run_plane_live/catalog.rs"]
mod catalog;
#[path = "run_plane_live/effect_ledgers.rs"]
mod effect_ledgers;
#[path = "run_plane_live/partition.rs"]
mod partition;
#[path = "run_plane_live/run_history.rs"]
mod run_history;
#[path = "run_plane_live/target_identity.rs"]
mod target_identity;

use partition::{
    partition_plane_active_lease_refusal_leg, partition_plane_authored_ordering_refusal_leg,
    partition_plane_cutover_leg, partition_plane_dead_letter_refusal_leg,
    partition_plane_unobservable_lease_refusal_leg,
};

use run_history::{
    child_run_cutover_leg, failure_detail_cutover_leg, node_runs_retirement_leg,
    persisted_literal_check_drift_leg, rerun_lineage_cutover_leg, shared_runner_legacy_leg,
};

use effect_ledgers::{
    effect_writer_cutover_leg, effect_writer_populated_refusal_leg, frame_identity_cutover_leg,
};

use catalog::{capture_mode_additive_leg, stored_suite_cutover_leg, two_plane_residency_leg};

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::{
    CredentialGeneration, DISPATCH_READER_ROLE, WorkloadRoleFamily, WorkloadRoleScope,
    effect_writer_generation_role, project_env_database_name, sql as provision_sql,
    workload_generation_role,
};
use wamn_ctl::reconcile_run_plane::{
    self, RECONCILE_TARGET_REFUSAL_PREFIX, ReconcileRunPlaneArgs, ReconcileTargetError,
    ReconcileTargetErrorKind,
};
use wamn_ctl::verification_policy::project_environment_policy;
use wamn_schema_control::{BareSchemaName, RunPlaneActionKind, rewrite_schema};

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
/// The CONTROL plane's canonical record. Read here only to lift the co-resident
/// relation's declaration text, never installed as a whole.
const CONTROL_PORTABLE_STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

const SCHEMA: &str = "rp_live";
const DISPATCH_READER_PASSWORD: &str = "dispatch-reader-run-plane-probe";
const GUEST_GENERATION_PASSWORD: &str = "guest-generation-run-plane-probe";
const EMPTY_EXECUTION_BUNDLE_HASH: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const CLI_ORG: &str = "acme";
const CLI_PROJECT: &str = "billing";
const CLI_ENV: &str = "dev";
const CLI_INSTANCE: &str = "k3m9x2p7";

async fn seed_system_env_policy(su: &Client, durability_class: &str) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         CREATE SCHEMA registry AUTHORIZATION wamn_system; \
         SET ROLE wamn_system; \
         CREATE TABLE registry.env_policies ( \
           org text NOT NULL, name text NOT NULL, recovery_domain jsonb NOT NULL, \
           promotion_rank int NOT NULL, instances int NOT NULL, storage text NOT NULL, \
           cpu text NOT NULL, memory text NOT NULL, image text NOT NULL, \
           backup_cadence text NOT NULL, wal_retention text NOT NULL, \
           hibernation text NOT NULL, durability_class text NOT NULL, \
           PRIMARY KEY (org, name)); \
         CREATE TABLE registry.project_envs ( \
           org text NOT NULL, project text NOT NULL, env text NOT NULL, \
           secret_name text NOT NULL, secret_namespace text, \
           instance_suffix text NOT NULL, PRIMARY KEY (org, project, env)); \
         RESET ROLE",
    )
    .await
    .expect("create system env-policy fixture");
    su.execute(
        "INSERT INTO registry.env_policies \
           (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image, \
            backup_cadence,wal_retention,hibernation,durability_class) \
         VALUES ('acme','dev','\"own\"',0,1,'1Gi','100m','128Mi','postgres','','','off',$1)",
        &[&durability_class],
    )
    .await
    .expect("seed system env policy");
    su.execute(
        "INSERT INTO registry.project_envs \
           (org,project,env,secret_name,instance_suffix) \
         VALUES ($1,$2,$3,'wamn-db-acme--billing--dev',$4)",
        &[&CLI_ORG, &CLI_PROJECT, &CLI_ENV, &CLI_INSTANCE],
    )
    .await
    .expect("seed recorded project-env target");
}

async fn seed_pre_durability_system_env_policy(su: &Client) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         CREATE SCHEMA registry AUTHORIZATION wamn_system; \
         SET ROLE wamn_system; \
         CREATE TABLE registry.env_policies ( \
           org text NOT NULL, name text NOT NULL, recovery_domain jsonb NOT NULL, \
           promotion_rank int NOT NULL, instances int NOT NULL, storage text NOT NULL, \
           cpu text NOT NULL, memory text NOT NULL, image text NOT NULL, \
           backup_cadence text NOT NULL, wal_retention text NOT NULL, \
           hibernation text NOT NULL, PRIMARY KEY (org, name)); \
         CREATE TABLE registry.project_envs ( \
           org text NOT NULL, project text NOT NULL, env text NOT NULL, \
           secret_name text NOT NULL, secret_namespace text, \
           instance_suffix text NOT NULL, PRIMARY KEY (org, project, env)); \
         INSERT INTO registry.env_policies \
           (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image, \
            backup_cadence,wal_retention,hibernation) \
         VALUES ('acme','dev','\"own\"',0,1,'1Gi','100m','128Mi','postgres','','','off'); \
         INSERT INTO registry.project_envs \
           (org,project,env,secret_name,instance_suffix) \
         VALUES ('acme','billing','dev','wamn-db-acme--billing--dev','k3m9x2p7'); \
         RESET ROLE",
    )
    .await
    .expect("create pre-durability system env-policy fixture");
}

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

fn database_url(base_url: &str, database: &str) -> String {
    let mut url = url::Url::parse(base_url).expect("parse Postgres fixture URL");
    url.set_path(&format!("/{database}"));
    url.to_string()
}

async fn recreate_database(su: &Client, database: &str) {
    su.batch_execute(&provision_sql::drop_database_named_sql(database))
        .await
        .expect("drop stale target database");
    su.batch_execute(&provision_sql::create_database_named_sql(database))
        .await
        .expect("create target database");
}

async fn drop_database(su: &Client, database: &str) {
    su.batch_execute(&provision_sql::drop_database_named_sql(database))
        .await
        .expect("drop target database");
}

/// The dispatch-reader A generation this leg dials as, DERIVED from the same
/// builder `provision-project-env` uses rather than spelled (`wamn-0h0g.22.24`).
fn dispatch_reader_generation(database: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::DispatchReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: "acme",
            project: "billing",
            environment: "dev",
            database,
        },
        CredentialGeneration::A,
    )
    .expect("the dispatch reader takes a project-environment scope")
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

async fn seed_run_admission_facts(
    su: &Client,
    tenant_id: &str,
    package_id: &str,
    effective_release_id: i32,
    environment: &str,
    durability_class: &str,
) {
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ($1,$2,$3)"
        ),
        &[&tenant_id, &environment, &durability_class],
    )
    .await
    .expect("seed the project-local environment policy");
    su.execute(
        "INSERT INTO catalog.packages \
           (tenant_id,package_id,package_version,manifest_sha256) \
         VALUES ($1,$2,'1.0.0',$3)",
        &[&tenant_id, &package_id, &EMPTY_EXECUTION_BUNDLE_HASH],
    )
    .await
    .expect("seed run-pin package");
    su.execute(
        "INSERT INTO catalog.effective_releases \
           (tenant_id,effective_release_id,environment,verified_publisher_principal) \
         VALUES ($1,$2,$3,'run-plane-live')",
        &[&tenant_id, &effective_release_id, &environment],
    )
    .await
    .expect("seed run-pin effective release");
    su.execute(
        "INSERT INTO catalog.effective_release_packages \
           (tenant_id,effective_release_id,package_id,package_version) \
         VALUES ($1,$2,$3,'1.0.0')",
        &[&tenant_id, &effective_release_id, &package_id],
    )
    .await
    .expect("seed run-pin package membership");
}

/// Hermetic reset: drop the target schema + the shared `catalog` schema and
/// ensure the `wamn_app` role, so every leg builds its own starting state.
/// Hermetic per CLUSTER, not merely per schema (wamn-0h0g.12.123). PostgreSQL
/// roles are cluster-wide, and the reconciler now converges
/// `wamn_dispatch_reader`'s in-database surface WHEN THAT ROLE EXISTS — so a
/// reader left behind by another gate against the same container would make
/// `current_noop_leg`'s first plan legitimately non-empty. `DROP OWNED BY` is
/// what makes the role droppable: `DROP ROLE` refuses while any acl entry
/// anywhere still names it. `wamn_app` and the two writer roles are created or
/// hardened because the run-plane DDL and reconciler name them.
async fn reset(su: &Client) {
    su.batch_execute(&provision_sql::ensure_app_acl_role_sql())
        .await
        .expect("harden stable app ACL role");
    su.batch_execute(&provision_sql::drain_app_role_sessions_sql())
        .await
        .expect("drain retired app login sessions after hardening commit");
    su.batch_execute(&format!(
        "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
         DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
         DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $reader_generations$ DECLARE generation record; BEGIN \
           FOR generation IN SELECT rolname FROM pg_roles \
                              WHERE rolname ~ '^{DISPATCH_READER_ROLE}_[0-9a-f]{{40}}_[ab]$' LOOP \
             EXECUTE format('DROP OWNED BY %I', generation.rolname); \
             EXECUTE format('DROP ROLE %I', generation.rolname); \
           END LOOP; \
         END $reader_generations$; \
         DO $reader$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}') THEN \
             EXECUTE 'DROP OWNED BY {DISPATCH_READER_ROLE}'; \
             EXECUTE 'DROP ROLE {DISPATCH_READER_ROLE}'; \
           END IF; \
         END $reader$; \
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
             'REVOKE CONNECT ON DATABASE %I FROM wamn_app', current_database() \
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
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane schema");
    }
}

/// The retired flow registry (`deploy/sql/flows.sql`), deleted by
/// wamn-0h0g.12.102 (e45ca35b). It is no longer one of the run-plane files
/// `install_current_run_plane` applies — but `partition_plane_cutover_sql`
/// still locks and preflights it for schemas that physically retain the table,
/// so a leg showing that refusal must install it. Mirrors the unit-side
/// fixture `run_plane::add_legacy_flow_registry`.
async fn install_legacy_flow_registry(su: &Client) {
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.flows ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           flow_id text NOT NULL, version int NOT NULL, \
           active boolean NOT NULL DEFAULT false, \
           graph_json jsonb NOT NULL, \
           created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,flow_id,version));"
    ))
    .await
    .expect("install retired flow registry");
}

fn assert_db_code(error: tokio_postgres::Error, expected: &str, context: &str) {
    let actual = error
        .as_db_error()
        .map(|database| database.code().code())
        .unwrap_or("non-database-error");
    assert_eq!(actual, expected, "{context}: {error}");
}

fn assert_db_code_in_chain(error: &anyhow::Error, expected: &str, context: &str) {
    let actual = error
        .chain()
        .find_map(|source| {
            source
                .downcast_ref::<tokio_postgres::Error>()
                .and_then(tokio_postgres::Error::as_db_error)
                .map(|database| database.code().code())
        })
        .unwrap_or("non-database-error");
    assert_eq!(actual, expected, "{context}: {error:#}");
}

#[tokio::test]
async fn run_plane_reconcile_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-1wdq run-plane gate");
        return;
    };
    let su = connect(&url).await;
    node_runs_retirement_leg(&su).await;
    shared_runner_legacy_leg(&su).await;
    frame_identity_cutover_leg(&su).await;
    effect_writer_cutover_leg(&su).await;
    effect_writer_populated_refusal_leg(&su).await;
    provisioner_minted_generation_leg(&su).await;
    forced_rls_owner_refusal_leg(&su).await;
    partition_plane_authored_ordering_refusal_leg(&su).await;
    partition_plane_cutover_leg(&su).await;
    partition_plane_active_lease_refusal_leg(&su).await;
    partition_plane_unobservable_lease_refusal_leg(&su).await;
    partition_plane_dead_letter_refusal_leg(&su).await;
    let cli_database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    recreate_database(&su, &cli_database).await;
    let cli_url = database_url(&url, &cli_database);
    let cli_su = connect(&cli_url).await;
    v1_era_drifted_leg(&cli_su, &su, &url, &cli_url).await;
    drop(cli_su);
    drop_database(&su, &cli_database).await;
    queue_missing_leg(&su).await;
    from_zero_leg(&su, &url).await;
    child_run_cutover_leg(&su).await;
    rerun_lineage_cutover_leg(&su).await;
    failure_detail_cutover_leg(&su).await;
    capture_mode_additive_leg(&su, &url).await;
    stored_suite_cutover_leg(&su).await;
    environment_policy_row_security_leg(&su, &url).await;
    current_noop_leg(&su).await;
    two_plane_residency_leg(&su).await;
    retired_effect_disposition_cutover_leg(&su).await;
    persisted_literal_check_drift_leg(&su).await;
    dispatch_reader_read_surface_leg(&su, &url).await;
}

/// Own entry so the provisioner-minted generation contract can be run — and
/// mutated — alone (wamn-0h0g.12.178).
#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn provisioner_minted_generation_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    provisioner_minted_generation_leg(&su).await;
}

/// The dispatcher read principal's in-database surface (wamn-0h0g.12.123).
///
/// The `SELECT` grants target relations in the run-plane schema, which does not
/// exist at provision time — so the reconciler owns them, on the same convergent
/// footing as every other privilege it holds. **Runs last**: it is the only leg
/// that needs `wamn_dispatch_reader` to EXIST, and it drops the role again on the
/// way out so nothing downstream inherits it.
async fn dispatch_reader_read_surface_leg(su: &Client, url: &str) {
    reset(su).await;
    install_current_run_plane(su).await;
    let schema = schema();
    let database: String = su
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read current database")
        .get(0);

    // Provisioning's half (wamn-0h0g.12.122, cut over to generations by
    // wamn-0h0g.22.24), from the SAME builders `provision-project-env` emits:
    // the connection-free stable ACL role, the REVOKE that converges a
    // pre-cutover CONNECT off it, and one A/B generation which is the only thing
    // that can actually log in. Everything after this point must come from the
    // reconciler alone — that is what "no manual SQL" means.
    let reader_generation = dispatch_reader_generation(&database);
    su.batch_execute(&provision_sql::ensure_workload_acl_role_sql(
        WorkloadRoleFamily::DispatchReader,
    ))
    .await
    .expect("mint the stable dispatch-reader ACL role");
    su.batch_execute(&provision_sql::revoke_dispatch_reader_connect_sql(
        &database,
    ))
    .await
    .expect("converge the stable dispatch reader off CONNECT");
    su.batch_execute(&provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::DispatchReader,
        &database,
        &reader_generation,
        DISPATCH_READER_PASSWORD,
        "2100-01-01T00:00:00Z",
    ))
    .await
    .expect("prepare the dispatch-reader generation");
    // The STABLE role is connection-free and the GENERATION holds the CONNECT.
    // Asserted from the server, because that inversion is the whole bead.
    let stable_connect: bool = su
        .query_one(
            &format!("SELECT has_database_privilege('{DISPATCH_READER_ROLE}', $1, 'CONNECT')"),
            &[&database],
        )
        .await
        .expect("read stable dispatch-reader CONNECT")
        .get(0);
    assert!(
        !stable_connect,
        "the cluster-global dispatch reader still holds CONNECT: every generation \
         inherits it into every database on the cluster"
    );
    let generation_connect: bool = su
        .query_one(
            "SELECT has_database_privilege($1, $2, 'CONNECT')",
            &[&reader_generation, &database],
        )
        .await
        .expect("read generation CONNECT")
        .get(0);
    assert!(
        generation_connect,
        "the generation cannot reach its database"
    );

    // A schema at the schema of record still owes the reader its read surface:
    // deploy/sql grants the reader nothing, and this verb is where it lands.
    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("apply the reader read surface");
    assert_eq!(
        plan.actions
            .iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>(),
        vec![RunPlaneActionKind::RepairDispatchReaderPrivilege],
        "a current schema owes exactly the reader repair: {:#?}",
        plan.actions
    );

    // *** THE wamn-0h0g.12.40 GUARD. *** An observation arm that encodes a shape
    // the grant can never satisfy leaves drift permanently true, and the
    // reconciler plans this repair on EVERY pass without ever converging. Only a
    // live second pass against the CONVERGED database can catch that.
    let again = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("second reconcile");
    assert!(
        again.is_noop(),
        "the reader repair repeats on a converged database — the observation \
         arm encodes a state the grant cannot reach: {:#?}",
        again.actions
    );
    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("third reconcile, read-only");
    assert!(dry.is_noop(), "dry-run drift: {:#?}", dry.actions);

    // The dispatcher dials and reads, with no manual SQL between provisioning
    // and the read.
    let reader = connect_as(url, &reader_generation, DISPATCH_READER_PASSWORD).await;
    for relation in ["run_queue", "effect_attempts"] {
        reader
            .query_one(&format!("SELECT count(*) FROM {SCHEMA}.{relation}"), &[])
            .await
            .unwrap_or_else(|error| panic!("reader cannot read {relation}: {error}"));
    }
    // …and nothing wider. `runs` is the relation the dispatcher never touches.
    for denied in [
        format!("SELECT count(*) FROM {SCHEMA}.runs"),
        format!("INSERT INTO {SCHEMA}.run_queue (tenant_id) VALUES ('t')"),
    ] {
        let error = reader
            .batch_execute(&denied)
            .await
            .expect_err(&format!("reader was allowed {denied:?}"));
        assert_db_code(error, "42501", &denied);
    }
    drop(reader);

    // A widened reader narrows back: the repair REVOKEs over the same scope it
    // grants, so this is convergence and not merely a first-time install.
    su.batch_execute(&format!(
        "GRANT SELECT ON {SCHEMA}.runs TO \"{DISPATCH_READER_ROLE}\"; \
         GRANT INSERT, UPDATE ON {SCHEMA}.run_queue TO \"{DISPATCH_READER_ROLE}\"; \
         GRANT CREATE ON SCHEMA {SCHEMA} TO \"{DISPATCH_READER_ROLE}\";"
    ))
    .await
    .expect("widen the reader");
    let widened = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("observe the widened reader");
    assert_eq!(
        widened
            .actions
            .iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>(),
        vec![RunPlaneActionKind::RepairDispatchReaderPrivilege],
        "a widened reader is drift: {:#?}",
        widened.actions
    );
    reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("narrow the reader back");
    let narrowed = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile after narrowing");
    assert!(
        narrowed.is_noop(),
        "the narrowed reader did not converge: {:#?}",
        narrowed.actions
    );

    let reader = connect_as(url, &reader_generation, DISPATCH_READER_PASSWORD).await;
    let error = reader
        .batch_execute(&format!("SELECT count(*) FROM {SCHEMA}.runs"))
        .await
        .expect_err("the widened SELECT on runs survived the reconcile");
    assert_db_code(error, "42501", "narrowed reader reads runs");
    drop(reader);
    assert!(
        !su.query_one(
            "SELECT pg_catalog.has_schema_privilege($1, $2, 'CREATE')",
            &[&DISPATCH_READER_ROLE, &SCHEMA],
        )
        .await
        .expect("probe reader CREATE")
        .get::<_, bool>(0),
        "the widened schema CREATE survived the reconcile"
    );

    // Leave the cluster as this leg found it: the role is cluster-wide.
    reset(su).await;
}

/// Also a leg of `run_plane_reconcile_live`, and a separate entry for the same
/// reason `stored_suite_cutover_live` is: so it can be run — and reached — on
/// its own. Run the whole file with `-- --test-threads=1`; the entries share the
/// `catalog` schema, the run-plane schema, and the cluster-wide roles.
#[tokio::test]
async fn dispatch_reader_read_surface_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the dispatch-reader read-surface gate");
        return;
    };
    let su = connect(&url).await;
    dispatch_reader_read_surface_leg(&su, &url).await;
}

#[tokio::test]
async fn environment_policy_row_security_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the environment-policy RLS gate");
        return;
    };
    let su = connect(&url).await;
    environment_policy_row_security_leg(&su, &url).await;
}

#[tokio::test]
async fn registry_durability_schema_ensure_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the registry durability migration gate");
        return;
    };
    let system_su = connect(&url).await;
    let database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    recreate_database(&system_su, &database).await;
    let target_url = database_url(&url, &database);
    let target_su = connect(&target_url).await;
    registry_durability_schema_ensure_leg(&target_su, &system_su, &url, &target_url).await;
    drop(target_su);
    drop_database(&system_su, &database).await;
}

#[tokio::test]
async fn retired_effect_disposition_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping retired disposition cutover gate");
        return;
    };
    let su = connect(&url).await;
    retired_effect_disposition_cutover_leg(&su).await;
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
         CREATE TABLE {SCHEMA}.runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, \
           flow_id text NOT NULL, flow_version int NOT NULL, \
           status text NOT NULL, \
           created_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,run_id)); \
         ALTER TABLE {SCHEMA}.runs ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {SCHEMA}.runs FORCE ROW LEVEL SECURITY; \
         CREATE POLICY runs_tenant ON {SCHEMA}.runs \
           USING (tenant_id = NULLIF(current_setting('app.tenant',true),'')) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant',true),'')); \
         RESET ROLE; \
         INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,status) VALUES \
           ('hidden','legacy','f',1,'running'); \
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
        su.query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
            .await
            .expect("hidden legacy row remains")
            .get::<_, i64>(0),
        1
    );
    assert!(
        !column_exists(su, "runs", "capture_mode").await,
        "refusal performs no schema mutation"
    );
    assert!(
        !table_exists(su, SCHEMA, "effect_attempts").await,
        "refusal occurs before ledger activation"
    );
}

/// Remove one disposable generation role.
///
/// `DROP OWNED BY` reaches only the CURRENT database (plus the shared-object
/// grants, which is where this leg's `CONNECT` lives), and it must run before
/// `DROP ROLE` or the drop is refused. Guarded by an existence check so a leg
/// can call it both before and after minting.
async fn drop_generation_role(su: &Client, role: &str) {
    su.batch_execute(&format!(
        "DO $drop_generation$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
             EXECUTE 'DROP OWNED BY {role}'; \
             EXECUTE 'DROP ROLE {role}'; \
           END IF; \
         END $drop_generation$;"
    ))
    .await
    .expect("drop the disposable generation role");
}

/// Mint one guest-SQL generation LOGIN for `tenant` and dial it
/// (`wamn-0h0g.22.36`).
///
/// # THE BARE `wamn_app` ACL ROLE CANNOT STAND IN FOR THIS
///
/// After `wamn-0h0g.22.6` the tenant floor is
/// `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
/// and `current_tenant_key` recovers a key ONLY from the guest generation
/// pattern. Under the ACL role it derives NULL, a NULL-compared predicate
/// matches nothing, and the read returns ZERO ROWS WITH NO ERROR — an assertion
/// dialing `wamn_app` cannot tell REFUSED from MATCHED-NOTHING, which is the
/// whole point of a FORCE-RLS probe.
///
/// The login name is DERIVED by the production builder, never spelled, so the
/// digest under test is the digest `provision-project-env` would mint. The
/// production prepare builder also supplies the exact membership edge, direct
/// database `CONNECT`, finite expiry and stable-role hardening. No test
/// authenticates as the stable `wamn_app` ACL role.
async fn mint_guest_generation(su: &Client, url: &str, tenant: &str) -> (String, Client) {
    let database: String = su
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read the reconciled database")
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("the guest family takes a tenant scope");
    drop_generation_role(su, &generation).await;
    su.batch_execute(&provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &generation,
        GUEST_GENERATION_PASSWORD,
        "2100-01-01T00:00:00Z",
    ))
    .await
    .expect("mint the guest generation login through the production builder");
    // A SUPERUSER (or BYPASSRLS) FIXTURE MASKS RLS ENTIRELY, so the probe role
    // is asserted unprivileged from `pg_roles` — across everything it inherits,
    // because the attribute is recovered through the whole membership chain.
    let masking: Vec<String> = su
        .query(
            "SELECT rolname FROM pg_catalog.pg_roles \
              WHERE (rolsuper OR rolbypassrls) AND pg_has_role($1, oid, 'USAGE')",
            &[&generation],
        )
        .await
        .expect("probe the generation's superuser/bypassrls reach")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(
        masking.is_empty(),
        "the guest generation reaches an RLS-masking role: {masking:?}"
    );
    let client = connect_as(url, &generation, GUEST_GENERATION_PASSWORD).await;
    (generation, client)
}

/// wamn-0h0g.12.178: the reconciler must ACCEPT the generation shape its OWN
/// provisioner mints.
///
/// Every other generation leg in this file builds its role by hand with a bare
/// `GRANT wamn_effect_writer TO <generation>`, which PostgreSQL 16+ defaults to
/// `SET TRUE`. The prepare path emits `SET FALSE` — the tighter posture
/// `docs/exe-model.md` names "rotating login generations with no `SET ROLE`
/// escape" — so the edge production actually carries never reached this check,
/// and `generation_role_contract_violation_sql` was left demanding the opposite
/// of what the provisioner writes (`358f6792` flipped the provisioner and its
/// own check without flipping the reconciler). This leg mints the generation
/// through the provisioner's OWN batch, so the shape under test is the shape
/// `provision-project-env --prepare` installs, and then requires the real verb
/// to converge.
async fn provisioner_minted_generation_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");

    let database: String = su
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read the reconciled database")
        .get(0);
    let generation = effect_writer_generation_role("t1", &database, CredentialGeneration::A);
    drop_generation_role(su, &generation).await;
    su.batch_execute(&provision_sql::prepare_effect_writer_generation_sql(
        &database,
        &generation,
        "run-plane-prepared-generation",
        "2099-01-01T00:00:00Z",
    ))
    .await
    .expect("mint the generation through the real prepare batch");

    // The server's own answer for the edge the provisioner just wrote, pinned
    // so a later provisioner change cannot silently re-open the disagreement.
    let edge = su
        .query_one(
            "SELECT edge.admin_option, edge.inherit_option, edge.set_option \
               FROM pg_catalog.pg_auth_members AS edge \
               JOIN pg_catalog.pg_roles AS parent ON parent.oid = edge.roleid \
               JOIN pg_catalog.pg_roles AS member ON member.oid = edge.member \
              WHERE member.rolname = $1 AND parent.rolname = 'wamn_effect_writer'",
            &[&generation],
        )
        .await
        .expect("read the minted stable-role membership edge");
    assert_eq!(
        (
            edge.get::<_, bool>(0),
            edge.get::<_, bool>(1),
            edge.get::<_, bool>(2),
        ),
        (false, true, false),
        "the prepare path grants ADMIN FALSE, INHERIT TRUE, SET FALSE"
    );

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("the reconciler accepts a generation its own provisioner minted");
    assert!(
        plan.is_noop(),
        "a prepared generation is not schema drift: {:#?}",
        plan.actions
    );

    drop_generation_role(su, &generation).await;
}

/// Manifestations 1 + 4: the 2jkm.41-sweep drift set plus the outbox era.
async fn v1_era_drifted_leg(su: &Client, system_su: &Client, system_url: &str, target_url: &str) {
    reset(su).await;
    seed_system_env_policy(system_su, "durable").await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");

    // Current-era runs/flows (the drift was queue-side)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
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
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "durable").await;
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, package_id, registration_id, entity_id, registration) \
         VALUES ('t1', 'cat', 'r1', 'e', \
                 $1::text::jsonb)",
        &[&r#"{"registration-id":"r1","package-id":"cat","source-package-id":"cat","partition-key":"serial","retained":"yes","state":"shadow"}"#],
    )
    .await
    .expect("seed a retired-key registration");
    // A pre-existing queue row: the ADD COLUMN defaults must land on it.
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
              environment) \
             VALUES ('t1','r-old','f',1,'cat',1,'dev'); \
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

    su.batch_execute(&format!(
        "DELETE FROM {SCHEMA}.environment_policies WHERE tenant_id='t1'"
    ))
    .await
    .expect("remove the temporary pre-projection policy");
    let temporary_policy_rows: i64 = su
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.environment_policies WHERE tenant_id='t1'"),
            &[],
        )
        .await
        .expect("verify the temporary pre-projection policy is absent")
        .get(0);
    assert_eq!(
        temporary_policy_rows, 0,
        "real CLI projection starts without a local policy row"
    );

    // The REAL CLI path (arg validation + connect + apply + print).
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        tenant: "t1".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane applies");

    let projected: String = su
        .query_one(
            &format!(
                "SELECT durability_class FROM {SCHEMA}.environment_policies \
                 WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await
        .expect("read projected durable policy")
        .get(0);
    assert_eq!(projected, "durable");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment,durability_class) \
         VALUES ('t1','r-policy-durable','f',1,'cat',1,'dev','durable')"
    ))
    .await
    .expect("seed an explicitly durable run");

    system_su
        .batch_execute(
            "UPDATE registry.env_policies SET durability_class='standard' \
          WHERE org='acme' AND name='dev'",
        )
        .await
        .expect("change the system env policy");
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        tenant: "t1".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: true,
    })
    .await
    .expect("dry-run observes changed environment policy");
    let after_dry_run: String = su
        .query_one(
            &format!(
                "SELECT durability_class FROM {SCHEMA}.environment_policies \
                 WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await
        .expect("read policy after dry-run")
        .get(0);
    assert_eq!(after_dry_run, "durable", "dry-run mutated local policy");
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        tenant: "t1".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile changed environment policy");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment,durability_class) \
         VALUES ('t1','r-policy-standard','f',1,'cat',1,'dev','standard')"
    ))
    .await
    .expect("seed an explicitly standard run");
    let classes: Vec<(String, String)> = su
        .query(
            &format!(
                "SELECT run_id,durability_class FROM {SCHEMA}.runs \
                 WHERE run_id IN ('r-policy-durable','r-policy-standard') ORDER BY run_id"
            ),
            &[],
        )
        .await
        .expect("read frozen run classes")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        classes,
        [
            ("r-policy-durable".to_string(), "durable".to_string()),
            ("r-policy-standard".to_string(), "standard".to_string()),
        ],
        "policy changes must not rewrite stored run classes"
    );

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
              WHERE tenant_id = 't1' AND package_id = 'cat' AND registration_id = 'r1'",
            &[],
        )
        .await
        .expect("read registration")
        .get(0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&registration).expect("parse registration"),
        serde_json::json!({
            "registration-id": "r1",
            "package-id": "cat",
            "source-package-id": "cat",
            "retained": "yes"
        })
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

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile applies");
    assert!(!plan.is_noop());

    assert!(table_exists(su, SCHEMA, "run_queue").await);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
    // The FK to runs resolves: a run then its queue row insert cleanly.
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
              environment) \
             VALUES ('t1','r1','f',1,'cat',1,'dev'); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("FK insert path");

    let claimable = indexdef(su, "run_queue_claimable")
        .await
        .expect("global FIFO claim index");
    assert!(claimable.contains("(tenant_id, available_at, stream_seq, run_id, lease_expires_at)"));
}

/// Manifestations 3 + 5 + 6 (the ephemeral-fixture wipe): a database with no
/// project schemas. The fixed runtime roles are cluster-scoped and shared, so
/// this leg must preserve them even when another database has an object owned
/// by `wamn_app`. Dry-run first (strictly read-only), then apply provisions run
/// plane + `catalog`, and a functional smoke as `wamn_app` shows grants + RLS
/// isolation from the applied sections.
async fn from_zero_leg(su: &Client, base_url: &str) {
    reset(su).await;
    let schema = schema();
    let sentinel_database = "wamn_run_plane_from_zero_role_sentinel";
    recreate_database(su, sentinel_database).await;
    let sentinel_url = database_url(base_url, sentinel_database);
    let sentinel = connect(&sentinel_url).await;
    sentinel
        .batch_execute(
            "CREATE TABLE public.wamn_app_role_sentinel (id int PRIMARY KEY); \
             ALTER TABLE public.wamn_app_role_sentinel OWNER TO wamn_app; \
             CREATE TABLE public.wamn_scenario_author_role_sentinel \
               (id int PRIMARY KEY); \
             ALTER TABLE public.wamn_scenario_author_role_sentinel \
               OWNER TO wamn_scenario_author;",
        )
        .await
        .expect("create cross-database runtime-role ownership sentinel");
    drop(sentinel);

    let role_oids_before = su
        .query_one(
            "SELECT (SELECT oid::bigint FROM pg_roles WHERE rolname = 'wamn_app'), \
                    (SELECT oid::bigint FROM pg_roles \
                      WHERE rolname = 'wamn_scenario_author')",
            &[],
        )
        .await
        .expect("snapshot shared runtime roles");
    let role_oids_before = (
        role_oids_before.get::<_, i64>(0),
        role_oids_before.get::<_, i64>(1),
    );

    // --dry-run is STRICTLY read-only: it changes neither shared roles nor tables.
    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("dry-run plans");
    assert!(!dry.is_noop());
    let role_oids_after = su
        .query_one(
            "SELECT (SELECT oid::bigint FROM pg_roles WHERE rolname = 'wamn_app'), \
                    (SELECT oid::bigint FROM pg_roles \
                      WHERE rolname = 'wamn_scenario_author')",
            &[],
        )
        .await
        .expect("re-read shared runtime roles");
    assert_eq!(
        (
            role_oids_after.get::<_, i64>(0),
            role_oids_after.get::<_, i64>(1),
        ),
        role_oids_before,
        "dry-run preserves the shared cluster roles"
    );
    assert!(
        !table_exists(su, SCHEMA, "runs").await,
        "dry-run creates nothing"
    );

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("from-zero reconcile applies");
    assert!(!plan.is_noop());

    // Exactly the run-plane record: `run-state.sql` + `run-queue.sql`. The three
    // authoring-test relations left this roster with wamn-0h0g.9.11.2
    // (38860fab) — `deploy/sql/control-portable-store.sql` provisions them into
    // the same schema, and this verb never applies that file.
    for t in [
        "runs",
        "environment_policies",
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "operator_run_actions",
        "run_queue",
    ] {
        assert!(
            table_exists(su, SCHEMA, t).await,
            "run-plane table {t} provisioned"
        );
    }
    for t in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(
            !table_exists(su, SCHEMA, t).await,
            "the portable store's relation {t} is not this verb's to provision"
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
    // The project-authoring relations left `catalog-schema.sql` with
    // wamn-0h0g.9.11.3 (805701ec); `deploy/sql/control-portable-store.sql` owns
    // them and `control_portable_store` pins them. This verb applies the
    // catalog record only, so from-zero must NOT produce them.
    for table in ["authoring_command_audit"] {
        assert!(
            !table_exists(su, "catalog", table).await,
            "the portable store's relation catalog.{table} is not this verb's to provision"
        );
    }

    // Functional smoke as the runtime principal: the sections' grants + RLS
    // hold. wamn-0h0g.22.7 (b1d42599) took every run-plane WRITE away from
    // `wamn_app` — table SELECT and DELETE on `runs`, nothing at all on
    // `run_queue` — so both tenants' rows are seeded as superuser and the guest
    // is shown to READ its own under RLS and to be REFUSED on write.
    //
    // *** THE PRINCIPAL IS A MINTED GENERATION LOGIN, NOT `SET ROLE wamn_app`
    // (wamn-0h0g.22.36). *** After wamn-0h0g.22.6 the floor is
    // `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`
    // and `current_tenant_key` recovers a key ONLY from the guest generation
    // pattern. Under the BARE ACL role it derives NULL, a NULL-compared
    // predicate matches nothing, and PostgreSQL returns ZERO ROWS WITH NO
    // ERROR — so the retired probe could not tell REFUSED from MATCHED-NOTHING
    // and read 0 where it demanded 1. The login name is DERIVED by the
    // production builder, never spelled, so the digest under test is the digest
    // `provision-project-env` would mint.
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "standard").await;
    seed_run_admission_facts(su, "t2", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
              environment) \
             VALUES ('t1','r1','f',1,'cat',1,'dev'), \
                    ('t2','r2','f',1,'cat',1,'dev'); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("seed both tenants' run-plane rows");
    let seeded: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
        .await
        .expect("superuser sees the whole table")
        .get(0);
    assert_eq!(seeded, 2, "both tenants' rows exist before the guest reads");

    let (guest_generation, guest) = mint_guest_generation(su, base_url, "t1").await;
    for refused in [
        format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment) \
             VALUES ('t1','r3','f',1,'cat',1,'dev')"
        ),
        format!("INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1','r3')"),
    ] {
        let denied = guest
            .batch_execute(&refused)
            .await
            .expect_err("the runtime role writes no run-plane row");
        assert_db_code(denied, "42501", "runtime-role write refusal");
    }
    // THE POST-STATE, not merely a count: the row the guest reads is ITS OWN,
    // and the foreign tenant's row — shown present above — is absent from the
    // result. One query settles admission and isolation together, so a
    // regression to matched-nothing fails on the left half and a regression to
    // a `USING (true)` floor fails on the right.
    let visible = guest
        .query(
            &format!("SELECT tenant_id, run_id FROM {SCHEMA}.runs ORDER BY run_id"),
            &[],
        )
        .await
        .expect("tenant read");
    let visible: Vec<(String, String)> =
        visible.iter().map(|row| (row.get(0), row.get(1))).collect();
    assert_eq!(
        visible,
        vec![("t1".to_string(), "r1".to_string())],
        "the guest generation sees exactly its own tenant's row"
    );
    // `runs` is the only run-plane relation the guest role can still read;
    // wamn-0h0g.22.7 (b1d42599) left it nothing at all on `run_queue`.
    let queue_denied = guest
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .expect_err("the runtime role cannot read the queue at all");
    assert_db_code(queue_denied, "42501", "runtime-role queue read refusal");
    drop(guest);
    // Roles are CLUSTER-wide: leave none behind for the legs that follow.
    drop_generation_role(su, &guest_generation).await;

    let sentinel = connect(&sentinel_url).await;
    let sentinel_owners = sentinel
        .query_one(
            "SELECT pg_get_userbyid((SELECT relowner FROM pg_class \
                                      WHERE oid = \
                                        'public.wamn_app_role_sentinel'::regclass)), \
                    pg_get_userbyid((SELECT relowner FROM pg_class \
                                      WHERE oid = \
                                        'public.wamn_scenario_author_role_sentinel'::regclass))",
            &[],
        )
        .await
        .expect("read cross-database runtime-role ownership sentinels");
    let app_owner: String = sentinel_owners.get(0);
    let author_owner: String = sentinel_owners.get(1);
    assert_eq!(
        app_owner, "wamn_app",
        "from-zero reconciliation preserves sibling-database runtime ownership"
    );
    assert_eq!(
        author_owner, "wamn_scenario_author",
        "from-zero reconciliation preserves sibling-database author ownership"
    );
    drop(sentinel);
    drop_database(su, sentinel_database).await;
}

async fn visible_environment_policy_rows(app: &Client, tenant: &str) -> i64 {
    app.query_one("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .expect("set app tenant for environment-policy probe");
    app.query_one(
        &format!("SELECT count(*) FROM {SCHEMA}.environment_policies"),
        &[],
    )
    .await
    .expect("read visible environment policies")
    .get(0)
}

async fn registry_durability_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'type', pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
           'not-null', attribute.attnotnull, \
           'default', pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid), \
           'check', (SELECT pg_catalog.pg_get_constraintdef(constraint_row.oid, true) \
                       FROM pg_catalog.pg_constraint AS constraint_row \
                      WHERE constraint_row.conrelid = 'registry.env_policies'::regclass \
                        AND constraint_row.conname = 'env_policies_durability_class_check'))::text \
         FROM pg_catalog.pg_attribute AS attribute \
         LEFT JOIN pg_catalog.pg_attrdef AS default_row \
           ON default_row.adrelid = attribute.attrelid \
          AND default_row.adnum = attribute.attnum \
        WHERE attribute.attrelid = 'registry.env_policies'::regclass \
          AND attribute.attname = 'durability_class'",
        &[],
    )
    .await
    .expect("snapshot registry durability schema")
    .get(0)
}

async fn registry_env_policy_catalog_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       attribute.attname, \
                       pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
                       attribute.attnotnull, \
                       pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid)) \
                       ORDER BY attribute.attnum) \
               FROM pg_catalog.pg_attribute AS attribute \
               LEFT JOIN pg_catalog.pg_attrdef AS default_row \
                 ON default_row.adrelid = attribute.attrelid \
                AND default_row.adnum = attribute.attnum \
              WHERE attribute.attrelid = 'registry.env_policies'::regclass \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped), \
             '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       constraint_row.conname, constraint_row.contype, \
                       pg_catalog.pg_get_constraintdef(constraint_row.oid, true)) \
                       ORDER BY constraint_row.conname) \
               FROM pg_catalog.pg_constraint AS constraint_row \
              WHERE constraint_row.conrelid = 'registry.env_policies'::regclass), \
             '[]'::jsonb))::text",
        &[],
    )
    .await
    .expect("snapshot registry env-policy catalog")
    .get(0)
}

/// Missing-column mutant for the shared system-registry schema ensure. The
/// real reconcile consumer must upgrade before it reads, and a second run must
/// leave the exact column + CHECK catalog unchanged.
async fn registry_durability_schema_ensure_leg(
    target_su: &Client,
    system_su: &Client,
    system_url: &str,
    target_url: &str,
) {
    reset(target_su).await;
    install_current_run_plane(target_su).await;
    seed_pre_durability_system_env_policy(system_su).await;
    let before: bool = system_su
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
              WHERE table_schema='registry' AND table_name='env_policies' \
                AND column_name='durability_class')",
            &[],
        )
        .await
        .expect("probe missing durability column")
        .get(0);
    assert!(!before, "missing-column mutant was not installed");

    let args = |dry_run| ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        tenant: "t1".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run,
    };
    let before_dry_run = registry_env_policy_catalog_snapshot(system_su).await;
    reconcile_run_plane::run(args(true))
        .await
        .expect("dry-run observes a pre-carrier registry without migrating it");
    assert_eq!(
        registry_env_policy_catalog_snapshot(system_su).await,
        before_dry_run,
        "pre-carrier registry schema must remain byte-exact under dry-run"
    );
    assert!(
        !system_su
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
              WHERE table_schema='registry' AND table_name='env_policies' \
                AND column_name='durability_class')",
                &[],
            )
            .await
            .expect("probe carrier after dry-run")
            .get::<_, bool>(0),
        "dry-run must not add the registry durability carrier"
    );
    let projected_rows: i64 = target_su
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.environment_policies"),
            &[],
        )
        .await
        .expect("count local policy rows after dry-run")
        .get(0);
    assert_eq!(
        projected_rows, 0,
        "dry-run must not project the legacy standard policy into the run plane"
    );

    reconcile_run_plane::run(args(false))
        .await
        .expect("reconcile upgrades the system env-policy schema");
    let first = registry_durability_schema_snapshot(system_su).await;
    assert!(first.contains("\"type\": \"text\""), "{first}");
    assert!(first.contains("\"not-null\": true"), "{first}");
    assert!(first.contains("'standard'::text"), "{first}");
    assert!(first.contains("'durable'::text"), "{first}");

    let projected_row = target_su
        .query_one(
            &format!(
                "SELECT expected_environment, durability_class \
                   FROM {SCHEMA}.environment_policies WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await
        .expect("read policy projected from the upgraded registry");
    let projected = (projected_row.get(0), projected_row.get(1));
    assert_eq!(projected, ("dev".to_string(), "standard".to_string()));

    reconcile_run_plane::run(args(true))
        .await
        .expect("second registry schema ensure is idempotent");
    assert_eq!(registry_durability_schema_snapshot(system_su).await, first);
}

/// Named mutants for the four independent ways an existing env-policy table
/// can lose tenant confinement. Each repair is followed by a fresh observation
/// showing the catalog converged, not merely that the SQL happened to run.
async fn environment_policy_row_security_leg(su: &Client, url: &str) {
    reset(su).await;
    install_current_run_plane(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.environment_policies \
           (tenant_id, expected_environment, durability_class) \
         VALUES ('t1', 'dev', 'durable')"
    ))
    .await
    .expect("seed projected environment policy");
    let (guest_generation, guest) = mint_guest_generation(su, url, "t2").await;

    let mutants = [
        (
            "disabled RLS",
            format!("ALTER TABLE {SCHEMA}.environment_policies DISABLE ROW LEVEL SECURITY"),
        ),
        (
            "unforced RLS",
            format!("ALTER TABLE {SCHEMA}.environment_policies NO FORCE ROW LEVEL SECURITY"),
        ),
        (
            "missing tenant policy",
            format!(
                "DROP POLICY environment_policies_tenant \
                   ON {SCHEMA}.environment_policies"
            ),
        ),
        (
            "widened tenant policy",
            format!(
                "DROP POLICY environment_policies_tenant \
                   ON {SCHEMA}.environment_policies; \
                 CREATE POLICY environment_policies_tenant \
                   ON {SCHEMA}.environment_policies AS PERMISSIVE \
                   FOR ALL TO wamn_app USING (true) WITH CHECK (true); \
                 CREATE POLICY environment_policies_extra \
                   ON {SCHEMA}.environment_policies FOR SELECT USING (true)"
            ),
        ),
    ];

    for (mutant, mutation) in mutants {
        su.batch_execute(&mutation)
            .await
            .unwrap_or_else(|error| panic!("install {mutant} mutant: {error}"));
        if mutant == "disabled RLS" {
            assert_eq!(
                visible_environment_policy_rows(&guest, "t2").await,
                1,
                "the disabled-RLS mutant must expose the foreign tenant row"
            );
        }

        let dry = reconcile_run_plane::reconcile(su, &schema, false)
            .await
            .unwrap_or_else(|error| panic!("observe {mutant} mutant: {error}"));
        assert_eq!(dry.actions.len(), 1, "{mutant}: {:#?}", dry.actions);
        assert_eq!(dry.actions[0].kind, RunPlaneActionKind::RepairRowSecurity);
        assert_eq!(dry.actions[0].target, "environment_policies.row-security");

        let applied = reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .unwrap_or_else(|error| panic!("repair {mutant} mutant: {error}"));
        assert_eq!(applied.actions, dry.actions, "{mutant} plan changed");
        if mutant == "disabled RLS" {
            assert_eq!(
                visible_environment_policy_rows(&guest, "t2").await,
                0,
                "the repaired policy must hide the foreign tenant row"
            );
        }

        let again = reconcile_run_plane::reconcile(su, &schema, false)
            .await
            .unwrap_or_else(|error| panic!("re-observe repaired {mutant} mutant: {error}"));
        assert!(
            again.is_noop(),
            "{mutant} repair did not converge: {:#?}",
            again.actions
        );
    }
    drop(guest);
    drop_generation_role(su, &guest_generation).await;
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
    // The record is the six `run-state.sql` tables plus `run_queue`, pinned by
    // `record_tables_are_pinned`. The authoring-test orchestration relations that
    // used to make this twelve left the record with wamn-0h0g.9.11.3 (805701ec);
    // this assertion had been unreachable behind an earlier leg's abort ever since.
    assert_eq!(
        dry.at_target.len(),
        7,
        "all seven run-plane record tables at target: {:?}",
        dry.at_target
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

/// PLAN 6A additive storage and the host/guest authority boundary. This shows
/// both ctl provisioning paths add their retained sections, then exercises
/// adversarial direct, inherited, membership, and ownership authority drift.
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

/// Historical pre-cutover disposition hardening test, retained only as source
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
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
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

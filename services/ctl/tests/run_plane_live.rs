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
//!   remain shared. `--dry-run` first, proven STRICTLY read-only; then the apply
//!   provisions everything — run plane + `catalog` schema — and a functional
//!   smoke as `wamn_app` proves the sections' grants + RLS isolation end-to-end.
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

use tokio_postgres::{Client, NoTls};

use wamn_control_provision::{
    DISPATCH_READER_ROLE, project_env_database_name, sql as provision_sql,
};
use wamn_ctl::reconcile_run_plane::{
    self, RECONCILE_TARGET_REFUSAL_PREFIX, ReconcileRunPlaneArgs, ReconcileTargetError,
    ReconcileTargetErrorKind,
};
use wamn_schema_control::{BareSchemaName, RunPlaneActionKind, rewrite_schema};

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

const SCHEMA: &str = "rp_live";
const DISPATCH_READER_PASSWORD: &str = "dispatch-reader-run-plane-probe";
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

async fn seed_target_guard_registry(su: &Client) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         CREATE SCHEMA registry AUTHORIZATION wamn_system; \
         SET ROLE wamn_system; \
         CREATE TABLE registry.project_envs ( \
           org text NOT NULL, project text NOT NULL, env text NOT NULL, \
           secret_name text NOT NULL, secret_namespace text, \
           instance_suffix text NOT NULL, PRIMARY KEY (org, project, env)); \
         INSERT INTO registry.project_envs \
           (org,project,env,secret_name,instance_suffix) VALUES \
           ('acme','billing','dev','wamn-db-acme--billing--dev','k3m9x2p7'), \
           ('acme','ledger','dev','wamn-db-acme--ledger--dev','q80zdw41'), \
           ('acme','billing','prod','wamn-db-acme--billing--prod','p7c4n2v8'); \
         RESET ROLE; \
         CREATE TABLE registry.env_policies ( \
           org text NOT NULL, name text NOT NULL, recovery_domain jsonb NOT NULL, \
           promotion_rank int NOT NULL, instances int NOT NULL, storage text NOT NULL, \
           cpu text NOT NULL, memory text NOT NULL, image text NOT NULL, \
           backup_cadence text NOT NULL, wal_retention text NOT NULL, \
           hibernation text NOT NULL, PRIMARY KEY (org, name)); \
         INSERT INTO registry.env_policies \
           (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image, \
            backup_cadence,wal_retention,hibernation) VALUES \
           ('acme','dev','\"own\"',0,1,'1Gi','100m','128Mi','postgres','','','off'), \
           ('acme','prod','\"own\"',1,1,'1Gi','100m','128Mi','postgres','','','off')",
    )
    .await
    .expect("seed target-identity registry fixture");
}

async fn target_guard_system_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'policy-columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       attribute.attname, \
                       pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
                       attribute.attnotnull, \
                       pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid)) \
                      ORDER BY attribute.attnum) \
               FROM pg_catalog.pg_attribute AS attribute \
               LEFT JOIN pg_catalog.pg_attrdef AS default_row \
                 ON default_row.adrelid=attribute.attrelid \
                AND default_row.adnum=attribute.attnum \
              WHERE attribute.attrelid='registry.env_policies'::regclass \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped), '[]'::jsonb), \
           'policy-owner', ( \
             SELECT pg_catalog.pg_get_userbyid(relation.relowner) \
               FROM pg_catalog.pg_class AS relation \
              WHERE relation.oid='registry.env_policies'::regclass), \
           'policies', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(org,name,recovery_domain,promotion_rank) \
                              ORDER BY org,name) \
               FROM registry.env_policies), '[]'::jsonb), \
           'project-envs', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       org,project,env,secret_name,secret_namespace,instance_suffix) \
                      ORDER BY org,project,env) \
               FROM registry.project_envs), '[]'::jsonb), \
           'roles', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array( \
                       rolname,rolsuper,rolcreatedb,rolcreaterole,rolcanlogin,rolbypassrls) \
                      ORDER BY rolname) \
               FROM pg_catalog.pg_roles WHERE rolname LIKE 'wamn_%'), '[]'::jsonb), \
           'memberships', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(member.rolname,granted.rolname) \
                              ORDER BY member.rolname,granted.rolname) \
               FROM pg_catalog.pg_auth_members AS membership \
               JOIN pg_catalog.pg_roles AS member ON member.oid=membership.member \
               JOIN pg_catalog.pg_roles AS granted ON granted.oid=membership.roleid \
              WHERE member.rolname LIKE 'wamn_%' OR granted.rolname LIKE 'wamn_%'), \
             '[]'::jsonb))::text",
        &[],
    )
    .await
    .expect("snapshot registry target and cluster roles")
    .get(0)
}

async fn target_guard_database_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'database-acl', (SELECT datacl::text FROM pg_catalog.pg_database \
                             WHERE datname=current_database()), \
           'schemas', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(namespace.nspname, \
                                                pg_catalog.pg_get_userbyid(namespace.nspowner)) \
                              ORDER BY namespace.nspname) \
               FROM pg_catalog.pg_namespace AS namespace \
              WHERE namespace.nspname <> 'information_schema' \
                AND namespace.nspname NOT LIKE 'pg_%'), '[]'::jsonb), \
           'relations', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(namespace.nspname,relation.relname, \
                                                relation.relkind,relation.relrowsecurity, \
                                                relation.relforcerowsecurity) \
                              ORDER BY namespace.nspname,relation.relname) \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid=relation.relnamespace \
              WHERE namespace.nspname <> 'information_schema' \
                AND namespace.nspname NOT LIKE 'pg_%'), '[]'::jsonb), \
           'policies', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(schemaname,tablename,policyname,roles,cmd,qual,with_check) \
                              ORDER BY schemaname,tablename,policyname) \
               FROM pg_catalog.pg_policies \
              WHERE schemaname <> 'information_schema' \
                AND schemaname NOT LIKE 'pg_%'), '[]'::jsonb))::text",
        &[],
    )
    .await
    .expect("snapshot project database target")
    .get(0)
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

fn target_guard_args(
    system_url: &str,
    target_url: &str,
    project: &str,
    environment: &str,
    dry_run: bool,
) -> ReconcileRunPlaneArgs {
    ReconcileRunPlaneArgs {
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: CLI_ORG.to_string(),
        project: project.to_string(),
        tenant: "t1".to_string(),
        env: environment.to_string(),
        schema: SCHEMA.to_string(),
        dry_run,
    }
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
    catalog_id: &str,
    catalog_version: i32,
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
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ($1,$2,$3,$4,'0.1','applied')",
        &[&tenant_id, &catalog_id, &catalog_version, &environment],
    )
    .await
    .expect("seed run-pin catalog");
    su.execute(
        "INSERT INTO catalog.releases \
           (tenant_id,catalog_id,catalog_version) \
         VALUES ($1,$2,$3)",
        &[&tenant_id, &catalog_id, &catalog_version],
    )
    .await
    .expect("seed run-pin release manifest");
}

/// Hermetic reset: drop the target schema + the shared `catalog` schema and
/// ensure the `wamn_app` role, so every leg builds its own starting state.
/// Hermetic per CLUSTER, not merely per schema (wamn-0h0g.12.123). PostgreSQL
/// roles are cluster-wide, and the reconciler now converges
/// `wamn_dispatch_reader`'s in-database surface WHEN THAT ROLE EXISTS — so a
/// reader left behind by another gate against the same container would make
/// `current_noop_leg`'s first plan legitimately non-empty. `DROP OWNED BY` is
/// what makes the role droppable: `DROP ROLE` refuses while any acl entry
/// anywhere still names it. `wamn_app` and the two writer roles are only
/// created-or-hardened because other legs dial them.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
         DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
         DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $reader$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}') THEN \
             EXECUTE 'DROP OWNED BY {DISPATCH_READER_ROLE}'; \
             EXECUTE 'DROP ROLE {DISPATCH_READER_ROLE}'; \
           END IF; \
         END $reader$; \
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
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
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

/// The retired flow registry (`deploy/sql/flows.sql`), deleted by
/// wamn-0h0g.12.102 (e45ca35b). It is no longer one of the run-plane files
/// `install_current_run_plane` applies — but `partition_plane_cutover_sql`
/// still locks and preflights it for schemas that physically retain the table,
/// so a leg proving that refusal must install it. Mirrors the unit-side
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

async fn install_legacy_failure_detail(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN fail_node text, \
           ADD COLUMN fail_reason text;"
    ))
    .await
    .expect("install retired failure-detail columns");
}

async fn seed_failure_detail_run(su: &Client, run_id: &str) {
    seed_run_admission_facts(su, "failure-detail", "cat", 1, "dev", "standard").await;
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,status,fail_kind,terminal_reason, \
                caller_outcome_kind,caller_outcome_json,caller_http_status, \
                caller_released_at,fail_node,fail_reason) \
             VALUES ('failure-detail',$1,'f',1,'cat',1,'dev','failed','terminal', \
                     'typed-caller-failure','failed', \
                     '{{\"class\":\"terminal\",\"detail\":\"typed-caller\"}}'::jsonb,500, \
                     now(),'deleted-plan-node','obsolete coordinate detail')"
        ),
        &[&run_id],
    )
    .await
    .expect("seed populated retired failure detail");
}

async fn assert_retained_failure_record(su: &Client, run_id: &str) {
    let row = su
        .query_one(
            &format!(
                "SELECT fail_kind,terminal_reason,caller_outcome_kind, \
                        caller_outcome_json::text,caller_http_status,status \
                   FROM {SCHEMA}.runs \
                  WHERE tenant_id='failure-detail' AND run_id=$1"
            ),
            &[&run_id],
        )
        .await
        .expect("read retained failure record");
    assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("terminal"));
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("typed-caller-failure")
    );
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("failed"));
    assert_eq!(
        row.get::<_, Option<String>>(3).as_deref(),
        Some("{\"class\": \"terminal\", \"detail\": \"typed-caller\"}")
    );
    assert_eq!(row.get::<_, Option<i32>>(4), Some(500));
    assert_eq!(row.get::<_, String>(5), "failed");
}

async fn failure_detail_snapshot(su: &Client) -> String {
    su.query_one(
        &format!(
            "SELECT jsonb_build_object( \
               'columns', COALESCE(( \
                 SELECT jsonb_agg(jsonb_build_array(attribute.attname, \
                                                    attribute.attnotnull, \
                                                    pg_catalog.format_type( \
                                                      attribute.atttypid,attribute.atttypmod)) \
                                  ORDER BY attribute.attnum) \
                   FROM pg_catalog.pg_attribute AS attribute \
                  WHERE attribute.attrelid='{SCHEMA}.runs'::regclass \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped), '[]'::jsonb), \
               'rows', COALESCE(( \
                 SELECT jsonb_agg(to_jsonb(run_row) ORDER BY tenant_id,run_id) \
                   FROM {SCHEMA}.runs AS run_row), '[]'::jsonb), \
               'view', pg_catalog.pg_get_viewdef( \
                 pg_catalog.to_regclass('{SCHEMA}.retired_failure_detail_dependency'),true))::text"
        ),
        &[],
    )
    .await
    .expect("snapshot failure-detail dependency and rows")
    .get(0)
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
    authoring_storage_authority_leg(&su, &url).await;
    retired_effect_disposition_cutover_leg(&su).await;
    persisted_literal_check_drift_leg(&su).await;
    dispatch_reader_read_surface_leg(&su, &url).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn frame_identity_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    frame_identity_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn effect_writer_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    effect_writer_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn partition_plane_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    partition_plane_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn partition_plane_active_lease_refusal_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    partition_plane_active_lease_refusal_leg(&su).await;
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

    // Provisioning's half (wamn-0h0g.12.122), from the SAME builders
    // `provision-project-env` emits: the role, and CONNECT on this database.
    // Everything after this point must come from the reconciler alone — that is
    // what "no manual SQL" means.
    su.batch_execute(&provision_sql::ensure_dispatch_reader_role_sql(
        DISPATCH_READER_PASSWORD,
    ))
    .await
    .expect("mint the dispatch reader");
    su.batch_execute(&provision_sql::grant_dispatch_reader_connect_sql(&database))
        .await
        .expect("grant the dispatch reader CONNECT");

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
    let reader = connect_as(url, DISPATCH_READER_ROLE, DISPATCH_READER_PASSWORD).await;
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

    let reader = connect_as(url, DISPATCH_READER_ROLE, DISPATCH_READER_PASSWORD).await;
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

#[tokio::test]
async fn stored_suite_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the stored-suite cutover gate");
        return;
    };
    let su = connect(&url).await;
    stored_suite_cutover_leg(&su).await;
}

#[tokio::test]
async fn authoring_storage_authority_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the authoring authority gate");
        return;
    };
    let su = connect(&url).await;
    authoring_storage_authority_leg(&su, &url).await;
}

#[tokio::test]
async fn child_run_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the child-run cutover gate");
        return;
    };
    let su = connect(&url).await;
    child_run_cutover_leg(&su).await;
}

#[tokio::test]
async fn rerun_lineage_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the rerun-lineage cutover gate");
        return;
    };
    let su = connect(&url).await;
    rerun_lineage_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn failure_detail_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    failure_detail_cutover_leg(&su).await;
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

/// The public CLI shell refuses every registry/database identity disagreement
/// before either the system-policy carrier or either project database changes.
#[tokio::test]
async fn reconcile_target_identity_guard_live() {
    let Some(system_url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the run-plane target-identity gate");
        return;
    };
    let system_su = connect(&system_url).await;
    let primary_database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    let sibling_project_database =
        project_env_database_name(CLI_ORG, "ledger", CLI_ENV, "q80zdw41");
    let sibling_environment_database =
        project_env_database_name(CLI_ORG, CLI_PROJECT, "prod", "p7c4n2v8");
    let unrelated_database = "wamn-run-plane-unrelated";
    for database in [primary_database.as_str(), unrelated_database] {
        recreate_database(&system_su, database).await;
    }
    seed_target_guard_registry(&system_su).await;

    let primary_url = database_url(&system_url, &primary_database);
    let unrelated_url = database_url(&system_url, unrelated_database);
    let primary_su = connect(&primary_url).await;
    let unrelated_su = connect(&unrelated_url).await;
    unrelated_su
        .batch_execute(&format!(
            "CREATE SCHEMA target_spoof; \
             CREATE FUNCTION target_spoof.current_database() RETURNS name \
               LANGUAGE sql IMMUTABLE AS $$ SELECT '{primary_database}'::name $$"
        ))
        .await
        .expect("install a search-path identity-spoof function");
    let mut spoofed_unrelated_url =
        url::Url::parse(&unrelated_url).expect("parse unrelated database URL");
    spoofed_unrelated_url
        .query_pairs_mut()
        .append_pair("options", "-csearch_path=target_spoof,pg_catalog");
    let spoofed_unrelated_url = spoofed_unrelated_url.to_string();
    let system_before = target_guard_system_snapshot(&system_su).await;
    let primary_before = target_guard_database_snapshot(&primary_su).await;
    let unrelated_before = target_guard_database_snapshot(&unrelated_su).await;

    let cases = [
        (
            "valid triple with unrelated URL",
            CLI_PROJECT,
            CLI_ENV,
            unrelated_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(primary_database.as_str()),
            Some(unrelated_database),
        ),
        (
            "search-path spoofed unrelated URL",
            CLI_PROJECT,
            CLI_ENV,
            spoofed_unrelated_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(primary_database.as_str()),
            Some(unrelated_database),
        ),
        (
            "registered sibling project with primary URL",
            "ledger",
            CLI_ENV,
            primary_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(sibling_project_database.as_str()),
            Some(primary_database.as_str()),
        ),
        (
            "registered sibling environment with primary URL",
            CLI_PROJECT,
            "prod",
            primary_url.as_str(),
            ReconcileTargetErrorKind::DatabaseTarget,
            Some(sibling_environment_database.as_str()),
            Some(primary_database.as_str()),
        ),
        (
            "unrecorded triple",
            "absent",
            CLI_ENV,
            primary_url.as_str(),
            ReconcileTargetErrorKind::RegistryTarget,
            None,
            None,
        ),
    ];

    for dry_run in [true, false] {
        for &(label, project, environment, target_url, kind, expected, actual) in &cases {
            let error = reconcile_run_plane::run(target_guard_args(
                &system_url,
                target_url,
                project,
                environment,
                dry_run,
            ))
            .await
            .unwrap_err();
            let refusal = error
                .downcast_ref::<ReconcileTargetError>()
                .unwrap_or_else(|| {
                    panic!("{label} returned an untyped refusal with dry_run={dry_run}: {error}")
                });
            assert_eq!(refusal.kind(), kind, "{label}, dry_run={dry_run}");
            assert_eq!(
                refusal.is_registry_target(),
                kind == ReconcileTargetErrorKind::RegistryTarget,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.is_database_target(),
                kind == ReconcileTargetErrorKind::DatabaseTarget,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.expected_database(),
                expected,
                "{label}, dry_run={dry_run}"
            );
            assert_eq!(
                refusal.actual_database(),
                actual,
                "{label}, dry_run={dry_run}"
            );
            let message = refusal.to_string();
            assert!(
                message.starts_with(RECONCILE_TARGET_REFUSAL_PREFIX),
                "{label} lost the stable target-refusal prefix: {message}"
            );
            assert!(
                !message.contains(&*system_url) && !message.contains(target_url),
                "{label} leaked a database URL: {message}"
            );
            if kind == ReconcileTargetErrorKind::RegistryTarget {
                assert!(
                    std::error::Error::source(refusal).is_some(),
                    "{label} discarded the registry lookup source"
                );
            }
            assert_eq!(
                target_guard_system_snapshot(&system_su).await,
                system_before,
                "{label} mutated the pre-carrier registry or cluster roles with dry_run={dry_run}"
            );
            assert_eq!(
                target_guard_database_snapshot(&primary_su).await,
                primary_before,
                "{label} mutated the primary project database with dry_run={dry_run}"
            );
            assert_eq!(
                target_guard_database_snapshot(&unrelated_su).await,
                unrelated_before,
                "{label} mutated the unrelated database with dry_run={dry_run}"
            );
        }
    }

    assert!(
        primary_su
            .query_one("SELECT to_regnamespace($1) IS NULL", &[&SCHEMA])
            .await
            .expect("probe refused primary schema")
            .get::<_, bool>(0),
        "wrong-target attempts created the run-plane schema"
    );
    system_su
        .batch_execute("ALTER TABLE registry.env_policies OWNER TO wamn_system")
        .await
        .expect("make the policy fixture readable for the correct-target path");
    reset(&primary_su).await;

    let system_before_dry_run = target_guard_system_snapshot(&system_su).await;
    let primary_before_dry_run = target_guard_database_snapshot(&primary_su).await;
    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        true,
    ))
    .await
    .expect("correct target dry-run plans without writing");
    assert_eq!(
        target_guard_system_snapshot(&system_su).await,
        system_before_dry_run,
        "correct-target dry-run changed the system plane"
    );
    assert_eq!(
        target_guard_database_snapshot(&primary_su).await,
        primary_before_dry_run,
        "correct-target dry-run changed the project plane"
    );

    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        false,
    ))
    .await
    .expect("correct target apply converges the run plane");
    assert!(
        primary_su
            .query_one(
                "SELECT to_regclass($1) IS NOT NULL",
                &[&format!("{SCHEMA}.runs")],
            )
            .await
            .expect("probe converged run plane")
            .get::<_, bool>(0),
        "correct target did not converge the run plane"
    );
    let projected_row = primary_su
        .query_one(
            &format!(
                "SELECT expected_environment,durability_class \
                   FROM {SCHEMA}.environment_policies WHERE tenant_id='t1'"
            ),
            &[],
        )
        .await
        .expect("read converged project-local policy");
    let projected = (projected_row.get(0), projected_row.get(1));
    assert_eq!(projected, (CLI_ENV.to_string(), "standard".to_string()));
    let carrier_present: bool = system_su
        .query_one(
            "SELECT EXISTS (SELECT FROM information_schema.columns \
              WHERE table_schema='registry' AND table_name='env_policies' \
                AND column_name='durability_class')",
            &[],
        )
        .await
        .expect("probe converged system durability carrier")
        .get(0);
    assert!(
        carrier_present,
        "correct apply did not converge the policy carrier"
    );

    let system_converged = target_guard_system_snapshot(&system_su).await;
    let primary_converged = target_guard_database_snapshot(&primary_su).await;
    reconcile_run_plane::run(target_guard_args(
        &system_url,
        &primary_url,
        CLI_PROJECT,
        CLI_ENV,
        false,
    ))
    .await
    .expect("correct target replay remains converged");
    assert_eq!(
        target_guard_system_snapshot(&system_su).await,
        system_converged
    );
    assert_eq!(
        target_guard_database_snapshot(&primary_su).await,
        primary_converged
    );
    assert_eq!(
        target_guard_database_snapshot(&unrelated_su).await,
        unrelated_before,
        "correct-target convergence touched the unrelated database"
    );

    drop(primary_su);
    drop(unrelated_su);
    for database in [primary_database.as_str(), unrelated_database] {
        drop_database(&system_su, database).await;
    }
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

/// Persisted authored ordering bytes have no lossless global-FIFO backfill.
/// Refuse under a flow-table lock before DDL, preserve the bytes, and converge
/// once only default-omitted current graphs remain.
async fn partition_plane_authored_ordering_refusal_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_flow_registry(su).await;
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

async fn retired_shape_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname = 'effect_attempts'), '[]'::jsonb), \
           'indexes', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(indexname,indexdef) ORDER BY indexname) \
               FROM pg_indexes WHERE schemaname=$1 \
                AND tablename = 'effect_attempts'), '[]'::jsonb), \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,a.attname,a.attnotnull, \
                                                pg_get_expr(d.adbin,d.adrelid)) \
                              ORDER BY c.relname,a.attnum) \
               FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
               LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname = 'effect_attempts' \
                AND a.attnum > 0 AND NOT a.attisdropped), '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("read retired schema snapshot")
    .get(0)
}

async fn create_old_frame_identity_tables(su: &Client, effect: bool, populated: bool) {
    reset(su).await;
    su.batch_execute(&format!("CREATE SCHEMA {SCHEMA};"))
        .await
        .expect("create frame-cutover schema");
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
    {
        let label = "effect-only";
        create_old_frame_identity_tables(su, true, true).await;
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("seed role-membership mutation sentinel");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated legacy identity must refuse before DDL");
        assert!(
            format!("{error:#}").contains("effect-writer-cutover-requires-empty-ledger"),
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

    // Late frame-identity drift on a CURRENT, populated schema. Since
    // wamn-0h0g.26.3.1 (204220e8) `effect_attempts` is the only frame-identity
    // target, so each case drifts it: the relation CHECK, the occurrence
    // identity, and a resurrected legacy `node_id` carrier.
    for (label, drift_sql) in [
        (
            "drifted-frame-check",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
                   DROP CONSTRAINT effect_attempts_frame_relation_check, \
                   ADD CONSTRAINT effect_attempts_frame_relation_check CHECK (frame_id >= 0);"
            ),
        ),
        (
            "drifted-occurrence-key",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
                   DROP CONSTRAINT effect_attempts_occurrence_key, \
                   ADD CONSTRAINT effect_attempts_occurrence_key \
                     UNIQUE (tenant_id,run_id,local_node_id,occurrence);"
            ),
        ),
        (
            "retained-legacy-node-id",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
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
        seed_run_admission_facts(su, "t1", "frame-cat", 1, "dev", "standard").await;
        su.batch_execute(&format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                status) \
             VALUES ('t1',$${label}$$,'f',1,'frame-cat',1,'dev','running'); \
             INSERT INTO {SCHEMA}.effect_attempts \
               (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
             VALUES ('t1','00000000-0000-0000-0000-000000000413',$${label}$$, \
                     $${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                     'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0, \
                     'not-required','2099-01-02 UTC','sha256:input'); \
             {drift_sql}"
        ))
        .await
        .expect("seed populated frame drift");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated late frame identity drift must refuse before DDL");
        assert!(
            format!("{error:#}").contains("requires-empty"),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            retired_shape_schema_snapshot(su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
    }

    create_old_frame_identity_tables(su, true, false).await;
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
              WHERE table_schema=$1 AND table_name='effect_attempts' \
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
    seed_run_admission_facts(su, "t1", "frame-cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            status) \
         VALUES ('t1','framed-current','f',1,'frame-cat',1,'dev','running');"
    ))
    .await
    .expect("seed a current populated run");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("an absent effect peer is recreated without a frame refusal");
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
                    ('effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN \
                    ('effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("snapshot effect-writer schema")
    .get(0)
}

async fn install_empty_incompatible_effect_writer_shape(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
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
    seed_run_admission_facts(su, "t1", "writer-cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            status) \
         VALUES ('t1','writer-projection','f',1,'writer-cat',1,'dev', \
                 'running');"
    ))
    .await
    .expect("seed the run the writer cutover reconciles around");
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
        3,
        "the three incompatible ledgers are locked"
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
         GRANT SELECT (lease_generation) ON {SCHEMA}.run_queue TO wamn_effect_writer;"
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
    // wamn-0h0g.26.3.1 (204220e8) retired the node-runs projection, and with it
    // the retired projection's ACL target and every rogue-projection-authority
    // path this leg used to close. `wamn_projection_rogue_member` survives only
    // as the transitive-membership witness the generation contract below needs.
    su.batch_execute(
        "DO $roles$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_projection_rogue_member') THEN \
             CREATE ROLE wamn_projection_rogue_member NOLOGIN INHERIT; \
           END IF; \
         END $roles$;",
    )
    .await
    .expect("install the transitive-membership witness");

    let generation = "wamn_effect_writer_0000000000000000000000000000000000000000_a";
    su.batch_execute(&format!(
        "DO $generation$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{generation}') THEN \
             CREATE ROLE {generation} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $generation$; \
         GRANT wamn_effect_writer TO {generation}; \
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
            &format!("SELECT pg_has_role('wamn_projection_rogue_member','{generation}','MEMBER')"),
            &[],
        )
        .await
        .expect("read retained refused membership")
        .get::<_, bool>(0),
        "refusal is atomic and does not silently rewrite role membership"
    );
    su.batch_execute(&format!(
        "REVOKE {generation} FROM wamn_projection_rogue_member; \
         REVOKE wamn_effect_writer FROM {generation};"
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
        .expect("the writer ACL converges after authority removal");
    assert!(!clean.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
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

/// wamn-0h0g.26.3.1 (204220e8) retired the node-runs projection. A schema
/// provisioned before it still carries the relation, so the reconciler plans
/// ONE `RetireNodeRuns` action, executes it before role bootstrap, and returns
/// — every other repair waits for the next pass. Without this leg the arm has
/// no live watcher.
async fn node_runs_retirement_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for node-runs retirement");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current run-state for node-runs retirement");
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.node_runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
           occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, status text NOT NULL, \
           PRIMARY KEY (tenant_id,run_id,node_id,occurrence)); \
         INSERT INTO {SCHEMA}.node_runs(tenant_id,run_id,node_id,occurrence,seq,status) \
           VALUES ('t1','r1','n1',0,0,'success'); \
         GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_run_projection_writer; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {SCHEMA}.node_runs \
           TO wamn_run_projection_writer; \
         GRANT SELECT ON {SCHEMA}.runs TO wamn_run_projection_writer;"
    ))
    .await
    .expect("install a surviving node-runs projection");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("a surviving projection retires");
    assert_eq!(
        plan.actions
            .iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>(),
        vec![RunPlaneActionKind::RetireNodeRuns],
        "retirement is a one-action plan: {:#?}",
        plan.actions
    );
    assert!(
        !table_exists(su, SCHEMA, "node_runs").await,
        "populated projection rows are discarded with their relation"
    );
    let projection_authority: bool = su
        .query_one(
            &format!(
                "SELECT has_schema_privilege('wamn_run_projection_writer','{SCHEMA}','USAGE')"
            ),
            &[],
        )
        .await
        .expect("read retired projection-writer authority")
        .get(0);
    assert!(
        !projection_authority,
        "the projection writer keeps schema authority after its relation is gone"
    );
    let projection_table_authority: bool = su
        .query_one(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class AS relation \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
                   JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
                  WHERE relation.relnamespace = to_regnamespace('{SCHEMA}') \
                    AND grantee.rolname = 'wamn_run_projection_writer')"
            ),
            &[],
        )
        .await
        .expect("read retired projection-writer table authority")
        .get(0);
    // The grant on the SURVIVING `runs` is what makes this observable: dropping
    // node_runs takes its own ACL with it, so only a grant on a relation that
    // outlives the projection can witness the `ON ALL TABLES` revoke.
    assert!(
        !projection_table_authority,
        "the projection writer keeps a table grant somewhere in the schema"
    );

    // The next pass sees an ordinary schema and reaches everything the
    // one-action plan deferred.
    assert!(
        !reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .expect("the pass after retirement proceeds")
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::RetireNodeRuns),
        "retirement is idempotent"
    );
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
           created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tenant_id,run_id)); \
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
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog schema");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs(tenant_id,run_id,flow_id,flow_version,status) \
           VALUES ('t1','history-run','f',1,'completed');"
    ))
    .await
    .expect("seed compatible shared history");

    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated shared-runner legacy fixture must refuse the pin carriers");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("shared-runner refusal is a database refusal");
    // The bespoke `execution-pin-cutover-requires-empty-run-and-release-membership`
    // refusal no longer exists anywhere in the tree. The RULE it enforced does:
    // the admission pins are NOT NULL in the record, so PostgreSQL itself
    // refuses the ADD against populated legacy rows rather than let the
    // reconciler fabricate provenance for them. That is the documented
    // contract — "a legacy row that violates the canonical contract aborts
    // reconciliation rather than being rewritten or deleted".
    assert_eq!(database.code().code(), "23502");
    assert!(
        database.message().contains("catalog_id") && database.message().contains("runs"),
        "the refusal names the pin carrier it could not fabricate: {}",
        database.message()
    );

    // What the refusal must preserve is the HISTORY and the absence of
    // fabricated provenance. It is NOT a pre-mutation refusal: only
    // `PRE_ROLE_BOOTSTRAP_ACTIONS` run ahead of everything else, and creating a
    // missing record table is not one of them, so earlier actions in the same
    // pass legitimately land before this one aborts.
    let retained = su
        .query_one(
            &format!("SELECT count(*), min(flow_id), min(status) FROM {SCHEMA}.runs"),
            &[],
        )
        .await
        .expect("read the refusal-preserved legacy history");
    assert_eq!(retained.get::<_, i64>(0), 1);
    assert_eq!(retained.get::<_, Option<String>>(1).as_deref(), Some("f"));
    assert_eq!(
        retained.get::<_, Option<String>>(2).as_deref(),
        Some("completed")
    );
    for column in ["catalog_id", "catalog_version", "environment"] {
        assert!(
            !column_exists(su, "runs", column).await,
            "refusal leaves pin column {column} absent"
        );
    }
}

/// Retired child/wait state is removed only when every durable row is ordinary.
async fn child_run_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_child_run_state(su).await;
    seed_run_admission_facts(su, "child-cutover", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,trigger_source, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('child-cutover','retained-run','f',1,'cat',1,'dev', \
                 'event','source-run','event-root',3);"
    ))
    .await
    .expect("seed retained ordinary run");

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
                "SELECT r.trigger_source, r.event_source_run_id, r.event_root_run_id, r.event_depth \
                   FROM {SCHEMA}.runs AS r \
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
    seed_run_admission_facts(su, "rerun-cutover", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN replay_of text, ADD COLUMN root_run_id text; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,root_run_id) \
           WHERE root_run_id IS NOT NULL; \
         INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,trigger_source,input_json,state_json,replay_of,root_run_id, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('rerun-cutover','retained-run','f',1,'cat',1,'dev', \
                 'event','{{\"payload\":7}}','{{\"cursor\":9}}', \
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
                   NOT has_any_column_privilege('wamn_app','{SCHEMA}.runs','INSERT') \
                     AND NOT has_any_column_privilege('wamn_app','{SCHEMA}.runs','UPDATE') \
                     AND has_table_privilege('wamn_app','{SCHEMA}.runs','SELECT') \
                     AND has_table_privilege('wamn_app','{SCHEMA}.runs','DELETE')"
            ),
            &[],
        )
        .await
        .expect("read retained event-lineage contract");
    assert!(retained_event_contract.get::<_, bool>(0));
    assert!(retained_event_contract.get::<_, bool>(1));
    // The guest role writes no run column at all since wamn-0h0g.22.7
    // (b1d42599): `wamn_app` holds table SELECT and DELETE and nothing else, so
    // event lineage is unreachable to it by ACL as well as by the immutability
    // trigger above. This used to demand column INSERT, an authority the
    // reconciler now revokes.
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

/// The retired per-node failure detail is deliberately discarded: its node
/// coordinate no longer has a live representation. The retained failure class
/// and typed caller outcome remain on the same run row. `RESTRICT` makes an
/// un-inventoried dependency a loud, atomic refusal before role bootstrap.
async fn failure_detail_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_failure_detail(su).await;
    seed_failure_detail_run(su, "retained-run").await;

    let before_dry_run = failure_detail_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("retired failure detail plans a row-preserving cutover");
    assert_eq!(
        dry.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover),
        "failure-detail deletion is leading on the current legacy shape: {:#?}",
        dry.actions
    );
    assert_eq!(
        dry.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::FailureDetailCutover)
            .count(),
        1,
        "one legacy shape must plan one cutover: {:#?}",
        dry.actions
    );
    assert!(dry.extra_columns.is_empty());
    assert_eq!(
        failure_detail_snapshot(su).await,
        before_dry_run,
        "dry-run mutated populated failure history"
    );

    let applied = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("populated retired failure detail is deliberately discarded");
    assert_eq!(
        applied.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover)
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(
            !column_exists(su, "runs", retired).await,
            "retired failure detail remains: {retired}"
        );
    }
    assert_retained_failure_record(su, "retained-run").await;
    let again = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("failure-detail second reconcile plans");
    assert!(
        again.is_noop(),
        "failure-detail cutover did not converge: {:#?}",
        again.actions
    );

    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_failure_detail(su).await;
    seed_failure_detail_run(su, "dependent-run").await;
    su.batch_execute(&format!(
        "CREATE VIEW {SCHEMA}.retired_failure_detail_dependency AS \
           SELECT tenant_id,run_id,fail_reason FROM {SCHEMA}.runs; \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("install dependent-view and role-bootstrap sentinels");

    let before_refusal = failure_detail_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("dependent failure detail still plans a cutover");
    assert_eq!(
        dry.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover),
        "dependent refusal must lead before role repair: {:#?}",
        dry.actions
    );
    assert_eq!(failure_detail_snapshot(su).await, before_refusal);
    let membership_before: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read pre-bootstrap membership sentinel after dry-run")
        .get(0);
    assert!(membership_before, "dry-run mutated role membership");

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("dependent view must refuse failure-detail retirement");
    assert_db_code_in_chain(&error, "2BP01", "dependent failure-detail view refusal");
    assert_eq!(
        failure_detail_snapshot(su).await,
        before_refusal,
        "RESTRICT refusal changed columns, view, or populated row"
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(
            column_exists(su, "runs", retired).await,
            "atomic refusal lost {retired}"
        );
    }
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read role-bootstrap sentinel after refusal")
        .get(0);
    assert!(
        membership_retained,
        "dependent-object refusal must precede role bootstrap"
    );

    su.batch_execute(&format!(
        "DROP VIEW {SCHEMA}.retired_failure_detail_dependency;"
    ))
    .await
    .expect("remove the external dependency only");
    let recovered = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("dependency-free failure detail cuts over");
    assert_eq!(
        recovered.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover)
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(!column_exists(su, "runs", retired).await);
    }
    assert_retained_failure_record(su, "dependent-run").await;
    let membership_repaired: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read repaired role membership")
        .get(0);
    assert!(
        !membership_repaired,
        "role repair did not converge after cutover"
    );
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe recovered failure-detail cutover")
            .is_noop()
    );
}

/// A populated queue is retained when no worker holds a live lease. Only the
/// retired partition state is removed, and the global FIFO claim index lands
/// in its exact record shape.
async fn partition_plane_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_partition_plane(su).await;
    seed_run_admission_facts(su, "partition", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment) \
         VALUES ('partition','retained-run','f',1,'cat',1,'dev'); \
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
                seed_run_admission_facts(su, "leased", "cat", 1, "dev", "standard").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                        environment) \
                     VALUES ('leased','active-run','f',1,'cat',1,'dev'); \
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
                seed_run_admission_facts(su, "ambiguous", "cat", 1, "dev", "standard").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                        environment) \
                     VALUES ('ambiguous','queue-run','f',1,'cat',1,'dev'); \
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
    seed_run_admission_facts(su, "dead-letter", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment) \
         VALUES ('dead-letter','failed-run','f',1,'cat',1,'dev'); \
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
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment) \
         VALUES ('t1','r-policy-durable','f',1,'cat',1,'dev')"
    ))
    .await
    .expect("admit a run under the projected durable policy");

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
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment) \
         VALUES ('t1','r-policy-standard','f',1,'cat',1,'dev')"
    ))
    .await
    .expect("admit a run under the changed standard policy");
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
        "policy changes affect future admissions without rewriting existing runs"
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
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
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
/// plane + `catalog`, and a functional smoke as `wamn_app` proves grants + RLS
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
    for table in [
        "flow_drafts",
        "draft_safe_connection_grants",
        "authoring_command_audit",
    ] {
        assert!(
            !table_exists(su, "catalog", table).await,
            "the portable store's relation catalog.{table} is not this verb's to provision"
        );
    }

    // Functional smoke as the runtime role: the sections' grants + RLS hold.
    // wamn-0h0g.22.7 (b1d42599) took every run-plane WRITE away from
    // `wamn_app` — table SELECT and DELETE on `runs`, nothing at all on
    // `run_queue` — so the row is seeded as superuser and the guest role is
    // proven to READ under RLS and to be REFUSED on write.
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment) \
             VALUES ('t1','r1','f',1,'cat',1,'dev'); \
         INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1', 'r1');"
    ))
    .await
    .expect("seed a tenant run-plane row");
    su.batch_execute("SET ROLE wamn_app; SELECT set_config('app.tenant', 't1', false);")
        .await
        .expect("assume the runtime role");
    for refused in [
        format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment) \
             VALUES ('t1','r2','f',1,'cat',1,'dev')"
        ),
        format!("INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ('t1','r2')"),
    ] {
        let denied = su
            .batch_execute(&refused)
            .await
            .expect_err("the runtime role writes no run-plane row");
        assert_db_code(denied, "42501", "runtime-role write refusal");
    }
    // `runs` is the only run-plane relation the guest role can still read;
    // wamn-0h0g.22.7 (b1d42599) left it nothing at all on `run_queue`.
    let visible: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
        .await
        .expect("tenant read")
        .get(0);
    assert_eq!(visible, 1, "own tenant sees its row");
    let queue_denied = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .expect_err("the runtime role cannot read the queue at all");
    assert_db_code(queue_denied, "42501", "runtime-role queue read refusal");
    su.batch_execute("SELECT set_config('app.tenant', 't2', false)")
        .await
        .expect("switch tenant");
    let foreign: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
        .await
        .expect("foreign read")
        .get(0);
    assert_eq!(foreign, 0, "RLS isolates the foreign tenant");
    su.batch_execute("RESET ROLE; SELECT set_config('app.tenant', '', false)")
        .await
        .expect("drop back to superuser");

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

async fn visible_environment_policy_rows(url: &str, tenant: &str) -> i64 {
    let app = connect_as(url, "wamn_app", "wamn_app").await;
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
/// proving the catalog converged, not merely that the SQL happened to run.
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
                visible_environment_policy_rows(url, "t2").await,
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
                visible_environment_policy_rows(url, "t2").await,
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

async fn capture_mode_additive_leg(su: &Client, url: &str) {
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
    seed_run_admission_facts(su, "t1", "capture", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            status) \
         VALUES ('t1','legacy-off','f',1,'capture',1,'dev', \
                 'completed'); \
         DROP TRIGGER runs_admission_pins_immutable ON {SCHEMA}.runs; \
         ALTER TABLE {SCHEMA}.runs \
           DROP CONSTRAINT runs_capture_mode_source_check, \
           DROP COLUMN capture_mode;"
    ))
    .await
    .expect("build populated pre-capture carrier schema");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("capture carrier reconcile applies to populated history");
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::AddColumn && action.target == "runs.capture_mode"
    }));
    let mode: String = su
        .query_one(
            &format!("SELECT capture_mode FROM {SCHEMA}.runs WHERE run_id='legacy-off'"),
            &[],
        )
        .await
        .expect("read legacy defaulted mode")
        .get(0);
    assert_eq!(mode, "off");

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
                    status,trigger_source,capture_mode) \
                 VALUES ('t1','published-full','f',1,'capture',1,'dev', \
                         'completed','http','full')"
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
                status,trigger_source,capture_mode) \
             VALUES ('t1','draft-full','f',1,'capture',1,'dev', \
                     'completed','scenario-draft','full')"
        ),
        &[],
    )
    .await
    .expect("canonical direct draft may carry full");

    // wamn-0h0g.22.7 (b1d42599) replaced the capture-mode COLUMN confinement
    // with a whole-relation one: `wamn_app` holds table SELECT and DELETE on
    // `runs` and no write of any shape. The admission that used to prove the
    // `off` default moved to the private management path, so what this leg
    // still proves live is the confinement the reconciler restores.
    let app = connect_as(url, "wamn_app", "wamn_app").await;
    app.batch_execute("SELECT set_config('app.tenant','t1',false)")
        .await
        .expect("enter application tenant for capture authority probes");
    for (label, refused) in [
        (
            "admission",
            format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                    status,trigger_source) \
                 VALUES ('t1','app-forged','f',1,'capture',1,'dev','dispatched','test')"
            ),
        ),
        (
            "capture-mode update",
            format!("UPDATE {SCHEMA}.runs SET capture_mode='full' WHERE run_id='draft-full'"),
        ),
        (
            "ordinary update",
            format!("UPDATE {SCHEMA}.runs SET status='running' WHERE run_id='draft-full'"),
        ),
    ] {
        let denied = app
            .execute(&refused, &[])
            .await
            .expect_err(&format!("{label} must be refused"));
        assert_db_code(denied, "42501", label);
    }
    let readable: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE run_id='draft-full'"),
            &[],
        )
        .await
        .expect("the application role retains its tenant read")
        .get(0);
    assert_eq!(readable, 1);

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("capture carrier second reconcile plans");
    assert!(
        again.is_noop(),
        "capture carrier converged: {:#?}",
        again.actions
    );
}

async fn stored_suite_cutover_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog before stored-suite cutover");
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
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
    // The authoring-test orchestration relations left the run-plane record with
    // wamn-0h0g.9.11.3 (805701ec) and now live only in the control database's
    // portable store, which this verb must never apply. A legacy run plane
    // carries them in its own schema, and that is the shape this cutover exists
    // to reach — so synthesize it here, exactly as the stored-suite tables above
    // are synthesized.
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.authoring_test_run_reservations ( \
           tenant_id text NOT NULL, report_id text NOT NULL, \
           PRIMARY KEY (tenant_id, report_id)); \
         CREATE TABLE {SCHEMA}.authoring_test_reports ( \
           tenant_id text NOT NULL, report_id text NOT NULL, \
           PRIMARY KEY (tenant_id, report_id)); \
         CREATE TABLE {SCHEMA}.authoring_test_case_runs ( \
           tenant_id text NOT NULL, report_id text NOT NULL, ordinal int NOT NULL, \
           PRIMARY KEY (tenant_id, report_id, ordinal));"
    ))
    .await
    .expect("synthesize the legacy run-plane authoring-test orchestration shape");
    // The pre-wamn-0h0g.15.27 test-set store, with the live grant that made it
    // invisible to the privilege reconciler once the relation left the record,
    // and the two FK columns on RETAINED tables that block its drop.
    su.batch_execute(&format!(
        "CREATE FUNCTION {SCHEMA}.reject_immutable_authoring_test_set_change() RETURNS trigger \
           LANGUAGE plpgsql AS $guard$ BEGIN RETURN NEW; END $guard$; \
         CREATE TABLE {SCHEMA}.authoring_test_sets ( \
           tenant_id text NOT NULL, test_set_hash text NOT NULL, \
           PRIMARY KEY (tenant_id, test_set_hash)); \
         CREATE TRIGGER authoring_test_sets_update_immutable \
           BEFORE UPDATE ON {SCHEMA}.authoring_test_sets \
           FOR EACH ROW EXECUTE FUNCTION \
             {SCHEMA}.reject_immutable_authoring_test_set_change(); \
         GRANT SELECT, INSERT ON {SCHEMA}.authoring_test_sets TO wamn_scenario_author; \
         ALTER TABLE {SCHEMA}.authoring_test_run_reservations \
           ADD COLUMN test_set_hash text NOT NULL, \
           ADD CONSTRAINT authoring_test_reservation_test_set_fk \
             FOREIGN KEY (tenant_id, test_set_hash) \
             REFERENCES {SCHEMA}.authoring_test_sets (tenant_id, test_set_hash); \
         ALTER TABLE {SCHEMA}.authoring_test_reports \
           ADD COLUMN test_set_hash text NOT NULL, \
           ADD CONSTRAINT authoring_test_report_test_set_fk \
             FOREIGN KEY (tenant_id, test_set_hash) \
             REFERENCES {SCHEMA}.authoring_test_sets (tenant_id, test_set_hash);"
    ))
    .await
    .expect("install the retired test-set store and its FK columns");
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
        "authoring_test_sets",
    ] {
        assert!(
            !table_exists(su, SCHEMA, table).await,
            "retired table {table} is absent"
        );
    }
    // The FK columns had to go first, or the parent DROP TABLE would have
    // refused on the dependency — and a surviving NOT NULL orphan would have
    // refused every reservation and report INSERT (wamn-0h0g.15.78).
    for table in ["authoring_test_run_reservations", "authoring_test_reports"] {
        assert!(
            !column_exists(su, table, "test_set_hash").await,
            "{table}.test_set_hash survived the test-set retirement"
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
                  'reject_immutable_authoring_report_change', \
                  'reject_immutable_authoring_test_set_change')",
            &[&SCHEMA],
        )
        .await
        .expect("count retired stored-suite functions")
        .get(0);
    assert_eq!(retired_functions, 0);

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("stored-suite cutover second reconcile plans");
    assert!(
        again.is_noop(),
        "stored-suite cutover converged: {:#?}",
        again.actions
    );
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
         GRANT INSERT, UPDATE, DELETE ON catalog.releases TO wamn_scenario_author; \
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_test_run_reservations TO wamn_app; \
         GRANT ALL PRIVILEGES ON {SCHEMA}.authoring_test_case_runs TO PUBLIC; \
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
                "SELECT has_table_privilege(current_user,'catalog.releases','SELECT') \
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
                    catalog_version,passed,summary) \
                 VALUES ('t1','forged','validated-a','cat',1,true,'{{}}'::jsonb)"
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
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    // Regress the run failure vocabulary to the predecessor constraint. The
    // node-error vocabulary that stood beside it left CHECK_SPECS with the
    // projection (wamn-0h0g.26.3.1, 204220e8).
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs DROP CONSTRAINT runs_fail_kind_check; \
         ALTER TABLE {SCHEMA}.runs ADD CONSTRAINT runs_fail_kind_check \
             CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input'));"
    ))
    .await
    .expect("regress the persisted failure vocabulary");
    // A run whose runaway verdict we will try to record.
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
              environment) \
             VALUES ('t1','r-budget','f',1,'cat',1,'dev');"
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

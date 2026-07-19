//! wamn project provisioning (2.3) — the **pure** core.
//!
//! Standing up a project turns the SQL-emitting E3 crates into a live system:
//! given a project id, provision a per-project Postgres **database** on the
//! shared cluster (D6: CloudNativePG, one shared Cluster with a database per
//! project), granting the shared [`APP_ROLE`] `CONNECT` and revoking it from
//! `PUBLIC`. The output 2.4 (system schema) consumes is *a provisioned,
//! credentialed, `wamn_app`-roled, empty project database*.
//!
//! This crate is the pure core (SR3 / house rule 1): identifier naming, the
//! `CREATE DATABASE` / role-bootstrap / `GRANT CONNECT` text builders, the
//! per-project credential Secret renderer, the connection-URL composer, and — for
//! the four-tier topology — the org [`Cluster` SET](crate::org) renderer (one
//! cluster per recovery-domain owner, each sized by its env policy — D18, cjv.21)
//! and the per-project-env CNPG [`Database` CR](crate::database) renderer
//! (wamn-q3n.7) — no DB, no K8s client, no clock. The effects live in the
//! `provision-project` / `provision-org` / `provision-project-env` subcommands
//! (`wamn-ctl`); the `provisionbench` gate (`wamn-gates`) drives the whole path
//! against a real cluster.
//!
//! # Isolation model
//!
//! Postgres roles are **cluster-global**, so one shared cluster has one shared
//! `wamn_app` role (the grantee every generated floor and hand-written schema
//! already targets). Cross-project isolation is therefore **not** at the role
//! level — it is:
//!
//! 1. **per-project DATABASE** — a component resolved to project *a* holds a
//!    connection pool to *a*'s database only and physically cannot address
//!    another project's database (Postgres has no cross-database queries);
//! 2. **per-DB CONNECT** — `PUBLIC` is revoked and only `wamn_app` is granted
//!    `CONNECT`, so no unexpected role reaches a project database;
//! 3. **RLS within** — the 3.2 tenant floor confines rows by `app.tenant`.
//!
//! Per-project **distinct** roles/passwords (stronger credential isolation) are
//! a hardening follow-up (8.2), not this MVP.
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: `wamn-run-queue`'s `claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

pub mod backup;
pub mod copy;
pub mod database;
pub mod dump;
mod error;
mod name;
pub mod org;
pub mod restore;
pub mod secret;
pub mod sql;

pub use backup::{
    BACKUP_PLUGIN_NAME, MINIO_ENDPOINT, OBJECT_STORE_SECRET, WAL_BUCKET, cluster_backup_plugin,
    object_store_name, render_object_store, render_scheduled_backup, scheduled_backup_name,
};
pub use copy::{
    COPY_SAGA_KIND, CopyInclude, CopyMode, CopyRequest, CopyScope, CopyStep, count_rows_sql,
    list_schema_tables_sql, pg_restore_data_only_argv, plan_copy, quiesce_database_sql,
    terminate_database_backends_sql, unquiesce_database_sql,
};
pub use database::render_project_env_database;
pub use dump::{
    DEFAULT_BUCKET, DEFAULT_DUMP_SCHEDULE, dump_object_key, dump_resource_name, pg_dump_argv,
    render_project_env_dump_cronjob, render_project_env_dump_job, validate_dump_resource_name,
};
pub use error::ProvisionError;
pub use name::{
    APP_ROLE, CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_PREFIX, MAX_DB_NAME_LEN, MAX_PROJECT_ID_LEN,
    cdc_object_name, compose_url, database_name, event_stream_name, project_env_cdc_secret_name,
    project_env_database_name, project_env_secret_name, secret_name, validate_project_env,
    validate_project_env_cdc, validate_project_id,
};
pub use org::{OrgClusters, render_org_cluster_set};
pub use restore::{pg_restore_argv, restore_scratch_db_name, validate_restore_scratch_name};
pub use secret::{render_project_env_cdc_secret_manifest, render_project_env_secret_manifest};

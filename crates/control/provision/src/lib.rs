//! wamn project provisioning (2.3) — the **pure** core.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
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
//! correctly here and still misbehave live: a prior run-queue batch claim
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

#[cfg(feature = "ops")]
pub mod backup;
pub mod control_author;
#[cfg(feature = "ops")]
pub mod copy;
pub mod database;
#[cfg(feature = "ops")]
pub mod dump;
mod error;
pub mod management_admitter;
mod name;
pub mod org;
#[cfg(feature = "ops")]
pub mod restore;
pub mod saga;
pub mod secret;
pub mod sql;
/// The two T1 control-database read consumers' scoped credential contract.
pub mod system_reader;
#[cfg(feature = "ops")]
pub mod state;
/// The pure derivation guest RLS uses to reach a tenant from `current_user`.
pub mod tenant_key;
pub mod workload_role;

#[cfg(feature = "ops")]
pub use backup::{
    BACKUP_PLUGIN_NAME, MINIO_ENDPOINT, OBJECT_STORE_SECRET, WAL_BUCKET, cluster_backup_plugin,
    object_store_name, render_object_store, render_scheduled_backup, scheduled_backup_name,
};
pub use control_author::{
    ControlAuthoringConnection, ControlAuthoringUrlError, ControlAuthoringUrlErrorKind,
    control_author_generation_role, control_author_scope_hash, parse_control_authoring_url,
};
#[cfg(feature = "ops")]
pub use copy::{
    COPY_SAGA_KIND, CopyRequest, CopyStep, count_rows_sql, list_schema_tables_sql,
    pg_restore_data_only_argv, plan_copy, quiesce_database_sql, terminate_database_backends_sql,
    unquiesce_database_sql,
};
pub use database::render_project_env_database;
#[cfg(feature = "ops")]
pub use dump::{
    DEFAULT_BUCKET, DEFAULT_DUMP_SCHEDULE, dump_object_key, dump_resource_name, pg_dump_argv,
    render_project_env_dump_cronjob, render_project_env_dump_job, validate_dump_resource_name,
};
pub use error::ProvisionError;
pub use management_admitter::{
    ManagementAdmissionConnection, ManagementAdmissionUrlError, ManagementAdmissionUrlErrorKind,
    management_admitter_generation_role, management_admitter_scope_hash,
    parse_management_admission_url,
};
pub use name::{
    APP_ROLE, CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, CONTROL_AUTHOR_SECRET_PREFIX, DB_OWNER_ROLE,
    DB_PREFIX, DISPATCH_READER_ROLE, EFFECT_WRITER_SECRET_PREFIX, GUEST_SECRET_PREFIX,
    INSTANCE_SUFFIX_LEN, MANAGEMENT_ADMITTER_SECRET_PREFIX, MAX_DB_NAME_LEN, MAX_NAMESPACE_LEN,
    MAX_NAMESPACE_STEM_LEN, MAX_PROJECT_ID_LEN, NAMESPACE_PREFIX, cdc_object_name, compose_url,
    control_author_secret_name, database_name, event_stream_name, management_admitter_secret_name,
    project_env_cdc_secret_name, project_env_database_name, project_env_effect_writer_secret_name,
    project_env_guest_secret_name, project_env_namespace, project_env_secret_name, secret_name,
    validate_instance_suffix, validate_project_env, validate_project_env_cdc, validate_project_id,
    workload_secret_name,
};
pub use org::{OrgClusters, render_org_cluster_set};
#[cfg(feature = "ops")]
pub use restore::{pg_restore_argv, restore_scratch_db_name, validate_restore_scratch_name};
pub use system_reader::{
    SystemReader, SystemReaderConnection, SystemReaderUrlError, SystemReaderUrlErrorKind,
    parse_system_reader_url, system_reader_generation_role, system_reader_scope_hash,
};

pub use secret::{
    WorkloadSecretBody, render_control_author_secret_manifest,
    render_effect_writer_secret_manifest, render_guest_secret_manifest,
    render_management_admitter_secret_manifest, render_project_env_cdc_secret_manifest,
    render_project_env_secret_manifest, render_workload_secret_manifest,
};
pub use wamn_run_state::{
    CredentialGeneration, EFFECT_WRITER_CREDENTIAL_KEY, EFFECT_WRITER_CREDENTIAL_PATH,
    EFFECT_WRITER_CREDENTIAL_SCHEMA_VERSION, EFFECT_WRITER_ROLE, EffectWriterCredential,
    EffectWriterCredentialError, EffectWriterCredentialErrorKind, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, effect_writer_credential, effect_writer_generation_role,
    effect_writer_scope_hash, parse_effect_writer_credential, validate_effect_writer_credential,
};
pub use workload_role::{
    CONTROL_AUTHOR_ROLE, MANAGEMENT_ADMITTER_ROLE, PLATFORM_GROUP_ROLE, RETENTION_ROLE,
    SERVICE_READER_ROLE, WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeError,
    WorkloadRoleScopeKind, WorkloadSecretBodyKind, legacy_effect_writer_generation_role,
    workload_generation_role, workload_role_scope_hash,
};

/// Core control-database schema, applied first by a fresh bootstrap.
pub const SYSTEM_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/system-schema.sql");

/// Dormant portable-store extension, applied after [`SYSTEM_SCHEMA_SQL`].
pub const CONTROL_PORTABLE_STORE_SQL: &str =
    include_str!("../../../../deploy/sql/control-portable-store.sql");

/// Ordered fresh-control bootstrap record. Keeping the portable extension in
/// this composition prevents a caller from provisioning the legacy registry
/// floor while silently omitting the control authoring/release store.
pub const CONTROL_BOOTSTRAP_SQL: [&str; 2] = [SYSTEM_SCHEMA_SQL, CONTROL_PORTABLE_STORE_SQL];

/// Operations persistence extension, installed after the core system schema.
#[cfg(feature = "ops")]
pub const OPS_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/ops-schema.sql");

//! Database declarations, restricted login identities and privilege SQL.
//!
//! CloudNativePG owns login roles and their basic-auth Secrets. SQL declarations
//! own passwordless privilege roles and grants. Each login has CONNECT only on
//! its declared database. Project identities remain distinct inside that database.

pub mod apply;
pub mod audit_retention;
pub mod connection;
pub mod environment_names;
mod error;
pub mod host_credentials;
pub mod identity_role;
pub mod migration;
pub mod migrations;
mod name;
pub mod operation_grants;
pub mod platform_principals;
/// The pure derivation guest RLS uses to reach a project from `current_user`.
pub mod project_key;
/// The two T1 control-database read consumers' targeted credential contract.
pub mod readers;
pub mod session_target;
pub mod sql;
/// Test databases with the system control store minimum.
#[cfg(any(test, feature = "test-util"))]
pub mod test_database;
pub mod workload_role;

pub use error::ProvisionError;
pub use name::{
    APP_ROLE, CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_OWNER_ROLE, DB_PREFIX, GUEST_SECRET_PREFIX,
    INSTANCE_SUFFIX_LEN, MAX_DB_NAME_LEN, MAX_NAMESPACE_LEN, MAX_NAMESPACE_STEM_LEN,
    MAX_PROJECT_ID_LEN, NAMESPACE_PREFIX, cdc_object_name, control_administration_key,
    control_administration_secret_name, org_workload_secret_name, project_env_cdc_secret_name,
    project_env_database_name, project_env_guest_secret_name, project_env_key,
    project_env_namespace, project_env_secret_name, validate_instance_suffix, validate_project_env,
    validate_project_env_cdc, validate_project_id, workload_secret_name,
};
pub use platform_principals::{
    PlatformDomainError, bind_platform_principal_sql, platform_principals_sql,
    validate_platform_domain,
};
pub use readers::{SystemReader, system_reader_identity_hash, system_reader_login};
/// The platform component list, for callers that bind a component as the actor.
pub use wamn_identity_protocol::PlatformComponent;
pub use workload_role::{
    ADMINISTRATION_ROLE, AUDIT_RETENTION_ROLE, CONTROL_ROLE, PLATFORM_GROUP_ROLE, RETENTION_ROLE,
    SERVICE_READER_ROLE, WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityError,
    WorkloadRoleIdentityType, workload_login, workload_role_identity_hash,
};

/// Core control-database schema, applied first by a fresh bootstrap.
///
/// It carries `deploy/sql/record-history.sql` first, because the identity
/// relations have stamp triggers. That file grants to `wamn_db_owner`, so an
/// applier that runs as `wamn_system` runs [`sql::ensure_db_owner_role_sql`]
/// before it.
pub const SYSTEM_SCHEMA_SQL: &str = concat!(
    include_str!("../../../../deploy/sql/record-history.sql"),
    include_str!("../../../../deploy/sql/system-schema.sql"),
);

/// Targeted connection-reader authority for control database logins.
pub const CONTROL_PORTABLE_STORE_SQL: &str =
    include_str!("../../../../deploy/sql/control-portable-store.sql");

/// The declarations applied to a fresh control database, in order.
pub const CONTROL_BOOTSTRAP_SQL: [&str; 2] = [SYSTEM_SCHEMA_SQL, CONTROL_PORTABLE_STORE_SQL];

/// The per-project authorization and configuration schema, applied to a project
/// database after its catalog schema.
///
/// Every relation it creates carries a stamp trigger that calls
/// `wamn_history.stamp_row()`, so the applier installs
/// `deploy/sql/record-history.sql` first. `CATALOG_SCHEMA_SQL` carries that
/// file, so an applier that applied the catalog schema already holds it
/// (`wamn-0h0g.9.15`).
pub const APP_SCHEMA_SQL: &str = concat!(include_str!("../../../../deploy/sql/app-schema.sql"),);

mod placement;
mod template;
pub use placement::{
    ClusterRef, DEFAULT_PG_IMAGE, EnvPolicy, Org, OrgEnvPolicy, Placement, RecoveryDomain,
    cluster_of,
};
pub use template::Template;

/// Project event trigger installed by the authenticated administrator during initialization.
pub const MIGRATION_PRIVILEGES_SQL: &str =
    include_str!("../../../../deploy/sql/migration-privileges.sql");

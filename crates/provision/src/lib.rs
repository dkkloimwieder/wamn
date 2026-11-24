//! Database declarations, restricted login identities and privilege SQL.
//!
//! CloudNativePG owns managed login roles. Project logins authenticate with
//! client certificates. SQL declarations own privilege roles and grants.

pub mod apply;
pub mod audit_retention;
pub mod environment_names;
mod error;
pub mod identity_role;
pub mod migration;
pub mod migrations;
mod name;
pub mod operation_grants;
pub mod sql;
pub mod system_reader;
pub mod transaction_identity;
pub mod workload_role;

pub use error::ProvisionError;
pub use name::{
    CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_PREFIX, INSTANCE_SUFFIX_LEN, MAX_DB_NAME_LEN,
    MAX_NAMESPACE_LEN, MAX_NAMESPACE_STEM_LEN, MAX_PROJECT_ID_LEN, NAMESPACE_PREFIX,
    cdc_object_name, cdc_reader_name, project_env_database_name, project_env_key,
    project_env_namespace, project_env_secret_name, validate_project_env, validate_project_env_cdc,
};
pub use transaction_identity::{bind_platform_principal, bind_platform_principal_sql};
/// The platform component list, for callers that bind a component as the actor.
pub use wamn_identity_protocol::PlatformComponent;
pub use workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityError, WorkloadRoleIdentityType,
    workload_login,
};

/// Core control-database schema, applied first by a fresh bootstrap.
///
/// It installs record history before identity and registry storage.
pub const SYSTEM_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/record-history.sql"),
    include_str!("../../../deploy/sql/system-schema.sql"),
    include_str!("../../../deploy/sql/definer-privileges.sql"),
);

/// Targeted connection-reader authority for control database logins.
pub const CONTROL_PORTABLE_STORE_SQL: &str =
    include_str!("../../../deploy/sql/control-portable-store.sql");

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
pub const APP_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/app-schema.sql"),
    include_str!("../../../deploy/sql/definer-privileges.sql"),
);

/// Complete catalog bootstrap for a fresh project database.
pub const CATALOG_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/catalog-schema-prefix.sql"),
    include_str!("../../../deploy/sql/reject-immutable-row-change.sql"),
    include_str!("../../../deploy/sql/record-history.sql"),
    include_str!("../../../deploy/sql/record-history-app-grants.sql"),
    include_str!("../../../deploy/sql/catalog-schema.sql"),
    include_str!("../../../deploy/sql/definer-privileges.sql"),
);

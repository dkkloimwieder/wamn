//! Database declarations, restricted login identities and privilege SQL.
//!
//! CloudNativePG owns managed login roles. Project logins authenticate with
//! client certificates. SQL declarations own privilege roles and grants.

pub mod apply;
pub mod apply_package;
pub use wamn_postgres::audit_retention;
pub mod environment_names;
pub mod environment_sql_effects;
mod error;
pub mod identity_role;
pub use wamn_postgres::project_migration as migration;
pub mod migrations;
mod name;
pub use wamn_postgres::operation_grants;
pub mod platform_environment;
pub mod platform_migrations;
pub mod provision_org;
pub mod provision_system;
mod roles;
pub mod sql;
pub mod system_reader;
pub use wamn_postgres::platform_identity as transaction_identity;
pub use wamn_postgres::provisioning_transaction as transaction;

pub use error::ProvisionError;
pub use name::{
    CDC_OBJECT_PREFIX, CDC_SECRET_PREFIX, DB_PREFIX, INSTANCE_SUFFIX_LEN, MAX_DB_NAME_LEN,
    MAX_NAMESPACE_LEN, MAX_NAMESPACE_STEM_LEN, MAX_PROJECT_ID_LEN, NAMESPACE_PREFIX,
    cdc_object_name, cdc_reader_name, project_env_database_name, project_env_key,
    project_env_namespace, project_env_secret_name, validate_project_env, validate_project_env_cdc,
};
pub use transaction_identity::{bind_platform_principal, bind_platform_principal_sql};
/// The platform component list, for callers that bind a component as the actor.
pub use wamn_postgres::PlatformComponent;

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

pub use wamn_postgres::project_setup::{APP_SCHEMA_SQL, CATALOG_SCHEMA_SQL};

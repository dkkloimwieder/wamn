//! Platform and org installation workflows.

mod apply;
pub mod database_files;
pub mod deployment;
pub mod environment;
pub mod identity_role;
pub mod install;
mod migration_history;
pub mod org;
pub mod org_install;
pub mod org_secrets;
mod platform_database;
pub mod platform_install;
mod platform_registry;
mod platform_sql;
pub mod provision_org;
pub mod provision_system;
pub mod release_chart;
pub mod sql;
pub mod system_reader;
pub mod worker_mounts;

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

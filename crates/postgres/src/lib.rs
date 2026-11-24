//! Verified PostgreSQL connections from explicit component configuration.

pub mod administration;
pub mod audit_retention;
pub mod authorization;
pub mod build_database;
pub mod capture_sql;
pub mod catalog;
mod connection_guard;
mod connection_options;
pub mod control;
pub mod database;
pub mod environment_configuration;
pub mod environment_fill;
pub mod environment_org;
pub mod environment_registry;
pub mod environment_schema;
pub mod environment_sql;
mod executor;
mod file_catalog;
pub mod file_connections;
pub mod identity;
pub mod identity_error;
pub mod identity_sql;
pub mod migration_scope;
pub mod migrations;
pub mod oauth_authorization;
pub mod operation_grants;
pub mod platform_database;
pub mod platform_identity;
pub mod platform_login;
pub mod platform_migrations;
pub mod platform_sql;
pub mod project_migration;
pub mod project_setup;
pub mod provisioning_transaction;
pub mod prune_record_history;
pub mod prune_run_history;
pub mod release_requirements;

pub use platform_identity::{bind_platform_principal, bind_platform_principal_sql};
pub use wamn_identity_protocol::PlatformComponent;

pub use connection_guard::ConnectionGuard;
pub use connection_options::{
    ConnectionInput, IdentityMismatch, configuration, connect, connect_options,
    database_configuration, identity, mounted_options, pool, read_connection_config, read_password,
    require_identity,
};
pub use executor::{
    ExecutorOpenError, open_executor_read, open_executor_transaction, run_executor_transaction,
};

pub mod operation;
pub mod project_connections;
pub mod run_store;
pub mod schema;
pub mod workflow;

mod sql_error;
pub use sql_error::{connection_lost, map_pg_error, postgres_error_detail};

mod transaction_identity;
pub use transaction_identity::{
    BoundIdentity, TransactionIdentity, bind_transaction_identity, prepare_identity,
    transaction_identity_sql,
};

mod checkout;
mod host_project;
mod package_catalog;
mod pool;

pub use checkout::CheckoutError;
pub use host_project::HostProject;
pub use package_catalog::installed_package_artifacts;
pub use pool::{
    ClassCredentials, CredentialProvider, Object, ProjectConfig, ProjectPool, ResolvedCredential,
    StaticCredentialProvider, WamnPostgres, WamnPostgresConfig, destroy_connection,
};

mod registry;
pub use registry::{
    ENVIRONMENT_LOCK_SQL, ENVIRONMENT_TRANSACTION_LOCK_SQL, LifecycleLock, REGISTRY_HISTORY_TABLE,
    Registry, RegistryTransaction, decode_text,
};

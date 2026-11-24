//! Verified PostgreSQL connections from explicit component configuration.

pub mod advisory_lock;
mod connection_guard;
mod connection_options;
pub mod connection_readiness;
pub mod database;
mod executor;
pub mod migration_scope;
pub mod migrations;
pub mod operation_grant_lock;
pub mod package_migrations;
pub mod platform_identity;
pub mod platform_sql;
pub mod provisioning_transaction;

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

mod sql_error;
pub use sql_error::{connection_lost, map_pg_error, postgres_error_detail};

mod transaction_identity;
pub use transaction_identity::{
    BoundIdentity, TransactionIdentity, bind_transaction_identity, prepare_identity,
    transaction_identity_sql,
};

mod checkout;
mod host_project;
mod pool;

pub use checkout::CheckoutError;
pub use host_project::HostProject;
pub use pool::{
    ClassCredentials, CredentialProvider, Object, ProjectConfig, ProjectPool, ResolvedCredential,
    StaticCredentialProvider, WamnPostgres, WamnPostgresConfig, destroy_connection,
};

mod registry;
pub use registry::decode_text;

/// Environment exclusion key shared by lifecycle operations and CDC publication.
/// Acquire this key before an application key or a database mutation lock.
pub const ENVIRONMENT_LOCK_SQL: &str =
    "SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Transaction-scoped exclusion for integration mutations. Params: org, environment.
pub const ENVIRONMENT_TRANSACTION_LOCK_SQL: &str =
    "SELECT pg_try_advisory_xact_lock(hashtextextended('wamn-env/' || $1 || '/' || $2, 0))";

/// Migration history table in the registry schema.
pub const REGISTRY_HISTORY_TABLE: &str = "registry._sqlx_migrations";

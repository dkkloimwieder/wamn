//! PostgreSQL role names.

/// Application statements.
pub const APP_ROLE: &str = "wamn_app";
/// Dynamic SQL reads.
pub const QUERY_ROLE: &str = "wamn_query";
/// Host platform work and retention.
pub const PLATFORM_ROLE: &str = "wamn_platform";
/// Application login in a project database.
pub const APP_LOGIN: &str = "wamn_app_login";
/// Read-only login in a project database.
pub const QUERY_LOGIN: &str = "wamn_query_login";
/// Platform login in a project database.
pub const PLATFORM_LOGIN: &str = "wamn_platform_login";
/// Replication login in a project database.
pub const CDC_LOGIN: &str = "wamn_cdc";
/// Project database owner login.
pub const MIGRATION_ROLE: &str = "wamn_migration";
/// System database owner.
pub const SYSTEM_ROLE: &str = "wamn_system";
/// Host system reads, with one login per environment.
pub const SYSTEM_READER_ROLE: &str = "wamn_system_reader";
/// Environment apply login, retained until chart provisioning replaces it.
pub const APPLY_ROLE: &str = "wamn_apply";
/// Identity issuer.
pub const IDENTITY_ISSUER_ROLE: &str = "wamn_identity_issuer";
/// Organization control login prefix.
pub const CONTROL_ROLE: &str = "wamn_control";
/// System migration login prefix.
pub const SYSTEM_MIGRATION_ROLE: &str = "wamn_platform_sql";
/// PostgreSQL administrator.
pub const POSTGRES_ROLE: &str = "postgres";
/// PostgreSQL checkpoint privilege used by CDC.
pub const CHECKPOINT_ROLE: &str = "pg_checkpoint";

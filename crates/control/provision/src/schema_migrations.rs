//! Image-owned migration bytes and history DDL, separate from frozen initialization.

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// The image's two independent SQL histories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationScope {
    System,
    Project,
}

/// Create a scope's immutable history when its matching baseline is adopted.
/// The caller owns the transaction and binds the provisioning actor.
pub fn history_sql(scope: MigrationScope) -> String {
    let (schema, scope_name, owner) = match scope {
        MigrationScope::System => ("registry", "system", "wamn_system"),
        MigrationScope::Project => ("platform", "project", "wamn_db_owner"),
    };
    let mut sql = include_str!("../../../../deploy/sql/schema-migration-history.sql")
        .replace("@schema@", schema)
        .replace("@scope@", scope_name)
        .replace("@owner@", owner);
    // The project apply login inherits its database-owner grants. System apply
    // receives only the same read access as registry.schema_baseline.
    if scope == MigrationScope::System {
        sql.push_str("GRANT SELECT ON registry.schema_migration TO wamn_apply;");
    }
    sql
}

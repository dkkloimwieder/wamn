use super::sql;
use std::fmt::Write as _;

/// Render the CDC SQL for the project-env database, with the failover slot first.
/// No write precedes slot creation, even when the caller runs the whole bundle
/// in one transaction. The schema guard follows, then the decode-time entity
/// and exclusion maps, then the replication role's grants. Package apply owns
/// the map rows. Apply the role SQL to the cluster before this bundle.
/// Every statement is idempotent (safe to apply again).
pub fn cdc_sql_bundle(schemas: &[String], cdc_name: &str, db_name: &str) -> String {
    format!(
        "{}\n{}",
        sql::create_failover_slot_sql(cdc_name),
        cdc_schema_sql_bundle(schemas, cdc_name, db_name)
    )
}

pub(super) fn cdc_schema_sql_bundle(schemas: &[String], cdc_name: &str, db_name: &str) -> String {
    let mut bundle = String::new();
    if schemas.is_empty() {
        writeln!(
            bundle,
            "\nGRANT CONNECT ON DATABASE {} TO {};",
            postgres_protocol::escape::escape_identifier(db_name),
            postgres_protocol::escape::escape_identifier(cdc_name)
        )
        .expect("write the empty capture grant");
    }
    for schema in schemas {
        writeln!(
            bundle,
            "\n{schema_guard};\n{entity_map};\n{exclusion_map};\n{grants}",
            schema_guard = sql::ensure_schema_sql(schema),
            entity_map = sql::ensure_entity_map_sql(schema),
            exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema),
            grants = sql::grant_replication_access_sql(db_name, cdc_name, schema),
        )
        .expect("write CDC SQL into a string");
    }
    bundle
}

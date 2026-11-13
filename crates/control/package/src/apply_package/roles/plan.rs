/// PostgreSQL's constraint node names the referenced relation in `pktable`.
pub(super) fn foreign_key_tables(
    value: &serde_json::Value,
    tables: &mut std::collections::BTreeSet<String>,
) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(table) = fields.get("pktable")
                && let (Some(schema), Some(relation)) =
                    (table["schemaname"].as_str(), table["relname"].as_str())
            {
                let relation = postgres_protocol::escape::escape_identifier(relation);
                tables.insert(if schema.is_empty() {
                    relation
                } else {
                    format!(
                        "{}.{relation}",
                        postgres_protocol::escape::escape_identifier(schema)
                    )
                });
            }
            for field in fields.values() {
                foreign_key_tables(field, tables);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                foreign_key_tables(item, tables);
            }
        }
        _ => {}
    }
}

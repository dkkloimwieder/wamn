//! PostgreSQL bind projection for pure schema-control statements.

use tokio_postgres::types::ToSql;
use wamn_schema_control::Value;

pub(crate) fn as_postgres(values: &[Value]) -> Vec<&(dyn ToSql + Sync)> {
    values
        .iter()
        .map(|value| -> &(dyn ToSql + Sync) {
            match value {
                Value::Text(value) => value,
                Value::NullableText(value) => value,
                Value::Int(value) => value,
                Value::NullableInt(value) => value,
                Value::Bool(value) => value,
            }
        })
        .collect()
}

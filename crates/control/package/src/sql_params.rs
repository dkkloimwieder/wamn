//! Native binds for schema-control statements.
use sqlx::{Postgres, postgres::PgArguments, query::Query};
use wamn_schema_control::Value;

pub fn bind<'a>(
    mut query: Query<'a, Postgres, PgArguments>,
    values: &'a [Value],
) -> Query<'a, Postgres, PgArguments> {
    for value in values {
        query = match value {
            Value::Text(value) => query.bind(value),
            Value::Int(value) => query.bind(value),
            Value::NullableInt(value) => query.bind(value),
            Value::Bool(value) => query.bind(value),
        };
    }
    query
}

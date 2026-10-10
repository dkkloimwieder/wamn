//! Statement types from SQLx's PostgreSQL describe path
//! (docs/plan/platform-deploy.md §7.1 step 3).
//!
//! PostgreSQL's RowDescription carries no nullability. SQLx infers it from the
//! catalog and from `EXPLAIN VERBOSE` for outer joins, so this module calls
//! SQLx's describe as a library call and never a raw `PREPARE`. No verifier
//! crate is generated for it.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use sqlx::{
    AssertSqlSafe, Column as _, Connection as _, Either, Executor as _, SqlSafeStr as _,
    TypeInfo as _,
};
use wamn_schema_introspection::ir::ColumnType;

/// What describe says about one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DescribedStatement {
    /// The PostgreSQL type name of each parameter, in `$n` order.
    pub(crate) parameters: Vec<String>,
    /// Each result column, in order.
    pub(crate) columns: Vec<DescribedColumn>,
}

/// One result column of a described statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DescribedColumn {
    pub(crate) name: String,
    /// The PostgreSQL type name.
    pub(crate) type_name: String,
    /// SQLx's answer: `Some(false)` cannot be null, `Some(true)` can be null,
    /// `None` unknown, as for an expression.
    pub(crate) nullable: Option<bool>,
}

/// Describe each statement of `corpus` against the database at `database_url`.
///
/// The keys of `corpus` are statement paths and name each statement in an
/// error. The connection uses the database's own search path.
pub(crate) async fn describe_statements(
    database_url: &str,
    corpus: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, DescribedStatement>> {
    let mut connection = sqlx::PgConnection::connect(database_url)
        .await
        .context("connect to the verification database to describe statements")?;
    let mut described = BTreeMap::new();
    for (path, bytes) in corpus {
        let sql =
            std::str::from_utf8(bytes).with_context(|| format!("statement {path} is not UTF-8"))?;
        let describe = (&mut connection)
            .describe(AssertSqlSafe(sql.to_owned()).into_sql_str())
            .await
            .with_context(|| format!("describe statement {path}"))?;
        let parameters = match describe.parameters {
            Some(Either::Left(types)) => types.iter().map(|ty| ty.name().to_owned()).collect(),
            Some(Either::Right(_)) | None => {
                bail!("describe of statement {path} gave no parameter types")
            }
        };
        let columns = describe
            .columns
            .iter()
            .zip(describe.nullable.iter())
            .map(|(column, nullable)| DescribedColumn {
                name: column.name().to_owned(),
                type_name: column.type_info().name().to_owned(),
                nullable: *nullable,
            })
            .collect();
        described.insert(
            path.clone(),
            DescribedStatement {
                parameters,
                columns,
            },
        );
    }
    connection
        .close()
        .await
        .context("close the describe connection")?;
    Ok(described)
}

/// The transport type of a PostgreSQL type name that SQLx reports, if the
/// manifest vocabulary has one.
pub(crate) fn column_type(postgres_name: &str) -> Option<ColumnType> {
    match postgres_name {
        "BOOL" => Some(ColumnType::Boolean),
        "INT4" => Some(ColumnType::Int32),
        "INT8" => Some(ColumnType::Int64),
        "FLOAT8" => Some(ColumnType::Float64),
        "TEXT" => Some(ColumnType::Text),
        "BYTEA" => Some(ColumnType::Bytes),
        "NUMERIC" => Some(ColumnType::Numeric),
        "TIMESTAMPTZ" => Some(ColumnType::Timestamptz),
        "JSON" | "JSONB" => Some(ColumnType::Json),
        "UUID" => Some(ColumnType::Uuid),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_manifest_type_has_one_postgres_name() {
        for (name, expected) in [
            ("BOOL", ColumnType::Boolean),
            ("INT4", ColumnType::Int32),
            ("INT8", ColumnType::Int64),
            ("FLOAT8", ColumnType::Float64),
            ("TEXT", ColumnType::Text),
            ("BYTEA", ColumnType::Bytes),
            ("NUMERIC", ColumnType::Numeric),
            ("TIMESTAMPTZ", ColumnType::Timestamptz),
            ("JSONB", ColumnType::Json),
            ("UUID", ColumnType::Uuid),
        ] {
            assert_eq!(column_type(name), Some(expected));
        }
        assert_eq!(column_type("INT2"), None);
    }
}

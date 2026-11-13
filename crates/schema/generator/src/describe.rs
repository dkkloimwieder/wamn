//! Statement types from SQLx's PostgreSQL describe path
//! (docs/operations/building.md#wamn-build).
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
    /// The prepared column's relation OID names a view.
    pub(crate) view_column: bool,
}

/// Describe each statement of `corpus` against the database at `database_url`.
///
/// The keys of `corpus` are statement paths and name each statement in an
/// error. The connection uses the database's own search path.
pub(crate) async fn describe_statements<'a>(
    database_url: &str,
    corpus: impl IntoIterator<Item = (&'a String, &'a Vec<u8>)>,
) -> Result<BTreeMap<String, DescribedStatement>> {
    let mut connection = sqlx::PgConnection::connect(database_url)
        .await
        .context("connect to the verification database to describe statements")?;
    let views: Vec<sqlx::postgres::types::Oid> = sqlx::query_scalar(
        AssertSqlSafe(
            "SELECT c.oid FROM pg_catalog.pg_class c \
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            JOIN pg_catalog.pg_views v ON v.schemaname = n.nspname AND v.viewname = c.relname"
                .to_owned(),
        )
        .into_sql_str(),
    )
    .fetch_all(&mut connection)
    .await
    .context("read view identities for statement descriptions")?;
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
                view_column: column.relation_id().is_some_and(|id| views.contains(&id)),
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

pub(crate) use wamn_scalar::postgres_type as column_type;

/// Compare the accessor's value shape with PostgreSQL's description.
pub(crate) fn validate_contract(
    path: &str,
    description: &DescribedStatement,
    binds: &[crate::generate::StatementValueContract],
    columns: &[crate::generate::StatementValueContract],
) -> Result<(), crate::ManifestError> {
    let valid = description.parameters.len() == binds.len()
        && description.columns.len() == columns.len()
        && description
            .parameters
            .iter()
            .zip(binds)
            .all(|(name, bind)| column_type(name) == Some(bind.ty))
        && description
            .columns
            .iter()
            .zip(columns)
            .all(|(actual, expected)| {
                actual.name == expected.name
                    && column_type(&actual.type_name) == Some(expected.ty)
                    && (expected.nullable || actual.nullable != Some(true) || actual.view_column)
            });
    if valid {
        return Ok(());
    }
    Err(crate::ManifestError::for_path(
        crate::ManifestErrorType::InvalidOperation,
        format!(
            "{path}: SQLx description does not fit the accessor's parameters and selected columns"
        ),
        path,
    ))
}

//! Plain statement descriptions used to check generated accessors.

use anyhow::{Result, ensure};

/// What describe says about one statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescribedStatement {
    /// The PostgreSQL type name of each parameter, in `$n` order.
    pub parameters: Vec<String>,
    /// Each result column, in order.
    pub columns: Vec<DescribedColumn>,
}

/// One result column of a described statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescribedColumn {
    pub name: String,
    /// The PostgreSQL type name.
    pub type_name: String,
    /// SQLx's answer: `Some(false)` cannot be null, `Some(true)` can be null,
    /// `None` unknown, as for an expression.
    pub nullable: Option<bool>,
    /// The prepared column's relation OID names a view.
    pub view_column: bool,
}

impl DescribedColumn {
    /// SQLx aliases can state only nullability. The SQL type comes from PostgreSQL.
    pub(crate) fn contract(&self) -> Result<(&str, Option<bool>)> {
        let name = column_name(&self.name);
        let nullable = match self.name.as_bytes().last() {
            Some(b'!') => {
                ensure!(
                    self.nullable != Some(true) || self.view_column,
                    "SQLx describes a nullable column"
                );
                Some(false)
            }
            Some(b'?') => Some(true),
            _ => self.nullable,
        };
        ensure!(
            !name.contains([':', '!', '?']),
            "use only SQLx ! or ? suffixes for nullability"
        );
        Ok((name, nullable))
    }
}

pub(crate) fn column_name(alias: &str) -> &str {
    alias.strip_suffix(['!', '?']).unwrap_or(alias)
}

pub(crate) use wamn_scalar::postgres_type as column_type;
pub(crate) use wamn_scalar::postgres_type as result_type;

/// Compare the accessor's value shape with PostgreSQL's description.
pub(crate) fn validate_contract(
    path: &str,
    description: &DescribedStatement,
    binds: &[crate::generate::StatementValueContract],
    columns: &[crate::generate::StatementValueContract],
) -> Result<(), wamn_package::manifest::ManifestError> {
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
                actual.contract().is_ok_and(|(name, nullable)| {
                    name == expected.name
                        && result_type(&actual.type_name) == Some(expected.ty)
                        && (expected.nullable || nullable != Some(true) || actual.view_column)
                })
            });
    if valid {
        return Ok(());
    }
    Err(wamn_package::manifest::ManifestError::for_path(
        wamn_package::manifest::ManifestErrorType::InvalidOperation,
        format!(
            "{path}: SQLx description does not fit the accessor's parameters and selected columns"
        ),
        path,
    ))
}

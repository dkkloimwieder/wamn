//! Database evidence supplied by the caller during package verification.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::Result;
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::source_name::SourceName;
use wamn_schema::ir::CatalogIr;

use crate::AuthoredSql;

pub use crate::describe::{DescribedColumn, DescribedStatement};
pub use crate::history::RelationColumn;

/// One resolved migration source.
#[derive(Debug)]
pub struct Migration {
    pub version: i64,
    pub up: bool,
    pub sql: String,
}

/// An isolated database on the verification server.
pub struct VerificationDatabase {
    pub name: String,
    pub url: String,
}

impl fmt::Debug for VerificationDatabase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationDatabase")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A history table and the SQL that creates it.
#[derive(Debug)]
pub struct HistoryTable<'a> {
    pub schema: &'a str,
    pub table: &'a str,
    pub sql: String,
}

/// Resolve migration files and obtain live PostgreSQL facts for a package build.
#[async_trait::async_trait]
pub trait BuildDatabase: Sync {
    async fn migration_directory(&self, path: &Path) -> Result<Vec<Migration>>;

    async fn package_migrations(&self, root: &Path) -> Result<Vec<Migration>>;

    async fn create_verification_database(&self, server: &str) -> Result<VerificationDatabase>;

    async fn drop_verification_database(&self, server: &str, name: &str) -> Result<()>;

    async fn server_version(&self, url: &str) -> Result<u32>;

    async fn migrate(
        &self,
        url: &str,
        schemas: &[String],
        directories: &[&Path],
        history: &[HistoryTable<'_>],
    ) -> Result<()>;

    async fn describe_statements(
        &self,
        url: &str,
        statements: &[AuthoredSql<'_>],
    ) -> Result<BTreeMap<String, DescribedStatement>>;

    async fn read_catalog(
        &self,
        url: &str,
        schemas: &[&str],
        selected: &[String],
    ) -> Result<CatalogIr>;

    async fn ordered_columns(
        &self,
        url: &str,
        manifest: &PackageManifest,
    ) -> Result<BTreeMap<SourceName, Vec<RelationColumn>>>;
}

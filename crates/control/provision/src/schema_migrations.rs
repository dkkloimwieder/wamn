//! Platform migration records and upgrade lists.
//!
//! The full schema files own the schemas and build fresh installations.
//! The migration lists stay empty until development is stable. Real migrations
//! can then start at 0001, using the existing record tables and upgrade machinery.

use sha2::{Digest as _, Sha256};

/// One migration file, embedded in the binary.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The path under `deploy/sql`, such as `migrations/system/0001_example.sql`.
    pub relative_path: &'static str,
    /// The file's SQL. It has no `BEGIN` or `COMMIT` of its own.
    pub sql: &'static str,
}

impl Migration {
    /// The ordinal of the file name, such as 1 for `0001_example.sql`.
    pub fn ordinal(&self) -> i32 {
        let name = self
            .relative_path
            .rsplit('/')
            .next()
            .expect("a path has a file name");
        name[..4]
            .parse()
            .expect("a migration name starts with four digits")
    }

    /// `sha256:<hex>` of the file's bytes.
    pub fn sha256(&self) -> String {
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(self.sql.as_bytes()))
        )
    }
}

/// Which database a migration list belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationTarget {
    /// The system database `wamn_system`, recorded in `registry.schema_migrations`.
    System,
    /// A project-environment database, recorded in `platform.schema_migrations`.
    Project,
}

impl MigrationTarget {
    /// The files of this target, in order.
    pub fn migrations(self) -> &'static [Migration] {
        match self {
            Self::System => SYSTEM_MIGRATIONS,
            Self::Project => PROJECT_MIGRATIONS,
        }
    }

    /// The record table of this target.
    pub fn record_table(self) -> &'static str {
        match self {
            Self::System => "registry.schema_migrations",
            Self::Project => "platform.schema_migrations",
        }
    }

    /// The statement that creates the record table. The full schema files
    /// carry the same text.
    pub fn record_table_sql(self) -> &'static str {
        match self {
            Self::System => SYSTEM_RECORD_TABLE_SQL,
            Self::Project => PROJECT_RECORD_TABLE_SQL,
        }
    }

    /// The statement that records one file: `$1` ordinal, `$2` path, `$3` sha256.
    pub fn record_sql(self) -> String {
        format!(
            "INSERT INTO {} (ordinal, relative_path, sha256) VALUES ($1, $2, $3)",
            self.record_table()
        )
    }
}

/// The record table of the system database.
pub const SYSTEM_RECORD_TABLE_SQL: &str =
    include_str!("../../../../deploy/sql/system-schema-migrations.sql");

/// The record table of a project-environment database.
pub const PROJECT_RECORD_TABLE_SQL: &str =
    include_str!("../../../../deploy/sql/app-schema-migrations.sql");

/// System migrations, empty until development is stable.
pub const SYSTEM_MIGRATIONS: &[Migration] = &[];

/// Project migrations, empty until development is stable.
pub const PROJECT_MIGRATIONS: &[Migration] = &[];

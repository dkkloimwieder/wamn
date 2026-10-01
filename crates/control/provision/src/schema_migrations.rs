//! Numbered platform migrations of `docs/plan/schema-upgrade.md`.
//!
//! A platform schema change goes into the full schema files and into a new
//! file under `deploy/sql/migrations/system/` or `deploy/sql/migrations/project/`
//! in the same commit. A fresh install records every file as applied, because
//! the full files already hold them. `upgrade-schema` applies and records the
//! rest. Each list below names every file of its directory, in order, and a
//! test compares the list with the directory.

use sha2::{Digest as _, Sha256};

/// One migration file, embedded in the binary.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The path under `deploy/sql`, such as `migrations/system/0001_capture_gap.sql`.
    pub relative_path: &'static str,
    /// The file's SQL. It has no `BEGIN` or `COMMIT` of its own.
    pub sql: &'static str,
}

impl Migration {
    /// The ordinal of the file name, such as 1 for `0001_capture_gap.sql`.
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
    /// A project-environment database, recorded in `app_system.schema_migrations`.
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
            Self::Project => "app_system.schema_migrations",
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

/// Every file of `deploy/sql/migrations/system/`, in order.
pub const SYSTEM_MIGRATIONS: &[Migration] = &[
    Migration {
        relative_path: "migrations/system/0001_capture_gap.sql",
        sql: include_str!("../../../../deploy/sql/migrations/system/0001_capture_gap.sql"),
    },
    Migration {
        relative_path: "migrations/system/0002_event_reader_schema.sql",
        sql: include_str!("../../../../deploy/sql/migrations/system/0002_event_reader_schema.sql"),
    },
    Migration {
        relative_path: "migrations/system/0003_kind_to_type.sql",
        sql: include_str!("../../../../deploy/sql/migrations/system/0003_kind_to_type.sql"),
    },
    Migration {
        relative_path: "migrations/system/0004_env_policy_durability.sql",
        sql: include_str!(
            "../../../../deploy/sql/migrations/system/0004_env_policy_durability.sql"
        ),
    },
];

/// Every file of `deploy/sql/migrations/project/`, in order.
pub const PROJECT_MIGRATIONS: &[Migration] = &[Migration {
    relative_path: "migrations/project/0001_kind_to_type.sql",
    sql: include_str!("../../../../deploy/sql/migrations/project/0001_kind_to_type.sql"),
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn directory_files(target: &str) -> Vec<String> {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/sql/migrations")
            .join(target);
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut files: Vec<String> = entries
            .map(|entry| {
                format!(
                    "migrations/{target}/{}",
                    entry.unwrap().file_name().to_str().unwrap()
                )
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn each_list_names_every_file_of_its_directory_in_order() {
        for (target, name) in [
            (MigrationTarget::System, "system"),
            (MigrationTarget::Project, "project"),
        ] {
            let listed: Vec<String> = target
                .migrations()
                .iter()
                .map(|migration| migration.relative_path.to_owned())
                .collect();
            assert_eq!(listed, directory_files(name), "{name} migrations");
            for (index, migration) in target.migrations().iter().enumerate() {
                assert_eq!(
                    migration.ordinal(),
                    i32::try_from(index + 1).unwrap(),
                    "{} is out of order",
                    migration.relative_path
                );
                let on_disk = std::fs::read_to_string(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../deploy/sql")
                        .join(migration.relative_path),
                )
                .unwrap();
                assert_eq!(on_disk, migration.sql, "{}", migration.relative_path);
            }
        }
    }
}

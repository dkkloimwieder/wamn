//! Ordered, immutable migration prefix planning shared by every SQL target.

use crate::package_migrations::{PackageMigrationError, PackageMigrationErrorType};
use crate::{
    MigrationOutcome, MigrationSource, PACKAGE_MIGRATION_DRIFT_REFUSAL,
    PACKAGE_MIGRATION_DUPLICATE_REFUSAL, PACKAGE_MIGRATION_GAP_REFUSAL, PendingMigration,
    RecordedMigration, SqlStatement,
};
use std::collections::BTreeSet;
use wamn_catalog::PackageCoordinate;
use wamn_digest::bytes_sha256;

/// Pending files and exact SQL, in matching order.
#[derive(Debug)]
pub struct MigrationPlan {
    pub pending: Vec<PendingMigration>,
    pub statements: Vec<SqlStatement>,
}

/// Validate all files before any SQL can execute.
///
/// `prefix` includes the trailing slash. Empty platform directories are valid.
/// Package callers enforce their manifest's initial-file requirement separately.
pub fn plan_migrations(
    sources: &[MigrationSource],
    recorded: &[RecordedMigration],
    prefix: &str,
    coordinate: Option<&PackageCoordinate>,
) -> Result<MigrationPlan, PackageMigrationError> {
    let migrations = normalized_migrations(sources, prefix)?;
    validate_recorded_prefix(coordinate, recorded, &migrations)?;
    let completed = recorded
        .iter()
        .take_while(|row| row.outcome != MigrationOutcome::Started)
        .count();
    let mut statements = Vec::with_capacity(migrations.len() - completed);
    for migration in migrations.iter().skip(completed) {
        let sql = std::str::from_utf8(&migration.source.bytes).map_err(|source| {
            PackageMigrationError::with_source(
                PackageMigrationErrorType::InvalidDirectory,
                "migration SQL is not UTF-8",
                source,
            )
            .at_path(&migration.source.relative_path)
        })?;
        statements.push(SqlStatement {
            summary: format!("apply {}", migration.source.relative_path),
            sql: sql.into(),
            params: Vec::new(),
        });
    }
    let pending = migrations
        .iter()
        .skip(completed)
        .map(|migration| PendingMigration {
            ordinal: migration.ordinal,
            relative_path: migration.source.relative_path.clone(),
            sha256: migration.sha256.clone(),
            transactional: wamn_catalog::manifest::migration_is_transactional(
                &migration.source.bytes,
            ),
            started: recorded.iter().any(|row| {
                row.ordinal == migration.ordinal && row.outcome == MigrationOutcome::Started
            }),
        })
        .collect();
    Ok(MigrationPlan {
        pending,
        statements,
    })
}

/// Refuse unsupported file execution before the first mutation of a batch.
pub fn validate_execution_sql(
    file: &PendingMigration,
    statement: &SqlStatement,
) -> Result<(), PackageMigrationError> {
    let parsed = pg_query::parse(&statement.sql).map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorType::InvalidDirectory,
            "migration SQL cannot be parsed",
            source,
        )
        .at_path(&file.relative_path)
    })?;
    if !wamn_catalog::manifest::migration_statement_count_is_safe(
        statement.sql.as_bytes(),
        parsed.protobuf.stmts.len(),
    ) {
        return Err(PackageMigrationError::new(
            PackageMigrationErrorType::InvalidDirectory,
            "a nontransactional file must contain exactly one SQL statement",
        )
        .at_path(&file.relative_path));
    }
    // Transaction control would separate a file's effects from its record.
    if parsed.protobuf.stmts.iter().any(|raw| {
        matches!(
            raw.stmt.as_ref().and_then(|stmt| stmt.node.as_ref()),
            Some(pg_query::NodeEnum::TransactionStmt(_))
        )
    }) {
        return Err(PackageMigrationError::new(
            PackageMigrationErrorType::InvalidDirectory,
            "migration SQL cannot control transactions",
        )
        .at_path(&file.relative_path));
    }
    Ok(())
}

struct NormalizedMigration<'a> {
    ordinal: u32,
    source: &'a MigrationSource,
    sha256: String,
}

fn normalized_migrations<'a>(
    sources: &'a [MigrationSource],
    prefix: &str,
) -> Result<Vec<NormalizedMigration<'a>>, PackageMigrationError> {
    let mut migrations = sources
        .iter()
        .map(|source| {
            let ordinal = migration_ordinal(&source.relative_path, prefix)?;
            Ok(NormalizedMigration {
                ordinal,
                source,
                sha256: bytes_sha256(&source.bytes),
            })
        })
        .collect::<Result<Vec<_>, PackageMigrationError>>()?;
    migrations.sort_by(|left, right| {
        left.ordinal
            .cmp(&right.ordinal)
            .then_with(|| left.source.relative_path.cmp(&right.source.relative_path))
    });

    let mut paths = BTreeSet::new();
    for (index, migration) in migrations.iter().enumerate() {
        if !paths.insert(migration.source.relative_path.as_str())
            || (index > 0 && migrations[index - 1].ordinal == migration.ordinal)
        {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::Duplicate,
                format!(
                    "{PACKAGE_MIGRATION_DUPLICATE_REFUSAL}: ordinal or path occurs more than once"
                ),
            )
            .at_path(&migration.source.relative_path));
        }
        let expected = u32::try_from(index + 1).expect("migration count fits u32");
        if migration.ordinal != expected {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::Gap,
                format!(
                    "{PACKAGE_MIGRATION_GAP_REFUSAL}: expected ordinal {expected:04}, found {:04}",
                    migration.ordinal
                ),
            )
            .at_path(&migration.source.relative_path));
        }
    }
    Ok(migrations)
}

fn migration_ordinal(path: &str, prefix: &str) -> Result<u32, PackageMigrationError> {
    let Some(file_name) = path.strip_prefix(prefix) else {
        return Err(invalid_path(path, prefix));
    };
    let Some(stem) = file_name.strip_suffix(".sql") else {
        return Err(invalid_path(path, prefix));
    };
    let Some((digits, name)) = stem.split_once('_') else {
        return Err(invalid_path(path, prefix));
    };
    if digits.len() != 4
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid_path(path, prefix));
    }
    digits.parse::<u32>().map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorType::InvalidDirectory,
            "migration ordinal is invalid",
            source,
        )
        .at_path(path)
    })
}

fn invalid_path(path: &str, prefix: &str) -> PackageMigrationError {
    PackageMigrationError::new(
        PackageMigrationErrorType::InvalidDirectory,
        format!("migration path must be {prefix}NNNN_snake_case.sql"),
    )
    .at_path(path)
}

fn validate_recorded_prefix(
    coordinate: Option<&PackageCoordinate>,
    recorded: &[RecordedMigration],
    actual: &[NormalizedMigration<'_>],
) -> Result<(), PackageMigrationError> {
    let mut paths = BTreeSet::new();
    for (index, row) in recorded.iter().enumerate() {
        if row.outcome == MigrationOutcome::Started && index + 1 != recorded.len() {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::InvalidDirectory,
                "an unfinished migration must be the last recorded file",
            )
            .at_path(&row.relative_path));
        }
        if !paths.insert(row.relative_path.as_str())
            || (index > 0 && recorded[index - 1].ordinal == row.ordinal)
        {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::Duplicate,
                format!(
                    "{PACKAGE_MIGRATION_DUPLICATE_REFUSAL}: recorded ordinal or path occurs more than once"
                ),
            )
            .at_path(&row.relative_path));
        }
        let expected = u32::try_from(index + 1).expect("migration count fits u32");
        if row.ordinal != expected {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::Gap,
                format!(
                    "{PACKAGE_MIGRATION_GAP_REFUSAL}: migration records expected ordinal {expected:04}, found {:04}",
                    row.ordinal
                ),
            )
            .at_path(&row.relative_path));
        }
    }
    if recorded.len() > actual.len() {
        let missing = &recorded[actual.len()];
        return Err(PackageMigrationError::drift(
            PackageMigrationErrorType::MigrationDrift,
            PACKAGE_MIGRATION_DRIFT_REFUSAL,
            coordinate,
            &missing.relative_path,
            &missing.sha256,
            "missing",
        ));
    }
    for (index, row) in recorded.iter().enumerate() {
        let migration = &actual[index];
        if row.ordinal != migration.ordinal
            || row.relative_path != migration.source.relative_path
            || row.sha256 != migration.sha256
        {
            return Err(PackageMigrationError::drift(
                PackageMigrationErrorType::MigrationDrift,
                PACKAGE_MIGRATION_DRIFT_REFUSAL,
                coordinate,
                &row.relative_path,
                &row.sha256,
                &migration.sha256,
            ));
        }
        if row.outcome == MigrationOutcome::Started
            && wamn_catalog::manifest::migration_is_transactional(&migration.source.bytes)
        {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorType::InvalidDirectory,
                "a transactional migration cannot have a started outcome",
            )
            .at_path(&row.relative_path));
        }
    }
    Ok(())
}

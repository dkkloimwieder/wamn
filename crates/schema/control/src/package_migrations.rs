//! Pure package-directory migration planning.
//!
//! The effect shell supplies the exact `wamn.json` bytes, every file under the
//! package-owned `migrations/` directory, and the immutable rows already stored
//! for the package coordinate. This module validates the complete prefix and
//! returns one ordered transaction body; it performs no filesystem or database
//! I/O.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use sha2::{Digest as _, Sha256};
use wamn_catalog::PackageCoordinate;
use wamn_schema_generator::{PackageManifest, validate_operation_vocabulary};

use crate::{SqlStatement, Value};

pub const PACKAGE_MANIFEST_PATH: &str = "wamn.json";
pub const PACKAGE_MANIFEST_DRIFT_REFUSAL: &str = "package-manifest-drift";
pub const PACKAGE_MIGRATION_DRIFT_REFUSAL: &str = "package-migration-drift";
pub const PACKAGE_MIGRATION_DUPLICATE_REFUSAL: &str = "package-migration-duplicate";
pub const PACKAGE_MIGRATION_GAP_REFUSAL: &str = "package-migration-gap";
pub const PREDECESSOR_PREFIX_MISMATCH_REFUSAL: &str = "predecessor-prefix-mismatch";

/// One exact migration file discovered under a package directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSource {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// One immutable applied migration ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMigration {
    pub ordinal: u32,
    pub relative_path: String,
    pub sha256: String,
}

/// Existing immutable state for one package coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPackage {
    pub coordinate: PackageCoordinate,
    pub predecessor_version: Option<String>,
    pub manifest_sha256: String,
    pub migrations: Vec<RecordedMigration>,
}

/// Exact package-directory bytes supplied by the effect shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDirectory {
    pub manifest_bytes: Vec<u8>,
    pub migrations: Vec<MigrationSource>,
}

/// Sole package-local model-key to physical relation mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedModel {
    pub model_id: String,
    pub schema: String,
    pub table: String,
}

/// One validated pending file and its immutable ledger identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMigration {
    pub ordinal: u32,
    pub relative_path: String,
    pub sha256: String,
}

/// Complete ordered body for one database transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMigrationPlan {
    pub coordinate: PackageCoordinate,
    pub predecessor_version: Option<String>,
    pub manifest_sha256: String,
    pub models: Vec<ManagedModel>,
    pub verified_prefix: Vec<PendingMigration>,
    pub pending: Vec<PendingMigration>,
    pub statements: Vec<SqlStatement>,
}

impl PackageMigrationPlan {
    /// A converged package executes no root, ledger, or migration writes.
    pub fn is_noop(&self) -> bool {
        self.statements.is_empty()
    }
}

/// Stable package migration refusal class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageMigrationErrorKind {
    InvalidManifest,
    InvalidDirectory,
    ManifestDrift,
    Duplicate,
    Gap,
    MigrationDrift,
    PredecessorPrefixMismatch,
}

impl PackageMigrationErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidManifest => "invalid-manifest",
            Self::InvalidDirectory => "invalid-directory",
            Self::ManifestDrift => PACKAGE_MANIFEST_DRIFT_REFUSAL,
            Self::Duplicate => PACKAGE_MIGRATION_DUPLICATE_REFUSAL,
            Self::Gap => PACKAGE_MIGRATION_GAP_REFUSAL,
            Self::MigrationDrift => PACKAGE_MIGRATION_DRIFT_REFUSAL,
            Self::PredecessorPrefixMismatch => PREDECESSOR_PREFIX_MISMATCH_REFUSAL,
        }
    }
}

/// Contextual package migration refusal.
#[derive(Debug)]
pub struct PackageMigrationError {
    kind: PackageMigrationErrorKind,
    context: String,
    coordinate: Option<String>,
    path: Option<String>,
    recorded_hash: Option<String>,
    actual_hash: Option<String>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl PackageMigrationError {
    pub const fn kind(&self) -> PackageMigrationErrorKind {
        self.kind
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn coordinate(&self) -> Option<&str> {
        self.coordinate.as_deref()
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn recorded_hash(&self) -> Option<&str> {
        self.recorded_hash.as_deref()
    }

    pub fn actual_hash(&self) -> Option<&str> {
        self.actual_hash.as_deref()
    }

    fn new(kind: PackageMigrationErrorKind, context: impl Into<String>) -> Self {
        Self {
            kind,
            context: context.into(),
            coordinate: None,
            path: None,
            recorded_hash: None,
            actual_hash: None,
            source: None,
        }
    }

    fn with_source(
        kind: PackageMigrationErrorKind,
        context: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: Some(Box::new(source)),
            ..Self::new(kind, context)
        }
    }

    fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn drift(
        kind: PackageMigrationErrorKind,
        context: impl Into<String>,
        coordinate: &PackageCoordinate,
        path: impl Into<String>,
        recorded_hash: impl Into<String>,
        actual_hash: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            coordinate: Some(coordinate_text(coordinate)),
            path: Some(path.into()),
            recorded_hash: Some(recorded_hash.into()),
            actual_hash: Some(actual_hash.into()),
            source: None,
        }
    }
}

impl fmt::Display for PackageMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.context)?;
        if let Some(coordinate) = &self.coordinate {
            write!(formatter, "; coordinate={coordinate}")?;
        }
        if let Some(path) = &self.path {
            write!(formatter, "; file={path}")?;
        }
        if let Some(recorded) = &self.recorded_hash {
            write!(formatter, "; recorded-sha256={recorded}")?;
        }
        if let Some(actual) = &self.actual_hash {
            write!(formatter, "; actual-sha256={actual}")?;
        }
        Ok(())
    }
}

impl Error for PackageMigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Validate a package directory against its immutable applied prefix.
pub fn plan_package_migrations(
    directory: &PackageDirectory,
    applied: Option<&AppliedPackage>,
) -> Result<PackageMigrationPlan, PackageMigrationError> {
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes).map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorKind::InvalidManifest,
            "wamn.json does not match the strict package manifest",
            source,
        )
        .at_path(PACKAGE_MANIFEST_PATH)
    })?;
    validate_operation_vocabulary(&manifest).map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorKind::InvalidManifest,
            "wamn.json has an invalid semantic manifest vocabulary",
            source,
        )
        .at_path(PACKAGE_MANIFEST_PATH)
    })?;
    let coordinate = PackageCoordinate::new(&manifest.package.id, &manifest.package.version)
        .map_err(|source| {
            PackageMigrationError::with_source(
                PackageMigrationErrorKind::InvalidManifest,
                "wamn.json package coordinate is invalid",
                source,
            )
            .at_path(PACKAGE_MANIFEST_PATH)
        })?;
    if let Some(predecessor) = manifest.package.predecessor_version.as_deref() {
        PackageCoordinate::new(&manifest.package.id, predecessor).map_err(|source| {
            PackageMigrationError::with_source(
                PackageMigrationErrorKind::InvalidManifest,
                "wamn.json predecessor package coordinate is invalid",
                source,
            )
            .at_path(PACKAGE_MANIFEST_PATH)
        })?;
    }
    let predecessor_version = manifest.package.predecessor_version.clone();
    let manifest_sha256 = sha256(&directory.manifest_bytes);

    if let Some(existing) = applied {
        if existing.coordinate != coordinate {
            return plan_package_migrations_from_predecessor(directory, existing);
        }
        if existing.manifest_sha256 != manifest_sha256 {
            return Err(PackageMigrationError::drift(
                PackageMigrationErrorKind::ManifestDrift,
                PACKAGE_MANIFEST_DRIFT_REFUSAL,
                &coordinate,
                PACKAGE_MANIFEST_PATH,
                &existing.manifest_sha256,
                &manifest_sha256,
            ));
        }
    }

    let migrations = normalized_migrations(&directory.migrations)?;
    let recorded = applied.map_or(&[][..], |state| state.migrations.as_slice());
    validate_recorded_prefix(&coordinate, recorded, &migrations)?;

    let mut models = Vec::with_capacity(manifest.models.len());
    for (model_id, model) in manifest.models {
        for (field, value) in [
            ("model-id", model_id.as_str()),
            ("schema", model.schema.as_str()),
            ("table", model.table.as_str()),
        ] {
            if !snake_identifier(value) {
                return Err(PackageMigrationError::new(
                    PackageMigrationErrorKind::InvalidManifest,
                    format!("wamn.json {field} {value:?} is not singular snake_case"),
                )
                .at_path(PACKAGE_MANIFEST_PATH));
            }
        }
        models.push(ManagedModel {
            model_id,
            schema: model.schema,
            table: model.table,
        });
    }
    let pending_sources = &migrations[recorded.len()..];
    let pending = pending_sources
        .iter()
        .map(|migration| PendingMigration {
            ordinal: migration.ordinal,
            relative_path: migration.source.relative_path.clone(),
            sha256: migration.sha256.clone(),
        })
        .collect::<Vec<_>>();
    let statements = transaction_statements(
        &coordinate,
        predecessor_version.as_deref(),
        &manifest_sha256,
        applied.is_none(),
        &[],
        pending_sources,
    )?;

    Ok(PackageMigrationPlan {
        coordinate,
        predecessor_version,
        manifest_sha256,
        models,
        verified_prefix: Vec::new(),
        pending,
        statements,
    })
}

/// Plan a cumulative new coordinate from exactly its declared predecessor.
fn plan_package_migrations_from_predecessor(
    directory: &PackageDirectory,
    predecessor: &AppliedPackage,
) -> Result<PackageMigrationPlan, PackageMigrationError> {
    let fresh = plan_package_migrations(directory, None)?;
    let migrations = normalized_migrations(&directory.migrations)?;
    let first_path = migrations
        .first()
        .expect("normalized package migration streams are nonempty")
        .source
        .relative_path
        .clone();
    let declared_predecessor = fresh.predecessor_version.as_deref();
    if predecessor.coordinate.package_id() != fresh.coordinate.package_id()
        || declared_predecessor != Some(predecessor.coordinate.package_version())
    {
        return Err(PackageMigrationError {
            kind: PackageMigrationErrorKind::PredecessorPrefixMismatch,
            context: format!(
                "{PREDECESSOR_PREFIX_MISMATCH_REFUSAL}: applied coordinate {} is not the declared predecessor of {}",
                coordinate_text(&predecessor.coordinate),
                coordinate_text(&fresh.coordinate)
            ),
            coordinate: Some(coordinate_text(&fresh.coordinate)),
            path: Some(first_path),
            recorded_hash: None,
            actual_hash: None,
            source: None,
        });
    }
    if predecessor.migrations.is_empty() {
        return Err(PackageMigrationError {
            kind: PackageMigrationErrorKind::PredecessorPrefixMismatch,
            context: format!(
                "{PREDECESSOR_PREFIX_MISMATCH_REFUSAL}: declared predecessor has no applied migration prefix"
            ),
            coordinate: Some(coordinate_text(&fresh.coordinate)),
            path: Some(first_path),
            recorded_hash: Some("missing".into()),
            actual_hash: Some(migrations[0].sha256.clone()),
            source: None,
        });
    }
    validate_recorded_prefix(&predecessor.coordinate, &predecessor.migrations, &migrations)
        .map_err(|source| {
            let path = source.path.clone().or_else(|| Some(first_path.clone()));
            let recorded_hash = source.recorded_hash.clone();
            let actual_hash = source.actual_hash.clone();
            PackageMigrationError {
                kind: PackageMigrationErrorKind::PredecessorPrefixMismatch,
                context: format!(
                    "{PREDECESSOR_PREFIX_MISMATCH_REFUSAL}: {} does not equal the cumulative prefix of {}",
                    coordinate_text(&predecessor.coordinate),
                    coordinate_text(&fresh.coordinate)
                ),
                coordinate: Some(coordinate_text(&fresh.coordinate)),
                path,
                recorded_hash,
                actual_hash,
                source: Some(Box::new(source)),
            }
        })?;

    let prefix_len = predecessor.migrations.len();
    let prefix_sources = &migrations[..prefix_len];
    let pending_sources = &migrations[prefix_len..];
    let verified_prefix = prefix_sources.iter().map(migration_identity).collect();
    let pending = pending_sources.iter().map(migration_identity).collect();
    let statements = transaction_statements(
        &fresh.coordinate,
        fresh.predecessor_version.as_deref(),
        &fresh.manifest_sha256,
        true,
        prefix_sources,
        pending_sources,
    )?;

    Ok(PackageMigrationPlan {
        coordinate: fresh.coordinate,
        predecessor_version: fresh.predecessor_version,
        manifest_sha256: fresh.manifest_sha256,
        models: fresh.models,
        verified_prefix,
        pending,
        statements,
    })
}

fn migration_identity(migration: &NormalizedMigration<'_>) -> PendingMigration {
    PendingMigration {
        ordinal: migration.ordinal,
        relative_path: migration.source.relative_path.clone(),
        sha256: migration.sha256.clone(),
    }
}

struct NormalizedMigration<'a> {
    ordinal: u32,
    source: &'a MigrationSource,
    sha256: String,
}

fn normalized_migrations(
    sources: &[MigrationSource],
) -> Result<Vec<NormalizedMigration<'_>>, PackageMigrationError> {
    if sources.is_empty() {
        return Err(PackageMigrationError::new(
            PackageMigrationErrorKind::Gap,
            format!("{PACKAGE_MIGRATION_GAP_REFUSAL}: package has no 0001 migration"),
        ));
    }
    let mut migrations = sources
        .iter()
        .map(|source| {
            let ordinal = migration_ordinal(&source.relative_path)?;
            Ok(NormalizedMigration {
                ordinal,
                source,
                sha256: sha256(&source.bytes),
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
                PackageMigrationErrorKind::Duplicate,
                format!(
                    "{PACKAGE_MIGRATION_DUPLICATE_REFUSAL}: ordinal or path occurs more than once"
                ),
            )
            .at_path(&migration.source.relative_path));
        }
        let expected = u32::try_from(index + 1).expect("migration count fits u32");
        if migration.ordinal != expected {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorKind::Gap,
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

fn migration_ordinal(path: &str) -> Result<u32, PackageMigrationError> {
    let Some(file_name) = path.strip_prefix("migrations/") else {
        return Err(invalid_path(path));
    };
    let Some(stem) = file_name.strip_suffix(".sql") else {
        return Err(invalid_path(path));
    };
    let Some((digits, name)) = stem.split_once('_') else {
        return Err(invalid_path(path));
    };
    if digits.len() != 4
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid_path(path));
    }
    digits.parse::<u32>().map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorKind::InvalidDirectory,
            "migration ordinal is invalid",
            source,
        )
        .at_path(path)
    })
}

fn invalid_path(path: &str) -> PackageMigrationError {
    PackageMigrationError::new(
        PackageMigrationErrorKind::InvalidDirectory,
        "migration path must be migrations/NNNN_snake_case.sql",
    )
    .at_path(path)
}

fn validate_recorded_prefix(
    coordinate: &PackageCoordinate,
    recorded: &[RecordedMigration],
    actual: &[NormalizedMigration<'_>],
) -> Result<(), PackageMigrationError> {
    let mut paths = BTreeSet::new();
    for (index, row) in recorded.iter().enumerate() {
        if !paths.insert(row.relative_path.as_str())
            || (index > 0 && recorded[index - 1].ordinal == row.ordinal)
        {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorKind::Duplicate,
                format!(
                    "{PACKAGE_MIGRATION_DUPLICATE_REFUSAL}: recorded ordinal or path occurs more than once"
                ),
            )
            .at_path(&row.relative_path));
        }
        let expected = u32::try_from(index + 1).expect("migration count fits u32");
        if row.ordinal != expected {
            return Err(PackageMigrationError::new(
                PackageMigrationErrorKind::Gap,
                format!(
                    "{PACKAGE_MIGRATION_GAP_REFUSAL}: recorded ledger expected ordinal {expected:04}, found {:04}",
                    row.ordinal
                ),
            )
            .at_path(&row.relative_path));
        }
    }
    if recorded.len() > actual.len() {
        let missing = &recorded[actual.len()];
        return Err(PackageMigrationError::drift(
            PackageMigrationErrorKind::MigrationDrift,
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
                PackageMigrationErrorKind::MigrationDrift,
                PACKAGE_MIGRATION_DRIFT_REFUSAL,
                coordinate,
                &row.relative_path,
                &row.sha256,
                &migration.sha256,
            ));
        }
    }
    Ok(())
}

fn transaction_statements(
    coordinate: &PackageCoordinate,
    predecessor_version: Option<&str>,
    manifest_sha256: &str,
    register_root: bool,
    verified_prefix: &[NormalizedMigration<'_>],
    pending: &[NormalizedMigration<'_>],
) -> Result<Vec<SqlStatement>, PackageMigrationError> {
    let mut statements =
        Vec::with_capacity(usize::from(register_root) + verified_prefix.len() + pending.len() * 2);
    if register_root {
        statements.push(SqlStatement {
            summary: "register immutable package root".into(),
            sql: "SELECT catalog.register_package(\
                  NULLIF(current_setting('app.tenant', true), ''), $1, $2, $3, $4::text)"
                .into(),
            params: vec![
                Value::Text(coordinate.package_id().into()),
                Value::Text(coordinate.package_version().into()),
                Value::Text(manifest_sha256.into()),
                Value::NullableText(predecessor_version.map(str::to_owned)),
            ],
        });
    }
    for migration in verified_prefix {
        statements.push(record_migration_statement(
            coordinate, migration, "inherit",
        )?);
    }
    for migration in pending {
        let sql = std::str::from_utf8(&migration.source.bytes).map_err(|source| {
            PackageMigrationError::with_source(
                PackageMigrationErrorKind::InvalidDirectory,
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
        statements.push(record_migration_statement(coordinate, migration, "record")?);
    }
    Ok(statements)
}

fn record_migration_statement(
    coordinate: &PackageCoordinate,
    migration: &NormalizedMigration<'_>,
    action: &str,
) -> Result<SqlStatement, PackageMigrationError> {
    Ok(SqlStatement {
        summary: format!("{action} {}", migration.source.relative_path),
        sql: "INSERT INTO catalog.package_migrations \
              (tenant_id, package_id, package_version, ordinal, relative_path, sha256) \
              VALUES (NULLIF(current_setting('app.tenant', true), ''), $1, $2, $3, $4, $5)"
            .into(),
        params: vec![
            Value::Text(coordinate.package_id().into()),
            Value::Text(coordinate.package_version().into()),
            Value::Int(i32::try_from(migration.ordinal).map_err(|source| {
                PackageMigrationError::with_source(
                    PackageMigrationErrorKind::InvalidDirectory,
                    "migration ordinal exceeds PostgreSQL integer",
                    source,
                )
                .at_path(&migration.source.relative_path)
            })?),
            Value::Text(migration.source.relative_path.clone()),
            Value::Text(migration.sha256.clone()),
        ],
    })
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(7 + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn coordinate_text(coordinate: &PackageCoordinate) -> String {
    format!(
        "{}@{}",
        coordinate.package_id(),
        coordinate.package_version()
    )
}

fn snake_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.ends_with('_')
        && !value.contains("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> Vec<u8> {
        format!(
            r#"{{"package":{{"id":"orders","version":"{version}"}},"required_platform_policy_contract":{{"id":"orders_access","state":"unsatisfied"}},"models":{{"purchase_order":{{"schema":"receiving","table":"purchase_order","owner":"orders","operations":{{"get":{{"permission":"purchase_order.get","error_details":{{"invalid_input":{{"required":["field"]}},"not_found":{{"required":["field","id"]}},"retry":{{}},"timeout":{{}},"permission_denied":{{"required":["operation"]}},"internal_error":{{}}}},"result":"one"}}}}}}}},"connections":{{"postgres":{{"interface":"wamn:postgres@0.1.0"}}}},"components":{{"data":{{"connections":["postgres"]}}}}}}"#
        )
        .into_bytes()
    }

    fn manifest_with_predecessor(version: &str, predecessor: &str) -> Vec<u8> {
        let mut manifest: serde_json::Value = serde_json::from_slice(&manifest(version)).unwrap();
        manifest["package"]["predecessor_version"] = serde_json::Value::String(predecessor.into());
        serde_json::to_vec(&manifest).unwrap()
    }

    fn source(path: &str, sql: &str) -> MigrationSource {
        MigrationSource {
            relative_path: path.into(),
            bytes: sql.as_bytes().to_vec(),
        }
    }

    fn directory() -> PackageDirectory {
        PackageDirectory {
            manifest_bytes: manifest("1.0.0"),
            migrations: vec![
                source("migrations/0002_add_receipt.sql", "SELECT 2;"),
                source("migrations/0001_initial.sql", "SELECT 1;"),
            ],
        }
    }

    #[test]
    fn directory_order_is_derived_and_second_convergence_is_noop() {
        let first = plan_package_migrations(&directory(), None).unwrap();
        assert_eq!(
            first.pending[0].relative_path,
            "migrations/0001_initial.sql"
        );
        assert_eq!(
            first.pending[1].relative_path,
            "migrations/0002_add_receipt.sql"
        );
        assert!(!first.is_noop());
        assert_eq!(first.models[0].model_id, "purchase_order");
        assert_eq!(first.predecessor_version, None);

        let applied = AppliedPackage {
            coordinate: first.coordinate.clone(),
            predecessor_version: None,
            manifest_sha256: first.manifest_sha256.clone(),
            migrations: first
                .pending
                .iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path.clone(),
                    sha256: migration.sha256.clone(),
                })
                .collect(),
        };
        let again = plan_package_migrations(&directory(), Some(&applied)).unwrap();
        assert!(again.is_noop());
        assert!(again.statements.is_empty());
    }

    #[test]
    fn package_application_refuses_invalid_custom_operation_semantics() {
        let mut directory = directory();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&directory.manifest_bytes).expect("fixture is JSON");
        manifest["custom_operations"] = serde_json::json!({
            "quality.create_inspection": {
                "kind": "projection",
                "visibility": "private",
                "permission": "quality.create_inspection"
            }
        });
        directory.manifest_bytes = serde_json::to_vec(&manifest).expect("serialize manifest");

        let error = plan_package_migrations(&directory, None)
            .expect_err("package application admitted a private permission");
        assert_eq!(error.kind(), PackageMigrationErrorKind::InvalidManifest);
        assert!(
            error
                .to_string()
                .contains("invalid semantic manifest vocabulary")
        );
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("must not declare a permission"))
        );
    }

    #[test]
    fn cumulative_upgrade_inherits_exact_prefix_and_runs_only_suffix() {
        let predecessor_plan = plan_package_migrations(&directory(), None).unwrap();
        let predecessor = AppliedPackage {
            coordinate: predecessor_plan.coordinate,
            predecessor_version: None,
            manifest_sha256: predecessor_plan.manifest_sha256,
            migrations: predecessor_plan
                .pending
                .iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path.clone(),
                    sha256: migration.sha256.clone(),
                })
                .collect(),
        };
        let mut cumulative = directory();
        cumulative.manifest_bytes = manifest_with_predecessor("1.1.0", "1.0.0");
        cumulative
            .migrations
            .push(source("migrations/0003_add_supplier.sql", "SELECT 3;"));

        let fresh = plan_package_migrations(&cumulative, None).unwrap();
        assert!(fresh.verified_prefix.is_empty());
        assert_eq!(fresh.pending.len(), 3, "fresh targets execute every file");

        let upgrade = plan_package_migrations(&cumulative, Some(&predecessor)).unwrap();
        assert_eq!(upgrade.predecessor_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            upgrade
                .verified_prefix
                .iter()
                .map(|migration| migration.relative_path.as_str())
                .collect::<Vec<_>>(),
            [
                "migrations/0001_initial.sql",
                "migrations/0002_add_receipt.sql"
            ]
        );
        assert_eq!(upgrade.pending.len(), 1);
        assert_eq!(
            upgrade.pending[0].relative_path,
            "migrations/0003_add_supplier.sql"
        );
        assert!(!upgrade.is_noop());
    }

    #[test]
    fn cumulative_upgrade_refuses_the_first_short_or_divergent_prefix_file() {
        let predecessor_plan = plan_package_migrations(&directory(), None).unwrap();
        let predecessor = AppliedPackage {
            coordinate: predecessor_plan.coordinate,
            predecessor_version: None,
            manifest_sha256: predecessor_plan.manifest_sha256,
            migrations: predecessor_plan
                .pending
                .iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path.clone(),
                    sha256: migration.sha256.clone(),
                })
                .collect(),
        };
        let mut cumulative = directory();
        cumulative.manifest_bytes = manifest_with_predecessor("1.1.0", "1.0.0");

        let mut short = cumulative.clone();
        short
            .migrations
            .retain(|migration| migration.relative_path == "migrations/0001_initial.sql");
        let error = plan_package_migrations(&short, Some(&predecessor))
            .expect_err("a short cumulative prefix refuses");
        assert_eq!(
            error.kind(),
            PackageMigrationErrorKind::PredecessorPrefixMismatch
        );
        assert_eq!(error.path(), Some("migrations/0002_add_receipt.sql"));

        cumulative
            .migrations
            .iter_mut()
            .find(|migration| migration.relative_path == "migrations/0001_initial.sql")
            .unwrap()
            .bytes
            .push(b' ');
        let error = plan_package_migrations(&cumulative, Some(&predecessor))
            .expect_err("a divergent cumulative prefix refuses");
        assert_eq!(
            error.kind(),
            PackageMigrationErrorKind::PredecessorPrefixMismatch
        );
        assert_eq!(error.path(), Some("migrations/0001_initial.sql"));
    }

    #[test]
    fn same_coordinate_manifest_drift_names_coordinate_and_both_hashes() {
        let first = plan_package_migrations(&directory(), None).unwrap();
        let applied = AppliedPackage {
            coordinate: first.coordinate,
            predecessor_version: None,
            manifest_sha256: "sha256:recorded".into(),
            migrations: Vec::new(),
        };
        let error = plan_package_migrations(&directory(), Some(&applied)).unwrap_err();
        assert_eq!(error.kind(), PackageMigrationErrorKind::ManifestDrift);
        assert_eq!(error.coordinate(), Some("orders@1.0.0"));
        assert_eq!(error.path(), Some(PACKAGE_MANIFEST_PATH));
        assert_eq!(error.recorded_hash(), Some("sha256:recorded"));
        assert!(error.actual_hash().unwrap().starts_with("sha256:"));
    }

    #[test]
    fn gaps_duplicates_and_applied_byte_drift_are_distinct() {
        let mut gap = directory();
        gap.migrations
            .retain(|migration| migration.relative_path == "migrations/0002_add_receipt.sql");
        assert_eq!(
            plan_package_migrations(&gap, None).unwrap_err().kind(),
            PackageMigrationErrorKind::Gap
        );

        let mut duplicate = directory();
        duplicate
            .migrations
            .push(source("migrations/0001_other.sql", "SELECT 3;"));
        assert_eq!(
            plan_package_migrations(&duplicate, None)
                .unwrap_err()
                .kind(),
            PackageMigrationErrorKind::Duplicate
        );

        let first = plan_package_migrations(&directory(), None).unwrap();
        let applied = AppliedPackage {
            coordinate: first.coordinate,
            predecessor_version: None,
            manifest_sha256: first.manifest_sha256,
            migrations: vec![RecordedMigration {
                ordinal: 1,
                relative_path: "migrations/0001_initial.sql".into(),
                sha256: "sha256:changed".into(),
            }],
        };
        let error = plan_package_migrations(&directory(), Some(&applied)).unwrap_err();
        assert_eq!(error.kind(), PackageMigrationErrorKind::MigrationDrift);
        assert_eq!(error.path(), Some("migrations/0001_initial.sql"));
        assert_eq!(error.recorded_hash(), Some("sha256:changed"));
        assert!(error.actual_hash().unwrap().starts_with("sha256:"));

        let first = plan_package_migrations(&directory(), None).unwrap();
        for (migrations, expected) in [
            (
                vec![
                    RecordedMigration {
                        ordinal: 1,
                        relative_path: "migrations/0001_initial.sql".into(),
                        sha256: first.pending[0].sha256.clone(),
                    },
                    RecordedMigration {
                        ordinal: 1,
                        relative_path: "migrations/0002_add_receipt.sql".into(),
                        sha256: first.pending[1].sha256.clone(),
                    },
                ],
                PackageMigrationErrorKind::Duplicate,
            ),
            (
                vec![RecordedMigration {
                    ordinal: 2,
                    relative_path: "migrations/0002_add_receipt.sql".into(),
                    sha256: first.pending[1].sha256.clone(),
                }],
                PackageMigrationErrorKind::Gap,
            ),
        ] {
            let applied = AppliedPackage {
                coordinate: first.coordinate.clone(),
                predecessor_version: None,
                manifest_sha256: first.manifest_sha256.clone(),
                migrations,
            };
            assert_eq!(
                plan_package_migrations(&directory(), Some(&applied))
                    .unwrap_err()
                    .kind(),
                expected
            );
        }
    }

    #[test]
    fn model_relation_identifiers_follow_the_canonical_snake_case_law() {
        for (valid, invalid) in [
            ("\"purchase_order\":{", "\"purchase_order_\":{"),
            ("\"schema\":\"receiving\"", "\"schema\":\"receiving__data\""),
            (
                "\"table\":\"purchase_order\"",
                "\"table\":\"PurchaseOrder\"",
            ),
        ] {
            let mut refused = directory();
            let manifest = String::from_utf8(refused.manifest_bytes).expect("manifest is UTF-8");
            refused.manifest_bytes = manifest.replacen(valid, invalid, 1).into_bytes();

            let error = plan_package_migrations(&refused, None)
                .expect_err("noncanonical model relation identity refuses");
            assert_eq!(error.kind(), PackageMigrationErrorKind::InvalidManifest);
            assert_eq!(error.path(), Some(PACKAGE_MANIFEST_PATH));
        }
    }
}

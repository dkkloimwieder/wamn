//! Pure package-directory migration planning.
//!
//! The effect shell supplies the exact `wamn.json` bytes, every file under the
//! package-owned `migrations/` directory, and the immutable rows already stored
//! for the installed package schema. This module validates the complete prefix and
//! returns ordered files and their completion records; it performs no filesystem or database
//! I/O.

use std::error::Error;
use std::fmt;

use wamn_catalog::PackageCoordinate;
use wamn_catalog::manifest::PackageManifest;
use wamn_record_history::history_table_name;

use crate::{SqlStatement, Value};

pub const PACKAGE_MANIFEST_PATH: &str = "wamn.json";
pub const PACKAGE_MIGRATION_DRIFT_REFUSAL: &str = "package-migration-drift";
pub const PACKAGE_MIGRATION_DUPLICATE_REFUSAL: &str = "package-migration-duplicate";
pub const PACKAGE_MIGRATION_GAP_REFUSAL: &str = "package-migration-gap";
pub const PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL: &str = "package-coordinate-without-artifact";

/// One exact migration file discovered under a package directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSource {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// One immutable applied migration record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMigration {
    pub ordinal: u32,
    pub relative_path: String,
    pub sha256: String,
    pub outcome: MigrationOutcome,
}

/// Persisted completion state of one immutable migration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Applied,
    /// The user asserted these effects. WAMN did not execute this file.
    Adopted,
    Started,
}

/// Recorded SQL state, independent of the selected executable release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPackage {
    pub coordinate: PackageCoordinate,
    /// The package artifact digest recorded in `catalog.packages`. `None` is a
    /// coordinate that `wamn dev` applied from a working tree.
    pub artifact_digest: Option<String>,
    pub baseline_fingerprint: String,
    pub physical_schema_fingerprint: String,
    pub migrations: Vec<RecordedMigration>,
}

/// Exact package-directory bytes supplied by the effect shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDirectory {
    pub manifest_bytes: Vec<u8>,
    pub migrations: Vec<MigrationSource>,
    /// The digest of the verified package artifact these bytes come from
    /// (docs/operations/building.md#reading-a-pushed-package). The caller supplies it. `None` is a
    /// working tree, which only `wamn dev` applies.
    pub artifact_digest: Option<String>,
}

/// Sole package-local model-key to physical relation mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedModel {
    pub model_id: String,
    pub schema: String,
    pub table: String,
}

/// Package-owned relation excluded from CDC publication.
///
/// An internal relation is excluded by its declaration. The history table of a
/// logged relation is excluded by the declaration of that relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcExcludedRelation {
    pub relation_id: String,
    pub schema: String,
    pub table: String,
}

/// One validated pending file and its immutable record identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMigration {
    pub ordinal: u32,
    pub relative_path: String,
    pub sha256: String,
    pub transactional: bool,
    pub started: bool,
}

/// Ordered pending files and statements for the SQL executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMigrationPlan {
    pub coordinate: PackageCoordinate,
    pub artifact_digest: Option<String>,
    pub models: Vec<ManagedModel>,
    pub cdc_excluded_relations: Vec<CdcExcludedRelation>,
    pub pending: Vec<PendingMigration>,
    pub statements: Vec<SqlStatement>,
}

impl PackageMigrationPlan {
    /// A converged package executes no record or migration writes.
    pub fn is_noop(&self) -> bool {
        self.statements.is_empty()
    }
}

/// Stable package migration refusal class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageMigrationErrorType {
    InvalidManifest,
    InvalidDirectory,
    Duplicate,
    Gap,
    MigrationDrift,
    CoordinateContentConflict,
    CoordinateWithoutArtifact,
}

impl PackageMigrationErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidManifest => "invalid-manifest",
            Self::InvalidDirectory => "invalid-directory",
            Self::Duplicate => PACKAGE_MIGRATION_DUPLICATE_REFUSAL,
            Self::Gap => PACKAGE_MIGRATION_GAP_REFUSAL,
            Self::MigrationDrift => PACKAGE_MIGRATION_DRIFT_REFUSAL,
            Self::CoordinateContentConflict => "package-coordinate-content-conflict",
            Self::CoordinateWithoutArtifact => PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL,
        }
    }
}

/// Contextual package migration refusal.
#[derive(Debug)]
pub struct PackageMigrationError {
    type_: PackageMigrationErrorType,
    // Boxed strings (M-BOX-DST): the context is written once and only read
    // back, and the narrower fields keep the whole error inside the size every
    // `Result` in this module carries (`clippy::result_large_err`).
    context: Box<str>,
    coordinate: Option<Box<str>>,
    path: Option<Box<str>>,
    recorded_hash: Option<Box<str>>,
    actual_hash: Option<Box<str>>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl PackageMigrationError {
    pub const fn error_type(&self) -> PackageMigrationErrorType {
        self.type_
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

    pub(crate) fn new(type_: PackageMigrationErrorType, context: impl Into<String>) -> Self {
        Self {
            type_,
            context: context.into().into_boxed_str(),
            coordinate: None,
            path: None,
            recorded_hash: None,
            actual_hash: None,
            source: None,
        }
    }

    pub(crate) fn with_source(
        type_: PackageMigrationErrorType,
        context: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: Some(Box::new(source)),
            ..Self::new(type_, context)
        }
    }

    pub(crate) fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into().into_boxed_str());
        self
    }

    pub(crate) fn drift(
        type_: PackageMigrationErrorType,
        context: impl Into<String>,
        coordinate: Option<&PackageCoordinate>,
        path: impl Into<String>,
        recorded_hash: impl Into<String>,
        actual_hash: impl Into<String>,
    ) -> Self {
        Self {
            type_,
            context: context.into().into_boxed_str(),
            coordinate: coordinate.map(|value| coordinate_text(value).into_boxed_str()),
            path: Some(path.into().into_boxed_str()),
            recorded_hash: Some(recorded_hash.into().into_boxed_str()),
            actual_hash: Some(actual_hash.into().into_boxed_str()),
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

/// The refusal for a coordinate recorded with other bytes than presented: a
/// working-tree install (no digest) on either side, or another artifact.
fn digest_conflict(
    coordinate: &PackageCoordinate,
    recorded: Option<&str>,
    presented: Option<&str>,
) -> PackageMigrationError {
    let (type_, literal) = if recorded.is_none() || presented.is_none() {
        (
            PackageMigrationErrorType::CoordinateWithoutArtifact,
            PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL,
        )
    } else {
        (
            PackageMigrationErrorType::CoordinateContentConflict,
            "package-coordinate-content-conflict",
        )
    };
    let shown = |digest: Option<&str>| digest.unwrap_or("<working tree>").to_owned();
    PackageMigrationError {
        type_,
        context: format!(
            "{literal}: coordinate={} recorded-digest={} presented-digest={}",
            coordinate_text(coordinate),
            shown(recorded),
            shown(presented)
        )
        .into_boxed_str(),
        coordinate: Some(coordinate_text(coordinate).into_boxed_str()),
        path: None,
        recorded_hash: Some(shown(recorded).into_boxed_str()),
        actual_hash: Some(shown(presented).into_boxed_str()),
        source: None,
    }
}

/// Preserve package ownership and the immutable identity of the recorded source.
pub fn validate_package_schema_source(
    coordinate: &PackageCoordinate,
    artifact_digest: Option<&str>,
    existing: &AppliedPackage,
) -> Result<(), PackageMigrationError> {
    if existing.coordinate.package_id() != coordinate.package_id() {
        return Err(PackageMigrationError::new(
            PackageMigrationErrorType::InvalidDirectory,
            "installed baseline belongs to another package",
        ));
    }
    if existing.coordinate == *coordinate && existing.artifact_digest.as_deref() != artifact_digest
    {
        return Err(digest_conflict(
            coordinate,
            existing.artifact_digest.as_deref(),
            artifact_digest,
        ));
    }
    Ok(())
}

/// Validate a package directory against its immutable applied prefix.
pub fn plan_package_migrations(
    directory: &PackageDirectory,
    applied: Option<&AppliedPackage>,
    recorded: &[RecordedMigration],
) -> Result<PackageMigrationPlan, PackageMigrationError> {
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes).map_err(|source| {
        PackageMigrationError::with_source(
            PackageMigrationErrorType::InvalidManifest,
            "wamn.json does not match the strict package manifest",
            source,
        )
        .at_path(PACKAGE_MANIFEST_PATH)
    })?;
    let coordinate = PackageCoordinate::new(&manifest.package.id, &manifest.package.version)
        .map_err(|source| {
            PackageMigrationError::with_source(
                PackageMigrationErrorType::InvalidManifest,
                "wamn.json package coordinate is invalid",
                source,
            )
            .at_path(PACKAGE_MANIFEST_PATH)
        })?;
    let artifact_digest = directory.artifact_digest.clone();

    if let Some(existing) = applied {
        validate_package_schema_source(&coordinate, artifact_digest.as_deref(), existing)?;
    }

    if !manifest.owns_no_table() && directory.migrations.is_empty() {
        return Err(PackageMigrationError::new(
            PackageMigrationErrorType::Gap,
            format!("{PACKAGE_MIGRATION_GAP_REFUSAL}: package has no 0001 migration"),
        ));
    }
    let migration_plan = crate::migrations::plan_migrations(
        &directory.migrations,
        recorded,
        "migrations/",
        Some(&coordinate),
    )?;

    let mut models = Vec::with_capacity(manifest.models.len());
    let mut history_exclusions = Vec::new();
    for (model_id, model) in manifest.models {
        for (field, value) in [
            ("model-id", model_id.as_str()),
            ("schema", model.schema.as_str()),
            ("table", model.table.as_str()),
        ] {
            if !snake_identifier(value) {
                return Err(PackageMigrationError::new(
                    PackageMigrationErrorType::InvalidManifest,
                    format!("wamn.json {field} {value:?} is not singular snake_case"),
                )
                .at_path(PACKAGE_MANIFEST_PATH));
            }
        }
        // The history table of a logged relation is excluded from CDC. Its
        // table name is its relation id, and the relation owner owns it.
        if model.log_retention().is_some() {
            let history = history_table_name(&model.table);
            history_exclusions.push(CdcExcludedRelation {
                relation_id: history.clone(),
                schema: model.schema.clone(),
                table: history,
            });
        }
        models.push(ManagedModel {
            model_id,
            schema: model.schema,
            table: model.table,
        });
    }
    let cdc_excluded_relations = manifest
        .internal_relations
        .into_iter()
        .map(|(relation_id, relation)| CdcExcludedRelation {
            relation_id,
            schema: relation.schema,
            table: relation.table,
        })
        .chain(history_exclusions)
        .collect();
    let pending = migration_plan.pending;
    let mut statements = Vec::with_capacity(pending.len() * 2);
    for (file, sql) in pending.iter().zip(migration_plan.statements) {
        statements.push(sql);
        statements.push(record_migration_statement(&coordinate, file)?);
    }

    Ok(PackageMigrationPlan {
        coordinate,
        artifact_digest,
        models,
        cdc_excluded_relations,
        pending,
        statements,
    })
}

fn record_migration_statement(
    coordinate: &PackageCoordinate,
    migration: &PendingMigration,
) -> Result<SqlStatement, PackageMigrationError> {
    Ok(SqlStatement {
        summary: format!("record {}", migration.relative_path),
        sql: "INSERT INTO catalog.package_migrations \
              (project_env_key, package_id, package_version, ordinal, relative_path, sha256, outcome, applied_at) \
              VALUES (NULLIF(current_setting('app.project_env_key', true), ''), $1, $2, $3, $4, $5, $6, \
                      CASE WHEN $6 = 'applied' THEN now() END)"
            .into(),
        params: vec![
            Value::Text(coordinate.package_id().into()),
            Value::Text(coordinate.package_version().into()),
            Value::Int(i32::try_from(migration.ordinal).map_err(|source| {
                PackageMigrationError::with_source(
                    PackageMigrationErrorType::InvalidDirectory,
                    "migration ordinal exceeds PostgreSQL integer",
                    source,
                )
                .at_path(&migration.relative_path)
            })?),
            Value::Text(migration.relative_path.clone()),
            Value::Text(migration.sha256.clone()),
            Value::Text(if migration.transactional { "applied" } else { "started" }.into()),
        ],
    })
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
            r#"{{"package":{{"id":"orders","version":"{version}"}},"required_platform_policy_contract":{{"id":"orders_access","state":"unsatisfied"}},"models":{{"widget_tag":{{"schema":"fixture","table":"widget_tag","owner":"orders","audit_log":{{"columns":[],"retention":"none"}},"operations":{{"get":{{"permission":"widget_tag.get","result":"one"}}}}}}}},"connections":{{}},"components":{{"data":{{"connections":["postgres"]}}}}}}"#
        )
        .into_bytes()
    }

    fn source(path: &str, sql: &str) -> MigrationSource {
        MigrationSource {
            relative_path: path.into(),
            bytes: sql.as_bytes().to_vec(),
        }
    }

    const ARTIFACT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn directory() -> PackageDirectory {
        PackageDirectory {
            manifest_bytes: manifest("1.0.0"),
            migrations: vec![
                source("migrations/0002_add_tag.sql", "SELECT 2;"),
                source("migrations/0001_initial.sql", "SELECT 1;"),
            ],
            artifact_digest: Some(ARTIFACT.into()),
        }
    }

    /// Each migration `sha256` is persisted in `catalog.package_migrations`
    /// and compared on every reapply, so its bytes must never move. The golden
    /// value is the sha256 of the literal preimage below, computed outside this
    /// code. The plan carries the artifact digest its caller supplies.
    #[test]
    fn the_persisted_package_digest_bytes_are_pinned() {
        const PINNED_MANIFEST: &str = concat!(
            r#"{"package":{"id":"orders","version":"1.0.0"},"#,
            r#""required_platform_policy_contract":{"id":"orders_access","state":"unsatisfied"},"#,
            r#""models":{"widget_tag":{"schema":"fixture","table":"widget_tag","#,
            r#""owner":"orders","audit_log":{"columns":[],"retention":"none"},"#,
            r#""operations":{"get":{"permission":"widget_tag.get","#,
            r#""result":"one"}}}},"#,
            r#""connections":{},"#,
            r#""components":{"data":{"connections":["postgres"]}}}"#,
        );
        const PINNED_MIGRATION: &str = "SELECT 1;";

        let pinned = PackageDirectory {
            manifest_bytes: PINNED_MANIFEST.as_bytes().to_vec(),
            migrations: vec![source("migrations/0001_initial.sql", PINNED_MIGRATION)],
            artifact_digest: Some(ARTIFACT.into()),
        };
        let plan = plan_package_migrations(&pinned, None, &[]).unwrap();

        assert_eq!(plan.artifact_digest.as_deref(), Some(ARTIFACT));
        assert_eq!(
            plan.pending[0].sha256,
            "sha256:17db4fd369edb9244b9f91d9aeed145c3d04ad8ba6e95d06247f07a63527d11a"
        );
    }

    #[test]
    fn directory_order_is_derived_and_second_convergence_is_noop() {
        let first = plan_package_migrations(&directory(), None, &[]).unwrap();
        assert_eq!(
            first.pending[0].relative_path,
            "migrations/0001_initial.sql"
        );
        assert_eq!(
            first.pending[1].relative_path,
            "migrations/0002_add_tag.sql"
        );
        assert!(!first.is_noop());
        assert_eq!(first.models[0].model_id, "widget_tag");

        let applied = AppliedPackage {
            coordinate: first.coordinate.clone(),
            artifact_digest: first.artifact_digest.clone(),
            baseline_fingerprint: "sha256:baseline".into(),
            physical_schema_fingerprint: "sha256:baseline".into(),
            migrations: first
                .pending
                .iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path.clone(),
                    sha256: migration.sha256.clone(),
                    outcome: MigrationOutcome::Applied,
                })
                .collect(),
        };
        let again =
            plan_package_migrations(&directory(), Some(&applied), &applied.migrations).unwrap();
        assert!(again.is_noop());
        assert!(again.statements.is_empty());
        let mut executable = directory();
        executable.manifest_bytes = manifest("2.0.0");
        executable.artifact_digest = Some("sha256:another-artifact".into());
        assert!(
            plan_package_migrations(&executable, Some(&applied), &applied.migrations)
                .unwrap()
                .is_noop()
        );
        executable
            .migrations
            .push(source("migrations/0003_pending.sql", "SELECT 3;"));
        let next =
            plan_package_migrations(&executable, Some(&applied), &applied.migrations).unwrap();
        assert_eq!(next.pending.len(), 1);
        assert_eq!(next.pending[0].relative_path, "migrations/0003_pending.sql");
        executable.migrations[1].bytes.push(b' ');
        assert_eq!(
            plan_package_migrations(&executable, Some(&applied), &applied.migrations)
                .unwrap_err()
                .error_type(),
            PackageMigrationErrorType::MigrationDrift
        );
    }

    #[test]
    fn same_coordinate_other_artifact_is_a_content_conflict_naming_both_digests() {
        let first = plan_package_migrations(&directory(), None, &[]).unwrap();
        let applied = AppliedPackage {
            coordinate: first.coordinate,
            artifact_digest: Some("sha256:recorded".into()),
            baseline_fingerprint: "sha256:baseline".into(),
            physical_schema_fingerprint: "sha256:baseline".into(),
            migrations: Vec::new(),
        };
        let error =
            plan_package_migrations(&directory(), Some(&applied), &applied.migrations).unwrap_err();
        assert_eq!(
            error.error_type(),
            PackageMigrationErrorType::CoordinateContentConflict
        );
        assert_eq!(error.coordinate(), Some("orders@1.0.0"));
        assert_eq!(error.recorded_hash(), Some("sha256:recorded"));
        assert_eq!(error.actual_hash(), Some(ARTIFACT));

        // A coordinate that wamn dev applied from a working tree has no
        // digest, and a release apply over it refuses with its own type.
        let working_tree = AppliedPackage {
            artifact_digest: None,
            ..applied
        };
        let error =
            plan_package_migrations(&directory(), Some(&working_tree), &working_tree.migrations)
                .unwrap_err();
        assert_eq!(
            error.error_type(),
            PackageMigrationErrorType::CoordinateWithoutArtifact
        );
        assert!(
            error
                .context()
                .starts_with(PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL)
        );
    }

    #[test]
    fn gaps_duplicates_and_applied_byte_drift_are_distinct() {
        let mut gap = directory();
        gap.migrations
            .retain(|migration| migration.relative_path == "migrations/0002_add_tag.sql");
        assert_eq!(
            plan_package_migrations(&gap, None, &[])
                .unwrap_err()
                .error_type(),
            PackageMigrationErrorType::Gap
        );

        let mut duplicate = directory();
        duplicate
            .migrations
            .push(source("migrations/0001_other.sql", "SELECT 3;"));
        assert_eq!(
            plan_package_migrations(&duplicate, None, &[])
                .unwrap_err()
                .error_type(),
            PackageMigrationErrorType::Duplicate
        );

        let first = plan_package_migrations(&directory(), None, &[]).unwrap();
        let applied = AppliedPackage {
            coordinate: first.coordinate,
            artifact_digest: first.artifact_digest,
            baseline_fingerprint: "sha256:baseline".into(),
            physical_schema_fingerprint: "sha256:baseline".into(),
            migrations: vec![RecordedMigration {
                ordinal: 1,
                relative_path: "migrations/0001_initial.sql".into(),
                sha256: "sha256:changed".into(),
                outcome: MigrationOutcome::Applied,
            }],
        };
        let error =
            plan_package_migrations(&directory(), Some(&applied), &applied.migrations).unwrap_err();
        assert_eq!(
            error.error_type(),
            PackageMigrationErrorType::MigrationDrift
        );
        assert_eq!(error.path(), Some("migrations/0001_initial.sql"));
        assert_eq!(error.recorded_hash(), Some("sha256:changed"));
        assert!(error.actual_hash().unwrap().starts_with("sha256:"));

        let first = plan_package_migrations(&directory(), None, &[]).unwrap();
        for (migrations, expected) in [
            (
                vec![
                    RecordedMigration {
                        ordinal: 1,
                        relative_path: "migrations/0001_initial.sql".into(),
                        sha256: first.pending[0].sha256.clone(),
                        outcome: MigrationOutcome::Applied,
                    },
                    RecordedMigration {
                        ordinal: 1,
                        relative_path: "migrations/0002_add_tag.sql".into(),
                        sha256: first.pending[1].sha256.clone(),
                        outcome: MigrationOutcome::Applied,
                    },
                ],
                PackageMigrationErrorType::Duplicate,
            ),
            (
                vec![RecordedMigration {
                    ordinal: 2,
                    relative_path: "migrations/0002_add_tag.sql".into(),
                    sha256: first.pending[1].sha256.clone(),
                    outcome: MigrationOutcome::Applied,
                }],
                PackageMigrationErrorType::Gap,
            ),
        ] {
            let applied = AppliedPackage {
                coordinate: first.coordinate.clone(),
                artifact_digest: first.artifact_digest.clone(),
                baseline_fingerprint: "sha256:baseline".into(),
                physical_schema_fingerprint: "sha256:baseline".into(),
                migrations,
            };
            assert_eq!(
                plan_package_migrations(&directory(), Some(&applied), &applied.migrations)
                    .unwrap_err()
                    .error_type(),
                expected
            );
        }
    }

    /// Spec test 13: a logged relation derives the CDC exclusion of its history table.
    #[test]
    fn a_logged_relation_derives_the_cdc_exclusion_of_its_history_table() {
        let unlogged = plan_package_migrations(&directory(), None, &[]).unwrap();
        assert_eq!(unlogged.cdc_excluded_relations, Vec::new());

        let mut logged = directory();
        let manifest = String::from_utf8(logged.manifest_bytes).expect("manifest is UTF-8");
        logged.manifest_bytes = manifest
            .replacen(r#""retention":"none""#, r#""retention":"P30D""#, 1)
            .into_bytes();
        let plan = plan_package_migrations(&logged, None, &[]).unwrap();
        assert_eq!(
            plan.cdc_excluded_relations,
            [CdcExcludedRelation {
                relation_id: "widget_tag_history".into(),
                schema: "fixture".into(),
                table: "widget_tag_history".into(),
            }]
        );
    }

    #[test]
    fn model_relation_identifiers_follow_the_canonical_snake_case_law() {
        for (valid, invalid) in [
            ("\"widget_tag\":{", "\"widget_tag_\":{"),
            ("\"schema\":\"fixture\"", "\"schema\":\"fixture__data\""),
            ("\"table\":\"widget_tag\"", "\"table\":\"WidgetTag\""),
        ] {
            let mut refused = directory();
            let manifest = String::from_utf8(refused.manifest_bytes).expect("manifest is UTF-8");
            refused.manifest_bytes = manifest.replacen(valid, invalid, 1).into_bytes();

            let error = plan_package_migrations(&refused, None, &[])
                .expect_err("noncanonical model relation identity refuses");
            assert_eq!(
                error.error_type(),
                PackageMigrationErrorType::InvalidManifest
            );
            assert_eq!(error.path(), Some(PACKAGE_MANIFEST_PATH));
        }
    }
}

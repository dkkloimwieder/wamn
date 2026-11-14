//! Package inputs and the physical relation declarations used by CDC.

use wamn_catalog::PackageCoordinate;
use wamn_catalog::manifest::PackageManifest;
use wamn_record_history::history_table_name;

/// An immutable authored or compiler-generated migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSource {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// The current package SQL source, independent of executable release selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPackage {
    pub coordinate: PackageCoordinate,
    pub artifact_digest: Option<String>,
}

/// Exact inputs read from one verified package tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDirectory {
    pub manifest_bytes: Vec<u8>,
    pub migrations: Vec<MigrationSource>,
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

/// Package relation declarations. SQLx owns migration planning.
#[derive(Debug)]
pub struct PackageMigrationPlan {
    pub coordinate: PackageCoordinate,
    pub models: Vec<ManagedModel>,
    pub cdc_excluded_relations: Vec<CdcExcludedRelation>,
}

/// Read package declarations. SQLx resolves migration files when execution is prepared.
pub fn package_metadata(directory: &PackageDirectory) -> anyhow::Result<PackageMigrationPlan> {
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
    let coordinate = PackageCoordinate::new(&manifest.package.id, &manifest.package.version)?;
    let mut models = Vec::new();
    let mut cdc_excluded_relations = Vec::new();
    for (model_id, model) in manifest.models {
        if model.log_retention().is_some() {
            let table = history_table_name(&model.table);
            cdc_excluded_relations.push(CdcExcludedRelation {
                relation_id: table.clone(),
                schema: model.schema.clone(),
                table,
            });
        }
        models.push(ManagedModel {
            model_id,
            schema: model.schema,
            table: model.table,
        });
    }
    cdc_excluded_relations.extend(manifest.internal_relations.into_iter().map(
        |(relation_id, relation)| CdcExcludedRelation {
            relation_id,
            schema: relation.schema,
            table: relation.table,
        },
    ));
    Ok(PackageMigrationPlan {
        coordinate,
        models,
        cdc_excluded_relations,
    })
}

/// Refuse a different artifact for an already recorded coordinate.
pub fn validate_package_schema_source(
    coordinate: &PackageCoordinate,
    artifact_digest: Option<&str>,
    existing: &AppliedPackage,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        existing.coordinate.package_id() == coordinate.package_id(),
        "installed schema belongs to another package"
    );
    anyhow::ensure!(
        existing.coordinate != *coordinate
            || existing.artifact_digest.as_deref() == artifact_digest,
        "package-coordinate-content-conflict: {coordinate:?}"
    );
    Ok(())
}

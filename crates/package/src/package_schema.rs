//! Package inputs and the physical relation declarations used by CDC.

use crate::manifest::PackageManifest;
use crate::{PackageCoordinate, canonical_name::CanonicalName};
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
    pub artifact_digest: Option<wamn_digest::Digest>,
}

/// Exact inputs read from one verified package tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDirectory {
    pub manifest_bytes: Vec<u8>,
    pub migrations: Vec<MigrationSource>,
    pub artifact_digest: Option<wamn_digest::Digest>,
}

/// Sole package-local model-key to physical relation mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedModel {
    pub model_id: CanonicalName,
    pub schema: CanonicalName,
    pub table: CanonicalName,
}

/// Package-owned relation excluded from CDC publication.
///
/// An internal relation is excluded by its declaration. The history table of a
/// logged relation is excluded by the declaration of that relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcExcludedRelation {
    pub relation_id: CanonicalName,
    pub schema: CanonicalName,
    pub table: CanonicalName,
}

/// Package relation declarations. SQLx owns migration planning.
#[derive(Debug)]
pub struct PackageMetadata {
    pub coordinate: PackageCoordinate,
    pub models: Vec<ManagedModel>,
    pub cdc_excluded_relations: Vec<CdcExcludedRelation>,
}

/// Read package declarations. SQLx resolves migration files when execution is prepared.
pub fn package_metadata<D: serde::de::DeserializeOwned>(
    directory: &PackageDirectory,
) -> anyhow::Result<PackageMetadata> {
    let manifest = PackageManifest::<D>::from_slice(&directory.manifest_bytes)?;
    let coordinate = PackageCoordinate::new(manifest.package.id, manifest.package.version)?;
    let mut models = Vec::new();
    let mut cdc_excluded_relations = Vec::new();
    for (model_id, model) in manifest.models {
        if model.log_retention().is_some() {
            let table: CanonicalName = history_table_name(model.table.as_str()).parse()?;
            cdc_excluded_relations.push(CdcExcludedRelation {
                relation_id: table.clone(),
                schema: model.schema.clone().into(),
                table,
            });
        }
        models.push(ManagedModel {
            model_id: model_id.into(),
            schema: model.schema.into(),
            table: model.table.into(),
        });
    }
    cdc_excluded_relations.extend(manifest.internal_relations.into_iter().map(
        |(relation_id, relation)| CdcExcludedRelation {
            relation_id: relation_id.into(),
            schema: relation.schema.into(),
            table: relation.table.into(),
        },
    ));
    Ok(PackageMetadata {
        coordinate,
        models,
        cdc_excluded_relations,
    })
}

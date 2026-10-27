//! Relation-owned field declarations inherited by overlay models.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::{GenerateError, GenerateErrorType, PackageManifest};

/// Read the same base builds that component composition uses. The receipt
/// records each manifest’s digest. Release resolution checks the package
/// artifact pin.
pub(crate) fn load_base_manifests(
    package_root: &Path,
    output_root: &Path,
    manifest: &PackageManifest,
) -> Result<Vec<PackageManifest>> {
    manifest
        .base_dependencies
        .values()
        .map(|dependency| {
            let output = crate::build::package_output(output_root, &dependency.package);
            let receipt = crate::receipt::read_current(
                &crate::output::dependency_root(package_root, &dependency.package),
                &output,
            )
            .with_context(|| format!("build the base package {} first", dependency.package))?;
            let bytes = fs::read(output.join(crate::authoring::COMPILED_MANIFEST_NAME))
                .with_context(|| format!("read the manifest of base {}", dependency.package))?;
            ensure!(
                crate::generate::sha256(&bytes) == receipt.outputs.manifest,
                "base {} manifest differs from its build receipt",
                dependency.package
            );
            let base = PackageManifest::from_slice(&bytes)?;
            ensure!(
                base.package.id == dependency.package
                    && base.package.version == dependency.version
                    && receipt.package.id == base.package.id
                    && receipt.package.version == base.package.version,
                "base {} build does not match version {}",
                dependency.package,
                dependency.version
            );
            Ok(base)
        })
        .collect()
}

/// Complete owner facts before validation and generation.
pub(crate) fn inherit_model_fields(
    manifest: &mut PackageManifest,
    bases: &[PackageManifest],
) -> Result<(), GenerateError> {
    let invalid = |detail| GenerateError::new(GenerateErrorType::InvalidModel, detail);
    for (name, model) in &mut manifest.models {
        if model.owner == manifest.package.id {
            continue;
        }
        let base = bases.iter().find(|base| {
            base.package.id == model.owner && manifest.base_dependencies.values().any(|pin| {
                pin.package == base.package.id && pin.version == base.package.version
            })
        }).ok_or_else(|| invalid(format!(
            "{name} needs the compiled manifest of its relation owner {} at the declared base version", model.owner
        )))?;
        let owned = base
            .models
            .values()
            .find(|owned| {
                owned.owner == model.owner
                    && owned.schema == model.schema
                    && owned.table == model.table
            })
            .ok_or_else(|| {
                invalid(format!(
                    "{name} has no relation {}.{} owned by {} in the base manifest",
                    model.schema, model.table, model.owner
                ))
            })?;
        for (field, declared) in &model.fields {
            if declared.number.is_none() && declared.role.is_none() {
                continue;
            }
            if model.field_owner(field) != model.owner {
                return Err(invalid(format!(
                    "{name}.{field} belongs to {}, not to the relation owner {}; only the relation owner's fields are numbered or stamped",
                    model.field_owner(field),
                    model.owner
                )));
            }
            if owned.fields.get(field) != Some(declared) {
                return Err(invalid(format!(
                    "{name}.{field} disagrees with the number or actor declaration of relation owner {}",
                    model.owner
                )));
            }
        }
        for (field, declaration) in &owned.fields {
            if declaration.number.is_none() && declaration.role.is_none() {
                continue;
            }
            model.fields.insert(field.clone(), declaration.clone());
            if let Some(sequence) = &declaration.number {
                let inherited = base.sequences.get(sequence).ok_or_else(|| {
                    invalid(format!(
                        "{name}.{field} numbers from undeclared base sequence {sequence}"
                    ))
                })?;
                if manifest
                    .sequences
                    .get(sequence)
                    .is_some_and(|own| own != inherited)
                {
                    return Err(invalid(format!(
                        "{name}.{field} sequence {sequence} disagrees with relation owner {}",
                        model.owner
                    )));
                }
                manifest
                    .sequences
                    .insert(sequence.clone(), inherited.clone());
            }
        }
    }
    Ok(())
}

/// Persist inherited members and preserve the other authored members.
pub(crate) fn complete_manifest(bytes: &[u8], bases: &[PackageManifest]) -> Result<Vec<u8>> {
    let mut manifest = PackageManifest::from_slice(bytes)?;
    inherit_model_fields(&mut manifest, bases)?;
    let mut document: serde_json::Value = serde_json::from_slice(bytes)?;
    for (name, model) in &manifest.models {
        if model.owner != manifest.package.id && !model.fields.is_empty() {
            document["models"][name]["fields"] = serde_json::to_value(&model.fields)?;
        }
    }
    if !manifest.sequences.is_empty() {
        document["sequences"] = serde_json::to_value(&manifest.sequences)?;
    }
    Ok(serde_json::to_vec(&document)?)
}

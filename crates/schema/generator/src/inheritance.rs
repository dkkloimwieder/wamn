//! Relation-owned field declarations inherited by overlay models.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::manifest::PackageManifest;
use crate::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};

/// Read the same base builds that component composition uses. The build record
/// records each manifest’s digest. Release resolution tests the package
/// artifact pin.
pub(crate) fn load_base_manifests(
    output_root: &Path,
    dependencies: &std::collections::BTreeMap<String, crate::manifest::BaseDependencyRequirement>,
) -> Result<Vec<PackageManifest>> {
    dependencies
        .values()
        .map(|dependency| {
            let output = crate::build::package_output(output_root, &dependency.package);
            let bytes = fs::read(output.join(crate::authoring::COMPILED_MANIFEST_NAME))
                .with_context(|| format!("read the manifest of base {}", dependency.package))?;
            let base = PackageManifest::from_slice(&bytes)?;
            ensure!(
                base.package.id == dependency.package && base.package.version == dependency.version,
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

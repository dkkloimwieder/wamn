use super::{
    BTreeSet, CatalogIr, ComponentAdmission, ComponentDeclarationError,
    ComponentDeclarationErrorType, LocalBindingSelection, PackageInput, PackageManifest,
    ProductionDevStageError, SelectedComponentArtifact,
};

pub(super) fn project_catalog_for_package(
    catalog: &CatalogIr,
    target: &PackageManifest,
    installed: &[PackageInput],
) -> Result<CatalogIr, ProductionDevStageError> {
    let manifests = installed
        .iter()
        .map(|package| package.manifest.clone())
        .collect::<Vec<_>>();
    wamn_schema_generator::project_package_catalog(catalog, target, &manifests).map_err(|source| {
        ProductionDevStageError::invalid("project package catalog", source.to_string())
    })
}

/// The store alias that the binding selections give a component: the one name
/// the operator passes as the environment document in the cluster.
pub(super) fn selected_store_alias(
    selections: &[LocalBindingSelection],
    artifact: &SelectedComponentArtifact,
) -> anyhow::Result<Option<String>> {
    let mut aliases = selections
        .iter()
        .filter(|selection| {
            *selection.package_id == *artifact.package_id
                && *selection.component == *artifact.component
        })
        .map(|selection| selection.store_alias.clone())
        .collect::<BTreeSet<_>>()
        .into_iter();
    let alias = aliases.next();
    anyhow::ensure!(
        aliases.next().is_none(),
        "the binding selections give {}::{} more than one store alias",
        artifact.package_id,
        artifact.component
    );
    Ok(alias)
}

/// Refuse a binding store alias that differs from the `params.store_alias` a
/// wiring node of the same package gives the component (wamn-qj68). The wiring
/// node is the authored source; the binding selection only repeats it.
pub(super) fn require_wiring_store_alias(
    components: &std::collections::BTreeMap<String, BTreeSet<String>>,
    artifact: &SelectedComponentArtifact,
    store_alias: Option<&str>,
) -> Result<(), ProductionDevStageError> {
    let Some(store_alias) = store_alias else {
        return Ok(());
    };
    for wired in components
        .get(artifact.component.as_ref())
        .into_iter()
        .flatten()
    {
        if wired != store_alias {
            return Err(ProductionDevStageError::invalid(
                "require store alias",
                format!(
                    "the binding selections give {}::{} store alias {store_alias:?}, but the build input gives {wired:?}",
                    artifact.package_id, artifact.component,
                ),
            ));
        }
    }
    Ok(())
}

/// Refuse an admission whose identity differs from the artifact selected for it.
pub(super) fn require_admitted_identity(
    admission: &ComponentAdmission,
    artifact: &SelectedComponentArtifact,
) -> Result<(), ProductionDevStageError> {
    if admission.package_id() != artifact.package_id.as_ref()
        || admission.package_version() != artifact.package_version.as_ref()
        || admission.component() != artifact.component.as_ref()
        || admission.component_digest() != artifact.digest.as_ref()
    {
        return Err(ProductionDevStageError::invalid(
            "carry admitted component identity",
            format!(
                "{}@{}::{} digest {} differs from selected {}@{}::{} digest {}",
                admission.package_id(),
                admission.package_version(),
                admission.component(),
                admission.component_digest(),
                artifact.package_id,
                artifact.package_version,
                artifact.component,
                artifact.digest
            ),
        ));
    }
    Ok(())
}

/// Translate a failure to read authored base digests into its stage failure.
pub(super) fn base_digests_stage_error(
    source: ComponentDeclarationError,
) -> ProductionDevStageError {
    const OPERATION: &str = "read authored base digests";
    match source.error_type() {
        ComponentDeclarationErrorType::Read | ComponentDeclarationErrorType::Parse => {
            ProductionDevStageError::owner(OPERATION, source.into())
        }
        ComponentDeclarationErrorType::ManifestInvalid
        | ComponentDeclarationErrorType::TemplateInvalid => {
            ProductionDevStageError::invalid(OPERATION, source.to_string())
        }
    }
}

/// Translate a failure to render a component declaration into its stage failure.
pub(super) fn declaration_stage_error(
    source: ComponentDeclarationError,
) -> ProductionDevStageError {
    match source.error_type() {
        ComponentDeclarationErrorType::Read => {
            ProductionDevStageError::owner("read component declaration template", source.into())
        }
        ComponentDeclarationErrorType::Parse => {
            ProductionDevStageError::owner("parse component declaration template", source.into())
        }
        ComponentDeclarationErrorType::ManifestInvalid
        | ComponentDeclarationErrorType::TemplateInvalid => {
            ProductionDevStageError::invalid("render component declaration", source.to_string())
        }
    }
}

pub(super) fn require_command_success(
    operation: &'static str,
    output: &std::process::Output,
) -> Result<(), ProductionDevStageError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(ProductionDevStageError::invalid(
        operation,
        if stderr.is_empty() {
            format!("command exited with {}", output.status)
        } else {
            format!("command exited with {}: {stderr}", output.status)
        },
    ))
}

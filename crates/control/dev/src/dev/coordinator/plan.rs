use super::{
    ComponentAdmission, ComponentDeclarationError, ComponentDeclarationErrorType,
    ProductionDevStageError, SelectedComponentArtifact,
};

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

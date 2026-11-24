use std::fmt;

pub const APPLY_PACKAGE_REFUSAL: &str = "apply-package-refused";
pub const PACKAGE_ARTIFACT_REQUIRED_REFUSAL: &str = "package-artifact-required";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPackageErrorType {
    ArtifactRequired,
}

#[derive(Debug)]
pub struct ApplyPackageError {
    coordinate: String,
}
impl ApplyPackageError {
    pub(super) fn artifact_required(coordinate: &str) -> Self {
        Self {
            coordinate: coordinate.into(),
        }
    }
    pub const fn error_type(&self) -> ApplyPackageErrorType {
        ApplyPackageErrorType::ArtifactRequired
    }
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }
}
impl fmt::Display for ApplyPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{APPLY_PACKAGE_REFUSAL} ({PACKAGE_ARTIFACT_REQUIRED_REFUSAL}): coordinate={}",
            self.coordinate
        )
    }
}
impl std::error::Error for ApplyPackageError {}

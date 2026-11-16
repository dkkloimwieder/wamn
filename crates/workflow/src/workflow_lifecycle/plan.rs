use super::{WorkflowLifecycleError, WorkflowLifecycleErrorType};

/// The key of one workflow version.
#[derive(Clone, Copy, Debug)]
pub struct WorkflowVersion<'a> {
    pub environment: &'a wamn_registry::EnvName,
    pub application: &'a wamn_catalog::ApplicationName,
    pub workflow_id: &'a wamn_catalog::canonical_name::CanonicalName,
    pub version: u32,
}

impl WorkflowVersion<'_> {
    pub(super) fn version(&self) -> Result<i32, WorkflowLifecycleError> {
        i32::try_from(self.version).map_err(|error| {
            WorkflowLifecycleError::with_source(
                WorkflowLifecycleErrorType::NotFound,
                format!(
                    "workflow {:?} version {} exceeds the catalog storage width",
                    self.workflow_id, self.version
                ),
                error,
            )
        })
    }

    pub(super) fn not_found(&self) -> WorkflowLifecycleError {
        WorkflowLifecycleError::new(
            WorkflowLifecycleErrorType::NotFound,
            format!(
                "workflow {:?} version {} of application {:?} is not authored in environment {:?}",
                self.workflow_id, self.version, self.application, self.environment
            ),
        )
    }
}

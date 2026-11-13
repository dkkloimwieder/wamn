//! The environment instance assigned after the development database is recreated.

use std::fmt;

/// The project has no environment projection to receive an instance.
pub const ENVIRONMENT_INSTANCE_WITHOUT_PROJECTION: &str =
    "environment-instance-assignment-without-projection";

/// Update only the instance and timestamp of the selected project.
pub fn assign_environment_instance_sql() -> &'static str {
    "UPDATE catalog.project_environments SET environment_instance = $2, projected_at = now() WHERE project_env_key = $1"
}

/// Refuse an assignment that did not update a projected project.
pub fn check_environment_instance_assignment(
    project_env_key: &str,
    affected_rows: u64,
) -> Result<(), EnvironmentInstanceAssignmentError> {
    if affected_rows == 0 {
        Err(EnvironmentInstanceAssignmentError {
            project_env_key: project_env_key.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// A refused assignment with the project and the existing provisioning remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInstanceAssignmentError {
    project_env_key: String,
}

impl fmt::Display for EnvironmentInstanceAssignmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{ENVIRONMENT_INSTANCE_WITHOUT_PROJECTION}: project_env_key={}: name the project when provisioning the project-env, so the control store carries its environment identity",
            self.project_env_key
        )
    }
}

impl std::error::Error for EnvironmentInstanceAssignmentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_projection_accepts_its_instance_assignment() {
        assert!(check_environment_instance_assignment("org-a", 1).is_ok());
    }

    #[test]
    fn an_absent_projection_keeps_the_refusal_and_remedy() {
        assert!(check_environment_instance_assignment("org-a", 0).is_err());
    }
}

//! The environment instance claimed after the development database is recreated.

use std::fmt;

/// The tenant has no environment projection to receive an instance.
pub const ENVIRONMENT_INSTANCE_WITHOUT_PROJECTION: &str =
    "environment-instance-claim-without-projection";

/// Update only the instance and timestamp of the selected tenant.
pub fn claim_environment_instance_sql() -> &'static str {
    "UPDATE catalog.tenant_environments SET environment_instance = $2, projected_at = now() WHERE env_key = $1"
}

/// Refuse a claim that did not update a projected tenant.
pub fn check_environment_instance_claim(
    env_key: &str,
    affected_rows: u64,
) -> Result<(), EnvironmentInstanceClaimError> {
    if affected_rows == 0 {
        Err(EnvironmentInstanceClaimError {
            env_key: env_key.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// A refused claim with the tenant and the existing provisioning remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInstanceClaimError {
    env_key: String,
}

impl fmt::Display for EnvironmentInstanceClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{ENVIRONMENT_INSTANCE_WITHOUT_PROJECTION}: env_key={}: name the tenant when provisioning the project-env, so the control store carries its environment identity",
            self.env_key
        )
    }
}

impl std::error::Error for EnvironmentInstanceClaimError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_projection_accepts_its_instance_claim() {
        assert!(check_environment_instance_claim("tenant-a", 1).is_ok());
    }

    #[test]
    fn an_absent_projection_keeps_the_refusal_and_remedy() {
        let error = check_environment_instance_claim("tenant-a", 0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "environment-instance-claim-without-projection: env_key=tenant-a: name the tenant when provisioning the project-env, so the control store carries its environment identity"
        );
    }
}

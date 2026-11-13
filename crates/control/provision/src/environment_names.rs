//! Resource names derived from validated environment coordinates.

use sha2::{Digest as _, Sha256};

use crate::ProvisionError;
use crate::validate_project_env;

/// The longest slug in a release name: the env name cut to this length.
pub const SLUG_LEN: usize = 24;
/// The hex characters of the coordinate hash in a release name (80 bits).
pub const HASH_LEN: usize = 20;

/// The name of shared environment resources: `e-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/project/env`. The name is at most 47
/// characters, inside Helm's bound of 53.
///
/// # Errors
///
/// When the coordinate is not valid.
pub fn environment_resource_name(
    org: &str,
    project: &str,
    env: &str,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    let hash = hex::encode(Sha256::digest(format!("{org}/{project}/{env}")));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("e-{slug}-{}", &hash[..HASH_LEN]))
}

/// The Helm release and host group of one application.
pub fn application_release_name(
    org: &str,
    project: &str,
    env: &str,
    application: &str,
) -> Result<String, ProvisionError> {
    validate_project_env(org, project, env)?;
    let hash = hex::encode(Sha256::digest(format!(
        "{org}/{project}/{env}/{application}"
    )));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("r-{slug}-{}", &hash[..HASH_LEN]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_names_are_bounded_and_refuse_invalid_coordinates() {
        let env = "a".repeat(30);
        let name = application_release_name("a", "wms", &env, "orders").unwrap();
        assert!(name.len() <= 47);
        assert_ne!(
            name,
            application_release_name("a", "wms", &env, "receiving").unwrap()
        );
        assert!(application_release_name("acme", "wms", "Bad Env", "orders").is_err());
        assert!(environment_resource_name("acme", "wms", "Bad Env").is_err());
    }
}

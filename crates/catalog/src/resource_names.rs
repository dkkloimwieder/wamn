//! Resource names derived from catalog application and package names.

use wamn_digest::sha256;

/// A scheduled application's service principal.
pub fn application_subject(
    key: &wamn_registry::ProjectEnvKey,
    package: &crate::PackageName,
) -> String {
    format!("wamn-app-{key}--{package}")
}

/// The Helm release of one application.
pub fn application_release_name(
    org: &str,
    project: &str,
    env: &wamn_registry::EnvName,
    application: &crate::ApplicationName,
) -> Result<String, wamn_registry::name::NameError> {
    wamn_registry::name::validate_coordinate("org", org)?;
    wamn_registry::name::validate_coordinate("project", project)?;
    let hash = hex::encode(sha256(
        format!("{org}/{env}/{project}/{application}").as_bytes(),
    ));
    let slug: String = env
        .as_str()
        .chars()
        .take(wamn_registry::name::SLUG_LEN)
        .collect();
    Ok(format!(
        "r-{slug}-{}",
        &hash[..wamn_registry::name::HASH_LEN]
    ))
}

/// The durable event consumer for an application and source.
pub fn application_event_consumer(
    application: &crate::ApplicationName,
    package: &crate::PackageName,
    entity: &str,
) -> String {
    let identity = format!(
        "{}:{application}{}:{package}{}:{entity}",
        application.as_str().len(),
        package.as_str().len(),
        entity.len(),
    );
    format!("application_{}", hex::encode(sha256(identity.as_bytes())))
}

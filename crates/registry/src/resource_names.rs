//! Resource names derived from catalog application and package names.

use wamn_digest::sha256;

/// A scheduled application's service principal.
pub fn application_subject(
    key: &crate::ProjectEnvKey,
    package: &wamn_package::PackageName,
) -> String {
    format!("wamn-app-{key}--{package}")
}

/// The Helm release of one application.
pub fn application_release_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    application: &crate::ApplicationName,
) -> String {
    let hash = hex::encode(sha256(
        format!("{org}/{env}/{project}/{application}").as_bytes(),
    ));
    let slug: String = env.as_str().chars().take(crate::name::SLUG_LEN).collect();
    format!("r-{slug}-{}", &hash[..crate::name::HASH_LEN])
}

/// The durable event consumer for an application and source.
pub fn application_event_consumer(
    application: &crate::ApplicationName,
    package: &wamn_package::PackageName,
    entity: &str,
) -> String {
    let identity = format!(
        "{}:{application}{}:{package}{}:{entity}",
        application.as_str().len(),
        package.as_str().len(),
        entity.len(),
    );
    format!("app_{}", hex::encode(sha256(identity.as_bytes())))
}

/// Name the durable pull consumer of an application's data-change workflow.
pub fn workflow_event_consumer(
    application: &crate::ApplicationName,
    workflow_id: &wamn_package::canonical_name::CanonicalName,
) -> String {
    let identity = format!("{}:{application}{workflow_id}", application.as_str().len());
    format!("workflow_{}", hex::encode(sha256(identity.as_bytes())))
}

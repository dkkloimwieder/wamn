use super::DevActivationIdentity;

/// Fingerprint of the standup a template belongs to.
///
/// A template is only pristine FOR THE ENVIRONMENT IT WAS TAKEN FROM. Point
/// `dev.json` at another database or another environment and the clone would be
/// someone else's provisioned state, so the run refuses instead.
///
/// Database NAMES and identity, never the URLs: a rotated credential does not
/// make a template stale, and a hash of a URL would say it did.
pub fn template_fingerprint(
    target_database: &str,
    system_database: &str,
    identity: &DevActivationIdentity,
) -> String {
    let fields = serde_json::json!({
        "target_database": target_database,
        "system_database": system_database,
        "org": identity.org,
        "project": identity.project,
        "environment": identity.environment,
        "project_env_key": identity.project_env_key(),
        "catalog": identity.catalog,
    });
    wamn_digest::canonical_json_sha256(&fields)
}

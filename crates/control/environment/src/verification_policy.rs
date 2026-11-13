//! Project authoritative environment policies into verification databases.

use anyhow::Context as _;
use wamn_release_control::verification_policy::read_authoritative_environment_policy;
use wamn_schema_control::BareSchemaName;

/// Result of projecting one authoritative policy into a verification database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentPolicyProjection {
    changed: bool,
    source_policy_org: Box<str>,
    environment: Box<str>,
    source_policy_hash: Box<str>,
}

impl EnvironmentPolicyProjection {
    /// Whether this invocation inserted or repaired the exact projected fact.
    pub const fn changed(&self) -> bool {
        self.changed
    }

    /// Organization that owns the authoritative source policy.
    pub fn source_policy_org(&self) -> &str {
        &self.source_policy_org
    }

    /// Environment-policy name carried by the projection.
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// Canonical SHA-256 of the full authoritative policy value.
    pub fn source_policy_hash(&self) -> &str {
        &self.source_policy_hash
    }
}

/// Read one authoritative policy and project only that fact into a verification
/// database. Both URLs remain caller-pinned deployment configuration; policy
/// contents are never accepted from the caller.
pub async fn project_environment_policy(
    system_database_url: &str,
    verification_database_url: &str,
    run_schema: &BareSchemaName,
    source_policy_org: &str,
    project_env_key: &str,
    environment: &str,
) -> anyhow::Result<EnvironmentPolicyProjection> {
    let source =
        read_authoritative_environment_policy(system_database_url, source_policy_org, environment)
            .await?;
    let mut target = wamn_postgres::connect(verification_database_url)
        .await
        .context("connect to the verification policy target")?;

    let changed = crate::reconcile_run_plane::converge_environment_policy(
        &mut target,
        run_schema,
        project_env_key,
        environment,
        &source,
        true,
    )
    .await;
    drop(target);
    let changed = changed?;
    Ok(EnvironmentPolicyProjection {
        changed,
        source_policy_org: source.source_policy_org,
        environment: source.policy.name.to_string().into_boxed_str(),
        source_policy_hash: source.source_policy_hash,
    })
}

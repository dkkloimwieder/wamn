//! Exact projection of one authoritative environment-policy fact.
//!
//! The system database remains the sole policy authority. This module reads
//! one full policy value there, hashes its canonical JSON once, and carries
//! that source identity and hash into a verification database. It does not
//! copy any other system or application state.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control_registry::{DurabilityClass, EnvPolicy};
use wamn_schema_control::BareSchemaName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthoritativeEnvironmentPolicy {
    pub(crate) source_policy_org: Box<str>,
    pub(crate) policy: EnvPolicy,
    pub(crate) source_policy_hash: Box<str>,
}

impl AuthoritativeEnvironmentPolicy {
    pub(crate) fn environment(&self) -> &str {
        self.policy.name.as_str()
    }

    pub(crate) const fn durability_class(&self) -> DurabilityClass {
        self.policy.durability_class
    }
}

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
    tenant_id: &str,
    environment: &str,
) -> anyhow::Result<EnvironmentPolicyProjection> {
    let source = read_authoritative_environment_policy(
        system_database_url,
        source_policy_org,
        environment,
        false,
    )
    .await?;
    let (target, connection) = tokio_postgres::connect(verification_database_url, NoTls)
        .await
        .context("connect to the verification policy target")?;
    let connection_task = tokio::spawn(connection);
    let changed = crate::reconcile_run_plane::converge_environment_policy(
        &target, run_schema, tenant_id, &source, true,
    )
    .await;
    drop(target);
    let driven = connection_task
        .await
        .context("join verification policy target")?;
    driven.context("drive verification policy target")?;
    let changed = changed?;
    Ok(EnvironmentPolicyProjection {
        changed,
        source_policy_org: source.source_policy_org,
        environment: source.policy.name.to_string().into_boxed_str(),
        source_policy_hash: source.source_policy_hash,
    })
}

pub(crate) async fn read_authoritative_environment_policy(
    system_database_url: &str,
    source_policy_org: &str,
    environment: &str,
    ensure_durability_schema: bool,
) -> anyhow::Result<AuthoritativeEnvironmentPolicy> {
    anyhow::ensure!(
        !source_policy_org.is_empty(),
        "source policy organization must not be empty"
    );
    anyhow::ensure!(!environment.is_empty(), "environment must not be empty");
    let (source, connection) = tokio_postgres::connect(system_database_url, NoTls)
        .await
        .context("connect to the authoritative environment-policy store")?;
    let connection_task = tokio::spawn(connection);
    let loaded = async {
        source
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("assume the environment-policy owner")?;
        if ensure_durability_schema {
            crate::env_policies::ensure_env_policy_durability_schema(&source).await?;
        }
        let policy = if ensure_durability_schema {
            crate::env_policies::read_env_policy(&source, source_policy_org, environment).await?
        } else {
            crate::env_policies::observe_env_policy(&source, source_policy_org, environment).await?
        }
        .with_context(|| {
            format!(
                "environment-policy-source-absent: environment {environment:?} names no policy owned by organization {source_policy_org:?}"
            )
        })?;
        authoritative_environment_policy(source_policy_org, policy)
    }
    .await;
    drop(source);
    let driven = connection_task
        .await
        .context("join authoritative environment-policy connection")?;
    driven.context("drive authoritative environment-policy connection")?;
    loaded
}

fn authoritative_environment_policy(
    source_policy_org: &str,
    policy: EnvPolicy,
) -> anyhow::Result<AuthoritativeEnvironmentPolicy> {
    let value =
        serde_json::to_value(&policy).context("serialize authoritative environment policy")?;
    let source_policy_hash = wamn_execution_contract::canonical_json_sha256(&value);
    Ok(AuthoritativeEnvironmentPolicy {
        source_policy_org: source_policy_org.to_owned().into_boxed_str(),
        policy,
        source_policy_hash: source_policy_hash.into_boxed_str(),
    })
}

#[cfg(test)]
mod tests {
    use wamn_control_registry::{Env, EnvPolicy, RecoveryDomain};

    use super::authoritative_environment_policy;

    fn policy() -> EnvPolicy {
        EnvPolicy {
            name: Env::new("dev"),
            recovery_domain: RecoveryDomain::Own,
            promotion_rank: 10,
            instances: 1,
            storage: "2Gi".to_owned(),
            cpu: "200m".to_owned(),
            memory: "256Mi".to_owned(),
            image: "postgres:18".to_owned(),
            backup_cadence: String::new(),
            wal_retention: String::new(),
            hibernation: "eligible".to_owned(),
            durability_class: wamn_control_registry::DurabilityClass::Standard,
        }
    }

    #[test]
    fn source_hash_covers_policy_fields_outside_the_projected_subset() {
        let first = authoritative_environment_policy("acme", policy()).unwrap();
        let mut changed = policy();
        changed.storage = "4Gi".to_owned();
        let changed = authoritative_environment_policy("acme", changed).unwrap();
        assert_ne!(first.source_policy_hash, changed.source_policy_hash);
    }
}

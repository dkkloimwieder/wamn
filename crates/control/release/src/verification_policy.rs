//! Exact projection of one authoritative environment-policy fact.
//!
//! The system database remains the sole policy authority. This module reads
//! one full policy value there, hashes its canonical JSON once, and carries
//! that source identity and hash into a verification database. It does not
//! copy any other system or application state.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control_registry::{DurabilityClass, EnvPolicy};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoritativeEnvironmentPolicy {
    pub source_policy_org: Box<str>,
    pub policy: EnvPolicy,
    pub source_policy_hash: Box<str>,
}

impl AuthoritativeEnvironmentPolicy {
    pub const fn durability_class(&self) -> DurabilityClass {
        self.policy.durability_class
    }
}

pub async fn read_authoritative_environment_policy(
    system_database_url: &str,
    source_policy_org: &str,
    environment: &str,
    require_durability_column: bool,
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
        let policy = if require_durability_column {
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

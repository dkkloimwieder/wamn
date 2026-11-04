use super::{DurabilityClass, EnvPolicy};
use anyhow::Context as _;

pub(super) fn authoritative_environment_policy(
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

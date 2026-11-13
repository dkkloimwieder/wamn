use super::{OrgEnvPolicy, Template};
use wamn_control_provision::{Org, RecoveryDomain};
use wamn_registry::identifiers;

pub(super) fn stamped_org(
    template: &Template,
    org_id: &str,
    pool: &str,
) -> anyhow::Result<(Org, Vec<OrgEnvPolicy>)> {
    anyhow::ensure!(identifiers::valid_project(org_id), "invalid org id");
    let (org, policies) = template.stamp(org_id, pool);
    if let Some(pool) = org.placement.pool() {
        anyhow::ensure!(
            pool.len() <= 63
                && !pool.is_empty()
                && pool
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && pool
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && pool
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "invalid pool name"
        );
    }
    let mut names = std::collections::HashSet::new();
    for policy in &policies {
        anyhow::ensure!(
            identifiers::project_id_reason(policy.policy.name.as_str()).is_none()
                && names.insert(policy.policy.name.as_str()),
            "invalid or duplicate environment policy"
        );
    }
    for policy in &policies {
        let mut visited = std::collections::HashSet::new();
        let mut current = &policy.policy;
        while let RecoveryDomain::SharedWith(target) = &current.recovery_domain {
            anyhow::ensure!(
                visited.insert(current.name.as_str()),
                "environment policy recovery cycle"
            );
            current = &policies
                .iter()
                .find(|entry| entry.policy.name == *target)
                .ok_or_else(|| anyhow::anyhow!("unknown recovery environment"))?
                .policy;
        }
    }
    Ok((org, policies))
}

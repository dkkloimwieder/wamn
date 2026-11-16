use super::{OrgEnvPolicy, Template};
use wamn_control_provision::{Org, RecoveryDomain};

pub(super) fn stamped_org(
    template: &Template,
    org_id: &wamn_registry::OrgId,
    pool: &str,
) -> anyhow::Result<(Org, Vec<OrgEnvPolicy>)> {
    let (org, policies) = template.stamp(org_id.clone(), pool);
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
            names.insert(&policy.policy.name),
            "duplicate environment policy"
        );
    }
    for policy in &policies {
        let mut visited = std::collections::HashSet::new();
        let mut current = &policy.policy;
        while let RecoveryDomain::SharedWith(target) = &current.recovery_domain {
            anyhow::ensure!(
                visited.insert(&current.name),
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

use super::{OrgEnvPolicy, Registry, SCHEMA_VERSION, Template};

pub(super) fn fmt_issues(issues: &[wamn_control_registry::Issue]) -> String {
    issues
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn stamped_org(
    template: &Template,
    org_id: &str,
    pool: &str,
) -> anyhow::Result<(wamn_control_registry::Org, Vec<OrgEnvPolicy>)> {
    let (org, stamped) = template.stamp(org_id, pool);

    // Validate the org id (slug / reserved-prefix), placement, and the stamped
    // policy set by running the one-org registry through the model's validator.
    let reg = Registry {
        schema_version: SCHEMA_VERSION.to_string(),
        env_policies: stamped.clone(),
        orgs: vec![org.clone()],
        projects: Vec::new(),
        project_envs: Vec::new(),
    };
    reg.validate()
        .map_err(|issues| anyhow::anyhow!("invalid org: {}", fmt_issues(&issues)))?;

    Ok((org, stamped))
}

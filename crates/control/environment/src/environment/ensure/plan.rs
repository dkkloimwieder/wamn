use super::Triple;

/// `wamn-pat-management-author-<org>--<project>--<env>`.
pub(in super::super) fn management_author_pat_name(triple: &Triple) -> String {
    format!(
        "wamn-pat-management-author-{}",
        wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env)
    )
}

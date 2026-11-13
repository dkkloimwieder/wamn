use super::Triple;

/// `wamn-pat-management-author-<org>--<project>--<env>`.
pub(in super::super) fn management_author_pat_name(triple: &Triple) -> String {
    format!(
        "wamn-pat-management-author-{}",
        wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env)
    )
}

/// The account and grants used by hosts to read the environment credential store.
pub fn credential_access(account: &str, secret: &str) -> [(&'static str, serde_json::Value); 3] {
    use serde_json::json;
    [
        (
            "serviceaccount",
            json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":account}}),
        ),
        (
            "role",
            json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role","metadata":{"name":account},"rules":[{"apiGroups":[""],"resources":["secrets"],"resourceNames":[secret],"verbs":["get"]}]}),
        ),
        (
            "rolebinding",
            json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":account},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":account},"subjects":[{"kind":"ServiceAccount","name":account}]}),
        ),
    ]
}

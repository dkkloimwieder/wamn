use super::{
    CredentialDatabase, CredentialGeneration, EnvironmentDocument, HostCredential, Platform,
    PrivateDirectory, Triple, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadRoleFamily,
};

/// Prepare the Secret and database selection of one environment credential.
pub(super) fn credential_request(
    credential: HostCredential,
    platform: &Platform,
    document: &EnvironmentDocument,
    private: &PrivateDirectory,
    cluster: &str,
    project_url: &str,
) -> WorkloadActionRequest {
    WorkloadActionRequest {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        system_database_url: Some(platform.system_database_url.clone()),
        target_admin_database_url: (credential.database == CredentialDatabase::Project)
            .then(|| project_url.to_owned()),
        cluster: Some(cluster.to_owned()),
        db_host: platform.database_host.clone(),
        db_port: platform.database_port,
        namespace: platform.target.namespace.clone(),
        action: WorkloadGenerationAction {
            family: credential.family,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(private.path(credential.file_name)),
        emit_role_sql: None,
        control_administration_patch: (credential.family == WorkloadRoleFamily::Administration)
            .then(|| private.path("control-administration-patch.json")),
    }
}

/// `wamn-pat-management-author-<org>--<project>--<env>`.
pub(in super::super) fn management_author_pat_name(triple: &Triple) -> String {
    format!(
        "wamn-pat-management-author-{}",
        wamn_control_registry::project_env_key(&triple.org, &triple.project, &triple.env)
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

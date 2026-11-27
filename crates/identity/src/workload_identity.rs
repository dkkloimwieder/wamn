//! Rules for admitting identities authenticated by the workload token reviewer.

use sqlx::PgConnection;
use wamn_registry::{EnvName, OrgId, ProjectId, name};

use wamn_identity_protocol::nats::{SignUser, UserPurpose};

use crate::nats::{self, Permissions, SubjectPermission};

/// Control authenticates in its organization namespace.
pub fn org(namespace: String, account: &str) -> Option<OrgId> {
    let org = OrgId::try_from(namespace).ok()?;
    (account == name::control_service_account(&org)).then_some(org)
}

/// Registry mapping for the namespace authenticated by TokenReview.
#[derive(Debug)]
pub struct Env {
    pub org: OrgId,
    pub env: EnvName,
    pub active: bool,
}

pub async fn mapped_env(
    client: &mut PgConnection,
    namespace: &str,
) -> Result<Option<Env>, sqlx::Error> {
    // A standalone read finishes before any further Kubernetes or KMS call.
    sqlx::query_as::<_, (String, String, bool)>(
        "SELECT e.org, e.env, e.active AND o.active
         FROM registry.env e JOIN registry.org o ON o.id = e.org WHERE e.namespace = $1",
    )
    .bind(namespace)
    .fetch_optional(client)
    .await?
    .map(|(org, env, active)| {
        Ok(Env {
            org: wamn_postgres::decode_text(org, 0)?,
            env: wamn_postgres::decode_text(env, 1)?,
            active,
        })
    })
    .transpose()
}

pub fn account_allowed(env: &Env, account: &str, org: &OrgId, name: &EnvName) -> bool {
    env.active
        && &env.org == org
        && &env.env == name
        && account == wamn_registry::name::ENV_WORKER_ACCOUNT
}

/// Resolve the project named by the purpose, if any.
pub fn project(purpose: &UserPurpose) -> Option<&ProjectId> {
    match purpose {
        UserPurpose::Cdc { project } | UserPurpose::Recovery { project } => Some(project),
        _ => None,
    }
}

pub fn user_allowed(env: &Env, account: &str, request: &SignUser, project_active: bool) -> bool {
    if !env.active || env.org != request.org || env.env != request.env {
        return false;
    }
    match &request.purpose {
        UserPurpose::Recovery { project } => {
            project_active
                && account == name::project_operation_account(&env.org, project, &env.env)
        }
        UserPurpose::Cdc { .. } => project_active && account == name::ENV_WORKER_ACCOUNT,
        UserPurpose::Workflow
        | UserPurpose::Ingress
        | UserPurpose::WorkflowNative
        | UserPurpose::StreamAdmin
        | UserPurpose::HostData => account == name::ENV_WORKER_ACCOUNT,
    }
}

/// Check that a project and its deployment are both active.
pub async fn project_active(
    client: &mut PgConnection,
    env: &Env,
    project: &ProjectId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT FROM registry.project_env d JOIN registry.project p ON p.org = d.org AND p.id = d.project
             WHERE d.org = $1 AND d.env = $2 AND d.project = $3 AND d.active AND p.active
         )",
    )
    .bind(env.org.as_str())
    .bind(env.env.as_str())
    .bind(project.as_str())
    .fetch_one(client)
    .await
}

pub fn host(env: &Env, account: &str, pod: Option<&str>) -> Option<String> {
    if !env.active || account != name::env_service_account(&env.org, &env.env) {
        return None;
    }
    // The pod ID must fit one host subject token.
    pod.filter(|pod| {
        !pod.is_empty()
            && pod.len() <= 128
            && pod
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
    .map(str::to_owned)
}

pub fn env_permissions(request: &SignUser, inbox: &str) -> (String, Permissions) {
    let mut subscribe = vec![format!("{inbox}.>")];
    let workload = || {
        [
            "$JS.API.STREAM.INFO.*",
            "$JS.API.CONSUMER.INFO.>",
            "$JS.API.CONSUMER.CREATE.>",
            "$JS.API.CONSUMER.DELETE.>",
            "$JS.API.CONSUMER.MSG.NEXT.>",
            "$JS.ACK.>",
        ]
        .map(str::to_owned)
        .into()
    };
    let (name_prefix, publish) = match &request.purpose {
        UserPurpose::Workflow => ("workflow".into(), workload()),
        UserPurpose::Ingress => {
            subscribe.push("_nats_push.>".into());
            ("ingress".into(), workload())
        }
        UserPurpose::WorkflowNative => ("workflow-native".into(), workload()),
        UserPurpose::Cdc { project } => (
            format!("cdc-{project}"),
            vec![name::stream_subjects(&request.org, project, &request.env)],
        ),
        UserPurpose::Recovery { project } => {
            let stream = name::stream_name(&request.org, project, &request.env);
            (
                format!("recovery-{project}"),
                vec![
                    format!("$JS.API.STREAM.INFO.{stream}"),
                    format!("$JS.API.STREAM.MSG.GET.{stream}"),
                ],
            )
        }
        UserPurpose::StreamAdmin => (
            "stream-admin".into(),
            [
                "$JS.API.STREAM.INFO.*",
                "$JS.API.STREAM.CREATE.*",
                "$JS.API.STREAM.DELETE.*",
                "$JS.API.STREAM.PURGE.*",
                "$JS.API.CONSUMER.INFO.>",
                "$JS.API.CONSUMER.CREATE.>",
            ]
            .map(str::to_owned)
            .into(),
        ),
        UserPurpose::HostData => ("wasmcloud-data".into(), Vec::new()),
    };
    let name = format!(
        "{name_prefix}-{}",
        name::env_key(&request.org, &request.env)
    );
    let permissions = Permissions {
        pub_: SubjectPermission {
            deny: if publish.is_empty() {
                vec![">".into()]
            } else {
                Vec::new()
            },
            allow: publish,
        },
        sub: SubjectPermission {
            allow: subscribe,
            deny: Vec::new(),
        },
        resp: None,
    };
    (name, permissions)
}

/// The subjects granted to an authenticated host.
pub fn host_permissions(pod: &str, inbox_prefix: &str) -> Permissions {
    Permissions {
        pub_: SubjectPermission {
            allow: vec![
                format!("runtime.host.{pod}.__control.ready"),
                format!("runtime.operator.heartbeat.{pod}"),
                format!("{inbox_prefix}.>"),
                format!("{}.>", nats::RUNTIME_OPERATOR_INBOX),
            ],
            deny: Vec::new(),
        },
        sub: SubjectPermission {
            allow: vec![format!("runtime.host.{pod}.>"), format!("{inbox_prefix}.>")],
            deny: Vec::new(),
        },
        resp: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> Env {
        Env {
            org: "acme".parse().unwrap(),
            env: "dev".parse().unwrap(),
            active: true,
        }
    }

    #[test]
    fn account_management_is_bound_to_the_active_mapped_env_worker() {
        let mut env = mapping();
        let worker = name::ENV_WORKER_ACCOUNT;
        assert!(account_allowed(&env, worker, &env.org, &env.env));
        assert!(!account_allowed(
            &env,
            worker,
            &env.org,
            &"prod".parse().unwrap()
        ));
        assert!(!account_allowed(
            &env,
            worker,
            &"other".parse().unwrap(),
            &env.env
        ));
        assert!(!account_allowed(&env, "default", &env.org, &env.env));
        env.active = false;
        assert!(!account_allowed(&env, worker, &env.org, &env.env));
    }

    #[test]
    fn user_purpose_must_match_the_account_and_active_project() {
        let mapping = mapping();
        let project: ProjectId = "billing".parse().unwrap();
        let project_account = name::project_operation_account(&mapping.org, &project, &mapping.env);
        let mut request = SignUser {
            org: mapping.org.clone(),
            env: mapping.env.clone(),
            purpose: UserPurpose::Recovery { project },
            public_key: String::new(),
        };
        let worker = name::ENV_WORKER_ACCOUNT;
        assert!(user_allowed(&mapping, &project_account, &request, true));
        assert!(!user_allowed(&mapping, &project_account, &request, false));
        assert!(!user_allowed(&mapping, worker, &request, true));
        request.purpose = UserPurpose::Cdc {
            project: "billing".parse().unwrap(),
        };
        assert!(user_allowed(&mapping, worker, &request, true));
        assert!(!user_allowed(&mapping, &project_account, &request, true));
        request.purpose = UserPurpose::StreamAdmin;
        assert!(user_allowed(&mapping, worker, &request, false));
        assert!(!user_allowed(&mapping, &project_account, &request, false));
    }

    #[test]
    fn scheduler_uses_the_mapped_host_account() {
        let mut env = mapping();
        let host_account = name::env_service_account(&env.org, &env.env);
        assert_eq!(
            host(&env, &host_account, Some("pod-0")).as_deref(),
            Some("pod-0")
        );
        assert!(host(&env, name::ENV_WORKER_ACCOUNT, Some("pod-0")).is_none());
        assert!(host(&env, &host_account, Some("pod.*")).is_none());
        env.active = false;
        assert!(host(&env, &host_account, Some("pod-0")).is_none());
    }
}

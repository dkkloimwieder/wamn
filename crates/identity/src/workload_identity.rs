//! Rules for admitting identities authenticated by the workload token reviewer.

use wamn_registry::{EnvName, OrgId, name};

use wamn_identity_protocol::nats::{SignUser, UserPurpose};

use crate::nats::{self, Permissions, SubjectPermission};

/// The service account allowed to use a protected route.
#[derive(Clone, Copy, Debug)]
pub enum Role {
    Control,
    Worker,
}

pub fn org(namespace: String, account: &str, role: Role) -> Option<OrgId> {
    let org = OrgId::try_from(namespace).ok()?;
    let allowed = match role {
        Role::Control => name::control_service_account(&org),
        Role::Worker => name::operation(&org),
    };
    (account == allowed).then_some(org)
}

pub fn host(namespace: String, account: &str, pod: Option<String>) -> Option<String> {
    let org = OrgId::try_from(namespace).ok()?;
    let env = account
        .rsplit_once("--")
        .and_then(|(_, env)| env.parse::<EnvName>().ok())?;
    if account != name::env_service_account(&org, &env) {
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

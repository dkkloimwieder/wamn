//! Command requirements used by install Jobs and their rendered Roles.
use super::job::{Access, JobRequest, PolicyRule};
use anyhow::{Context as _, ensure};
use std::collections::BTreeMap;

/// One namespace-scoped resource family used by a command.
#[derive(Debug)]
pub struct Permission {
    pub namespace: &'static str,
    pub group: &'static str,
    pub resources: &'static [&'static str],
    pub verbs: &'static [&'static str],
}
/// The source of a variable exported by an exact install script.
#[derive(Debug)]
pub enum Source {
    File(&'static str),
    Secret {
        namespace: &'static str,
        name: &'static str,
        key: &'static str,
    },
}
/// A script export and the input that supplies it.
#[derive(Debug)]
pub struct Export {
    pub variable: &'static str,
    pub source: Source,
}
/// One command's Kubernetes access and required environment.
#[derive(Debug)]
pub struct Contract {
    pub program: &'static str,
    pub verb: &'static str,
    pub access: &'static [Permission],
    pub required: &'static [&'static str],
    pub credential_stdout: bool,
    pub files: &'static [&'static str],
    pub exports: &'static [Export],
    script: Option<&'static str>,
}
const EMPTY: Contract = Contract {
    program: "",
    verb: "",
    access: &[],
    required: &[],
    credential_stdout: false,
    files: &[],
    exports: &[],
    script: None,
};
const ALL: &[&str] = &[
    "get", "list", "watch", "create", "patch", "update", "delete",
];
const APPLY: &[Permission] = &[
    Permission {
        namespace: "platform",
        group: "postgresql.cnpg.io",
        resources: &["clusters", "databases", "publications"],
        verbs: &["get", "list", "watch", "create", "patch"],
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["secrets", "services", "configmaps"],
        verbs: ALL,
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["pods"],
        verbs: &["get", "list", "watch"],
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["pods/log"],
        verbs: &["get"],
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["events"],
        verbs: &["list"],
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["serviceaccounts"],
        verbs: &["get", "create"],
    },
    Permission {
        namespace: "hosts",
        group: "rbac.authorization.k8s.io",
        resources: &["roles", "rolebindings"],
        verbs: &["get", "create"],
    },
    Permission {
        namespace: "hosts",
        group: "apps",
        resources: &["deployments", "replicasets"],
        verbs: ALL,
    },
    Permission {
        namespace: "hosts",
        group: "networking.k8s.io",
        resources: &["networkpolicies"],
        verbs: ALL,
    },
    Permission {
        namespace: "hosts",
        group: "runtime.wasmcloud.dev",
        resources: &["workloaddeployments"],
        verbs: ALL,
    },
];
const CONNECTIONS: &[Permission] = &[Permission {
    namespace: "hosts",
    group: "",
    resources: &["secrets"],
    verbs: &["get", "create", "update"],
}];
const CREDENTIALS: &[Permission] = &[Permission {
    namespace: "hosts",
    group: "",
    resources: &["secrets"],
    verbs: &["get", "create", "patch"],
}];
const OPERATOR: &[Permission] = &[
    Permission {
        namespace: "identity",
        group: "",
        resources: &["secrets"],
        verbs: &["get"],
    },
    Permission {
        namespace: "hosts",
        group: "",
        resources: &["secrets"],
        verbs: &["get", "create", "patch"],
    },
];
const INVITE: &[Permission] = &[Permission {
    namespace: "identity",
    group: "",
    resources: &["secrets"],
    verbs: &["get"],
}];
const WEB: &[Permission] = &[Permission {
    namespace: "hosts",
    group: "",
    resources: &["secrets"],
    verbs: &["get", "list"],
}];

const CONTRACTS: &[Contract] = &[
    Contract {
        program: "wamn-ctl",
        verb: "provision-system",
        required: &["WAMN_SYSTEM_ADMIN_URL"],
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "provision-org",
        required: &["WAMN_SYSTEM_ADMIN_URL"],
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "provision-project-env",
        required: &["WAMN_SYSTEM_ADMIN_URL", "WAMN_TARGET_ADMIN_DATABASE_URL"],
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "provision-identity-issuer",
        required: &["WAMN_SYSTEM_ADMIN_URL"],
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "invite",
        required: &[
            "WAMN_SYSTEM_ADMIN_URL",
            "WAMN_PAT_ISSUER",
            "WAMN_PAT_CLIENT_CERT",
            "WAMN_PAT_CLIENT_KEY",
            "WAMN_NATS_URL",
            "WAMN_NATS_TLS_CA",
            "WAMN_NATS_TLS_CERT",
            "WAMN_NATS_TLS_KEY",
        ],
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "env apply",
        files: &[
            "/inputs/environment.k",
            "/inputs/kubeconfig",
            "/inputs/registry.json",
            "/inputs/Chart.yaml",
            "/inputs/values.yaml",
        ],
        required: &[
            "WAMN_APPLY_DATABASE_URL",
            "WAMN_SYSTEM_ADMIN_URL",
            "WAMN_RELEASE_CHART",
            "WAMN_RELEASE_ARTIFACT_BASE",
            "WAMN_REGISTRY_AUTH_FILE",
            "WAMN_NATS_URL",
            "WAMN_NATS_TLS_CA",
            "WAMN_NATS_TLS_CERT",
            "WAMN_NATS_TLS_KEY",
        ],
        access: APPLY,
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "env connections",
        files: &[
            "/inputs/environment.k",
            "/inputs/kubeconfig",
            "/inputs/registry.json",
        ],
        required: &[
            "WAMN_CONTROL_DATABASE_URL",
            "WAMN_RELEASE_CHART",
            "WAMN_RELEASE_ARTIFACT_BASE",
            "WAMN_REGISTRY_AUTH_FILE",
            "WAMN_NATS_URL",
            "WAMN_NATS_TLS_CA",
            "WAMN_NATS_TLS_CERT",
            "WAMN_NATS_TLS_KEY",
        ],
        access: CONNECTIONS,
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "control credentials",
        exports: &[Export {
            variable: "WAMN_SYSTEM_ADMIN_URL",
            source: Source::File("/etc/wamn/postgres/uri"),
        }],
        files: &["/inputs/kubeconfig", "/etc/wamn/postgres/uri"],
        required: &[
            "WAMN_SYSTEM_ADMIN_URL",
            "ORG",
            "OWNER_EMAIL",
            "DATABASE_HOST",
        ],
        access: CREDENTIALS,
        script: Some(super::control::PROVISION),
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "copy operator",
        files: &["/inputs/metadata.json"],
        required: &["OPERATOR"],
        access: OPERATOR,
        script: Some(super::control::COPY_OPERATOR),
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "invite",
        exports: &[
            Export {
                variable: "WAMN_SYSTEM_ADMIN_URL",
                source: Source::File("/etc/wamn/postgres/uri"),
            },
            Export {
                variable: "WAMN_PAT_CLIENT_CERT",
                source: Source::Secret {
                    namespace: "identity",
                    name: "operator-$ORG",
                    key: "tls.crt",
                },
            },
            Export {
                variable: "WAMN_PAT_CLIENT_KEY",
                source: Source::Secret {
                    namespace: "identity",
                    name: "operator-$ORG",
                    key: "tls.key",
                },
            },
            Export {
                variable: "WAMN_PAT_SERVER_CA",
                source: Source::Secret {
                    namespace: "identity",
                    name: "identity-tls",
                    key: "ca.crt",
                },
            },
        ],
        files: &["/inputs/kubeconfig"],
        required: &[
            "WAMN_SYSTEM_ADMIN_URL",
            "WAMN_PAT_ISSUER",
            "WAMN_PAT_CLIENT_CERT",
            "WAMN_PAT_CLIENT_KEY",
            "WAMN_NATS_URL",
            "WAMN_NATS_TLS_CA",
            "WAMN_NATS_TLS_CERT",
            "WAMN_NATS_TLS_KEY",
            "ORG",
            "OWNER_EMAIL",
        ],
        access: INVITE,
        script: Some(super::invite::INVITE),
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "web upload",
        exports: &[Export {
            variable: "WAMN_CONTROL_DATABASE_URL",
            source: Source::File("/etc/wamn/registry-reader/url"),
        }],
        files: &[
            "/inputs/kubeconfig",
            "/inputs/registry.json",
            "/inputs/Chart.yaml",
            "/inputs/values.yaml",
        ],
        required: &[
            "WAMN_CONTROL_DATABASE_URL",
            "WAMN_RELEASE_CHART",
            "WAMN_REGISTRY_AUTH_FILE",
            "WAMN_NATS_URL",
            "WAMN_NATS_TLS_CA",
            "WAMN_NATS_TLS_CERT",
            "WAMN_NATS_TLS_KEY",
        ],
        access: WEB,
        script: Some(super::web::UPLOAD),
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "web upload --platform-ui",
        files: &[
            "/inputs/chart/Chart.yaml",
            "/inputs/chart/platform-build.json",
            "/inputs/deploy/platform/release/Chart.yaml",
            "/inputs/apps/platform_ui/dist/index.html",
        ],
        required: &["WAMN_RELEASE_CHART", "ORG", "BUCKET"],
        script: Some(super::web::PLATFORM_UI),
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "owner state",
        exports: &[Export {
            variable: "PGPASSWORD",
            source: Source::File("/etc/wamn/postgres/password"),
        }],
        files: &["/inputs/state.sql"],
        required: &[
            "PGHOST",
            "PGUSER",
            "PGDATABASE",
            "PGPASSWORD",
            "OWNER_EMAIL",
        ],
        script: Some(super::invite::OWNER_STATE),
        ..EMPTY
    },
    Contract {
        program: "wamn-identity",
        verb: "publish",
        required: &["WAMN_IDENTITY_ISSUER", "WAMN_IDENTITY_DATABASE_URL"],
        ..EMPTY
    },
    Contract {
        program: "wamn-identity",
        verb: "activate",
        required: &["WAMN_IDENTITY_ISSUER", "WAMN_IDENTITY_DATABASE_URL"],
        ..EMPTY
    },
    Contract {
        program: "psql",
        verb: "--no-psqlrc",
        required: &["PGHOST", "PGUSER", "PGDATABASE", "PGPASSWORD"],
        ..EMPTY
    },
    Contract {
        program: "curl",
        verb: "--fail",
        files: &["/inputs/ca.crt"],
        ..EMPTY
    },
    Contract {
        program: "curl",
        verb: "--silent",
        files: &["/etc/edge/tls.crt"],
        ..EMPTY
    },
    Contract {
        program: "wamn-edge",
        verb: "--config",
        files: &["/inputs/web.json"],
        ..EMPTY
    },
];

/// Resolve the executable and verb from the actual container command.
pub fn resolve(command: &[String], arguments: &[String]) -> anyhow::Result<&'static Contract> {
    let program = command
        .first()
        .map_or("wamn-ctl", String::as_str)
        .rsplit('/')
        .next()
        .unwrap();
    ensure!(
        command.len() <= 1 || (program == "sh" && command.len() == 2 && command[1] == "-ec"),
        "unknown install executable arguments"
    );
    if program == "sh"
        && arguments
            .first()
            .is_some_and(|script| script == super::job::HANDOFF)
    {
        ensure!(
            command.get(1).is_some_and(|flag| flag == "-ec"),
            "unknown install shell command"
        );
        return resolve(
            &["wamn-ctl".into()],
            arguments.get(2..).context("Secret handoff has no verb")?,
        );
    }
    CONTRACTS
        .iter()
        .find(|contract| {
            contract.program == program
                && match contract.script {
                    Some(script) => {
                        command.get(1).is_some_and(|flag| flag == "-ec")
                            && arguments.first().is_some_and(|actual| actual == script)
                    }
                    None => {
                        contract
                            .verb
                            .split_whitespace()
                            .zip(arguments)
                            .all(|(expected, actual)| expected == actual)
                            && arguments.len() >= contract.verb.split_whitespace().count()
                    }
                }
        })
        .with_context(|| format!("unknown install command {program}; no command contract"))
}

/// Resolve a Job before its Secret handoff wrapper is rendered.
pub fn request(request: &JobRequest) -> anyhow::Result<&'static Contract> {
    resolve(&request.command, &request.arguments)
}

/// Render namespace Roles from the command's sole permission declaration.
pub fn access(job: &JobRequest) -> anyhow::Result<Vec<Access>> {
    let mut namespaces: BTreeMap<&str, Vec<PolicyRule>> = BTreeMap::new();
    for permission in request(job)?.access {
        namespaces
            .entry(permission.namespace)
            .or_default()
            .push(PolicyRule {
                api_groups: vec![permission.group.into()],
                resources: permission
                    .resources
                    .iter()
                    .map(|value| (*value).into())
                    .collect(),
                verbs: permission
                    .verbs
                    .iter()
                    .map(|value| (*value).into())
                    .collect(),
            });
    }
    Ok(namespaces
        .into_iter()
        .map(|(namespace, rules)| Access {
            namespace: namespace.into(),
            rules,
        })
        .collect())
}

//! Command permissions used by bootstrap Jobs and their rendered Roles.
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
/// One command's Kubernetes access.
#[derive(Debug)]
pub struct Contract {
    pub program: &'static str,
    pub verb: &'static str,
    pub access: &'static [Permission],
    script: Option<&'static str>,
}
const EMPTY: Contract = Contract {
    program: "",
    verb: "",
    access: &[],
    script: None,
};

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

const CONTRACTS: &[Contract] = &[
    Contract {
        program: "wamn-ctl",
        verb: "provision-system",
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "provision-org",
        ..EMPTY
    },
    Contract {
        program: "wamn-ctl",
        verb: "provision-identity-issuer",
        ..EMPTY
    },
    Contract {
        program: "sh",
        verb: "control credentials",
        access: CREDENTIALS,
        script: Some(super::control::PROVISION),
    },
    Contract {
        program: "sh",
        verb: "copy operator",
        access: OPERATOR,
        script: Some(super::control::COPY_OPERATOR),
    },
    Contract {
        program: "wamn-identity",
        verb: "publish",
        ..EMPTY
    },
    Contract {
        program: "wamn-identity",
        verb: "activate",
        ..EMPTY
    },
    Contract {
        program: "psql",
        verb: "--no-psqlrc",
        ..EMPTY
    },
    Contract {
        program: "curl",
        verb: "--fail",
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

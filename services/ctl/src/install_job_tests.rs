//! Compare the production install requests with the contracts beside their CLI arguments.

use std::collections::BTreeSet;

use wamn_control::platform::job::{JobRequest, JobSecret, install_job_requests};

#[derive(Debug)]
pub(crate) struct Environment {
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
}

fn contract(verb: &str) -> Environment {
    use crate::{env_verbs, identity_verbs, package_verbs, provisioning_verbs, release_verbs, web};
    match verb {
        "provision-system" => provisioning_verbs::PROVISION_SYSTEM_INSTALL_ENV,
        "provision-org" => provisioning_verbs::PROVISION_ORG_INSTALL_ENV,
        "provision-project-env" => provisioning_verbs::PROVISION_PROJECT_ENV_INSTALL_ENV,
        "provision-identity-issuer" => identity_verbs::IDENTITY_ISSUER_INSTALL_ENV,
        "invite" => identity_verbs::INVITE_INSTALL_ENV,
        "apply-package" => package_verbs::APPLY_PACKAGE_INSTALL_ENV,
        "reconcile-package-data-access" => package_verbs::RECONCILE_PACKAGE_DATA_ACCESS_INSTALL_ENV,
        "reconcile-replica-identity" => package_verbs::RECONCILE_REPLICA_IDENTITY_INSTALL_ENV,
        "publish-release" => release_verbs::PUBLISH_RELEASE_INSTALL_ENV,
        "env apply" | "env connections" => env_verbs::ENV_APPLY_INSTALL_ENV,
        "web upload" => web::WEB_UPLOAD_INSTALL_ENV,
        "web upload --platform-ui" => web::PLATFORM_UI_INSTALL_ENV,
        _ => panic!("install verb {verb} has no environment contract"),
    }
}

fn missing(verb: &str, request: &JobRequest, secrets: &[JobSecret]) -> Vec<&'static str> {
    let contract = contract(verb);
    let mut available: BTreeSet<&str> = request
        .environment
        .keys()
        .map(String::as_str)
        .chain(secrets.iter().map(|secret| secret.environment.as_str()))
        .collect();
    // The install wrappers use explicit export NAME=value statements.
    for script in &request.arguments {
        for line in script.lines() {
            if let Some(exports) = line.trim().strip_prefix("export ") {
                available.extend(
                    exports
                        .split_whitespace()
                        .filter_map(|word| word.split_once('=').map(|(name, _)| name)),
                );
            }
        }
    }
    assert!(
        contract
            .optional
            .iter()
            .all(|name| !contract.required.contains(name))
    );
    contract
        .required
        .iter()
        .copied()
        .filter(|name| !available.contains(name))
        .collect()
}

fn requests() -> Vec<(&'static str, JobRequest, Vec<JobSecret>)> {
    let directory = std::env::temp_dir().join(format!(
        "wamn-install-job-contract-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let registry = wamn_package_artifact::PackageRegistry {
        artifact_base: "registry/packages".into(),
        registry_auth_file: directory.join("registry.json"),
        insecure_registry: false,
        oci_ca_paths: vec![],
    };
    std::fs::write(&registry.registry_auth_file, "{}").unwrap();
    let requests = install_job_requests(&registry).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    requests
}

#[test]
fn install_jobs_supply_required_environment() {
    for (verb, request, secrets) in requests() {
        assert_eq!(
            missing(verb, &request, &secrets),
            Vec::<&str>::new(),
            "{}",
            request.name
        );
        if request.environment.contains_key("WAMN_AUTH_NATS_URL") {
            assert_eq!(
                request.environment["WAMN_AUTH_NATS_URL"],
                "nats://nats.platform.svc.cluster.local:4222"
            );
            let mounted = verb == "web upload";
            for (variable, key) in [
                ("WAMN_AUTH_NATS_TLS_CA", "ca.crt"),
                ("WAMN_AUTH_NATS_TLS_CERT", "tls.crt"),
                ("WAMN_AUTH_NATS_TLS_KEY", "tls.key"),
            ] {
                let directory = if mounted {
                    "/etc/wamn/auth-nats"
                } else {
                    "/inputs"
                };
                assert_eq!(request.environment[variable], format!("{directory}/{key}"));
                if !mounted {
                    assert!(request.files.contains_key(key));
                }
            }
            assert_eq!(
                request
                    .environment
                    .get("WAMN_AUTH_NATS_TLS_SERVER_NAME")
                    .map(String::as_str),
                if mounted {
                    None
                } else {
                    Some("nats.platform.svc.cluster.local")
                }
            );
        }
        if verb == "provision-org" {
            assert!(
                !request
                    .environment
                    .keys()
                    .any(|name| name.starts_with("WAMN_AUTH_NATS_"))
            );
        }
        if verb == "invite" {
            // The invite Job runs in the platform namespace, where
            // wasmcloud-runtime-tls does not exist, so it copies the ctl client.
            assert!(
                !request
                    .secret_mounts
                    .iter()
                    .any(|mount| mount.name == "wasmcloud-runtime-tls")
            );
        }
    }
}

#[test]
fn contract_refuses_package_0_1_115_and_original_invite_without_auth_nats() {
    for (verb, mut request, secrets) in requests() {
        if !matches!(verb, "apply-package" | "invite") {
            continue;
        }
        assert!(missing(verb, &request, &secrets).is_empty());
        request
            .environment
            .retain(|name, _| !name.starts_with("WAMN_AUTH_NATS_"));
        assert_eq!(
            missing(verb, &request, &secrets),
            [
                "WAMN_AUTH_NATS_URL",
                "WAMN_AUTH_NATS_TLS_CA",
                "WAMN_AUTH_NATS_TLS_CERT",
                "WAMN_AUTH_NATS_TLS_KEY",
            ],
            "{}",
            request.name
        );
    }
}

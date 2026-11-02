//! Compare the production install requests with the contracts beside their CLI arguments.

use std::collections::BTreeSet;

use wamn_control::platform::job::{JobRequest, JobSecret, install_job_requests};

fn missing(request: &JobRequest, secrets: &[JobSecret]) -> Vec<&'static str> {
    let contract = wamn_control::platform::job_contract::request(request).unwrap();
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
            missing(&request, &secrets),
            Vec::<&str>::new(),
            "{}",
            request.name
        );
        if request.environment.contains_key("WAMN_NATS_URL") {
            assert_eq!(
                request.environment["WAMN_NATS_URL"],
                "nats://wamn-nats.platform.svc.cluster.local:4222"
            );
            for (variable, key) in [
                ("WAMN_NATS_TLS_CA", "ca.crt"),
                ("WAMN_NATS_TLS_CERT", "tls.crt"),
                ("WAMN_NATS_TLS_KEY", "tls.key"),
            ] {
                assert_eq!(request.environment[variable], format!("/inputs/{key}"));
                assert!(request.files.contains_key(key));
            }
            assert_eq!(
                request
                    .environment
                    .get("WAMN_NATS_TLS_SERVER_NAME")
                    .map(String::as_str),
                Some("wamn-nats.platform.svc.cluster.local")
            );
        }
        if verb == "provision-org" {
            assert!(
                !request
                    .environment
                    .keys()
                    .any(|name| name.starts_with("WAMN_NATS_"))
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
        assert!(missing(&request, &secrets).is_empty());
        request
            .environment
            .retain(|name, _| !name.starts_with("WAMN_NATS_"));
        assert_eq!(
            missing(&request, &secrets),
            [
                "WAMN_NATS_URL",
                "WAMN_NATS_TLS_CA",
                "WAMN_NATS_TLS_CERT",
                "WAMN_NATS_TLS_KEY",
            ],
            "{}",
            request.name
        );
    }
}

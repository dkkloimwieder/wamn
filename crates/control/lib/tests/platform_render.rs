//! Platform objects rendered without a cluster.
use serde_json::json;
use wamn_control as control;
#[path = "support/platform_install.rs"]
mod platform_install;
use std::path::Path;
use std::time::Instant;
#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::fixture_environment;

#[test]
fn prekind_platform_renders() -> anyhow::Result<()> {
    use wamn_gate_harness::render_check::{
        assert_complete, credential_secrets, environment, operator_prerequisites, template,
    };
    let started = Instant::now();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    // The platform publishes built application releases in kind. A fixture H
    // document renders the same HTTP and materializer role shapes without Wasm.
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(
        &release_documents::fixture_releases()?[2],
    )?;
    for project in ["receiving", "wms"] {
        let (_, mut document) = fixture_environment(&repository, project)?;
        document.org = "test".into();
        let path = repository.join(format!("deploy/gcp/host-group-{project}.yaml"));
        let group = wamn_control::platform::render_host_group(
            &std::fs::read_to_string(&path)?,
            &document,
            "registry.invalid/wamn",
            "render",
        )?;
        let path = work.path().join(format!("host-group-{project}.yaml"));
        std::fs::write(&path, serde_yaml::to_string(&group)?)?;
        let mut secrets = credential_secrets(&document.triple(), "hosts");
        secrets.extend(operator_prerequisites("hosts"));
        secrets.extend(wamn_control::platform::render_event_secrets(&document)?);
        // cert-manager issues the environment's NATS certificate Secret.
        secrets.push(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": format!("wamn-nats-test--{project}--dev"), "namespace": "hosts"},
            "data": {"ca.crt": "", "tls.crt": "", "tls.key": ""}
        }));
        assert_complete(
            &template(&environment(&document, &manifest, &path, 70)?, "hosts")?,
            &secrets,
        )?;
        println!(
            "platform {project}: {}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    println!("prekind platform renders elapsed {:?}", started.elapsed());
    Ok(())
}

#[test]
fn prekind_install_renders() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    assert!(
        render
            .objects
            .iter()
            .all(|(_, object)| object["kind"].is_string())
    );
    assert_eq!(render.documents.len(), 2);
    assert_eq!(render.fixtures.releases.len(), 2);
    assert_eq!(render.cluster.org, "test");
    Ok(())
}

// A suspended Job is a declaration. Its resumed version marks execution.
fn pod(object: &serde_json::Value) -> Option<&serde_json::Value> {
    match object["kind"].as_str()? {
        "Pod" => Some(&object["spec"]),
        "Job" if object["spec"]["suspend"] == true => None,
        "Deployment" | "StatefulSet" | "DaemonSet" | "Job" | "ReplicaSet" => {
            Some(&object["spec"]["template"]["spec"])
        }
        "CronJob" => Some(&object["spec"]["jobTemplate"]["spec"]["template"]["spec"]),
        _ => None,
    }
}

fn identity(object: &serde_json::Value) -> String {
    format!(
        "{}/{}/{}",
        object["kind"].as_str().unwrap_or("?"),
        object["metadata"]["namespace"].as_str().unwrap_or("?"),
        object["metadata"]["name"].as_str().unwrap_or("?")
    )
}

fn relation_failure(
    objects: &[(&str, serde_json::Value)],
    consumer: usize,
    field: &str,
    kind: &str,
    namespace: &str,
    name: &str,
) -> Option<String> {
    let target = objects.iter().position(|(_, object)| {
        object["kind"] == kind
            && object["metadata"]["namespace"] == namespace
            && object["metadata"]["name"] == name
    });
    if target.is_some_and(|position| position < consumer) {
        return None;
    }
    let producer = target.map_or_else(
        || "absent".to_owned(),
        |position| format!("{}:{}", objects[position].0, position),
    );
    Some(format!(
        "{} step {}:{} field {field} -> {kind}/{namespace}/{name} producer step {producer}",
        identity(&objects[consumer].1),
        objects[consumer].0,
        consumer
    ))
}

fn account_failures(objects: &[(&str, serde_json::Value)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (index, (_, object)) in objects.iter().enumerate() {
        let Some(spec) = pod(object) else { continue };
        if let Some(account) = spec["serviceAccountName"].as_str()
            && let Some(failure) = relation_failure(
                objects,
                index,
                "serviceAccountName",
                "ServiceAccount",
                object["metadata"]["namespace"].as_str().unwrap(),
                account,
            )
        {
            failures.push(failure);
        }
    }
    failures
}

#[test]
fn install_service_accounts_precede_consumers() -> anyhow::Result<()> {
    let failures = account_failures(&platform_install::render()?.objects);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_service_accounts_reject_a_missing_rendered_reference() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, object) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Deployment"
                && pod(object).is_some_and(|spec| spec["serviceAccountName"].is_string())
        })
        .unwrap();
    object["spec"]["template"]["spec"]["serviceAccountName"] = json!("missing-account");
    let failures = account_failures(&objects);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(
        failures[0].contains("serviceAccountName -> ServiceAccount/")
            && failures[0].contains("missing-account producer step absent")
    );
    Ok(())
}

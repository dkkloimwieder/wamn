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

#[derive(Debug)]
struct InputReference<'a> {
    field: String,
    kind: &'static str,
    name: &'a str,
    key: Option<&'a str>,
    optional: bool,
}

fn input_references(spec: &serde_json::Value) -> Vec<InputReference<'_>> {
    let mut references = Vec::new();
    for (index, volume) in spec["volumes"].as_array().into_iter().flatten().enumerate() {
        let sources = std::iter::once((format!("volumes[{index}]"), volume)).chain(
            volume["projected"]["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(move |(source, value)| {
                    (
                        format!("volumes[{index}].projected.sources[{source}]"),
                        value,
                    )
                }),
        );
        for (field, source) in sources {
            for (member, kind) in [("secret", "Secret"), ("configMap", "ConfigMap")] {
                let source = &source[member];
                let Some(name) = source["secretName"]
                    .as_str()
                    .or_else(|| source["name"].as_str())
                else {
                    continue;
                };
                let keys = source["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item["key"].as_str())
                    .map(Some)
                    .collect::<Vec<_>>();
                for key in if keys.is_empty() { vec![None] } else { keys } {
                    references.push(InputReference {
                        field: format!("{field}.{member}"),
                        kind,
                        name,
                        key,
                        optional: source["optional"] == true,
                    });
                }
            }
        }
    }
    for group in ["containers", "initContainers"] {
        for (index, container) in spec[group].as_array().into_iter().flatten().enumerate() {
            for (member, key_member) in [("env", "valueFrom"), ("envFrom", "")] {
                for (variable, entry) in container[member]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let source = if key_member.is_empty() {
                        entry
                    } else {
                        &entry[key_member]
                    };
                    for (name, kind) in [
                        ("secretKeyRef", "Secret"),
                        ("configMapKeyRef", "ConfigMap"),
                        ("secretRef", "Secret"),
                        ("configMapRef", "ConfigMap"),
                    ] {
                        if let Some(target) = source[name]["name"].as_str() {
                            references.push(InputReference {
                                field: format!("{group}[{index}].{member}[{variable}].{name}"),
                                kind,
                                name: target,
                                key: source[name]["key"].as_str(),
                                optional: source[name]["optional"] == true,
                            });
                        }
                    }
                }
            }
        }
    }
    references
}

fn produces_input(object: &serde_json::Value, namespace: &str, input: &InputReference<'_>) -> bool {
    if object["metadata"]["namespace"] != namespace {
        return false;
    }
    if object["kind"] == input.kind && object["metadata"]["name"] == input.name {
        return input.key.is_none_or(|key| {
            object["data"].get(key).is_some() || object["stringData"].get(key).is_some()
        });
    }
    if input.kind != "Secret" {
        return false;
    }
    if object["kind"] == "Certificate" && object["spec"]["secretName"] == input.name {
        return input
            .key
            .is_none_or(|key| ["ca.crt", "tls.crt", "tls.key"].contains(&key));
    }
    if object["kind"] == "Cluster"
        && object["apiVersion"]
            .as_str()
            .is_some_and(|version| version.starts_with("postgresql.cnpg.io/"))
    {
        let name = object["spec"]["superuserSecret"]["name"]
            .as_str()
            .map_or_else(
                || format!("{}-superuser", object["metadata"]["name"].as_str().unwrap()),
                str::to_owned,
            );
        return object["spec"]["enableSuperuserAccess"] == true
            && name == input.name
            && input.key.is_none_or(|key| {
                [
                    "username", "password", "uri", "dbname", "host", "port", "pgpass", "jdbc-uri",
                ]
                .contains(&key)
            });
    }
    false
}

fn input_failures(
    objects: &[(&str, serde_json::Value)],
    owner_inputs: &[serde_json::Value],
) -> Vec<String> {
    let mut failures = Vec::new();
    for (index, (step, object)) in objects.iter().enumerate() {
        let Some(spec) = pod(object) else { continue };
        let namespace = object["metadata"]["namespace"].as_str().unwrap();
        for input in input_references(spec) {
            if input.optional
                || owner_inputs
                    .iter()
                    .any(|object| produces_input(object, namespace, &input))
            {
                continue;
            }
            let producer = objects
                .iter()
                .position(|(_, object)| produces_input(object, namespace, &input));
            if producer.is_some_and(|producer| producer < index) {
                continue;
            }
            let target_step = producer.map_or_else(
                || "absent".into(),
                |index| format!("{}:{index}", objects[index].0),
            );
            failures.push(format!("{} step {step}:{index} field {} -> {}/{namespace}/{} key {} producer step {target_step}", identity(object), input.field, input.kind, input.name, input.key.unwrap_or("*")));
        }
    }
    failures
}

#[test]
fn install_secret_and_configmap_inputs_precede_consumers() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let failures = input_failures(&render.objects, &render.owner_inputs);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_inputs_reject_a_missing_rendered_key() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let (_, object) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Deployment" && object["metadata"]["name"] == "identity"
        })
        .unwrap();
    object["spec"]["template"]["spec"]["containers"][0]["env"].as_array_mut().unwrap().push(json!({"name":"MISSING", "valueFrom":{"secretKeyRef":{"name":"identity-resend","key":"missing-key"}}}));
    let failures = input_failures(&objects, &render.owner_inputs);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("identity-resend key missing-key producer step absent"));
    Ok(())
}

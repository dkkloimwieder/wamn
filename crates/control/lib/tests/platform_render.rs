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
    let rows = render
        .objects
        .iter()
        .filter_map(|(_, object)| object["data"]["release.json"].as_str())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row["webs"], json!({}));
    }
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

fn role_failures(objects: &[(&str, serde_json::Value)]) -> Vec<String> {
    use wamn_control::platform::job_contract;
    let mut failures = Vec::new();
    for (index, (step, object)) in objects.iter().enumerate() {
        if object["kind"] != "Job" {
            continue;
        }
        let Some(spec) = pod(object) else { continue };
        let namespace = object["metadata"]["namespace"].as_str().unwrap();
        let account = spec["serviceAccountName"].as_str().unwrap_or("default");
        for container in spec["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(spec["initContainers"].as_array().into_iter().flatten())
        {
            let command: Vec<String> =
                serde_json::from_value(container["command"].clone()).unwrap_or_default();
            let arguments: Vec<String> =
                serde_json::from_value(container["args"].clone()).unwrap_or_default();
            let contract = match job_contract::resolve(&command, &arguments) {
                Ok(contract) => contract,
                Err(error) => {
                    failures.push(format!("{} step {step}:{index} field command -> command contract producer step absent: {error}",identity(object)));
                    continue;
                }
            };
            if !contract.access.is_empty() && spec["automountServiceAccountToken"] == false {
                failures.push(format!("{} step {step}:{index} field automountServiceAccountToken -> API credentials producer step absent", identity(object)));
            }
            for access in contract.access {
                let roles = objects
                    .iter()
                    .enumerate()
                    .filter(|(position, (_, binding))| {
                        *position < index
                            && binding["kind"] == "RoleBinding"
                            && binding["metadata"]["namespace"] == access.namespace
                            && binding["subjects"].as_array().is_some_and(|subjects| {
                                subjects.iter().any(|subject| {
                                    subject["kind"] == "ServiceAccount"
                                        && subject["name"] == account
                                        && subject["namespace"] == namespace
                                })
                            })
                    })
                    .filter_map(|(_, (_, binding))| {
                        objects.iter().enumerate().find(|(position, (_, role))| {
                            *position < index
                                && role["kind"] == binding["roleRef"]["kind"]
                                && role["kind"] == "Role"
                                && role["metadata"]["name"] == binding["roleRef"]["name"]
                                && role["metadata"]["namespace"] == access.namespace
                        })
                    })
                    .collect::<Vec<_>>();
                for resource in access.resources {
                    for verb in access.verbs {
                        let permits = |values: &serde_json::Value, required: &str| {
                            values.as_array().is_some_and(|values| {
                                values.iter().any(|value| value == required || value == "*")
                            })
                        };
                        if roles.iter().any(|(_, (_, role))| {
                            role["rules"].as_array().is_some_and(|rules| {
                                rules.iter().any(|rule| {
                                    permits(&rule["apiGroups"], access.group)
                                        && permits(&rule["resources"], resource)
                                        && permits(&rule["verbs"], verb)
                                })
                            })
                        }) {
                            continue;
                        }
                        let producer = roles.first().map_or_else(
                            || "absent".into(),
                            |(position, (step, _))| format!("{step}:{position}"),
                        );
                        failures.push(format!("{} step {step}:{index} field command ({}) -> Role/{}/{}/{resource}/{verb} producer step {producer}",identity(object),contract.verb,access.namespace,access.group));
                    }
                }
            }
        }
    }
    failures
}

#[test]
fn install_job_roles_cover_command_access() -> anyhow::Result<()> {
    let failures = role_failures(&platform_install::render()?.objects);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_job_roles_reject_a_missing_rendered_verb() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, role) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Role"
                && object["metadata"]["namespace"] == "hosts"
                && object["metadata"]["name"] == "platform-first-apply-test--wms--dev"
        })
        .unwrap();
    let permission = role["rules"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|rule| {
            rule["resources"]
                .as_array()
                .unwrap()
                .contains(&json!("pods"))
        })
        .unwrap();
    permission["verbs"]
        .as_array_mut()
        .unwrap()
        .retain(|verb| verb != "list");
    let failures = role_failures(&objects);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("/pods/list producer step"));
    Ok(())
}

#[test]
fn install_jobs_refuse_an_unknown_command() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, job) = objects
        .iter_mut()
        .find(|(_, object)| object["kind"] == "Job" && object["spec"]["suspend"] == false)
        .unwrap();
    let container = &mut job["spec"]["template"]["spec"]["containers"][0];
    container["command"] = json!(["/usr/local/bin/wamn-ctl"]);
    container["args"] = json!(["unlisted-command"]);
    let failures = role_failures(&objects);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("no command contract"));
    Ok(())
}

fn available_input(
    objects: &[(&str, serde_json::Value)],
    owner_inputs: &[serde_json::Value],
    consumer: usize,
    namespace: &str,
    input: &InputReference<'_>,
) -> bool {
    objects[..consumer]
        .iter()
        .any(|(_, object)| produces_input(object, namespace, input))
        || owner_inputs
            .iter()
            .any(|object| produces_input(object, namespace, input))
}

fn mounted_input(
    objects: &[(&str, serde_json::Value)],
    owner_inputs: &[serde_json::Value],
    index: usize,
    spec: &serde_json::Value,
    container: &serde_json::Value,
    path: &str,
) -> bool {
    let namespace = objects[index].1["metadata"]["namespace"].as_str().unwrap();
    container["volumeMounts"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|mount| {
            let Some(base) = mount["mountPath"].as_str() else {
                return false;
            };
            let Some(relative) = path
                .strip_prefix(base)
                .filter(|relative| relative.is_empty() || relative.starts_with('/'))
            else {
                return false;
            };
            let Some(volume) = spec["volumes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|volume| volume["name"] == mount["name"])
            else {
                return false;
            };
            for (member, kind) in [("secret", "Secret"), ("configMap", "ConfigMap")] {
                let source = &volume[member];
                let Some(name) = source["secretName"]
                    .as_str()
                    .or_else(|| source["name"].as_str())
                else {
                    continue;
                };
                let key = if relative.is_empty() {
                    None
                } else if let Some(items) = source["items"].as_array() {
                    let entry = relative.trim_start_matches('/');
                    if let Some(item) = items.iter().find(|item| item["path"] == entry) {
                        item["key"].as_str()
                    } else if items.iter().any(|item| {
                        item["path"]
                            .as_str()
                            .is_some_and(|path| path.starts_with(&format!("{entry}/")))
                    }) {
                        None
                    } else {
                        continue;
                    }
                } else {
                    Some(relative.trim_start_matches('/'))
                };
                if available_input(
                    objects,
                    owner_inputs,
                    index,
                    namespace,
                    &InputReference {
                        field: path.into(),
                        kind,
                        name,
                        key,
                        optional: false,
                    },
                ) {
                    return true;
                }
            }
            false
        })
}

fn command_input_failures(
    objects: &[(&str, serde_json::Value)],
    owner_inputs: &[serde_json::Value],
) -> Vec<String> {
    use wamn_control::platform::job_contract::{self, Source};
    let mut failures = Vec::new();
    for (index, (step, object)) in objects.iter().enumerate() {
        if object["kind"] != "Job" {
            continue;
        }
        let Some(spec) = pod(object) else { continue };
        let namespace = object["metadata"]["namespace"].as_str().unwrap();
        for container in spec["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(spec["initContainers"].as_array().into_iter().flatten())
        {
            let command: Vec<String> =
                serde_json::from_value(container["command"].clone()).unwrap_or_default();
            let arguments: Vec<String> =
                serde_json::from_value(container["args"].clone()).unwrap_or_default();
            let contract = match job_contract::resolve(&command, &arguments) {
                Ok(contract) => contract,
                Err(error) => {
                    failures.push(format!("{} step {step}:{index} field command -> inputs producer step absent: {error}",identity(object)));
                    continue;
                }
            };
            let environment = container["env"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["name"].as_str().map(|name| (name, entry)))
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut exported = std::collections::BTreeSet::new();
            for export in contract.exports {
                let (present, target) = match &export.source {
                    Source::File(path) => (
                        mounted_input(objects, owner_inputs, index, spec, container, path),
                        (*path).to_owned(),
                    ),
                    Source::Secret {
                        namespace,
                        name,
                        key,
                    } => {
                        let mut name = (*name).to_owned();
                        for (variable, entry) in &environment {
                            if let Some(value) = entry["value"].as_str() {
                                name = name.replace(&format!("${variable}"), value);
                            }
                        }
                        let present = available_input(
                            objects,
                            owner_inputs,
                            index,
                            namespace,
                            &InputReference {
                                field: export.variable.into(),
                                kind: "Secret",
                                name: &name,
                                key: Some(key),
                                optional: false,
                            },
                        );
                        (present, format!("Secret/{namespace}/{name}/{key}"))
                    }
                };
                if present {
                    exported.insert(export.variable);
                } else {
                    failures.push(format!(
                        "{} step {step}:{index} field export {} -> {target} producer step absent",
                        identity(object),
                        export.variable
                    ));
                }
            }
            let expansions = arguments
                .iter()
                .skip(usize::from(command.first().is_some_and(|program| {
                    program.rsplit('/').next() == Some("sh")
                })))
                .flat_map(|argument| argument.split("$(").skip(1))
                .filter_map(|part| part.split_once(')').map(|(name, _)| name))
                .filter(|name| {
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                });
            let required = contract
                .required
                .iter()
                .copied()
                .chain(expansions)
                .collect::<std::collections::BTreeSet<_>>();
            for variable in required {
                if exported.contains(variable) {
                    continue;
                }
                let present = environment.get(variable).is_some_and(|entry| {
                    if let Some(value) = entry["value"].as_str() {
                        return !value.starts_with("/inputs")
                            || mounted_input(objects, owner_inputs, index, spec, container, value);
                    }
                    let reference = &entry["valueFrom"]["secretKeyRef"];
                    reference["name"].as_str().is_some_and(|name| {
                        available_input(
                            objects,
                            owner_inputs,
                            index,
                            namespace,
                            &InputReference {
                                field: variable.into(),
                                kind: "Secret",
                                name,
                                key: reference["key"].as_str(),
                                optional: false,
                            },
                        )
                    })
                });
                if !present {
                    failures.push(format!("{} step {step}:{index} field env.{variable} -> required input producer step absent",identity(object)));
                }
            }
            for path in contract.files {
                if !mounted_input(objects, owner_inputs, index, spec, container, path) {
                    failures.push(format!(
                        "{} step {step}:{index} field volumeMounts -> {path} producer step absent",
                        identity(object)
                    ));
                }
            }
        }
    }
    failures
}

#[test]
fn install_job_commands_receive_required_inputs() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let failures = command_input_failures(&render.objects, &render.owner_inputs);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_job_commands_reject_a_missing_environment_input() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let (_, job) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Job"
                && object["spec"]["suspend"] == false
                && object["metadata"]["name"] == "platform-first-apply-test--wms--dev"
        })
        .unwrap();
    job["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| entry["name"] != "WAMN_APPLY_DATABASE_URL");
    let failures = command_input_failures(&objects, &render.owner_inputs);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(
        failures[0].contains("env.WAMN_APPLY_DATABASE_URL -> required input producer step absent")
    );
    Ok(())
}

#[test]
fn install_job_commands_reject_an_unbacked_export() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let (_, job) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Job"
                && object["spec"]["suspend"] == false
                && object["metadata"]["name"] == "platform-control-credentials-test"
        })
        .unwrap();
    let mounts = job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
        .as_array_mut()
        .unwrap();
    let mount = mounts
        .iter_mut()
        .find(|mount| mount["mountPath"] == "/etc/wamn/postgres")
        .unwrap();
    mount["mountPath"] = json!("/elsewhere");
    let failures = command_input_failures(&objects, &render.owner_inputs);
    assert!(
        failures.iter().any(|failure| failure.contains(
            "field export WAMN_SYSTEM_ADMIN_URL -> /etc/wamn/postgres/uri producer step absent"
        )),
        "{}",
        failures.join("\n")
    );
    Ok(())
}

fn selector_failures(objects: &[(&str, serde_json::Value)]) -> Vec<String> {
    let latest = objects
        .iter()
        .enumerate()
        .map(|(index, (_, object))| (identity(object), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut failures = Vec::new();
    for index in latest.values().copied() {
        let (step, service) = &objects[index];
        if service["kind"] != "Service" {
            continue;
        }
        let namespace = &service["metadata"]["namespace"];
        let selector = service["spec"]["selector"]
            .as_object()
            .filter(|selector| !selector.is_empty());
        let matches = latest
            .values()
            .copied()
            .filter(|position| {
                let object = &objects[*position].1;
                if &object["metadata"]["namespace"] != namespace {
                    return false;
                }
                if let Some(selector) = selector {
                    if pod(object).is_none() {
                        return false;
                    }
                    let labels = if object["kind"] == "Pod" {
                        &object["metadata"]["labels"]
                    } else if object["kind"] == "CronJob" {
                        &object["spec"]["jobTemplate"]["spec"]["template"]["metadata"]["labels"]
                    } else {
                        &object["spec"]["template"]["metadata"]["labels"]
                    };
                    selector.iter().all(|(key, value)| &labels[key] == value)
                } else {
                    object["kind"] == "WorkloadDeployment"
                        && object["spec"]["template"]["spec"]["kubernetes"]["service"]["name"]
                            == service["metadata"]["name"]
                }
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            let targets = matches
                .iter()
                .map(|position| {
                    format!(
                        "{} step {}:{position}",
                        identity(&objects[*position].1),
                        objects[*position].0
                    )
                })
                .collect::<Vec<_>>();
            failures.push(format!(
                "{} step {step}:{index} field {} -> {} count {} producer steps [{}]",
                identity(service),
                if selector.is_some() {
                    "spec.selector"
                } else {
                    "WorkloadDeployment.kubernetes.service.name"
                },
                if selector.is_some() {
                    "pod template"
                } else {
                    "WorkloadDeployment"
                },
                matches.len(),
                if targets.is_empty() {
                    "absent".into()
                } else {
                    targets.join(", ")
                }
            ));
        }
    }
    failures
}

#[test]
fn install_service_selectors_resolve_one_pod_template() -> anyhow::Result<()> {
    let failures = selector_failures(&platform_install::render()?.objects);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_service_selectors_reject_an_unmatched_reference() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, service) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Service" && object["metadata"]["name"] == "wamn-edge"
        })
        .unwrap();
    service["spec"]["selector"]["absent-label"] = json!("absent-value");
    let failures = selector_failures(&objects);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("spec.selector -> pod template count 0 producer steps [absent]"));
    Ok(())
}

#[test]
fn install_selectorless_services_reject_a_missing_workload_reference() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, workload) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "WorkloadDeployment" && object["metadata"]["name"] == "c-test-http"
        })
        .unwrap();
    workload["spec"]["template"]["spec"]["kubernetes"]["service"]["name"] =
        json!("missing-service");
    let failures = selector_failures(&objects);
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("WorkloadDeployment count 0"));
    Ok(())
}

fn diagnostic_failures(objects: &[(&str, serde_json::Value)]) -> Vec<String> {
    use wamn_control::platform::job_contract;
    let mut failures = Vec::new();
    for (index, (step, object)) in objects.iter().enumerate() {
        if object["kind"] != "Job" {
            continue;
        }
        let Some(spec) = pod(object) else { continue };
        for container in spec["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(spec["initContainers"].as_array().into_iter().flatten())
        {
            let command: Vec<String> =
                serde_json::from_value(container["command"].clone()).unwrap_or_default();
            let arguments: Vec<String> =
                serde_json::from_value(container["args"].clone()).unwrap_or_default();
            let contract = job_contract::resolve(&command, &arguments);
            let credential = contract
                .as_ref()
                .is_ok_and(|contract| contract.credential_stdout);
            if let Err(error) = contract {
                failures.push(format!("{} step {step}:{index} field command -> command contract producer step absent: {error}",identity(object)));
            }
            if command
                .first()
                .is_none_or(|program| program.rsplit('/').next() != Some("sh"))
            {
                continue;
            }
            let script = arguments
                .first()
                .map_or("", String::as_str)
                .split_whitespace()
                .collect::<String>();
            let stdout = script
                .replace("2>/dev/null", "")
                .replace("2>>/dev/null", "")
                .contains(">/dev/null")
                || script.contains("1>&-")
                || script.contains("&>/dev/null");
            let stderr = script.contains("2>/dev/null")
                || script.contains("2>>/dev/null")
                || script.contains("2>&-")
                || script.contains("&>/dev/null")
                || (stdout && script.contains("2>&1"));
            for (field, discards) in [("stderr", stderr), ("stdout", stdout && !credential)] {
                if discards {
                    failures.push(format!("{} step {step}:{index} field command.{field} -> discarded {field} producer step {step}:{index} (command)",identity(object)));
                }
            }
        }
    }
    failures
}

#[test]
fn install_jobs_preserve_diagnostics() -> anyhow::Result<()> {
    let failures = diagnostic_failures(&platform_install::render()?.objects);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_jobs_reject_discarded_stderr_without_printing_credentials() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, job) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Job"
                && object["spec"]["suspend"] == false
                && object["metadata"]["name"] == "platform-control-credentials-test"
        })
        .unwrap();
    let container = &mut job["spec"]["template"]["spec"]["containers"][0];
    let secret = "private-credential-sentinel";
    container["env"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"SECRET","value":secret}));
    let script = container["args"][0].as_str().unwrap();
    container["args"][0] = json!(format!("{script}\n: {secret} 2>/dev/null"));
    let failures = diagnostic_failures(&objects);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("command.stderr -> discarded stderr"))
    );
    assert!(failures.iter().all(|failure| !failure.contains(secret)));
    Ok(())
}

#[test]
fn install_jobs_reject_discarded_public_stdout() -> anyhow::Result<()> {
    let mut objects = platform_install::render()?.objects.clone();
    let (_, job) = objects
        .iter_mut()
        .find(|(_, object)| {
            object["kind"] == "Job"
                && object["spec"]["suspend"] == false
                && object["metadata"]["name"] == "platform-invite-test"
        })
        .unwrap();
    let script = job["spec"]["template"]["spec"]["containers"][0]["args"][0]
        .as_str()
        .unwrap()
        .to_owned();
    job["spec"]["template"]["spec"]["containers"][0]["args"][0] =
        json!(format!("{script}\n: >/dev/null"));
    let failures = diagnostic_failures(&objects);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("command.stdout -> discarded stdout"))
    );
    Ok(())
}

fn mounted_text(
    objects: &[(&str, serde_json::Value)],
    index: usize,
    path: &str,
) -> anyhow::Result<String> {
    use anyhow::Context as _;
    use base64::Engine as _;
    let job = &objects[index].1;
    let spec = pod(job).context("the Job is not executable")?;
    let mount = spec["containers"][0]["volumeMounts"]
        .as_array()
        .context("Job mounts")?
        .iter()
        .find(|mount| {
            mount["mountPath"]
                .as_str()
                .is_some_and(|prefix| path.starts_with(&format!("{prefix}/")))
        })
        .context("document mount")?;
    let volume = spec["volumes"]
        .as_array()
        .context("Job volumes")?
        .iter()
        .find(|volume| volume["name"] == mount["name"])
        .context("document volume")?;
    let relative = path
        .strip_prefix(mount["mountPath"].as_str().unwrap())
        .unwrap()
        .trim_start_matches('/');
    let source = &volume["secret"];
    let key = source["items"]
        .as_array()
        .context("document items")?
        .iter()
        .find(|item| item["path"] == relative)
        .and_then(|item| item["key"].as_str())
        .context("document key")?;
    let secret = objects[..index]
        .iter()
        .rev()
        .find(|(_, object)| {
            object["kind"] == "Secret"
                && object["metadata"]["name"] == source["secretName"]
                && object["metadata"]["namespace"] == job["metadata"]["namespace"]
        })
        .map(|(_, object)| object)
        .context("document Secret")?;
    if let Some(value) = secret["stringData"][key].as_str() {
        return Ok(value.into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret["data"][key].as_str().context("document data")?)?;
    Ok(String::from_utf8(bytes)?)
}

#[derive(Debug)]
struct EnvironmentJobs {
    documents: Vec<(
        usize,
        wamn_control::environment::document::EnvironmentDocument,
    )>,
    failures: Vec<String>,
}

fn environment_jobs(objects: &[(&str, serde_json::Value)]) -> anyhow::Result<EnvironmentJobs> {
    use anyhow::Context as _;
    use wamn_control::environment::document::EnvironmentDocument;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let mut compiled = std::collections::BTreeMap::<String, EnvironmentDocument>::new();
    let mut result = EnvironmentJobs {
        documents: Vec::new(),
        failures: Vec::new(),
    };
    for (index, (step, object)) in objects.iter().enumerate() {
        if object["kind"] != "Job" {
            continue;
        }
        let Some(spec) = pod(object) else { continue };
        let decoded = (|| -> anyhow::Result<Option<EnvironmentDocument>> {
            let container = &spec["containers"][0];
            let command: Vec<String> = serde_json::from_value(container["command"].clone())?;
            let arguments: Vec<String> = serde_json::from_value(container["args"].clone())?;
            let contract = wamn_control::platform::job_contract::resolve(&command, &arguments)?;
            if !matches!(contract.verb, "env apply" | "env connections") {
                return Ok(None);
            }
            let text = mounted_text(
                objects,
                index,
                arguments.get(2).context("document argument")?,
            )?;
            let document = if let Some(document) = compiled.get(&text) {
                document.clone()
            } else {
                let path = scratch.path().join(format!("document-{index}.k"));
                std::fs::write(&path, &text)?;
                let document = EnvironmentDocument::compile(&path)?;
                compiled.insert(text, document.clone());
                document
            };
            Ok(Some(document))
        })();
        match decoded {
            Ok(Some(document)) => result.documents.push((index,document)),
            Ok(None) => {},
            Err(_) => result.failures.push(format!("{} step {step}:{index} field args[2] -> compiled /inputs/environment.k producer step absent or invalid",identity(object))),
        }
    }
    Ok(result)
}

fn source_document<'a>(
    render: &'a platform_install::Render,
    document: &wamn_control::environment::document::EnvironmentDocument,
) -> &'a str {
    render
        .documents
        .iter()
        .find(|(_, candidate)| {
            candidate.org == document.org
                && candidate.project == document.project
                && candidate.env == document.env
        })
        .and_then(|(path, _)| path.to_str())
        .unwrap_or("unmapped rendered environment.k")
}

fn connection_failures(
    objects: &[(&str, serde_json::Value)],
    render: &platform_install::Render,
) -> anyhow::Result<Vec<String>> {
    let EnvironmentJobs {
        documents,
        mut failures,
    } = environment_jobs(objects)?;
    let mut created = std::collections::BTreeMap::new();
    for (index, document) in &documents {
        let container = &objects[*index].1["spec"]["template"]["spec"]["containers"][0];
        if container["args"][1] != "connections" {
            continue;
        }
        for name in document.integrations.keys() {
            let connection = mounted_text(objects, *index, &format!("/inputs/{name}.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            let integration =
                mounted_text(objects, *index, &format!("/inputs/{name}.integration.json"))
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            if connection.is_some_and(|value| {
                value["name"] == *name
                    && value["project"] == document.project
                    && value["env"] == document.env
            }) && integration
                .is_some_and(|value| value["name"] == *name && value["project"] == document.project)
            {
                created
                    .entry((
                        document.org.clone(),
                        document.project.clone(),
                        document.env.clone(),
                        name.clone(),
                    ))
                    .or_insert(*index);
            }
        }
    }
    for (index, document) in documents {
        let (step, job) = &objects[index];
        if job["spec"]["template"]["spec"]["containers"][0]["args"][1] != "apply"
            || document.release == wamn_control::environment::document::DeclaredRelease::None
        {
            continue;
        }
        for (application, definition) in &document.applications {
            for (alias, name) in &definition.connections {
                let producer = created.get(&(
                    document.org.clone(),
                    document.project.clone(),
                    document.env.clone(),
                    name.clone(),
                ));
                if producer.is_some_and(|producer| *producer < index) {
                    continue;
                }
                let target = producer.map_or_else(
                    || "absent".into(),
                    |position| format!("{}:{position}", objects[*position].0),
                );
                failures.push(format!("{} document {} step {step}:{index} field applications.{application}.connections.{alias} -> connection {}/{}/{}/{name} producer step {target}",identity(job),source_document(render,&document),document.org,document.project,document.env));
            }
        }
    }
    Ok(failures)
}

fn replace_job_document(
    objects: &mut [(&str, serde_json::Value)],
    job_name: &str,
    document: &wamn_control::environment::document::EnvironmentDocument,
) {
    use base64::Engine as _;
    let job = &objects
        .iter()
        .find(|(_, object)| object["kind"] == "Job" && object["metadata"]["name"] == job_name)
        .unwrap()
        .1;
    let volume = job["spec"]["template"]["spec"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == "inputs")
        .unwrap();
    let name = volume["secret"]["secretName"].clone();
    let key = volume["secret"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "environment.k")
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, secret) = objects
        .iter_mut()
        .find(|(_, object)| object["kind"] == "Secret" && object["metadata"]["name"] == name)
        .unwrap();
    if secret["stringData"].get(&key).is_some() {
        secret["stringData"][&key] = json!(document.to_kcl());
    } else {
        secret["data"][&key] =
            json!(base64::engine::general_purpose::STANDARD.encode(document.to_kcl()));
    }
}

#[test]
fn install_connections_precede_first_apply() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let failures = connection_failures(&render.objects, render)?;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_connections_name_an_uncreated_rendered_binding() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let mut document = render
        .documents
        .iter()
        .find(|(_, document)| document.project == "wms")
        .unwrap()
        .1
        .clone();
    document
        .applications
        .get_mut("wamn_wms")
        .unwrap()
        .connections
        .insert("labels".into(), "missing-connection".into());
    replace_job_document(&mut objects, "platform-apply-test--wms--dev", &document);
    let failures = connection_failures(&objects, render)?;
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("a-unqualified-wms.k") && failures[0].contains("connections.labels -> connection test/wms/dev/missing-connection producer step absent"));
    Ok(())
}

fn application_failures(
    objects: &[(&str, serde_json::Value)],
    render: &platform_install::Render,
) -> anyhow::Result<Vec<String>> {
    let EnvironmentJobs {
        documents,
        mut failures,
    } = environment_jobs(objects)?;
    for (index, document) in documents {
        let (step, job) = &objects[index];
        let key =
            wamn_control_registry::project_env_key(&document.org, &document.project, &document.env);
        let producer = objects
            .iter()
            .enumerate()
            .find(|(_, (_, object))| {
                object["kind"] == "ConfigMap"
                    && object["metadata"]["name"] == format!("wamn-install-platform-release-{key}")
            })
            .map_or_else(
                || "absent".into(),
                |(position, (step, _))| format!("{step}:{position}"),
            );
        let member = render
            .fixtures
            .releases
            .get(&key)
            .and_then(|manifest| manifest.composed_entry.as_ref())
            .map(|entry| &entry.application);
        let Some(member) = member else {
            failures.push(format!("{} document {} step {step}:{index} field release -> composed-entry.application producer step {producer}",identity(job),source_document(render,&document)));
            continue;
        };
        let package = &member.package_id;
        if wamn_catalog::application_for_package(&document.applications, package).is_some() {
            continue;
        }
        let owners = document
            .applications
            .iter()
            .filter(|(_, definition)| definition.packages.contains(package))
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        failures.push(format!("{} document {} step {step}:{index} field applications[*].packages -> composed-entry.application package {package} owners {owners:?} producer step {producer}",identity(job),source_document(render,&document)));
    }
    Ok(failures)
}

#[test]
fn install_documents_resolve_one_application() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let failures = application_failures(&render.objects, render)?;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_documents_reject_ambiguous_rendered_package_ownership() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let mut document = render
        .documents
        .iter()
        .find(|(_, document)| document.project == "wms")
        .unwrap()
        .1
        .clone();
    let duplicate = document.applications["wamn_wms"].clone();
    document.applications.insert("duplicate".into(), duplicate);
    replace_job_document(&mut objects, "platform-apply-test--wms--dev", &document);
    let failures = application_failures(&objects, render)?;
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(
        failures[0].contains("a-unqualified-wms.k")
            && failures[0].contains("package wamn_wms owners [\"duplicate\", \"wamn_wms\"]")
            && failures[0].contains("producer step 4.14:")
    );
    Ok(())
}

#[test]
fn install_documents_select_the_package_owner_with_multiple_applications() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let mut document = render
        .documents
        .iter()
        .find(|(_, document)| document.project == "wms")
        .unwrap()
        .1
        .clone();
    document.applications.insert(
        "other".into(),
        wamn_catalog::ApplicationDefinition {
            packages: ["unrelated-package".into()].into(),
            connections: std::collections::BTreeMap::new(),
        },
    );
    replace_job_document(&mut objects, "platform-apply-test--wms--dev", &document);
    let failures = application_failures(&objects, render)?;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn install_documents_reject_an_unowned_rendered_package() -> anyhow::Result<()> {
    let render = platform_install::render()?;
    let mut objects = render.objects.clone();
    let mut document = render
        .documents
        .iter()
        .find(|(_, document)| document.project == "wms")
        .unwrap()
        .1
        .clone();
    document.applications.clear();
    replace_job_document(&mut objects, "platform-apply-test--wms--dev", &document);
    let failures = application_failures(&objects, render)?;
    assert_eq!(failures.len(), 1, "{}", failures.join("\n"));
    assert!(failures[0].contains("package wamn_wms owners []"));
    Ok(())
}

#[cfg(feature = "test-util")]
fn copy_journey_inputs(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_journey_inputs(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(feature = "test-util")]
fn journey_apply(
    repository: &Path,
    render: &platform_install::Render,
) -> anyhow::Result<Vec<serde_json::Value>> {
    Ok(control::platform::render::install(
        &render.cluster,
        &render
            .documents
            .iter()
            .map(|(_, document)| document.clone())
            .collect::<Vec<_>>(),
        repository,
        &render.fixtures,
    )?
    .into_iter()
    .map(|(_, object)| object)
    .collect())
}

#[cfg(feature = "test-util")]
fn operator_cpu(objects: &[serde_json::Value]) -> &serde_json::Value {
    &objects
        .iter()
        .find(|object| {
            object["kind"] == "Deployment" && object["metadata"]["name"] == "runtime-operator"
        })
        .expect("the production operator deployment")["spec"]["template"]["spec"]["containers"][0]["resources"]
        ["requests"]["cpu"]
}

#[cfg(feature = "test-util")]
fn journey_cluster_change(
    project: &str,
    entry: fn(
        &Path,
        &control::platform::journey::ClusterInput<'_>,
    ) -> anyhow::Result<control::platform::journey::Cluster>,
) -> anyhow::Result<()> {
    use control::platform::{install, journey};
    let render = platform_install::build()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    copy_journey_inputs(&repository.join("deploy"), &work.path().join("deploy"))?;
    let original: Vec<_> = render
        .objects
        .iter()
        .map(|(_, object)| object.clone())
        .collect();
    assert_ne!(operator_cpu(&original), "137m");
    let archive = work
        .path()
        .join("deploy/platform/release/charts/runtime-operator-2.11.0.tgz");
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(work.path())
        .status()?;
    assert!(status.success(), "unpack the temporary operator chart");
    let values_path = work.path().join("runtime-operator/values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&values_path)?)?;
    values["operator"]["resources"]["requests"]["cpu"] = "137m".into();
    std::fs::write(values_path, serde_yaml::to_string(&values)?)?;
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(work.path())
        .arg("runtime-operator")
        .status()?;
    assert!(
        status.success(),
        "pack the changed temporary operator chart"
    );
    let scopes = [wamn_control_registry::Triple::new("test", project, "dev")];
    let bundle = entry(
        work.path(),
        &journey::ClusterInput {
            operator_namespace: "wamn-system",
            host_namespace: "wamn-system",
            org: "test",
            environments: &scopes,
        },
    )?;
    let rendered = install::helm_template(&bundle.operator)?;
    let apply = journey_apply(work.path(), &render)?;
    assert_eq!(
        operator_cpu(&apply),
        "137m",
        "platform apply uses the changed chart"
    );
    assert_eq!(
        operator_cpu(&rendered),
        operator_cpu(&apply),
        "{project} uses the changed production operator chart"
    );
    assert_eq!(
        bundle.operator["values"]["global"]["nats"]["schedulerUrl"],
        "nats://wamn-nats.wamn-system.svc.cluster.local:4222"
    );
    assert!(
        bundle
            .prerequisites
            .iter()
            .any(|object| object["kind"] == "ServiceAccount"
                && object["metadata"]["name"] == "wamn-host"
                && object["metadata"]["namespace"] == "wamn-system")
    );
    if project == "delivery" {
        let database =
            journey::delivery_database(work.path(), "application-database", "application-pg")?;
        assert_eq!(database.objects[1]["metadata"]["name"], "application-pg");
        assert_eq!(
            database.objects[1]["metadata"]["namespace"],
            "application-database"
        );
        assert_eq!(database.fixture_objects[0]["spec"]["type"], "NodePort");
        assert_eq!(
            database.fixture_objects[0]["spec"]["selector"]["cnpg.io/cluster"],
            "application-pg"
        );
        assert!(
            bundle
                .prerequisites
                .iter()
                .any(|object| object["kind"] == "Deployment"
                    && object["metadata"]["name"] == "cnpg-controller-manager")
        );
    } else {
        journey_release_change(project, work.path(), &render)?;
    }
    Ok(())
}

#[cfg(feature = "test-util")]
fn journey_release_change(
    project: &str,
    repository: &Path,
    render: &platform_install::Render,
) -> anyhow::Result<()> {
    use control::platform::{install, journey};
    use control::release_chart;
    let environment = &render
        .documents
        .iter()
        .find(|(_, document)| document.project == project)
        .expect("journey environment")
        .1;
    let key = wamn_control_registry::project_env_key(
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let chart = render.fixtures.directory.join("release-chart");
    let defaults = chart.join("values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&defaults)?)?;
    values["platform"]["roles"]["http"]["maxConcurrency"] = 7.into();
    std::fs::write(defaults, serde_yaml::to_string(&values)?)?;
    let values = release_chart::ValuesInput {
        org: environment.org.clone(),
        project: project.into(),
        env: environment.env.clone(),
        manifest_digest: environment.release.to_string(),
        artifact_base: "registry.invalid/wamn/releases".into(),
        route_host: environment.route_host.clone(),
        applications: environment.applications.clone(),
        roles: release_chart::roles(
            &render.fixtures.releases[&key],
            &environment.org,
            project,
            &environment.env,
        ),
        drain_bound_seconds: render.fixtures.drain_bound_seconds,
        actor: render.fixtures.actor.clone(),
        host_group: serde_yaml::from_str(
            "{env: [{name: WAMN_COMPONENT_ARTIFACT_BASE, value: registry.invalid/wamn/components}]}",
        )?,
    };
    let bundle = journey::release(&journey::ReleaseInput {
        chart: &chart,
        namespace: "wamn-system",
        values: &values,
        nats_url: "nats://wamn-nats.wamn-system.svc.cluster.local:4222",
        otlp_endpoint: "http://otel-collector.wamn-system.svc.cluster.local:4317",
        insecure_registry: true,
    })?;
    let rendered = install::helm_template(&bundle.chart)?;
    let apply = journey_apply(repository, render)?;
    let name = format!(
        "{}-http",
        release_chart::release_name(&environment.org, project, &environment.env)?
    );
    let concurrency = |objects: &[serde_json::Value]| {
        objects
            .iter()
            .find(|object| {
                object["kind"] == "WorkloadDeployment" && object["metadata"]["name"] == name
            })
            .expect("HTTP workload")["spec"]["template"]["spec"]["components"][0]["maxConcurrency"]
            .clone()
    };
    assert_eq!(
        concurrency(&apply),
        7,
        "platform apply uses the changed release defaults"
    );
    assert_eq!(
        concurrency(&rendered),
        concurrency(&apply),
        "the live release adapter uses the changed defaults"
    );
    assert!(
        bundle
            .objects
            .iter()
            .any(|object| object["kind"] == "ServiceAccount")
    );
    assert_eq!(
        bundle.chart["values"]["applications"],
        serde_json::to_value(&values.applications)?
    );
    Ok(())
}

#[cfg(feature = "test-util")]
#[test]
fn receiving_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("receiving", control::platform::journey::cluster)
}

#[cfg(feature = "test-util")]
#[test]
fn wms_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("wms", control::platform::journey::cluster)
}

#[cfg(feature = "test-util")]
#[test]
fn delivery_uses_install_builders() -> anyhow::Result<()> {
    journey_cluster_change("delivery", control::platform::journey::delivery)
}

#[cfg(feature = "test-util")]
#[test]
fn identity_uses_install_builders() -> anyhow::Result<()> {
    use control::platform::{identity, install, journey};
    let render = platform_install::build()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    copy_journey_inputs(&repository.join("deploy"), &work.path().join("deploy"))?;
    let values_path = work.path().join("deploy/platform/identity/values.yaml");
    let mut values: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&values_path)?)?;
    values["resources"]["requests"]["cpu"] = "139m".into();
    std::fs::write(values_path, serde_yaml::to_string(&values)?)?;
    let apply = journey_apply(work.path(), &render)?;
    let cpu = |objects: &[serde_json::Value], name: &str| {
        objects
            .iter()
            .find(|object| object["kind"] == "Deployment" && object["metadata"]["name"] == name)
            .expect("identity deployment")["spec"]["template"]["spec"]["containers"][0]["resources"]
            ["requests"]["cpu"]
            .clone()
    };
    assert_eq!(cpu(&apply, "identity"), "139m");
    for mode in [
        identity::Mode::Foundation,
        identity::Mode::Session {
            targets: vec!["fixture-target".into()],
            nats: None,
        },
    ] {
        let foundation = matches!(mode, identity::Mode::Foundation);
        let entry = journey::identity(&identity::ChartInput {
            chart: &work.path().join("deploy/platform/identity"),
            name: "fixture-identity",
            namespace: "fixture-identity",
            image: "wamn-identity:fixture",
            pull_policy: "Never",
            issuer: "https://fixture-identity.fixture-identity.svc.cluster.local",
            database_secret: "fixture-db",
            tls_secret: "fixture-tls",
            operator_ca_secret: "",
            resend_secret: "",
            resend_from: "",
            invite_url: "",
            mode,
        })?;
        let rendered = install::helm_template(&entry)?;
        assert_eq!(cpu(&rendered, "fixture-identity"), cpu(&apply, "identity"));
        assert_eq!(entry["values"]["authNats"]["url"], "");
        assert_eq!(
            entry["values"]["sessionTargetSecrets"],
            if foundation {
                json!([])
            } else {
                json!(["fixture-target"])
            }
        );
        let deployment = rendered
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        let volumes = deployment["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let variables = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        assert!(
            variables
                .iter()
                .any(|variable| variable["name"] == "WAMN_IDENTITY_DATABASE_URL"
                    && variable["valueFrom"]["secretKeyRef"]["name"] == "fixture-db")
        );
        assert!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-tls")
        );
        assert_eq!(
            volumes
                .iter()
                .any(|volume| volume["secret"]["secretName"] == "fixture-target"),
            !foundation
        );
    }
    Ok(())
}

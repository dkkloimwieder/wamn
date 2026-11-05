//! Render the platform install batches without a cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::{Value, json};

/// Live values supplied to the install batches instead of reading a cluster.
#[derive(Debug)]
pub struct Fixtures {
    pub identity_tls: Value,
    pub drain_bound_seconds: u32,
    pub actor: String,
    pub edge_certificate: String,
    pub proxy_uid: String,
    pub https_results: Vec<Value>,
    pub web_hosts: BTreeSet<String>,
    pub control_operator: Value,
    pub platform_ui_files: BTreeMap<String, Vec<u8>>,
    pub cdc_credentials: BTreeMap<String, Value>,
    pub host_image: String,
    pub releases: BTreeMap<String, wamn_catalog::ServingManifest>,
    pub web_clients: BTreeMap<String, BTreeMap<String, String>>,
    pub applied: BTreeMap<String, Vec<Value>>,
    pub oauth_key: [u8; 32],
    pub oauth_exists: bool,
    pub registry: wamn_package_artifact::PackageRegistry,
    pub identity_database: Value,
    pub identity_image: String,
    pub postgres_image: String,
    pub environment_urls: BTreeMap<String, String>,
    pub ctl_nats: Value,
    pub directory: PathBuf,
    pub apply_url: String,
    pub postgres_superuser: Value,
    pub runner_image: String,
    pub job_uid: String,
    pub emitted: BTreeMap<String, Value>,
    pub outputs: BTreeMap<String, String>,
    pub broker_authorization_exists: bool,
}

use super::document::ClusterDocument;
use crate::environment::document::EnvironmentDocument;

/// Render a fresh kind install in apply order without executing it.
///
/// # Errors
/// Returns an error if an install manifest cannot be read or decoded.
pub fn install(
    document: &ClusterDocument,
    environments: &[EnvironmentDocument],
    repository: &Path,
    fixtures: &Fixtures,
) -> anyhow::Result<Vec<(&'static str, Value)>> {
    let mut objects = Vec::new();
    append(
        &mut objects,
        "4.3",
        super::install::cert_manager_objects(repository, document.provider)?,
    );
    append(
        &mut objects,
        "4.3",
        super::install::edge_certificate_objects(repository, document)?,
    );
    append(&mut objects, "4.4", super::install::runtime_namespaces());
    append(
        &mut objects,
        "4.4",
        super::install::runtime_certificates(repository, document)?,
    );
    append(
        &mut objects,
        "4.4",
        super::install::nats_objects(repository, document, fixtures.broker_authorization_exists)?,
    );
    append(
        &mut objects,
        "4.4",
        super::install::helm_template(&super::install::operator_objects(repository, document)?[0])?,
    );
    append(
        &mut objects,
        "4.4",
        super::install::runtime_events(repository, "hosts", "platform")?,
    );
    append(
        &mut objects,
        "4.4.1",
        super::install::collector_objects(repository, document)?,
    );
    append(
        &mut objects,
        "4.5",
        super::install::postgres_operator_objects(repository, document.provider)?,
    );
    append(
        &mut objects,
        "4.5",
        super::install::postgres_cluster_objects(repository, document)?,
    );
    let superuser = serde_json::from_value(fixtures.postgres_superuser.clone())?;
    let admin = super::install::administrator(&superuser)?;
    for batch in super::install::provisioning_inputs(document, &fixtures.runner_image, &superuser)?
    {
        append(
            &mut objects,
            "4.7",
            job_objects(&batch, "platform", fixtures)?,
        );
    }
    append(
        &mut objects,
        "4.7",
        super::install::identity_certificate_objects(repository, document)?,
    );
    let identity_tls = serde_json::from_value(fixtures.identity_tls.clone())?;
    append(
        &mut objects,
        "4.7",
        super::install::identity_ca_objects(&identity_tls)?,
    );
    let authorization = objects
        .iter()
        .find(|(_, object)| {
            object["kind"] == "Secret" && object["metadata"]["name"] == "wamn-nats-authorization"
        })
        .map(|(_, object)| object.clone())
        .context("missing initial authorization fixture")?;
    append(
        &mut objects,
        "4.8",
        super::broker::objects(&authorization, &document.org, environments)?,
    );
    append(
        &mut objects,
        "4.8",
        super::install::nats_objects(repository, document, true)?,
    );
    append(
        &mut objects,
        "4.9",
        super::manifests::platform_inputs(repository, document)?,
    );
    let auth = serde_json::from_value(fixtures.ctl_nats.clone())?;
    for environment in environments {
        let mut environment = environment.clone();
        let batch = super::environments::apply_request(
            document,
            &mut environment,
            &fixtures.runner_image,
            true,
            &auth,
            &fixtures.directory.join("release-chart"),
            (&admin, &fixtures.apply_url),
        )?;
        append(
            &mut objects,
            "4.11",
            job_objects(&batch, "platform", fixtures)?,
        );
    }
    let mut targets = Vec::new();
    for environment in environments {
        let key = environment_key(environment);
        let url = fixtures
            .environment_urls
            .get(&key)
            .context("environment URL fixture")?;
        let batch =
            super::identity::session_inputs(environment, &fixtures.runner_image, &admin, url)?;
        append(
            &mut objects,
            "4.12",
            job_objects(&batch, "identity", fixtures)?,
        );
        targets.push(format!("wamn-session-role-reader-{key}"));
    }
    let chart =
        super::identity::chart_objects(repository, document, &fixtures.identity_image, targets)?;
    let values = chart[0]["values"].clone();
    append(
        &mut objects,
        "4.12",
        super::install::helm_template(&chart[0])?,
    );
    let probe = super::identity::key_probe(
        &fixtures.postgres_image,
        &superuser,
        &fixtures.identity_image,
    )?;
    append(
        &mut objects,
        "4.12",
        job_objects(&probe, "platform", fixtures)?,
    );
    let state: Value = serde_json::from_str(
        fixtures
            .outputs
            .get(&probe.0.name)
            .context("session key state fixture")?,
    )?;
    let kid = if let Some(kid) = state["kid"].as_str() {
        kid.to_owned()
    } else {
        let credential = serde_json::from_value(fixtures.identity_database.clone())?;
        let publish = super::identity::key_publish(&fixtures.identity_image, &credential)?;
        append(
            &mut objects,
            "4.12",
            job_objects(&publish, "identity", fixtures)?,
        );
        let public: Value = serde_json::from_str(
            fixtures
                .outputs
                .get(&publish.0.name)
                .context("published key fixture")?,
        )?;
        let kid = public["kid"]
            .as_str()
            .context("published kid fixture")?
            .to_owned();
        let activate =
            super::identity::key_activate_inputs(&fixtures.identity_image, &kid, &publish.1);
        append(
            &mut objects,
            "4.12",
            job_objects(&activate, "identity", fixtures)?,
        );
        kid
    };
    let batch = super::identity::check_inputs(&identity_tls, &kid, &values)?;
    append(
        &mut objects,
        "4.12",
        job_objects(&batch, "hosts", fixtures)?,
    );
    for environment in environments
        .iter()
        .filter(|environment| !environment.integrations.is_empty())
    {
        let files = super::environments::connection_documents(
            &repository.join("deploy/gcp/connections"),
            environment,
        )?;
        let batch = super::environments::connection_inputs(
            document,
            environment,
            &fixtures.runner_image,
            &auth,
            files,
            &admin,
        )?;
        append(
            &mut objects,
            "4.16.1",
            job_objects(&batch, "hosts", fixtures)?,
        );
    }
    if !environments.is_empty() && !fixtures.oauth_exists {
        append(
            &mut objects,
            "4.17",
            super::oauth::key_objects(&fixtures.oauth_key),
        );
    }
    for environment in environments {
        let key = environment_key(environment);
        let mut environment = environment.clone();
        let mut batch = super::environments::apply_request(
            document,
            &mut environment,
            &fixtures.runner_image,
            false,
            &auth,
            &fixtures.directory.join("release-chart"),
            (&admin, &fixtures.apply_url),
        )?;
        let database = url::Url::parse(
            fixtures
                .environment_urls
                .get(&key)
                .context("environment URL fixture")?,
        )?;
        let suffix = database
            .path()
            .rsplit_once("--")
            .context("fixture database instance suffix")?
            .1;
        let source = std::fs::read_to_string(repository.join(format!(
            "deploy/gcp/host-group-{}.yaml",
            environment.project
        )))?;
        super::environments::released_request_inputs(
            document,
            &environment,
            &mut batch.0,
            &fixtures.registry,
            &fixtures.directory.join("release-chart"),
            &source,
            suffix,
        )?;
        let group = fixtures
            .directory
            .join(format!("host-group-{}.yaml", environment.project));
        std::fs::write(
            &group,
            batch
                .0
                .files
                .get("host-group.yaml")
                .context("rendered host group")?,
        )?;
        let manifest = fixtures
            .releases
            .get(&key)
            .context("release manifest fixture")?;
        let (values, _) = crate::environment::write::release_values(
            &environment,
            manifest,
            &environment.release.to_string(),
            &super::packages::artifact_base(&fixtures.registry, "releases")?,
            fixtures.drain_bound_seconds,
            &fixtures.actor,
            Some(&group),
            fixtures
                .web_clients
                .get(&key)
                .context("web client fixture")?,
        )?;
        let entry = super::environments::release_chart_objects(
            &fixtures.directory.join("release-chart"),
            &environment,
            &values,
        )?;
        let chart_objects = super::install::helm_template(&entry[0])?;
        append(
            &mut objects,
            "4.17",
            job_objects_with(&batch, "platform", fixtures, &chart_objects)?,
        );
    }
    append(&mut objects, "4.18", vec![super::cdc::account()]);
    for environment in environments {
        if fixtures
            .cdc_credentials
            .contains_key(&environment_key(environment))
        {
            append(
                &mut objects,
                "4.18",
                vec![super::cdc::deployment(environment, &fixtures.host_image)],
            );
        }
    }
    if !document.buckets.web.is_empty() {
        for environment in environments
            .iter()
            .filter(|environment| environment.client.is_some())
        {
            let batch = super::web::upload_request(
                environment,
                &document.buckets.web,
                &fixtures.runner_image,
                &fixtures.registry,
                &auth,
                &fixtures.directory.join("release-chart"),
            )?;
            append(
                &mut objects,
                "4.19",
                job_objects(&batch, "hosts", fixtures)?,
            );
        }
    }
    let batch = super::control::provision_inputs(document, &fixtures.runner_image);
    append(
        &mut objects,
        "4.19.1",
        job_objects(&batch, "platform", fixtures)?,
    );
    let batch = super::control::operator_inputs(
        document,
        &fixtures.runner_image,
        fixtures.control_operator["metadata"]["resourceVersion"]
            .as_str()
            .context("operator resource version fixture")?,
    )?;
    append(
        &mut objects,
        "4.19.1",
        job_objects(&batch, "platform", fixtures)?,
    );
    if environments.is_empty() && !fixtures.oauth_exists {
        append(
            &mut objects,
            "4.19.1",
            super::oauth::key_objects(&fixtures.oauth_key),
        );
    }
    let mut control_values = super::control::release_values(document)?;
    append(
        &mut objects,
        "4.19.1",
        super::control::registry_ca_objects(
            &document.org,
            &fixtures.registry,
            &mut control_values,
        )?,
    );
    let entry = super::control::chart_objects(
        document,
        &fixtures.directory.join("release-chart"),
        &control_values,
    )?;
    append(
        &mut objects,
        "4.19.1",
        super::install::helm_template(&entry[0])?,
    );
    let chart: Value = serde_yaml::from_slice(&std::fs::read(
        fixtures.directory.join("release-chart/values.yaml"),
    )?)?;
    append(
        &mut objects,
        "4.19.1",
        super::control::service_objects(repository, document, &chart)?,
    );
    // Control host credentials and release precede this upload.
    if !document.buckets.web.is_empty() {
        let batch = super::web::platform_ui_with_files(
            document,
            &fixtures.runner_image,
            fixtures.platform_ui_files.clone(),
        );
        append(
            &mut objects,
            "4.19.1",
            job_objects(&batch, "hosts", fixtures)?,
        );
    }
    let owner_state = super::invite::state_inputs(document, &fixtures.postgres_image);
    append(
        &mut objects,
        "4.19.1",
        job_objects(&owner_state, "platform", fixtures)?,
    );
    if super::invite::needs_invite(
        fixtures
            .outputs
            .get(&owner_state.0.name)
            .context("owner state fixture")?,
    )? {
        let batch = super::invite::invite_inputs(document, &fixtures.runner_image, &auth)?;
        append(
            &mut objects,
            "4.19.1",
            job_objects(&batch, "platform", fixtures)?,
        );
    }
    let routes = super::edge::routes(document, environments)?;
    let chart: Value = serde_yaml::from_slice(&std::fs::read(
        fixtures.directory.join("release-chart/values.yaml"),
    )?)?;
    append(
        &mut objects,
        "4.20",
        super::edge::objects(document, &routes, &chart)?,
    );
    if !document.buckets.web.is_empty() {
        let batch = super::edge::check_inputs(document, &routes, &chart, &fixtures.web_hosts)?;
        append(&mut objects, "4.20", job_objects(&batch, "edge", fixtures)?);
    }
    let entry = super::edge::proxy_objects(repository, document, &routes)?;
    append(
        &mut objects,
        "4.20",
        super::install::helm_template(&entry[0])?,
    );
    let mut routes = vec![(
        document.route_hosts.control.clone(),
        format!("c-{}", document.org),
        "GET".to_owned(),
        "/api/wamn_control/control/mine".to_owned(),
    )];
    for environment in environments {
        let manifest = fixtures
            .releases
            .get(&environment_key(environment))
            .context("release manifest fixture")?;
        let (method, path) = super::check::api_route(manifest)?;
        routes.push((
            environment.route_host.clone(),
            wamn_release_control::release_chart::release_name(
                &environment.org,
                &environment.project,
                &environment.env,
            )?,
            method,
            path,
        ));
    }
    for (host, _, method, path) in &routes {
        for (method, path, _) in super::check::requests(
            method,
            path,
            !document.buckets.web.is_empty() && fixtures.web_hosts.contains(host),
        ) {
            let batch = super::check::kind_inputs(host, method, path);
            append(&mut objects, "4.21", job_objects(&batch, "edge", fixtures)?);
        }
    }
    let releases = routes
        .iter()
        .map(|(_, name, _, _)| {
            objects
                .iter()
                .rev()
                .find(|(_, object)| {
                    object["kind"] == "ConfigMap" && object["metadata"]["name"] == *name
                })
                .map(|(_, object)| object["data"].clone())
                .context("rendered release ConfigMap")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let proxy = objects
        .iter()
        .find(|(_, object)| {
            object["kind"] == "Deployment" && object["metadata"]["name"] == "wamn-edge"
        })
        .context("rendered proxy Deployment")?;
    let digest = super::check::completion_digest(
        document,
        &releases,
        &fixtures.edge_certificate,
        &proxy.1["spec"],
        &json!(fixtures.proxy_uid),
    );
    append(
        &mut objects,
        "4.21",
        super::check::completion_objects(document, &digest, &fixtures.https_results)?,
    );
    Ok(objects)
}

fn environment_key(environment: &EnvironmentDocument) -> String {
    wamn_control_registry::project_env_key(&environment.org, &environment.project, &environment.env)
}

fn job_objects(
    batch: &super::job::JobInputs,
    namespace: &str,
    fixtures: &Fixtures,
) -> anyhow::Result<Vec<Value>> {
    job_objects_with(batch, namespace, fixtures, &[])
}

fn job_objects_with(
    batch: &super::job::JobInputs,
    namespace: &str,
    fixtures: &Fixtures,
    nested: &[Value],
) -> anyhow::Result<Vec<Value>> {
    let (request, secrets, inputs) = batch;
    let mut objects = super::job::start_objects(request, namespace, secrets)?;
    let owner = json!({"name":request.name, "namespace":namespace, "uid":fixtures.job_uid});
    objects.extend(super::job::support_objects(request, &owner, secrets)?);
    objects.push(super::job::resume_object(request, namespace, secrets)?);
    if request.emits_secret {
        let output = fixtures
            .emitted
            .get(&request.name)
            .ok_or_else(|| anyhow::anyhow!("missing emitted fixture {}", request.name))?;
        objects.extend(super::job::emitted_objects(&output.to_string())?);
    }
    if let Some(applied) = fixtures.applied.get(&request.name) {
        objects.extend_from_slice(applied);
    }
    objects.extend_from_slice(nested);
    let output = if request.capture_output {
        fixtures
            .outputs
            .get(&request.name)
            .ok_or_else(|| anyhow::anyhow!("missing stdout fixture {}", request.name))?
            .as_str()
    } else {
        ""
    };
    objects.extend(super::job::completion_objects(
        request,
        &super::job::completion_digest(request, inputs)?,
        output,
    )?);
    Ok(objects)
}

fn append(objects: &mut Vec<(&'static str, Value)>, step: &'static str, batch: Vec<Value>) {
    objects.extend(
        batch
            .into_iter()
            .map(|object| (step, super::manifests::apply_namespace(object, "platform"))),
    );
}

#[cfg(test)]
use crate as control;
#[cfg(test)]
#[path = "../../tests/support/platform_install.rs"]
mod fixture;
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::Value;

    use super::super::job_contract::{self, Contract, Source};
    use crate::environment::document::{DeclaredRelease, EnvironmentDocument};

    #[test]
    fn kind_install_renders_in_install_order() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let objects = &render.objects;
        let steps = super::super::steps::install_steps(&render.cluster, "render");
        assert_eq!(
            objects.iter().any(|(step, _)| *step == "4.19"),
            render
                .documents
                .iter()
                .any(|(_, environment)| environment.client.is_some()),
            "web upload Jobs require a client pin"
        );
        let positions = objects
            .iter()
            .map(|(number, _)| {
                steps
                    .iter()
                    .position(|step| step.number == *number)
                    .expect("known install step")
            })
            .collect::<Vec<_>>();
        assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
        Ok(())
    }

    fn pod_spec(object: &Value) -> Option<&Value> {
        match object["kind"].as_str()? {
            "Pod" => Some(&object["spec"]),
            // A suspended Job declares the pod; its resumed rendering consumes inputs.
            "Job" if object["spec"]["suspend"] == true => None,
            "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" => {
                Some(&object["spec"]["template"]["spec"])
            }
            "CronJob" => Some(&object["spec"]["jobTemplate"]["spec"]["template"]["spec"]),
            _ => None,
        }
    }

    fn pods(render: &super::fixture::Render) -> impl Iterator<Item = (usize, &Value, &Value)> {
        render
            .objects
            .iter()
            .enumerate()
            .filter_map(|(index, (_, object))| pod_spec(object).map(|spec| (index, object, spec)))
    }

    fn containers(spec: &Value) -> impl Iterator<Item = &Value> {
        spec["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(spec["initContainers"].as_array().into_iter().flatten())
    }

    fn object<'a>(
        render: &'a super::fixture::Render,
        before: usize,
        kind: &str,
        namespace: &Value,
        name: &Value,
    ) -> Option<&'a Value> {
        render.objects[..before]
            .iter()
            .rev()
            .map(|(_, value)| value)
            .find(|value| {
                value["kind"] == kind
                    && &value["metadata"]["namespace"] == namespace
                    && &value["metadata"]["name"] == name
            })
    }

    fn volume_sources(volume: &Value) -> impl Iterator<Item = (&'static str, &Value)> {
        std::iter::once(volume)
            .chain(
                volume["projected"]["sources"]
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .flat_map(|source| {
                [
                    ("Secret", &source["secret"]),
                    ("ConfigMap", &source["configMap"]),
                ]
            })
            .filter(|(_, source)| !source.is_null())
    }

    fn input_sources(spec: &Value) -> Vec<(&'static str, &Value)> {
        let mut sources = spec["volumes"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(volume_sources)
            .collect::<Vec<_>>();
        for container in containers(spec) {
            for entry in container["env"].as_array().into_iter().flatten() {
                sources.extend(
                    [
                        ("Secret", &entry["valueFrom"]["secretKeyRef"]),
                        ("ConfigMap", &entry["valueFrom"]["configMapKeyRef"]),
                    ]
                    .into_iter()
                    .filter(|(_, source)| !source.is_null()),
                );
            }
            for entry in container["envFrom"].as_array().into_iter().flatten() {
                sources.extend(
                    [
                        ("Secret", &entry["secretRef"]),
                        ("ConfigMap", &entry["configMapRef"]),
                    ]
                    .into_iter()
                    .filter(|(_, source)| !source.is_null()),
                );
            }
        }
        sources
    }

    fn input<'a>(
        render: &'a super::fixture::Render,
        before: usize,
        kind: &str,
        namespace: &Value,
        name: &Value,
    ) -> Option<&'a Value> {
        object(render, before, kind, namespace, name)
            .or_else(|| {
                render.owner_inputs.iter().find(|value| {
                    value["kind"] == kind
                        && &value["metadata"]["namespace"] == namespace
                        && &value["metadata"]["name"] == name
                })
            })
            .or_else(|| {
                (kind == "Secret")
                    .then(|| {
                        render.objects[..before]
                            .iter()
                            .rev()
                            .map(|(_, value)| value)
                            .find(|value| {
                                if &value["metadata"]["namespace"] != namespace {
                                    return false;
                                }
                                if value["kind"] == "Certificate" {
                                    return &value["spec"]["secretName"] == name;
                                }
                                if value["kind"] != "Cluster"
                                    || value["spec"]["enableSuperuserAccess"] != true
                                    || !value["apiVersion"].as_str().is_some_and(|version| {
                                        version.starts_with("postgresql.cnpg.io/")
                                    })
                                {
                                    return false;
                                }
                                if value["spec"]["superuserSecret"]["name"].is_string() {
                                    return &value["spec"]["superuserSecret"]["name"] == name;
                                }
                                // CNPG defines the default Secret suffix outside the rendered Cluster.
                                name == &format!(
                                    "{}-superuser",
                                    value["metadata"]["name"].as_str().unwrap()
                                )
                            })
                    })
                    .flatten()
            })
    }

    fn mounted_input<'a>(
        render: &'a super::fixture::Render,
        index: usize,
        spec: &'a Value,
        container: &Value,
        path: &str,
    ) -> Option<(&'a Value, Option<&'a str>)> {
        let namespace = &render.objects[index].1["metadata"]["namespace"];
        for mount in container["volumeMounts"].as_array().into_iter().flatten() {
            let base = mount["mountPath"].as_str().unwrap();
            let Some(relative) = path
                .strip_prefix(base)
                .filter(|tail| tail.is_empty() || tail.starts_with('/'))
            else {
                continue;
            };
            let relative = relative.trim_start_matches('/');
            let volume = spec["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|volume| volume["name"] == mount["name"])
                .unwrap();
            for (kind, source) in volume_sources(volume) {
                let name = source.get("secretName").unwrap_or(&source["name"]);
                let Some(producer) = input(render, index, kind, namespace, name) else {
                    continue;
                };
                let key = if relative.is_empty() {
                    None
                } else if let Some(items) = source["items"].as_array() {
                    if let Some(item) = items.iter().find(|item| item["path"] == relative) {
                        Some(item["key"].as_str().unwrap())
                    } else if items.iter().any(|item| {
                        item["path"]
                            .as_str()
                            .unwrap()
                            .starts_with(&format!("{relative}/"))
                    }) {
                        None
                    } else {
                        continue;
                    }
                } else {
                    // The key belongs to the rendered producer, not to the caller's path.
                    let key = producer["data"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .chain(producer["stringData"].as_object().into_iter().flatten())
                        .map(|(key, _)| key.as_str())
                        .find(|key| *key == relative);
                    if key.is_none()
                        && matches!(producer["kind"].as_str(), Some("Secret" | "ConfigMap"))
                    {
                        continue;
                    }
                    key
                };
                if let Some(key) = key
                    && matches!(producer["kind"].as_str(), Some("Secret" | "ConfigMap"))
                    && producer["data"].get(key).is_none()
                    && producer["stringData"].get(key).is_none()
                {
                    continue;
                }
                return Some((producer, key));
            }
        }
        None
    }

    fn mounted_text(
        render: &super::fixture::Render,
        index: usize,
        spec: &Value,
        container: &Value,
        path: &str,
    ) -> anyhow::Result<String> {
        let (producer, key) = mounted_input(render, index, spec, container, path).unwrap();
        let key = key.unwrap();
        if let Some(text) = producer["stringData"][key].as_str() {
            return Ok(text.to_owned());
        }
        Ok(String::from_utf8(
            STANDARD.decode(producer["data"][key].as_str().unwrap())?,
        )?)
    }

    fn contract(container: &Value) -> anyhow::Result<&'static Contract> {
        let command: Vec<String> = serde::Deserialize::deserialize(&container["command"])?;
        let arguments: Vec<String> = serde::Deserialize::deserialize(&container["args"])?;
        job_contract::resolve(&command, &arguments)
    }

    fn environment_jobs(
        render: &super::fixture::Render,
    ) -> anyhow::Result<Vec<(usize, &Value, &'static str, EnvironmentDocument)>> {
        let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let mut documents = Vec::new();
        for (index, _, spec) in pods(render).filter(|(_, object, _)| object["kind"] == "Job") {
            for container in containers(spec) {
                let contract = contract(container)?;
                // These command verbs define consumers of an environment document.
                if !matches!(contract.verb, "env apply" | "env connections") {
                    continue;
                }
                let path = container["args"][2].as_str().unwrap();
                let text = mounted_text(render, index, spec, container, path)?;
                let file = work.path().join(format!("{index}.k"));
                std::fs::write(&file, text)?;
                let document = EnvironmentDocument::compile(&file)?;
                documents.push((index, container, contract.verb, document));
            }
        }
        Ok(documents)
    }

    #[test]
    fn install_service_accounts_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let consumers = pods(render)
            .filter(|(_, _, spec)| spec["serviceAccountName"].is_string())
            .collect::<Vec<_>>();
        assert!(!consumers.is_empty());
        for (index, consumer, spec) in consumers {
            assert!(
                object(
                    render,
                    index,
                    "ServiceAccount",
                    &consumer["metadata"]["namespace"],
                    &spec["serviceAccountName"]
                )
                .is_some(),
                "{}",
                consumer["metadata"]["name"]
            );
        }
        Ok(())
    }

    #[test]
    fn install_inputs_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let consumers = pods(render)
            .flat_map(|(index, consumer, spec)| {
                input_sources(spec)
                    .into_iter()
                    // Kubernetes permits an optional reference to omit its producer.
                    .filter(|(_, source)| source["optional"] != true)
                    .map(move |(kind, source)| (index, consumer, kind, source))
            })
            .collect::<Vec<_>>();
        assert!(!consumers.is_empty());
        for (index, consumer, kind, source) in consumers {
            let name = source.get("secretName").unwrap_or(&source["name"]);
            assert!(
                input(
                    render,
                    index,
                    kind,
                    &consumer["metadata"]["namespace"],
                    name
                )
                .is_some(),
                "{} -> {kind}/{name}",
                consumer["metadata"]["name"]
            );
        }
        Ok(())
    }

    #[test]
    fn install_roles_cover_commands() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let mut permissions = 0;
        for (index, job, spec) in pods(render).filter(|(_, object, _)| object["kind"] == "Job") {
            for container in containers(spec) {
                for access in contract(container)?.access {
                    assert_ne!(spec["automountServiceAccountToken"], false);
                    for resource in access.resources {
                        for verb in access.verbs {
                            permissions += 1;
                            assert!(
                                render.objects[..index].iter().any(|(_, binding)| {
                                    binding["kind"] == "RoleBinding"
                                        && binding["metadata"]["namespace"] == access.namespace
                                        && binding["subjects"].as_array().unwrap().iter().any(
                                            |subject| {
                                                subject["kind"] == "ServiceAccount"
                                                    && subject["name"] == spec["serviceAccountName"]
                                                    && subject["namespace"]
                                                        == job["metadata"]["namespace"]
                                            },
                                        )
                                        && binding["roleRef"]["kind"] == "Role"
                                        && object(
                                            render,
                                            index,
                                            "Role",
                                            &binding["metadata"]["namespace"],
                                            &binding["roleRef"]["name"],
                                        )
                                        .is_some_and(
                                            |role| {
                                                role["rules"].as_array().unwrap().iter().any(
                                                    |rule| {
                                                        [
                                                            ("apiGroups", access.group),
                                                            ("resources", *resource),
                                                            ("verbs", *verb),
                                                        ]
                                                        .into_iter()
                                                        .all(|(field, required)| {
                                                            rule[field]
                                                                .as_array()
                                                                .unwrap()
                                                                .iter()
                                                                .any(|value| {
                                                                    value == required
                                                                        || value == "*"
                                                                })
                                                        })
                                                    },
                                                )
                                            },
                                        )
                                }),
                                "{} {resource}/{verb}",
                                job["metadata"]["name"]
                            );
                        }
                    }
                }
            }
        }
        assert!(permissions > 0);
        Ok(())
    }

    #[test]
    fn install_command_inputs() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let mut inputs = 0;
        for (index, job, spec) in pods(render).filter(|(_, object, _)| object["kind"] == "Job") {
            for container in containers(spec) {
                let contract = contract(container)?;
                for path in contract.files {
                    inputs += 1;
                    assert!(
                        mounted_input(render, index, spec, container, path).is_some(),
                        "{} {path}",
                        job["metadata"]["name"]
                    );
                }
                for export in contract.exports {
                    inputs += 1;
                    match export.source {
                        Source::File(path) => {
                            assert!(mounted_input(render, index, spec, container, path).is_some());
                        }
                        Source::Secret {
                            namespace, name, ..
                        } => {
                            let mut name = name.to_owned();
                            for entry in container["env"].as_array().into_iter().flatten() {
                                if let Some(value) = entry["value"].as_str() {
                                    name = name.replace(
                                        &format!("${}", entry["name"].as_str().unwrap()),
                                        value,
                                    );
                                }
                            }
                            assert!(
                                input(
                                    render,
                                    index,
                                    "Secret",
                                    &Value::from(namespace),
                                    &Value::from(name)
                                )
                                .is_some()
                            );
                        }
                    }
                }
                let shell = container["command"][0]
                    .as_str()
                    .is_some_and(|program| program.rsplit('/').next() == Some("sh"));
                for argument in container["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .skip(usize::from(shell))
                {
                    for name in argument
                        .as_str()
                        .unwrap()
                        .split("$(")
                        .skip(1)
                        .filter_map(|part| part.split_once(')').map(|(name, _)| name))
                    {
                        if name.is_empty()
                            || !name
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                        {
                            continue;
                        }
                        inputs += 1;
                        assert!(
                            container["env"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|entry| entry["name"] == name)
                                || contract
                                    .exports
                                    .iter()
                                    .any(|export| export.variable == name)
                        );
                    }
                }
                for entry in container["env"].as_array().into_iter().flatten() {
                    if contract.required.contains(&entry["name"].as_str().unwrap())
                        && let Some(path) = entry["value"]
                            .as_str()
                            .filter(|value| value.starts_with('/'))
                    {
                        inputs += 1;
                        assert!(mounted_input(render, index, spec, container, path).is_some());
                    }
                }
            }
        }
        assert!(inputs > 0);
        Ok(())
    }

    #[test]
    fn install_service_targets_match() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let latest = render
            .objects
            .iter()
            .map(|(_, object)| {
                (
                    (
                        object["kind"].as_str().unwrap(),
                        object["metadata"]["namespace"].as_str(),
                        object["metadata"]["name"].as_str().unwrap(),
                    ),
                    object,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let services = latest
            .values()
            .copied()
            .filter(|object| object["kind"] == "Service")
            .collect::<Vec<_>>();
        assert!(!services.is_empty());
        for service in services {
            assert!(latest.values().any(|workload| {
                if workload["metadata"]["namespace"] != service["metadata"]["namespace"] { return false; }
                if let Some(selector) = service["spec"]["selector"].as_object().filter(|selector| !selector.is_empty()) {
                    if pod_spec(workload).is_none() { return false; }
                    let labels = match workload["kind"].as_str().unwrap() {
                        "Pod" => &workload["metadata"]["labels"],
                        "CronJob" => &workload["spec"]["jobTemplate"]["spec"]["template"]["metadata"]["labels"],
                        _ => &workload["spec"]["template"]["metadata"]["labels"],
                    };
                    selector.iter().all(|(key, value)| &labels[key] == value)
                } else {
                    workload["kind"] == "WorkloadDeployment" && workload["spec"]["template"]["spec"]["kubernetes"]["service"]["name"] == service["metadata"]["name"]
                }
            }), "{}", service["metadata"]["name"]);
        }
        Ok(())
    }

    #[test]
    fn install_scripts_preserve_output() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let scripts = pods(render)
            .filter(|(_, object, _)| object["kind"] == "Job")
            .flat_map(|(_, _, spec)| containers(spec))
            .filter(|container| {
                container["command"][0]
                    .as_str()
                    .is_some_and(|program| program.rsplit('/').next() == Some("sh"))
            })
            .collect::<Vec<_>>();
        assert!(!scripts.is_empty());
        for container in scripts {
            let script = container["args"][0]
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect::<String>();
            // /dev/null is the shell's discard sink, independent of any rendered name.
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
            assert!(!stderr);
            assert!(contract(container)?.credential_stdout || !stdout);
        }
        Ok(())
    }

    #[test]
    fn install_connections_precede_apply() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let documents = environment_jobs(render)?;
        let mut created = BTreeMap::new();
        for (index, container, verb, document) in &documents {
            if *verb != "env connections" {
                continue;
            }
            let spec = pod_spec(&render.objects[*index].1).unwrap();
            let arguments = container["args"].as_array().unwrap();
            let directory = arguments
                .windows(2)
                .find(|pair| pair[0] == "--documents")
                .unwrap()[1]
                .as_str()
                .unwrap();
            for name in document.integrations.keys() {
                // env connections defines these two file suffixes for each integration name.
                let connection: Value = serde_json::from_str(&mounted_text(
                    render,
                    *index,
                    spec,
                    container,
                    &format!("{directory}/{name}.json"),
                )?)?;
                let integration: Value = serde_json::from_str(&mounted_text(
                    render,
                    *index,
                    spec,
                    container,
                    &format!("{directory}/{name}.integration.json"),
                )?)?;
                assert_eq!(connection["name"], *name);
                assert_eq!(integration["name"], connection["name"]);
                assert_eq!(connection["project"], document.project);
                assert_eq!(connection["env"], document.env);
                assert_eq!(integration["project"], document.project);
                created
                    .entry((&document.org, &document.project, &document.env, name))
                    .or_insert(*index);
            }
        }
        assert!(!created.is_empty());
        let mut bindings = 0;
        for (index, _, verb, document) in &documents {
            if *verb != "env apply" || document.release == DeclaredRelease::None {
                continue;
            }
            for definition in document.applications.values() {
                for name in definition.connections.values() {
                    bindings += 1;
                    assert!(
                        created
                            .get(&(&document.org, &document.project, &document.env, name))
                            .is_some_and(|producer| producer < index)
                    );
                }
            }
        }
        assert!(bindings > 0);
        Ok(())
    }

    #[test]
    fn install_documents_keep_package_owners() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let documents = environment_jobs(render)?;
        assert!(!documents.is_empty());
        for (_, _, _, document) in documents {
            let key = wamn_control_registry::project_env_key(
                &document.org,
                &document.project,
                &document.env,
            );
            let member = &render.fixtures.releases[&key]
                .composed_entry
                .as_ref()
                .unwrap()
                .application;
            let owners = document
                .applications
                .values()
                .filter(|definition| definition.packages.contains(&member.package_id))
                .collect::<Vec<_>>();
            // A composed application package must have one owner, regardless of its name.
            assert_eq!(owners.len(), 1);
        }
        Ok(())
    }
}

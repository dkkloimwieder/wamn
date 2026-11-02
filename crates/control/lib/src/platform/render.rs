//! Render the platform install batches without a cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::{Value, json};

/// One registry artifact supplied to package installation without a registry call.
#[derive(Debug)]
pub struct Package {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub files: BTreeMap<String, String>,
}

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
    pub gates_image: String,
    pub releases: BTreeMap<String, wamn_catalog::ServingManifest>,
    pub web_clients: BTreeMap<String, BTreeMap<String, String>>,
    pub applied: BTreeMap<String, Vec<Value>>,
    pub oauth_key: [u8; 32],
    pub oauth_exists: bool,
    pub registry: wamn_package_artifact::PackageRegistry,
    pub packages: BTreeMap<String, Vec<Package>>,
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
    for environment in environments {
        let key = environment_key(environment);
        let secrets = super::packages::credentials_from(
            &admin,
            fixtures
                .environment_urls
                .get(&key)
                .context("environment URL fixture")?,
        )?;
        let database = url::Url::parse(&secrets[1].value)?;
        let packages = fixtures.packages.get(&key).context("package fixtures")?;
        let tags = packages
            .iter()
            .map(|package| format!("{}-{}", package.id, package.version))
            .collect::<Vec<_>>();
        let artifacts = packages
            .iter()
            .map(|package| package.digest.clone())
            .collect::<Vec<_>>();
        for (package, tag) in packages.iter().zip(&tags) {
            let request = super::packages::package_job(
                &key,
                tag.clone(),
                environment,
                &fixtures.runner_image,
                &fixtures.registry,
                &auth,
            )?;
            let batch = super::packages::artifact_inputs(
                request,
                &package.digest,
                database.path(),
                &secrets,
            );
            append(
                &mut objects,
                "4.13",
                job_objects(&batch, "platform", fixtures)?,
            );
        }
        if !packages.is_empty() {
            let request = super::packages::access_job(
                &key,
                environment,
                &fixtures.runner_image,
                &fixtures.registry,
                &tags,
            )?;
            let batch =
                super::packages::artifacts_inputs(request, &artifacts, database.path(), &secrets);
            append(
                &mut objects,
                "4.13",
                job_objects(&batch, "platform", fixtures)?,
            );
        }
        for package in packages {
            let request = super::packages::replica_job(
                &key,
                &package.id,
                &fixtures.runner_image,
                &package.files,
            );
            let batch = super::packages::artifact_inputs(
                request,
                &package.digest,
                database.path(),
                &secrets,
            );
            append(
                &mut objects,
                "4.13",
                job_objects(&batch, "platform", fixtures)?,
            );
        }
    }
    for environment in environments {
        let key = environment_key(environment);
        let packages = fixtures.packages.get(&key).context("package fixtures")?;
        let coordinates = packages
            .iter()
            .map(|package| (package.id.clone(), package.version.clone()))
            .collect::<Vec<_>>();
        let artifacts = packages
            .iter()
            .map(|package| package.digest.clone())
            .collect::<Vec<_>>();
        let request = super::releases::publish_request(
            &key,
            &environment.to_kcl(),
            &fixtures.runner_image,
            &fixtures.registry,
            &coordinates,
        )?;
        let secrets = super::packages::credentials_from(
            &admin,
            fixtures
                .environment_urls
                .get(&key)
                .context("environment URL fixture")?,
        )?;
        let database = url::Url::parse(&secrets[1].value)?;
        let batch =
            super::packages::artifacts_inputs(request, &artifacts, database.path(), &secrets);
        append(
            &mut objects,
            "4.14",
            job_objects(&batch, "platform", fixtures)?,
        );
    }
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
                vec![super::cdc::deployment(environment, &fixtures.gates_image)],
            );
        }
    }
    if !document.buckets.web.is_empty() {
        for environment in environments {
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
            crate::release_chart::release_name(
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
    #[test]
    fn kind_install_renders_in_install_order() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let objects = &render.objects;
        assert_eq!(render.documents.len(), 2);
        assert_eq!(render.fixtures.releases.len(), 2);
        assert_eq!(render.owner_inputs.len(), 1);
        // Owner and local-build steps apply no Kubernetes objects.
        for step in [
            "4.3", "4.4", "4.4.1", "4.5", "4.7", "4.8", "4.9", "4.11", "4.12", "4.13", "4.14",
            "4.16.1", "4.17", "4.18", "4.19", "4.19.1", "4.20", "4.21",
        ] {
            assert!(
                objects.iter().any(|(number, _)| *number == step),
                "empty step {step}"
            );
        }
        for kind in [
            "Deployment",
            "Service",
            "Certificate",
            "ServiceAccount",
            "Role",
            "RoleBinding",
            "Secret",
            "ConfigMap",
            "Job",
        ] {
            assert!(
                objects.iter().any(|(_, object)| object["kind"] == kind),
                "missing kind {kind}"
            );
        }
        let steps = super::super::steps::install_steps(&render.cluster, "render");
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
}

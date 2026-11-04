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
            "4.16.1", "4.17", "4.18", "4.19.1", "4.20", "4.21",
        ] {
            assert!(
                objects.iter().any(|(number, _)| *number == step),
                "empty step {step}"
            );
        }
        assert_eq!(
            objects.iter().any(|(step, _)| *step == "4.19"),
            render
                .documents
                .iter()
                .any(|(_, environment)| environment.client.is_some()),
            "web upload Jobs require a client pin"
        );
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

    fn object<'a>(
        render: &'a super::fixture::Render,
        kind: &str,
        namespace: &str,
        name: &str,
    ) -> (usize, &'a serde_json::Value) {
        render
            .objects
            .iter()
            .enumerate()
            .find(|(_, (_, value))| {
                value["kind"] == kind
                    && value["metadata"]["namespace"] == namespace
                    && value["metadata"]["name"] == name
                    && value["spec"]["suspend"] != true
            })
            .map(|(index, (_, value))| (index, value))
            .unwrap()
    }

    #[test]
    fn install_service_accounts_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (kind, namespace, name, account) in [
            ("Deployment", "platform", "otel-collector", "otel-collector"),
            ("Deployment", "cert-manager", "cert-manager", "cert-manager"),
            (
                "Deployment",
                "cert-manager",
                "cert-manager-cainjector",
                "cert-manager-cainjector",
            ),
            (
                "Deployment",
                "cert-manager",
                "cert-manager-webhook",
                "cert-manager-webhook",
            ),
            (
                "Deployment",
                "cnpg-system",
                "cnpg-controller-manager",
                "cnpg-manager",
            ),
            (
                "Deployment",
                "platform",
                "runtime-operator",
                "wamn-runtime-operator",
            ),
            (
                "Deployment",
                "hosts",
                "cdc-reader-test--wms--dev",
                "event-reader",
            ),
            (
                "Deployment",
                "hosts",
                "cdc-reader-test--receiving--dev",
                "event-reader",
            ),
            (
                "Deployment",
                "hosts",
                "hostgroup-c-test",
                "wamn-control-test",
            ),
            (
                "Deployment",
                "hosts",
                "hostgroup-r-dev-e2fc889f978641f76d92",
                "wamn-host-test--wms--dev",
            ),
            (
                "Deployment",
                "hosts",
                "hostgroup-r-dev-092a942d31ab372be553",
                "wamn-host-test--receiving--dev",
            ),
            ("Job", "hosts", "platform-ui-test", "wamn-host"),
            (
                "Job",
                "platform",
                "platform-first-apply-test--wms--dev",
                "platform-first-apply-test--wms--dev",
            ),
            (
                "Job",
                "platform",
                "platform-first-apply-test--receiving--dev",
                "platform-first-apply-test--receiving--dev",
            ),
            (
                "Job",
                "platform",
                "platform-apply-test--wms--dev",
                "platform-apply-test--wms--dev",
            ),
            (
                "Job",
                "platform",
                "platform-apply-test--receiving--dev",
                "platform-apply-test--receiving--dev",
            ),
            (
                "Job",
                "hosts",
                "platform-connections-test--wms--dev",
                "platform-connections-test--wms--dev",
            ),
            (
                "Job",
                "platform",
                "platform-control-credentials-test",
                "platform-control-credentials-test",
            ),
            (
                "Job",
                "platform",
                "platform-control-operator-test",
                "platform-control-operator-test",
            ),
            (
                "Job",
                "platform",
                "platform-invite-test",
                "platform-invite-test",
            ),
        ] {
            let (consumer_index, consumer) = object(render, kind, namespace, name);
            let (account_index, account) = object(render, "ServiceAccount", namespace, account);
            assert_eq!(
                consumer["spec"]["template"]["spec"]["serviceAccountName"],
                account["metadata"]["name"]
            );
            assert_eq!(
                consumer["metadata"]["namespace"],
                account["metadata"]["namespace"]
            );
            assert!(account_index < consumer_index, "{name}");
        }
        Ok(())
    }

    #[test]
    fn install_job_secrets_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (job_name, namespace, variable, secret_name, key) in [
            (
                "platform-system",
                "platform",
                "WAMN_SYSTEM_ADMIN_URL",
                "wamn-system-admin",
                "url",
            ),
            (
                "platform-org",
                "platform",
                "WAMN_SYSTEM_ADMIN_URL",
                "wamn-system-admin",
                "url",
            ),
            (
                "platform-identity-issuer",
                "platform",
                "WAMN_SYSTEM_ADMIN_URL",
                "wamn-system-admin",
                "url",
            ),
            (
                "platform-session-key-state",
                "platform",
                "PGPASSWORD",
                "platform-key-probe",
                "password",
            ),
            (
                "platform-session-key-publish",
                "identity",
                "WAMN_IDENTITY_DATABASE_URL",
                "platform-key-identity",
                "url",
            ),
            (
                "platform-session-key-activate",
                "identity",
                "WAMN_IDENTITY_DATABASE_URL",
                "platform-key-identity",
                "url",
            ),
            (
                "platform-connections-test--wms--dev",
                "hosts",
                "WAMN_CONTROL_DATABASE_URL",
                "wamn-system-admin",
                "url",
            ),
        ] {
            let (job_index, job) = object(render, "Job", namespace, job_name);
            let (secret_index, secret) = object(render, "Secret", namespace, secret_name);
            let entry = job["spec"]["template"]["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == variable)
                .unwrap();
            assert_eq!(
                entry["valueFrom"]["secretKeyRef"]["name"],
                secret["metadata"]["name"]
            );
            assert_eq!(entry["valueFrom"]["secretKeyRef"]["key"], key);
            assert!(secret["stringData"][key].is_string());
            assert_eq!(
                job["metadata"]["namespace"],
                secret["metadata"]["namespace"]
            );
            assert!(secret_index < job_index, "{job_name}");
        }
        for project in ["wms", "receiving"] {
            for (prefix, variable, secret_name) in [
                ("first-apply", "WAMN_SYSTEM_ADMIN_URL", "wamn-system-admin"),
                ("first-apply", "WAMN_APPLY_DATABASE_URL", "wamn-env-apply"),
                ("apply", "WAMN_SYSTEM_ADMIN_URL", "wamn-system-admin"),
                ("apply", "WAMN_APPLY_DATABASE_URL", "wamn-env-apply"),
                ("session", "WAMN_SYSTEM_ADMIN_URL", "wamn-system-admin"),
                (
                    "session",
                    "WAMN_TARGET_ADMIN_DATABASE_URL",
                    "platform-session-admin",
                ),
            ] {
                let namespace = if prefix == "session" {
                    "identity"
                } else {
                    "platform"
                };
                let name = format!("platform-{prefix}-test--{project}--dev");
                let (job_index, job) = object(render, "Job", namespace, &name);
                let (secret_index, secret) = object(render, "Secret", namespace, secret_name);
                let entry = job["spec"]["template"]["spec"]["containers"][0]["env"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["name"] == variable)
                    .unwrap();
                assert_eq!(
                    entry["valueFrom"]["secretKeyRef"]["name"],
                    secret["metadata"]["name"]
                );
                assert_eq!(entry["valueFrom"]["secretKeyRef"]["key"], "url");
                assert!(secret["stringData"]["url"].is_string());
                assert!(secret_index < job_index, "{name}");
            }
        }
        Ok(())
    }

    #[test]
    fn install_workload_inputs_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (deployment, volume_name, namespace, certificate_name) in [
            ("identity", "serving-tls", "identity", "identity-tls"),
            (
                "identity",
                "operator-ca",
                "identity",
                "identity-operator-ca",
            ),
            ("identity", "auth-nats", "identity", "wamn-identity-nats"),
            ("wamn-edge", "serving-tls", "edge", "wamn-edge-tls"),
            (
                "runtime-operator",
                "operator-cert",
                "platform",
                "wasmcloud-operator-tls",
            ),
        ] {
            let (consumer_index, consumer) = object(render, "Deployment", namespace, deployment);
            let (certificate_index, certificate) =
                object(render, "Certificate", namespace, certificate_name);
            let volume = consumer["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|volume| volume["name"] == volume_name)
                .unwrap();
            assert_eq!(
                volume["secret"]["secretName"],
                certificate["spec"]["secretName"]
            );
            assert_eq!(
                consumer["metadata"]["namespace"],
                certificate["metadata"]["namespace"]
            );
            assert!(certificate_index < consumer_index);
        }
        let (identity_index, identity) = object(render, "Deployment", "identity", "identity");
        for (volume_name, secret_name) in [
            (
                "session-target-0",
                "wamn-session-role-reader-test--wms--dev",
            ),
            (
                "session-target-1",
                "wamn-session-role-reader-test--receiving--dev",
            ),
        ] {
            let (secret_index, secret) = object(render, "Secret", "identity", secret_name);
            let volume = identity["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|volume| volume["name"] == volume_name)
                .unwrap();
            assert_eq!(volume["secret"]["secretName"], secret["metadata"]["name"]);
            assert_eq!(volume["secret"]["items"][0]["key"], "target.json");
            assert!(secret["stringData"]["target.json"].is_string());
            assert!(secret_index < identity_index);
        }
        let (database_index, database) = object(render, "Secret", "identity", "wamn-identity-db");
        let variables = identity["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        let database_url = variables
            .iter()
            .find(|entry| entry["name"] == "WAMN_IDENTITY_DATABASE_URL")
            .unwrap();
        assert_eq!(
            database_url["valueFrom"]["secretKeyRef"]["name"],
            database["metadata"]["name"]
        );
        assert_eq!(database_url["valueFrom"]["secretKeyRef"]["key"], "url");
        assert!(database["stringData"]["url"].is_string());
        assert!(database_index < identity_index);
        let resend = variables
            .iter()
            .find(|entry| entry["name"] == "RESEND_API_KEY")
            .unwrap();
        assert_eq!(
            resend["valueFrom"]["secretKeyRef"]["name"],
            render.owner_inputs[0]["metadata"]["name"]
        );
        assert_eq!(
            identity["metadata"]["namespace"],
            render.owner_inputs[0]["metadata"]["namespace"]
        );
        assert_eq!(resend["valueFrom"]["secretKeyRef"]["key"], "api-key");
        assert!(render.owner_inputs[0]["stringData"]["api-key"].is_string());
        for project in ["wms", "receiving"] {
            let (capture_index, capture) = object(
                render,
                "Deployment",
                "hosts",
                &format!("cdc-reader-test--{project}--dev"),
            );
            for (index, family) in [(0, "cdc"), (1, "registry-reader")] {
                let (secret_index, secret) = object(
                    render,
                    "Secret",
                    "hosts",
                    &format!("wamn-{family}-test--{project}--dev"),
                );
                let reference = &capture["spec"]["template"]["spec"]["containers"][0]["env"][index]
                    ["valueFrom"]["secretKeyRef"];
                assert_eq!(reference["name"], secret["metadata"]["name"]);
                assert_eq!(reference["key"], "url");
                assert!(secret["stringData"]["url"].is_string());
                assert!(secret_index < capture_index);
            }
            let (certificate_index, certificate) =
                object(render, "Certificate", "hosts", "wamn-cdc-reader-nats");
            assert_eq!(
                capture["spec"]["template"]["spec"]["volumes"][0]["secret"]["secretName"],
                certificate["spec"]["secretName"]
            );
            assert!(certificate_index < capture_index);
        }
        Ok(())
    }

    #[test]
    fn install_configmap_inputs_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (kind, namespace, consumer_name, volume_name, producer_name) in [
            (
                "StatefulSet",
                "platform",
                "wamn-nats",
                "config",
                "wamn-nats-config",
            ),
            (
                "Deployment",
                "platform",
                "otel-collector",
                "config",
                "otel-collector",
            ),
            ("Deployment", "edge", "wamn-edge", "config", "wamn-edge"),
            (
                "Deployment",
                "edge",
                "wamn-edge",
                "identity-ca",
                "identity-ca",
            ),
            (
                "Deployment",
                "hosts",
                "hostgroup-c-test",
                "registry-pull",
                "wamn-registry-helper",
            ),
            (
                "Deployment",
                "hosts",
                "hostgroup-c-test",
                "identity-ca",
                "identity-ca",
            ),
        ] {
            let (consumer_index, consumer) = object(render, kind, namespace, consumer_name);
            let (producer_index, producer) = object(render, "ConfigMap", namespace, producer_name);
            let volume = consumer["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|volume| volume["name"] == volume_name)
                .unwrap();
            assert_eq!(volume["configMap"]["name"], producer["metadata"]["name"]);
            assert_eq!(
                consumer["metadata"]["namespace"],
                producer["metadata"]["namespace"]
            );
            assert!(
                producer_index < consumer_index,
                "{consumer_name}/{volume_name}"
            );
        }
        let (collector_index, collector) =
            object(render, "Deployment", "platform", "otel-collector");
        let (values_index, values) =
            object(render, "ConfigMap", "platform", "otel-collector-values");
        assert_eq!(
            collector["spec"]["template"]["spec"]["containers"][0]["envFrom"][0]["configMapRef"]["name"],
            values["metadata"]["name"]
        );
        assert!(values_index < collector_index);
        let (_, edge) = object(render, "Deployment", "edge", "wamn-edge");
        let (_, ca) = object(render, "ConfigMap", "edge", "identity-ca");
        assert_eq!(
            edge["spec"]["template"]["spec"]["volumes"][2]["configMap"]["items"][0]["key"],
            "ca.crt"
        );
        assert!(ca["data"]["ca.crt"].is_string());
        for project in ["wms", "receiving"] {
            let release = crate::release_chart::release_name("test", project, "dev")?;
            let (host_index, host) = object(
                render,
                "Deployment",
                "hosts",
                &format!("hostgroup-{release}"),
            );
            for (volume_name, producer_name) in [
                ("registry-pull", "wamn-registry-helper"),
                ("identity-ca", "identity-ca"),
            ] {
                let (producer_index, producer) =
                    object(render, "ConfigMap", "hosts", producer_name);
                let volume = host["spec"]["template"]["spec"]["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|volume| volume["name"] == volume_name)
                    .unwrap();
                assert_eq!(volume["configMap"]["name"], producer["metadata"]["name"]);
                assert!(producer_index < host_index);
            }
        }
        Ok(())
    }

    #[test]
    fn install_apply_roles_cover_commands() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for project in ["wms", "receiving"] {
            for prefix in ["first-apply", "apply"] {
                let name = format!("platform-{prefix}-test--{project}--dev");
                let (job_index, job) = object(render, "Job", "platform", &name);
                let spec = &job["spec"]["template"]["spec"];
                assert_eq!(spec["automountServiceAccountToken"], true);
                assert_eq!(spec["containers"][0]["args"][0], "env");
                assert_eq!(spec["containers"][0]["args"][1], "apply");
                for namespace in ["platform", "hosts"] {
                    let (role_index, role) = object(render, "Role", namespace, &name);
                    let (binding_index, binding) = object(render, "RoleBinding", namespace, &name);
                    assert_eq!(binding["roleRef"]["kind"], role["kind"]);
                    assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
                    assert_eq!(binding["subjects"][0]["name"], spec["serviceAccountName"]);
                    assert_eq!(
                        binding["subjects"][0]["namespace"],
                        job["metadata"]["namespace"]
                    );
                    assert_eq!(
                        binding["metadata"]["namespace"],
                        role["metadata"]["namespace"]
                    );
                    if namespace == "platform" {
                        assert_eq!(
                            role["rules"],
                            serde_json::json!([{
                                "apiGroups":["postgresql.cnpg.io"], "resources":["clusters","databases","publications"],
                                "verbs":["get","list","watch","create","patch"]
                            }])
                        );
                    } else {
                        assert_eq!(
                            role["rules"],
                            serde_json::json!([
                                {"apiGroups":[""],"resources":["secrets","services","configmaps"],"verbs":["get","list","watch","create","patch","update","delete"]},
                                {"apiGroups":[""],"resources":["pods"],"verbs":["get","list","watch"]},
                                {"apiGroups":[""],"resources":["pods/log"],"verbs":["get"]},
                                {"apiGroups":[""],"resources":["events"],"verbs":["list"]},
                                {"apiGroups":[""],"resources":["serviceaccounts"],"verbs":["get","create"]},
                                {"apiGroups":["rbac.authorization.k8s.io"],"resources":["roles","rolebindings"],"verbs":["get","create"]},
                                {"apiGroups":["apps"],"resources":["deployments","replicasets"],"verbs":["get","list","watch","create","patch","update","delete"]},
                                {"apiGroups":["networking.k8s.io"],"resources":["networkpolicies"],"verbs":["get","list","watch","create","patch","update","delete"]},
                                {"apiGroups":["runtime.wasmcloud.dev"],"resources":["workloaddeployments"],"verbs":["get","list","watch","create","patch","update","delete"]}
                            ])
                        );
                    }
                    assert!(role_index < job_index && binding_index < job_index);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn install_other_roles_cover_commands() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (job_namespace, name, role_namespace, verbs) in [
            (
                "hosts",
                "platform-connections-test--wms--dev",
                "hosts",
                serde_json::json!(["get", "create", "update"]),
            ),
            (
                "platform",
                "platform-control-credentials-test",
                "hosts",
                serde_json::json!(["get", "create", "patch"]),
            ),
            (
                "platform",
                "platform-control-operator-test",
                "identity",
                serde_json::json!(["get"]),
            ),
            (
                "platform",
                "platform-control-operator-test",
                "hosts",
                serde_json::json!(["get", "create", "patch"]),
            ),
            (
                "platform",
                "platform-invite-test",
                "identity",
                serde_json::json!(["get"]),
            ),
        ] {
            let (job_index, job) = object(render, "Job", job_namespace, name);
            let (role_index, role) = object(render, "Role", role_namespace, name);
            let (binding_index, binding) = object(render, "RoleBinding", role_namespace, name);
            assert_eq!(
                job["spec"]["template"]["spec"]["automountServiceAccountToken"],
                true
            );
            assert_eq!(binding["roleRef"]["kind"], role["kind"]);
            assert_eq!(binding["roleRef"]["name"], role["metadata"]["name"]);
            assert_eq!(binding["subjects"][0]["kind"], "ServiceAccount");
            assert_eq!(
                binding["subjects"][0]["name"],
                job["spec"]["template"]["spec"]["serviceAccountName"]
            );
            assert_eq!(
                binding["subjects"][0]["namespace"],
                job["metadata"]["namespace"]
            );
            assert_eq!(
                binding["metadata"]["namespace"],
                role["metadata"]["namespace"]
            );
            assert_eq!(
                role["rules"],
                serde_json::json!([{"apiGroups":[""],"resources":["secrets"],"verbs":verbs}])
            );
            assert!(role_index < job_index && binding_index < job_index);
        }
        Ok(())
    }

    #[test]
    fn install_apply_command_inputs() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for project in ["wms", "receiving"] {
            for (prefix, namespace) in [
                ("first-apply", "platform"),
                ("apply", "platform"),
                ("connections", "hosts"),
            ] {
                if prefix == "connections" && project == "receiving" {
                    continue;
                }
                let name = format!("platform-{prefix}-test--{project}--dev");
                let (job_index, job) = object(render, "Job", namespace, &name);
                let spec = &job["spec"]["template"]["spec"];
                let container = &spec["containers"][0];
                let (files_index, files) =
                    object(render, "Secret", namespace, &format!("{name}-files"));
                let volume = &spec["volumes"][0];
                assert_eq!(volume["secret"]["secretName"], files["metadata"]["name"]);
                assert_eq!(container["volumeMounts"][0]["name"], volume["name"]);
                assert_eq!(container["volumeMounts"][0]["mountPath"], "/inputs");
                assert_eq!(container["args"][2], "/inputs/environment.k");
                assert!(files_index < job_index);
                for filename in ["environment.k", "kubeconfig", "registry.json"] {
                    let item = volume["secret"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["path"] == filename)
                        .unwrap();
                    assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
                }
                if prefix != "connections" {
                    for filename in ["Chart.yaml", "values.yaml"] {
                        let item = volume["secret"]["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|item| item["path"] == filename)
                            .unwrap();
                        assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
                    }
                }
                for (variable, value) in [
                    ("WAMN_RELEASE_CHART", "/inputs"),
                    ("WAMN_REGISTRY_AUTH_FILE", "/inputs/registry.json"),
                    (
                        "WAMN_RELEASE_ARTIFACT_BASE",
                        "registry.invalid/wamn/releases",
                    ),
                    (
                        "WAMN_NATS_URL",
                        "nats://wamn-nats.platform.svc.cluster.local:4222",
                    ),
                ] {
                    let entry = container["env"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["name"] == variable)
                        .unwrap();
                    assert_eq!(entry["value"], value);
                }
                for (variable, filename) in [
                    ("WAMN_NATS_TLS_CA", "ca.crt"),
                    ("WAMN_NATS_TLS_CERT", "tls.crt"),
                    ("WAMN_NATS_TLS_KEY", "tls.key"),
                ] {
                    let entry = container["env"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["name"] == variable)
                        .unwrap();
                    let item = volume["secret"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["path"] == filename)
                        .unwrap();
                    assert_eq!(
                        entry["value"],
                        format!("/inputs/{}", item["path"].as_str().unwrap())
                    );
                    assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn install_package_command_inputs() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for project in ["wms", "receiving"] {
            let key = format!("test--{project}--dev");
            let package = format!("wamn_{project}");
            let tag = format!("{package}-1.0.0");
            let package_job = format!(
                "platform-package-{}",
                &super::super::job::input_digest(&serde_json::json!([key, tag]))[..24]
            );
            let replica_job = format!(
                "platform-replica-{}",
                &super::super::job::input_digest(&serde_json::json!([key, package]))[..24]
            );
            for (name, filenames) in [
                (package_job, &["registry.json"][..]),
                (replica_job, &["package/wamn.json"][..]),
                (
                    format!("platform-package-access-{key}"),
                    &["registry.json"][..],
                ),
                (
                    format!("platform-release-{key}"),
                    &["environment.k", "registry.json"][..],
                ),
            ] {
                let (job_index, job) = object(render, "Job", "platform", &name);
                let spec = &job["spec"]["template"]["spec"];
                let container = &spec["containers"][0];
                let (files_index, files) =
                    object(render, "Secret", "platform", &format!("{name}-files"));
                assert_eq!(
                    spec["volumes"][0]["secret"]["secretName"],
                    files["metadata"]["name"]
                );
                assert_eq!(
                    container["volumeMounts"][0]["name"],
                    spec["volumes"][0]["name"]
                );
                assert_eq!(container["volumeMounts"][0]["mountPath"], "/inputs");
                assert!(files_index < job_index);
                for filename in filenames {
                    let item = spec["volumes"][0]["secret"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["path"] == *filename)
                        .unwrap();
                    assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
                }
                if filenames.contains(&"registry.json") {
                    let entry = container["env"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["name"] == "WAMN_REGISTRY_AUTH_FILE")
                        .unwrap();
                    assert_eq!(entry["value"], "/inputs/registry.json");
                }
                if container["args"][0] == "apply-package" {
                    for (variable, filename) in [
                        ("WAMN_NATS_TLS_CA", "ca.crt"),
                        ("WAMN_NATS_TLS_CERT", "tls.crt"),
                        ("WAMN_NATS_TLS_KEY", "tls.key"),
                    ] {
                        let entry = container["env"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|entry| entry["name"] == variable)
                            .unwrap();
                        let item = spec["volumes"][0]["secret"]["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|item| item["path"] == filename)
                            .unwrap();
                        assert_eq!(
                            entry["value"],
                            format!("/inputs/{}", item["path"].as_str().unwrap())
                        );
                        assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
                    }
                }
                for (variable, secret_name) in [
                    ("WAMN_PG_ADMIN_URL", "platform-package-admin"),
                    ("WAMN_SYSTEM_ADMIN_URL", "wamn-system-admin"),
                ] {
                    let entry = container["env"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["name"] == variable)
                        .unwrap();
                    let (secret_index, secret) = object(render, "Secret", "platform", secret_name);
                    assert_eq!(
                        entry["valueFrom"]["secretKeyRef"]["name"],
                        secret["metadata"]["name"]
                    );
                    assert_eq!(entry["valueFrom"]["secretKeyRef"]["key"], "url");
                    assert!(secret["stringData"]["url"].is_string());
                    assert!(secret_index < job_index);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn install_script_command_inputs() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (name, filename) in [
            ("platform-control-credentials-test", "kubeconfig"),
            ("platform-control-operator-test", "metadata.json"),
            ("platform-invite-test", "kubeconfig"),
            ("platform-owner-state-test", "state.sql"),
        ] {
            let (job_index, job) = object(render, "Job", "platform", name);
            let spec = &job["spec"]["template"]["spec"];
            let (files_index, files) =
                object(render, "Secret", "platform", &format!("{name}-files"));
            assert_eq!(
                spec["volumes"][0]["secret"]["secretName"],
                files["metadata"]["name"]
            );
            assert_eq!(
                spec["containers"][0]["volumeMounts"][0]["name"],
                spec["volumes"][0]["name"]
            );
            assert_eq!(
                spec["containers"][0]["volumeMounts"][0]["mountPath"],
                "/inputs"
            );
            let item = spec["volumes"][0]["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["path"] == filename)
                .unwrap();
            assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
            assert!(files_index < job_index);
        }
        let (cluster_index, cluster) = object(render, "Cluster", "platform", "wamn-pg");
        assert_eq!(cluster["spec"]["enableSuperuserAccess"], true);
        for (name, key) in [
            ("platform-control-credentials-test", "uri"),
            ("platform-invite-test", "uri"),
            ("platform-owner-state-test", "password"),
        ] {
            let (job_index, job) = object(render, "Job", "platform", name);
            let spec = &job["spec"]["template"]["spec"];
            assert_eq!(
                spec["volumes"][1]["secret"]["secretName"],
                format!(
                    "{}-superuser",
                    cluster["metadata"]["name"].as_str().unwrap()
                )
            );
            assert_eq!(spec["volumes"][1]["secret"]["items"][0]["key"], key);
            assert_eq!(spec["volumes"][1]["secret"]["items"][0]["path"], key);
            assert_eq!(
                spec["containers"][0]["volumeMounts"][1]["name"],
                spec["volumes"][1]["name"]
            );
            assert_eq!(
                spec["containers"][0]["volumeMounts"][1]["mountPath"],
                "/etc/wamn/postgres"
            );
            assert!(cluster_index < job_index);
        }
        let (_, operator_job) = object(render, "Job", "platform", "platform-control-operator-test");
        let (operator_index, operator) = object(render, "Certificate", "identity", "operator-test");
        let (_, invite) = object(render, "Job", "platform", "platform-invite-test");
        let (tls_index, _) = object(render, "Certificate", "identity", "identity-tls");
        let operator_env = operator_job["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap();
        assert_eq!(operator_env[0]["name"], "OPERATOR");
        assert_eq!(operator_env[0]["value"], operator["spec"]["secretName"]);
        let (invite_index, _) = object(render, "Job", "platform", "platform-invite-test");
        assert!(operator_index < invite_index && tls_index < invite_index);
        let (_, invite_files) = object(render, "Secret", "platform", "platform-invite-test-files");
        let invite_spec = &invite["spec"]["template"]["spec"];
        for (variable, filename) in [
            ("WAMN_NATS_TLS_CA", "ca.crt"),
            ("WAMN_NATS_TLS_CERT", "tls.crt"),
            ("WAMN_NATS_TLS_KEY", "tls.key"),
        ] {
            let entry = invite_spec["containers"][0]["env"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == variable)
                .unwrap();
            let item = invite_spec["volumes"][0]["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["path"] == filename)
                .unwrap();
            assert_eq!(
                entry["value"],
                format!("/inputs/{}", item["path"].as_str().unwrap())
            );
            assert!(invite_files["stringData"][item["key"].as_str().unwrap()].is_string());
        }
        for (variable, value) in [
            ("ORG", render.cluster.org.as_str()),
            ("OWNER_EMAIL", render.cluster.owner_email.as_str()),
            (
                "WAMN_PAT_ISSUER",
                "https://identity.identity.svc.cluster.local",
            ),
        ] {
            let entry = invite["spec"]["template"]["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == variable)
                .unwrap();
            assert_eq!(entry["value"], value);
        }
        Ok(())
    }

    #[test]
    fn install_service_targets_match() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (namespace, service_name, deployment_name, labels) in [
            (
                "platform",
                "otel-collector",
                "otel-collector",
                &["app.kubernetes.io/name"][..],
            ),
            (
                "platform",
                "runtime-operator",
                "runtime-operator",
                &["wasmcloud.com/name"][..],
            ),
            (
                "cnpg-system",
                "cnpg-webhook-service",
                "cnpg-controller-manager",
                &["app.kubernetes.io/name"][..],
            ),
            (
                "cert-manager",
                "cert-manager",
                "cert-manager",
                &[
                    "app.kubernetes.io/name",
                    "app.kubernetes.io/instance",
                    "app.kubernetes.io/component",
                ][..],
            ),
            (
                "cert-manager",
                "cert-manager-cainjector",
                "cert-manager-cainjector",
                &[
                    "app.kubernetes.io/name",
                    "app.kubernetes.io/instance",
                    "app.kubernetes.io/component",
                ][..],
            ),
            (
                "cert-manager",
                "cert-manager-webhook",
                "cert-manager-webhook",
                &[
                    "app.kubernetes.io/name",
                    "app.kubernetes.io/instance",
                    "app.kubernetes.io/component",
                ][..],
            ),
            (
                "identity",
                "identity",
                "identity",
                &["app.kubernetes.io/name", "app.kubernetes.io/instance"][..],
            ),
            (
                "edge",
                "wamn-edge",
                "wamn-edge",
                &["app.kubernetes.io/name", "app.kubernetes.io/instance"][..],
            ),
        ] {
            let (_, service) = object(render, "Service", namespace, service_name);
            let (_, deployment) = object(render, "Deployment", namespace, deployment_name);
            assert_eq!(
                service["metadata"]["namespace"],
                deployment["metadata"]["namespace"]
            );
            for label in labels {
                assert_eq!(
                    service["spec"]["selector"][label],
                    deployment["spec"]["template"]["metadata"]["labels"][label]
                );
            }
        }
        let (_, nats) = object(render, "StatefulSet", "platform", "wamn-nats");
        for name in ["wamn-nats", "wamn-nats-headless"] {
            let (_, service) = object(render, "Service", "platform", name);
            assert_eq!(
                service["spec"]["selector"]["app.kubernetes.io/name"],
                nats["spec"]["template"]["metadata"]["labels"]["app.kubernetes.io/name"]
            );
        }
        let releases = [
            crate::release_chart::release_name("test", "wms", "dev")?,
            crate::release_chart::release_name("test", "receiving", "dev")?,
            "c-test".into(),
        ];
        for release in releases {
            let (_, service) = object(render, "Service", "hosts", &format!("{release}-http"));
            let (_, workload) = object(
                render,
                "WorkloadDeployment",
                "hosts",
                &format!("{release}-http"),
            );
            assert_eq!(
                workload["spec"]["template"]["spec"]["kubernetes"]["service"]["name"],
                service["metadata"]["name"]
            );
            assert_eq!(
                workload["metadata"]["namespace"],
                service["metadata"]["namespace"]
            );
            if release == "c-test" {
                continue;
            }
            let (_, files) = object(render, "Service", "hosts", &format!("{release}-files"));
            let (_, host) = object(
                render,
                "Deployment",
                "hosts",
                &format!("hostgroup-{release}"),
            );
            for label in ["wasmcloud.com/hostgroup", "wamn.release"] {
                assert_eq!(
                    files["spec"]["selector"][label],
                    host["spec"]["template"]["metadata"]["labels"][label]
                );
            }
        }
        Ok(())
    }

    #[test]
    fn install_scripts_preserve_output() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (namespace, name) in [
            ("platform", "platform-system"),
            ("platform", "platform-identity-issuer"),
            ("identity", "platform-session-test--wms--dev"),
            ("identity", "platform-session-test--receiving--dev"),
            ("platform", "platform-control-credentials-test"),
            ("platform", "platform-control-operator-test"),
            ("platform", "platform-owner-state-test"),
            ("platform", "platform-invite-test"),
            ("hosts", "platform-ui-test"),
        ] {
            let (_, job) = object(render, "Job", namespace, name);
            let container = &job["spec"]["template"]["spec"]["containers"][0];
            assert_eq!(container["command"], serde_json::json!(["/bin/sh", "-ec"]));
            let script = container["args"][0].as_str().unwrap();
            assert!(!script.contains("/dev/null"), "{name}");
            assert!(
                !script.contains("1>&-") && !script.contains("2>&-"),
                "{name}"
            );
        }
        Ok(())
    }

    #[test]
    fn install_connections_precede_apply() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        let environment = &render
            .documents
            .iter()
            .find(|(_, document)| document.project == "wms")
            .unwrap()
            .1;
        let (connection_index, connections) = object(
            render,
            "Job",
            "hosts",
            "platform-connections-test--wms--dev",
        );
        let (apply_index, apply) =
            object(render, "Job", "platform", "platform-apply-test--wms--dev");
        assert!(connection_index < apply_index);
        let (_, files) = object(
            render,
            "Secret",
            "hosts",
            "platform-connections-test--wms--dev-files",
        );
        let items = connections["spec"]["template"]["spec"]["volumes"][0]["secret"]["items"]
            .as_array()
            .unwrap();
        let document_item = items
            .iter()
            .find(|item| item["path"] == "environment.k")
            .unwrap();
        assert_eq!(
            files["stringData"][document_item["key"].as_str().unwrap()]
                .as_str()
                .unwrap()
                .as_bytes(),
            environment.to_kcl().as_bytes()
        );
        let connection_item = items
            .iter()
            .find(|item| item["path"] == "wms-labels.json")
            .unwrap();
        let integration_item = items
            .iter()
            .find(|item| item["path"] == "wms-labels.integration.json")
            .unwrap();
        let connection: serde_json::Value = serde_json::from_str(
            files["stringData"][connection_item["key"].as_str().unwrap()]
                .as_str()
                .unwrap(),
        )?;
        let integration: serde_json::Value = serde_json::from_str(
            files["stringData"][integration_item["key"].as_str().unwrap()]
                .as_str()
                .unwrap(),
        )?;
        assert_eq!(
            connection["name"],
            environment.applications["wamn_wms"].connections["labels"]
        );
        assert_eq!(integration["name"], connection["name"]);
        assert_eq!(connection["project"], environment.project);
        assert_eq!(connection["env"], environment.env);
        assert_eq!(integration["project"], environment.project);
        assert!(
            environment
                .integrations
                .contains_key(connection["name"].as_str().unwrap())
        );
        assert_eq!(
            connections["spec"]["template"]["spec"]["containers"][0]["args"][2],
            apply["spec"]["template"]["spec"]["containers"][0]["args"][2]
        );
        Ok(())
    }

    #[test]
    fn install_documents_keep_package_owners() -> anyhow::Result<()> {
        let render = super::fixture::render()?;

        for (_, environment) in &render.documents {
            let key = format!("test--{}--dev", environment.project);
            let member = &render.fixtures.releases[&key]
                .composed_entry
                .as_ref()
                .unwrap()
                .application;
            let application = format!("wamn_{}", environment.project);
            assert_eq!(environment.applications.len(), 1);
            assert_eq!(
                environment.applications[&application]
                    .packages
                    .iter()
                    .collect::<Vec<_>>(),
                [&member.package_id]
            );
            for (prefix, namespace) in [
                ("first-apply", "platform"),
                ("apply", "platform"),
                ("connections", "hosts"),
            ] {
                if prefix == "connections" && environment.project == "receiving" {
                    continue;
                }
                let name = format!("platform-{prefix}-{key}");
                let (_, job) = object(render, "Job", namespace, &name);
                let (_, files) = object(render, "Secret", namespace, &format!("{name}-files"));
                let volume = &job["spec"]["template"]["spec"]["volumes"][0];
                let item = volume["secret"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["path"] == "environment.k")
                    .unwrap();
                let actual = files["stringData"][item["key"].as_str().unwrap()]
                    .as_str()
                    .unwrap()
                    .as_bytes();
                if prefix == "first-apply" {
                    let mut initial = environment.clone();
                    initial.release = crate::environment::document::DeclaredRelease::None;
                    assert_eq!(actual, initial.to_kcl().as_bytes());
                } else {
                    assert_eq!(actual, environment.to_kcl().as_bytes());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn install_host_inputs_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        for project in ["wms", "receiving"] {
            let release = crate::release_chart::release_name("test", project, "dev")?;
            let (host_index, host) = object(
                render,
                "Deployment",
                "hosts",
                &format!("hostgroup-{release}"),
            );
            let variables = host["spec"]["template"]["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap();
            for (variable, family) in [
                ("WAMN_PG_URL", "guest"),
                ("WAMN_SYSTEM_URL", "identity-reader"),
                ("WAMN_CONNECTION_STORE_URL", "conn-reader"),
                ("WAMN_EXECUTOR_PLATFORM_PG_URL", "executor-platform"),
                ("WAMN_HTTP_ADMITTER_PG_URL", "http-admitter"),
                ("WAMN_EVENT_MATERIALIZER_PG_URL", "event-materializer"),
                ("WAMN_ADMINISTRATION_PG_URL", "administration"),
            ] {
                let (secret_index, secret) = object(
                    render,
                    "Secret",
                    "hosts",
                    &format!("wamn-{family}-test--{project}--dev"),
                );
                let entry = variables
                    .iter()
                    .find(|entry| entry["name"] == variable)
                    .unwrap();
                assert_eq!(
                    entry["valueFrom"]["secretKeyRef"]["name"],
                    secret["metadata"]["name"]
                );
                assert_eq!(entry["valueFrom"]["secretKeyRef"]["key"], "url");
                assert!(secret["stringData"]["url"].is_string());
                assert!(secret_index < host_index);
            }
        }
        for release in [
            crate::release_chart::release_name("test", "wms", "dev")?,
            crate::release_chart::release_name("test", "receiving", "dev")?,
            "c-test".into(),
        ] {
            let (host_index, host) = object(
                render,
                "Deployment",
                "hosts",
                &format!("hostgroup-{release}"),
            );
            let volumes = host["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap();
            for (volume_name, certificate_name) in [
                ("runtime-cert", "wasmcloud-runtime-tls"),
                (
                    "data-cert",
                    if release == "c-test" {
                        "wamn-nats-control-test"
                    } else if release == crate::release_chart::release_name("test", "wms", "dev")? {
                        "wamn-nats-test--wms--dev"
                    } else {
                        "wamn-nats-test--receiving--dev"
                    },
                ),
            ] {
                let (certificate_index, certificate) =
                    object(render, "Certificate", "hosts", certificate_name);
                let volume = volumes
                    .iter()
                    .find(|volume| volume["name"] == volume_name)
                    .unwrap();
                assert_eq!(
                    volume["secret"]["secretName"],
                    certificate["spec"]["secretName"]
                );
                assert!(certificate_index < host_index);
            }
            let (oauth_index, oauth) = object(render, "Secret", "hosts", "wamn-oauth");
            let volume = volumes
                .iter()
                .find(|volume| volume["name"] == "oauth-key")
                .unwrap();
            assert_eq!(volume["secret"]["secretName"], oauth["metadata"]["name"]);
            assert!(oauth_index < host_index);
        }
        let (host_index, host) = object(render, "Deployment", "hosts", "hostgroup-c-test");
        let (secret_index, secret) = object(render, "Secret", "hosts", "wamn-control-test");
        let entry = host["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "WAMN_CONTROL_URL")
            .unwrap();
        assert_eq!(
            entry["valueFrom"]["secretKeyRef"]["name"],
            secret["metadata"]["name"]
        );
        assert_eq!(entry["valueFrom"]["secretKeyRef"]["key"], "url");
        assert!(secret["stringData"]["url"].is_string());
        assert!(secret_index < host_index);
        for (volume_name, secret_name) in [
            ("identity-operator", "operator-test"),
            ("control-administration", "wamn-control-administration-test"),
        ] {
            let (secret_index, secret) = object(render, "Secret", "hosts", secret_name);
            let volume = host["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|volume| volume["name"] == volume_name)
                .unwrap();
            assert_eq!(volume["secret"]["secretName"], secret["metadata"]["name"]);
            assert!(secret_index < host_index);
        }
        Ok(())
    }

    #[test]
    fn install_nats_inputs_precede_consumers() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        let (nats_index, nats) = object(render, "StatefulSet", "platform", "wamn-nats");
        let (authorization_index, authorization) =
            object(render, "Secret", "platform", "wamn-nats-authorization");
        let (tls_index, tls) = object(render, "Certificate", "platform", "wasmcloud-nats-tls");
        let volumes = nats["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let authorization_volume = volumes
            .iter()
            .find(|volume| volume["name"] == "authorization")
            .unwrap();
        let tls_volume = volumes
            .iter()
            .find(|volume| volume["name"] == "tls")
            .unwrap();
        assert_eq!(
            authorization_volume["secret"]["secretName"],
            authorization["metadata"]["name"]
        );
        assert_eq!(
            authorization_volume["secret"]["items"][0]["key"],
            "authorization.conf"
        );
        assert!(authorization["data"]["authorization.conf"].is_string());
        assert_eq!(
            tls_volume["secret"]["secretName"],
            tls["spec"]["secretName"]
        );
        assert!(authorization_index < nats_index && tls_index < nats_index);
        Ok(())
    }

    #[test]
    fn install_tool_command_inputs() -> anyhow::Result<()> {
        let render = super::fixture::render()?;
        for (namespace, name, filename, argument, directory) in [
            ("hosts", "platform-identity-check", "ca.crt", 4, "/inputs"),
            ("edge", "platform-edge-check", "web.json", 1, "/inputs"),
        ] {
            let (job_index, job) = object(render, "Job", namespace, name);
            let (secret_index, files) =
                object(render, "Secret", namespace, &format!("{name}-files"));
            let spec = &job["spec"]["template"]["spec"];
            let volume = &spec["volumes"][0];
            let container = &spec["containers"][0];
            assert_eq!(volume["secret"]["secretName"], files["metadata"]["name"]);
            assert_eq!(container["volumeMounts"][0]["name"], volume["name"]);
            assert_eq!(container["volumeMounts"][0]["mountPath"], directory);
            let item = volume["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["path"] == filename)
                .unwrap();
            assert_eq!(
                container["args"][argument],
                format!("{directory}/{}", item["path"].as_str().unwrap())
            );
            assert!(files["stringData"][item["key"].as_str().unwrap()].is_string());
            assert!(secret_index < job_index);
        }
        let (ui_index, ui) = object(render, "Job", "hosts", "platform-ui-test");
        let (files_index, files) = object(render, "Secret", "hosts", "platform-ui-test-files");
        let spec = &ui["spec"]["template"]["spec"];
        assert_eq!(
            spec["volumes"][0]["secret"]["secretName"],
            files["metadata"]["name"]
        );
        assert_eq!(
            spec["containers"][0]["volumeMounts"][0]["name"],
            spec["volumes"][0]["name"]
        );
        assert_eq!(
            spec["containers"][0]["volumeMounts"][0]["mountPath"],
            "/inputs"
        );
        for filename in [
            "chart/Chart.yaml",
            "chart/platform-build.json",
            "deploy/platform/release/Chart.yaml",
            "apps/platform_ui/dist/index.html",
        ] {
            let item = spec["volumes"][0]["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["path"] == filename)
                .unwrap();
            assert!(files["data"][item["key"].as_str().unwrap()].is_string());
        }
        assert!(files_index < ui_index);
        let (certificate_index, certificate) =
            object(render, "Certificate", "edge", "wamn-edge-tls");
        for (host, path) in [
            ("control.example.com", "/api/wamn_control/control/mine"),
            ("wms.example.com", "/api/sample/read"),
            ("receiving.example.com", "/api/sample/read"),
            ("control.example.com", "/"),
            ("control.example.com", "/config.json"),
        ] {
            let name = format!(
                "platform-https-{}",
                &super::super::job::input_digest(&serde_json::json!([host, "GET", path]))[..16]
            );
            let (job_index, job) = object(render, "Job", "edge", &name);
            let spec = &job["spec"]["template"]["spec"];
            assert_eq!(
                spec["volumes"][0]["secret"]["secretName"],
                certificate["spec"]["secretName"]
            );
            assert_eq!(spec["volumes"][0]["secret"]["items"][0]["key"], "tls.crt");
            assert_eq!(spec["containers"][0]["args"][7], "/etc/edge/tls.crt");
            assert_eq!(
                spec["containers"][0]["volumeMounts"][0]["name"],
                spec["volumes"][0]["name"]
            );
            assert_eq!(
                spec["containers"][0]["volumeMounts"][0]["mountPath"],
                "/etc/edge"
            );
            assert!(certificate_index < job_index);
        }
        Ok(())
    }
}

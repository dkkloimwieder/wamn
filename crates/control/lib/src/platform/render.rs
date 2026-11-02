//! Render the platform install batches without a cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::job::{JobRequest, JobSecret};
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

/// Render the install batches in their apply order without executing them.
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
        super::install::cert_manager_objects(repository, document)?,
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
        super::install::operator_objects(repository, document)?,
    );
    append(
        &mut objects,
        "4.4",
        super::install::runtime_events(repository, document)?,
    );
    append(
        &mut objects,
        "4.4.1",
        super::install::collector_objects(repository, document)?,
    );
    append(
        &mut objects,
        "4.5",
        super::install::postgres_operator_objects(repository, document)?,
    );
    append(
        &mut objects,
        "4.5",
        super::install::postgres_cluster_objects(repository, document)?,
    );
    let superuser = serde_json::from_value(fixtures.postgres_superuser.clone())?;
    let admin = super::install::administrator(&superuser)?;
    for request in super::install::provisioning_jobs(document, &fixtures.runner_image) {
        append(
            &mut objects,
            "4.7",
            job_objects(
                &request,
                "platform",
                &json!({}),
                std::slice::from_ref(&admin),
                fixtures,
            )?,
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
        .ok_or_else(|| anyhow::anyhow!("missing initial authorization fixture"))?;
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
        let (request, secrets) = super::environments::apply_request(
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
            job_objects(
                &request,
                "platform",
                &json!({"environment": environment}),
                &secrets,
                fixtures,
            )?,
        );
    }
    let mut targets = Vec::new();
    for environment in environments {
        let key = environment_key(environment);
        let url = fixtures
            .environment_urls
            .get(&key)
            .context("environment URL fixture")?;
        let (request, secrets, inputs) =
            super::identity::session_inputs(environment, &fixtures.runner_image, &admin, url)?;
        append(
            &mut objects,
            "4.12",
            job_objects(&request, "identity", &inputs, &secrets, fixtures)?,
        );
        targets.push(format!("wamn-session-role-reader-{key}"));
    }
    let chart =
        super::identity::chart_objects(repository, document, &fixtures.identity_image, targets)?;
    let values = chart[0]["values"].clone();
    append(&mut objects, "4.12", chart);
    let (probe, secrets) = super::identity::key_probe(&fixtures.postgres_image, &superuser)?;
    let issuer = super::identity::ISSUER;
    append(
        &mut objects,
        "4.12",
        job_objects(
            &probe,
            "platform",
            &json!({"issuer":issuer,"identity_image":fixtures.identity_image}),
            &secrets,
            fixtures,
        )?,
    );
    let state: Value = serde_json::from_str(
        fixtures
            .outputs
            .get(&probe.name)
            .context("session key state fixture")?,
    )?;
    let kid = if let Some(kid) = state["kid"].as_str() {
        kid.to_owned()
    } else {
        let credential = serde_json::from_value(fixtures.identity_database.clone())?;
        let (publish, secrets) =
            super::identity::key_publish(&fixtures.identity_image, &credential)?;
        append(
            &mut objects,
            "4.12",
            job_objects(
                &publish,
                "identity",
                &json!({"issuer":issuer}),
                &secrets,
                fixtures,
            )?,
        );
        let public: Value = serde_json::from_str(
            fixtures
                .outputs
                .get(&publish.name)
                .context("published key fixture")?,
        )?;
        let kid = public["kid"]
            .as_str()
            .context("published kid fixture")?
            .to_owned();
        append(
            &mut objects,
            "4.12",
            job_objects(
                &super::identity::key_activate_request(&fixtures.identity_image, &kid),
                "identity",
                &json!({"issuer":issuer,"kid":kid}),
                &secrets,
                fixtures,
            )?,
        );
        kid
    };
    append(
        &mut objects,
        "4.12",
        job_objects(
            &super::identity::check_request(&super::install::secret_text(&identity_tls, "ca.crt")?),
            "hosts",
            &json!({"kid":kid,"values":values}),
            &[],
            fixtures,
        )?,
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
            .map(|package| &package.digest)
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
            append(
                &mut objects,
                "4.13",
                job_objects(
                    &request,
                    "platform",
                    &json!({"artifact":package.digest,"database":database.path()}),
                    &secrets,
                    fixtures,
                )?,
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
            append(
                &mut objects,
                "4.13",
                job_objects(
                    &request,
                    "platform",
                    &json!({"artifacts":artifacts,"database":database.path()}),
                    &secrets,
                    fixtures,
                )?,
            );
        }
        for package in packages {
            let request = super::packages::replica_job(
                &key,
                &package.id,
                &fixtures.runner_image,
                &package.files,
            );
            append(
                &mut objects,
                "4.13",
                job_objects(
                    &request,
                    "platform",
                    &json!({"artifact":package.digest,"database":database.path()}),
                    &secrets,
                    fixtures,
                )?,
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
            .map(|package| &package.digest)
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
        append(
            &mut objects,
            "4.14",
            job_objects(
                &request,
                "platform",
                &json!({"artifacts":artifacts,"database":database.path()}),
                &secrets,
                fixtures,
            )?,
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
        let (request, credential) = super::environments::connection_inputs(
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
            job_objects(
                &request,
                "hosts",
                &json!({"environment":environment}),
                std::slice::from_ref(&credential),
                fixtures,
            )?,
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
        let (mut request, secrets) = super::environments::apply_request(
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
            &mut request,
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
            request
                .files
                .get("host-group.yaml")
                .context("rendered host group")?,
        )?;
        let manifest = fixtures
            .releases
            .get(&key)
            .context("release manifest fixture")?;
        let mut applications = environment.applications.clone();
        for application in applications.values_mut() {
            application.connections.clear();
        }
        let batch = super::environments::release_chart_objects(
            &fixtures.directory.join("release-chart"),
            &crate::release_chart::ValuesInput {
                org: environment.org.clone(),
                project: environment.project.clone(),
                env: environment.env.clone(),
                manifest_digest: environment.release.to_string(),
                artifact_base: super::packages::artifact_base(&fixtures.registry, "releases")?,
                route_host: environment.route_host.clone(),
                applications,
                roles: crate::release_chart::roles(
                    manifest,
                    &environment.org,
                    &environment.project,
                    &environment.env,
                ),
                drain_bound_seconds: 70,
                actor: "platform render".into(),
                host_group: serde_yaml::Mapping::new(),
            },
            &group,
            fixtures
                .web_clients
                .get(&key)
                .context("web client fixture")?,
        )?;
        let chart_objects = super::environments::template_objects(&batch[0])?;
        append(
            &mut objects,
            "4.17",
            job_objects_with(
                &request,
                "platform",
                &json!({"environment":environment}),
                &secrets,
                fixtures,
                &chart_objects,
            )?,
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
            let request = super::web::upload_request(
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
                job_objects(
                    &request,
                    "hosts",
                    &json!({"environment":environment}),
                    &[],
                    fixtures,
                )?,
            );
        }
    }
    append(
        &mut objects,
        "4.19.1",
        job_objects(
            &super::control::provision_request(document, &fixtures.runner_image),
            "platform",
            &json!({"org":document.org}),
            &[],
            fixtures,
        )?,
    );
    append(
        &mut objects,
        "4.19.1",
        job_objects(
            &super::control::operator_request(document, &fixtures.runner_image)?,
            "platform",
            &json!({"source_version":fixtures.control_operator["metadata"]["resourceVersion"]}),
            &[],
            fixtures,
        )?,
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
        super::environments::template_objects(&entry[0])?,
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
        let request = super::web::platform_ui_with_files(
            document,
            &fixtures.runner_image,
            fixtures.platform_ui_files.clone(),
        );
        append(
            &mut objects,
            "4.19.1",
            job_objects(
                &request,
                "hosts",
                &json!({"org":document.org}),
                &[],
                fixtures,
            )?,
        );
    }
    let owner_state = super::invite::state_request(document, &fixtures.postgres_image);
    append(
        &mut objects,
        "4.19.1",
        job_objects(
            &owner_state,
            "platform",
            &json!({"org":document.org}),
            &[],
            fixtures,
        )?,
    );
    if super::invite::needs_invite(
        fixtures
            .outputs
            .get(&owner_state.name)
            .context("owner state fixture")?,
    )? {
        append(
            &mut objects,
            "4.19.1",
            job_objects(
                &super::invite::request(document, &fixtures.runner_image, &auth)?,
                "platform",
                &json!({"org":document.org}),
                &[],
                fixtures,
            )?,
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
        let request = super::edge::check_inputs(document, &routes, &chart, &fixtures.web_hosts)?;
        append(
            &mut objects,
            "4.20",
            job_objects(&request, "edge", &json!({"routes":routes}), &[], fixtures)?,
        );
    }
    let entry = super::edge::proxy_objects(repository, document, &routes)?;
    append(
        &mut objects,
        "4.20",
        super::environments::template_objects(&entry[0])?,
    );
    Ok(objects)
}

fn environment_key(environment: &EnvironmentDocument) -> String {
    wamn_control_registry::project_env_key(&environment.org, &environment.project, &environment.env)
}

fn job_objects(
    request: &JobRequest,
    namespace: &str,
    inputs: &Value,
    secrets: &[JobSecret],
    fixtures: &Fixtures,
) -> anyhow::Result<Vec<Value>> {
    job_objects_with(request, namespace, inputs, secrets, fixtures, &[])
}

fn job_objects_with(
    request: &JobRequest,
    namespace: &str,
    inputs: &Value,
    secrets: &[JobSecret],
    fixtures: &Fixtures,
    nested: &[Value],
) -> anyhow::Result<Vec<Value>> {
    let mut objects = super::job::start_objects(request, namespace, secrets)?;
    let owner = json!({"name":request.name, "namespace":namespace, "uid":fixtures.job_uid});
    objects.extend(super::job::support_objects(request, &owner, secrets)?);
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
    objects.extend(batch.into_iter().map(|object| (step, object)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_release() -> anyhow::Result<wamn_catalog::ServingManifest> {
        let digest = format!("sha256:{}", "1".repeat(64));
        let operation = "edge-device:sample/read@2.1.0";
        let mut descriptor = serde_json::to_value(wamn_catalog::ComponentDescriptor::named(
            "device", "0.1", &digest,
        ))?;
        descriptor["operations"] = json!({operation:{"registered-operation":wamn_catalog::operation_token("edge_device","2.1.0","sample","read"),"input-ports":[],"output-ports":[],"parameters":[]}});
        let descriptor: wamn_catalog::ComponentDescriptor = serde_json::from_value(descriptor)?;
        let admitted = descriptor
            .into_admitted(wamn_catalog::ComponentPackageScope {
                env_key: String::new(),
                package_id: "edge_device".into(),
                package_version: "2.1.0".into(),
            })
            .component;
        let component = wamn_catalog::ServingComponent::project(&admitted, Vec::new())?;
        let value = json!({"format-version":11,"release":{"packages":[{"package-id":"edge_device","package-version":"2.1.0"}]},"components":[component],
            "attachments":{"sample-read-http":{"auth-policy":{"modes":["session"]},"component":"device","definition":{"id":"sample-read-http","route":{"method":"GET","path":"/sample/read"},"run-deadline-ms":30000,"type":"http"},"definition-hash":format!("sha256:{}","5".repeat(64)),"operation":operation,"package-id":"edge_device","type":"http"}},
            "routes":[{"component":"device","operation":operation,"package-id":"edge_device","type":"get"}]});
        Ok(wamn_catalog::ServingManifest::from_canonical_bytes(
            &wamn_execution_contract::canonical_json_bytes(&value),
        )?
        .0)
    }

    #[test]
    fn kind_install_renders_in_install_order() -> anyhow::Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let cluster = ClusterDocument::from_json(&serde_json::to_vec(&json!({
            "provider":"kind", "project_id":"", "zone":"", "cluster_name":"render",
            "org":"test", "owner_email":"owner@example.com", "registry":"registry.invalid/wamn",
            "buckets":{"web":"fixture-web", "otel":"", "state":""},
            "route_hosts":{"control":"control.example.com"}, "resend_from":"sender@example.com"
        }))?)?;
        // platform_apply_kind writes the WMS document first as a-unqualified-wms.k.
        let documents = ["wms", "receiving"]
            .into_iter()
            .map(|project| {
                let mut environment = EnvironmentDocument::compile(
                    &repository.join(format!("deploy/gcp/environments/dkk--{project}--dev.k")),
                )?;
                environment.org.clone_from(&cluster.org);
                environment.route_host = format!("{project}.example.com");
                Ok(environment)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let image = format!(
            "registry.invalid/wamn/fixture:render@sha256:{}",
            "1".repeat(64)
        );
        crate::release_chart::stamp(
            &repository.join("deploy/platform/release"),
            &crate::release_chart::ImageSet {
                host: image.clone(),
                http: image.clone(),
                materializer: image.clone(),
            },
            Some(&crate::release_chart::QualificationImages {
                runner: image.clone(),
                gates: image.clone(),
                identity: image,
            }),
            &work.path().join("release-chart"),
        )?;
        let chart_file = work.path().join("release-chart/values.yaml");
        let mut chart: Value = serde_yaml::from_slice(&std::fs::read(&chart_file)?)?;
        chart["platform"]["edge"]["image"] = json!(format!(
            "registry.invalid/wamn/edge@sha256:{}",
            "1".repeat(64)
        ));
        std::fs::write(&chart_file, serde_yaml::to_string(&chart)?)?;
        let mut fixtures = Fixtures {
            identity_tls: json!({"data": {"ca.crt": "Zml4dHVyZS1jYQ=="}}),
            broker_authorization_exists: false,
            web_hosts:["control.example.com".into()].into(),
            control_operator:json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"operator-test","namespace":"identity","resourceVersion":"1"},"type":"kubernetes.io/tls","data":{"ca.crt":"Zml4dHVyZS1jYQ==","tls.crt":"Zml4dHVyZS1jcnQ=","tls.key":"Zml4dHVyZS1rZXk="}}),
            platform_ui_files:BTreeMap::new(),
            cdc_credentials:BTreeMap::new(),
            gates_image:format!("registry.invalid/wamn/gates@sha256:{}","1".repeat(64)),
            releases:BTreeMap::new(),
            web_clients:BTreeMap::new(),
            applied:BTreeMap::new(),
            oauth_key:[0x41;32],
            oauth_exists:false,
            registry: wamn_package_artifact::PackageRegistry {artifact_base:"registry.invalid/wamn/packages".into(), registry_auth_file:work.path().join("registry.json"), insecure_registry:true, oci_ca_paths:Vec::new()},
            packages:BTreeMap::new(),
            identity_database: json!({"data":{"url":"cG9zdGdyZXNxbDovL2lkZW50aXR5OmZpeHR1cmVAZGF0YWJhc2Uvd2Ftbl9zeXN0ZW0="}}),
            identity_image: format!("registry.invalid/wamn/identity@sha256:{}", "1".repeat(64)),
            postgres_image: "postgres:18".into(),
            environment_urls: BTreeMap::new(),
            ctl_nats: json!({"data":{"ca.crt":"Zml4dHVyZS1jYQ==", "tls.crt":"Zml4dHVyZS1jcnQ=", "tls.key":"Zml4dHVyZS1rZXk="}}),
            directory: work.path().to_owned(),
            apply_url: "postgresql://wamn_apply:fixture@database/wamn_system".into(),
            postgres_superuser: json!({"data":{"password":"cGFzc3dvcmQ="}}),
            runner_image: format!("registry.invalid/wamn/runner@sha256:{}", "1".repeat(64)),
            job_uid: "00000000-0000-0000-0000-000000000001".into(),
            emitted: [
                ("platform-system".into(), json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"wamn-apply","namespace":"platform","labels":{"app.kubernetes.io/name":"wamn-apply"}},
                    "stringData":{"url":"postgresql://wamn_apply:fixture@database/wamn_system"}})),
                ("platform-identity-issuer".into(), json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"wamn-identity-db","namespace":"identity","labels":{"app.kubernetes.io/name":"wamn-identity","app.kubernetes.io/managed-by":"wamn"}},
                    "stringData":{"url":"postgresql://identity:fixture@database/wamn_system"}})),
            ].into(),
            outputs: [
                ("platform-session-key-state".into(),r#"{"kid":null}"#.into()),
                ("platform-session-key-publish".into(),r#"{"kid":"fixture-kid"}"#.into()),
                ("platform-identity-check".into(),r#"{"keys":[{"kid":"fixture-kid"}]}"#.into()),
            ].into(),
        };
        fixtures
            .outputs
            .insert("platform-owner-state-test".into(), "f\n".into());
        fixtures.platform_ui_files = [
            (
                "chart/Chart.yaml".into(),
                std::fs::read(work.path().join("release-chart/Chart.yaml"))?,
            ),
            (
                format!("chart/{}", crate::platform_build::BUILD_RECORD_FILE),
                b"{}".to_vec(),
            ),
            (
                "deploy/platform/release/Chart.yaml".into(),
                std::fs::read(repository.join("deploy/platform/release/Chart.yaml"))?,
            ),
            (
                "apps/platform_ui/dist/index.html".into(),
                b"<!doctype html><title>fixture</title>".to_vec(),
            ),
        ]
        .into();
        fixtures.applied.insert(
            "platform-control-credentials-test".into(),
            vec![
                wamn_control_provision::render_org_workload_secret_manifest(
                    wamn_control_provision::WorkloadRoleFamily::Control,
                    "test",
                    "hosts",
                    "postgresql://control:fixture@database/wamn_system",
                ),
                wamn_control_provision::render_control_administration_secret_manifest(
                    "test", "hosts",
                ),
            ],
        );
        let mut operator = fixtures.control_operator.clone();
        operator["metadata"] = json!({"name":"operator-test","namespace":"hosts"});
        fixtures
            .applied
            .insert("platform-control-operator-test".into(), vec![operator]);
        std::fs::write(&fixtures.registry.registry_auth_file, "{}")?;
        let mut manifest = fixture_release()?;
        let component = manifest.components.first().context("fixture component")?;
        let member = wamn_catalog::ServingEntryMember {
            package_id: component.package_id.clone(),
            component: component.descriptor.component.clone(),
            digest: component.descriptor.component_digest.clone(),
        };
        manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
            reference: format!("entries@sha256:{}", "1".repeat(64)),
            route_digest: format!("sha256:{}", "2".repeat(64)),
            application: member.clone(),
            members: [member].into(),
        });
        manifest =
            wamn_catalog::ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?.0;
        for environment in &documents {
            use wamn_control_provision::{
                WorkloadRoleFamily, WorkloadRoleScope, WorkloadSecretBody,
            };
            let key = environment_key(environment);
            let cdc = wamn_control_provision::render_project_env_cdc_secret_manifest(
                &environment.triple(),
                "render01",
                "hosts",
                &format!("postgresql://fixture:fixture@database/wamn-db-{key}--render01"),
            );
            fixtures.cdc_credentials.insert(key.clone(), cdc.clone());
            fixtures
                .applied
                .insert(format!("platform-apply-{key}"), vec![cdc]);

            fixtures.releases.insert(key.clone(), manifest.clone());
            fixtures.web_clients.insert(key.clone(), BTreeMap::new());

            fixtures.outputs.insert(
                format!("platform-release-{key}"),
                environment.release.to_string(),
            );

            let id = environment
                .applications
                .keys()
                .next()
                .context("fixture application")?
                .clone();
            fixtures.packages.insert(
                key.clone(),
                vec![Package {
                    id: id.clone(),
                    version: "1.0.0".into(),
                    digest: format!("sha256:{}", "2".repeat(64)),
                    files: [
                        (
                            "wamn.json".into(),
                            json!({"package":{"id":id,"version":"1.0.0"}}).to_string(),
                        ),
                        ("migrations/0001.sql".into(), "SELECT 1;".into()),
                    ]
                    .into(),
                }],
            );

            let database = wamn_control_provision::project_env_database_name(
                &environment.org,
                &environment.project,
                &environment.env,
                "render01",
            );
            fixtures.environment_urls.insert(
                key.clone(),
                format!("postgresql://fixture:fixture@database/{database}"),
            );
            let role = wamn_control_provision::workload_generation_role(
                WorkloadRoleFamily::SessionRoleReader,
                WorkloadRoleScope::ProjectEnvironment {
                    org: &environment.org,
                    project: &environment.project,
                    environment: &environment.env,
                    database: &database,
                },
                wamn_run_state::CredentialGeneration::A,
            )?;
            let target = wamn_control_provision::session_target::SessionTarget::new(
                &environment.triple(),
                "render01",
                "fixture",
                &format!("postgresql://{role}:fixture@database/{database}"),
            )?;
            fixtures.emitted.insert(
                format!("platform-session-{key}"),
                wamn_control_provision::render_workload_secret_manifest(
                    WorkloadRoleFamily::SessionRoleReader,
                    &environment.triple(),
                    "identity",
                    WorkloadSecretBody::SessionTarget(&target),
                ),
            );
        }
        let objects = install(&cluster, &documents, &repository, &fixtures)?;
        // This list grows as each install file gains its shared batch builders.
        for step in [
            "4.3", "4.4", "4.4.1", "4.5", "4.7", "4.8", "4.9", "4.11", "4.12", "4.13", "4.14",
            "4.16.1",
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
        let steps = super::super::steps::install_steps(&cluster, "render");
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

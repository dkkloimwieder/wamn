//! Render the platform install batches without a cluster.

use std::collections::BTreeMap;
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

    #[test]
    fn kind_install_renders_in_install_order() -> anyhow::Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let cluster = ClusterDocument::from_json(&serde_json::to_vec(&json!({
            "provider":"kind", "project_id":"", "zone":"", "cluster_name":"render",
            "org":"test", "owner_email":"owner@example.com", "registry":"registry.invalid/wamn",
            "buckets":{"web":"", "otel":"", "state":""},
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
        let mut fixtures = Fixtures {
            identity_tls: json!({"data": {"ca.crt": "Zml4dHVyZS1jYQ=="}}),
            broker_authorization_exists: false,
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
        std::fs::write(&fixtures.registry.registry_auth_file, "{}")?;
        for environment in &documents {
            use wamn_control_provision::{
                WorkloadRoleFamily, WorkloadRoleScope, WorkloadSecretBody,
            };
            let key = environment_key(environment);
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

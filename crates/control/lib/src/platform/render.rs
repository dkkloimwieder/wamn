//! Render the platform install batches without a cluster.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::job::{JobRequest, JobSecret};
use serde_json::{Value, json};

/// Live values supplied to the install batches instead of reading a cluster.
#[derive(Debug)]
pub struct Fixtures {
    pub identity_tls: Value,
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
    Ok(objects)
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
        let fixtures = Fixtures {
            identity_tls: json!({"data": {"ca.crt": "Zml4dHVyZS1jYQ=="}}),
            broker_authorization_exists: false,
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
            outputs: BTreeMap::new(),
        };
        let objects = install(&cluster, &documents, &repository, &fixtures)?;
        // This list grows as each install file gains its shared batch builders.
        for step in ["4.3", "4.4", "4.4.1", "4.5", "4.7", "4.8", "4.9", "4.11"] {
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

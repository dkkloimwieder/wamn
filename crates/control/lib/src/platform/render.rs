//! Render the platform install batches without a cluster.

use std::path::Path;

use serde_json::Value;

/// Live values supplied to the install batches instead of reading a cluster.
#[derive(Debug)]
pub struct Fixtures {
    pub identity_tls: Value,
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
    _environments: &[EnvironmentDocument],
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
        let fixtures = Fixtures {
            identity_tls: json!({"data": {"ca.crt": "Zml4dHVyZS1jYQ=="}}),
            broker_authorization_exists: false,
        };
        let objects = install(&cluster, &documents, &repository, &fixtures)?;
        // This list grows as each install file gains its shared batch builders.
        for step in ["4.3", "4.4", "4.4.1", "4.5", "4.7", "4.8", "4.9"] {
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

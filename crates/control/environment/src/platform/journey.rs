//! Production install inputs adapted for disposable application clusters.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_control_registry::Triple;

use super::document::Provider;
use super::{identity, manifests, resources};
use wamn_release_control::release_chart::{self, ValuesInput};

/// The explicit scope of a cluster's operator and NATS users.
#[derive(Debug)]
pub struct ClusterInput<'a> {
    /// The release chart directory that carries the pinned operator archive:
    /// a qualification case runs where only the stamped chart exists.
    pub release_chart: &'a Path,
    pub operator_namespace: &'a str,
    pub host_namespace: &'a str,
    pub org: &'a str,
    pub environments: &'a [Triple],
}

/// Common prerequisites, NATS objects, and the production operator chart entry.
#[derive(Debug)]
pub struct Cluster {
    pub prerequisites: Vec<Value>,
    pub nats: Vec<Value>,
    pub operator: Value,
}

/// Build the kept delivery cluster's platform and PostgreSQL prerequisites.
///
/// # Errors
/// Returns an error when a production input cannot be rendered.
pub fn delivery(repository: &Path, input: &ClusterInput<'_>) -> anyhow::Result<Cluster> {
    let mut inputs = cluster(repository, input)?;
    inputs
        .prerequisites
        .extend(resources::postgres_operator_objects(
            repository,
            Provider::Kind,
        )?);
    Ok(inputs)
}

/// Build common platform inputs for explicit operator and host namespaces.
///
/// # Errors
/// Returns an error when a production input cannot be rendered.
pub fn cluster(repository: &Path, input: &ClusterInput<'_>) -> anyhow::Result<Cluster> {
    let mut prerequisites = vec![resources::namespace_object(input.operator_namespace)];
    if input.host_namespace != input.operator_namespace {
        prerequisites.push(resources::namespace_object(input.host_namespace));
    }
    prerequisites.extend(resources::cert_manager_objects(repository, Provider::Kind)?);
    prerequisites.extend(resources::runtime_events(
        repository,
        input.host_namespace,
        input.operator_namespace,
    )?);
    prerequisites.extend(manifests::host_account_objects(
        repository,
        input.host_namespace,
    )?);
    // A kind host group mounts the local OAuth provider's key Secret.
    prerequisites.extend(super::oauth::key_objects(
        &super::oauth::new_key()?,
        input.host_namespace,
    ));
    let mut values = manifests::operator_base_values(repository)?;
    let url = format!(
        "nats://wamn-nats.{}.svc.cluster.local:4222",
        input.operator_namespace
    );
    values["global"]["nats"] = json!({"schedulerUrl":url,"dataUrl":url});
    values["operator"]["watchNamespaces"] = json!([input.host_namespace]);
    values["operator"]["hostNamespaces"] = json!([input.host_namespace]);
    Ok(Cluster {
        prerequisites,
        nats: super::kind_nats(
            repository,
            input.operator_namespace,
            input.org,
            input.environments,
        )?,
        operator: resources::operator_entry(
            input.release_chart,
            input.operator_namespace,
            &values,
        )?,
    })
}

/// A production PostgreSQL batch and its explicit delivery-only resources.
#[derive(Debug)]
pub struct Database {
    pub objects: Vec<Value>,
    pub fixture_objects: Vec<Value>,
}

/// Build a delivery database with an explicit fixture-only NodePort Service.
///
/// # Errors
/// Returns an error when the production PostgreSQL manifest cannot be read.
pub fn delivery_database(
    repository: &Path,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Database> {
    let mut objects = vec![resources::namespace_object(namespace)];
    objects.extend(resources::postgres_cluster_inputs(
        repository,
        "deploy/infra/cnpg-cluster.yaml",
        Provider::Kind,
        name,
        namespace,
    )?);
    Ok(Database {
        objects,
        fixture_objects: vec![json!({"apiVersion":"v1","kind":"Service",
            "metadata":{"name":format!("{name}-node"),"namespace":namespace},
            "spec":{"type":"NodePort",
                "selector":{"cnpg.io/cluster":name,"cnpg.io/instanceRole":"primary"},
                "ports":[{"port":5432,"targetPort":5432}]}})],
    })
}

/// Explicit endpoint substitutions for an application release in a test cluster.
#[derive(Debug)]
pub struct ReleaseInput<'a> {
    pub chart: &'a Path,
    pub namespace: &'a str,
    pub values: &'a ValuesInput,
    pub nats_url: &'a str,
    pub otlp_endpoint: &'a str,
    pub insecure_registry: bool,
}

/// Account access objects and a production release chart input.
#[derive(Debug)]
pub struct Release {
    pub objects: Vec<Value>,
    pub chart: Value,
}

/// Build release values and account access before explicit fixture substitutions.
///
/// # Errors
/// Returns an error when the production values or chart metadata are invalid.
pub fn release(input: &ReleaseInput<'_>) -> anyhow::Result<Release> {
    let values = release_values(
        input.values,
        input.nats_url,
        input.otlp_endpoint,
        input.insecure_registry,
    )?;
    let account = wamn_credential_store::kubernetes::service_account_name(
        &input.values.org,
        &input.values.project,
        &input.values.env,
    );
    let secret = wamn_credential_store::kubernetes::secret_name(
        &input.values.org,
        &input.values.project,
        &input.values.env,
    );
    let objects = crate::environment::ensure::credential_access(&account, &secret)
        .into_iter()
        .map(|(_, object)| manifests::apply_namespace(object, input.namespace))
        .collect();
    Ok(Release {
        objects,
        chart: json!({"chart":input.chart,"version":release_chart::chart_version(input.chart)?,
            "name":release_chart::release_name(&input.values.org,&input.values.project,&input.values.env)?,
            "namespace":input.namespace,"values":values}),
    })
}

/// Render production values with explicit test endpoints.
///
/// # Errors
/// Returns an error when production values cannot be rendered.
pub fn release_values(
    values_input: &ValuesInput,
    nats_url: &str,
    otlp_endpoint: &str,
    insecure_registry: bool,
) -> anyhow::Result<Value> {
    let mut values = serde_json::to_value(release_chart::values(values_input)?)?;
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    for key in ["schedulerNatsUrl", "dataNatsUrl"] {
        group[key] = nats_url.into();
    }
    if insecure_registry {
        group["extraArgs"]
            .as_array_mut()
            .context("release host arguments")?
            .push("--allow-insecure-registries".into());
    }
    let endpoint = group["env"]
        .as_array_mut()
        .context("release host variables")?
        .iter_mut()
        .find(|variable| variable["name"] == "OTEL_EXPORTER_OTLP_ENDPOINT")
        .context("release OTLP endpoint")?;
    endpoint["value"] = otlp_endpoint.into();
    Ok(values)
}

/// Build identity inputs for explicit foundation or session mode.
///
/// # Errors
/// Returns an error when the production identity chart input is invalid.
pub fn identity(input: &identity::ChartInput<'_>) -> anyhow::Result<Value> {
    identity::chart_input(input)
}

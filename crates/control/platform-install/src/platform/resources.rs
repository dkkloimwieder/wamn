//! Platform resource rendering, Helm execution, and bounded Kubernetes application.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

pub async fn helm(target: &Target, arguments: &[&std::ffi::OsStr]) -> anyhow::Result<Vec<u8>> {
    let mut args = vec![
        std::ffi::OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        std::ffi::OsStr::new("--kube-context"),
        std::ffi::OsStr::new(&target.context),
    ];
    args.extend_from_slice(arguments);
    wamn_kubernetes::helm::run(&args).await
}

/// Apply one service release and wait for readiness.
pub(crate) async fn apply_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let owner = Target {
        namespace: entry["namespace"]
            .as_str()
            .context("service namespace")?
            .into(),
        ..target.clone()
    };
    wamn_release_control::release_chart::recover_pending(&owner, name).await?;
    upgrade_release(&owner, entry, directory).await
}

/// Upgrade one release after the caller recovers pending operations.
pub(crate) async fn upgrade_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let chart = entry["chart"].as_str().context("service chart")?;
    let values = directory.join(format!("values-{name}.yaml"));
    std::fs::write(&values, serde_json::to_vec(&entry["values"])?)?;
    wamn_kubernetes::helm::upgrade(
        target,
        Path::new(chart),
        name,
        &values,
        Duration::from_secs(600),
    )
    .await
}

/// Require the references rendered by this release before changing any resource.
pub(crate) async fn require_inputs(target: &Target, entry: &Value) -> anyhow::Result<()> {
    let objects = wamn_kubernetes::helm::template(entry)?;
    wamn_kubernetes::helm::require_references(
        entry["namespace"].as_str().context("service namespace")?,
        &objects,
        async |namespace, type_, name| {
            kubectl::get(
                &target.kubeconfig,
                &target.context,
                namespace,
                &type_.to_ascii_lowercase(),
                name,
            )
            .await
        },
    )
    .await
}

pub use super::installed::release_values;

/// Build a namespace prerequisite.
pub fn namespace_object(namespace: &str) -> Value {
    json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace}})
}

/// Submit an explicit credential or bootstrap object.
///
/// # Errors
/// Returns a Kubernetes error with the resource name and namespace.
pub async fn apply_object(target: &Target, object: &Value) -> anyhow::Result<()> {
    let namespace = object["metadata"]["namespace"]
        .as_str()
        .unwrap_or(&target.namespace);
    let name = object["metadata"]["name"]
        .as_str()
        .context("manifest object has no metadata.name")?;
    let type_ = object["kind"]
        .as_str()
        .context("manifest object has no type")?;
    kubectl::apply(&target.kubeconfig, &target.context, namespace, object)
        .await
        .with_context(|| format!("apply {type_} {namespace}/{name}"))
}

/// Registry authority used by native development tools.
pub const LOCAL_REGISTRY: &str = "localhost:5001";
/// Registry authority used by Kubernetes workloads.
pub const POD_REGISTRY: &str = "wamn-registry.platform.svc.cluster.local:5000";

pub fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

//! Platform resource rendering, Helm execution, and bounded Kubernetes application.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_kubernetes::{
    Target,
    helm::{self, Install, Wait},
    kubectl,
};

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

/// Apply one service release with its caller's wait strategy.
pub(crate) async fn apply_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
    wait: Wait,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let namespace = entry["namespace"]
        .as_str()
        .context("service release namespace")?;
    helm::recover_pending(target, namespace, name, wait).await?;
    upgrade_release(target, entry, directory, wait, Install::CreateNamespace).await
}

/// Upgrade one release after the caller recovers pending operations.
pub(crate) async fn upgrade_release(
    target: &Target,
    entry: &Value,
    directory: &Path,
    wait: Wait,
    install: Install,
) -> anyhow::Result<()> {
    let name = entry["name"].as_str().context("service release name")?;
    let namespace = entry["namespace"]
        .as_str()
        .context("service release namespace")?;
    let chart = entry["chart"].as_str().context("service chart")?;
    let values = directory.join(format!("values-{name}.yaml"));
    std::fs::write(&values, serde_json::to_vec(&entry["values"])?)?;
    helm::upgrade(
        target,
        namespace,
        Path::new(chart),
        name,
        &values,
        wait,
        install,
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

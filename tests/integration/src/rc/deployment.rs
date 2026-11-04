//! The RC host keeps the production base and its declared event environment.

use std::fs;
use std::path::Path;

use anyhow::ensure;
use async_nats::jetstream::stream;
use serde_json::json;
use wamn_control_provision::events;
use wamn_control_registry::Triple;

use super::{NAMESPACE, Resources, checked, kubectl};

pub(super) fn source_declaration(repository: &Path) -> anyhow::Result<stream::Config> {
    let source: stream::Config = serde_json::from_slice(&fs::read(
        repository.join("deploy/gates/rc-event-stream.json"),
    )?)?;
    validate_source(&source)?;
    Ok(source)
}
fn validate_source(source: &stream::Config) -> anyhow::Result<()> {
    ensure!(
        source.num_replicas > 0 && !source.duplicate_window.is_zero(),
        "RC must declare stream replicas and a duplicate window"
    );
    ensure!(
        source.duplicate_window.subsec_nanos() == 0,
        "RC activation carries whole duplicate-window seconds"
    );
    let expected = events::source_stream_config(
        &Triple::new("rc", "app", "dev"),
        source.num_replicas,
        source.duplicate_window,
    );
    ensure!(
        events::stream_config_matches(&expected, source),
        "RC event stream must match its declared environment and native source configuration"
    );
    Ok(())
}

/// The cert-manager CA, the one NATS and the operator. The hosts run in
/// `NAMESPACE`, so the environment's certificate lives there
/// (docs/plan/alignment.md R5).
pub(super) async fn install_platform(resources: &Resources) -> anyhow::Result<()> {
    checked(kubectl(resources).args(["create", "namespace", NAMESPACE])).await?;
    let mut inputs = wamn_environment_control::platform::journey::cluster(
        &resources.repository,
        &wamn_environment_control::platform::journey::ClusterInput {
            operator_namespace: "wamn-system",
            host_namespace: NAMESPACE,
            org: "rc",
            environments: &[Triple::new("rc", "app", "dev")],
        },
    )?;
    let certificate = format!(
        "wamn-nats-{}",
        wamn_control_registry::project_env_key("rc", "app", "dev")
    );
    for object in &mut inputs.nats {
        if object["kind"] == "Certificate" && object["metadata"]["name"] == certificate.as_str() {
            object["metadata"]["namespace"] = json!(NAMESPACE);
        }
    }
    resources
        .lifecycle
        .install(
            &resources.repository,
            NAMESPACE,
            &inputs.prerequisites,
            &inputs.nats,
            &inputs.operator,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_stream_refuses_foreign_subjects_or_unset_bounds() {
        let source: stream::Config = serde_json::from_str(include_str!(
            "../../../../deploy/gates/rc-event-stream.json"
        ))
        .unwrap();
        validate_source(&source).unwrap();
        let mut changed = source.clone();
        changed.subjects = vec!["evt.other.>".into()];
        assert!(validate_source(&changed).is_err());
        let mut changed = source.clone();
        changed.num_replicas = 0;
        assert!(validate_source(&changed).is_err());
        let mut changed = source;
        changed.duplicate_window = std::time::Duration::ZERO;
        assert!(validate_source(&changed).is_err());
    }
}

//! The RC host keeps the production base and its declared event environment.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use async_nats::jetstream::stream;
use serde_json::{Value, json};
use wamn_control_provision::events;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::platform;

use super::{CLUSTER, NAMESPACE, Resources, apply, checked, kubectl, resources::write_private};

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
    let operator = resources.work.join("operator-values.yaml");
    write_private(
        &operator,
        &serde_json::to_vec(
            &json!({"operator":{"watchNamespaces":[NAMESPACE],"hostNamespaces":[NAMESPACE],"allowSharedHosts":false}}),
        )?,
    )?;
    let mut nats = wamn_control::platform::kind_nats(
        &resources.repository,
        "wamn-system",
        "rc",
        &[Triple::new("rc", "app", "dev")],
    )?;
    let certificate = format!(
        "wamn-nats-{}",
        wamn_control_registry::project_env_key("rc", "app", "dev")
    );
    for object in &mut nats {
        if object["kind"] == "Certificate" && object["metadata"]["name"] == certificate.as_str() {
            object["metadata"]["namespace"] = json!(NAMESPACE);
        }
    }
    platform::install(
        &resources.repository,
        &resources.lifecycle,
        CLUSTER,
        &resources.work,
        NAMESPACE,
        &operator,
        &nats,
    )
    .await
}

/// The host release, after the event stream exists.
pub(super) async fn install_host(
    resources: &Resources,
    source: &stream::Config,
) -> anyhow::Result<()> {
    for (name, document) in [
        (
            "host-db.json",
            json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"wamn-host-db","namespace":NAMESPACE},"stringData":{"url":"postgres://postgres@127.0.0.1:1/postgres?sslmode=disable"}}),
        ),
        (
            "registry-pull.json",
            json!({"apiVersion":"v1","kind":"Secret","type":"kubernetes.io/dockerconfigjson","metadata":{"name":"wamn-registry-pull","namespace":NAMESPACE},"stringData":{".dockerconfigjson":serde_json::to_string(&json!({"auths":{"registry.wamn-system.svc.cluster.local:5000":{"username":"unused","password":"unused","auth":"dW51c2VkOnVudXNlZA=="}}}))?}}),
        ),
    ] {
        let path = resources.work.join(name);
        write_private(&path, &serde_json::to_vec(&document)?)?;
        apply(resources, &path).await?;
    }
    let base: Value = serde_yaml::from_str(&fs::read_to_string(
        resources
            .repository
            .join("deploy/platform/values-host-default.yaml"),
    )?)?;
    let rendered = host_values(base, &resources.host_image, source)?;
    let values = resources.work.join("host-values.yaml");
    write_private(&values, &serde_json::to_vec(&rendered)?)?;
    super::resources::recorded(
        resources,
        "install-host",
        tokio::process::Command::new(&resources.lifecycle)
            .arg("install-host")
            .arg(CLUSTER)
            .arg(&resources.work)
            .arg(NAMESPACE)
            .arg(&values),
    )
    .await?;
    checked(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "rollout",
        "status",
        "deployment/hostgroup-default",
        "--timeout=180s",
    ]))
    .await?;
    Ok(())
}
fn host_values(mut base: Value, image: &str, source: &stream::Config) -> anyhow::Result<Value> {
    let (repository, tag) = image
        .rsplit_once(':')
        .context("RC image has its explicit tag")?;
    ensure!(
        repository == "wamn-host",
        "RC must retain its native host repository"
    );
    base["runtime"]["image"]["tag"] = json!(tag);
    let groups = base["runtime"]["hostGroups"]
        .as_array_mut()
        .context("host base declares its groups")?;
    ensure!(groups.len() == 1, "RC base declares exactly one host group");
    let group = &mut groups[0];
    ensure!(
        group["replicas"] == 3,
        "RC retains three production host replicas"
    );
    group["namespace"] = json!(NAMESPACE);
    group
        .as_object_mut()
        .context("host group is an object")?
        .remove("extraArgs");
    // The event coordinates, and the environment's certificate for the data
    // connection to the one NATS (docs/plan/alignment.md R2, R5).
    group["env"]
        .as_array_mut()
        .context("host group declares its environment")?
        .extend(
            [
                ("WAMN_EVT_ORG", "rc".to_owned()),
                ("WAMN_EVT_PROJECT", "app".to_owned()),
                ("WAMN_EVT_ENV", "dev".to_owned()),
                ("WAMN_EVT_STREAM_REPLICAS", source.num_replicas.to_string()),
                (
                    "WAMN_EVT_DUP_WINDOW_SECS",
                    source.duplicate_window.as_secs().to_string(),
                ),
            ]
            .map(|(name, value)| json!({"name": name, "value": value})),
        );
    base["global"]["certificates"]["dataCertSecretName"] = json!(format!(
        "wamn-nats-{}",
        wamn_control_registry::project_env_key("rc", "app", "dev")
    ));
    Ok(base)
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
    #[test]
    fn rc_host_preserves_credentials_and_native_settings() {
        let base: Value = serde_yaml::from_str(include_str!(
            "../../../../deploy/platform/values-host-default.yaml"
        ))
        .unwrap();
        let actual = host_values(
            base.clone(),
            "wamn-host:rc-test-debug",
            &source_declaration(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            actual["runtime"]["hostGroups"][0]["volumes"],
            base["runtime"]["hostGroups"][0]["volumes"]
        );
        assert_eq!(
            actual["runtime"]["hostGroups"][0]["volumeMounts"],
            base["runtime"]["hostGroups"][0]["volumeMounts"]
        );
        assert_eq!(actual["runtime"]["hostGroups"][0]["replicas"], 3);
        let old = base["runtime"]["hostGroups"][0]["env"].as_array().unwrap();
        let new = actual["runtime"]["hostGroups"][0]["env"]
            .as_array()
            .unwrap();
        // The base environment, then the five event coordinates.
        assert_eq!(old.len() + 5, new.len());
        for (a, b) in old.iter().zip(new) {
            assert_eq!(a, b);
        }
        assert_eq!(new[old.len()]["name"], "WAMN_EVT_ORG");
        assert_eq!(
            actual["global"]["certificates"]["dataCertSecretName"],
            format!(
                "wamn-nats-{}",
                wamn_control_registry::project_env_key("rc", "app", "dev")
            )
        );
    }
}

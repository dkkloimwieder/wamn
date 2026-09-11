//! The RC host keeps the production base and its declared event environment.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use async_nats::jetstream::stream;
use serde_json::{Value, json};
use wamn_control_provision::events;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::{event_broker::EventBroker, platform};

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

pub(super) async fn install(
    resources: &Resources,
    broker: &EventBroker,
    source: &stream::Config,
    server: &str,
) -> anyhow::Result<()> {
    checked(kubectl(resources).args(["create", "namespace", NAMESPACE])).await?;
    let operator = resources.work.join("operator-values.yaml");
    write_private(
        &operator,
        &serde_json::to_vec(
            &json!({"operator":{"watchNamespaces":[NAMESPACE],"hostNamespaces":[NAMESPACE],"allowSharedHosts":false}}),
        )?,
    )?;
    platform::install(
        &resources.repository,
        &resources.lifecycle,
        CLUSTER,
        &resources.work,
        NAMESPACE,
        &operator,
    )
    .await?;
    for (name, document) in [
        (
            "host-db.json",
            json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"wamn-host-db","namespace":NAMESPACE},"stringData":{"url":"postgres://postgres@127.0.0.1:1/postgres?sslmode=disable"}}),
        ),
        (
            "registry-pull.json",
            json!({"apiVersion":"v1","kind":"Secret","type":"kubernetes.io/dockerconfigjson","metadata":{"name":"wamn-registry-pull","namespace":NAMESPACE},"stringData":{".dockerconfigjson":serde_json::to_string(&json!({"auths":{"registry.wamn-system.svc.cluster.local:5000":{"username":"unused","password":"unused","auth":"dW51c2VkOnVudXNlZA=="}}}))?}}),
        ),
        (
            "event-nats.json",
            json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":{"name":"wamn-event-nats","namespace":NAMESPACE},"stringData":{
                "username":broker.runtime.username,"password":fs::read_to_string(&broker.runtime.password_file)?,"org":"rc","project":"app","environment":"dev",
                "stream_replicas":source.num_replicas.to_string(),"dup_window_secs":source.duplicate_window.as_secs().to_string(),
            }}),
        ),
        (
            "materializer-nats.json",
            json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":{"name":"wamn-materializer-nats","namespace":NAMESPACE},"stringData":{"binding.json":fs::read_to_string(&broker.binding)?}}),
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
    let rendered = host_values(base, &resources.host_image, server)?;
    let values = resources.work.join("host-values.yaml");
    write_private(&values, &serde_json::to_vec(&rendered)?)?;
    checked(
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
fn host_values(mut base: Value, image: &str, server: &str) -> anyhow::Result<Value> {
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
    let env = group["env"]
        .as_array_mut()
        .context("host base declares environment entries")?;
    let mut urls = env
        .iter_mut()
        .filter(|entry| entry["name"] == "WAMN_EVT_NATS_URL");
    let entry = urls.next().context("host base declares its event URL")?;
    entry["value"] = json!(server);
    ensure!(
        urls.next().is_none(),
        "host base declares only one event URL"
    );
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
            "nats://127.0.0.2:4222",
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
        assert_eq!(old.len(), new.len());
        for (a, b) in old.iter().zip(new) {
            if a["name"] != "WAMN_EVT_NATS_URL" {
                assert_eq!(a, b);
            }
        }
    }
}

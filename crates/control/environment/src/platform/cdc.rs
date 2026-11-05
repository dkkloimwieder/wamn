//! CDC readers use the environment's registered credentials in hosts. env
//! apply writes the CDC Secret for every installed application set.
//! An environment without that capture credential gets no reader.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::environment::document::EnvironmentDocument;
use crate::kubectl;
use wamn_release_control::release_chart;

use super::install::apply_object;

pub(super) async fn apply(
    target: &release_chart::Target,
    environments: &Path,
    directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    // The published host image carries the native reader executable.
    let image = release_chart::image_set(&directory.join("release-chart"))
        .await?
        .host;
    let mut changed = apply_object(target, &account()).await?;
    for (_, environment) in super::broker::documents(environments)? {
        let env_key = env_key(&environment);
        if kubectl::get::<Value>(
            &target.kubeconfig,
            &target.context,
            "hosts",
            "secret",
            &format!("wamn-cdc-{env_key}"),
        )
        .await?
        .is_none()
        {
            progress(&format!(
                "4.18 CDC reader {}/{}/{}: skipped: no capture credential",
                environment.org, environment.project, environment.env
            ))?;
            continue;
        }
        let object = deployment(&environment, &image);
        changed |= apply_object(target, &object).await?;
        let name = object["metadata"]["name"]
            .as_str()
            .context("CDC Deployment has no name")?;
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &[
                "rollout",
                "status",
                &format!("deployment/{name}"),
                "--timeout=180s",
            ],
            None,
        )
        .await?;
    }
    Ok(changed)
}

pub(super) fn account() -> Value {
    json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": {"name": "event-reader", "namespace": "hosts"},
        "automountServiceAccountToken": false
    })
}

fn env_key(environment: &EnvironmentDocument) -> String {
    wamn_control_registry::project_env_key(&environment.org, &environment.project, &environment.env)
}

pub(super) fn deployment(environment: &EnvironmentDocument, image: &str) -> Value {
    let env_key = env_key(environment);
    let name = format!("cdc-reader-{env_key}");
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name, "namespace": "hosts"},
        "spec": {
            "replicas": 1, "strategy": {"type": "Recreate"},
            "selector": {"matchLabels": {"app": name}},
            "template": {
                "metadata": {"labels": {"app": name}},
                "spec": {
                    "serviceAccountName": "event-reader", "automountServiceAccountToken": false,
                    "containers": [{
                        "name": "cdc-reader", "image": image,
                        "command": ["/usr/local/bin/wamn-cdc-reader"],
                        "args": [
                            format!("--org={}", environment.org),
                            format!("--project={}", environment.project),
                            format!("--env={}", environment.env),
                            "--stream-replicas=1", "--dup-window-secs=120"
                        ],
                        "env": [
                            {"name": "WAMN_CDC_URL", "valueFrom": {"secretKeyRef": {
                                "name": format!("wamn-cdc-{env_key}"), "key": "url"
                            }}},
                            {"name": "WAMN_SYSTEM_URL", "valueFrom": {"secretKeyRef": {
                                "name": format!("wamn-registry-reader-{env_key}"), "key": "url"
                            }}},
                            {"name": "WAMN_NATS_URL", "value": "nats://wamn-nats.platform.svc.cluster.local:4222"},
                            {"name": "WAMN_NATS_TLS_CA", "value": "/etc/wamn/nats/ca.crt"},
                            {"name": "WAMN_NATS_TLS_CERT", "value": "/etc/wamn/nats/tls.crt"},
                            {"name": "WAMN_NATS_TLS_KEY", "value": "/etc/wamn/nats/tls.key"}
                        ],
                        "volumeMounts": [{"name": "nats", "mountPath": "/etc/wamn/nats", "readOnly": true}],
                        "resources": {"requests": {"cpu": "50m", "memory": "64Mi"}}
                    }],
                    "volumes": [{"name": "nats", "secret": {"secretName": "wamn-cdc-reader-nats"}}]
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::manifests::already_applied;

    #[test]
    fn readers_use_environment_secret_names_and_detect_live_drift() {
        for project in ["receiving", "wms"] {
            let environment: EnvironmentDocument = serde_json::from_value(json!({
                "org": "test", "project": project, "env": "dev", "release": "none",
                "target": "cloud", "route_host": "test.example.com", "policy": "dev", "applications": {}
            }))
            .unwrap();
            let desired = deployment(&environment, "registry/host@sha256:abc");
            let pod = &desired["spec"]["template"]["spec"];
            let reader = &pod["containers"][0];
            assert_eq!(reader["image"], "registry/host@sha256:abc");
            assert_eq!(
                reader["env"][0]["valueFrom"]["secretKeyRef"]["name"],
                format!("wamn-cdc-test--{project}--dev")
            );
            assert_eq!(
                reader["env"][1]["valueFrom"]["secretKeyRef"]["name"],
                format!("wamn-registry-reader-test--{project}--dev")
            );
            assert_eq!(
                pod["volumes"][0]["secret"]["secretName"],
                "wamn-cdc-reader-nats"
            );
            assert_eq!(desired["spec"]["strategy"]["type"], "Recreate");
            let mut live = desired.clone();
            live["metadata"]["resourceVersion"] = "123".into();
            live["spec"]["template"]["spec"]["dnsPolicy"] = "ClusterFirst".into();
            assert!(already_applied(&desired, &live));
            live["spec"]["template"]["spec"]["containers"][0]["image"] = "old".into();
            assert!(!already_applied(&desired, &live));
        }
    }
}

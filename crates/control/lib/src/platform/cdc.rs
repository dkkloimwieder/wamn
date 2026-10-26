//! CDC readers use the environment's registered credentials in hosts. env
//! apply writes the CDC Secret for every installed application set.
//! An environment without that capture credential gets no reader.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::environment::document::EnvironmentDocument;
use crate::{kubectl, release_chart};

use super::install::apply_object;

pub(super) async fn apply(
    target: &release_chart::Target,
    environments: &Path,
    directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    // The stamped gates image already carries the native reader executable.
    let image = release_chart::qualification_images(&directory.join("release-chart"))
        .await?
        .gates;
    let mut changed = apply_object(target, &account()).await?;
    for (_, environment) in super::broker::documents(environments)? {
        let tenant = tenant(&environment);
        if kubectl::get::<Value>(
            &target.kubeconfig,
            &target.context,
            "hosts",
            "secret",
            &format!("wamn-cdc-{tenant}"),
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

fn account() -> Value {
    json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": {"name": "event-reader", "namespace": "hosts"},
        "automountServiceAccountToken": false
    })
}

fn tenant(environment: &EnvironmentDocument) -> String {
    wamn_control_registry::project_env_tenant(
        &environment.org,
        &environment.project,
        &environment.env,
    )
}

fn deployment(environment: &EnvironmentDocument, image: &str) -> Value {
    let tenant = tenant(environment);
    let name = format!("cdc-reader-{tenant}");
    let publisher = format!("evt-nats-{tenant}-publisher");
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
                                "name": format!("wamn-cdc-{tenant}"), "key": "url"
                            }}},
                            {"name": "WAMN_SYSTEM_URL", "valueFrom": {"secretKeyRef": {
                                "name": format!("wamn-registry-reader-{tenant}"), "key": "url"
                            }}},
                            {"name": "WAMN_EVT_NATS_URL", "value": "nats://evt-nats.platform.svc.cluster.local:4222"},
                            {"name": "WAMN_EVT_NATS_USERNAME", "valueFrom": {"secretKeyRef": {
                                "name": publisher, "key": "username"
                            }}},
                            {"name": "WAMN_EVT_NATS_PASSWORD_FILE", "value": "/etc/evt-nats/password"}
                        ],
                        "volumeMounts": [{"name": "publisher", "mountPath": "/etc/evt-nats", "readOnly": true}],
                        "resources": {"requests": {"cpu": "50m", "memory": "64Mi"}}
                    }],
                    "volumes": [{"name": "publisher", "secret": {
                        "secretName": publisher, "items": [{"key": "password", "path": "password"}]
                    }}]
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
            let desired = deployment(&environment, "registry/gates@sha256:abc");
            let pod = &desired["spec"]["template"]["spec"];
            let reader = &pod["containers"][0];
            assert_eq!(reader["image"], "registry/gates@sha256:abc");
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
                format!("evt-nats-test--{project}--dev-publisher")
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

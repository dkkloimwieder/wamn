//! Wait for configuration projected into the org control service.

use anyhow::Context as _;
use serde_json::Value;
use wamn_kubernetes::{Target, kubectl};

/// Wait until every control host observes the environment's administration login change.
pub async fn wait_administration(
    target: &Target,
    triple: &wamn_registry::Triple,
    present: bool,
    budget: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, async {
        let name = wamn_provision::control_administration_secret_name(&triple.org);
        let Some(secret) = kubectl::get::<Value>(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            "configmap",
            &name,
        )
        .await?
        else {
            return Ok(());
        };
        let key = wamn_provision::control_administration_key(&triple.project, &triple.env);
        let login = present
            .then(|| {
                secret["data"][&key]
                    .as_str()
                    .map(str::to_owned)
                    .context("administration configuration is absent")
            })
            .transpose()?;
        let path = format!("/etc/wamn/control-administration/{key}");
        let selector = format!(
            "app.kubernetes.io/name={}",
            wamn_registry::name::control_resource(&triple.org)
        );
        let script = if present {
            r#"test -r "$1" && cmp -s "$1" -"#
        } else {
            r#"test ! -e "$1""#
        };
        loop {
            let output = kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                &target.namespace,
                &["get", "pods", "-l", &selector, "-o", "json"],
                None,
            )
            .await?;
            let pods: Value = serde_json::from_str(&output).context("decode control host pods")?;
            let pods = pods["items"].as_array().context("control host pod list")?;
            let mut projected = !present || !pods.is_empty();
            for pod in pods {
                let pod = pod["metadata"]["name"]
                    .as_str()
                    .context("control host pod name")?;
                projected &= kubectl::kubectl(
                    &target.kubeconfig,
                    &target.context,
                    &target.namespace,
                    &[
                        "exec", "-i", pod, "-c", "control", "--", "sh", "-c", script, "--", &path,
                    ],
                    Some(login.as_deref().unwrap_or_default().as_bytes()),
                )
                .await
                .is_ok();
            }
            if projected {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "control administration login projection for {triple} exceeded the readiness budget"
        )
    })?
}

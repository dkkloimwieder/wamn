//! Exercise unchanged Helm application and pending-upgrade recovery.
//! The platform journey supplies the cluster. A small chart records the selected
//! digest without application images or guest artifacts.

use super::{POLL_LIMIT, Run, get, kubectl, success};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::process::Command;
use wamn_environment_control::environment::{
    Platform,
    analyse::{Analysis, Authorities, Policy, ReleaseFacts, Revision},
    document::EnvironmentDocument,
    write,
};
use wamn_release_control::release_chart::{ImageSet, release_name};

pub(super) async fn check(run: &Run) -> anyhow::Result<()> {
    let chart = run.directory.join("reapply-chart");
    std::fs::create_dir_all(chart.join("templates"))?;
    std::fs::write(
        chart.join("Chart.yaml"),
        "apiVersion: v2\nname: release\nversion: 0.1.0\n",
    )?;
    std::fs::write(
        chart.join("templates/selection.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}\ndata:\n  digest: {{ .Values.release.manifestDigest | quote }}\n",
    )?;
    let manifest: wamn_catalog::ServingManifest = serde_json::from_value(json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": []}, "components": [], "routes": [], "attachments": {},
    }))?;
    let digest = wamn_execution_contract::bytes_sha256(&manifest.canonical_bytes());
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org": run.document.org, "project": "reapply", "env": "dev", "target": "cloud", "policy": "dev",
        "applications": {"fixture": {"current": digest, "hostname": "reapply.example", "packages": ["fixture"]}}}))?)?;
    let name = release_name(&document.org, &document.project, &document.env)?;
    // The write boundary consumes the manifest and policy, not registry or DB credentials.
    let release = ReleaseFacts {
        digest,
        manifest,
        image_set: ImageSet {
            host: String::new(),
            http: String::new(),
            materializer: String::new(),
        },
        qualification: Ok(None),
        required_connections: BTreeSet::new(),
    };
    let platform = Platform {
        system_database_url: String::new(),
        target: run.target.clone(),
        chart,
        release_artifact_base: "registry.example/releases".into(),
        registry_auth_file: run.directory.join("unused-auth.json"),
        oci_ca_paths: Vec::new(),
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let mut analysis = Analysis {
        authorities: Authorities {
            release_name: name.clone(),
            connections: BTreeMap::new(),
            artifact_bindings: BTreeSet::new(),
            policy: Policy {
                name: "dev".into(),
                readiness_budget_seconds: 60,
                drain_bound_seconds: 70,
            },
            row: None,
            project: None,
            release: None,
            revision: None,
            live: BTreeMap::new(),
        },
        application: "fixture".into(),
        notices: Vec::new(),
        actor: "reapply-test".into(),
        plan: Vec::new(),
        packages: Vec::new(),
    };
    let clients = BTreeMap::new();
    write::write(&platform, &document, &analysis, &release, None, &clients).await?;
    for interrupted in [false, true] {
        let status = helm_json(run, &["status", &name, "-o", "json"]).await?;
        let version = status["version"].as_u64().context("Helm revision")?;
        let values = helm_json(run, &["get", "values", &name, "-o", "json"]).await?;
        analysis.authorities.revision = Some(Revision {
            number: version.try_into()?,
            chart: "release-0.1.0".into(),
            values,
        });
        if interrupted {
            interrupt_upgrade(run, &platform, &name).await?;
        }
        let previous = helm_json(run, &["status", &name, "-o", "json"]).await?;
        write::write(&platform, &document, &analysis, &release, None, &clients).await?;
        let after = helm_json(run, &["status", &name, "-o", "json"]).await?;
        // Recovery itself writes a rollback revision. Require an upgrade after it.
        ensure!(
            after["version"].as_u64().context("new Helm revision")?
                > previous["version"]
                    .as_u64()
                    .context("previous Helm revision")?
                    + u64::from(interrupted)
                && after["info"]["status"] == "deployed",
            "unchanged write did not complete a new Helm upgrade (interrupted={interrupted})"
        );
        let selected = get(run, "platform", "configmap", &name).await?;
        ensure!(
            selected["data"]["digest"] == release.digest,
            "write changed the selected release"
        );
    }
    success(helm(run).args(["uninstall", &name, "--wait"])).await?;
    Ok(())
}

fn helm(run: &Run) -> Command {
    let mut command = Command::new("helm");
    command
        .arg("--kubeconfig")
        .arg(&run.target.kubeconfig)
        .args([
            "--kube-context",
            &run.target.context,
            "-n",
            &run.target.namespace,
        ]);
    command
}

async fn helm_json(run: &Run, arguments: &[&str]) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &success(helm(run).args(arguments)).await?.stdout,
    )?)
}

async fn interrupt_upgrade(run: &Run, platform: &Platform, name: &str) -> anyhow::Result<()> {
    let hook_name = format!("{name}-interrupt");
    let hook = platform.chart.join("templates/interrupt.yaml");
    // An unschedulable hook pauses real Helm without downloading another image.
    std::fs::write(
        &hook,
        format!(
            "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {hook_name}\n  annotations:\n    helm.sh/hook: pre-upgrade\nspec:\n  nodeSelector:\n    wamn.test/unschedulable: 'true'\n  restartPolicy: Never\n  containers:\n    - name: wait\n      image: unavailable.invalid/wait:never\n"
        ),
    )?;
    let log = std::fs::File::create(run.directory.join("environment-interrupted-upgrade.log"))?;
    let mut child = helm(run)
        .args(["upgrade", name])
        .arg(&platform.chart)
        .args(["--reuse-values", "--wait", "--timeout", "5m"])
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let pending = tokio::time::timeout(POLL_LIMIT, async {
        loop {
            let status = helm_json(run, &["status", name, "-o", "json"]).await?;
            let pod = success(kubectl(run).args([
                "-n",
                &run.target.namespace,
                "get",
                "pod",
                &hook_name,
                "--ignore-not-found",
                "-o",
                "name",
            ]))
            .await?;
            if status["info"]["status"] == "pending-upgrade" && !pod.stdout.is_empty() {
                return Ok::<_, anyhow::Error>(());
            }
            ensure!(
                child.try_wait()?.is_none(),
                "Helm exited before interruption"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    let stopped = child.kill().await;
    std::fs::remove_file(hook)?;
    pending.context("Helm did not reach the pre-upgrade hook")??;
    stopped?;
    success(kubectl(run).args(["-n", &run.target.namespace, "delete", "pod", &hook_name])).await?;
    Ok(())
}

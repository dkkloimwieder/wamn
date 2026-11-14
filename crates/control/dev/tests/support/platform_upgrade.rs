//! One deployed upgrade case. The existing journey prepares applications and owns cleanup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_environment::platform::{
    artifact_plan::{self, SelectedChart},
    document::UpgradeTarget,
    status::{self, Category},
};

use super::{Journey, Run, data, get, observer, query, stored};

async fn release_values(
    journey: &Journey<'_>,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Value> {
    let run = journey.run;
    let output = super::success(
        tokio::process::Command::new("helm")
            .arg("--kubeconfig")
            .arg(&run.target.kubeconfig)
            .args([
                "--kube-context",
                &run.target.context,
                "get",
                "values",
                name,
                "-n",
                namespace,
                "-o",
                "json",
            ]),
    )
    .await?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub(super) fn inputs(run: &Run) -> anyhow::Result<SelectedChart> {
    let path = PathBuf::from(
        std::env::var_os("WAMN_RELEASE_CHART")
            .context("set WAMN_RELEASE_CHART to the prepared upgrade chart")?,
    );
    let selected = artifact_plan::select(&path)?;
    let original = artifact_plan::select(&run.chart)?;
    for image in ["http", "identity", "host"] {
        ensure!(
            selected
                .image(image)?
                .rsplit_once('@')
                .context("selected image digest")?
                .1
                != original
                    .image(image)?
                    .rsplit_once('@')
                    .context("original image digest")?
                    .1,
            "prepare a distinct {image} artifact before the upgrade case"
        );
    }
    Ok(selected)
}

async fn update(
    journey: &Journey<'_>,
    label: &str,
    target: &UpgradeTarget,
    chart: &Path,
) -> anyhow::Result<()> {
    let selection = match target {
        UpgradeTarget::Control => "control".to_owned(),
        UpgradeTarget::Identity => "identity".to_owned(),
        UpgradeTarget::Host { release, group } => format!("host:{release}/{group}"),
    };
    let mut command = journey.command(&["platform", "deploy"]);
    command
        .arg(journey.run.directory.join("cluster.k"))
        .arg("--chart")
        .arg(chart)
        .arg("--only")
        .arg(selection);
    journey.apply(label, &mut command).await?;
    let observed = status::read_target(&journey.run.document, &journey.run.target, target).await;
    ensure!(
        observed.category == Category::Ready,
        "{label}: the selected target is not ready"
    );
    Ok(())
}

async fn installed_chart(journey: &Journey<'_>, target: &UpgradeTarget) -> anyhow::Result<PathBuf> {
    let (name, namespace) = target.owner(&journey.run.document);
    let values = release_values(journey, namespace, &name).await?;
    Ok(values
        .pointer("/platform/chart")
        .and_then(Value::as_str)
        .map_or_else(|| journey.run.chart.clone(), PathBuf::from))
}

async fn application_target(journey: &Journey<'_>, app: &str) -> anyhow::Result<UpgradeTarget> {
    let observed = journey.status().await?;
    let release = observed["applications"][app]["observed"]["release_name"]
        .as_str()
        .context("installed application release")?;
    let values = release_values(journey, "hosts", release).await?;
    let groups = values
        .pointer("/runtime-operator/runtime/hostGroups")
        .and_then(Value::as_array)
        .context("installed host groups")?;
    ensure!(
        groups.len() == 1,
        "the fixture application must own one host group"
    );
    Ok(UpgradeTarget::Host {
        release: release.into(),
        group: groups[0]["name"]
            .as_str()
            .context("installed group name")?
            .into(),
    })
}

async fn snapshot(journey: &Journey<'_>, target: &UpgradeTarget) -> anyhow::Result<Value> {
    let run = journey.run;
    let (name, namespace) = target.owner(&run.document);
    let (deployment, selector) = match target {
        UpgradeTarget::Identity => (
            "identity".into(),
            "app.kubernetes.io/name=wamn-identity".into(),
        ),
        UpgradeTarget::Control => (
            wamn_registry::name::resource_suffix("hostgroup", &name),
            wamn_registry::name::host_selector(&name),
        ),
        UpgradeTarget::Host { group, .. } => (
            wamn_registry::name::resource_suffix("hostgroup", group),
            wamn_registry::name::host_selector(group),
        ),
    };
    let deployment = get(run, namespace, "deployment", &deployment).await?;
    let pods = get(run, namespace, "pods", &format!("-l={selector}")).await?;
    let pods: BTreeMap<_, _> = pods["items"]
        .as_array()
        .context("target pods")?
        .iter()
        .filter(|pod| pod["metadata"]["deletionTimestamp"].is_null())
        .map(|pod| {
            (
                pod["metadata"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                pod["metadata"]["uid"].clone(),
            )
        })
        .collect();
    ensure!(!pods.is_empty(), "{target:?} has no pods");
    let values = release_values(journey, namespace, &name).await?;
    let releases = helm_releases(&run.document.cluster_name, &run.directory).await?;
    let release = releases
        .iter()
        .find(|row| row["name"] == name.as_ref() && row["namespace"] == namespace)
        .context("observed Helm release")?;
    let group = match target {
        UpgradeTarget::Control => Some(name.as_ref()),
        UpgradeTarget::Host { group, .. } => Some(group.as_str()),
        UpgradeTarget::Identity => None,
    };
    let mut workloads = BTreeMap::new();
    if let Some(group) = group {
        let observed = get(run, namespace, "workloaddeployments", "").await?;
        for workload in observed["items"]
            .as_array()
            .context("target workloads")?
            .iter()
            .filter(|workload| {
                workload
                    .pointer("/spec/template/spec/hostSelector/hostgroup")
                    .and_then(Value::as_str)
                    == Some(group)
            })
        {
            workloads.insert(workload["metadata"]["name"].as_str().context("workload name")?.to_owned(),
                json!({"uid":workload["metadata"]["uid"], "generation":workload["metadata"]["generation"], "spec":workload["spec"]}));
        }
        ensure!(!workloads.is_empty(), "{target:?} has no assigned workload");
    }
    Ok(
        json!({"revision":release["revision"], "values":values, "pods":pods,
        "uid":deployment["metadata"]["uid"], "generation":deployment["metadata"]["generation"], "spec":deployment["spec"], "workloads":workloads}),
    )
}

async fn image_ready(
    journey: &Journey<'_>,
    target: &UpgradeTarget,
    image: &str,
) -> anyhow::Result<()> {
    let observed = status::read_target(&journey.run.document, &journey.run.target, target).await;
    let digest = image.rsplit_once('@').context("prepared image digest")?.1;
    let mut running = observed
        .observations
        .iter()
        .filter(|entry| !entry.draining)
        .flat_map(|entry| entry.running_images.values());
    ensure!(
        observed.category == Category::Ready
            && running.next().is_some_and(|image| image.ends_with(digest))
            && running.all(|image| image.ends_with(digest)),
        "{target:?} does not run the selected image digest"
    );
    Ok(())
}

async fn authority(journey: &Journey<'_>) -> anyhow::Result<Value> {
    let mut system = wamn_postgres::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    query(
        &mut system,
        "SELECT jsonb_build_array( \
        (SELECT jsonb_agg(xmin::text ORDER BY kid) FROM identity.session_keys), \
        (SELECT jsonb_agg(xmin::text ORDER BY issuer) FROM identity.session_signing_state), \
        (SELECT jsonb_agg(xmin::text ORDER BY principal_id) FROM identity.password_credentials))",
    )
    .await
}

pub(super) async fn exercise(
    journey: &Journey<'_>,
    artifacts: &BTreeMap<&str, String>,
) -> anyhow::Result<()> {
    let selected = inputs(journey.run)?;
    journey
        .apply(
            "provision-upgrade-environment",
            journey
                .cli(
                    &journey.run.binary.with_file_name("wamn-ctl"),
                    &["env", "apply"],
                )
                .arg(&journey.file)
                .args(["--application", "alpha"]),
        )
        .await?;
    for (app, key) in [("beta", "b0"), ("alpha", "a0")] {
        journey
            .apply(
                &format!("initialize-{app}"),
                journey
                    .command(&[
                        "schema",
                        "apply",
                        app,
                        "--release",
                        &artifacts[key],
                        "--env",
                    ])
                    .arg(&journey.file),
            )
            .await?;
        journey
            .apply(
                &format!("install-{app}"),
                &mut journey.deploy(app, &artifacts[key]),
            )
            .await?;
    }
    let rows = data(journey).await?;
    let mut database = observer(journey).await?;
    let baseline = stored(journey, &mut database).await?;
    let alpha = application_target(journey, "alpha").await?;
    let beta = application_target(journey, "beta").await?;
    let beta_owner = snapshot(journey, &beta).await?;
    let alpha_before = snapshot(journey, &alpha).await?;
    let control = UpgradeTarget::Control;
    let identity = UpgradeTarget::Identity;
    let credentials = authority(journey).await?;
    let control_chart = installed_chart(journey, &control).await?;
    let identity_chart = installed_chart(journey, &identity).await?;
    let identity_before = snapshot(journey, &identity).await?;
    let default_before = release_values(journey, "platform", "wamn").await?;
    update(journey, "upgrade-control", &control, selected.directory()).await?;
    let control_after = snapshot(journey, &control).await?;
    ensure!(
        snapshot(journey, &identity).await? == identity_before,
        "control upgrade changed identity"
    );
    ensure!(
        snapshot(journey, &alpha).await? == alpha_before
            && snapshot(journey, &beta).await? == beta_owner,
        "control upgrade changed an application"
    );
    let name = format!("c-{}-http", journey.run.document.org);
    let workload = get(journey.run, "hosts", "workloaddeployment", &name).await?;
    ensure!(
        workload["spec"]["template"]["spec"]["components"]
            .as_array()
            .context("control components")?
            .iter()
            .any(|component| component["image"]
                .as_str()
                .is_some_and(|image| image
                    .ends_with(selected.image("http").unwrap().split_once('/').unwrap().1))),
        "control did not select the prepared executable"
    );
    update(journey, "upgrade-identity", &identity, selected.directory()).await?;
    image_ready(journey, &identity, selected.image("identity")?).await?;
    ensure!(
        snapshot(journey, &control).await? == control_after,
        "identity upgrade changed control"
    );
    ensure!(
        snapshot(journey, &alpha).await? == alpha_before,
        "service upgrades changed alpha"
    );
    ensure!(
        snapshot(journey, &beta).await? == beta_owner,
        "service upgrades changed beta"
    );
    let identity_after = snapshot(journey, &identity).await?;

    let original = artifact_plan::select(&journey.run.chart)?;
    for (label, chart, image) in [
        (
            "upgrade-alpha-host",
            selected.directory(),
            selected.image("host")?,
        ),
        (
            "reselect-alpha-host",
            journey.run.chart.as_path(),
            original.image("host")?,
        ),
    ] {
        update(journey, label, &alpha, chart).await?;
        image_ready(journey, &alpha, image).await?;
        if label == "upgrade-alpha-host" {
            ensure!(
                snapshot(journey, &alpha).await?["workloads"] == alpha_before["workloads"],
                "host update changed application workloads"
            );
            journey
                .apply("reapply-alpha-code", &mut journey.reapply().await?)
                .await?;
            image_ready(journey, &alpha, image).await?;
        }
        ensure!(
            snapshot(journey, &beta).await? == beta_owner,
            "alpha host update or code reapply changed beta"
        );
        ensure!(
            snapshot(journey, &control).await? == control_after
                && snapshot(journey, &identity).await? == identity_after,
            "application operation changed a service"
        );
        ensure!(
            authority(journey).await? == credentials,
            "upgrade changed credentials or signing state"
        );
        ensure!(
            release_values(journey, "platform", "wamn").await? == default_before,
            "upgrade changed the platform default"
        );
        ensure!(
            stored(journey, &mut database).await? == baseline,
            "upgrade changed the database or schema identity"
        );
        for (app, (counter, widget)) in &rows {
            ensure!(
                journey
                    .route(app, "counter/get", &json!([{"id":counter["id"]}]))
                    .await?
                    == *counter
                    && journey
                        .route(app, "widget/get", &json!([{"id":widget["id"]}]))
                        .await?
                        == *widget,
                "{app} data changed during upgrade"
            );
        }
    }
    // Restore only the service selections changed by this case before environment cleanup.
    update(journey, "restore-control", &control, &control_chart).await?;
    update(journey, "restore-identity", &identity, &identity_chart).await?;
    Ok(())
}

/// Every Helm release in every namespace, in every state. Helm 4 has no
/// `--all`, so each state is named.
async fn helm_releases(cluster: &str, work: &Path) -> anyhow::Result<Vec<Value>> {
    let output = super::success(
        tokio::process::Command::new("helm")
            .arg("list")
            .arg("--kubeconfig")
            .arg(work.join("kubeconfig"))
            .args([
                "--kube-context",
                &format!("kind-{cluster}"),
                "--all-namespaces",
                "--deployed",
                "--failed",
                "--pending",
                "--superseded",
                "--uninstalling",
                "--uninstalled",
                "-o",
                "json",
            ]),
    )
    .await?;
    let Value::Array(releases) = serde_json::from_slice(&output.stdout)? else {
        anyhow::bail!("Helm returns a release array");
    };
    Ok(releases)
}

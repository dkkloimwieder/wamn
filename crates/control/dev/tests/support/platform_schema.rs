//! One deployed system and selected-project SQL case, with application isolation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_environment_control::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_environment_control::platform::artifact_plan::{self, SelectedChart};

use super::{Journey, Run, helm_history};

fn image_digests(
    value: &Value,
    path: &str,
    images: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                let path = format!("{path}/{name}");
                if name == "image"
                    && let Some(image) = value.as_str().filter(|s| !s.is_empty())
                {
                    images.insert(
                        path,
                        image.rsplit_once('@').context("image digest")?.1.into(),
                    );
                } else {
                    image_digests(value, &path, images)?;
                }
            }
        }
        Value::Array(entries) => {
            for (index, entry) in entries.iter().enumerate() {
                image_digests(entry, &format!("{path}/{index}"), images)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn inputs(run: &Run) -> anyhow::Result<SelectedChart> {
    let path = PathBuf::from(
        std::env::var_os("WAMN_RELEASE_CHART")
            .context("set WAMN_RELEASE_CHART to the prepared schema chart")?,
    );
    let selected = artifact_plan::select(&path)?;
    let original = artifact_plan::select(&run.chart)?;
    let mut images = BTreeMap::new();
    let mut installed = BTreeMap::new();
    for (chart, images) in [(&selected, &mut images), (&original, &mut installed)] {
        image_digests(chart.values(), "", images)?;
        images.insert(
            "/runtime-operator/runtime/image".into(),
            chart
                .image("host")?
                .rsplit_once('@')
                .context("host image digest")?
                .1
                .into(),
        );
    }
    let ctl = images
        .remove("/platform/ctl/image")
        .context("selected ctl digest")?;
    let original_ctl = installed
        .remove("/platform/ctl/image")
        .context("installed ctl digest")?;
    ensure!(
        ctl != original_ctl,
        "prepare a distinct ctl artifact before the schema case"
    );
    ensure!(
        images == installed,
        "the schema chart must preserve every other image digest"
    );
    Ok(selected)
}

async fn schema(
    journey: &Journey<'_>,
    label: &str,
    action: &str,
    scope: &str,
    chart: &Path,
) -> anyhow::Result<String> {
    let mut command = journey.command(&["platform", "schema", action, "--scope", scope]);
    command.arg("--chart").arg(chart);
    if scope == "project" {
        command.arg("--env").arg(&journey.file);
    }
    let output = journey.run(label, &mut command).await?;
    ensure!(
        output.status.success(),
        "{scope} {action} failed; see {}",
        journey.run.directory.display()
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn files<'a>(report: &'a str, section: &str) -> anyhow::Result<BTreeSet<&'a str>> {
    let mut lines = report.lines();
    let heading = format!("{section} files:");
    ensure!(
        lines.any(|line| line == heading),
        "SQL report lacks {section} files"
    );
    Ok(lines
        .take_while(|line| line.starts_with("  "))
        .map(str::trim)
        .collect())
}

async fn applications(journey: &Journey<'_>) -> anyhow::Result<Value> {
    let status = journey.status().await?;
    let mut applications = BTreeMap::new();
    for (app, selection) in status["applications"]
        .as_object()
        .context("application selections")?
    {
        let observed = &selection["observed"];
        let name = observed["release_name"]
            .as_str()
            .context("application Helm release")?;
        let helm = helm_history(journey.run, name).await?;
        let served: BTreeSet<_> = observed["live"]
            .as_object()
            .context("served releases")?
            .keys()
            .map(String::as_str)
            .collect();
        ensure!(
            served.contains(&selection["current"].as_str().context("selected release")?),
            "{app} does not serve its selected release"
        );
        applications.insert(
            app,
            json!({
                "current":selection["current"], "previous":selection["previous"],
                "hostname":selection["hostname"], "served":served,
                "release_name":name, "revision":observed["revision"],
                "helm_revision":helm["revision"], "helm_status":helm["status"]
            }),
        );
    }
    Ok(serde_json::to_value(applications)?)
}

async fn database_state(journey: &Journey<'_>) -> anyhow::Result<Value> {
    let status = journey.status().await?;
    let mut state = BTreeMap::new();
    for scope in ["system_schema", "project_schema"] {
        let report = status[scope]
            .as_object()
            .context("recorded platform SQL state")?;
        state.insert(
            scope,
            json!({
                "initialization":report["initialization"], "installed":report["installed"],
                "uncertain":report["uncertain"]
            }),
        );
    }
    Ok(serde_json::to_value(state)?)
}

pub(super) async fn exercise(journey: &Journey<'_>, release: &str) -> anyhow::Result<()> {
    let selected = inputs(journey.run)?;
    let chart = selected.directory();
    journey
        .apply("install-schema-dev", &mut journey.deploy(release))
        .await?;
    let mut document = EnvironmentDocument::compile(&journey.file)?;
    document.env = format!("isolated-{}", journey.environment);
    let application = document
        .applications
        .get_mut("alpha")
        .context("isolated application")?;
    application.current = DeclaredRelease::None;
    application.previous = None;
    application.hostname = journey.isolated_host.into();
    let file = journey
        .run
        .directory
        .join("environments/schema-isolation.k");
    std::fs::write(&file, document.to_kcl())?;
    let other = Journey {
        file,
        environment: &document.env,
        ..*journey
    };
    other
        .apply("install-schema-isolation", &mut other.deploy(release))
        .await?;
    let mut rows = Vec::new();
    let mut before = Vec::new();
    for environment in [journey, &other] {
        rows.push(
            environment
                .route(
                    "alpha",
                    "counter/create",
                    &json!([{
                        "request_id":uuid::Uuid::new_v4().to_string(),
                        "idempotency_key":uuid::Uuid::new_v4().to_string(),
                        "label":environment.environment
                    }]),
                )
                .await?,
        );
        before.push(applications(environment).await?);
    }

    let baseline = database_state(journey).await?;
    let isolated = database_state(&other).await?;
    let system = schema(journey, "system-before", "diff", "system", chart).await?;
    let pending_system = files(&system, "pending")?;
    ensure!(
        pending_system.len() == 1,
        "prepare exactly one pending system file"
    );
    ensure!(
        database_state(journey).await? == baseline && database_state(&other).await? == isolated,
        "system diff changed recorded SQL state"
    );
    schema(journey, "system-apply", "apply", "system", chart).await?;
    let system_after = schema(journey, "system-after", "diff", "system", chart).await?;
    ensure!(
        files(&system_after, "pending")?.is_empty()
            && pending_system.is_subset(&files(&system_after, "installed")?),
        "system SQL Job did not install the selected pending file"
    );

    let baseline = database_state(journey).await?;
    let isolated = database_state(&other).await?;
    let project = schema(journey, "project-before", "diff", "project", chart).await?;
    let pending_project = files(&project, "pending")?;
    ensure!(
        pending_project.len() == 1,
        "prepare exactly one pending project file"
    );
    ensure!(
        database_state(journey).await? == baseline && database_state(&other).await? == isolated,
        "project diff changed recorded SQL state"
    );
    schema(journey, "project-apply", "apply", "project", chart).await?;
    let project_after = schema(journey, "project-after", "diff", "project", chart).await?;
    ensure!(
        files(&project_after, "pending")?.is_empty()
            && pending_project.is_subset(&files(&project_after, "installed")?),
        "project SQL Job did not install the selected pending file"
    );
    let other_diff = schema(&other, "project-isolation", "diff", "project", chart).await?;
    ensure!(
        files(&other_diff, "pending")? == pending_project
            && database_state(&other).await? == isolated,
        "project SQL changed the second environment"
    );

    for ((environment, row), before) in [journey, &other].into_iter().zip(rows).zip(before) {
        ensure!(
            environment
                .route("alpha", "counter/get", &json!([{"id":row["id"]}]))
                .await?
                == row,
            "{} lost its application row",
            environment.environment
        );
        ensure!(
            applications(environment).await? == before,
            "{} changed application selections, Helm releases or served releases",
            environment.environment
        );
    }
    journey
        .apply(
            "platform-schema-status",
            journey
                .command(&["platform", "status"])
                .arg(journey.run.directory.join("cluster.k")),
        )
        .await?;
    journey.status().await?;
    other.delete_environment().await
}

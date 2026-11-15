//! One deployed system and selected-project SQL case, with application isolation.

use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_environment::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_environment::platform::artifact_plan::{self, SelectedChart};

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
    target: &str,
    chart: &Path,
) -> anyhow::Result<String> {
    let mut command = journey.command(&["platform", "schema", action, "--target", target]);
    command.arg("--chart").arg(chart);
    if target == "project" {
        command.arg("--env").arg(&journey.file);
    }
    let output = journey.run(label, &mut command).await?;
    ensure!(
        output.status.success(),
        "{target} {action} failed; see {}",
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
                "current":selection["current"],
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
    for target in ["system_schema", "project_schema"] {
        let report = status[target]
            .as_object()
            .context("recorded platform SQL state")?;
        state.insert(
            target,
            json!({
                "initialization":report["initialization"], "installed":report["installed"],
                "uncertain":report["uncertain"]
            }),
        );
    }
    Ok(serde_json::to_value(state)?)
}

async fn relations(
    databases: &mut [sqlx::PgConnection; 3],
) -> anyhow::Result<[BTreeSet<String>; 3]> {
    let mut sets = std::array::from_fn(|_| BTreeSet::new());
    for (database, set) in databases.iter_mut().zip(&mut sets) {
        *set = sqlx::query(
            "SELECT format('%I.%I', n.nspname, c.relname)
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.relkind IN ('r', 'p', 'v', 'm')
                   AND n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
                   AND c.relname <> 'schema_migration'",
        )
        .fetch_all(&mut *database)
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    }
    Ok(sets)
}

async fn database_effect(
    databases: &mut [sqlx::PgConnection; 3],
    before: &[BTreeSet<String>; 3],
    applied: usize,
) -> anyhow::Result<()> {
    let after = relations(databases).await?;
    for (index, (before, after)) in before.iter().zip(&after).enumerate() {
        if index != applied {
            ensure!(
                before == after,
                "SQL apply changed database {index} relations"
            );
            continue;
        }
        let mut added = after.difference(before);
        let table = added.next().context("SQL apply created no probe table")?;
        ensure!(
            added.next().is_none() && before.is_subset(after),
            "SQL apply must add exactly one table in database {index}: {before:?} -> {after:?}"
        );
        let database = &mut databases[index];
        ensure!(
            sqlx::query(
                "SELECT relkind IN ('r', 'p') FROM pg_class WHERE oid = $1::text::regclass"
            )
            .bind(table)
            .fetch_one(&mut *database)
            .await?
            .get::<bool, _>(0),
            "SQL apply created {table}, which is not a table"
        );
        let count: i64 = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&mut *database)
            .await?
            .get(0);
        ensure!(
            count == 1,
            "SQL probe {table} has {count} rows instead of one"
        );
    }
    Ok(())
}

pub(super) async fn exercise(journey: &Journey<'_>, release: &str) -> anyhow::Result<()> {
    let selected = inputs(journey.run)?;
    let chart = selected.directory();
    let mut document = EnvironmentDocument::compile(&journey.file)?;
    document.env = format!("isolated-{}", journey.environment);
    let application = document
        .applications
        .get_mut("alpha")
        .context("isolated application")?;
    application.current = DeclaredRelease::None;
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
    for environment in [journey, &other] {
        environment
            .apply(
                "provision-schema-environment",
                environment
                    .cli(
                        &environment.run.binary.with_file_name("wamn-ctl"),
                        &["env", "apply"],
                    )
                    .arg(&environment.file)
                    .args(["--application", "alpha"]),
            )
            .await?;
        environment
            .apply(
                "initialize-schema-application",
                &mut super::schema_command(environment, "apply", Some(release)),
            )
            .await?;
        environment
            .apply(
                "install-schema-application",
                &mut environment.deploy("alpha", release),
            )
            .await?;
    }
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

    let mut system_database =
        wamn_postgres::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system_database)
        .await
        .map(drop)?;
    let mut databases = [
        system_database,
        super::observer(journey).await?,
        super::observer(&other).await?,
    ];
    let baseline = database_state(journey).await?;
    let isolated = database_state(&other).await?;
    let mut system = schema(journey, "system-before", "diff", "system", chart).await?;
    let pending_system = files(&mut system, "pending")?;
    ensure!(
        pending_system.len() == 1,
        "prepare exactly one pending system file"
    );
    ensure!(
        database_state(journey).await? == baseline && database_state(&other).await? == isolated,
        "system diff changed recorded SQL state"
    );
    let relations_before = relations(&mut databases).await?;
    schema(journey, "system-apply", "apply", "system", chart).await?;
    database_effect(&mut databases, &relations_before, 0).await?;
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
    let relations_before = relations(&mut databases).await?;
    schema(journey, "project-apply", "apply", "project", chart).await?;
    database_effect(&mut databases, &relations_before, 1).await?;
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

//! Observe shared reset through the existing provisioning and schema commands.
//!
//! Stale-event admission after a schema change has no test: deployed environments
//! have no workflow authoring operation (wamn-alrvy.19). Event disposal and isolation
//! remain observable through the deployed broker.

use sqlx::Row as _;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_environment_control::environment::document::{
    DeclaredRelease, EnvironmentDocument, SchemaMode,
};

use super::{
    Journey, POLL_LIMIT, ProcessGroup, get, observer, runtime, same_database, schema_command,
    stored,
};

pub(super) async fn designate(journey: &Journey<'_>) -> anyhow::Result<()> {
    let binary = journey.run.binary.with_file_name("wamn-ctl");
    journey
        .apply(
            "designate-disposable",
            journey
                .cli(
                    &binary,
                    &[
                        "provision-project-env",
                        "--org",
                        &journey.run.document.org,
                        "--project",
                        journey.project,
                        "--env",
                        journey.environment,
                        "--target",
                        "cloud",
                        "--disposable",
                        "--cluster-namespace",
                        "platform",
                        "--namespace",
                        "hosts",
                        "--secret-namespace",
                        "hosts",
                        "--emit-database",
                        "-",
                        "--emit-secret",
                    ],
                )
                .arg(journey.run.directory.join("disposable-credential.json")),
        )
        .await?;
    ensure!(
        journey.status().await?["schema"]["disposable"] == true,
        "explicit provisioning did not record disposable authorization"
    );
    Ok(())
}

// Applying published SQL uses the same CNPG reset as explicit reset.
pub(super) async fn published(
    journey: &Journey<'_>,
    candidate: &str,
    rows: &mut BTreeMap<String, (Value, Value)>,
) -> anyhow::Result<()> {
    let mut database = observer(journey).await?;
    let before = stored(journey, &mut database).await?;
    let selections = journey.status().await?;
    drop(database);
    attempt(
        journey,
        "reset-from-release",
        &mut schema_command(journey, "apply", Some(candidate)),
        Stop::Complete,
        true,
    )
    .await?;
    let mut database = observer(journey).await?;
    let after = stored(journey, &mut database).await?;
    recreated(&before, &after)?;
    ensure!(
        after["fingerprint"] != before["fingerprint"],
        "published reset retained the old schema"
    );
    removed(&mut database, rows).await?;
    ensure!(sqlx::query("SELECT EXISTS(SELECT FROM pg_index WHERE indexrelid=to_regclass('alpha_data.widget_note_idx') AND indisvalid)").fetch_one(&mut database).await?.get::<bool, _>(0), "reset omitted the selected published index");
    let status = journey.status().await?;
    for (app, selection) in selections["applications"]
        .as_object()
        .context("canonical application selections")?
    {
        ensure!(
            status["applications"][app]["current"] == selection["current"]
                && status["applications"][app]["previous"] == selection["previous"],
            "schema reset changed {app}'s code selections"
        );
    }
    journey
        .apply("deploy-reset-schema-code", &mut journey.deploy(candidate))
        .await?;
    refill(journey, rows).await
}

fn recreated(before: &Value, after: &Value) -> anyhow::Result<()> {
    ensure!(
        before["cluster"] == after["cluster"],
        "reset replaced the CNPG Cluster"
    );
    ensure!(
        before["database"][1] == after["database"][1],
        "reset changed the database name"
    );
    ensure!(
        before["database"][0] != after["database"][0]
            && before["initialization"] != after["initialization"],
        "reset did not create a fresh database initialization"
    );
    Ok(())
}

async fn refill(
    journey: &Journey<'_>,
    rows: &mut BTreeMap<String, (Value, Value)>,
) -> anyhow::Result<()> {
    for (app, row) in rows.iter_mut() {
        let counter = journey
            .route(
                app,
                "counter/create",
                &json!([{
                    "request_id":uuid::Uuid::new_v4().to_string(),
                    "idempotency_key":uuid::Uuid::new_v4().to_string(), "label":app
                }]),
            )
            .await?;
        let widget = journey.route(app, "widget/create", &json!([{
            "request_id":uuid::Uuid::new_v4().to_string(),
            "idempotency_key":uuid::Uuid::new_v4().to_string(),
            "id":uuid::Uuid::new_v4().to_string(), "code":"standard", "maker_id":null,"note":app
        }])).await?;
        *row = (counter, widget);
    }
    Ok(())
}

async fn removed(
    database: &mut sqlx::PgConnection,
    rows: &BTreeMap<String, (Value, Value)>,
) -> anyhow::Result<()> {
    for (app, (_, widget)) in rows {
        let id = widget["id"].as_str().context("witness widget id")?;
        let exists: bool = sqlx::query(sqlx::AssertSqlSafe(
            (&format!("SELECT EXISTS(SELECT FROM {app}_data.widget WHERE id=$1::text::uuid)"))
                .to_string(),
        ))
        .bind(&id)
        .fetch_one(&mut *database)
        .await?
        .get(0);
        ensure!(!exists, "shared reset retained {app}'s old row");
    }
    Ok(())
}

// The baseline alone precedes application SQL. Every pinned package must commit
// its recipe before the restored schema is an interruption point.
async fn restored_schema(
    database: &mut sqlx::PgConnection,
    pending: &Value,
) -> anyhow::Result<Option<Value>> {
    if !sqlx::query(
        "SELECT to_regclass('platform.schema_baseline') IS NOT NULL \
             AND to_regclass('catalog.packages') IS NOT NULL \
             AND to_regclass('alpha_data.widget_note_idx') IS NOT NULL",
    )
    .fetch_one(&mut *database)
    .await?
    .get::<bool, _>(0)
    {
        return Ok(None);
    }
    let Some(row) = sqlx::query("SELECT jsonb_build_object(\
                'initialization',installed_at::text,\
                'index_oid',(SELECT indexrelid::bigint FROM pg_index \
                    WHERE indexrelid=to_regclass('alpha_data.widget_note_idx') AND indisvalid),\
                'packages',(SELECT coalesce(jsonb_object_agg(package_id,physical_schema_fingerprint),'{}'::jsonb) FROM catalog.packages)\
             ) FROM platform.schema_baseline WHERE singleton").fetch_optional(&mut *database)
        .await?
    else {
        return Ok(None);
    };
    let observed: Value = row.get(0);
    let sources = pending["inputs"]["sources"]
        .as_object()
        .context("pinned schema sources")?;
    let installed = observed["packages"]
        .as_object()
        .context("installed package fingerprints")?;
    if observed["initialization"] != pending["initialization_identity"]
        || observed["index_oid"].is_null()
        || installed.len() != sources.len()
        || sources
            .iter()
            .any(|(id, source)| installed.get(id) != source.get("fingerprint"))
    {
        return Ok(None);
    }
    Ok(Some(observed))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Complete,
    Pinned,
    Restored,
}

// Interruption targets durable product state, never command-message order.
async fn attempt(
    journey: &Journey<'_>,
    label: &str,
    command: &mut Command,
    stop: Stop,
    expect_recreation: bool,
) -> anyhow::Result<Value> {
    let before = journey.status().await?;
    let name = before["schema"]["database_instance"]
        .as_str()
        .context("database name")?;
    let resource = get(journey.run, "platform", "database", name).await?;
    let mut system = wamn_postgres::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    let started = Instant::now();
    command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(
            journey.run.directory.join(format!("{label}.stdout")),
        )?)
        .stderr(std::fs::File::create(
            journey.run.directory.join(format!("{label}.stderr")),
        )?);
    let mut child = command.spawn()?;
    let group = ProcessGroup(
        rustix::process::Pid::from_raw(i32::try_from(child.id().context("reset PID")?)?)
            .context("positive reset PID")?,
    );
    let observed = tokio::time::timeout(super::super::COMMAND_LIMIT, async {
        let mut absent = false;
        let mut present = false;
        let mut pending = Value::Null;
        loop {
            if let Some(exit) = child.try_wait()? {
                ensure!(stop == Stop::Complete, "{label}: reset exited before its interruption point");
                ensure!(exit.success(), "{label}: reset failed; see {}", journey.run.directory.display());
                ensure!(!expect_recreation || (absent && present), "{label}: CNPG absent then present was not observed");
                break Ok::<_, anyhow::Error>(pending);
            }
            let found = get(journey.run, "platform", "database", name).await?;
            ensure!(found["metadata"]["uid"] == resource["metadata"]["uid"] && found["spec"]["name"] == name,
                "reset replaced the Database resource or changed its name");
            if found["spec"]["ensure"] == "absent" {
                ensure!(expect_recreation, "retry destroyed an already initialized database");
                if !absent {
                    stopped(journey, &before).await?;
                    eprintln!("{label} CNPG absent: {found}");
                }
                absent = true;
            } else if absent && !present && found["spec"]["ensure"] == "present" {
                eprintln!("{label} CNPG present: {found}");
                present = true;
            }
            pending = sqlx::query("SELECT pending_database FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3").bind(&journey.run.document.org).bind(&journey.project).bind(&journey.environment).fetch_one(&mut system).await?.get::<Option<Value>, _>(0).unwrap_or(Value::Null);
            if !pending.is_null() && stop == Stop::Pinned {
                eprintln!("{label} pinned reset: {pending}");
                break Ok(pending);
            }
            if !pending.is_null() && present && stop == Stop::Restored
                && let Ok(mut database) = observer(journey).await
                && let Some(schema) = restored_schema(&mut database, &pending).await?
            {
                eprintln!("{label} restored schema: {schema}");
                stopped(journey, &before).await?;
                break Ok(pending);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await;
    if stop != Stop::Complete || !matches!(&observed, Ok(Ok(_))) {
        let _ = rustix::process::kill_process_group(group.0, rustix::process::Signal::KILL);
    }
    let exited = child.wait().await;
    drop(group);
    eprintln!("{label}: {:.2}s", started.elapsed().as_secs_f64());
    let pending = observed.with_context(|| format!("{label}: reset observation timed out"))??;
    if stop != Stop::Complete {
        ensure!(!exited?.success(), "interrupted reset returned success");
        std::fs::write(
            journey.run.directory.join(format!("{label}.pending.json")),
            serde_json::to_vec_pretty(&pending)?,
        )?;
    }
    Ok(pending)
}

async fn stopped(journey: &Journey<'_>, status: &Value) -> anyhow::Result<()> {
    let document = EnvironmentDocument::compile(&journey.file)?;
    let shared = wamn_control_provision::environment_names::environment_resource_name(
        &document.org,
        &document.project,
        &document.env,
    )?;
    let mut names = vec![
        format!("hostgroup-{shared}"),
        wamn_control_provision::project_env_cdc_secret_name(
            &document.org,
            &document.project,
            &document.env,
        ),
    ];
    for app in status["applications"]
        .as_object()
        .context("applications")?
        .values()
    {
        names.push(format!(
            "hostgroup-{}",
            app["observed"]["release_name"]
                .as_str()
                .context("host group")?
        ));
    }
    for name in names {
        let deployment = get(journey.run, "hosts", "deployment", &name).await?;
        ensure!(
            deployment["spec"]["replicas"] == 0
                && deployment["status"]["readyReplicas"].as_u64().unwrap_or(0) == 0,
            "database writer {name} remained ready during reset"
        );
        eprintln!("stopped writer: {deployment}");
    }
    Ok(())
}

pub(super) async fn recovery(
    journey: &Journey<'_>,
    digest: &str,
    capture: &str,
    rows: &mut BTreeMap<String, (Value, Value)>,
) -> anyhow::Result<()> {
    eprintln!(
        "scenario 5: reset interruption before recreation and after restored SQL; isolate a non-disposable environment"
    );
    let mut document = EnvironmentDocument::compile(&journey.file)?;
    document.env = format!("isolated-{}", journey.environment);
    document.schema.mode = Some(SchemaMode::Migrate);
    document.integrations.clear();
    document.applications.retain(|name, _| name == "beta");
    let beta = document
        .applications
        .get_mut("beta")
        .context("isolated beta")?;
    beta.current = DeclaredRelease::None;
    beta.previous = None;
    beta.hostname = journey.isolated_host.into();
    let file = journey.run.directory.join("environments/isolation.k");
    std::fs::write(&file, document.to_kcl())?;
    let other = Journey {
        run: journey.run,
        file,
        project: journey.project,
        environment: &document.env,
        isolated_host: journey.isolated_host,
        variables: journey.variables,
        token: journey.token,
        client: journey.client,
        edge: journey.edge,
    };
    other
        .apply(
            "provision-isolation",
            other
                .cli(
                    &other.run.binary.with_file_name("wamn-ctl"),
                    &["env", "apply"],
                )
                .arg(&other.file)
                .args(["--application", "beta"]),
        )
        .await?;
    other
        .apply(
            "install-isolation-schema",
            other
                .command(&["schema", "apply", "beta", "--release", digest, "--env"])
                .arg(&other.file),
        )
        .await?;
    other
        .apply("install-isolation", &mut other.deploy(digest))
        .await?;
    ensure!(
        other.status().await?["schema"]["disposable"] == false,
        "isolation environment must remain non-disposable"
    );
    let mut database = observer(journey).await?;
    let before = stored(journey, &mut database).await?;
    let mut other_database = observer(&other).await?;
    let other_runtime = runtime(&other, "beta").await?;
    let broker = broker(journey).await?;
    let isolated_event = event(&other, &broker, "counter", "isolated-before-reset").await?;
    other
        .apply(
            "add-isolation-schema",
            other
                .command(&["schema", "apply", "beta", "--release", capture, "--env"])
                .arg(&other.file),
        )
        .await?;
    ensure!(
        runtime(&other, "beta").await? == other_runtime,
        "schema apply changed the serving application"
    );
    other
        .apply("deploy-isolation-capture", &mut other.deploy(capture))
        .await?;
    event(&other, &broker, "capture_counter", "added-schema").await?;
    let isolated = stored(&other, &mut other_database).await?;
    let other_runtime = runtime(&other, "beta").await?;
    let old_event = event(journey, &broker, "counter", "old-before-reset").await?;
    // The isolation target also observes item 4's refusal without another environment.
    let refused = other
        .run(
            "durable-reset-refused",
            other
                .command(&["schema", "reset", "--env"])
                .arg(&other.file),
        )
        .await?;
    ensure!(
        !refused.status.success(),
        "reset accepted a target without disposable authorization"
    );
    ensure!(
        stored(&other, &mut other_database).await? == isolated,
        "refused reset changed the durable database"
    );
    ensure!(
        runtime(&other, "beta").await? == other_runtime,
        "refused reset changed the durable workload"
    );
    let mut reset = journey.command(&["schema", "reset", "--env"]);
    reset.arg(&journey.file);
    let pending = attempt(
        journey,
        "interrupt-reset-before-cnpg",
        &mut reset,
        Stop::Pinned,
        true,
    )
    .await?;
    let mut database = observer(journey).await?;
    ensure!(
        stored(journey, &mut database).await? == before,
        "pre-CNPG interruption already destroyed the old database"
    );
    for (app, (_, widget)) in rows.iter() {
        let id = widget["id"].as_str().context("old widget")?;
        ensure!(
            sqlx::query(sqlx::AssertSqlSafe(
                (&format!("SELECT EXISTS(SELECT FROM {app}_data.widget WHERE id=$1::text::uuid)"))
                    .to_string()
            ))
            .bind(&id)
            .fetch_one(&mut database)
            .await?
            .get::<bool, _>(0),
            "pre-CNPG interruption lost {app}'s old row"
        );
    }
    let selections = journey.status().await?;
    let mut reset = journey.command(&["schema", "reset", "--env"]);
    reset.arg(&journey.file);
    attempt(
        journey,
        "retry-reset-before-cnpg",
        &mut reset,
        Stop::Complete,
        true,
    )
    .await?;
    let mut database = observer(journey).await?;
    let after = stored(journey, &mut database).await?;
    recreated(&before, &after)?;
    ensure!(
        after["initialization"] == pending["initialization_identity"],
        "reset retry changed its pinned initialization"
    );
    for app in selections["applications"]
        .as_object()
        .context("selected applications")?
        .keys()
    {
        let status = journey.status().await?;
        ensure!(
            status["applications"][app]["current"] == selections["applications"][app]["current"]
                && status["applications"][app]["previous"]
                    == selections["applications"][app]["previous"],
            "reset retry rotated {app}'s selections"
        );
    }
    removed(&mut database, rows).await?;
    let stream = broker.get_stream(&old_event.stream).await?;
    let missing = stream
        .get_raw_message(old_event.message.sequence)
        .await
        .expect_err("old event survived the reset");
    ensure!(
        missing.kind() == async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound,
        "old-event observation failed for a reason other than disposal: {missing}"
    );
    refill(journey, rows).await?;
    let mut reset = journey.command(&["schema", "reset", "--env"]);
    reset.arg(&journey.file);
    let pending = attempt(
        journey,
        "interrupt-reset-after-sql",
        &mut reset,
        Stop::Restored,
        true,
    )
    .await?;
    let mut database = observer(journey).await?;
    let restored = stored(journey, &mut database).await?;
    let restored_sql = restored_schema(&mut database, &pending)
        .await?
        .context("committed reset schema")?;
    removed(&mut database, rows).await?;
    ensure!(
        journey.status().await?["schema"]["pending_database"] == pending,
        "missed the interruption window: reset completed before the process stopped"
    );
    ensure!(
        restored["initialization"] == pending["initialization_identity"],
        "restored database does not carry its pinned identity"
    );
    drop(database);
    let mut reset = journey.command(&["schema", "reset", "--env"]);
    reset.arg(&journey.file);
    attempt(
        journey,
        "retry-reset-after-sql",
        &mut reset,
        Stop::Complete,
        false,
    )
    .await?;
    let mut database = observer(journey).await?;
    let completed = stored(journey, &mut database).await?;
    same_database(&restored, &completed)?;
    ensure!(
        completed["fingerprint"] == before["fingerprint"]
            && restored_schema(&mut database, &pending).await? == Some(restored_sql),
        "retry omitted installed SQL or replaced the committed schema"
    );
    removed(&mut database, rows).await?;
    let status = journey.status().await?;
    ensure!(
        status["schema"]["pending_database"].is_null(),
        "reset retry left pinned work unfinished"
    );
    for app in selections["applications"]
        .as_object()
        .context("selected applications")?
        .keys()
    {
        ensure!(
            status["applications"][app]["current"] == selections["applications"][app]["current"]
                && status["applications"][app]["previous"]
                    == selections["applications"][app]["previous"],
            "restored retry rotated {app}'s selections"
        );
    }
    let preserved = broker
        .get_stream(&isolated_event.stream)
        .await?
        .get_raw_message(isolated_event.message.sequence)
        .await?;
    ensure!(
        preserved.payload == isolated_event.message.payload,
        "reset discarded or changed another environment's event"
    );
    let row = other
        .route(
            "beta",
            "counter/get",
            &json!([{"id":isolated_event.row["id"]}]),
        )
        .await?;
    ensure!(
        row == isolated_event.row
            && stored(&other, &mut other_database).await? == isolated
            && runtime(&other, "beta").await? == other_runtime,
        "reset changed the other environment's data, database or workload"
    );
    refill(journey, rows).await?;
    event(journey, &broker, "counter", "new-after-reset").await?;
    drop(other_database);
    other.delete_environment().await
}

async fn broker(journey: &Journey<'_>) -> anyhow::Result<async_nats::jetstream::Context> {
    let nats = wamn_broker::nats::PlatformNats {
        nats_url: journey.variables["WAMN_NATS_URL"].clone(),
        nats_tls_ca: journey.variables["WAMN_NATS_TLS_CA"].as_str().into(),
        nats_tls_cert: journey.variables["WAMN_NATS_TLS_CERT"].as_str().into(),
        nats_tls_key: journey.variables["WAMN_NATS_TLS_KEY"].as_str().into(),
        nats_tls_server_name: None,
    };
    Ok(async_nats::jetstream::new(nats.connect().await?))
}

struct Event {
    stream: String,
    message: async_nats::jetstream::message::StreamMessage,
    row: Value,
}

// Read a real CDC event produced by an application write. No synthetic publisher or consumer.
async fn event(
    journey: &Journey<'_>,
    broker: &async_nats::jetstream::Context,
    model: &str,
    label: &str,
) -> anyhow::Result<Event> {
    let document = EnvironmentDocument::compile(&journey.file)?;
    let name = wamn_event_wire::stream_name(&document.org, &document.project, &document.env);
    let mut stream = broker.get_stream(&name).await?;
    let mut sequence = stream.info().await?.state.last_sequence + 1;
    let row = journey
        .route(
            "beta",
            &format!("{model}/create"),
            &json!([{
                "request_id":uuid::Uuid::new_v4().to_string(),
                "idempotency_key":uuid::Uuid::new_v4().to_string(),"label":label
            }]),
        )
        .await?;
    let message = tokio::time::timeout(POLL_LIMIT, async {
        loop {
            let last = stream.info().await?.state.last_sequence;
            while sequence <= last {
                let message = stream.get_raw_message(sequence).await?;
                let envelope: Value = serde_json::from_slice(&message.payload)?;
                if envelope["package_id"] == "beta" && envelope["new"]["label"] == label {
                    return Ok::<_, anyhow::Error>(message);
                }
                sequence += 1;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("application write produced no CDC event")??;
    eprintln!(
        "CDC event {name}/{}: {}",
        message.sequence,
        String::from_utf8_lossy(&message.payload)
    );
    Ok(Event {
        stream: name,
        message,
        row,
    })
}

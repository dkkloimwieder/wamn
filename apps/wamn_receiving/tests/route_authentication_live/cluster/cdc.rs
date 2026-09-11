//! Receiving CDC provisioning through the existing control library.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use wamn_cdc_reader::EventReaderArgs;
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_ctl::dev::environment::{
    ProvisionedRoute, connect, generation_args, read_json, secret_value,
};
use wamn_ctl::enable_cdc_project_env::EnableCdcProjectEnvArgs;
use wamn_gate_harness::journey::JourneyDocument;

use super::super::{ENVIRONMENT, ORG, PROJECT};

pub(super) async fn configure(
    inputs: &JourneyDocument,
    route: &ProvisionedRoute,
    work: &Path,
    replication_password: &str,
    nats_url: &str,
    nats_username: String,
    nats_password_file: PathBuf,
    provisioning: &wamn_test_infrastructure::event_broker::Credentials,
    consumers: &[async_nats::jetstream::consumer::pull::Config],
    source: &async_nats::jetstream::stream::Config,
) -> anyhow::Result<EventReaderArgs> {
    let mut admin_url = reqwest::Url::parse(&route.database_url)?;
    let database_host = admin_url
        .host_str()
        .context("the Receiving database has a TCP host")?
        .to_owned();
    admin_url.set_path("/postgres");
    let cdc_secret = work.join("cdc-reader.json");
    let registry_secret = work.join("registry-reader.json");
    let (admin, admin_task) = connect(admin_url.as_str()).await?;
    let (project, project_task) = connect(&route.database_url).await?;
    let configured = wamn_gate_harness::environment::configure_cdc(
        EnableCdcProjectEnvArgs {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            env: ENVIRONMENT.to_owned(),
            schema: "receiving".to_owned(),
            system_database_url: Some(inputs.system_pg_url.clone()),
            cluster: Some("route-auth-pg18".to_owned()),
            replication_password: replication_password.to_owned(),
            db_host: Some(database_host.clone()),
            db_port: 5432,
            namespace: inputs.host_secret_namespace.clone(),
            secret_namespace: Some(inputs.host_secret_namespace.clone()),
            stream: None,
            nats_url: nats_url.to_owned(),
            nats_username: provisioning.username.clone(),
            nats_password_file: provisioning.password_file.clone(),
            stream_replicas: source.num_replicas,
            dup_window_secs: source.duplicate_window.as_secs(),
            consumer_config: consumers
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<_, _>>()?,
            emit_role_sql: Some(work.join("cdc-role.sql")),
            emit_cdc_sql: Some(work.join("cdc.sql")),
            emit_secret: Some(cdc_secret.clone()),
        },
        admin.as_ref(),
        project.as_ref(),
    )
    .await;
    project_task.abort();
    admin_task.abort();
    configured?;
    let mut generation = generation_args(
        WorkloadRoleFamily::RegistryReader,
        &inputs.system_pg_url,
        None,
        &registry_secret,
    );
    generation.namespace = inputs.host_secret_namespace.clone();
    wamn_ctl::provision_project_env::run(generation).await?;
    for path in [&cdc_secret, &registry_secret] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o777 == 0o600,
            "the reader credential must have mode 0600"
        );
        let secret = read_json(path)?;
        let url = reqwest::Url::parse(
            secret["stringData"]["url"]
                .as_str()
                .context("the reader credential has a database URL")?,
        )?;
        ensure!(
            secret["kind"] == "Secret"
                && secret["type"] == "Opaque"
                && url.host_str() == Some(database_host.as_str())
                && url.port() == Some(5432),
            "the reader credential must select the owned PostgreSQL endpoint"
        );
    }
    Ok(EventReaderArgs {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        system_database_url: secret_value(&registry_secret, "url")?,
        cdc_url: secret_value(&cdc_secret, "url")?,
        nats_url: nats_url.to_owned(),
        nats_username: Some(nats_username),
        nats_password_file: Some(nats_password_file),
        sslmode: "disable".to_owned(),
        stream_replicas: source.num_replicas,
        dup_window_secs: source.duplicate_window.as_secs(),
        feedback_secs: 1,
        stall_threshold_secs: 30,
        slot_poll_secs: 0,
        slot_safe_wal_warn_bytes: 268_435_456,
    })
}

/// Start the production reader with the same shutdown token used by its binary.
pub(super) fn start(
    args: EventReaderArgs,
    log: &Path,
) -> anyhow::Result<(
    pg_walstream::CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    use tracing::instrument::WithSubscriber as _;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(fs::File::create(log)?)
        .finish();
    let cancellation = pg_walstream::CancellationToken::new();
    let task = tokio::spawn(
        wamn_cdc_reader::run_with_token(args, cancellation.clone()).with_subscriber(subscriber),
    );
    Ok((cancellation, task))
}

/// Keep the reader's session assertion and inspect only its two named streams.
pub(super) async fn ready(
    task: &tokio::task::JoinHandle<anyhow::Result<()>>,
    observer: &async_nats::Client,
    evidence: &Path,
) -> anyhow::Result<()> {
    use super::super::MATERIALIZER_STREAM;
    use std::time::{Duration, Instant};
    let jetstream = async_nats::jetstream::new(observer.clone());
    let advisory_name = wamn_event_wire::delivery_advisory_stream(MATERIALIZER_STREAM);
    let log = evidence.join("cdc-reader.log");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        ensure!(
            !task.is_finished(),
            "the production CDC reader stopped before stream readiness"
        );
        if let (Ok(mut source), Ok(mut advisories)) = (
            jetstream.get_stream(MATERIALIZER_STREAM).await,
            jetstream.get_stream(&advisory_name).await,
        ) {
            let reader_log = fs::read_to_string(&log)?;
            if reader_log.contains("walsender session open; draining") {
                let source_info = source.info().await?;
                let advisory_info = advisories.info().await?;
                ensure!(
                    source_info.config.name == MATERIALIZER_STREAM
                        && advisory_info.config.name == advisory_name,
                    "the reader must use its declared event and advisory streams"
                );
                let observed = |info: &async_nats::jetstream::stream::Info| {
                    serde_json::json!({
                        "config":info.config,
                        "state":{"messages":info.state.messages,"bytes":info.state.bytes,
                            "first_seq":info.state.first_sequence,"last_seq":info.state.last_sequence,
                            "consumer_count":info.state.consumer_count},
                    })
                };
                fs::write(
                    evidence.join("nats-reader-ready.json"),
                    serde_json::to_vec_pretty(
                        &serde_json::json!({"source":observed(source_info),"advisories":observed(advisory_info)}),
                    )?,
                )?;
                fs::write(
                    evidence.join("cdc-reader-ready.log"),
                    reader_log
                        .lines()
                        .filter(|line| line.contains("walsender session open; draining"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )?;
                return Ok(());
            }
        }
        ensure!(
            Instant::now() < deadline,
            "the production CDC reader did not open its walsender session"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

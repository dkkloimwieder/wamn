//! RC environment setup and the retained native socket and trace tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control_provision::events;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::{event_broker, rendering, workload};

mod bootstrap;
mod deployment;
mod jobs;
mod resources;

use resources::Resources;

const CLUSTER: &str = "wamn-rc";
const NAMESPACE: &str = "wamn-rc";
const TENANT: &str = "00000000-0000-0000-0000-000000000001";

/// Run the RC on its explicitly owned disposable cluster.
#[derive(Args, Debug)]
pub struct RcArgs {
    /// Create the declared environment and run its native tests.
    #[arg(long)]
    pub apply: bool,
    /// New result directory under the main repository's evidence directory.
    #[arg(long, required_if_eq("apply", "true"))]
    pub evidence_dir: Option<PathBuf>,
}

/// Plan or execute the retained RC setup, bootstrap and two native Jobs.
pub async fn run(args: RcArgs) -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("integration crate has a repository parent")?;
    let source = source_head(repository).await?;
    let declaration = deployment::source_declaration(repository)?;
    if !args.apply {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source":source,"cluster":CLUSTER,"context":"kind-wamn-rc",
                "namespace":NAMESPACE,"build_profile":"debug",
                "event_stream":declaration,
                "order":["setup","bootstrap","M2","socket-test","trace-test","cleanup"],
                "deferred":["wamn-0h0g.15.153"],"deferred_disposition":"post-merge; not claimed",
            }))?
        );
        return Ok(());
    }
    let evidence = args
        .evidence_dir
        .context("--apply requires --evidence-dir")?;
    let mut resources = match resources::prepare(repository, &evidence, &source).await {
        Ok(resources) => resources,
        Err(error) => clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("{error:#}"),
        )
        .exit(),
    };
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut interrupted = false;
    let result = tokio::select! {
        result = execute(&mut resources, declaration) => result,
        _ = interrupt.recv() => { interrupted = true; Err(anyhow::anyhow!("RC interrupted")) },
        _ = terminate.recv() => { interrupted = true; Err(anyhow::anyhow!("RC terminated")) },
        _ = hangup.recv() => { interrupted = true; Err(anyhow::anyhow!("RC lost its controlling session")) },
    };
    if result.is_err() {
        resources::diagnostics(&resources).await;
    }
    let cleanup = resources::cleanup(&mut resources).await;
    let unchanged = async {
        ensure!(
            source_head(repository).await? == source,
            "RC source changed during the test run"
        );
        resources::clean_source(repository).await
    }
    .await;
    let capture = resources::finish_result(
        &resources.evidence,
        &json!({
            "source":source,"cluster":CLUSTER,"namespace":NAMESPACE,
            "passed":result.is_ok() && cleanup.is_ok() && unchanged.is_ok(),
            "failure":result.as_ref().err().map(|error| format!("{error:#}")),
            "cleanup":cleanup.as_ref().err().map(|error| format!("{error:#}")),
            "source_changed":unchanged.as_ref().err().map(|error| format!("{error:#}")),
            "selected_tests":["socket-test","trace-test"],
            "socket_test_completed":resources.evidence.join("socket-test-verdict.json").is_file(),
            "trace_test_completed":resources.evidence.join("trace-test-verdict.json").is_file(),
            "deferred":["wamn-0h0g.15.153"],"deferred_disposition":"post-merge; not claimed",
        }),
    );
    if interrupted {
        std::process::exit(130);
    }
    result?;
    cleanup?;
    unchanged?;
    capture
}

async fn execute(
    resources: &mut Resources,
    source: async_nats::jetstream::stream::Config,
) -> anyhow::Result<()> {
    let scope = Triple::new("rc", "app", "dev");
    let advisory = events::advisory_stream_config(&scope, source.num_replicas);
    let broker = event_broker::prepare(&resources.work, &scope, TENANT, &source, &advisory, &[])?;
    resources::write_private(
        &resources.work.join("kind.yaml"),
        rendering::render_kind_cluster(&fs::read_to_string(
            resources.repository.join("deploy/infra/kind-config.yaml"),
        )?)?
        .as_bytes(),
    )?;
    // The separate existing PAT service is a native debug prerequisite.
    resources::recorded(
        resources,
        "identity-build",
        Command::new("cargo")
            .current_dir(&resources.repository)
            .args(["build", "--locked", "--offline", "-p", "wamn-identity"]),
    )
    .await?;
    resources.owned = true;
    resources::recorded(
        resources,
        "image-build",
        Command::new(&resources.lifecycle)
            .arg("build-images")
            .arg(&resources.repository)
            .arg(&resources.source)
            .arg(&resources.host_image)
            .arg(&resources.gates_image)
            .arg(&resources.postgres_image),
    )
    .await?;
    resources::recorded(
        resources,
        "create",
        Command::new(&resources.lifecycle)
            .arg("create")
            .arg(CLUSTER)
            .arg(&resources.work)
            .arg(&resources.host_image)
            .arg(&resources.gates_image)
            .arg(&resources.postgres_image),
    )
    .await?;
    let nats: Value = serde_json::from_slice(
        &resources::recorded(
            resources,
            "inspect-nats",
            Command::new(&resources.lifecycle).args(["inspect", "wamn-rc-nats"]),
        )
        .await?,
    )
    .context("parse the owned broker network")?;
    let nats_host = nats
        .pointer("/Networks/kind/IPAddress")
        .and_then(Value::as_str)
        .context("the owned broker has a kind network address")?;
    let _: std::net::Ipv4Addr = nats_host.parse().context("the broker address is IPv4")?;
    let server = format!("nats://{nats_host}:4222");
    event_broker::write_binding(&broker, &server, &source)?;
    let client = connect_broker(&broker.provisioning, &server).await?;
    wamn_ctl::event_streams::provision(
        &async_nats::jetstream::new(client),
        &scope,
        source.num_replicas,
        source.duplicate_window,
        &[],
    )
    .await?;
    save(
        resources,
        "event-stream.json",
        &serde_json::to_value(&source)?,
    )?;
    let host_digest = workload::image_ready(
        &resources.lifecycle,
        CLUSTER,
        &resources.work,
        &resources.host_image,
        &resources.source,
        "debug",
        &resources.evidence,
    )
    .await?;
    jobs::inspect_image(resources).await?;
    deployment::install(resources, &broker, &source, &server).await?;
    workload::hosts_ready(
        &resources.lifecycle,
        CLUSTER,
        &resources.work,
        NAMESPACE,
        &resources.host_image,
        &host_digest,
        3,
        &resources.evidence,
    )
    .await?;
    bootstrap::run(resources).await?;
    save(
        resources,
        "m2.json",
        &json!({"status":"not_run","reason":"da58814f removed the wake test Job and integration harness","deferred":"wamn-0h0g.15.26"}),
    )?;
    jobs::install_dependencies(resources).await?;
    let socket = jobs::run(resources, "socket-test", Duration::from_secs(180)).await?;
    let trace = jobs::run(resources, "trace-test", Duration::from_secs(240)).await?;
    save(
        resources,
        "m0-verdict.json",
        &json!({"schema_version":"0.1","source":resources.source,
        "scope":{"executed":["socket-test","trace-test"],"deferred":["wamn-0h0g.15.153"],
        "deferred_disposition":"post-merge; not claimed"},"verdict":"pass","failure_classes":[],
        "jobs":[socket,trace]}),
    )?;
    Ok(())
}

async fn connect_broker(
    credentials: &event_broker::Credentials,
    server: &str,
) -> anyhow::Result<async_nats::Client> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Ok(client) = event_broker::connect(credentials, server).await {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("the owned event broker did not start")
}

fn kubectl(resources: &Resources) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(resources.work.join("kubeconfig"))
        .args(["--context", "kind-wamn-rc", "--request-timeout=180s"]);
    command
}
async fn apply(resources: &Resources, path: &Path) -> anyhow::Result<()> {
    checked(kubectl(resources).args(["apply", "-f"]).arg(path)).await?;
    Ok(())
}
async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run {program}"))?;
    ensure!(
        output.status.success(),
        "{program} failed with {}; private output was not printed",
        output.status
    );
    Ok(output.stdout)
}
async fn command_json(command: &mut Command) -> anyhow::Result<Value> {
    serde_json::from_slice(&checked(command).await?).context("parse command JSON")
}
fn save(resources: &Resources, name: &str, value: &Value) -> anyhow::Result<()> {
    resources::write_private(
        &resources.evidence.join(name),
        &serde_json::to_vec_pretty(value)?,
    )
}
async fn source_head(repository: &Path) -> anyhow::Result<String> {
    Ok(String::from_utf8(
        checked(Command::new("git").current_dir(repository).args([
            "rev-parse",
            "--verify",
            "HEAD",
        ]))
        .await?,
    )?
    .trim()
    .to_owned())
}

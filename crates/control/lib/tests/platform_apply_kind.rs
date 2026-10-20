//! Install the platform on a private kind cluster and require an unchanged rerun.
//!
//! Requires Docker, kind, Helm and kubectl on PATH and public registry access.
//! No external database, bucket or DNS is required. The test owns its cluster
//! and keeps its input, kubeconfig and progress under /var/tmp.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_control::kubectl;
use wamn_control::platform::document::ClusterDocument;
use wamn_control::platform::{install, steps};

struct ClusterGuard {
    name: String,
}

impl Drop for ClusterGuard {
    fn drop(&mut self) {
        match Command::new("kind")
            .args(["delete", "cluster", "--name", &self.name])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("delete kind cluster {} exited {status}", self.name),
            Err(error) => eprintln!("delete kind cluster {}: {error}", self.name),
        }
    }
}

const STEP_TITLES: [&str; 5] = [
    "4.3 cert-manager and the edge certificate",
    "4.4 Namespaces, runtime operator and internal CA",
    "4.4.1 Collector",
    "4.5 PostgreSQL",
    "4.8 Event NATS",
];

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker, kind, helm, kubectl and network access to public registries"]
async fn platform_apply_kind() -> anyhow::Result<()> {
    let mut suffix = [0u8; 6];
    SystemRandom::new()
        .fill(&mut suffix)
        .map_err(|_| anyhow::anyhow!("generate the cluster suffix"))?;
    let name = format!("platform-apply-{}", hex::encode(suffix));
    let directory = Path::new("/var/tmp").join(&name);
    std::fs::create_dir(&directory).context("create the fixture directory")?;
    let work = directory.join("work");
    std::fs::create_dir(&work).context("create the install work directory")?;
    let file = directory.join("cluster.k");
    std::fs::write(
        &file,
        format!(
            r#"import cluster

cluster.Cluster {{
    provider = "kind"
    project_id = ""
    zone = ""
    cluster_name = "{name}"
    org = "test"
    owner_email = "owner@example.com"
    registry = "localhost:5000"
    buckets = {{web = "", otel = ""}}
    route_hosts = {{control = "control.example.com"}}
    resend_from = "sender@example.com"
}}
"#
        ),
    )?;
    let document = ClusterDocument::compile(&file).context("compile the kind cluster document")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    let _cluster = ClusterGuard { name: name.clone() };
    println!("work directory {}", directory.display());
    let mut progress = std::fs::File::create(directory.join("progress.log"))?;
    let mut first = Vec::new();
    install::apply(&document, &version, &repository, &work, |line| {
        println!("{line}");
        writeln!(progress, "{line}")?;
        first.push(line.to_owned());
        Ok(())
    })
    .await?;
    require_steps(&first, "done")?;

    let kubeconfig = work.join(format!("{name}.kubeconfig"));
    let context = format!("kind-{name}");
    kubectl::wait(
        &kubeconfig,
        &context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
        "condition=Ready",
        "60s",
    )
    .await?;
    kubectl::kubectl(
        &kubeconfig,
        &context,
        "platform",
        &[
            "rollout",
            "status",
            "statefulset/evt-nats",
            "--timeout=300s",
        ],
        None,
    )
    .await?;
    kubectl::wait(
        &kubeconfig,
        &context,
        "platform",
        "job",
        "evt-nats-tap-stream",
        "condition=complete",
        "180s",
    )
    .await?;
    let log = kubectl::kubectl(
        &kubeconfig,
        &context,
        "platform",
        &["logs", "job/evt-nats-tap-stream"],
        None,
    )
    .await?;
    std::fs::write(directory.join("tap-stream.log"), &log)?;
    ensure!(
        log.contains("WAMN_TAP"),
        "tap Job log does not name WAMN_TAP"
    );

    let mut second = Vec::new();
    install::apply(&document, &version, &repository, &work, |line| {
        println!("{line}");
        writeln!(progress, "{line}")?;
        second.push(line.to_owned());
        Ok(())
    })
    .await?;
    require_steps(&second, "skipped")?;
    Ok(())
}

fn require_steps(lines: &[String], result: &str) -> anyhow::Result<()> {
    let completed: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with("4.") && !line.ends_with(": starting"))
        .map(String::as_str)
        .collect();
    let expected = STEP_TITLES.map(|title| format!("{title}: {result}"));
    ensure!(
        completed == expected,
        "expected {expected:?}, got {completed:?}"
    );
    Ok(())
}

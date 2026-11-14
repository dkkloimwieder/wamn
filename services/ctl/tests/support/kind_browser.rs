//! Browser setup through product commands on an installed disposable platform.

use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_environment::platform::{
    document::{ClusterDocument, Provider},
    provider,
};
use wamn_release_control::release_chart::Target;

pub fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} for the installed browser fixture"))
}

pub struct Fixture {
    pub repository: PathBuf,
    pub root: PathBuf,
    pub environment: EnvironmentDocument,
    pub status: Value,
    target: Target,
    cluster: ClusterDocument,
}

impl Fixture {
    pub async fn prepare(name: &str) -> anyhow::Result<Self> {
        let cluster = ClusterDocument::compile(Path::new(&required("WAMN_TEST_CLUSTER")?))?;
        ensure!(
            cluster.provider == Provider::Kind,
            "browser fixture requires the kind tool"
        );
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let root = PathBuf::from(required("WAMN_TIMINGS_ROOT")?).join(name);
        std::fs::create_dir_all(&root)?;
        let mut target = Target {
            kubeconfig: root.join("kubeconfig"),
            context: String::new(),
            namespace: cluster.org.clone(),
        };
        target.context = provider::existing_kubeconfig(&cluster, &target.kubeconfig).await?;
        let output = wamn_kubernetes::kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "platform",
            &["get", "namespace", "platform", "-o", "json"],
            None,
        )
        .await?;
        let namespace: Value = serde_json::from_str(&output)?;
        ensure!(
            namespace["metadata"]["annotations"]["wamn.dev/disposable"] == "true",
            "browser fixture requires a disposable platform"
        );
        let file = PathBuf::from(required("WAMN_DEV_ENV")?).join("environment.k");
        let environment = EnvironmentDocument::compile(&file)?;
        ensure!(
            environment.org == cluster.org,
            "browser fixture org differs from its installed platform"
        );
        let product = |args: &[&str]| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_wamn"));
            command.current_dir(&repository).args(args);
            command
        };
        success(product(&["env", "apply"]).arg(&file)).await?;
        success(
            product(&["dev", "--config"])
                .arg(&file)
                .arg("--overlay-root")
                .arg(repository.join("apps/wamn_receiving")),
        )
        .await?;
        let output = success(product(&["status", "--env"]).arg(&file)).await?;
        let status = serde_json::from_slice(&output)?;
        Ok(Self {
            repository,
            root,
            environment,
            status,
            target,
            cluster,
        })
    }

    pub async fn browser_environment(&self, project: &str) -> anyhow::Result<()> {
        let mut environment = self.environment.clone();
        environment.project = project.to_owned();
        environment.applications.clear();
        let file = self.root.join("browser-project.k");
        std::fs::write(&file, environment.to_kcl())?;
        success(
            Command::new(env!("CARGO_BIN_EXE_wamn"))
                .current_dir(&self.repository)
                .args(["env", "apply"])
                .arg(file),
        )
        .await?;
        Ok(())
    }

    pub async fn invitation(&self, email: &str) -> anyhow::Result<String> {
        let deployment = wamn_kubernetes::kubectl::kubectl(
            &self.target.kubeconfig,
            &self.target.context,
            &self.cluster.org,
            &["get", "deployment", "identity", "-o", "json"],
            None,
        )
        .await?;
        let deployment: Value = serde_json::from_str(&deployment)?;
        let container = deployment["spec"]["template"]["spec"]["containers"]
            .as_array()
            .context("identity containers")?
            .iter()
            .find(|container| container["name"] == "identity")
            .context("identity container")?;
        let args = container["args"].as_array().context("identity arguments")?;
        let path = args
            .windows(2)
            .find(|pair| pair[0] == "--local-mail")
            .and_then(|pair| pair[1].as_str())
            .context("installed kind identity has no invitation outbox")?;
        let mail = wamn_kubernetes::kubectl::kubectl(
            &self.target.kubeconfig,
            &self.target.context,
            &self.cluster.org,
            &[
                "exec",
                "deployment/identity",
                "-c",
                "identity",
                "--",
                "cat",
                path,
            ],
            None,
        )
        .await?;
        let messages = mail
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let message = messages
            .iter()
            .rev()
            .find(|message| message["to"] == email && message["subject"] == "Your WAMN invitation")
            .context("identity outbox has no browser invitation")?;
        message["text"]
            .as_str()
            .context("invitation text")?
            .lines()
            .find_map(|line| line.strip_prefix("Invitation code: "))
            .map(str::to_owned)
            .context("invitation code")
    }
}

async fn success(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command.output().await?;
    ensure!(
        output.status.success(),
        "product command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

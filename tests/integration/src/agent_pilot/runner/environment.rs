//! Start the pilot's existing disposable services and pinned standup binary.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::json;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker::{self, EventBroker};

use super::{Run, directory, process, string};
use crate::agent_pilot::{text, write, write_json};

#[derive(Debug)]
pub(super) struct Broker {
    events: EventBroker,
    server: String,
    replicas: usize,
    duplicate_window: u64,
}

impl Run {
    pub(super) fn lifecycle(&self, action: &str) -> Command {
        let mut command = Command::new(self.tree.join("tools/agent-pilot-run"));
        command
            .arg("lifecycle")
            .arg(action)
            .arg(&self.tree)
            .arg(&self.args.run)
            .arg(self.directory.join("env"))
            .env("WAMN_STD_VIRT_PG_PORT", "54332")
            .env("WAMN_STD_VIRT_REGISTRY_PORT", "5003")
            .env("WAMN_ROUTE_REGISTRY_PORT", "5004")
            .env(
                "WAMN_ROUTE_REGISTRY_HTPASSWD",
                self.directory.join("env/htpasswd"),
            )
            .env("WAMN_RECEIVING_DEV_NATS_PORT", "4224")
            .env("WAMN_RECEIVING_DEV_TEMPO_PORT", "3201")
            .env("WAMN_RECEIVING_DEV_OTLP_PORT", "4319");
        command
    }

    pub(super) async fn start_infrastructure(&self) -> anyhow::Result<Broker> {
        let environment = self.directory.join("env");
        let replicas = usize::try_from(
            self.task["environment"]["event_stream_replicas"]
                .as_u64()
                .context("the pilot declares event stream replicas")?,
        )?;
        let duplicate_window = self.task["environment"]["event_dup_window_secs"]
            .as_u64()
            .context("the pilot declares the event duplicate window")?;
        let scope = Triple::new(
            text(&self.task["identity"]["org"]),
            text(&self.task["identity"]["project"]),
            text(&self.task["identity"]["env"]),
        );
        let source = source_stream_config(&scope, replicas, Duration::from_secs(duplicate_window));
        let advisory = advisory_stream_config(&scope, replicas);
        let events = event_broker::prepare(
            &environment,
            &scope,
            text(&self.task["identity"]["tenant"]),
            &source,
            &advisory,
            &[],
        )?;
        // This native Compose override keeps scheduler and event authority separate.
        write_json(
            &environment.join("compose.json"),
            &json!({"services":{"receiving-pilot-events":{
                "image":"nats:2.10-alpine","command":["--config=/etc/nats/nats.conf","--jetstream","--store_dir=/data"],
                "ports":["127.0.0.1::4222"],"volumes":[{"type":"bind","source":events.configuration,"target":"/etc/nats/nats.conf","read_only":true}],
                "healthcheck":{"test":["CMD-SHELL","wget -qO- http://127.0.0.1:8222/healthz | grep -q ok"],"interval":"1s","timeout":"2s","retries":60}
            }}}),
        )?;
        let password = string(Command::new("openssl").args(["rand", "-hex", "32"])).await?;
        let mut prepare = self.lifecycle("prepare-registry");
        let mut child = prepare
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .context("registry password input exists")?;
        input.write_all(password.as_bytes()).await?;
        input.write_all(b"\n").await?;
        drop(input);
        let result = child.wait_with_output().await?;
        anyhow::ensure!(
            result.status.success(),
            "registry password preparation failed"
        );
        write(&environment.join("htpasswd"), &result.stdout)?;
        directory(&environment.join("docker"))?;
        write_json(
            &environment.join("docker/config.json"),
            &json!({"auths":{"127.0.0.1:5004":{"username":"wamn-pilot","password":password}}}),
        )?;
        self.logged(&mut self.lifecycle("up")).await?;
        let port = string(&mut self.lifecycle("event-port")).await?;
        let (host, port) = port
            .rsplit_once(':')
            .context("event broker has a published port")?;
        anyhow::ensure!(
            host == "127.0.0.1",
            "event broker is published only on loopback"
        );
        let port: u16 = port.parse()?;
        let server = format!("nats://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let mut ready = false;
        for _ in 0..60 {
            if client
                .get("http://127.0.0.1:3201/ready")
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::ensure!(ready, "Tempo never became ready");
        let (postgres, connection) = tokio_postgres::connect(
            "postgresql://postgres:probe@127.0.0.1:54332/postgres",
            tokio_postgres::NoTls,
        )
        .await?;
        let task = tokio::spawn(connection);
        let query = postgres.simple_query("select 1").await;
        drop(postgres);
        task.await??;
        query.context("the disposable PostgreSQL did not answer a query")?;
        let wash = string(&mut Command::new(self.tree.join("tools/install-wash"))).await?;
        self.logged(
            Command::new(wash)
                .args(["oci", "push", "127.0.0.1:5004/wamn/flow-http:pilot"])
                .arg(self.target.join("wasm32-wasip2/debug/http_route.wasm"))
                .arg("--insecure")
                .env("DOCKER_CONFIG", environment.join("docker")),
        )
        .await?;
        Ok(Broker {
            events,
            server,
            replicas,
            duplicate_window,
        })
    }

    pub(super) async fn standup(&self, broker: &Broker) -> anyhow::Result<()> {
        for (field, value) in [
            ("org", "acme"),
            ("project", "receiving"),
            ("env", "dev"),
            ("tenant", "receiving-route-auth"),
        ] {
            let named = text(&self.task["identity"][field]);
            anyhow::ensure!(
                named == value,
                "the standup fixes identity.{field} to {value} and the manifest names {named}"
            );
        }
        let packages = self.task["package_sources"]
            .as_array()
            .filter(|packages| !packages.is_empty())
            .context("the manifest names no package_sources; the standup requires at least one")?;
        let mut command = Command::new("setsid");
        command.arg("nohup");
        if self.args.standup == "dev-env" {
            command.arg(self.target.join("debug/wamn-dev-env"));
        } else {
            command
                .arg(self.target.join("debug/wamn"))
                .args(["dev", "up"]);
        }
        command
            .current_dir(self.directory.join("worktree"))
            .args([
                "--system-database-url",
                "postgresql://postgres:probe@127.0.0.1:54332/postgres",
                "--root",
            ])
            .arg(self.directory.join("env"))
            .args(["--nats-url", "nats://127.0.0.1:4224", "--event-nats-url"])
            .arg(&broker.server)
            .arg("--event-nats-username")
            .arg(&broker.events.runtime.username)
            .arg("--event-nats-password-file")
            .arg(&broker.events.runtime.password_file)
            .arg("--event-provisioning-username")
            .arg(&broker.events.provisioning.username)
            .arg("--event-provisioning-password-file")
            .arg(&broker.events.provisioning.password_file)
            .arg("--stream-replicas")
            .arg(broker.replicas.to_string())
            .arg("--dup-window-secs")
            .arg(broker.duplicate_window.to_string())
            .args([
                "--tempo-query-url",
                "http://127.0.0.1:3201",
                "--otel-exporter-otlp-endpoint",
                "http://127.0.0.1:4319",
                "--component-artifact-base",
                "127.0.0.1:5004/wamn/components",
                "--release-artifact-base",
                "127.0.0.1:5004/wamn/releases",
                "--registry-auth-file",
            ])
            .arg(self.directory.join("env/docker/config.json"))
            .arg("--route-host")
            .arg(text(&self.task["identity"]["route_host"]))
            .args([
                "--flow-http-workload-image",
                "127.0.0.1:5004/wamn/flow-http:pilot",
                "--host-binary",
            ])
            .arg(self.target.join("debug/wamn-host"))
            .arg("--scenario-worker-binary")
            .arg(self.target.join("debug/wamn-scenario-worker"));
        for package in packages {
            command
                .arg("--package")
                .arg(self.directory.join("worktree").join(text(package)));
        }
        let log = process::append(&self.directory.join("env.log"))?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        let pid = child.id().context("standup process has an id")?;
        write(
            &self.directory.join("env/standup.pid"),
            format!("{pid}\n").as_bytes(),
        )?;
        for _ in 0..180 {
            let ready = ["env/dev.json", "env/route-caller-pat.json"]
                .iter()
                .all(|path| {
                    self.directory
                        .join(path)
                        .metadata()
                        .is_ok_and(|m| m.len() > 0)
                });
            if ready && process::listening(8088).await? {
                return Ok(());
            }
            anyhow::ensure!(
                child.try_wait()?.is_none(),
                "the {} standup exited; see {}",
                self.args.standup,
                self.directory.join("env.log").display()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::bail!(
            "the {} standup did not hold a Gate on 8088; see {}",
            self.args.standup,
            self.directory.join("env.log").display()
        )
    }
}

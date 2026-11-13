//! Local measurement through the product development commands.

use std::os::unix::fs::PermissionsExt as _;
use tokio::io::AsyncWriteExt as _;

use crate::{bench_data as data, process};
use anyhow::{Context as _, ensure};
use clap::Args as ClapArgs;
use serde::Deserialize as _;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent CLI switches retain the measurement commands"
)]
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, default_value_t = 60)]
    duration_secs: u64,
    #[arg(long, default_value_t = 16)]
    concurrency: usize,
    #[arg(long)]
    profile: bool,
    #[arg(long)]
    pg_minimum: bool,
    #[arg(long, conflicts_with = "pg_minimum")]
    app_path: bool,
    #[arg(long)]
    mix: bool,
    #[arg(long)]
    contention: bool,
    #[arg(long,value_parser=["get","update","query","receipt-query","create","record_receipt","app-path"])]
    call: Vec<String>,
}

pub(super) struct Local<'a> {
    root: &'a Path,
    work: PathBuf,
    bin: PathBuf,
    containers: Vec<String>,
    development: Option<tokio::process::Child>,
    sampler: Option<tokio::process::Child>,
    prefix: String,
    database: String,
    host_pid: PathBuf,
}
impl Local<'_> {
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(self.root);
        command
    }
    async fn logged(&self, name: &str, command: &mut Command) -> anyhow::Result<String> {
        let text = process::execute(command).await?;
        std::fs::write(self.work.join(name), &text)?;
        Ok(text)
    }
    async fn container(&mut self, args: &[&str]) -> anyhow::Result<()> {
        let id = process::output(
            self.root,
            "docker",
            &[&["run", "--pull=never", "-d"][..], args].concat(),
        )
        .await?;
        self.containers.push(id.trim().to_owned());
        Ok(())
    }
    async fn sql(&self, sql: &str) -> anyhow::Result<String> {
        process::output(
            self.root,
            "psql",
            &[&self.database, "-XAt", "-v", "ON_ERROR_STOP=1", "-c", sql],
        )
        .await
    }
    async fn sql_file(&self, path: &Path, args: &[&str]) -> anyhow::Result<()> {
        process::execute(
            self.command("psql")
                .args([&self.database, "-X", "-v", "ON_ERROR_STOP=1"])
                .args(args)
                .arg("-f")
                .arg(path),
        )
        .await?;
        Ok(())
    }
    async fn profile_start(&mut self, call: &str) -> anyhow::Result<()> {
        std::fs::write(
            self.work.join(format!("{call}.database-before.json")),
            self.sql(include_str!("../bench/database-snapshot.sql"))
                .await?,
        )?;
        let log = std::fs::File::create(self.work.join(format!("{call}.wait-events.log")))?;
        let mut command = self.command("psql");
        command
            .args([&self.database, "-XAt", "-v", "ON_ERROR_STOP=1"])
            .stdin(std::process::Stdio::piped())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        child.stdin.take().context("psql stdin")?.write_all(b"SELECT json_build_object('at', clock_timestamp(), 'sessions', coalesce(json_agg(json_build_object('pid', pid, 'user', usename, 'backend_type', backend_type, 'state', state, 'wait_event_type', wait_event_type, 'wait_event', wait_event, 'query_id', query_id, 'query', query)), '[]')) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid();\n\\watch 0.1\n").await?;
        self.sampler = Some(child);
        Ok(())
    }
    async fn profile_stop(&mut self, call: &str) -> anyhow::Result<()> {
        if let Some(mut child) = self.sampler.take() {
            child.kill().await?;
        }
        std::fs::write(
            self.work.join(format!("{call}.database-after.json")),
            self.sql(include_str!("../bench/database-snapshot.sql"))
                .await?,
        )?;
        let rows=process::output(self.root,"psql",&[&self.database,"-X","--csv","-v","ON_ERROR_STOP=1","-c","SELECT s.*, r.rolname FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC"]).await?;
        std::fs::write(self.work.join(format!("{call}.statements.csv")), rows)?;
        data::database_delta(&self.work, call)
    }
    async fn seed(&self, scale: usize) -> anyhow::Result<()> {
        self.sql_file(&self.root.join("apps/wamn_receiving/tests/fixtures/receiving-seed.sql"),&["-v",&format!("scale={scale}"),"-c","SET auto_explain.sample_rate=0; SET log_transaction_sample_rate=0; SET log_statement_sample_rate=0"]).await
    }
    async fn stop_development(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.development.take() {
            if let Some(pid) = child.id() {
                let pid = rustix::process::Pid::from_raw(i32::try_from(pid)?)
                    .context("development pid")?;
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
            if tokio::time::timeout(Duration::from_secs(30), child.wait())
                .await
                .is_err()
            {
                child.kill().await?;
            }
        }
        if self.work.join("env/identity-process.json").is_file() {
            self.logged(
                "down.log",
                self.command(self.bin.join("wamn").to_str().context("wamn path")?)
                    .args(["dev", "down", "--root"])
                    .arg(self.work.join("env")),
            )
            .await?;
        }
        Ok(())
    }
    async fn cleanup(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.sampler.take() {
            let _ = child.kill().await;
        }
        let mut failures = Vec::new();
        if let Err(error) = self.stop_development().await {
            failures.push(format!("{error:#}"));
        }
        if !self.containers.is_empty() {
            let logs = self
                .command("docker")
                .args(["logs", "--timestamps", &format!("{}-postgres", self.prefix)])
                .output()
                .await?;
            let mut bytes = logs.stdout;
            bytes.extend(logs.stderr);
            std::fs::write(self.work.join("postgres.log"), bytes)?;

            let mut command = self.command("docker");
            command.args(["rm", "-fv"]).args(&self.containers);
            if let Err(error) = process::execute(&mut command).await {
                failures.push(format!("{error:#}"));
            }
        }
        if self.host_pid.exists() {
            let pid = std::fs::read_to_string(&self.host_pid)?;
            let map = PathBuf::from(format!("/tmp/perf-{}.map", pid.trim()));
            if std::fs::read_link(&map).is_ok_and(|path| path == self.work.join("guest.map")) {
                std::fs::remove_file(map)?;
            }
        }
        ensure!(failures.is_empty(), "{}", failures.join("\n"));
        Ok(())
    }
    fn cpu(&self) -> anyhow::Result<u64> {
        let pid = std::fs::read_to_string(&self.host_pid)?;
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))?;
        let fields = stat
            .rsplit_once(") ")
            .context("host stat")?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        Ok(fields[11].parse::<u64>()? + fields[12].parse::<u64>()?)
    }
}

pub async fn run(root: &Path, args: &Args) -> anyhow::Result<()> {
    ensure!(
        args.duration_secs > 0 && args.concurrency > 0,
        "duration and concurrency must be positive"
    );
    ensure!(
        !args.pg_minimum || !args.profile,
        "PostgreSQL replay does not use perf"
    );
    let work = tempfile::Builder::new()
        .prefix("wamn-bench-")
        .tempdir_in(std::env::temp_dir())?
        .keep();
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let target = if target.is_absolute() {
        target
    } else {
        root.join(target)
    };
    let prefix = format!(
        "wamn-lbench-{}",
        work.file_name().context("run directory")?.to_string_lossy()
    );
    let mut local = Local {
        root,
        bin: target.join("release"),
        host_pid: work.join("host.pid"),
        work,
        containers: Vec::new(),
        development: None,
        sampler: None,
        prefix,
        database: String::new(),
    };
    let result = measure(&mut local, args, &target).await;
    let cleanup = local.cleanup().await;
    println!("Local run files: {}", local.work.display());
    result?;
    cleanup
}

async fn measure(local: &mut Local<'_>, args: &Args, target: &Path) -> anyhow::Result<()> {
    let root = local.root;
    std::fs::create_dir(local.work.join("env"))?;
    if args.profile {
        ensure!(
            std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")?
                .trim()
                .parse::<i64>()?
                <= 1,
            "profiling requires kernel.perf_event_paranoid <= 1"
        );
    }
    let mut sockets = Vec::new();
    for _ in 0..3 {
        sockets.push(std::net::TcpListener::bind("127.0.0.1:0")?);
    }
    let ports = sockets
        .iter()
        .map(|socket| socket.local_addr().map(|a| a.port()))
        .collect::<Result<Vec<_>, _>>()?;
    drop(sockets);
    local.database = format!(
        "postgresql://postgres:probe@127.0.0.1:{}/wamn_system",
        ports[0]
    );
    let cluster: Value =
        serde_yaml::from_slice(&std::fs::read(root.join("deploy/infra/cnpg-cluster.yaml"))?)?;
    let mut postgres = vec![
        "--name".to_owned(),
        format!("{}-postgres", local.prefix),
        "--cpuset-cpus".into(),
        "4-5".into(),
        "-e".into(),
        "POSTGRES_PASSWORD=probe".into(),
        "-e".into(),
        "POSTGRES_DB=wamn_system".into(),
        "-p".into(),
        format!("127.0.0.1:{}:5432", ports[0]),
        "postgres:18".into(),
    ];
    for parameter in [
        "wal_level=logical",
        "shared_preload_libraries=pg_stat_statements,auto_explain",
        "pg_stat_statements.track=all",
        "pg_stat_statements.track_planning=on",
        "track_io_timing=on",
        "track_wal_io_timing=on",
        "auto_explain.log_min_duration=0",
        "auto_explain.log_analyze=on",
        "auto_explain.log_buffers=on",
        "auto_explain.log_wal=on",
        "auto_explain.log_nested_statements=on",
        "auto_explain.log_timing=off",
        "auto_explain.sample_rate=0.01",
        "auto_explain.log_format=json",
        "log_transaction_sample_rate=0.01",
        "log_min_duration_sample=0",
        "log_statement_sample_rate=0.01",
        "log_line_prefix=%m [%p] user=%u db=%d xid=%v ",
    ] {
        postgres.extend(["-c".into(), parameter.into()]);
    }
    for name in [
        "max_slot_wal_keep_size",
        "pg_stat_statements.max",
        "pg_stat_statements.track",
    ] {
        let value = cluster["spec"]["postgresql"]["parameters"][name]
            .as_str()
            .context("PostgreSQL parameter")?;
        postgres.extend(["-c".into(), format!("{name}={value}")]);
    }
    local
        .container(&postgres.iter().map(String::as_str).collect::<Vec<_>>())
        .await?;
    ready(root,"docker",&["exec",&format!("{}-postgres",local.prefix),"sh","-c","test \"$(cat /proc/1/comm)\" = postgres && pg_isready -h 127.0.0.1 -U postgres -d wamn_system"],60).await?;
    if args.pg_minimum {
        return pg_minimum(local, args).await;
    }
    crate::tasks::collector(root, &local.work.join("env"))?;
    process::run(
        root,
        "cargo",
        &[
            "build",
            "--offline",
            "--locked",
            "--release",
            "-p",
            "wamn-host",
            "-p",
            "wamn-runtime",
            "--bin",
            "wamn-host",
            "--bin",
            "docker-credential-wamn",
        ],
    )
    .await?;
    process::run(
        root,
        "cargo",
        &[
            "build",
            "--offline",
            "--locked",
            "--release",
            "-p",
            "wamn-ctl",
            "--bin",
            "wamn",
            "-p",
            "wamn-identity",
            "--bin",
            "wamn-identity",
            "-p",
            "wamn-cdc-reader",
            "--bin",
            "wamn-cdc-reader",
            "-p",
            "wamn-test-infrastructure",
            "--example",
            "event_broker_files",
        ],
    )
    .await?;
    let flags = crate::guest_rustflags(root).await?;
    let route = wamn_build::build_guest(
        root,
        &root.join("apps/Cargo.toml"),
        "http-route",
        Some(target),
    )
    .await?;
    let materializer = wamn_build::build_guest(
        root,
        &root.join("apps/Cargo.toml"),
        "materializer",
        Some(target),
    )
    .await?;
    let http = data_digest(&route)?;
    let materializer_digest = data_digest(&materializer)?;
    local
        .logged(
            "build.log",
            local
                .command(local.bin.join("wamn").to_str().context("wamn path")?)
                .env("DATABASE_URL", &local.database)
                .arg("build")
                .arg(root.join("apps/wamn_receiving")),
        )
        .await?;
    // The product owns the development identity and permission files.
    let coordinates = [
        wamn_dev_control::dev::environment::ORG,
        wamn_dev_control::dev::environment::PROJECT,
        wamn_dev_control::dev::environment::ENVIRONMENT,
        wamn_dev_control::dev::environment::PROJECT_ENV_KEY,
    ];
    local
        .logged(
            "event-broker.log",
            local
                .command(
                    local
                        .bin
                        .join("examples/event_broker_files")
                        .to_str()
                        .context("broker path")?,
                )
                .arg(&local.work)
                .args(coordinates)
                .arg("1")
                .arg(root.join("apps/target/wamn/wamn_receiving/wamn.json")),
        )
        .await?;
    let deployment = std::fs::read_to_string(root.join("deploy/infra/nats-jetstream.yaml"))?;
    let documents = serde_yaml::Deserializer::from_str(&deployment)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let configuration = documents
        .iter()
        .find_map(|doc| doc.pointer("/data/nats.conf").and_then(Value::as_str))
        .context("NATS configuration")?
        .replace("$POD_NAME", &format!("{}-events", local.prefix));
    std::fs::write(local.work.join("events.conf"), configuration)?;
    let memory = documents
        .iter()
        .find_map(|doc| {
            doc.pointer("/spec/template/spec/containers/0/resources/limits/memory")
                .and_then(Value::as_str)
        })
        .context("NATS memory limit")?
        .to_owned();
    let mut events = vec![
        "--name".to_owned(),
        format!("{}-events", local.prefix),
        "--memory".into(),
        memory,
        "-p".into(),
        format!("127.0.0.1:{}:4222", ports[1]),
        "--tmpfs".into(),
        "/var/run/nats".into(),
    ];
    for (source, destination) in [
        ("events.conf", "/etc/nats-config/nats.conf"),
        (
            "event-nats/authorization.conf",
            "/etc/nats-authorization/authorization.conf",
        ),
        ("event-nats/ca.crt", "/etc/nats-tls/ca.crt"),
        ("event-nats/server.crt", "/etc/nats-tls/tls.crt"),
        ("event-nats/server.key", "/etc/nats-tls/tls.key"),
    ] {
        events.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst={destination},readonly",
                local.work.join(source).display()
            ),
        ]);
    }
    events.extend([
        "nats:2.10-alpine".into(),
        "--config=/etc/nats-config/nats.conf".into(),
    ]);
    local
        .container(&events.iter().map(String::as_str).collect::<Vec<_>>())
        .await?;
    let collector = data::read(&local.work.join("env/local-artifacts/otel/compose.json"))?;
    let collector = &collector["services"]["receiving-dev-collector"];
    local
        .container(&[
            "--name",
            &format!("{}-collector", local.prefix),
            "--user",
            collector["user"].as_str().context("collector user")?,
            "-p",
            &format!("127.0.0.1:{}:4317", ports[2]),
            "--env",
            &format!(
                "CLUSTER={}",
                collector["environment"]["CLUSTER"]
                    .as_str()
                    .context("collector cluster")?
            ),
            "--mount",
            &format!(
                "type=bind,src={},dst=/var/lib/otelcol",
                local.work.join("env/local-artifacts/otel").display()
            ),
            collector["image"].as_str().context("collector image")?,
            "--config=/var/lib/otelcol/config.yaml",
        ])
        .await?;
    ready(
        root,
        "docker",
        &[
            "exec",
            &format!("{}-events", local.prefix),
            "wget",
            "-qO-",
            "http://127.0.0.1:8222/healthz",
        ],
        60,
    )
    .await?;
    let host = local.bin.join("wamn-host");
    let build_started = std::time::Instant::now();
    if args.profile {
        process::execute(
            local
                .command("cargo")
                .env("CARGO_PROFILE_RELEASE_DEBUG", "line-tables-only")
                .env(
                    "RUSTFLAGS",
                    "-C force-frame-pointers=yes -C link-arg=-fuse-ld=mold",
                )
                .args([
                    "build",
                    "--offline",
                    "--locked",
                    "--release",
                    "-p",
                    "wamn-host",
                    "-p",
                    "wamn-runtime",
                    "--bin",
                    "wamn-host",
                    "--bin",
                    "docker-credential-wamn",
                ]),
        )
        .await?;
    }
    std::fs::write(
        local.work.join("host-build.seconds"),
        format!("{}", build_started.elapsed().as_secs_f64()),
    )?;
    std::fs::write(local.work.join("host.sha256"), data_digest(&host)?)?;
    std::fs::write(
        local.work.join("revision"),
        process::output(root, "git", &["rev-parse", "HEAD"]).await?,
    )?;
    std::fs::write(
        local.work.join("source.diff"),
        process::output(
            root,
            "git",
            &[
                "diff",
                "--",
                "xtask",
                "apps/wamn_receiving",
                "crates/platform/engine/src/engine.rs",
            ],
        )
        .await?,
    )?;
    let wrapper = local.work.join("host");
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/bash\nset -euo pipefail\necho \"$$\" > {}\n{}exec /usr/bin/taskset -c 0-3 {} \"$@\"\n",
            quote(&local.host_pid),
            if args.profile {
                format!(
                    "ln -s {} \"/tmp/perf-$$.map\"\n",
                    quote(&local.work.join("guest.map"))
                )
            } else {
                String::new()
            },
            quote(&host)
        ),
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
    std::fs::copy(
        root.join("tests/integration/fixtures/local-environments/receiving.k"),
        local.work.join("env/environment.k"),
    )?;
    local
        .logged(
            "up.log",
            local
                .command(local.bin.join("wamn").to_str().context("wamn path")?)
                .args(["dev", "up", "--package"])
                .arg(root.join("apps/wamn_receiving"))
                .arg("--system-database-url")
                .arg(&local.database)
                .arg("--root")
                .arg(local.work.join("env"))
                .args(["--nats-url", &format!("nats://127.0.0.1:{}", ports[1])])
                .arg("--nats-tls-ca")
                .arg(local.work.join("event-nats/ca.crt"))
                .arg("--nats-tls-cert")
                .arg(local.work.join("event-nats/client-wamn-dev.crt"))
                .arg("--nats-tls-key")
                .arg(local.work.join("event-nats/client-wamn-dev.key"))
                .args([
                    "--stream-replicas",
                    "1",
                    "--dup-window-secs",
                    "120",
                    "--otel-exporter-otlp-endpoint",
                    &format!("http://127.0.0.1:{}", ports[2]),
                    "--route-host",
                    "receiving.localhost",
                    "--platform-domain",
                    "example.invalid",
                ])
                .arg("--host-binary")
                .arg(&wrapper)
                .arg("--cdc-reader-binary")
                .arg(local.bin.join("wamn-cdc-reader"))
                .arg("--flow-http-component")
                .arg(&route)
                .arg("--materializer-component")
                .arg(&materializer),
        )
        .await?;
    let log = std::fs::File::create(local.work.join("dev.log"))?;
    local.development = Some(
        local
            .command(local.bin.join("wamn").to_str().context("wamn path")?)
            .env("RUSTFLAGS", flags)
            .args(["dev", "--config"])
            .arg(local.work.join("env/dev.json"))
            .arg("--overlay-root")
            .arg(root.join("apps/wamn_receiving"))
            .arg("--hold")
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()?,
    );
    let pattern = regex::Regex::new(r"(?m)^run served: ([^ ]*) host=([^ ]*) ")?;
    let mut served = None;
    for _ in 0..900 {
        let log = std::fs::read_to_string(local.work.join("dev.log"))?;
        if let Some(found) = pattern.captures(&log) {
            served = Some((found[1].to_owned(), found[2].to_owned()));
            break;
        }
        ensure!(
            local
                .development
                .as_mut()
                .context("dev process")?
                .try_wait()?
                .is_none(),
            "development host exited: {log}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let (url, host_name) = served.context("development host did not serve")?;

    data::read(&local.work.join("env/dev.json"))?["target_database_url"]
        .as_str()
        .context("target database")?
        .clone_into(&mut local.database);
    let scale = if args.app_path { 1 } else { 1000 };
    local.seed(scale).await?;
    local
        .sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .await?;
    let calls = if args.app_path {
        vec!["app-path".into()]
    } else if args.mix {
        vec!["mix".into()]
    } else if !args.call.is_empty() {
        args.call.clone()
    } else if args.contention {
        vec!["update".into()]
    } else {
        ["get", "update", "query", "record_receipt", "mix"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    for call in calls {
        measure_call(local, args, &call, &url, &host_name, target).await?;
    }
    let output = local.work.join("result.json");
    if args.app_path && !args.mix {
        let pid = std::fs::read_to_string(&local.host_pid)?;
        let environment = std::fs::read(format!("/proc/{}/environ", pid.trim()))?;
        let mut sampler = json!({});
        for entry in environment.split(|b| *b == 0) {
            if let Some(index) = entry.iter().position(|b| *b == b'=') {
                let (name, value) = (&entry[..index], &entry[index + 1..]);
                if [
                    b"OTEL_TRACES_SAMPLER".as_slice(),
                    b"OTEL_TRACES_SAMPLER_ARG",
                ]
                .contains(&name)
                {
                    sampler[String::from_utf8_lossy(name).as_ref()] =
                        String::from_utf8(value.to_vec())?.into();
                }
            }
        }
        data::write(&local.work.join("host-telemetry.json"), &sampler)?;
        local.stop_development().await?;
        process::run(
            root,
            "docker",
            &["stop", &format!("{}-collector", local.prefix)],
        )
        .await?;
        data::app_result(
            &local.work,
            &output,
            &http,
            &materializer_digest,
            false,
            scale,
        )?;
    } else {
        data::write(
            &local.work.join("metadata.json"),
            &json!({"concurrency":args.concurrency,"http_route_sha256":http,"materializer_sha256":materializer_digest,"edge":false}),
        )?;
        data::result(&local.work, &output, args.concurrency)?;
    }
    Ok(())
}

async fn ready(root: &Path, program: &str, args: &[&str], attempts: usize) -> anyhow::Result<()> {
    for _ in 0..attempts {
        if process::output(root, program, args).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("local service did not become ready")
}
fn data_digest(path: &Path) -> anyhow::Result<String> {
    Ok(wamn_digest::bytes_sha256(&std::fs::read(path)?))
}

async fn measure_call(
    local: &mut Local<'_>,
    args: &Args,
    call: &str,
    url: &str,
    host: &str,
    target: &Path,
) -> anyhow::Result<()> {
    for graph in if args.profile {
        vec!["fp", "dwarf"]
    } else {
        vec!["none"]
    } {
        if graph == "dwarf" {
            let result = data::read(&local.work.join(format!("{call}.fp.summary.json")))?;
            if result["shallow_percent"].as_f64().unwrap_or(0.0).max(
                result["stacks_with_unknown_percent"]
                    .as_f64()
                    .unwrap_or(0.0),
            ) <= 10.0
            {
                break;
            }
        }
        let label = if graph == "dwarf" {
            format!("{call}-dwarf")
        } else {
            call.to_owned()
        };
        let log = local.work.join(format!("{label}.log"));
        let output = std::fs::File::create(&log)?;
        let mut command = local.command("taskset");
        command
            .args(["-c", "6-7"])
            .arg(std::env::current_exe()?)
            .args(["bench", "--base-url", url, "--host", host, "--pat-file"])
            .arg(local.work.join("env/operator-pat.json"))
            .args([
                "--call",
                if call == "mix" { "get" } else { call },
                "--concurrency",
                &args.concurrency.to_string(),
                "--duration-secs",
                &args.duration_secs.to_string(),
                "--tier",
                "local",
                "--db-url",
                &local.database,
                "--output",
            ])
            .arg(local.work.join(format!("{label}.json")))
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true);
        if call == "mix" {
            command.arg("--mix");
        }
        if args.contention {
            command.arg("--contention");
        }
        let mut child = command.spawn()?;
        loop {
            if std::fs::read_to_string(&log)?
                .lines()
                .any(|line| line.starts_with("benchmark-start "))
            {
                break;
            }
            ensure!(
                child.try_wait()?.is_none(),
                "benchmark exited before timing: {}",
                std::fs::read_to_string(&log)?
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let cpu = local.cpu()?;
        local.profile_start(&label).await?;
        let pid = std::fs::read_to_string(&local.host_pid)?;
        let mut perf = if args.profile {
            let log = std::fs::File::create(local.work.join(format!("{call}.{graph}.perf.log")))?;
            Some(
                local
                    .command("perf")
                    .args([
                        "record",
                        "--no-buildid-cache",
                        "-e",
                        "cpu-clock:u",
                        "-F",
                        "99",
                        "--call-graph",
                        if graph == "dwarf" {
                            "dwarf,16384"
                        } else {
                            graph
                        },
                        "-p",
                        pid.trim(),
                        "-o",
                    ])
                    .arg(local.work.join(format!("{call}.{graph}.perf.data")))
                    .stdout(log.try_clone()?)
                    .stderr(log)
                    .kill_on_drop(true)
                    .spawn()?,
            )
        } else {
            None
        };
        ensure!(
            child.wait().await?.success(),
            "benchmark failed: {}",
            std::fs::read_to_string(&log)?
        );
        let ticks = process::output(local.root, "getconf", &["CLK_TCK"]).await?;
        std::fs::write(
            local.work.join(format!("{label}.host-cpu")),
            format!("{cpu} {} {}", local.cpu()?, ticks.trim()),
        )?;
        if let Some(child) = perf.as_mut() {
            let pid =
                rustix::process::Pid::from_raw(i32::try_from(child.id().context("perf pid")?)?)
                    .context("perf pid")?;
            rustix::process::kill_process(pid, rustix::process::Signal::INT)?;
            let status = child.wait().await?;
            ensure!(
                status.success() || status.code() == Some(130),
                "perf record failed"
            );
        }
        local.profile_stop(&label).await?;
        if args.profile {
            let input = local.work.join(format!("{call}.{graph}.perf.data"));
            let stacks = local.work.join(format!("{call}.{graph}.stacks"));
            let decoded = local
                .logged(
                    &format!("{call}.{graph}.stacks"),
                    local
                        .command("perf")
                        .env_remove("DEBUGINFOD_URLS")
                        .args(["script", "--no-inline", "-F", "+period", "-i"])
                        .arg(&input),
                )
                .await?;
            drop(decoded);
            data::name_guests(local.root, &target.join("wasm32-wasip2/release"), &stacks).await?;
            let home = PathBuf::from(std::env::var_os("HOME").context("HOME")?);
            let mut collapse = local.command(
                home.join(".cargo/bin/inferno-collapse-perf")
                    .to_str()
                    .context("collapse tool path")?,
            );
            collapse
                .stdin(std::fs::File::open(&stacks)?)
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true);
            let mut collapse = collapse.spawn()?;
            let pipe = collapse
                .stdout
                .take()
                .context("collapsed stacks")?
                .into_owned_fd()?;
            let mut flamegraph = local.command(
                home.join(".cargo/bin/inferno-flamegraph")
                    .to_str()
                    .context("flamegraph tool path")?,
            );
            flamegraph
                .stdin(std::process::Stdio::from(pipe))
                .stdout(std::fs::File::create(
                    local.work.join(format!("{call}.{graph}.svg")),
                )?)
                .kill_on_drop(true);
            ensure!(
                flamegraph.spawn()?.wait().await?.success() && collapse.wait().await?.success(),
                "render flamegraph"
            );
            for (name, args) in [
                ("self", vec!["--no-children"]),
                ("inclusive", vec!["--children"]),
            ] {
                let text = local
                    .logged(
                        &format!("{call}.{graph}.{name}.txt"),
                        local
                            .command("perf")
                            .env_remove("DEBUGINFOD_URLS")
                            .args(["report", "--stdio", "--no-inline"])
                            .args(args)
                            .args(["--call-graph", "none", "--percent-limit", "0", "-i"])
                            .arg(&input),
                    )
                    .await?;
                drop(text);
            }
            data::profile_summary(
                &stacks,
                &local.work.join(format!("{call}.{graph}.summary.json")),
            )?;
        }
    }
    Ok(())
}

async fn pg_minimum(local: &mut Local<'_>, args: &Args) -> anyhow::Result<()> {
    ensure!(
        args.concurrency <= 1000,
        "1000-order seed supports at most 1000 clients"
    );
    process::run(
        local.root,
        "cargo",
        &[
            "build",
            "--offline",
            "--release",
            "-p",
            "wamn-dev-control",
            "--example",
            "pg_minimum_database",
        ],
    )
    .await?;
    local
        .logged(
            "database.log",
            local
                .command(
                    local
                        .bin
                        .join("examples/pg_minimum_database")
                        .to_str()
                        .context("database builder path")?,
                )
                .arg(&local.database)
                .arg(local.root.join("apps/wamn_receiving")),
        )
        .await?;
    local.seed(1000).await?;
    local
        .sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .await?;
    process::run(local.root, "pgbench", &[&local.database, "-i", "-s", "10"]).await?;
    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(&std::fs::read(
        wamn_compiler::package_manifest_path(&local.root.join("apps/wamn_receiving")),
    )?)?;
    let operation = manifest
        .compiled_operations
        .get("purchase_order.update")
        .context("compiled purchase_order.update")?
        .operation
        .operation
        .as_str();
    for call in ["select-only", "simple-update", "get", "update"] {
        local.sql("SELECT pg_stat_statements_reset()").await?;
        local.profile_start(call).await?;
        let replay = matches!(call, "get" | "update");
        let url = if replay {
            format!(
                "{}?options=-csearch_path%3Dreceiving,public%20-cstatement_timeout%3D5000%20-crole%3Dwamn_app",
                local.database
            )
        } else {
            local.database.clone()
        };
        let mut command = local.command("taskset");
        command
            .args([
                "-c",
                "6-7",
                "pgbench",
                &url,
                "-n",
                "-M",
                "prepared",
                "-c",
                &args.concurrency.to_string(),
                "-j",
                "2",
                "-T",
                &args.duration_secs.to_string(),
                "-r",
                "-l",
                "--log-prefix",
            ])
            .arg(local.work.join(format!("{call}.latencies")));
        for (name, value) in [
            ("clients", args.concurrency.to_string()),
            ("sequence", "0".into()),
            (
                "run",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_secs()
                    .to_string(),
            ),
            ("operation", operation.into()),
            (
                "claim_operation",
                operation
                    .rsplit_once('@')
                    .map_or(operation, |(name, _)| name)
                    .into(),
            ),
            ("statement_timeout", "5000".into()),
            ("schema", "receiving".into()),
            ("principal", "770df186-ac15-579e-b46b-c297cae2011b".into()),
        ] {
            command.args(["-D", &format!("{name}={value}")]);
        }
        if replay {
            command.arg("-f").arg(
                local
                    .root
                    .join(format!("xtask/bench/pg-minimum/{call}.sql")),
            );
        } else {
            command.args(["-b", call]);
        }
        local.logged(&format!("{call}.log"), &mut command).await?;
        local.profile_stop(call).await?;
    }
    std::fs::write(local.work.join("update-counts"),local.sql("SELECT sum(row_version - 1), (SELECT count(*) FROM platform.write_log WHERE result IS NOT NULL) FROM receiving.purchase_order").await?)?;
    data::pg_result(
        &local.work,
        &local.work.join("result.json"),
        args.concurrency,
    )
}

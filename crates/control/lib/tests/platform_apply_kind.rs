//! Install the platform on a private kind cluster and require an unchanged rerun.
//!
//! Requires Docker with buildx manifest reads, kind, Helm, kubectl, pnpm, OpenSSL,
//! package build tools, and debug wamn and wamn-ctl binaries.
//! Set WAMN_RELEASE_CHART to the chart from the local train build.
//! Use an empty DOCKER_CONFIG for anonymous pulls from its loopback registry.
//! The fixture copies all seven images to its registry:2 container and stamps
//! them. It never builds platform images. Public registry access is required.
//! Without WAMN_RELEASE_CHART, the legacy GCP source requires agent credentials.
//! No external database, bucket or DNS is required. The test writes its input,
//! kubeconfig and progress under /var/tmp. A failed run keeps its cluster, both
//! registries, secrets, and fixture directory. A passing full run removes them.
//! A TLS artifact registry serves Receiving, WMS, and their unqualified releases.
//! WMS publishes through the same release Job as Receiving.
//! 4.16.1 writes the WMS integration from deploy/gcp/connections first.
//! WMS stays unqualified and must refuse. The host qualifies Receiving, then
//! the test applies only Receiving through HTTPS checks in 4.21 and requires an unchanged rerun.
//! Before the rerun, the test deletes the first apply completion, as a new
//! platform image changes its digest, and requires that 4.11 keeps the release.
//! Independent checks record their failures and continue. Required setup stops
//! the case if a later check needs it. The final error names every failed check.
//! WAMN_KIND_KEEP=never removes them after either outcome. Other nonempty values
//! fail. A kept run writes private kept.json with resource names, registry ports,
//! completed steps, failed checks, saved inputs, and the cleanup command:
//! tools/kind-kept-delete /absolute/path/to/kept.json.
//! Set WAMN_KIND_REUSE to kept.json and WAMN_KIND_FROM to one name in
//! support/platform_checks.rs::STEPS. Both variables are required together.
//! The case refuses unknown names or missing completed dependencies.
//! Resume skips earlier steps, uses saved release digests and comparison state,
//! and runs the selected step and every later step with ready dependencies.
//! A rerun always keeps the cluster and writes a new kept.json under the fixture.
//! Required failures report dependent steps as not run. Independent checks continue.
//! HTTPS Jobs pull the existing pinned curl image from the public registry.

use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use wamn_control::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_control::kubectl;
use wamn_control::platform::document::ClusterDocument;
use wamn_control::platform::{install, steps};
#[path = "support/platform_images.rs"]
mod platform_images;
use platform_images::stamp_registry;
use wamn_gate_harness::Failures;
use wash_runtime::oci::OciConfig;

#[path = "support/kind_checks.rs"]
mod kind_checks;
#[path = "support/platform_checks.rs"]
mod platform_checks;
use platform_checks::{
    Data, FailedCheck, Fixture, Kept, STEPS, missing, rerun_charts, resume, reuse_from,
    stale_images, unchanged_versions,
};

struct Run {
    kept: Kept,
    path: std::path::PathBuf,
    failures: Failures,
    step: &'static str,
    keep: bool,
    reuse: bool,
    success: bool,
}

impl Run {
    fn record(&mut self, check: &str, result: anyhow::Result<()>) {
        if let Err(error) = &result {
            self.kept
                .failed_step
                .get_or_insert_with(|| self.step.to_owned());
            self.kept.failed.push(FailedCheck {
                step: self.step.to_owned(),
                check: check.to_owned(),
                error: format!("{error:#}"),
            });
        }
        self.failures.record(check, result);
    }

    fn capture<T>(&mut self, check: &str, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.record(check, Err(error));
                None
            }
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)
            .context("open kept.json")?;
        file.write_all(&serde_json::to_vec_pretty(&self.kept)?)
            .context("write kept.json")
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Err(error) = self.save() {
            eprintln!("save kept run {}: {error:#}", self.path.display());
            return;
        }
        if self.reuse || (self.keep && !self.success) {
            eprintln!(
                "kept cluster={} kubeconfig={} failed={:?} cleanup={}",
                self.kept.cluster,
                self.kept.kubeconfig.display(),
                self.kept.failed,
                self.kept.cleanup
            );
            return;
        }
        match Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/kind-kept-delete"))
            .arg(&self.path)
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("{} exited {status}", self.kept.cleanup),
            Err(error) => eprintln!("{}: {error}", self.kept.cleanup),
        }
    }
}

fn suffix() -> anyhow::Result<String> {
    let mut bytes = [0u8; 6];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("generate the cluster suffix"))?;
    Ok(hex::encode(bytes))
}

fn require_absent(kept: &Kept) -> anyhow::Result<()> {
    let output = Command::new("kind")
        .args(["get", "clusters"])
        .stdin(Stdio::null())
        .output()
        .context("list kind clusters before setup")?;
    ensure!(
        output.status.success(),
        "list kind clusters: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|name| name == kept.cluster),
        "cluster {} exists; cleanup: {}",
        kept.cluster,
        kept.cleanup
    );
    for name in [&kept.registry, &kept.artifact_registry] {
        let containers = docker(&["ps", "-a", "--format", "{{.Names}}"])?;
        ensure!(
            !containers.lines().any(|existing| existing == name),
            "registry {name} exists; cleanup: {}",
            kept.cleanup
        );
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: bin:docker, bin:kind, bin:helm, bin:kubectl, bin:pnpm, bin:cargo, bin:rustc, bin:wamn, bin:wamn-ctl, env:WAMN_RELEASE_CHART"]
async fn platform_apply_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:docker, bin:kind, bin:helm, bin:kubectl, bin:pnpm, bin:cargo, bin:rustc, bin:wamn, bin:wamn-ctl, env:WAMN_RELEASE_CHART",
    );
    let keep = kind_checks::keep_on_failure(std::env::var("WAMN_KIND_KEEP").ok().as_deref())?;
    let reuse = std::env::var("WAMN_KIND_REUSE").ok();
    let from = std::env::var("WAMN_KIND_FROM").ok();
    let selection = reuse_from(reuse.as_deref(), from.as_deref())?;
    let (mut kept, path, start) = if let Some((path, start)) = selection {
        let mut kept: Kept =
            serde_json::from_slice(&std::fs::read(path).context("read WAMN_KIND_REUSE kept.json")?)
                .context("parse platform kept.json")?;
        resume(start, &mut kept.completed)?;
        ensure!(
            kept.fixture.is_some(),
            "kept run lacks required fixture setup"
        );
        let directory = kept.directory.join(format!("rerun-{}", suffix()?));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        kept.failed.clear();
        kept.failed_step = None;
        kept.not_run.clear();
        (kept, directory.join("kept.json"), start)
    } else {
        let name = format!("platform-apply-{}", suffix()?);
        let directory = Path::new("/var/tmp").join(&name);
        let work = directory.join("work");
        let path = directory.join("kept.json");
        let kept = Kept {
            cluster: name.clone(),
            kubeconfig: work.join(format!("{name}.kubeconfig")),
            registry: format!("{name}-registry"),
            artifact_registry: format!("{name}-artifacts"),
            artifact_registry_port: 5000,
            directory: directory.clone(),
            work,
            cleanup: format!("tools/kind-kept-delete {}", path.display()),
            ..Kept::default()
        };
        require_absent(&kept)?;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .context("create fixture directory")?;
        (kept, path, 0)
    };
    kept.cleanup = format!("tools/kind-kept-delete {}", path.display());
    let mut run = Run {
        kept,
        path,
        failures: Failures::default(),
        step: STEPS[start].name,
        keep,
        reuse: selection.is_some(),
        success: false,
    };
    let setup = if run.reuse {
        refresh(&run).await
    } else {
        prepare(&mut run).await
    };
    if setup.is_ok() {
        let result = scenario(&mut run, start).await;
        run.record("required setup", result);
    } else {
        run.record("required setup", setup);
        run.kept.not_run = STEPS.iter().map(|step| step.name.to_owned()).collect();
        run.record(
            "not run",
            Err(anyhow::anyhow!("{}", run.kept.not_run.join(", "))),
        );
    }
    let saved = run.save();
    run.record("save kept.json", saved);
    let result = std::mem::take(&mut run.failures).finish();
    run.success = result.is_ok();
    result
}

async fn prepare(run: &mut Run) -> anyhow::Result<()> {
    let name = run.kept.cluster.clone();
    let directory = run.kept.directory.clone();
    let work = run.kept.work.clone();
    std::fs::create_dir(&work).context("create the install work directory")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    println!("work directory {}", directory.display());
    let port = registry(&name)?;
    run.kept.registry_port = port;
    let artifacts = artifact_registry(&name, &directory).await?;
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
    registry = "localhost:{port}"
    buckets = {{web = "", otel = "", state = ""}}
    route_hosts = {{control = "control.example.com"}}
    resend_from = "sender@example.com"
}}
"#
        ),
    )?;
    let document = ClusterDocument::compile(&file).context("compile the kind cluster document")?;
    let chart = fixture_images(
        &repository,
        &directory,
        &document.registry,
        &version,
        &[directory.join("fixture-chart")],
    )
    .await?;
    let environments = directory.join("environments");
    std::fs::create_dir(&environments)?;
    let (source, mut environment) = fixture_environment(&repository, "receiving")?;
    let host_group = format!("host-group-{}.yaml", environment.project);
    std::fs::copy(
        repository.join("deploy/gcp").join(&host_group),
        directory.join(&host_group),
    )?;
    environment.org.clone_from(&document.org);
    environment.release = DeclaredRelease::None;
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .context("control host has a domain")?
        .1;
    environment.route_host = format!("receiving.{domain}");
    let input = environments.join("test--receiving--dev.k");
    std::fs::write(&input, environment.to_kcl())?;
    let mut values: Value = serde_json::from_slice(&std::fs::read(source.with_extension("json"))?)?;
    values["cluster"] = name.clone().into();
    values["context"] = format!("kind-{name}").into();
    values["registry"] = document.registry.clone().into();
    values["route_host"] = environment.route_host.clone().into();
    values["web_client"]["bucket"] = "".into();
    values["gates_image"] = chart["platform"]["gates"]["image"].clone();
    std::fs::write(
        input.with_extension("json"),
        serde_json::to_vec_pretty(&values)?,
    )?;

    let (wms_source, mut wms_environment) = fixture_environment(&repository, "wms")?;
    let connections = directory.join("connections");
    std::fs::create_dir(&connections)?;
    for entry in std::fs::read_dir(repository.join("deploy/gcp/connections"))? {
        let entry = entry?;
        std::fs::copy(entry.path(), connections.join(entry.file_name()))?;
    }
    std::fs::copy(
        repository.join("deploy/gcp/host-group-wms.yaml"),
        directory.join("host-group-wms.yaml"),
    )?;
    wms_environment.org.clone_from(&document.org);
    wms_environment.release = DeclaredRelease::None;
    wms_environment.route_host = format!("wms.{domain}");
    // Keep the unqualified environment first so both refusal runs exercise WMS.
    let wms_input = environments.join("a-unqualified-wms.k");
    std::fs::write(&wms_input, wms_environment.to_kcl())?;
    let mut wms_values: Value =
        serde_json::from_slice(&std::fs::read(wms_source.with_extension("json"))?)?;
    wms_values["cluster"] = name.clone().into();
    wms_values["context"] = format!("kind-{name}").into();
    wms_values["registry"] = document.registry.clone().into();
    wms_values["route_host"] = wms_environment.route_host.clone().into();
    wms_values["web_client"]["bucket"] = "".into();
    wms_values["gates_image"] = chart["platform"]["gates"]["image"].clone();
    std::fs::write(
        wms_input.with_extension("json"),
        serde_json::to_vec_pretty(&wms_values)?,
    )?;

    // The fixture supplies the owner input before the verb starts.
    let kubeconfig = work.join(format!("{name}.kubeconfig"));
    let context = document.provider.kubeconfig(&document, &kubeconfig).await?;
    kubectl::apply(
        &kubeconfig,
        &context,
        "identity",
        &json!({
            "apiVersion":"v1","kind":"Namespace","metadata":{"name":"identity"}
        }),
    )
    .await?;
    kubectl::apply(
        &kubeconfig,
        &context,
        "identity",
        &json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"identity-resend","namespace":"identity"},
            "stringData":{"api-key":"re_platform_fixture"}
        }),
    )
    .await?;

    run.kept.fixture = Some(Fixture {
        version,
        document,
        artifacts,
        environment,
        wms_environment,
        host_group,
    });
    Ok(())
}

/// Copies this checkout's images into the fixture registry and stamps each chart from them.
/// Returns the values of the first chart.
async fn fixture_images(
    repository: &Path,
    directory: &Path,
    registry: &str,
    version: &str,
    charts: &[std::path::PathBuf],
) -> anyhow::Result<Value> {
    let source_chart = platform_images::source_chart(repository, directory)?;
    let source_images = wamn_control::release_chart::image_set(&source_chart).await?;
    let source_registry = source_images
        .host
        .split("/wamn-host")
        .next()
        .context("host registry")?;
    copy_images(source_registry, registry, version).await?;
    let source: Value = serde_yaml::from_slice(&std::fs::read(source_chart.join("values.yaml"))?)?;
    let mut first = None;
    for chart in charts {
        if chart.exists() {
            std::fs::remove_dir_all(chart)?;
        }
        stamp_registry(repository, registry, version, chart)?;
        let values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values.yaml"))?)?;
        let stale = stale_images(&source, &values);
        ensure!(
            stale.is_empty(),
            "{} images differ from {}: {}",
            chart.display(),
            source_chart.display(),
            stale.join(", ")
        );
        first.get_or_insert(values);
    }
    first.context("no chart to stamp")
}

/// A rerun runs this checkout's images, so a merged fix reaches the kept cluster.
async fn refresh(run: &Run) -> anyhow::Result<()> {
    let fixture = run.kept.fixture.as_ref().context("missing fixture setup")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    ensure!(
        version == fixture.version,
        "the kept fixture runs platform {} but this checkout is {version}",
        fixture.version
    );
    let chart = fixture_images(
        &repository,
        &run.kept.directory,
        &fixture.document.registry,
        &version,
        &rerun_charts(&run.kept.directory, &run.kept.work),
    )
    .await?;
    for name in ["test--receiving--dev.json", "a-unqualified-wms.json"] {
        let path = run.kept.directory.join("environments").join(name);
        let mut values: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        values["gates_image"] = chart["platform"]["gates"]["image"].clone();
        std::fs::write(&path, serde_json::to_vec_pretty(&values)?)?;
    }
    Ok(())
}

async fn scenario(run: &mut Run, start: usize) -> anyhow::Result<()> {
    let Fixture {
        version,
        document,
        artifacts,
        mut environment,
        mut wms_environment,
        host_group,
    } = run.kept.fixture.clone().context("missing fixture setup")?;
    let name = run.kept.cluster.clone();
    let directory = run.kept.directory.clone();
    let work = run.kept.work.clone();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let environments = directory.join("environments");
    let input = environments.join("test--receiving--dev.k");
    let wms_input = environments.join("a-unqualified-wms.k");
    let prefix = format!("{}: release = \"", input.display());
    let wms_prefix = format!("{}: release = \"", wms_input.display());
    let kubeconfig = run.kept.kubeconfig.clone();
    let context = format!("kind-{name}");
    let selected = directory.join("qualified-environments");
    let selected_input = selected.join("test--receiving--dev.k");
    let invite_log_path = work.join("jobs/platform-invite-test.log");
    let mut serving = document.clone();
    serving.registry = artifacts
        .artifact_base
        .strip_suffix("/packages")
        .context("fixture package registry suffix")?
        .to_owned();
    let mut progress = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("progress.log"))?;
    let Data {
        mut first,
        mut digest,
        mut wms_digest,
        mut publication_log,
        mut refused_lines,
        mut second,
        mut secrets,
        mut applied,
        mut https_before,
        mut edge,
        mut invite_log,
        mut mail,
        mut invite_modified,
        mut control_secrets,
        mut repeated,
    } = run.kept.data.clone();
    if !digest.is_empty() {
        environment.release = DeclaredRelease::Digest(digest.clone());
    }
    if !wms_digest.is_empty() {
        wms_environment.release = DeclaredRelease::Digest(wms_digest.clone());
    }
    for step in &STEPS[start..] {
        run.step = step.name;
        if let Some(dependency) = missing(step, &run.kept.completed) {
            run.kept.not_run.push(step.name.to_owned());
            run.record(
                "not run",
                Err(anyhow::anyhow!(
                    "{} needs missing completed step {dependency}",
                    step.name
                )),
            );
            continue;
        }
        println!("{}: starting", step.name);
        let result = async {
            match step.name {
                "4.3 through 4.14" => {
                    environment.release = DeclaredRelease::None;
                    wms_environment.release = DeclaredRelease::None;
                    std::fs::write(&input, environment.to_kcl())?;
                    std::fs::write(&wms_input, wms_environment.to_kcl())?;
                    first.clear();
                    let start = Instant::now();
                    println!("platform install start {}", Utc::now().to_rfc3339());
                    let installed = install::apply(
                        &document,
                        &version,
                        &repository,
                        &environments,
                        &work,
                        &artifacts,
                        |line| {
                            println!("{line}");
                            writeln!(progress, "{line}")?;
                            first.push(line.to_owned());
                            Ok(())
                        },
                    )
                    .await;
                    println!(
                        "platform install end {} elapsed {:?} result {}",
                        Utc::now().to_rfc3339(),
                        start.elapsed(),
                        if installed.is_ok() { "pass" } else { "fail" }
                    );
                    installed?;

                    first
                        .iter()
                        .find_map(|line| {
                            line.strip_prefix(&prefix)
                                .and_then(|line| line.strip_suffix('"'))
                        })
                        .context("publication printed no release line")?
                        .clone_into(&mut digest);
                    run.record(
                        "4.11",
                        async {
                            ensure!(
                                EnvironmentDocument::compile(&input)?.release == DeclaredRelease::None,
                                "platform apply changed the environment file"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    first
                        .iter()
                        .find_map(|line| {
                            line.strip_prefix(&wms_prefix)
                                .and_then(|line| line.strip_suffix('"'))
                        })
                        .context("publication printed no release line")?
                        .clone_into(&mut wms_digest);
                    run.record(
                        "4.14",
                        async {
                            ensure!(
                                EnvironmentDocument::compile(&wms_input)?.release == DeclaredRelease::None,
                                "platform apply changed the WMS environment file"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    wms_environment.release = DeclaredRelease::Digest(wms_digest.clone());
                    std::fs::write(&wms_input, wms_environment.to_kcl())?;
                    environment.release = DeclaredRelease::Digest(digest.clone());
                    std::fs::write(&input, environment.to_kcl())?;
                }
                "installed objects and NATS" => {
                    let ready = kubectl::wait(
                        &kubeconfig,
                        &context,
                        "platform",
                        "cluster.postgresql.cnpg.io",
                        "wamn-pg",
                        "condition=Ready",
                        "60s",
                    )
                    .await;
                    run.record("4.5 PostgreSQL ready", ready);
                    let ready = kubectl::kubectl(
                        &kubeconfig,
                        &context,
                        "platform",
                        &[
                            "rollout",
                            "status",
                            "statefulset/wamn-nats",
                            "--timeout=300s",
                        ],
                        None,
                    )
                    .await;
                    run.record("4.4 NATS rollout", ready.map(|_| ()));
                    let result = async {
                        // R4: a client without a certificate is refused, because the server
                        // requires TLS and maps every user from its certificate.
                        let anonymous = kubectl::kubectl(
                            &kubeconfig,
                            &context,
                            "platform",
                            &[
                                "exec",
                                "-i",
                                "wamn-nats-0",
                                "-c",
                                "nats",
                                "--",
                                "nc",
                                "-w",
                                "2",
                                "127.0.0.1",
                                "4222",
                            ],
                            Some(b"CONNECT {}\r\nPING\r\n"),
                        )
                        .await?;
                        ensure!(
                            !anonymous.contains("PONG"),
                            "NATS accepted a connection without a certificate: {anonymous}"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.4 NATS anonymous refusal", result);
                    let result = async {
                        // R1: one NATS. The operator release renders none.
                        let deployments = kubectl::kubectl(
                            &kubeconfig,
                            &context,
                            "platform",
                            &["get", "deployment,statefulset", "-o", "name"],
                            None,
                        )
                        .await?;
                        let nats: Vec<_> = deployments
                            .lines()
                            .filter(|line| line.contains("nats"))
                            .collect();
                        ensure!(
                            nats == ["statefulset.apps/wamn-nats"],
                            "the platform namespace runs another NATS: {nats:?}"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.4 one NATS", result);
                    // R3: cert-manager owns each Secret the operator chart names.
                    for (namespace, name) in [
                        ("platform", "wasmcloud-nats-tls"),
                        ("platform", "wasmcloud-operator-tls"),
                        ("hosts", "wasmcloud-runtime-tls"),
                        ("hosts", "wasmcloud-data-tls"),
                    ] {
                        let result = async {
                            let secret: Value = kubectl::get(&kubeconfig, &context, namespace, "secret", name)
                                .await?
                                .with_context(|| format!("missing Secret {namespace}/{name}"))?;
                            ensure!(
                                secret["metadata"]["annotations"]["cert-manager.io/certificate-name"] == name,
                                "Secret {namespace}/{name} has no owning cert-manager Certificate"
                            );
                            Ok(())
                        }
                        .await;
                        run.record(&format!("4.4 certificate {namespace}/{name}"), result);
                    }
                }
                "4.15 unqualified release refusal" => {
                    if let Some(log) = run.capture(
                        "4.14 publication log",
                        std::fs::read_to_string(work.join("jobs/platform-release-test--wms--dev.log"))
                            .context("read WMS publication log"),
                    ) {
                        publication_log = log;
                    }
                    let result = async {
                        let completed: Value = kubectl::get(
                            &kubeconfig,
                            &context,
                            "platform",
                            "configmap",
                            "wamn-install-platform-release-test--wms--dev",
                        )
                        .await?
                        .context("publication has no completion record")?;
                        ensure!(
                            completed["data"]["output"].as_str().map(str::trim) == Some(wms_digest.as_str()),
                            "publication retained output other than its digest"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.14 publication completion", result);
                    for suffix in ["identity-reader", "control-author", "management-admitter"] {
                        let result = async {
                            let secret: Option<Value> = kubectl::get(
                                &kubeconfig,
                                &context,
                                "identity",
                                "secret",
                                &format!("platform-release-test--wms--dev-{suffix}"),
                            )
                            .await?;
                            ensure!(
                                secret.is_none(),
                                "publication created a retired gate {suffix} Secret"
                            );
                            Ok(())
                        }
                        .await;
                        run.record("4.14 retired gate Secret", result);
                    }
                    refused_lines.clear();
                    let result = async {
                        let refused = install::apply(
                            &document,
                            &version,
                            &repository,
                            &environments,
                            &work,
                            &artifacts,
                            |line| {
                                println!("{line}");
                                writeln!(progress, "{line}")?;
                                refused_lines.push(line.to_owned());
                                Ok(())
                            },
                        )
                        .await
                        .err()
                        .context("an unqualified release must refuse")?;
                        let refusal = format!("{refused:#}");
                        writeln!(progress, "{refusal}")?;
                        Ok(refusal)
                    }
                    .await;
                    let refusal = result.as_ref().ok().cloned().unwrap_or_default();
                    run.record("4.15 unqualified release refusal", result.map(|_| ()));
                    secrets = secret_versions(&kubeconfig, &context, run).await;
                    run.record(
                        "private logs",
                        require_private_logs(&work, &format!("{}\n{refusal}", first.join("\n")), &secrets),
                    );
                    second.clear();
                    let result = async {
                        let repeated = install::apply(
                            &document,
                            &version,
                            &repository,
                            &environments,
                            &work,
                            &artifacts,
                            |line| {
                                println!("{line}");
                                writeln!(progress, "{line}")?;
                                second.push(line.to_owned());
                                Ok(())
                            },
                        )
                        .await
                        .err()
                        .context("the unchanged unqualified release still refuses")?;
                        ensure!(
                            format!("{repeated:#}").contains(&format!("release {wms_digest} has no qualification")),
                            "{repeated:#}"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.15 repeated unqualified release refusal", result);
                }
                "4.16.1 WMS integration" => {
                    // WMS declares wms-labels, so 4.16.1 writes it once, with the empty
                    // credential of its custom auth in the environment's credential Secret.
                    let result = async {
                        let name = wamn_credential_store::kubernetes::secret_name(&document.org, "wms", "dev");
                        ensure!(
                            kubectl::get::<Value>(&kubeconfig, &context, "hosts", "secret", &name)
                                .await?
                                .is_some(),
                            "4.16.1 wrote no credential Secret hosts/{name}"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.16.1 credential Secret", result);
                    run.record(
                        "4.14",
                        async {
                            ensure!(
                                std::fs::read_to_string(work.join("jobs/platform-release-test--wms--dev.log"))?
                                    == publication_log,
                                "unchanged publication ran its release Job again"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    let after = secret_versions(&kubeconfig, &context, run).await;
                    run.record(
                        "4.12 unchanged Secrets after refusal",
                        unchanged_versions(&secrets, &after),
                    );
                }
                "qualify-release Receiving" => {
                    // Qualification stays on the Docker host. Receiving has one required case.
                    let mut releases = artifacts.clone();
                    releases.artifact_base = artifacts
                        .artifact_base
                        .strip_suffix("/packages")
                        .context("fixture package registry suffix")?
                        .to_owned()
                        + "/releases";
                    let started = Instant::now();
                    let target = std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into());
                    let mut command = tokio::process::Command::new(repository.join(target).join("debug/wamn-ctl"));
                    command
                        .current_dir(&repository)
                        .args([
                            "qualify-release",
                            "--release-digest",
                            &digest,
                            "--artifact-base",
                            &releases.artifact_base,
                        ])
                        .arg("--registry-auth-file")
                        .arg(&releases.registry_auth_file)
                        .arg("--release-chart")
                        .arg(work.join("release-chart"));
                    if releases.insecure_registry {
                        command.arg("--insecure-registry");
                    }
                    for ca in &releases.oci_ca_paths {
                        command.arg("--oci-ca-path").arg(ca);
                    }
                    let qualified = wamn_control::owned_command::execute(
                        &mut command,
                        std::time::Duration::from_hours(3),
                        std::time::Duration::from_secs(60),
                    )
                    .await
                    .and_then(|output| {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        eprint!("{stderr}");
                        print!("{}", String::from_utf8_lossy(&output.stdout));
                        ensure!(
                            output.status.success(),
                            "qualify-release exited {}: {stderr}",
                            output.status
                        );
                        Ok(())
                    });
                    let timing = format!(
                        "qualify-release Receiving wall time {:.3}s result {}",
                        started.elapsed().as_secs_f64(),
                        if qualified.is_ok() { "pass" } else { "fail" }
                    );
                    println!("{timing}");
                    let logged = writeln!(progress, "{timing}").context("write qualification timing");
                    run.record("qualification timing", logged);
                    qualified?;
                }
                "4.17 Apply with the release" => {
                    // The HTTP mirror supplies native images to containerd. Workload processes
                    // need the same role digests at the pod-reachable TLS artifact registry.
                    serving_chart(&document, &serving, &work, &artifacts).await?;
                    serving_credentials(&kubeconfig, &context, &directory, &host_group, &artifacts).await?;
                    std::fs::create_dir_all(&selected)?;
                    std::fs::copy(&input, &selected_input)?;
                    std::fs::copy(
                        input.with_extension("json"),
                        selected_input.with_extension("json"),
                    )?;
                    applied.clear();
                    install::apply(
                        &serving,
                        &version,
                        &repository,
                        &selected,
                        &work,
                        &artifacts,
                        |line| {
                            println!("{line}");
                            writeln!(progress, "{line}")?;
                            applied.push(line.to_owned());
                            if line == "4.19.1 Owner invite: starting" {
                                local_owner_mail(&kubeconfig, &context)?;
                            }
                            Ok(())
                        },
                    )
                    .await?;
                    let result = async {
                        let release = wamn_control::environment::analyse::deployed_release(
                            &wamn_control::release_chart::Target {
                                kubeconfig: kubeconfig.clone(),
                                context: context.clone(),
                                namespace: "hosts".into(),
                            },
                            "test--receiving--dev",
                        )
                        .await?;
                        ensure!(
                            release.as_deref() == Some(digest.as_str()),
                            "Receiving serves another release"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.17 deployed release", result);
                }
                "4.18 CDC readers" => {
                    // Capture and its reader exist even without an event registration.
                    for (kind, name) in [
                        ("deployment", "cdc-reader-test--receiving--dev"),
                        ("secret", "wamn-cdc-test--receiving--dev"),
                    ] {
                        run.record(
                            "4.18",
                            async {
                                ensure!(
                                    kubectl::get::<Value>(&kubeconfig, &context, "hosts", kind, name)
                                        .await?
                                        .is_some(),
                                    "Receiving has no {kind} {name} for application capture"
                                );
                                Ok(())
                            }
                            .await,
                        );
                    }
                }
                "4.19 Web" => {}
                "4.19.1 Control host" => {
                    let result = async {
                        let control: Value = kubectl::get(
                            &kubeconfig,
                            &context,
                            "hosts",
                            "deployment",
                            "hostgroup-c-test",
                        )
                        .await?
                        .context("missing control host Deployment")?;
                        ensure!(
                            control["status"]["observedGeneration"] == control["metadata"]["generation"]
                                && control["status"]["updatedReplicas"] == 1
                                && control["status"]["availableReplicas"] == 1,
                            "control host did not roll out"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.19.1 control host rollout", result);
                }
                "4.21 HTTPS" => {
                    https_before = https_state(
                        &kubeconfig,
                        &context,
                        &[&serving.route_hosts.control, &environment.route_host],
                        run,
                    )
                    .await?;
                }
                "4.20 Rust edge" => {
                    let snapshot = async {
                        kubectl::get(&kubeconfig, &context, "edge", "deployment", "wamn-edge-web")
                            .await?
                            .context("missing Rust edge Deployment")
                    }
                    .await;
                    if let Some(snapshot) = run.capture("4.20 Rust edge Deployment", snapshot) {
                        edge = snapshot;
                    }
                    run.record(
                        "4.20",
                        async {
                            ensure!(
                                edge["status"]["observedGeneration"] == edge["metadata"]["generation"]
                                    && edge["status"]["updatedReplicas"] == 1
                                    && edge["status"]["availableReplicas"] == 1,
                                "Rust edge did not roll out"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    let result = async {
                        let control_row: Value =
                            kubectl::get(&kubeconfig, &context, "hosts", "configmap", "c-test")
                                .await?
                                .context("missing chart-owned control release row")?;
                        let control_row: Value = serde_json::from_str(
                            control_row["data"]["release.json"]
                                .as_str()
                                .context("control release JSON")?,
                        )?;
                        ensure!(
                            control_row["platformVersion"] == version
                                && control_row["host"] == document.route_hosts.control,
                            "control release row does not select the installed platform UI"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.19.1 control release row", result);
                }
                "4.19.1 Owner invite" => {
                    if let Some(snapshot) = run.capture(
                        "4.19.1 invite log",
                        std::fs::read_to_string(&invite_log_path).context("read invite log"),
                    ) {
                        invite_log = snapshot;
                    }
                    let snapshot = owner_mail(&kubeconfig, &context).await;
                    if let Some(snapshot) = run.capture("4.19.1 owner mail", snapshot) {
                        mail = snapshot;
                    }
                    invite_modified = run.capture(
                        "4.19.1 invite modification time",
                        (|| Ok(std::fs::metadata(&invite_log_path)?.modified()?))(),
                    );
                    run.record(
                        "4.19.1",
                        async {
                            ensure!(
                                mail.lines().count() == 1,
                                "the owner did not receive exactly one invitation"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    secrets = secret_versions(&kubeconfig, &context, run).await;
                    control_secrets = control_secret_versions(&kubeconfig, &context, run).await;
                }
                "unchanged rerun / 4.11 keeps the release" => {
                    // A new platform image changes the first apply digest. Without its
                    // completion, the rerun must still keep the release that 4.17 applied.
                    let deleted = Command::new("kubectl")
                        .arg("--kubeconfig")
                        .arg(&kubeconfig)
                        .args(["--context", &context, "--namespace", "platform"])
                        .args([
                            "delete",
                            "configmap",
                            "wamn-install-platform-first-apply-test--receiving--dev",
                            "--ignore-not-found",
                        ])
                        .stdin(Stdio::null())
                        .output()
                        .context("delete the first apply completion")?;
                    ensure!(
                        deleted.status.success(),
                        "delete the first apply completion: {}",
                        String::from_utf8_lossy(&deleted.stderr)
                    );
                    repeated.clear();
                    install::apply(
                        &serving,
                        &version,
                        &repository,
                        &selected,
                        &work,
                        &artifacts,
                        |line| {
                            println!("{line}");
                            writeln!(progress, "{line}")?;
                            repeated.push(line.to_owned());
                            Ok(())
                        },
                    )
                    .await?;
                    let result = async {
                        let release = wamn_control::environment::analyse::deployed_release(
                            &wamn_control::release_chart::Target {
                                kubeconfig: kubeconfig.clone(),
                                context: context.clone(),
                                namespace: "hosts".into(),
                            },
                            "test--receiving--dev",
                        )
                        .await?;
                        ensure!(
                            release.as_deref() == Some(digest.as_str()),
                            "the rerun did not keep the applied release"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.11 keeps the applied release", result);
                }
                "4.21 unchanged HTTPS state" => {
                    let result = async {
                        let https_after = https_state(
                            &kubeconfig,
                            &context,
                            &[&serving.route_hosts.control, &environment.route_host],
                            run,
                        )
                        .await?;
                        ensure!(
                            https_after == https_before,
                            "unchanged apply rewrote the proxy or reran HTTPS checks"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.21 unchanged HTTPS state", result);
                }
                "4.20 unchanged edge Deployment" => {
                    let result = async {
                        let edge_after: Value =
                            kubectl::get(&kubeconfig, &context, "edge", "deployment", "wamn-edge-web")
                                .await?
                                .context("Rust edge Deployment disappeared")?;
                        ensure!(
                            edge_after["metadata"]["resourceVersion"] == edge["metadata"]["resourceVersion"],
                            "unchanged apply rewrote the Rust edge Deployment"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.20 unchanged edge Deployment", result);
                }
                "4.19.1 unchanged invitation and Secrets" => {
                    run.record(
                        "4.19.1",
                        async {
                            ensure!(
                                std::fs::read_to_string(&invite_log_path)? == invite_log
                                    && Some(std::fs::metadata(&invite_log_path)?.modified()?) == invite_modified,
                                "unchanged apply reran the invite Job"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    run.record(
                        "4.19.1",
                        async {
                            ensure!(
                                owner_mail(&kubeconfig, &context).await? == mail,
                                "unchanged apply sent another invitation"
                            );
                            Ok(())
                        }
                        .await,
                    );
                    let after = control_secret_versions(&kubeconfig, &context, run).await;
                    run.record(
                        "4.19.1 unchanged control Secrets",
                        unchanged_versions(&control_secrets, &after),
                    );
                }
                "remaining checks" => {
                    let result = async {
                        let edge_check: Option<Value> = kubectl::get(
                            &kubeconfig,
                            &context,
                            "edge",
                            "configmap",
                            "wamn-install-platform-edge-check",
                        )
                        .await?;
                        ensure!(
                            edge_check.is_none() && !work.join("jobs/platform-edge-check.log").exists(),
                            "kind started a bucket check Job"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.20 no bucket completion", result);
                    let result = async {
                        let edge_jobs: Value = serde_json::from_str(
                            &kubectl::kubectl(
                                &kubeconfig,
                                &context,
                                "edge",
                                &["get", "jobs", "-o", "json"],
                                None,
                            )
                            .await?,
                        )?;
                        ensure!(
                            !edge_jobs["items"]
                                .as_array()
                                .context("edge Jobs")?
                                .iter()
                                .any(|job| job["metadata"]["name"]
                                    .as_str()
                                    .is_some_and(|name| name.starts_with("platform-edge-check"))),
                            "kind retains a bucket check Job"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.20 no bucket Job", result);
                    for name in [
                        "platform-control-credentials-test-files",
                        "platform-control-operator-test-files",
                        "platform-invite-test-files",
                        "platform-owner-state-test-files",
                    ] {
                        let result = async {
                            let secret: Option<Value> =
                                kubectl::get(&kubeconfig, &context, "platform", "secret", name).await?;
                            ensure!(
                                secret.is_none(),
                                "the Job-owned Secret {name} remains after deletion"
                            );
                            Ok(())
                        }
                        .await;
                        run.record("4.19.1 temporary Secret deletion", result);
                    }
                    let result = async {
                        let ui_completion: Option<Value> = kubectl::get(
                            &kubeconfig,
                            &context,
                            "platform",
                            "configmap",
                            "wamn-install-platform-ui-test",
                        )
                        .await?;
                        ensure!(
                            ui_completion.is_none() && !work.join("jobs/platform-ui-test.log").exists(),
                            "kind started a platform UI Job"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.19.1 no platform UI Job", result);
                    let result = async {
                        let jobs: Value = serde_json::from_str(
                            &kubectl::kubectl(
                                &kubeconfig,
                                &context,
                                "hosts",
                                &["get", "jobs", "-o", "json"],
                                None,
                            )
                            .await?,
                        )?;
                        ensure!(
                            !jobs["items"]
                                .as_array()
                                .context("Jobs have no items")?
                                .iter()
                                .any(|job| job["metadata"]["name"]
                                    .as_str()
                                    .is_some_and(|name| name.starts_with("platform-web-"))),
                            "kind started a web Job"
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.19 no web Job", result);
                    let result = async {
                        let completion: Option<Value> = kubectl::get(
                            &kubeconfig,
                            &context,
                            "platform",
                            "configmap",
                            "wamn-install-platform-web-test--receiving--dev",
                        )
                        .await?;
                        ensure!(completion.is_none(), "kind completed a web upload Job");
                        Ok(())
                    }
                    .await;
                    run.record("4.19 no upload completion", result);
                }
                "4.12 unchanged Secrets" => {
                    let after = secret_versions(&kubeconfig, &context, run).await;
                    run.record(
                        "4.12 unchanged Secrets after qualified apply",
                        unchanged_versions(&secrets, &after),
                    );
                }
                "private logs" => {
                    let result = async {
                        let logs = std::fs::read_to_string(directory.join("progress.log"))?;
                        run.record("private logs", require_private_logs(&work, &logs, &secrets));
                        run.record(
                            "private control logs",
                            require_private_logs(&work, &logs, &control_secrets),
                        );
                        let registry_logs = async {
                            let registry_secret = kubectl::get(
                                &kubeconfig,
                                &context,
                                "hosts",
                                "secret",
                                "platform-fixture-registry",
                            )
                            .await?
                            .context("fixture registry Secret")?;
                            require_private_logs(
                                &work,
                                &logs,
                                &[("platform-fixture-registry".into(), registry_secret)],
                            )
                        }
                        .await;
                        run.record("private registry logs", registry_logs);
                        let invitation: Value = serde_json::from_str(mail.trim())
                            .map_err(|_| anyhow::anyhow!("invalid local invitation mail"))?;
                        let invitation_text = invitation["text"]
                            .as_str()
                            .context("invitation has no text")?;
                        let mut all_logs = logs.clone();
                        for entry in std::fs::read_dir(work.join("jobs"))? {
                            all_logs.push_str(&std::fs::read_to_string(entry?.path())?);
                        }
                        run.record(
                            "4.19.1",
                            async {
                                ensure!(
                                    !all_logs.contains(invitation_text),
                                    "invitation mail reached a log"
                                );
                                Ok(())
                            }
                            .await,
                        );
                        let code = invitation_text
                            .lines()
                            .find_map(|line| line.strip_prefix("Invitation code: "))
                            .context("local invitation has no code")?;
                        let (_, secret) = code
                            .split_once(':')
                            .context("local invitation has no secret")?;
                        run.record(
                            "4.19.1",
                            async {
                                ensure!(
                                    !all_logs.contains(code) && !all_logs.contains(secret),
                                    "an invitation code or secret reached a log"
                                );
                                Ok(())
                            }
                            .await,
                        );
                        Ok(())
                    }
                    .await;
                    run.record("4.19.1 private logs", result);
                }
                _ => unreachable!("the step list owns dispatch"),
            }
            Ok(())
        }.await;
        if result.is_ok() {
            run.kept.completed.push(step.name.to_owned());
        }
        run.record(step.name, result);
        run.kept.data = Data {
            first: first.clone(),
            digest: digest.clone(),
            wms_digest: wms_digest.clone(),
            publication_log: publication_log.clone(),
            refused_lines: refused_lines.clone(),
            second: second.clone(),
            secrets: secrets.clone(),
            applied: applied.clone(),
            https_before: https_before.clone(),
            edge: edge.clone(),
            invite_log: invite_log.clone(),
            mail: mail.clone(),
            invite_modified,
            control_secrets: control_secrets.clone(),
            repeated: repeated.clone(),
        };
        let saved = run.save();
        run.record("save kept.json", saved);
    }
    Ok(())
}

// The released identity binary already supports private local mail. Select it
// after the install upgrades identity and before the first invitation.
fn local_owner_mail(kubeconfig: &Path, context: &str) -> anyhow::Result<()> {
    let run = |args: &[&str]| -> anyhow::Result<Vec<u8>> {
        let output = Command::new("kubectl")
            .arg("--kubeconfig")
            .arg(kubeconfig)
            .args(["--context", context, "--namespace", "identity"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("configure fixture mail")?;
        ensure!(
            output.status.success(),
            "fixture mail command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    };
    let deployment: Value =
        serde_json::from_slice(&run(&["get", "deployment", "identity", "-o", "json"])?)?;
    let mut args = deployment["spec"]["template"]["spec"]["containers"][0]["args"].clone();
    if !args
        .as_array()
        .context("identity arguments")?
        .iter()
        .any(|arg| arg == "--local-mail")
    {
        args.as_array_mut().context("identity arguments")?.extend([
            json!("--local-mail"),
            json!("/var/run/wamn-fixture-mail/mail.jsonl"),
        ]);
        let patch = json!([
            {"op": "replace", "path": "/spec/template/spec/containers/0/args", "value": args},
            {"op": "add", "path": "/spec/template/spec/containers/0/volumeMounts/-", "value": {
                "name": "fixture-mail", "mountPath": "/var/run/wamn-fixture-mail"
            }},
            {"op": "add", "path": "/spec/template/spec/volumes/-", "value": {
                "name": "fixture-mail", "emptyDir": {"medium": "Memory"}
            }}
        ]);
        run(&[
            "patch",
            "deployment",
            "identity",
            "--type=json",
            "--patch",
            &serde_json::to_string(&patch)?,
        ])?;
    }
    run(&["rollout", "status", "deployment/identity", "--timeout=300s"])?;
    Ok(())
}

async fn owner_mail(kubeconfig: &Path, context: &str) -> anyhow::Result<String> {
    kubectl::kubectl(
        kubeconfig,
        context,
        "identity",
        &[
            "exec",
            "deployment/identity",
            "--",
            "cat",
            "/var/run/wamn-fixture-mail/mail.jsonl",
        ],
        Some(&[]),
    )
    .await
}

async fn control_secret_versions(
    kubeconfig: &Path,
    context: &str,
    run: &mut Run,
) -> Vec<(String, Value)> {
    let mut secrets = Vec::new();
    for name in [
        "wamn-control-test",
        "wamn-control-administration-test",
        "operator-test",
    ] {
        let result = async {
            kubectl::get::<Value>(kubeconfig, context, "hosts", "secret", name)
                .await?
                .with_context(|| format!("missing control Secret {name}"))
        }
        .await;
        if let Some(mut secret) = run.capture(&format!("4.19.1 control Secret {name}"), result) {
            // Kubernetes omits data on the first-install empty administration Secret.
            if secret.get("data").is_none() {
                secret["data"] = json!({});
            }
            secrets.push((name.into(), secret));
        }
    }
    secrets
}

async fn serving_chart(
    source: &ClusterDocument,
    serving: &ClusterDocument,
    work: &Path,
    artifacts: &wamn_control::package_artifact::PackageRegistry,
) -> anyhow::Result<()> {
    use wamn_control::release_chart;
    let chart = work.join("release-chart");
    let saved = work.join("qualification-chart");
    if !saved.exists() {
        std::fs::rename(&chart, &saved).context("save the qualification chart")?;
    }
    let original = release_chart::image_set(&saved).await?;
    let mut images = original.clone();
    let mut qualification = release_chart::qualification_images(&saved).await?;
    for (role, reference) in [
        ("http", &images.http),
        ("materializer", &images.materializer),
    ] {
        copy_role(
            reference,
            &source.registry,
            &serving.registry,
            role,
            &artifacts.oci_ca_paths,
        )
        .await?;
    }
    // The serving registry has a path, as cluster.k's does. The HTTP mirror
    // serves containerd the native images under that path too.
    let (_, path) = serving
        .registry
        .split_once('/')
        .context("fixture serving registry path")?;
    for image in [
        &images.host,
        &qualification.runner,
        &qualification.gates,
        &qualification.identity,
    ] {
        let tagged = image.split('@').next().unwrap_or(image);
        let rest = tagged
            .strip_prefix(&source.registry)
            .context("fixture image is outside its source registry")?;
        let mirrored = format!("{}/{path}{rest}", source.registry);
        docker(&["tag", tagged, &mirrored])?;
        docker(&["push", &mirrored])?;
    }
    for image in [
        &mut images.host,
        &mut images.http,
        &mut images.materializer,
        &mut qualification.runner,
        &mut qualification.gates,
        &mut qualification.identity,
    ] {
        *image = format!(
            "{}{}",
            serving.registry,
            image
                .strip_prefix(&source.registry)
                .context("fixture image is outside its source registry")?
        );
    }
    ensure!(
        wamn_control::delivery::qualification_artifact::image_set_digests(&original)?
            == wamn_control::delivery::qualification_artifact::image_set_digests(&images)?,
        "fixture registry copy changed the qualification key"
    );
    release_chart::stamp(&saved, &images, Some(&qualification), &chart)?;
    // The image set holds no edge image, so the stamp keeps the built one.
    // Mirror it like the host and point the chart at the serving registry.
    let values_path = chart.join("values.yaml");
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(&values_path)?)?;
    let edge = values["platform"]["edge"]["image"]
        .as_str()
        .context("fixture chart edge image")?
        .to_owned();
    let rest = edge
        .strip_prefix(&source.registry)
        .context("fixture edge image is outside its source registry")?;
    let tagged = edge.split('@').next().unwrap_or(&edge);
    let mirrored = format!(
        "{}/{path}{}",
        source.registry,
        rest.split('@').next().unwrap_or(rest)
    );
    docker(&["tag", tagged, &mirrored])?;
    docker(&["push", &mirrored])?;
    values["platform"]["edge"]["image"] = format!("{}{rest}", serving.registry).into();
    std::fs::write(&values_path, serde_yaml::to_string(&values)?)?;
    let mirrors = work.join(format!("{}.registry", source.cluster_name));
    let destination = mirrors.join(
        serving
            .registry
            .split('/')
            .next()
            .context("fixture serving registry host")?,
    );
    std::fs::create_dir_all(&destination)?;
    std::fs::copy(
        mirrors.join(&source.registry).join("hosts.toml"),
        destination.join("hosts.toml"),
    )?;
    Ok(())
}

// Preserve manifest bytes, including the config descriptor, so the qualification
// applies to exactly the same role digests at the pod-reachable TLS address.
async fn copy_role(
    source: &str,
    from: &str,
    to: &str,
    role: &str,
    ca_paths: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
    use oci_client::manifest::{IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
    use oci_client::secrets::RegistryAuth;
    use oci_client::{Client, Reference, RegistryOperation};
    let client = Client::try_from(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(vec![from.into()]),
        extra_root_certificates: ca_paths
            .iter()
            .map(|path| {
                Ok(Certificate {
                    encoding: CertificateEncoding::Pem,
                    data: std::fs::read(path)?,
                })
            })
            .collect::<anyhow::Result<_>>()?,
        ..ClientConfig::default()
    })?;
    let source: Reference = source.parse()?;
    let (bytes, digest) = client
        .pull_manifest_raw(
            &source,
            &RegistryAuth::Anonymous,
            &[OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE],
        )
        .await?;
    ensure!(
        source.digest() == Some(digest.as_str()),
        "role registry returned another manifest"
    );
    ensure!(
        format!(
            "sha256:{}",
            hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes))
        ) == digest,
        "role manifest digest mismatch"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)?;
    let (host, path) = to.split_once('/').context("serving registry path")?;
    let target = Reference::with_digest(host.into(), format!("{path}/wamn/{role}"), digest);
    client
        .auth(&target, &RegistryAuth::Anonymous, RegistryOperation::Push)
        .await?;
    for blob in std::iter::once(&manifest.config).chain(&manifest.layers) {
        let mut data = Vec::new();
        client.pull_blob(&source, blob, &mut data).await?;
        client.push_blob(&target, data, &blob.digest).await?;
    }
    client
        .push_manifest_raw(
            &target,
            bytes,
            manifest
                .media_type
                .as_deref()
                .unwrap_or(OCI_IMAGE_MEDIA_TYPE)
                .parse()?,
        )
        .await?;
    Ok(())
}

async fn serving_credentials(
    kubeconfig: &Path,
    context: &str,
    directory: &Path,
    host_group: &str,
    artifacts: &wamn_control::package_artifact::PackageRegistry,
) -> anyhow::Result<()> {
    kubectl::apply(
        kubeconfig,
        context,
        "hosts",
        &json!({
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": "platform-fixture-registry", "namespace": "hosts"},
            "stringData": {
                "config.json": std::fs::read_to_string(&artifacts.registry_auth_file)?,
                "ca.crt": std::fs::read_to_string(&artifacts.oci_ca_paths[0])?
            }
        }),
    )
    .await?;
    let path = directory.join(host_group);
    let mut group: Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    let env = group["env"]
        .as_array_mut()
        .context("host group environment")?;
    env.retain(|value| {
        value["name"] != "WAMN_REGISTRY_TOKEN_METADATA"
            && value["name"] != "WAMN_REGISTRY_AUTH_FILE"
    });
    env.push(json!({"name": "WAMN_REGISTRY_AUTH_FILE", "value": "/etc/wamn/registry/config.json"}));
    for volume in group["volumes"]
        .as_array_mut()
        .context("host group volumes")?
    {
        if volume["name"] == "registry-pull" {
            *volume = json!({"name": "registry-pull", "secret": {"secretName": "platform-fixture-registry"}});
        }
    }
    group["ociCaPaths"] = json!(["/etc/wamn/registry/ca.crt"]);
    std::fs::write(path, serde_yaml::to_string(&group)?)?;
    Ok(())
}

async fn https_state(
    kubeconfig: &Path,
    context: &str,
    hosts: &[&str],
    run: &mut Run,
) -> anyhow::Result<Vec<Value>> {
    let mut versions = Vec::new();
    let proxy = async {
        kubectl::get::<Value>(kubeconfig, context, "edge", "deployment", "wamn-edge")
            .await?
            .context("missing nginx Deployment")
    }
    .await;
    if let Some(proxy) = run.capture("4.21 nginx Deployment", proxy) {
        versions.push(proxy["metadata"]["resourceVersion"].clone());
        run.record(
            "4.21 nginx rollout",
            (|| {
                ensure!(
                    proxy["status"]["observedGeneration"] == proxy["metadata"]["generation"]
                        && proxy["status"]["availableReplicas"] == 1,
                    "nginx did not roll out"
                );
                Ok(())
            })(),
        );
        let pod = &proxy["spec"]["template"]["spec"];
        for (kind, name) in [("secret", "wamn-edge-tls"), ("configMap", "identity-ca")] {
            run.record(
                &format!("4.21 nginx mount {name}"),
                (|| {
                    let key = if kind == "secret" {
                        "secretName"
                    } else {
                        "name"
                    };
                    ensure!(
                        pod["volumes"]
                            .as_array()
                            .context("nginx volumes")?
                            .iter()
                            .any(|volume| volume[kind][key] == name),
                        "nginx does not mount {name}"
                    );
                    Ok(())
                })(),
            );
        }
    }
    let service = async {
        kubectl::get::<Value>(kubeconfig, context, "edge", "service", "wamn-edge")
            .await?
            .context("missing nginx Service")
    }
    .await;
    if let Some(service) = run.capture("4.21 nginx Service", service) {
        run.record(
            "4.21 kind NEG refusal",
            (|| {
                ensure!(
                    service["metadata"]["annotations"]["cloud.google.com/neg"].is_null(),
                    "kind requests a GCP NEG"
                );
                Ok(())
            })(),
        );
        run.record(
            "4.21 HTTPS port",
            (|| {
                ensure!(
                    service["spec"]["ports"][0]["port"] == 443,
                    "nginx Service lacks HTTPS"
                );
                Ok(())
            })(),
        );
    }
    let serial = async {
        let secret: Value = kubectl::get(kubeconfig, context, "edge", "secret", "wamn-edge-tls")
            .await?
            .context("missing edge certificate")?;
        let certificate = STANDARD.decode(
            secret["data"]["tls.crt"]
                .as_str()
                .context("edge certificate data")?,
        )?;
        let mut openssl = Command::new("openssl")
            .args(["x509", "-noout", "-serial"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        openssl
            .stdin
            .take()
            .context("openssl stdin")?
            .write_all(&certificate)?;
        let output = openssl.wait_with_output()?;
        ensure!(
            output.status.success(),
            "openssl could not read the edge certificate"
        );
        Ok(String::from_utf8(output.stdout)?
            .trim()
            .strip_prefix("serial=")
            .context("openssl certificate serial")?
            .to_ascii_lowercase())
    }
    .await;
    let serial = run.capture("4.21 certificate serial", serial);
    let completion = async {
        kubectl::get::<Value>(
            kubeconfig,
            context,
            "platform",
            "configmap",
            "wamn-install-platform-https-check",
        )
        .await?
        .context("missing HTTPS completion")
    }
    .await;
    if let Some(completion) = run.capture("4.21 HTTPS completion", completion) {
        versions.push(completion["metadata"]["resourceVersion"].clone());
        run.record(
            "4.21 kind skips",
            (|| {
                ensure!(
                    completion["data"]["load_balancer"] == "skipped: kind"
                        && completion["data"]["web"] == "skipped: no bucket",
                    "HTTPS completion lacks kind skips"
                );
                Ok(())
            })(),
        );
        let results = (|| -> anyhow::Result<Vec<Value>> {
            Ok(serde_json::from_str(
                completion["data"]["results"]
                    .as_str()
                    .context("HTTPS results")?,
            )?)
        })();
        if let Some(results) = run.capture("4.21 HTTPS results", results) {
            for host in hosts {
                let result = results
                    .iter()
                    .find(|result| result["host"].as_str() == Some(*host))
                    .with_context(|| format!("no HTTPS result for {host}"));
                let Some(result) = run.capture(&format!("4.21 HTTPS result {host}"), result) else {
                    continue;
                };
                run.record(
                    &format!("4.21 HTTPS API {host}"),
                    (|| {
                        ensure!(
                            result["status"] == 401
                                && result["path"]
                                    .as_str()
                                    .is_some_and(|path| path.starts_with("/api/")),
                            "HTTPS API mismatch for {host}"
                        );
                        Ok(())
                    })(),
                );
                if let Some(serial) = &serial {
                    run.record(
                        &format!("4.21 HTTPS certificate {host}"),
                        (|| {
                            ensure!(
                                result["serial"].as_str() == Some(serial),
                                "HTTPS certificate mismatch for {host}"
                            );
                            Ok(())
                        })(),
                    );
                }
                let key = wamn_control::platform::job::input_digest(&json!([
                    host,
                    result["method"],
                    result["path"]
                ]));
                let job_name = format!("platform-https-{}", &key[..16]);
                let record = async {
                    kubectl::get::<Value>(
                        kubeconfig,
                        context,
                        "platform",
                        "configmap",
                        &format!("wamn-install-{job_name}"),
                    )
                    .await?
                    .context("missing HTTPS Job completion")
                }
                .await;
                if let Some(record) =
                    run.capture(&format!("4.21 HTTPS Job completion {host}"), record)
                {
                    versions.push(record["metadata"]["resourceVersion"].clone());
                }
                let absent = async {
                    let job: Option<Value> =
                        kubectl::get(kubeconfig, context, "edge", "job", &job_name).await?;
                    ensure!(job.is_none(), "completed HTTPS Job remains");
                    Ok(())
                }
                .await;
                run.record(&format!("4.21 completed HTTPS Job {host}"), absent);
            }
        }
    }
    ensure!(
        versions.len() == hosts.len() + 2,
        "HTTPS comparison state is incomplete"
    );
    Ok(versions)
}

fn require_private_logs(
    work: &Path,
    progress: &str,
    secrets: &[(String, Value)],
) -> anyhow::Result<()> {
    let mut logs = progress.to_owned();
    for entry in std::fs::read_dir(work.join("jobs"))? {
        logs.push_str(&std::fs::read_to_string(entry?.path())?);
    }
    let mut failures = Failures::default();
    failures.record(
        "PAT in logs",
        (|| {
            ensure!(
                !logs.contains(wamn_session::PAT_TOKEN_PREFIX),
                "a PAT reached the install progress or Job logs"
            );
            Ok(())
        })(),
    );
    failures.record(
        "database credentials in logs",
        (|| {
            ensure!(
                !database_userinfo(&logs),
                "a database URL with credentials reached the install progress or Job logs"
            );
            Ok(())
        })(),
    );
    for (name, secret) in secrets {
        let Some(data) = secret["data"].as_object() else {
            failures.record(name, Err(anyhow::anyhow!("Secret has no data")));
            continue;
        };
        for (key, encoded) in data {
            failures.record(
                &format!("{name}/{key}"),
                (|| {
                    let encoded = encoded.as_str().context("Secret data is not text")?;
                    let decoded = STANDARD.decode(encoded).context("decode fixture Secret")?;
                    let value =
                        std::str::from_utf8(&decoded).context("fixture Secret is not UTF-8")?;
                    let mut private = Vec::new();
                    if matches!(
                        key.as_str(),
                        "password" | "api-key" | "url" | "uri" | "tls.key"
                    ) {
                        private.push(value.to_owned());
                        if key == "url" {
                            private_url_password(value, &mut private);
                        }
                    } else if matches!(
                        key.as_str(),
                        "binding.json" | "target.json" | "authorization.conf" | "config.json"
                    ) {
                        let value: Value = serde_json::from_str(
                            value.strip_prefix("authorization: ").unwrap_or(value),
                        )?;
                        private_values(&value, &mut private);
                    }
                    for value in private {
                        ensure!(
                            !logs.contains(&value) && !logs.contains(&STANDARD.encode(&value)),
                            "a credential reached the install progress or Job logs"
                        );
                    }
                    Ok(())
                })(),
            );
        }
    }
    failures.finish()
}

fn private_values(value: &Value, values: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "password" | "database_url") {
                    if let Some(value) = value.as_str() {
                        values.push(value.to_owned());
                        if key == "database_url" {
                            private_url_password(value, values);
                        }
                    }
                } else {
                    private_values(value, values);
                }
            }
        }
        Value::Array(array) => array.iter().for_each(|value| private_values(value, values)),
        _ => {}
    }
}

// The gate sidecar logs its sanitized endpoint, so only a URL with userinfo is a leak.
fn database_userinfo(logs: &str) -> bool {
    ["postgresql://", "postgres://"].iter().any(|scheme| {
        logs.split(scheme).skip(1).any(|rest| {
            rest.split(|c: char| c == '/' || c.is_whitespace())
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    })
}

fn private_url_password(value: &str, values: &mut Vec<String>) {
    if let Ok(url) = url::Url::parse(value)
        && let Some(password) = url.password()
    {
        values.push(password.to_owned());
    }
}

async fn secret_versions(kubeconfig: &Path, context: &str, run: &mut Run) -> Vec<(String, Value)> {
    let mut secrets = Vec::new();
    for (namespace, name) in [
        ("platform", "wamn-pg-superuser"),
        ("platform", "wamn-nats-authorization"),
        ("hosts", "wamn-nats-test--receiving--dev"),
        ("identity", "wamn-session-role-reader-test--receiving--dev"),
        ("identity", "identity-resend"),
        ("identity", "wamn-identity-db"),
    ] {
        let result = async {
            kubectl::get::<Value>(kubeconfig, context, namespace, "secret", name)
                .await?
                .with_context(|| format!("missing Secret {namespace}/{name}"))
        }
        .await;
        if let Some(secret) = run.capture(&format!("4.12 Secret {namespace}/{name}"), result) {
            secrets.push((format!("{namespace}/{name}"), secret));
        }
    }
    secrets
}

async fn copy_images(source: &str, destination: &str, version: &str) -> anyhow::Result<()> {
    for name in [
        "wamn-host",
        "wamn-runner",
        "wamn-gates",
        "wamn-identity",
        "wamn-edge-web",
    ] {
        let from = format!("{source}/{name}:{version}");
        let to = format!("{destination}/{name}:{version}");
        docker(&["pull", &from])?;
        docker(&["tag", &from, &to])?;
        docker(&["push", &to])?;
    }
    for role in ["http", "materializer"] {
        let from = format!("{source}/wamn/{role}:{version}");
        let to = format!("{destination}/wamn/{role}:{version}");
        platform_images::copy_role(&from, &to, OciConfig::new_insecure()).await?;
    }
    Ok(())
}

async fn artifact_registry(
    cluster: &str,
    directory: &Path,
) -> anyhow::Result<wamn_control::package_artifact::PackageRegistry> {
    let certs = directory.join("artifact-registry");
    std::fs::DirBuilder::new().mode(0o700).create(&certs)?;
    let name = format!("{cluster}-artifacts");
    certificate(&certs, "127.0.0.1")?;
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "kind",
        "-v",
        &format!("{}:/certs:ro", certs.display()),
        "-e",
        "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/tls.crt",
        "-e",
        "REGISTRY_HTTP_TLS_KEY=/certs/tls.key",
        "registry:2",
    ])?;
    let address = docker(&[
        "inspect",
        "--format",
        "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
        &name,
    ])?
    .trim()
    .to_owned();
    certificate(&certs, &address)?;
    docker(&["restart", &name])?;
    let authority = format!("{address}:5000");
    let mut password = [0u8; 32];
    SystemRandom::new()
        .fill(&mut password)
        .map_err(|_| anyhow::anyhow!("generate registry fixture credential"))?;
    let auth = certs.join("config.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(&json!({"auths": {&authority: {
            "username": "fixture", "password": hex::encode(password)
        }}}))?,
    )?;
    let certificate = reqwest::Certificate::from_pem(&std::fs::read(certs.join("ca.crt"))?)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(certificate)
        .build()?;
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if client
            .get(format!("https://{authority}/v2/"))
            .send()
            .await
            .is_ok_and(|reply| reply.status().is_success())
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "artifact registry did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Ok(wamn_control::package_artifact::PackageRegistry {
        // Qualification needs <registry>/<path>/releases (delivery::sibling_bases).
        artifact_base: format!("{authority}/wamn/packages"),
        registry_auth_file: auth,
        insecure_registry: false,
        oci_ca_paths: vec![certs.join("ca.crt")],
    })
}

// rustls refuses a CA certificate as the server certificate, so the fixture
// signs a CA:FALSE server certificate with its own CA.
fn certificate(directory: &Path, address: &str) -> anyhow::Result<()> {
    let ca = directory.join("ca.crt");
    if !ca.exists() {
        openssl(
            Command::new("openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-days",
                    "1",
                    "-subj",
                    "/CN=platform-artifacts-ca",
                    "-keyout",
                ])
                .arg(directory.join("ca.key"))
                .arg("-out")
                .arg(&ca),
        )?;
    }
    openssl(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=platform-artifacts",
                "-addext",
                &format!("subjectAltName=IP:{address}"),
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-CA",
            ])
            .arg(&ca)
            .arg("-CAkey")
            .arg(directory.join("ca.key"))
            .arg("-keyout")
            .arg(directory.join("tls.key"))
            .arg("-out")
            .arg(directory.join("tls.crt")),
    )
}

fn openssl(command: &mut Command) -> anyhow::Result<()> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .context("create the registry TLS certificate")?;
    ensure!(
        output.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn registry(cluster: &str) -> anyhow::Result<u16> {
    let network = Command::new("docker")
        .args(["network", "inspect", "kind"])
        .stdin(Stdio::null())
        .output()
        .context("look for the kind network")?;
    if !network.status.success() {
        docker(&["network", "create", "kind"])?;
    }
    let name = format!("{cluster}-registry");
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "kind",
        "-p",
        "127.0.0.1:0:5000",
        "registry:2",
    ])?;
    let binding = docker(&["port", &name, "5000/tcp"])?;
    binding
        .trim()
        .strip_prefix("127.0.0.1:")
        .context("the registry has no loopback port")?
        .parse()
        .context("read the registry port")
}

fn docker(arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("docker")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .context("run the fixture Docker command")?;
    ensure!(
        output.status.success(),
        "docker {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("read the fixture Docker output")
}

#[path = "support/release_documents.rs"]
pub mod release_documents;
use release_documents::fixture_environment;

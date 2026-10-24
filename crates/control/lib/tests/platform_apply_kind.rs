//! Install the platform on a private kind cluster and require an unchanged rerun.
//!
//! Requires Docker with buildx manifest reads, kind, Helm, kubectl, pnpm, OpenSSL,
//! package build tools, and debug wamn and wamn-ctl binaries.
//! The runner exports CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent and
//! DOCKER_CONFIG pointing to a config.json with the GCP gcloud credential helper.
//! The fixture reads deploy/gcp/cluster.k and requires its registry to hold all
//! seven images of Chart.yaml's version. It copies them to its registry:2 container
//! and stamps them. It never builds images or needs image build tools.
//! Network access to the GCP registry and public registries is required.
//! No external database, bucket or DNS is required. The test keeps its input,
//! kubeconfig and progress under /var/tmp and removes its cluster and registries.
//! A TLS artifact registry serves Receiving, WMS, and their unqualified releases.
//! WMS publishes through the native gate sidecar with an in-memory PAT.
//! WMS stays unqualified and must refuse. The host qualifies Receiving, then
//! the test applies only Receiving through HTTPS checks in 4.21 and requires an unchanged rerun.
//! HTTPS Jobs pull the existing pinned curl image from the public registry.

use std::io::Write as _;
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
use wash_runtime::oci::OciConfig;

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
        match Command::new("docker")
            .args([
                "rm",
                "-f",
                &format!("{}-registry", self.name),
                &format!("{}-artifacts", self.name),
            ])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("delete registry {}-registry exited {status}", self.name),
            Err(error) => eprintln!("delete registry {}-registry: {error}", self.name),
        }
    }
}

const STEP_TITLES: [&str; 12] = [
    "4.3 cert-manager and the edge certificate",
    "4.4 Namespaces, runtime operator and internal CA",
    "4.4.1 Collector",
    "4.5 PostgreSQL",
    "4.6 Packages and platform build",
    "4.7 System database, org and identity issuer",
    "4.8 Event NATS",
    "4.9 Platform install inputs",
    "4.11 First apply",
    "4.12 Identity and session key",
    "4.13 Packages",
    "4.14 Publish releases",
];

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker, kind, helm, kubectl, pnpm, debug CLI binaries, package build tools, agent identity, GCP and public registry access; no image builds"]
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
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    let source_chart = platform_images::source_chart(&repository, &directory)?;
    let source_images = wamn_control::release_chart::image_set(&source_chart).await?;
    let source_registry = source_images
        .host
        .split("/wamn-host")
        .next()
        .context("host registry")?;
    println!("work directory {}", directory.display());
    let _cluster = ClusterGuard { name: name.clone() };
    let port = registry(&name)?;
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
    copy_images(source_registry, &document.registry, &version).await?;
    let fixture_chart = directory.join("fixture-chart");
    stamp_registry(&repository, &document.registry, &version, &fixture_chart)?;
    let chart: Value = serde_yaml::from_slice(&std::fs::read(fixture_chart.join("values.yaml"))?)?;
    let environments = directory.join("environments");
    std::fs::create_dir(&environments)?;
    let source = repository.join("deploy/gcp/environments/dkk--receiving--dev.k");
    let mut environment = EnvironmentDocument::compile(&source)?;
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

    let wms_source = repository.join("deploy/gcp/environments/dkk--wms--dev.k");
    let mut wms_environment = EnvironmentDocument::compile(&wms_source)?;
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

    let mut progress = std::fs::File::create(directory.join("progress.log"))?;
    let mut first = Vec::new();
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
    for (namespace, kind, name) in [
        ("hosts", "configmap", "identity-ca"),
        ("edge", "configmap", "identity-ca"),
        ("identity", "secret", "wamn-identity-db"),
        ("platform", "configmap", "wamn-install-platform-system"),
        ("platform", "configmap", "wamn-install-platform-org"),
        (
            "platform",
            "configmap",
            "wamn-install-platform-identity-issuer",
        ),
        (
            "identity",
            "secret",
            "wamn-session-role-reader-test--receiving--dev",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-first-apply-test--receiving--dev",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-session-key-activate",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-identity-check",
        ),
    ] {
        let object: Option<Value> =
            kubectl::get(&kubeconfig, &context, namespace, kind, name).await?;
        ensure!(object.is_some(), "missing {kind} {namespace}/{name}");
    }
    // nats-server opens to anonymous clients when its users list is empty.
    let anonymous = kubectl::kubectl(
        &kubeconfig,
        &context,
        "platform",
        &[
            "exec",
            "-i",
            "evt-nats-0",
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
        anonymous.contains("Authorization Violation") && !anonymous.contains("PONG"),
        "event NATS accepted an anonymous connection: {anonymous}"
    );
    let prefix = format!("{}: release = \"", input.display());
    let digest = first
        .iter()
        .find_map(|line| {
            line.strip_prefix(&prefix)
                .and_then(|line| line.strip_suffix('"'))
        })
        .context("publication printed no release line")?
        .to_owned();
    ensure!(
        EnvironmentDocument::compile(&input)?.release == DeclaredRelease::None,
        "platform apply changed the environment file"
    );
    let wms_prefix = format!("{}: release = \"", wms_input.display());
    let wms_digest = first
        .iter()
        .find_map(|line| {
            line.strip_prefix(&wms_prefix)
                .and_then(|line| line.strip_suffix('"'))
        })
        .context("wired publication printed no release line")?
        .to_owned();
    ensure!(
        EnvironmentDocument::compile(&wms_input)?.release == DeclaredRelease::None,
        "platform apply changed the WMS environment file"
    );
    let gate_log = std::fs::read_to_string(work.join("jobs/platform-release-test--wms--dev.log"))?;
    ensure!(
        gate_log.contains(&wms_digest),
        "gate Job log contains no WMS release digest"
    );
    ensure!(
        gate_log.contains("control authoring credential accepted"),
        "gate Job log contains no sidecar startup"
    );
    let completed: Value = kubectl::get(
        &kubeconfig,
        &context,
        "platform",
        "configmap",
        "wamn-install-platform-release-test--wms--dev",
    )
    .await?
    .context("wired publication has no completion record")?;
    ensure!(
        completed["data"]["output"].as_str().map(str::trim) == Some(wms_digest.as_str()),
        "wired publication retained output other than its digest"
    );
    for suffix in ["identity-reader", "control-author", "management-admitter"] {
        let secret: Option<Value> = kubectl::get(
            &kubeconfig,
            &context,
            "identity",
            "secret",
            &format!("platform-release-test--wms--dev-{suffix}"),
        )
        .await?;
        ensure!(secret.is_none(), "gate Job retained its {suffix} Secret");
    }
    wms_environment.release = DeclaredRelease::Digest(wms_digest.clone());
    std::fs::write(&wms_input, wms_environment.to_kcl())?;
    environment.release = DeclaredRelease::Digest(digest.clone());
    std::fs::write(&input, environment.to_kcl())?;
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
            Ok(())
        },
    )
    .await
    .expect_err("an unqualified release must refuse");
    let refusal = format!("{refused:#}");
    ensure!(
        refusal.contains(&format!(
            "release {wms_digest} has no qualification on the chart's image set:"
        )),
        "{refusal}"
    );
    ensure!(
        refusal.contains(&format!("qualify-release --release-digest {wms_digest}")),
        "{refusal}"
    );
    writeln!(progress, "{refusal}")?;
    let secrets = secret_versions(&kubeconfig, &context).await?;
    require_private_logs(&work, &format!("{}\n{refusal}", first.join("\n")), &secrets)?;

    let mut second = Vec::new();
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
    .expect_err("the unchanged unqualified release still refuses");
    ensure!(
        format!("{repeated:#}").contains(&format!("release {wms_digest} has no qualification")),
        "{repeated:#}"
    );
    require_steps(&second, "skipped")?;
    ensure!(
        second
            .iter()
            .any(|line| line == &format!("{wms_prefix}{wms_digest}\"")),
        "unchanged wired publication did not return the same release line"
    );
    ensure!(
        std::fs::read_to_string(work.join("jobs/platform-release-test--wms--dev.log"))? == gate_log,
        "unchanged wired publication ran its gate Job again"
    );
    for ((name, first), (_, second)) in secrets
        .iter()
        .zip(secret_versions(&kubeconfig, &context).await?)
    {
        ensure!(
            first["metadata"]["resourceVersion"] == second["metadata"]["resourceVersion"],
            "unchanged apply rewrote Secret {name}"
        );
    }
    // Qualification stays on the Docker host. Receiving has one required case.
    let mut releases = artifacts.clone();
    releases.artifact_base = artifacts
        .artifact_base
        .strip_suffix("/packages")
        .context("fixture package registry suffix")?
        .to_owned()
        + "/releases";
    let started = Instant::now();
    let qualified = wamn_control::delivery::qualification::qualify(
        wamn_control::delivery::qualification::QualifyReleaseRequest {
            release_digest: digest.clone(),
            releases,
            release_chart: work.join("release-chart"),
        },
    )
    .await;
    let timing = format!(
        "qualify-release Receiving wall time {:.3}s result {}",
        started.elapsed().as_secs_f64(),
        if qualified.is_ok() { "pass" } else { "fail" }
    );
    println!("{timing}");
    writeln!(progress, "{timing}")?;
    qualified?;

    // The HTTP mirror supplies native images to containerd. Workload processes
    // need the same role digests at the pod-reachable TLS artifact registry.
    let mut serving = document.clone();
    serving.registry = artifacts
        .artifact_base
        .strip_suffix("/packages")
        .context("fixture package registry suffix")?
        .to_owned();
    serving_chart(&document, &serving, &work, &artifacts).await?;
    serving_credentials(&kubeconfig, &context, &directory, &host_group, &artifacts).await?;
    let selected = directory.join("qualified-environments");
    std::fs::create_dir(&selected)?;
    let selected_input = selected.join("test--receiving--dev.k");
    std::fs::copy(&input, &selected_input)?;
    std::fs::copy(
        input.with_extension("json"),
        selected_input.with_extension("json"),
    )?;
    let mut applied = Vec::new();
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
    for title in [
        "4.17 Apply with the release",
        "4.18 CDC readers",
        "4.19.1 Control credentials",
        "4.19.1 Control host",
        "4.19.1 Owner invite",
        "4.20 Rust edge",
    ] {
        ensure!(
            applied
                .iter()
                .any(|line| line.starts_with(&format!("{title}: done "))),
            "{title} did not run"
        );
    }
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
    // Receiving registers no event, so env apply enables no CDC and 4.18
    // deploys no reader (owner ruling 2026-10-22). environment_apply_kind
    // runs the CDC reader.
    require_cdc_reader_skip(&applied)?;
    for (kind, name) in [
        ("deployment", "cdc-reader-test--receiving--dev"),
        ("secret", "wamn-cdc-test--receiving--dev"),
    ] {
        ensure!(
            kubectl::get::<Value>(&kubeconfig, &context, "hosts", kind, name)
                .await?
                .is_none(),
            "Receiving has a {kind} {name} without an event registration"
        );
    }
    // Env apply creates the number_sequence row of Receiving's purchase_order
    // sequence with the package's default pattern (wamn-83eth.3).
    let pattern = number_sequence_pattern(&kubeconfig, &context, "purchase_order").await?;
    ensure!(
        pattern == "PO-{seq:po,width=6}",
        "env apply did not create the purchase_order number_sequence row, found {pattern:?}"
    );
    require_web_skip(&applied)?;
    require_platform_ui_skip(&applied)?;
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
    require_edge_skip(&applied)?;
    let https_before = https_state(
        &kubeconfig,
        &context,
        &[&serving.route_hosts.control, &environment.route_host],
    )
    .await?;
    let edge: Value = kubectl::get(&kubeconfig, &context, "edge", "deployment", "wamn-edge-web")
        .await?
        .context("missing Rust edge Deployment")?;
    ensure!(
        edge["status"]["observedGeneration"] == edge["metadata"]["generation"]
            && edge["status"]["updatedReplicas"] == 1
            && edge["status"]["availableReplicas"] == 1,
        "Rust edge did not roll out"
    );
    // The verb installs from the serving chart, whose images the fixture moved.
    let serving_values: Value =
        serde_yaml::from_slice(&std::fs::read(work.join("release-chart/values.yaml"))?)?;
    ensure!(
        edge["spec"]["template"]["spec"]["containers"][0]["image"]
            == serving_values["platform"]["edge"]["image"],
        "Rust edge does not use the stamped image"
    );
    ensure!(
        edge["spec"]["template"]["spec"]["serviceAccountName"] == "wamn-edge-web",
        "Rust edge has another service account"
    );
    let service: Value = kubectl::get(&kubeconfig, &context, "edge", "service", "wamn-edge-web")
        .await?
        .context("missing Rust edge Service")?;
    ensure!(
        service["spec"]["selector"] == edge["spec"]["template"]["metadata"]["labels"],
        "Rust edge Service selects another workload"
    );
    let access: Value = kubectl::get(&kubeconfig, &context, "hosts", "role", "wamn-edge-web")
        .await?
        .context("missing Rust edge Role")?;
    ensure!(
        access["rules"][0]["verbs"] == json!(["get"])
            && access["rules"][0]["resources"] == json!(["configmaps"]),
        "Rust edge can read beyond the release ConfigMaps"
    );
    let control_row: Value = kubectl::get(&kubeconfig, &context, "hosts", "configmap", "c-test")
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
    let invite_log_path = work.join("jobs/platform-invite-test.log");
    let invite_log = std::fs::read_to_string(&invite_log_path)?;
    ensure!(
        invite_log == "owner invitation accepted",
        "invite Job did not complete"
    );
    let invite_modified = std::fs::metadata(&invite_log_path)?.modified()?;
    let mail = owner_mail(&kubeconfig, &context).await?;
    ensure!(
        mail.lines().count() == 1,
        "the owner did not receive exactly one invitation"
    );
    let mut secrets = secret_versions(&kubeconfig, &context).await?;
    let control_secrets = control_secret_versions(&kubeconfig, &context).await?;
    secrets.extend(control_secrets.clone());
    let mut repeated = Vec::new();
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
    require_steps(&repeated, "skipped")?;
    for title in [
        "4.17 Apply with the release",
        "4.18 CDC readers",
        "4.19.1 Control credentials",
        "4.19.1 Control host",
        "4.19.1 Owner invite",
        "4.20 Rust edge",
    ] {
        ensure!(
            repeated
                .iter()
                .any(|line| line.starts_with(&format!("{title}: skipped "))),
            "{title} did not skip"
        );
    }
    require_cdc_reader_skip(&repeated)?;
    require_web_skip(&repeated)?;
    require_platform_ui_skip(&repeated)?;
    require_edge_skip(&repeated)?;
    let https_after = https_state(
        &kubeconfig,
        &context,
        &[&serving.route_hosts.control, &environment.route_host],
    )
    .await?;
    ensure!(
        https_after == https_before,
        "unchanged apply rewrote the proxy or reran HTTPS checks"
    );
    let edge_after: Value =
        kubectl::get(&kubeconfig, &context, "edge", "deployment", "wamn-edge-web")
            .await?
            .context("Rust edge Deployment disappeared")?;
    ensure!(
        edge_after["metadata"]["resourceVersion"] == edge["metadata"]["resourceVersion"],
        "unchanged apply rewrote the Rust edge Deployment"
    );
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
    ensure!(
        std::fs::read_to_string(&invite_log_path)? == invite_log
            && std::fs::metadata(&invite_log_path)?.modified()? == invite_modified,
        "unchanged apply reran the invite Job"
    );
    ensure!(
        owner_mail(&kubeconfig, &context).await? == mail,
        "unchanged apply sent another invitation"
    );
    for ((name, before), (_, after)) in control_secrets
        .iter()
        .zip(control_secret_versions(&kubeconfig, &context).await?)
    {
        ensure!(
            before["metadata"]["resourceVersion"] == after["metadata"]["resourceVersion"],
            "unchanged apply rewrote control Secret {name}"
        );
    }
    for name in [
        "platform-control-credentials-test-files",
        "platform-control-operator-test-files",
        "platform-invite-test-files",
        "platform-owner-state-test-files",
    ] {
        let secret: Option<Value> =
            kubectl::get(&kubeconfig, &context, "platform", "secret", name).await?;
        ensure!(
            secret.is_none(),
            "the Job-owned Secret {name} remains after deletion"
        );
    }
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
    for line in [
        "4.15 Qualification: skipped: qualify-release on the build machine",
        "4.16 Record the releases: skipped: owner input",
    ] {
        ensure!(
            repeated.iter().any(|reported| reported == line),
            "missing owner-step skip: {line}"
        );
    }
    for ((name, first), (_, second)) in secrets
        .iter()
        .zip(secret_versions(&kubeconfig, &context).await?)
    {
        ensure!(
            first["metadata"]["resourceVersion"] == second["metadata"]["resourceVersion"],
            "unchanged apply rewrote Secret {name}"
        );
    }
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
    let completion: Option<Value> = kubectl::get(
        &kubeconfig,
        &context,
        "platform",
        "configmap",
        "wamn-install-platform-web-test--receiving--dev",
    )
    .await?;
    ensure!(completion.is_none(), "kind completed a web upload Job");
    let logs = std::fs::read_to_string(directory.join("progress.log"))?;
    let registry_secret = kubectl::get(
        &kubeconfig,
        &context,
        "hosts",
        "secret",
        "platform-fixture-registry",
    )
    .await?
    .context("fixture registry Secret")?;
    require_private_logs(&work, &logs, &secrets)?;
    let invitation: Value = serde_json::from_str(mail.trim())
        .map_err(|_| anyhow::anyhow!("invalid local invitation mail"))?;
    let invitation_text = invitation["text"]
        .as_str()
        .context("invitation has no text")?;
    let mut all_logs = logs.clone();
    for entry in std::fs::read_dir(work.join("jobs"))? {
        all_logs.push_str(&std::fs::read_to_string(entry?.path())?);
    }
    ensure!(
        !all_logs.contains(invitation_text),
        "invitation mail reached a log"
    );
    let code = invitation_text
        .lines()
        .find_map(|line| line.strip_prefix("Invitation code: "))
        .context("local invitation has no code")?;
    let (_, secret) = code
        .split_once(':')
        .context("local invitation has no secret")?;
    ensure!(
        !all_logs.contains(code) && !all_logs.contains(secret),
        "an invitation code or secret reached a log"
    );
    require_private_logs(
        &work,
        &logs,
        &[("platform-fixture-registry".into(), registry_secret)],
    )?;
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
    run(&["rollout", "status", "deployment/identity", "--timeout=300s"])?;
    Ok(())
}

// The pattern of one number_sequence row in the Receiving project database,
// read with psql in the primary pod of wamn-pg. A read only: the test holds
// no database credential outside the cluster.
async fn number_sequence_pattern(
    kubeconfig: &Path,
    context: &str,
    sequence: &str,
) -> anyhow::Result<String> {
    let primary = kubectl::kubectl(
        kubeconfig,
        context,
        "platform",
        &[
            "get",
            "pods",
            "-l",
            "cnpg.io/cluster=wamn-pg,cnpg.io/instanceRole=primary",
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ],
        None,
    )
    .await?;
    let psql = |database: String, sql: String| {
        let primary = primary.trim().to_owned();
        async move {
            let output = kubectl::kubectl(
                kubeconfig,
                context,
                "platform",
                &[
                    "exec",
                    &primary,
                    "-c",
                    "postgres",
                    "--",
                    "psql",
                    "--no-psqlrc",
                    "--tuples-only",
                    "--no-align",
                    "--set",
                    "ON_ERROR_STOP=1",
                    "--dbname",
                    &database,
                    "--command",
                    &sql,
                ],
                None,
            )
            .await?;
            anyhow::Ok(output.trim().to_owned())
        }
    };
    let database = psql(
        "postgres".into(),
        "SELECT datname FROM pg_database WHERE datname LIKE 'wamn-db-test--receiving--dev--%'"
            .into(),
    )
    .await?;
    ensure!(
        !database.is_empty() && !database.contains('\n'),
        "expected one Receiving project database, found {database:?}"
    );
    psql(
        database,
        format!("SELECT pattern FROM platform.number_sequence WHERE name = '{sequence}'"),
    )
    .await
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
) -> anyhow::Result<Vec<(String, Value)>> {
    let mut secrets = Vec::new();
    for name in [
        "wamn-control-test",
        "wamn-control-administration-test",
        "operator-test",
    ] {
        let mut secret: Value = kubectl::get(kubeconfig, context, "hosts", "secret", name)
            .await?
            .with_context(|| format!("missing control Secret {name}"))?;
        // Kubernetes omits data on the first-install empty administration Secret.
        if secret.get("data").is_none() {
            secret["data"] = json!({});
        }
        secrets.push((name.into(), secret));
    }
    Ok(secrets)
}

fn require_platform_ui_skip(lines: &[String]) -> anyhow::Result<()> {
    for action in ["upload", "check"] {
        ensure!(
            lines
                .iter()
                .any(|line| line == &format!("4.19.1 Platform UI {action}: skipped: no bucket")),
            "missing platform UI {action} no-bucket skip"
        );
    }
    Ok(())
}

async fn serving_chart(
    source: &ClusterDocument,
    serving: &ClusterDocument,
    work: &Path,
    artifacts: &wamn_control::package_artifact::PackageRegistry,
) -> anyhow::Result<()> {
    use wamn_control::release_chart;
    let chart = work.join("release-chart");
    let original = release_chart::image_set(&chart).await?;
    let mut images = original.clone();
    let mut qualification = release_chart::qualification_images(&chart).await?;
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
    let saved = work.join("qualification-chart");
    std::fs::rename(&chart, &saved)?;
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
    std::fs::create_dir(&destination)?;
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
    env.retain(|value| value["name"] != "WAMN_REGISTRY_TOKEN_METADATA");
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
) -> anyhow::Result<Vec<Value>> {
    let proxy: Value = kubectl::get(kubeconfig, context, "edge", "deployment", "wamn-edge")
        .await?
        .context("missing nginx Deployment")?;
    ensure!(
        proxy["status"]["observedGeneration"] == proxy["metadata"]["generation"]
            && proxy["status"]["availableReplicas"] == 1,
        "nginx did not roll out"
    );
    let pod = &proxy["spec"]["template"]["spec"];
    ensure!(
        pod["volumes"]
            .as_array()
            .context("nginx volumes")?
            .iter()
            .any(|volume| volume["secret"]["secretName"] == "wamn-edge-tls"),
        "nginx does not mount its certificate"
    );
    ensure!(
        pod["volumes"]
            .as_array()
            .context("nginx volumes")?
            .iter()
            .any(|volume| volume["configMap"]["name"] == "identity-ca"),
        "nginx does not mount the identity CA"
    );
    let service: Value = kubectl::get(kubeconfig, context, "edge", "service", "wamn-edge")
        .await?
        .context("missing nginx Service")?;
    ensure!(
        service["metadata"]["annotations"]["cloud.google.com/neg"].is_null(),
        "kind requests a GCP NEG"
    );
    ensure!(
        service["spec"]["ports"][0]["port"] == 443,
        "nginx Service lacks HTTPS"
    );
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
    let output = String::from_utf8(output.stdout)?;
    let serial = output
        .trim()
        .strip_prefix("serial=")
        .context("openssl certificate serial")?
        .to_ascii_lowercase();
    let completion: Value = kubectl::get(
        kubeconfig,
        context,
        "platform",
        "configmap",
        "wamn-install-platform-https-check",
    )
    .await?
    .context("missing HTTPS completion")?;
    ensure!(
        completion["data"]["load_balancer"] == "skipped: kind"
            && completion["data"]["web"] == "skipped: no bucket",
        "HTTPS completion lacks kind skips"
    );
    let results: Vec<Value> = serde_json::from_str(
        completion["data"]["results"]
            .as_str()
            .context("HTTPS results")?,
    )?;
    ensure!(
        results.len() == hosts.len(),
        "HTTPS completion has another host count"
    );
    let mut versions = vec![
        proxy["metadata"]["resourceVersion"].clone(),
        completion["metadata"]["resourceVersion"].clone(),
    ];
    for host in hosts {
        let result = results
            .iter()
            .find(|result| result["host"].as_str() == Some(*host))
            .with_context(|| format!("no HTTPS result for {host}"))?;
        ensure!(
            result["status"] == 401
                && result["serial"] == serial
                && result["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with("/api/")),
            "HTTPS API or certificate mismatch for {host}"
        );
        let key = wamn_control::platform::job::input_digest(&json!([
            host,
            result["method"],
            result["path"]
        ]));
        let job_name = format!("platform-https-{}", &key[..16]);
        let record: Value = kubectl::get(
            kubeconfig,
            context,
            "platform",
            "configmap",
            &format!("wamn-install-{job_name}"),
        )
        .await?
        .context("missing HTTPS Job completion")?;
        versions.push(record["metadata"]["resourceVersion"].clone());
        let job: Option<Value> =
            kubectl::get(kubeconfig, context, "edge", "job", &job_name).await?;
        ensure!(job.is_none(), "completed HTTPS Job remains");
    }
    Ok(versions)
}

fn require_edge_skip(lines: &[String]) -> anyhow::Result<()> {
    ensure!(
        lines
            .iter()
            .any(|line| line == "4.20 Edge bucket check: skipped: no bucket"),
        "missing edge no-bucket skip"
    );
    Ok(())
}

fn require_cdc_reader_skip(lines: &[String]) -> anyhow::Result<()> {
    ensure!(
        lines
            .iter()
            .any(|line| line == "4.18 CDC reader test/receiving/dev: skipped: no registration"),
        "missing Receiving CDC reader skip"
    );
    Ok(())
}

fn require_web_skip(lines: &[String]) -> anyhow::Result<()> {
    for action in ["upload", "check"] {
        ensure!(
            lines
                .iter()
                .any(|line| line
                    == &format!("4.19 Web {action} test/receiving/dev: skipped: no bucket")),
            "missing no-bucket {action} skip"
        );
    }
    Ok(())
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
    ensure!(
        !logs.contains(wamn_session::PAT_TOKEN_PREFIX),
        "a PAT reached the install progress or Job logs"
    );
    ensure!(
        !database_userinfo(&logs),
        "a database URL with credentials reached the install progress or Job logs"
    );
    for (_, secret) in secrets {
        for (key, encoded) in secret["data"].as_object().context("Secret has no data")? {
            let encoded = encoded.as_str().context("Secret data is not text")?;
            let decoded = STANDARD.decode(encoded).context("decode fixture Secret")?;
            let value = std::str::from_utf8(&decoded).context("fixture Secret is not UTF-8")?;
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
                let value: Value =
                    serde_json::from_str(value.strip_prefix("authorization: ").unwrap_or(value))?;
                private_values(&value, &mut private);
            }
            for value in private {
                ensure!(
                    !logs.contains(&value) && !logs.contains(&STANDARD.encode(&value)),
                    "a credential reached the install progress or Job logs"
                );
            }
        }
    }

    Ok(())
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

async fn secret_versions(kubeconfig: &Path, context: &str) -> anyhow::Result<Vec<(String, Value)>> {
    let names = [
        ("platform", "wamn-pg-superuser"),
        ("platform", "evt-nats-authorization"),
        ("platform", "evt-nats-test--receiving--dev-provisioning"),
        ("platform", "evt-nats-test--receiving--dev-publisher"),
        ("platform", "evt-nats-test--receiving--dev-observer"),
        ("hosts", "evt-nats-test--receiving--dev-publisher"),
        ("hosts", "evt-nats-test--receiving--dev-runtime"),
        ("hosts", "evt-nats-test--receiving--dev-materializer"),
        ("identity", "wamn-session-role-reader-test--receiving--dev"),
        ("identity", "identity-resend"),
        ("identity", "wamn-identity-db"),
    ];
    let mut secrets = Vec::new();
    for (namespace, name) in names {
        let secret = kubectl::get(kubeconfig, context, namespace, "secret", name)
            .await?
            .with_context(|| format!("missing Secret {namespace}/{name}"))?;
        secrets.push((format!("{namespace}/{name}"), secret));
    }
    Ok(secrets)
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
    use std::os::unix::fs::DirBuilderExt as _;
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

fn require_steps(lines: &[String], result: &str) -> anyhow::Result<()> {
    let completed: Vec<_> = lines
        .iter()
        .filter(|line| {
            STEP_TITLES
                .iter()
                .any(|title| line.starts_with(&format!("{title}: ")))
                && !line.ends_with(": starting")
        })
        .map(String::as_str)
        .collect();
    let completed = completed
        .iter()
        .map(|line| {
            let (step, elapsed) = line.rsplit_once(' ').context("step has no elapsed time")?;
            let seconds: f64 = elapsed
                .strip_suffix('s')
                .context("elapsed time has no seconds suffix")?
                .parse()?;
            ensure!(
                seconds.is_finite() && seconds >= 0.0,
                "invalid elapsed time"
            );
            Ok(step)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let expected = STEP_TITLES.map(|title| format!("{title}: {result}"));
    ensure!(
        completed == expected,
        "expected {expected:?}, got {completed:?}"
    );
    Ok(())
}

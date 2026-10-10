//! One application delivered to the kept delivery cluster through
//! `wamn-ctl env apply` (docs/plan/platform-deploy.md §10.1, §17.2 Epic 6).
//!
//! The application's own fixture publishes its release in its project
//! database, starts its session issuer and renders its host overlay. Then the
//! delivery is the same for every application:
//!
//! 1. The host, identity and gates images go to an owned native registry.
//!    `prepare-release` writes the candidate, the nodes pull the images, and
//!    `qualify-release` runs the application's qualifying cases.
//! 2. The role images are pushed, and a copy of the release chart is stamped
//!    with the host image and the role images (R15).
//! 3. `publish-qualified-release` pushes the manifest to the TLS registry and
//!    records the qualification on the chart's image set, which `env apply`
//!    reads (R12).
//! 4. The host group's platform part is the fixture's overlay group less what
//!    `apply` derives. The document is `env show` of the environment with the
//!    release digest and the route host, and `wamn-ctl env apply` runs on it.
//! 5. The checks: the hosts, each role's placement, the A1 probe, and A12 when
//!    another environment is in the namespace.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use serde_yaml::Value as Yaml;
use tokio::process::Command;
use wamn_catalog::ServingManifest;
use wamn_control::delivery::Candidate;
use wamn_control::environment::document::DeclaredRelease;
use wamn_control::environment::{EventBroker, Platform};
use wamn_control::release_chart::{ImageSet, Target, release_name, roles, stamp};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::delivery_cluster::{self, KeptCluster, NAMESPACE, REGISTRY_SECRET};
use wamn_test_infrastructure::release_chart::{
    self as checks, Environment, Release, a1_probe, hosts_ready, role_placed,
};

/// One run of an owned delivery command may take this long; `qualify-release`
/// runs the application's cluster cases.
const COMMAND_TIMEOUT: Duration = Duration::from_hours(3);
/// The host group's replicas on the kept cluster.
const REPLICAS: u32 = 1;
/// Where the hosts mount [`REGISTRY_SECRET`].
const REGISTRY_MOUNT: &str = "/etc/wamn/registry";

/// The host variables that `env apply` derives from the coordinate, the
/// policy and the credential Secrets (`environment/write.rs`,
/// `release_chart::values`). The platform part of the host group must not set them.
const DERIVED_VARIABLES: [&str; 10] = [
    "WAMN_PG_URL",
    "WAMN_SYSTEM_URL",
    "WAMN_EXECUTOR_PLATFORM_PG_URL",
    "WAMN_HTTP_ADMITTER_PG_URL",
    "WAMN_EVENT_MATERIALIZER_PG_URL",
    "WAMN_ADMINISTRATION_PG_URL",
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WASMCLOUD_HOST_ENVIRONMENT",
    "WAMN_DRAIN_BOUND_SECONDS",
];
/// The host group keys that the release chart derives.
const DERIVED_KEYS: [&str; 5] = ["name", "namespace", "service", "extraArgs", "image"];

/// One application on the kept cluster.
pub struct Application<'a> {
    pub repository: &'a Path,
    pub kept: &'a KeptCluster,
    /// `<kept>-<app>`: the owned name of the application's images, native
    /// registry and service containers.
    pub owner: &'a str,
    /// The application's private directory, which holds a copy of the kept
    /// `kubeconfig`.
    pub work: &'a Path,
    pub evidence: &'a Path,
    /// The clean commit the case runs.
    pub source: &'a str,
    /// The target directory of the built programs and guests.
    pub target: &'a Path,
    pub triple: Triple,
    pub tenant: &'a str,
    pub route_host: &'a str,
    /// The owner URL of the project database the fixture published in.
    pub project_database_url: &'a str,
    pub system_database_url: &'a str,
    /// The release the fixture published, `sha256:<hex>`.
    pub release_digest: &'a str,
    /// Where the fixture pushed the release manifest: the plain HTTP registry.
    pub release_artifact_base: &'a str,
    /// Values every command result redacts.
    pub secrets: Vec<String>,
}

impl std::fmt::Debug for Application<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Application")
            .field("owner", &self.owner)
            .field("triple", &self.triple)
            .field("release_digest", &self.release_digest)
            .finish_non_exhaustive()
    }
}

/// What the delivery needs beyond the published release.
#[derive(Debug)]
pub struct Inputs<'a> {
    /// The fixture's rendered host overlay: one host group.
    pub overlay: &'a Path,
    /// Fixed Secret and ConfigMap names of the overlay, each with the
    /// coordinate-named copy the case created in the operator namespace (§9.1).
    pub renames: &'a [(String, String)],
    /// The namespace of the application's CloudNativePG `Cluster`.
    pub database_namespace: &'a str,
    /// The address every credential URL names, at port 5432.
    pub database_host: &'a str,
    pub events: EventBroker,
}

/// What the delivery left, for the case's own result.
#[derive(Debug)]
pub struct Delivered {
    pub release_name: String,
    pub host_image: String,
    pub plan: String,
}

impl Application<'_> {
    fn ctl(&self) -> PathBuf {
        self.target.join("debug/wamn-ctl")
    }

    fn adapter(&self) -> PathBuf {
        self.repository.join("tools/delivery-owned")
    }

    /// Receiving carries a gates image, as `tools/delivery-owned` builds it.
    fn gates(&self) -> bool {
        self.owner.ends_with("-receiving")
    }

    fn native(&self, role: &str) -> anyhow::Result<String> {
        Ok(
            fs::read_to_string(self.work.join(format!("native-{role}-image")))
                .with_context(|| format!("read the native {role} image"))?
                .trim()
                .to_owned(),
        )
    }
}

/// Build the application's host, identity and gates images as
/// `wamn-<role>:<owner>`, and load the identity and gates images into the
/// kept cluster for the session issuer, which runs them with `pullPolicy: Never`.
pub async fn build_images(app: &Application<'_>) -> anyhow::Result<()> {
    step(
        app,
        "build-images",
        Command::new(app.adapter())
            .arg("build-images")
            .arg(app.owner)
            .arg(app.work)
            .arg(app.source),
    )
    .await?;
    for role in std::iter::once("identity").chain(app.gates().then_some("gates")) {
        app.kept
            .load_image(&format!("wamn-{role}:{}", app.owner))
            .await?;
    }
    Ok(())
}

/// Qualify, publish and apply the application's release, then check it.
pub async fn deliver(app: &Application<'_>, inputs: &Inputs<'_>) -> anyhow::Result<Delivered> {
    step(
        app,
        "native-registry",
        Command::new(app.adapter())
            .arg("native-registry")
            .arg(app.owner)
            .arg(app.work)
            .arg(app.source),
    )
    .await?;
    let host_image = app.native("host")?;
    let (candidate, manifest, qualification) = qualify(app, &host_image).await?;

    // The platform image set: the qualified host image and the role images.
    let (http, materializer) = push_roles(app).await?;
    let chart = stamp(
        &app.repository.join("deploy/platform/release"),
        &ImageSet {
            host: host_image.clone(),
            http,
            materializer,
        },
        &app.work.join("release-chart"),
    )?;

    // The manifest goes to the TLS registry, and the qualification is recorded
    // on the chart's image set (R12).
    let tls_releases = format!("{}/wamn/releases", app.kept.registry);
    let ca = app.kept.ca.to_string_lossy().into_owned();
    step(
        app,
        "publish-qualified-release",
        Command::new(app.ctl())
            .arg("publish-qualified-release")
            .arg("--qualification")
            .arg(&qualification)
            .args([
                "--database-url",
                app.project_database_url,
                "--control-database-url",
                app.system_database_url,
                "--org",
                &app.triple.org,
                "--project",
                &app.triple.project,
                "--environment",
                app.triple.env.as_str(),
                "--tenant",
                app.tenant,
                "--release-digest",
                app.release_digest,
                "--artifact-base",
                &tls_releases,
                "--oci-ca-path",
                &ca,
            ])
            .arg("--registry-auth-file")
            .arg(&app.kept.registry_auth)
            .arg("--release-chart")
            .arg(&chart),
    )
    .await?;
    let platform = Platform {
        system_database_url: app.system_database_url.to_owned(),
        target: Target {
            kubeconfig: app.kept.kubeconfig(),
            context: app.kept.context(),
            namespace: NAMESPACE.to_owned(),
        },
        chart,
        release_artifact_base: tls_releases,
        registry_auth_file: app.kept.registry_auth.clone(),
        oci_ca_paths: vec![app.kept.ca.clone()],
        package_artifact_base: None,
        database_namespace: inputs.database_namespace.to_owned(),
        database_host: Some(inputs.database_host.to_owned()),
        database_port: 5432,
        host_group: Some(host_group(app, inputs.overlay, inputs.renames)?),
        pat_issuer: wamn_control::pat_client::PatIssuerConfig::default(),
        events: Some(inputs.events.clone()),
    };
    let plan = apply(app, &platform).await?;
    let name = release_name(
        &app.triple.org,
        &app.triple.project,
        app.triple.env.as_str(),
    )?;
    check(app, &name, &host_image, &candidate, &manifest).await?;
    Ok(Delivered {
        release_name: name,
        host_image,
        plan,
    })
}

/// `prepare-release`, the nodes' pull of the native images, and
/// `qualify-release`. Returns the candidate, its manifest and the
/// qualification result.
async fn qualify(
    app: &Application<'_>,
    host_image: &str,
) -> anyhow::Result<(Candidate, ServingManifest, PathBuf)> {
    let manifest = app.evidence.join("manifest.json");
    let candidate_path = app.evidence.join("candidate.json");
    let qualification = app.evidence.join("qualification.json");
    let mut prepare = Command::new(app.ctl());
    prepare
        .arg("prepare-release")
        .args([
            "--route-host",
            app.route_host,
            "--database-url",
            app.project_database_url,
            "--org",
            &app.triple.org,
            "--project",
            &app.triple.project,
            "--environment",
            app.triple.env.as_str(),
            "--tenant",
            app.tenant,
            "--release-digest",
            app.release_digest,
            "--artifact-base",
            app.release_artifact_base,
        ])
        .arg("--target-directory")
        .arg(app.target)
        .arg("--manifest-output")
        .arg(&manifest)
        .arg("--candidate-output")
        .arg(&candidate_path)
        .args(["--host-image", host_image])
        .args(["--identity-image", &app.native("identity")?])
        .args([
            "--native-registry-endpoint",
            &format!("{}-native-registry:5000", app.owner),
            "--native-registry-insecure",
        ]);
    if app.gates() {
        prepare.args(["--gates-image", &app.native("gates")?]);
    }
    step(app, "prepare-release", &mut prepare).await?;
    let candidate = Candidate::read(&candidate_path)?;
    native_registry_files(&candidate, app.work)?;
    step(
        app,
        "load-native",
        Command::new(app.adapter())
            .arg("load-native")
            .arg(app.owner)
            .arg(app.work)
            .arg(&app.kept.name),
    )
    .await?;
    step(
        app,
        "qualify-release",
        Command::new(app.ctl())
            .arg("qualify-release")
            .arg("--repository")
            .arg(app.repository)
            .arg("--revision")
            .arg(app.source)
            .arg("--candidate")
            .arg(&candidate_path)
            .arg("--result")
            .arg(&qualification),
    )
    .await?;
    let (manifest, digest) = candidate.manifest()?;
    ensure!(
        digest.as_str() == app.release_digest,
        "the candidate names release {digest}, not {}",
        app.release_digest
    );
    Ok((candidate, manifest, qualification))
}

/// The nodes' hosts file for the native registry, as the application
/// fixtures' `delivery::registry_files` writes it.
fn native_registry_files(candidate: &Candidate, work: &Path) -> anyhow::Result<()> {
    let endpoint = candidate
        .native_registry_endpoint
        .as_deref()
        .context("the candidate names its native registry")?;
    let (authority, _) = candidate
        .host_image
        .split_once('/')
        .context("the native host image has a registry authority")?;
    let endpoint = serde_json::to_string(&format!("http://{endpoint}"))?;
    fs::write(work.join("native-registry-authority"), authority)?;
    fs::write(
        work.join("native-registry-hosts.toml"),
        format!(
            "server = {endpoint}\n[host.{endpoint}]\n  capabilities = [\"pull\", \"resolve\"]\n"
        ),
    )?;
    Ok(())
}

/// Push the http and materializer role components over HTTP, and return
/// each as the TLS registry's `<repository>@sha256:<hex>`: one storage.
async fn push_roles(app: &Application<'_>) -> anyhow::Result<(String, String)> {
    let wash = String::from_utf8(
        checked(&mut Command::new(app.repository.join("tools/install-wash"))).await?,
    )?
    .trim()
    .to_owned();
    ensure!(!wash.is_empty(), "install-wash returned no executable path");
    let docker = app.work.join("role-docker");
    fs::create_dir_all(&docker)?;
    fs::copy(&app.kept.registry_auth, docker.join("config.json"))?;
    let mut pushed = Vec::new();
    for (file, repository) in [
        ("http_route.wasm", "flow-http"),
        ("materializer.wasm", "materializer"),
    ] {
        let reference = format!("{}/wamn/{repository}:{}", app.kept.http_registry, app.owner);
        let output = checked(
            Command::new(&wash)
                .env("DOCKER_CONFIG", &docker)
                .args(["-o", "json", "oci", "push", "--insecure", &reference])
                .arg(app.target.join("wasm32-wasip2/release").join(file)),
        )
        .await?;
        let body: Value =
            serde_json::from_slice(&output).context("read the role publication result")?;
        let digest = body["data"]["digest"]
            .as_str()
            .filter(|digest| digest.starts_with("sha256:"))
            .context("the role publication returned no digest")?;
        write_evidence(app.evidence, &format!("{repository}-push.json"), &body)?;
        pushed.push(format!("{}/wamn/{repository}@{digest}", app.kept.registry));
    }
    let materializer = pushed.pop().context("the materializer image")?;
    let http = pushed.pop().context("the http image")?;
    Ok((http, materializer))
}

/// The platform part of the host group (`WAMN_RELEASE_HOST_GROUP`): the
/// overlay's one group less what the release chart and `apply` derive, with
/// the kept registry in place of the fixture's, one replica, and each fixed
/// per-environment name replaced by its coordinate-named copy.
fn host_group(
    app: &Application<'_>,
    overlay: &Path,
    renames: &[(String, String)],
) -> anyhow::Result<PathBuf> {
    let values: Yaml = serde_yaml::from_str(&fs::read_to_string(overlay)?)
        .with_context(|| format!("parse {}", overlay.display()))?;
    let groups = values["runtime"]["hostGroups"]
        .as_sequence()
        .context("the overlay has host groups")?;
    ensure!(groups.len() == 1, "the overlay has one host group");
    let mut group = groups[0]
        .as_mapping()
        .context("the host group is a mapping")?
        .clone();
    for key in DERIVED_KEYS {
        group.remove(key);
    }
    group.insert("replicas".into(), REPLICAS.into());
    let env = group
        .get_mut("env")
        .and_then(Yaml::as_sequence_mut)
        .context("the host group has env")?;
    env.retain(|entry| {
        entry["name"]
            .as_str()
            .is_none_or(|name| !DERIVED_VARIABLES.contains(&name))
    });
    for entry in env.iter_mut() {
        let value = match entry["name"].as_str() {
            Some("WAMN_COMPONENT_ARTIFACT_BASE") => {
                format!("{}/wamn/components", app.kept.registry)
            }
            Some("DOCKER_CONFIG") => REGISTRY_MOUNT.to_owned(),
            Some("WAMN_REGISTRY_AUTH_FILE") => format!("{REGISTRY_MOUNT}/config.json"),
            _ => continue,
        };
        entry["value"] = value.into();
    }
    let volumes = group
        .get_mut("volumes")
        .and_then(Yaml::as_sequence_mut)
        .context("the host group has volumes")?;
    volumes.retain(|volume| volume["name"].as_str() != Some("registry-pull"));
    volumes.push(serde_yaml::from_str(&format!(
        "{{name: registry-pull, secret: {{secretName: {REGISTRY_SECRET}, optional: false, \
         items: [{{key: .dockerconfigjson, path: config.json}}, {{key: ca.crt, path: ca.crt}}]}}}}"
    ))?);
    let mounts = group
        .get_mut("volumeMounts")
        .and_then(Yaml::as_sequence_mut)
        .context("the host group has volume mounts")?;
    if !mounts
        .iter()
        .any(|mount| mount["name"].as_str() == Some("registry-pull"))
    {
        mounts.push(serde_yaml::from_str(&format!(
            "{{name: registry-pull, mountPath: {REGISTRY_MOUNT}, readOnly: true}}"
        ))?);
    }
    let paths = group
        .entry("ociCaPaths".into())
        .or_insert_with(|| Yaml::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("ociCaPaths is a list")?;
    paths.push(format!("{REGISTRY_MOUNT}/ca.crt").into());
    let mut group = Yaml::Mapping(group);
    rename(&mut group, renames);
    let path = app.work.join("host-group.yaml");
    fs::write(&path, serde_yaml::to_string(&group)?)?;
    fs::copy(&path, app.evidence.join("host-group.yaml"))?;
    Ok(path)
}

/// Replace every string equal to a fixed name by its coordinate-named copy.
fn rename(value: &mut Yaml, renames: &[(String, String)]) {
    match value {
        Yaml::String(text) => {
            if let Some((_, to)) = renames.iter().find(|(from, _)| from == text) {
                to.clone_into(text);
            }
        }
        Yaml::Sequence(items) => {
            for item in items {
                rename(item, renames);
            }
        }
        Yaml::Mapping(mapping) => {
            for (_, item) in mapping.iter_mut() {
                rename(item, renames);
            }
        }
        _ => {}
    }
}

/// Write the environment document and run `wamn-ctl env apply` on it. The
/// document is what `env show` synthesizes from the authorities, which the
/// fixture wrote, with the release digest and the route host (R21).
async fn apply(app: &Application<'_>, platform: &Platform) -> anyhow::Result<String> {
    let mut document = wamn_control::environment::show::show(platform, &app.triple).await?;
    document.release = DeclaredRelease::Digest(app.release_digest.to_owned());
    app.route_host.clone_into(&mut document.route_host);
    let file = app.evidence.join("environment.k");
    fs::write(&file, document.to_kcl())?;
    let mut command = Command::new(app.ctl());
    command.args(["env", "apply"]).arg(&file);
    for (name, value) in variables(platform) {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let output = step(app, "env-apply", &mut command).await?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// The environment of `wamn-ctl env` for `platform`
/// ([`Platform::from_env`]). `None` removes an inherited variable.
fn variables(platform: &Platform) -> Vec<(&'static str, Option<OsString>)> {
    let text = |value: &str| Some(OsString::from(value));
    let path = |value: &Path| Some(value.as_os_str().to_owned());
    let paths = platform
        .oci_ca_paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":");
    let events = platform.events.as_ref();
    vec![
        (
            "WAMN_CONTROL_DATABASE_URL",
            text(&platform.system_database_url),
        ),
        ("KUBECONFIG", path(&platform.target.kubeconfig)),
        ("WAMN_RELEASE_NAMESPACE", text(&platform.target.namespace)),
        ("WAMN_RELEASE_CHART", path(&platform.chart)),
        (
            "WAMN_RELEASE_ARTIFACT_BASE",
            text(&platform.release_artifact_base),
        ),
        (
            "WAMN_REGISTRY_AUTH_FILE",
            path(&platform.registry_auth_file),
        ),
        ("WAMN_OCI_CA_PATHS", text(&paths)),
        (
            "WAMN_PACKAGE_ARTIFACT_BASE",
            platform.package_artifact_base.as_deref().and_then(text),
        ),
        (
            "WAMN_DATABASE_NAMESPACE",
            text(&platform.database_namespace),
        ),
        (
            "WAMN_DATABASE_HOST",
            platform.database_host.as_deref().and_then(text),
        ),
        (
            "WAMN_DATABASE_PORT",
            text(&platform.database_port.to_string()),
        ),
        (
            "WAMN_RELEASE_HOST_GROUP",
            platform.host_group.as_deref().and_then(path),
        ),
        ("WAMN_IDENTITY_URL", None),
        ("WAMN_IDENTITY_CLIENT_CERT", None),
        ("WAMN_IDENTITY_CLIENT_KEY", None),
        ("WAMN_IDENTITY_SERVER_CA", None),
        (
            "WAMN_EVENT_NATS_URL",
            events.and_then(|events| text(&events.nats_url)),
        ),
        (
            "WAMN_EVENT_NATS_USERNAME",
            events.and_then(|events| text(&events.nats_username)),
        ),
        (
            "WAMN_EVENT_NATS_PASSWORD_FILE",
            events.and_then(|events| path(&events.nats_password_file)),
        ),
        (
            "WAMN_EVENT_STREAM_REPLICAS",
            events.and_then(|events| text(&events.stream_replicas.to_string())),
        ),
        (
            "WAMN_EVENT_DUP_WINDOW_SECS",
            events.and_then(|events| text(&events.dup_window_secs.to_string())),
        ),
    ]
}

/// The checks after `apply`: the hosts, each role the release implies, the
/// A1 probe, and A12 against every other environment release in the namespace.
async fn check(
    app: &Application<'_>,
    name: &str,
    host_image: &str,
    candidate: &Candidate,
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    let label = wamn_engine::release_manifest::release_label(app.release_digest)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let release = Release {
        cluster: &app.kept.name,
        work: &app.kept.work,
        namespace: NAMESPACE,
        name,
        label: &label,
        digest: app.release_digest,
        evidence: app.evidence,
    };
    let (_, runtime_digest) = candidate
        .host_image
        .rsplit_once('@')
        .context("the candidate host image is pinned by digest")?;
    let hosts = hosts_ready(&release, host_image, runtime_digest, REPLICAS).await?;
    let mut results = vec![json!({"check":"hosts ready","result":"pass","release":name})];
    let implied = roles(
        manifest,
        &app.triple.org,
        &app.triple.project,
        app.triple.env.as_str(),
    );
    for role in &implied {
        role_placed(&release, role.name.as_str(), &hosts).await?;
        results.push(json!({"check":"role placed","role":role.name.as_str(),"result":"pass"}));
    }
    if implied.iter().any(|role| role.name.as_str() == "http") {
        let probe = a1_probe(&release).await?;
        results
            .push(json!({"check":"A1 wrong release not placed","result":"pass","observed":probe}));
    }
    let environments: Vec<Environment> = checks::helm_releases(&app.kept.name, &app.kept.work)
        .await?
        .into_iter()
        .filter(|entry| entry["namespace"] == NAMESPACE && entry["chart"] == checks::CHART)
        .map(|entry| -> anyhow::Result<Environment> {
            let release = entry["name"].as_str().context("a release has a name")?;
            let values: Value = serde_json::from_slice(
                &std::process::Command::new("helm")
                    .args([
                        "get",
                        "values",
                        release,
                        "-n",
                        NAMESPACE,
                        "-o",
                        "json",
                        "--kubeconfig",
                    ])
                    .arg(app.kept.kubeconfig())
                    .args(["--kube-context", &app.kept.context()])
                    .output()
                    .context("start helm get values")?
                    .stdout,
            )
            .context("read the release values")?;
            let coordinate = values["environment"]
                .as_str()
                .context("the release values name the environment")?
                .replace('/', "--");
            Ok(Environment {
                name: release.to_owned(),
                coordinate,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    if environments.len() < 2 {
        results.push(json!({"check":"A12 two applications","result":"not-run",
            "reason":"this is the only environment on the kept cluster; the next case checks it"}));
    } else {
        let observed = checks::environments_isolated(
            &app.kept.name,
            &app.kept.work,
            NAMESPACE,
            &environments,
            app.evidence,
        )
        .await?;
        results.push(json!({"check":"A12 two applications","result":"pass","observed":observed}));
    }
    for result in &results {
        println!("DELIVERY-CHECK {result}");
    }
    write_evidence(app.evidence, "delivery-checks.json", &json!(results)).map(drop)
}

/// Write one result file into the evidence directory.
///
/// # Errors
///
/// When the file cannot be written.
pub fn write_evidence(evidence: &Path, name: &str, value: &Value) -> anyhow::Result<PathBuf> {
    let path = evidence.join(name);
    fs::write(&path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Run one owned delivery command in the repository, record it with its
/// output, and redact the application's secrets in the record.
async fn step(app: &Application<'_>, name: &str, command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let redact = |text: String| {
        app.secrets
            .iter()
            .fold(text, |text, secret| text.replace(secret, "<private>"))
    };
    let standard = command.as_std();
    let arguments = std::iter::once(standard.get_program())
        .chain(standard.get_args())
        .map(|value| redact(value.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    command.current_dir(app.repository).kill_on_drop(true);
    let output = wamn_control::delivery::qualification::execute_owned(command, COMMAND_TIMEOUT)
        .await
        .map_err(|error| anyhow::anyhow!(redact(format!("{error:#}"))))
        .with_context(|| format!("execute owned delivery {name}"))?;
    write_evidence(
        app.evidence,
        &format!("{name}-command.json"),
        &json!({
            "command":arguments,"exit_code":output.status.code(),
            "stdout":redact(String::from_utf8_lossy(&output.stdout).into_owned()),
            "stderr":redact(String::from_utf8_lossy(&output.stderr).into_owned()),
        }),
    )?;
    ensure!(
        output.status.success(),
        "owned delivery {name} failed with {}; see its command result",
        output.status
    );
    Ok(output.stdout)
}

async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start {program}"))?;
    ensure!(
        output.status.success(),
        "{program} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Copy the kept cluster's `kubeconfig` into the application's directory,
/// for the fixture helpers that read `<work>/kubeconfig`.
///
/// # Errors
///
/// When the copy fails.
pub fn share_kubeconfig(kept: &KeptCluster, work: &Path) -> anyhow::Result<()> {
    fs::copy(kept.kubeconfig(), work.join("kubeconfig")).context("copy the kept kubeconfig")?;
    Ok(())
}

/// The application's private directory under the kept cluster's. A second
/// run of the same application on one kept cluster is refused: its
/// environment already exists there.
///
/// # Errors
///
/// When the directory exists or cannot be made.
pub fn application_directory(kept: &KeptCluster, application: &str) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let work = kept.work.join(application);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&work)
        .with_context(|| {
            format!(
                "the kept cluster {} already ran the {application} delivery; remove it with \
                 tools/delivery-owned remove-kept {}",
                kept.name, kept.name
            )
        })?;
    Ok(work)
}

/// The kept cluster's mappings of one fixed per-environment name to its copy
/// named from the coordinate (§9.1): `<name>-<org>--<project>--<env>`.
pub fn coordinate_name(name: &str, triple: &Triple) -> String {
    format!(
        "{name}-{}--{}--{}",
        triple.org,
        triple.project,
        triple.env.as_str()
    )
}

/// Ensure the kept cluster named by `WAMN_DELIVERY_CLUSTER`.
///
/// # Errors
///
/// When the name is missing or refused, or the platform cannot be ensured.
pub async fn kept_cluster(repository: &Path) -> anyhow::Result<KeptCluster> {
    delivery_cluster::ensure(repository, &delivery_cluster::name_from_env()?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_group_keeps_only_the_platform_part() {
        let mut group: Yaml = serde_yaml::from_str(
            "{env: [{name: WAMN_EVT_NATS_USERNAME, valueFrom: {secretKeyRef: {name: wamn-event-nats}}}], \
              volumes: [{name: event-nats, secret: {secretName: wamn-event-nats}}]}",
        )
        .unwrap();
        rename(
            &mut group,
            &[(
                "wamn-event-nats".to_owned(),
                "wamn-event-nats-acme--wms--dev".to_owned(),
            )],
        );
        let text = serde_yaml::to_string(&group).unwrap();
        assert!(!text.contains("wamn-event-nats\n"));
        assert_eq!(text.matches("wamn-event-nats-acme--wms--dev").count(), 2);
        assert!(text.contains("name: event-nats"));
    }

    #[test]
    fn a_coordinate_name_is_the_secret_form() {
        assert_eq!(
            coordinate_name("host-session-public-ca", &Triple::new("acme", "wms", "dev")),
            "host-session-public-ca-acme--wms--dev"
        );
    }
}

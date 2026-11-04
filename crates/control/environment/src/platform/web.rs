//! Upload each environment's pinned web artifact through an install Job.

use std::path::{Path, PathBuf};
use std::time::Instant;
use wamn_package_control::web_artifact::{Sink, write_platform_ui};

use anyhow::{Context as _, ensure};
use serde_json::json;
use wamn_package_artifact::PackageRegistry;

use crate::environment::document::EnvironmentDocument;
use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::document::ClusterDocument;
use super::install::SecretData;
use super::job::{self, JobRequest, SecretMount};

pub(super) const UPLOAD: &str = "unset WAMN_EVENT_NATS_URL\nexport WAMN_CONTROL_DATABASE_URL=\"$(cat /etc/wamn/registry-reader/url)\"\nexec /usr/local/bin/wamn \"$@\"";

pub(super) const PLATFORM_UI: &str = "cd /inputs; exec /usr/local/bin/wamn web upload --platform-ui --org \"$ORG\" --bucket \"$BUCKET\"";

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    environments: &Path,
    directory: &Path,
    registry: &PackageRegistry,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for (_, environment) in super::broker::documents(environments)? {
        let coordinate = format!(
            "{}/{}/{}",
            environment.org, environment.project, environment.env
        );
        if document.buckets.web.is_empty() {
            for action in ["upload", "check"] {
                progress(&format!(
                    "4.19 Web {action} {coordinate}: skipped: no bucket"
                ))?;
            }
            continue;
        }
        if environment.client.is_none() {
            progress(&format!(
                "4.19 Web upload {coordinate}: skipped: no web client"
            ))?;
            continue;
        }
        let started = Instant::now();
        progress(&format!("4.19 Web upload {coordinate}: starting"))?;
        // The Job runs in the hosts namespace, so it copies the ctl NATS
        // client like the package and environment Jobs.
        let auth: SecretData = kubectl::get(
            &target.kubeconfig,
            &target.context,
            "platform",
            "secret",
            "wamn-ctl-nats",
        )
        .await?
        .context("missing Secret platform/wamn-ctl-nats")?;
        let (request, secrets, inputs) = upload_request(
            &environment,
            &document.buckets.web,
            &super::install::runner_image(directory)?,
            registry,
            &auth,
            &directory.join("release-chart"),
        )?;
        let mut target = target.clone();
        target.namespace = "hosts".into();
        let changed = job::run(&target, directory, &request, &inputs, &secrets).await?;
        progress(&format!(
            "4.19 Web upload {coordinate}: {} {:.1}s",
            if changed { "done" } else { "skipped" },
            started.elapsed().as_secs_f64()
        ))?;
    }
    Ok(())
}

pub(super) fn request(
    environment: &EnvironmentDocument,
    bucket: &str,
    image: &str,
    registry: &PackageRegistry,
    auth: &SecretData,
) -> anyhow::Result<JobRequest> {
    let client = environment
        .client
        .as_ref()
        .context("web upload requires a web client")?;
    ensure!(!bucket.is_empty(), "web upload requires a bucket");
    let env_key = wamn_control_registry::project_env_key(
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let mut request = JobRequest {
        name: format!("platform-web-{env_key}"),
        image: image.into(),
        calls_kubernetes: true,
        service_account: Some("wamn-host".into()),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![
            UPLOAD.into(),
            "web-upload".into(),
            "web".into(),
            "upload".into(),
            "--web".into(),
            client.web.clone(),
            "--org".into(),
            environment.org.clone(),
            "--artifact-base".into(),
            registry.artifact_base.clone(),
            "--registry-auth-file".into(),
            "/inputs/registry.json".into(),
            "--bucket".into(),
            format!("gs://{bucket}"),
        ],
        environment: [
            ("KUBECONFIG".into(), "/inputs/kubeconfig".into()),
            ("WAMN_RELEASE_NAMESPACE".into(), "hosts".into()),
            ("WAMN_RELEASE_CHART".into(), "/inputs".into()),
        ]
        .into(),
        secret_mounts: vec![SecretMount {
            name: format!("wamn-registry-reader-{env_key}"),
            path: "/etc/wamn/registry-reader".into(),
            keys: vec!["url".into()],
        }],
        ..JobRequest::default()
    };
    job::auth_nats_inputs(&mut request, auth)?;
    super::packages::registry_inputs(&mut request, registry)?;
    if registry.insecure_registry {
        request.arguments.push("--insecure-registry".into());
    }
    Ok(request)
}

pub(super) async fn platform_ui(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if document.buckets.web.is_empty() {
        for action in ["upload", "check"] {
            progress(&format!("4.19.1 Platform UI {action}: skipped: no bucket"))?;
        }
        return Ok(());
    }
    let started = Instant::now();
    progress("4.19.1 Platform UI upload: starting")?;
    let (request, secrets, inputs) = platform_ui_inputs(
        document,
        &super::install::runner_image(directory)?,
        repository,
        directory,
    )?;
    let hosts = Target {
        namespace: "hosts".into(),
        ..target.clone()
    };
    let changed = job::run(&hosts, directory, &request, &inputs, &secrets).await?;
    progress(&format!(
        "4.19.1 Platform UI upload: {} {:.1}s",
        if changed { "done" } else { "skipped" },
        started.elapsed().as_secs_f64()
    ))?;
    Ok(())
}

/// The platform UI Job with every file `wamn web upload --platform-ui` reads
/// under its working directory `/inputs`: the stamped chart and its build
/// receipt, the platform chart of the repository, and the built UI.
fn platform_ui_inputs(
    document: &ClusterDocument,
    image: &str,
    repository: &Path,
    directory: &Path,
) -> anyhow::Result<job::JobInputs> {
    let mut inputs = std::collections::BTreeMap::new();
    for name in ["Chart.yaml", crate::platform_build::BUILD_RECORD_FILE] {
        inputs.insert(
            format!("chart/{name}"),
            std::fs::read(directory.join("release-chart").join(name))?,
        );
    }
    inputs.insert(
        "deploy/platform/release/Chart.yaml".into(),
        std::fs::read(repository.join("deploy/platform/release/Chart.yaml"))?,
    );
    let dist = crate::platform_build::platform_ui_dist(repository);
    let mut files = Vec::new();
    wamn_package_control::web_artifact::collect(&dist, &mut files)?;
    for path in files {
        let relative = path
            .strip_prefix(&dist)?
            .to_str()
            .context("platform UI path is not UTF-8")?;
        inputs.insert(
            format!("apps/platform_ui/dist/{relative}"),
            std::fs::read(path)?,
        );
    }
    Ok(platform_ui_with_files(document, image, inputs))
}

pub(super) fn platform_ui_request(document: &ClusterDocument, image: &str) -> JobRequest {
    JobRequest {
        name: format!("platform-ui-{}", document.org),
        image: image.into(),
        service_account: Some("wamn-host".into()),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![PLATFORM_UI.into()],
        environment: [
            ("ORG".into(), document.org.clone()),
            ("BUCKET".into(), format!("gs://{}", document.buckets.web)),
            ("WAMN_RELEASE_CHART".into(), "/inputs/chart".into()),
        ]
        .into(),
        ..JobRequest::default()
    }
}

pub(super) fn upload_request(
    environment: &EnvironmentDocument,
    bucket: &str,
    image: &str,
    registry: &PackageRegistry,
    auth: &SecretData,
    chart: &Path,
) -> anyhow::Result<job::JobInputs> {
    let mut request = request(environment, bucket, image, registry, auth)?;
    super::environments::chart_files(chart, Path::new(""), &mut request)?;
    request.files.insert(
        "kubeconfig".into(),
        super::environments::kubeconfig().to_string(),
    );
    Ok((request, Vec::new(), json!({"environment":environment})))
}

pub(super) fn platform_ui_with_files(
    document: &ClusterDocument,
    image: &str,
    files: std::collections::BTreeMap<String, Vec<u8>>,
) -> job::JobInputs {
    let mut request = platform_ui_request(document, image);
    request.binary_files = files;
    (request, Vec::new(), json!({"org":document.org}))
}

/// The platform UI has one bucket and one prefix per platform version.
pub const PLATFORM_UI_BUCKET: &str = "gs://wamn-dev-web";

/// Upload the local platform UI only when its digest matches the stamped chart.
/// The current directory is the repository whose platform UI was built.
pub async fn upload_platform_ui(
    repository: &Path,
    chart: &Path,
    org: &str,
    bucket: &str,
) -> anyhow::Result<String> {
    let (dist, root) = platform_ui_source(repository, chart)?;
    let sink = Sink::parse(bucket)?;
    let root = [sink.prefix.trim_matches('/'), root.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let store = sink.store()?;
    write_platform_ui(store.as_ref(), &root, &dist, org).await?;
    Ok(format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket))
}

pub(crate) fn platform_ui_source(
    repository: &Path,
    chart: &Path,
) -> anyhow::Result<(PathBuf, String)> {
    use crate::platform_build::{
        BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
    };
    let build_record_path = chart.join(BUILD_RECORD_FILE);
    let build_record: PlatformBuildRecord =
        serde_json::from_slice(&std::fs::read(&build_record_path).with_context(|| {
            format!(
                "read {}; run `wamn-ctl build-platform`",
                build_record_path.display()
            )
        })?)?;
    let version = wamn_release_control::release_chart::chart_version(chart)?;
    let platform = wamn_release_control::release_chart::chart_version(
        &repository.join("deploy/platform/release"),
    )?;
    ensure!(
        version == platform,
        "the stamped chart version {version} differs from platform {platform}"
    );
    let dist = platform_ui_dist(repository);
    ensure!(
        platform_ui_digest(&dist)? == build_record.platform_ui,
        "the platform UI build {} differs from the platform build record {}",
        dist.display(),
        build_record_path.display()
    );
    Ok((dist, format!("platform-ui/{version}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_ui_job_carries_every_file_the_upload_reads() {
        use crate::platform_build::{
            BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
        };
        let work =
            std::env::temp_dir().join(format!("wamn-platform-ui-job-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let repository = work.join("repository");
        let directory = work.join("work");
        let dist = platform_ui_dist(&repository);
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), b"index").unwrap();
        std::fs::write(dist.join("assets/ui.js"), b"script").unwrap();
        let platform = repository.join("deploy/platform/release");
        std::fs::create_dir_all(&platform).unwrap();
        std::fs::write(platform.join("Chart.yaml"), "version: 0.1.7").unwrap();
        let chart = directory.join("release-chart");
        std::fs::create_dir_all(&chart).unwrap();
        std::fs::write(chart.join("Chart.yaml"), "version: 0.1.7").unwrap();
        std::fs::write(
            chart.join(BUILD_RECORD_FILE),
            serde_json::to_vec(&PlatformBuildRecord {
                platform_ui: platform_ui_digest(&dist).unwrap(),
                stage_durations_ms: std::collections::BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let document = serde_json::from_value(json!({
            "provider": "gcp", "project_id": "p", "zone": "z", "cluster_name": "test",
            "org": "test", "owner_email": "owner@example.com", "registry": "registry",
            "buckets": {"web": "test-web", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        let (request, _, _) =
            platform_ui_inputs(&document, "image", &repository, &directory).unwrap();
        // The Job's input volume, with the request's files only.
        let inputs = work.join("inputs");
        for (name, bytes) in request
            .files
            .iter()
            .map(|(name, text)| (name, text.as_bytes()))
            .chain(
                request
                    .binary_files
                    .iter()
                    .map(|(name, bytes)| (name, bytes.as_slice())),
            )
        {
            let path = inputs.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        assert_eq!(request.environment["WAMN_RELEASE_CHART"], "/inputs/chart");
        assert!(request.arguments[0].starts_with("cd /inputs;"));
        let (source, root) =
            crate::platform::web::platform_ui_source(&inputs, &inputs.join("chart")).unwrap();
        assert_eq!(source, platform_ui_dist(&inputs));
        assert_eq!(root, "platform-ui/0.1.7");
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    async fn empty_platform_bucket_reports_both_skips_without_reading_inputs() {
        let document = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "", "cluster_name": "test",
            "org": "test", "owner_email": "owner@example.com", "registry": "localhost:5000",
            "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        let target = Target {
            kubeconfig: "/absent".into(),
            context: "absent".into(),
            namespace: "platform".into(),
        };
        let mut lines = Vec::new();
        platform_ui(
            &target,
            &document,
            Path::new("/absent"),
            Path::new("/absent"),
            |line| {
                lines.push(line.to_owned());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(
            lines,
            [
                "4.19.1 Platform UI upload: skipped: no bucket",
                "4.19.1 Platform UI check: skipped: no bucket"
            ]
        );
    }

    #[tokio::test]
    async fn an_environment_without_a_client_skips_before_reading_job_inputs() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let environment: EnvironmentDocument = serde_json::from_value(json!({
            "org": "test", "project": "receiving", "env": "dev", "target": "dev",
            "release": "none", "route_host": "test.example.com", "policy": "dev",
            "applications": {}
        }))
        .unwrap();
        std::fs::write(scratch.path().join("environment.k"), environment.to_kcl()).unwrap();
        let document = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "", "cluster_name": "test",
            "org": "test", "owner_email": "owner@example.com", "registry": "localhost:5000",
            "buckets": {"web": "test-web", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.com"}, "resend_from": "sender@example.com"
        }))
        .unwrap();
        let registry = PackageRegistry {
            artifact_base: "registry/packages".into(),
            registry_auth_file: "/absent".into(),
            insecure_registry: false,
            oci_ca_paths: Vec::new(),
        };
        let target = Target {
            kubeconfig: "/absent".into(),
            context: "absent".into(),
            namespace: "platform".into(),
        };
        let mut lines = Vec::new();
        apply(
            &target,
            &document,
            scratch.path(),
            Path::new("/absent"),
            &registry,
            |line| {
                lines.push(line.to_owned());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(
            lines,
            ["4.19 Web upload test/receiving/dev: skipped: no web client"]
        );
    }

    #[test]
    fn upload_uses_the_web_digest_org_bucket_and_existing_identity_without_event_nats() {
        let environment: EnvironmentDocument = serde_json::from_value(json!({
            "org": "test", "project": "receiving", "env": "dev",
            "release": format!("sha256:{}", "a".repeat(64)),
            "client": {"package": "wamn_receiving", "web": format!("sha256:{}", "b".repeat(64))},
            "target": "cloud", "route_host": "test.example.com", "policy": "dev", "applications": {}
        }))
        .unwrap();
        let directory = std::env::temp_dir().join(format!("wamn-web-job-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let registry = PackageRegistry {
            artifact_base: "registry/packages".into(),
            registry_auth_file: directory.join("registry.json"),
            insecure_registry: false,
            oci_ca_paths: vec![],
        };
        std::fs::write(&registry.registry_auth_file, "{}").unwrap();
        let auth = serde_json::from_value(json!({"data": {
            "ca.crt": "Y2E=", "tls.crt": "Y2VydA==", "tls.key": "a2V5"
        }}))
        .unwrap();
        let request = request(
            &environment,
            "test-web",
            "registry/runner@sha256:abc",
            &registry,
            &auth,
        )
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(request.service_account.as_deref(), Some("wamn-host"));
        assert!(request.arguments.contains(&"gs://test-web".into()));
        assert!(
            request
                .arguments
                .windows(2)
                .any(|pair| pair == ["--web", &environment.client.as_ref().unwrap().web])
        );
        assert!(
            request
                .arguments
                .windows(2)
                .any(|pair| pair == ["--org", "test"])
        );
        assert!(
            request
                .arguments
                .windows(2)
                .any(|pair| pair == ["--artifact-base", "registry/packages"])
        );
        assert!(!request.arguments.contains(&"--release".into()));
        assert!(!request.arguments.contains(&environment.release.to_string()));
        assert!(!request.environment.contains_key("WAMN_EVENT_NATS_URL"));
        assert_eq!(
            request.secret_mounts[0].name,
            "wamn-registry-reader-test--receiving--dev"
        );
        assert_eq!(
            super::super::job_contract::access(&request).unwrap()[0].rules[0].verbs,
            ["get", "list"]
        );
    }
}

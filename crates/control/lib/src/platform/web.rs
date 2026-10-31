//! Upload each environment's released web clients through an install Job.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use serde_json::json;
use wamn_package_artifact::PackageRegistry;

use crate::environment::document::{DeclaredRelease, EnvironmentDocument};
use crate::release_chart::Target;

use super::document::ClusterDocument;
use super::job::{self, Access, JobRequest, PolicyRule, SecretMount};

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
        let started = Instant::now();
        progress(&format!("4.19 Web upload {coordinate}: starting"))?;
        let mut request = request(
            &environment,
            &document.buckets.web,
            &super::install::runner_image(directory)?,
            registry,
        )?;
        super::environments::chart_files(
            &directory.join("release-chart"),
            Path::new(""),
            &mut request,
        )?;
        request.files.insert(
            "kubeconfig".into(),
            super::environments::kubeconfig().to_string(),
        );
        let mut target = target.clone();
        target.namespace = "hosts".into();
        let changed = job::run(
            &target,
            directory,
            &request,
            &json!({"environment": environment}),
            &[],
        )
        .await?;
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
) -> anyhow::Result<JobRequest> {
    let DeclaredRelease::Digest(digest) = &environment.release else {
        anyhow::bail!("web upload requires a declared release");
    };
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
            "unset WAMN_EVENT_NATS_URL\nexport WAMN_CONTROL_DATABASE_URL=\"$(cat /etc/wamn/registry-reader/url)\"\nexec /usr/local/bin/wamn \"$@\"".into(),
            "web-upload".into(), "web".into(), "upload".into(),
            format!("{}/{}/{}", environment.org, environment.project, environment.env),
            "--release".into(), digest.clone(), "--bucket".into(), format!("gs://{bucket}/clients"),
        ],
        environment: [
            ("KUBECONFIG".into(), "/inputs/kubeconfig".into()),
            ("WAMN_RELEASE_NAMESPACE".into(), "hosts".into()),
            ("WAMN_RELEASE_CHART".into(), "/inputs".into()),
        ].into(),
        secret_mounts: vec![
            SecretMount { name: format!("wamn-registry-reader-{env_key}"), path: "/etc/wamn/registry-reader".into(), keys: vec!["url".into()] },
        ],
        permissions: vec![Access {
            namespace: "hosts".into(),
            rules: vec![PolicyRule {
                api_groups: vec![String::new()], resources: vec!["secrets".into()],
                verbs: ["get", "list"].map(str::to_owned).into(),
            }],
        }],
        ..JobRequest::default()
    };
    job::auth_nats_inputs(&mut request, None)?;
    super::packages::registry_inputs(&mut request, registry)?;
    request.environment.insert(
        "WAMN_PACKAGE_ARTIFACT_BASE".into(),
        registry.artifact_base.clone(),
    );
    request.environment.insert(
        "WAMN_RELEASE_ARTIFACT_BASE".into(),
        super::packages::artifact_base(registry, "releases")?,
    );
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
    let request = platform_ui_inputs(
        document,
        &super::install::runner_image(directory)?,
        repository,
        directory,
    )?;
    let hosts = Target {
        namespace: "hosts".into(),
        ..target.clone()
    };
    let changed = job::run(
        &hosts,
        directory,
        &request,
        &json!({"org": document.org}),
        &[],
    )
    .await?;
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
) -> anyhow::Result<JobRequest> {
    let mut request = platform_ui_request(document, image);
    for name in ["Chart.yaml", crate::platform_build::RECEIPT_FILE] {
        request.binary_files.insert(
            format!("chart/{name}"),
            std::fs::read(directory.join("release-chart").join(name))?,
        );
    }
    request.binary_files.insert(
        "deploy/platform/release/Chart.yaml".into(),
        std::fs::read(repository.join("deploy/platform/release/Chart.yaml"))?,
    );
    let dist = crate::platform_build::platform_ui_dist(repository);
    let mut files = Vec::new();
    crate::web_upload::collect(&dist, &mut files)?;
    for path in files {
        let relative = path
            .strip_prefix(&dist)?
            .to_str()
            .context("platform UI path is not UTF-8")?;
        request.binary_files.insert(
            format!("apps/platform_ui/dist/{relative}"),
            std::fs::read(path)?,
        );
    }
    Ok(request)
}

pub(super) fn platform_ui_request(document: &ClusterDocument, image: &str) -> JobRequest {
    JobRequest {
        name: format!("platform-ui-{}", document.org),
        image: image.into(),
        service_account: Some("wamn-host".into()),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![
            "cd /inputs; exec /usr/local/bin/wamn web upload --platform-ui --org \"$ORG\" --bucket \"$BUCKET\"".into(),
        ],
        environment: [
            ("ORG".into(), document.org.clone()),
            ("BUCKET".into(), format!("gs://{}", document.buckets.web)),
            ("WAMN_RELEASE_CHART".into(), "/inputs/chart".into()),
        ].into(),
        ..JobRequest::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_ui_job_carries_every_file_the_upload_reads() {
        use crate::platform_build::{
            PlatformBuildReceipt, RECEIPT_FILE, platform_ui_digest, platform_ui_dist,
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
            chart.join(RECEIPT_FILE),
            serde_json::to_vec(&PlatformBuildReceipt {
                platform_ui: platform_ui_digest(&dist).unwrap(),
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
        let request = platform_ui_inputs(&document, "image", &repository, &directory).unwrap();
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
            crate::web_upload::platform_ui_source(&inputs, &inputs.join("chart")).unwrap();
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

    #[test]
    fn upload_uses_the_release_bucket_and_existing_identity_without_event_nats() {
        let environment: EnvironmentDocument = serde_json::from_value(json!({
            "org": "test", "project": "receiving", "env": "dev",
            "release": format!("sha256:{}", "a".repeat(64)),
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
        let request = request(
            &environment,
            "test-web",
            "registry/runner@sha256:abc",
            &registry,
        )
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(request.service_account.as_deref(), Some("wamn-host"));
        assert!(request.arguments.contains(&"gs://test-web/clients".into()));
        assert!(request.arguments.contains(&environment.release.to_string()));
        assert!(!request.environment.contains_key("WAMN_EVENT_NATS_URL"));
        assert_eq!(
            request.secret_mounts[0].name,
            "wamn-registry-reader-test--receiving--dev"
        );
        assert_eq!(request.permissions[0].rules[0].verbs, ["get", "list"]);
    }
}

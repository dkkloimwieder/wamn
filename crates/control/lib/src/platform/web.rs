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
        )?;
        super::packages::registry_inputs(&mut request, registry)?;
        super::environments::chart_files(
            &directory.join("release-chart"),
            Path::new(""),
            &mut request,
        )?;
        request.files.insert(
            "kubeconfig".into(),
            super::environments::kubeconfig().to_string(),
        );
        request.environment.insert(
            "WAMN_PACKAGE_ARTIFACT_BASE".into(),
            registry.artifact_base.clone(),
        );
        request.environment.insert(
            "WAMN_RELEASE_ARTIFACT_BASE".into(),
            super::packages::artifact_base(registry, "releases")?,
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

fn request(
    environment: &EnvironmentDocument,
    bucket: &str,
    image: &str,
) -> anyhow::Result<JobRequest> {
    let DeclaredRelease::Digest(digest) = &environment.release else {
        anyhow::bail!("web upload requires a declared release");
    };
    ensure!(!bucket.is_empty(), "web upload requires a bucket");
    let tenant = wamn_control_registry::project_env_tenant(
        &environment.org,
        &environment.project,
        &environment.env,
    );
    Ok(JobRequest {
        name: format!("platform-web-{tenant}"),
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
            ("WAMN_AUTH_NATS_URL".into(), "nats://nats.platform.svc.cluster.local:4222".into()),
            ("WAMN_AUTH_NATS_TLS_CA".into(), "/etc/wamn/auth-nats/ca.crt".into()),
            ("WAMN_AUTH_NATS_TLS_CERT".into(), "/etc/wamn/auth-nats/tls.crt".into()),
            ("WAMN_AUTH_NATS_TLS_KEY".into(), "/etc/wamn/auth-nats/tls.key".into()),
        ].into(),
        secret_mounts: vec![
            SecretMount { name: format!("wamn-registry-reader-{tenant}"), path: "/etc/wamn/registry-reader".into(), keys: vec!["url".into()] },
            SecretMount { name: "wasmcloud-runtime-tls".into(), path: "/etc/wamn/auth-nats".into(), keys: ["ca.crt", "tls.crt", "tls.key"].map(str::to_owned).into() },
        ],
        permissions: vec![Access {
            namespace: "hosts".into(),
            rules: vec![PolicyRule {
                api_groups: vec![String::new()], resources: vec!["secrets".into()],
                verbs: ["get", "list"].map(str::to_owned).into(),
            }],
        }],
        ..JobRequest::default()
    })
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
    let mut request = JobRequest {
        name: format!("platform-ui-{}", document.org),
        image: super::install::runner_image(directory)?,
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
    };
    for name in ["Chart.yaml", crate::platform_build::RECEIPT_FILE] {
        request.binary_files.insert(
            format!("chart/{name}"),
            std::fs::read(directory.join("release-chart").join(name))?,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let request = request(&environment, "test-web", "registry/runner@sha256:abc").unwrap();
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

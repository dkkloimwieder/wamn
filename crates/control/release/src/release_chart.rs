//! The `release` chart of one environment
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! An environment is one Helm release of `deploy/platform/release`: the pinned
//! runtime-operator subchart, host-only, plus one WorkloadDeployment per role.
//! This module derives the release name, renders the install values, stamps the
//! platform image set and the qualification images into the chart's defaults,
//! reads them back, and runs the Helm install and uninstall.
//!
//! The platform image set lives only in the chart's default values (R15). The
//! install values carry no image digest and no integer release id: the release
//! is named by its manifest digest alone (R1).

pub(super) mod plan;
pub use plan::{events_plugin, release_name, roles, values, workload_name};
use plan::{recovery, split_host_image};

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use tokio::process::Command;
use wamn_catalog::{AttachmentType, ServingManifest};
use wamn_engine::release_manifest::release_label;

/// The platform version: `version` in the `Chart.yaml` of the release chart
/// directory `chart`. The chart is its one owner, so a binary does not change
/// with the version (wamn-6qo7v).
pub fn chart_version(chart: &Path) -> anyhow::Result<String> {
    read_platform_version(&chart.join("Chart.yaml"))
}

/// Read the platform version from the release chart's top-level version.
///
/// # Errors
///
/// Returns an error naming the path if the chart cannot be read or decoded.
pub fn read_platform_version(path: &Path) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Chart {
        version: String,
    }

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("read platform chart {}", path.display()))?;
    let chart: Chart = serde_yaml::from_str(&source).with_context(|| {
        format!(
            "read top-level version from platform chart {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        !chart.version.trim().is_empty(),
        "platform chart {} has an empty version",
        path.display()
    );
    Ok(chart.version)
}

pub use plan::ImageSet;

pub use plan::QualificationImages;

/// Copy `chart` to `out` with the image set, and the qualification images
/// when given, written into its default values.
///
/// The platform build step stamps both; its chart's defaults are the one
/// source of them. A case that copies the roles by digest into its own
/// registry stamps a copy of that chart with the copied set and `None`, so
/// the copy keeps the source's qualification images. `chart` must hold its
/// built subchart archive.
pub fn stamp(
    chart: &Path,
    set: &ImageSet,
    qualification: Option<&QualificationImages>,
    out: &Path,
) -> anyhow::Result<PathBuf> {
    let (repository, tag) = split_host_image(&set.host)?;
    let mut pinned = vec![("http", &set.http), ("materializer", &set.materializer)];
    if let Some(images) = qualification {
        pinned.extend([
            ("runner", &images.runner),
            ("gates", &images.gates),
            ("identity", &images.identity),
        ]);
    }
    for (name, image) in pinned {
        ensure!(
            image.contains("@sha256:"),
            "the {name} image {image} is not pinned by digest"
        );
    }
    copy_dir(chart, out)?;
    let path = out.join("values.yaml");
    let mut values: Value = serde_yaml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("parse the chart's default values")?;
    let image = &mut values["runtime-operator"]["runtime"]["image"];
    image["registry"] = "".into();
    image["repository"] = repository.into();
    image["tag"] = tag.into();
    let platform = &mut values["platform"];
    platform["roles"]["http"]["image"] = set.http.clone().into();
    platform["roles"]["materializer"]["image"] = set.materializer.clone().into();
    if let Some(images) = qualification {
        platform["runner"]["image"] = images.runner.clone().into();
        platform["gates"]["image"] = images.gates.clone().into();
        platform["identity"]["image"] = images.identity.clone().into();
    }
    std::fs::write(&path, serde_yaml::to_string(&values)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(out.to_owned())
}

/// The default values of `chart`, with `helm show values`.
async fn default_values(chart: &Path) -> anyhow::Result<Value> {
    let shown = helm(&["show".as_ref(), "values".as_ref(), chart.as_os_str()]).await?;
    serde_yaml::from_str(&shown).context("parse the chart's values")
}

/// A stamped default of the chart, which must not be empty.
fn stamped(value: &Value, name: &str) -> anyhow::Result<String> {
    let text = value.as_str().unwrap_or_default();
    ensure!(
        !text.is_empty(),
        "the chart's default {name} is not stamped"
    );
    Ok(text.to_owned())
}

/// Read the image set from the default values of `chart`.
pub async fn image_set(chart: &Path) -> anyhow::Result<ImageSet> {
    let values = default_values(chart).await?;
    let image = &values["runtime-operator"]["runtime"]["image"];
    let registry = image["registry"].as_str().unwrap_or_default();
    let repository = stamped(&image["repository"], "host repository")?;
    let tag = stamped(&image["tag"], "host tag")?;
    let host = if registry.is_empty() {
        format!("{repository}:{tag}")
    } else {
        format!("{registry}/{repository}:{tag}")
    };
    let roles = &values["platform"]["roles"];
    Ok(ImageSet {
        host,
        http: stamped(&roles["http"]["image"], "http image")?,
        materializer: stamped(&roles["materializer"]["image"], "materializer image")?,
    })
}

/// Read the qualification images from the default values of `chart`.
pub async fn qualification_images(chart: &Path) -> anyhow::Result<QualificationImages> {
    let values = default_values(chart).await?;
    let platform = &values["platform"];
    Ok(QualificationImages {
        runner: stamped(&platform["runner"]["image"], "runner image")?,
        gates: stamped(&platform["gates"]["image"], "gates image")?,
        identity: stamped(&platform["identity"]["image"], "identity image")?,
    })
}

pub use plan::Role;

pub use plan::RoleName;

pub use plan::MATERIALIZER_DRAIN_BOUND_SECONDS;

pub use plan::TELEMETRY_VARIABLES;

pub use plan::ValuesInput;

pub use plan::SOCKET_EGRESS;

/// Where Helm writes: an explicit kubeconfig, context and namespace.
#[derive(Debug, Clone)]
pub struct Target {
    pub kubeconfig: PathBuf,
    pub context: String,
    /// The release namespace (R6).
    pub namespace: String,
}

/// Install or upgrade one environment's release at the version of `chart` and
/// wait for its host Deployment. `description` is kept with the revision.
pub async fn upgrade(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Path,
    timeout: Duration,
    description: &str,
) -> anyhow::Result<()> {
    let timeout = format!("{}s", timeout.as_secs());
    let version = chart_version(chart)?;
    helm(&[
        "upgrade".as_ref(),
        "--install".as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--version".as_ref(),
        version.as_ref(),
        "--description".as_ref(),
        description.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        "-f".as_ref(),
        values.as_os_str(),
        "--skip-crds".as_ref(),
        "--wait".as_ref(),
        "--timeout".as_ref(),
        timeout.as_ref(),
        "--rollback-on-failure".as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ])
    .await
    .map(drop)
}

use plan::Recovery;

/// Clear a pending operation on one environment's release (wamn-jovpg).
///
/// Only the apply of this environment writes its release, and it holds the
/// install lock, so a pending operation here belongs to a writer that died,
/// for example an install Job deleted while Helm waited. Helm refuses every
/// later upgrade with "another operation (install/upgrade/rollback) is in
/// progress" until the pending revision is cleared.
///
/// # Errors
///
/// When Helm fails.
pub async fn recover_pending(target: &Target, name: &str) -> anyhow::Result<()> {
    let filter = format!("^{name}$");
    let listed = helm(&[
        "list".as_ref(),
        "--pending".as_ref(),
        "--filter".as_ref(),
        filter.as_ref(),
        "-o".as_ref(),
        "json".as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
    ])
    .await?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_str(&listed).context("decode helm list")?;
    let Some(status) = releases
        .iter()
        .find(|release| release["name"] == name)
        .and_then(|release| release["status"].as_str())
    else {
        return Ok(());
    };
    match recovery(status) {
        Some(Recovery::Uninstall) => uninstall(target, name).await,
        Some(Recovery::Rollback) => helm(&[
            "rollback".as_ref(),
            name.as_ref(),
            "--kubeconfig".as_ref(),
            target.kubeconfig.as_os_str(),
            "--kube-context".as_ref(),
            target.context.as_ref(),
            "--namespace".as_ref(),
            target.namespace.as_ref(),
            "--wait".as_ref(),
        ])
        .await
        .map(drop),
        None => Ok(()),
    }
}

/// Uninstall one environment's release and wait for its objects to go.
pub async fn uninstall(target: &Target, name: &str) -> anyhow::Result<()> {
    helm(&[
        "uninstall".as_ref(),
        name.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        "--wait".as_ref(),
    ])
    .await
    .map(drop)
}

async fn helm(arguments: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    let output = Command::new("helm")
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")?;
    ensure!(
        output.status.success(),
        "helm {} exited {}: {}",
        arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("helm wrote text")
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn only_a_pending_operation_is_recovered() {
        assert_eq!(recovery("pending-install"), Some(Recovery::Uninstall));
        assert_eq!(recovery("pending-upgrade"), Some(Recovery::Rollback));
        assert_eq!(recovery("pending-rollback"), Some(Recovery::Rollback));
        assert_eq!(recovery("deployed"), None);
        assert_eq!(recovery("failed"), None);
    }

    const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn input() -> ValuesInput {
        let mut group = Mapping::new();
        group.insert("replicas".into(), 3.into());
        group.insert(
            "env".into(),
            serde_yaml::from_str(
                "[{name: WAMN_COMPONENT_ARTIFACT_BASE, value: registry.example/components}]",
            )
            .expect("yaml"),
        );
        ValuesInput {
            org: "acme".into(),
            project: "wms".into(),
            env: "prod".into(),
            manifest_digest: DIGEST.into(),
            artifact_base: "registry.example/releases".into(),
            route_host: "wms.acme.example".into(),
            applications: [(
                "wms".to_owned(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["wamn_wms".to_owned()].into(),
                },
            )]
            .into(),
            roles: vec![Role {
                entry: Some(format!("entries@sha256:{}", "a".repeat(64))),
                statements: vec!["sql-0".to_owned()],
                name: RoleName::Http,
                config: Mapping::new(),
                environment: None,
            }],
            drain_bound_seconds: 300,
            actor: "ops".into(),
            host_group: group,
        }
    }

    #[test]
    fn the_chart_version_and_app_version_are_one() {
        let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/release");
        let version = chart_version(&chart).expect("Chart.yaml has a version");
        let text = std::fs::read_to_string(chart.join("Chart.yaml")).expect("read Chart.yaml");
        let chart: Value = serde_yaml::from_str(&text).expect("Chart.yaml parses");
        assert_eq!(chart["appVersion"].as_str(), Some(version.as_str()));
    }

    #[test]
    fn release_name_is_bounded_stable_and_cut() {
        use wamn_control_provision::environment_state::{HASH_LEN, SLUG_LEN};
        let name = release_name("acme", "wms", "prod").expect("valid coordinate");
        assert_eq!(
            name,
            release_name("acme", "wms", "prod").expect("valid coordinate")
        );
        assert!(name.starts_with("r-prod-") && name.len() == "r-prod-".len() + HASH_LEN);
        assert_eq!(
            release_name("acme", "wms", "dev").expect("valid coordinate"),
            "r-dev-2d9920a989cb26764e77"
        );
        // A valid env is bounded by the database name it provisions, so a short
        // org and project leave room for an env longer than the slug.
        let long = "a".repeat(30);
        let name = release_name("a", "wms", &long).expect("valid coordinate");
        assert_eq!(name.len(), 2 + SLUG_LEN + 1 + HASH_LEN);
        assert!(name.len() <= 47);
        let other = release_name("a", "wms", &format!("{}b", "a".repeat(29))).expect("valid");
        assert_ne!(name, other, "envs that share the slug differ by hash");
        assert_ne!(
            release_name("acme", "wms", "prod").expect("valid"),
            release_name("acme", "wms2", "prod").expect("valid")
        );
    }

    #[test]
    fn values_pin_the_release_and_entry_without_overriding_the_runtime_image() {
        let values = values(&input()).expect("values render");
        let name = release_name("acme", "wms", "prod").expect("valid coordinate");
        let label = release_label(DIGEST).expect("digest");
        assert!(values.get("releaseLabel").is_none());
        let runtime = &values["runtime-operator"]["runtime"];
        assert_eq!(runtime["terminationGracePeriodSeconds"].as_u64(), Some(300));
        assert_eq!(
            runtime["serviceAccount"]["name"],
            "wamn-host-acme--wms--prod"
        );
        assert_eq!(runtime["serviceAccount"]["create"], false);
        assert_eq!(runtime["automountServiceAccountToken"], true);
        assert_eq!(
            runtime["podLabels"]["wamn.release"].as_str(),
            Some(label.as_str())
        );
        assert_eq!(
            runtime["podAnnotations"]["wamn.release-digest"].as_str(),
            Some(DIGEST)
        );
        assert_eq!(
            runtime["podAnnotations"]["wamn.environment"].as_str(),
            Some("acme/wms/prod")
        );
        let group = &runtime["hostGroups"][0];
        assert_eq!(group["name"].as_str(), Some(name.as_str()));
        assert_eq!(group["replicas"].as_u64(), Some(3));
        assert_eq!(
            group["ports"],
            serde_yaml::from_str::<Value>("[{name: files, containerPort: 8082, protocol: TCP}]")
                .expect("yaml")
        );
        assert_eq!(
            group["env"],
            serde_yaml::from_str::<Value>(
                "[{name: WAMN_COMPONENT_ARTIFACT_BASE, value: registry.example/components}, \
                 {name: WAMN_ORG, value: acme}, {name: WAMN_PROJECT, value: wms}, \
                 {name: WAMN_DRAIN_BOUND_SECONDS, value: '300'}, \
                 {name: WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS, value: '10'}, \
                 {name: OTEL_EXPORTER_OTLP_ENDPOINT, \
                  value: 'http://otel-collector.platform.svc.cluster.local:4317'}, \
                 {name: OTEL_SERVICE_NAME, value: wamn-host}, \
                 {name: OTEL_TRACES_SAMPLER, value: parentbased_traceidratio}, \
                 {name: OTEL_TRACES_SAMPLER_ARG, value: '0.1'}]"
            )
            .expect("yaml")
        );
        assert_eq!(
            group["extraArgs"],
            serde_yaml::from_str::<Value>(&format!(
                "[--release-artifact-base=registry.example/releases, --release-manifest-digest={DIGEST}, \
                 --environment=prod, --files-addr=0.0.0.0:8082, '--applications={{\"wms\":{{\"packages\":[\"wamn_wms\"]}}}}']"
            ))
            .expect("yaml")
        );
        assert_eq!(
            values["applications"],
            serde_yaml::from_str::<Value>("{wms: {packages: [wamn_wms]}}").expect("yaml")
        );
        assert_eq!(group["networking"]["socketEgress"].as_str(), Some("count"));
        assert!(runtime.get("image").is_none());
        assert_eq!(values["audit"]["actor"].as_str(), Some("ops"));
        assert!(values.get("platform").is_none());
        assert_eq!(
            values["roles"][0]["entry"].as_str(),
            Some(
                format!(
                    "registry.example/components/entries@sha256:{}",
                    "a".repeat(64)
                )
                .as_str()
            )
        );
    }

    #[test]
    fn a_retired_workflow_http_attachment_does_not_render_the_http_role() {
        let value = serde_json::json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},
            "components":[],"routes":[],"attachments":{},
            "workflow":{"attachments":{"orders-http":{"type":"http","wiring-id":"orders"}}}
        });
        let (manifest, _) = ServingManifest::from_canonical_bytes(
            &wamn_execution_contract::canonical_json_bytes(&value),
        )
        .unwrap();
        let roles = roles(&manifest, "acme", "orders", "dev");
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, RoleName::Materializer);
    }

    #[test]
    fn an_empty_release_still_renders_the_materializer() {
        let manifest: ServingManifest = serde_json::from_value(serde_json::json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": []}, "components": [], "routes": [], "attachments": {},
        }))
        .unwrap();
        let roles = roles(&manifest, "acme", "orders", "dev");
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, RoleName::Materializer);
        assert_eq!(
            roles[0].environment.as_ref().unwrap()["WAMN_MAT_STREAM"].as_str(),
            Some(wamn_control_provision::event_stream_name("acme", "orders", "dev").as_str())
        );
    }

    #[test]
    fn the_named_application_supplies_the_http_statement_instances() {
        let mut manifest: ServingManifest = serde_json::from_value(serde_json::json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": []}, "components": [], "routes": [],
            "attachments": {"orders-http": {
                "type": "http", "package-id": "wamn_wms", "component": "wms",
                "operation": "get", "definition-hash": DIGEST,
                "definition": {}, "auth-policy": {"modes": ["none"]}
            }}
        }))
        .unwrap();

        let application = wamn_catalog::ServingEntryMember {
            package_id: "wamn_wms".into(),
            component: "wms".into(),
            digest: DIGEST.into(),
        };
        for (name, imports) in [
            ("wms", vec!["sql-application"]),
            ("base", vec!["sql-0", "sql-1"]),
        ] {
            manifest.components.insert(wamn_catalog::ServingComponent {
                package_id: "wamn_wms".into(),
                descriptor: wamn_catalog::ComponentDescriptor::named(name, "0.1", DIGEST),
                operations: BTreeMap::new(),
                runtime_schema: None,
                statement_instances: imports
                    .into_iter()
                    .map(|name| (name.to_owned(), BTreeMap::new()))
                    .collect(),
            });
        }
        manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
            reference: format!("entries@{DIGEST}"),
            route_digest: DIGEST.into(),
            application: application.clone(),
            members: [
                application,
                wamn_catalog::ServingEntryMember {
                    package_id: "wamn_wms".into(),
                    component: "base".into(),
                    digest: DIGEST.into(),
                },
            ]
            .into(),
        });
        assert_eq!(
            roles(&manifest, "acme", "orders", "dev")[0].statements,
            ["sql-application"],
            "the named application owns the imports even when another member has more"
        );
    }

    #[test]
    fn values_refuse_a_materializer_bound_equal_to_or_above_the_host_bound() {
        for host_bound in [
            0,
            MATERIALIZER_DRAIN_BOUND_SECONDS,
            MATERIALIZER_DRAIN_BOUND_SECONDS - 1,
        ] {
            let mut input = input();
            input.drain_bound_seconds = host_bound;
            let error = values(&input).expect_err("materializer must stop before its host");
            assert_eq!(
                error.to_string(),
                format!(
                    "materializer_drain_bound_seconds (10) must be shorter than drain_bound_seconds ({host_bound})"
                )
            );
        }
        for host_bound in [MATERIALIZER_DRAIN_BOUND_SECONDS + 1, 30] {
            let mut input = input();
            input.drain_bound_seconds = host_bound;
            assert!(values(&input).is_ok(), "host bound {host_bound} is valid");
        }
    }

    #[test]
    fn values_refuse_an_entry_without_a_component_base() {
        let mut input = input();
        input.host_group.remove("env");
        assert!(values(&input).is_err());
    }

    #[test]
    fn values_refuse_a_host_group_that_sets_derived_keys() {
        for key in ["name", "namespace", "service", "extraArgs", "image"] {
            let mut input = input();
            input.host_group.insert(key.into(), "x".into());
            assert!(values(&input).is_err(), "{key} is refused");
        }
        let mut input = input();
        input.manifest_digest = "e3b0".into();
        assert!(values(&input).is_err());
        for variable in [
            "WAMN_ORG",
            "WASMCLOUD_HOST_ENVIRONMENT",
            "WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT",
            "WASMTIME_POOLING_TABLE_KEEP_RESIDENT",
            "WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT",
        ] {
            let mut input = super::tests::input();
            input.host_group.insert(
                "env".into(),
                serde_yaml::from_str(&format!("[{{name: {variable}, value: other}}]"))
                    .expect("yaml"),
            );
            assert!(values(&input).is_err(), "{variable} is refused");
        }
    }

    #[test]
    fn host_image_splits_into_the_subcharts_fields() {
        assert_eq!(
            split_host_image("localhost:5000/wamn-host:dev@sha256:ab").expect("split"),
            (
                "localhost:5000/wamn-host".to_owned(),
                "dev@sha256:ab".to_owned()
            )
        );
        assert!(split_host_image("localhost:5000/wamn-host@sha256:ab").is_err());
        assert!(split_host_image("wamn-host:dev").is_err());
    }
}

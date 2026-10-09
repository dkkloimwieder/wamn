//! The `release` chart of one environment (docs/plan/platform-deploy.md §9).
//!
//! An environment is one Helm release of `deploy/platform/release`: the pinned
//! runtime-operator subchart, host-only, plus one WorkloadDeployment per role.
//! This module derives the release name, renders the install values, stamps the
//! platform image set into the chart's defaults, reads the set back, and runs
//! the Helm install and uninstall.
//!
//! The platform image set lives only in the chart's default values (R15). The
//! install values carry no image digest and no integer release id: the release
//! is named by its manifest digest alone (R1).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_yaml::{Mapping, Value};
use tokio::process::Command;
use wamn_catalog::{AttachmentType, ServingManifest};
use wamn_engine::release_manifest::release_label;

/// The chart version this module renders for. It equals `version` in
/// `deploy/platform/release/Chart.yaml`.
pub const CHART_VERSION: &str = "0.1.0";

/// The longest slug in a release name: the env name cut to this length.
const SLUG_LEN: usize = 24;
/// The hex characters of the coordinate hash in a release name (80 bits).
const HASH_LEN: usize = 20;

/// The Helm release name of an environment: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/project/env`. The name is at most 47
/// characters, inside Helm's bound of 53. It is also the environment's host group.
pub fn release_name(org: &str, project: &str, env: &str) -> anyhow::Result<String> {
    wamn_control_provision::validate_project_env(org, project, env)
        .context("the environment coordinate is not valid")?;
    let coordinate = format!("{org}/{project}/{env}");
    let hash = hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        coordinate.as_bytes(),
    ));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("r-{slug}-{}", &hash[..HASH_LEN]))
}

/// The platform image set: the host image and the role component images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSet {
    /// `<repository>:<tag>@sha256:<hex>`.
    pub host: String,
    /// `<repository>@sha256:<hex>`.
    pub http: String,
    /// `<repository>@sha256:<hex>`.
    pub materializer: String,
}

/// Copy `chart` to `out` with the image set written into its default values.
///
/// This is the platform build step: the stamped chart's defaults are the one
/// source of the image set. `chart` must hold its built subchart archive.
pub fn stamp(chart: &Path, set: &ImageSet, out: &Path) -> anyhow::Result<PathBuf> {
    let (repository, tag) = split_host_image(&set.host)?;
    for (role, image) in [("http", &set.http), ("materializer", &set.materializer)] {
        ensure!(
            image.contains("@sha256:"),
            "the {role} image {image} is not pinned by digest"
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
    values["platform"]["roles"]["http"]["image"] = set.http.clone().into();
    values["platform"]["roles"]["materializer"]["image"] = set.materializer.clone().into();
    std::fs::write(&path, serde_yaml::to_string(&values)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(out.to_owned())
}

/// Read the image set from the default values of `chart`, with `helm show values`.
pub async fn image_set(chart: &Path) -> anyhow::Result<ImageSet> {
    let shown = helm(&["show".as_ref(), "values".as_ref(), chart.as_os_str()]).await?;
    let values: Value = serde_yaml::from_str(&shown).context("parse the chart's values")?;
    let text = |value: &Value, name: &str| -> anyhow::Result<String> {
        let text = value.as_str().unwrap_or_default();
        ensure!(
            !text.is_empty(),
            "the chart's default {name} is not stamped"
        );
        Ok(text.to_owned())
    };
    let image = &values["runtime-operator"]["runtime"]["image"];
    let registry = image["registry"].as_str().unwrap_or_default();
    let repository = text(&image["repository"], "host repository")?;
    let tag = text(&image["tag"], "host tag")?;
    let host = if registry.is_empty() {
        format!("{repository}:{tag}")
    } else {
        format!("{registry}/{repository}:{tag}")
    };
    Ok(ImageSet {
        host,
        http: text(&values["platform"]["roles"]["http"]["image"], "http image")?,
        materializer: text(
            &values["platform"]["roles"]["materializer"]["image"],
            "materializer image",
        )?,
    })
}

/// One role workload: `http` or `materializer`, with its configuration.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: RoleName,
    /// `localResources.config` of the role's component.
    pub config: Mapping,
    /// `localResources.environment.config`; the materializer only.
    pub environment: Option<Mapping>,
}

/// The roles a release can imply (R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleName {
    Http,
    Materializer,
}

impl RoleName {
    /// `http` or `materializer`, the suffix of the role's object names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Materializer => "materializer",
        }
    }
}

/// The roles a release implies (R4, epic decision D6): http iff the manifest
/// has an http attachment, materializer iff it has an event registration.
/// Each role's configuration derives from the coordinate.
pub fn roles(manifest: &ServingManifest, org: &str, project: &str, env: &str) -> Vec<Role> {
    let tenant = wamn_control_provision::project_env_tenant(org, project, env);
    let scope = |extra: &[(&str, &str)]| -> Mapping {
        [
            ("wamn.tenant", tenant.as_str()),
            ("wamn.project", project),
            ("wamn.environment", env),
        ]
        .iter()
        .chain(extra)
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
    };
    let mut roles = Vec::new();
    if manifest
        .attachments
        .values()
        .any(|attachment| attachment.type_ == AttachmentType::Http)
    {
        roles.push(Role {
            name: RoleName::Http,
            config: scope(&[]),
            environment: None,
        });
    }
    if !manifest.workflow.registrations.is_empty() {
        let stream = wamn_control_provision::event_stream_name(org, project, env);
        let environment = [
            ("WAMN_MAT_STREAM", stream.as_str()),
            ("WAMN_MAT_ORG", org),
            ("WAMN_MAT_PROJECT", project),
            ("WAMN_MAT_ENV", env),
            ("WAMN_MAT_TENANT", tenant.as_str()),
        ]
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect();
        roles.push(Role {
            name: RoleName::Materializer,
            config: scope(&[("wamn.postgres.authority", "event-materializer")]),
            environment: Some(environment),
        });
    }
    roles
}

/// The host variables that carry the environment coordinate and route host
/// (R1). The host derives its tenant from the coordinate and expects the route
/// host, because the manifest names neither. The operator chart sets
/// `WASMCLOUD_HOST_ENVIRONMENT` to the pod namespace first, and this later
/// entry replaces it, because the namespace is shared by every environment
/// (R6).
const HOST_VARIABLES: [&str; 4] = [
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WASMCLOUD_HOST_ENVIRONMENT",
    "WAMN_ROUTE_HOST",
];

/// The inputs of one environment's install values.
#[derive(Debug, Clone)]
pub struct ValuesInput {
    pub org: String,
    pub project: String,
    pub env: String,
    /// `sha256:<hex>` of the release manifest.
    pub manifest_digest: String,
    pub artifact_base: String,
    pub route_host: String,
    pub roles: Vec<Role>,
    /// The drain bound of the environment policy, rendered as the host pods'
    /// `terminationGracePeriodSeconds` (R20).
    pub drain_bound_seconds: u32,
    /// Who applied. It renders nothing and is kept with the revision (§9.2).
    pub actor: String,
    /// The body of the one host group entry: env, volumes, volumeMounts,
    /// replicas, http, resources, ociCaPaths. The name, namespace, service and
    /// release arguments are this module's and are refused here.
    pub host_group: Mapping,
}

/// Render the install values of one environment.
pub fn values(input: &ValuesInput) -> anyhow::Result<Value> {
    let name = release_name(&input.org, &input.project, &input.env)?;
    let label = release_label(&input.manifest_digest)
        .context("the release manifest digest is not a sha256 digest")?;
    for key in ["name", "namespace", "service", "extraArgs", "image"] {
        ensure!(
            !input.host_group.contains_key(key),
            "the host group body sets `{key}`, which the release chart derives"
        );
    }
    let environment = format!("{}/{}/{}", input.org, input.project, input.env);
    let mut group = input.host_group.clone();
    group.insert("name".into(), name.into());
    let variables = group
        .entry("env".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the host group body's `env` is not a list")?;
    ensure!(
        !variables.iter().any(|variable| {
            variable["name"]
                .as_str()
                .is_some_and(|name| HOST_VARIABLES.contains(&name))
        }),
        "the host group body sets a scope variable, which the release chart derives"
    );
    for (variable, value) in
        HOST_VARIABLES
            .into_iter()
            .zip([&input.org, &input.project, &input.env, &input.route_host])
    {
        variables.push(mapping([
            ("name", variable.to_owned()),
            ("value", value.clone()),
        ]));
    }
    group.insert(
        "extraArgs".into(),
        Value::Sequence(vec![
            format!("--release-artifact-base={}", input.artifact_base).into(),
            format!("--release-manifest-digest={}", input.manifest_digest).into(),
        ]),
    );
    let roles = input
        .roles
        .iter()
        .map(|role| {
            let mut entry = Mapping::new();
            entry.insert("name".into(), role.name.as_str().into());
            entry.insert("config".into(), Value::Mapping(role.config.clone()));
            if let Some(environment) = &role.environment {
                entry.insert("environment".into(), Value::Mapping(environment.clone()));
            }
            Value::Mapping(entry)
        })
        .collect();
    let mut runtime = Mapping::new();
    runtime.insert(
        "terminationGracePeriodSeconds".into(),
        input.drain_bound_seconds.into(),
    );
    runtime.insert("podLabels".into(), mapping([("wamn.release", label)]));
    runtime.insert(
        "podAnnotations".into(),
        mapping([
            ("wamn.release-digest", input.manifest_digest.clone()),
            ("wamn.environment", environment.clone()),
        ]),
    );
    runtime.insert(
        "hostGroups".into(),
        Value::Sequence(vec![Value::Mapping(group)]),
    );
    let mut operator = Mapping::new();
    operator.insert("runtime".into(), Value::Mapping(runtime));
    let mut values = Mapping::new();
    values.insert(
        "release".into(),
        mapping([
            ("manifestDigest", input.manifest_digest.clone()),
            ("artifactBase", input.artifact_base.clone()),
        ]),
    );
    values.insert("environment".into(), environment.into());
    values.insert("routeHost".into(), input.route_host.clone().into());
    values.insert("roles".into(), Value::Sequence(roles));
    values.insert("audit".into(), mapping([("actor", input.actor.clone())]));
    values.insert("runtime-operator".into(), Value::Mapping(operator));
    Ok(Value::Mapping(values))
}

/// Where Helm writes: an explicit kubeconfig, context and namespace.
#[derive(Debug, Clone)]
pub struct Target {
    pub kubeconfig: PathBuf,
    pub context: String,
    /// The operator release's namespace (R6).
    pub namespace: String,
}

/// Install or upgrade one environment's release at [`CHART_VERSION`] and wait
/// for its host Deployment. `description` is kept with the revision.
pub async fn upgrade(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Path,
    timeout: Duration,
    description: &str,
) -> anyhow::Result<()> {
    let timeout = format!("{}s", timeout.as_secs());
    helm(&[
        "upgrade".as_ref(),
        "--install".as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--version".as_ref(),
        CHART_VERSION.as_ref(),
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

/// Split `<repository>:<tag>@sha256:<hex>` into the repository and
/// `<tag>@sha256:<hex>`, the form the subchart's split image fields take.
fn split_host_image(image: &str) -> anyhow::Result<(String, String)> {
    let (name, digest) = image
        .rsplit_once('@')
        .with_context(|| format!("the host image {image} is not pinned by digest"))?;
    ensure!(
        digest.starts_with("sha256:"),
        "the host image {image} is not pinned by a sha256 digest"
    );
    let (repository, tag) = name
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .with_context(|| format!("the host image {image} has no tag"))?;
    Ok((repository.to_owned(), format!("{tag}@{digest}")))
}

fn mapping<const N: usize>(entries: [(&str, String); N]) -> Value {
    Value::Mapping(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
    )
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
    use super::*;

    const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn input() -> ValuesInput {
        let mut group = Mapping::new();
        group.insert("replicas".into(), 3.into());
        ValuesInput {
            org: "acme".into(),
            project: "wms".into(),
            env: "prod".into(),
            manifest_digest: DIGEST.into(),
            artifact_base: "registry.example/releases".into(),
            route_host: "wms.acme.example".into(),
            roles: vec![Role {
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
    fn chart_version_is_the_chart_files() {
        let chart = include_str!("../../../../deploy/platform/release/Chart.yaml");
        let chart: Value = serde_yaml::from_str(chart).expect("Chart.yaml parses");
        assert_eq!(chart["version"].as_str(), Some(CHART_VERSION));
    }

    #[test]
    fn release_name_is_bounded_stable_and_cut() {
        let name = release_name("acme", "wms", "prod").expect("valid coordinate");
        assert_eq!(
            name,
            release_name("acme", "wms", "prod").expect("valid coordinate")
        );
        assert!(name.starts_with("r-prod-") && name.len() == "r-prod-".len() + HASH_LEN);
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
    fn values_pin_the_release_by_digest_and_carry_no_image() {
        let values = values(&input()).expect("values render");
        let name = release_name("acme", "wms", "prod").expect("valid coordinate");
        let label = release_label(DIGEST).expect("digest");
        assert!(values.get("releaseLabel").is_none());
        let runtime = &values["runtime-operator"]["runtime"];
        assert_eq!(runtime["terminationGracePeriodSeconds"].as_u64(), Some(300));
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
            group["env"],
            serde_yaml::from_str::<Value>(
                "[{name: WAMN_ORG, value: acme}, {name: WAMN_PROJECT, value: wms}, \
                 {name: WASMCLOUD_HOST_ENVIRONMENT, value: prod}, \
                 {name: WAMN_ROUTE_HOST, value: wms.acme.example}]"
            )
            .expect("yaml")
        );
        assert_eq!(
            group["extraArgs"],
            serde_yaml::from_str::<Value>(&format!(
                "[--release-artifact-base=registry.example/releases, --release-manifest-digest={DIGEST}]"
            ))
            .expect("yaml")
        );
        assert!(runtime.get("image").is_none());
        assert_eq!(values["audit"]["actor"].as_str(), Some("ops"));
        assert!(values.get("platform").is_none());
        let text = serde_yaml::to_string(&values).expect("yaml");
        assert_eq!(
            text.matches("sha256:").count(),
            3,
            "the digest appears only as the release"
        );
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
        let mut input = super::tests::input();
        input.host_group.insert(
            "env".into(),
            serde_yaml::from_str("[{name: WAMN_ORG, value: other}]").expect("yaml"),
        );
        assert!(values(&input).is_err(), "a scope variable is refused");
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

//! Published image coordinates required by platform installation.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::Value;

use super::document::{ClusterDocument, UpgradeTarget};

/// A prepared platform chart validated before provider setup.
#[derive(Debug)]
pub struct SelectedChart {
    directory: PathBuf,
    reference: String,
    download: Option<tempfile::TempDir>,
    /// The version declared by the selected release chart.
    pub version: String,
    values: Value,
    host: String,
}

impl SelectedChart {
    pub fn is_downloaded(&self) -> bool {
        self.download.is_some()
    }

    /// Borrow the parsed chart defaults.
    pub fn values(&self) -> &Value {
        &self.values
    }

    /// Borrow the immutable chart reference.
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Borrow the canonical chart directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Borrow the host, a top-level platform image, or a role image by name.
    ///
    /// # Errors
    /// Returns an error if the named image is absent or unselected.
    pub fn image(&self, name: &str) -> anyhow::Result<&str> {
        if name == "host" {
            return Ok(&self.host);
        }
        let platform = &self.values["platform"];
        platform
            .get(name)
            .or_else(|| platform["roles"].get(name))
            .and_then(|entry| entry["image"].as_str())
            .filter(|image| !image.is_empty())
            .with_context(|| {
                format!(
                    "selected chart {} has no {name} image",
                    self.directory.display()
                )
            })
    }

    /// Prepare the selection in its owning release's values without applying resources.
    /// Service owners consume their selected chart on reapply. Host owners preserve
    /// the group image override instead of replacing the application chart or components.
    ///
    /// # Errors
    /// Returns an error for malformed values or an absent or ambiguous host group.
    pub fn selection_values(
        &self,
        document: &ClusterDocument,
        target: &UpgradeTarget,
        mut installed: Value,
    ) -> anyhow::Result<Value> {
        ensure!(
            installed.is_object(),
            "installed Helm values must be an object"
        );
        match target {
            UpgradeTarget::Control | UpgradeTarget::Identity => {
                let platform = installed
                    .as_object_mut()
                    .expect("validated object")
                    .entry("platform")
                    .or_insert_with(|| serde_json::json!({}));
                ensure!(
                    platform.is_object(),
                    "installed platform values must be an object"
                );
                platform["chart"] = serde_json::to_value(self.reference())?;
            }
            UpgradeTarget::Host { release, group } => {
                ensure!(
                    release != &wamn_registry::name::control_resource(&document.org),
                    "select the control service with --only control"
                );
                let groups = installed
                    .pointer_mut("/runtime-operator/runtime/hostGroups")
                    .and_then(Value::as_array_mut)
                    .context("installed release has no host groups")?;
                let mut matching = groups.iter_mut().filter(|entry| entry["name"] == *group);
                let selected = matching.next().context("selected host group is absent")?;
                ensure!(
                    matching.next().is_none(),
                    "selected host group is ambiguous"
                );
                ensure!(
                    selected
                        .get("namespace")
                        .is_none_or(|namespace| namespace == "hosts"),
                    "selected host group must be in hosts"
                );
                let reference: oci_client::Reference = self.host.parse()?;
                let mut image = self.values["runtime-operator"]["runtime"]["image"].clone();
                // Helm falls back to the old chart's registry for an empty group registry.
                image["registry"] = reference.registry().into();
                image["repository"] = reference.repository().into();
                selected["image"] = image;
            }
        }
        Ok(installed)
    }
}

/// Select a complete prepared chart using filesystem reads only.
///
/// # Errors
/// Returns an error for missing payload files, invalid metadata, or unpinned images.
pub fn select(chart: &Path) -> anyhow::Result<SelectedChart> {
    let reference = chart.to_str().context("chart reference must be UTF-8")?;
    let download = if reference.starts_with("oci://") {
        let (_, digest) = reference
            .rsplit_once('@')
            .context("chart requires an immutable digest")?;
        wamn_package_artifact::ensure_digest(digest)?;
        let directory = tempfile::tempdir()?;
        let authority = reference
            .trim_start_matches("oci://")
            .split('/')
            .next()
            .unwrap_or_default();
        let loopback = authority.starts_with("localhost:") || authority.starts_with("127.0.0.1:");
        let pull_reference = if loopback && std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
            format!(
                "oci://{}/{}",
                super::resources::POD_REGISTRY,
                reference
                    .trim_start_matches("oci://")
                    .split_once('/')
                    .context("chart repository")?
                    .1
            )
        } else {
            reference.to_owned()
        };
        let mut command = std::process::Command::new("helm");
        command
            .args(["pull", &pull_reference, "--untar", "--untardir"])
            .arg(directory.path());
        if loopback || authority == super::resources::POD_REGISTRY {
            command.arg("--plain-http");
        }
        if let Some(configuration) = std::env::var_os("WAMN_REGISTRY_AUTH_FILE") {
            command.arg("--registry-config").arg(configuration);
        }
        let output = command.output().context("fetch the selected chart")?;
        ensure!(
            output.status.success(),
            "fetch chart: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(directory)
    } else {
        None
    };
    let directory = if let Some(download) = &download {
        let mut entries = std::fs::read_dir(download.path())?;
        let entry = entries.next().context("chart archive is empty")??;
        ensure!(
            entries.next().is_none(),
            "chart archive must contain one chart"
        );
        entry.path()
    } else {
        chart
            .canonicalize()
            .with_context(|| format!("resolve selected chart {}", chart.display()))?
    };
    let reference = if download.is_some() {
        reference.to_owned()
    } else {
        directory.to_string_lossy().into_owned()
    };
    let metadata = read_yaml(&directory.join("Chart.yaml"))?;
    let version = metadata["version"]
        .as_str()
        .filter(|version| !version.trim().is_empty())
        .context("selected chart has no version")?
        .to_owned();
    for name in [
        "infra",
        "installation",
        "identity",
        "edge",
        "org",
        "environment",
    ] {
        require_file(&directory.join("platform").join(name).join("Chart.yaml"))?;
    }
    let values = read_yaml(&directory.join("values.yaml"))?;
    let image = &values["runtime-operator"]["runtime"]["image"];
    let repository = image["repository"]
        .as_str()
        .context("host has no repository")?;
    let tag = image["tag"].as_str().context("host has no tag")?;
    let registry = match image.get("registry") {
        None => "",
        Some(registry) => registry
            .as_str()
            .context("host registry must be a string")?,
    };
    let host = if registry.is_empty() {
        format!("{repository}:{tag}")
    } else {
        format!("{registry}/{repository}:{tag}")
    };
    validate_image(&host).context("invalid selected host image")?;
    validate_platform_images(&values["platform"]).context("invalid selected platform image")?;
    for name in ["ctl", "control", "identity", "edge", "roles/http"] {
        ensure!(
            values
                .pointer(&format!("/platform/{name}/image"))
                .and_then(Value::as_str)
                .is_some_and(|image| !image.is_empty()),
            "selected chart {} has no {name} image",
            directory.display()
        );
    }
    Ok(SelectedChart {
        directory,
        reference,
        download,
        version,
        values,
        host,
    })
}

pub(super) fn platform_chart(
    operator: Option<&Value>,
    supplied: Option<&Path>,
) -> anyhow::Result<SelectedChart> {
    let Some(operator) = operator else {
        return select(supplied.context(
            "--chart is required for a fresh installation without release platform/wamn. No resources changed.",
        )?);
    };
    let path = operator["platform"]["chart"]
        .as_str()
        .filter(|path| !path.is_empty())
        .context("platform/wamn has no platform.chart default. No resources changed.")?;
    select(Path::new(path)).with_context(|| {
        format!(
            "recorded chart {path} for owner release platform/wamn is unavailable or invalid. Restore the prepared chart at that path. No resources changed."
        )
    })
}

pub(super) fn resolve_chart(
    document: &ClusterDocument,
    selection: &UpgradeTarget,
    operator: Option<&Value>,
    control: Option<&Value>,
    identity: Option<&Value>,
    supplied: &Path,
) -> anyhow::Result<SelectedChart> {
    if operator.is_none() {
        return select(supplied);
    }
    let (installed, only) = match selection {
        UpgradeTarget::Control => (control, "control"),
        UpgradeTarget::Identity => (identity, "identity"),
        UpgradeTarget::Host { .. } => return select(supplied),
    };
    let path = [installed, operator]
        .into_iter()
        .flatten()
        .filter_map(|values| values["platform"]["chart"].as_str())
        .find(|chart| !chart.is_empty());
    let (name, namespace) = selection.owner(document);
    let path = path.with_context(|| {
        format!(
            "cannot resolve owner release {namespace}/{name}: platform/wamn has no platform.chart default. No resources changed."
        )
    })?;
    select(Path::new(path)).with_context(|| {
        format!(
            "recorded chart {path} for owner release {namespace}/{name} is unavailable or invalid. Restore the prepared chart at that path, or select a new one explicitly with --only {only}. No resources changed."
        )
    })
}

fn read_yaml(file: &Path) -> anyhow::Result<Value> {
    serde_yaml::from_slice(
        &std::fs::read(file).with_context(|| format!("read {}", file.display()))?,
    )
    .with_context(|| format!("decode {}", file.display()))
}

fn require_file(file: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(file)
        .with_context(|| format!("read prepared payload {}", file.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "missing or empty prepared payload {}",
        file.display()
    );
    Ok(())
}

fn validate_image(image: &str) -> anyhow::Result<()> {
    let reference: oci_client::Reference = image
        .parse()
        .with_context(|| format!("invalid OCI image reference {image}"))?;
    let digest = reference
        .digest()
        .and_then(|digest| digest.strip_prefix("sha256:"));
    ensure!(
        digest.is_some_and(|digest| digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "image {image} requires a SHA-256 digest of 64 lowercase hexadecimal digits"
    );
    Ok(())
}

fn validate_platform_images(value: &Value) -> anyhow::Result<()> {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if name == "image" {
                    if !value.is_null() && value != "" {
                        validate_image(value.as_str().context("image must be a string")?)?;
                    }
                } else {
                    validate_platform_images(value)?;
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                validate_platform_images(entry)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Package all charts beside the stamped release. Workers fetch this complete archive.
pub fn prepare_payload(repository: &Path, chart: &Path) -> anyhow::Result<()> {
    let platform = chart.join("platform");
    std::fs::create_dir_all(&platform)?;
    for (name, source) in [
        ("infra", "deploy/infra/chart"),
        ("installation", "deploy/platform/installation"),
        ("identity", "deploy/platform/identity"),
        ("edge", "deploy/platform/edge"),
        ("org", "deploy/platform/org"),
        ("environment", "deploy/platform/environment"),
    ] {
        copy_package_output(&repository.join(source), &platform.join(name))?;
    }
    for source in ["identity", "edge", "release"] {
        let output = std::process::Command::new("helm")
            .arg("package")
            .arg(repository.join("deploy/platform").join(source))
            .arg("--destination")
            .arg(platform.join("installation/charts"))
            .output()
            .context("package platform subchart")?;
        ensure!(
            output.status.success(),
            "package platform subchart: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub fn copy_package_output(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_package_output(&entry.path(), &destination)?;
        } else {
            ensure!(
                entry.file_type()?.is_file(),
                "package output must contain regular files"
            );
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use super::test_util::Chart;

    fn prepared_chart() -> Chart {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        super::test_util::prepared_chart(&repository).unwrap()
    }

    fn write_values(chart: &Chart, values: &Value) {
        std::fs::write(
            chart.0.join("values.yaml"),
            serde_yaml::to_string(values).unwrap(),
        )
        .unwrap();
    }

    fn image_pointers(value: &Value, path: &str, pointers: &mut Vec<String>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    let path = format!("{path}/{key}");
                    if key == "image" && value.as_str().is_some_and(|image| !image.is_empty()) {
                        pointers.push(path);
                    } else {
                        image_pointers(value, &path, pointers);
                    }
                }
            }
            Value::Array(entries) => {
                for (index, value) in entries.iter().enumerate() {
                    image_pointers(value, &format!("{path}/{index}"), pointers);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn service_chart_resolution_prefers_own_then_default_then_fresh_supplied() -> anyhow::Result<()>
    {
        let supplied = prepared_chart();
        let default = prepared_chart();
        let own = prepared_chart();
        let operator = json!({"platform":{"chart":default.0}});
        let unavailable_default = json!({"platform":{"chart":default.0.join("missing")}});
        let recorded = json!({"platform":{"chart":own.0}});
        let legacy = json!({});
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
        for selection in [UpgradeTarget::Control, UpgradeTarget::Identity] {
            for (operator, installed, expected) in [
                (None, None, &supplied.0),
                (Some(&operator), None, &default.0),
                (Some(&operator), Some(&legacy), &default.0),
                (Some(&operator), Some(&recorded), &own.0),
                (Some(&unavailable_default), Some(&recorded), &own.0),
                (None, Some(&recorded), &supplied.0),
            ] {
                let (control, identity) = match selection {
                    UpgradeTarget::Control => (installed, Some(&recorded)),
                    UpgradeTarget::Identity => (Some(&recorded), installed),
                    UpgradeTarget::Host { .. } => unreachable!(),
                };
                let selected = resolve_chart(
                    &document,
                    &selection,
                    operator,
                    control,
                    identity,
                    &supplied.0,
                )?;
                assert_eq!(selected.directory(), expected.canonicalize()?);
            }
        }
        Ok(())
    }

    #[test]
    fn selection_accepts_independent_registries_and_tags() {
        let chart = prepared_chart();
        let selected = select(&chart.0.join("platform/..")).unwrap();
        assert_eq!(selected.directory(), chart.0.canonicalize().unwrap());
        assert_eq!(
            selected.version,
            read_yaml(&chart.0.join("Chart.yaml")).unwrap()["version"]
        );
        assert_eq!(
            selected.image("host").unwrap(),
            format!("localhost:5001/host:host-build@sha256:{}", "a".repeat(64))
        );
        for (name, entry) in selected.values()["platform"].as_object().unwrap() {
            if let Some(image) = entry["image"].as_str().filter(|image| !image.is_empty()) {
                assert_eq!(selected.image(name).unwrap(), image);
            }
        }
        for (name, entry) in selected.values()["platform"]["roles"].as_object().unwrap() {
            assert_eq!(selected.image(name).unwrap(), entry["image"]);
        }
        assert!(selected.image("runner").is_err());
        assert!(selected.image("unknown").is_err());
    }

    #[test]
    fn selection_requires_each_selected_core_image() {
        let chart = prepared_chart();
        let mut values = select(&chart.0).unwrap().values;
        let mut pointers = vec!["/runtime-operator/runtime/image/tag".to_owned()];
        image_pointers(&values["platform"], "/platform", &mut pointers);
        for pointer in pointers {
            let original = values.pointer_mut(&pointer).unwrap().take();
            for missing in [Value::Null, json!("")] {
                *values.pointer_mut(&pointer).unwrap() = missing;
                write_values(&chart, &values);
                assert!(select(&chart.0).is_err(), "{pointer}");
            }
            *values.pointer_mut(&pointer).unwrap() = original;
        }
    }

    #[test]
    fn selection_validates_all_selected_images_recursively() {
        let chart = prepared_chart();
        let mut values = select(&chart.0).unwrap().values;
        let digest = "a".repeat(64);
        values["platform"]["extra"] =
            json!({"nested": [{"image": format!("extra.example/optional@sha256:{digest}")}]});
        let mut pointers = vec!["/runtime-operator/runtime/image/tag".to_owned()];
        image_pointers(&values["platform"], "/platform", &mut pointers);
        write_values(&chart, &values);
        assert!(select(&chart.0).is_ok());
        for pointer in pointers {
            let original = values.pointer_mut(&pointer).unwrap().take();
            let image = original.as_str().unwrap();
            let (reference, _) = image.split_once('@').unwrap();
            for invalid in [
                json!(reference),
                json!(format!("{reference}@sha256:{}", "a".repeat(63))),
                json!(format!("{reference}@sha256:{}", "a".repeat(65))),
                json!(format!("{reference}@sha256:{}", "A".repeat(64))),
                json!(format!("{reference}@sha256:{}", "g".repeat(64))),
                json!(format!("{reference}@sha512:{}", "a".repeat(128))),
                json!(format!("{image}@sha256:{digest}")),
                json!(format!("bad space@sha256:{digest}")),
                json!(42),
            ] {
                *values.pointer_mut(&pointer).unwrap() = invalid;
                write_values(&chart, &values);
                assert!(select(&chart.0).is_err(), "{pointer}");
            }
            *values.pointer_mut(&pointer).unwrap() = original;
        }
    }
}

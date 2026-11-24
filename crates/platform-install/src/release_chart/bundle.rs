//! Select and assemble the published chart bundle.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, ensure};
use serde_json::Value;

use super::{POD_REGISTRY, pod_registry_reference};
use crate::platform_registry::require_image_digest;

/// A prepared platform chart validated before provider setup.
#[derive(Debug)]
pub struct SelectedChart {
    directory: PathBuf,
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
}

/// Read a local chart or pull a complete prepared chart with Helm.
///
/// # Errors
/// Returns an error for missing payload files, invalid metadata, or unpinned images.
pub fn select(chart: &Path) -> anyhow::Result<SelectedChart> {
    let reference = chart.to_str().context("chart reference must be UTF-8")?;
    let download = if let Some(oci_reference) = reference.strip_prefix("oci://") {
        let (_, digest) = reference
            .rsplit_once('@')
            .context("chart requires an immutable digest")?;
        digest.parse::<wamn_digest::Digest>()?;
        let directory = tempfile::tempdir()?;
        let (authority, _) = oci_reference.split_once('/').context("chart repository")?;
        let loopback = wamn_registry::registry_is_loopback(authority);
        let pull_reference = if loopback && std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
            pod_registry_reference(reference)
        } else {
            Cow::Borrowed(reference)
        };
        let plain_http = loopback || authority == POD_REGISTRY;
        wamn_kubernetes::helm::pull_chart(&pull_reference, directory.path(), plain_http)?;
        Some(directory)
    } else {
        None
    };
    let directory = if let Some(download) = &download {
        let entries = std::fs::read_dir(download.path()).context("read pulled chart archive")?;
        let mut chart_directory = None;
        for entry in entries {
            let path = entry.context("read pulled chart archive entry")?.path();
            if path.join("Chart.yaml").is_file() {
                ensure!(
                    chart_directory.is_none(),
                    "pulled archive holds several charts"
                );
                chart_directory = Some(path);
            }
        }
        chart_directory.context("pulled archive holds no chart")?
    } else {
        chart
            .canonicalize()
            .with_context(|| format!("resolve selected chart {}", chart.display()))?
    };
    let metadata = read_yaml(&directory.join("Chart.yaml"))?;
    let version = metadata["version"]
        .as_str()
        .filter(|version| !version.trim().is_empty())
        .context("selected chart has no version")?
        .to_owned();
    for name in [
        "infra",
        "platform",
        "org",
        "environment",
        "project",
        "application",
    ] {
        require_file(&directory.join("platform").join(name).join("Chart.yaml"))?;
    }
    let values = read_yaml(&directory.join("values.yaml"))?;
    let host = values["platform"]["host"]["image"]
        .as_str()
        .context("host has no image")?
        .to_owned();
    require_image_digest(&host).context("invalid selected host image")?;
    validate_platform_images(&values["platform"]).context("invalid selected platform image")?;
    for name in [
        "ctl",
        "control",
        "identity",
        "edge",
        "roles/ingress",
        "workflow",
    ] {
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
        download,
        version,
        values,
        host,
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

fn validate_platform_images(value: &Value) -> anyhow::Result<()> {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if name == "image" {
                    if !value.is_null() && value != "" {
                        require_image_digest(value.as_str().context("image must be a string")?)?;
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
    // Helm stores every chart file in the release record, including unrendered assets.
    // Install the stamped application chart from this leaf to keep the bundle out of that record.
    let application = platform.join("application");
    std::fs::create_dir(&application)?;
    for name in ["Chart.yaml", "values.yaml"] {
        std::fs::copy(chart.join(name), application.join(name))?;
    }
    copy_package_output(&chart.join("templates"), &application.join("templates"))?;
    for (name, source) in [
        ("infra", "deploy/charts/infrastructure"),
        ("platform", "deploy/charts/platform"),
        ("org", "deploy/charts/org"),
        ("environment", "deploy/charts/environment"),
        ("project", "deploy/charts/project"),
    ] {
        copy_package_output(&repository.join(source), &platform.join(name))?;
    }
    Ok(())
}

fn copy_package_output(from: &Path, to: &Path) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::release_chart::test_util::{Chart, prepared_chart as prepare_fixture};

    fn prepared_chart() -> Chart {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        prepare_fixture(&repository).unwrap()
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
        let mut pointers = Vec::new();
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
        let mut pointers = Vec::new();
        image_pointers(&values["platform"], "/platform", &mut pointers);
        write_values(&chart, &values);
        assert!(select(&chart.0).is_ok());
        for pointer in pointers {
            let original = values.pointer_mut(&pointer).unwrap().take();
            let image = original.as_str().unwrap();
            let (reference, _) = image.split_once('@').unwrap();
            for invalid in [
                json!(reference),
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

//! Published image coordinates required by platform installation.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::Value;

// The portable payload emitted by platform_build (DEP-02).
const PAYLOAD_FILES: &[&str] = &[
    "platform/resources/registry.yaml",
    "platform/identity/Chart.yaml",
    "platform/identity/values.yaml",
    "platform/identity/templates/deployment.yaml",
    "platform/identity/templates/service.yaml",
    "platform/resources/cert-manager.yaml",
    "platform/resources/cnpg-operator.yaml",
    "platform/resources/cnpg-cluster.yaml",
    "platform/resources/nats-jetstream.yaml",
    "platform/resources/nats-certificates.yaml",
    "platform/resources/values-wamn.yaml",
    "platform/resources/values-operator.yaml",
    "platform/resources/identity-certificate.yaml",
    "platform/resources/identity-operator-ca.yaml",
    "platform/resources/wamn-host-serviceaccount.yaml",
    "platform/resources/registry-helper.yaml",
    "platform/resources/runtime-operator-events-rbac.example.yaml",
    "platform/resources/control-status-rbac.yaml",
    "platform/resources/control-service.yaml",
];

/// A prepared platform chart validated before provider setup.
#[derive(Debug)]
pub struct SelectedChart {
    directory: PathBuf,
    /// The version declared by the selected release chart.
    pub version: String,
    /// The packaged runtime operator selected by the chart's dependency metadata.
    pub operator_archive: PathBuf,
    values: Value,
    host: String,
}

impl SelectedChart {
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

/// Select a complete prepared chart using filesystem reads only.
///
/// # Errors
/// Returns an error for missing payload files, invalid metadata, or unpinned images.
pub fn select(chart: &Path) -> anyhow::Result<SelectedChart> {
    let directory = chart
        .canonicalize()
        .with_context(|| format!("resolve selected chart {}", chart.display()))?;
    let metadata = read_yaml(&directory.join("Chart.yaml"))?;
    let version = metadata["version"]
        .as_str()
        .filter(|version| !version.trim().is_empty())
        .context("selected chart has no version")?
        .to_owned();
    let dependency = metadata["dependencies"]
        .as_array()
        .and_then(|dependencies| {
            dependencies
                .iter()
                .find(|entry| entry["name"] == "runtime-operator")
        })
        .context("selected chart has no runtime-operator dependency")?;
    let operator_version = dependency["version"]
        .as_str()
        .filter(|version| !version.is_empty() && !version.contains(['/', '\\']))
        .context("runtime-operator dependency has no valid version")?;
    let operator_archive = directory
        .join("charts")
        .join(format!("runtime-operator-{operator_version}.tgz"));
    require_file(&operator_archive)?;
    for file in PAYLOAD_FILES {
        require_file(&directory.join(file))?;
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
    for name in ["ctl", "identity", "roles/http", "roles/materializer"] {
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
        version,
        operator_archive,
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

pub(super) fn runner_image(directory: &Path) -> anyhow::Result<String> {
    let path = directory.join("release-chart/values.yaml");
    let values: Value = serde_yaml::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?;
    values["platform"]["runner"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .map(str::to_owned)
        .context("the stamped release chart has no pinned runner image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Chart(PathBuf);

    impl Drop for Chart {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn prepared_chart() -> Chart {
        use wamn_release_control::release_chart::{self, ImageSet};

        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let chart =
            Chart(std::env::temp_dir().join(format!("wamn-chart-{}", uuid::Uuid::new_v4())));
        let digest = "a".repeat(64);
        release_chart::stamp(
            &repository.join("deploy/platform/release"),
            &ImageSet {
                host: format!("localhost:5001/host:host-build@sha256:{digest}"),
                http: format!("registry.example/components/http@sha256:{digest}"),
                materializer: format!("other.example/materializer:component-build@sha256:{digest}"),
            },
            None,
            &chart.0,
        )
        .unwrap();
        for file in PAYLOAD_FILES {
            let target = chart.0.join(file);
            let source = if let Some(identity) = file.strip_prefix("platform/identity/") {
                repository.join("deploy/platform/identity").join(identity)
            } else {
                let basename = Path::new(file).file_name().unwrap();
                ["deploy/infra", "deploy/platform", "deploy/kind"]
                    .into_iter()
                    .map(|root| repository.join(root).join(basename))
                    .find(|path| path.is_file())
                    .unwrap()
            };
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(source, target).unwrap();
        }
        let mut values = read_yaml(&chart.0.join("values.yaml")).unwrap();
        values["platform"]["ctl"] =
            json!({"image": format!("ctl.example/wamn-ctl:ctl-build@sha256:{digest}")});
        values["platform"]["identity"]["image"] =
            json!(format!("identity.example/identity@sha256:{digest}"));
        write_values(&chart, &values);
        chart
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
    fn selection_accepts_independent_registries_tags_and_packaged_operator_version() {
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
        let mut metadata = read_yaml(&chart.0.join("Chart.yaml")).unwrap();
        let operator = metadata["dependencies"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["name"] == "runtime-operator")
            .unwrap();
        operator["version"] = json!("9.8.7");
        let archive = chart.0.join("charts/runtime-operator-9.8.7.tgz");
        std::fs::rename(&selected.operator_archive, &archive).unwrap();
        std::fs::write(
            chart.0.join("Chart.yaml"),
            serde_yaml::to_string(&metadata).unwrap(),
        )
        .unwrap();
        assert_eq!(select(&chart.0).unwrap().operator_archive, archive);
        metadata["dependencies"] = json!([]);
        std::fs::write(
            chart.0.join("Chart.yaml"),
            serde_yaml::to_string(&metadata).unwrap(),
        )
        .unwrap();
        assert!(select(&chart.0).is_err());
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

    #[test]
    fn selection_requires_the_packaged_payload() {
        let chart = prepared_chart();
        let selected = select(&chart.0).unwrap();
        let mut files = vec![
            selected.operator_archive,
            chart.0.join("Chart.yaml"),
            chart.0.join("values.yaml"),
        ];
        let mut directories = vec![chart.0.join("platform")];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        for file in files {
            let bytes = std::fs::read(&file).unwrap();
            std::fs::remove_file(&file).unwrap();
            assert!(select(&chart.0).is_err(), "{}", file.display());
            std::fs::write(&file, []).unwrap();
            assert!(select(&chart.0).is_err(), "{}", file.display());
            std::fs::write(file, bytes).unwrap();
        }
    }
}

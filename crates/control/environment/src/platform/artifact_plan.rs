//! Published image coordinates required by platform installation.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::Value;

use super::document::{ClusterDocument, UpgradeTarget};

// The portable payload emitted by platform_build (DEP-02).
const PAYLOAD_FILES: &[&str] = &[
    "platform/resources/registry.yaml",
    "platform/resources/letsencrypt.yaml",
    "platform/edge/Chart.yaml",
    "platform/edge/values.yaml",
    "platform/edge/templates/configmap.yaml",
    "platform/edge/templates/deployment.yaml",
    "platform/edge/templates/service.yaml",
    "platform/edge/templates/rbac.yaml",
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
                    .expect("checked object")
                    .entry("platform")
                    .or_insert_with(|| serde_json::json!({}));
                ensure!(
                    platform.is_object(),
                    "installed platform values must be an object"
                );
                platform["chart"] = serde_json::to_value(self.directory())?;
            }
            UpgradeTarget::Host { release, group } => {
                ensure!(
                    release != &format!("c-{}", document.org),
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
    for name in [
        "ctl",
        "identity",
        "edge",
        "roles/http",
        "roles/materializer",
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
        version,
        operator_archive,
        values,
        host,
    })
}

pub(super) fn platform_chart(
    operator: Option<&Value>,
    supplied: &Path,
) -> anyhow::Result<SelectedChart> {
    let Some(operator) = operator else {
        return select(supplied);
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
    fn unavailable_recorded_service_charts_require_restoration_without_substitution()
    -> anyhow::Result<()> {
        let supplied = prepared_chart();
        let invalid = prepared_chart();
        std::fs::write(invalid.0.join("Chart.yaml"), "version: ''")?;
        let missing = invalid.0.join("missing");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
        for (selection, only) in [
            (UpgradeTarget::Control, "control"),
            (UpgradeTarget::Identity, "identity"),
        ] {
            for path in [&missing, &invalid.0] {
                let recorded = json!({"platform":{"chart":path}});
                let valid = json!({"platform":{"chart":supplied.0}});
                let legacy = json!({});
                for (operator, installed) in [(&valid, &recorded), (&recorded, &legacy)] {
                    let error = resolve_chart(
                        &document,
                        &selection,
                        Some(operator),
                        Some(installed),
                        Some(installed),
                        &supplied.0,
                    )
                    .unwrap_err()
                    .to_string();
                    let (name, namespace) = selection.owner(&document);
                    assert!(
                        error.contains(&format!("owner release {namespace}/{name}")),
                        "{error}"
                    );
                    assert!(error.contains(path.to_str().unwrap()), "{error}");
                    assert!(error.contains(&format!(
                        "Restore the prepared chart at that path, or select a new one explicitly with --only {only}"
                    )), "{error}");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn service_selection_records_its_chart_in_the_existing_owner() -> anyhow::Result<()> {
        use super::super::{control, identity, resources};

        let chart = prepared_chart();
        let selected = select(&chart.0)?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let document = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
        let identity = identity::platform_chart_input(
            &selected.directory().join("platform/identity"),
            &document,
            selected.image("identity")?,
        )?;
        for (target, mut installed) in [
            (
                UpgradeTarget::Control,
                control::release_values(&document, selected.values(), false)?,
            ),
            (UpgradeTarget::Identity, identity["values"].clone()),
        ] {
            installed["platform"] = json!({"chart":"/previous/chart", "retained":"configuration"});
            let mut values = selected.selection_values(&document, &target, installed.clone())?;
            let (name, namespace) = target.owner(&document);
            let entry = match target {
                UpgradeTarget::Control => json!({
                    "chart":selected.directory(), "version":selected.version,
                    "name":name, "namespace":namespace,
                    "values":control::release_values(&document, selected.values(), false)?,
                }),
                UpgradeTarget::Identity => identity::platform_chart_input(
                    &selected.directory().join("platform/identity"),
                    &document,
                    selected.image("identity")?,
                )?,
                UpgradeTarget::Host { .. } => unreachable!(),
            };
            let objects = resources::helm_template(&entry)?;
            let deployment = objects
                .iter()
                .find(|object| object["kind"] == "Deployment")
                .unwrap();
            assert_eq!(deployment["metadata"]["namespace"], namespace);
            let image = &deployment["spec"]["template"]["spec"]["containers"][0]["image"];
            assert_eq!(
                image,
                selected.image(if target == UpgradeTarget::Control {
                    "host"
                } else {
                    "identity"
                })?
            );
            values["platform"]["chart"] = installed["platform"]["chart"].clone();
            assert_eq!(values, installed);
        }
        assert!(
            selected
                .selection_values(&document, &UpgradeTarget::Control, json!([]))
                .is_err()
        );
        assert!(
            selected
                .selection_values(&document, &UpgradeTarget::Identity, json!({"platform":[]}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn host_selection_renders_only_the_named_group_without_reselecting_application_code()
    -> anyhow::Result<()> {
        use super::super::resources;
        use crate::environment::{document::EnvironmentDocument, ensure, write};

        let chart = prepared_chart();
        let mut defaults = read_yaml(&chart.0.join("values.yaml"))?;
        defaults["runtime-operator"]["runtime"]["image"]["registry"] = "base.example".into();
        defaults["runtime-operator"]["runtime"]["image"]["repository"] = "host".into();
        write_values(&chart, &defaults);
        let original = select(&chart.0)?;
        let upgrade = prepared_chart();
        let mut defaults = read_yaml(&upgrade.0.join("values.yaml"))?;
        defaults["runtime-operator"]["runtime"]["image"] = json!({
            "registry":"", "repository":"upgrade.example/host",
            "tag":format!("upgrade@sha256:{}", "b".repeat(64)), "pull_policy":"IfNotPresent",
        });
        write_values(&upgrade, &defaults);
        let selected = select(&upgrade.0)?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let cluster = ClusterDocument::compile(&repository.join("deploy/kind/cluster.k"))?;
        let digest = format!("sha256:{}", "c".repeat(64));
        let environment = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":cluster.org, "project":"fixture", "env":"dev", "target":"cloud", "policy":"dev",
            "applications":{
                "alpha":{"packages":["platform_fixture"],"current":digest,"hostname":"alpha.example"},
                "beta":{"packages":["platform_fixture_overlay"],"current":digest,"hostname":"beta.example"},
            },
        }))?)?;
        let manifest = serde_json::from_value(json!({
            "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release":{"packages":[]}, "components":[], "routes":[], "attachments":{},
        }))?;
        let overlay = chart.0.join("test-host.yaml");
        std::fs::write(&overlay, "replicas: 1\n")?;
        let mut owners = Vec::new();
        for application in environment.applications.keys() {
            let (values, _) = write::release_values(
                &environment,
                application,
                &manifest,
                &digest,
                "registry.example/releases",
                70,
                "render",
                Some(&overlay),
            )?;
            owners.push(serde_json::to_value(values)?);
        }
        owners.push(ensure::materializer_host_values(
            &environment,
            70,
            1,
            120,
            Some(&overlay),
            false,
        )?);
        for (index, installed) in owners.iter().enumerate() {
            let mut installed = installed.clone();
            let unrelated =
                &owners[(index + 1) % owners.len()]["runtime-operator"]["runtime"]["hostGroups"][0];
            installed["runtime-operator"]["runtime"]["hostGroups"]
                .as_array_mut()
                .unwrap()
                .push(unrelated.clone());
            let group = installed["runtime-operator"]["runtime"]["hostGroups"][0]["name"]
                .as_str()
                .unwrap();
            let target = UpgradeTarget::Host {
                release: group.into(),
                group: group.into(),
            };
            let mut values = selected.selection_values(&cluster, &target, installed.clone())?;
            let (name, namespace) = target.owner(&cluster);
            let entry = json!({"chart":original.directory(), "version":original.version,
                "name":name, "namespace":namespace, "values":installed});
            let before = resources::helm_template(&entry)?;
            let mut entry = entry;
            entry["values"] = values.clone();
            let mut after = resources::helm_template(&entry)?;
            let deployment = |object: &&Value| {
                object["kind"] == "Deployment"
                    && object["metadata"]["name"] == format!("hostgroup-{group}")
            };
            let previous = before
                .iter()
                .find(deployment)
                .context("original host deployment")?;
            let updated = after
                .iter_mut()
                .find(|object| deployment(&&**object))
                .context("selected host deployment")?;
            let image = &mut updated["spec"]["template"]["spec"]["containers"][0]["image"];
            assert_eq!(
                image,
                &json!(format!(
                    "upgrade.example/host:upgrade@sha256:{}",
                    "b".repeat(64)
                ))
            );
            *image = previous["spec"]["template"]["spec"]["containers"][0]["image"].clone();
            assert_eq!(after, before);
            values["runtime-operator"]["runtime"]["hostGroups"][0]
                .as_object_mut()
                .unwrap()
                .remove("image");
            assert_eq!(values, installed);
            let missing = UpgradeTarget::Host {
                release: group.into(),
                group: "missing".into(),
            };
            assert!(
                selected
                    .selection_values(&cluster, &missing, installed.clone())
                    .is_err()
            );
            let mut duplicate = installed.clone();
            let groups = duplicate["runtime-operator"]["runtime"]["hostGroups"]
                .as_array_mut()
                .unwrap();
            groups.push(groups[0].clone());
            assert!(
                selected
                    .selection_values(&cluster, &target, duplicate)
                    .is_err()
            );
            let control = UpgradeTarget::Host {
                release: format!("c-{}", cluster.org),
                group: group.into(),
            };
            assert!(
                selected
                    .selection_values(&cluster, &control, installed)
                    .is_err()
            );
        }
        Ok(())
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

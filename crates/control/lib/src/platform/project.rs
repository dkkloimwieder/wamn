//! The project document declares the GCP cluster and its node pools.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};

use super::document::{kcl_value, lower, name, token};

/// One project document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDocument {
    pub project_id: String,
    pub zone: String,
    pub cluster_name: String,
    pub release_channel: String,
    pub network: String,
    pub subnetwork: String,
    pub pod_secondary_range_name: String,
    pub service_secondary_range_name: String,
    pub node_service_account: String,
    pub node_pools: Vec<NodePool>,
    pub edge: Edge,
}

/// The Service-owned endpoint group used by the TCP proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub neg_name: String,
    pub neg_zone: String,
}

/// One fixed-size node pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePool {
    pub name: String,
    pub machine_type: String,
    pub spot: bool,
    pub disk_type: String,
    /// Disk size in GiB.
    pub disk_size: u32,
    /// The fixed node count, including zero.
    pub size: u32,
    /// One key=value label, or an empty string.
    #[serde(default)]
    pub label: String,
    /// One key=value:effect taint, or an empty string.
    #[serde(default)]
    pub taint: String,
}

impl ProjectDocument {
    /// Compile a `project.k` file and read it.
    ///
    /// # Errors
    ///
    /// When KCL refuses the file, or when the JSON is not a valid document.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let bytes = wamn_schema_generator::compile_schema_file(file, "project")?;
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid project", file.display()))
    }

    /// Read a document from its JSON form and check every field.
    ///
    /// # Errors
    ///
    /// When a field is unknown or missing, or a value is refused.
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        let document: Self = serde_json::from_slice(bytes).context("decode the project JSON")?;
        document.check()?;
        Ok(document)
    }

    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            self.edge.neg_name == "wamn-edge",
            "edge.neg_name must be wamn-edge"
        );
        let neg_zone: Vec<_> = self.edge.neg_zone.split('-').collect();
        ensure!(
            neg_zone.len() == 3
                && lower(neg_zone[0])
                && neg_zone[2].len() == 1
                && lower(neg_zone[2])
                && {
                    let region = neg_zone[1].trim_end_matches(|c: char| c.is_ascii_digit());
                    lower(region) && region.len() < neg_zone[1].len()
                },
            "edge.neg_zone is not a GCP zone"
        );
        ensure!(
            name(&self.cluster_name, 40),
            "cluster_name is not a lowercase name"
        );
        ensure!(
            matches!(
                self.release_channel.as_str(),
                "rapid" | "regular" | "stable" | "extended"
            ),
            "release_channel is not a named channel"
        );
        ensure!(
            self.project_id.len() >= 6 && name(&self.project_id, 30),
            "project_id is not a GCP project id"
        );
        let zone: Vec<_> = self.zone.split('-').collect();
        ensure!(
            zone.len() == 3 && lower(zone[0]) && zone[2].len() == 1 && lower(zone[2]) && {
                let region = zone[1].trim_end_matches(|c: char| c.is_ascii_digit());
                lower(region) && region.len() < zone[1].len()
            },
            "zone is not a GCP zone"
        );
        for (field, value) in [
            ("network", &self.network),
            ("subnetwork", &self.subnetwork),
            ("pod_secondary_range_name", &self.pod_secondary_range_name),
            (
                "service_secondary_range_name",
                &self.service_secondary_range_name,
            ),
        ] {
            ensure!(name(value, 63), "{field} is not a lowercase name");
        }
        ensure!(
            self.node_service_account
                .split_once('@')
                .is_some_and(|(account, domain)| {
                    name(account, usize::MAX)
                        && domain
                            .strip_suffix(".iam.gserviceaccount.com")
                            .is_some_and(|project| name(project, usize::MAX))
                }),
            "node_service_account is not a GCP service account email"
        );
        ensure!(!self.node_pools.is_empty(), "node_pools is empty");
        let mut pools = BTreeSet::new();
        for pool in &self.node_pools {
            ensure!(
                name(&pool.name, 63) && pools.insert(&pool.name),
                "node pool names are invalid or repeated"
            );
            ensure!(
                name(&pool.machine_type, usize::MAX),
                "node pool {} has an invalid machine_type",
                pool.name
            );
            ensure!(
                matches!(
                    pool.disk_type.as_str(),
                    "pd-standard" | "pd-balanced" | "pd-ssd"
                ),
                "node pool {} has an invalid disk_type",
                pool.name
            );
            ensure!(
                pool.disk_size > 0,
                "node pool {} has zero disk_size",
                pool.name
            );
            ensure!(
                pool.label.is_empty() || label(&pool.label),
                "node pool {} has an invalid label",
                pool.name
            );
            ensure!(
                pool.taint.is_empty()
                    || pool.taint.split_once(':').is_some_and(|(value, effect)| {
                        label(value)
                            && matches!(effect, "NoSchedule" | "PreferNoSchedule" | "NoExecute")
                    }),
                "node pool {} has an invalid taint",
                pool.name
            );
        }
        Ok(())
    }

    /// The canonical bytes: RFC 8785 JSON.
    ///
    /// # Errors
    ///
    /// Never for a valid document.
    pub fn canonical_json(&self) -> anyhow::Result<Vec<u8>> {
        let value = serde_json::to_value(self).context("encode the project")?;
        Ok(wamn_execution_contract::canonical_json_bytes(&value))
    }

    /// The document as a `project.k` file that compiles to this document.
    pub fn to_kcl(&self) -> String {
        let value = serde_json::to_value(self).expect("a project document encodes");
        format!("import project\n\nproject.Project {}\n", kcl_value(&value))
    }
}

fn label(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(key, value)| token(key, "_.-") && token(value, "_.-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn document() -> Value {
        json!({
            "project_id": "wamn-dev", "zone": "us-central1-a",
            "cluster_name": "wamn", "release_channel": "regular",
            "network": "wamn", "subnetwork": "wamn-us-central1",
            "pod_secondary_range_name": "pods", "service_secondary_range_name": "services",
            "node_service_account": "wamn-nodes@wamn-dev.iam.gserviceaccount.com",
            "edge": {"neg_name": "wamn-edge", "neg_zone": "us-central1-a"},
            "node_pools": [
                {"name": "main", "machine_type": "e2-standard-2", "spot": true, "disk_type": "pd-standard", "disk_size": 30, "size": 1, "label": "", "taint": ""},
                {"name": "bench", "machine_type": "e2-standard-2", "spot": true, "disk_type": "pd-standard", "disk_size": 30, "size": 1, "label": "role=bench-host", "taint": "role=bench-host:NoSchedule"},
                {"name": "pg", "machine_type": "e2-standard-2", "spot": false, "disk_type": "pd-standard", "disk_size": 50, "size": 1, "label": "role=postgres", "taint": "role=postgres:NoSchedule"}
            ]
        })
    }

    fn compile(source: &str) -> anyhow::Result<Vec<u8>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "wamn-project-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).expect("create the directory");
        // The document and its embedded schema module share the name.
        let file = directory.join("project.k");
        std::fs::write(&file, source).expect("write the document");
        let result = wamn_schema_generator::compile_schema_file(&file, "project");
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        result
    }

    #[test]
    fn the_kcl_form_compiles_back_to_the_same_document() {
        let parsed = ProjectDocument::from_json(document().to_string().as_bytes()).unwrap();
        let bytes = compile(&parsed.to_kcl()).expect("the document compiles");
        assert_eq!(ProjectDocument::from_json(&bytes).unwrap(), parsed);
    }

    #[test]
    fn the_committed_project_compiles_to_the_runbook_values() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/gcp/project.k");
        let compiled = ProjectDocument::compile(&file).expect("the committed document compiles");
        assert_eq!(serde_json::to_value(compiled).unwrap(), document());
    }

    #[test]
    fn kcl_and_json_refuse_fields_the_schema_lacks() {
        for pointer in ["", "/node_pools/0", "/edge"] {
            let mut unknown = document();
            unknown.pointer_mut(pointer).unwrap()["unknown"] = json!("refused");
            let source = format!("import project\nproject.Project {}\n", kcl_value(&unknown));
            assert!(compile(&source).is_err(), "KCL accepted {pointer}/unknown");
            let error = ProjectDocument::from_json(unknown.to_string().as_bytes()).unwrap_err();
            assert!(
                format!("{error:#}").contains("unknown field `unknown`"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn kcl_and_json_refuse_bad_values() {
        for (pointer, value) in [
            ("/project_id", json!("bad")),
            ("/zone", json!("us-central1")),
            ("/edge/neg_zone", json!("us-central1")),
            ("/edge/neg_name", json!("other")),
            ("/cluster_name", json!("Bad")),
            ("/release_channel", json!("latest")),
            ("/network", json!("")),
            ("/node_service_account", json!("nodes@example.com")),
            ("/node_pools/0/disk_size", json!(0)),
            ("/node_pools/0/size", json!(-1)),
            ("/node_pools/0/disk_type", json!("unknown")),
            ("/node_pools/0/spot", json!("true")),
            ("/node_pools/0/label", json!("role")),
            ("/node_pools/0/taint", json!("role=main:unknown")),
            ("/node_pools/1/name", json!("main")),
            ("/node_pools", json!([])),
        ] {
            let mut bad = document();
            *bad.pointer_mut(pointer).unwrap() = value;
            let source = format!("import project\nproject.Project {}\n", kcl_value(&bad));
            assert!(compile(&source).is_err(), "KCL accepted {pointer}");
            assert!(
                ProjectDocument::from_json(bad.to_string().as_bytes()).is_err(),
                "JSON accepted {pointer}"
            );
        }
    }

    #[test]
    fn canonical_bytes_do_not_depend_on_key_order() {
        fn reversed(value: &Value) -> String {
            match value {
                Value::Object(fields) => format!(
                    "{{{}}}",
                    fields
                        .iter()
                        .rev()
                        .map(|(key, value)| {
                            format!(
                                "{}:{}",
                                serde_json::to_string(key).unwrap(),
                                reversed(value)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Value::Array(items) => format!(
                    "[{}]",
                    items.iter().map(reversed).collect::<Vec<_>>().join(",")
                ),
                _ => value.to_string(),
            }
        }
        let value = document();
        let ordered = value.to_string();
        let reordered = reversed(&value);
        assert_ne!(ordered, reordered);
        let a = ProjectDocument::from_json(ordered.as_bytes()).unwrap();
        let b = ProjectDocument::from_json(reordered.as_bytes()).unwrap();
        assert_eq!(a.canonical_json().unwrap(), b.canonical_json().unwrap());
    }

    #[test]
    fn zero_sized_pools_round_trip() {
        let mut value = document();
        value["node_pools"][0]["size"] = json!(0);
        let parsed = ProjectDocument::from_json(value.to_string().as_bytes()).unwrap();
        let bytes = compile(&parsed.to_kcl()).expect("zero-sized pools compile");
        assert_eq!(ProjectDocument::from_json(&bytes).unwrap(), parsed);
    }
}

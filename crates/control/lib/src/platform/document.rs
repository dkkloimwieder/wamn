//! The cluster document, `cluster.k` (docs/plan/platform-install.md P1, P3).
//!
//! The document names the cluster, its node pools and the platform inputs,
//! and nothing else. Owner resources are read by name, never created from
//! this document. It compiles with the `cluster` KCL schema module to JSON,
//! which this module reads into [`ClusterDocument`]. Its canonical form is
//! RFC 8785 JSON, as `environment.k` is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One cluster document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterDocument {
    pub provider: Provider,
    /// GCP-only inputs are empty strings for kind.
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
    pub platform_version: String,
    pub org: String,
    pub owner_email: String,
    /// Registry host and optional repository path, without a URL scheme.
    pub registry: String,
    pub buckets: Buckets,
    pub route_hosts: RouteHosts,
    pub resend_from: String,
}

/// The cluster provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Gcp,
    Kind,
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

/// Owner-supplied buckets, by name. Empty strings for kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buckets {
    pub web: String,
    pub otel: String,
}

/// The control host and each environment's route host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteHosts {
    pub control: String,
    /// Full org/project/env coordinate to route host.
    pub environments: BTreeMap<String, String>,
}

impl ClusterDocument {
    /// Compile a `cluster.k` file and read it.
    ///
    /// # Errors
    ///
    /// When KCL refuses the file, or when the JSON is not a valid document.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let bytes = wamn_schema_generator::compile_schema_file(file, "cluster")?;
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid cluster", file.display()))
    }

    /// Read a document from its JSON form and check every field.
    ///
    /// # Errors
    ///
    /// When a field is unknown or missing, or a value is refused.
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        let document: Self = serde_json::from_slice(bytes).context("decode the cluster JSON")?;
        document.check()?;
        Ok(document)
    }

    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            name(&self.cluster_name, 40),
            "cluster_name is not a lowercase name"
        );
        ensure!(coordinate_name(&self.org), "org is not a coordinate name");
        let version: Vec<_> = self.platform_version.split('.').collect();
        ensure!(
            version.len() == 3
                && version.iter().all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| byte.is_ascii_digit())
                        && (part.len() == 1 || !part.starts_with('0'))
                }),
            "platform_version is not major.minor.patch"
        );
        for (field, value) in [
            ("owner_email", &self.owner_email),
            ("resend_from", &self.resend_from),
        ] {
            ensure!(email(value), "{field} is not an email address");
        }
        let mut registry = self.registry.split('/');
        ensure!(
            token(registry.next().unwrap_or_default(), ".:-")
                && registry.all(|part| token(part, "._-")),
            "registry is not a registry host and optional repository path"
        );
        ensure!(
            matches!(
                self.release_channel.as_str(),
                "rapid" | "regular" | "stable" | "extended"
            ) || (self.provider == Provider::Kind && self.release_channel.is_empty()),
            "release_channel is not a named channel"
        );
        if self.provider == Provider::Gcp {
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
            for (field, value) in [("web", &self.buckets.web), ("otel", &self.buckets.otel)] {
                ensure!(
                    (3..=63).contains(&value.len()) && dns_name(value),
                    "buckets.{field} is not a lowercase bucket name"
                );
            }
        }
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
        ensure!(
            dns_name(&self.route_hosts.control),
            "route_hosts.control is not a lowercase DNS name"
        );
        let mut hosts = BTreeSet::from([&self.route_hosts.control]);
        for (coordinate, host) in &self.route_hosts.environments {
            let parts: Vec<_> = coordinate.split('/').collect();
            ensure!(
                parts.len() == 3
                    && parts[0] == self.org
                    && parts.iter().all(|part| coordinate_name(part)),
                "route coordinate {coordinate:?} does not belong to org/project/env"
            );
            ensure!(
                dns_name(host) && hosts.insert(host),
                "route host {host:?} is invalid or repeated"
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
        let value = serde_json::to_value(self).context("encode the cluster")?;
        Ok(wamn_execution_contract::canonical_json_bytes(&value))
    }

    /// The document as a `cluster.k` file that compiles to this document.
    pub fn to_kcl(&self) -> String {
        let value = serde_json::to_value(self).expect("a cluster document encodes");
        format!("import cluster\n\ncluster.Cluster {}\n", kcl_value(&value))
    }
}

fn lower(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn token(value: &str, punctuation: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || punctuation.as_bytes().contains(&byte)
        })
}

fn name(value: &str, max: usize) -> bool {
    value.len() <= max
        && token(value, "-")
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && !value.ends_with('-')
}

fn coordinate_name(value: &str) -> bool {
    token(value, "_-") && value.starts_with(|c: char| c.is_ascii_lowercase())
}

fn dns_name(value: &str) -> bool {
    value.len() <= 253
        && value
            .split('.')
            .all(|part| part.len() <= 63 && token(part, "-") && !part.ends_with('-'))
}

fn email(value: &str) -> bool {
    !value.chars().any(char::is_whitespace)
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.contains('@')
                && domain
                    .split_once('.')
                    .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        })
}

fn label(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(key, value)| token(key, "_.-") && token(value, "_.-"))
}

/// JSON strings also quote KCL strings; booleans use KCL's spelling.
fn kcl_value(value: &Value) -> String {
    match value {
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(kcl_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{} = {}",
                        serde_json::to_string(key).expect("a string encodes"),
                        kcl_value(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(_) | Value::Number(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn document() -> Value {
        json!({
            "provider": "gcp", "project_id": "wamn-dev", "zone": "us-central1-a",
            "cluster_name": "wamn", "release_channel": "regular",
            "network": "wamn", "subnetwork": "wamn-us-central1",
            "pod_secondary_range_name": "pods", "service_secondary_range_name": "services",
            "node_service_account": "wamn-nodes@wamn-dev.iam.gserviceaccount.com",
            "node_pools": [
                {"name": "main", "machine_type": "e2-standard-2", "spot": true, "disk_type": "pd-standard", "disk_size": 30, "size": 1, "label": "", "taint": ""},
                {"name": "bench", "machine_type": "e2-standard-2", "spot": true, "disk_type": "pd-standard", "disk_size": 30, "size": 1, "label": "role=bench-host", "taint": "role=bench-host:NoSchedule"},
                {"name": "pg", "machine_type": "e2-standard-2", "spot": false, "disk_type": "pd-standard", "disk_size": 50, "size": 1, "label": "role=postgres", "taint": "role=postgres:NoSchedule"}
            ],
            "platform_version": "0.1.31", "org": "dkk", "owner_email": "dkkloimwieder@gmail.com",
            "registry": "us-central1-docker.pkg.dev/wamn-dev/wamn",
            "buckets": {"web": "wamn-dev-web", "otel": "wamn-dev-otel"},
            "route_hosts": {"control": "control.wamn.dev", "environments": {
                "dkk/receiving/dev": "receiving.wamn.dev", "dkk/wms/dev": "wms.wamn.dev"
            }},
            "resend_from": "noreply@wamn.dev"
        })
    }

    fn compile(source: &str) -> anyhow::Result<Vec<u8>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "wamn-cluster-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).expect("create the directory");
        // The document and its embedded schema module share the name.
        let file = directory.join("cluster.k");
        std::fs::write(&file, source).expect("write the document");
        let result = wamn_schema_generator::compile_schema_file(&file, "cluster");
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        result
    }

    #[test]
    fn the_kcl_form_compiles_back_to_the_same_document() {
        let parsed = ClusterDocument::from_json(document().to_string().as_bytes()).unwrap();
        let bytes = compile(&parsed.to_kcl()).expect("the document compiles");
        assert_eq!(ClusterDocument::from_json(&bytes).unwrap(), parsed);
    }

    #[test]
    fn the_committed_cluster_compiles_to_the_runbook_values() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/gcp/cluster.k");
        let compiled = ClusterDocument::compile(&file).expect("the committed document compiles");
        assert_eq!(serde_json::to_value(compiled).unwrap(), document());
    }

    #[test]
    fn kcl_and_json_refuse_fields_the_schema_lacks() {
        for pointer in ["", "/node_pools/0", "/buckets", "/route_hosts"] {
            let mut unknown = document();
            unknown.pointer_mut(pointer).unwrap()["unknown"] = json!("refused");
            let source = format!("import cluster\ncluster.Cluster {}\n", kcl_value(&unknown));
            assert!(compile(&source).is_err(), "KCL accepted {pointer}/unknown");
            let error = ClusterDocument::from_json(unknown.to_string().as_bytes()).unwrap_err();
            assert!(
                format!("{error:#}").contains("unknown field `unknown`"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn kcl_and_json_refuse_bad_values() {
        for (pointer, value) in [
            ("/provider", json!("aws")),
            ("/project_id", json!("bad")),
            ("/zone", json!("us-central1")),
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
            ("/platform_version", json!("latest")),
            ("/org", json!("other")),
            ("/owner_email", json!("owner")),
            ("/resend_from", json!("no reply@wamn.dev")),
            ("/registry", json!("https://registry.example")),
            ("/buckets/web", json!("")),
            ("/route_hosts/control", json!("bad..host")),
            (
                "/route_hosts/environments/dkk~1wms~1dev",
                json!("control.wamn.dev"),
            ),
        ] {
            let mut bad = document();
            *bad.pointer_mut(pointer).unwrap() = value;
            let source = format!("import cluster\ncluster.Cluster {}\n", kcl_value(&bad));
            assert!(compile(&source).is_err(), "KCL accepted {pointer}");
            assert!(
                ClusterDocument::from_json(bad.to_string().as_bytes()).is_err(),
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
        let a = ClusterDocument::from_json(ordered.as_bytes()).unwrap();
        let b = ClusterDocument::from_json(reordered.as_bytes()).unwrap();
        assert_eq!(a.canonical_json().unwrap(), b.canonical_json().unwrap());
    }

    #[test]
    fn kind_needs_no_gcp_resources_and_zero_sized_pools_round_trip() {
        let mut value = document();
        value["provider"] = json!("kind");
        for field in [
            "project_id",
            "zone",
            "release_channel",
            "network",
            "subnetwork",
            "pod_secondary_range_name",
            "service_secondary_range_name",
            "node_service_account",
        ] {
            value[field] = json!("");
        }
        value["buckets"] = json!({"web": "", "otel": ""});
        value["registry"] = json!("localhost:5000/wamn");
        value["node_pools"][0]["size"] = json!(0);
        let parsed = ClusterDocument::from_json(value.to_string().as_bytes()).unwrap();
        let bytes = compile(&parsed.to_kcl()).expect("kind compiles without GCP resources");
        assert_eq!(ClusterDocument::from_json(&bytes).unwrap(), parsed);
    }
}

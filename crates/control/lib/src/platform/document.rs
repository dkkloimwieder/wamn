//! The cluster document, `cluster.k` (docs/plan/platform-install.md P1, P3).
//!
//! The document names the cluster and the platform inputs,
//! and nothing else. Owner resources are read by name, never created from
//! this document. It compiles with the `cluster` KCL schema module to JSON,
//! which this module reads into [`ClusterDocument`]. Its canonical form is
//! RFC 8785 JSON, as `environment.k` is.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One cluster document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterDocument {
    pub provider: Provider,
    /// GCP-only inputs are empty strings for kind.
    pub project_id: String,
    pub zone: String,
    pub cluster_name: String,
    pub org: String,
    pub owner_email: String,
    /// Registry host and optional repository path, without a URL scheme.
    pub registry: String,
    /// Daily cloud build spend limit in USD.
    #[serde(default = "default_build_cloud_cap_usd")]
    pub build_cloud_cap_usd: f64,
    pub buckets: Buckets,
    pub route_hosts: RouteHosts,
    pub resend_from: String,
}

fn default_build_cloud_cap_usd() -> f64 {
    3.0
}

/// The cluster provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Gcp,
    Kind,
}

/// Owner-supplied buckets, by name. Empty strings for kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buckets {
    pub web: String,
    pub otel: String,
    /// The OpenTofu state bucket. `build-platform` keeps its daily counter below `build/`.
    pub state: String,
}

/// The control route host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteHosts {
    pub control: String,
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
            self.build_cloud_cap_usd.is_finite() && self.build_cloud_cap_usd >= 0.0,
            "build_cloud_cap_usd must be a finite, nonnegative amount"
        );
        ensure!(
            name(&self.cluster_name, 40),
            "cluster_name is not a lowercase name"
        );
        ensure!(coordinate_name(&self.org), "org is not a coordinate name");
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
                ("web", &self.buckets.web),
                ("otel", &self.buckets.otel),
                ("state", &self.buckets.state),
            ] {
                ensure!(
                    (3..=63).contains(&value.len()) && dns_name(value),
                    "buckets.{field} is not a lowercase bucket name"
                );
            }
        }
        ensure!(
            dns_name(&self.route_hosts.control),
            "route_hosts.control is not a lowercase DNS name"
        );
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

pub(super) fn lower(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

pub(super) fn token(value: &str, punctuation: &str) -> bool {
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

pub(super) fn name(value: &str, max: usize) -> bool {
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

/// JSON strings also quote KCL strings; booleans use KCL's spelling.
pub(super) fn kcl_value(value: &Value) -> String {
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
            "cluster_name": "wamn",
            "org": "dkk", "owner_email": "dkkloimwieder@gmail.com",
            "registry": "us-central1-docker.pkg.dev/wamn-dev/wamn",
            "build_cloud_cap_usd": 3.0,
            "buckets": {"web": "wamn-dev-web", "otel": "wamn-dev-otel", "state": "wamn-dev-tofu-state"},
            "route_hosts": {"control": "control.wamn.dev"},
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
        for pointer in ["", "/buckets", "/route_hosts"] {
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
            ("/build_cloud_cap_usd", json!(-1.0)),
            ("/project_id", json!("bad")),
            ("/zone", json!("us-central1")),
            ("/cluster_name", json!("Bad")),
            ("/org", json!("Bad")),
            ("/owner_email", json!("owner")),
            ("/resend_from", json!("no reply@wamn.dev")),
            ("/registry", json!("https://registry.example")),
            ("/buckets/web", json!("")),
            ("/route_hosts/control", json!("bad..host")),
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
    fn install_steps_use_the_supplied_version_and_control_domain() {
        let mut value = document();
        value["route_hosts"]["control"] = json!("control.example.org");
        let parsed = ClusterDocument::from_json(value.to_string().as_bytes()).unwrap();
        let steps = super::super::steps::install_steps(&parsed, "2.3.4");
        let certificate = steps.iter().find(|step| step.number == "4.3").unwrap();
        assert!(
            certificate
                .inputs
                .contains(&("certificate DNS name", "*.example.org".into(),))
        );
        let build = steps.iter().find(|step| step.number == "4.6").unwrap();
        assert!(build.inputs.contains(&("platform_version", "2.3.4".into())));
        assert!(build.inputs.contains(&(
            "platform images",
            "us-central1-docker.pkg.dev/wamn-dev/wamn/{wamn-host,wamn-runner,wamn-gates,wamn-identity,wamn-edge-web,wamn/http,wamn/materializer}:2.3.4".into(),
        )));
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
    fn kind_needs_no_gcp_resources() {
        let mut value = document();
        value["provider"] = json!("kind");
        for field in ["project_id", "zone"] {
            value[field] = json!("");
        }
        value["buckets"] = json!({"web": "", "otel": "", "state": ""});
        value["registry"] = json!("localhost:5000/wamn");
        let parsed = ClusterDocument::from_json(value.to_string().as_bytes()).unwrap();
        let bytes = compile(&parsed.to_kcl()).expect("kind compiles without GCP resources");
        assert_eq!(ClusterDocument::from_json(&bytes).unwrap(), parsed);
    }
}

//! The cluster document, `cluster.k` (docs/plan/platform-install.md P1, P3).
//!
//! The document names the cluster and the platform inputs,
//! and nothing else. Owner resources are read by name, never created from
//! this document. It compiles with the `cluster` KCL schema module to JSON,
//! which this module reads into [`ClusterDocument`]. Its canonical form is
//! RFC 8785 JSON, as `environment.k` is.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One cluster document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterDocument {
    /// GCP-only inputs are empty strings for kind.
    pub project_id: String,
    pub zone: String,
    pub cluster_name: String,
    pub org: wamn_registry::OrgId,
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

/// One existing Helm owner observed by platform status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeTarget {
    Control,
    Identity,
    /// One application or shared host group in the hosts namespace.
    Host {
        release: String,
        group: String,
    },
}

impl UpgradeTarget {
    /// The existing Helm release name and namespace that own this selection.
    pub fn owner<'a>(&'a self, document: &'a ClusterDocument) -> (Cow<'a, str>, &'a str) {
        match self {
            Self::Control => (
                wamn_registry::name::control_resource(&document.org).into(),
                document.org.as_str(),
            ),
            Self::Identity => ("identity".into(), document.org.as_str()),
            Self::Host { release, .. } => (release.as_str().into(), document.org.as_str()),
        }
    }
}

/// Owner-supplied buckets, by name. Empty strings for kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buckets {
    pub web: String,
    pub otel: String,
    /// The OpenTofu state bucket.
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
        let bytes = crate::kcl::compile(file, "cluster")?;
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid cluster", file.display()))
    }

    /// Read a document from its JSON form and require_valid every field.
    ///
    /// # Errors
    ///
    /// When a field is unknown or missing, or a value is refused.
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        let document: Self = serde_json::from_slice(bytes).context("decode the cluster JSON")?;
        document.require_valid()?;
        Ok(document)
    }

    fn require_valid(&self) -> anyhow::Result<()> {
        ensure!(
            self.build_cloud_cap_usd.is_finite() && self.build_cloud_cap_usd >= 0.0,
            "build_cloud_cap_usd must be a finite, nonnegative amount"
        );
        ensure!(
            name(&self.cluster_name, 40),
            "cluster_name is not a lowercase name"
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
        Ok(wamn_digest::canonical_json_bytes(&value))
    }

    /// The document as a `cluster.k` file that compiles to this document.
    pub fn to_kcl(&self) -> String {
        let value = serde_json::to_value(self).expect("a cluster document encodes");
        format!("import cluster\n\ncluster.Cluster {}\n", kcl_value(&value))
    }
}

pub fn lower(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_lowercase())
}

pub fn token(value: &str, punctuation: &str) -> bool {
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

pub fn name(value: &str, max: usize) -> bool {
    value.len() <= max
        && token(value, "-")
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && !value.ends_with('-')
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
pub fn kcl_value(value: &Value) -> String {
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

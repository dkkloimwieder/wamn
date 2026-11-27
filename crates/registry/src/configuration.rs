//! Typed applied project settings and environment connection pins.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Settings shared by applied configuration and deploy input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSettings {
    pub org: crate::OrgId,
    pub project: crate::ProjectId,
    pub env: crate::EnvName,
    /// The immutable deployment target name.
    pub target: String,
    #[serde(default)]
    pub schema: Schema,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

/// Applied project configuration with its shared environment settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConfiguration {
    #[serde(flatten)]
    pub settings: EnvSettings,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
    pub revocation_seconds: u32,
    pub apps: BTreeMap<crate::AppName, AppConfiguration>,
}

/// Configuration shared by all releases of an application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, ConnectionReference>,
}

/// A bare connection name. The version is pinned once for the environment.
#[derive(Debug, Clone, PartialEq, Eq, schemars::JsonSchema)]
pub struct ConnectionReference(#[schemars(regex(pattern = "^[a-z][a-z0-9_-]{0,62}$"))] String);

impl ConnectionReference {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Invalid bare connection name.
#[derive(Debug)]
pub struct ParseConnectionReferenceError;

impl fmt::Display for ParseConnectionReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid connection name")
    }
}
impl std::error::Error for ParseConnectionReferenceError {}

impl TryFrom<String> for ConnectionReference {
    type Error = ParseConnectionReferenceError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if valid_connection_name(&name) {
            Ok(Self(name))
        } else {
            Err(ParseConnectionReferenceError)
        }
    }
}

pub fn valid_connection_name(name: &str) -> bool {
    name.len() <= 63
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

impl fmt::Display for ConnectionReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ConnectionReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ConnectionReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name).map_err(serde::de::Error::custom)
    }
}

/// Schema behavior; an omitted mode follows the stored disposable designation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SchemaMode>,
}

/// The schema lifecycle mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SchemaMode {
    Reset,
    Migrate,
}

/// A package and its immutable web artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Client {
    #[schemars(with = "String")]
    pub package: wamn_package::PackageName,
    #[schemars(with = "String", regex(pattern = "^sha256:[0-9a-f]{64}$"))]
    pub web: wamn_digest::Digest,
}

/// The environment pin for a published integration type version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    #[schemars(range(min = 1))]
    pub version: i32,
}

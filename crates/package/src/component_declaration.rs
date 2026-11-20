//! Authored component declarations.

use crate::{PackageCoordinate, canonical_name::CanonicalName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// One author-declared input or output port before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentPortDeclaration {
    pub name: CanonicalName,
    pub schema: Value,
}

/// One author-declared component parameter before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentParameterDeclaration {
    pub name: CanonicalName,
    pub schema: Value,
    pub required: bool,
}

/// The closed set of connection types an admitted component may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentConnectionType {
    Http,
    Blobstore,
}

impl ComponentConnectionType {
    /// Exact WIT `namespace:package` whose import this connection type needs.
    pub fn import_package(self) -> &'static str {
        match self {
            Self::Http => "wamn:connection",
            Self::Blobstore => "wasmcloud:blobstore",
        }
    }
}

/// The source of a connection name: a declared alias or a node parameter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ComponentConnectionAlias {
    Name(CanonicalName),
    Configuration { parameter: CanonicalName },
}

impl From<CanonicalName> for ComponentConnectionAlias {
    fn from(name: CanonicalName) -> Self {
        Self::Name(name)
    }
}
impl ComponentConnectionAlias {
    /// The static alias, when the declaration does not read node configuration.
    pub fn name(&self) -> Option<&CanonicalName> {
        match self {
            Self::Name(name) => Some(name),
            Self::Configuration { .. } => None,
        }
    }
    /// Resolve a declaration against the host-owned node configuration.
    pub fn resolve(&self, configuration: &BTreeMap<CanonicalName, Value>) -> Option<CanonicalName> {
        match self {
            Self::Name(name) => Some(name.clone()),
            Self::Configuration { parameter } => configuration
                .get(parameter)?
                .as_str()
                .and_then(|name| name.parse().ok()),
        }
    }
    /// Whether an application alias can supply this declaration's binding.
    pub fn accepts(&self, name: &CanonicalName) -> bool {
        match self {
            Self::Name(declared) => declared == name,
            Self::Configuration { .. } => true,
        }
    }
}

/// The operations a published connection permits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionMode {
    Read,
    Write,
    /// Modes of configured palette aliases, indexed by package and alias.
    ByBinding(BTreeMap<crate::PackageName, BTreeMap<CanonicalName, ConnectionMode>>),
}
impl ConnectionMode {
    /// Read the sealed mode of one concrete binding. Nested maps are refused.
    pub fn read_only(&self, package: &crate::PackageName, alias: &CanonicalName) -> Option<bool> {
        let mode = match self {
            Self::ByBinding(packages) => packages.get(package)?.get(alias)?,
            mode => mode,
        };
        match mode {
            Self::Read => Some(true),
            Self::Write => Some(false),
            Self::ByBinding(_) => None,
        }
    }
}

/// One portable connection the component reaches under an author-chosen alias.
///
/// The alias is the author-owned half: it is the `requirement` string the guest
/// names at the `wamn:connection` boundary, and the coordinate an environment
/// binds to a concrete instance. Connection SEMANTICS are platform-owned and
/// selected whole by `requirement-type`, never authored field by field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentConnection {
    pub mode: ConnectionMode,
    pub store_alias: ComponentConnectionAlias,
    pub requirement_type: ComponentConnectionType,
}

/// One operation exported by component bytes before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentOperationDeclaration {
    /// Explicit application permission identity. Palette operations carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<crate::registered_operation::RegisteredOperation>,
    /// A matching successful payload shows that this registered command committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_result_schema: Option<Value>,
    pub input_ports: Vec<ComponentPortDeclaration>,
    pub output_ports: Vec<ComponentPortDeclaration>,
    pub parameters: Vec<ComponentParameterDeclaration>,
}

/// Component-owned facts presented with exact component bytes for admission.
///
/// Each map key is the exact exported `wamn:node/handler` instance name. A
/// package operation repeats that key in `registered-operation`; palette
/// operations leave the authorization identity absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentDeclaration<T = PackageCoordinate> {
    /// Whether the verified package manifest declares a data change handler.
    #[serde(skip)]
    pub data_change: bool,
    pub target: T,
    pub component: CanonicalName,
    pub interface_version: CanonicalName,
    pub operations: BTreeMap<CanonicalName, ComponentOperationDeclaration>,
    /// External connections available to every operation of this component.
    #[serde(default)]
    pub connections: Vec<ComponentConnection>,
}

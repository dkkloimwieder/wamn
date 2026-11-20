//! Package-owned deployment facts shared by publication and deployment.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wamn_digest::Digest;

use super::source_name::SourceName;
use crate::canonical_name::CanonicalName;
use crate::{ComponentConnection, ComponentOperationDeclaration, ServingAttachment};

/// Build output carries route and component declarations. Publication replaces
/// the component declarations with admitted facts and pins the OCI images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "D: Deserialize<'de>"))]
pub struct CompiledDeploy<D> {
    pub routes: BTreeMap<CanonicalName, ServingAttachment>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub declarations: BTreeMap<CanonicalName, CompiledComponentDeclaration>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<CanonicalName, PublishedComponent<D>>,
    /// Package OCI digests keyed by the aliases in `base_dependencies`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub base_packages: BTreeMap<SourceName, Digest>,
}

/// Generated admission input. The enclosing package supplies the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CompiledComponentDeclaration {
    pub interface_version: CanonicalName,
    pub operations: BTreeMap<CanonicalName, ComponentOperationDeclaration>,
    pub connections: Vec<ComponentConnection>,
}

/// The admitted facts of a component and the digest of its stock OCI image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedComponent<D> {
    pub descriptor: D,
    pub image_digest: Digest,
}

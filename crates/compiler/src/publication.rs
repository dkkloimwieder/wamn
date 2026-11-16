//! Final publication documents. Readers do not resolve authored references.
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use wamn_catalog::ServingAttachment;

pub use crate::generate::publication::{ATTACHMENTS, COMPONENTS};

/// Read final publication documents from the compiler output.
pub fn read(root: &Path, relative: &str) -> anyhow::Result<Value> {
    let relative = Path::new(relative).strip_prefix("generated")?;
    Ok(serde_json::from_slice(&std::fs::read(
        crate::output_root(root).join(relative),
    )?)?)
}

/// Read the final attachment map.
pub fn read_package_attachments(
    root: &Path,
) -> anyhow::Result<BTreeMap<wamn_catalog::canonical_name::CanonicalName, ServingAttachment>> {
    Ok(serde_json::from_value(read(root, ATTACHMENTS)?)?)
}

/// Component facts before a deployment supplies its environment coordinate.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CompiledComponentDeclaration {
    pub target: wamn_catalog::PackageCoordinate,
    pub component: wamn_catalog::canonical_name::CanonicalName,
    pub interface_version: wamn_catalog::canonical_name::CanonicalName,
    pub operations: BTreeMap<
        wamn_catalog::canonical_name::CanonicalName,
        wamn_catalog::ComponentOperationDeclaration,
    >,
    pub connections: Vec<wamn_catalog::ComponentConnection>,
}

/// Read final component declarations without deployment identity.
pub fn read_components(
    root: &Path,
) -> anyhow::Result<
    BTreeMap<wamn_catalog::canonical_name::CanonicalName, CompiledComponentDeclaration>,
> {
    Ok(serde_json::from_value(read(root, COMPONENTS)?)?)
}

//! Read exact component publication inputs.
use anyhow::Context as _;
use std::collections::BTreeSet;
use std::path::PathBuf;
use wamn_component_policy::ComponentPackageCoordinate;
use wamn_package::ComponentDeclaration;
use wamn_release::component_admission::{
    ComponentAdmission, ComponentAdmissionEngine, ComponentPackage,
};

/// Source-owned inputs for one exact component admission.
#[derive(Debug)]
pub struct AdmitComponentRequest {
    /// Package root whose strict manifest and generated evidence own the component.
    pub package: PathBuf,
    /// Exact wasm component bytes to admit.
    pub component_bytes: PathBuf,
    /// Package-owned component declaration to bind to the bytes.
    pub declaration: PathBuf,
    /// Closed platform import set granted to these exact bytes.
    pub admitted_platform_packages: Vec<String>,
    /// The component digest of each base the package pins: the authored
    /// `base_dependencies` digests, with a base this run built layered over
    /// its pin for a disposable target.
    pub base_digests: BTreeSet<wamn_digest::Digest>,
}

/// Read and admit a built component without publishing it.
pub fn admit_component(
    engine: &dyn ComponentAdmissionEngine,
    args: AdmitComponentRequest,
) -> anyhow::Result<ComponentAdmission> {
    let directory = crate::package_source::read_package_directory(&args.package)?;
    let package = ComponentPackage::validate(&directory)?;
    let component_bytes = std::fs::read(&args.component_bytes)
        .with_context(|| format!("read component bytes {}", args.component_bytes.display()))?;
    let declaration_bytes = std::fs::read(&args.declaration)
        .with_context(|| format!("read component declaration {}", args.declaration.display()))?;
    let document: serde_json::Value = serde_json::from_slice(&declaration_bytes)
        .with_context(|| format!("parse component declaration {}", args.declaration.display()))?;
    let declaration: ComponentDeclaration<ComponentPackageCoordinate> =
        serde_json::from_value(document).with_context(|| {
            format!("parse component declaration {}", args.declaration.display())
        })?;

    wamn_release::component_admission::admit_component(
        engine,
        &package,
        component_bytes,
        declaration,
        args.admitted_platform_packages.into_iter().collect(),
        args.base_digests,
    )
}

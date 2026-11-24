//! Platform-owned palette components, selected from Cargo packages with declarations.
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use wamn_component_policy::{ComponentAdmission, ComponentAdmissionRequest, ComponentDescriptor};

/// Palette facts have no application package or environment coordinate.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PaletteDeclaration {
    pub component: wamn_package::canonical_name::CanonicalName,
    pub interface_version: wamn_package::canonical_name::CanonicalName,
    #[serde(default)]
    pub connections: Vec<wamn_package::ComponentConnection>,
    pub operations: std::collections::BTreeMap<
        wamn_package::canonical_name::CanonicalName,
        wamn_package::ComponentOperationDeclaration,
    >,
}

/// One platform palette component and its declaration.
#[derive(Debug)]
pub struct PaletteComponent {
    pub name: wamn_package::canonical_name::CanonicalName,
    pub file: PathBuf,
    pub declaration: ComponentDescriptor,
}

fn sources(repository: &Path) -> Result<Vec<(PathBuf, String, PaletteDeclaration)>> {
    let mut sources = Vec::new();
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(repository.join("apps/Cargo.toml"))
        .no_deps()
        .exec()?;
    for package in metadata.packages {
        let manifest = package.manifest_path.into_std_path_buf();
        let Some(directory) = manifest.parent() else {
            continue;
        };
        let path = directory.join("declaration.json.in");
        if path.is_file() {
            let declaration = serde_json::from_slice(&fs::read(path)?)?;
            sources.push((manifest, package.name.to_string(), declaration));
        }
    }
    Ok(sources)
}

/// Platform component names, without reading application package declarations.
pub fn names(repository: &Path) -> Result<Vec<wamn_package::canonical_name::CanonicalName>> {
    Ok(sources(repository)?
        .into_iter()
        .map(|(_, _, declaration)| declaration.component)
        .collect())
}

/// Seal one palette declaration against the exact bytes that the platform publishes.
fn seal(
    bytes: &[u8],
    declaration: PaletteDeclaration,
    admission: &impl ComponentAdmission,
) -> Result<ComponentDescriptor> {
    let admitted_platform_packages = declaration
        .operations
        .keys()
        .filter_map(|operation| {
            operation
                .as_str()
                .split_once('/')
                .map(|(package, _)| package.to_owned())
        })
        .chain(
            declaration
                .connections
                .iter()
                .map(|connection| connection.requirement_type.import_package().to_owned()),
        )
        .collect();
    admission.admit(
        bytes,
        ComponentAdmissionRequest {
            declaration: wamn_package::ComponentDeclaration {
                data_change: false,
                target: wamn_package::palette_package(),
                component: declaration.component,
                interface_version: declaration.interface_version,
                operations: declaration.operations,
                connections: declaration.connections,
            },
            admitted_platform_packages,
            base_digests: std::collections::BTreeSet::new(),
        },
    )
}

/// Build palette guests once through the shared compiler, independently of applications.
pub async fn build(
    repository: &Path,
    admission: &impl ComponentAdmission,
) -> Result<Vec<PaletteComponent>> {
    let mut palette = Vec::new();
    for (manifest, package, declaration) in sources(repository)? {
        let file =
            crate::build_guest(repository, &manifest, &package, crate::GuestTarget::WasiP2).await?;
        let declaration = seal(&fs::read(&file)?, declaration, admission)?;
        palette.push(PaletteComponent {
            name: declaration.component.clone(),
            file,
            declaration,
        });
    }
    palette.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(palette)
}

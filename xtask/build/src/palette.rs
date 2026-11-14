//! Platform-owned palette components, selected from Cargo packages with declarations.
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
/// Palette facts have no application package or environment coordinate.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PaletteDeclaration {
    pub component: String,
    pub interface_version: String,
    pub operations: std::collections::BTreeMap<String, wamn_catalog::ComponentOperationDeclaration>,
}

/// One platform palette component and its declaration.
#[derive(Debug)]
pub struct PaletteComponent {
    pub name: String,
    pub file: PathBuf,
    pub declaration: wamn_catalog::ComponentDescriptor,
}

fn sources(repository: &Path) -> Result<Vec<(PathBuf, String, PaletteDeclaration)>> {
    let mut sources = Vec::new();
    for workspace in ["apps/Cargo.toml", "apps/platform/no-std/Cargo.toml"] {
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(repository.join(workspace))
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
    }
    Ok(sources)
}

/// Platform component names, without reading application package declarations.
pub fn names(repository: &Path) -> Result<Vec<String>> {
    Ok(sources(repository)?
        .into_iter()
        .map(|(_, _, declaration)| declaration.component)
        .collect())
}

/// Seal one palette declaration against the exact bytes that the platform publishes.
fn seal(
    bytes: &[u8],
    declaration: PaletteDeclaration,
) -> Result<wamn_catalog::ComponentDescriptor> {
    let engine = wamn_engine::build_engine(&[])?;
    let admitted_platform_packages = declaration
        .operations
        .keys()
        .filter_map(|operation| {
            operation
                .split_once('/')
                .map(|(package, _)| package.to_owned())
        })
        .chain(
            declaration
                .operations
                .values()
                .flat_map(|operation| &operation.connections)
                .filter_map(wamn_catalog::OperationConnection::host)
                .map(|connection| connection.requirement_type.import_package().to_owned()),
        )
        .collect();
    let facts = wamn_engine::component_admission::validate_component_admission(
        &engine,
        bytes,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration: wamn_catalog::ComponentDeclaration {
                target: wamn_catalog::palette_target("platform-palette"),
                component: declaration.component,
                interface_version: declaration.interface_version,
                operations: declaration.operations,
            },
            admitted_platform_packages,
            base_digests: std::collections::BTreeSet::new(),
        },
    )?;
    Ok(wamn_catalog::ComponentDescriptor::new(
        facts.component,
        facts.connections,
    ))
}

/// Build palette guests once through the shared compiler, independently of applications.
pub async fn build(repository: &Path) -> Result<Vec<PaletteComponent>> {
    let mut palette = Vec::new();
    for (manifest, package, declaration) in sources(repository)? {
        let file = crate::build_guest(repository, &manifest, &package, None).await?;
        let declaration = seal(&fs::read(&file)?, declaration)?;
        palette.push(PaletteComponent {
            name: declaration.component.clone(),
            file,
            declaration,
        });
    }
    palette.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(palette)
}

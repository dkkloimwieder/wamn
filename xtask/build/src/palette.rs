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
    pub connections: Vec<wamn_catalog::ComponentConnection>,
}

/// One platform palette component and its declaration.
#[derive(Debug)]
pub struct PaletteComponent {
    pub name: String,
    pub file: PathBuf,
    pub declaration: PaletteDeclaration,
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

/// Read palette declarations for a publication that reuses existing component artifacts.
pub fn declarations(repository: &Path) -> Result<Vec<PaletteDeclaration>> {
    Ok(sources(repository)?
        .into_iter()
        .map(|(_, _, declaration)| declaration)
        .collect())
}

/// Build palette guests once through the shared compiler, independently of applications.
pub async fn build(repository: &Path) -> Result<Vec<PaletteComponent>> {
    let mut palette = Vec::new();
    for (manifest, package, declaration) in sources(repository)? {
        let file = crate::build_guest(repository, &manifest, &package, None).await?;
        palette.push(PaletteComponent {
            name: declaration.component.clone(),
            file,
            declaration,
        });
    }
    palette.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(palette)
}

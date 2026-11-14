//! Read the platform's sealed palette for release and development resolution.
use anyhow::Context as _;
use std::path::Path;
use wamn_catalog::{AdmittedComponentFacts, PALETTE_FILE, PalettePin, palette_target};

#[derive(Debug)]
pub struct PaletteComponent {
    pub facts: AdmittedComponentFacts,
    pub bytes: Vec<u8>,
}

/// Read the palette beside the selected platform route guest and verify its local bytes.
/// Platform artifacts carry both files in the same distribution directory.
pub fn read(route_guest: &Path, project_env_key: &str) -> anyhow::Result<Vec<PaletteComponent>> {
    let path = route_guest.with_file_name(PALETTE_FILE);
    let palette = wamn_catalog::read_palette(
        &std::fs::read(&path)
            .with_context(|| format!("read platform palette {}", path.display()))?,
    )?;
    let directory = path.parent().expect("palette has its guest's directory");
    palette
        .into_values()
        .map(|PalettePin { descriptor, .. }| {
            let file = directory.join(format!(
                "{}.wasm",
                descriptor
                    .component_digest
                    .strip_prefix("sha256:")
                    .expect("palette digest validated")
            ));
            let bytes = std::fs::read(&file)
                .with_context(|| format!("read platform component {}", file.display()))?;
            anyhow::ensure!(
                wamn_digest::bytes_sha256(&bytes) == descriptor.component_digest,
                "platform component differs from its palette pin"
            );
            Ok(PaletteComponent {
                facts: descriptor.into_admitted(palette_target(project_env_key)),
                bytes,
            })
        })
        .collect()
}

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[expect(
    dead_code,
    reason = "the shared helper exposes conformance-only constructors and encoders"
)]
#[path = "src/effect_provider_revision.rs"]
mod effect_provider_revision;

fn repository_root(manifest_dir: &Path) -> Result<PathBuf, String> {
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "execution-host must live at crates/execution/host".to_string())
}

fn run() -> Result<(), String> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| "CARGO_MANIFEST_DIR is not set".to_string())?,
    );
    let repository_root = repository_root(&manifest_dir)?;
    let provider_manifest = manifest_dir.join("effect-provider-revision.json");
    println!("cargo:rerun-if-changed={}", provider_manifest.display());
    let manifest_bytes = fs::read(&provider_manifest)
        .map_err(|error| format!("read {}: {error}", provider_manifest.display()))?;
    let manifest = effect_provider_revision::parse_manifest(&manifest_bytes)
        .map_err(|error| error.to_string())?;
    for relative in effect_provider_revision::governed_watch_paths(&manifest)
        .map_err(|error| error.to_string())?
    {
        println!(
            "cargo:rerun-if-changed={}",
            repository_root.join(relative).display()
        );
    }
    let inputs = effect_provider_revision::collect_revision_inputs(&repository_root, &manifest)
        .map_err(|error| error.to_string())?;
    let revision =
        effect_provider_revision::revision(&inputs).map_err(|error| error.to_string())?;
    let output =
        PathBuf::from(env::var_os("OUT_DIR").ok_or_else(|| "OUT_DIR is not set".to_string())?)
            .join("effect_provider_revision.rs");
    fs::write(
        &output,
        format!(
            "/// Build-generated native effect-provider recipe revision.\n\
             pub const EFFECT_PROVIDER_REVISION: &str = {revision:?};\n"
        ),
    )
    .map_err(|error| format!("write {}: {error}", output.display()))?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        panic!("effect-provider revision generation failed: {error}");
    }
}

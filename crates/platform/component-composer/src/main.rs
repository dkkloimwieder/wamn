//! Command line of the build-time composer; see the library for the rules.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use wamn_component_composer::compose;

#[derive(Debug, Parser)]
#[command(
    name = "wamn-component-composer",
    version,
    about = "Compose an overlay component with its bases"
)]
struct Args {
    /// Overlay main component.
    #[arg(long, value_name = "COMPONENT")]
    overlay: PathBuf,

    /// Component of a base the overlay package pins in `base_dependencies`.
    #[arg(long = "base", value_name = "COMPONENT", required = true)]
    bases: Vec<PathBuf>,

    /// Destination for the composed component bytes.
    #[arg(long, value_name = "COMPONENT")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let overlay = read_component(&args.overlay)?;
    let bases = args
        .bases
        .iter()
        .map(|component| read_component(component))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let composed = compose(overlay, bases)?;
    std::fs::write(&args.output, composed)
        .with_context(|| format!("write composed component {}", args.output.display()))
}

fn read_component(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read component {}", path.display()))
}

//! Command line of the build-time composer; see the library for the rules.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use wamn_component_composer::{compose, compose_control_entry};

#[derive(Debug, Parser)]
#[command(
    name = "wamn-component-composer",
    version,
    about = "Compose an overlay with its bases or build the platform control entry"
)]
struct Args {
    /// Overlay main component.
    #[arg(
        long,
        value_name = "COMPONENT",
        required_unless_present = "control_route",
        conflicts_with = "control_route"
    )]
    overlay: Option<PathBuf>,

    /// Component of a base the overlay package pins in `base_dependencies`.
    #[arg(
        long = "base",
        value_name = "COMPONENT",
        required_unless_present = "control_route",
        conflicts_with = "control_route"
    )]
    bases: Vec<PathBuf>,

    /// Pinned route guest for the platform control entry, without application operations.
    #[arg(long, value_name = "COMPONENT")]
    control_route: Option<PathBuf>,

    /// Destination for the composed component bytes.
    #[arg(long, value_name = "COMPONENT")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let composed = if let Some(route) = &args.control_route {
        compose_control_entry(read_component(route)?)?
    } else {
        let overlay = read_component(args.overlay.as_deref().expect("clap requires an overlay"))?;
        let bases = args
            .bases
            .iter()
            .map(|component| read_component(component))
            .collect::<anyhow::Result<Vec<_>>>()?;
        compose(overlay, bases)?
    };
    std::fs::write(&args.output, composed)
        .with_context(|| format!("write composed component {}", args.output.display()))
}

fn read_component(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read component {}", path.display()))
}

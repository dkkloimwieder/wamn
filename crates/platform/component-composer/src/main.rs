//! Command line of the build-time composer; see the library for the rules.

use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use clap::Parser;
use serde_json::Value;
use wamn_component_composer::{Base, compose};

#[derive(Debug, Parser)]
#[command(
    name = "wamn-component-composer",
    version,
    about = "Compose an overlay component with its base and participants"
)]
struct Args {
    /// Component declaration of the overlay (`publication/components/*.json.in`).
    #[arg(long, value_name = "DECLARATION")]
    overlay_declaration: PathBuf,

    /// Overlay main component.
    #[arg(long, value_name = "COMPONENT")]
    overlay: PathBuf,

    /// Component declaration of a base. Pair each one with a `--base`, in order.
    #[arg(long = "base-declaration", value_name = "DECLARATION", required = true)]
    base_declarations: Vec<PathBuf>,

    /// Base component. Pair each one with a `--base-declaration`, in order.
    #[arg(long = "base", value_name = "COMPONENT", required = true)]
    bases: Vec<PathBuf>,

    /// Participant component that a base calls before commit.
    #[arg(long = "participant", value_name = "COMPONENT")]
    participants: Vec<PathBuf>,

    /// Generated no-op participant of a base. Composition uses it only to
    /// fill an optional slot for which the overlay names no participant.
    #[arg(long = "no-op-participant", value_name = "COMPONENT")]
    no_op_participants: Vec<PathBuf>,

    /// Destination for the composed component bytes.
    #[arg(long, value_name = "COMPONENT")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(
        args.base_declarations.len() == args.bases.len(),
        "each --base-declaration needs one --base: {} declarations, {} components",
        args.base_declarations.len(),
        args.bases.len()
    );
    let overlay_declaration = read_declaration(&args.overlay_declaration)?;
    let overlay = read_component(&args.overlay)?;
    let bases = args
        .base_declarations
        .iter()
        .zip(&args.bases)
        .map(|(declaration, component)| {
            Ok(Base {
                declaration: read_declaration(declaration)?,
                bytes: read_component(component)?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let participants = args
        .participants
        .iter()
        .map(|path| read_component(path))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let no_op_participants = args
        .no_op_participants
        .iter()
        .map(|path| read_component(path))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let composed = compose(
        &overlay_declaration,
        overlay,
        bases,
        participants,
        &no_op_participants,
    )?;
    std::fs::write(&args.output, composed)
        .with_context(|| format!("write composed component {}", args.output.display()))
}

fn read_declaration(path: &Path) -> anyhow::Result<Value> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read component declaration {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse component declaration {}", path.display()))
}

fn read_component(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read component {}", path.display()))
}

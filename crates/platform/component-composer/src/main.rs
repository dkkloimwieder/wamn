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
    about = "Compose an overlay component with its bases"
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
    let composed = compose(&overlay_declaration, overlay, bases)?;
    std::fs::write(&args.output, composed)
        .with_context(|| format!("write composed component {}", args.output.display()))
}

/// Read an authored declaration template, with its operation references
/// resolved against the wamn.json of the package that holds it.
fn read_declaration(path: &Path) -> anyhow::Result<Value> {
    wamn_schema_generator::operation_reference::read_authored_document(
        path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Declaration,
    )
    .with_context(|| format!("read component declaration {}", path.display()))
}

fn read_component(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read component {}", path.display()))
}

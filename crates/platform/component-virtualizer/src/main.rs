//! Deterministically removes ambient WASI CLI imports from native tenant components.

use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use clap::Parser;
use wasi_virt::WasiVirt;

const WASI_VIRT_SOURCE_REV: &str = "448f6df8f688cee5d6995e96b1ffc31f9bf00742";
const WASI_VIRT_ADAPTER_SHA256: &str =
    "28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632";

#[derive(Debug, Parser)]
#[command(
    name = "wamn-component-virtualizer",
    version,
    about = "Normalize a native tenant component to the closed Wamn import policy"
)]
struct Args {
    /// Native wasm32-wasip2 component to normalize.
    #[arg(long, value_name = "COMPONENT")]
    input: PathBuf,

    /// Destination for the normalized component bytes.
    #[arg(long, value_name = "COMPONENT")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let input = args
        .input
        .canonicalize()
        .with_context(|| format!("resolve input component {}", args.input.display()))?;
    let output_identity = output_identity(&args.output)?;
    ensure!(
        input != output_identity,
        "input and output must be different files: {}",
        input.display()
    );

    let normalized = virtualize(&input)?;
    std::fs::write(&args.output, normalized)
        .with_context(|| format!("write normalized component {}", args.output.display()))?;

    eprintln!(
        "wasi-virt-source-rev={WASI_VIRT_SOURCE_REV} \
         wasi-virt-adapter-sha256=sha256:{WASI_VIRT_ADAPTER_SHA256}"
    );
    Ok(())
}

fn output_identity(output: &Path) -> anyhow::Result<PathBuf> {
    if output.exists() {
        return output
            .canonicalize()
            .with_context(|| format!("resolve output component {}", output.display()));
    }

    let file_name = output
        .file_name()
        .context("output path must name a component file")?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .with_context(|| format!("resolve output directory {}", parent.display()))?;
    Ok(parent.join(file_name))
}

fn virtualize(input: &Path) -> anyhow::Result<Vec<u8>> {
    let mut virtualizer = WasiVirt::new();

    // Preserve the already-admitted clock capability while encapsulating the
    // ambient CLI surface linked by Rust std.
    virtualizer.clocks(true);
    virtualizer.env().deny_all();
    virtualizer.exit(false);
    virtualizer.stdio().deny();
    virtualizer.wasm_opt(false);
    virtualizer.compose_component_path(input);
    virtualizer
        .filter_imports()
        .context("filter virtualization to the component's imported WASI surface")?;

    let result = virtualizer
        .finish()
        .context("compose the pinned WASI virtualization adapter")?;
    Ok(result.adapter)
}

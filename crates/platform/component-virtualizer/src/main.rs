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
    virtualize_bytes(&std::fs::read(input)?)
}

fn virtualize_bytes(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use wac_graph::{CompositionGraph, EncodeOptions, plug, types::Package};

    let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(bytes)?
    else {
        anyhow::bail!("virtualization requires a component");
    };
    let imports: Vec<_> = resolve.worlds[world]
        .imports
        .values()
        .filter_map(|item| match item {
            wit_parser::WorldItem::Interface { id, .. } => resolve.id_of(*id),
            _ => None,
        })
        .collect();
    let imported = |prefix: &str| imports.iter().any(|name| name.starts_with(prefix));
    let mut virtualizer = WasiVirt::new();
    if imported("wasi:clocks/") {
        virtualizer.clocks(true);
    }
    if imported("wasi:cli/environment") {
        virtualizer.env().deny_all();
    }
    if imported("wasi:cli/exit") {
        virtualizer.exit(false);
    }
    if imported("wasi:cli/std") || imported("wasi:cli/terminal") {
        virtualizer.stdio().deny();
    }
    virtualizer.wasm_opt(false);
    let result = virtualizer
        .finish()
        .context("build the pinned WASI virtualization adapter")?;
    // The repository's composer reads current component-model named imports.
    let mut graph = CompositionGraph::new();
    let adapter = Package::from_bytes("wasi-virt", None, result.adapter, graph.types_mut())?;
    let adapter = graph.register_package(adapter)?;
    let component = Package::from_bytes("component", None, bytes, graph.types_mut())?;
    let component = graph.register_package(component)?;
    plug(&mut graph, vec![adapter], component)
        .context("compose the pinned WASI virtualization adapter")?;
    graph
        .encode(EncodeOptions::default())
        .context("encode the virtualized component")
}

#[cfg(test)]
mod tests {
    #[test]
    fn virtualization_preserves_two_labels_of_one_typed_interface() -> anyhow::Result<()> {
        let mut resolve = wit_parser::Resolve::new();
        resolve.push_str("environment.wit", "package wasi:cli@0.2.12; interface environment { get-environment: func() -> list<tuple<string, string>>; }")?;
        let package = resolve.push_str("named.wit", "package example:meter@1.0.0; interface read { get: func() -> u16; } world client { import first: read; import second: read; import wasi:cli/environment@0.2.12; }")?;
        let world = resolve.select_world(&[package], Some("client"))?;
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )?;
        let bytes = wit_component::ComponentEncoder::default()
            .module(&module)?
            .validate(true)
            .encode()?;
        let bytes = super::virtualize_bytes(&bytes)?;
        let wit_component::DecodedWasm::Component(resolve, world) = wit_component::decode(&bytes)?
        else {
            panic!("component")
        };
        for label in ["first", "second"] {
            let item =
                &resolve.worlds[world].imports[&wit_parser::WorldKey::Name(label.to_owned())];
            let wit_parser::WorldItem::Interface { id, .. } = item else {
                panic!("interface")
            };
            assert_eq!(
                resolve.id_of(*id).as_deref(),
                Some("example:meter/read@1.0.0")
            );
        }
        assert!(!resolve.worlds[world].imports.values().any(|item| matches!(item, wit_parser::WorldItem::Interface { id, .. } if resolve.id_of(*id).as_deref() == Some("wasi:cli/environment@0.2.12"))));
        Ok(())
    }
}

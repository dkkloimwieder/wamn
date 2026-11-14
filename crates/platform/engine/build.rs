use std::path::PathBuf;

fn main() {
    let source = "../../../wit/wamn-node";
    println!("cargo:rerun-if-changed={source}");
    let mut resolve = wit_parser::Resolve::new();
    let (package, _) = resolve.push_dir(source).expect("read node contract");
    let world = resolve
        .select_world(&[package], Some("node"))
        .expect("resolve node world");
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )
    .expect("embed node contract");
    let bytes = wit_component::ComponentEncoder::default()
        .module(&module)
        .expect("read reference module")
        .validate(true)
        .encode()
        .expect("encode node reference");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    std::fs::write(output.join("node-contract.wasm"), bytes).expect("write node reference");
}

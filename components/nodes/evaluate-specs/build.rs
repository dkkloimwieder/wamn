#[path = "src/contract.rs"]
mod contract;

fn main() {
    println!("cargo::rerun-if-changed=Cargo.toml");
    let manifest = std::fs::read_to_string("Cargo.toml").expect("Cargo.toml must be readable");
    if let Err(error) = contract::validate_manifest(&manifest, "evaluate-specs") {
        panic!("evaluate-specs component contract rejected: {error}");
    }
}

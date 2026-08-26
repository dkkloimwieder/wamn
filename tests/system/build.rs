use std::fs;
use std::path::Path;

const FORBIDDEN_CRATES: &[&str] = &[
    "wamn_cdc_reader",
    "wamn_ctl",
    "wamn_dispatcher",
    "wamn_executor",
    "wamn_host",
    "wamn_run_worker",
    "wamn_scenario_worker",
    "wamn_waker",
];

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");

    let manifest = fs::read_to_string("Cargo.toml").expect("system Cargo.toml reads");
    assert!(
        !manifest.contains("services/"),
        "system proofs must not depend on service package paths"
    );
    for forbidden in FORBIDDEN_CRATES {
        let package = forbidden.replace('_', "-");
        assert!(
            !manifest.contains(&package),
            "system proofs must not declare forbidden service package `{}`",
            package
        );
    }
    // wamn-hopk R5: a recursive source scan for forbidden `use` lines stood
    // here, complete with its own hand-rolled comment skip. It is deleted. The
    // manifest check above is the same rule at the only layer that can enforce
    // it absolutely: a crate absent from [dependencies] cannot be imported at
    // all, and trying is E0432 at build time.
}


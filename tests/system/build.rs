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
    scan_sources(Path::new("src"));
}

fn scan_sources(dir: &Path) {
    for entry in fs::read_dir(dir).expect("system source directory reads") {
        let path = entry.expect("system source entry reads").path();
        if path.is_dir() {
            scan_sources(&path);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("system source reads");
        for (line_number, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for forbidden in FORBIDDEN_CRATES {
                assert!(
                    !code.contains(&format!("{forbidden}::"))
                        && !code.contains(&format!("use {forbidden}")),
                    "{}:{} imports forbidden service crate `{forbidden}`",
                    path.display(),
                    line_number + 1
                );
            }
        }
    }
}

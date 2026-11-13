use std::fs;
use std::io;
use std::path::Path;

use wamn_digest::Hasher;

const INPUTS: &[&str] = &[
    "src",
    "../control/src",
    "../control/Cargo.toml",
    "kcl",
    "../../catalog/model/src",
    "../../catalog/model/Cargo.toml",
    "Cargo.toml",
    "build.rs",
    "../../platform/component-virtualizer/src",
    "../../platform/component-virtualizer/Cargo.toml",
    "../../../deploy/sql/record-history.sql",
    "../../../deploy/sql/record-history-app-grants.sql",
];

fn main() -> io::Result<()> {
    // platform_build::inputs::declared_reads needs literal directives.
    // Keep these paths equal to INPUTS. Cargo watches directories recursively.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../control/src");
    println!("cargo:rerun-if-changed=../control/Cargo.toml");
    println!("cargo:rerun-if-changed=kcl");
    println!("cargo:rerun-if-changed=../../catalog/model/src");
    println!("cargo:rerun-if-changed=../../catalog/model/Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../platform/component-virtualizer/src");
    println!("cargo:rerun-if-changed=../../platform/component-virtualizer/Cargo.toml");
    println!("cargo:rerun-if-changed=../../../deploy/sql/record-history.sql");
    println!("cargo:rerun-if-changed=../../../deploy/sql/record-history-app-grants.sql");

    let mut digest = Hasher::new();
    // Templates live in Rust emitters and KCL modules. Hash whole trees so
    // new emitters and templates join the fingerprint without a file list.
    for path in INPUTS {
        hash_path(Path::new(path), &mut digest)?;
    }
    println!(
        "cargo:rustc-env=WAMN_GENERATOR_FINGERPRINT={}",
        wamn_digest::format_sha256(&digest.finalize())
    );
    Ok(())
}

fn hash_path(path: &Path, digest: &mut Hasher) -> io::Result<()> {
    if path.is_dir() {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        for entry in entries {
            hash_path(&entry, digest)?;
        }
    } else {
        // A NUL separates the path from the fixed-length content digest.
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(wamn_digest::sha256(&fs::read(path)?));
    }
    Ok(())
}

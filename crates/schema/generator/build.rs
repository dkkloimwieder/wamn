use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest as _, Sha256};

fn main() -> io::Result<()> {
    let mut digest = Sha256::new();
    // Templates live in Rust emitters and KCL modules. Hash whole trees so
    // new emitters and templates join the fingerprint without a file list.
    for path in [
        "src",
        "kcl",
        "Cargo.toml",
        "build.rs",
        "../../../deploy/sql/record-history.sql",
        "../../../deploy/sql/record-history-app-grants.sql",
    ] {
        hash_path(Path::new(path), &mut digest)?;
    }
    println!(
        "cargo:rustc-env=WAMN_GENERATOR_FINGERPRINT=sha256:{}",
        hex::encode(digest.finalize())
    );
    Ok(())
}

fn hash_path(path: &Path, digest: &mut Sha256) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
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
        digest.update(Sha256::digest(fs::read(path)?));
    }
    Ok(())
}

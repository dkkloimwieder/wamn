use std::fs;
use std::path::{Path, PathBuf};

use wamn_digest::{Hasher, format_sha256};

fn frame(hash: &mut Hasher, bytes: &[u8]) {
    hash.update(
        u64::try_from(bytes.len())
            .expect("source file size fits in u64")
            .to_be_bytes(),
    );
    hash.update(bytes);
}

fn hash_path(path: &Path, hash: &mut Hasher) {
    frame(
        hash,
        path.to_str().expect("generator path is UTF-8").as_bytes(),
    );
    if path.is_dir() {
        let mut paths = fs::read_dir(path)
            .expect("read generator sources")
            .map(|entry| entry.expect("read generator source entry").path())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        for path in paths {
            hash_path(&path, hash);
        }
    } else {
        frame(hash, &fs::read(path).expect("read generator source"));
    }
}

fn main() {
    let mut hash = Hasher::new();
    for input in [
        "build.rs",
        "Cargo.toml",
        "src",
        "kcl",
        "../package/src",
        "../routes/src/exposure.rs",
        "../routes/src/route_identity.rs",
        "../schema/src",
        "../scalar/src",
        "../record-history/src",
        "../number-pattern/src",
        "../postgres/Cargo.toml",
        "../postgres/src",
        "../role/src",
        "../identity-protocol/src",
        "../../deploy/sql/record-history.sql",
        "../../deploy/sql/record-history-app-grants.sql",
    ] {
        println!("cargo:rerun-if-changed={input}");
        hash_path(Path::new(input), &mut hash);
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(
        output.join("generator-id"),
        format_sha256(&hash.finalize()).to_string(),
    )
    .expect("write generator identity");
}

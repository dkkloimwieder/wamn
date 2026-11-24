//! Local integration builds use real artifacts.

use std::path::Path;

use wamn_oci::integration;

fn fixture() -> wamn_test_infrastructure::scratch::ScratchRoot {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration");
    std::fs::create_dir_all(root.join("integrations/fixture/src")).unwrap();
    std::fs::create_dir_all(root.join("tools")).unwrap();
    for file in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        std::fs::copy(
            source.join(file),
            root.join("integrations/fixture").join(file),
        )
        .unwrap();
    }
    // The fixture imports the platform integration interface from the
    // repository's `wit/`.
    std::fs::create_dir_all(root.join("wit/wamn-integration")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/wamn-integration/package.wit"),
        root.join("wit/wamn-integration/package.wit"),
    )
    .unwrap();
    // The scratch repository keeps the same pinned Rust toolchain.
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml"),
        root.join("rust-toolchain.toml"),
    )
    .unwrap();
    scratch
}

#[tokio::test]
async fn build_digest_tracks_source_bytes_and_survives_another_checkout() {
    let first = fixture();
    let a = integration::build(
        first.path(),
        &"fixture".parse().unwrap(),
        wamn_build::build_component,
    )
    .await
    .unwrap();
    let b = integration::build(
        first.path(),
        &"fixture".parse().unwrap(),
        wamn_build::build_component,
    )
    .await
    .unwrap();
    assert_eq!(a.digest, b.digest);
    let other = fixture();
    let c = integration::build(
        other.path(),
        &"fixture".parse().unwrap(),
        wamn_build::build_component,
    )
    .await
    .unwrap();
    assert_eq!(a.digest, c.digest);
    let source = first.path().join("integrations/fixture/src/lib.rs");
    let bytes = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        format!("{bytes}\n// A source-only edit changes artifact identity.\n"),
    )
    .unwrap();
    let edited = integration::build(
        first.path(),
        &"fixture".parse().unwrap(),
        wamn_build::build_component,
    )
    .await
    .unwrap();
    assert_ne!(a.digest, edited.digest);
}

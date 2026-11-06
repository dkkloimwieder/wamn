//! Delivery native builds use one feature selection and ignore ambient build overrides.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

#[test]
fn native_build_uses_fixed_inputs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scratch = std::env::temp_dir().join(format!("wamn-native-build-{}", std::process::id()));
    fs::create_dir(&scratch).unwrap();
    let cargo = scratch.join("cargo");
    fs::write(&cargo, r#"#!/bin/bash
printf '%s\n' "$@"
printf 'flags=%s\nincremental=%s\ncc=%s\nepoch=%s\n' "$CARGO_ENCODED_RUSTFLAGS" "$CARGO_INCREMENTAL" "$CC" "$SOURCE_DATE_EPOCH"
[[ -z ${RUSTFLAGS+x} && -z ${CARGO_FEATURES+x} && -z ${CARGO_PROFILE_DEV_OPT_LEVEL+x} && -z ${WAMN_DELIVERY_CANDIDATE+x} && -z ${DATABASE_URL+x} ]]
"#).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let target = scratch.join("target");
    let output = Command::new("bash")
        .arg(root.join("tools/delivery-owned"))
        .arg("build-native")
        .arg(&target)
        .env(
            "PATH",
            format!("{}:{}", scratch.display(), std::env::var("PATH").unwrap()),
        )
        .env("RUSTFLAGS", "ambient-flags")
        .env("CARGO_ENCODED_RUSTFLAGS", "ambient-encoded-flags")
        .env("CARGO_FEATURES", "ops")
        .env("CARGO_PROFILE_DEV_OPT_LEVEL", "3")
        .env("WAMN_DELIVERY_CANDIDATE", "ambient-candidate")
        .env("DATABASE_URL", "ambient-database")
        .env("CC", "ambient-compiler")
        .env("SOURCE_DATE_EPOCH", "999")
        .output()
        .unwrap();
    fs::remove_dir_all(&scratch).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "build\n--locked\n--offline\n--profile\ndev\n--bins\n--target-dir\n{}\n-p\nwamn-host\n-p\nwamn-ctl\n-p\nwamn-identity\n-p\nwamn-cdc-reader\n-p\nwamn-scenario-worker\nflags=-C\u{1f}link-arg=-fuse-ld=mold\u{1f}--remap-path-prefix={0}=/wamn-build\nincremental=0\ncc=clang\nepoch=0\n",
            target.display()
        )
    );
}

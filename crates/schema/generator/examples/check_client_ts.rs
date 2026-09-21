//! Type-checks the generated TypeScript bindings for the platform fixture.
//!
//! Run this by hand. `tsc` is not a build step and not a test, so nothing in
//! `cargo build` or `cargo test` needs Node.
//!
//! ```bash
//! cargo run --locked --offline -p wamn-schema-generator --example check_client_ts
//! ```
//!
//! It emits the fixture bindings into a temporary directory, writes a strict
//! `tsconfig.json` beside them, and runs `tsc` from `PATH`. Pass a directory to
//! keep the emitted source.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use wamn_schema_generator::client_ts::emit_ts_client;

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;

/// The lowest `tsc` that accepts every option below.
const MINIMUM_TSC: &str = "5.5";

/// A package marker, so `NodeNext` reads the emitted modules as ES modules.
const PACKAGE_JSON: &str = "{ \"type\": \"module\" }\n";

/// Strict means every option that turns a silent `any` into a refusal.
const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022"],
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noImplicitAny": true,
    "strictNullChecks": true,
    "exactOptionalPropertyTypes": true,
    "noUncheckedIndexedAccess": true,
    "noUnusedLocals": true,
    "noEmit": true,
    "skipLibCheck": true,
    "types": []
  },
  "include": ["**/*.ts"]
}
"#;

fn main() -> Result<()> {
    let root = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => std::env::temp_dir().join(format!("wamn-check-client-ts-{}", std::process::id())),
    };
    let version = tsc_version()?;
    let files = emit_ts_client(&fixture::client_release())
        .map_err(|error| anyhow::anyhow!("the platform fixture emits: {error}"))?;

    if root.exists() {
        std::fs::remove_dir_all(&root)
            .with_context(|| format!("{} must be replaceable", root.display()))?;
    }
    std::fs::create_dir_all(&root)?;
    std::fs::write(root.join("package.json"), PACKAGE_JSON)?;
    std::fs::write(root.join("tsconfig.json"), TSCONFIG)?;
    for file in &files {
        // Every emitted path starts with `generated/client-ts/`. The check
        // needs the modules beside each other, not that prefix.
        let name = Path::new(file.path())
            .file_name()
            .context("an emitted path names a file")?;
        std::fs::write(root.join(name), file.bytes())?;
    }

    println!("tsc {version}");
    println!("checking {} modules in {}", files.len(), root.display());
    let status = Command::new("tsc")
        .arg("--project")
        .arg(&root)
        .status()
        .context("running tsc")?;
    if !status.success() {
        bail!("tsc refused the generated bindings in {}", root.display());
    }
    println!("the generated bindings type-check under tsc strict");
    Ok(())
}

fn tsc_version() -> Result<String> {
    let output = Command::new("tsc")
        .arg("--version")
        .output()
        .map_err(|error| {
            anyhow::anyhow!(
                "tsc must be on PATH, version {MINIMUM_TSC} or later: {error}. \
             Install it with `npm install --global typescript`."
            )
        })?;
    if !output.status.success() {
        bail!("`tsc --version` failed with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

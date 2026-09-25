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
//! `tsconfig.json` beside them, and runs `tsc` from `PATH`. The emitted
//! modules import the hand-written runtime, so the configuration maps that
//! specifier to `web/runtime` in this checkout. Pass a directory to keep the
//! emitted source.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use wamn_schema_generator::client_ts::{RUNTIME_PACKAGE, emit_ts_client};

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;

/// The lowest `tsc` that accepts every option below.
const MINIMUM_TSC: &str = "5.5";

/// A package marker, so `NodeNext` reads the emitted modules as ES modules.
const PACKAGE_JSON: &str = "{ \"type\": \"module\" }\n";

/// Strict means every option that turns a silent `any` into a refusal.
///
/// The two markers take the runtime's package name and the path of
/// `web/runtime/src/index.ts` in this checkout, because the emitted modules
/// import the runtime by name and this directory installs nothing.
const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM"],
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
    "types": [],
    "baseUrl": ".",
    "paths": { "RUNTIME_PACKAGE": ["RUNTIME_PATH"] }
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
    let runtime = repository_root()?.join("web/runtime/src/index.ts");
    std::fs::write(
        root.join("tsconfig.json"),
        TSCONFIG
            .replace("RUNTIME_PACKAGE", RUNTIME_PACKAGE)
            .replace("RUNTIME_PATH", &runtime.display().to_string()),
    )?;
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

/// The checkout that holds `web/runtime`, from this crate's own location.
fn repository_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .context("the generator crate sits three directories below the repository root")
}

fn tsc_version() -> Result<String> {
    let output = Command::new("tsc")
        .arg("--version")
        .output()
        .map_err(|error| {
            anyhow::anyhow!(
                "tsc must be on PATH, version {MINIMUM_TSC} or later: {error}. \
             Install it with `pnpm add --global typescript`."
            )
        })?;
    if !output.status.success() {
        bail!("`tsc --version` failed with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

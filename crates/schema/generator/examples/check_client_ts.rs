//! Type-checks the generated TypeScript bindings for the platform fixture.
//!
//! Run this by hand. `tsc` is not a build step and not a test, so nothing in
//! `cargo build` or `cargo test` needs Node.
//!
//! ```bash
//! cargo run --locked --offline -p wamn-schema-generator --example check_client_ts
//! ```
//!
//! It emits fixture bindings under sync-bench source, writes a strict
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
        None => repository_root()?.join("web/sync-bench/src/generated/check-client-ts"),
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

    // Exercise the same binding surfaces with each integer key wire type.
    for ty in ["int32", "int64"] {
        let mut release = fixture::client_release();
        for model in &mut release.models {
            integer_ids(&mut model.fields, ty);
            for operation in &mut model.operations {
                if operation.type_ == "create" {
                    operation.input_fields.retain(|field| field.path != "id");
                }
                integer_ids(&mut operation.input_fields, ty);
                integer_ids(&mut operation.result_fields, ty);
                if let Some(route) = &mut operation.route {
                    integer_ids(&mut route.response.fields, ty);
                }
            }
        }
        let directory = root.join(ty);
        std::fs::create_dir(&directory)?;
        for file in emit_ts_client(&release)? {
            let name = Path::new(file.path())
                .file_name()
                .context("an emitted path names a file")?;
            std::fs::write(directory.join(name), file.bytes())?;
        }
    }

    println!("tsc {version}");
    println!(
        "checking UUID, int32, and int64 modules in {}",
        root.display()
    );
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

fn integer_ids(fields: &mut [wamn_schema_generator::client_ir::FieldIr], ty: &str) {
    for field in fields {
        if field.path.rsplit('.').next() == Some("id") {
            ty.clone_into(&mut field.type_name);
        }
        integer_ids(&mut field.children, ty);
    }
}

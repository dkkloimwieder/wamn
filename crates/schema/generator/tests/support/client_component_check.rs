//! The existing description check, shared by the example and integration test.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde_json::json;
use wamn_schema_generator::client_component::emit_ts_components;
use wamn_schema_generator::client_plan::ClientPlan;

pub fn check_descriptions(
    release: &wamn_schema_generator::client_ir::ClientContractIr,
) -> Result<()> {
    let repository = repository_root()?;
    let root = std::env::temp_dir().join(format!("wamn-check-descriptions-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let result = check(&repository, &root, release);
    std::fs::remove_dir_all(&root).context("remove generated description check files")?;
    result
}

fn check(
    repository: &Path,
    root: &Path,
    release: &wamn_schema_generator::client_ir::ClientContractIr,
) -> Result<()> {
    let files = emit_ts_components(&ClientPlan::from_ir(release))?;
    for file in &files {
        let name = Path::new(file.path())
            .file_name()
            .context("a generated module name")?;
        std::fs::write(root.join(name), file.bytes())?;
    }
    let wfe = repository.join("web/wfe/packages");
    let platform = repository.join("apps/platform_ui/node_modules");
    let configuration = json!({
        "compilerOptions": {
            "target": "ES2022", "lib": ["ES2022", "DOM", "DOM.Iterable"],
            "module": "ESNext", "moduleResolution": "Bundler",
            "jsx": "preserve", "jsxImportSource": "@solidjs/web",
            "customConditions": ["wfe-source"],
            "strict": true, "allowImportingTsExtensions": true,
            "noUncheckedIndexedAccess": true, "noUnusedLocals": true,
            "noEmit": true, "skipLibCheck": true, "types": [],
            "paths": {
                "@wfe/generator": [wfe.join("generator/src/index.ts")],
                "@wfe/components": [wfe.join("components/src/index.ts")],
                "@wfe/components/*": [wfe.join("components/src/*/index.ts")],
                "@wfe/form": [wfe.join("form/src/index.ts")],
                "solid-js": [platform.join("solid-js")],
                "@solidjs/web": [platform.join("@solidjs/web")],
                "@solidjs/web/*": [platform.join("@solidjs/web/*")],
                "d3-shape": [platform.join("@types/d3-shape")]
            }
        },
        "include": ["*.ts"]
    });
    std::fs::write(
        root.join("tsconfig.json"),
        serde_json::to_vec_pretty(&configuration)?,
    )?;
    let status = Command::new("pnpm")
        .current_dir(repository)
        .args(["exec", "tsc", "--project"])
        .arg(root)
        .status()
        .context("run the existing TypeScript check")?;
    if !status.success() {
        bail!("wfe refused the emitted descriptions: {status}");
    }
    println!(
        "{} generated modules type-check against @wfe/generator",
        files.len()
    );
    Ok(())
}

fn repository_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .context("the generator sits three directories below the repository root")
}

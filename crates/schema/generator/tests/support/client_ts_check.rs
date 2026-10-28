use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr};
use wamn_schema_generator::client_ts::emit_ts_client;

pub fn behavior(releases: &[(&str, ClientContractIr)]) -> Result<()> {
    with_clients(releases, |repository, root| {
        let output = Command::new("pnpm")
            .current_dir(repository.join("web/runtime"))
            .args(["exec", "vitest", "run", "--config"])
            .arg(repository.join("crates/schema/generator/tests/client_ts_behavior.config.mjs"))
            .env("WAMN_CLIENT_TS_TEST_ROOT", root)
            .output()
            .context("run emitted client behavior tests")?;
        ensure!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    })
}

pub fn type_check(release: &ClientContractIr) -> Result<()> {
    let repository = repository_root();
    if !repository.join("node_modules/.bin/tsc").exists() {
        eprintln!(
            "SKIP emitted_client_type_checks_with_workspace_tsc: workspace tsc is absent. Run `pnpm install --offline` at the repository root."
        );
        return Ok(());
    }
    let mut widened = release.clone();
    widened
        .models
        .iter_mut()
        .find(|model| model.name == "widget")
        .unwrap()
        .operations
        .iter_mut()
        .find(|operation| operation.name == "query")
        .unwrap()
        .paging
        .as_mut()
        .unwrap()
        .sort
        .as_mut()
        .unwrap()
        .fields
        .push("code".to_owned());
    with_clients(
        &[("fixture", release.clone()), ("widened", widened)],
        |repository, root| {
            std::fs::write(
                root.join("consumer.ts"),
                include_str!("client_ts_consumer.ts"),
            )?;
            let configuration = json!({
                "compilerOptions": {
                    "target": "ES2022", "lib": ["ES2022", "DOM"],
                    "module": "NodeNext", "moduleResolution": "NodeNext",
                    "strict": true, "exactOptionalPropertyTypes": true,
                    "noUncheckedIndexedAccess": true, "noUnusedLocals": true,
                    "noEmit": true, "skipLibCheck": true, "types": [],
                    "paths": {
                        "@wamn/web-runtime": [repository.join("web/runtime/src/index.ts")],
                        "@wfe/sync": [repository.join("web/wfe/packages/sync/dist/index.d.ts")]
                    }
                },
                "include": ["**/*.ts"]
            });
            std::fs::write(
                root.join("tsconfig.json"),
                serde_json::to_vec_pretty(&configuration)?,
            )?;
            let output = Command::new("pnpm")
                .current_dir(repository)
                .args(["exec", "tsc", "--project"])
                .arg(root)
                .output()
                .context("compile the emitted client with workspace tsc")?;
            ensure!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        },
    )
}

fn with_clients(
    releases: &[(&str, ClientContractIr)],
    test: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<()> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "wamn-client-ts-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root)?;
    let result = (|| {
        std::fs::write(root.join("package.json"), r#"{"type":"module"}"#)?;
        for (name, release) in releases {
            write_client(&root.join(name), release)?;
        }
        for ty in ["int32", "int64"] {
            let mut release = releases[0].1.clone();
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
            write_client(&root.join(ty), &release)?;
        }
        test(&repository_root(), &root)
    })();
    std::fs::remove_dir_all(&root).context("remove emitted client test files")?;
    result
}

fn write_client(root: &Path, release: &ClientContractIr) -> Result<()> {
    std::fs::create_dir(root)?;
    for file in emit_ts_client(release)? {
        let name = Path::new(file.path())
            .file_name()
            .context("an emitted module name")?;
        std::fs::write(root.join(name), file.bytes())?;
    }
    Ok(())
}

fn integer_ids(fields: &mut [FieldIr], ty: &str) {
    for field in fields {
        if field.path.rsplit('.').next() == Some("id") {
            ty.clone_into(&mut field.type_name);
        }
        integer_ids(&mut field.children, ty);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .to_path_buf()
}

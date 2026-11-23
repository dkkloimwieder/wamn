//! Compile generated SQL accessors against the guest library in both output layouts.

use super::{AuthoredSql, GenerationInput, StatementTransactionality, generate};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The guest library and the directories of every library Cargo built with it.
///
/// Cargo places each library in a directory of its own, so the test asks
/// Cargo for the artifacts of the build of this unit test target. That build
/// is fresh while the test runs, so Cargo only reports the paths.
fn guest_library() -> (PathBuf, BTreeSet<PathBuf>) {
    let output =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "test",
                "--no-run",
                "--quiet",
                "--message-format=json",
                "--package",
                env!("CARGO_PKG_NAME"),
                "--lib",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            // The outer build sets this package's build-script directory. Build
            // scripts of the dependencies read it, so the report would rebuild them.
            .env_remove("OUT_DIR")
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "Cargo did not report the test build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut library = None;
    let mut directories = BTreeSet::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        for file in message["filenames"].as_array().into_iter().flatten() {
            let path = PathBuf::from(file.as_str().unwrap());
            let extension = path.extension().and_then(|extension| extension.to_str());
            if !matches!(extension, Some("rlib" | "rmeta" | "so")) {
                continue;
            }
            if extension == Some("rlib") && message["target"]["name"] == "wamn_guest" {
                library = Some(path.clone());
            }
            directories.insert(path.parent().unwrap().to_owned());
        }
    }
    (
        library.expect("Cargo built the guest library dependency"),
        directories,
    )
}

#[test]
fn authored_sql_accessors_compile_in_both_output_layouts() {
    use wamn_schema::ir::{CatalogIr, Column, ColumnDefault, Constraint, Table};
    let catalog = CatalogIr::new(vec![Table::new(
        "warehouse",
        "location",
        vec![
            Column::new("id", wamn_scalar::SqlValueType::Uuid, false, None, None),
            Column::new(
                "created_at",
                wamn_scalar::SqlValueType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![Constraint::primary_key("location_pkey", ["id"]).unwrap()],
        Vec::new(),
    )]);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "package": {"id": "sql_layout", "version": "1.0.0"},
        "required_platform_policy_contract": {"id": "sql_access", "state": "satisfied"},
        "custom_operations": {"location.lookup": {
            "type": "projection", "visibility": "public", "permission": "location.lookup",
            "connection": "postgres", "input": {"fields": []},
            "result": {"class": "one", "fields": [{"path": "value", "type": "int32", "nullable": false}]},
            "errors": ["invalid_input", "not_found", "retry", "timeout", "permission_denied", "internal_error"],
            "statements": {"lookup": {
                "path": "query/location.sql", "fetch": "one", "parameters": [],
                "row": [{"name": "value", "type": "int32", "nullable": false}]
            }}
        }},
        "models": {"location": {
            "schema": "warehouse", "table": "location", "owner": "sql_layout",
            "audit_log": {"columns": ["created_at"], "retention": "none"},
            "operations": {"get": {"permission": "location.get", "result": "one"}}
        }}, "components": {"data": {}}, "connections": {}
    })).unwrap();
    let sources = [AuthoredSql::new(
        "query/location.sql",
        b"SELECT 1::integer AS value",
    )];
    let transactionality = StatementTransactionality::from_paths(BTreeMap::default());
    let root = std::env::temp_dir().join(format!("wamn-sql-layout-{}", uuid::Uuid::new_v4()));
    let (library, dependencies) = guest_library();
    let build = root.join("build");
    let unpacked = root.join("unpacked");
    for (generated_root, authored_root) in [
        (build.as_path(), build.as_path()),
        (unpacked.join("generated").as_path(), unpacked.as_path()),
    ] {
        let input = GenerationInput::new(&catalog, &manifest, &sources, &transactionality)
            .with_output_paths(generated_root, authored_root);
        let package = generate(&input).unwrap();
        for file in package.files() {
            let path =
                generated_root.join(Path::new(file.path()).strip_prefix("generated").unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.bytes()).unwrap();
        }
        for source in &sources {
            let path = authored_root.join(source.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source.bytes).unwrap();
        }
        let consumer = generated_root.join("consumer.rs");
        std::fs::write(&consumer, r#"
            #[path = "wamn/location.rs"] mod location;
            #[path = "wamn/location_lookup.rs"] mod location_lookup;
            pub async fn read(connection: &mut wamn_guest::postgres_statements::Connection, id: wamn_guest::postgres_statements::Uuid) {
                let _ = location::get(connection, id).await;
                let _ = location_lookup::lookup(connection).await;
            }
        "#).unwrap();
        let output = std::process::Command::new("rustc")
            .args([
                "--edition=2024",
                "--crate-type=lib",
                "--crate-name=sql_layout",
                "--emit=metadata",
            ])
            .arg("--extern")
            .arg(format!("wamn_guest={}", library.display()))
            .args(dependencies.iter().flat_map(|directory| {
                [
                    "-L".to_owned(),
                    format!("dependency={}", directory.display()),
                ]
            }))
            .arg("--out-dir")
            .arg(generated_root)
            .arg(consumer)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated accessor did not compile: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

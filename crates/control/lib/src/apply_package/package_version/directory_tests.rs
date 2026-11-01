use std::path::{Path, PathBuf};

use super::{read_package_directory, read_source_package_directory};

#[test]
fn local_source_preparation_reads_migrations_without_build_output() {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path().join("source_package");
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(
        root.join("wamn.k"),
        r#"import manifest
manifest.Package {
    package = {id = "source_package", version = "1.0.0"}
    required_platform_policy_contract = {id = "source_data_access", state = "satisfied"}
    models = {
        item = manifest.Model("item", "source_data") {
            table = "item"
            owner = "source_package"
            audit_log = {columns = ["created_at"]}
            operations: {get: {result = "one"}}
        }
    }
    components = {source = {connections = ["postgres"]}}
}
"#,
    )
    .unwrap();
    std::fs::write(root.join("migrations/0001_initial.sql"), b"SELECT 1;\n").unwrap();

    let directory = read_source_package_directory(&root).unwrap();
    let manifest =
        wamn_schema_generator::PackageManifest::from_slice(&directory.manifest_bytes).unwrap();
    assert_eq!(manifest.package.id, "source_package");
    assert_eq!(directory.artifact_digest, None);
    assert_eq!(directory.migrations.len(), 1);
    assert_eq!(directory.migrations[0].bytes, b"SELECT 1;\n");
    assert!(!wamn_schema_generator::output_root(&root).exists());
    assert!(read_package_directory(&root).is_err());
}

const FILES: [(&str, &str); 3] = [
    (
        "0001_initial.sql",
        "CREATE TABLE inventory.lane (id uuid);\n",
    ),
    (
        "0002_lane_note.sql",
        "ALTER TABLE inventory.lane ADD COLUMN note text;\n",
    ),
    (
        "0003_lane_code.sql",
        "ALTER TABLE inventory.lane ADD COLUMN code text;\n",
    ),
];

fn write_package(root: &Path, order: [usize; 3]) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(root.join("wamn.json"), b"{}").unwrap();
    for index in order {
        let (name, sql) = FILES[index];
        std::fs::write(root.join("migrations").join(name), sql).unwrap();
    }
}

fn package_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wamn-package-directory-{label}-{}",
        std::process::id()
    ))
}

/// Two directories that hold the same files give the same package bytes,
/// whatever order the files were created in or the file system lists them in.
#[test]
fn the_creation_order_of_migration_files_does_not_change_the_package_bytes() {
    let forward = package_root("forward");
    let reverse = package_root("reverse");
    write_package(&forward, [0, 1, 2]);
    write_package(&reverse, [2, 1, 0]);

    let forward_bytes = read_package_directory(&forward).unwrap();
    let reverse_bytes = read_package_directory(&reverse).unwrap();
    std::fs::remove_dir_all(&forward).unwrap();
    std::fs::remove_dir_all(&reverse).unwrap();

    assert_eq!(forward_bytes, reverse_bytes);
    let paths = forward_bytes
        .migrations
        .iter()
        .map(|migration| migration.relative_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "migrations/0001_initial.sql",
            "migrations/0002_lane_note.sql",
            "migrations/0003_lane_code.sql",
        ]
    );
}

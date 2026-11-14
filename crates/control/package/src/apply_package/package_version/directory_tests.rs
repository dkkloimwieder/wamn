use std::path::{Path, PathBuf};

use super::read_package_directory;

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
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("wamn.k"), "# authored fixture").unwrap();
    let output = wamn_compiler::output_root(root);
    std::fs::create_dir_all(output.join("migrations")).unwrap();
    std::fs::write(
        output.join("wamn.json"),
        include_bytes!("../../../tests/fixtures/apply_package/base/generated/wamn.json"),
    )
    .unwrap();
    std::fs::create_dir_all(output.join("history")).unwrap();
    std::fs::write(output.join("history/0004_record_history.sql"), "SELECT 4;").unwrap();
    for index in order {
        let (name, sql) = FILES[index];
        std::fs::write(output.join("migrations").join(name), sql).unwrap();
    }
}

fn package_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wamn-package-directory-{label}-{}",
        std::process::id()
    ))
}

/// The reader combines authored and generated migrations in SQLx version order.
#[tokio::test]
async fn compiled_inputs_share_one_sequence_for_authored_and_generated_migrations() {
    for (label, order) in [("forward", [0, 1, 2]), ("reverse", [2, 1, 0])] {
        let root = package_root(label);
        write_package(&root, order);
        read_package_directory(&root).unwrap();
        let plan = wamn_schema_control::migrations::migrator(&super::migration_root(&root))
            .await
            .unwrap();
        assert_eq!(plan.iter().count(), FILES.len() + 1);
        for (pending, (name, _)) in plan.iter().zip(FILES) {
            let ordinal: u32 = name.split_once('_').unwrap().0.parse().unwrap();
            assert_eq!(pending.version, i64::from(ordinal));
        }
        std::fs::remove_dir_all(wamn_compiler::output_root(&root)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

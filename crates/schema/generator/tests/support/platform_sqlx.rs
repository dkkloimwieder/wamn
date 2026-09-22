use std::fs;

use wamn_schema_generator::stage_sqlx_verifier;

use super::fixture;

#[test]
fn platform_sql_corpus_stages_real_sqlx_macros_without_an_operation_list() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root");
    let scratch = repository
        .join("target/platform-sqlx-fixture")
        .join(std::process::id().to_string());
    let package = scratch.join("package");
    let verifier = scratch.join("verifier");
    materialize_fixture(&package);

    let staged = stage_sqlx_verifier(&package, &verifier, repository)
        .expect("stage the generated SQLx verifier");
    let source = fs::read_to_string(verifier.join("src/lib.rs")).expect("read verifier source");
    assert_eq!(
        staged.queries,
        source.matches("sqlx::query_file_as!").count(),
        "every discovered accessor has one real SQLx macro"
    );
    assert!(
        staged.queries >= 6,
        "CRUD and custom SQL are both discovered"
    );
    assert!(source.contains("native::widget::WidgetRow"));
    assert!(source.contains("native::widget_archive::ArchiveRow"));
    assert_eq!(
        fs::read(verifier.join("command/widget/archive.sql")).unwrap(),
        fs::read(package.join("command/widget/archive.sql")).unwrap(),
        "the macro compiles the exact runtime SQL bytes"
    );
    let metadata = std::process::Command::new("cargo")
        .current_dir(&verifier)
        .args(["metadata", "--locked", "--offline", "--no-deps"])
        .output()
        .expect("resolve the staged verifier manifest");
    assert!(
        metadata.status.success(),
        "staged verifier must resolve from the committed lockfile: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );

    fs::remove_dir_all(scratch).expect("remove platform SQLx fixture");
}

fn materialize_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root).expect("create platform fixture root");
    std::fs::write(
        root.join("wamn.json"),
        serde_json::to_vec(&fixture::manifest()).expect("serialize platform fixture manifest"),
    )
    .expect("write platform fixture manifest");
    for (path, bytes) in [
        ("query/widget.sql", fixture::QUERY_SQL),
        (
            "query/widget_by_created_at_descending.sql",
            fixture::QUERY_DESCENDING_SQL,
        ),
        ("query/widget_list.sql", fixture::LIST_SQL),
        (
            "query/widget_maker_list.sql",
            fixture::WIDGET_MAKER_LIST_SQL,
        ),
        ("command/widget/archive.sql", fixture::ARCHIVE_SQL),
        ("command/widget/claim.sql", fixture::CLAIM_SQL),
        ("command/widget/replay.sql", fixture::REPLAY_SQL),
        ("command/widget/finalize.sql", fixture::FINALIZE_SQL),
    ] {
        let destination = root.join(path);
        std::fs::create_dir_all(destination.parent().expect("fixture SQL parent"))
            .expect("create fixture SQL parent");
        std::fs::write(destination, bytes).expect("write fixture SQL");
    }
    for file in fixture::generate_fixture().files() {
        let destination = root.join(file.path());
        std::fs::create_dir_all(destination.parent().expect("generated fixture parent"))
            .expect("create generated fixture parent");
        std::fs::write(destination, file.bytes()).expect("write generated fixture artifact");
    }
}

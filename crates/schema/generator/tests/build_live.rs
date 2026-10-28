//! Live PostgreSQL 18 tests of `wamn build` (docs/plan/platform-deploy.md §7.1, R10).
//!
//! Each test copies the platform fixture, without its committed `generated/`,
//! into a directory of its own and builds it against the server of the test
//! process. The build creates and drops its own verification database there.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wamn_schema_generator::build::{
    BuildRequest, build_components, build_package, generate_package, package_output,
};
use wamn_schema_generator::receipt;
use wamn_schema_generator::{GenerateError, GenerateErrorType, PackageManifest};

/// A test-owned copy of the fixture and its overlay, side by side.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("wamn-build-live-{}-{name}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove a stale workspace");
        }
        for (source, package) in [
            (
                wamn_fixture_package::package_root(),
                wamn_fixture_package::PACKAGE_ID,
            ),
            (
                wamn_fixture_package::overlay_root(),
                wamn_fixture_package::OVERLAY_PACKAGE_ID,
            ),
        ] {
            copy_source(&source, &root.join(package));
        }
        Self { root }
    }

    fn package(&self) -> PathBuf {
        self.root.join(wamn_fixture_package::PACKAGE_ID)
    }

    fn overlay(&self) -> PathBuf {
        self.root.join(wamn_fixture_package::OVERLAY_PACKAGE_ID)
    }

    /// Replace one exact text of the fixture's `wamn.k`.
    fn edit_manifest(&self, from: &str, to: &str) {
        let path = self.package().join("wamn.k");
        let source = fs::read_to_string(&path).expect("read the fixture wamn.k");
        assert_eq!(source.matches(from).count(), 1, "{from} is not unique");
        fs::write(&path, source.replace(from, to)).expect("write the fixture wamn.k");
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove the workspace");
    }
}

/// Copy a package's source tree: everything except build output.
fn copy_source(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create the package copy");
    for entry in fs::read_dir(source).expect("read the package") {
        let entry = entry.expect("read a package entry");
        let name = entry.file_name();
        if name == "generated" || name == "target" {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        if entry.file_type().expect("inspect a package entry").is_dir() {
            copy_source(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy a package file");
        }
    }
}

/// Every file below `root`, by its path relative to `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read an output directory") {
            let path = entry.expect("read an output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).expect("read an output file");
                files.insert(path.strip_prefix(root).unwrap().to_owned(), bytes);
            }
        }
    }
    files
}

async fn build(package: &Path, output_root: &Path, database: &str) -> anyhow::Result<Value> {
    build_package(&BuildRequest::new(package, output_root, database)).await?;
    let path = package_output(output_root, wamn_fixture_package::PACKAGE_ID).join("wamn.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn statement<'a>(manifest: &'a Value, operation: &str, name: &str) -> &'a Value {
    &manifest["custom_operations"][operation]["statements"][name]
}

/// R10(2) and R10(3): the copy holds no generated verifier, so pass one
/// describes without one; two builds give the same bytes and receipts; the
/// manifest is canonical and hashed once; an overlay records its base.
#[tokio::test(flavor = "current_thread")]
async fn stale_and_clean_builds_give_identical_outputs_and_receipts() {
    let workspace = Workspace::new("identical");
    workspace.edit_manifest(
        "manifest.Package {",
        "manifest.Package {\n    client_package = {name = \"@wamn/platform-fixture-client\"}",
    );
    // The fixture normally has no browser client. Its update-only model
    // needs a read to supply the revision when this test enables components.
    workspace.edit_manifest(
        "operations: {\n                update: {writable_fields = [\"label\"], revision_field = \"edit_version\", result = \"one\"}",
        "operations: {\n                get: {revision_field = \"edit_version\", result = \"one\"}\n                update: {writable_fields = [\"label\"], revision_field = \"edit_version\", result = \"one\"}",
    );
    let database = wamn_test_postgres::database();
    assert!(!workspace.package().join("generated").exists());

    let first = workspace.root.join("target-first");
    let second = workspace.root.join("target-second");
    // The default output can hold an older manifest even when the caller
    // selects a different output root. Authored routes must use current owners.
    let previous = wamn_schema_generator::output_root(&workspace.package());
    fs::create_dir_all(&previous).expect("create previous build output");
    let mut old: Value = serde_json::from_slice(
        &wamn_schema_generator::compile_manifest(&workspace.package()).unwrap(),
    )
    .unwrap();
    old["package"]["version"] = Value::String("1.0.0".to_owned());
    fs::write(
        previous.join("wamn.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .expect("write the old manifest");
    let stale = package_output(&first, wamn_fixture_package::PACKAGE_ID)
        .join("client-ts/components/obsolete.tsx");
    fs::create_dir_all(stale.parent().unwrap()).expect("create stale client output");
    fs::write(&stale, b"obsolete").expect("write an obsolete component");
    let receipt_first = build_package(&BuildRequest::new(
        &workspace.package(),
        &first,
        database.url(),
    ))
    .await
    .expect("build the fixture");
    fs::remove_dir_all(&previous).expect("remove the previous build output");
    let receipt_second = build_package(&BuildRequest::new(
        &workspace.package(),
        &second,
        database.url(),
    ))
    .await
    .expect("build the fixture again");

    let output_first = package_output(&first, wamn_fixture_package::PACKAGE_ID);
    let output_second = package_output(&second, wamn_fixture_package::PACKAGE_ID);
    let files = snapshot(&output_first);
    assert_eq!(files, snapshot(&output_second));
    for path in [
        "data/counter.rs",
        "wit/counter_query_codec.rs",
        "client-ts/counter.ts",
        "sql/counter/changes.sql",
    ] {
        assert!(
            files.contains_key(Path::new(path)),
            "the bigint model participates in determinism: {path}"
        );
    }
    let bindings = std::str::from_utf8(&files[Path::new("client-ts/widget.ts")]).unwrap();
    let components =
        std::str::from_utf8(&files[Path::new("client-ts/components/widget.ts")]).unwrap();
    assert!(bindings.contains("export async function archive("));
    assert!(components.contains("export const WidgetArchiveForm ="));
    assert_eq!(receipt_first, receipt_second);
    assert_eq!(receipt_first, receipt::read(&output_first).unwrap());
    assert!(
        files.keys().any(|path| path.starts_with("native-verifier")),
        "pass two generated no verifier"
    );
    assert!(!workspace.package().join("generated").exists());

    let manifest = &files[Path::new("wamn.json")];
    let value: Value = serde_json::from_slice(manifest).unwrap();
    assert_eq!(
        manifest,
        &wamn_execution_contract::canonical_json_bytes(&value),
        "wamn.json is not canonical"
    );
    let digest = receipt_first.outputs.manifest.clone();
    let data_access: Value =
        serde_json::from_slice(&files[Path::new("platform-policy/data-access.json")]).unwrap();
    assert_eq!(
        data_access.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["contract", "package", "relations", "role", "schemas"],
        "the data-access overlay carries no package identity"
    );
    assert_eq!(
        receipt_first.outputs.files.len(),
        files.len() - 1,
        "the receipt lists every output but itself"
    );
    assert_eq!(
        receipt_first.inputs.digest,
        receipt::input_digest(&workspace.package()).unwrap()
    );

    let overlay = build_package(&BuildRequest::new(
        &workspace.overlay(),
        &first,
        database.url(),
    ))
    .await
    .expect("build the overlay over the built base");
    assert_eq!(overlay.dependencies.len(), 1);
    assert_eq!(overlay.dependencies[0].id, wamn_fixture_package::PACKAGE_ID);
    assert_eq!(overlay.dependencies[0].digest, digest);
}

/// §6.1 row 3 with the owner's nullability rule (wamn-00rts.3): a left-out
/// type is derived; a nullable declaration over a column that SQLx says cannot
/// be null is kept; a not-null declaration over a column that SQLx says can be
/// null is refused.
#[tokio::test(flavor = "current_thread")]
async fn statement_types_are_derived_and_nullability_only_widens() {
    let database = wamn_test_postgres::database();

    let accepted = Workspace::new("accepted");
    accepted.edit_manifest(
        "row = [manifest.uuid(\"widget_id\")]",
        "row = [manifest.Value {name = \"widget_id\"}]",
    );
    accepted.edit_manifest(
        "manifest.int32(\"edit_version\"), manifest.nullable",
        "manifest.nullable(manifest.int32(\"edit_version\")), manifest.nullable",
    );
    let manifest = build(
        &accepted.package(),
        &accepted.root.join("target"),
        database.url(),
    )
    .await
    .expect("build with a left-out type and a nullable declaration over NOT NULL");
    let record_batch = statement(&manifest, "widget.record_batch", "find_widget");
    assert_eq!(record_batch["row"][0]["type"], "uuid");
    let archive = statement(&manifest, "widget.archive", "archive");
    assert_eq!(archive["row"][1]["name"], "edit_version");
    assert_eq!(archive["row"][1]["type"], "int32");
    assert_eq!(archive["row"][1]["nullable"], true);

    let refused = Workspace::new("refused");
    refused.edit_manifest(
        "manifest.nullable(manifest.text(\"note\"))",
        "manifest.Value {name = \"note\", type = \"text\", nullable = False}",
    );
    let error = build(
        &refused.package(),
        &refused.root.join("target"),
        database.url(),
    )
    .await
    .expect_err("a not-null declaration on a nullable column was accepted");
    assert!(
        format!("{error:#}").contains("row value note is declared not null"),
        "{error:#}"
    );
}

fn projection_view_workspace(name: &str) -> Workspace {
    let workspace = Workspace::new(name);
    let migration = workspace.package().join("migrations/0001_initial.sql");
    let mut sql = fs::read_to_string(&migration).unwrap();
    sql.push_str("\nCREATE VIEW inventory.widget_screen AS SELECT id, code, edit_version, to_jsonb(widget) AS attributes FROM inventory.widget;\n");
    fs::write(migration, sql).unwrap();
    fs::write(
        workspace.package().join("query/widget_list.sql"),
        "SELECT id, code, edit_version, attributes FROM widget_screen ORDER BY id;\n",
    )
    .unwrap();
    workspace
}

fn authored_feed_workspace(name: &str, definition: &str) -> Workspace {
    let workspace = Workspace::new(name);
    workspace.edit_manifest(
        "\"widget.list\" = manifest.Projection(\"widget.list\") {",
        "\"widget.list\" = manifest.Projection(\"widget.list\") {\n            feed = True",
    );
    workspace.edit_manifest(
        "audit_log = {columns = [\"created_at\"]}",
        "audit_log = {columns = [\"created_at\"], retention = \"P30D\"}",
    );
    fs::write(workspace.package().join("migrations/0003_feed.sql"), format!(
        "ALTER TABLE inventory.widget ADD COLUMN tag_id uuid CONSTRAINT widget_tag_id_fkey REFERENCES inventory.widget_tag(id);\n\
         ALTER TABLE inventory.widget_maker ADD COLUMN supplier_id uuid CONSTRAINT widget_maker_supplier_id_fkey REFERENCES inventory.widget_tag(id);\n\
         CREATE VIEW inventory.widget_screen AS {definition};\n"
    )).unwrap();
    fs::write(
        workspace.package().join("query/widget_list.sql"),
        "SELECT id, code, edit_version, attributes FROM widget_screen ORDER BY id;\n",
    )
    .unwrap();
    workspace
}

const RECEIPT_FEED_VIEW: &str = "SELECT line.id, line.code, line.edit_version,
    to_jsonb(line) AS attributes, line.maker_id, line.tag_id
    FROM inventory.widget AS line
    JOIN inventory.widget_maker AS orders ON orders.id = line.maker_id
    JOIN inventory.widget_tag AS items ON items.id = line.tag_id";

async fn authored_feed_build(workspace: &Workspace) -> anyhow::Result<Value> {
    let database = wamn_test_postgres::database();
    let output_root = workspace.root.join("target/wamn");
    generate_package(&BuildRequest::new(
        &workspace.package(),
        &output_root,
        database.url(),
    ))
    .await?;
    let bytes =
        fs::read(package_output(&output_root, wamn_fixture_package::PACKAGE_ID).join("wamn.json"))?;
    Ok(
        serde_json::from_slice::<Value>(&bytes)?["custom_operations"]["widget.list"]["feed_source"]
            .clone(),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_receipt_screen_records_its_join_path() {
    let workspace = authored_feed_workspace("feed-receipt", RECEIPT_FEED_VIEW);
    let source = authored_feed_build(&workspace).await.unwrap();
    assert_eq!(
        source["root"],
        serde_json::json!({"schema": "inventory", "table": "widget"})
    );
    assert_eq!(
        source["read_set"],
        serde_json::json!([
            {"schema": "inventory", "table": "widget"},
            {"schema": "inventory", "table": "widget_maker"},
            {"schema": "inventory", "table": "widget_tag"}
        ])
    );
    assert_eq!(
        source["join_path"],
        serde_json::json!([
            {"table": {"schema": "inventory", "table": "widget_maker"},
             "linking_column": "maker_id", "direction": "referenced_by_link",
             "foreign_key_table": {"schema": "inventory", "table": "widget"},
             "foreign_key_columns": [{"column": "maker_id", "referenced_column": "id"}]},
            {"table": {"schema": "inventory", "table": "widget_tag"},
             "linking_column": "tag_id", "direction": "referenced_by_link",
             "foreign_key_table": {"schema": "inventory", "table": "widget"},
             "foreign_key_columns": [{"column": "tag_id", "referenced_column": "id"}]}
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_grouped_summary_records_both_link_directions() {
    let workspace = authored_feed_workspace("feed-grouped", "SELECT orders.id, orders.name AS code,
        orders.edit_version::int4 AS edit_version, jsonb_build_object('lines', count(line.id)) AS attributes,
        orders.supplier_id FROM inventory.widget_maker AS orders
        JOIN inventory.widget_tag AS supplier ON supplier.id = orders.supplier_id
        LEFT JOIN inventory.widget AS line ON line.maker_id = orders.id
        GROUP BY orders.id, supplier.label");
    workspace.edit_manifest(
        "lists = {model = \"widget\", key_field = \"id\", display_field = \"code\"}",
        "lists = {model = \"widget_maker\", key_field = \"id\", display_field = \"code\"}",
    );
    let source = authored_feed_build(&workspace).await.unwrap();
    assert_eq!(source["root"]["table"], "widget_maker");
    assert_eq!(source["join_path"][0]["direction"], "references_root");
    assert_eq!(
        source["join_path"][0]["foreign_key_columns"],
        serde_json::json!([{"column": "maker_id", "referenced_column": "id"}])
    );
    assert_eq!(source["join_path"][1]["linking_column"], "supplier_id");
    assert_eq!(source["join_path"][1]["direction"], "referenced_by_link");
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_a_one_to_many_view_without_grouping() {
    let workspace = authored_feed_workspace("feed-many", "SELECT orders.id, orders.name AS code,
        orders.edit_version::int4 AS edit_version, to_jsonb(orders) AS attributes
        FROM inventory.widget_maker AS orders JOIN inventory.widget AS line ON line.maker_id = orders.id");
    workspace.edit_manifest(
        "lists = {model = \"widget\", key_field = \"id\", display_field = \"code\"}",
        "lists = {model = \"widget_maker\", key_field = \"id\", display_field = \"code\"}",
    );
    let error = authored_feed_build(&workspace).await.unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("widget.list")
            && message.contains("view inventory.widget_screen")
            && message.contains("primary key"),
        "{message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_a_base_table_without_a_link() {
    let workspace = authored_feed_workspace("feed-no-link", "SELECT line.id, line.code, line.edit_version,
        to_jsonb(items) AS attributes FROM inventory.widget AS line CROSS JOIN inventory.widget_tag AS items");
    let message = format!("{:#}", authored_feed_build(&workspace).await.unwrap_err());
    assert!(
        message.contains("widget.list") && message.contains("widget_tag has no link"),
        "{message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_statement_tables_and_joins() {
    for (name, sql) in [
        (
            "table",
            "SELECT id, code, edit_version, to_jsonb(widget) AS attributes FROM widget",
        ),
        (
            "join",
            "SELECT screen.id, screen.code, screen.edit_version, screen.attributes FROM widget_screen AS screen JOIN widget ON widget.id = screen.id",
        ),
    ] {
        let workspace =
            authored_feed_workspace(&format!("feed-statement-{name}"), RECEIPT_FEED_VIEW);
        fs::write(workspace.package().join("query/widget_list.sql"), sql).unwrap();
        let message = format!("{:#}", authored_feed_build(&workspace).await.unwrap_err());
        assert!(
            message.contains("widget.list")
                && message.contains("relation widget is not an authored view"),
            "{message}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_statement_grouping_windows_and_limits() {
    for (name, sql, reason) in [
        (
            "group",
            "SELECT id, code, edit_version, attributes FROM widget_screen GROUP BY id, code, edit_version, attributes",
            "GROUP BY",
        ),
        (
            "limit",
            "SELECT id, code, edit_version, attributes FROM widget_screen LIMIT 5",
            "LIMIT",
        ),
        (
            "window",
            "SELECT id, code, edit_version, to_jsonb(row_number() OVER ()) AS attributes FROM widget_screen",
            "window",
        ),
    ] {
        let workspace =
            authored_feed_workspace(&format!("feed-statement-{name}"), RECEIPT_FEED_VIEW);
        fs::write(workspace.package().join("query/widget_list.sql"), sql).unwrap();
        let message = format!("{:#}", authored_feed_build(&workspace).await.unwrap_err());
        assert!(
            message.contains("widget.list") && message.contains(reason),
            "{message}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_a_base_table_without_history() {
    let workspace = authored_feed_workspace("feed-no-history", RECEIPT_FEED_VIEW);
    workspace.edit_manifest(
        "audit_log = {columns = [], retention = \"P30D\"}",
        "audit_log = {columns = [], retention = \"none\"}",
    );
    let message = format!("{:#}", authored_feed_build(&workspace).await.unwrap_err());
    assert!(
        message.contains("widget.list")
            && message.contains("table inventory.widget_tag needs history with retention"),
        "{message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn authored_feed_refuses_a_key_other_than_the_root_id() {
    let workspace = authored_feed_workspace("feed-wrong-key", RECEIPT_FEED_VIEW);
    workspace.edit_manifest(
        "lists = {model = \"widget\", key_field = \"id\", display_field = \"code\"}",
        "lists = {model = \"widget\", key_field = \"code\", display_field = \"code\"}",
    );
    let message = format!("{:#}", authored_feed_build(&workspace).await.unwrap_err());
    assert!(
        message.contains("widget.list")
            && message.contains("key field code must select the root inventory.widget.id"),
        "{message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn projection_view_keeps_non_null_and_nullable_declarations() {
    for nullable in [false, true] {
        let database = wamn_test_postgres::database();
        let workspace = projection_view_workspace(if nullable {
            "view-nullable"
        } else {
            "view-required"
        });
        if nullable {
            workspace.edit_manifest(
                "row = [manifest.uuid(\"id\"), manifest.text(\"code\"), manifest.int32(\"edit_version\"), manifest.Value {name = \"attributes\", type = \"json\", nullable = False}]",
                "row = [manifest.nullable(manifest.uuid(\"id\")), manifest.nullable(manifest.text(\"code\")), manifest.nullable(manifest.int32(\"edit_version\")), manifest.nullable(manifest.json(\"attributes\"))]",
            );
        } else {
            workspace.edit_manifest(
                "row = [manifest.uuid(\"id\"), manifest.text(\"code\"), manifest.int32(\"edit_version\"), manifest.Value {name = \"attributes\", type = \"json\", nullable = False}]",
                "row = [manifest.Value {name = \"id\", type = \"uuid\", nullable = False}, manifest.Value {name = \"code\", type = \"text\", nullable = False}, manifest.Value {name = \"edit_version\", type = \"int32\", nullable = False}, manifest.Value {name = \"attributes\", type = \"json\", nullable = False}]",
            );
        }
        let output_root = workspace.root.join("target/wamn");
        generate_package(&BuildRequest::new(
            &workspace.package(),
            &output_root,
            database.url(),
        ))
        .await
        .expect("generate the projection over a migrated view");
        let output = package_output(&output_root, wamn_fixture_package::PACKAGE_ID);
        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join("wamn.json")).unwrap()).unwrap();
        let projection = &manifest["custom_operations"]["widget.list"];
        assert_eq!(projection["statements"]["list"]["row"][1]["type"], "text");
        for field in projection["statements"]["list"]["row"].as_array().unwrap() {
            assert_eq!(field["nullable"], nullable, "{field}");
        }
        assert_eq!(projection["relations"][0]["table"], "widget_screen");
        assert_eq!(
            projection["relations"][0]["select_fields"],
            serde_json::json!(["attributes", "code", "edit_version", "id"])
        );
        let rows = fs::read_to_string(output.join("wamn/widget_list.rs")).unwrap();
        assert!(
            rows.contains(if nullable {
                "pub code: Option<String>,"
            } else {
                "pub code: String,"
            }),
            "{rows}"
        );
        let map: Value =
            serde_json::from_slice(&fs::read(output.join("source-map/widget_list.json")).unwrap())
                .unwrap();
        assert_eq!(
            map["wamn_accessors"][0]["sqlx_view_columns"],
            serde_json::json!(["id", "code", "edit_version", "attributes"])
        );
        database.execute(&["CREATE SCHEMA inventory"]).unwrap();
        for migration in ["0001_initial.sql", "0002_counter.sql"] {
            let sql =
                fs::read_to_string(workspace.package().join("migrations").join(migration)).unwrap();
            database.execute(&[&sql]).unwrap();
        }
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let verifier = workspace.root.join("verifier");
        wamn_schema_generator::stage_sqlx_verifier(&workspace.package(), &verifier, repository)
            .expect("stage the view projection verifier");
        let source = fs::read_to_string(verifier.join("src/lib.rs")).unwrap();
        let source = source
            .lines()
            .filter(|line| {
                !line.trim_start().starts_with("let _ =") || line.contains("native::widget_list::")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            source.contains("native::widget_list::ListRowSqlx"),
            "{source}"
        );
        fs::write(verifier.join("src/lib.rs"), &source).unwrap();
        let manifest =
            PackageManifest::from_slice(&fs::read(output.join("wamn.json")).unwrap()).unwrap();
        let url = wamn_schema_generator::package_database_url(database.url(), &manifest).unwrap();
        let compile = || {
            std::process::Command::new("cargo")
                .current_dir(&verifier)
                .args(["check", "--lib", "--locked", "--offline"])
                .env("DATABASE_URL", &url)
                .env("SQLX_OFFLINE", "false")
                .output()
                .expect("compile the staged view projection macro")
        };
        fs::write(
            verifier.join("src/lib.rs"),
            source.replace("::ListRowSqlx", "::ListRow"),
        )
        .unwrap();
        let original = compile();
        assert_eq!(
            original.status.success(),
            nullable,
            "original SQLx carrier: {}",
            String::from_utf8_lossy(&original.stderr)
        );
        fs::write(verifier.join("src/lib.rs"), source).unwrap();
        let result = compile();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let workspace = projection_view_workspace("view-undeclared");
    let database = wamn_test_postgres::database();
    let output_root = workspace.root.join("target/wamn");
    generate_package(&BuildRequest::new(
        &workspace.package(),
        &output_root,
        database.url(),
    ))
    .await
    .expect("generate an undeclared view value as nullable");
    let manifest: Value = serde_json::from_slice(
        &fs::read(package_output(&output_root, wamn_fixture_package::PACKAGE_ID).join("wamn.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        statement(&manifest, "widget.list", "list")["row"][1]["nullable"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
async fn projection_view_refuses_a_wrong_declared_type_and_names_the_field() {
    let workspace = projection_view_workspace("view-wrong-type");
    workspace.edit_manifest(
        "manifest.text(\"code\"), manifest.int32(\"edit_version\")",
        "manifest.int32(\"code\"), manifest.int32(\"edit_version\")",
    );
    let database = wamn_test_postgres::database();
    let error = generate_package(&BuildRequest::new(
        &workspace.package(),
        &workspace.root.join("target/wamn"),
        database.url(),
    ))
    .await
    .expect_err("refuse a projection whose declared type differs from its view column");
    assert!(
        format!("{error:#}").contains("code is declared int32, and the statement gives text"),
        "{error:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn projection_view_does_not_hide_nullable_table_columns() {
    let workspace = projection_view_workspace("view-with-table");
    workspace.edit_manifest(
        "manifest.text(\"code\"), manifest.int32(\"edit_version\")",
        "manifest.Value {name = \"code\", type = \"text\", nullable = False}, manifest.int32(\"edit_version\")",
    );
    fs::write(workspace.package().join("query/widget_list.sql"),
        "SELECT screen.id, widget.note AS code, screen.edit_version, screen.attributes FROM widget_screen AS screen JOIN widget ON widget.id = screen.id ORDER BY screen.id;\n").unwrap();
    let database = wamn_test_postgres::database();
    let error = generate_package(&BuildRequest::new(
        &workspace.package(),
        &workspace.root.join("target/wamn"),
        database.url(),
    ))
    .await
    .expect_err("a view read must keep the nullable table-column refusal");
    assert!(
        format!("{error:#}").contains("row value code is declared not null"),
        "{error:#}"
    );
}

/// A changed base component must invalidate its pin even when its manifest
/// and the overlay's inputs still match the current build.
#[tokio::test(flavor = "current_thread")]
async fn a_stale_base_component_pin_refuses_fresh_and_current_builds() {
    let workspace = Workspace::new("stale-base-pin");
    fs::write(workspace.root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let database = wamn_test_postgres::database();
    let output_root = workspace.root.join("target/wamn");
    let base_request = BuildRequest::new(&workspace.package(), &output_root, database.url());
    let base = generate_package(&base_request)
        .await
        .expect("generate the base");
    let base_output = package_output(&output_root, &base.package.id);

    // Supply component output without invoking the guest compiler. This test
    // exercises build admission before compilation or composition starts.
    let record_component = |output: &Path, bytes: &[u8]| {
        let manifest =
            PackageManifest::from_slice(&fs::read(output.join("wamn.json")).unwrap()).unwrap();
        let name = manifest.components.keys().next().unwrap().clone();
        let path = format!("components/{name}.wasm");
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        fs::create_dir_all(output.join("components")).unwrap();
        fs::write(output.join(&path), bytes).unwrap();
        let mut built = receipt::read(output).unwrap();
        built.outputs.components = vec![receipt::ReceiptComponent {
            name,
            path,
            sha256: digest.clone(),
        }];
        fs::write(output.join(receipt::RECEIPT_FILE), built.to_bytes()).unwrap();
        digest
    };
    let pin = record_component(&base_output, b"\0asm\x0d\0\x01\0");
    let overlay = workspace.overlay();
    let manifest_path = overlay.join("wamn.k");
    let manifest =
        PackageManifest::from_slice(&wamn_schema_generator::compile_manifest(&overlay).unwrap())
            .unwrap();
    let source = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        source.replace(&manifest.base_dependencies["base_fixture"].digest, &pin),
    )
    .unwrap();
    let request = BuildRequest::new(&overlay, &output_root, database.url());
    let built = generate_package(&request)
        .await
        .expect("generate the overlay");
    let output = package_output(&output_root, &built.package.id);
    record_component(&output, b"\0asm\x0d\0\x01\0");
    // A current build must check its base without calling Cargo.
    build_package(&request)
        .await
        .expect("accept the matching base pin");

    let built_digest = record_component(&base_output, b"\0asm\x0d\0\x01\0\0\x02\x01x");
    assert_eq!(
        receipt::read(&base_output).unwrap().outputs.manifest,
        base.outputs.manifest
    );
    let before = snapshot(&output);
    let current_error = build_package(&request)
        .await
        .expect_err("a current build accepted a stale base pin");
    assert_eq!(
        before,
        snapshot(&output),
        "a refused current build changed its output"
    );
    fs::write(overlay.join("input.txt"), "changed input").unwrap();
    let fresh_error = build_package(&request)
        .await
        .expect_err("a fresh build accepted a stale base pin");
    for error in [current_error, fresh_error] {
        let refusal = error
            .downcast_ref::<GenerateError>()
            .expect("typed refusal");
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidComponent);
        assert_eq!(refusal.object(), Some(base.package.id.as_str()));
        let message = error.to_string();
        for expected in [&built.package.id, &base.package.id, &pin, &built_digest] {
            assert!(message.contains(expected), "{message} lacks {expected}");
        }
    }
}

/// Step 7 composes an overlay whose `operations` object names a base
/// dependency: Acme over Receiving. The composed component embeds the base
/// component's exact bytes, which push-package admission requires of a
/// declared dependency. It builds into a copy of the build output of
/// `wamn build apps/wamn_receiving apps/client_acme_receiving`, so run that
/// first.
#[tokio::test(flavor = "current_thread")]
async fn an_overlay_is_composed_with_the_bytes_of_its_base() {
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let built = apps.join("target/wamn");
    let output_root =
        std::env::temp_dir().join(format!("wamn-build-live-{}-compose", std::process::id()));
    let _ = fs::remove_dir_all(&output_root);
    for package in ["wamn_receiving", "client_acme_receiving"] {
        let source = package_output(&built, package);
        receipt::read_current(&apps.join(package), &source)
            .expect("read the current package build");
        for (path, bytes) in snapshot(&source) {
            let target = package_output(&output_root, package).join(path);
            fs::create_dir_all(target.parent().unwrap()).expect("create an output directory");
            fs::write(target, bytes).expect("copy an output file");
        }
    }
    let request = BuildRequest::new(&apps.join("client_acme_receiving"), &output_root, "");

    let receipt = build_components(&request, "client_acme_receiving")
        .await
        .expect("compose Acme over Receiving");

    let component = |package: &str, name: &str| {
        fs::read(package_output(&output_root, package).join(format!("components/{name}.wasm")))
            .expect("read a built component")
    };
    let base = component("wamn_receiving", "receiving");
    let composed = component("client_acme_receiving", "client_acme_receiving");
    assert!(composed.len() > base.len());
    assert!(
        composed.windows(base.len()).any(|window| window == base),
        "the composed component embeds the base bytes"
    );
    assert!(
        receipt
            .outputs
            .components
            .iter()
            .any(|entry| entry.name == "client_acme_receiving"),
        "build.json records the composed component"
    );
    fs::remove_dir_all(&output_root).expect("remove the output copy");
}

/// Nested bases retain their generated identity and supply overlay migrations.
#[tokio::test(flavor = "current_thread")]
async fn nested_base_paths_preserve_outputs_and_supply_migrations() {
    let workspace = Workspace::new("nested-base");
    let database = wamn_test_postgres::database();
    let output = workspace.root.join("target/wamn");
    let first = wamn_schema_generator::build::generate_package(&BuildRequest::new(
        &workspace.package(),
        &output,
        database.url(),
    ))
    .await
    .unwrap();
    fs::create_dir(workspace.root.join("platform")).unwrap();
    let nested = workspace
        .root
        .join("platform")
        .join(wamn_fixture_package::PACKAGE_ID);
    fs::rename(workspace.package(), &nested).unwrap();
    let second = wamn_schema_generator::build::generate_package(&BuildRequest::new(
        &nested,
        &output,
        database.url(),
    ))
    .await
    .unwrap();
    assert_eq!(first.inputs, second.inputs);
    assert_eq!(first.outputs.manifest, second.outputs.manifest);
    assert_eq!(first.outputs.files, second.outputs.files);
    wamn_schema_generator::build::generate_package(&BuildRequest::new(
        &workspace.overlay(),
        &output,
        database.url(),
    ))
    .await
    .expect("the top-level overlay migrates its nested base");
}

/// Owner declarations survive compilation at a caller-selected output root.
#[tokio::test(flavor = "current_thread")]
async fn overlay_builds_inherit_owner_fields_from_receipted_manifests() {
    use wamn_schema_generator::build::generate_package;

    let workspace = Workspace::new("inherited-fields");
    workspace.edit_manifest(
        "audit_log = {columns = [\"created_at\"], retention = \"P30D\"}\n            # A hard delete:",
        "fields = {updated_by = {role = \"actor\"}}\n            audit_log = {columns = [\"created_at\", \"updated_at\", \"updated_by\"], retention = \"P30D\"}\n            # A hard delete:",
    );
    fs::write(
        workspace.package().join("migrations/9999_actor.sql"),
        "ALTER TABLE inventory.widget ADD COLUMN updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP, ADD COLUMN updated_by uuid NOT NULL;",
    )
    .unwrap();
    let database = wamn_test_postgres::database();
    let output = workspace.root.join("selected-output");
    generate_package(&BuildRequest::new(
        &workspace.package(),
        &output,
        database.url(),
    ))
    .await
    .unwrap();
    let request = BuildRequest::new(&workspace.overlay(), &output, database.url());
    generate_package(&request).await.unwrap();
    let manifest: Value = serde_json::from_slice(
        &fs::read(
            package_output(&output, wamn_fixture_package::OVERLAY_PACKAGE_ID).join("wamn.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["models"]["widget"]["fields"]["updated_by"]["role"],
        "actor"
    );

    let base_path = package_output(&output, wamn_fixture_package::PACKAGE_ID).join("wamn.json");
    let mut changed = fs::read(&base_path).unwrap();
    changed.push(b' ');
    fs::write(base_path, changed).unwrap();
    let error = generate_package(&request).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("manifest differs from its build receipt"),
        "{error:#}"
    );
}

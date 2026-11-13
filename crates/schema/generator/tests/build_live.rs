//! Live PostgreSQL 18 tests of `wamn build` (docs/operations/building.md#wamn-build).
//!
//! Each test copies the platform fixture, without its committed `generated/`,
//! into a directory of its own and builds it against the server of the test
//! process. The build creates and drops its own verification database there.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_compiler::build::{BuildRequest, generate_package, package_output};
use wamn_compiler::build_record;
use wamn_compiler::manifest::PackageManifest;

#[tokio::test(flavor = "current_thread")]
async fn concurrent_builds_initialize_history_on_one_server() -> anyhow::Result<()> {
    use std::time::Duration;

    let mut server = wamn_test_postgres::start(&[])?;
    let first = server.create_database("first")?;
    let second = server.create_database("second")?;
    let (admin, connection) = tokio_postgres::connect(first.url(), tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    admin.batch_execute("CREATE ROLE wamn_app NOLOGIN").await?;
    // Hold both creators after their absence tests, before either role insert.
    admin
        .batch_execute("BEGIN; LOCK TABLE pg_catalog.pg_authid IN SHARE MODE")
        .await?;
    let first_workspace = Workspace::new("concurrent-first");
    let second_workspace = Workspace::new("concurrent-second");
    let first_package = first_workspace.package();
    let second_package = second_workspace.package();
    let first_output = first_workspace.root.join("target/wamn");
    let second_output = second_workspace.root.join("target/wamn");
    let first_request = BuildRequest::new(&first_package, &first_output, first.url());
    let second_request = BuildRequest::new(&second_package, &second_output, second.url());
    let unlock = async {
        let waiting = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let waiting: i64 = admin
                    .query_one(
                        "SELECT count(*) FROM pg_locks WHERE relation='pg_catalog.pg_authid'::regclass \
                         AND mode='RowExclusiveLock' AND NOT granted",
                        &[],
                    )
                    .await?
                    .get(0);
                if waiting == 2 {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        admin.batch_execute("COMMIT").await?;
        waiting??;
        Ok::<_, anyhow::Error>(())
    };
    let (first_result, second_result, unlocked) = tokio::join!(
        generate_package(&first_request),
        generate_package(&second_request),
        unlock,
    );
    unlocked?;
    first_result?;
    second_result?;
    drop(admin);
    connection.await??;
    Ok(())
}

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
async fn build(package: &Path, output_root: &Path, database: &str) -> anyhow::Result<Value> {
    generate_package(&BuildRequest::new(package, output_root, database)).await?;
    let path = package_output(output_root, wamn_fixture_package::PACKAGE_ID).join("wamn.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn statement<'a>(manifest: &'a Value, operation: &str, name: &str) -> &'a Value {
    &manifest["custom_operations"][operation]["statements"][name]
}

/// R10(2) and R10(3): the copy holds no generated verifier, so pass one
/// describes without one; two builds give the same bytes and build records; the
/// manifest is canonical and hashed once; an overlay records its base.
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
    fs::write(workspace.package().join("migrations/0002_feed.sql"), format!(
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
    let original = r#"widget_tag = manifest.Model("widget_tag", _schema) {
            table = "widget_tag"
            owner = "platform_fixture"
            server_owned_fields = ["edit_version"]
            audit_log = {columns = [], retention = "P30D"}"#;
    workspace.edit_manifest(original, &original.replace("P30D", "none"));
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

/// Owner declarations survive compilation at a caller-selected output root.
#[tokio::test(flavor = "current_thread")]
async fn overlay_builds_inherit_owner_fields_from_manifests_with_build_records() {
    use wamn_compiler::build::generate_package;

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
    let base = generate_package(&BuildRequest::new(
        &workspace.package(),
        &output,
        database.url(),
    ))
    .await
    .unwrap();
    fs::write(
        package_output(&output, &base.package.id).join(build_record::BUILD_RECORD_FILE),
        base.to_bytes(),
    )
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
    generate_package(&request)
        .await
        .expect_err("changed compiled base bytes must refuse");
}

/// PostgreSQL describes the accessor's binds at the build interface.
#[tokio::test]
async fn a_statement_takes_exactly_the_parameters_its_accessor_binds() {
    let database = wamn_test_postgres::database();
    for (label, path, sql) in [
        (
            "list-binds",
            "query/widget_list.sql",
            "SELECT id, code, edit_version, to_jsonb(widget) AS attributes FROM widget WHERE id = $1 ORDER BY id",
        ),
        (
            "archive-binds",
            "command/widget/archive.sql",
            "SELECT id, edit_version, note FROM widget WHERE id = $1 AND edit_version = $2 FOR UPDATE",
        ),
    ] {
        let workspace = Workspace::new(label);
        fs::write(workspace.package().join(path), sql).unwrap();
        let request = BuildRequest::new(
            &workspace.package(),
            &workspace.root.join("output"),
            database.url(),
        );
        assert!(generate_package(&request).await.is_err());
    }
}

//! Live PostgreSQL 18 tests of `wamn build` (docs/operations/building.md#wamn-build).
//!
//! Each test copies the platform fixture, without its committed `generated/`,
//! into a directory of its own and builds it against a server of its own. The
//! build creates and drops its own verification database there.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sqlx::Row as _;
use wamn_codegen::build::{BuildRequest, generate_package, package_output};
use wamn_postgres::build_database::PostgresBuildDatabase;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker"]
async fn concurrent_builds_initialize_history_on_one_server() -> anyhow::Result<()> {
    use std::time::Duration;

    let server = wamn_test_infrastructure::containers::start_postgres().await?;
    let first = server.create_database("first").await?;
    let second = server.create_database("second").await?;
    let mut admin = wamn_postgres::connect(first.as_str()).await?;

    // Hold both creators after their absence tests, before either role insert.
    sqlx::raw_sql("BEGIN; LOCK TABLE pg_catalog.pg_authid IN SHARE MODE")
        .execute(&mut admin)
        .await
        .map(drop)?;
    let first_workspace = Workspace::new("concurrent-first");
    let second_workspace = Workspace::new("concurrent-second");
    let first_package = first_workspace.package();
    let second_package = second_workspace.package();
    let first_output = first_workspace.root.join("target/wamn");
    let second_output = second_workspace.root.join("target/wamn");
    let first_request = BuildRequest::refresh(&first_package, &first_output, first.as_str());
    let second_request = BuildRequest::refresh(&second_package, &second_output, second.as_str());
    let unlock = async {
        let waiting = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let waiting: i64 = sqlx::query(
                    "SELECT count(*) FROM pg_locks WHERE relation='pg_catalog.pg_authid'::regclass \
                         AND mode='RowExclusiveLock' AND NOT granted",
                )
                .fetch_one(&mut admin)
                .await?
                .get(0);
                if waiting == 2 {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        sqlx::raw_sql("COMMIT")
            .execute(&mut admin)
            .await
            .map(drop)?;
        waiting??;
        Ok::<_, anyhow::Error>(())
    };
    let (first_result, second_result, unlocked) = tokio::join!(
        generate_package(&PostgresBuildDatabase, &first_request),
        generate_package(&PostgresBuildDatabase, &second_request),
        unlock,
    );
    unlocked?;
    first_result?;
    second_result?;
    drop(admin);
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
    generate_package(
        &PostgresBuildDatabase,
        &BuildRequest::refresh(package, output_root, database),
    )
    .await?;
    let path = package_output(
        output_root,
        &wamn_fixture_package::PACKAGE_ID
            .parse()
            .expect("valid fixture name"),
    )
    .join("wamn.json");
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
#[tokio::test]
#[ignore = "requires: docker"]
async fn statement_types_are_derived_and_nullability_only_widens() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();

    let accepted = Workspace::new("accepted");
    let archive_sql = accepted
        .package()
        .join("command/widget/archive/archive.sql");
    let sql = fs::read_to_string(&archive_sql).unwrap();
    fs::write(
        &archive_sql,
        sql.replace(
            "SELECT id, edit_version, note",
            r#"SELECT id, edit_version AS "edit_version?", note"#,
        ),
    )
    .unwrap();
    let manifest = build(
        &accepted.package(),
        &accepted.root.join("target"),
        database.as_str(),
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
    let archive_sql = refused.package().join("command/widget/archive/archive.sql");
    let sql = fs::read_to_string(&archive_sql).unwrap();
    fs::write(
        &archive_sql,
        sql.replace(
            "SELECT id, edit_version, note",
            r#"SELECT id, edit_version, note AS "note!""#,
        ),
    )
    .unwrap();
    let _error = build(
        &refused.package(),
        &refused.root.join("target"),
        database.as_str(),
    )
    .await
    .expect_err("a not-null declaration on a nullable column was accepted");
}

/// The columns of the projection view, with the expression that computes each.
const PROJECTION_VIEW: [(&str, &str); 4] = [
    ("id", "id"),
    ("code", "code"),
    ("edit_version", "edit_version"),
    ("attributes", "to_jsonb(widget)"),
];

fn projection_view_workspace(name: &str) -> Workspace {
    let workspace = Workspace::new(name);
    let migration = workspace.package().join("migrations/0001_initial.sql");
    let sql = fs::read_to_string(&migration).unwrap();
    let columns = PROJECTION_VIEW
        .iter()
        .map(|(column, expression)| format!("{expression} AS {column}"))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        migration,
        format!(
            "{sql}\nCREATE VIEW inventory.widget_screen AS SELECT {columns} FROM inventory.widget;\n"
        ),
    )
    .unwrap();
    let selected = PROJECTION_VIEW
        .iter()
        .map(|(column, _)| format!(r#"{column} AS "{column}!""#))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        workspace.package().join("query/widget/list/list.sql"),
        format!("SELECT {selected} FROM widget_screen ORDER BY id;"),
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
         CREATE VIEW inventory.widget_screen AS {definition};\n"
    )).unwrap();
    fs::write(
        workspace.package().join("query/widget/list/list.sql"),
        r#"SELECT id AS "id!", code AS "code!", edit_version AS "edit_version!", attributes AS "attributes!" FROM widget_screen ORDER BY id;"#,
    )
    .unwrap();
    workspace
}

const RECEIPT_FEED_VIEW: &str = "SELECT line.id, line.code, line.edit_version,
    to_jsonb(line) AS attributes, line.maker_id, line.tag_id
    FROM inventory.widget AS line
    JOIN inventory.widget_maker AS orders ON orders.id = line.maker_id
    JOIN inventory.widget_tag AS items ON items.id = line.tag_id";

async fn authored_feed_build(database: &url::Url, workspace: &Workspace) -> anyhow::Result<()> {
    let output_root = workspace.root.join("target/wamn");
    generate_package(
        &PostgresBuildDatabase,
        &BuildRequest::refresh(&workspace.package(), &output_root, database.as_str()),
    )
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_a_one_to_many_view_without_grouping() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    let workspace = authored_feed_workspace("feed-many", "SELECT orders.id, orders.name AS code,
        orders.edit_version::int4 AS edit_version, to_jsonb(orders) AS attributes
        FROM inventory.widget_maker AS orders JOIN inventory.widget AS line ON line.maker_id = orders.id");
    workspace.edit_manifest(
        "lists = {\"model\" = \"widget\", \"key_field\" = [\"id\"], \"display_field\" = \"code\"}",
        "lists = {model = \"widget_maker\", key_field = \"id\", display_field = \"code\"}",
    );
    authored_feed_build(&database, &workspace)
        .await
        .unwrap_err();
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_a_base_table_without_a_link() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    let workspace = authored_feed_workspace("feed-no-link", "SELECT line.id, line.code, line.edit_version,
        to_jsonb(items) AS attributes FROM inventory.widget AS line CROSS JOIN inventory.widget_tag AS items");
    authored_feed_build(&database, &workspace)
        .await
        .unwrap_err();
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_statement_tables_and_joins() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
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
        fs::write(workspace.package().join("query/widget/list/list.sql"), sql).unwrap();
        authored_feed_build(&database, &workspace)
            .await
            .unwrap_err();
    }
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_statement_grouping_windows_and_limits() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    for (name, sql) in [
        (
            "group",
            "SELECT id, code, edit_version, attributes FROM widget_screen GROUP BY id, code, edit_version, attributes",
        ),
        (
            "limit",
            "SELECT id, code, edit_version, attributes FROM widget_screen LIMIT 5",
        ),
        (
            "window",
            "SELECT id, code, edit_version, to_jsonb(row_number() OVER ()) AS attributes FROM widget_screen",
        ),
    ] {
        let workspace =
            authored_feed_workspace(&format!("feed-statement-{name}"), RECEIPT_FEED_VIEW);
        fs::write(workspace.package().join("query/widget/list/list.sql"), sql).unwrap();
        authored_feed_build(&database, &workspace)
            .await
            .unwrap_err();
    }
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_a_base_table_without_history() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    let workspace = authored_feed_workspace("feed-no-history", RECEIPT_FEED_VIEW);
    let original = r#"widget_tag = manifest.Model("widget_tag", _schema) {
            table = "widget_tag"
            owner = "platform_fixture"
            server_owned_fields = ["edit_version"]
            audit_log = {columns = [], retention = "P30D"}"#;
    workspace.edit_manifest(original, &original.replace("P30D", "none"));
    authored_feed_build(&database, &workspace)
        .await
        .unwrap_err();
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn authored_feed_refuses_a_key_other_than_the_root_id() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    let workspace = authored_feed_workspace("feed-wrong-key", RECEIPT_FEED_VIEW);
    workspace.edit_manifest(
        "lists = {\"model\" = \"widget\", \"key_field\" = [\"id\"], \"display_field\" = \"code\"}",
        "lists = {\"model\" = \"widget\", \"key_field\" = [\"code\"], \"display_field\" = \"code\"}",
    );
    authored_feed_build(&database, &workspace)
        .await
        .unwrap_err();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker"]
async fn projection_view_keeps_non_null_and_nullable_declarations() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    for nullable in [false, true] {
        let database = server
            .create_database(if nullable { "nullable" } else { "required" })
            .await
            .unwrap();
        let workspace = projection_view_workspace(if nullable {
            "view-nullable"
        } else {
            "view-required"
        });
        if nullable {
            let path = workspace.package().join("query/widget/list/list.sql");
            let sql = fs::read_to_string(&path).unwrap();
            fs::write(&path, sql.replace('!', "?")).unwrap();
        }
        let output_root = workspace.root.join("target/wamn");
        generate_package(
            &PostgresBuildDatabase,
            &BuildRequest::refresh(&workspace.package(), &output_root, database.as_str()),
        )
        .await
        .expect("generate the projection over a migrated view");
        let output = package_output(
            &output_root,
            &wamn_fixture_package::PACKAGE_ID
                .parse()
                .expect("valid fixture name"),
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join("wamn.json")).unwrap()).unwrap();
        let projection = &manifest["custom_operations"]["widget.list"];
        assert_eq!(projection["statements"]["list"]["row"][1]["type"], "text");
        for field in projection["statements"]["list"]["row"].as_array().unwrap() {
            assert_eq!(field["nullable"], nullable, "{field}");
        }
        assert_eq!(projection["relations"][0]["table"], "widget_screen");
        let mut columns = PROJECTION_VIEW.map(|(column, _)| column);
        columns.sort_unstable();
        assert_eq!(
            projection["relations"][0]["select_fields"],
            serde_json::json!(columns)
        );
    }
}

/// Owner declarations survive compilation at a caller-selected output root.
#[tokio::test]
#[ignore = "requires: docker"]
async fn overlay_builds_inherit_owner_fields_from_compiled_manifests() {
    use wamn_codegen::build::generate_package;
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();

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
    let output = workspace.root.join("selected-output");
    generate_package(
        &PostgresBuildDatabase,
        &BuildRequest::refresh(&workspace.package(), &output, database.as_str()),
    )
    .await
    .unwrap();
    let request = BuildRequest::refresh(&workspace.overlay(), &output, database.as_str());
    generate_package(&PostgresBuildDatabase, &request)
        .await
        .unwrap();
    let manifest: Value = serde_json::from_slice(
        &fs::read(
            package_output(
                &output,
                &wamn_fixture_package::OVERLAY_PACKAGE_ID
                    .parse()
                    .expect("valid fixture name"),
            )
            .join("wamn.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["models"]["widget"]["fields"]["updated_by"]["role"],
        "actor"
    );
}

/// PostgreSQL describes the accessor's binds at the build interface.
#[tokio::test]
#[ignore = "requires: docker"]
async fn a_statement_takes_exactly_the_parameters_its_accessor_binds() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = server.create_database("tests").await.unwrap();
    for (label, path, sql) in [
        (
            "list-binds",
            "query/widget/list/list.sql",
            "SELECT id, code, edit_version, to_jsonb(widget) AS attributes FROM widget WHERE id = $1 ORDER BY id",
        ),
        (
            "archive-binds",
            "command/widget/archive/archive.sql",
            "SELECT id, edit_version, note FROM widget WHERE id = $1 AND edit_version = $2 FOR UPDATE",
        ),
    ] {
        let workspace = Workspace::new(label);
        fs::write(workspace.package().join(path), sql).unwrap();
        let request = BuildRequest::refresh(
            &workspace.package(),
            &workspace.root.join("output"),
            database.as_str(),
        );
        assert!(
            generate_package(&PostgresBuildDatabase, &request)
                .await
                .is_err()
        );
    }
}

/// A declaration-only change appends a history migration without changing released checksums.
#[tokio::test]
#[ignore = "requires: docker"]
async fn history_changes_append_immutable_versions_and_execute_during_build() -> anyhow::Result<()>
{
    let server = wamn_test_infrastructure::containers::start_postgres().await?;
    let database = server.create_database("tests").await?;
    let workspace = Workspace::new("history-versions");
    let package = workspace.package();
    let output = workspace.root.join("target/wamn");
    let request = BuildRequest::refresh(&package, &output, database.as_str());
    generate_package(&PostgresBuildDatabase, &request).await?;
    let directory = package_output(
        &output,
        &wamn_fixture_package::PACKAGE_ID
            .parse()
            .expect("valid fixture name"),
    );
    let original = sqlx::migrate::Migrator::new(directory.join("history")).await?;
    generate_package(&PostgresBuildDatabase, &request).await?;
    let repeated = sqlx::migrate::Migrator::new(directory.join("history")).await?;
    assert!(
        original
            .iter()
            .map(|migration| (&migration.version, &migration.checksum))
            .eq(repeated
                .iter()
                .map(|migration| (&migration.version, &migration.checksum)))
    );
    let previous = original.iter().last().unwrap().version;
    let duplicate = package.join(format!("migrations/{previous:04}_duplicate.sql"));
    fs::write(&duplicate, "SELECT 1;")?;
    assert!(
        generate_package(&PostgresBuildDatabase, &request)
            .await
            .is_err()
    );
    fs::remove_file(duplicate)?;

    let authored_version = previous + 2;
    fs::write(
        package.join(format!("migrations/{authored_version:04}_append.sql")),
        "SELECT 1;",
    )?;
    generate_package(&PostgresBuildDatabase, &request).await?;
    fs::remove_dir_all(&output)?;
    let inserted = package.join(format!("migrations/{:04}_insert.sql", previous + 1));
    fs::write(&inserted, "SELECT 1;")?;
    assert!(
        generate_package(&PostgresBuildDatabase, &request)
            .await
            .is_err()
    );
    fs::remove_file(inserted)?;

    let highest = wamn_postgres::build_database::migrator(&package)
        .await?
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap();

    let path = package.join("wamn.k");
    let source = fs::read_to_string(&path)?;
    fs::write(
        path,
        source.replacen("retention = \"P30D\"", "retention = \"unlimited\"", 1),
    )?;
    generate_package(&PostgresBuildDatabase, &request).await?;
    let changed = sqlx::migrate::Migrator::new(directory.join("history")).await?;
    for released in original.iter() {
        assert!(
            changed
                .iter()
                .any(|migration| migration.version == released.version
                    && migration.checksum == released.checksum)
        );
    }
    assert_eq!(changed.iter().last().unwrap().version, highest + 1);
    Ok(())
}

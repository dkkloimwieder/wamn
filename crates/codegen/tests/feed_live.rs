use std::fs;

use sqlx::Row as _;
use wamn_codegen::build::{BuildRequest, generate_package, package_output};
use wamn_test_infrastructure::containers::Postgres;

async fn install_feed(
    server: &Postgres,
    database_name: &str,
    tables: &[&str],
    key_type: &str,
    migration: &str,
) -> (sqlx::PgConnection, String) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    let output = directory.path().join("output");
    fs::create_dir_all(root.join("migrations")).unwrap();
    fs::create_dir_all(root.join("query/feed/load")).unwrap();
    fs::write(root.join("migrations/0001_initial.sql"), migration).unwrap();
    fs::write(
        root.join("query/feed/load/load.sql"),
        r#"SELECT id AS "id!" FROM screen ORDER BY id;"#,
    )
    .unwrap();

    let models = tables
        .iter()
        .map(|name| {
            format!(
                r#"
        {name} = manifest.Model("{name}", "app") {{
            table = "{name}"
            owner = "fixture"
            audit_log = {{columns = [], retention = "P30D"}}
            operations = {{}}
        }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        root.join("wamn.k"),
        format!(
            r#"import manifest

manifest.Package {{
    package = {{id = "fixture", version = "1.0.0"}}
    required_platform_policy_contract = {{id = "fixture", state = "satisfied"}}
    models = {{{models}
    }}
    components = {{fixture = {{}}}}
    custom_operations = {{
        "feed.load" = manifest.Projection("feed.load") {{
            feed = True
            lists = {{model = "line", key_field = "id", display_field = "id"}}
            statements = {{load = {{fetch = "bounded_list", binds = []}}}}
        }}
    }}
}}"#
        ),
    )
    .unwrap();

    let wit = root.join("component/wit/deps/fixture-feed");
    fs::create_dir_all(&wit).unwrap();
    fs::write(
        wit.join("custom.wit"),
        "interface load {
    use wamn:node/types@0.2.0.{node-context, node-error};
    type uuid = string;

    record load-feed { cursor: option<string>, limit: option<u32> }
    record load-request { feed: option<load-feed> }
    record invalid-input-detail { field: string }
    record permission-denied-detail { operation: string }
    variant load-error {
        cursor-expired, internal-error, invalid-input(invalid-input-detail),
        permission-denied(permission-denied-detail), retry, timeout,
    }
    record load-item { input: result<load-request, invalid-input-detail> }
    record load-row { id: KEY_TYPE, row-version: s64 }
    record load-page { item: list<load-row>, next-cursor: option<string>, cursor: string }
    record load-change { id: string, row-version: s64, data: option<load-row> }
    record load-changes { rows: list<load-change>, next-cursor: string, complete: bool }
    variant load-result { page(load-page), changes(load-changes) }
    record load-outcome { outcome: result<load-result, load-error> }

    run: async func(ctx: node-context, input: list<load-item>) -> result<list<load-outcome>, node-error>;
}".replace("KEY_TYPE", key_type),
    ).unwrap();

    let manifest = generate_package(&BuildRequest::refresh(&root, &output, server.admin_url()))
        .await
        .unwrap();
    let built = package_output(&output, &manifest.package.id);
    let sql = fs::read_to_string(built.join("sql/feed_load/feed_changes.sql")).unwrap();

    let database = server.create_database(database_name).await.unwrap();
    wamn_codegen::build_database::migrate(database.as_str(), &["app".into()], &[&built], &[])
        .await
        .unwrap();
    let connection = wamn_postgres::connect(database.as_str()).await.unwrap();
    (connection, sql)
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn affected_keys_follow_each_link_and_both_images() {
    let server = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let (mut connection, sql) = install_feed(
        &server,
        "linked_items",
        &["line", "customer_order", "item"],
        "uuid",
        "CREATE TABLE app.item (id uuid CONSTRAINT item_id_pkey PRIMARY KEY);
         CREATE TABLE app.customer_order (id uuid CONSTRAINT customer_order_id_pkey PRIMARY KEY,
             item_id uuid CONSTRAINT customer_order_item_id_fkey REFERENCES app.item(id));
         CREATE TABLE app.line (id uuid CONSTRAINT line_id_pkey PRIMARY KEY,
             customer_order_id uuid CONSTRAINT line_customer_order_id_fkey REFERENCES app.customer_order(id));
         CREATE VIEW app.screen AS SELECT line.id, line.customer_order_id, customer_order.item_id
             FROM app.line JOIN app.customer_order ON customer_order.id = line.customer_order_id
             JOIN app.item ON item.id = customer_order.item_id;",
    )
    .await;
    // Leave the root rows intact so only the deleted relation has a new history entry.
    sqlx::raw_sql("ALTER TABLE customer_order DROP CONSTRAINT customer_order_item_id_fkey")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO item VALUES ('00000000-0000-0000-0000-000000000010'); INSERT INTO customer_order VALUES ('00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000010'); INSERT INTO line VALUES ('00000000-0000-0000-0000-000000000030','00000000-0000-0000-0000-000000000020')")
        .execute(&mut connection).await.unwrap();
    let cursor: i64 = sqlx::query_scalar("SELECT pg_current_xact_id()::text::int8")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("DELETE FROM item")
        .execute(&mut connection)
        .await
        .unwrap();
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(cursor)
        .bind(None::<uuid::Uuid>)
        .bind(100_i64)
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let changed: Vec<uuid::Uuid> = rows.iter().filter_map(|row| row.get("change_id")).collect();
    assert_eq!(changed, [uuid::Uuid::from_u128(48)]);
    drop(connection);

    let (mut connection, sql) = install_feed(
        &server,
        "relinked_orders",
        &["line", "customer_order"],
        "uuid",
        "CREATE TABLE app.line (id uuid CONSTRAINT line_id_pkey PRIMARY KEY);
         CREATE TABLE app.customer_order (id uuid CONSTRAINT customer_order_id_pkey PRIMARY KEY,
             line_id uuid CONSTRAINT customer_order_line_id_fkey REFERENCES app.line(id));
         CREATE VIEW app.screen AS SELECT line.id FROM app.line
             LEFT JOIN app.customer_order ON customer_order.line_id = line.id GROUP BY line.id;",
    )
    .await;
    sqlx::raw_sql("INSERT INTO line VALUES ('00000000-0000-0000-0000-000000000001'),('00000000-0000-0000-0000-000000000002'); INSERT INTO customer_order VALUES ('00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000000001')")
        .execute(&mut connection).await.unwrap();
    let cursor: i64 = sqlx::query_scalar("SELECT pg_current_xact_id()::text::int8")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE customer_order SET line_id='00000000-0000-0000-0000-000000000002'")
        .execute(&mut connection)
        .await
        .unwrap();
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(cursor)
        .bind(None::<uuid::Uuid>)
        .bind(100_i64)
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let changed: Vec<uuid::Uuid> = rows.iter().filter_map(|row| row.get("change_id")).collect();
    assert_eq!(
        changed,
        [uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)]
    );
    drop(connection);

    let (mut connection, sql) = install_feed(
        &server,
        "numeric_link",
        &["line", "customer_order"],
        "s32",
        "CREATE TABLE app.line (id int4 GENERATED BY DEFAULT AS IDENTITY CONSTRAINT line_id_pkey PRIMARY KEY);
         CREATE TABLE app.customer_order (id uuid CONSTRAINT customer_order_id_pkey PRIMARY KEY,
             line_id int8 CONSTRAINT customer_order_line_id_fkey REFERENCES app.line(id));
         CREATE VIEW app.screen AS SELECT line.id FROM app.line
             LEFT JOIN app.customer_order ON customer_order.line_id = line.id GROUP BY line.id;",
    ).await;
    sqlx::raw_sql("INSERT INTO line VALUES (1),(2); INSERT INTO customer_order VALUES ('00000000-0000-0000-0000-000000000010',1)").execute(&mut connection).await.unwrap();
    let cursor: i64 = sqlx::query_scalar("SELECT pg_current_xact_id()::text::int8")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE customer_order SET line_id=2")
        .execute(&mut connection)
        .await
        .unwrap();
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(cursor)
        .bind(None::<i32>)
        .bind(100_i64)
        .fetch_all(&mut connection)
        .await
        .unwrap();
    let changed: Vec<i32> = rows.iter().filter_map(|row| row.get("change_id")).collect();
    assert_eq!(changed, [1, 2]);
}

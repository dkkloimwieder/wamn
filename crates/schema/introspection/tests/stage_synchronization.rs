use std::collections::BTreeMap;

use wamn_schema_introspection::ir::ColumnType;
use wamn_schema_introspection::migration_policy::{StageRelation, inspect_stage_synchronization};

const SQL: &str = "CREATE FUNCTION inventory.mirror_note() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $mirror$BEGIN NEW.next_note := NEW.note; RETURN NEW; END$mirror$;
CREATE TRIGGER mirror_note BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note();";

fn relation() -> StageRelation {
    StageRelation {
        schema: "inventory".into(),
        name: "widget".into(),
        columns: BTreeMap::from([
            ("id".into(), ColumnType::Int32),
            ("note".into(), ColumnType::Text),
            ("next_note".into(), ColumnType::Text),
        ]),
    }
}

#[test]
fn synchronization_declarations_pin_relation_and_invoked_function() {
    let inspected = inspect_stage_synchronization(
        "expand.sql",
        SQL.as_bytes(),
        &relation(),
        &["next_note".into()],
    )
    .unwrap();
    assert_eq!(inspected.schema, "inventory");
    assert_eq!(inspected.relation, "widget");
    assert_eq!(inspected.function, "mirror_note");
    assert_eq!(inspected.trigger, "mirror_note");
    assert_eq!(
        inspected.body,
        "BEGIN NEW.next_note := NEW.note; RETURN NEW; END"
    );
    for (sql, named) in [
        (
            SQL.replace("SECURITY INVOKER", "SECURITY DEFINER"),
            "SECURITY DEFINER",
        ),
        (
            SQL.replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION"),
            "CREATE OR REPLACE FUNCTION",
        ),
        (
            SQL.replace("ON inventory.widget", "ON inventory.foreign_widget"),
            "foreign_widget",
        ),
        (
            SQL.replace(
                "EXECUTE FUNCTION inventory.mirror_note()",
                "EXECUTE FUNCTION inventory.external()",
            ),
            "external",
        ),
        (SQL.replace("BEFORE INSERT", "AFTER INSERT"), "AFTER"),
        (
            SQL.replace("FOR EACH ROW", "FOR EACH STATEMENT"),
            "STATEMENT",
        ),
        (
            SQL.replace("NEW.next_note := NEW.note", "NEW.id := 9"),
            "id",
        ),
        (SQL.replace("NEW.note;", "now();"), "now"),
        (
            format!("{SQL} DROP TABLE inventory.widget;"),
            "one CREATE FUNCTION",
        ),
    ] {
        let error = inspect_stage_synchronization(
            "expand.sql",
            sql.as_bytes(),
            &relation(),
            &["next_note".into()],
        )
        .unwrap_err();
        assert!(error.to_string().contains(named), "{error}");
    }
}

#[tokio::test]
async fn invoker_mirror_retains_rows_and_needs_only_source_column_write_permission() {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("stage_mirror").unwrap();
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection_task = tokio::spawn(connection);
    client.batch_execute("CREATE SCHEMA inventory; CREATE TABLE inventory.widget (id integer PRIMARY KEY, note text, next_note text); INSERT INTO inventory.widget VALUES (1,'retained',NULL); CREATE ROLE mirror_application; GRANT USAGE ON SCHEMA inventory TO mirror_application; GRANT SELECT(id,note), INSERT(id,note), UPDATE(note) ON inventory.widget TO mirror_application;").await.unwrap();
    inspect_stage_synchronization(
        "expand.sql",
        SQL.as_bytes(),
        &relation(),
        &["next_note".into()],
    )
    .unwrap();
    client.batch_execute(SQL).await.unwrap();
    let retained = client
        .query_one(
            "SELECT note,next_note FROM inventory.widget WHERE id=1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(retained.get::<_, String>(0), "retained");
    assert_eq!(retained.get::<_, Option<String>>(1), None);
    client.batch_execute("SET ROLE mirror_application; UPDATE inventory.widget SET note='changed' WHERE id=1; INSERT INTO inventory.widget(id,note) VALUES (2,'new'); RESET ROLE").await.unwrap();
    let rows = client
        .query(
            "SELECT note,next_note FROM inventory.widget ORDER BY id",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.get::<_, String>(0), row.get::<_, String>(1));
    }
    assert!(!client.query_one("SELECT has_column_privilege('mirror_application','inventory.widget','next_note','UPDATE')", &[]).await.unwrap().get::<_, bool>(0));
    client
        .batch_execute("SET ROLE mirror_application")
        .await
        .unwrap();
    let refused = client
        .batch_execute("UPDATE inventory.widget SET next_note='unauthorized' WHERE id=1")
        .await
        .unwrap_err();
    assert_eq!(
        refused.code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    client.batch_execute("RESET ROLE").await.unwrap();
    drop(client);
    connection_task.await.unwrap().unwrap();
}

#[test]
fn synchronization_refuses_platform_trigger_names() {
    for name in [
        wamn_record_history::STAMP_TRIGGER,
        wamn_record_history::LOG_TRIGGER,
        wamn_catalog::VERSION_NOTE_TRIGGER,
        wamn_catalog::VERSION_BUMP_TRIGGER,
    ] {
        let sql = SQL.replace(
            "CREATE TRIGGER mirror_note",
            &format!("CREATE TRIGGER {name}"),
        );
        let error = inspect_stage_synchronization(
            "expand.sql",
            sql.as_bytes(),
            &relation(),
            &["next_note".into()],
        )
        .unwrap_err();
        assert!(error.to_string().contains(name), "{error}");
        assert!(
            error.to_string().contains("reserved for the platform"),
            "{error}"
        );
    }
}

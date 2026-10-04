use std::collections::BTreeMap;

use wamn_schema_introspection::ir::ColumnType;
use wamn_schema_introspection::migration_policy::{StageRelation, inspect_stage_synchronization};
use wamn_schema_introspection::postgres::{
    StageSynchronizationExpectation, read_catalog, read_catalog_with_synchronizations,
    verify_stage_synchronizations,
};

const SQL: &str = "CREATE FUNCTION inventory.mirror_note() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $mirror$BEGIN NEW.next_note := NEW.note; RETURN NEW; END$mirror$; CREATE TRIGGER mirror_note BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note();";

#[tokio::test]
async fn catalog_requires_exact_expected_synchronizations_and_refuses_every_drift() {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("catalog_sync").unwrap();
    let (mut client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    client.batch_execute("CREATE SCHEMA inventory; CREATE TABLE inventory.widget (id integer CONSTRAINT widget_id_pkey PRIMARY KEY, note text, next_note text); INSERT INTO inventory.widget VALUES (1,'retained',NULL); CREATE ROLE sync_wrong_owner;").await.unwrap();
    let baseline = read_catalog(&client, &["inventory"]).await.unwrap();
    let relation = StageRelation {
        schema: "inventory".into(),
        name: "widget".into(),
        columns: BTreeMap::from([
            ("id".into(), ColumnType::Int32),
            ("note".into(), ColumnType::Text),
            ("next_note".into(), ColumnType::Text),
        ]),
    };
    let writable_columns = vec!["next_note".into()];
    let definition =
        inspect_stage_synchronization("expand.sql", SQL.as_bytes(), &relation, &writable_columns)
            .unwrap();
    let owner_role: String = client
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    let expected = vec![StageSynchronizationExpectation {
        definition,
        writable_columns,
        owner_role,
    }];
    client.batch_execute(SQL).await.unwrap();
    assert!(
        read_catalog(&client, &["inventory"])
            .await
            .unwrap_err()
            .to_string()
            .contains("outside the supported catalog set")
    );
    verify_stage_synchronizations(&client, &expected)
        .await
        .unwrap();
    let admitted = read_catalog_with_synchronizations(&client, &["inventory"], &[], &expected)
        .await
        .unwrap();
    assert_eq!(format!("{baseline:?}"), format!("{admitted:?}"));

    for (tamper, named) in [
        (
            "ALTER FUNCTION inventory.mirror_note() SECURITY DEFINER",
            "function attributes",
        ),
        (
            "ALTER FUNCTION inventory.mirror_note() STABLE",
            "function attributes",
        ),
        (
            "ALTER FUNCTION inventory.mirror_note() STRICT",
            "function attributes",
        ),
        (
            "ALTER FUNCTION inventory.mirror_note() SET search_path=pg_catalog",
            "function attributes",
        ),
        (
            "ALTER FUNCTION inventory.mirror_note() COST 1",
            "function attributes",
        ),
        (
            "REVOKE EXECUTE ON FUNCTION inventory.mirror_note() FROM PUBLIC",
            "function attributes",
        ),
        (
            "ALTER FUNCTION inventory.mirror_note() OWNER TO sync_wrong_owner",
            "function owner",
        ),
        (
            "ALTER TABLE inventory.widget OWNER TO sync_wrong_owner",
            "expected owner",
        ),
        (
            "CREATE OR REPLACE FUNCTION inventory.mirror_note() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $$BEGIN NEW.next_note := 'tampered'; RETURN NEW; END$$",
            "body differs",
        ),
        (
            "CREATE FUNCTION inventory.mirror_note(integer) RETURNS integer LANGUAGE sql AS $$SELECT $1$$",
            "overloaded",
        ),
        (
            "DROP FUNCTION inventory.mirror_note() CASCADE",
            "function is missing",
        ),
        (
            "DROP TRIGGER mirror_note ON inventory.widget",
            "trigger is missing",
        ),
        (
            "ALTER TABLE inventory.widget DISABLE TRIGGER mirror_note",
            "trigger differs",
        ),
        (
            "DROP TRIGGER mirror_note ON inventory.widget; CREATE TRIGGER mirror_note AFTER INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note()",
            "trigger differs",
        ),
        (
            "DROP TRIGGER mirror_note ON inventory.widget; CREATE TRIGGER mirror_note BEFORE INSERT OR UPDATE OF note ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note()",
            "trigger differs",
        ),
        (
            "DROP TRIGGER mirror_note ON inventory.widget; CREATE TRIGGER mirror_note BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION inventory.mirror_note()",
            "trigger differs",
        ),
        (
            "DROP TRIGGER mirror_note ON inventory.widget; CREATE TRIGGER mirror_note BEFORE INSERT OR UPDATE ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note('argument')",
            "trigger differs",
        ),
        (
            "ALTER TABLE inventory.widget ALTER COLUMN note TYPE integer USING 0",
            "actual synchronization row types",
        ),
    ] {
        let tx = client.transaction().await.unwrap();
        tx.batch_execute(tamper)
            .await
            .unwrap_or_else(|error| panic!("{tamper}: {error}"));
        let error = verify_stage_synchronizations(&tx, &expected)
            .await
            .expect_err(tamper);
        assert!(error.to_string().contains(named), "{tamper}: {error}");
        tx.rollback().await.unwrap();
    }
    let mut drift = expected.clone();
    drift[0].writable_columns.clear();
    assert!(
        verify_stage_synchronizations(&client, &drift)
            .await
            .unwrap_err()
            .to_string()
            .contains("explicitly writable")
    );
    assert!(
        verify_stage_synchronizations(&client, &[expected[0].clone(), expected[0].clone()])
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(
        read_catalog_with_synchronizations(&client, &[], &[], &expected)
            .await
            .unwrap_err()
            .to_string()
            .contains("outside the included")
    );
    assert!(
        read_catalog_with_synchronizations(
            &client,
            &["inventory"],
            &[("inventory", "widget")],
            &expected
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("outside the included")
    );

    client
        .batch_execute(
            "CREATE FUNCTION inventory.extra() RETURNS integer LANGUAGE sql AS $$SELECT 1$$",
        )
        .await
        .unwrap();
    assert!(
        read_catalog_with_synchronizations(&client, &["inventory"], &[], &expected)
            .await
            .unwrap_err()
            .to_string()
            .contains("absent from the exact expected")
    );
    client.batch_execute("DROP FUNCTION inventory.extra(); CREATE TRIGGER mirror_extra BEFORE INSERT ON inventory.widget FOR EACH ROW EXECUTE FUNCTION inventory.mirror_note()").await.unwrap();
    assert!(
        read_catalog_with_synchronizations(&client, &["inventory"], &[], &expected)
            .await
            .unwrap_err()
            .to_string()
            .contains("absent from the exact expected")
    );
    client
        .batch_execute("DROP TRIGGER mirror_extra ON inventory.widget")
        .await
        .unwrap();
    verify_stage_synchronizations(&client, &expected)
        .await
        .unwrap();
    let row = client
        .query_one(
            "SELECT note,next_note FROM inventory.widget WHERE id=1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "retained");
    assert_eq!(row.get::<_, Option<String>>(1), None);
    drop(client);
    task.await.unwrap().unwrap();
}

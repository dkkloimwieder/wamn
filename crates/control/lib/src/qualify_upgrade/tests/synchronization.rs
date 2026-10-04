use super::*;
use serde_json::json;

#[tokio::test]
async fn expand_installs_owned_mirror_and_retains_rows_during_single_column_writes() {
    let mut fixture = fixture("stage-synchronization", false).await;
    let candidate = fixture.root.join("candidate");
    fs::write(
        &fixture.suffix,
        "ALTER TABLE inventory.widget_tag ADD COLUMN mirror_note text;",
    )
    .unwrap();
    let ordinal: u32 = fixture
        .suffix
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split('_')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let synchronization = candidate.join(format!(
        "migrations/{:04}_synchronize_note.sql",
        ordinal + 1
    ));
    let sql = "CREATE FUNCTION inventory.mirror_widget_note() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $$BEGIN NEW.mirror_note := NEW.label; RETURN NEW; END$$;\nCREATE TRIGGER mirror_widget_note BEFORE INSERT OR UPDATE ON inventory.widget_tag FOR EACH ROW EXECUTE FUNCTION inventory.mirror_widget_note();";
    fs::write(&synchronization, sql).unwrap();
    let declaration = json!({"phase":"expand", "preconditions":{"retained":"SELECT EXISTS (SELECT 1 FROM inventory.widget_tag WHERE label = 'retained tag')"}, "postconditions":{"retained":"SELECT EXISTS (SELECT 1 FROM inventory.widget_tag WHERE label = 'retained tag' AND mirror_note IS NULL)","second":"SELECT EXISTS (SELECT 1 FROM inventory.widget_tag WHERE label = 'second tag')"}});
    let authored_path = candidate.join("wamn.k");
    let authored = fs::read_to_string(&authored_path).unwrap();
    fs::write(
        &authored_path,
        authored.replacen(
            "manifest.Package {",
            &format!("manifest.Package {{\n upgrade_stage = {declaration}"),
            1,
        ),
    )
    .unwrap();
    fs::write(
        wamn_schema_generator::package_manifest_path(&candidate),
        wamn_schema_generator::compile_manifest(&candidate).unwrap(),
    )
    .unwrap();
    let generation = fixture.server.database("upgrade_generation").unwrap();
    connect(generation.url()).await.batch_execute(&format!("ALTER TABLE inventory.widget_maker DROP COLUMN note; SET ROLE wamn_db_owner; ALTER TABLE inventory.widget_tag ADD COLUMN mirror_note text; {sql}; RESET ROLE")).await.unwrap();
    wamn_schema_generator::materialize_package_verified(
        MaterializeMode::Write,
        generation.url(),
        &candidate,
    )
    .await
    .unwrap();
    fixture.source.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:seed-mirror',true); INSERT INTO inventory.widget_tag(label) VALUES ('second tag'); COMMIT").await.unwrap();
    let original = state(&mut fixture.source).await;
    let qualified = qualify_upgrade_with_observer(
        request(&fixture, "synchronization.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    fixture.source.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:fail-mirror-postcondition',true); UPDATE inventory.widget_tag SET label='missing second' WHERE label='second tag'; COMMIT").await.unwrap();
    let failed = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{failed:#}").contains("postconditions second"),
        "{failed:#}"
    );
    let absent = fixture.source.query_one("SELECT to_regprocedure('inventory.mirror_widget_note()') IS NULL, NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='inventory' AND table_name='widget_tag' AND column_name='mirror_note'), NOT EXISTS (SELECT 1 FROM catalog.package_definition_owners WHERE tenant_id=$1 AND definition_type IN ('synchronization_function','synchronization_trigger'))", &[&TENANT]).await.unwrap();
    assert!(absent.get::<_, bool>(0) && absent.get::<_, bool>(1) && absent.get::<_, bool>(2));
    fixture.source.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:restore-mirror-postcondition',true); UPDATE inventory.widget_tag SET label='second tag' WHERE label='missing second'; COMMIT").await.unwrap();
    let result = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(result.changed);
    assert_eq!(result.migrations_applied, 2);
    let owners: i64 = fixture.source.query_one("SELECT count(*) FROM catalog.package_definition_owners WHERE tenant_id=$1 AND owner_package_id='platform_fixture' AND schema_name='inventory' AND relation_name='widget_tag' AND definition_type IN ('synchronization_function','synchronization_trigger')", &[&TENANT]).await.unwrap().get(0);
    assert_eq!(owners, 2);
    let retained = fixture
        .source
        .query_one(
            "SELECT label,mirror_note FROM inventory.widget_tag WHERE label='retained tag'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(retained.get::<_, String>(0), "retained tag");
    assert_eq!(retained.get::<_, Option<String>>(1), None);
    let first = connect(fixture.source_database.url()).await;
    let second = connect(fixture.source_database.url()).await;
    let first_write = first.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:mirror-first',true); SET LOCAL ROLE wamn_app; UPDATE inventory.widget_tag SET label='first write' WHERE label='retained tag'; COMMIT");
    let second_write = second.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:mirror-second',true); SET LOCAL ROLE wamn_app; UPDATE inventory.widget_tag SET label='second write' WHERE label='second tag'; COMMIT");
    let (first_result, second_result) = tokio::join!(first_write, second_write);
    first_result.unwrap();
    second_result.unwrap();
    for row in fixture
        .source
        .query(
            "SELECT label,mirror_note FROM inventory.widget_tag ORDER BY label",
            &[],
        )
        .await
        .unwrap()
    {
        assert_eq!(row.get::<_, String>(0), row.get::<_, String>(1));
    }
    assert!(
        !fixture
            .source
            .query_one(
                "SELECT has_column_privilege('wamn_app','inventory.widget_tag','mirror_note','UPDATE')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    let repeated = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(!repeated.changed);
    fixture.source.batch_execute("CREATE TRIGGER extra_mirror BEFORE INSERT OR UPDATE ON inventory.widget_tag FOR EACH ROW EXECUTE FUNCTION inventory.mirror_widget_note()").await.unwrap();
    let extra = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(format!("{extra:#}").contains("extra_mirror"), "{extra:#}");
    fixture
        .source
        .batch_execute("DROP TRIGGER extra_mirror ON inventory.widget_tag")
        .await
        .unwrap();
    let manifest_path = wamn_schema_generator::package_manifest_path(&candidate);
    let mut next: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    next["package"]["version"] = json!("2.3.0");
    next["package"]["predecessor_version"] = json!(CANDIDATE_VERSION);
    next["upgrade_stage"] = json!({"phase":"backfill", "preconditions":{"ready":"SELECT true"}, "postconditions":{"complete":"SELECT true"}, "backfill":{"batch_size":1,"initial_cursor":{},"sql":"WITH batch AS (SELECT id FROM inventory.widget_tag WHERE mirror_note IS NULL ORDER BY id LIMIT $2 FOR UPDATE), updated AS (UPDATE inventory.widget_tag AS target SET mirror_note = target.label FROM batch WHERE target.id = batch.id RETURNING target.id) SELECT $1::jsonb AS next_cursor, NOT EXISTS (SELECT 1 FROM batch) AS complete"}});
    fs::write(&manifest_path, serde_json::to_vec(&next).unwrap()).unwrap();
    fixture
        .source
        .batch_execute("ALTER FUNCTION inventory.mirror_widget_note() SECURITY DEFINER")
        .await
        .unwrap();
    let drift =
        apply_qualification_package(apply_request(fixture.source_database.url(), &candidate))
            .await
            .unwrap_err();
    assert!(
        format!("{drift:#}").contains("SECURITY INVOKER"),
        "{drift:#}"
    );
    assert!(!fixture.source.query_one("SELECT EXISTS (SELECT 1 FROM catalog.package_upgrade_stages WHERE tenant_id=$1 AND package_version='2.3.0')",&[&TENANT]).await.unwrap().get::<_,bool>(0));
    fixture
        .source
        .batch_execute("ALTER FUNCTION inventory.mirror_widget_note() SECURITY INVOKER")
        .await
        .unwrap();
    let backfill =
        apply_qualification_package(apply_request(fixture.source_database.url(), &candidate))
            .await
            .unwrap();
    assert!(backfill.changed);
    assert_eq!(backfill.migrations_applied, 0);
    fixture.server.stop().unwrap();
}

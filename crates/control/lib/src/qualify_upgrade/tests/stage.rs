use super::*;
use serde_json::json;

#[tokio::test]
async fn backfill_commits_batches_resumes_after_failed_postcondition_and_installs_last() {
    let mut fixture = fixture("stage-backfill", false).await;
    let candidate = fixture.root.join("candidate");
    fs::remove_file(&fixture.suffix).unwrap();
    let sql = "WITH batch AS (SELECT id FROM inventory.widget WHERE note IS NULL ORDER BY id LIMIT $2 FOR UPDATE), \
        updated AS (UPDATE inventory.widget AS target SET note = 'backfilled' FROM batch \
        WHERE target.id = batch.id RETURNING target.id) \
        SELECT $1::jsonb AS next_cursor, NOT EXISTS (SELECT 1 FROM batch) AS complete";
    let declaration = json!({
        "phase": "backfill", "preconditions": {"ready": "SELECT true"},
        "postconditions": {"retained": "SELECT NOT EXISTS (SELECT 1 FROM inventory.widget WHERE code = 'priority' AND note = 'backfilled')"},
        "backfill": {"sql": sql, "batch_size": 1, "initial_cursor": {"phase": "fill-notes"}}
    });
    let authored_path = candidate.join("wamn.k");
    let authored = fs::read_to_string(&authored_path).unwrap();
    fs::write(
        &authored_path,
        authored.replacen(
            "manifest.Package {",
            &format!("manifest.Package {{\n    upgrade_stage = {declaration}"),
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
    connect(generation.url())
        .await
        .batch_execute("ALTER TABLE inventory.widget_maker DROP COLUMN note")
        .await
        .unwrap();
    wamn_schema_generator::materialize_package_verified(
        MaterializeMode::Write,
        generation.url(),
        &candidate,
    )
    .await
    .unwrap();
    let original = state(&mut fixture.source).await;
    let result = qualify_upgrade_with_observer(
        request(&fixture, "backfill.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    let (evidence, _, _) = read_qualification(&result.result).unwrap();
    assert!(evidence.candidate_suffix.is_empty());

    fixture.source.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:seed-backfill',true); UPDATE inventory.widget SET note=NULL WHERE code='priority'; COMMIT").await.unwrap();
    let error = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("postconditions retained"),
        "{error:#}"
    );
    let progress = fixture.source.query_one(
        "SELECT status,cursor,completed_batches FROM catalog.package_upgrade_stages WHERE tenant_id=$1 AND package_id='platform_fixture' AND package_version=$2",
        &[&TENANT, &CANDIDATE_VERSION],
    ).await.unwrap();
    assert_eq!(progress.get::<_, String>(0), "in_progress");
    assert_eq!(
        progress.get::<_, serde_json::Value>(1),
        declaration["backfill"]["initial_cursor"]
    );
    assert_eq!(progress.get::<_, i64>(2), 2);
    assert_eq!(
        fixture
            .source
            .query_one(
                crate::apply_package::SELECT_CURRENT_PACKAGE_VERSION_SQL,
                &[&TENANT, &"platform_fixture"]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        PREDECESSOR_VERSION
    );
    assert_eq!(
        fixture
            .source
            .query_one(
                "SELECT count(*) FROM inventory.widget WHERE note='backfilled'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );

    let competing = fixture.root.join("competing");
    wamn_fixture_package::write_upgrade_package(&competing);
    let mut competing_manifest: serde_json::Value =
        serde_json::from_slice(&wamn_schema_generator::compile_manifest(&competing).unwrap())
            .unwrap();
    competing_manifest["package"]["version"] = json!("2.3.0");
    fs::write(
        wamn_schema_generator::package_manifest_path(&competing),
        serde_json::to_vec(&competing_manifest).unwrap(),
    )
    .unwrap();
    let error = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &competing,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("unfinished stage"),
        "{error:#}"
    );

    let manifest_path = wamn_schema_generator::package_manifest_path(&candidate);
    let frozen = fs::read(&manifest_path).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&frozen).unwrap();
    changed.as_object_mut().unwrap().remove("upgrade_stage");
    fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let error = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &candidate,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("immutable stage manifest"),
        "{error:#}"
    );
    fs::write(&manifest_path, frozen).unwrap();

    fixture.source.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:correct-backfill',true); UPDATE inventory.widget SET note='retained note' WHERE code='priority'; COMMIT").await.unwrap();
    let applied = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(applied.changed);
    assert_eq!(applied.migrations_applied, 0);
    let progress = fixture.source.query_one(
        "SELECT status,completed_batches FROM catalog.package_upgrade_stages WHERE tenant_id=$1 AND package_version=$2",
        &[&TENANT, &CANDIDATE_VERSION],
    ).await.unwrap();
    assert_eq!(progress.get::<_, String>(0), "completed");
    assert_eq!(progress.get::<_, i64>(1), 3);
    let repeated = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(!repeated.changed);
    fixture.server.stop().unwrap();
}

#[tokio::test]
async fn expand_conditions_preserve_rows_stop_on_lock_and_commit_with_evidence() {
    let mut fixture = fixture("stage-expand", false).await;
    let candidate = fixture.root.join("candidate");
    let manifest_path = wamn_schema_generator::package_manifest_path(&candidate);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["upgrade_stage"] = json!({
        "phase": "expand",
        "preconditions": {"retained": "SELECT EXISTS (SELECT 1 FROM inventory.widget_maker WHERE name = 'retained maker')"},
        "postconditions": {"expanded": "SELECT false"}
    });
    let original = state(&mut fixture.source).await;
    for (name, precondition, postcondition, expected) in [
        (
            "post-false",
            "SELECT true",
            "SELECT false",
            "postconditions expanded",
        ),
        (
            "foreign-relation",
            "SELECT EXISTS (SELECT 1 FROM catalog.packages)",
            "SELECT true",
            "catalog.packages",
        ),
    ] {
        manifest["upgrade_stage"]["preconditions"]["retained"] = json!(precondition);
        manifest["upgrade_stage"]["postconditions"]["expanded"] = json!(postcondition);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = qualify_upgrade_with_observer(
            request(&fixture, &format!("{name}.json")),
            WorkloadObserver::Captured(fixture.serving.clone()),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert_eq!(state(&mut fixture.source).await, original);
    }
    manifest["upgrade_stage"]["preconditions"]["retained"] =
        json!("SELECT EXISTS (SELECT 1 FROM inventory.widget_maker WHERE name = 'retained maker')");
    manifest["upgrade_stage"]["postconditions"]["expanded"] =
        json!("SELECT EXISTS (SELECT 1 FROM inventory.widget_maker WHERE note IS NULL)");
    let authored_path = candidate.join("wamn.k");
    let authored = fs::read_to_string(&authored_path).unwrap();
    fs::write(
        &authored_path,
        authored.replacen(
            "manifest.Package {",
            &format!(
                "manifest.Package {{\n    upgrade_stage = {}",
                manifest["upgrade_stage"]
            ),
            1,
        ),
    )
    .unwrap();
    fs::write(
        &manifest_path,
        wamn_schema_generator::compile_manifest(&candidate).unwrap(),
    )
    .unwrap();
    let generation = fixture.server.database("upgrade_generation").unwrap();
    wamn_schema_generator::materialize_package_verified(
        MaterializeMode::Write,
        generation.url(),
        &candidate,
    )
    .await
    .unwrap();
    let result = qualify_upgrade_with_observer(
        request(&fixture, "expand.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    let (evidence, _, _) = read_qualification(&result.result).unwrap();
    assert_eq!(evidence.format_version, 3);
    assert!(evidence.upgrade_stage.is_some());
    assert_eq!(state(&mut fixture.source).await, original);
    let retained = retained_rows(&fixture.source).await;

    let mut blocker = connect(fixture.source_database.url()).await;
    let lock = blocker.transaction().await.unwrap();
    lock.batch_execute("LOCK TABLE inventory.widget_maker IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let error = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("lock timeout"), "{error:#}");
    lock.rollback().await.unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    let applied = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(applied.changed);
    assert_eq!(retained_rows(&fixture.source).await, retained);
    let repeated = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &result.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(!repeated.changed);
    fixture.server.stop().unwrap();
}

#[tokio::test]
async fn stage_evidence_is_strict_and_unimplemented_execution_preserves_data() {
    let mut fixture = fixture("stage-format", false).await;
    let original = state(&mut fixture.source).await;
    let result = qualify_upgrade_with_observer(
        request(&fixture, "additive.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    let (mut evidence, additive_bytes, _) = read_qualification(&result.result).unwrap();
    assert_eq!(evidence.format_version, 1);
    assert!(evidence.upgrade_stage.is_none());
    let additive_value: serde_json::Value = serde_json::from_slice(&additive_bytes).unwrap();
    assert!(additive_value.get("upgrade_stage").is_none());

    let declaration = json!({
        "phase": "backfill",
        "preconditions": {"expanded": "SELECT true"},
        "postconditions": {"complete": "SELECT true"},
        "backfill": {
            "sql": "SELECT $1::jsonb AS next_cursor, true AS complete WHERE $2::int4 > 0",
            "batch_size": 100,
            "initial_cursor": null
        }
    });
    evidence.format_version = 3;
    evidence.upgrade_stage = Some(serde_json::from_value(declaration.clone()).unwrap());
    let staged = serde_json::to_value(&evidence).unwrap();
    let stage_bytes = wamn_execution_contract::canonical_json_bytes(&staged);
    assert_eq!(decode_qualification(&stage_bytes).unwrap(), evidence);
    assert_ne!(bytes_digest(&stage_bytes), bytes_digest(&additive_bytes));

    for (path, replacement) in [
        ("/format_version", json!(1)),
        ("/upgrade_stage/phase", json!("manual")),
        ("/upgrade_stage/backfill/batch_size", json!(0)),
        ("/upgrade_stage/preconditions", json!({})),
        ("/upgrade_stage/postconditions", json!({})),
        ("/candidate_package/predecessor_version", json!("wrong")),
    ] {
        let mut changed = staged.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        let bytes = wamn_execution_contract::canonical_json_bytes(&changed);
        assert!(decode_qualification(&bytes).is_err(), "accepted {path}");
    }
    let mut missing_cursor = staged.clone();
    missing_cursor["upgrade_stage"]["backfill"]
        .as_object_mut()
        .unwrap()
        .remove("initial_cursor");
    assert!(
        decode_qualification(&wamn_execution_contract::canonical_json_bytes(
            &missing_cursor
        ))
        .is_err()
    );

    let candidate = fixture.root.join("candidate");
    let manifest_path = wamn_schema_generator::package_manifest_path(&candidate);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["upgrade_stage"] = declaration;
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let declared = identity_from_directory(&read_package_directory(&candidate).unwrap()).unwrap();
    assert_ne!(
        declared.manifest_sha256,
        evidence.candidate_package.manifest_sha256
    );

    let refused = request(&fixture, "stage.json");
    let output = refused.result.clone();
    let error =
        qualify_upgrade_with_observer(refused, WorkloadObserver::Captured(fixture.serving.clone()))
            .await
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("backfill stage cannot carry a DDL suffix"),
        "{error:#}"
    );
    assert!(!output.exists());
    let error = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &candidate,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("backfill stage cannot carry a DDL suffix"),
        "{error:#}"
    );
    let suffix = fs::read(&fixture.suffix).unwrap();
    for (index, (section, sql, operation)) in [
        ("preconditions", "DO $$ BEGIN NULL; END $$", "DO"),
        ("backfill", "EXECUTE hidden_statement", "EXECUTE"),
        ("preconditions", "SELECT now() IS NOT NULL", "now"),
        ("postconditions", "SELECT 1 + 1 = 2", "+"),
        ("backfill", "SELECT coalesce($1, '{}'::jsonb)", "coalesce"),
        (
            "migration",
            "CREATE FUNCTION inventory.mirror() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN RETURN NEW; END $$;",
            "SECURITY DEFINER",
        ),
        (
            "migration",
            "CREATE FUNCTION inventory.mirror() RETURNS trigger LANGUAGE plpgsql SECURITY INVOKER AS $$ BEGIN EXECUTE 'SELECT 1'; RETURN NEW; END $$;",
            "EXECUTE",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut changed = manifest.clone();
        if section == "migration" {
            fs::write(&fixture.suffix, sql).unwrap();
        } else if section == "backfill" {
            changed["upgrade_stage"]["backfill"]["sql"] = json!(sql);
        } else {
            changed["upgrade_stage"][section] = json!({"refused": sql});
        }
        fs::write(&manifest_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let refused = request(&fixture, &format!("dynamic-{index}.json"));
        let output = refused.result.clone();
        let error = qualify_upgrade_with_observer(
            refused,
            WorkloadObserver::Captured(fixture.serving.clone()),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(operation), "{error:#}");
        assert!(!output.exists());
        let error = crate::apply_package::apply_package(apply_request(
            fixture.source_database.url(),
            &candidate,
        ))
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(operation), "{error:#}");
        fs::write(&fixture.suffix, &suffix).unwrap();
    }
    assert_eq!(state(&mut fixture.source).await, original);
    fixture.server.stop().unwrap();
}

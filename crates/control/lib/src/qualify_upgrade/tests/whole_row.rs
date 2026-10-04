//! The explicit whole-row exception commits schema, grants, and evidence together.

use super::*;
use serde_json::json;

async fn whole_row_fixture(name: &str) -> Fixture {
    let fixture = fixture(name, true).await;
    let candidate = fixture.root.join("candidate");
    let declaration = json!({
        "phase": "expand",
        "exceptions": ["whole_row_grants"],
        "preconditions": {
            "retained_widget": "SELECT EXISTS (SELECT 1 FROM inventory.widget WHERE code = 'priority')"
        },
        "postconditions": {
            "retained_whole_row": "SELECT EXISTS (SELECT 1 FROM inventory.widget WHERE code = 'priority' AND note = 'retained note' AND upgrade_note IS NULL)"
        }
    });
    let authored_path = candidate.join("wamn.k");
    let authored = fs::read_to_string(&authored_path).unwrap();
    assert_eq!(authored.matches("manifest.Package {").count(), 1);
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
    wamn_schema_generator::materialize_package_verified(
        MaterializeMode::Write,
        generation.url(),
        &candidate,
    )
    .await
    .unwrap();
    fixture
}

async fn retained_whole_row_data(client: &Client) -> Vec<String> {
    client.query(
        "SELECT identity FROM ( \
           SELECT 'maker:' || to_jsonb(t)::text || ':' || t.xmin::text AS identity FROM inventory.widget_maker t \
           UNION ALL SELECT 'widget:' || (to_jsonb(t) - 'upgrade_note')::text || ':' || t.xmin::text FROM inventory.widget t \
           UNION ALL SELECT 'tag:' || to_jsonb(t)::text || ':' || t.xmin::text FROM inventory.widget_tag t \
         ) observed ORDER BY identity",
        &[],
    ).await.unwrap().into_iter().map(|row| row.get(0)).collect()
}

#[tokio::test]
async fn whole_row_exception_commits_schema_grants_and_evidence_together() {
    let mut fixture = whole_row_fixture("whole-row-exception").await;
    let candidate = fixture.root.join("candidate");
    let original = state(&mut fixture.source).await;
    let retained = retained_whole_row_data(&fixture.source).await;
    let missing_evidence = crate::apply_package::apply_package(apply_request(
        fixture.source_database.url(),
        &candidate,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{missing_evidence:#}").contains("qualification"),
        "{missing_evidence:#}"
    );
    assert_eq!(state(&mut fixture.source).await, original);
    let qualified = qualify_upgrade_with_observer(
        request(&fixture, "whole-row.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    let (evidence, canonical, digest) = read_qualification(&qualified.result).unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    assert_eq!(evidence.format_version, 3);
    assert_eq!(digest, qualified.sha256);
    assert_eq!(evidence.predecessor_privileges, original.1);
    assert_eq!(
        evidence.upgrade_stage.as_ref().unwrap().exceptions,
        BTreeSet::from([wamn_schema_generator::UpgradeStageException::WholeRowGrants])
    );
    assert!(evidence.post_privileges.column.contains(&(
        "inventory".into(),
        "widget".into(),
        "upgrade_note".into(),
        "SELECT".into(),
    )));

    let suffix = fs::read(&fixture.suffix).unwrap();
    let mut changed = suffix.clone();
    changed.push(b'\n');
    fs::write(&fixture.suffix, changed).unwrap();
    let error = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("qualified candidate statements or generated artifacts changed"),
        "{diagnostic}"
    );
    assert_eq!(state(&mut fixture.source).await, original);
    fs::write(&fixture.suffix, suffix).unwrap();

    fixture
        .source
        .batch_execute("GRANT SELECT ON inventory.widget TO wamn_app")
        .await
        .unwrap();
    let drifted = state(&mut fixture.source).await;
    let error = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("predecessor privileges changed"),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.source).await, drifted);
    let transaction = fixture.source.transaction().await.unwrap();
    restore_upgrade_privileges(&transaction, &evidence.schemas, &original.1)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(state(&mut fixture.source).await, original);

    let statement = fixture
        .manifest
        .components
        .iter()
        .flat_map(|component| component.operations.values())
        .flat_map(|operation| operation.statements.values())
        .find(|statement| statement.sql.contains("to_jsonb(widget)"))
        .expect("frozen predecessor corpus contains its whole-row SELECT")
        .sql
        .clone();
    let reader_sql = statement.clone();
    let observer = connect(fixture.source_database.url()).await;
    observer
        .batch_execute("SET ROLE wamn_app; SET search_path=inventory")
        .await
        .unwrap();
    let (before_send, before_seen) = tokio::sync::oneshot::channel();
    let (after_send, after_seen) = tokio::sync::oneshot::channel();
    let (stop_send, mut stop_seen) = tokio::sync::oneshot::channel();
    let reader = tokio::spawn(async move {
        let mut before_send = Some(before_send);
        let mut after_send = Some(after_send);
        let mut before_count = 0;
        let mut after_count = 0;
        loop {
            tokio::select! {
                _ = &mut stop_seen => break,
                result = observer.query(&reader_sql, &[]) => {
                    let rows = result?;
                    assert_eq!(rows.len(), 2);
                    let expanded = rows.iter().all(|row| {
                        let attributes: serde_json::Value = row.get("attributes");
                        attributes.get("upgrade_note") == Some(&serde_json::Value::Null)
                    });
                    if expanded {
                        after_count += 1;
                        if let Some(send) = after_send.take() { let _ = send.send(()); }
                    } else {
                        before_count += 1;
                        if let Some(send) = before_send.take() { let _ = send.send(()); }
                    }
                }
            }
        }
        Ok::<_, tokio_postgres::Error>((before_count, after_count))
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), before_seen)
        .await
        .unwrap()
        .unwrap();

    let applied = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    let observed_after = tokio::time::timeout(std::time::Duration::from_secs(30), after_seen).await;
    let _ = stop_send.send(());
    let (before_count, after_count) = reader
        .await
        .unwrap()
        .expect("predecessor whole-row reads must remain authorized across apply");
    observed_after.unwrap().unwrap();
    assert!(before_count > 0 && after_count > 0);
    assert!(applied.changed);
    assert_eq!(applied.migrations_applied, 1);
    assert_eq!(retained_whole_row_data(&fixture.source).await, retained);
    assert_eq!(state(&mut fixture.source).await.1, evidence.post_privileges);
    let persisted = fixture.source.query_one(
        "SELECT canonical_bytes,result_sha256 FROM catalog.package_upgrade_qualifications WHERE tenant_id=$1 AND package_id='platform_fixture' AND candidate_package_version=$2",
        &[&TENANT, &CANDIDATE_VERSION],
    ).await.unwrap();
    assert_eq!(persisted.get::<_, Vec<u8>>(0), canonical);
    assert_eq!(persisted.get::<_, String>(1), qualified.sha256);

    plan_predecessor(&mut fixture.source, &fixture.manifest, &fixture.serving)
        .await
        .unwrap();
    let transaction = fixture.source.transaction().await.unwrap();
    transaction
        .batch_execute("SET LOCAL ROLE wamn_app; SET LOCAL search_path=inventory")
        .await
        .unwrap();
    let prepared = transaction.prepare(&statement).await.unwrap();
    let rows = transaction.query(&prepared, &[]).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let attributes: serde_json::Value = row.get("attributes");
        assert_eq!(
            attributes.get("upgrade_note"),
            Some(&serde_json::Value::Null)
        );
    }
    transaction.rollback().await.unwrap();
    let completed = state(&mut fixture.source).await;
    let repeated = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap();
    assert!(!repeated.changed);
    assert_eq!(state(&mut fixture.source).await, completed);
    fixture.server.stop().unwrap();
}

#[tokio::test]
async fn whole_row_postcondition_failure_rolls_back_schema_grants_and_evidence() {
    let mut fixture = whole_row_fixture("whole-row-rollback").await;
    let candidate = fixture.root.join("candidate");
    let original = state(&mut fixture.source).await;
    let suffix = fs::read(&fixture.suffix).unwrap();
    fs::write(&fixture.suffix,
        "ALTER TABLE inventory.widget ADD COLUMN upgrade_note text NOT NULL DEFAULT 'not_required';\n",
    ).unwrap();
    let refused = qualify_upgrade_with_observer(
        request(&fixture, "whole-row-default-refused.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{refused:#}")
            .contains("widget.upgrade_note must be a nullable column without a default"),
        "{refused:#}"
    );
    assert!(!fixture.root.join("whole-row-default-refused.json").exists());
    assert_eq!(state(&mut fixture.source).await, original);
    fs::write(&fixture.suffix, suffix).unwrap();
    let qualified = qualify_upgrade_with_observer(
        request(&fixture, "whole-row-rollback.json"),
        WorkloadObserver::Captured(fixture.serving.clone()),
    )
    .await
    .unwrap();
    assert_eq!(state(&mut fixture.source).await, original);
    fixture.source.batch_execute(
        "BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true),set_config('app.operation','admin:fail-whole-row-postcondition',true); UPDATE inventory.widget SET note='changed after qualification' WHERE code='priority'; COMMIT",
    ).await.unwrap();
    let before_attempt = state(&mut fixture.source).await;
    let retained = retained_whole_row_data(&fixture.source).await;
    let error = crate::apply_package::apply_qualified_package_observed(
        apply_request(fixture.source_database.url(), &candidate),
        &qualified.result,
        fixture.serving.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("postconditions retained_whole_row"),
        "{error:#}"
    );
    assert_eq!(state(&mut fixture.source).await, before_attempt);
    assert_eq!(retained_whole_row_data(&fixture.source).await, retained);
    let absent = fixture.source.query_one(
        "SELECT NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='inventory' AND table_name='widget' AND column_name='upgrade_note'), \
         NOT EXISTS (SELECT 1 FROM catalog.package_upgrade_qualifications WHERE tenant_id=$1 AND package_id='platform_fixture' AND candidate_package_version=$2), \
         NOT EXISTS (SELECT 1 FROM catalog.package_upgrade_stages WHERE tenant_id=$1 AND package_id='platform_fixture' AND package_version=$2)",
        &[&TENANT, &CANDIDATE_VERSION],
    ).await.unwrap();
    assert!(absent.get::<_, bool>(0) && absent.get::<_, bool>(1) && absent.get::<_, bool>(2));
    plan_predecessor(&mut fixture.source, &fixture.manifest, &fixture.serving)
        .await
        .unwrap();
    fixture.server.stop().unwrap();
}

//! Admit both applications and resolve their pull and push workflows in one release.
mod consumer_live;
mod support;

use serde_json::json;
use std::collections::BTreeSet;
use wamn_catalog::{ComponentPackageScope, PackageCoordinate, WiringDocument};
use wamn_control::publish_release::{PublishReleaseRequest, assemble_local_release};
use wamn_schema_generator::operation_reference::{AuthoredDocument, read_authored_document};

#[tokio::test]
async fn both_applications_publish_with_exact_dependencies_and_private_state() -> anyhow::Result<()>
{
    let repository = support::repository();
    let integration = repository.join("apps/platform/wamn_integration");
    let consumer = repository.join("apps/rest_consumer_fixture");
    let connector = repository.join("apps/rest_connector");
    let roots = vec![integration.clone(), consumer.clone(), connector.clone()];
    let base = support::admit(&integration, "wamn_integration", None, BTreeSet::new());
    let destination = support::admit(&consumer, "rest_consumer_fixture", None, BTreeSet::new());
    let component = support::admit(
        &connector,
        "rest_connector",
        None,
        BTreeSet::from([
            base.component_digest().into(),
            destination.component_digest().into(),
        ]),
    );
    let admissions = vec![
        base,
        destination,
        component,
        support::admit(
            &connector,
            "http-page",
            Some("apps/platform/no-std/http-page/declaration.json.in"),
            BTreeSet::new(),
        ),
        support::admit(
            &connector,
            "jsonata",
            Some("apps/platform/execution/jsonata/declaration.json.in"),
            BTreeSet::new(),
        ),
    ];
    let scope = ComponentPackageScope {
        tenant_id: "rest--publication--test".into(),
        package_id: "rest_connector".into(),
        package_version: "0.1.0".into(),
    };
    let documents: Vec<_> = ["rest_item", "rest_item_pushed"]
        .into_iter()
        .map(|name| {
            let document: WiringDocument = serde_json::from_value(
                read_authored_document(
                    &connector.join(format!("publication/wirings/{name}.json")),
                    AuthoredDocument::Wiring,
                )
                .unwrap(),
            )
            .unwrap();
            (scope.clone(), document)
        })
        .collect();
    let args = PublishReleaseRequest {
        applications: serde_json::from_value(serde_json::json!({"rest": {"packages": ["wamn_integration", "rest_connector", "rest_consumer_fixture"]}})).unwrap(),
        application: None,
        database_url: String::new(),
        control_database_url: String::new(),
        org: "rest".into(),
        project: "publication".into(),
        environment: "test".into(),
        verified_publisher_principal: "publisher".into(),
        run_schema: "catalog".into(),
        packages: [
            "wamn_integration",
            "rest_connector",
            "rest_consumer_fixture",
        ]
        .into_iter()
        .map(|name| PackageCoordinate::new(name, "0.1.0").unwrap())
        .collect(),
    };
    let assembled = assemble_local_release(&args, &roots, &admissions).unwrap();
    let release = &assembled.published.manifest;
    let packages = args
        .packages
        .iter()
        .map(|p| p.package_id().to_owned())
        .collect();
    for (_, document) in &documents {
        wamn_catalog::gate_workflow_document(&scope.tenant_id, &packages, release, document)?;
    }
    composed_sql_uses_released_schemas(&roots, release, &admissions).await?;
    let mut wrong_port = documents.clone();
    wrong_port[0]
        .1
        .edges
        .iter_mut()
        .find(|edge| edge.from == "rows")
        .unwrap()
        .from_port = "main".into();
    assert!(
        wamn_catalog::gate_workflow_document(
            &scope.tenant_id,
            &packages,
            release,
            &wrong_port[0].1
        )
        .is_err()
    );
    // Environment workflows resolve the node operation against the release,
    // not the package-local operation-dependency alias.
    let mut wrong_dependency = documents;
    wrong_dependency[0]
        .1
        .nodes
        .get_mut("record")
        .unwrap()
        .operation = "rest-consumer-fixture:item/other@0.1.0".into();
    assert!(
        wamn_catalog::gate_workflow_document(
            &scope.tenant_id,
            &packages,
            release,
            &wrong_dependency[0].1
        )
        .is_err()
    );
    Ok(())
}

async fn composed_sql_uses_released_schemas(
    roots: &[std::path::PathBuf],
    release: &wamn_catalog::ServingManifest,
    admissions: &[wamn_control::component_admission::ComponentAdmission],
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (client, connection) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(connection);
    let guest_url = consumer_live::install(&client, &database, roots, release).await?;
    let source = consumer_live::source(&client, "sql-proof").await?;
    let (mut guest, guest_connection) =
        tokio_postgres::connect(&guest_url, tokio_postgres::NoTls).await?;
    let guest_driver = tokio::spawn(guest_connection);
    for (package, expected_schema) in [
        ("rest_connector", "wamn_integration"),
        ("rest_consumer_fixture", "rest_consumer_fixture"),
    ] {
        let component = release
            .components
            .iter()
            .find(|component| {
                component.package_id == package && component.descriptor.component == package
            })
            .unwrap();
        assert_eq!(component.runtime_schema.as_deref(), Some(expected_schema));
        let transaction = guest.transaction().await?;
        transaction
            .query_one(
                "SELECT set_config('search_path', $1, true)",
                &[&component.runtime_schema],
            )
            .await?;
        let schema: String = transaction
            .query_one("SELECT current_schema()::text", &[])
            .await?
            .get(0);
        assert_eq!(schema, expected_schema);
        if package == "rest_connector" {
            let statements: Vec<_> = component
                .statement_instances
                .values()
                .flat_map(|set| set.values())
                .collect();
            let sql = |path: &str| {
                statements
                    .iter()
                    .find(|statement| statement.path == path)
                    .unwrap()
                    .sql
                    .as_str()
            };
            transaction
                .query(
                    sql("command/setting/write.sql"),
                    &[&"last_synced_at", &json!("2026-10-21T12:00:00Z")],
                )
                .await?;
            let value: serde_json::Value = transaction
                .query_one(sql("command/setting/read.sql"), &[&"last_synced_at"])
                .await?
                .get(0);
            assert_eq!(value, json!("2026-10-21T12:00:00Z"));
            transaction
                .query_one(
                    sql("command/setting/write.sql"),
                    &[&"last_synced_at", &json!("2026-10-21T12:01:00Z")],
                )
                .await?;
            transaction
                .query(
                    sql("command/log/append.sql"),
                    &[&"delivery", &"item", &"pushed", &None::<serde_json::Value>],
                )
                .await?;
            assert!(
                transaction
                    .query_one("SELECT to_regclass('item') IS NULL", &[])
                    .await?
                    .get::<_, bool>(0)
            );
        } else {
            transaction
                .query_one(
                    wamn_runtime::plugins::wamn_postgres::write_log::CLAIM_SQL,
                    &[
                        &"rest-consumer-fixture:item/record",
                        &"sql-proof",
                        &vec![1_u8],
                    ],
                )
                .await?;
            let statement = component
                .operations
                .values()
                .flat_map(|operation| operation.statements.values())
                .next()
                .unwrap();
            let key: String = transaction
                .query_one(&statement.sql, &[&"item", &"name", &source])
                .await?
                .get(0);
            assert_eq!(key, "item");
            transaction
                .query_one(
                    wamn_runtime::plugins::wamn_postgres::write_log::FINISH_SQL,
                    &[&"rest-consumer-fixture:item/record", &"sql-proof", &"{}"],
                )
                .await?;
            assert!(
                transaction
                    .query_one("SELECT to_regclass('setting') IS NULL", &[])
                    .await?
                    .get::<_, bool>(0)
            );
        }
        transaction.commit().await?;
    }
    eprintln!(
        "released connector SQL, consumer item insert, write-log claim and finish all passed under the App login"
    );
    consumer_live::invoke(&database, &guest_url, release, admissions).await?;
    let checkpoint: serde_json::Value = client
        .query_one(
            "SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(
        checkpoint,
        json!("2026-10-21T12:01:00Z"),
        "a failing consumer leaves last_synced_at unchanged"
    );
    let stored = client
        .query_one(
            "SELECT item.name, log.result::jsonb FROM rest_consumer_fixture.item AS item \
             CROSS JOIN platform.write_log AS log \
             WHERE item.key = 'native-item' AND log.operation = 'rest-consumer-fixture:item/record' \
             AND log.idempotency_key = 'live-key'",
            &[],
        )
        .await?;
    assert_eq!(stored.get::<_, String>(0), "native name");
    assert_eq!(
        stored.get::<_, serde_json::Value>(1),
        json!({"value":{"keys":["native-item"]}})
    );
    drop(guest);
    guest_driver.await??;
    drop(client);
    driver.await??;
    Ok(())
}

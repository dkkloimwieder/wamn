//! Admit both applications and resolve their pull and push workflows in one release.
mod consumer_live;
mod support;

use serde_json::json;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;
use wamn_catalog::{ComponentPackageCoordinate, PackageCoordinate, WiringDocument};
use wamn_publish::publish_release::{PublishReleaseRequest, assemble_release};

#[sqlx::test(migrations = false)]
async fn both_applications_publish_with_exact_dependencies_and_private_state(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
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
    let target = ComponentPackageCoordinate {
        project_env_key: "rest--test--publication".into(),
        package_id: "rest_connector".into(),
        package_version: "0.1.1".into(),
    };
    let documents: Vec<_> = ["rest_item", "rest_item_pushed"]
        .into_iter()
        .map(|name| {
            let document: WiringDocument = serde_json::from_value(
                serde_json::from_slice::<serde_json::Value>(
                    &std::fs::read(connector.join(format!("tests/workflows/{name}.json"))).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            (target.clone(), document)
        })
        .collect();
    let args = PublishReleaseRequest {
        applications: serde_json::from_value(serde_json::json!({"rest": {"packages": ["wamn_integration", "rest_connector", "rest_consumer_fixture"]}})).unwrap(),
        application: None,
        org: "rest".into(),
        project: "publication".into(),
        environment: "test".into(),
        packages: [
            "wamn_integration",
            "rest_connector",
            "rest_consumer_fixture",
        ]
        .into_iter()
        .map(|name| PackageCoordinate::new(name, "0.1.1").unwrap())
        .collect(),
    };
    let assembled = assemble_release(&args, &roots, &admissions, &[]).unwrap();
    let release = &assembled.published.manifest;
    let packages = args
        .packages
        .iter()
        .map(|p| p.package_id().to_owned())
        .collect();
    for (_, document) in &documents {
        wamn_catalog::condition_workflow_document(
            &target.project_env_key,
            &packages,
            release,
            document,
        )?;
    }
    composed_sql_uses_released_schemas(&pool, &roots, release, &admissions).await?;
    let mut wrong_port = documents.clone();
    wrong_port[0]
        .1
        .edges
        .iter_mut()
        .find(|edge| edge.from == "rows")
        .unwrap()
        .from_port = "main".into();
    assert!(
        wamn_catalog::condition_workflow_document(
            &target.project_env_key,
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
        wamn_catalog::condition_workflow_document(
            &target.project_env_key,
            &packages,
            release,
            &wrong_dependency[0].1
        )
        .is_err()
    );
    Ok(())
}

async fn composed_sql_uses_released_schemas(
    pool: &sqlx::PgPool,
    roots: &[std::path::PathBuf],
    release: &wamn_catalog::ServingManifest,
    admissions: &[wamn_publish::component_admission::ComponentAdmission],
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(pool);
    let mut client = wamn_postgres::connect(database.as_str()).await?;

    let guest_url = consumer_live::install(&mut client, &database, roots, release).await?;
    let source = consumer_live::source(&mut client, "sql-proof").await?;
    let mut guest = wamn_postgres::connect(&guest_url).await?;

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
        let mut transaction = guest.begin().await?;
        sqlx::query("SELECT set_config('search_path', $1, true)")
            .bind(&component.runtime_schema)
            .fetch_one(&mut *transaction)
            .await?;
        let schema: String = sqlx::query("SELECT current_schema()::text")
            .fetch_one(&mut *transaction)
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
            sqlx::query(sqlx::AssertSqlSafe(
                (sql("command/setting/write.sql")).to_string(),
            ))
            .bind(&"last_synced_at")
            .bind(&json!("2026-10-21T12:00:00Z"))
            .fetch_all(&mut *transaction)
            .await?;
            let value: serde_json::Value = sqlx::query(sqlx::AssertSqlSafe(
                (sql("command/setting/read.sql")).to_string(),
            ))
            .bind(&"last_synced_at")
            .fetch_one(&mut *transaction)
            .await?
            .get(0);
            assert_eq!(value, json!("2026-10-21T12:00:00Z"));
            sqlx::query(sqlx::AssertSqlSafe(
                (sql("command/setting/write.sql")).to_string(),
            ))
            .bind(&"last_synced_at")
            .bind(&json!("2026-10-21T12:01:00Z"))
            .fetch_one(&mut *transaction)
            .await?;
            sqlx::query(sqlx::AssertSqlSafe(
                (sql("command/log/append.sql")).to_string(),
            ))
            .bind(&"delivery")
            .bind(&"item")
            .bind(&"pushed")
            .bind(&None::<serde_json::Value>)
            .fetch_all(&mut *transaction)
            .await?;
            assert!(
                sqlx::query("SELECT to_regclass('item') IS NULL")
                    .fetch_one(&mut *transaction)
                    .await?
                    .get::<bool, _>(0)
            );
        } else {
            sqlx::query(wamn_postgres_capability::write_log::CLAIM_SQL)
                .bind(&"rest-consumer-fixture:item/record")
                .bind(&"sql-proof")
                .bind(&vec![1_u8])
                .fetch_one(&mut *transaction)
                .await?;
            let statement = component
                .descriptor
                .operations
                .values()
                .flat_map(|operation| operation.statements.values())
                .next()
                .unwrap();
            let key: String = sqlx::query(sqlx::AssertSqlSafe((&statement.sql).to_string()))
                .bind(&"item")
                .bind(&"name")
                .bind(&source)
                .fetch_one(&mut *transaction)
                .await?
                .get(0);
            assert_eq!(key, "item");
            sqlx::query(wamn_postgres_capability::write_log::FINISH_SQL)
                .bind(&"rest-consumer-fixture:item/record")
                .bind(&"sql-proof")
                .bind(&"{}")
                .fetch_one(&mut *transaction)
                .await?;
            assert!(
                sqlx::query("SELECT to_regclass('setting') IS NULL")
                    .fetch_one(&mut *transaction)
                    .await?
                    .get::<bool, _>(0)
            );
        }
        transaction.commit().await?;
    }
    eprintln!(
        "released connector SQL, consumer item insert, write-log claim and finish all passed under the App login"
    );
    consumer_live::invoke(&database, &guest_url, release, admissions).await?;
    let checkpoint: serde_json::Value =
        sqlx::query("SELECT value FROM wamn_integration.setting WHERE key='last_synced_at'")
            .fetch_one(&mut client)
            .await?
            .get(0);
    assert_eq!(
        checkpoint,
        json!("2026-10-21T12:01:00Z"),
        "a failing consumer leaves last_synced_at unchanged"
    );
    let stored = sqlx::query("SELECT item.name, log.result::jsonb FROM rest_consumer_fixture.item AS item \
             CROSS JOIN platform.write_log AS log \
             WHERE item.key = 'native-item' AND log.operation = 'rest-consumer-fixture:item/record' \
             AND log.idempotency_key = 'live-key'").fetch_one(&mut client)
        .await?;
    assert_eq!(stored.get::<String, _>(0), "native name");
    assert_eq!(
        stored.get::<serde_json::Value, _>(1),
        json!({"value":{"keys":["native-item"]}})
    );
    drop(guest);
    drop(client);
    Ok(())
}

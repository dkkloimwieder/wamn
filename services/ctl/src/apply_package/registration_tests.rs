use super::register_package;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::PackageCoordinate;
use wamn_schema_control::{PackageMigrationError, PackageMigrationErrorKind};

const TENANT: &str = "package-registration";

async fn connect(url: &str, control: bool) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    if control {
        client.batch_execute("SET ROLE wamn_system").await.unwrap();
    }
    client
        .batch_execute("SET app.tenant = 'package-registration'")
        .await
        .unwrap();
    client
}

async fn register(
    client: &mut Client,
    version: &str,
    hash: &str,
    predecessor: Option<&str>,
) -> anyhow::Result<bool> {
    let tx = client.transaction().await?;
    let inserted = register_package(
        &tx,
        TENANT,
        &PackageCoordinate::new("receiving", version).unwrap(),
        hash,
        predecessor,
    )
    .await?;
    tx.commit().await?;
    Ok(inserted)
}

fn refusal(error: anyhow::Error, kind: PackageMigrationErrorKind) {
    let error = error.downcast_ref::<PackageMigrationError>().unwrap();
    assert_eq!(error.kind(), kind);
    assert!(error.context().starts_with(kind.as_str()));
}

#[tokio::test]
#[ignore = "requires fresh PostgreSQL 18 in WAMN_CTL_PG_URL"]
async fn registration_serializes_replay_conflicts_successors_and_rollback() {
    let url = std::env::var("WAMN_CTL_PG_URL").expect("owned PostgreSQL database URL");
    let (admin, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    admin.batch_execute(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto; \
         DO $roles$ DECLARE name text; BEGIN \
         FOREACH name IN ARRAY ARRAY['wamn_system','wamn_control_author','wamn_app','wamn_scenario_author'] LOOP \
         IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = name) THEN \
         EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', name); \
         END IF; END LOOP; \
         EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $roles$;"
    ).await.unwrap();
    let hash = format!("sha256:{}", "a".repeat(64));
    let other_hash = format!("sha256:{}", "b".repeat(64));
    for (control, schema) in [
        (
            false,
            include_str!("../../../../deploy/sql/catalog-schema.sql"),
        ),
        (
            true,
            include_str!("../../../../deploy/sql/control-portable-store.sql"),
        ),
    ] {
        admin.batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS wamn_run CASCADE; DROP SCHEMA IF EXISTS wamn_authority CASCADE;").await.unwrap();
        if control {
            admin.batch_execute("SET ROLE wamn_system").await.unwrap();
        }
        admin.batch_execute(schema).await.unwrap();
        admin.batch_execute("RESET ROLE").await.unwrap();
        let mut first = connect(&url, control).await;
        let mut second = connect(&url, control).await;
        assert!(register(&mut first, "1.0.0", &hash, None).await.unwrap());
        let timestamp: String = first
            .query_one("SELECT registered_at::text FROM catalog.packages", &[])
            .await
            .unwrap()
            .get(0);
        assert!(!register(&mut second, "1.0.0", &hash, None).await.unwrap());
        assert_eq!(
            first
                .query_one("SELECT registered_at::text FROM catalog.packages", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            timestamp
        );
        refusal(
            register(&mut second, "1.0.0", &other_hash, None)
                .await
                .unwrap_err(),
            PackageMigrationErrorKind::CoordinateContentConflict,
        );
        refusal(
            register(&mut second, "1.0.0", &hash, Some("0.9.0"))
                .await
                .unwrap_err(),
            PackageMigrationErrorKind::CoordinatePredecessorConflict,
        );

        // Each contender reaches the actual caller while the winning transaction holds its lineage lock.
        for (winner, contender, contender_hash, expected) in [
            ("2.0.0", "2.0.0", hash.as_str(), None),
            (
                "3.0.0",
                "3.0.0",
                other_hash.as_str(),
                Some(PackageMigrationErrorKind::CoordinateContentConflict),
            ),
            (
                "4.0.0",
                "4.1.0",
                hash.as_str(),
                Some(PackageMigrationErrorKind::PredecessorNotCurrent),
            ),
        ] {
            let predecessor = match winner {
                "2.0.0" => "1.0.0",
                "3.0.0" => "2.0.0",
                _ => "3.0.0",
            };
            let tx = first.transaction().await.unwrap();
            assert!(
                register_package(
                    &tx,
                    TENANT,
                    &PackageCoordinate::new("receiving", winner).unwrap(),
                    &hash,
                    Some(predecessor)
                )
                .await
                .unwrap()
            );
            let pending = register(&mut second, contender, contender_hash, Some(predecessor));
            tokio::pin!(pending);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut pending)
                    .await
                    .is_err(),
                "registration must wait for the lineage transaction"
            );
            tx.commit().await.unwrap();
            let outcome = pending.await;
            if let Some(kind) = expected {
                refusal(outcome.unwrap_err(), kind);
            } else {
                assert!(!outcome.unwrap());
            }
        }
        // Exact retries of older versions remain valid after a successor is installed.
        assert!(!register(&mut first, "1.0.0", &hash, None).await.unwrap());
        let tx = first.transaction().await.unwrap();
        assert!(
            register_package(
                &tx,
                TENANT,
                &PackageCoordinate::new("receiving", "5.0.0").unwrap(),
                &hash,
                Some("4.0.0")
            )
            .await
            .unwrap()
        );
        tx.batch_execute("CREATE TABLE catalog.registration_rollback (id integer); INSERT INTO catalog.registration_rollback VALUES (1)").await.unwrap();
        tx.batch_execute("SELECT 1/0").await.unwrap_err();
        tx.rollback().await.unwrap();
        assert!(
            !first
                .query_one(
                    "SELECT EXISTS (SELECT FROM catalog.packages WHERE package_version = '5.0.0')",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert!(
            first
                .query_one(
                    "SELECT to_regclass('catalog.registration_rollback')::text",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, Option<String>>(0)
                .is_none()
        );
        assert_eq!(
            first
                .query_one("SELECT count(*) FROM catalog.packages", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            4
        );
        assert!(first.query_one("SELECT to_regprocedure('catalog.register_package(text,text,text,text,text)')::text", &[]).await.unwrap().get::<_, Option<String>>(0).is_none());
    }
}

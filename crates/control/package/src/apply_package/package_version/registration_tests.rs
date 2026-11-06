use super::{installed_environment_fingerprint, register_package};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::PackageCoordinate;
use wamn_schema_control::{PackageMigrationError, PackageMigrationErrorType};

const ENV_KEY: &str = "package-registration";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET app.env_key = 'package-registration'")
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
    crate::apply_package::bind_apply_package_principal(&tx).await?;
    let inserted = register_package(
        &tx,
        ENV_KEY,
        &PackageCoordinate::new("inventory", version).unwrap(),
        Some(hash),
        predecessor,
        &format!("sha256:{}", "c".repeat(64)),
    )
    .await?;
    tx.commit().await?;
    Ok(inserted)
}

fn refusal(error: &anyhow::Error, kind: PackageMigrationErrorType) {
    let error = error.downcast_ref::<PackageMigrationError>().unwrap();
    assert_eq!(error.error_type(), kind);
    assert!(error.context().starts_with(kind.as_str()));
}

#[tokio::test]
async fn registration_preserves_replays_and_the_installed_baseline() {
    let _lock = wamn_test_postgres::lock();
    let hash = format!("sha256:{}", "a".repeat(64));
    let other_hash = format!("sha256:{}", "b".repeat(64));
    // Registration runs against catalog.packages in the project database, the
    // only copy.
    {
        let database = wamn_catalog::test_database::tenant();
        database
            .execute(&[include_str!(
                "../../../../../../deploy/sql/app-schema-baseline.sql"
            )])
            .unwrap();
        let url = database.url();
        let mut first = connect(url).await;
        let mut second = connect(url).await;
        crate::schema_baseline::record_project(&first)
            .await
            .unwrap();
        assert!(register(&mut first, "1.0.0", &hash, None).await.unwrap());
        let environment_fingerprint = installed_environment_fingerprint(&first, ENV_KEY)
            .await
            .unwrap();
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
            &register(&mut second, "1.0.0", &other_hash, None)
                .await
                .unwrap_err(),
            PackageMigrationErrorType::CoordinateContentConflict,
        );
        refusal(
            &register(&mut second, "1.0.0", &hash, Some("0.9.0"))
                .await
                .unwrap_err(),
            PackageMigrationErrorType::CoordinatePredecessorConflict,
        );

        assert!(
            !register(&mut second, "2.0.0", &other_hash, Some("1.0.0"))
                .await
                .unwrap()
        );
        assert_eq!(
            installed_environment_fingerprint(&first, ENV_KEY)
                .await
                .unwrap(),
            environment_fingerprint
        );
        let baseline = first.query_one(
            "SELECT package_version, artifact_digest, registered_at::text, physical_schema_fingerprint FROM catalog.packages", &[]
        ).await.unwrap();
        assert_eq!(baseline.get::<_, String>(0), "1.0.0");
        assert_eq!(baseline.get::<_, String>(1), hash);
        assert_eq!(baseline.get::<_, String>(2), timestamp);
        assert_eq!(
            baseline.get::<_, String>(3),
            format!("sha256:{}", "c".repeat(64))
        );
    }
}

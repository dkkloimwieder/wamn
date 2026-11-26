//! PostgreSQL refusal of an attempt to rebind a registered relation.

#[tokio::test]
#[ignore = "requires: docker"]
async fn package_apply_refuses_rebinding_an_existing_relation() -> anyhow::Result<()> {
    use wamn_environment::apply_package::{self, ApplyPackageRequest};
    use wamn_release::package_source::PackageRoot;

    const INITIAL: &str = "CREATE TABLE fixture_app.item(id int PRIMARY KEY)";
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let migration = postgres.create_project_database("project").await?;
    wamn_environment::platform_migrations::install_project(
        &mut wamn_postgres::connect(migration.as_str()).await?,
    )
    .await?;
    let migration_input = String::from(migration);
    let mut database = url::Url::parse(postgres.admin_url())?;
    database.set_path("project");
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let write_package =
        |name: &str, entity: &str, version: &str, initial: &str| -> anyhow::Result<PackageRoot> {
            let root = scratch.path().join(name);
            std::fs::create_dir_all(root.join("generated"))?;
            std::fs::create_dir_all(root.join("migrations/notes"))?;
            std::fs::write(root.join("migrations/README.md"), "migration notes")?;
            let manifest = serde_json::json!({"package":{"id":name,"version":version},
            "required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},
            "models":{(entity):{"schema":"fixture_app","table":"item","owner":name,
                "audit_log":{"columns":[],"retention":"none"},"operations":{}}},
            "connections":{},"components":{}});
            std::fs::write(
                root.join("generated/wamn.json"),
                serde_json::to_vec(&manifest)?,
            )?;
            std::fs::write(root.join("migrations/0001_initial.up.sql"), initial)?;
            std::fs::write(
                root.join("migrations/0001_initial.down.sql"),
                "DROP TABLE fixture_app.item",
            )?;
            Ok(PackageRoot {
                path: root,
                artifact_digest: Some(
                    (format!("sha256:{}", "a".repeat(64)))
                        .parse::<wamn_digest::Digest>()
                        .unwrap(),
                ),
            })
        };
    let request = async |package| {
        let directory = wamn_release::package_source::read_package_root(&package)?;
        let migration_root = wamn_release::package_source::migration_root(&package.path);
        apply_package::apply_package_from_directory(
            ApplyPackageRequest {
                package,
                database_url: database.as_str().to_owned(),
                migration_database_url: migration_input.clone(),
                org: "mapping".parse().unwrap(),
                project: "fixture".parse().unwrap(),
                environment: "dev".parse().unwrap(),
            },
            directory,
            &migration_root,
        )
        .await
    };
    request(write_package("first", "item", "1.0.0", INITIAL)?).await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let original: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT 'fixture_app.item'::regclass::oid")
            .fetch_one(&mut admin)
            .await?;
    for package in [
        write_package("second", "item", "1.0.0", "SELECT 1")?,
        write_package("first", "replacement", "2.0.0", INITIAL)?,
    ] {
        assert!(request(package).await.is_err());
        let (oid, original_mapping): (sqlx::postgres::types::Oid,bool) = sqlx::query_as(
            "SELECT relation_oid::oid, package_id='first' AND entity_id='item' FROM fixture_app.wamn_entity WHERE relation_oid='fixture_app.item'::regclass")
            .fetch_one(&mut admin).await?;
        assert_eq!(oid, original);
        assert!(original_mapping);
    }
    Ok(())
}

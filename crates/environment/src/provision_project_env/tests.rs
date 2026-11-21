use super::{provisioning_transaction, registry::record_project_env};
use sqlx::{Connection as _, Row as _};

use super::write_secret_json;
use serde_json::{Value, json};
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use wamn_registry::Triple;

#[test]
fn secret_writes_replace_links_without_following_or_sharing_them() {
    let work = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    for file_type in ["file", "symlink", "hardlink"] {
        let source = work.path().join(format!("{file_type}-source.json"));
        let output = work.path().join(format!("{file_type}-output.json"));
        let original = json!({"stringData": {"token": "original-secret"}});
        std::fs::write(&source, serde_json::to_vec(&original).unwrap()).unwrap();
        match file_type {
            "file" => {
                std::fs::write(&output, b"old credential material").unwrap();
                std::fs::set_permissions(&output, Permissions::from_mode(0o644)).unwrap();
            }
            "symlink" => std::os::unix::fs::symlink(&source, &output).unwrap(),
            "hardlink" => std::fs::hard_link(&source, &output).unwrap(),
            _ => unreachable!(),
        }
        let replacement = json!({"stringData": {"token": "replacement-secret"}});
        write_secret_json(&output, &replacement).unwrap();
        assert!(
            !std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let stored: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(
            stored["stringData"]["token"],
            replacement["stringData"]["token"]
        );
        let retained: Value = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        assert_eq!(
            retained["stringData"]["token"],
            original["stringData"]["token"]
        );
    }
}

/// `env apply` passes no marker, so a row `wamn dev up` marked disposable
/// stays disposable in the environment (wamn-p9xix).
#[tokio::test]
#[ignore = "requires: docker"]
async fn a_record_without_a_marker_keeps_the_disposable_row() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("project").await.unwrap();
    wamn_provision::test_database::system(database.as_str()).await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut transaction = client.begin().await.unwrap();
    sqlx::raw_sql("INSERT INTO platform.tx_identity(principal_id,role,application) VALUES('770df186-ac15-579e-b46b-c297cae2011b','','fixture')")
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(wamn_registry::test_util::seed_demo_org_sql())
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    transaction.commit().await.unwrap();
    let triple = Triple::new(
        "demo".parse().unwrap(),
        "dev".parse().unwrap(),
        "inventory".parse().unwrap(),
    );
    for disposable in [Some(true), None] {
        let mut transaction = provisioning_transaction(&mut client).await.unwrap();
        record_project_env(
            &mut transaction,
            &triple,
            600,
            300,
            "cloud",
            "db-first",
            Some("system"),
            &"abcd1234".parse().unwrap(),
            disposable,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    }
    let registry: bool = sqlx::query(wamn_registry::test_util::disposable_environment_sql())
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get(0);
    assert!(registry, "a record without a marker keeps the registry row");
}

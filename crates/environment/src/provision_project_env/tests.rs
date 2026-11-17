use super::registry::do_record_project_env;
use sqlx::{Connection as _, Row as _};

use super::*;

/// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
/// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
/// collapses every env onto its pool. (The live routing through the DB is
/// validated by the in-cluster acceptance test; here we pin the pure derivation the
/// subcommand calls.)
#[test]
fn cluster_is_derived_by_placement_and_policy() {
    use wamn_provision::EnvPolicy;
    let ded = Org::dedicated("demo".parse().unwrap());
    assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "demo-dev");
    assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "demo-prod");
    let pooled = Org::pooled("try".parse().unwrap(), "wamn-pg");
    assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
}

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
#[sqlx::test(migrations = false)]
async fn a_record_without_a_marker_keeps_the_disposable_row(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
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
    sqlx::raw_sql(wamn_registry::test_util::seed_disposable_policy_sql())
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
        do_record_project_env(
            &mut client,
            &triple,
            None,
            "cloud",
            "db-first",
            Some("system"),
            &"abcd1234".parse().unwrap(),
            disposable,
        )
        .await
        .unwrap();
    }
    let registry: bool = sqlx::query(wamn_registry::test_util::disposable_environment_sql())
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get(0);
    assert!(registry, "a record without a marker keeps the registry row");
}

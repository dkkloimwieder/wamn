use super::registry::do_record_project_env;
use sqlx::{Connection as _, Row as _};

use super::*;

#[test]
fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
    // The name validation runs first — a reserved / non-slug project id fails
    // without touching the registry or emitting a CR.
    assert!(validate_project_env("demo", "wamn-x", &"dev".parse().unwrap()).is_err());
    assert!(validate_project_env("demo", "Bad", &"prod".parse().unwrap()).is_err());
    assert!(validate_project_env("demo", "billing", &"prod".parse().unwrap()).is_ok());
}

/// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
/// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
/// collapses every env onto its pool. (The live routing through the DB is
/// validated by the in-cluster acceptance test; here we pin the pure derivation the
/// subcommand calls.)
#[test]
fn cluster_is_derived_by_placement_and_policy() {
    use wamn_control_provision::EnvPolicy;
    let ded = Org::dedicated("demo");
    assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "demo-dev");
    assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "demo-prod");
    let pooled = Org::pooled("try", "wamn-pg");
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

/// A run with only PAT outputs mints for a recorded environment and writes
/// nothing to the registry, so it refuses the two registry values (wamn-rjtf).
#[tokio::test(flavor = "current_thread")]
async fn a_pat_only_run_refuses_the_registry_values_of_a_provisioning_run() {
    for (secret_namespace, disposable) in [(Some("hosts".to_owned()), None), (None, Some(true))] {
        let _error = provision_project_env(&ProvisionProjectEnvRequest {
            changes: wamn_broker::Publisher::default(),
            org: "acme".into(),
            project: "billing".into(),
            env: "dev".parse().unwrap(),
            policy: None,
            target: "cloud".to_owned(),
            disposable,
            system_database_url: Some("postgres://unused.invalid/wamn_system".into()),
            cluster: None,
            connection_limit: None,
            cluster_namespace: "platform".into(),
            namespace: "hosts".into(),
            secret_namespace,
            emit_database: None,
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
            pat_issuer: PatIssuerConfig::default(),
            emit_management_author_pat_secret: None,
            emit_operator_pat_secret: Some("/tmp/wamn-rjtf-unused.json".into()),
        })
        .await
        .expect_err("a PAT-only run must refuse a registry value");
    }
}

/// `env apply` passes no marker, so a row `wamn dev up` marked disposable
/// stays disposable in the environment (wamn-p9xix).
#[sqlx::test(migrations = false)]
async fn a_record_without_a_marker_keeps_the_disposable_row(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
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
    let triple = Triple::new("demo", "dev".parse().unwrap(), "inventory");
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

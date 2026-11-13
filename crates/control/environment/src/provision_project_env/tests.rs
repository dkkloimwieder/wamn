use super::registry::{INSTANCE_SUFFIX_ALPHABET, do_record_project_env};
use sqlx::Row as _;
use std::collections::BTreeSet;

use super::*;

/// The mint is the whole non-reuse mechanism (wamn-0h0g.13.57): every draw
/// must satisfy the pure crate's rule, and the draws must actually differ.
/// A constant, a counter, or a triple-derived suffix collapses `seen`.
#[test]
fn the_minted_instance_suffix_is_a_fresh_valid_dns_label_tail() {
    // A duplicated symbol would bias the `% len()` fold; a symbol outside
    // `[a-z0-9]` would leave the namespace an illegal DNS-1123 label.
    assert_eq!(INSTANCE_SUFFIX_ALPHABET.len(), 36);
    assert_eq!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        INSTANCE_SUFFIX_ALPHABET.len()
    );
    assert!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    );

    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let suffix = mint_instance_suffix().expect("mint an instance suffix");
        validate_instance_suffix(&suffix).expect("a minted suffix satisfies the pure rule");
        seen.insert(suffix);
    }
    assert_eq!(seen.len(), 64, "64 draws collapsed to {}", seen.len());
}

#[test]
fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
    // The name validation runs first — a reserved / non-slug project id fails
    // without touching the registry or emitting a CR.
    assert!(validate_project_env("demo", "wamn-x", "dev").is_err());
    assert!(validate_project_env("demo", "Bad", "prod").is_err());
    assert!(validate_project_env("demo", "billing", "prod").is_ok());
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
fn secret_files_are_plain_json_and_mode_0600() {
    let path = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-mode-{}.json",
        std::process::id()
    ));
    std::fs::write(&path, b"old credential material").unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

    let document = json!({"stringData": {"token": "test-token"}});
    write_secret_json(&path, &document).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, document);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn secret_writes_replace_links_without_following_or_sharing_them() {
    let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-links-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();

    let db_path = root.join("db.json");
    let management_path = root.join("management.json");
    std::os::unix::fs::symlink(&db_path, &management_path).unwrap();
    let db_document = json!({"stringData": {"url": "db-secret"}});
    let management_document = json!({"stringData": {"token": "management-secret"}});
    write_secret_json(&db_path, &db_document).unwrap();
    write_secret_json(&management_path, &management_document).unwrap();
    assert!(
        !std::fs::symlink_metadata(&management_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let stored_db: Value = serde_json::from_slice(&std::fs::read(&db_path).unwrap()).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    assert_eq!(stored_db, db_document);
    assert_eq!(stored_management, management_document);

    let route_path = root.join("route.json");
    std::fs::hard_link(&management_path, &route_path).unwrap();
    let route_document = json!({"stringData": {"token": "route-secret"}});
    write_secret_json(&route_path, &route_document).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    let stored_route: Value = serde_json::from_slice(&std::fs::read(&route_path).unwrap()).unwrap();
    assert_eq!(stored_management, management_document);
    assert_eq!(stored_route, route_document);

    std::fs::remove_file(db_path).unwrap();
    std::fs::remove_file(management_path).unwrap();
    std::fs::remove_file(route_path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

/// A run with only PAT outputs mints for a recorded environment and writes
/// nothing to the registry, so it refuses the two registry values (wamn-rjtf).
#[tokio::test(flavor = "current_thread")]
async fn a_pat_only_run_refuses_the_registry_values_of_a_provisioning_run() {
    for (secret_namespace, disposable) in [(Some("hosts".to_owned()), None), (None, Some(true))] {
        let error = provision_project_env(&ProvisionProjectEnvRequest {
            changes: wamn_broker::Publisher::default(),
            org: "acme".into(),
            project: "billing".into(),
            env: "dev".into(),
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
        assert_eq!(
            error.to_string(),
            "--secret-namespace and --disposable belong to a provisioning run, which passes an --emit-* output"
        );
    }
}

/// `env apply` passes no marker, so a row `wamn dev up` marked disposable
/// stays disposable in the environment (wamn-p9xix).
#[tokio::test]
async fn a_record_without_a_marker_keeps_the_disposable_row() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql("BEGIN; SET LOCAL app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs (id,placement_type) VALUES ('demo','dedicated'); INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('demo','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18'); COMMIT;").execute(&mut client).await.map(drop).unwrap();
    let triple = Triple::new("demo", "dev", "inventory");
    for disposable in [Some(true), None] {
        do_record_project_env(
            &mut client,
            &triple,
            None,
            "cloud",
            "db-first",
            Some("system"),
            "abcd1234",
            disposable,
        )
        .await
        .unwrap();
    }
    let registry: bool =
        sqlx::query("SELECT disposable FROM registry.environments WHERE org='demo' AND env='dev'")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get(0);
    assert!(registry, "a record without a marker keeps the registry row");
}

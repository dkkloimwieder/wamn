//! Disposable-PostgreSQL tests for package sealing at release publication.

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{EffectiveReleaseId, ManifestDigest, PackageCoordinate, ServingRelease};
use wamn_ctl::publish_release::{
    DeploymentCoordinate, attest_deployment, project_release_identity,
};

const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const CONTROL_STORE: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
const TENANT: &str = "publish-release-live";
const INSERT_MIGRATION_SQL: &str = "\
INSERT INTO catalog.package_migrations (\
       tenant_id, package_id, package_version, ordinal, relative_path, sha256\
     ) VALUES ($1, $2, $3, $4, $5, $6)";
const INSERT_MEMBERSHIP_SQL: &str = "\
INSERT INTO catalog.effective_release_packages (\
       tenant_id, effective_release_id, package_id, package_version\
     ) VALUES ($1, $2, $3, $4)";

#[derive(Clone, Copy)]
enum Store {
    Project,
    Control,
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn install(client: &Client, store: Store) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             CREATE EXTENSION IF NOT EXISTS pgcrypto; \
             DO $roles$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[\
                 'wamn_system', 'wamn_control_author', 'wamn_app', \
                 'wamn_scenario_author'\
               ] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB \
                                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', \
                                  role_name); \
                 END IF; \
               END LOOP; \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                              current_database()); \
             END $roles$;",
        )
        .await
        .expect("reset release-publication schemas and ensure prerequisite roles");

    match store {
        Store::Project => client
            .batch_execute(CATALOG_SCHEMA)
            .await
            .expect("install project package catalog"),
        Store::Control => {
            client
                .batch_execute("SET ROLE wamn_system")
                .await
                .expect("assume the control-store owner");
            let installed = client.batch_execute(CONTROL_STORE).await;
            client
                .batch_execute("RESET ROLE")
                .await
                .expect("leave the control-store owner");
            installed.expect("install control portable store");
        }
    }
}

async fn seed_package_and_release(client: &Client) {
    client
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("claim the test tenant");
    for (package_id, package_version, predecessor_version) in [
        ("receiving", "1.0.0", None),
        ("receiving", "2.0.0", Some("1.0.0")),
    ] {
        client
            .execute(
                "INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256, predecessor_version) VALUES ($1, $2, $3, $4, $5)",
                &[
                    &TENANT,
                    &package_id,
                    &package_version,
                    &format!("sha256:{}", "a".repeat(64)),
                    &predecessor_version,
                ],
            )
            .await
            .expect("register an immutable package coordinate");
    }
    client
        .execute(
            INSERT_MIGRATION_SQL,
            &[
                &TENANT,
                &"receiving",
                &"1.0.0",
                &1_i32,
                &"migrations/0001_initial.sql",
                &format!("sha256:{}", "b".repeat(64)),
            ],
        )
        .await
        .expect("record the migration that precedes release membership");
    client
        .execute(
            "INSERT INTO catalog.effective_releases (\
                   tenant_id, effective_release_id, environment, \
                   verified_publisher_principal\
                 ) VALUES ($1, 1, 'dev', 'publisher')",
            &[&TENANT],
        )
        .await
        .expect("register release identities");
}

async fn assert_immutable_rows(client: &Client, store: Store) {
    let row = client
        .query_one(
            "SELECT pg_get_userbyid(proowner)::text, current_user::text, \
             NOT EXISTS (SELECT FROM aclexplode(COALESCE(proacl, acldefault('f', proowner))) \
                         WHERE grantee = 0 AND privilege_type = 'EXECUTE') \
               FROM pg_proc \
              WHERE oid = 'catalog.reject_immutable_row_change()'::regprocedure",
            &[],
        )
        .await
        .expect("read the installed integrity function owner and privileges");
    let expected_owner = match store {
        Store::Project => row.get::<_, String>(1),
        Store::Control => "wamn_system".to_owned(),
    };
    assert_eq!(row.get::<_, String>(0), expected_owner);
    assert!(
        row.get::<_, bool>(2),
        "PUBLIC cannot execute the integrity function"
    );

    for statement in [
        "UPDATE catalog.package_migrations SET sha256 = 'sha256:' || repeat('c', 64)",
        "DELETE FROM catalog.package_migrations",
    ] {
        let error = client
            .execute(statement, &[])
            .await
            .expect_err("the installed trigger must refuse changes to recorded migration bytes");
        let database_error = error
            .as_db_error()
            .expect("PostgreSQL enforces immutability");
        assert_eq!(database_error.code().code(), "55000");
        assert_eq!(
            database_error.message(),
            "catalog.package_migrations is immutable"
        );
    }
}

async fn assert_package_seal(url: &str, store: Store) {
    let installer = connect(url).await;
    install(&installer, store).await;
    seed_package_and_release(&installer).await;
    assert_immutable_rows(&installer, store).await;

    let mut publisher = connect(url).await;
    let migrator = connect(url).await;
    migrator
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("claim the migration tenant");
    let publication = publisher
        .transaction()
        .await
        .expect("begin release membership");
    publication
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
        .await
        .expect("claim the publication tenant");
    publication
        .execute(
            INSERT_MEMBERSHIP_SQL,
            &[&TENANT, &1_i32, &"receiving", &"1.0.0"],
        )
        .await
        .expect("insert the first membership while holding the package row");

    let late_hash = format!("sha256:{}", "c".repeat(64));
    let late_ordinal = 2_i32;
    let late_params: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
        &TENANT,
        &"receiving",
        &"1.0.0",
        &late_ordinal,
        &"migrations/0002_late.sql",
        &late_hash,
    ];
    let late_migration = migrator.execute(INSERT_MIGRATION_SQL, &late_params);
    tokio::pin!(late_migration);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut late_migration)
            .await
            .is_err(),
        "migration insertion did not serialize on the package row"
    );
    publication
        .commit()
        .await
        .expect("commit release membership and its seal");
    let refused = late_migration
        .await
        .expect_err("migration after committed membership must refuse");
    let refused = refused
        .as_db_error()
        .expect("the package seal refusal comes from PostgreSQL");
    assert_eq!(refused.code().code(), "55000");
    assert_eq!(refused.message(), "package-version-sealed");
    assert_eq!(
        refused.detail(),
        Some("coordinate=receiving@1.0.0 belongs to an effective release")
    );
    assert_eq!(
        refused.hint(),
        Some("create and apply a new package version for additional migrations")
    );

    for (ordinal, path, hash_byte) in [
        (1_i32, "migrations/0001_initial.sql", "b"),
        (2_i32, "migrations/0002_new_version.sql", "e"),
    ] {
        assert_eq!(
            installer
                .execute(
                    INSERT_MIGRATION_SQL,
                    &[
                        &TENANT,
                        &"receiving",
                        &"2.0.0",
                        &ordinal,
                        &path,
                        &format!("sha256:{}", hash_byte.repeat(64)),
                    ],
                )
                .await
                .expect("a cumulative new package coordinate remains writable"),
            1
        );
    }
}

#[tokio::test]
async fn package_seal_and_attestation_winner_are_server_enforced() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping publish-release live test; WAMN_CTL_PG_URL is unset");
        return;
    };

    assert_package_seal(&url, Store::Project).await;
    assert_package_seal(&url, Store::Control).await;

    let release = ServingRelease {
        tenant_id: TENANT.to_owned(),
        effective_release_id: EffectiveReleaseId::new(1).unwrap(),
        environment: "dev".to_owned(),
        packages: BTreeSet::from([PackageCoordinate::new("receiving", "1.0.0").unwrap()]),
    };
    let coordinate = DeploymentCoordinate::new("acme", "receiving", &release);
    let digest = ManifestDigest::parse(format!("sha256:{}", "f".repeat(64))).unwrap();
    let (first, second) = tokio::join!(
        attest_deployment(&url, &coordinate, &digest, Some("0123456789abcdef")),
        attest_deployment(&url, &coordinate, &digest, Some("0123456789abcdef"))
    );
    assert_eq!(
        first.expect("first identical attestation succeeds"),
        second.expect("concurrent identical attestation returns the winner")
    );

    connect(&url)
        .await
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE;",
        )
        .await
        .expect("clean release-publication schemas");
}

#[tokio::test]
async fn release_identity_and_attestation_decisions_preserve_concurrent_winners() {
    use wamn_schema_control::attestation::{AttestationError, AttestationErrorKind};
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping release identity and attestation test: WAMN_CTL_PG_URL is unset");
        return;
    };
    let inspector = connect(&url).await;
    install(&inspector, Store::Control).await;
    let coordinate = DeploymentCoordinate {
        tenant_id: TENANT.to_owned(),
        effective_release_id: 7,
        triple: wamn_control_registry::Triple::new("acme", "billing", "prod"),
    };
    let (first, second) = tokio::join!(
        project_release_identity(&url, &coordinate),
        project_release_identity(&url, &coordinate)
    );
    first.expect("first identity projection");
    second.expect("identical concurrent identity projection");
    let initial = inspector.query_one("SELECT created_at FROM catalog.effective_releases WHERE tenant_id = $1 AND effective_release_id = 7", &[&TENANT]).await.unwrap();
    let initial_at: chrono::DateTime<chrono::Utc> = initial.get(0);
    project_release_identity(&url, &coordinate).await.unwrap();
    assert_eq!(initial_at, inspector.query_one("SELECT created_at FROM catalog.effective_releases WHERE tenant_id = $1 AND effective_release_id = 7", &[&TENANT]).await.unwrap().get::<_, chrono::DateTime<chrono::Utc>>(0));
    let mut changed = coordinate.clone();
    changed.triple.env = "dev".into();
    let error = project_release_identity(&url, &changed).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::IdentityProjectionConflict
    );
    assert!(
        error
            .to_string()
            .starts_with("effective-release-identity-projection-content-conflict:")
    );
    assert_eq!(inspector.query_one("SELECT environment FROM catalog.effective_releases WHERE tenant_id = $1 AND effective_release_id = 7", &[&TENANT]).await.unwrap().get::<_, String>(0), "prod");

    let mut raced = coordinate.clone();
    raced.effective_release_id = 8;
    changed.effective_release_id = 8;
    let (first, second) = tokio::join!(
        project_release_identity(&url, &raced),
        project_release_identity(&url, &changed)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "only one conflicting identity wins"
    );
    let refusal = first.err().or_else(|| second.err()).unwrap();
    assert_eq!(
        refusal.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::IdentityProjectionConflict
    );
    let winner_env: String = inspector.query_one("SELECT environment FROM catalog.effective_releases WHERE tenant_id = $1 AND effective_release_id = 8", &[&TENANT]).await.unwrap().get(0);
    assert!(["prod", "dev"].contains(&winner_env.as_str()));
    // A competing insert must wait, then compare the committed winner.
    let mut held = connect(&url).await;
    let transaction = held.transaction().await.unwrap();
    transaction.execute("INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment) VALUES ($1,9,'prod')", &[&TENANT]).await.unwrap();
    let mut waiting_coordinate = changed.clone();
    waiting_coordinate.effective_release_id = 9;
    let waiting = project_release_identity(&url, &waiting_coordinate);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut waiting)
            .await
            .is_err()
    );
    transaction.commit().await.unwrap();
    let error = waiting.await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::IdentityProjectionConflict
    );
    let mut other_tenant = changed.clone();
    other_tenant.tenant_id = "other-tenant".to_owned();
    project_release_identity(&url, &other_tenant)
        .await
        .expect("a different tenant owns its release identity");

    let hash = ManifestDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
    let other_hash = ManifestDigest::parse(format!("sha256:{}", "b".repeat(64))).unwrap();
    let (first, second) = tokio::join!(
        attest_deployment(&url, &coordinate, &hash, Some("0123456789abcdef")),
        attest_deployment(&url, &coordinate, &hash, Some("0123456789abcdef"))
    );
    let first_at = first.unwrap();
    assert_eq!(first_at, second.unwrap());
    for (digest, source) in [
        (&other_hash, Some("0123456789abcdef")),
        (&hash, Some("fedcba9876543210")),
        (&hash, None),
    ] {
        let error = attest_deployment(&url, &coordinate, digest, source)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AttestationError>().unwrap().kind(),
            AttestationErrorKind::ContentConflict
        );
        assert!(
            error
                .to_string()
                .starts_with("deployment-attestation-content-conflict:")
        );
    }
    let row = inspector.query_one("SELECT deployed_manifest_hash, source_commit, attested_at FROM catalog.deployment_attestations WHERE tenant_id=$1 AND environment_instance='' AND effective_release_id=7", &[&TENANT]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), hash.as_str());
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("0123456789abcdef")
    );
    assert_eq!(
        row.get::<_, chrono::DateTime<chrono::Utc>>(2)
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        first_at
    );

    // The marker alone cannot replace the old deployment.
    inspector.execute("INSERT INTO catalog.tenant_environments (tenant_id,org,project,env,instance_suffix,disposable,environment_instance) VALUES ($1,'acme','billing','prod','abcd1234',true,'')", &[&TENANT]).await.unwrap();
    let error = attest_deployment(&url, &coordinate, &other_hash, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::ContentConflict
    );
    inspector.execute("UPDATE catalog.tenant_environments SET environment_instance='16384' WHERE tenant_id=$1", &[&TENANT]).await.unwrap();
    let instance_at = attest_deployment(&url, &coordinate, &other_hash, None)
        .await
        .unwrap();
    assert_eq!(
        instance_at,
        attest_deployment(&url, &coordinate, &other_hash, None)
            .await
            .unwrap()
    );
    for source in [Some("0123456789abcdef"), Some("")] {
        let error = attest_deployment(&url, &coordinate, &other_hash, source)
            .await
            .unwrap_err();
        let kind = error.downcast_ref::<AttestationError>().unwrap().kind();
        assert_eq!(
            kind,
            if source == Some("") {
                AttestationErrorKind::Storage
            } else {
                AttestationErrorKind::ContentConflict
            }
        );
    }
    let error = attest_deployment(&url, &coordinate, &hash, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::ContentConflict
    );
    assert_eq!(
        inspector
            .query_one(
                "SELECT count(*) FROM catalog.deployment_attestations WHERE tenant_id=$1",
                &[&TENANT]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(inspector.query_one("SELECT deployed_manifest_hash FROM catalog.deployment_attestations WHERE tenant_id=$1 AND environment_instance=''", &[&TENANT]).await.unwrap().get::<_, String>(0), hash.as_str());

    let mut content_race = coordinate.clone();
    content_race.triple.project = "other-project".into();
    let (first, second) = tokio::join!(
        attest_deployment(&url, &content_race, &hash, None),
        attest_deployment(&url, &content_race, &other_hash, None)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "only one differing attestation wins"
    );
    let (winning_at, expected_hash) = match (first, second) {
        (Ok(at), Err(error)) => {
            assert_eq!(
                error.downcast_ref::<AttestationError>().unwrap().kind(),
                AttestationErrorKind::ContentConflict
            );
            (at, hash.as_str())
        }
        (Err(error), Ok(at)) => {
            assert_eq!(
                error.downcast_ref::<AttestationError>().unwrap().kind(),
                AttestationErrorKind::ContentConflict
            );
            (at, other_hash.as_str())
        }
        other => panic!("unexpected concurrent attestation results: {other:?}"),
    };
    let row = inspector.query_one("SELECT deployed_manifest_hash, attested_at FROM catalog.deployment_attestations WHERE tenant_id=$1 AND environment_instance='16384' AND project_id='other-project'", &[&TENANT]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), expected_hash);
    assert_eq!(
        row.get::<_, chrono::DateTime<chrono::Utc>>(1)
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        winning_at
    );

    // A rolled-back first insert leaves the waiting native writer free to win.
    let transaction = held.transaction().await.unwrap();
    transaction.execute("INSERT INTO catalog.deployment_attestations (tenant_id,environment_instance,effective_release_id,org_id,project_id,environment,deployed_manifest_hash,source_commit,attested_at) VALUES ($1,'16384',7,'acme','rollback','prod',$2,NULL,'2026-08-15T12:00:00Z')", &[&TENANT, &hash.as_str()]).await.unwrap();
    let mut rollback_coordinate = coordinate.clone();
    rollback_coordinate.triple.project = "rollback".into();
    let waiting = attest_deployment(&url, &rollback_coordinate, &other_hash, None);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut waiting)
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
    let recorded = waiting.await.unwrap();
    let row = inspector.query_one("SELECT deployed_manifest_hash,attested_at FROM catalog.deployment_attestations WHERE tenant_id=$1 AND project_id='rollback'", &[&TENANT]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), other_hash.as_str());
    assert_eq!(
        row.get::<_, chrono::DateTime<chrono::Utc>>(1)
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        recorded
    );

    // An invalid foreign key rolls the write back without adding a row.
    let mut missing_release = coordinate.clone();
    missing_release.effective_release_id = 99;
    let error = attest_deployment(&url, &missing_release, &hash, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AttestationError>().unwrap().kind(),
        AttestationErrorKind::Storage
    );
    assert_eq!(inspector.query_one("SELECT count(*) FROM catalog.deployment_attestations WHERE effective_release_id=99", &[]).await.unwrap().get::<_, i64>(0), 0);

    // The owner role still reads only its claimed tenant.
    inspector
        .batch_execute("SET ROLE wamn_system; SET app.tenant = 'other-tenant'")
        .await
        .unwrap();
    assert_eq!(
        inspector
            .query_one(
                "SELECT count(*) FROM catalog.effective_releases WHERE tenant_id=$1",
                &[&TENANT]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        inspector
            .query_one(
                "SELECT count(*) FROM catalog.deployment_attestations WHERE tenant_id=$1",
                &[&TENANT]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    inspector.batch_execute("RESET ROLE; DROP SCHEMA catalog CASCADE; DROP SCHEMA wamn_run CASCADE; DROP SCHEMA wamn_authority CASCADE").await.unwrap();
}

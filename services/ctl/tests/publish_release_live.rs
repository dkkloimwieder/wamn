//! Disposable-PostgreSQL proofs for package sealing at release publication.

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{EffectiveReleaseId, ManifestDigest, PackageCoordinate, ServingRelease};
use wamn_ctl::publish_release::{DeploymentCoordinate, attest_deployment};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const CONTROL_STORE: &str = include_str!("../../../deploy/sql/control-portable-store.sql");
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
            .query_one(
                "SELECT catalog.register_package($1, $2, $3, $4, $5)",
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

async fn prove_package_seal(url: &str, store: Store) {
    let installer = connect(url).await;
    install(&installer, store).await;
    seed_package_and_release(&installer).await;

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
        eprintln!("skipping publish-release live proof; WAMN_CTL_PG_URL is unset");
        return;
    };

    prove_package_seal(&url, Store::Project).await;
    prove_package_seal(&url, Store::Control).await;

    let release = ServingRelease {
        tenant_id: TENANT.to_owned(),
        effective_release_id: EffectiveReleaseId::new(1).unwrap(),
        environment: "dev".to_owned(),
        packages: BTreeSet::from([PackageCoordinate::new("receiving", "1.0.0").unwrap()]),
    };
    let coordinate = DeploymentCoordinate::new("acme", "receiving", &release);
    let digest = ManifestDigest::parse(format!("sha256:{}", "f".repeat(64))).unwrap();
    let (first, second) = tokio::join!(
        attest_deployment(&url, &coordinate, &digest),
        attest_deployment(&url, &coordinate, &digest)
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

//! Live tests for the promotion transaction that owns wiring activation.

use std::time::Duration;

use tokio_postgres::{Client, IsolationLevel, NoTls};
use wamn_catalog::{WiringActivationError, WiringActivationErrorKind};

use super::{CLAIM_TENANT_SQL, PromoteArgs, activate_once};

fn hash(letter: char) -> String {
    format!("sha256:{}", letter.to_string().repeat(64))
}

fn args(url: &str) -> PromoteArgs {
    PromoteArgs {
        source_database_url: url.to_owned(),
        target_database_url: url.to_owned(),
        control_database_url: url.to_owned(),
        org: "org".to_owned(),
        project: "project".to_owned(),
        tenant: "t1".to_owned(),
        source_effective_release_id: 1,
        target_effective_release_id: 1,
        source_environment: "stage".to_owned(),
        target_environment: "prod".to_owned(),
        run_schema: "unused".to_owned(),
        artifact_base: "unused".to_owned(),
        registry_auth_file: "unused".into(),
        insecure_registry: false,
        principal: "spiffe://wamn.test/publisher".to_owned(),
        reason: "test activation".to_owned(),
    }
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        connection.await.expect("database connection");
    });
    client
}

async fn fresh_catalog(client: &Client) {
    client
        .batch_execute(
            "DO $$ BEGIN
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN
             CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOBYPASSRLS;
           END IF;
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOBYPASSRLS;
           END IF;
         END $$;
         DROP SCHEMA IF EXISTS catalog CASCADE;",
        )
        .await
        .expect("prepare fresh catalog");
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install catalog");
    client
        .batch_execute(&format!(
            "SET app.tenant = 't1';
         INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256)
           VALUES ('t1','shop','1.0.0','{a}'), ('t1','shop','2.0.0','{b}');
         INSERT INTO catalog.effective_releases
           (tenant_id, effective_release_id, environment, verified_publisher_principal)
           VALUES ('t1',1,'prod','spiffe://wamn.test/publisher');
         INSERT INTO catalog.effective_release_packages
           (tenant_id, effective_release_id, package_id, package_version)
           VALUES ('t1',1,'shop','1.0.0');
         INSERT INTO catalog.effective_release_heads
           (tenant_id, environment, effective_release_id) VALUES ('t1','prod',1);
         INSERT INTO catalog.wirings
           (tenant_id, package_id, package_version, wiring_id, version, graph_json, wiring_hash)
           VALUES ('t1','shop','1.0.0','orders-create',1,'{{}}','{a}'),
                  ('t1','shop','1.0.0','orders-create',2,'{{}}','{b}'),
                  ('t1','shop','2.0.0','orders-create',3,'{{}}','{c}');",
            a = hash('a'),
            b = hash('b'),
            c = hash('c'),
        ))
        .await
        .expect("seed releases and definitions");
}

async fn begin(client: &mut Client) -> tokio_postgres::Transaction<'_> {
    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::Serializable)
        .start()
        .await
        .expect("start promotion transaction");
    tx.query_one(CLAIM_TENANT_SQL, &[&"t1"])
        .await
        .expect("claim tenant");
    tx
}

async fn counts(client: &Client) -> (i64, i64) {
    let row = client
        .query_one(
            "SELECT (SELECT count(*) FROM catalog.wiring_activation),
                (SELECT count(*) FROM catalog.wiring_activation_events)",
            &[],
        )
        .await
        .expect("read activation and history counts");
    (row.get(0), row.get(1))
}

async fn refuses_unreleased_absent_and_foreign_definitions(
    client: &mut Client,
    args: &PromoteArgs,
) {
    for (wiring, digest) in [
        ("orders-create", hash('c')),
        ("orders-create", hash('d')),
        ("absent-wiring", hash('a')),
    ] {
        let tx = begin(client).await;
        let error = activate_once(&tx, args, "shop", wiring, &digest)
            .await
            .expect_err("refused definition");
        let refusal = error
            .downcast_ref::<WiringActivationError>()
            .expect("Rust activation error");
        assert_eq!(
            refusal.kind(),
            WiringActivationErrorKind::DefinitionNotInRelease
        );
        assert!(
            refusal
                .to_string()
                .starts_with("wiring-activation-definition-not-in-effective-release:")
        );
        tx.rollback().await.expect("rollback refused activation");
        assert_eq!(counts(client).await, (0, 0));
    }
    let tx = begin(client).await;
    tx.query_one(CLAIM_TENANT_SQL, &[&"other-tenant"])
        .await
        .expect("claim other tenant");
    let error = activate_once(&tx, args, "shop", "orders-create", &hash('a'))
        .await
        .expect_err("foreign definition");
    assert_eq!(
        error
            .downcast_ref::<WiringActivationError>()
            .expect("Rust refusal")
            .kind(),
        WiringActivationErrorKind::DefinitionNotInRelease
    );
    tx.rollback().await.expect("rollback foreign claim");
    assert_eq!(counts(client).await, (0, 0));
}

async fn commit_retry_and_rollback_keep_one_history(client: &mut Client, args: &PromoteArgs) {
    let tx = begin(client).await;
    assert!(
        activate_once(&tx, args, "shop", "orders-create", &hash('a'))
            .await
            .expect("first activation")
    );
    tx.commit().await.expect("commit activation");
    assert_eq!(counts(client).await, (1, 1));
    let tx = begin(client).await;
    assert!(
        !activate_once(&tx, args, "shop", "orders-create", &hash('a'))
            .await
            .expect("exact retry")
    );
    tx.commit().await.expect("commit retry");
    assert_eq!(counts(client).await, (1, 1));
    let tx = begin(client).await;
    assert!(
        activate_once(&tx, args, "shop", "orders-create", &hash('b'))
            .await
            .expect("new definition")
    );
    tx.rollback().await.expect("abort activation and history");
    assert_eq!(counts(client).await, (1, 1));
    let actual: String = client
        .query_one(
            "SELECT confirmed_definition_hash FROM catalog.wiring_activation",
            &[],
        )
        .await
        .expect("read unchanged activation")
        .get(0);
    assert_eq!(actual, hash('a'));
}

async fn retirement_refuses_change_but_keeps_exact_retry(client: &mut Client, args: &PromoteArgs) {
    client
        .execute(
            "INSERT INTO catalog.wiring_tombstones
        (tenant_id,package_id,environment,wiring_id,reason)
        VALUES ('t1','shop','prod','orders-create','test retirement')",
            &[],
        )
        .await
        .expect("retire wiring");
    let tx = begin(client).await;
    assert!(
        !activate_once(&tx, args, "shop", "orders-create", &hash('a'))
            .await
            .expect("existing activation is unchanged")
    );
    let error = activate_once(&tx, args, "shop", "orders-create", &hash('b'))
        .await
        .expect_err("retired wiring");
    assert_eq!(
        error
            .downcast_ref::<WiringActivationError>()
            .expect("Rust refusal")
            .kind(),
        WiringActivationErrorKind::Tombstoned
    );
    tx.rollback().await.expect("rollback retirement refusal");
    assert_eq!(counts(client).await, (1, 1));
}

async fn concurrent_changes_keep_serializable_refusal(client: &mut Client, url: &str) {
    // The second test environment has no retirement record.
    client
        .batch_execute(
            "INSERT INTO catalog.effective_releases
        (tenant_id,effective_release_id,environment,verified_publisher_principal)
        VALUES ('t1',2,'test','spiffe://wamn.test/publisher');
        INSERT INTO catalog.effective_release_packages
        (tenant_id,effective_release_id,package_id,package_version) VALUES ('t1',2,'shop','1.0.0');
        INSERT INTO catalog.effective_release_heads
        (tenant_id,environment,effective_release_id) VALUES ('t1','test',2);",
        )
        .await
        .expect("seed second environment head");
    let mut first_args = self::args(url);
    first_args.target_environment = "test".to_owned();
    let tx = begin(client).await;
    assert!(
        activate_once(&tx, &first_args, "shop", "orders-create", &hash('a'))
            .await
            .expect("first writer")
    );
    let mut other = connect(url).await;
    let pid: i32 = other
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("second backend")
        .get(0);
    let mut second_args = self::args(url);
    second_args.target_environment = "test".to_owned();
    let second = tokio::spawn(async move {
        let tx = begin(&mut other).await;
        let result = activate_once(&tx, &second_args, "shop", "orders-create", &hash('b')).await;
        tx.rollback().await.expect("rollback competing writer");
        result
    });
    let observer = connect(url).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = observer
                .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                .await
                .expect("observe blocked writer")
                .get(0);
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second writer waits for the first transaction");
    tx.commit().await.expect("commit first writer");
    let error = second
        .await
        .expect("second task")
        .expect_err("concurrent writer must retry transaction");
    let database_error = error
        .downcast_ref::<tokio_postgres::Error>()
        .expect("real database refusal");
    assert_eq!(
        database_error.code(),
        Some(&tokio_postgres::error::SqlState::T_R_SERIALIZATION_FAILURE)
    );
    assert_eq!(counts(client).await, (2, 2));
}

#[tokio::test]
#[ignore = "requires WAMN_CTL_PG_URL for a disposable PostgreSQL database"]
async fn promotion_activation_retains_decisions_and_transaction_boundaries() {
    let url =
        std::env::var("WAMN_CTL_PG_URL").expect("set WAMN_CTL_PG_URL for a disposable database");
    let mut client = connect(&url).await;
    fresh_catalog(&client).await;
    let args = args(&url);
    refuses_unreleased_absent_and_foreign_definitions(&mut client, &args).await;
    commit_retry_and_rollback_keep_one_history(&mut client, &args).await;
    retirement_refuses_change_but_keeps_exact_retry(&mut client, &args).await;
    concurrent_changes_keep_serializable_refusal(&mut client, &url).await;
}

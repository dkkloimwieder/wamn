//! Live test of the release reconciliation of authored role permissions
//! (docs/plan/platform-ui.md §4.1, wamn-a40n.1).
//!
//! An authored role selects roots in release A. Release B bumps the package
//! version, adds and removes a dependency, removes one operation and removes a
//! whole package. The test holds the process lock of its server, because the
//! schema installer creates cluster-wide roles.

use std::collections::{BTreeMap, BTreeSet};

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ArtifactHash, ServingComponent, ServingComponentOperation};
use wamn_control::role_permissions::{
    ReleaseClosures, ReleasePermissionOutcome, reconcile_release_permissions,
};
use wamn_test_infrastructure::locked_database;

const RECORD_HISTORY: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";

/// One component of `package` whose exports register the given operations,
/// each with its folded call-graph permissions.
fn component(package: &str, operations: &[(&str, &[&str])]) -> ServingComponent {
    ServingComponent {
        package_id: package.to_owned(),
        component: package.to_owned(),
        interface_version: "1".to_owned(),
        digest: ArtifactHash::parse(format!("sha256:{}", "0".repeat(64))).unwrap(),
        operations: operations
            .iter()
            .enumerate()
            .map(|(index, (sealed, required))| {
                (
                    format!("export-{index}"),
                    ServingComponentOperation {
                        pre_commit: None,
                        registered_operation: Some((*sealed).to_owned()),
                        permissions: std::iter::once(*sealed)
                            .chain(required.iter().copied())
                            .map(str::to_owned)
                            .collect(),
                        fresh_only: false,
                        committed_result_schema: None,
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )
            })
            .collect(),
    }
}

async fn reconcile(client: &mut Client, closures: &ReleaseClosures) -> ReleasePermissionOutcome {
    let tx = client.transaction().await.unwrap();
    let outcome = reconcile_release_permissions(&tx, "t1", closures)
        .await
        .expect("reconcile the candidate release");
    tx.commit().await.unwrap();
    outcome
}

async fn rows(client: &Client) -> BTreeSet<String> {
    client
        .query(
            "SELECT permission || ' by ' || required_by FROM app_system.permissions \
             WHERE tenant_id = 't1' AND role_name = 'clerk'",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[tokio::test]
async fn authored_roles_follow_the_candidate_release() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (mut client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOINHERIT NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             {RECORD_HISTORY}\n{RECORD_HISTORY_APP_GRANTS}\n{APP_SCHEMA}"
        ))
        .await
        .expect("install the application schema");
    // clerk selects four roots and one reference that no release serves.
    client
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{PROVISIONING}', true), \
               set_config('app.operation', 'admin:release-permission-fixture', true); \
             INSERT INTO app_system.roles (tenant_id, name) VALUES ('t1', 'clerk'); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
             SELECT 't1', 'clerk', root, root FROM unnest(ARRAY['shop:order/create', \
               'shop:report/run', 'shop:line/list', 'other:thing/get', 'gone:thing/get']) AS root; \
             COMMIT;"
        ))
        .await
        .expect("seed the selected roots");

    // Release A: order/create requires order/get and line/list, and
    // report/run shares line/list.
    let release_a = ReleaseClosures::from_components(&[
        component(
            "shop",
            &[
                (
                    "shop:order/create@1.0.0",
                    &["shop:order/get@1.0.0", "shop:line/list@1.0.0"],
                ),
                ("shop:order/get@1.0.0", &[]),
                ("shop:line/list@1.0.0", &[]),
                ("shop:report/run@1.0.0", &["shop:line/list@1.0.0"]),
            ],
        ),
        component("other", &[("other:thing/get@3.0.0", &[])]),
    ]);
    let outcome = reconcile(&mut client, &release_a).await;
    assert_eq!(
        outcome,
        ReleasePermissionOutcome {
            roots_removed: 1,
            required_added: 3,
            required_removed: 0,
        }
    );
    assert_eq!(
        rows(&client).await,
        set(&[
            "shop:order/create by shop:order/create",
            "shop:order/get by shop:order/create",
            "shop:line/list by shop:order/create",
            "shop:report/run by shop:report/run",
            "shop:line/list by shop:report/run",
            "shop:line/list by shop:line/list",
            "other:thing/get by other:thing/get",
        ]),
        "release A: the unserved root goes and each root takes its closure"
    );

    // Release B: shop moves to 1.1.0, order/create adds order/audit and drops
    // line/list, report/run is gone, and the package other is gone.
    let release_b = ReleaseClosures::from_components(&[component(
        "shop",
        &[
            (
                "shop:order/create@1.1.0",
                &["shop:order/get@1.1.0", "shop:order/audit@1.1.0"],
            ),
            ("shop:order/get@1.1.0", &[]),
            ("shop:order/audit@1.1.0", &[]),
            ("shop:line/list@1.1.0", &[]),
        ],
    )]);
    let outcome = reconcile(&mut client, &release_b).await;
    assert_eq!(
        outcome,
        ReleasePermissionOutcome {
            roots_removed: 2,
            required_added: 1,
            required_removed: 1,
        }
    );
    assert_eq!(
        rows(&client).await,
        set(&[
            "shop:order/create by shop:order/create",
            "shop:order/get by shop:order/create",
            "shop:order/audit by shop:order/create",
            "shop:line/list by shop:line/list",
        ]),
        "release B: the version bump keeps order/create, the added dependency \
         arrives, the dropped one goes, line/list stays through its own selection, \
         and the removed operation and package lose their roots and closures"
    );

    assert_eq!(
        reconcile(&mut client, &release_b).await,
        ReleasePermissionOutcome::default(),
        "a second reconciliation of the same release changes nothing"
    );
}

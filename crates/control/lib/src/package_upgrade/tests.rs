use std::collections::BTreeMap;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentSqlStatement, PackageCoordinate, ServingManifest};

use super::{
    AcceptedUpgrade, matching_reconciliation_evidence, persist, read_accepted, require_prefix,
};
use crate::qualify_upgrade::{PackageIdentity, UpgradeQualification, read_current_packages};
use crate::reconcile_package_data_access::upgrade::read_upgrade_privileges;

mod vector {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs"
    ));
}

fn predecessor_manifest() -> ServingManifest {
    let (mut manifest, digest) =
        ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
    assert_eq!(digest.as_str(), vector::DIGEST);
    manifest.release.packages =
        [PackageCoordinate::new("platform_fixture", "2.0.0").unwrap()].into();
    let mut component = manifest.components.iter().next().unwrap().clone();
    component.package_id = "platform_fixture".to_owned();
    component.operations.values_mut().next().unwrap().statements = BTreeMap::from([(
        format!("sha256:{}", "1".repeat(64)),
        ComponentSqlStatement {
            name: "read".to_owned(),
            path: "queries/read.sql".to_owned(),
            sql: "SELECT to_jsonb(widget) FROM widget".to_owned(),
            binds: Vec::new(),
            columns: Vec::new(),
            transactional: false,
        },
    )]);
    manifest.components = [component].into();
    manifest
}

fn assert_complete_world_selection(original: &AcceptedUpgrade) {
    let mut first = original.clone();
    let mut second_old = first.evidence.predecessor_package.clone();
    second_old.package_id = "second_fixture".to_owned();
    let mut second_new = first.evidence.candidate_package.clone();
    second_new.package_id = second_old.package_id.clone();
    first.evidence.predecessor_packages.push(second_old.clone());
    first.evidence.presented_packages.push(second_old.clone());
    let mut second = first.clone();
    second.evidence.predecessor_packages = first.evidence.presented_packages.clone();
    second.evidence.predecessor_package = second_old;
    second.evidence.candidate_package = second_new.clone();
    second.evidence.presented_packages[1] = second_new;
    let installed = &second.evidence.presented_packages;
    let roots = &second.evidence.presented_packages;
    let chosen =
        matching_reconciliation_evidence(&[first.clone(), second.clone()], installed, roots)
            .unwrap();
    assert_eq!(
        chosen, second.evidence,
        "the complete later world supersedes earlier proof without timestamps"
    );
    assert!(matching_reconciliation_evidence(&[first], installed, roots).is_err());
    let mut changed_roots = roots.clone();
    changed_roots[0].artifact_digest = format!("sha256:{}", "4".repeat(64));
    assert!(
        matching_reconciliation_evidence(&[second.clone()], installed, &changed_roots).is_err()
    );
    let mut conflicting = second.clone();
    conflicting
        .evidence
        .post_privileges
        .schema
        .insert(("inventory".to_owned(), "CREATE".to_owned()));
    assert!(
        matching_reconciliation_evidence(&[second.clone(), conflicting], installed, roots).is_err()
    );
}

async fn seed(client: &mut Client, manifest: &ServingManifest) -> AcceptedUpgrade {
    client
        .batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;")
        .await
        .unwrap();
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .unwrap();
    client
        .batch_execute(&format!(
            "SELECT set_config('app.user_id', '{}', false)",
            wamn_control_provision::PlatformComponent::ApplyPackage.principal_id()
        ))
        .await
        .unwrap();
    client.batch_execute(
        "CREATE SCHEMA inventory; \
         CREATE TABLE inventory.widget (id int, note text); \
         CREATE TABLE inventory.successor_only (id int); \
         GRANT USAGE ON SCHEMA inventory TO wamn_app; \
         GRANT SELECT (id,note) ON inventory.widget TO wamn_app; \
         INSERT INTO catalog.packages (env_key,package_id,package_version,predecessor_version,artifact_digest) VALUES \
           ('upgrade-test','platform_fixture','2.0.0',NULL,'sha256:' || repeat('a',64)), \
           ('upgrade-test','platform_fixture','2.1.0','2.0.0','sha256:' || repeat('b',64)); \
         INSERT INTO catalog.package_migrations (env_key,package_id,package_version,ordinal,relative_path,sha256) VALUES \
           ('upgrade-test','platform_fixture','2.0.0',1,'migrations/0001_initial.sql','sha256:' || repeat('c',64)), \
           ('upgrade-test','platform_fixture','2.1.0',1,'migrations/0001_initial.sql','sha256:' || repeat('c',64)), \
           ('upgrade-test','platform_fixture','2.1.0',2,'migrations/0002_successor.sql','sha256:' || repeat('d',64));",
    ).await.unwrap();
    client
        .execute(
            "INSERT INTO catalog.releases (env_key,manifest_digest,canonical_bytes) \
         VALUES ('upgrade-test',$1,$2)",
            &[&manifest.digest().as_str(), &manifest.canonical_bytes()],
        )
        .await
        .unwrap();
    let tx = client.transaction().await.unwrap();
    let candidate = read_current_packages(&tx, "upgrade-test")
        .await
        .unwrap()
        .remove(0);
    let predecessor = PackageIdentity {
        package_version: "2.0.0".to_owned(),
        predecessor_version: None,
        artifact_digest: format!("sha256:{}", "a".repeat(64)),
        migrations: candidate.migrations[..1].to_vec(),
        ..candidate.clone()
    };
    let schemas = vec!["inventory".to_owned()];
    let privileges = read_upgrade_privileges(&tx, &schemas).await.unwrap();
    tx.rollback().await.unwrap();
    let evidence = UpgradeQualification {
        overlay: None,
        upgrade_stage: None,
        format_version: crate::qualify_upgrade::PLAIN_FORMAT,
        env_key: "upgrade-test".to_owned(),
        environment: "test".to_owned(),
        predecessor_manifest_digest: manifest.digest().as_str().to_owned(),
        predecessor_package: predecessor.clone(),
        candidate_package: candidate.clone(),
        candidate_suffix: candidate.migrations[1..].to_vec(),
        predecessor_packages: vec![predecessor],
        presented_packages: vec![candidate],
        schemas,
        predecessor_privileges: privileges.clone(),
        post_privileges: privileges,
    };
    let value = serde_json::to_value(&evidence).unwrap();
    AcceptedUpgrade {
        evidence,
        bytes: wamn_execution_contract::canonical_json_bytes(&value),
        sha256: wamn_execution_contract::canonical_json_sha256(&value),
    }
}

#[tokio::test]
async fn persisted_immediate_predecessor_requires_exact_post_state_and_live_planning() {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("upgrade_acceptance").unwrap();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    let manifest = predecessor_manifest();
    let accepted = seed(&mut client, &manifest).await;
    assert_complete_world_selection(&accepted);

    let tx = client.transaction().await.unwrap();
    persist(&tx, &accepted, &format!("sha256:{}", "f".repeat(64)))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let tx = client.transaction().await.unwrap();
    assert!(
        read_accepted(&tx, "upgrade-test", "platform_fixture", "2.1.0")
            .await
            .unwrap()
            .is_none()
    );
    persist(&tx, &accepted, &format!("sha256:{}", "f".repeat(64)))
        .await
        .unwrap();
    let role: String = tx
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        role, "postgres",
        "SQL proof restores the enclosing transaction's role"
    );
    let stamped: bool = tx.query_one(
        "SELECT created_by = $1::text::uuid AND updated_by = $1::text::uuid AND created_at = updated_at FROM catalog.package_upgrade_qualifications",
        &[&wamn_control_provision::PlatformComponent::ApplyPackage.principal_id().to_string()],
    ).await.unwrap().get(0);
    assert!(stamped);
    tx.commit().await.unwrap();

    let mut changed = accepted.evidence.clone();
    changed.predecessor_package.migrations[0].sha256 = format!("sha256:{}", "e".repeat(64));
    assert!(
        require_prefix(&changed).is_err(),
        "a non-prefix predecessor is never rollback evidence"
    );

    let tx = client.transaction().await.unwrap();
    assert_eq!(
        read_upgrade_privileges(&tx, &accepted.evidence.schemas)
            .await
            .unwrap(),
        accepted.evidence.post_privileges
    );
    tx.rollback().await.unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    server.stop().unwrap();
}

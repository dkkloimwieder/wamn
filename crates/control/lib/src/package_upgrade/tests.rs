use std::collections::BTreeMap;
use std::path::PathBuf;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentSqlStatement, EffectiveReleaseId, PackageCoordinate, ServingManifest};

use super::{
    AcceptedUpgrade, matching_reconciliation_evidence, persist, read_accepted,
    require_compatible_schema, require_prefix,
};
use crate::qualify_upgrade::workload::{
    ObjectIdentity, PackageWorkload, ServingWorkloads, WorkloadTarget,
};
use crate::qualify_upgrade::{
    PackageIdentity, PresentedRootIdentity, UpgradeQualification, read_current_packages,
};
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
    manifest.release.tenant_id = "upgrade-test".to_owned();
    manifest.release.environment = "test".to_owned();
    manifest.release.effective_release_id = EffectiveReleaseId::new(1).unwrap();
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

fn object() -> ObjectIdentity {
    ObjectIdentity {
        name: "fixture".to_owned(),
        uid: "fixture-uid".to_owned(),
        generation: 1,
        spec_sha256: format!("sha256:{}", "a".repeat(64)),
    }
}

fn root_identity(package: PackageIdentity) -> PresentedRootIdentity {
    PresentedRootIdentity {
        package,
        statement_corpus_sha256: format!("sha256:{}", "1".repeat(64)),
        package_identity_sha256: format!("sha256:{}", "2".repeat(64)),
        data_access_sha256: format!("sha256:{}", "3".repeat(64)),
    }
}

fn assert_complete_world_selection(original: &AcceptedUpgrade) {
    let mut first = original.clone();
    let mut second_old = first.evidence.predecessor_package.clone();
    second_old.package_id = "second_fixture".to_owned();
    let mut second_new = first.evidence.candidate_package.clone();
    second_new.package_id = second_old.package_id.clone();
    first.evidence.predecessor_packages.push(second_old.clone());
    first.evidence.presented_packages.push(second_old.clone());
    first.evidence.presented_roots = first
        .evidence
        .presented_packages
        .iter()
        .cloned()
        .map(root_identity)
        .collect();
    let mut second = first.clone();
    second.evidence.predecessor_packages = first.evidence.presented_packages.clone();
    second.evidence.predecessor_package = second_old;
    second.evidence.candidate_package = second_new.clone();
    second.evidence.presented_packages[1] = second_new;
    second.evidence.presented_roots = second
        .evidence
        .presented_packages
        .iter()
        .cloned()
        .map(root_identity)
        .collect();
    let installed = &second.evidence.presented_packages;
    let roots = &second.evidence.presented_roots;
    let chosen =
        matching_reconciliation_evidence(&[first.clone(), second.clone()], installed, roots)
            .unwrap();
    assert_eq!(
        chosen, second.evidence,
        "the complete later world supersedes earlier proof without timestamps"
    );
    assert!(matching_reconciliation_evidence(&[first], installed, roots).is_err());
    let mut changed_roots = roots.clone();
    changed_roots[0].statement_corpus_sha256 = format!("sha256:{}", "4".repeat(64));
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
    client.batch_execute(
        "CREATE SCHEMA inventory; \
         CREATE TABLE inventory.widget (id int, note text); \
         CREATE TABLE inventory.successor_only (id int); \
         GRANT USAGE ON SCHEMA inventory TO wamn_app; \
         GRANT SELECT (id,note) ON inventory.widget TO wamn_app; \
         INSERT INTO catalog.packages (tenant_id,package_id,package_version,predecessor_version,manifest_sha256) VALUES \
           ('upgrade-test','platform_fixture','2.0.0',NULL,'sha256:' || repeat('a',64)), \
           ('upgrade-test','platform_fixture','2.1.0','2.0.0','sha256:' || repeat('b',64)); \
         INSERT INTO catalog.package_migrations (tenant_id,package_id,package_version,ordinal,relative_path,sha256) VALUES \
           ('upgrade-test','platform_fixture','2.0.0',1,'migrations/0001_initial.sql','sha256:' || repeat('c',64)), \
           ('upgrade-test','platform_fixture','2.1.0',1,'migrations/0001_initial.sql','sha256:' || repeat('c',64)), \
           ('upgrade-test','platform_fixture','2.1.0',2,'migrations/0002_successor.sql','sha256:' || repeat('d',64)); \
         INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment) VALUES ('upgrade-test',1,'test');",
    ).await.unwrap();
    client.execute(
        "INSERT INTO catalog.release_manifest_snapshots (tenant_id,effective_release_id,manifest_digest,canonical_bytes) \
         VALUES ('upgrade-test',1,$1,$2)",
        &[&manifest.digest().as_str(), &manifest.canonical_bytes()],
    ).await.unwrap();
    let tx = client.transaction().await.unwrap();
    let candidate = read_current_packages(&tx, "upgrade-test")
        .await
        .unwrap()
        .remove(0);
    let predecessor = PackageIdentity {
        package_version: "2.0.0".to_owned(),
        predecessor_version: None,
        manifest_sha256: format!("sha256:{}", "a".repeat(64)),
        migrations: candidate.migrations[..1].to_vec(),
        ..candidate.clone()
    };
    let schemas = vec!["inventory".to_owned()];
    let privileges = read_upgrade_privileges(&tx, &schemas).await.unwrap();
    tx.rollback().await.unwrap();
    let evidence = UpgradeQualification {
        overlay: None,
        format_version: 1,
        tenant: "upgrade-test".to_owned(),
        environment: "test".to_owned(),
        predecessor_release_id: 1,
        predecessor_manifest_digest: manifest.digest().as_str().to_owned(),
        predecessor_package: predecessor.clone(),
        candidate_package: candidate.clone(),
        candidate_suffix: candidate.migrations[1..].to_vec(),
        predecessor_packages: vec![predecessor],
        presented_packages: vec![candidate],
        presented_roots: Vec::new(),
        schemas,
        predecessor_privileges: privileges.clone(),
        post_privileges: privileges,
        workload_target: WorkloadTarget {
            kubeconfig: PathBuf::from("unused"),
            context: "test".to_owned(),
            namespace: "test".to_owned(),
            host_deployment: "host".to_owned(),
            package_workloads: BTreeMap::new(),
        },
        serving_workloads: ServingWorkloads {
            host_deployment: object(),
            manifest_digest: manifest.digest().as_str().to_owned(),
            packages: BTreeMap::from([(
                "platform_fixture".to_owned(),
                PackageWorkload {
                    deployment: object(),
                    replica_set: object(),
                    workloads: BTreeMap::new(),
                    schema: "inventory".to_owned(),
                },
            )]),
        },
    };
    let value = serde_json::to_value(&evidence).unwrap();
    AcceptedUpgrade {
        evidence,
        bytes: wamn_execution_contract::canonical_json_bytes(&value),
        sha256: wamn_execution_contract::canonical_json_sha256(&value),
        observed_workloads: None,
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

    let mut tx = client.transaction().await.unwrap();
    let refusal = require_compatible_schema(&mut tx, &manifest)
        .await
        .unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("persisted immediate-predecessor")
    );
    tx.rollback().await.unwrap();
    let tx = client.transaction().await.unwrap();
    persist(&tx, &accepted).await.unwrap();
    tx.rollback().await.unwrap();
    let mut tx = client.transaction().await.unwrap();
    assert!(
        read_accepted(&tx, "upgrade-test", "platform_fixture", "2.1.0")
            .await
            .unwrap()
            .is_none()
    );
    persist(&tx, &accepted).await.unwrap();
    require_compatible_schema(&mut tx, &manifest)
        .await
        .expect("persisted proof admits its immediate predecessor");
    let role: String = tx
        .query_one("SELECT current_user::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        role, "postgres",
        "SQL proof restores the enclosing transaction's role"
    );
    tx.commit().await.unwrap();

    let mut changed_release = manifest.clone();
    changed_release.release.effective_release_id = EffectiveReleaseId::new(2).unwrap();
    let mut tx = client.transaction().await.unwrap();
    assert!(
        require_compatible_schema(&mut tx, &changed_release)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let mut changed = accepted.evidence.clone();
    changed.predecessor_package.migrations[0].sha256 = format!("sha256:{}", "e".repeat(64));
    assert!(
        require_prefix(&changed).is_err(),
        "a non-prefix predecessor is never rollback evidence"
    );

    client
        .batch_execute("REVOKE SELECT (note) ON inventory.widget FROM wamn_app")
        .await
        .unwrap();
    let mut tx = client.transaction().await.unwrap();
    assert!(
        require_compatible_schema(&mut tx, &manifest)
            .await
            .unwrap_err()
            .to_string()
            .contains("data-access post-state changed")
    );
    tx.rollback().await.unwrap();
    client.batch_execute("GRANT SELECT (note) ON inventory.widget TO wamn_app; ALTER TABLE inventory.widget ADD COLUMN ungranted text").await.unwrap();
    let mut tx = client.transaction().await.unwrap();
    assert_eq!(
        read_upgrade_privileges(&tx, &accepted.evidence.schemas)
            .await
            .unwrap(),
        accepted.evidence.post_privileges
    );
    let refusal = require_compatible_schema(&mut tx, &manifest)
        .await
        .unwrap_err();
    assert!(
        format!("{refusal:#}").contains("retained schema"),
        "{refusal:#}"
    );
    tx.rollback().await.unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    server.stop().unwrap();
}

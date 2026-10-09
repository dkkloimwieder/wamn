//! Explicit abandonment of an existing backfill without executing package SQL.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use wamn_schema_generator::UpgradeStagePhase;

use super::{
    ApplyPackageRequest, CLAIM_TENANT_SQL, LOCK_PACKAGE_SQL, PreparedPackage,
    bind_apply_package_principal, current_package_version, prepare_package,
};
use crate::package_upgrade::progress::{self, StageIdentity};
use crate::qualify_upgrade::{self, UpgradeQualification};

/// Retained progress after a stage is explicitly abandoned.
#[derive(Debug)]
pub struct AbandonStageOutcome {
    pub package_id: String,
    pub package_version: String,
    pub installed_version: String,
    pub cursor: Value,
    pub completed_batches: i64,
}

/// Abandon an existing backfill identified by exact package and qualification bytes.
///
/// This changes only the recorded stage status. It neither observes workloads
/// nor executes package SQL. An exact repeat returns the retained cursor.
///
/// # Errors
/// Refuses changed identities, a changed installed predecessor, absent progress,
/// or a completed stage.
pub async fn abandon_stage(
    request: ApplyPackageRequest,
    evidence_path: &Path,
) -> anyhow::Result<AbandonStageOutcome> {
    ensure!(!request.tenant.is_empty(), "tenant must not be empty");
    let package = prepare_package(&request.package)?;
    let (evidence, _, digest) = qualify_upgrade::read_qualification(evidence_path)?;
    require_exact_stage(&request.tenant, &package, &evidence)?;
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect to project environment for stage abandonment")?;
    let task = tokio::spawn(connection);
    let result = abandon_existing(&mut client, &request.tenant, &package, &evidence, &digest).await;
    drop(client);
    if result.is_err() {
        task.abort();
    } else {
        task.await
            .context("join stage abandonment connection")?
            .context("drive stage abandonment connection")?;
    }
    result
}

fn require_exact_stage(
    tenant: &str,
    package: &PreparedPackage,
    evidence: &UpgradeQualification,
) -> anyhow::Result<()> {
    let stage = package
        .manifest
        .upgrade_stage
        .as_ref()
        .context("abandonment requires a backfill stage")?;
    ensure!(
        stage.phase == UpgradeStagePhase::Backfill,
        "only a backfill stage can be abandoned"
    );
    ensure!(
        evidence.tenant == tenant,
        "abandonment qualification tenant mismatch"
    );
    ensure!(
        evidence.upgrade_stage.as_ref() == Some(stage),
        "abandonment qualification stage mismatch"
    );
    let candidate = qualify_upgrade::identity_from_directory(&package.directory)?;
    ensure!(
        evidence.candidate_package == candidate,
        "abandonment qualification candidate package or migration bytes mismatch"
    );
    ensure!(
        evidence.predecessor_package.package_id == candidate.package_id
            && candidate.predecessor_version.as_deref()
                == Some(evidence.predecessor_package.package_version.as_str()),
        "abandonment qualification predecessor mismatch"
    );
    Ok(())
}

async fn abandon_existing(
    client: &mut Client,
    tenant: &str,
    package: &PreparedPackage,
    evidence: &UpgradeQualification,
    digest: &str,
) -> anyhow::Result<AbandonStageOutcome> {
    let candidate = &evidence.candidate_package;
    let tx = client
        .transaction()
        .await
        .context("begin stage abandonment")?;
    tx.batch_execute("SET LOCAL lock_timeout = '5s'").await?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant]).await?;
    bind_apply_package_principal(&tx).await?;
    tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
        .await?;
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &candidate.package_id])
        .await?;
    let installed_version = current_package_version(&tx, tenant, &candidate.package_id)
        .await?
        .context("abandonment requires its installed predecessor")?;
    ensure!(
        installed_version == evidence.predecessor_package.package_version,
        "abandonment requires unchanged installed predecessor {}@{}; found {}",
        candidate.package_id,
        evidence.predecessor_package.package_version,
        installed_version
    );
    let installed = qualify_upgrade::read_current_packages(&tx, tenant).await?;
    ensure!(
        installed
            .iter()
            .any(|identity| identity == &evidence.predecessor_package),
        "abandonment installed predecessor manifest or migrations changed"
    );
    let identity = StageIdentity {
        tenant_id: tenant.to_owned(),
        package_id: candidate.package_id.clone(),
        package_version: candidate.package_version.clone(),
        predecessor_version: installed_version.clone(),
        package_artifact_digest: crate::package_artifact::package_artifact_digest(&package.root)?,
        predecessor_release_digest: evidence.predecessor_manifest_digest.clone(),
        evidence_digest: digest.to_owned(),
    };
    // Recheck the supplied snapshot immediately before the only durable mutation.
    require_exact_stage(tenant, package, evidence)?;
    let progress = progress::abandon(&tx, &identity).await?;
    tx.commit().await.context("commit stage abandonment")?;
    Ok(AbandonStageOutcome {
        package_id: candidate.package_id.clone(),
        package_version: candidate.package_version.clone(),
        installed_version,
        cursor: progress.cursor,
        completed_batches: progress.completed_batches,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::qualify_upgrade::workload::{ServingWorkloads, WorkloadTarget};
    use crate::reconcile_package_data_access::upgrade::UpgradePrivileges;

    struct Files(PathBuf);
    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_evidence(root: &Path, evidence: &UpgradeQualification) -> (PathBuf, String) {
        let value = serde_json::to_value(evidence).unwrap();
        let bytes = wamn_execution_contract::canonical_json_bytes(&value);
        let digest = wamn_execution_contract::canonical_json_sha256(&value);
        let path = root.join("qualification.json");
        std::fs::write(&path, bytes).unwrap();
        (path, digest)
    }

    #[tokio::test]
    async fn abandonment_requires_exact_bytes_and_preserves_installed_data_and_cursor() {
        const TENANT: &str = "abandon-test";
        let files =
            Files(std::env::temp_dir().join(format!("wamn-abandon-stage-{}", std::process::id())));
        std::fs::create_dir(&files.0).unwrap();
        std::fs::create_dir(files.0.join("migrations")).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_package/base");
        let original = super::super::read_package_directory(&fixture).unwrap();
        let predecessor = qualify_upgrade::identity_from_directory(&original).unwrap();
        let mut manifest: Value = serde_json::from_slice(&original.manifest_bytes).unwrap();
        manifest["package"]["version"] = json!("2.0.0");
        manifest["package"]["predecessor_version"] = json!(predecessor.package_version);
        // Abandonment must never evaluate these conditions or run the batch SQL.
        manifest["upgrade_stage"] = json!({
            "phase":"backfill",
            "preconditions":{"ready":"SELECT false"},
            "postconditions":{"finished":"SELECT false"},
            "backfill":{"sql":"SELECT $1::jsonb AS next_cursor, true AS complete", "batch_size":10,"initial_cursor":null}
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        std::fs::write(files.0.join("wamn.json"), &manifest_bytes).unwrap();
        for migration in &original.migrations {
            std::fs::write(files.0.join(&migration.relative_path), &migration.bytes).unwrap();
        }
        let prepared = prepare_package(&files.0).unwrap();
        let candidate = qualify_upgrade::identity_from_directory(&prepared.directory).unwrap();
        let evidence = UpgradeQualification {
            format_version: 3,
            tenant: TENANT.into(),
            environment: "offline".into(),
            predecessor_manifest_digest: format!("sha256:{}", "a".repeat(64)),
            predecessor_package: predecessor.clone(),
            candidate_package: candidate.clone(),
            candidate_suffix: Vec::new(),
            predecessor_packages: vec![predecessor.clone()],
            presented_packages: vec![candidate.clone()],
            presented_roots: Vec::new(),
            schemas: vec!["inventory".into()],
            predecessor_privileges: UpgradePrivileges::default(),
            post_privileges: UpgradePrivileges::default(),
            workload_target: WorkloadTarget {
                kubeconfig: PathBuf::from("must-not-read"),
                context: "unused".into(),
                namespace: "unused".into(),
                host_deployment: "unused".into(),
                package_workloads: BTreeMap::new(),
            },
            serving_workloads: ServingWorkloads {
                manifest_digest: format!("sha256:{}", "a".repeat(64)),
                packages: BTreeMap::new(),
            },
            overlay: None,
            upgrade_stage: prepared.manifest.upgrade_stage.clone(),
        };
        let (evidence_path, digest) = write_evidence(&files.0, &evidence);
        let identity = StageIdentity {
            tenant_id: TENANT.into(),
            package_id: candidate.package_id.clone(),
            package_version: candidate.package_version.clone(),
            predecessor_version: predecessor.package_version.clone(),
            package_artifact_digest: crate::package_artifact::package_artifact_digest(&files.0)
                .unwrap(),
            predecessor_release_digest: evidence.predecessor_manifest_digest.clone(),
            evidence_digest: digest,
        };
        let mut server = wamn_test_postgres::start(&[]).unwrap();
        let database = server.create_database("abandon_stage").unwrap();
        let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        client
            .batch_execute(
                "CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;",
            )
            .await
            .unwrap();
        client
            .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
            .await
            .unwrap();
        client.execute("INSERT INTO catalog.packages (tenant_id,package_id,package_version,predecessor_version,manifest_sha256) VALUES ($1,$2,$3,$4,$5)",
            &[&TENANT,&predecessor.package_id,&predecessor.package_version,&predecessor.predecessor_version,&predecessor.manifest_sha256]).await.unwrap();
        for migration in &predecessor.migrations {
            let ordinal = i32::try_from(migration.ordinal).unwrap();
            client.execute("INSERT INTO catalog.package_migrations (tenant_id,package_id,package_version,ordinal,relative_path,sha256) VALUES ($1,$2,$3,$4,$5,$6)",
                &[&TENANT,&predecessor.package_id,&predecessor.package_version,&ordinal,&migration.relative_path,&migration.sha256]).await.unwrap();
        }
        client.batch_execute("CREATE TABLE retained (id int PRIMARY KEY, converted bool NOT NULL); INSERT INTO retained VALUES (1,false),(2,false);").await.unwrap();
        let tx = client.transaction().await.unwrap();
        progress::open(&tx, &identity, &Value::Null).await.unwrap();
        tx.execute("UPDATE retained SET converted=true WHERE id=1", &[])
            .await
            .unwrap();
        progress::advance(&tx, &identity, &json!({"id":1}))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let request = || ApplyPackageRequest {
            package: files.0.clone(),
            database_url: database.url().to_owned(),
            tenant: TENANT.into(),
        };

        let mut changed_bytes = manifest_bytes.clone();
        changed_bytes.push(b'\n');
        std::fs::write(files.0.join("wamn.json"), changed_bytes).unwrap();
        assert!(
            abandon_stage(request(), &evidence_path)
                .await
                .unwrap_err()
                .to_string()
                .contains("candidate package or migration bytes mismatch")
        );
        std::fs::write(files.0.join("wamn.json"), &manifest_bytes).unwrap();
        let mut changed_evidence = evidence.clone();
        changed_evidence.environment = "other".into();
        write_evidence(&files.0, &changed_evidence);
        assert!(
            abandon_stage(request(), &evidence_path)
                .await
                .unwrap_err()
                .to_string()
                .contains("evidence_digest mismatch")
        );
        write_evidence(&files.0, &evidence);
        let mut wrong_tenant = request();
        wrong_tenant.tenant = "other".into();
        assert!(
            abandon_stage(wrong_tenant, &evidence_path)
                .await
                .unwrap_err()
                .to_string()
                .contains("tenant mismatch")
        );
        assert_eq!(
            client
                .query_one("SELECT status FROM catalog.package_upgrade_stages", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "in_progress"
        );

        for _ in 0..2 {
            let abandoned = abandon_stage(request(), &evidence_path).await.unwrap();
            assert_eq!(abandoned.installed_version, predecessor.package_version);
            assert_eq!(abandoned.cursor, json!({"id":1}));
            assert_eq!(abandoned.completed_batches, 1);
        }
        let rows = client
            .query("SELECT id,converted FROM retained ORDER BY id", &[])
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| (row.get::<_, i32>(0), row.get::<_, bool>(1)))
                .collect::<Vec<_>>(),
            vec![(1, true), (2, false)]
        );
        let tx = client.transaction().await.unwrap();
        assert_eq!(
            qualify_upgrade::read_current_packages(&tx, TENANT)
                .await
                .unwrap(),
            vec![predecessor]
        );
        assert!(
            progress::open(&tx, &identity, &Value::Null)
                .await
                .unwrap_err()
                .to_string()
                .contains("retained cursor")
        );
        assert!(
            progress::advance(&tx, &identity, &json!({"id":2}))
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            client
                .query_one("SELECT status FROM catalog.package_upgrade_stages", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "abandoned"
        );
        drop(client);
        connection.await.unwrap().unwrap();
    }
}

//! Durable backfill progress owned by the platform, within the caller's transaction.

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio_postgres::{Row, Transaction};

/// Exact immutable identity of one package stage and its accepted qualification:
/// the package artifact digest, the predecessor release digest and the
/// evidence digest (docs/plan/platform-deploy.md §10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StageIdentity {
    pub package_id: String,
    pub package_version: String,
    pub predecessor_version: String,
    pub package_artifact_digest: String,
    pub predecessor_release_digest: String,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageStatus {
    InProgress,
    Abandoned,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StageProgress {
    pub status: StageStatus,
    pub cursor: Value,
    pub completed_batches: i64,
}

/// A package family cannot interleave different unfinished stage versions.
pub(crate) async fn require_no_other_stage(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    version: &str,
    package_artifact_digest: &str,
) -> anyhow::Result<()> {
    let installed: bool = tx
        .query_one(
            "SELECT to_regclass('catalog.package_upgrade_stages') IS NOT NULL",
            &[],
        )
        .await?
        .try_get(0)?;
    if !installed {
        return Ok(());
    }
    for row in tx
        .query(
            "SELECT package_version,cursor,package_artifact_digest FROM catalog.package_upgrade_stages \
         WHERE tenant_id=$1 AND package_id=$2 AND (package_version=$3 OR status='in_progress') \
         ORDER BY package_version",
            &[&tenant, &package, &version],
        )
        .await?
    {
        if row.try_get::<_, String>(0)? == version {
            ensure!(
                row.try_get::<_, String>(2)? == package_artifact_digest,
                "package {package}@{version} differs from its immutable stage artifact; retained cursor {}",
                row.try_get::<_, Value>(1)?
            );
            continue;
        }
        bail!(
            "package {package} has unfinished stage {}; retained cursor {}",
            row.try_get::<_, String>(0)?,
            row.try_get::<_, Value>(1)?
        );
    }
    Ok(())
}

/// Create or lock a stage. A completed retry returns the retained final cursor.
pub(crate) async fn open(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
    initial_cursor: &Value,
) -> anyhow::Result<StageProgress> {
    tx.execute(
        "INSERT INTO catalog.package_upgrade_stages \
         (tenant_id,package_id,package_version,predecessor_version,package_artifact_digest,\
          predecessor_release_digest,evidence_digest,status,cursor) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'in_progress',$8) \
         ON CONFLICT (tenant_id,package_id,package_version) DO NOTHING",
        &[
            &tenant,
            &identity.package_id,
            &identity.package_version,
            &identity.predecessor_version,
            &identity.package_artifact_digest,
            &identity.predecessor_release_digest,
            &identity.evidence_digest,
            initial_cursor,
        ],
    )
    .await
    .context("create package upgrade stage progress")?;
    let progress = locked(tx, tenant, identity).await?;
    ensure!(
        progress.status != StageStatus::Abandoned,
        "package upgrade stage {}@{} was abandoned; retained cursor {}",
        identity.package_id,
        identity.package_version,
        progress.cursor
    );
    Ok(progress)
}

/// Record a committed batch in the same transaction as its application writes.
pub(crate) async fn advance(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
    cursor: &Value,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, tenant, identity).await?;
    require_in_progress(identity, &progress)?;
    let row = tx.query_one(
        "UPDATE catalog.package_upgrade_stages SET cursor=$4, completed_batches=completed_batches+1 \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 \
         RETURNING status,cursor,completed_batches",
        &[&tenant, &identity.package_id, &identity.package_version, cursor],
    ).await.context("record package upgrade batch cursor")?;
    decode(&row)
}

/// Complete a stage after its last batch and postconditions succeed.
/// An exact retry returns the existing result without changing the record.
pub(crate) async fn complete(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, tenant, identity).await?;
    if progress.status == StageStatus::Completed {
        return Ok(progress);
    }
    require_in_progress(identity, &progress)?;
    set_status(tx, tenant, identity, "completed").await
}

/// Abandon a stage and return its retained cursor without changing installed packages.
pub(crate) async fn abandon(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, tenant, identity).await?;
    if progress.status == StageStatus::Abandoned {
        return Ok(progress);
    }
    require_in_progress(identity, &progress)?;
    set_status(tx, tenant, identity, "abandoned").await
}

async fn set_status(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
    status: &str,
) -> anyhow::Result<StageProgress> {
    let row = tx
        .query_one(
            "UPDATE catalog.package_upgrade_stages SET status=$4 \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 \
         RETURNING status,cursor,completed_batches",
            &[
                &tenant,
                &identity.package_id,
                &identity.package_version,
                &status,
            ],
        )
        .await
        .context("record package upgrade stage status")?;
    decode(&row)
}

async fn locked(
    tx: &Transaction<'_>,
    tenant: &str,
    identity: &StageIdentity,
) -> anyhow::Result<StageProgress> {
    let row = tx.query_opt(
        "SELECT predecessor_version,package_artifact_digest,predecessor_release_digest,evidence_digest,\
                status,cursor,completed_batches \
         FROM catalog.package_upgrade_stages \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 FOR UPDATE",
        &[&tenant, &identity.package_id, &identity.package_version],
    ).await.context("lock package upgrade stage progress")?
        .with_context(|| format!("package upgrade stage {}@{} has no recorded cursor", identity.package_id, identity.package_version))?;
    for (field, expected) in [
        ("predecessor_version", &identity.predecessor_version),
        ("package_artifact_digest", &identity.package_artifact_digest),
        (
            "predecessor_release_digest",
            &identity.predecessor_release_digest,
        ),
        ("evidence_digest", &identity.evidence_digest),
    ] {
        let recorded: &str = row.try_get(field)?;
        ensure!(
            recorded == expected,
            "package upgrade stage {}@{} {field} mismatch: recorded {recorded}, presented {expected}",
            identity.package_id,
            identity.package_version
        );
    }
    decode(&row)
}

fn decode(row: &Row) -> anyhow::Result<StageProgress> {
    let status = match row.try_get::<_, &str>("status")? {
        "in_progress" => StageStatus::InProgress,
        "abandoned" => StageStatus::Abandoned,
        "completed" => StageStatus::Completed,
        value => bail!("unknown package upgrade stage status {value}"),
    };
    Ok(StageProgress {
        status,
        cursor: row.try_get("cursor")?,
        completed_batches: row.try_get("completed_batches")?,
    })
}

fn require_in_progress(identity: &StageIdentity, progress: &StageProgress) -> anyhow::Result<()> {
    ensure!(
        progress.status == StageStatus::InProgress,
        "package upgrade stage {}@{} is {:?}; retained cursor {}",
        identity.package_id,
        identity.package_version,
        progress.status,
        progress.cursor
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_postgres::NoTls;
    use wamn_test_infrastructure::record_history::assert_stamp;

    fn identity(version: &str) -> StageIdentity {
        StageIdentity {
            package_id: "fixture".into(),
            package_version: version.into(),
            predecessor_version: "1.0.0".into(),
            package_artifact_digest: format!("sha256:{}", "a".repeat(64)),
            predecessor_release_digest: format!("sha256:{}", "e".repeat(64)),
            evidence_digest: format!("sha256:{}", "b".repeat(64)),
        }
    }

    #[tokio::test]
    async fn migration_installs_matching_carrier_and_guards_platform_updates() {
        let mut server = wamn_test_postgres::start(&[]).unwrap();
        let database = server.create_database("stage_migration").unwrap();
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
        let catalog = "SELECT jsonb_build_object(            'columns', (SELECT jsonb_agg(jsonb_build_array(attname,format_type(atttypid,atttypmod),attnotnull) ORDER BY attnum) FROM pg_attribute WHERE attrelid='catalog.package_upgrade_stages'::regclass AND attnum>0 AND NOT attisdropped AND attname NOT IN ('created_at','created_by','updated_at','updated_by')),            'constraints', (SELECT jsonb_agg(pg_get_constraintdef(oid) ORDER BY conname) FROM pg_constraint WHERE conrelid='catalog.package_upgrade_stages'::regclass AND NOT (contype='n' AND conkey && ARRAY(SELECT attnum FROM pg_attribute WHERE attrelid=conrelid AND attname IN ('created_at','created_by','updated_at','updated_by')))),            'policies', (SELECT jsonb_agg(jsonb_build_array(polname,polroles,pg_get_expr(polqual,polrelid),pg_get_expr(polwithcheck,polrelid)) ORDER BY polname) FROM pg_policy WHERE polrelid='catalog.package_upgrade_stages'::regclass),            'security', (SELECT jsonb_build_array(relrowsecurity,relforcerowsecurity,relowner::regrole::text,relacl::text) FROM pg_class WHERE oid='catalog.package_upgrade_stages'::regclass),            'triggers', (SELECT jsonb_agg(pg_get_triggerdef(oid) ORDER BY tgname) FROM pg_trigger WHERE tgrelid='catalog.package_upgrade_stages'::regclass AND NOT tgisinternal AND tgname <> 'wamn_record_history_stamp'))";
        // Historical migrations retain their prior carrier; stamps belong to fresh databases.
        let fresh: Value = client.query_one(catalog, &[]).await.unwrap().get(0);
        client.batch_execute("DROP TABLE catalog.package_upgrade_stages; DROP FUNCTION catalog.guard_package_upgrade_stage_change(); DROP INDEX catalog.package_definition_owners_synchronization_function; ALTER TABLE catalog.package_upgrade_qualifications DROP COLUMN package_artifact_digest; ALTER TABLE catalog.connection_requirements ADD CONSTRAINT connection_requirements_component_fkey CHECK (true);").await.unwrap();
        for migration in [
            include_str!(
                "../../../../../deploy/sql/migrations/project/0010_package_upgrade_stages.sql"
            ),
            include_str!("../../../../../deploy/sql/migrations/project/0014_stage_digests.sql"),
        ] {
            client.batch_execute(migration).await.unwrap();
        }
        let migrated: Value = client.query_one(catalog, &[]).await.unwrap().get(0);
        assert_eq!(fresh, migrated);
        let identity = identity("2.0.0");
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        open(&tx, "stage-test", &identity, &Value::Null)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        for sql in [
            "UPDATE catalog.package_upgrade_stages SET predecessor_version='0.9.0'",
            "UPDATE catalog.package_upgrade_stages SET package_artifact_digest='sha256:' || repeat('c',64)",
            "UPDATE catalog.package_upgrade_stages SET predecessor_release_digest='sha256:' || repeat('c',64)",
            "UPDATE catalog.package_upgrade_stages SET evidence_digest='sha256:' || repeat('c',64)",
            "UPDATE catalog.package_upgrade_stages SET cursor='1'::jsonb",
            "UPDATE catalog.package_upgrade_stages SET completed_batches=2",
            "DELETE FROM catalog.package_upgrade_stages",
        ] {
            let tx = client.transaction().await.unwrap();
            crate::apply_package::bind_apply_package_principal(&tx)
                .await
                .unwrap();
            let error = tx.batch_execute(sql).await.unwrap_err();
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
            );
            tx.rollback().await.unwrap();
        }
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        complete(&tx, "stage-test", &identity).await.unwrap();
        tx.commit().await.unwrap();
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        let error = tx
            .batch_execute("UPDATE catalog.package_upgrade_stages SET status='in_progress'")
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
        );
        tx.rollback().await.unwrap();
        drop(client);
        connection.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn committed_cursor_resumes_and_interruption_rolls_back_data_and_cursor() {
        let mut server = wamn_test_postgres::start(&[]).unwrap();
        let database = server.create_database("stage_progress").unwrap();
        let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        client.batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN; CREATE ROLE wamn_db_owner NOLOGIN;").await.unwrap();
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
        client.batch_execute("CREATE TABLE retained (id int PRIMARY KEY, converted bool NOT NULL); INSERT INTO retained VALUES (1,false),(2,false); INSERT INTO catalog.packages (tenant_id,package_id,package_version,artifact_digest) VALUES ('stage-test','fixture','1.0.0','sha256:' || repeat('c',64));").await.unwrap();
        let identity = identity("2.0.0");
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        let initial = open(&tx, "stage-test", &identity, &Value::Null)
            .await
            .unwrap();
        assert_eq!(initial.cursor, Value::Null);
        tx.commit().await.unwrap();
        assert_stamp(
            &client,
            "catalog.package_upgrade_stages",
            &format!(
                "tenant_id = 'stage-test' AND package_version = '{}'",
                identity.package_version
            ),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
        )
        .await;

        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        open(&tx, "stage-test", &identity, &json!("ignored on resume"))
            .await
            .unwrap();
        tx.execute("UPDATE retained SET converted=true WHERE id=1", &[])
            .await
            .unwrap();
        advance(&tx, "stage-test", &identity, &json!({"id":1}))
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        assert_eq!(
            open(&tx, "stage-test", &identity, &Value::Null)
                .await
                .unwrap(),
            initial
        );
        assert!(
            !tx.query_one("SELECT converted FROM retained WHERE id=1", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        tx.execute("UPDATE retained SET converted=true WHERE id=1", &[])
            .await
            .unwrap();
        let committed = advance(&tx, "stage-test", &identity, &json!({"id":1}))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_stamp(
            &client,
            "catalog.package_upgrade_stages",
            &format!(
                "tenant_id = 'stage-test' AND package_version = '{}'",
                identity.package_version
            ),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
        )
        .await;
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        assert_eq!(
            open(&tx, "stage-test", &identity, &Value::Null)
                .await
                .unwrap(),
            committed
        );
        assert_eq!(committed.completed_batches, 1);
        tx.commit().await.unwrap();
        assert_stamp(
            &client,
            "catalog.package_upgrade_stages",
            &format!(
                "tenant_id = 'stage-test' AND package_version = '{}'",
                identity.package_version
            ),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
        )
        .await;

        for field in [
            "package_artifact_digest",
            "predecessor_release_digest",
            "evidence_digest",
            "predecessor_version",
        ] {
            let mut changed = identity.clone();
            let other = format!("sha256:{}", "d".repeat(64));
            match field {
                "package_artifact_digest" => changed.package_artifact_digest = other,
                "predecessor_release_digest" => changed.predecessor_release_digest = other,
                "evidence_digest" => changed.evidence_digest = other,
                _ => changed.predecessor_version = "0.9.0".into(),
            }
            let tx = client.transaction().await.unwrap();
            crate::apply_package::bind_apply_package_principal(&tx)
                .await
                .unwrap();
            assert!(
                open(&tx, "stage-test", &changed, &Value::Null)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(field)
            );
            tx.rollback().await.unwrap();
        }
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        let abandoned = abandon(&tx, "stage-test", &identity).await.unwrap();
        assert_eq!(abandoned.cursor, committed.cursor);
        tx.commit().await.unwrap();
        assert_stamp(
            &client,
            "catalog.package_upgrade_stages",
            &format!(
                "tenant_id = 'stage-test' AND package_version = '{}'",
                identity.package_version
            ),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
        )
        .await;
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        assert!(
            open(&tx, "stage-test", &identity, &Value::Null)
                .await
                .unwrap_err()
                .to_string()
                .contains("retained cursor {\"id\":1}")
        );
        assert!(
            advance(&tx, "stage-test", &identity, &json!({"id":2}))
                .await
                .is_err()
        );
        assert!(complete(&tx, "stage-test", &identity).await.is_err());
        assert_eq!(
            abandon(&tx, "stage-test", &identity).await.unwrap(),
            abandoned
        );
        assert_eq!(
            tx.query_one(
                "SELECT package_version FROM catalog.packages WHERE tenant_id='stage-test'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
            "1.0.0"
        );
        tx.rollback().await.unwrap();

        let completed_identity = self::identity("3.0.0");
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        open(&tx, "stage-test", &completed_identity, &Value::Null)
            .await
            .unwrap();
        advance(&tx, "stage-test", &completed_identity, &json!({"id":2}))
            .await
            .unwrap();
        let completed = complete(&tx, "stage-test", &completed_identity)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_stamp(
            &client,
            "catalog.package_upgrade_stages",
            &format!(
                "tenant_id = 'stage-test' AND package_version = '{}'",
                completed_identity.package_version
            ),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::ApplyPackage
                .principal_id()
                .to_string(),
        )
        .await;
        let tx = client.transaction().await.unwrap();
        crate::apply_package::bind_apply_package_principal(&tx)
            .await
            .unwrap();
        assert_eq!(
            open(&tx, "stage-test", &completed_identity, &Value::Null)
                .await
                .unwrap(),
            completed
        );
        assert_eq!(
            complete(&tx, "stage-test", &completed_identity)
                .await
                .unwrap(),
            completed
        );
        assert!(
            advance(&tx, "stage-test", &completed_identity, &Value::Null)
                .await
                .is_err()
        );
        assert!(
            abandon(&tx, "stage-test", &completed_identity)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();

        client
            .batch_execute("GRANT USAGE ON SCHEMA catalog TO wamn_db_owner")
            .await
            .unwrap();
        for role in ["wamn_db_owner", "wamn_app"] {
            for sql in [
                "UPDATE catalog.package_upgrade_stages SET cursor='null'::jsonb",
                "DELETE FROM catalog.package_upgrade_stages",
                "INSERT INTO catalog.package_upgrade_stages SELECT * FROM catalog.package_upgrade_stages",
                "TRUNCATE catalog.package_upgrade_stages",
                "ALTER TABLE catalog.package_upgrade_stages DISABLE ROW LEVEL SECURITY",
            ] {
                let tx = client.transaction().await.unwrap();
                crate::apply_package::bind_apply_package_principal(&tx)
                    .await
                    .unwrap();
                tx.batch_execute(&format!("SET LOCAL ROLE {role}"))
                    .await
                    .unwrap();
                let error = tx.batch_execute(sql).await.unwrap_err();
                assert_eq!(
                    error.code(),
                    Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
                    "{role}: {sql}"
                );
                tx.rollback().await.unwrap();
            }
        }
        drop(client);
        connection.await.unwrap().unwrap();
    }
}

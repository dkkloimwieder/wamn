//! Durable backfill progress owned by the platform, within the caller's transaction.

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio_postgres::{Row, Transaction};

/// Exact immutable identity of one package stage and its accepted qualification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StageIdentity {
    pub tenant_id: String,
    pub package_id: String,
    pub package_version: String,
    pub predecessor_version: String,
    pub manifest_sha256: String,
    pub qualification_sha256: String,
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
    manifest_sha256: &str,
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
            "SELECT package_version,cursor,manifest_sha256 FROM catalog.package_upgrade_stages \
         WHERE tenant_id=$1 AND package_id=$2 AND (package_version=$3 OR status='in_progress') \
         ORDER BY package_version",
            &[&tenant, &package, &version],
        )
        .await?
    {
        if row.try_get::<_, String>(0)? == version {
            ensure!(
                row.try_get::<_, String>(2)? == manifest_sha256,
                "package {package}@{version} differs from its immutable stage manifest; retained cursor {}",
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
    identity: &StageIdentity,
    initial_cursor: &Value,
) -> anyhow::Result<StageProgress> {
    tx.execute(
        "INSERT INTO catalog.package_upgrade_stages \
         (tenant_id,package_id,package_version,predecessor_version,manifest_sha256,qualification_sha256,status,cursor) \
         VALUES ($1,$2,$3,$4,$5,$6,'in_progress',$7) \
         ON CONFLICT (tenant_id,package_id,package_version) DO NOTHING",
        &[&identity.tenant_id, &identity.package_id, &identity.package_version,
          &identity.predecessor_version, &identity.manifest_sha256, &identity.qualification_sha256,
          initial_cursor],
    ).await.context("create package upgrade stage progress")?;
    let progress = locked(tx, identity).await?;
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
    identity: &StageIdentity,
    cursor: &Value,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, identity).await?;
    require_in_progress(identity, &progress)?;
    let row = tx.query_one(
        "UPDATE catalog.package_upgrade_stages SET cursor=$4, completed_batches=completed_batches+1 \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 \
         RETURNING status,cursor,completed_batches",
        &[&identity.tenant_id, &identity.package_id, &identity.package_version, cursor],
    ).await.context("record package upgrade batch cursor")?;
    decode(&row)
}

/// Complete a stage after its last batch and postconditions succeed.
/// An exact retry returns the existing result without changing the record.
pub(crate) async fn complete(
    tx: &Transaction<'_>,
    identity: &StageIdentity,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, identity).await?;
    if progress.status == StageStatus::Completed {
        return Ok(progress);
    }
    require_in_progress(identity, &progress)?;
    set_status(tx, identity, "completed").await
}

/// Abandon a stage and return its retained cursor without changing installed packages.
pub(crate) async fn abandon(
    tx: &Transaction<'_>,
    identity: &StageIdentity,
) -> anyhow::Result<StageProgress> {
    let progress = locked(tx, identity).await?;
    if progress.status == StageStatus::Abandoned {
        return Ok(progress);
    }
    require_in_progress(identity, &progress)?;
    set_status(tx, identity, "abandoned").await
}

async fn set_status(
    tx: &Transaction<'_>,
    identity: &StageIdentity,
    status: &str,
) -> anyhow::Result<StageProgress> {
    let row = tx
        .query_one(
            "UPDATE catalog.package_upgrade_stages SET status=$4 \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 \
         RETURNING status,cursor,completed_batches",
            &[
                &identity.tenant_id,
                &identity.package_id,
                &identity.package_version,
                &status,
            ],
        )
        .await
        .context("record package upgrade stage status")?;
    decode(&row)
}

async fn locked(tx: &Transaction<'_>, identity: &StageIdentity) -> anyhow::Result<StageProgress> {
    let row = tx.query_opt(
        "SELECT predecessor_version,manifest_sha256,qualification_sha256,status,cursor,completed_batches \
         FROM catalog.package_upgrade_stages \
         WHERE tenant_id=$1 AND package_id=$2 AND package_version=$3 FOR UPDATE",
        &[&identity.tenant_id, &identity.package_id, &identity.package_version],
    ).await.context("lock package upgrade stage progress")?
        .with_context(|| format!("package upgrade stage {}@{} has no recorded cursor", identity.package_id, identity.package_version))?;
    for (field, expected) in [
        ("predecessor_version", &identity.predecessor_version),
        ("manifest_sha256", &identity.manifest_sha256),
        ("qualification_sha256", &identity.qualification_sha256),
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

    fn identity(version: &str) -> StageIdentity {
        StageIdentity {
            tenant_id: "stage-test".into(),
            package_id: "fixture".into(),
            package_version: version.into(),
            predecessor_version: "1.0.0".into(),
            manifest_sha256: format!("sha256:{}", "a".repeat(64)),
            qualification_sha256: format!("sha256:{}", "b".repeat(64)),
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
        let catalog = "SELECT jsonb_build_object(            'columns', (SELECT jsonb_agg(jsonb_build_array(attname,format_type(atttypid,atttypmod),attnotnull) ORDER BY attnum) FROM pg_attribute WHERE attrelid='catalog.package_upgrade_stages'::regclass AND attnum>0 AND NOT attisdropped),            'constraints', (SELECT jsonb_agg(pg_get_constraintdef(oid) ORDER BY conname) FROM pg_constraint WHERE conrelid='catalog.package_upgrade_stages'::regclass),            'policies', (SELECT jsonb_agg(jsonb_build_array(polname,polroles,pg_get_expr(polqual,polrelid),pg_get_expr(polwithcheck,polrelid)) ORDER BY polname) FROM pg_policy WHERE polrelid='catalog.package_upgrade_stages'::regclass),            'security', (SELECT jsonb_build_array(relrowsecurity,relforcerowsecurity,relowner::regrole::text,relacl::text) FROM pg_class WHERE oid='catalog.package_upgrade_stages'::regclass),            'triggers', (SELECT jsonb_agg(pg_get_triggerdef(oid) ORDER BY tgname) FROM pg_trigger WHERE tgrelid='catalog.package_upgrade_stages'::regclass AND NOT tgisinternal))";
        let fresh: Value = client.query_one(catalog, &[]).await.unwrap().get(0);
        client.batch_execute("DROP TABLE catalog.package_upgrade_stages; DROP FUNCTION catalog.guard_package_upgrade_stage_change(); DROP INDEX catalog.package_definition_owners_synchronization_function;").await.unwrap();
        client
            .batch_execute(include_str!(
                "../../../../../deploy/sql/migrations/project/0010_package_upgrade_stages.sql"
            ))
            .await
            .unwrap();
        let migrated: Value = client.query_one(catalog, &[]).await.unwrap().get(0);
        assert_eq!(fresh, migrated);
        let identity = identity("2.0.0");
        let tx = client.transaction().await.unwrap();
        open(&tx, &identity, &Value::Null).await.unwrap();
        tx.commit().await.unwrap();
        for sql in [
            "UPDATE catalog.package_upgrade_stages SET predecessor_version='0.9.0'",
            "UPDATE catalog.package_upgrade_stages SET manifest_sha256='sha256:' || repeat('c',64)",
            "UPDATE catalog.package_upgrade_stages SET qualification_sha256='sha256:' || repeat('c',64)",
            "UPDATE catalog.package_upgrade_stages SET cursor='1'::jsonb",
            "UPDATE catalog.package_upgrade_stages SET completed_batches=2",
            "DELETE FROM catalog.package_upgrade_stages",
        ] {
            let tx = client.transaction().await.unwrap();
            let error = tx.batch_execute(sql).await.unwrap_err();
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
            );
            tx.rollback().await.unwrap();
        }
        let tx = client.transaction().await.unwrap();
        complete(&tx, &identity).await.unwrap();
        tx.commit().await.unwrap();
        let tx = client.transaction().await.unwrap();
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
        client.batch_execute("CREATE TABLE retained (id int PRIMARY KEY, converted bool NOT NULL); INSERT INTO retained VALUES (1,false),(2,false); INSERT INTO catalog.packages (tenant_id,package_id,package_version,manifest_sha256) VALUES ('stage-test','fixture','1.0.0','sha256:' || repeat('c',64));").await.unwrap();
        let identity = identity("2.0.0");
        let tx = client.transaction().await.unwrap();
        let initial = open(&tx, &identity, &Value::Null).await.unwrap();
        assert_eq!(initial.cursor, Value::Null);
        tx.commit().await.unwrap();

        let tx = client.transaction().await.unwrap();
        open(&tx, &identity, &json!("ignored on resume"))
            .await
            .unwrap();
        tx.execute("UPDATE retained SET converted=true WHERE id=1", &[])
            .await
            .unwrap();
        advance(&tx, &identity, &json!({"id":1})).await.unwrap();
        tx.rollback().await.unwrap();
        let tx = client.transaction().await.unwrap();
        assert_eq!(open(&tx, &identity, &Value::Null).await.unwrap(), initial);
        assert!(
            !tx.query_one("SELECT converted FROM retained WHERE id=1", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        tx.execute("UPDATE retained SET converted=true WHERE id=1", &[])
            .await
            .unwrap();
        let committed = advance(&tx, &identity, &json!({"id":1})).await.unwrap();
        tx.commit().await.unwrap();
        let tx = client.transaction().await.unwrap();
        assert_eq!(open(&tx, &identity, &Value::Null).await.unwrap(), committed);
        assert_eq!(committed.completed_batches, 1);
        tx.commit().await.unwrap();

        for field in [
            "manifest_sha256",
            "qualification_sha256",
            "predecessor_version",
        ] {
            let mut changed = identity.clone();
            match field {
                "manifest_sha256" => changed.manifest_sha256 = format!("sha256:{}", "d".repeat(64)),
                "qualification_sha256" => {
                    changed.qualification_sha256 = format!("sha256:{}", "d".repeat(64))
                }
                _ => changed.predecessor_version = "0.9.0".into(),
            }
            let tx = client.transaction().await.unwrap();
            assert!(
                open(&tx, &changed, &Value::Null)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(field)
            );
            tx.rollback().await.unwrap();
        }
        let tx = client.transaction().await.unwrap();
        let abandoned = abandon(&tx, &identity).await.unwrap();
        assert_eq!(abandoned.cursor, committed.cursor);
        tx.commit().await.unwrap();
        let tx = client.transaction().await.unwrap();
        assert!(
            open(&tx, &identity, &Value::Null)
                .await
                .unwrap_err()
                .to_string()
                .contains("retained cursor {\"id\":1}")
        );
        assert!(advance(&tx, &identity, &json!({"id":2})).await.is_err());
        assert!(complete(&tx, &identity).await.is_err());
        assert_eq!(abandon(&tx, &identity).await.unwrap(), abandoned);
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
        open(&tx, &completed_identity, &Value::Null).await.unwrap();
        advance(&tx, &completed_identity, &json!({"id":2}))
            .await
            .unwrap();
        let completed = complete(&tx, &completed_identity).await.unwrap();
        tx.commit().await.unwrap();
        let tx = client.transaction().await.unwrap();
        assert_eq!(
            open(&tx, &completed_identity, &Value::Null).await.unwrap(),
            completed
        );
        assert_eq!(complete(&tx, &completed_identity).await.unwrap(), completed);
        assert!(
            advance(&tx, &completed_identity, &Value::Null)
                .await
                .is_err()
        );
        assert!(abandon(&tx, &completed_identity).await.is_err());
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

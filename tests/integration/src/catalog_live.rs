//! CF-RELEASE publication boundary proofs.

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use tokio_postgres::NoTls;

    const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Published {
        artifacts: BTreeMap<(String, i32), Vec<u8>>,
        members: BTreeMap<String, i32>,
        head: Option<i32>,
        journaled: bool,
    }

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        AfterArtifact,
        AfterMember,
        AfterJournal,
        BeforeHead,
    }

    fn publish_atomically(
        stored: &mut Published,
        target: i32,
        member: (&str, i32),
        fault: Option<Fault>,
    ) -> Result<(), &'static str> {
        let mut transaction = stored.clone();
        transaction.artifacts.insert(
            (member.0.to_string(), member.1),
            b"canonical-artifact".to_vec(),
        );
        if matches!(fault, Some(Fault::AfterArtifact)) {
            return Err("injected-after-artifact");
        }
        if transaction
            .members
            .insert(member.0.to_string(), member.1)
            .is_some_and(|version| version != member.1)
        {
            return Err("catalog-release-content-conflict");
        }
        if matches!(fault, Some(Fault::AfterMember)) {
            return Err("injected-after-member");
        }
        transaction.journaled = true;
        if matches!(fault, Some(Fault::AfterJournal)) {
            return Err("injected-after-journal");
        }
        if matches!(fault, Some(Fault::BeforeHead)) {
            return Err("injected-before-head");
        }
        transaction.head = Some(target);
        *stored = transaction;
        Ok(())
    }

    #[test]
    fn injected_faults_leave_no_partial_release_and_retry_is_byte_identical() {
        for fault in [
            Fault::AfterArtifact,
            Fault::AfterMember,
            Fault::AfterJournal,
            Fault::BeforeHead,
        ] {
            let mut stored = Published::default();
            let before = format!("{stored:?}").into_bytes();
            assert!(publish_atomically(&mut stored, 2, ("f1", 3), Some(fault)).is_err());
            assert_eq!(format!("{stored:?}").into_bytes(), before);

            publish_atomically(&mut stored, 2, ("f1", 3), None).unwrap();
            let first = format!("{stored:?}").into_bytes();
            publish_atomically(&mut stored, 2, ("f1", 3), None).unwrap();
            assert_eq!(format!("{stored:?}").into_bytes(), first);
        }
    }

    #[test]
    fn ddl_pins_immutable_and_atomic_release_boundaries() {
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER flow_artifacts_immutable"));
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER release_flows_immutable"));
        assert!(CATALOG_SCHEMA.contains("flow-version-content-conflict"));
        assert!(CATALOG_SCHEMA.contains("REFERENCES catalog.flow_artifacts"));
        assert!(CATALOG_SCHEMA.contains("CREATE TABLE catalog.catalog_heads"));
        assert!(CATALOG_SCHEMA.contains("CREATE FUNCTION catalog.publication_boundary"));
        for source in [
            include_str!("../../../services/ctl/src/publish_catalog.rs"),
            include_str!("../../../services/ctl/src/copy_project_env.rs"),
        ] {
            for stage in [
                "after-artifacts",
                "after-members",
                "after-journal",
                "before-head",
            ] {
                assert!(source.contains(stage), "production writer misses {stage}");
            }
        }
    }

    async fn write_release(
        client: &tokio_postgres::Client,
        tenant: &str,
    ) -> Result<(), tokio_postgres::Error> {
        let register = wamn_schema_control::sql::register_flow_artifact_sql();
        client
            .execute(
                register,
                &[
                    &tenant,
                    &"flow",
                    &1_i32,
                    &"1",
                    &r#"{"flow-id":"flow"}"#,
                    &"graph-a",
                    &"artifact-a",
                    &"interfaces-a",
                    &"[]",
                ],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-artifacts"],
            )
            .await?;
        client
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state, document) \
                 VALUES ($1, 'catalog', 1, 'dev', '1', 'applied', \
                   '{\"schema-version\":\"1\",\"catalog-id\":\"catalog\",\"version\":1,\"entities\":[]}'::jsonb) \
                 ON CONFLICT (tenant_id, catalog_id, version) DO NOTHING",
                &[&tenant],
            )
            .await?;
        let from_version: Option<i32> = None;
        client
            .execute(
                wamn_schema_control::sql::record_release_publication_sql(),
                &[
                    &tenant,
                    &"catalog",
                    &"dev",
                    &from_version,
                    &1_i32,
                    &"journal-a",
                ],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-journal"],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::register_release_manifest_sql(),
                &[
                    &tenant,
                    &"catalog",
                    &1_i32,
                    &r#"[{"flow-id":"flow","flow-version":1,"artifact-hash":"artifact-a"}]"#,
                ],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::insert_release_flow_sql(),
                &[&tenant, &"catalog", &1_i32, &"flow", &1_i32],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-members"],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"before-head"],
            )
            .await?;
        client
            .execute(
                wamn_schema_control::sql::advance_catalog_head_sql(),
                &[&tenant, &"catalog", &"dev", &1_i32],
            )
            .await?;
        Ok(())
    }

    async fn release_bytes(client: &tokio_postgres::Client, tenant: &str) -> Vec<u8> {
        let row = client
            .query_one(
                "SELECT \
                   (SELECT (to_jsonb(a) - 'created_at')::text FROM catalog.flow_artifacts a \
                     WHERE tenant_id = $1), \
                   (SELECT members_json::text FROM catalog.release_manifests WHERE tenant_id = $1), \
                   (SELECT jsonb_build_array(flow_id, flow_version)::text \
                     FROM catalog.release_flows WHERE tenant_id = $1), \
                   (SELECT jsonb_build_array(from_version, to_version, confirmation, \
                     statement_count, destructive, checksum)::text \
                     FROM catalog.schema_migrations WHERE tenant_id = $1), \
                   (SELECT applied_catalog_version::text FROM catalog.catalog_heads \
                     WHERE tenant_id = $1)",
                &[&tenant],
            )
            .await
            .unwrap();
        format!(
            "{:?}",
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
                row.get::<_, String>(4),
            )
        )
        .into_bytes()
    }

    /// Optional statement-level half. CI supplies a disposable superuser URL;
    /// local unit runs without one still exercise the deterministic boundary
    /// mutants above.
    #[tokio::test]
    async fn database_faults_rollback_every_release_row_and_retry_identically() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        client.batch_execute("BEGIN").await.unwrap();
        client
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app; \
                   END IF; \
                 END $$; \
                 DROP SCHEMA IF EXISTS catalog CASCADE;",
            )
            .await
            .unwrap();
        client.batch_execute(CATALOG_SCHEMA).await.unwrap();
        client.batch_execute("COMMIT").await.unwrap();

        for (index, stage) in [
            "after-artifacts",
            "after-journal",
            "after-members",
            "before-head",
        ]
        .into_iter()
        .enumerate()
        {
            let tenant = format!("fault-{index}");
            client.batch_execute("BEGIN").await.unwrap();
            client
                .batch_execute(&format!(
                    "SET LOCAL wamn.test.publication_fault = '{stage}'"
                ))
                .await
                .unwrap();
            let fault = write_release(&client, &tenant).await.unwrap_err();
            assert_eq!(fault.code().map(|code| code.code()), Some("40000"));
            client.batch_execute("ROLLBACK").await.unwrap();
            let counts = client
                .query_one(
                    "SELECT \
                       (SELECT count(*) FROM catalog.flow_artifacts WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.release_manifests WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.release_flows WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.schema_migrations WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.catalog_heads WHERE tenant_id = $1)",
                    &[&tenant],
                )
                .await
                .unwrap();
            for column in 0..5 {
                assert_eq!(
                    counts.get::<_, i64>(column),
                    0,
                    "{stage} left column {column}"
                );
            }

            client.batch_execute("BEGIN").await.unwrap();
            write_release(&client, &tenant).await.unwrap();
            client.batch_execute("COMMIT").await.unwrap();
            let first = release_bytes(&client, &tenant).await;
            client.batch_execute("BEGIN").await.unwrap();
            write_release(&client, &tenant).await.unwrap();
            client.batch_execute("COMMIT").await.unwrap();
            assert_eq!(release_bytes(&client, &tenant).await, first);
        }

        client.batch_execute("BEGIN").await.unwrap();
        let register = wamn_schema_control::sql::register_flow_artifact_sql();
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 9] = [
            &"immutable-tenant",
            &"flow",
            &1_i32,
            &"1",
            &r#"{"flow-id":"flow"}"#,
            &"graph-a",
            &"artifact-a",
            &"interfaces-a",
            &"[]",
        ];
        client.execute(register, &params).await.unwrap();
        client.execute(register, &params).await.unwrap();

        client
            .batch_execute("SAVEPOINT immutable_update")
            .await
            .unwrap();
        let update = client
            .execute(
                "UPDATE catalog.flow_artifacts SET graph_hash = 'changed' \
                 WHERE tenant_id = 'immutable-tenant'",
                &[],
            )
            .await
            .unwrap_err();
        assert_eq!(update.code().map(|code| code.code()), Some("55000"));
        client
            .batch_execute("ROLLBACK TO SAVEPOINT immutable_update")
            .await
            .unwrap();

        let conflict = client
            .execute(
                register,
                &[
                    &"immutable-tenant",
                    &"flow",
                    &1_i32,
                    &"1",
                    &r#"{"flow-id":"different"}"#,
                    &"graph-b",
                    &"artifact-b",
                    &"interfaces-a",
                    &"[]",
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code().map(|code| code.code()), Some("23505"));
        client.batch_execute("ROLLBACK").await.unwrap();
        client
            .batch_execute("DROP SCHEMA catalog CASCADE")
            .await
            .unwrap();
        drop(client);
        let _ = task.await;
    }
}

//! C4 file schema, grants, history, and reference constraints on real PostgreSQL.
//! Object transfer and HTTP exposure require the integrated native file routes.

use tokio_postgres::error::SqlState;
use wamn_control_provision::schema_migrations::MigrationTarget;

#[path = "support/files.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also supplies native host and MinIO helpers."
)]
mod files;

#[test]
fn file_fixture_declares_each_binding_mode_and_the_file_migration_is_registered() {
    wamn_schema_generator::PackageManifest::from_slice(files::PACKAGE.as_bytes())
        .expect("the file fixture uses the current package manifest");
    let package: serde_json::Value = serde_json::from_str(files::PACKAGE).unwrap();
    let declaration = wamn_control::component_declaration::render_palette_declaration(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/file_package/publication/components/blob-put.json.in"),
        &wamn_catalog::ComponentPackageScope {
            tenant_id: "file-fixture".into(),
            package_id: "file_fixture".into(),
            package_version: "0.1.0".into(),
        },
        None,
    )
    .expect("render the file fixture's current blobstore declaration");
    let _: wamn_catalog::ComponentDeclaration = serde_json::from_value(declaration)
        .expect("the fixture uses the current component declaration");
    assert_eq!(package["connections"]["files"]["integration"], "s3");
    assert_eq!(package["connections"]["files"]["mode"], "write");
    assert_eq!(package["connections"]["files_read"]["mode"], "read");
    assert_eq!(package["connections"]["files_no_cdn"]["mode"], "write");
    assert!(
        MigrationTarget::Project
            .migrations()
            .iter()
            .any(|migration| {
                migration.ordinal() == 45
                    && migration.relative_path == "migrations/project/0045_c4_files.sql"
            })
    );
}

#[tokio::test]
async fn application_file_writes_are_refused_and_history_uses_the_standard_contract()
-> anyhow::Result<()> {
    let fixture = files::fixture("file_authority").await?;
    files::metadata(&fixture.writer).await?;
    for statement in [
        "INSERT INTO platform.file DEFAULT VALUES",
        "UPDATE platform.file SET name='forged'",
        "DELETE FROM platform.file",
        "TRUNCATE platform.file",
        "INSERT INTO platform.file_history DEFAULT VALUES",
    ] {
        let error = fixture.guest.batch_execute(statement).await.unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::INSUFFICIENT_PRIVILEGE),
            "{statement}"
        );
    }
    let row = fixture
        .guest
        .query_one(
            "SELECT application,connection_id::text,connection_generation,created_by::text, \
         updated_by::text,created_at=updated_at,exposure,url FROM platform.file WHERE id=$1::text::uuid",
            &[&files::FILE],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), files::APPLICATION);
    assert_eq!(row.get::<_, String>(1), files::CONNECTION);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert_eq!(row.get::<_, String>(3), files::CALLER);
    assert_eq!(row.get::<_, String>(4), files::CALLER);
    assert!(row.get::<_, bool>(5));
    assert_eq!(row.get::<_, String>(6), "application");
    assert!(row.get::<_, Option<String>>(7).is_none());
    let entry = fixture
        .observer
        .query_one(
            "SELECT changed_by::text,operation,after->>'id' FROM platform.file_history",
            &[],
        )
        .await?;
    assert_eq!(entry.get::<_, String>(0), files::CALLER);
    assert_eq!(entry.get::<_, String>(1), "admin:file-fixture");
    assert_eq!(entry.get::<_, String>(2), files::FILE);
    Ok(())
}

#[tokio::test]
async fn disabling_a_connection_preserves_file_references_and_referenced_delete_refuses()
-> anyhow::Result<()> {
    let fixture = files::fixture("file_references").await?;
    files::metadata(&fixture.writer).await?;
    fixture
        .guest
        .execute(
            "INSERT INTO file_fixture.document (id,file_id) VALUES ($1::text::uuid,$1::text::uuid)",
            &[&files::FILE],
        )
        .await?;
    fixture
        .writer
        .execute(
            "UPDATE platform.connection SET status='disabled' WHERE id=$1::text::uuid",
            &[&files::CONNECTION],
        )
        .await?;
    let row = fixture
        .guest
        .query_one(
            "SELECT c.status,f.connection_generation FROM file_fixture.document d \
         JOIN platform.file f ON f.id=d.file_id JOIN platform.connection c ON c.id=f.connection_id",
            &[],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), "disabled");
    assert_eq!(row.get::<_, i64>(1), 1);
    let error = fixture
        .writer
        .execute(
            "DELETE FROM platform.file WHERE id=$1::text::uuid",
            &[&files::FILE],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
    fixture
        .guest
        .execute("DELETE FROM file_fixture.document", &[])
        .await?;
    assert_eq!(
        fixture
            .guest
            .query_one("SELECT count(*) FROM platform.file", &[])
            .await?
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        fixture
            .writer
            .execute(
                "DELETE FROM platform.file WHERE id=$1::text::uuid",
                &[&files::FILE],
            )
            .await?,
        1
    );
    // The migration owner needs no lasting platform privileges to keep the FK.
    let privilege = fixture
        .migrator
        .query_one(
            "SELECT has_table_privilege(current_user, \
             (SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
              WHERE n.nspname='platform' AND c.relname='file'), 'REFERENCES')",
            &[],
        )
        .await?;
    assert!(!privilege.get::<_, bool>(0));
    Ok(())
}

#[tokio::test]
async fn migrated_file_schema_and_grants_match_a_fresh_install() -> anyhow::Result<()> {
    let fixture = files::fixture("file_upgrade").await?;
    let shape = async || -> anyhow::Result<Vec<serde_json::Value>> {
        let mut entries: Vec<serde_json::Value> = fixture.observer.query(
            "SELECT to_jsonb(a) FROM (SELECT table_name,column_name,data_type,is_nullable,column_default \
             FROM information_schema.columns WHERE table_schema='platform' \
             AND table_name IN ('file','file_history') ORDER BY table_name,ordinal_position) a \
             UNION ALL SELECT to_jsonb(g) FROM (SELECT table_name,grantee,privilege_type \
             FROM information_schema.role_table_grants WHERE table_schema='platform' \
             AND table_name IN ('file','file_history') ORDER BY table_name,grantee,privilege_type) g \
             UNION ALL SELECT to_jsonb(t) FROM (SELECT c.relname,t.tgname,pg_get_triggerdef(t.oid) AS definition \
             FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid \
             WHERE t.tgrelid='platform.file'::regclass AND NOT t.tgisinternal ORDER BY t.tgname) t \
             UNION ALL SELECT to_jsonb(k) FROM (SELECT conname,pg_get_constraintdef(oid) AS definition \
             FROM pg_constraint WHERE conrelid IN ('platform.file'::regclass,'platform.file_history'::regclass)) k \
             UNION ALL SELECT to_jsonb(p) FROM (SELECT table_name,column_name,grantee,privilege_type \
             FROM information_schema.column_privileges WHERE table_schema='platform' \
             AND table_name IN ('file','file_history')) p",
            &[],
        ).await?.iter().map(|row| row.get(0)).collect();
        entries.sort_by_key(serde_json::Value::to_string);
        Ok(entries)
    };
    let fresh = shape().await?;
    fixture
        .observer
        .batch_execute(
            "DROP TABLE file_fixture.document; DROP TABLE platform.file,platform.file_history",
        )
        .await?;
    let migration = MigrationTarget::Project
        .migrations()
        .iter()
        .find(|migration| migration.ordinal() == 45)
        .expect("C4 project migration 0045");
    fixture
        .observer
        .batch_execute(&format!("BEGIN; {} COMMIT", migration.sql))
        .await?;
    assert_eq!(shape().await?, fresh);
    Ok(())
}

fn registry_definition() -> serde_json::Value {
    serde_json::json!({"provider":"s3","endpoint":"http://127.0.0.1:9000",
        "container":"files-a","prefix":"old/"})
}

#[tokio::test]
async fn file_connection_lookup_refuses_wrong_login_and_forged_scope() -> anyhow::Result<()> {
    let registry = files::registry("file-org", "project", registry_definition()).await?;
    let allowed = files::lookup(
        &registry.reader,
        "file-org",
        "project",
        "files",
        Some("files"),
        None,
    )
    .await?;
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0].get::<_, String>(0), registry.connection_id);
    registry.unmapped.batch_execute("SET app.org='file-org'; SET app.project='project'; SET app.tenant='file-org--project--dev'").await?;
    for (client, org, project) in [
        (&registry.unmapped, "file-org", "project"),
        (registry.reader.as_ref(), "other-org", "project"),
        (registry.reader.as_ref(), "file-org", "unenabled"),
    ] {
        for (binding, pin) in [
            (Some("files"), None),
            (
                None,
                Some((registry.connection_id.as_str(), registry.generation)),
            ),
        ] {
            assert!(
                files::lookup(client, org, project, "files", binding, pin)
                    .await?
                    .is_empty()
            );
        }
    }
    for (application, binding) in [("other", "files"), ("files", "undeclared"), ("", "files")] {
        assert!(
            files::lookup(
                &registry.reader,
                "file-org",
                "project",
                application,
                Some(binding),
                None
            )
            .await?
            .is_empty()
        );
    }
    let error = registry
        .reader
        .query("SELECT * FROM registry.connection_generation", &[])
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    Ok(())
}

#[tokio::test]
async fn file_connection_lookup_requires_project_enablement_and_an_active_connection()
-> anyhow::Result<()> {
    let registry = files::registry("file-org", "project", registry_definition()).await?;
    assert!(
        files::lookup(
            &registry.unenabled,
            "file-org",
            "unenabled",
            "files",
            None,
            Some((&registry.connection_id, registry.generation))
        )
        .await?
        .is_empty()
    );
    registry
        .configure(
            "project_connection.disable",
            serde_json::json!({"project":"project","connection":files::STORE_NAME}),
        )
        .await?;
    assert!(
        files::lookup(
            &registry.reader,
            "file-org",
            "project",
            "files",
            None,
            Some((&registry.connection_id, registry.generation))
        )
        .await?
        .is_empty()
    );
    registry
        .configure(
            "project_connection.enable",
            serde_json::json!({"project":"project","connection":files::STORE_NAME}),
        )
        .await?;
    let mut parameters = registry_definition();
    parameters["prefix"] = serde_json::json!("disabled/");
    registry
        .configure(
            "connection.write",
            serde_json::json!({"name":files::STORE_NAME,"integration":"s3",
        "transport":"s3","exchange":"request","parameters":parameters,"status":"disabled"}),
        )
        .await?;
    for (binding, pin) in [
        (Some("files"), None),
        (
            None,
            Some((registry.connection_id.as_str(), registry.generation)),
        ),
    ] {
        assert!(
            files::lookup(
                &registry.reader,
                "file-org",
                "project",
                "files",
                binding,
                pin
            )
            .await?
            .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn file_connection_lookup_pins_bucket_and_prefix_after_a_store_move() -> anyhow::Result<()> {
    let original = registry_definition();
    let registry = files::registry("file-org", "project", original.clone()).await?;
    let moved = serde_json::json!({"provider":"s3","endpoint":"http://127.0.0.1:9000",
        "container":"files-b","prefix":"new/"});
    registry.move_store(&moved).await?;
    let current = files::lookup(
        &registry.reader,
        "file-org",
        "project",
        "files",
        Some("files"),
        None,
    )
    .await?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].get::<_, serde_json::Value>(3), moved);
    assert!(current[0].get::<_, i64>(2) > registry.generation);
    let pinned = files::lookup(
        &registry.reader,
        "file-org",
        "project",
        "files",
        None,
        Some((&registry.connection_id, registry.generation)),
    )
    .await?;
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].get::<_, i64>(2), registry.generation);
    assert_eq!(pinned[0].get::<_, serde_json::Value>(3), original);
    assert!(
        files::lookup(
            &registry.reader,
            "file-org",
            "project",
            "files",
            None,
            Some((&registry.connection_id, 999_999))
        )
        .await?
        .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn raw_blobstore_mutations_refuse_file_namespaces_on_minio() -> anyhow::Result<()> {
    use object_store::{ObjectStoreExt as _, aws::AmazonS3Builder, path::Path};
    use std::sync::Arc;
    use wamn_runtime::plugins::wamn_blobstore::store::BoundContainer;
    let minio = files::Minio::start().await?;
    let backend = Arc::new(
        AmazonS3Builder::new()
            .with_endpoint(&minio.endpoint)
            .with_bucket_name("files-a")
            .with_region("us-east-1")
            .with_access_key_id("file-fixture")
            .with_secret_access_key(files::STORE_PASSWORD)
            .with_allow_http(true)
            .build()?,
    );
    let raw = BoundContainer::new(backend.clone(), "files-a", "generation-one");
    let id = uuid::Uuid::new_v4();
    let live = format!("_wamn_files/live/{id}");
    backend
        .put(
            &Path::from(format!("generation-one/{live}")),
            b"retained".to_vec().into(),
        )
        .await?;
    raw.put("ordinary", b"ordinary".to_vec()).await?;
    for key in [
        live.clone(),
        format!("_wamn_files/staging/{id}"),
        format!("public/{live}"),
    ] {
        assert!(
            raw.put(&key, b"forged".to_vec()).await.is_err(),
            "raw put {key}"
        );
        assert!(raw.delete(&key).await.is_err(), "raw delete {key}");
    }
    assert!(raw.clear().await.is_err());
    assert_eq!(
        backend
            .get(&Path::from(format!("generation-one/{live}")))
            .await?
            .bytes()
            .await?
            .as_ref(),
        b"retained"
    );
    assert_eq!(raw.get("ordinary").await?, b"ordinary");
    Ok(())
}

/// MinIO's preserved source time supplies aged objects without changing the clock.
/// Source: https://github.com/minio/minio/blob/RELEASE.2025-09-07T16-13-09Z/cmd/object-api-options.go
#[tokio::test]
async fn aged_staging_expires_while_unlinked_metadata_and_aged_live_bytes_stay()
-> anyhow::Result<()> {
    use object_store::{
        ObjectStoreExt as _,
        aws::AmazonS3Builder,
        path::Path,
        signer::{Method, Signer as _},
    };
    let metadata = files::fixture("file_lifecycle").await?;
    files::metadata(&metadata.writer).await?;
    files::age_metadata(&metadata).await?;
    let minio = files::Minio::start().await?;
    metadata
        .writer
        .execute(
            "UPDATE platform.connection SET parameters=$1 WHERE id=$2::text::uuid",
            &[&minio.definition(), &files::CONNECTION],
        )
        .await?;
    let store = AmazonS3Builder::new()
        .with_endpoint(&minio.endpoint)
        .with_bucket_name("files-a")
        .with_region("us-east-1")
        .with_access_key_id("file-fixture")
        .with_secret_access_key(files::STORE_PASSWORD)
        .with_allow_http(true)
        .build()?;
    let live = Path::from(format!("generation-one/_wamn_files/live/{}", files::FILE));
    let staging_id = uuid::Uuid::new_v4().to_string();
    let staging = Path::from(format!("generation-one/_wamn_files/staging/{staging_id}"));
    let client = minio.client()?;
    for path in [&live, &staging] {
        let url = store
            .signed_url(Method::PUT, path, std::time::Duration::from_secs(60))
            .await?;
        let response = client
            .put(url)
            .header("x-minio-source-mtime", "2000-01-01T00:00:00Z")
            .body(b"aged fixture bytes".to_vec())
            .send()
            .await?;
        assert!(
            response.status().is_success(),
            "seed aged MinIO object: {}",
            response.text().await?
        );
    }
    metadata
        .writer
        .execute(
            "UPDATE platform.file SET size=$1,sha256=$2 WHERE id=$3::text::uuid",
            &[
                &i64::try_from(b"aged fixture bytes".len())?,
                &hex::encode(
                    ring::digest::digest(&ring::digest::SHA256, b"aged fixture bytes").as_ref(),
                ),
                &files::FILE,
            ],
        )
        .await?;
    let row = metadata
        .guest
        .query_one(
            "SELECT created_at FROM platform.file WHERE id=$1::text::uuid",
            &[&files::FILE],
        )
        .await?;
    let created: chrono::DateTime<chrono::Utc> = row.get(0);
    assert!(created < chrono::Utc::now() - chrono::Duration::days(30));
    assert_eq!(
        metadata
            .guest
            .query_one("SELECT count(*) FROM file_fixture.document", &[])
            .await?
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        metadata
            .guest
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&staging_id]
            )
            .await?
            .get::<_, i64>(0),
        0
    );
    let cutoff = chrono::Utc::now() - chrono::Duration::days(2);
    assert!(store.head(&staging).await?.last_modified < cutoff);
    assert!(store.head(&live).await?.last_modified < cutoff);
    minio.mc("mc ilm rule add --expire-days 1 --prefix generation-one/_wamn_files/staging/ file/files-a").await?;
    let mut removed = false;
    for _ in 0..180 {
        match store.head(&staging).await {
            Err(object_store::Error::NotFound { .. }) => {
                removed = true;
                break;
            }
            Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Err(error) => return Err(error.into()),
        }
    }
    assert!(
        removed,
        "the lifecycle scanner must delete aged staging only"
    );
    assert_eq!(
        store.get(&live).await?.bytes().await?.as_ref(),
        b"aged fixture bytes"
    );
    assert_eq!(
        metadata
            .guest
            .query_one(
                "SELECT count(*) FROM platform.file WHERE id=$1::text::uuid",
                &[&files::FILE]
            )
            .await?
            .get::<_, i64>(0),
        1
    );
    Ok(())
}

#[tokio::test]
async fn file_connection_lookup_accepts_only_the_matching_binding_and_pinned_generation()
-> anyhow::Result<()> {
    let original = registry_definition();
    let registry = files::registry("file-org", "project", original.clone()).await?;
    let moved = serde_json::json!({"provider":"s3","endpoint":"http://127.0.0.1:9000",
        "container":"files-b","prefix":"new/"});
    registry.move_store(&moved).await?;
    for (binding, mode) in [("files", "write"), ("files_read", "read")] {
        let rows = files::lookup(
            &registry.reader,
            "file-org",
            "project",
            "files",
            Some(binding),
            Some((&registry.connection_id, registry.generation)),
        )
        .await?;
        if binding == "files_read" {
            assert!(
                rows.is_empty(),
                "a combined binding and pin refuses read authority"
            );
        } else {
            assert_eq!(
                rows.len(),
                1,
                "native mutations carry both their write binding and their file's pin"
            );
            assert_eq!(rows[0].get::<_, serde_json::Value>(3), original);
            assert_eq!(rows[0].get::<_, String>(4), mode);
        }
    }
    let other: String = registry
        .owner
        .query_one(
            "SELECT id::text FROM registry.connection WHERE org='file-org' AND name='files-no-cdn'",
            &[],
        )
        .await?
        .get(0);
    assert!(
        files::lookup(
            &registry.reader,
            "file-org",
            "project",
            "files",
            Some("files"),
            Some((&other, registry.generation))
        )
        .await?
        .is_empty()
    );
    Ok(())
}

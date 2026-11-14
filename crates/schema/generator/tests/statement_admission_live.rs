//! Generated CRUD returns its declared outcomes.

use sqlx::Row as _;

#[sqlx::test(migrations = false)]
async fn generated_crud_outcomes_hold_under_column_grants(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

    use wamn_compiler::{
        AuthoredSql, GenerationInput, StatementTransactionality, compile_manifest, derive_manifest,
        generate,
    };

    let _serialized = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let mut client = wamn_postgres::connect(database.as_str()).await?;
    wamn_fixture_package::apply_migrations(&mut client).await?;
    let catalog = wamn_schema_introspection::postgres::read_catalog(
        &wamn_postgres::pool(database.as_str(), 1).await?,
        &[wamn_fixture_package::SCHEMA],
    )
    .await?;
    let manifest = compile_manifest(&wamn_fixture_package::package_root())?;
    let sources = wamn_fixture_package::authored_sql();
    let sources = sources
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
        .collect::<Vec<_>>();
    let manifest = derive_manifest(&catalog, &manifest, &sources, &[])?;
    let package = generate(&GenerationInput::new(
        &catalog,
        &manifest,
        &sources,
        &StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
    ))?;
    let sql = |action| -> anyhow::Result<&str> {
        Ok(std::str::from_utf8(
            package
                .file(&format!("generated/sql/counter/{action}.sql"))
                .expect("the fixture declares the counter operation")
                .bytes(),
        )?)
    };
    let get = sql("get")?;
    let update = sql("update")?;
    let delete = sql("delete")?;

    // Generate before adding the private column to retain the predecessor's grants.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
               THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; END $$; \
             ALTER TABLE inventory.counter ADD COLUMN private_note text DEFAULT 'untouched'; \
             GRANT USAGE ON SCHEMA inventory TO wamn_app; \
             GRANT SELECT (id, label, edit_version, created_at), UPDATE (label, edit_version), \
               DELETE ON inventory.counter TO wamn_app; \
             SET search_path = inventory;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    let id: i64 = sqlx::query("INSERT INTO counter (label) VALUES ('initial') RETURNING id")
        .fetch_one(&mut client)
        .await?
        .get(0);
    sqlx::raw_sql("SET ROLE wamn_app")
        .execute(&mut client)
        .await
        .map(drop)?;
    let privileges = sqlx::query(
        "SELECT has_table_privilege(current_user, 'inventory.counter', 'SELECT'), \
             has_column_privilege(current_user, 'inventory.counter', 'private_note', 'SELECT')",
    )
    .fetch_one(&mut client)
    .await?;
    assert!(!privileges.get::<bool, _>(0));
    assert!(!privileges.get::<bool, _>(1));

    let initial = sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
        .bind(&id)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(initial.get::<i64, _>("id"), id);
    assert_eq!(initial.get::<String, _>("label"), "initial");
    assert_eq!(initial.get::<i32, _>("edit_version"), 1);
    let created_at: Timestamp = initial.get("created_at");
    let changed = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
        .bind(&id)
        .bind(&1_i32)
        .bind(&true)
        .bind(&"changed")
        .fetch_one(&mut client)
        .await?;
    assert_eq!(changed.get::<String, _>("outcome"), "updated");
    assert_eq!(changed.get::<Option<i64>, _>("id"), Some(id));
    assert_eq!(changed.get::<Option<i32>, _>("edit_version"), Some(2));
    assert_eq!(
        changed.get::<Option<String>, _>("label").as_deref(),
        Some("changed")
    );
    assert_eq!(
        changed.get::<Option<Timestamp>, _>("created_at"),
        Some(created_at)
    );

    // The fixture calls its row_version edit_version, including the observed value.
    for (expected, present, value, outcome, version) in [
        (1_i32, true, "stale", "concurrency_conflict", Some(2_i32)),
        (2, true, "changed", "updated", Some(2)),
        (2, false, "ignored", "updated", Some(2)),
        (1, true, "changed", "concurrency_conflict", Some(2)),
    ] {
        let row = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
            .bind(&id)
            .bind(&expected)
            .bind(&present)
            .bind(&value)
            .fetch_one(&mut client)
            .await?;
        assert_eq!(row.get::<String, _>("outcome"), outcome);
        assert_eq!(row.get::<Option<i32>, _>("observed_edit_version"), version);
        if outcome == "updated" {
            assert_eq!(row.get::<Option<i32>, _>("edit_version"), Some(2));
            assert_eq!(row.get::<Option<i64>, _>("id"), Some(id));
            assert_eq!(
                row.get::<Option<String>, _>("label").as_deref(),
                Some("changed")
            );
            assert_eq!(
                row.get::<Option<Timestamp>, _>("created_at"),
                Some(created_at)
            );
        } else {
            assert_eq!(row.get::<Option<i64>, _>("id"), None);
            assert_eq!(row.get::<Option<String>, _>("label"), None);
            assert_eq!(row.get::<Option<i32>, _>("edit_version"), None);
            assert_eq!(row.get::<Option<Timestamp>, _>("created_at"), None);
        }
        let stored = sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_one(&mut client)
            .await?;
        assert_eq!(stored.get::<String, _>("label"), "changed");
        assert_eq!(stored.get::<i32, _>("edit_version"), 2);
        assert_eq!(stored.get::<Timestamp, _>("created_at"), created_at);
    }

    let stale = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&1_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(stale.get::<String, _>("outcome"), "concurrency_conflict");
    assert_eq!(
        stale.get::<Option<i32>, _>("observed_edit_version"),
        Some(2)
    );
    assert!(
        sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_optional(&mut client)
            .await?
            .is_some()
    );
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut client)
        .await
        .map(drop)?;
    assert_eq!(
        sqlx::query("SELECT private_note FROM counter WHERE id = $1")
            .bind(&id)
            .fetch_one(&mut client)
            .await?
            .get::<String, _>(0),
        "untouched"
    );
    sqlx::raw_sql("SET ROLE wamn_app")
        .execute(&mut client)
        .await
        .map(drop)?;

    let deleted = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(deleted.get::<String, _>("outcome"), "deleted");
    assert_eq!(
        deleted.get::<Option<i32>, _>("observed_edit_version"),
        Some(2)
    );
    assert!(
        sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_optional(&mut client)
            .await?
            .is_none()
    );
    let missing = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .bind(&true)
        .bind(&"missing")
        .fetch_one(&mut client)
        .await?;
    assert_eq!(missing.get::<String, _>("outcome"), "not_found");
    assert_eq!(missing.get::<Option<i32>, _>("observed_edit_version"), None);
    assert_eq!(missing.get::<Option<i64>, _>("id"), None);
    assert_eq!(missing.get::<Option<String>, _>("label"), None);
    assert_eq!(missing.get::<Option<i32>, _>("edit_version"), None);
    assert_eq!(missing.get::<Option<Timestamp>, _>("created_at"), None);
    let missing = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(missing.get::<String, _>("outcome"), "not_found");
    assert_eq!(missing.get::<Option<i32>, _>("observed_edit_version"), None);

    drop(client);
    Ok(())
}

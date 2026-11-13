//! Route writes keep private values in the store and generations in their target environment.

use sqlx::{Connection as _, Row as _};
use std::sync::Mutex;

use serde_json::{Value, json};
use wamn_broker::auth_changes::AuthDrops;
use wamn_kubernetes::{Store, StoreVersion};
use wamn_platform_identity::connections::administration;

#[derive(Debug, Default)]
struct RecordingStore(Mutex<Vec<Vec<u8>>>);

#[async_trait::async_trait]
impl Store for RecordingStore {
    async fn put(
        &self,
        _target: &wamn_registry::Triple,
        id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, wamn_kubernetes::Error> {
        self.0.lock().unwrap().push(value.to_vec());
        Ok(StoreVersion {
            store: "k8s",
            reference: format!("private-{id}"),
            version: "1".into(),
        })
    }
    async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, wamn_kubernetes::Error> {
        Ok(Vec::new())
    }
    async fn destroy(&self, _: &str, _: &str) -> Result<(), wamn_kubernetes::Error> {
        Ok(())
    }
}

async fn execute(
    client: &mut sqlx::PgConnection,
    store: &RecordingStore,
    operation: &str,
    request: Value,
) -> Value {
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    let result = administration::execute_with_store(
        &mut tx,
        "acme",
        operation,
        &request.to_string(),
        &mut AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        Some(store),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn write_and_copy_keep_environment_generations_and_credentials_separate() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("BEGIN; SET LOCAL ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    for env in ["dev", "prod"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql("acme", "app", env))
                .to_string(),
        ))
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    }
    sqlx::raw_sql("UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT").execute(&mut client).await.map(drop).unwrap();
    let store = RecordingStore::default();
    execute(
        &mut client,
        &store,
        "integration.write",
        json!({"project":"app","name":"erp","integration_type":"http","auth_type":"http_basic"}),
    )
    .await;
    let mut created = Vec::new();
    for env in ["dev", "prod"] {
        let credential = format!(r#"{{"username":"{env}","password":"private-{env}"}}"#);
        let response = execute(&mut client,&store,"connection.write",json!({"project":"app","env":env,"name":"erp",
            "parameters":{"primary-authority":"https://example.invalid","tls-verification":"disabled"},"credential":credential})).await;
        assert!(!response.to_string().contains("private-"));
        created.push(response);
    }
    execute(&mut client,&store,"connection.write",json!({"project":"app","env":"dev","name":"erp",
        "parameters":{"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}})).await;
    let prod = execute(
        &mut client,
        &store,
        "connection.get",
        json!({"project":"app","env":"prod","name":"erp"}),
    )
    .await;
    assert_eq!(prod["active_generation"], 1);
    assert_eq!(prod["parameters"]["tls-verification"], "disabled");
    let copied = execute(
        &mut client,
        &store,
        "connection.copy",
        json!({"project":"app","env":"prod","source_env":"dev","name":"erp"}),
    )
    .await;
    assert_eq!(copied["active_generation"], 2);
    assert_eq!(copied["parameters"]["tls-verification"], "verify-authority");
    assert_eq!(
        store.0.lock().unwrap().len(),
        2,
        "copy must never write a credential"
    );
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let rows = sqlx::query("SELECT c.env,c.active_generation,g.credential_id::text FROM registry.connection c \
        JOIN registry.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) ORDER BY c.env").fetch_all(&mut client).await.unwrap();
    assert_eq!(rows[0].get::<i64, _>(1), 2);
    assert_eq!(rows[1].get::<i64, _>(1), 2);
    let source: String = rows[0].get(2);
    let target: String = rows[1].get(2);
    assert_ne!(source, target);
    let credential_ids: Vec<String> = sqlx::query(
        "SELECT credential_id::text FROM registry.connection_generation \
        WHERE connection_id=$1::text::uuid ORDER BY generation",
    )
    .bind(&created[1]["id"].as_str().unwrap())
    .fetch_all(&mut client)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get(0))
    .collect();
    assert_eq!(credential_ids, vec![target.clone(), target]);
    let metadata: String =
        sqlx::query("SELECT jsonb_agg(to_jsonb(k))::text FROM identity.credential k")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get(0);
    assert!(!metadata.contains("private-dev"));
    assert!(!metadata.contains("private-prod"));
}

#[tokio::test]
async fn oauth_writers_invalidate_rows_in_the_generation_transaction() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("BEGIN; SET LOCAL ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql("acme", "app", "dev"))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT").execute(&mut client).await.map(drop).unwrap();
    let store = RecordingStore::default();
    execute(&mut client, &store, "integration.write", json!({"project":"app","name":"oauth","integration_type":"http","auth_type":"oauth2_client_credentials"})).await;
    let mut parameters = json!({"primary-authority":"https://example.invalid","tls-verification":"disabled",
        "auth":{"client_id":"first","token_url":"https://example.invalid/token","scopes":[]}});
    let created = execute(
        &mut client,
        &store,
        "connection.write",
        json!({"project":"app","env":"dev","name":"oauth",
        "parameters":parameters,"credential":r#"{"client_secret":"private-first"}"#}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    for (operation, change_auth) in [
        ("connection.set_credential", false),
        ("connection.write", false),
        ("connection.write", true),
    ] {
        sqlx::query("INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,updated_at)
            VALUES($1::text::uuid,'\\x01','\\x02','\\x03','file:test',now()+interval '1 hour',now())").bind(&id).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
        sqlx::query("INSERT INTO identity.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at)
            SELECT 'pending',id,active_generation,'\\x04','\\x03','\\x02','file:test',service_principal_id,now()+interval '10 minutes'
            FROM registry.connection WHERE id=$1::text::uuid").bind(&id).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
        let mut request = json!({"project":"app","env":"dev","name":"oauth"});
        if change_auth {
            parameters["auth"]["client_id"] = "second".into();
        } else {
            request["credential"] = r#"{"client_secret":"private-next"}"#.into();
        }
        if operation == "connection.write" {
            request["parameters"] = parameters.clone();
        }
        let result = execute(&mut client, &store, operation, request).await;
        assert!(result["active_generation"].as_i64().unwrap() > 1);
        let counts = sqlx::query("SELECT (SELECT count(*) FROM identity.oauth_token),(SELECT count(*) FROM identity.oauth_authorization)").fetch_one(&mut client).await.unwrap();
        assert_eq!(counts.get::<i64, _>(0), 0, "{operation} token invalidation");
        assert_eq!(
            counts.get::<i64, _>(1),
            0,
            "{operation} pending authorization invalidation"
        );
    }
}

#[tokio::test]
async fn jwt_connection_write_enforces_credential_and_auth_schemas() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("BEGIN; SET LOCAL ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql("acme", "app", "dev"))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT").execute(&mut client).await.map(drop).unwrap();
    let store = RecordingStore::default();
    execute(&mut client, &store, "integration.write", json!({"project":"app","name":"jwt","integration_type":"http","auth_type":"oauth2_client_credentials_jwt"})).await;
    let valid = json!({"project":"app","env":"dev","name":"jwt",
        "parameters":{"primary-authority":"https://example.invalid","tls-verification":"disabled",
            "auth":{"client_id":"client","token_url":"https://example.invalid/token","key_id":"certificate-id","algorithm":"PS256","scopes":["a","b"]}},
        "credential":r#"{"private_key":"PEM is parsed when the host signs"}"#});
    let mut invalid = Vec::new();
    for field in ["client_id", "token_url", "key_id", "algorithm"] {
        let mut request = valid.clone();
        request["parameters"]["auth"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        invalid.push(request);
    }
    for (field, value) in [
        ("algorithm", json!("PS384")),
        ("scope_separator", json!(";")),
        ("scopes", json!([1])),
        ("client_id", json!(1)),
        ("token_url", json!(1)),
        ("key_id", json!(1)),
    ] {
        let mut request = valid.clone();
        request["parameters"]["auth"][field] = value;
        invalid.push(request);
    }
    for credential in [
        "{}",
        r#"{"private_key":1}"#,
        r#"{"private_key":"PEM","extra":true}"#,
        r#"{"client_secret":"secret"}"#,
    ] {
        let mut request = valid.clone();
        request["credential"] = credential.into();
        invalid.push(request);
    }
    for request in invalid {
        let mut tx = client.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
        let result = administration::execute_with_store(
            &mut tx,
            "acme",
            "connection.write",
            &request.to_string(),
            &mut AuthDrops::default(),
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
            Some(&store),
        )
        .await;
        assert_eq!(result.unwrap_err().code, "invalid_input");
        tx.rollback().await.unwrap();
    }
    assert!(
        store.0.lock().unwrap().is_empty(),
        "refused values never enter the store"
    );
    for algorithm in ["PS256", "ES256", "RS256"] {
        for separator in [None, Some(" "), Some(",")] {
            let mut request = valid.clone();
            request["parameters"]["auth"]["algorithm"] = algorithm.into();
            if let Some(separator) = separator {
                request["parameters"]["auth"]["scope_separator"] = separator.into();
            }
            let result = execute(&mut client, &store, "connection.write", request).await;
            assert!(result["id"].is_string());
            assert!(!result.to_string().contains("PEM"));
        }
    }
    let mut request = valid;
    request["parameters"]["auth"]
        .as_object_mut()
        .unwrap()
        .remove("scopes");
    let result = execute(&mut client, &store, "connection.write", request).await;
    assert!(result["id"].is_string());
}

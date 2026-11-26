//! Connection administration against the installed project schema.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;
use wamn_connection::administration::{ConnectionError, Request, configure, prepare_write};
use wamn_connection::connection_credentials::{StoreVersion, WriteCredentials};
use wamn_connection::project_connections::Administration;
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::kubernetes::CredentialSecret;
use wamn_postgres::provisioning_transaction::provisioning_transaction;
use wamn_registry::OrgId;
use wamn_test_infrastructure::database;

#[derive(Debug, Default)]
struct Secrets(Mutex<BTreeMap<(String, String), Vec<u8>>>);

#[async_trait::async_trait]
impl WriteCredentials for Secrets {
    async fn put(
        &self,
        target: CredentialSecret<'_>,
        id: Uuid,
        value: &[u8],
    ) -> anyhow::Result<StoreVersion> {
        let version = id.to_string();
        self.0
            .lock()
            .unwrap()
            .insert((target.name.clone(), version.clone()), value.to_vec());
        Ok(StoreVersion {
            store: "k8s",
            reference: target.name,
            version,
        })
    }

    async fn destroy(&self, reference: &str, version: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .remove(&(reference.to_owned(), version.to_owned()));
        Ok(())
    }
}

async fn command(
    project: &mut PgConnection,
    system: &mut PgConnection,
    secrets: &Secrets,
    operation: &str,
    payload: &Value,
) -> anyhow::Result<()> {
    let org = "acme".parse::<OrgId>()?;
    let mut transaction = provisioning_transaction(project).await?;
    let mut database = Administration::new(&mut transaction, system, None);
    if operation == "connection.deactivate" {
        configure(&mut database, &org, operation, payload).await?;
        transaction.commit().await?;
        return Ok(());
    }
    let write = prepare_write(
        &mut database,
        |_| unreachable!("these host connections have no wasm authority"),
        Request {
            caller: PrincipalId::from(Uuid::nil()),
            org: &org,
            operation,
            payload: payload.clone(),
        },
        None,
    )
    .await;
    transaction.rollback().await?;
    let env = payload["env"].as_str().unwrap().parse()?;
    let project_id = payload["project"].as_str().unwrap().parse()?;
    let staged = write?.stage(secrets, &org, &project_id, &env).await?;
    let mut transaction = provisioning_transaction(project).await?;
    let mut database = Administration::new(&mut transaction, system, None);
    staged.publish(&mut database, &env).await?;
    transaction.commit().await?;
    Ok(())
}

async fn credentials(
    project: &mut PgConnection,
    id: Uuid,
) -> sqlx::Result<Vec<(Uuid, String, String)>> {
    sqlx::query_as(
        "SELECT id,ref,version FROM platform.credential WHERE connection_id=$1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(project)
    .await
}

async fn generations(
    project: &mut PgConnection,
    id: Uuid,
) -> sqlx::Result<Vec<(i64, Option<Uuid>)>> {
    sqlx::query_as(
        "SELECT generation,credential_id FROM platform.connection_generation
         WHERE connection_id=$1 ORDER BY generation",
    )
    .bind(id)
    .fetch_all(project)
    .await
}

async fn is_active(project: &mut PgConnection, id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT active FROM platform.connection WHERE id=$1")
        .bind(id)
        .fetch_one(project)
        .await
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn deactivation_keeps_history_and_activation_writes_a_fresh_generation() -> anyhow::Result<()>
{
    let project = database::project().await?;
    let system = database::system().await?;
    let mut project = project.admin().await?;
    let mut system = system.admin().await?;
    let secrets = Secrets::default();
    let mut tx = provisioning_transaction(&mut project).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ('invoices','http','oauth2_authorization_code',gen_random_uuid(),1,gen_random_uuid()) RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.connection_generation(connection_id,generation,parameters)
         VALUES ($1,1,$2)",
    )
    .bind(id)
    .bind(json!({
        "primary-authority":"api.example.invalid",
        "tls-verification":"verify-authority",
        "auth":{"client_id":"test-client","authorize_url":"https://api.example.invalid/authorize","token_url":"/token","scopes":[]}
    }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut request = json!({"project":"billing","env":"dev","name":"invoices"});
    for key in ["older fixture value", "current fixture value"] {
        request["credential"] = json!(json!({"client_secret":key}).to_string());
        command(
            &mut project,
            &mut system,
            &secrets,
            "connection.set_credential",
            &request,
        )
        .await?;
    }
    request.as_object_mut().unwrap().remove("credential");
    let saved_credentials = credentials(&mut project, id).await?;
    let saved_generations = generations(&mut project, id).await?;
    let active_generation = saved_generations.last().unwrap().0;
    let mut tx = provisioning_transaction(&mut project).await?;
    sqlx::query(
        "INSERT INTO platform.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,updated_at)
         VALUES ($1,decode('01','hex'),decode('02','hex'),decode('03','hex'),'fixture',now(),now())",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at)
         VALUES ('fixture',$1,$2,decode('01','hex'),decode('02','hex'),decode('03','hex'),'fixture',gen_random_uuid(),now())",
    )
    .bind(id)
    .bind(active_generation)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    command(
        &mut project,
        &mut system,
        &secrets,
        "connection.deactivate",
        &request,
    )
    .await?;
    assert!(!is_active(&mut project, id).await?);
    assert_eq!(credentials(&mut project, id).await?, saved_credentials);
    assert_eq!(generations(&mut project, id).await?, saved_generations);
    let oauth_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM platform.oauth_token WHERE connection_id=$1)
             OR EXISTS (SELECT FROM platform.oauth_authorization WHERE connection_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut project)
    .await?;
    assert!(!oauth_exists);

    let refused = command(
        &mut project,
        &mut system,
        &secrets,
        "connection.activate",
        &request,
    )
    .await
    .unwrap_err();
    let refusal = refused.downcast_ref::<ConnectionError>().unwrap();
    assert_eq!(
        (refusal.code, refusal.field),
        ("invalid_input", "credential")
    );

    let replacement = json!({"client_secret":"replacement fixture value"}).to_string();
    request["credential"] = json!(replacement);
    command(
        &mut project,
        &mut system,
        &secrets,
        "connection.activate",
        &request,
    )
    .await?;
    assert!(is_active(&mut project, id).await?);
    let (generation, credential) = generations(&mut project, id).await?.pop().unwrap();
    let credential = credential.unwrap();
    assert_eq!(generation, active_generation + 1);
    assert!(!saved_credentials.iter().any(|(id, _, _)| *id == credential));
    let (_, reference, version) = credentials(&mut project, id)
        .await?
        .into_iter()
        .find(|(id, _, _)| *id == credential)
        .unwrap();
    assert_eq!(
        secrets
            .0
            .lock()
            .unwrap()
            .get(&(reference, version))
            .unwrap(),
        replacement.as_bytes()
    );
    let mut tx = provisioning_transaction(&mut project).await?;
    let plain_id: Uuid = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ('plain','http','none',gen_random_uuid(),1,gen_random_uuid()) RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO platform.connection_generation(connection_id,generation,parameters) VALUES ($1,1,$2)")
        .bind(plain_id)
        .bind(json!({"primary-authority":"api.example.invalid","tls-verification":"verify-authority"}))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let plain = json!({"project":"billing","env":"dev","name":"plain"});
    command(
        &mut project,
        &mut system,
        &secrets,
        "connection.deactivate",
        &plain,
    )
    .await?;
    command(
        &mut project,
        &mut system,
        &secrets,
        "connection.activate",
        &plain,
    )
    .await?;
    assert!(is_active(&mut project, plain_id).await?);
    let (generation, credential) = generations(&mut project, plain_id).await?.pop().unwrap();
    assert!(generation > 1 && credential.is_none());
    Ok(())
}

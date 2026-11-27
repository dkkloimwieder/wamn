//! SQL operations on the caller's open project administration transaction.

use std::pin::Pin;

use crate::administration::StoredCredential;
use crate::administration::{ConnectionError, refuse};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::OrgId;

/// Whether administration needs to hold the row until transaction end.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Lookup {
    Read,
    Lock,
}

/// Data for a new connection before its first generation is written.
#[derive(Debug)]
pub(crate) struct NewConnection {
    pub id: Uuid,
    pub integration: String,
    pub auth: String,
    pub external: Option<Uuid>,
    pub target: Uuid,
    pub resource: Option<String>,
    pub authority: Option<String>,
}

/// How a credential or parameter change affects the current token.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TokenChange {
    Keep,
    Delete,
    Expire,
}

/// Parameters and immutable coordinates read from the copy source.
#[derive(Debug)]
pub struct CopySource {
    pub(crate) integration_type: String,
    pub(crate) auth_type: String,
    pub(crate) parameters: Value,
    pub(crate) authority: Option<String>,
}

/// Connection data projected from the project database for the control wire adapter.
#[derive(Debug)]
pub struct ConnectionRow {
    pub id: Uuid,
    pub name: String,
    pub integration_type: String,
    pub auth_type: String,
    pub external_system_id: Option<Uuid>,
    pub target_id: Uuid,
    pub resource_key: Option<String>,
    pub authority: Option<String>,
    pub active_generation: i64,
    pub service_principal_id: Uuid,
    pub active: bool,
    pub parameters: Value,
    pub credential_created_at: Option<String>,
    pub credential_created_by: Option<Uuid>,
    pub refresh_status: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConnectionState {
    pub id: Uuid,
    pub generation: i64,
    pub active: bool,
}

#[derive(Debug)]
pub(crate) struct WriteContext {
    pub state: ConnectionState,
    pub integration_type: String,
    pub auth_type: String,
    pub authority: Option<String>,
    pub external_system_id: Option<Uuid>,
    pub service_principal_id: Uuid,
}

type WriteService = for<'a, 'session> fn(
    &'a mut PgConnection,
    &'a mut Transaction<'session, Postgres>,
    &'a str,
    &'a str,
) -> Pin<
    Box<dyn Future<Output = anyhow::Result<PrincipalId>> + Send + 'a>,
>;

/// The authorized project and system sessions for one administration request.
#[derive(Debug)]
pub struct Administration<'a, 'session> {
    project: &'a mut Transaction<'session, Postgres>,
    system: &'a mut PgConnection,
    write_service: Option<WriteService>,
}

impl<'a, 'session> Administration<'a, 'session> {
    /// Borrow the request's project transaction and system session and its existing service writer.
    pub fn new(
        project: &'a mut Transaction<'session, Postgres>,
        system: &'a mut PgConnection,
        write_service: Option<WriteService>,
    ) -> Self {
        Self {
            project,
            system,
            write_service,
        }
    }

    pub(crate) async fn connection_id(
        &mut self,
        name: Option<&str>,
        lookup: Lookup,
    ) -> Result<Option<Uuid>, ConnectionError> {
        let sql = match lookup {
            Lookup::Read => "SELECT id FROM platform.connection WHERE name=$1",
            Lookup::Lock => "SELECT id FROM platform.connection WHERE name=$1 FOR NO KEY UPDATE",
        };
        sqlx::query_scalar(sql)
            .bind(name)
            .fetch_optional(&mut **self.project)
            .await
            .map_err(database_error)
    }

    pub(crate) async fn lock_connection(
        &mut self,
        name: &str,
    ) -> Result<Option<WriteContext>, ConnectionError> {
        sqlx::query(
            "SELECT id,active_generation,active,integration_type,auth_type,
                    authority,external_system_id,service_principal_id
             FROM platform.connection WHERE name=$1 FOR UPDATE",
        )
        .bind(name)
        .try_map(|row: PgRow| {
            Ok(WriteContext {
                state: ConnectionState {
                    id: row.try_get("id")?,
                    generation: row.try_get("active_generation")?,
                    active: row.try_get("active")?,
                },
                integration_type: row.try_get("integration_type")?,
                auth_type: row.try_get("auth_type")?,
                authority: row.try_get("authority")?,
                external_system_id: row.try_get("external_system_id")?,
                service_principal_id: row.try_get("service_principal_id")?,
            })
        })
        .fetch_optional(&mut **self.project)
        .await
        .map_err(database_error)
    }

    pub(crate) async fn integration_metadata(
        &mut self,
        integration: Option<&str>,
        auth: Option<&str>,
    ) -> Result<Option<Value>, ConnectionError> {
        sqlx::query_scalar("SELECT to_jsonb(t)||jsonb_build_object('auth_schema',a.parameter_schema,'credential_schema',a.credential_schema) FROM registry.integration_type t JOIN registry.integration_type_auth allowed ON allowed.integration_type=t.code JOIN registry.auth_type a ON a.code=allowed.auth_type WHERE t.code=$1 AND a.code=$2")
            .bind(integration).bind(auth).fetch_optional(&mut *self.system).await.map_err(database_error)
    }

    pub(crate) async fn target(
        &mut self,
        caller: PrincipalId,
        operation: &str,
        org: &OrgId,
        env: Option<&str>,
        exclusive: bool,
    ) -> Result<Uuid, ConnectionError> {
        let mut tx = self.system.begin().await.map_err(database_error)?;
        bind_actor(&mut tx, caller, operation).await?;
        let (target, target_type): (Uuid, String) = sqlx::query_as(
            "SELECT destination.id, destination.type FROM registry.env e \
                 JOIN registry.target destination ON destination.id=e.target_id \
                 WHERE e.org=$1 AND e.env=$2",
        )
        .bind(org.as_str())
        .bind(env)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| refuse("invalid_input", "target_id"))?;
        if exclusive && target_type == "cloud" {
            return Err(refuse(
                "exclusive_connection_requires_local_target",
                "integration_type",
            ));
        }
        tx.commit().await.map_err(database_error)?;
        Ok(target)
    }

    pub(crate) async fn versions(
        &mut self,
        integration: Option<&str>,
    ) -> Result<Vec<Value>, ConnectionError> {
        sqlx::query_scalar("SELECT to_jsonb(v) FROM registry.integration_type_version v WHERE integration_type=$1 ORDER BY version")
            .bind(integration).fetch_all(&mut *self.system).await.map_err(database_error)
    }

    pub(crate) async fn previous(
        &mut self,
        id: Uuid,
        generation: Option<i64>,
    ) -> Result<Option<(Value, Option<Uuid>)>, ConnectionError> {
        sqlx::query_as("SELECT parameters,credential_id FROM platform.connection_generation WHERE connection_id=$1 AND generation=$2")
            .bind(id).bind(generation).fetch_optional(&mut **self.project).await.map_err(database_error)
    }

    pub(crate) async fn required_version(
        &mut self,
        caller: PrincipalId,
        operation: &str,
        org: &OrgId,
        env: &str,
        name: &str,
    ) -> Result<Option<i32>, ConnectionError> {
        let mut transaction = self.system.begin().await.map_err(database_error)?;
        bind_actor(&mut transaction, caller, operation).await?;
        let version = required_connection_version(
            &mut transaction,
            org,
            &env.parse().map_err(|_| refuse("invalid_input", "env"))?,
            name,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;

        Ok(version)
    }

    pub(crate) async fn external_system_id(
        &mut self,
        name: Option<&str>,
        lookup: Lookup,
    ) -> Result<Option<Uuid>, ConnectionError> {
        let sql = match lookup {
            Lookup::Read => "SELECT id FROM platform.external_system WHERE name=$1",
            Lookup::Lock => "SELECT id FROM platform.external_system WHERE name=$1 FOR UPDATE",
        };
        sqlx::query_scalar(sql)
            .bind(name)
            .fetch_optional(&mut **self.project)
            .await
            .map_err(database_error)
    }

    pub(crate) async fn write_service(
        &mut self,
        name: &str,
        connection: &str,
    ) -> Result<PrincipalId, ConnectionError> {
        let write_service = self
            .write_service
            .take()
            .expect("one principal per connection");
        write_service(self.system, self.project, name, connection)
            .await
            .map_err(|source| {
                ConnectionError::with_source("database_error", "principal_id", source)
            })
    }

    pub(crate) async fn resource_value(
        &mut self,
        parameters: &Value,
        field: &str,
    ) -> Result<Option<String>, ConnectionError> {
        sqlx::query_scalar("SELECT $1::jsonb ->> $2::text")
            .bind(parameters)
            .bind(field)
            .fetch_one(&mut **self.project)
            .await
            .map_err(database_error)
    }

    pub(crate) async fn insert_connection(
        &mut self,
        row: &NewConnection,
        name: &str,
        principal: PrincipalId,
    ) -> Result<(), ConnectionError> {
        sqlx::query("INSERT INTO platform.connection(id,name,integration_type,auth_type,external_system_id,target_id,resource_key,active_generation,service_principal_id,authority) VALUES($1,$2,$3,$4,$5,$6,$7,1,$8,$9)")
                .bind(row.id).bind(name).bind(&row.integration).bind(&row.auth).bind(row.external).bind(row.target).bind(row.resource.as_deref()).bind(Uuid::from(principal)).bind(row.authority.as_deref()).execute(&mut **self.project).await.map_err(connection_write_error)?;

        Ok(())
    }

    pub(crate) async fn write_generation(
        &mut self,
        id: Uuid,
        credential_id: Option<Uuid>,
        stored: Option<&StoredCredential>,
        token_change: TokenChange,
        parameters: &Value,
    ) -> Result<(), ConnectionError> {
        let next: i64=sqlx::query_scalar("SELECT coalesce(max(generation),0)+1 FROM platform.connection_generation WHERE connection_id=$1").bind(id).fetch_one(&mut **self.project).await.map_err(database_error)?;
        if let Some(stored) = stored {
            sqlx::query("INSERT INTO platform.credential(id,connection_id,store,ref,version) VALUES($1,$2,$3,$4,$5)")
                .bind(stored.id).bind(id).bind(stored.version.store).bind(&stored.version.reference).bind(&stored.version.version).execute(&mut **self.project).await.map_err(database_error)?;
        }
        match token_change {
            TokenChange::Delete => {
                sqlx::query("DELETE FROM platform.oauth_token WHERE connection_id=$1")
                    .bind(id)
                    .execute(&mut **self.project)
                    .await
                    .map_err(database_error)?;
            }
            TokenChange::Expire => {
                sqlx::query(
                    "UPDATE platform.oauth_token SET expires_at='-infinity' WHERE connection_id=$1",
                )
                .bind(id)
                .execute(&mut **self.project)
                .await
                .map_err(database_error)?;
            }
            TokenChange::Keep => {}
        }
        sqlx::query("DELETE FROM platform.oauth_authorization WHERE connection_id=$1")
            .bind(id)
            .execute(&mut **self.project)
            .await
            .map_err(database_error)?;
        sqlx::query("INSERT INTO platform.connection_generation(connection_id,generation,parameters,credential_id) VALUES($1,$2,$3,$4)")
            .bind(id).bind(next).bind(parameters).bind(credential_id).execute(&mut **self.project).await.map_err(database_error)?;
        sqlx::query("UPDATE platform.connection SET active_generation=$2 WHERE id=$1")
            .bind(id)
            .bind(next)
            .execute(&mut **self.project)
            .await
            .map_err(database_error)?;

        Ok(())
    }

    pub(crate) async fn configure_integration_type(
        &mut self,
        org: &OrgId,
        operation: &str,
        request: &Value,
    ) -> Result<Value, ConnectionError> {
        configure_integration_type(self.system, org, operation, request)
            .await
            .map_err(database_error)
    }

    pub(crate) async fn list_external_systems(&mut self) -> Result<Value, ConnectionError> {
        sqlx::query_scalar("SELECT jsonb_build_object('external_systems',coalesce(jsonb_agg(to_jsonb(s) ORDER BY name),'[]'::jsonb)) FROM platform.external_system s").fetch_one(&mut **self.project).await.map_err(database_error)
    }

    pub(crate) async fn insert_external_system(
        &mut self,
        name: Option<&str>,
    ) -> Result<(), ConnectionError> {
        sqlx::query(
            "INSERT INTO platform.external_system(name) VALUES($1) ON CONFLICT(name) DO NOTHING",
        )
        .bind(name)
        .execute(&mut **self.project)
        .await
        .map_err(database_error)?;
        Ok(())
    }

    pub(crate) async fn delete_external_system(&mut self, id: Uuid) -> Result<(), ConnectionError> {
        sqlx::query("DELETE FROM platform.external_system WHERE id=$1")
            .bind(id)
            .execute(&mut **self.project)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    /// Refuse deleting an external system that another row references.
    pub(crate) async fn refuse_external_system_references(
        &mut self,
        id: Uuid,
    ) -> Result<(), ConnectionError> {
        super::references::refuse_external_system_references(self.project, id).await
    }

    pub(crate) async fn connection_ids(&mut self) -> Result<Vec<Uuid>, ConnectionError> {
        sqlx::query_scalar("SELECT id FROM platform.connection ORDER BY name")
            .fetch_all(&mut **self.project)
            .await
            .map_err(database_error)
    }

    pub(crate) async fn test_metadata(
        &mut self,
        integration: Option<&str>,
        auth: Option<&str>,
    ) -> Result<Value, ConnectionError> {
        sqlx::query_scalar("SELECT jsonb_build_object('runtime_type',t.runtime_type,'auth_schema',a.parameter_schema,'credential_schema',a.credential_schema,'versions',(SELECT jsonb_object_agg(v.version,to_jsonb(v)) FROM registry.integration_type_version v WHERE v.integration_type=t.code),'schemas',(SELECT jsonb_agg(parameter_schema ORDER BY version) FROM registry.integration_type_version WHERE integration_type=t.code)) FROM registry.integration_type t CROSS JOIN registry.auth_type a WHERE t.code=$1 AND a.code=$2")
                .bind(integration).bind(auth).fetch_one(&mut *self.system).await.map_err(database_error)
    }

    pub(crate) async fn credential_reference(
        &mut self,
        id: Uuid,
    ) -> Result<Value, ConnectionError> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('store',k.store,'ref',k.ref,'version',k.version) FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) JOIN platform.credential k ON k.id=g.credential_id WHERE c.id=$1").bind(id).fetch_optional(&mut **self.project).await.map_err(database_error)?.unwrap_or(Value::Null))
    }

    pub(crate) async fn activate_connection(&mut self, id: Uuid) -> Result<(), ConnectionError> {
        sqlx::query("UPDATE platform.connection SET active=true WHERE id=$1")
            .bind(id)
            .execute(&mut **self.project)
            .await
            .map_err(connection_write_error)?;
        Ok(())
    }

    pub(crate) async fn deactivate_connection(
        &mut self,
        id: Uuid,
    ) -> Result<Value, ConnectionError> {
        let row = sqlx::query_scalar(
            "UPDATE platform.connection SET active=false WHERE id=$1
             RETURNING jsonb_build_object('service_principal_id',service_principal_id,
                  'credentials',(SELECT coalesce(jsonb_agg(jsonb_build_object(
                       'store',store,'ref',ref,'version',version)),'[]'::jsonb)
                  FROM platform.credential WHERE connection_id=$1))",
        )
        .bind(id)
        .fetch_one(&mut **self.project)
        .await
        .map_err(connection_write_error)?;
        sqlx::query("DELETE FROM platform.oauth_token WHERE connection_id=$1")
            .bind(id)
            .execute(&mut **self.project)
            .await
            .map_err(database_error)?;
        sqlx::query("DELETE FROM platform.oauth_authorization WHERE connection_id=$1")
            .bind(id)
            .execute(&mut **self.project)
            .await
            .map_err(database_error)?;
        Ok(row)
    }

    pub(crate) async fn connection_summary(
        &mut self,
        id: Uuid,
    ) -> Result<ConnectionRow, ConnectionError> {
        sqlx::query(
            "SELECT c.id,c.name,c.integration_type,c.auth_type,c.external_system_id,
                    c.target_id,c.resource_key,c.authority,c.active_generation,c.service_principal_id,
                    c.active,g.parameters,to_jsonb(k.created_at) #>> '{}' AS credential_created_at,
                    k.created_by AS credential_created_by,o.refresh_status,
                    to_jsonb(o.expires_at) #>> '{}' AS expires_at
             FROM platform.connection c
             JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation)
             LEFT JOIN platform.credential k ON k.id=g.credential_id
             LEFT JOIN platform.oauth_token o ON o.connection_id=c.id WHERE c.id=$1",
        )
        .bind(id)
        .try_map(|row: PgRow| Ok(ConnectionRow {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            integration_type: row.try_get("integration_type")?,
            auth_type: row.try_get("auth_type")?,
            external_system_id: row.try_get("external_system_id")?,
            target_id: row.try_get("target_id")?,
            resource_key: row.try_get("resource_key")?,
            authority: row.try_get("authority")?,
            active_generation: row.try_get("active_generation")?,
            service_principal_id: row.try_get("service_principal_id")?,
            active: row.try_get("active")?,
            parameters: row.try_get("parameters")?,
            credential_created_at: row.try_get("credential_created_at")?,
            credential_created_by: row.try_get("credential_created_by")?,
            refresh_status: row.try_get("refresh_status")?,
            expires_at: row.try_get("expires_at")?,
        }))
        .fetch_one(&mut **self.project)
        .await
        .map_err(database_error)
    }

    pub(crate) async fn version_summary(
        &mut self,
        integration: &str,
    ) -> Result<(Vec<i32>, Vec<String>), ConnectionError> {
        sqlx::query_as(
            "SELECT ARRAY(SELECT version FROM registry.integration_type_version
                          WHERE integration_type=$1 ORDER BY version),
                    ARRAY(SELECT DISTINCT f.key FROM registry.integration_type_version v,
                          jsonb_each(v.parameter_schema->'properties') f
                          WHERE v.integration_type=$1 AND f.value->>'deprecated'='true'
                          ORDER BY f.key)",
        )
        .bind(integration)
        .fetch_one(&mut *self.system)
        .await
        .map_err(database_error)
    }
}

/// Read the source generation in the caller's authorized source transaction.
pub async fn copy_source(
    source: &mut PgConnection,
    name: &str,
) -> Result<Option<CopySource>, ConnectionError> {
    sqlx::query(
        "SELECT c.integration_type,c.auth_type,g.parameters,c.authority FROM platform.connection c
         JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation)
         WHERE c.name=$1",
    )
    .bind(name)
    .try_map(|row: PgRow| Ok(CopySource {
        integration_type: row.try_get("integration_type")?,
        auth_type: row.try_get("auth_type")?,
        parameters: row.try_get("parameters")?,
        authority: row.try_get("authority")?,
    }))
    .fetch_optional(source)
    .await
    .map_err(database_error)
}

pub(super) fn database_error(source: sqlx::Error) -> ConnectionError {
    ConnectionError::with_source("database_error", "connection", source.into())
}

fn connection_write_error(error: sqlx::Error) -> ConnectionError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        refuse("resource_in_use", "resource_key")
    } else {
        database_error(error)
    }
}

async fn bind_actor(
    tx: &mut Transaction<'_, Postgres>,
    caller: PrincipalId,
    operation: &str,
) -> Result<(), ConnectionError> {
    wamn_postgres::bind_transaction_identity(tx, &caller, "", "", Some(operation), None)
        .await
        .map_err(database_error)
}

/// Read the environment's version for a named connection.
pub async fn required_connection_version(
    system: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
    name: &str,
) -> Result<Option<i32>, ConnectionError> {
    sqlx::query_scalar(
        "SELECT version FROM registry.env_integration WHERE org=$1 AND env=$2 AND name=$3",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .bind(name)
    .fetch_optional(system)
    .await
    .map_err(database_error)
}

/// Run the registry operation for an integration type.
///
/// # Errors
/// Returns an error if the registry operation fails.
pub async fn configure_integration_type(
    connection: &mut PgConnection,
    org: &OrgId,
    operation: &str,
    request: &Value,
) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar("SELECT registry.configure_integration_type($1,$2,$3)")
        .bind(org.as_str())
        .bind(operation)
        .bind(request)
        .fetch_one(connection)
        .await
}

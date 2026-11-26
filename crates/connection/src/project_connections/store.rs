use serde_json::Value;
use sqlx::Row as _;
use std::collections::BTreeMap;
use uuid::Uuid;
use wamn_registry::connections::{ConnectionFact, ConnectionGeneration, CredentialReference};

struct ConnectionGenerationRow {
    name: String,
    integration_type: String,
    auth_type: String,
    external_system: Option<String>,
    parameters: Value,
}

pub(super) struct ConnectionStore<'a> {
    connection: &'a mut sqlx::PgConnection,
}

impl<'a> ConnectionStore<'a> {
    pub(super) fn new(connection: &'a mut sqlx::PgConnection) -> Self {
        Self { connection }
    }

    pub(super) async fn fact(&mut self, id: &Uuid) -> Result<Option<ConnectionFact>, sqlx::Error> {
        let Some(row) = sqlx::query("SELECT c.active_generation,c.active,g.credential_id,c.target_id,c.resource_key,c.service_principal_id,c.authority FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.id=$1")
        .bind(id).fetch_optional(&mut *self.connection).await? else { return Ok(None); };
        Ok(Some(ConnectionFact {
            active_generation: row.try_get("active_generation")?,
            active: row.try_get("active")?,
            credential_id: row.try_get("credential_id")?,
            target_id: row.try_get("target_id")?,
            resource_key: row.try_get("resource_key")?,
            authority: row.try_get("authority")?,
            service_principal_id: row.try_get::<Uuid, _>("service_principal_id")?.into(),
        }))
    }

    pub(super) async fn generation(
        &mut self,
        id: &Uuid,
        generation: i64,
    ) -> Result<Option<ConnectionGeneration>, sqlx::Error> {
        let Some(row)=sqlx::query("SELECT c.name,c.integration_type,c.auth_type,c.external_system_id::text AS external_system,g.parameters FROM platform.connection c JOIN platform.connection_generation g ON g.connection_id=c.id WHERE c.id=$1 AND g.generation=$2").bind(id).bind(generation).fetch_optional(&mut *self.connection).await? else{return Ok(None)};
        let row = ConnectionGenerationRow {
            name: row.try_get("name")?,
            integration_type: row.try_get("integration_type")?,
            auth_type: row.try_get("auth_type")?,
            external_system: row.try_get("external_system")?,
            parameters: row.try_get("parameters")?,
        };
        Ok(Some(ConnectionGeneration {
            name: row.name,
            integration_type: wamn_postgres::decode_text(row.integration_type, "integration_type")?,
            auth_type: row.auth_type,
            external_system: row.external_system,
            parameters: row.parameters,
        }))
    }

    pub(super) async fn credential(
        &mut self,
        id: &Uuid,
    ) -> Result<Option<CredentialReference>, sqlx::Error> {
        Ok(sqlx::query_as::<_, (String, String)>(
            "SELECT ref,version FROM platform.credential WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(&mut *self.connection)
        .await?
        .map(|(reference, version)| CredentialReference { reference, version }))
    }

    pub(super) async fn env(&mut self) -> Result<BTreeMap<String, Uuid>, sqlx::Error> {
        Ok(sqlx::query_as::<_, (String, Uuid)>(
            "SELECT name,id FROM platform.connection ORDER BY name",
        )
        .fetch_all(&mut *self.connection)
        .await?
        .into_iter()
        .collect())
    }
}

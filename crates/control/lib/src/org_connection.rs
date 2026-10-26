//! Org connection declarations and the snapshot used by environment apply.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use tokio_postgres::Client;
use wamn_catalog::{RequirementType, org_credential_handle};
use wamn_control_registry::sql::select_org_connections_sql;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::connections::{ConnectionWrite, write_connection};

/// Non-secret inputs to an org connection declaration.
#[derive(Debug)]
pub struct DeclareConnectionRequest {
    pub system_database_url: String,
    pub org: String,
    pub name: String,
    pub requirement_type: RequirementType,
    pub definition: Value,
}

/// The resolved inputs retained for one apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConnection {
    pub requirement_type: RequirementType,
    pub definition: Value,
    pub credential_handle: Option<String>,
    pub org_connection_name: Option<String>,
}

/// Validate the declaration without reading credentials or contacting the endpoint.
fn validate(
    org: &str,
    name: &str,
    requirement_type: RequirementType,
    definition: &Value,
) -> anyhow::Result<()> {
    wamn_control_registry::validate_org_id(org).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    org_credential_handle(org, name).map_err(anyhow::Error::msg)?;
    ensure!(
        requirement_type == RequirementType::Http,
        "org connections support only http"
    );
    requirement_type
        .check_definition(definition)
        .map_err(anyhow::Error::msg)
}

/// Insert or update a definition, refusing a type change. An identical retry writes nothing.
pub async fn declare(
    request: &DeclareConnectionRequest,
    changes: &AuthChanges,
) -> anyhow::Result<bool> {
    validate(
        &request.org,
        &request.name,
        request.requirement_type,
        &request.definition,
    )?;
    let mut client = crate::environment::analyse::connect(&request.system_database_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the org connection declaration")?;
    transaction
        .batch_execute("SET LOCAL ROLE wamn_system")
        .await?;
    transaction
        .batch_execute(
            &wamn_control_provision::platform_principals::bind_platform_principal_sql(
                wamn_project_state::PlatformComponent::Provisioning,
            ),
        )
        .await?;
    let old = transaction.query_opt(
        "SELECT id::text, integration, parameters::text, external_system_id::text FROM registry.connection WHERE org = $1 AND name = $2",
        &[&request.org, &request.name],
    ).await?;
    if let Some(old) = &old {
        ensure!(
            old.get::<_, String>(1) == "http",
            "an existing org connection cannot change type"
        );
        let definition: Value = serde_json::from_str(old.get(2))?;
        if definition == request.definition {
            return Ok(false);
        }
    }
    let mut drops = AuthDrops::default();
    write_connection(
        &transaction,
        &request.org,
        &ConnectionWrite {
            id: old.as_ref().map(|row| row.get(0)),
            integration: "http".into(),
            external_system_id: old.as_ref().and_then(|row| row.get(3)),
            name: request.name.clone(),
            transport: "http".into(),
            exchange: "request".into(),
            parameters: request.definition.clone(),
            status: "active".into(),
        },
        &mut drops,
    )
    .await?;
    transaction
        .commit()
        .await
        .context("commit the org connection declaration")?;
    changes.publish(drops).await;
    Ok(true)
}

/// Read all declarations for this org in one database snapshot.
pub async fn snapshot(
    client: &Client,
    org: &str,
) -> anyhow::Result<BTreeMap<String, ResolvedConnection>> {
    let mut connections = BTreeMap::new();
    for row in client
        .query(select_org_connections_sql(), &[&org])
        .await
        .context("read org connections")?
    {
        let name: String = row.get(0);
        let requirement_type: String = row.get(1);
        let requirement_type = match requirement_type.as_str() {
            "http" => RequirementType::Http,
            "gcs" | "s3" => RequirementType::Blobstore,
            _ => anyhow::bail!("org connection {name} has unsupported type {requirement_type}"),
        };
        let definition: Value = serde_json::from_str(row.get(2))?;
        requirement_type
            .check_definition(&definition)
            .map_err(anyhow::Error::msg)?;
        connections.insert(
            name.clone(),
            ResolvedConnection {
                requirement_type,
                definition,
                credential_handle: Some(format!(
                    "{}@{}",
                    org_credential_handle(org, &name).map_err(anyhow::Error::msg)?,
                    row.get::<_, i64>(3)
                )),
                org_connection_name: Some(name),
            },
        );
    }
    Ok(connections)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_validation_refuses_invalid_coordinates_types_and_definitions() {
        let definition = serde_json::json!({"primary-authority": "https://erp.example", "tls-verification": "verify-authority"});
        validate("acme", "erp", RequirementType::Http, &definition).unwrap();
        assert!(validate("", "erp", RequirementType::Http, &definition).is_err());
        assert!(validate("acme", "bad:name", RequirementType::Http, &definition).is_err());
        assert!(validate("acme", "erp", RequirementType::Blobstore, &definition).is_err());
        assert!(validate("acme", "erp", RequirementType::Http, &serde_json::json!({})).is_err());
        let mut secret = definition;
        secret["secret"] = "forbidden".into();
        assert!(validate("acme", "erp", RequirementType::Http, &secret).is_err());
    }
}

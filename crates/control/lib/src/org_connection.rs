//! Org connection declarations and the snapshot used by environment apply.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use tokio_postgres::Client;
use wamn_catalog::{RequirementType, org_credential_handle};
use wamn_control_registry::sql::{select_org_connections_sql, upsert_org_connection_sql};

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
    kind: RequirementType,
    definition: &Value,
) -> anyhow::Result<()> {
    wamn_control_registry::validate_org_id(org).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    org_credential_handle(org, name).map_err(anyhow::Error::msg)?;
    ensure!(
        kind == RequirementType::Http,
        "org connections support only http"
    );
    kind.check_definition(definition)
        .map_err(anyhow::Error::msg)
}

/// Insert or update a definition, refusing a type change. An identical retry writes nothing.
pub async fn declare(request: &DeclareConnectionRequest) -> anyhow::Result<bool> {
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
    let kind = request.requirement_type.descriptor().requirement_type;
    let changed = transaction
        .execute(
            upsert_org_connection_sql(),
            &[
                &request.org,
                &request.name,
                &kind,
                &request.definition.to_string(),
            ],
        )
        .await
        .context("declare the org connection")?;
    let stored: String = transaction
        .query_one(
            "SELECT type FROM registry.org_connections WHERE org = $1 AND name = $2",
            &[&request.org, &request.name],
        )
        .await?
        .get(0);
    ensure!(
        stored == kind,
        "an existing org connection cannot change type"
    );
    transaction
        .commit()
        .await
        .context("commit the org connection declaration")?;
    Ok(changed != 0)
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
        let kind: String = row.get(1);
        ensure!(
            kind == "http",
            "org connection {name} has unsupported type {kind}"
        );
        let definition: Value = serde_json::from_str(row.get(2))?;
        validate(org, &name, RequirementType::Http, &definition)?;
        connections.insert(
            name.clone(),
            ResolvedConnection {
                requirement_type: RequirementType::Http,
                definition,
                credential_handle: Some(
                    org_credential_handle(org, &name).map_err(anyhow::Error::msg)?,
                ),
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

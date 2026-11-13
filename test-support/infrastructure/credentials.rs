//! Test credentials held outside the database.
pub use wamn_kubernetes::test_util::MemoryStore;
use wamn_kubernetes::{Error, Store, StoreVersion};

/// A reference to material that the fixture already wrote outside Postgres.
#[derive(Debug)]
pub struct ReferencedCredentials<'a>(pub &'a StoreVersion);

#[async_trait::async_trait]
impl Store for ReferencedCredentials<'_> {
    async fn put(
        &self,
        _: &wamn_registry::Triple,
        _: uuid::Uuid,
        _: &[u8],
    ) -> Result<StoreVersion, Error> {
        Ok(StoreVersion {
            store: self.0.store,
            reference: self.0.reference.clone(),
            version: self.0.version.clone(),
        })
    }
    async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, Error> {
        unreachable!("fixture only writes references")
    }
    async fn destroy(&self, _: &str, _: &str) -> Result<(), Error> {
        unreachable!("fixture retains its store")
    }
}

/// Create project authority through the administration functions.
pub async fn define(
    tx: &tokio_postgres::Transaction<'_>,
    org: &str,
    request: &serde_json::Value,
    store: &dyn Store,
) -> anyhow::Result<()> {
    use serde_json::json;
    use wamn_broker::auth_changes::AuthDrops;
    use wamn_platform_identity::connections::administration;
    let mut drops = AuthDrops::default();
    if let Some(name) = request["external_system"].as_str() {
        administration::execute(
            tx,
            org,
            "external_system.write",
            &json!({"project":request["project"],"name":name}).to_string(),
            &mut drops,
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        )
        .await?;
    }
    administration::execute(tx,org,"integration.write",&json!({
        "project":request["project"],"name":request["name"],"integration_type":request["integration_type"],
        "auth_type":request["auth_type"],"external_system":request["external_system"],
    }).to_string(),&mut drops, &mut wamn_broker::connection_changes::ConnectionDrops::default()).await?;
    let mut connection = json!({"project":request["project"],"env":request["env"],"name":request["name"],"parameters":request["parameters"]});
    if request["auth_type"] != "none" {
        connection["credential"] = request
            .get("credential")
            .cloned()
            .unwrap_or_else(|| json!("{}"));
    }
    administration::execute_with_store(
        tx,
        org,
        "connection.write",
        &connection.to_string(),
        &mut drops,
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        Some(store),
    )
    .await?;
    Ok(())
}

/// Rotate an existing fixture through the administration path.
pub async fn set_reference(
    tx: &tokio_postgres::Transaction<'_>,
    org: &str,
    project: &str,
    environment: &str,
    name: &str,
    stored: &StoreVersion,
) -> anyhow::Result<()> {
    wamn_platform_identity::connections::administration::execute_with_store(
        tx,
        org,
        "connection.set_credential",
        &serde_json::json!({"project":project,"env":environment,"name":name,"credential":"{}"})
            .to_string(),
        &mut wamn_broker::auth_changes::AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        Some(&ReferencedCredentials(stored)),
    )
    .await?;
    Ok(())
}

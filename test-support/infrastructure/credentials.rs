//! Test credential values and immutable database references.

use wamn_credential_store::{Error, Store, StoreVersion, file::FileStore};

/// A private file store whose directory lives as long as its readers.
#[derive(Debug)]
pub struct LocalCredentials {
    directory: crate::scratch::ScratchRoot,
    file: FileStore,
}

impl LocalCredentials {
    pub fn create() -> anyhow::Result<Self> {
        let directory = crate::scratch::ScratchRoot::create()?;
        let file = FileStore::new(directory.path().to_owned());
        Ok(Self { directory, file })
    }

    pub fn directory(&self) -> &std::path::Path {
        self.directory.path()
    }
}

#[async_trait::async_trait]
impl Store for LocalCredentials {
    async fn put(&self, id: uuid::Uuid, value: &[u8]) -> Result<StoreVersion, Error> {
        self.file.put(id, value).await
    }
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        self.file.get(reference, version).await
    }
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.file.destroy(reference, version).await
    }
}

/// A reference to material that the fixture already wrote outside Postgres.
#[derive(Debug)]
pub struct ReferencedCredentials<'a>(pub &'a StoreVersion);

#[async_trait::async_trait]
impl Store for ReferencedCredentials<'_> {
    async fn put(&self, _: uuid::Uuid, _: &[u8]) -> Result<StoreVersion, Error> {
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
    use wamn_platform_identity::{auth_changes::AuthDrops, connections::administration};
    let mut drops = AuthDrops::default();
    if let Some(name) = request["external_system"].as_str() {
        administration::execute(
            tx,
            org,
            "external_system.write",
            &json!({"project":request["project"],"name":name}).to_string(),
            &mut drops,
        )
        .await?;
    }
    administration::execute(tx,org,"integration.write",&json!({
        "project":request["project"],"name":request["name"],"integration_type":request["integration_type"],
        "auth_type":request["auth_type"],"external_system":request["external_system"],
    }).to_string(),&mut drops).await?;
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
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
        Some(&ReferencedCredentials(stored)),
    )
    .await?;
    Ok(())
}

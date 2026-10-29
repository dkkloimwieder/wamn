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

/// Seed a reference after the test creates its environment connection.
pub async fn set_reference(
    tx: &tokio_postgres::Transaction<'_>,
    org: &str,
    project: &str,
    environment: &str,
    name: &str,
    stored: &StoreVersion,
) -> anyhow::Result<()> {
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    let row = tx.query_one("SELECT c.id::text,c.active_generation+1 FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.org=$1 AND c.project=$2 AND c.env=$3 AND i.name=$4 FOR UPDATE OF c", &[&org,&project,&environment,&name]).await?;
    let connection: String = row.get(0);
    let generation: i64 = row.get(1);
    let id = stored.version.clone();
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    tx.execute("INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by) VALUES ($1::text::uuid,$2::text::uuid,$3,$4,$5,now(),$6::text::uuid)", &[&id,&connection,&stored.store,&stored.reference,&stored.version,&actor]).await?;
    tx.execute("INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by) VALUES ($1::text::uuid,$2,$3::text::uuid,now(),$4::text::uuid)", &[&connection,&generation,&id,&actor]).await?;
    tx.execute(
        "UPDATE registry.connection SET active_generation=$2 WHERE id=$1::text::uuid",
        &[&connection, &generation],
    )
    .await?;
    Ok(())
}

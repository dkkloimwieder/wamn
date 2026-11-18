//! An in-memory credential fixture. It is never selected by deployed code.
use crate::{CredentialSecret, Error, Store, StoreVersion, failure};
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<BTreeMap<(String, String), Vec<u8>>>);
#[async_trait::async_trait]
impl Store for MemoryStore {
    async fn put(
        &self,
        target: CredentialSecret<'_>,
        id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, Error> {
        let reference = target.name;
        let version = id.to_string();
        self.0
            .lock()
            .expect("credential fixture lock poisoned")
            .insert((reference.clone(), version.clone()), value.to_vec());
        Ok(StoreVersion {
            store: "k8s",
            reference,
            version,
        })
    }
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        self.0
            .lock()
            .expect("credential fixture lock poisoned")
            .get(&(reference.into(), version.into()))
            .cloned()
            .ok_or_else(|| failure("get", reference, "credential absent"))
    }
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.0
            .lock()
            .expect("credential fixture lock poisoned")
            .remove(&(reference.into(), version.into()));
        Ok(())
    }
}

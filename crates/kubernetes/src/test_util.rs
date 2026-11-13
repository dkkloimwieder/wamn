//! An in-memory credential fixture. It is never selected by deployed code.
use crate::{Error, Store, StoreVersion, failure};
use std::{collections::BTreeMap, sync::Mutex};
use wamn_control_registry::{Triple, credential_secret_name};
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<BTreeMap<(String, String), Vec<u8>>>);
#[async_trait::async_trait]
impl Store for MemoryStore {
    async fn put(
        &self,
        scope: &Triple,
        id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, Error> {
        let reference = credential_secret_name(&scope.org, &scope.project, scope.env.as_str());
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

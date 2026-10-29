//! Atomic credential files for local targets.

use crate::{Error, Store, StoreVersion, failure};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

#[derive(Clone, Debug)]
pub struct FileStore {
    directory: PathBuf,
}

impl FileStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn path(&self, reference: &str, version: &str) -> Result<PathBuf, Error> {
        if reference != version || uuid::Uuid::parse_str(reference).is_err() {
            return Err(failure(
                "resolve",
                reference,
                "invalid credential reference",
            ));
        }
        Ok(self.directory.join(reference))
    }
}

#[async_trait::async_trait]
impl Store for FileStore {
    async fn put(&self, credential_id: uuid::Uuid, value: &[u8]) -> Result<StoreVersion, Error> {
        let reference = credential_id.to_string();
        let destination = self.path(&reference, &reference)?;
        let temporary = self.directory.join(format!(".{}", uuid::Uuid::new_v4()));
        let result = write(&temporary, &destination, value).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result.map_err(|source| failure("put", &reference, source))?;
        Ok(StoreVersion {
            store: "file",
            version: reference.clone(),
            reference,
        })
    }

    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        tokio::fs::read(self.path(reference, version)?)
            .await
            .map_err(|source| failure("get", reference, source))
    }

    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        match tokio::fs::remove_file(self.path(reference, version)?).await {
            Ok(()) => Ok(()),
            Err(source) if source.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            Err(source) => Err(failure("destroy", reference, source)),
        }
    }
}

async fn write(temporary: &Path, destination: &Path, value: &[u8]) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temporary)
        .await?;
    file.write_all(value).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temporary, destination).await?;
    tokio::fs::File::open(destination.parent().expect("credential directory"))
        .await?
        .sync_all()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[tokio::test]
    async fn private_atomic_replace_and_missing_destroy() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileStore::new(directory.path().to_owned());
        let id = uuid::Uuid::new_v4();
        let stored = store.put(id, b"first").await.unwrap();
        let path = directory.path().join(&stored.reference);
        let previous = std::fs::File::open(&path).unwrap();
        let inode = previous.metadata().unwrap().ino();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        store.put(id, b"second").await.unwrap();
        assert_ne!(inode, std::fs::metadata(&path).unwrap().ino());
        assert_eq!(
            store.get(&stored.reference, &stored.version).await.unwrap(),
            b"second"
        );
        let mut old = Vec::new();
        (&previous).read_to_end(&mut old).unwrap();
        assert_eq!(old, b"first");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        store
            .destroy(&stored.reference, &stored.version)
            .await
            .unwrap();
        store
            .destroy(&stored.reference, &stored.version)
            .await
            .unwrap();
    }
}

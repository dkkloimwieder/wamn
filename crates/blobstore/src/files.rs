//! Streaming storage for native file routes.
//!
//! The native caller authorizes the application binding before opening this store.
//! Upload to `<environment>/_wamn_files/staging/<id>`, commit metadata, then call `finish`.
//! The collector repairs committed staging and removes aged staging with no metadata row.
//! Public copies use `<environment>/public/_wamn_files/live/<id>` beneath the binding prefix.

use std::{
    error::Error,
    fmt,
    time::{Duration, SystemTime},
};

use futures_util::{StreamExt as _, TryStreamExt as _, stream::BoxStream};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use uuid::Uuid;
use wamn_storage::store::{self, ObjectError, ObjectPath, ObjectSigner, ObjectStream};

use crate::intake::MAX_OBJECT_BYTES;
use crate::store::{BoundContainer, StoreError};

/// Maximum lifetime of a file signed URL.
pub const MAX_URL_LIFETIME: Duration = Duration::from_mins(15);

/// The category of a native storage failure, before boundary translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageErrorType {
    Refused,
    NotFound,
    Body,
    Backend,
    Deadline,
}

/// A storage failure with its operation and original cause.
#[derive(Debug)]
pub struct FileStorageError {
    pub type_: FileStorageErrorType,
    pub operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl FileStorageError {
    pub fn new(
        type_: FileStorageErrorType,
        operation: &'static str,
        source: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            operation,
            source: source.into(),
        }
    }
}
impl fmt::Display for FileStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "file storage {} failed: {}", self.operation, self.source)
    }
}
impl Error for FileStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Metadata computed from the exact bytes sent to staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub id: Uuid,
    pub blob_key: String,
    pub size: u64,
    pub sha256: wamn_digest::Digest,
}

/// The file namespace beneath the environment segment.
pub const FILE_PREFIX: &str = "_wamn_files/";
/// Wait one minute beyond the longest invocation before removing uncommitted staging.
pub const MIN_STAGING_AGE: Duration =
    wamn_application::MAX_HOST_CALL_DURATION.saturating_add(Duration::from_mins(1));

/// One canonical live or staged object returned by the confined collector listing.
#[derive(Debug)]
pub struct FileObject {
    path: ObjectPath,
    pub blob_key: String,
    pub last_modified: SystemTime,
}

impl FileObject {
    /// The exact bucket key, including the binding prefix and public namespace.
    pub fn object_key(&self) -> &str {
        self.path.as_ref()
    }
}

/// Refuse raw changes to either private or public file objects.
pub(super) fn permit_raw_mutation(key: &str) -> Result<(), StoreError> {
    // Object-store Path removes empty segments. Apply the reservation to that spelling.
    let canonical = ObjectPath::new(key);
    let key = canonical.as_ref();
    let reserved = |key: &str| {
        let key = key.strip_prefix("public/").unwrap_or(key);
        key == FILE_PREFIX.trim_end_matches('/') || key.starts_with(FILE_PREFIX)
    };
    if key.split_once('/').is_some_and(|(_, key)| reserved(key)) {
        return Err(StoreError::Refused {
            verb: "raw mutation",
            reason: "file-managed keys are reserved for native file operations",
        });
    }
    Ok(())
}

fn backend(operation: &'static str, source: ObjectError) -> FileStorageError {
    let type_ = if source
        .source()
        .is_some_and(<dyn Error>::is::<tokio::time::error::Elapsed>)
    {
        FileStorageErrorType::Deadline
    } else if source.is_not_found() {
        FileStorageErrorType::NotFound
    } else {
        FileStorageErrorType::Backend
    };
    FileStorageError::new(type_, operation, source.into_source())
}

/// A store built from the connection's active generation.
#[derive(Clone)]
pub struct StoredFiles {
    bound: BoundContainer,
    signer: ObjectSigner,
    cdn_host: Option<url::Url>,
    environment: wamn_registry::EnvName,
    deadline: tokio::time::Instant,
}
impl fmt::Debug for StoredFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

/// The live key stored in `platform.file.blob_key`.
pub fn live_key(environment: &wamn_registry::EnvName, id: Uuid) -> String {
    format!("{environment}/{FILE_PREFIX}live/{id}")
}
fn staging_key(blob_key: &str) -> String {
    blob_key.replacen(
        &format!("{FILE_PREFIX}live/"),
        &format!("{FILE_PREFIX}staging/"),
        1,
    )
}
fn public_key(blob_key: &str) -> String {
    blob_key.replacen(FILE_PREFIX, &format!("public/{FILE_PREFIX}"), 1)
}

impl StoredFiles {
    /// Stream canonical live, public and staged objects beneath this generation's prefix.
    /// Malformed keys are logged and skipped. Listing never buffers the whole bucket.
    /// Maintenance uses each client's request limit without a deadline over the whole scan.
    pub async fn live_objects(
        &self,
    ) -> Result<BoxStream<'_, Result<FileObject, FileStorageError>>, FileStorageError> {
        self.permit("collect", true).await?;
        let private = self.path(&format!("{}/{FILE_PREFIX}live", self.environment))?;
        let public = self.path(&format!("{}/public/{FILE_PREFIX}live", self.environment))?;
        let staged = self.path(&format!("{}/{FILE_PREFIX}staging", self.environment))?;
        Ok(futures_util::stream::iter([private, public, staged])
            .flat_map(move |prefix| {
                let directory = format!("{}/", prefix.as_ref());
                self.bound
                    .store
                    .list(Some(&prefix))
                    .map_err(|error| backend("list files", error))
                    .try_filter_map(move |meta| {
                        let key = meta.location.as_ref();
                        let parsed = key.strip_prefix(&directory).and_then(|suffix| {
                            suffix
                                .parse::<Uuid>()
                                .ok()
                                .filter(|id| id.to_string() == suffix)
                        });
                        let object = if let Some(id) = parsed {
                            Some(FileObject {
                                path: meta.location.into(),
                                blob_key: live_key(&self.environment, id),
                                last_modified: meta.last_modified.into(),
                            })
                        } else {
                            tracing::warn!(
                                object_key = key,
                                "file collector skipped an unparseable key"
                            );
                            None
                        };
                        std::future::ready(Ok(object))
                    })
            })
            .boxed())
    }

    /// Delete only a listed canonical object in this store, after the caller reads its row.
    /// Re-read its age so a replacement since listing remains protected.
    pub async fn delete_collected(
        &self,
        object: &FileObject,
        cutoff: SystemTime,
    ) -> Result<bool, FileStorageError> {
        self.permit("collect", true).await?;
        self.collected_is_staged(object)?;
        let meta = match self.bound.store.head(&object.path).await {
            Ok(meta) => meta,
            Err(error) if error.is_not_found() => return Ok(false),
            Err(error) => return Err(backend("head orphan", error)),
        };
        if SystemTime::from(meta.last_modified) >= cutoff {
            return Ok(false);
        }
        match self.bound.store.delete(&object.path).await {
            Ok(()) => Ok(true),
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => Err(backend("delete orphan", error)),
        }
    }

    fn collected_is_staged(&self, object: &FileObject) -> Result<bool, FileStorageError> {
        let staged = self.path(&staging_key(&object.blob_key))?;
        if !object
            .blob_key
            .starts_with(&format!("{}/{FILE_PREFIX}live/", self.environment))
            || (object.path != self.path(&object.blob_key)?
                && object.path != self.path(&public_key(&object.blob_key))?
                && object.path != staged)
        {
            return Err(FileStorageError::new(
                FileStorageErrorType::Refused,
                "collect",
                "listed object belongs to another prefix",
            ));
        }
        Ok(object.path == staged)
    }

    /// Reconcile one listed object after an exact catalog lookup in this physical store.
    /// A row keeps live bytes and promotes staging. Absence permits only aged deletion.
    /// `now` and `live_cutoff` come from the collector's clock at the start of its pass.
    pub async fn reconcile_collected(
        &self,
        object: &FileObject,
        row_blob_key: Option<&str>,
        live_cutoff: SystemTime,
        now: SystemTime,
    ) -> Result<bool, FileStorageError> {
        self.permit("collect", true).await?;
        let staged = self.collected_is_staged(object)?;
        if let Some(key) = row_blob_key {
            if key != object.blob_key {
                return Err(FileStorageError::new(
                    FileStorageErrorType::Refused,
                    "collect",
                    "catalog row belongs to another object",
                ));
            }
            if staged {
                self.repair(key).await?;
            }
            return Ok(false);
        }
        let cutoff = if staged {
            now - MIN_STAGING_AGE
        } else {
            live_cutoff
        };
        self.delete_collected(object, cutoff).await
    }

    pub(super) fn new(
        bound: BoundContainer,
        signer: ObjectSigner,
        cdn_host: Option<url::Url>,
        environment: &wamn_registry::EnvName,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            bound,
            signer,
            cdn_host,
            environment: environment.to_owned(),
            deadline,
        }
    }

    async fn run<T>(
        &self,
        operation: &'static str,
        work: impl Future<Output = Result<T, FileStorageError>>,
    ) -> Result<T, FileStorageError> {
        let remaining = self
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(FileStorageError::new(
                FileStorageErrorType::Deadline,
                operation,
                "invocation deadline elapsed",
            ));
        }
        tokio::time::timeout(remaining, work)
            .await
            .map_err(|error| {
                FileStorageError::new(FileStorageErrorType::Deadline, operation, error)
            })?
    }

    async fn permit(&self, operation: &'static str, write: bool) -> Result<(), FileStorageError> {
        self.bound
            .permit_connection(operation)
            .await
            .map_err(|error| {
                FileStorageError::new(FileStorageErrorType::Refused, operation, error)
            })?;
        if write {
            self.bound.permit_write(operation).map_err(|error| {
                FileStorageError::new(FileStorageErrorType::Refused, operation, error)
            })?;
        }
        Ok(())
    }

    fn path(&self, key: &str) -> Result<ObjectPath, FileStorageError> {
        let valid = key
            .strip_prefix(&format!("{}/", self.environment))
            .is_some_and(|key| {
                [FILE_PREFIX, &format!("public/{FILE_PREFIX}")]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
            });
        if !valid {
            return Err(FileStorageError::new(
                FileStorageErrorType::Refused,
                "key",
                "file key belongs to another environment or an obsolete layout",
            ));
        }
        self.bound
            .path(key)
            .map_err(|error| FileStorageError::new(FileStorageErrorType::Refused, "key", error))
    }

    /// Stream into staging using bounded multipart buffers and compute SHA-256.
    /// The caller must insert no row until this returns successfully.
    pub async fn stage(
        &self,
        id: Uuid,
        mut body: impl AsyncRead + Unpin + Send,
    ) -> Result<StagedFile, FileStorageError> {
        self.run("stage", async {
            self.permit("put", true).await?;
            let path = self.path(&staging_key(&live_key(&self.environment, id)))?;
            // Server-side copies retain this attribute, including the public copy.
            let cache_control = "max-age=3600";
            let mut upload = self
                .bound
                .store
                .start_upload(&path, cache_control, self.deadline)
                .await
                .map_err(|error| backend("stage", error))?;
            let mut hash = wamn_digest::Hasher::new();
            let mut size = 0u64;
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let count = body.read(&mut buffer).await.map_err(|error| {
                    FileStorageError::new(FileStorageErrorType::Body, "read upload", error)
                })?;
                if count == 0 {
                    break;
                }
                size = size
                    .checked_add(count as u64)
                    .filter(|size| *size <= MAX_OBJECT_BYTES as u64)
                    .ok_or_else(|| {
                        FileStorageError::new(
                            FileStorageErrorType::Refused,
                            "size",
                            "file exceeds the 64 MiB upload limit",
                        )
                    })?;
                hash.update(&buffer[..count]);
                upload
                    .write(&buffer[..count])
                    .await
                    .map_err(|error| backend("stage", error))?;
            }
            self.permit("put", true).await?;
            if size == 0 {
                upload
                    .abort()
                    .await
                    .map_err(|error| backend("abort empty multipart", error))?;
                self.permit("put", true).await?;
                self.bound
                    .store
                    .put_empty(&path, cache_control)
                    .await
                    .map_err(|error| backend("stage empty file", error))?;
            } else {
                upload
                    .finish()
                    .await
                    .map_err(|error| backend("stage", error))?;
            }
            Ok(StagedFile {
                id,
                blob_key: live_key(&self.environment, id),
                size,
                sha256: wamn_digest::Digest::from(hash.finalize()),
            })
        })
        .await
    }

    async fn remove_key(&self, key: &str) -> Result<(), FileStorageError> {
        self.permit("delete", false).await?;
        match self.bound.store.delete(&self.path(key)?).await {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(backend("delete", error)),
        }
    }

    async fn repair(&self, blob_key: &str) -> Result<(), FileStorageError> {
        self.permit("repair", false).await?;
        let live = self.path(blob_key)?;
        let staged = self.path(&staging_key(blob_key))?;
        match self.bound.store.copy(&staged, &live).await {
            Ok(()) => self.remove_key(&staging_key(blob_key)).await,
            Err(error) if error.is_not_found() => {
                self.permit("repair", false).await?;
                self.bound
                    .store
                    .head(&live)
                    .await
                    .map(|_| ())
                    .map_err(|error| backend("repair", error))
            }
            Err(error) => Err(backend("repair", error)),
        }
    }

    async fn require_live(&self, blob_key: &str) -> Result<(), FileStorageError> {
        self.permit("head", false).await?;
        match self.bound.store.head(&self.path(blob_key)?).await {
            Ok(_) => Ok(()),
            Err(error) if error.is_not_found() => self.repair(blob_key).await,
            Err(error) => Err(backend("head", error)),
        }
    }

    /// Return a streaming read, with repair when the committed row still names staged bytes.
    pub async fn get(&self, blob_key: &str) -> Result<ObjectStream, FileStorageError> {
        let result = self
            .run("get", async {
                self.permit("get", false).await?;
                match self.bound.store.get(&self.path(blob_key)?).await {
                    Ok(result) => Ok(result),
                    Err(error) if error.is_not_found() => {
                        self.permit("get staging", false).await?;
                        let staged = self
                            .bound
                            .store
                            .get(&self.path(&staging_key(blob_key))?)
                            .await
                            .map_err(|error| backend("get staging", error))?;
                        // Repair is a storage recovery action, also permitted for a read binding.
                        self.repair(blob_key).await?;
                        Ok(staged)
                    }
                    Err(error) => Err(backend("get", error)),
                }
            })
            .await?;
        Ok(store::deadline_stream(result, self.deadline))
    }

    /// After metadata commit, copy staging to live and remove staging. Retries are safe.
    pub async fn finish(&self, id: Uuid) -> Result<(), FileStorageError> {
        self.run("finish", async {
            self.permit("put", true).await?;
            self.repair(&live_key(&self.environment, id)).await
        })
        .await
    }

    /// Delete public, live, and staged objects after the metadata deletion succeeds.
    pub async fn delete(&self, blob_key: &str) -> Result<(), FileStorageError> {
        self.run("delete", async {
            self.permit("delete", true).await?;
            self.remove_key(&public_key(blob_key)).await?;
            self.remove_key(blob_key).await?;
            self.remove_key(&staging_key(blob_key)).await
        })
        .await
    }

    /// Copy to the public namespace and return its generation's CDN URL.
    pub async fn expose(&self, blob_key: &str) -> Result<String, FileStorageError> {
        self.run("expose", async {
            self.permit("expose", true).await?;
            let mut url = self.cdn_host.clone().ok_or_else(|| {
                FileStorageError::new(
                    FileStorageErrorType::Refused,
                    "expose",
                    "public exposure requires cdn_host",
                )
            })?;
            // Establish the live object through the same staging recovery path.
            self.require_live(blob_key).await?;
            let key = public_key(blob_key);
            self.permit("expose", false).await?;
            self.bound
                .store
                .copy(&self.path(blob_key)?, &self.path(&key)?)
                .await
                .map_err(|error| backend("expose", error))?;
            url.set_path(self.path(&key)?.as_ref());
            Ok(url.into())
        })
        .await
    }

    /// Remove the public origin object immediately. Retries are safe.
    pub async fn unexpose(&self, blob_key: &str) -> Result<(), FileStorageError> {
        self.run("unexpose", async {
            self.permit("unexpose", true).await?;
            self.remove_key(&public_key(blob_key)).await
        })
        .await
    }

    /// Sign a confined GET using the pinned generation, for at most fifteen minutes.
    pub async fn signed_url(
        &self,
        blob_key: &str,
        lifetime: Duration,
    ) -> Result<String, FileStorageError> {
        self.run("signed URL", async {
            self.permit("signed URL", false).await?;
            if lifetime.is_zero() {
                return Err(FileStorageError::new(
                    FileStorageErrorType::Refused,
                    "signed URL",
                    "URL lifetime must be positive",
                ));
            }
            self.require_live(blob_key).await?;
            self.permit("signed URL", false).await?;
            self.signer
                .signed_get(&self.path(blob_key)?, lifetime.min(MAX_URL_LIFETIME))
                .await
                .map_err(|error| backend("signed URL", error))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::test_binding::admitted;
    use super::*;
    use std::sync::Arc;

    use bytes::Bytes;
    use object_store::{
        ObjectStore, ObjectStoreExt as _, PutMultipartOptions, PutOptions,
        memory::InMemory,
        path::Path,
        signer::{Method, Signer},
    };
    use std::{
        io,
        pin::Pin,
        sync::Mutex,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;
    use wamn_storage::store::Objects;

    fn store(read_only: bool, cdn: Option<&str>) -> (StoredFiles, Arc<InMemory>) {
        let backend = Arc::new(InMemory::new());
        let bound = admitted(
            Arc::clone(&backend) as Arc<dyn ObjectStore>,
            "bucket",
            "project/app/",
        )
        .with_read_only(read_only);
        (
            StoredFiles::new(
                bound,
                ObjectSigner::new(Arc::new(RecordingSigner::default())),
                cdn.map(|host| url::Url::parse(&format!("https://{host}")).unwrap()),
                &"dev".parse().unwrap(),
                tokio::time::Instant::now() + wamn_application::MAX_HOST_CALL_DURATION,
            ),
            backend,
        )
    }

    #[tokio::test]
    async fn staging_and_get_repair_use_the_bound_prefix() {
        let (files, backend) = store(false, Some("cdn.example.com"));
        let id = Uuid::new_v4();
        let staged = files.stage(id, &b"abc"[..]).await.unwrap();
        assert_eq!(staged.size, 3);
        let staging = Path::from(format!("project/app/dev/_wamn_files/staging/{id}"));
        let live = Path::from(format!(
            "project/app/{}",
            live_key(&"dev".parse().unwrap(), id)
        ));
        assert!(backend.head(&staging).await.is_ok());
        assert!(backend.head(&live).await.is_err());
        let reader = StoredFiles {
            bound: files.bound.clone().with_read_only(true),
            ..files.clone()
        };
        assert_eq!(
            reader
                .get(&live_key(&"dev".parse().unwrap(), id))
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .concat(),
            b"abc"[..]
        );
        assert!(backend.head(&staging).await.is_err());
        assert!(backend.head(&live).await.is_ok());
        files.finish(id).await.unwrap();
        files
            .expose(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        files
            .expose(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        let public = Path::from(format!(
            "project/app/{}",
            public_key(&live_key(&"dev".parse().unwrap(), id))
        ));
        backend.head(&public).await.unwrap();
        files
            .unexpose(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        files
            .unexpose(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        assert!(matches!(
            backend.get(&public).await,
            Err(object_store::Error::NotFound { .. })
        ));
        files
            .delete(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        files
            .delete(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        assert_eq!(
            files
                .get(&live_key(&"dev".parse().unwrap(), id))
                .await
                .err()
                .unwrap()
                .type_,
            FileStorageErrorType::NotFound
        );
    }

    #[tokio::test]
    async fn collection_deletes_uncommitted_staging_only_after_the_safety_window() {
        let (files, backend) = store(false, None);
        let staged = files.stage(Uuid::new_v4(), &b"orphan"[..]).await.unwrap();
        let object = files
            .live_objects()
            .await
            .unwrap()
            .try_next()
            .await
            .unwrap()
            .unwrap();
        let old_enough = object.last_modified + MIN_STAGING_AGE;
        assert!(
            !files
                .reconcile_collected(
                    &object,
                    None,
                    object.last_modified,
                    old_enough - Duration::from_millis(1),
                )
                .await
                .unwrap()
        );
        assert!(backend.head(&object.path).await.is_ok());
        assert!(
            files
                .reconcile_collected(
                    &object,
                    None,
                    object.last_modified,
                    old_enough + Duration::from_millis(1),
                )
                .await
                .unwrap()
        );
        assert!(matches!(
            backend.head(&object.path).await,
            Err(object_store::Error::NotFound { .. })
        ));
        assert!(matches!(
            backend.head(&files.path(&staged.blob_key).unwrap()).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn collection_promotes_staging_with_a_catalog_row_and_removes_staging() {
        let (files, backend) = store(false, None);
        let bytes = b"committed before the host stopped";
        let row = files.stage(Uuid::new_v4(), bytes.as_slice()).await.unwrap();
        let object = files
            .live_objects()
            .await
            .unwrap()
            .try_next()
            .await
            .unwrap()
            .unwrap();
        // The catalog lookup returns the canonical live key, even while bytes remain staged.
        assert!(
            !files
                .reconcile_collected(
                    &object,
                    Some(&row.blob_key),
                    object.last_modified,
                    object.last_modified,
                )
                .await
                .unwrap()
        );
        assert_eq!(
            backend
                .get(&files.path(&row.blob_key).unwrap())
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            bytes
        );
        assert!(matches!(
            backend.head(&object.path).await,
            Err(object_store::Error::NotFound { .. })
        ));
        files
            .reconcile_collected(
                &object,
                Some(&row.blob_key),
                object.last_modified,
                object.last_modified,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn collection_refuses_objects_from_another_environment_on_the_same_store() {
        let (files, backend) = store(false, None);
        let other = StoredFiles {
            environment: "prod".parse().unwrap(),
            ..files.clone()
        };
        let id = Uuid::new_v4();
        files.stage(id, &b"dev"[..]).await.unwrap();
        files.finish(id).await.unwrap();
        other.stage(id, &b"prod"[..]).await.unwrap();
        other.finish(id).await.unwrap();
        let objects = other
            .live_objects()
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].blob_key, format!("prod/_wamn_files/live/{id}"));
        assert_eq!(
            files
                .delete_collected(&objects[0], SystemTime::now() + Duration::from_secs(1))
                .await
                .unwrap_err()
                .type_,
            FileStorageErrorType::Refused
        );
        assert_eq!(
            files
                .live_objects()
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            backend
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn empty_and_multi_part_uploads_hash_the_transferred_bytes() {
        let (files, _) = store(false, None);
        for data in [Vec::new(), vec![42; 11 * 1024 * 1024]] {
            let id = Uuid::new_v4();
            let metadata = files.stage(id, data.as_slice()).await.unwrap();
            assert_eq!(metadata.size, data.len() as u64);
            assert_eq!(metadata.sha256, wamn_digest::bytes_sha256(&data));
            files.finish(id).await.unwrap();
            assert_eq!(
                files
                    .get(&live_key(&"dev".parse().unwrap(), id))
                    .await
                    .unwrap()
                    .try_collect::<Vec<_>>()
                    .await
                    .unwrap()
                    .concat(),
                data
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stage_deadline_stops_the_upload_and_its_tasks() {
        use object_store::limit::LimitStore;

        for (bytes, expected) in [
            (6 * 1024 * 1024, FileStorageErrorType::Body),
            (11 * 1024 * 1024, FileStorageErrorType::Deadline),
        ] {
            let (mut files, backend) = store(false, None);
            // Creation succeeds, but both part uploads and abort wait for a permit.
            files.bound.store = Objects::new(Arc::new(LimitStore::new(Arc::clone(&backend), 0)));
            files.deadline = tokio::time::Instant::now() + Duration::from_millis(10);
            let tasks = tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks();
            let body = tokio::io::repeat(42).take(bytes).chain(BrokenBody);
            assert_eq!(
                files.stage(Uuid::new_v4(), body).await.unwrap_err().type_,
                expected
            );
            if expected == FileStorageErrorType::Deadline {
                assert_eq!(tokio::time::Instant::now(), files.deadline);
            } else {
                assert!(tokio::time::Instant::now() < files.deadline);
            }
            tokio::task::yield_now().await;
            tokio::time::advance(files.deadline - tokio::time::Instant::now()).await;
            // Let cancellation run without advancing beyond the invocation deadline.
            tokio::task::yield_now().await;
            assert_eq!(tokio::time::Instant::now(), files.deadline);
            assert_eq!(
                tokio::runtime::Handle::current()
                    .metrics()
                    .num_alive_tasks(),
                tasks
            );
            assert!(
                backend
                    .list(None)
                    .try_collect::<Vec<_>>()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn collection_reaches_staging_after_the_invocation_time_limit() {
        use object_store::throttle::{ThrottleConfig, ThrottledStore};

        let (mut files, backend) = store(false, None);
        let row = files
            .stage(Uuid::new_v4(), &b"committed"[..])
            .await
            .unwrap();
        files.bound.store = Objects::new(Arc::new(ThrottledStore::new(
            Arc::clone(&backend),
            ThrottleConfig {
                wait_list_per_call: Duration::from_secs(11),
                ..Default::default()
            },
        )));
        let mut objects = files.live_objects().await.unwrap();
        let object = objects.try_next().await.unwrap().unwrap();
        assert!(tokio::time::Instant::now() > files.deadline);
        let now = SystemTime::now();
        files
            .reconcile_collected(&object, Some(&row.blob_key), now, now)
            .await
            .unwrap();
        assert!(backend.head(&object.path).await.is_err());
        assert!(
            backend
                .head(&files.path(&row.blob_key).unwrap())
                .await
                .is_ok()
        );
        files.delete_collected(&object, now).await.unwrap();
    }

    struct BrokenBody;
    impl AsyncRead for BrokenBody {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "truncated upload",
            )))
        }
    }
    #[tokio::test(start_paused = true)]
    async fn failed_and_cancelled_bodies_leave_no_committed_object() {
        let (files, backend) = store(false, None);
        let too_large = tokio::io::repeat(0).take(MAX_OBJECT_BYTES as u64 + 1);
        let overflow_id = Uuid::new_v4();
        assert_eq!(
            files.stage(overflow_id, too_large).await.unwrap_err().type_,
            FileStorageErrorType::Refused
        );
        assert!(
            backend
                .head(&Path::from(format!(
                    "project/app/{}",
                    staging_key(&live_key(&"dev".parse().unwrap(), overflow_id))
                )))
                .await
                .is_err()
        );
        let id = Uuid::new_v4();
        assert_eq!(
            files.stage(id, BrokenBody).await.unwrap_err().type_,
            FileStorageErrorType::Body
        );
        let (body, _sender) = tokio::io::duplex(1024);
        let operation = files.stage(id, body);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), operation)
                .await
                .is_err()
        );
        tokio::task::yield_now().await;
        assert!(
            backend
                .head(&Path::from(format!(
                    "project/app/{}",
                    staging_key(&live_key(&"dev".parse().unwrap(), id))
                )))
                .await
                .is_err()
        );
        assert!(
            backend
                .head(&Path::from(format!(
                    "project/app/{}",
                    live_key(&"dev".parse().unwrap(), id)
                )))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn raw_mutations_cannot_overwrite_or_remove_private_or_public_files() {
        let (files, _) = store(false, Some("cdn.example.com"));
        let id = Uuid::new_v4();
        files.bound.put("ordinary", vec![1]).await.unwrap();
        files.stage(id, &b"original"[..]).await.unwrap();
        for key in [
            staging_key(&live_key(&"dev".parse().unwrap(), id)),
            live_key(&"dev".parse().unwrap(), id),
            public_key(&live_key(&"dev".parse().unwrap(), id)),
            format!("dev//public//_wamn_files/live/{id}"),
            format!("prod/_wamn_files/live/{id}"),
            format!("prod/public/_wamn_files/live/{id}"),
        ] {
            assert!(matches!(
                files.bound.put(&key, vec![2]).await,
                Err(StoreError::Refused { .. })
            ));
            assert!(matches!(
                files.bound.delete(&key).await,
                Err(StoreError::Refused { .. })
            ));
        }
        assert!(files.bound.clear().await.is_err());
        assert_eq!(
            files
                .bound
                .get_range("ordinary", 0, u64::MAX)
                .await
                .unwrap(),
            vec![1]
        );
        assert!(files.path("../other/secret").is_err());
        files.finish(id).await.unwrap();
        files
            .expose(&live_key(&"dev".parse().unwrap(), id))
            .await
            .unwrap();
        assert!(files.bound.clear().await.is_err());
        assert_eq!(
            files
                .get(&live_key(&"dev".parse().unwrap(), id))
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .concat(),
            b"original"[..]
        );
        // Similar author keys remain legal.
        files
            .bound
            .put("_wamn_files_backup/item", vec![3])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn read_bindings_refuse_all_file_mutations() {
        let (files, backend) = store(true, Some("cdn.example.com"));
        let id = Uuid::new_v4();
        assert_eq!(
            files.stage(id, &b"x"[..]).await.unwrap_err().type_,
            FileStorageErrorType::Refused
        );
        assert!(files.finish(id).await.is_err());
        assert!(
            files
                .delete(&live_key(&"dev".parse().unwrap(), id))
                .await
                .is_err()
        );
        assert!(
            files
                .expose(&live_key(&"dev".parse().unwrap(), id))
                .await
                .is_err()
        );
        assert!(
            files
                .unexpose(&live_key(&"dev".parse().unwrap(), id))
                .await
                .is_err()
        );
        assert!(
            backend
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[derive(Debug, Default)]
    struct RecordingSigner(Mutex<Vec<(Path, Duration)>>);
    #[async_trait::async_trait]
    impl Signer for RecordingSigner {
        async fn signed_url(
            &self,
            method: Method,
            path: &Path,
            expires: Duration,
        ) -> object_store::Result<url::Url> {
            assert_eq!(method, Method::GET);
            self.0.lock().unwrap().push((path.clone(), expires));
            Ok(url::Url::parse("https://signed.example.com/read").unwrap())
        }
    }
    #[derive(Debug, Default)]
    struct MetadataOnlyStore {
        inner: Arc<InMemory>,
        calls: Mutex<Vec<(&'static str, Path)>>,
        fail_head: std::sync::atomic::AtomicBool,
    }

    impl fmt::Display for MetadataOnlyStore {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("MetadataOnlyStore")
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for MetadataOnlyStore {
        async fn put_opts(
            &self,
            path: &Path,
            payload: object_store::PutPayload,
            options: PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            self.inner.put_opts(path, payload, options).await
        }
        async fn put_multipart_opts(
            &self,
            path: &Path,
            options: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            self.inner.put_multipart_opts(path, options).await
        }
        async fn get_opts(
            &self,
            path: &Path,
            options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            assert!(
                options.head,
                "existence-only operations must not open a full GET"
            );
            self.calls.lock().unwrap().push(("head", path.clone()));
            if self.fail_head.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(object_store::Error::Generic {
                    store: "test",
                    source: "HEAD failed".into(),
                });
            }
            self.inner.get_opts(path, options).await
        }
        fn delete_stream(
            &self,
            paths: futures_util::stream::BoxStream<'static, object_store::Result<Path>>,
        ) -> futures_util::stream::BoxStream<'static, object_store::Result<Path>> {
            self.inner.delete_stream(paths)
        }
        fn list(
            &self,
            prefix: Option<&Path>,
        ) -> futures_util::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            self.inner.list(prefix)
        }
        async fn list_with_delimiter(
            &self,
            prefix: Option<&Path>,
        ) -> object_store::Result<object_store::ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }
        async fn copy_opts(
            &self,
            from: &Path,
            to: &Path,
            options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            self.calls.lock().unwrap().push(("copy", to.clone()));
            self.inner.copy_opts(from, to, options).await
        }
    }

    #[tokio::test]
    async fn expose_and_sign_use_head_and_repair_only_a_missing_live_object() {
        for missing_live in [false, true] {
            let backend = Arc::new(MetadataOnlyStore::default());
            let bound = admitted(
                Arc::<MetadataOnlyStore>::clone(&backend),
                "bucket",
                "project/app",
            );
            let files = StoredFiles::new(
                bound,
                ObjectSigner::new(Arc::new(RecordingSigner::default())),
                Some(url::Url::parse("https://cdn.example.com").unwrap()),
                &"dev".parse().unwrap(),
                tokio::time::Instant::now() + wamn_application::MAX_HOST_CALL_DURATION,
            );
            let id = Uuid::new_v4();
            let live = files.path(&live_key(&"dev".parse().unwrap(), id)).unwrap();
            let staged = files
                .path(&staging_key(&live_key(&"dev".parse().unwrap(), id)))
                .unwrap();
            backend
                .inner
                .put(&staged, Bytes::from_static(b"staged").into())
                .await
                .unwrap();
            if !missing_live {
                backend
                    .inner
                    .put(&live, Bytes::from_static(b"live").into())
                    .await
                    .unwrap();
            }
            files
                .signed_url(
                    &live_key(&"dev".parse().unwrap(), id),
                    Duration::from_secs(30),
                )
                .await
                .unwrap();
            files
                .expose(&live_key(&"dev".parse().unwrap(), id))
                .await
                .unwrap();
            {
                let calls = backend.calls.lock().unwrap();
                assert_eq!(calls.iter().filter(|(verb, _)| *verb == "head").count(), 2);
                assert_eq!(
                    calls
                        .iter()
                        .any(|(verb, to)| *verb == "copy" && to == &*live),
                    missing_live
                );
            }
            assert_eq!(backend.inner.head(&staged).await.is_err(), missing_live);
            let bytes = backend
                .inner
                .get(
                    &files
                        .path(&public_key(&live_key(&"dev".parse().unwrap(), id)))
                        .unwrap(),
                )
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(
                bytes.as_ref(),
                if missing_live {
                    &b"staged"[..]
                } else {
                    &b"live"[..]
                }
            );
        }
    }

    #[tokio::test]
    async fn expose_and_sign_propagate_head_failures_without_repair() {
        let backend = Arc::new(MetadataOnlyStore::default());
        backend
            .fail_head
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let bound = admitted(
            Arc::<MetadataOnlyStore>::clone(&backend),
            "bucket",
            "project/app",
        );
        let signer = Arc::new(RecordingSigner::default());
        let files = StoredFiles::new(
            bound,
            ObjectSigner::new(Arc::<RecordingSigner>::clone(&signer)),
            Some(url::Url::parse("https://cdn.example.com").unwrap()),
            &"dev".parse().unwrap(),
            tokio::time::Instant::now() + wamn_application::MAX_HOST_CALL_DURATION,
        );
        let id = Uuid::new_v4();
        assert_eq!(
            files
                .expose(&live_key(&"dev".parse().unwrap(), id))
                .await
                .unwrap_err()
                .type_,
            FileStorageErrorType::Backend
        );
        assert_eq!(
            files
                .signed_url(
                    &live_key(&"dev".parse().unwrap(), id),
                    Duration::from_secs(30)
                )
                .await
                .unwrap_err()
                .type_,
            FileStorageErrorType::Backend
        );
        assert!(
            backend
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(verb, _)| *verb == "head")
        );
        assert!(signer.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn signed_urls_confine_the_key_and_cap_lifetime() {
        let (mut files, _) = store(false, None);
        let signer = Arc::new(RecordingSigner::default());
        files.signer = ObjectSigner::new(Arc::<RecordingSigner>::clone(&signer));
        let id = Uuid::new_v4();
        files.stage(id, &b"x"[..]).await.unwrap();
        files
            .signed_url(
                &live_key(&"dev".parse().unwrap(), id),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        files
            .signed_url(
                &live_key(&"dev".parse().unwrap(), id),
                Duration::from_hours(1),
            )
            .await
            .unwrap();
        assert!(
            files
                .signed_url(&live_key(&"dev".parse().unwrap(), id), Duration::ZERO)
                .await
                .is_err()
        );
        let calls = signer.0.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                Path::from(format!(
                    "project/app/{}",
                    live_key(&"dev".parse().unwrap(), id)
                )),
                Duration::from_secs(30)
            )
        );
        assert_eq!(calls[1].1, MAX_URL_LIFETIME);
    }

    #[tokio::test]
    async fn public_exposure_requires_a_cdn_origin_before_copying() {
        use futures_util::TryStreamExt as _;
        let (files, backend) = store(false, None);
        assert_eq!(
            files
                .expose(&live_key(&"dev".parse().unwrap(), Uuid::new_v4()))
                .await
                .unwrap_err()
                .type_,
            FileStorageErrorType::Refused
        );
        assert!(
            backend
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

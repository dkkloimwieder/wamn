//! Streaming, generation-pinned storage for native file routes.
//!
//! The native caller authorizes the application binding before opening this store.
//! Upload to `_wamn_files/staging/<id>`, commit the metadata row, then call `finish`.
//! Only `_wamn_files/staging/` receives an object age lifecycle rule.
//! Public copies use `public/_wamn_files/live/<id>` beneath the same binding prefix.

use std::{
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_util::{StreamExt as _, TryStreamExt as _, stream::BoxStream};
use object_store::{
    Attribute, Attributes, ObjectStoreExt as _, PutMultipartOptions, PutOptions, WriteMultipart,
    signer::{Method, Signer},
};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use uuid::Uuid;

use super::store::{BoundContainer, StoreError};

/// The file namespace, relative to the binding prefix.
pub const FILE_PREFIX: &str = "_wamn_files/";
/// Maximum lifetime of a file signed URL.
pub const MAX_URL_LIFETIME: Duration = Duration::from_mins(15);

/// One canonical live object returned by the confined collector listing.
#[derive(Debug)]
pub struct LiveFileObject {
    path: object_store::path::Path,
    pub blob_key: String,
    pub last_modified: SystemTime,
}

impl LiveFileObject {
    /// The exact bucket key, including the binding prefix and public namespace.
    pub fn object_key(&self) -> &str {
        self.path.as_ref()
    }
}

/// Refuse raw changes to either private or public file objects.
pub(super) fn permit_raw_mutation(key: &str) -> Result<(), StoreError> {
    // Object-store Path removes empty segments. Apply the reservation to that spelling.
    let canonical = object_store::path::Path::from(key);
    let key = canonical.as_ref();
    if key == "_wamn_files"
        || key.starts_with(FILE_PREFIX)
        || key == "public/_wamn_files"
        || key.starts_with("public/_wamn_files/")
    {
        return Err(StoreError::Refused {
            verb: "raw mutation",
            reason: "file-managed keys are reserved for native file operations",
        });
    }
    Ok(())
}

/// The category of a native storage failure, before boundary translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageErrorKind {
    Refused,
    NotFound,
    Body,
    Backend,
}

/// A storage failure with its operation and original cause.
#[derive(Debug)]
pub struct FileStorageError {
    pub kind: FileStorageErrorKind,
    pub operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl FileStorageError {
    pub(super) fn new(
        kind: FileStorageErrorKind,
        operation: &'static str,
        source: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self {
            kind,
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

fn backend(operation: &'static str, source: object_store::Error) -> FileStorageError {
    let kind = if matches!(source, object_store::Error::NotFound { .. }) {
        FileStorageErrorKind::NotFound
    } else {
        FileStorageErrorKind::Backend
    };
    FileStorageError::new(kind, operation, source)
}

/// Metadata computed from the exact bytes sent to staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub id: Uuid,
    pub blob_key: String,
    pub size: u64,
    pub sha256: String,
    pub connection_generation: i64,
}

/// A store built from the file row's pinned connection generation.
#[derive(Clone)]
pub struct FileStore {
    bound: BoundContainer,
    signer: Option<Arc<dyn Signer>>,
    cdn_host: Option<url::Url>,
    generation: i64,
}
impl fmt::Debug for FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("bound", &self.bound)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Abort unfinished multipart uploads after body errors or task cancellation.
struct UploadGuard(Option<WriteMultipart>);
impl Drop for UploadGuard {
    fn drop(&mut self) {
        if let Some(upload) = self.0.take() {
            tokio::spawn(async move {
                if let Err(error) = upload.abort().await {
                    tracing::warn!(%error, "file staging multipart abort failed");
                }
            });
        }
    }
}

/// The live key stored in `platform.file.blob_key`.
pub fn live_key(id: Uuid) -> String {
    format!("{FILE_PREFIX}live/{id}")
}
fn staging_key(id: Uuid) -> String {
    format!("{FILE_PREFIX}staging/{id}")
}
fn public_key(id: Uuid) -> String {
    format!("public/{}", live_key(id))
}

impl FileStore {
    /// Stream canonical private and public live objects beneath this generation's prefix.
    /// Malformed keys are logged and skipped. Listing never buffers the whole bucket.
    pub async fn live_objects(
        &self,
    ) -> Result<BoxStream<'_, Result<LiveFileObject, FileStorageError>>, FileStorageError> {
        self.permit("collect", true).await?;
        let private = self.path("_wamn_files/live")?;
        let public = self.path("public/_wamn_files/live")?;
        Ok(futures_util::stream::iter([private, public])
            .flat_map(move |prefix| {
                let directory = format!("{prefix}/");
                self.bound
                    .store
                    .list(Some(&prefix))
                    .map_err(|error| backend("list live files", error))
                    .try_filter_map(move |meta| {
                        let key = meta.location.as_ref();
                        let parsed = key.strip_prefix(&directory).and_then(|suffix| {
                            suffix
                                .parse::<Uuid>()
                                .ok()
                                .filter(|id| id.to_string() == suffix)
                        });
                        let object = parsed.map(|id| LiveFileObject {
                            path: meta.location.clone(),
                            blob_key: live_key(id),
                            last_modified: meta.last_modified.into(),
                        });
                        if object.is_none() {
                            tracing::warn!(
                                object_key = key,
                                "file collector skipped an unparseable key"
                            );
                        }
                        std::future::ready(Ok(object))
                    })
            })
            .boxed())
    }

    /// Delete only a listed canonical object in this store, after the caller reads its row.
    /// Re-read its age so a replacement since listing remains protected.
    pub async fn delete_collected(
        &self,
        object: &LiveFileObject,
        cutoff: SystemTime,
    ) -> Result<bool, FileStorageError> {
        self.permit("collect", true).await?;
        if object.path != self.path(&object.blob_key)?
            && object.path != self.path(&format!("public/{}", object.blob_key))?
        {
            return Err(FileStorageError::new(
                FileStorageErrorKind::Refused,
                "collect",
                "listed object belongs to another prefix",
            ));
        }
        let meta = match self.bound.store.head(&object.path).await {
            Ok(meta) => meta,
            Err(object_store::Error::NotFound { .. }) => return Ok(false),
            Err(error) => return Err(backend("head orphan", error)),
        };
        if SystemTime::from(meta.last_modified) >= cutoff {
            return Ok(false);
        }
        match self.bound.store.delete(&object.path).await {
            Ok(()) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(backend("delete orphan", error)),
        }
    }

    pub(super) fn new(
        bound: BoundContainer,
        signer: Option<Arc<dyn Signer>>,
        cdn_host: Option<&str>,
        generation: i64,
    ) -> Result<Self, FileStorageError> {
        if generation < 1 {
            return Err(FileStorageError::new(
                FileStorageErrorKind::Refused,
                "open",
                "connection generation must be positive",
            ));
        }
        let cdn_host = cdn_host
            .map(|host| {
                let origin = if host.contains("://") {
                    host.to_owned()
                } else {
                    format!("https://{host}")
                };
                let url = url::Url::parse(&origin).map_err(|error| {
                    FileStorageError::new(FileStorageErrorKind::Refused, "cdn_host", error)
                })?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(FileStorageError::new(
                        FileStorageErrorKind::Refused,
                        "cdn_host",
                        "cdn_host must be an HTTP origin",
                    ));
                }
                Ok(url)
            })
            .transpose()?;
        Ok(Self {
            bound,
            signer,
            cdn_host,
            generation,
        })
    }

    async fn permit(&self, operation: &'static str, write: bool) -> Result<(), FileStorageError> {
        self.bound
            .permit_connection(operation)
            .await
            .map_err(|error| {
                FileStorageError::new(FileStorageErrorKind::Refused, operation, error)
            })?;
        if write {
            self.bound.permit_write(operation).map_err(|error| {
                FileStorageError::new(FileStorageErrorKind::Refused, operation, error)
            })?;
        }
        Ok(())
    }

    fn path(&self, key: &str) -> Result<object_store::path::Path, FileStorageError> {
        self.bound
            .path(key)
            .map_err(|error| FileStorageError::new(FileStorageErrorKind::Refused, "key", error))
    }

    /// Stream into staging using bounded multipart buffers and compute SHA-256.
    /// The caller must insert no row until this returns successfully.
    pub async fn stage(
        &self,
        id: Uuid,
        mut body: impl AsyncRead + Unpin,
    ) -> Result<StagedFile, FileStorageError> {
        self.permit("put", true).await?;
        let path = self.path(&staging_key(id))?;
        // Server-side copies retain this attribute, including the public copy.
        let attributes: Attributes = [(
            Attribute::CacheControl,
            object_store::AttributeValue::from("max-age=3600"),
        )]
        .into_iter()
        .collect();
        let upload = self
            .bound
            .store
            .put_multipart_opts(
                &path,
                PutMultipartOptions {
                    attributes: attributes.clone(),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| backend("stage", error))?;
        let mut upload = UploadGuard(Some(WriteMultipart::new(upload)));
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = body.read(&mut buffer).await.map_err(|error| {
                FileStorageError::new(FileStorageErrorKind::Body, "read upload", error)
            })?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .filter(|size| i64::try_from(*size).is_ok())
                .ok_or_else(|| {
                    FileStorageError::new(
                        FileStorageErrorKind::Refused,
                        "size",
                        "file exceeds the metadata size range",
                    )
                })?;
            hash.update(&buffer[..count]);
            let writer = upload
                .0
                .as_mut()
                .expect("upload remains active until finish");
            writer
                .wait_for_capacity(2)
                .await
                .map_err(|error| backend("stage", error))?;
            writer.write(&buffer[..count]);
        }
        let writer = upload.0.take().expect("upload remains active until finish");
        if size == 0 {
            writer
                .abort()
                .await
                .map_err(|error| backend("abort empty multipart", error))?;
            self.bound
                .store
                .put_opts(
                    &path,
                    Bytes::new().into(),
                    PutOptions {
                        attributes,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| backend("stage empty file", error))?;
        } else {
            writer
                .finish()
                .await
                .map_err(|error| backend("stage", error))?;
        }
        Ok(StagedFile {
            id,
            blob_key: live_key(id),
            size,
            sha256: hex::encode(hash.finalize()),
            connection_generation: self.generation,
        })
    }

    async fn remove_key(&self, key: &str) -> Result<(), FileStorageError> {
        match self.bound.store.delete(&self.path(key)?).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(backend("delete", error)),
        }
    }

    async fn repair(&self, id: Uuid) -> Result<(), FileStorageError> {
        let live = self.path(&live_key(id))?;
        let staged = self.path(&staging_key(id))?;
        match self.bound.store.copy(&staged, &live).await {
            Ok(()) => self.remove_key(&staging_key(id)).await,
            Err(object_store::Error::NotFound { .. }) => self
                .bound
                .store
                .head(&live)
                .await
                .map(|_| ())
                .map_err(|error| backend("repair", error)),
            Err(error) => Err(backend("repair", error)),
        }
    }

    async fn require_live(&self, id: Uuid) -> Result<(), FileStorageError> {
        match self.bound.store.head(&self.path(&live_key(id))?).await {
            Ok(_) => Ok(()),
            Err(object_store::Error::NotFound { .. }) => self.repair(id).await,
            Err(error) => Err(backend("head", error)),
        }
    }

    /// After metadata commit, copy staging to live and remove staging. Retries are safe.
    pub async fn finish(&self, id: Uuid) -> Result<(), FileStorageError> {
        self.permit("put", true).await?;
        self.repair(id).await
    }

    /// Return a streaming read, with repair when the committed row still names staged bytes.
    pub async fn get(&self, id: Uuid) -> Result<object_store::GetResult, FileStorageError> {
        self.permit("get", false).await?;
        match self.bound.store.get(&self.path(&live_key(id))?).await {
            Ok(result) => Ok(result),
            Err(object_store::Error::NotFound { .. }) => {
                let staged = self
                    .bound
                    .store
                    .get(&self.path(&staging_key(id))?)
                    .await
                    .map_err(|error| backend("get staging", error))?;
                // Repair is a storage recovery action, also permitted for a read binding.
                self.repair(id).await?;
                Ok(staged)
            }
            Err(error) => Err(backend("get", error)),
        }
    }

    /// Delete public, live, and staged objects after the metadata deletion succeeds.
    pub async fn delete(&self, id: Uuid) -> Result<(), FileStorageError> {
        self.permit("delete", true).await?;
        self.remove_key(&public_key(id)).await?;
        self.remove_key(&live_key(id)).await?;
        self.remove_key(&staging_key(id)).await
    }

    /// Copy to the public namespace and return its generation's CDN URL.
    pub async fn expose(&self, id: Uuid) -> Result<String, FileStorageError> {
        self.permit("expose", true).await?;
        let mut url = self.cdn_host.clone().ok_or_else(|| {
            FileStorageError::new(
                FileStorageErrorKind::Refused,
                "expose",
                "public exposure requires cdn_host",
            )
        })?;
        // Establish the live object through the same staging recovery path.
        self.require_live(id).await?;
        let key = public_key(id);
        self.bound
            .store
            .copy(&self.path(&live_key(id))?, &self.path(&key)?)
            .await
            .map_err(|error| backend("expose", error))?;
        url.set_path(self.path(&key)?.as_ref());
        Ok(url.into())
    }

    /// Remove the public origin object immediately. Retries are safe.
    pub async fn unexpose(&self, id: Uuid) -> Result<(), FileStorageError> {
        self.permit("unexpose", true).await?;
        self.remove_key(&public_key(id)).await
    }

    /// Sign a confined GET using the pinned generation, for at most fifteen minutes.
    pub async fn signed_url(
        &self,
        id: Uuid,
        lifetime: Duration,
    ) -> Result<String, FileStorageError> {
        self.permit("signed URL", false).await?;
        if lifetime.is_zero() {
            return Err(FileStorageError::new(
                FileStorageErrorKind::Refused,
                "signed URL",
                "URL lifetime must be positive",
            ));
        }
        let signer = self.signer.as_ref().ok_or_else(|| {
            FileStorageError::new(
                FileStorageErrorKind::Refused,
                "signed URL",
                "store has no signer",
            )
        })?;
        self.require_live(id).await?;
        signer
            .signed_url(
                Method::GET,
                &self.path(&live_key(id))?,
                lifetime.min(MAX_URL_LIFETIME),
            )
            .await
            .map(String::from)
            .map_err(|error| backend("signed URL", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::{ObjectStore, memory::InMemory, path::Path};
    use std::{
        io,
        pin::Pin,
        sync::Mutex,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    fn store(read_only: bool, cdn: Option<&str>) -> (FileStore, Arc<InMemory>) {
        let backend = Arc::new(InMemory::new());
        let bound = BoundContainer::new(
            backend.clone() as Arc<dyn ObjectStore>,
            "bucket",
            "tenant/app/",
        )
        .with_read_only(read_only);
        (FileStore::new(bound, None, cdn, 7).unwrap(), backend)
    }

    #[tokio::test]
    async fn staging_hash_size_and_get_repair_use_the_bound_prefix() {
        let (files, backend) = store(false, Some("cdn.example.com"));
        let id = Uuid::new_v4();
        let staged = files.stage(id, &b"abc"[..]).await.unwrap();
        assert_eq!(staged.size, 3);
        assert_eq!(
            staged.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(staged.connection_generation, 7);
        assert_eq!(staged.blob_key, live_key(id));
        let staging = Path::from(format!("tenant/app/{}", staging_key(id)));
        let live = Path::from(format!("tenant/app/{}", live_key(id)));
        assert!(backend.head(&staging).await.is_ok());
        assert!(backend.head(&live).await.is_err());
        let reader = FileStore {
            bound: files.bound.clone().with_read_only(true),
            ..files.clone()
        };
        assert_eq!(
            reader.get(id).await.unwrap().bytes().await.unwrap(),
            b"abc"[..]
        );
        assert!(backend.head(&staging).await.is_err());
        assert!(backend.head(&live).await.is_ok());
        files.finish(id).await.unwrap();
        let url = files.expose(id).await.unwrap();
        assert_eq!(
            url,
            format!("https://cdn.example.com/tenant/app/{}", public_key(id))
        );
        files.expose(id).await.unwrap();
        let public = Path::from(format!("tenant/app/{}", public_key(id)));
        assert_eq!(
            backend
                .get(&public)
                .await
                .unwrap()
                .attributes
                .get(&Attribute::CacheControl)
                .unwrap()
                .as_ref(),
            "max-age=3600"
        );
        files.unexpose(id).await.unwrap();
        files.unexpose(id).await.unwrap();
        assert!(matches!(
            backend.get(&public).await,
            Err(object_store::Error::NotFound { .. })
        ));
        files.delete(id).await.unwrap();
        files.delete(id).await.unwrap();
        assert_eq!(
            files.get(id).await.unwrap_err().kind,
            FileStorageErrorKind::NotFound
        );
    }

    #[tokio::test]
    async fn empty_and_multi_part_uploads_hash_the_transferred_bytes() {
        let (files, _) = store(false, None);
        for data in [Vec::new(), vec![42; 11 * 1024 * 1024]] {
            let id = Uuid::new_v4();
            let metadata = files.stage(id, data.as_slice()).await.unwrap();
            assert_eq!(metadata.size, data.len() as u64);
            assert_eq!(metadata.sha256, hex::encode(Sha256::digest(&data)));
            files.finish(id).await.unwrap();
            assert_eq!(
                files.get(id).await.unwrap().bytes().await.unwrap().as_ref(),
                data
            );
        }
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
    #[tokio::test]
    async fn failed_and_cancelled_bodies_leave_no_committed_object() {
        let (files, backend) = store(false, None);
        let id = Uuid::new_v4();
        assert_eq!(
            files.stage(id, BrokenBody).await.unwrap_err().kind,
            FileStorageErrorKind::Body
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
                .head(&Path::from(format!("tenant/app/{}", staging_key(id))))
                .await
                .is_err()
        );
        assert!(
            backend
                .head(&Path::from(format!("tenant/app/{}", live_key(id))))
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
            staging_key(id),
            live_key(id),
            public_key(id),
            format!("public//{}", live_key(id)),
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
        assert_eq!(files.bound.get("ordinary").await.unwrap(), vec![1]);
        assert!(files.path("../other/secret").is_err());
        files.finish(id).await.unwrap();
        files.expose(id).await.unwrap();
        assert!(files.bound.clear().await.is_err());
        assert_eq!(
            files.get(id).await.unwrap().bytes().await.unwrap(),
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
            files.stage(id, &b"x"[..]).await.unwrap_err().kind,
            FileStorageErrorKind::Refused
        );
        assert!(files.finish(id).await.is_err());
        assert!(files.delete(id).await.is_err());
        assert!(files.expose(id).await.is_err());
        assert!(files.unexpose(id).await.is_err());
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
            let bound = BoundContainer::new(backend.clone(), "bucket", "tenant/app");
            let files = FileStore::new(
                bound,
                Some(Arc::new(RecordingSigner::default())),
                Some("cdn.example.com"),
                7,
            )
            .unwrap();
            let id = Uuid::new_v4();
            let live = files.path(&live_key(id)).unwrap();
            let staged = files.path(&staging_key(id)).unwrap();
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
            files.signed_url(id, Duration::from_secs(30)).await.unwrap();
            files.expose(id).await.unwrap();
            {
                let calls = backend.calls.lock().unwrap();
                assert_eq!(calls.iter().filter(|(verb, _)| *verb == "head").count(), 2);
                assert_eq!(
                    calls
                        .iter()
                        .any(|(verb, to)| *verb == "copy" && *to == live),
                    missing_live
                );
            }
            assert_eq!(backend.inner.head(&staged).await.is_err(), missing_live);
            let bytes = backend
                .inner
                .get(&files.path(&public_key(id)).unwrap())
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
        let bound = BoundContainer::new(backend.clone(), "bucket", "tenant/app");
        let signer = Arc::new(RecordingSigner::default());
        let files =
            FileStore::new(bound, Some(signer.clone()), Some("cdn.example.com"), 7).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(
            files.expose(id).await.unwrap_err().kind,
            FileStorageErrorKind::Backend
        );
        assert_eq!(
            files
                .signed_url(id, Duration::from_secs(30))
                .await
                .unwrap_err()
                .kind,
            FileStorageErrorKind::Backend
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
        files.signer = Some(signer.clone());
        let id = Uuid::new_v4();
        files.stage(id, &b"x"[..]).await.unwrap();
        files.signed_url(id, Duration::from_secs(30)).await.unwrap();
        files.signed_url(id, Duration::from_hours(1)).await.unwrap();
        assert!(files.signed_url(id, Duration::ZERO).await.is_err());
        let calls = signer.0.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                Path::from(format!("tenant/app/{}", live_key(id))),
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
            files.expose(Uuid::new_v4()).await.unwrap_err().kind,
            FileStorageErrorKind::Refused
        );
        assert!(
            backend
                .list(None)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .is_empty()
        );
        for origin in [
            "",
            "https://cdn.example.com/path",
            "https://user:password@cdn.example.com",
            "ftp://cdn.example.com",
            "https://cdn.example.com?query",
        ] {
            assert!(
                FileStore::new(files.bound.clone(), None, Some(origin), 1).is_err(),
                "{origin}"
            );
        }
    }
}

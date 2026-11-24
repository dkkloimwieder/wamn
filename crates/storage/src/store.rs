//! Object transfer and signing operations for a configured bucket.

use std::error::Error;
use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt as _, stream::BoxStream};
use object_store::{
    Attribute, AttributeValue, Attributes, GetResult, ObjectMeta, ObjectStore, ObjectStoreExt as _,
    PutMultipartOptions, PutOptions, WriteMultipart, path::Path, signer,
};
use tokio::time::Instant;

/// A canonical object store key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectPath(Path);

impl ObjectPath {
    /// Canonicalize the supplied key according to the object store's rules.
    pub fn new(key: &str) -> Self {
        Self(Path::from(key))
    }
}

impl From<Path> for ObjectPath {
    fn from(path: Path) -> Self {
        Self(path)
    }
}

impl Deref for ObjectPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The transport failure categories used by a bound container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectErrorKind {
    NotFound,
    AccessDenied,
    AlreadyExists,
    Timeout,
    Other,
}

/// An object store failure that retains the original error.
#[derive(Debug)]
pub struct ObjectError(object_store::Error);

impl From<object_store::Error> for ObjectError {
    fn from(error: object_store::Error) -> Self {
        Self(error)
    }
}

impl fmt::Display for ObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for ObjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl ObjectError {
    /// Whether the requested object is absent.
    pub fn is_not_found(&self) -> bool {
        matches!(self.0, object_store::Error::NotFound { .. })
    }

    /// Classify the transport failure without losing its source.
    pub fn kind(&self) -> ObjectErrorKind {
        match &self.0 {
            object_store::Error::NotFound { .. } => ObjectErrorKind::NotFound,
            object_store::Error::PermissionDenied { .. }
            | object_store::Error::Unauthenticated { .. } => ObjectErrorKind::AccessDenied,
            object_store::Error::AlreadyExists { .. } => ObjectErrorKind::AlreadyExists,
            object_store::Error::Generic { source, .. } if is_timeout(source.as_ref()) => {
                ObjectErrorKind::Timeout
            }
            _ => ObjectErrorKind::Other,
        }
    }

    /// Recover the original transport error for boundary translation.
    pub fn into_source(self) -> Box<dyn Error + Send + Sync> {
        Box::new(self.0)
    }
}

// The client reports transport timeouts through its generic error source.
fn is_timeout(source: &(dyn Error + 'static)) -> bool {
    let rendered = source.to_string().to_ascii_lowercase();
    rendered.contains("timed out") || rendered.contains("timeout")
}

/// An object-store client scoped to one bucket.
pub struct Objects(Arc<dyn ObjectStore>);

impl Clone for Objects {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl fmt::Debug for Objects {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Objects").finish_non_exhaustive()
    }
}

impl Objects {
    /// Take ownership of a configured transport handle.
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self(store)
    }

    /// Overwrite the object at a canonical key with the supplied body.
    pub async fn put(&self, path: &ObjectPath, body: Vec<u8>) -> Result<(), ObjectError> {
        self.0.put(path, body.into()).await?;
        Ok(())
    }

    /// Read an object's metadata.
    pub async fn head(&self, path: &ObjectPath) -> Result<ObjectMeta, ObjectError> {
        Ok(self.0.head(path).await?)
    }

    /// Open an object read.
    pub async fn get(&self, path: &ObjectPath) -> Result<GetResult, ObjectError> {
        Ok(self.0.get(path).await?)
    }

    /// Read the selected bytes using the object's half-open byte range.
    pub async fn get_range(
        &self,
        path: &ObjectPath,
        range: Range<u64>,
    ) -> Result<Vec<u8>, ObjectError> {
        Ok(self.0.get_range(path, range).await?.to_vec())
    }

    /// Delete one object.
    pub async fn delete(&self, path: &ObjectPath) -> Result<(), ObjectError> {
        Ok(self.0.delete(path).await?)
    }

    /// Copy one object in the bucket.
    pub async fn copy(
        &self,
        source: &ObjectPath,
        destination: &ObjectPath,
    ) -> Result<(), ObjectError> {
        Ok(self.0.copy(source, destination).await?)
    }

    /// Stream object metadata under a canonical prefix.
    pub fn list(
        &self,
        prefix: Option<&ObjectPath>,
    ) -> impl Stream<Item = Result<ObjectMeta, ObjectError>> + Send + 'static + use<> {
        self.0
            .list(prefix.map(Deref::deref))
            .map(|entry| entry.map_err(Into::into))
    }

    /// Start a multipart upload with a deadline for abort attempts on drop.
    ///
    /// The caller must enforce the deadline while awaiting upload operations.
    pub async fn start_upload(
        &self,
        path: &ObjectPath,
        cache_control: &'static str,
        deadline: Instant,
    ) -> Result<Upload, ObjectError> {
        let upload = self
            .0
            .put_multipart_opts(
                path,
                PutMultipartOptions {
                    attributes: cache_control_attributes(cache_control),
                    ..Default::default()
                },
            )
            .await?;
        Ok(Upload {
            writer: Some(WriteMultipart::new(upload)),
            deadline,
        })
    }

    /// Write an empty object with the same attributes as a multipart upload.
    pub async fn put_empty(
        &self,
        path: &ObjectPath,
        cache_control: &'static str,
    ) -> Result<(), ObjectError> {
        self.0
            .put_opts(
                path,
                Bytes::new().into(),
                PutOptions {
                    attributes: cache_control_attributes(cache_control),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }
}

fn cache_control_attributes(value: &'static str) -> Attributes {
    [(Attribute::CacheControl, AttributeValue::from(value))]
        .into_iter()
        .collect()
}

/// A multipart upload with bounded part concurrency and cancellation on drop.
pub struct Upload {
    writer: Option<WriteMultipart>,
    deadline: Instant,
}

impl fmt::Debug for Upload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Upload").finish_non_exhaustive()
    }
}

impl Upload {
    /// Add bytes after waiting for capacity for another part.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), ObjectError> {
        let writer = self
            .writer
            .as_mut()
            .expect("upload remains active until finish");
        writer.wait_for_capacity(2).await?;
        writer.write(bytes);
        Ok(())
    }

    /// Commit the multipart upload.
    pub async fn finish(mut self) -> Result<(), ObjectError> {
        self.writer
            .take()
            .expect("upload remains active until finish")
            .finish()
            .await?;
        Ok(())
    }

    /// Abort an upload explicitly.
    pub async fn abort(mut self) -> Result<(), ObjectError> {
        self.writer
            .take()
            .expect("upload remains active until finish")
            .abort()
            .await?;
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if let Some(upload) = self.writer.take() {
            let deadline = self.deadline;
            if Instant::now() >= deadline {
                // Dropping WriteMultipart cancels its part tasks. Bucket lifecycle
                // cleanup owns remote parts when no invocation time remains.
                drop(upload);
                return;
            }
            tokio::spawn(async move {
                if Instant::now() >= deadline {
                    return;
                }
                match tokio::time::timeout_at(deadline, upload.abort()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "file staging multipart abort failed"),
                    Err(_) => tracing::warn!("file staging multipart abort reached its deadline"),
                }
            });
        }
    }
}

/// A signer for objects in a configured bucket.
pub struct ObjectSigner(Arc<dyn signer::Signer>);

impl Clone for ObjectSigner {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl fmt::Debug for ObjectSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectSigner")
            .finish_non_exhaustive()
    }
}

impl ObjectSigner {
    /// Take ownership of a configured signer.
    pub fn new(signer: Arc<dyn signer::Signer>) -> Self {
        Self(signer)
    }

    /// Sign a GET request for an object.
    pub async fn signed_get(
        &self,
        path: &ObjectPath,
        lifetime: Duration,
    ) -> Result<String, ObjectError> {
        Ok(self
            .0
            .signed_url(signer::Method::GET, path, lifetime)
            .await?
            .into())
    }
}

/// The body of a streaming object read.
pub type ObjectStream = BoxStream<'static, Result<Bytes, Box<dyn Error + Send + Sync>>>;

/// Stream the response body while the caller's deadline permits polling.
pub fn deadline_stream(result: GetResult, deadline: Instant) -> ObjectStream {
    let stream = result.into_stream();
    futures_util::stream::unfold(Some(stream), move |stream| async move {
        let mut stream = stream?;
        // Timeout polls work first, so do not poll a ready stream after expiry.
        let next = async {
            if Instant::now() >= deadline {
                std::future::pending().await
            } else {
                stream.next().await
            }
        };
        match tokio::time::timeout_at(deadline, next).await {
            Ok(Some(item)) => Some((item.map_err(Into::into), Some(stream))),
            Ok(None) => None,
            Err(error) => Some((
                Err(object_store::Error::Generic {
                    store: "blobstore",
                    source: Box::new(error),
                }
                .into()),
                None,
            )),
        }
    })
    .boxed()
}

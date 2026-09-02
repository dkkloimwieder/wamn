//! The bound container: the only thing that talks to an object store.
//!
//! Every WIT method body delegates here, and every key it accepts has already
//! passed [`confinement::resolve_key`]. Keeping the store calls in one place
//! means the bucket and prefix walls are applied on one path rather than in
//! sixteen.

use std::sync::Arc;

use object_store::{ObjectStore, ObjectStoreExt as _, path::Path};

use super::confinement::{self, KeyRefusal};
use super::intake::IntakeError;

/// Why a store operation failed.
///
/// Confinement and intake refusals are kept distinct from transport failures:
/// the first two are decided here before anything leaves the host, and only
/// [`StoreError::Backend`] describes the remote.
#[derive(Debug)]
pub enum StoreError {
    /// The guest-supplied key broke a containment rule.
    Confinement(KeyRefusal),
    /// The body was over the ceiling, or could not be proven complete.
    Intake(IntakeError),
    /// The object does not exist.
    NoSuchObject,
    /// The backend refused or failed.
    Backend(object_store::Error),
    /// The verb is refused by WAMN and will not be implemented in this shape.
    Refused {
        /// The verb refused.
        verb: &'static str,
        /// Why, in one sentence.
        reason: &'static str,
    },
}

impl StoreError {
    /// Stable wire code, for the WIT error mapping.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Confinement(refusal) => refusal.code(),
            Self::Intake(error) => error.code(),
            Self::NoSuchObject => "no_such_object",
            Self::Backend(_) => "backend_failure",
            Self::Refused { .. } => "verb_refused",
        }
    }
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Confinement(refusal) => {
                write!(formatter, "object key refused: {}", refusal.code())
            }
            Self::Intake(error) => write!(formatter, "{error}"),
            Self::NoSuchObject => formatter.write_str("no such object"),
            Self::Backend(error) => write!(formatter, "object store failed: {error}"),
            Self::Refused { verb, reason } => {
                write!(formatter, "{verb} is refused by this platform: {reason}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// What the store knows about one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHead {
    /// Object size in bytes.
    pub size: u64,
    /// Last-modified time, nanoseconds since the Unix epoch.
    pub last_modified_unix_nanos: u64,
}

/// One component's confined view of an object store.
///
/// The bucket is fixed by the binding — this type never names another — and
/// every key is resolved under `prefix`.
#[derive(Clone)]
pub struct BoundContainer {
    store: Arc<dyn ObjectStore>,
    container: String,
    prefix: String,
}

impl core::fmt::Debug for BoundContainer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The store handle can carry credentials; it is never rendered.
        formatter
            .debug_struct("BoundContainer")
            .field("container", &self.container)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl BoundContainer {
    /// Bind a component to one container and prefix.
    pub fn new(store: Arc<dyn ObjectStore>, container: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            store,
            container: container.into(),
            prefix: prefix.into(),
        }
    }

    /// The bound container's name. The only container this component can see.
    #[must_use]
    pub fn container(&self) -> &str {
        &self.container
    }

    fn path(&self, author_key: &str) -> Result<Path, StoreError> {
        let resolved = confinement::resolve_key(&self.prefix, author_key).map_err(StoreError::Confinement)?;
        Ok(Path::from(resolved))
    }

    /// Write `body` at the caller's key, overwriting any existing object.
    ///
    /// The caller supplies the key and this never generates one, so a
    /// redelivery of the same logical write lands on the same object — an
    /// idempotent overwrite rather than a duplicate.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure. The body has already been proven
    /// complete by the drain before it reaches here.
    pub async fn put(&self, author_key: &str, body: Vec<u8>) -> Result<(), StoreError> {
        let path = self.path(author_key)?;
        self.store
            .put(&path, body.into())
            .await
            .map(|_| ())
            .map_err(StoreError::Backend)
    }

    /// Read the whole object at the caller's key.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] when absent; confinement or backend
    /// failure otherwise.
    pub async fn get(&self, author_key: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.path(author_key)?;
        match self.store.get(&path).await {
            Ok(result) => result
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(StoreError::Backend),
            Err(object_store::Error::NotFound { .. }) => Err(StoreError::NoSuchObject),
            Err(error) => Err(StoreError::Backend(error)),
        }
    }

    /// Whether an object exists at the caller's key.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure. Absence is `Ok(false)`, not an error.
    pub async fn has(&self, author_key: &str) -> Result<bool, StoreError> {
        let path = self.path(author_key)?;
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(StoreError::Backend(error)),
        }
    }

    /// Delete the object at the caller's key.
    ///
    /// Deleting an absent object succeeds, as the contract requires.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure.
    pub async fn delete(&self, author_key: &str) -> Result<(), StoreError> {
        let path = self.path(author_key)?;
        match self.store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(StoreError::Backend(error)),
        }
    }

    /// Metadata for the object at the caller's key.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] when absent; confinement or backend
    /// failure otherwise.
    pub async fn head(&self, author_key: &str) -> Result<ObjectHead, StoreError> {
        let path = self.path(author_key)?;
        match self.store.head(&path).await {
            Ok(meta) => Ok(ObjectHead {
                size: meta.size,
                last_modified_unix_nanos: meta
                    .last_modified
                    .timestamp_nanos_opt()
                    .unwrap_or_default()
                    .unsigned_abs(),
            }),
            Err(object_store::Error::NotFound { .. }) => Err(StoreError::NoSuchObject),
            Err(error) => Err(StoreError::Backend(error)),
        }
    }

    /// Remove every object under the bound prefix.
    ///
    /// This empties the component's own prefix, never the container: a
    /// component that cannot name the container must not be able to empty it
    /// for everyone else sharing it.
    ///
    /// # Errors
    ///
    /// Backend failure.
    pub async fn clear(&self) -> Result<(), StoreError> {
        for key in self.list().await? {
            self.delete(&key).await?;
        }
        Ok(())
    }

    /// Every object under the bound prefix, as author-visible keys.
    ///
    /// Keys are returned RELATIVE to the prefix: a component that cannot name
    /// the prefix must not be shown it either, or listing would hand back the
    /// very coordinate the wall exists to hide.
    ///
    /// # Errors
    ///
    /// Backend failure.
    pub async fn list(&self) -> Result<Vec<String>, StoreError> {
        use futures_util::TryStreamExt as _;

        let trimmed = self.prefix.trim_end_matches('/');
        let scope = (!trimmed.is_empty()).then(|| Path::from(trimmed));
        let entries: Vec<_> = self
            .store
            .list(scope.as_ref())
            .try_collect()
            .await
            .map_err(StoreError::Backend)?;

        let strip = if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}/")
        };
        let mut keys: Vec<String> = entries
            .into_iter()
            .map(|meta| {
                let full = meta.location.as_ref();
                full.strip_prefix(&strip).unwrap_or(full).to_owned()
            })
            .collect();
        keys.sort_unstable();
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use object_store::memory::InMemory;

    use super::*;

    fn bound(prefix: &str) -> (BoundContainer, Arc<InMemory>) {
        let store = Arc::new(InMemory::new());
        (
            BoundContainer::new(Arc::clone(&store) as Arc<dyn ObjectStore>, "wamn-labels", prefix),
            store,
        )
    }

    #[tokio::test]
    async fn put_get_delete_list_round_trip() {
        let (container, _store) = bound("acme/labels");

        assert!(!container.has("a.zpl").await.expect("has succeeds"));
        container.put("a.zpl", b"^XA^XZ".to_vec()).await.expect("put");
        assert!(container.has("a.zpl").await.expect("has succeeds"));
        assert_eq!(container.get("a.zpl").await.expect("get"), b"^XA^XZ");
        assert_eq!(container.list().await.expect("list"), vec!["a.zpl".to_string()]);

        container.delete("a.zpl").await.expect("delete");
        assert!(!container.has("a.zpl").await.expect("has succeeds"));
        assert!(container.list().await.expect("list").is_empty());
    }

    /// The at-least-once rule: the CALLER supplies a deterministic key and put
    /// overwrites, so a redelivery is one object, not two.
    #[tokio::test]
    async fn a_redelivery_overwrites_rather_than_duplicating() {
        let (container, _store) = bound("p");

        container.put("pallet/PAL-42.zpl", b"first".to_vec()).await.expect("put");
        container.put("pallet/PAL-42.zpl", b"second".to_vec()).await.expect("redelivery");

        assert_eq!(
            container.list().await.expect("list"),
            vec!["pallet/PAL-42.zpl".to_string()],
            "a redelivery under the same key must not create a second object"
        );
        assert_eq!(container.get("pallet/PAL-42.zpl").await.expect("get"), b"second");
    }

    /// Listing must not hand back the prefix. A component that cannot NAME the
    /// prefix must not be shown it either, or listing leaks the coordinate the
    /// wall exists to hide.
    #[tokio::test]
    async fn listing_returns_author_relative_keys_and_never_the_prefix() {
        let (container, _store) = bound("acme/labels");
        container.put("a.zpl", b"x".to_vec()).await.expect("put");
        container.put("deep/b.zpl", b"x".to_vec()).await.expect("put");

        let keys = container.list().await.expect("list");
        assert_eq!(keys, vec!["a.zpl".to_string(), "deep/b.zpl".to_string()]);
        for key in &keys {
            assert!(
                !key.contains("acme/labels"),
                "listing leaked the bound prefix in {key:?}"
            );
        }
    }

    /// The wall holds against the store, not just in the resolver: a traversal
    /// key never reaches an object another binding could see.
    #[tokio::test]
    async fn a_traversal_key_is_refused_before_the_store_is_touched() {
        let (container, store) = bound("tenant-a");
        // An object belonging to another tenant, planted directly.
        store
            .put(&Path::from("tenant-b/secret"), b"theirs".to_vec().into())
            .await
            .expect("plant");

        let error = container.get("../tenant-b/secret").await.expect_err("must refuse");
        assert_eq!(error.code(), "key_parent_traversal");
        assert!(
            container.list().await.expect("list").is_empty(),
            "the neighbour's object must not be visible to this binding"
        );
    }

    /// Deleting an absent object succeeds, as the contract requires.
    #[tokio::test]
    async fn deleting_an_absent_object_succeeds() {
        let (container, _store) = bound("p");
        container.delete("never-existed").await.expect("delete is idempotent");
    }

    #[tokio::test]
    async fn getting_an_absent_object_is_no_such_object() {
        let (container, _store) = bound("p");
        let error = container.get("absent").await.expect_err("must fail");
        assert_eq!(error.code(), "no_such_object");
    }

    /// Two bindings on one store cannot see each other's objects.
    #[tokio::test]
    async fn two_prefixes_on_one_store_are_isolated() {
        let store = Arc::new(InMemory::new()) as Arc<dyn ObjectStore>;
        let a = BoundContainer::new(Arc::clone(&store), "wamn-labels", "tenant-a");
        let b = BoundContainer::new(Arc::clone(&store), "wamn-labels", "tenant-b");

        a.put("shared-name.zpl", b"a's".to_vec()).await.expect("put");
        b.put("shared-name.zpl", b"b's".to_vec()).await.expect("put");

        assert_eq!(a.get("shared-name.zpl").await.expect("get"), b"a's");
        assert_eq!(b.get("shared-name.zpl").await.expect("get"), b"b's");
        assert_eq!(a.list().await.expect("list"), vec!["shared-name.zpl".to_string()]);
    }

    #[tokio::test]
    async fn head_reports_size_and_a_real_timestamp() {
        let (container, _store) = bound("p");
        container.put("a.bin", vec![7; 42]).await.expect("put");

        let head = container.head("a.bin").await.expect("head");
        assert_eq!(head.size, 42);
        assert!(
            head.last_modified_unix_nanos > 0,
            "the timestamp must come from the store, not a fabricated zero"
        );
        assert_eq!(
            container.head("absent").await.expect_err("absent").code(),
            "no_such_object"
        );
    }

    /// Clearing empties the component's own PREFIX, never the container. A
    /// component that cannot name the container must not be able to empty it
    /// for everyone sharing it.
    #[tokio::test]
    async fn clear_empties_only_the_bound_prefix() {
        let store = Arc::new(InMemory::new()) as Arc<dyn ObjectStore>;
        let mine = BoundContainer::new(Arc::clone(&store), "wamn-labels", "tenant-a");
        let theirs = BoundContainer::new(Arc::clone(&store), "wamn-labels", "tenant-b");

        mine.put("x.zpl", b"mine".to_vec()).await.expect("put");
        mine.put("deep/y.zpl", b"mine".to_vec()).await.expect("put");
        theirs.put("x.zpl", b"theirs".to_vec()).await.expect("put");

        mine.clear().await.expect("clear");

        assert!(mine.list().await.expect("list").is_empty());
        assert_eq!(
            theirs.list().await.expect("list"),
            vec!["x.zpl".to_string()],
            "clearing one binding must not touch a neighbour sharing the container"
        );
        assert_eq!(theirs.get("x.zpl").await.expect("get"), b"theirs");
    }

    /// The store handle can carry credentials; Debug must never render it.
    #[tokio::test]
    async fn debug_does_not_render_the_store_handle() {
        let (container, _store) = bound("p");
        let rendered = format!("{container:?}");
        assert!(rendered.contains("wamn-labels") && rendered.contains('p'));
        assert!(
            !rendered.to_lowercase().contains("secret") && !rendered.contains("InMemory"),
            "the store handle must not be rendered: {rendered}"
        );
    }
}

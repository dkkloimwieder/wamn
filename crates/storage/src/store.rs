//! The bound container: the only thing that talks to an object store.
//!
//! Every WIT method body delegates here, and every key it accepts has already
//! passed [`confinement::resolve_key`]. Keeping the store calls in one place
//! means the bucket and prefix walls are applied on one path rather than in
//! sixteen.

use std::sync::Arc;

use object_store::{ObjectStore, ObjectStoreExt as _, path::Path};

use wamn_blobstore::confinement;
use wamn_blobstore::store::{
    BackendError, BackendErrorType, ObjectContainer, ObjectHead, StoreError,
};

fn backend(error: object_store::Error) -> StoreError {
    let type_ = match &error {
        object_store::Error::NotFound { .. } => BackendErrorType::NotFound,
        object_store::Error::PermissionDenied { .. }
        | object_store::Error::Unauthenticated { .. } => BackendErrorType::AccessDenied,
        object_store::Error::AlreadyExists { .. } => BackendErrorType::AlreadyExists,
        object_store::Error::Generic { source, .. } if is_timeout(source.as_ref()) => {
            BackendErrorType::Timeout
        }
        _ => BackendErrorType::Other,
    };
    StoreError::Backend(BackendError::new(type_, error))
}

// The client reports transport timeouts through its generic error source.
fn is_timeout(source: &(dyn std::error::Error + 'static)) -> bool {
    let rendered = source.to_string().to_ascii_lowercase();
    rendered.contains("timed out") || rendered.contains("timeout")
}

/// One component's confined view of an object store.
///
/// The bucket is fixed by the binding — this type never names another — and
/// every key is resolved under `prefix`.
#[derive(Clone)]
pub(super) struct BoundContainer {
    pub(super) store: Arc<dyn ObjectStore>,
    container: String,
    prefix: String,
    read_only: bool,
    authority: ConnectionAuthority,
}

#[derive(Clone)]
pub(super) enum ConnectionAuthority {
    Live {
        facts: Arc<wamn_connection::connection_facts::ConnectionFacts>,
        id: uuid::Uuid,
        generation: i64,
    },
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
    pub(super) fn new(
        store: Arc<dyn ObjectStore>,
        container: impl Into<String>,
        prefix: impl Into<String>,
        authority: ConnectionAuthority,
    ) -> Self {
        Self {
            store,
            container: container.into(),
            prefix: prefix.into(),
            read_only: false,
            authority,
        }
    }

    pub(super) fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub(super) async fn permit_connection(&self, verb: &'static str) -> Result<(), StoreError> {
        let ConnectionAuthority::Live {
            facts,
            id,
            generation,
        } = &self.authority;
        if !facts.answer(id).await.ok().flatten().is_some_and(|answer| {
            answer.fact.status == wamn_connection::connection_memory::ConnectionStatus::Active
                && answer.fact.active_generation == *generation
        }) {
            return Err(StoreError::Refused {
                verb,
                reason: "the connection is disabled, unavailable, or its generation changed",
            });
        }
        Ok(())
    }

    pub(super) fn permit_write(&self, verb: &'static str) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::Refused {
                verb,
                reason: "the connection binding is read-only",
            });
        }
        Ok(())
    }

    pub(super) fn path(&self, author_key: &str) -> Result<Path, StoreError> {
        let resolved =
            confinement::resolve_key(&self.prefix, author_key).map_err(StoreError::Confinement)?;
        Ok(Path::from(resolved))
    }
}

#[async_trait::async_trait]
impl ObjectContainer for BoundContainer {
    /// The bound container's name. The only container this component can see.
    fn container(&self) -> &str {
        &self.container
    }

    /// Write `body` at the caller's key, overwriting any existing object.
    ///
    /// The caller supplies the key and this never generates one, so a
    /// redelivery of the same logical write lands on the same object — an
    /// idempotent overwrite rather than a duplicate.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure. The drain tests that the body is
    /// complete before it reaches here.
    async fn put(&self, author_key: &str, body: Vec<u8>) -> Result<(), StoreError> {
        self.permit_connection("put").await?;
        self.permit_write("put")?;
        super::files::permit_raw_mutation(author_key)?;
        let path = self.path(author_key)?;
        self.store
            .put(&path, body.into())
            .await
            .map(|_| ())
            .map_err(backend)
    }

    /// Read an inclusive byte range without loading the complete object.
    async fn get_range(
        &self,
        author_key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, StoreError> {
        self.permit_connection("get").await?;
        let path = self.path(author_key)?;
        let meta = self.store.head(&path).await.map_err(|error| match error {
            object_store::Error::NotFound { .. } => StoreError::NoSuchObject,
            error => backend(error),
        })?;
        let to = end.saturating_add(1).min(meta.size);
        if start >= to {
            return Ok(Vec::new());
        }
        self.permit_connection("get").await?;
        self.store
            .get_range(&path, start..to)
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| StoreError::ResponseLost(Box::new(error)))
    }

    /// Whether an object exists at the caller's key.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure. Absence is `Ok(false)`, not an error.
    async fn has(&self, author_key: &str) -> Result<bool, StoreError> {
        self.permit_connection("has").await?;
        let path = self.path(author_key)?;
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(backend(error)),
        }
    }

    /// Delete the object at the caller's key.
    ///
    /// Deleting an absent object succeeds, as the contract requires.
    ///
    /// # Errors
    ///
    /// Confinement or backend failure.
    async fn delete(&self, author_key: &str) -> Result<(), StoreError> {
        self.permit_connection("delete").await?;
        self.permit_write("delete")?;
        super::files::permit_raw_mutation(author_key)?;
        let path = self.path(author_key)?;
        match self.store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(backend(error)),
        }
    }

    /// Metadata for the object at the caller's key.
    ///
    /// # Errors
    ///
    /// [`StoreError::NoSuchObject`] when absent; confinement or backend
    /// failure otherwise.
    async fn head(&self, author_key: &str) -> Result<ObjectHead, StoreError> {
        self.permit_connection("head").await?;
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
            Err(error) => Err(backend(error)),
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
    async fn clear(&self) -> Result<(), StoreError> {
        self.permit_connection("clear").await?;
        self.permit_write("clear")?;
        let keys = self.list().await?;
        // Refuse before deleting anything when file-managed objects share this prefix.
        for key in &keys {
            super::files::permit_raw_mutation(key)?;
        }
        for key in keys {
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
    async fn list(&self) -> Result<Vec<String>, StoreError> {
        use futures_util::TryStreamExt as _;

        self.permit_connection("list").await?;

        let trimmed = self.prefix.trim_end_matches('/');
        let scope = (!trimmed.is_empty()).then(|| Path::from(trimmed));
        let entries: Vec<_> = self
            .store
            .list(scope.as_ref())
            .try_collect()
            .await
            .map_err(backend)?;

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

/// Containers bound the way the plugin binds one, for this crate's unit tests.
#[cfg(test)]
pub(crate) mod test_binding {
    use std::sync::Arc;

    use object_store::ObjectStore;
    use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionCache as _};
    use wamn_connection::connection_memory::{ConnectionFact, ConnectionStatus};

    use super::{BoundContainer, ConnectionAuthority};

    const GENERATION: i64 = 1;

    /// Bind `store` under the live authority of a connection the host cache
    /// holds as active in its bound generation.
    pub(crate) fn admitted(
        store: Arc<dyn ObjectStore>,
        container: &str,
        prefix: &str,
    ) -> BoundContainer {
        bind(store, container, prefix, true)
    }

    /// Bind `store` under the live authority of a connection, which the host
    /// cache answers only when `active`. Otherwise the answer comes from a
    /// project database the host holds no login for.
    pub(crate) fn bind(
        store: Arc<dyn ObjectStore>,
        container: &str,
        prefix: &str,
        active: bool,
    ) -> BoundContainer {
        let application = Arc::new(wamn_application::application_config::ApplicationConfig {
            org: "acme".parse().unwrap(),
            project: "project-a".parse().unwrap(),
            environment: "dev".parse().unwrap(),
            application: "app".parse().unwrap(),
            grant_cache_seconds: 3600,
            components: Vec::new(),
        });
        let postgres = Arc::new(
            wamn_postgres::WamnPostgres::from_host_config(std::collections::BTreeMap::new())
                .unwrap(),
        );
        let database = Arc::clone(&postgres);
        let cache = Arc::new(wamn_postgres::authorization::Authorization::new(
            postgres, database,
        ));
        let facts = Arc::clone(&cache).facts(application).unwrap();
        let id = uuid::Uuid::new_v4();
        if active {
            cache.insert(
                facts.project_env_key(),
                id,
                Arc::new(ConnectionAnswer {
                    fact: ConnectionFact {
                        active_generation: GENERATION,
                        status: ConnectionStatus::Active,
                        credential_id: None,
                        service_principal_id: "5c2b7e1a-9d3f-4a6b-8c1e-2f4d6a8b0c3e"
                            .parse()
                            .unwrap(),
                        target_id: uuid::Uuid::new_v4(),
                        resource_key: None,
                        authority: None,
                    },
                    generation: Arc::new(wamn_registry::connections::ConnectionGeneration {
                        name: "files".into(),
                        integration_type: "s3".parse().unwrap(),
                        auth_type: "access_key".into(),
                        external_system: None,
                        parameters: serde_json::json!({}),
                    }),
                }),
            );
        }
        BoundContainer::new(
            store,
            container,
            prefix,
            ConnectionAuthority::Live {
                facts,
                id,
                generation: GENERATION,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use object_store::memory::InMemory;
    use wamn_blobstore::confinement::KeyRefusal;

    use super::test_binding::{admitted, bind};
    use super::*;

    fn bound(prefix: &str) -> (BoundContainer, Arc<InMemory>) {
        let store = Arc::new(InMemory::new());
        (
            admitted(
                Arc::clone(&store) as Arc<dyn ObjectStore>,
                "wamn-labels",
                prefix,
            ),
            store,
        )
    }

    #[test]
    fn recognized_backend_failures_keep_their_categories() {
        let missing = object_store::Error::NotFound {
            path: "item".into(),
            source: "missing".into(),
        };
        let denied = object_store::Error::PermissionDenied {
            path: "item".into(),
            source: "denied".into(),
        };
        for (error, expected) in [
            (missing, BackendErrorType::NotFound),
            (denied, BackendErrorType::AccessDenied),
        ] {
            let StoreError::Backend(error) = backend(error) else {
                panic!("expected a backend error");
            };
            assert_eq!(error.type_, expected);
        }
    }

    #[tokio::test]
    async fn unavailable_connection_refuses_every_store_call() {
        let container = bind(Arc::new(InMemory::new()), "wamn-labels", "p", false);
        assert!(matches!(
            container.put("item", vec![]).await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.get_range("item", 0, u64::MAX).await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.has("item").await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.head("item").await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.delete("item").await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.clear().await,
            Err(StoreError::Refused { .. })
        ));
        assert!(matches!(
            container.list().await,
            Err(StoreError::Refused { .. })
        ));
    }

    #[tokio::test]
    async fn put_get_delete_list_round_trip() {
        let (container, _store) = bound("org-a/labels");

        assert!(!container.has("a.zpl").await.expect("has succeeds"));
        container
            .put("a.zpl", b"^XA^XZ".to_vec())
            .await
            .expect("put");
        assert!(container.has("a.zpl").await.expect("has succeeds"));
        assert_eq!(
            container
                .get_range("a.zpl", 0, u64::MAX)
                .await
                .expect("get"),
            b"^XA^XZ"
        );
        assert_eq!(container.get_range("a.zpl", 1, 2).await.unwrap(), b"XA");
        assert!(
            container
                .get_range("a.zpl", 10, 20)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            container.list().await.expect("list"),
            vec!["a.zpl".to_string()]
        );

        container.delete("a.zpl").await.expect("delete");
        assert!(!container.has("a.zpl").await.expect("has succeeds"));
        assert!(container.list().await.expect("list").is_empty());
    }

    /// The at-least-once rule: the CALLER supplies a deterministic key and put
    /// overwrites, so a redelivery is one object, not two.
    #[tokio::test]
    async fn a_redelivery_overwrites_rather_than_duplicating() {
        let (container, _store) = bound("p");

        container
            .put("widget/W-42.zpl", b"first".to_vec())
            .await
            .expect("put");
        container
            .put("widget/W-42.zpl", b"second".to_vec())
            .await
            .expect("redelivery");

        assert_eq!(
            container.list().await.expect("list"),
            vec!["widget/W-42.zpl".to_string()],
            "a redelivery under the same key must not create a second object"
        );
        assert_eq!(
            container
                .get_range("widget/W-42.zpl", 0, u64::MAX)
                .await
                .expect("get"),
            b"second"
        );
    }

    /// Listing must not hand back the prefix. A component that cannot NAME the
    /// prefix must not be shown it either, or listing leaks the coordinate the
    /// wall exists to hide.
    #[tokio::test]
    async fn listing_returns_author_relative_keys_and_never_the_prefix() {
        let (container, _store) = bound("org-a/labels");
        container.put("a.zpl", b"x".to_vec()).await.expect("put");
        container
            .put("deep/b.zpl", b"x".to_vec())
            .await
            .expect("put");

        let keys = container.list().await.expect("list");
        assert_eq!(keys, vec!["a.zpl".to_string(), "deep/b.zpl".to_string()]);
        for key in &keys {
            assert!(
                !key.contains("org-a/labels"),
                "listing leaked the bound prefix in {key:?}"
            );
        }
    }

    /// The wall holds against the store, not just in the resolver: a traversal
    /// key never reaches an object another binding could see.
    #[tokio::test]
    async fn a_traversal_key_is_refused_before_the_store_is_touched() {
        let (container, store) = bound("org-a");
        // An object belonging to another project, planted directly.
        store
            .put(&Path::from("org-b/secret"), b"theirs".to_vec().into())
            .await
            .expect("plant");

        let error = container
            .get_range("../org-b/secret", 0, u64::MAX)
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            StoreError::Confinement(KeyRefusal::ParentTraversal)
        ));
        assert!(
            container.list().await.expect("list").is_empty(),
            "the neighbour's object must not be visible to this binding"
        );
    }

    /// Deleting an absent object succeeds, as the contract requires.
    #[tokio::test]
    async fn deleting_an_absent_object_succeeds() {
        let (container, _store) = bound("p");
        container
            .delete("never-existed")
            .await
            .expect("delete is idempotent");
    }

    #[tokio::test]
    async fn getting_an_absent_object_is_no_such_object() {
        let (container, _store) = bound("p");
        let error = container
            .get_range("absent", 0, u64::MAX)
            .await
            .expect_err("must fail");
        assert!(matches!(error, StoreError::NoSuchObject));
    }

    /// Two bindings on one store cannot see each other's objects.
    #[tokio::test]
    async fn two_prefixes_on_one_store_are_isolated() {
        let store = Arc::new(InMemory::new()) as Arc<dyn ObjectStore>;
        let a = admitted(Arc::clone(&store), "wamn-labels", "org-a");
        let b = admitted(Arc::clone(&store), "wamn-labels", "org-b");

        a.put("shared-name.zpl", b"a's".to_vec())
            .await
            .expect("put");
        b.put("shared-name.zpl", b"b's".to_vec())
            .await
            .expect("put");

        assert_eq!(
            a.get_range("shared-name.zpl", 0, u64::MAX)
                .await
                .expect("get"),
            b"a's"
        );
        assert_eq!(
            b.get_range("shared-name.zpl", 0, u64::MAX)
                .await
                .expect("get"),
            b"b's"
        );
        assert_eq!(
            a.list().await.expect("list"),
            vec!["shared-name.zpl".to_string()]
        );
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
        assert!(matches!(
            container.head("absent").await,
            Err(StoreError::NoSuchObject)
        ));
    }

    /// Clearing empties the component's own PREFIX, never the container. A
    /// component that cannot name the container must not be able to empty it
    /// for everyone sharing it.
    #[tokio::test]
    async fn clear_empties_only_the_bound_prefix() {
        let store = Arc::new(InMemory::new()) as Arc<dyn ObjectStore>;
        let mine = admitted(Arc::clone(&store), "wamn-labels", "org-a");
        let theirs = admitted(Arc::clone(&store), "wamn-labels", "org-b");

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
        assert_eq!(
            theirs.get_range("x.zpl", 0, u64::MAX).await.expect("get"),
            b"theirs"
        );
    }

    #[tokio::test]
    async fn read_binding_reads_but_refuses_every_mutation() {
        let (container, _) = bound("p");
        container.put("item", b"original".to_vec()).await.unwrap();
        let reader = container.with_read_only(true);
        assert!(reader.put("item", b"changed".to_vec()).await.is_err());
        assert!(reader.delete("item").await.is_err());
        assert!(reader.clear().await.is_err());
        assert_eq!(
            reader.get_range("item", 0, u64::MAX).await.unwrap(),
            b"original"
        );
        assert!(reader.has("item").await.unwrap());
        assert_eq!(reader.head("item").await.unwrap().size, 8);
        assert_eq!(reader.list().await.unwrap(), vec!["item"]);
    }

    /// The store handle can carry credentials; Debug must never render it.
    #[tokio::test]
    async fn debug_does_not_render_the_store_handle() {
        let private = "private-backend-marker";
        let store = object_store::prefix::PrefixStore::new(InMemory::new(), private);
        let container = admitted(Arc::new(store), "labels", "p");
        assert!(!format!("{container:?}").contains(private));
    }
}

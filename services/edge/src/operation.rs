//! The edge's `wamn:operation` host over the run-state file.
//!
//! The route guest calls this interface for each route that names a sealed
//! operation. Each call first runs the checks that the cloud host runs before
//! it touches a connection, in its order. `authorize` opens a read, which
//! claims nothing and binds no statement authority, because the box has no
//! SQL; `finish-read` ends it. `begin-operation` claims the idempotency key as
//! an intent in the SQLite intent store that the delivery and the `wamn-edge
//! intents` commands share, with the request hash as its input hash, and mints
//! a delivery id. A finished key answers the exact text that its commit
//! recorded, an unfinished key answers `uncertain`, a resolved key answers
//! `resolved` with its basis, and another request hash answers `conflict`.
//! `commit` finishes the intent with the exact result text, JSON or not, and
//! `rollback` releases it, so the key is unclaimed again. A commit that cannot
//! record the result leaves the intent open, so it answers `uncertain` until
//! an operator resolves it. `wal-position` returns the
//! number of finished intents in the file, a revision that only grows. One
//! operation is open per Store, and dropping the Store releases its claim.
//! The box has no event plane, so a commit emits no causation message.

use std::collections::HashSet;
use std::sync::Arc;

use wamn_engine::entry_operation::operation::{self, Begun, Refusal};
use wamn_engine::entry_operation::postgres_types::PgError;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::router_delivery::{authorize_registered_operation, bounded_node_deadline_ms};
use wamn_run_state::IntentStore as _;
use wamn_run_state::intent_store::{Intent, IntentId, StoreError, StoreErrorType};
use wamn_run_state_sqlite::{SqliteIntentStore, TextBegun};
use wash_runtime::engine::ctx::{ActiveCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, HasData, Resource, ResourceTable};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::release::EdgeRelease;

/// Host-plugin identity of the edge's operation host.
pub const EDGE_OPERATION_ID: &str = "wamn-edge-operation";

/// The edge's `wamn:operation` host over the loaded release.
#[derive(Debug)]
pub struct EdgeOperation {
    release: Arc<EdgeRelease>,
    /// The intent log of every claimed key.
    intents: SqliteIntentStore,
    /// The tenant of every intent: the organization the box serves.
    tenant: String,
    /// The manifest digest of `release`, the release of every intent.
    release_digest: String,
}

impl EdgeOperation {
    /// Serve the operation import for the operations of `release`, and claim
    /// the keys of `tenant` in `intents`.
    pub fn new(release: Arc<EdgeRelease>, intents: SqliteIntentStore, tenant: String) -> Self {
        let release_digest = release.release().release().manifest_digest.to_string();
        Self {
            release,
            intents,
            tenant,
            release_digest,
        }
    }

    /// The checks of a call that the cloud host makes before it touches a
    /// connection, in its order, and the package that owns the operation. A
    /// `begin` of a read claims nothing.
    fn admit(
        &self,
        caller: Option<&AuthenticatedCaller>,
        operation: &str,
        begin: bool,
    ) -> Result<String, Refusal> {
        let manifest = self.release.release().manifest();
        let (package, fact) = manifest
            .components
            .iter()
            .find_map(|component| {
                component
                    .operations
                    .get(operation)
                    .map(|fact| (&component.package_id, fact))
            })
            .ok_or(Refusal::UnknownOperation)?;
        let read = manifest
            .routes
            .iter()
            .any(|route| route.operation == operation && route.type_.is_read());
        if begin && read {
            return Err(Refusal::ReadsClaimNothing);
        }
        authorize_registered_operation(caller, fact.registered_operation.as_deref())
            .map_err(|_| Refusal::GrantDenied)?;
        // A present handle was issued for an attachment of this loaded release.
        if let Some(caller) = caller
            && manifest.attachment(caller.attachment_id()).is_none()
        {
            return Err(Refusal::InvalidCaller);
        }
        Ok(package.clone())
    }
}

#[async_trait::async_trait]
impl HostPlugin for EdgeOperation {
    fn id(&self) -> &'static str {
        EDGE_OPERATION_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:operation/operation@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "operation", &["operation"]) {
            return Ok(());
        }
        operation::add_to_linker::<_, EdgeOperationHost>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

/// The [`HasData`] marker of the edge's operation host.
struct EdgeOperationHost;

impl HasData for EdgeOperationHost {
    type Data<'a> = ActiveCtx<'a>;
}

/// What one Store has open.
#[derive(Debug, Default)]
enum Open {
    #[default]
    None,
    /// An authorized read.
    Read,
    /// A begin that is claiming its key.
    Claiming,
    /// A begun operation and its claim.
    Claimed(Claim),
}

/// The operation state of one Store, in its resource table.
#[derive(Debug, Default)]
struct StoreOperation(Open);

impl StoreOperation {
    /// Refuse a call that opens an operation while one is open, and leave
    /// that one open.
    fn idle(&self) -> Result<(), Refusal> {
        match self.0 {
            Open::None => Ok(()),
            _ => Err(Refusal::AlreadyBegun),
        }
    }

    /// Take the claim of the begun operation, and leave none open.
    fn take_claim(&mut self) -> Result<Claim, Refusal> {
        match std::mem::take(&mut self.0) {
            Open::Claimed(claim) => Ok(claim),
            other => {
                self.0 = other;
                Err(Refusal::NotBegun)
            }
        }
    }

    /// The intent store of the begun operation.
    fn claimed(&self) -> Result<SqliteIntentStore, Refusal> {
        match &self.0 {
            Open::Claimed(claim) => Ok(claim.intents.clone()),
            _ => Err(Refusal::NotBegun),
        }
    }
}

/// The Store's operation state, created on first use.
fn store_operation(
    table: &mut ResourceTable,
) -> wash_runtime::wasmtime::Result<&mut StoreOperation> {
    if !table.iter_mut().any(|entry| entry.is::<StoreOperation>()) {
        table.push(StoreOperation::default())?;
    }
    Ok(table
        .iter_mut()
        .find_map(|entry| entry.downcast_mut::<StoreOperation>())
        .expect("Store operation state exists"))
}

/// The claimed key of a begun operation. Dropping it unsettled releases the
/// key, as the cloud's Store drop rolls back its claim.
#[derive(Debug)]
struct Claim {
    intents: SqliteIntentStore,
    /// The intent, until the claim settles.
    id: Option<IntentId>,
}

impl Claim {
    /// Finish the intent with the exact text `result`. A result that is not
    /// recorded leaves the intent open, so its key answers `uncertain`.
    async fn commit(mut self, result: &str) -> Result<(), Refusal> {
        let id = self.id.take().expect("an unsettled claim holds its intent");
        self.intents
            .finish_text(&id, result)
            .await
            .map_err(|error| {
                tracing::warn!(intent = id.0, %error, "the commit did not record the result; the intent stays open");
                Refusal::Uncertain
            })
    }

    /// Release the intent, so its key is unclaimed again. A failed release
    /// leaves the intent open, so its key answers `uncertain`.
    async fn rollback(mut self) -> Result<(), Refusal> {
        let id = self.id.take().expect("an unsettled claim holds its intent");
        self.intents.release(&id).await.map_err(|error| {
            tracing::warn!(intent = id.0, %error, "the rollback did not release the key; the intent stays open");
            Refusal::Uncertain
        })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let intents = self.intents.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                intent = id.0,
                "the Store dropped outside the runtime; its intent stays open"
            );
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = intents.release(&id).await {
                tracing::warn!(intent = id.0, %error, "the dropped Store did not release its key; the intent stays open");
            }
        });
    }
}

/// The owned fields of one [`Intent`], built from the loaded release.
#[derive(Debug)]
struct ClaimRequest {
    tenant: String,
    release: String,
    package: String,
    operation: String,
    key: String,
    input_hash: String,
}

/// Claim the key of `request` in `intents`, and mint a delivery id for a new
/// claim. The refusals are the cloud's for the same stored state.
async fn claim(
    intents: &SqliteIntentStore,
    request: &ClaimRequest,
) -> Result<(Option<Claim>, Begun), Refusal> {
    let begun = intents
        .begin_text(&Intent {
            tenant: &request.tenant,
            release: &request.release,
            package: &request.package,
            operation: &request.operation,
            idempotency_key: &request.key,
            input_hash: &request.input_hash,
            deadline_ms: bounded_node_deadline_ms(None),
        })
        .await
        .map_err(|error| storage_refusal(&error))?;
    match begun {
        TextBegun::New(id) => Ok((
            Some(Claim {
                intents: intents.clone(),
                id: Some(id),
            }),
            Begun::New(uuid::Uuid::new_v4().to_string()),
        )),
        TextBegun::Finished(result) => Ok((None, Begun::Finished(result))),
        TextBegun::Uncertain(_) => Err(Refusal::Uncertain),
        TextBegun::Resolved { basis, .. } => Err(Refusal::Resolved(basis.as_str().to_owned())),
        TextBegun::Conflict(_) => Err(Refusal::Conflict),
    }
}

/// The refusal of a run-state file that failed before it claimed anything.
/// The interface names no store but PostgreSQL, so this is its catch-all
/// `query-error` with SQLSTATE 58030 (`io_error`).
fn storage_refusal(error: &StoreError) -> Refusal {
    tracing::warn!(%error, "the run-state file failed");
    Refusal::Postgres(PgError::QueryError(("58030".to_owned(), error.to_string())))
}

/// The revision that `wal-position` returns: the number of finished intents
/// in the run-state file. A finished intent is never released, so it only grows.
async fn revision(intents: &SqliteIntentStore) -> Result<String, Refusal> {
    intents
        .transact("revision", |transaction| {
            transaction
                .query_row(
                    "SELECT count(*) FROM intents WHERE finished_at IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| {
                    StoreError::new(StoreErrorType::Storage, "revision", error.to_string())
                })
        })
        .await
        .map(|count| count.to_string())
        .map_err(|error| storage_refusal(&error))
}

/// The claim that a checked begin makes.
type Entered = Option<(SqliteIntentStore, ClaimRequest)>;

/// Check the call's caller and operation, and open the read of an authorize
/// or the claiming begin. A begin returns the request of its claim.
fn enter<T: 'static + Send>(
    accessor: &Accessor<T, EdgeOperationHost>,
    caller: Option<Resource<AuthenticatedCaller>>,
    operation: &str,
    begin: Option<(String, Vec<u8>)>,
) -> wash_runtime::wasmtime::Result<Result<Entered, Refusal>> {
    accessor.with(|mut access| {
        let ctx = access.get();
        if let Err(refusal) = store_operation(ctx.table)?.idle() {
            return Ok(Err(refusal));
        }
        let Ok(caller) = caller
            .map(|caller| ctx.table.get(&caller).cloned())
            .transpose()
        else {
            return Ok(Err(Refusal::InvalidCaller));
        };
        let plugin = ctx.try_get_plugin::<EdgeOperation>(EDGE_OPERATION_ID)?;
        let package = match plugin.admit(caller.as_ref(), operation, begin.is_some()) {
            Ok(package) => package,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let Some((key, request_hash)) = begin else {
            store_operation(ctx.table)?.0 = Open::Read;
            return Ok(Ok(None));
        };
        // The route guest sends the `sha256:` text of the hash as its bytes.
        let input_hash = String::from_utf8(request_hash)
            .map_err(|_| wash_runtime::wasmtime::Error::msg("the request hash is not text"))?;
        let request = ClaimRequest {
            tenant: plugin.tenant.clone(),
            release: plugin.release_digest.clone(),
            package,
            operation: operation.to_owned(),
            key,
            input_hash,
        };
        let intents = plugin.intents.clone();
        store_operation(ctx.table)?.0 = Open::Claiming;
        Ok(Ok(Some((intents, request))))
    })
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the binding declares each operation call `async`; the edge answers reads from the \
              loaded release and the Store"
)]
impl<T: 'static + Send> operation::HostWithStore<T> for EdgeOperationHost {
    async fn authorize(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        Ok(enter(accessor, caller, &operation, None)?.map(drop))
    }

    async fn finish_read(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        accessor.with(|mut access| {
            let state = store_operation(access.get().table)?;
            if !matches!(state.0, Open::Read) {
                return Ok(Err(Refusal::NotBegun));
            }
            state.0 = Open::None;
            Ok(Ok(()))
        })
    }

    async fn begin_operation(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
        idempotency_key: String,
        request_hash: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
        let key = Some((idempotency_key, request_hash));
        let (intents, request) = match enter(accessor, caller, &operation, key)? {
            Ok(entered) => entered.expect("a begin returns its claim request"),
            Err(refusal) => return Ok(Err(refusal)),
        };
        let claimed = claim(&intents, &request).await;
        accessor.with(|mut access| {
            let state = store_operation(access.get().table)?;
            let (open, begun) = match claimed {
                Ok((Some(claim), begun)) => (Open::Claimed(claim), Ok(begun)),
                // Finished leaves no operation open.
                Ok((None, begun)) => (Open::None, Ok(begun)),
                Err(refusal) => (Open::None, Err(refusal)),
            };
            state.0 = open;
            Ok(begun)
        })
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        result: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let claim = accessor.with(|mut access| {
            store_operation(access.get().table).map(StoreOperation::take_claim)
        })?;
        Ok(match claim {
            Ok(claim) => claim.commit(&result).await,
            Err(refusal) => Err(refusal),
        })
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let claim = accessor.with(|mut access| {
            store_operation(access.get().table).map(StoreOperation::take_claim)
        })?;
        Ok(match claim {
            Ok(claim) => claim.rollback().await,
            Err(refusal) => Err(refusal),
        })
    }

    async fn wal_position(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<String, Refusal>> {
        let intents = accessor
            .with(|mut access| store_operation(access.get().table).map(|state| state.claimed()))?;
        Ok(match intents {
            Ok(intents) => revision(&intents).await,
            Err(refusal) => Err(refusal),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use wamn_engine::entry_operation::operation::{Begun, Refusal};
    use wamn_run_state::IntentStore as _;
    use wamn_run_state_sqlite::SqliteIntentStore;

    use super::{Claim, ClaimRequest, Open, StoreOperation, claim, revision};

    /// A fresh run-state file named for `test`.
    fn store(test: &str) -> (SqliteIntentStore, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("edge-operation-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create the store directory");
        let store = SqliteIntentStore::open(directory.join("edge.db")).expect("open the store");
        (store, directory)
    }

    fn request(key: &str, input_hash: &str) -> ClaimRequest {
        ClaimRequest {
            tenant: "org-a".to_owned(),
            release: "release".to_owned(),
            package: "edge_device".to_owned(),
            operation: "edge-device:sample/read@2.1.0".to_owned(),
            key: key.to_owned(),
            input_hash: input_hash.to_owned(),
        }
    }

    /// Claim a new key, and return the claim.
    async fn begin(store: &SqliteIntentStore, key: &str, input_hash: &str) -> Claim {
        match claim(store, &request(key, input_hash)).await {
            Ok((Some(claim), Begun::New(delivery))) => {
                assert!(!delivery.is_empty(), "a new claim mints a delivery id");
                claim
            }
            other => panic!("expected a new claim, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_committed_key_answers_its_result_and_another_hash_conflicts() {
        let (store, directory) = store("commit");
        let result = json!({"value": {"frame": "12.5 kg"}}).to_string();
        begin(&store, "k-1", "sha256:a")
            .await
            .commit(&result)
            .await
            .expect("commit");
        let again = claim(&store, &request("k-1", "sha256:a")).await;
        assert!(
            matches!(&again, Ok((None, Begun::Finished(stored))) if *stored == result),
            "{again:?}"
        );
        assert!(matches!(
            claim(&store, &request("k-1", "sha256:b")).await,
            Err(Refusal::Conflict)
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    /// Commit `result` on a new key, and assert that a retry answers the same
    /// bytes.
    async fn assert_round_trip(store: &SqliteIntentStore, key: &str, result: &str) {
        begin(store, key, "sha256:a")
            .await
            .commit(result)
            .await
            .expect("commit");
        match claim(store, &request(key, "sha256:a")).await {
            Ok((None, Begun::Finished(stored))) => assert_eq!(stored.as_bytes(), result.as_bytes()),
            other => panic!("expected the finished result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_result_that_is_not_json_finishes_the_key() {
        let (store, directory) = store("text");
        assert_round_trip(&store, "k-1", "12.5 kg, not JSON {").await;
        assert!(store.uncertain(10).await.expect("uncertain").is_empty());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn a_json_result_round_trips_byte_for_byte() {
        let (store, directory) = store("bytes");
        let result = "{ \"value\" :{\"frame\":  \"12.5 kg\"} ,\"b\":1.0 }\n";
        assert_round_trip(&store, "k-1", result).await;
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn a_rollback_frees_the_key_and_an_open_claim_is_uncertain() {
        let (store, directory) = store("rollback");
        let open = begin(&store, "k-1", "sha256:a").await;
        assert!(matches!(
            claim(&store, &request("k-1", "sha256:a")).await,
            Err(Refusal::Uncertain)
        ));
        open.rollback().await.expect("rollback");
        assert!(store.uncertain(10).await.expect("uncertain").is_empty());
        begin(&store, "k-1", "sha256:b")
            .await
            .rollback()
            .await
            .expect("the key is unclaimed again");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn a_dropped_claim_releases_its_key() {
        let (store, directory) = store("drop");
        drop(begin(&store, "k-1", "sha256:a").await);
        // The drop releases on the runtime; the next begin waits behind it on
        // the one writer, or finds the key free.
        tokio::task::yield_now().await;
        for _ in 0..100 {
            if store.uncertain(10).await.expect("uncertain").is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        begin(&store, "k-1", "sha256:b")
            .await
            .rollback()
            .await
            .expect("rollback");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn one_operation_is_open_and_settling_needs_a_begin() {
        let (store, directory) = store("state");
        let mut state = StoreOperation::default();
        assert!(
            matches!(state.take_claim(), Err(Refusal::NotBegun)),
            "commit and rollback"
        );
        assert!(
            matches!(state.claimed(), Err(Refusal::NotBegun)),
            "wal-position"
        );
        state.0 = Open::Read;
        assert!(matches!(state.idle(), Err(Refusal::AlreadyBegun)));
        assert!(
            matches!(state.take_claim(), Err(Refusal::NotBegun)),
            "a read is not begun"
        );
        assert!(
            matches!(state.0, Open::Read),
            "a refused call leaves the read open"
        );
        state.0 = Open::Claimed(begin(&store, "k-1", "sha256:a").await);
        assert!(matches!(state.idle(), Err(Refusal::AlreadyBegun)));
        assert!(
            state.claimed().is_ok(),
            "a refused begin leaves the operation open"
        );
        state
            .take_claim()
            .expect("the open claim")
            .rollback()
            .await
            .expect("rollback");
        assert!(state.idle().is_ok(), "rollback clears the open operation");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn the_wal_position_counts_finished_intents() {
        let (store, directory) = store("revision");
        let open = begin(&store, "k-1", "sha256:a").await;
        assert_eq!(revision(&store).await.expect("revision"), "0");
        open.commit("{}").await.expect("commit");
        let open = begin(&store, "k-2", "sha256:a").await;
        assert_eq!(revision(&store).await.expect("revision"), "1");
        open.rollback().await.expect("rollback");
        assert_eq!(
            revision(&store).await.expect("revision"),
            "1",
            "a release finishes nothing"
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}

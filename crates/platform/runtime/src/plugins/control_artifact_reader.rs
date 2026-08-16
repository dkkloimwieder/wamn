//! Host-only verified reads of immutable execution bundles from the control store.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime, Timeouts};
use tokio::time::timeout;
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;
use wamn_catalog::CatalogIdentityError;
use wamn_control_provision::{
    ArtifactReaderCredentialScope, ArtifactReaderEndpoint, artifact_reader_connection_config,
};

const CONTROL_ARTIFACT_POOL_MAX_SIZE: usize = 16;
const CONTROL_ARTIFACT_POOL_WAIT: Duration = Duration::from_secs(2);
const CONTROL_ARTIFACT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_ARTIFACT_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_ARTIFACT_STATEMENT_TIMEOUT_MS: u32 = 5_000;
const CONTROL_ARTIFACT_MAX_ATTEMPTS: usize = 2;
const EXECUTION_BUNDLE_FORMAT_VERSION: &str = "0.1";

const READ_EXECUTION_BUNDLES_SQL: &str = "\
SELECT tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length \
  FROM catalog.execution_bundles \
 WHERE tenant_id = $1 \
   AND execution_bundle_hash = ANY($2::text[]) \
 ORDER BY execution_bundle_hash COLLATE \"C\"";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlanCacheKey {
    tenant_id: Arc<str>,
    execution_bundle_hash: Arc<str>,
}

#[derive(Debug)]
struct PlanCacheState {
    entries: HashMap<PlanCacheKey, Arc<[u8]>>,
    least_to_most_recent: VecDeque<PlanCacheKey>,
}

/// Deterministic entry-bounded cache keyed by tenant and exact plan identity.
#[derive(Debug)]
pub(crate) struct ResolutionPlanCache {
    max_entries: usize,
    state: Mutex<PlanCacheState>,
}

impl ResolutionPlanCache {
    pub(crate) fn new(max_entries: usize) -> Result<Self, InvalidPlanCacheLimit> {
        let max_entries = NonZeroUsize::new(max_entries).ok_or(InvalidPlanCacheLimit)?;
        Ok(Self::from_non_zero(max_entries))
    }

    fn from_non_zero(max_entries: NonZeroUsize) -> Self {
        Self {
            max_entries: max_entries.get(),
            state: Mutex::new(PlanCacheState {
                entries: HashMap::with_capacity(max_entries.get()),
                least_to_most_recent: VecDeque::with_capacity(max_entries.get()),
            }),
        }
    }

    pub(crate) fn get(&self, tenant_id: &str, execution_bundle_hash: &str) -> Option<Arc<[u8]>> {
        let mut state = self
            .state
            .lock()
            .expect("resolution plan cache lock poisoned");
        let key = PlanCacheKey {
            tenant_id: Arc::from(tenant_id),
            execution_bundle_hash: Arc::from(execution_bundle_hash),
        };
        let bytes = state.entries.get(&key)?.clone();
        touch(&mut state.least_to_most_recent, &key);
        Some(bytes)
    }

    /// Insert bytes after the calling owner has completed its verification.
    /// The control-reader path hash-checks and parses before reaching this seam;
    /// the legacy project-byte path retains its preexisting SHA-only contract.
    pub(crate) fn insert_verified(
        &self,
        tenant_id: &str,
        execution_bundle_hash: &str,
        exact_bytes: Vec<u8>,
    ) -> Arc<[u8]> {
        let key = PlanCacheKey {
            tenant_id: Arc::from(tenant_id),
            execution_bundle_hash: Arc::from(execution_bundle_hash),
        };
        let mut state = self
            .state
            .lock()
            .expect("resolution plan cache lock poisoned");
        if let Some(bytes) = state.entries.get(&key).cloned() {
            touch(&mut state.least_to_most_recent, &key);
            return bytes;
        }
        while state.entries.len() >= self.max_entries {
            let evicted = state
                .least_to_most_recent
                .pop_front()
                .expect("non-empty cache has an eviction key");
            state.entries.remove(&evicted);
        }
        let bytes: Arc<[u8]> = exact_bytes.into();
        state.entries.insert(key.clone(), bytes.clone());
        state.least_to_most_recent.push_back(key);
        bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state
            .lock()
            .expect("resolution plan cache lock poisoned")
            .entries
            .len()
    }
}

fn touch(order: &mut VecDeque<PlanCacheKey>, key: &PlanCacheKey) {
    if let Some(position) = order.iter().position(|candidate| candidate == key) {
        order.remove(position);
    }
    order.push_back(key.clone());
}

/// The cache must retain at least one immutable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPlanCacheLimit;

impl fmt::Display for InvalidPlanCacheLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("resolution plan cache entry limit must be non-zero")
    }
}

impl Error for InvalidPlanCacheLimit {}

/// One immutable control-store row after coordinate, format, length, hash, and
/// execution-plan validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExecutionBundle {
    execution_bundle_hash: String,
    exact_bytes: Arc<[u8]>,
}

impl VerifiedExecutionBundle {
    pub fn execution_bundle_hash(&self) -> &str {
        &self.execution_bundle_hash
    }

    pub fn exact_bytes(&self) -> &[u8] {
        &self.exact_bytes
    }
}

/// Stable internal categories for artifact-reader failures. These are not a
/// guest, WIT, persisted, or wire taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlArtifactReaderErrorKind {
    Credential,
    TenantScope,
    NotFound,
    Timeout,
    Unavailable,
    Malformed,
    HashMismatch,
}

/// Contextual host-only artifact-reader error; credential material is never
/// included in its context or source messages.
#[derive(Debug)]
pub struct ControlArtifactReaderError {
    kind: ControlArtifactReaderErrorKind,
    context: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ControlArtifactReaderError {
    fn new(kind: ControlArtifactReaderErrorKind, context: impl Into<String>) -> Self {
        Self {
            kind,
            context: context.into(),
            source: None,
        }
    }

    fn with_source(
        kind: ControlArtifactReaderErrorKind,
        context: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }

    pub const fn kind(&self) -> ControlArtifactReaderErrorKind {
        self.kind
    }
}

impl fmt::Display for ControlArtifactReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl Error for ControlArtifactReaderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Debug, Clone)]
struct ArtifactRow {
    tenant_id: String,
    execution_bundle_hash: String,
    format_version: String,
    exact_bytes: Vec<u8>,
    byte_length: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactSourceErrorKind {
    Timeout,
    Unavailable,
}

#[derive(Debug)]
struct ArtifactSourceError {
    kind: ArtifactSourceErrorKind,
    context: String,
}

impl fmt::Display for ArtifactSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl Error for ArtifactSourceError {}

#[async_trait]
trait ArtifactSource: Send + Sync {
    async fn fetch(
        &self,
        tenant_id: &str,
        execution_bundle_hashes: &[String],
    ) -> Result<Vec<ArtifactRow>, ArtifactSourceError>;
}

struct PostgresArtifactSource {
    pool: Pool,
}

impl fmt::Debug for PostgresArtifactSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresArtifactSource")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ArtifactSource for PostgresArtifactSource {
    async fn fetch(
        &self,
        tenant_id: &str,
        execution_bundle_hashes: &[String],
    ) -> Result<Vec<ArtifactRow>, ArtifactSourceError> {
        let connection = self.pool.get().await.map_err(|error| {
            let kind = if matches!(error, deadpool_postgres::PoolError::Timeout(_)) {
                ArtifactSourceErrorKind::Timeout
            } else {
                ArtifactSourceErrorKind::Unavailable
            };
            ArtifactSourceError {
                kind,
                context: format!("control artifact pool checkout failed: {error}"),
            }
        })?;
        let cancel = connection.cancel_token();
        let parameters: [&(dyn ToSql + Sync); 2] = [&tenant_id, &execution_bundle_hashes];
        let query = connection.query(READ_EXECUTION_BUNDLES_SQL, &parameters);
        let rows = match timeout(CONTROL_ARTIFACT_QUERY_TIMEOUT, query).await {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                let kind = if error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
                {
                    ArtifactSourceErrorKind::Timeout
                } else {
                    ArtifactSourceErrorKind::Unavailable
                };
                return Err(ArtifactSourceError {
                    kind,
                    context: format!("control artifact query failed: {error}"),
                });
            }
            Err(_) => {
                let _ = timeout(CONTROL_ARTIFACT_CANCEL_TIMEOUT, cancel.cancel_query(NoTls)).await;
                let client = Object::take(connection);
                drop(client);
                return Err(ArtifactSourceError {
                    kind: ArtifactSourceErrorKind::Timeout,
                    context: "control artifact query exceeded its timeout".to_string(),
                });
            }
        };
        rows.into_iter()
            .map(|row| {
                Ok(ArtifactRow {
                    tenant_id: row.try_get(0).map_err(source_row_error)?,
                    execution_bundle_hash: row.try_get(1).map_err(source_row_error)?,
                    format_version: row.try_get(2).map_err(source_row_error)?,
                    exact_bytes: row.try_get(3).map_err(source_row_error)?,
                    byte_length: row.try_get(4).map_err(source_row_error)?,
                })
            })
            .collect()
    }
}

fn source_row_error(error: tokio_postgres::Error) -> ArtifactSourceError {
    ArtifactSourceError {
        kind: ArtifactSourceErrorKind::Unavailable,
        context: format!("control artifact row decode failed: {error}"),
    }
}

/// Dedicated control-plane reader with its own credential, pool, and verified
/// memory cache. Construction does not mount it into a host or guest.
pub struct ControlArtifactReader {
    tenant_id: Arc<str>,
    source: Arc<dyn ArtifactSource>,
    cache: ResolutionPlanCache,
}

impl fmt::Debug for ControlArtifactReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlArtifactReader")
            .field("tenant_id", &self.tenant_id)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl ControlArtifactReader {
    /// Validate the fixed `.9.10` credential against host-held scope and
    /// endpoint facts, then build a pool independent of run-state PostgreSQL.
    pub fn from_credential(
        document: &[u8],
        expected: &ArtifactReaderCredentialScope,
        expected_endpoint: &ArtifactReaderEndpoint,
        now: SystemTime,
        max_cache_entries: NonZeroUsize,
    ) -> Result<Self, ControlArtifactReaderError> {
        let mut config =
            artifact_reader_connection_config(document, expected, expected_endpoint, now).map_err(
                |error| {
                    ControlArtifactReaderError::with_source(
                        ControlArtifactReaderErrorKind::Credential,
                        "artifact-reader credential was refused",
                        error,
                    )
                },
            )?;
        config.connect_timeout(CONTROL_ARTIFACT_POOL_WAIT);
        config.options(format!(
            "-c statement_timeout={CONTROL_ARTIFACT_STATEMENT_TIMEOUT_MS}"
        ));
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(CONTROL_ARTIFACT_POOL_MAX_SIZE)
            .timeouts(Timeouts {
                wait: Some(CONTROL_ARTIFACT_POOL_WAIT),
                create: Some(CONTROL_ARTIFACT_POOL_WAIT),
                recycle: Some(CONTROL_ARTIFACT_POOL_WAIT),
            })
            .runtime(Runtime::Tokio1)
            .build()
            .map_err(|error| {
                ControlArtifactReaderError::with_source(
                    ControlArtifactReaderErrorKind::Unavailable,
                    "control artifact pool could not be built",
                    error,
                )
            })?;
        Ok(Self {
            tenant_id: Arc::from(expected.tenant_id.as_str()),
            source: Arc::new(PostgresArtifactSource { pool }),
            cache: ResolutionPlanCache::from_non_zero(max_cache_entries),
        })
    }

    /// Load and verify the requested immutable bundles for the one credential
    /// tenant. Cache hits perform no control-store call.
    pub async fn read(
        &self,
        tenant_id: &str,
        execution_bundle_hashes: &[String],
    ) -> Result<Vec<VerifiedExecutionBundle>, ControlArtifactReaderError> {
        if tenant_id != self.tenant_id.as_ref() {
            return Err(ControlArtifactReaderError::new(
                ControlArtifactReaderErrorKind::TenantScope,
                "artifact read tenant does not match the credential tenant",
            ));
        }

        let requested = execution_bundle_hashes
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if requested
            .iter()
            .any(|hash| !valid_execution_bundle_hash(hash))
        {
            return Err(ControlArtifactReaderError::new(
                ControlArtifactReaderErrorKind::Malformed,
                "artifact read contains a malformed execution-bundle hash",
            ));
        }

        let mut exact_by_hash = BTreeMap::new();
        let mut missing = Vec::new();
        for hash in requested {
            if let Some(bytes) = self.cache.get(tenant_id, hash) {
                exact_by_hash.insert(hash.to_string(), bytes);
            } else {
                missing.push(hash.to_string());
            }
        }
        if !missing.is_empty() {
            let mut attempt = 0;
            let rows = loop {
                attempt += 1;
                match self.source.fetch(tenant_id, &missing).await {
                    Ok(rows) => break rows,
                    Err(error)
                        if error.kind == ArtifactSourceErrorKind::Unavailable
                            && attempt < CONTROL_ARTIFACT_MAX_ATTEMPTS => {}
                    Err(error) => return Err(map_source_error(error)),
                }
            };
            let verified = validate_rows(tenant_id, &missing, rows)?;
            for (hash, bytes) in verified {
                let bytes = self.cache.insert_verified(tenant_id, &hash, bytes);
                exact_by_hash.insert(hash, bytes);
            }
        }

        Ok(exact_by_hash
            .into_iter()
            .map(
                |(execution_bundle_hash, exact_bytes)| VerifiedExecutionBundle {
                    execution_bundle_hash,
                    exact_bytes,
                },
            )
            .collect())
    }

    #[cfg(test)]
    fn from_source(
        tenant_id: &str,
        max_cache_entries: usize,
        source: Arc<dyn ArtifactSource>,
    ) -> Result<Self, InvalidPlanCacheLimit> {
        Ok(Self {
            tenant_id: Arc::from(tenant_id),
            source,
            cache: ResolutionPlanCache::new(max_cache_entries)?,
        })
    }
}

fn map_source_error(error: ArtifactSourceError) -> ControlArtifactReaderError {
    let kind = match error.kind {
        ArtifactSourceErrorKind::Timeout => ControlArtifactReaderErrorKind::Timeout,
        ArtifactSourceErrorKind::Unavailable => ControlArtifactReaderErrorKind::Unavailable,
    };
    ControlArtifactReaderError::with_source(kind, "control artifact read failed", error)
}

fn validate_rows(
    tenant_id: &str,
    requested: &[String],
    rows: Vec<ArtifactRow>,
) -> Result<BTreeMap<String, Vec<u8>>, ControlArtifactReaderError> {
    let requested = requested
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut verified = BTreeMap::new();
    for row in rows {
        if row.tenant_id != tenant_id
            || !requested.contains(row.execution_bundle_hash.as_str())
            || verified.contains_key(&row.execution_bundle_hash)
        {
            return Err(ControlArtifactReaderError::new(
                ControlArtifactReaderErrorKind::Malformed,
                "control artifact row coordinates do not match the request",
            ));
        }
        if row.format_version != EXECUTION_BUNDLE_FORMAT_VERSION {
            return Err(ControlArtifactReaderError::new(
                ControlArtifactReaderErrorKind::Malformed,
                "control artifact format-version is not 0.1",
            ));
        }
        if usize::try_from(row.byte_length).ok() != Some(row.exact_bytes.len()) {
            return Err(ControlArtifactReaderError::new(
                ControlArtifactReaderErrorKind::Malformed,
                "control artifact byte-length does not match exact bytes",
            ));
        }
        match wamn_catalog::read_execution_plan(&row.execution_bundle_hash, &row.exact_bytes) {
            Ok(_) => {}
            Err(CatalogIdentityError::ExecutionBundleHashMismatch) => {
                return Err(ControlArtifactReaderError::new(
                    ControlArtifactReaderErrorKind::HashMismatch,
                    "control artifact bytes do not match their digest",
                ));
            }
            Err(error) => {
                return Err(ControlArtifactReaderError::with_source(
                    ControlArtifactReaderErrorKind::Malformed,
                    "control artifact bytes are not a valid execution plan",
                    error,
                ));
            }
        }
        verified.insert(row.execution_bundle_hash, row.exact_bytes);
    }
    if verified.len() != requested.len() {
        return Err(ControlArtifactReaderError::new(
            ControlArtifactReaderErrorKind::NotFound,
            "one or more authorized execution bundles are missing",
        ));
    }
    Ok(verified)
}

fn valid_execution_bundle_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;
    use wamn_catalog::{
        ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanBody, ExecutionPlanNode,
        ExecutionPlanV2, ExecutionRuntimeRevision, ExecutionSourceMapEntry,
        HOST_EFFECT_CONTRACT_VERSION, RootTerminalBehavior, execution_bundle_hash,
    };

    use super::*;

    #[derive(Debug)]
    struct FakeSource {
        calls: AtomicUsize,
        results: Mutex<VecDeque<Result<Vec<ArtifactRow>, ArtifactSourceError>>>,
    }

    impl FakeSource {
        fn new(results: Vec<Result<Vec<ArtifactRow>, ArtifactSourceError>>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                results: Mutex::new(results.into()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ArtifactSource for FakeSource {
        async fn fetch(
            &self,
            _tenant_id: &str,
            _execution_bundle_hashes: &[String],
        ) -> Result<Vec<ArtifactRow>, ArtifactSourceError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.results
                .lock()
                .expect("fake source lock poisoned")
                .pop_front()
                .expect("fake source result configured")
        }
    }

    fn valid_plan_bytes() -> Vec<u8> {
        let node_id = ExecutionNodeId::new("entry").unwrap();
        let nodes = vec![ExecutionPlanNode {
            local_node_id: node_id.clone(),
            source_node_id: "entry".into(),
            node_type: "event".into(),
            config: serde_json::json!({}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }];
        let plan = ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: format!("sha256:{}", "2".repeat(64)),
                effect_provider_revision: format!("sha256:{}", "1".repeat(64)),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            format!("sha256:{}", "3".repeat(64)),
            ExecutionPlanBody {
                entry_instruction: node_id.clone(),
                nodes,
                edges: Vec::new(),
                root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
                entry_input_schema_guard: Value::Bool(true),
                callable_contract: None,
                source_map: vec![ExecutionSourceMapEntry {
                    local_node_id: node_id,
                    source_node_id: "entry".into(),
                }],
            },
        )
        .unwrap();
        serde_json::to_vec(&plan).unwrap()
    }

    fn row(tenant_id: &str, bytes: Vec<u8>) -> ArtifactRow {
        ArtifactRow {
            tenant_id: tenant_id.into(),
            execution_bundle_hash: execution_bundle_hash(&bytes),
            format_version: EXECUTION_BUNDLE_FORMAT_VERSION.into(),
            byte_length: i32::try_from(bytes.len()).unwrap(),
            exact_bytes: bytes,
        }
    }

    fn source_error(kind: ArtifactSourceErrorKind) -> ArtifactSourceError {
        ArtifactSourceError {
            kind,
            context: "injected source failure".into(),
        }
    }

    #[tokio::test]
    async fn first_read_fills_and_second_read_performs_no_store_call() {
        let row = row("tenant-a", valid_plan_bytes());
        let hash = row.execution_bundle_hash.clone();
        let source = Arc::new(FakeSource::new(vec![Ok(vec![row])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source.clone()).unwrap();

        let first = reader
            .read("tenant-a", std::slice::from_ref(&hash))
            .await
            .unwrap();
        let second = reader
            .read("tenant-a", std::slice::from_ref(&hash))
            .await
            .unwrap();

        assert_eq!(source.calls(), 1);
        assert_eq!(first, second);
        assert_eq!(reader.cache.len(), 1);
    }

    #[tokio::test]
    async fn tenant_mismatch_refuses_before_store_or_cache_access() {
        let source = Arc::new(FakeSource::new(Vec::new()));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source.clone()).unwrap();
        let error = reader
            .read("tenant-b", &[format!("sha256:{}", "1".repeat(64))])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ControlArtifactReaderErrorKind::TenantScope);
        assert_eq!(source.calls(), 0);
        assert_eq!(reader.cache.len(), 0);
    }

    #[tokio::test]
    async fn malformed_requested_hash_refuses_before_store_access() {
        let source = Arc::new(FakeSource::new(Vec::new()));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source.clone()).unwrap();
        let error = reader
            .read("tenant-a", &["sha256:not-a-digest".to_string()])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ControlArtifactReaderErrorKind::Malformed);
        assert_eq!(source.calls(), 0);
        assert_eq!(reader.cache.len(), 0);
    }

    #[tokio::test]
    async fn missing_timeout_and_unavailable_remain_distinct() {
        let hash = format!("sha256:{}", "1".repeat(64));
        let missing = Arc::new(FakeSource::new(vec![Ok(Vec::new())]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, missing).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", std::slice::from_ref(&hash))
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::NotFound
        );

        for (source_kind, expected) in [
            (
                ArtifactSourceErrorKind::Timeout,
                ControlArtifactReaderErrorKind::Timeout,
            ),
            (
                ArtifactSourceErrorKind::Unavailable,
                ControlArtifactReaderErrorKind::Unavailable,
            ),
        ] {
            let mut results = vec![Err(source_error(source_kind))];
            if source_kind == ArtifactSourceErrorKind::Unavailable {
                results.push(Err(source_error(source_kind)));
            }
            let source = Arc::new(FakeSource::new(results));
            let reader = ControlArtifactReader::from_source("tenant-a", 1, source).unwrap();
            assert_eq!(
                reader
                    .read("tenant-a", std::slice::from_ref(&hash))
                    .await
                    .unwrap_err()
                    .kind(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn unavailable_reads_retry_once_while_timeouts_do_not_retry() {
        let valid = row("tenant-a", valid_plan_bytes());
        let hash = valid.execution_bundle_hash.clone();
        let transient = Arc::new(FakeSource::new(vec![
            Err(source_error(ArtifactSourceErrorKind::Unavailable)),
            Ok(vec![valid.clone()]),
        ]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, transient.clone()).unwrap();
        reader
            .read("tenant-a", std::slice::from_ref(&hash))
            .await
            .unwrap();
        assert_eq!(transient.calls(), CONTROL_ARTIFACT_MAX_ATTEMPTS);

        let exhausted = Arc::new(FakeSource::new(vec![
            Err(source_error(ArtifactSourceErrorKind::Unavailable)),
            Err(source_error(ArtifactSourceErrorKind::Unavailable)),
            Ok(vec![valid]),
        ]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, exhausted.clone()).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", std::slice::from_ref(&hash))
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Unavailable
        );
        assert_eq!(exhausted.calls(), CONTROL_ARTIFACT_MAX_ATTEMPTS);

        let timed_out = Arc::new(FakeSource::new(vec![
            Err(source_error(ArtifactSourceErrorKind::Timeout)),
            Ok(Vec::new()),
        ]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, timed_out.clone()).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", std::slice::from_ref(&hash))
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Timeout
        );
        assert_eq!(timed_out.calls(), 1);
    }

    #[tokio::test]
    async fn malformed_and_hash_mismatch_never_enter_the_cache() {
        let malformed_bytes = b"not-json".to_vec();
        let malformed = row("tenant-a", malformed_bytes);
        let malformed_hash = malformed.execution_bundle_hash.clone();
        let source = Arc::new(FakeSource::new(vec![Ok(vec![malformed])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, source).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", &[malformed_hash])
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Malformed
        );
        assert_eq!(reader.cache.len(), 0);

        let mut mismatch = row("tenant-a", valid_plan_bytes());
        mismatch.execution_bundle_hash = execution_bundle_hash(b"different-valid-plan-identity");
        let mismatch_hash = mismatch.execution_bundle_hash.clone();
        let source = Arc::new(FakeSource::new(vec![Ok(vec![mismatch])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 1, source).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", &[mismatch_hash])
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::HashMismatch
        );
        assert_eq!(reader.cache.len(), 0);
    }

    #[tokio::test]
    async fn one_invalid_row_prevents_every_fill_in_the_batch() {
        let valid = row("tenant-a", valid_plan_bytes());
        let valid_hash = valid.execution_bundle_hash.clone();
        let malformed = row("tenant-a", b"not-json".to_vec());
        let malformed_hash = malformed.execution_bundle_hash.clone();
        let source = Arc::new(FakeSource::new(vec![Ok(vec![valid, malformed])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source).unwrap();

        assert_eq!(
            reader
                .read("tenant-a", &[valid_hash.clone(), malformed_hash])
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Malformed
        );
        assert_eq!(reader.cache.len(), 0);
        assert!(reader.cache.get("tenant-a", &valid_hash).is_none());
    }

    #[tokio::test]
    async fn format_length_and_coordinate_drift_are_malformed_before_fill() {
        let base = row("tenant-a", valid_plan_bytes());
        let hash = base.execution_bundle_hash.clone();
        let mut wrong_format = base.clone();
        wrong_format.format_version = "0.2".into();
        let mut wrong_length = base.clone();
        wrong_length.byte_length += 1;
        let mut wrong_tenant = base;
        wrong_tenant.tenant_id = "tenant-b".into();

        for drifted in [wrong_format, wrong_length, wrong_tenant] {
            let source = Arc::new(FakeSource::new(vec![Ok(vec![drifted])]));
            let reader = ControlArtifactReader::from_source("tenant-a", 1, source).unwrap();
            assert_eq!(
                reader
                    .read("tenant-a", std::slice::from_ref(&hash))
                    .await
                    .unwrap_err()
                    .kind(),
                ControlArtifactReaderErrorKind::Malformed
            );
            assert_eq!(reader.cache.len(), 0);
        }
    }

    #[tokio::test]
    async fn unrequested_and_duplicate_rows_are_refused_before_fill() {
        let requested = row("tenant-a", valid_plan_bytes());
        let requested_hash = requested.execution_bundle_hash.clone();
        let mut other_bytes = valid_plan_bytes();
        other_bytes.push(b' ');
        let unrequested = row("tenant-a", other_bytes);

        let source = Arc::new(FakeSource::new(vec![Ok(vec![unrequested])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", std::slice::from_ref(&requested_hash))
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Malformed
        );
        assert_eq!(reader.cache.len(), 0);

        let source = Arc::new(FakeSource::new(vec![Ok(vec![
            requested.clone(),
            requested,
        ])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source).unwrap();
        assert_eq!(
            reader
                .read("tenant-a", &[requested_hash])
                .await
                .unwrap_err()
                .kind(),
            ControlArtifactReaderErrorKind::Malformed
        );
        assert_eq!(reader.cache.len(), 0);
    }

    #[tokio::test]
    async fn duplicate_requests_are_sorted_deduplicated_and_only_misses_are_read() {
        let first = row("tenant-a", valid_plan_bytes());
        let first_hash = first.execution_bundle_hash.clone();
        let mut second_bytes = valid_plan_bytes();
        second_bytes.push(b' ');
        let second = row("tenant-a", second_bytes);
        let second_hash = second.execution_bundle_hash.clone();
        let source = Arc::new(FakeSource::new(vec![Ok(vec![first]), Ok(vec![second])]));
        let reader = ControlArtifactReader::from_source("tenant-a", 2, source.clone()).unwrap();

        reader
            .read("tenant-a", std::slice::from_ref(&first_hash))
            .await
            .unwrap();
        let result = reader
            .read(
                "tenant-a",
                &[second_hash.clone(), first_hash.clone(), second_hash.clone()],
            )
            .await
            .unwrap();

        assert_eq!(source.calls(), 2);
        assert_eq!(result.len(), 2);
        let expected = BTreeSet::from([first_hash, second_hash]);
        assert_eq!(
            result
                .iter()
                .map(VerifiedExecutionBundle::execution_bundle_hash)
                .collect::<BTreeSet<_>>(),
            expected.iter().map(String::as_str).collect()
        );
    }

    #[test]
    fn query_is_exactly_tenant_scoped_and_projects_only_authorized_columns() {
        assert_eq!(CONTROL_ARTIFACT_POOL_MAX_SIZE, 16);
        assert_eq!(CONTROL_ARTIFACT_POOL_WAIT, Duration::from_secs(2));
        assert_eq!(CONTROL_ARTIFACT_QUERY_TIMEOUT, Duration::from_secs(5));
        assert_eq!(CONTROL_ARTIFACT_CANCEL_TIMEOUT, Duration::from_secs(2));
        assert_eq!(CONTROL_ARTIFACT_STATEMENT_TIMEOUT_MS, 5_000);
        assert_eq!(CONTROL_ARTIFACT_MAX_ATTEMPTS, 2);
        assert!(READ_EXECUTION_BUNDLES_SQL.starts_with(
            "SELECT tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length "
        ));
        assert_eq!(READ_EXECUTION_BUNDLES_SQL.matches("tenant_id").count(), 2);
        for column in [
            "tenant_id",
            "execution_bundle_hash",
            "format_version",
            "exact_bytes",
            "byte_length",
        ] {
            assert!(READ_EXECUTION_BUNDLES_SQL.contains(column));
        }
        assert!(READ_EXECUTION_BUNDLES_SQL.contains("tenant_id = $1"));
        assert!(READ_EXECUTION_BUNDLES_SQL.contains("ANY($2::text[])"));
        assert!(READ_EXECUTION_BUNDLES_SQL.contains("COLLATE \"C\""));
        assert!(!READ_EXECUTION_BUNDLES_SQL.contains("created_at"));
        assert!(!READ_EXECUTION_BUNDLES_SQL.contains("SELECT *"));
    }
}

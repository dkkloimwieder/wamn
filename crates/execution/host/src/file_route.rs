//! Native streaming file routes and operation-only signed reads.
//!
//! Uploads finish staging before the metadata transaction. Existing files retain
//! the connection generation that wrote their bytes. No guest receives a body.
//!
//! POST `/api/<application>/files?binding=<alias>&name=<filename>` sends the raw
//! request body. Its Content-Type supplies the media type. GET
//! `/api/<application>/files/<id>` streams the stored bytes after session admission.

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::TryStreamExt as _;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Body as _, Frame};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, ReadBuf};
use uuid::Uuid;
use wamn_catalog::{ComponentConnectionType, HostAttachment, HostHandler};
use wamn_engine::flow_http_routing::{
    AuthenticatedCaller, CredentialType, FlowHttpRouting, Header,
};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_platform_identity::PrincipalId;
use wamn_runtime::plugins::wamn_blobstore::files::{
    FileStorageError, FileStorageErrorKind, FileStore, MAX_URL_LIFETIME,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;
use wasmtime_wasi_http::WasiBody;

const FILE_COLUMNS: &str = "id::text, connection_id::text, connection_generation, media_type, size, exposure, url, blob_key";

/// Exposure stored in the platform file catalog. Unknown literals refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    Private,
    Application,
    Public,
}

impl Exposure {
    fn literal(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Application => "application",
            Self::Public => "public",
        }
    }
}

/// Metadata returned by the short catalog read.
#[derive(Debug, Clone)]
struct FileRecord {
    id: Uuid,
    connection_id: String,
    generation: i64,
    media_type: String,
    size: i64,
    exposure: Exposure,
    url: Option<String>,
    blob_key: String,
}

/// The refusal literals of the file operation contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRefusal {
    InvalidInput,
    PermissionDenied,
    NotFound,
    Private,
    BindingRefused,
    Referenced,
    CdnHostRequired,
}

impl FileRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "file_not_found",
            Self::Private => "file_private",
            Self::BindingRefused => "binding_refused",
            Self::Referenced => "file_referenced",
            Self::CdnHostRequired => "cdn_host_required",
        }
    }
    fn status(self) -> u16 {
        match self {
            Self::InvalidInput => 400,
            Self::PermissionDenied | Self::Private | Self::BindingRefused => 403,
            Self::NotFound => 404,
            Self::Referenced | Self::CdnHostRequired => 409,
        }
    }
}

/// A file failure with the input field, refusal, and original cause.
#[derive(Debug)]
pub struct FileError {
    pub refusal: Option<FileRefusal>,
    pub field: &'static str,
    source: Option<anyhow::Error>,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(error) => write!(f, "file {} failed: {error}", self.field),
            None => write!(f, "file {} refused: {:?}", self.field, self.refusal),
        }
    }
}
impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(std::convert::AsRef::as_ref)
    }
}
fn refused(refusal: FileRefusal, field: &'static str) -> FileError {
    FileError {
        refusal: Some(refusal),
        field,
        source: None,
    }
}
fn failed(field: &'static str, source: impl Into<anyhow::Error>) -> FileError {
    FileError {
        refusal: None,
        field,
        source: Some(source.into()),
    }
}
impl From<tokio_postgres::Error> for FileError {
    fn from(error: tokio_postgres::Error) -> Self {
        if error.code() == Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION) {
            Self {
                refusal: Some(FileRefusal::Referenced),
                field: "file_id",
                source: Some(error.into()),
            }
        } else {
            failed("catalog", error)
        }
    }
}
impl From<FileStorageError> for FileError {
    fn from(error: FileStorageError) -> Self {
        let refusal = match error.kind {
            FileStorageErrorKind::NotFound => Some(FileRefusal::NotFound),
            FileStorageErrorKind::Refused => Some(FileRefusal::BindingRefused),
            FileStorageErrorKind::Body => Some(FileRefusal::InvalidInput),
            FileStorageErrorKind::Backend => None,
        };
        Self {
            refusal,
            field: "binding",
            source: Some(error.into()),
        }
    }
}

/// One generation selected by the scoped registry reader, never by the caller.
#[derive(Debug, Clone)]
pub struct FileConnection {
    pub id: String,
    pub name: String,
    pub generation: i64,
    pub definition: Value,
    pub read_only: bool,
}

/// The registry authority needed by native file handlers.
#[async_trait::async_trait]
pub trait FileConnections: Send + Sync + fmt::Debug {
    /// Resolve an application's binding, or an existing file's pinned generation.
    async fn lookup(
        &self,
        application: &str,
        binding: Option<&str>,
        pinned: Option<(&str, i64)>,
    ) -> Result<FileConnection, FileError>;
}

/// Login-scoped registry lookup. This client holds the connection-reader login.
///
/// `registry.lookup_file_connection(org, project, environment, application,
/// binding, connection_id, generation)` returns `connection_id uuid`,
/// `connection_name text`, `generation bigint`, `parameters jsonb`, and `mode text`.
/// Binding and both pin fields are nullable. The registry must enforce the
/// reader login's scope, active project enablement, and the application binding.
/// A pin selects that immutable generation, including its provider parameters.
pub struct RegistryFileConnections {
    pub client: Option<Arc<tokio_postgres::Client>>,
    pub org: String,
    pub project: String,
    pub environment: String,
}
impl fmt::Debug for RegistryFileConnections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryFileConnections")
            .field("project", &self.project)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}
#[async_trait::async_trait]
impl FileConnections for RegistryFileConnections {
    async fn lookup(
        &self,
        application: &str,
        binding: Option<&str>,
        pinned: Option<(&str, i64)>,
    ) -> Result<FileConnection, FileError> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| failed("binding", anyhow::anyhow!("connection reader unavailable")))?;
        let row = client.query_opt(
            "SELECT connection_id::text, connection_name, generation, parameters, mode FROM registry.lookup_file_connection($1,$2,$3,$4,$5,$6,$7)",
            &[&self.org, &self.project, &self.environment, &application, &binding, &pinned.map(|pin| pin.0), &pinned.map(|pin| pin.1)],
        ).await?.ok_or_else(|| refused(FileRefusal::BindingRefused, "binding"))?;
        let mode: String = row.try_get(4)?;
        let read_only = match mode.as_str() {
            "read" => true,
            "write" => false,
            _ => return Err(refused(FileRefusal::BindingRefused, "binding")),
        };
        Ok(FileConnection {
            id: row.try_get(0)?,
            name: row.try_get(1)?,
            generation: row.try_get(2)?,
            definition: row.try_get(3)?,
            read_only,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRequest {
    application: String,
    file_id: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    application: String,
    file_id: Uuid,
    binding: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposeRequest {
    application: String,
    file_id: Uuid,
    binding: String,
    exposure: Exposure,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    binding: String,
    name: String,
}

/// File handlers of one released application environment.
pub struct FileRoutes {
    release: Arc<LoadedRelease>,
    postgres: Arc<WamnPostgres>,
    blobstore: Arc<WamnBlobstore>,
    connections: Arc<dyn FileConnections>,
    project: String,
    administration_url: Option<String>,
}
impl fmt::Debug for FileRoutes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRoutes")
            .field("release", self.release.release())
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// A session advisory lock holds no database transaction during object I/O.
struct FileLock {
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for FileLock {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl FileRoutes {
    pub fn new(
        release: Arc<LoadedRelease>,
        postgres: Arc<WamnPostgres>,
        blobstore: Arc<WamnBlobstore>,
        connections: Arc<dyn FileConnections>,
        project: String,
        administration_url: Option<String>,
    ) -> Self {
        Self {
            release,
            postgres,
            blobstore,
            connections,
            project,
            administration_url,
        }
    }

    async fn lock(&self, id: Uuid) -> Result<(tokio_postgres::Client, FileLock), FileError> {
        let url = self.administration_url.as_deref().ok_or_else(|| {
            failed(
                "administration",
                anyhow::anyhow!("file mutation requires the administration login"),
            )
        })?;
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
        let guard = FileLock {
            driver: tokio::spawn(async move {
                if let Err(error) = connection.await {
                    tracing::warn!(%error, "file lock connection failed");
                }
            }),
        };
        let allowed: bool = client.query_one("SELECT NOT rolsuper AND pg_has_role(current_user, 'wamn_administration', 'MEMBER') FROM pg_roles WHERE rolname = current_user", &[]).await?.try_get(0)?;
        if !allowed {
            return Err(refused(FileRefusal::PermissionDenied, "administration"));
        }
        client
            .query_one(
                "SELECT pg_advisory_lock(hashtextextended($1, 0))",
                &[&format!("wamn:file:{id}")],
            )
            .await?;
        Ok((client, guard))
    }

    async fn store(
        &self,
        application: &str,
        binding: Option<&str>,
        pinned: Option<(&str, i64)>,
        write: bool,
    ) -> Result<(FileConnection, FileStore), FileError> {
        let app = self
            .release
            .scope()
            .applications
            .get(application)
            .ok_or_else(|| refused(FileRefusal::BindingRefused, "application"))?;
        if binding.is_some_and(|alias| {
            !app.connections.contains_key(alias) || !declares_store(&self.release, app, alias)
        }) {
            return Err(refused(FileRefusal::BindingRefused, "binding"));
        }
        let connection = self
            .connections
            .lookup(application, binding, pinned)
            .await?;
        require_binding(
            &self.release,
            application,
            binding,
            pinned,
            &connection,
            write,
        )?;
        let store = self
            .blobstore
            .file_store(
                &connection.name,
                connection.generation,
                &connection.definition,
                connection.read_only,
            )
            .await?;
        Ok((connection, store))
    }

    async fn record(
        &self,
        principal: &PrincipalId,
        application: &str,
        id: Uuid,
        action: &str,
    ) -> Result<FileRecord, FileError> {
        if !self.release.scope().applications.contains_key(application) {
            return Err(refused(FileRefusal::NotFound, "file_id"));
        }
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM platform.file WHERE id = $1::text::uuid AND application = $2"
        );
        self.postgres
            .administration_transaction(
                &self.project,
                &self.release.scope().env_key,
                principal,
                &format!(
                    "wamn-control:file/{action}@{}",
                    wamn_catalog::host_route_version()
                ),
                async |client| {
                    let row = client
                        .query_opt(&sql, &[&id.to_string(), &application])
                        .await?
                        .ok_or_else(|| refused(FileRefusal::NotFound, "file_id"))?;
                    decode_record(&row)
                },
            )
            .await
            .map_err(|error| failed("catalog", error))?
    }

    /// Stage bytes, commit one row, then promote staging to the live key.
    pub async fn put(
        &self,
        caller: &AuthenticatedCaller,
        application: &str,
        binding: &str,
        name: &str,
        media_type: &str,
        body: impl AsyncRead + Unpin + Send,
    ) -> Result<Uuid, FileError> {
        let principal = principal(caller, "put")?;
        if name.is_empty() || name.contains(['\0', '\r', '\n']) || !valid_media_type(media_type) {
            return Err(refused(FileRefusal::InvalidInput, "metadata"));
        }
        let (connection, store) = self.store(application, Some(binding), None, true).await?;
        let id = Uuid::new_v4();
        let staged = store.stage(id, body).await?;
        let size = i64::try_from(staged.size).map_err(|error| failed("size", error))?;
        self.postgres.administration_transaction(&self.project, &self.release.scope().env_key, &principal,
            &format!("wamn-control:file/put@{}", wamn_catalog::host_route_version()), async |client| {
                client.execute("INSERT INTO platform.file (id, application, connection_id, connection_generation, name, media_type, size, sha256, blob_key, exposure) VALUES ($1::text::uuid,$2,$3::text::uuid,$4,$5,$6,$7,$8,$9,'application')",
                    &[&id.to_string(), &application, &connection.id, &staged.connection_generation, &name, &media_type, &size, &staged.sha256, &staged.blob_key]).await?;
                Ok::<_, FileError>(())
            }).await.map_err(|error| failed("catalog", error))??;
        // A committed row remains readable through staging if promotion fails.
        if let Err(error) = store.finish(id).await {
            tracing::warn!(%error, %id, "file promotion deferred to the next read");
        }
        Ok(id)
    }

    /// Only a session with file.get reaches the application URL, including public files.
    async fn get(
        &self,
        caller: &AuthenticatedCaller,
        application: &str,
        id: Uuid,
    ) -> Result<hyper::Response<WasiBody>, FileError> {
        let principal = principal(caller, "get")?;
        if caller.credential_type() != CredentialType::Session {
            return Err(refused(FileRefusal::PermissionDenied, "session"));
        }
        let row = self.record(&principal, application, id, "get").await?;
        require_exposure(row.exposure)?;
        let (_, store) = self
            .store(
                application,
                None,
                Some((&row.connection_id, row.generation)),
                false,
            )
            .await?;
        let body = store
            .get(id)
            .await?
            .into_stream()
            .map_ok(Frame::data)
            .map_err(|error| {
                tracing::warn!(%error, "file response stream failed");
                wasmtime_wasi_http::Error::InternalError(Some("file response stream failed".into()))
            });
        hyper::Response::builder()
            .status(200)
            .header("content-type", &row.media_type)
            .header("content-length", row.size)
            .header("cache-control", "private, no-store")
            .header("x-content-type-options", "nosniff")
            .body(StreamBody::new(body).boxed_unsync())
            .map_err(|error| failed("response", error))
    }

    async fn delete(
        &self,
        caller: &AuthenticatedCaller,
        request: DeleteRequest,
    ) -> Result<Value, FileError> {
        let principal = principal(caller, "delete")?;
        let (_client, _lock) = self.lock(request.file_id).await?;
        let row = self
            .record(&principal, &request.application, request.file_id, "delete")
            .await?;
        let (connection, store) = self
            .store(
                &request.application,
                Some(&request.binding),
                Some((&row.connection_id, row.generation)),
                true,
            )
            .await?;
        self.postgres.administration_transaction(&self.project, &self.release.scope().env_key, &principal,
            &format!("wamn-control:file/delete@{}", wamn_catalog::host_route_version()), async |client| {
                let deleted = client.execute("DELETE FROM platform.file WHERE id = $1::text::uuid AND application = $2", &[&request.file_id.to_string(), &request.application]).await?;
                if deleted != 1 { return Err(refused(FileRefusal::NotFound, "file_id")); }
                Ok::<_, FileError>(())
            }).await.map_err(|error| failed("catalog", error))??;
        if let Err(error) = store.delete(request.file_id).await {
            // Metadata deletion is committed. The collector (wamn-2wdsf.4)
            // owns orphan cleanup, so this path does not retry object deletion.
            tracing::warn!(%error, file_id = %request.file_id, connection_id = %row.connection_id,
                generation = row.generation, object_key = %row.blob_key,
                bucket = connection.definition.get("container").and_then(|value| value.as_str()).unwrap_or_default(),
                prefix = connection.definition.get("prefix").and_then(|value| value.as_str()).unwrap_or_default(),
                "file metadata deletion committed but object cleanup failed");
        }
        Ok(json!({"file_id": request.file_id}))
    }

    async fn expose(
        &self,
        caller: &AuthenticatedCaller,
        request: ExposeRequest,
    ) -> Result<Value, FileError> {
        let principal = principal(caller, "expose")?;
        let (_client, _lock) = self.lock(request.file_id).await?;
        let row = self
            .record(&principal, &request.application, request.file_id, "expose")
            .await?;
        let (connection, store) = self
            .store(
                &request.application,
                Some(&request.binding),
                Some((&row.connection_id, row.generation)),
                true,
            )
            .await?;
        if request.exposure == Exposure::Public
            && connection
                .definition
                .get("cdn_host")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(refused(FileRefusal::CdnHostRequired, "exposure"));
        }
        let published = request.exposure == Exposure::Public && row.exposure != Exposure::Public;
        let url = change_exposure(
            &store,
            request.file_id,
            row.exposure,
            row.url,
            request.exposure,
        )
        .await?;
        let written = self.postgres.administration_transaction(&self.project, &self.release.scope().env_key, &principal,
            &format!("wamn-control:file/expose@{}", wamn_catalog::host_route_version()), async |client| {
                let updated = client.execute("UPDATE platform.file SET exposure = $3, url = $4 WHERE id = $1::text::uuid AND application = $2 AND (exposure IS DISTINCT FROM $3 OR url IS DISTINCT FROM $4)",
                    &[&request.file_id.to_string(), &request.application, &request.exposure.literal(), &url]).await?;
                Ok::<_, FileError>(updated)
            }).await.map_err(|error| failed("catalog", error))?;
        if let Err(error) = written {
            // A confirmed rollback can remove a newly copied public object.
            // A lost COMMIT answer remains uncertain and reaches the caller above.
            if published && let Err(cleanup) = store.unexpose(request.file_id).await {
                tracing::warn!(%cleanup, file_id = %request.file_id, "public copy cleanup after catalog rollback failed");
            }
            return Err(error);
        }
        Ok(json!({"file_id": request.file_id, "exposure": request.exposure, "url": url}))
    }

    /// Sign only under an active native operation's component authority.
    /// This method has no HTTP request field or guest byte-transfer interface.
    pub async fn signed_url(
        &self,
        invocations: &WamnBlobstore,
        component_id: &str,
        caller: &AuthenticatedCaller,
        id: Uuid,
        lifetime: Duration,
    ) -> Result<String, FileError> {
        let lifetime = signed_lifetime(lifetime)?;
        let invocation = invocations
            .invocation(component_id)
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        if !matches!(
            invocation.closure,
            wamn_runtime::plugins::connection_http::ConnectionExecutionClosure::Released
        ) {
            return Err(refused(FileRefusal::PermissionDenied, "operation"));
        }
        let application = wamn_catalog::application_for_package(
            &self.release.scope().applications,
            &invocation.package_id,
        )
        .ok_or_else(|| refused(FileRefusal::BindingRefused, "application"))?;
        // A bound invocation supplies the operation authority. The file.get URL
        // grant is deliberately separate from this operation's signed read.
        let served = self
            .release
            .manifest()
            .components
            .iter()
            .find(|component| {
                component.package_id == invocation.package_id
                    && component.descriptor.component_digest.as_str() == invocation.component_digest
            })
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        let operation = served
            .operations
            .get(&invocation.operation)
            .and_then(|operation| operation.registered_operation.as_deref())
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        if !caller.permits(operation) {
            return Err(refused(FileRefusal::PermissionDenied, "operation"));
        }
        let principal: PrincipalId = caller
            .principal_id()
            .parse()
            .map_err(|error| failed("principal", error))?;
        let row = self.record(&principal, application, id, "get").await?;
        let pin = Some((row.connection_id.as_str(), row.generation));
        let connection = self.connections.lookup(application, None, pin).await?;
        require_binding(&self.release, application, None, pin, &connection, false)?;
        // Signing is the host's declared result work. The operation grant
        // supplies read authority without a guest blobstore import. The
        // application's declared binding and pinned generation still apply.
        let store = self
            .blobstore
            .file_store(
                &connection.name,
                connection.generation,
                &connection.definition,
                connection.read_only,
            )
            .await?;
        store.signed_url(id, lifetime).await.map_err(Into::into)
    }

    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<Value, FileError> {
        match attachment.route.handler {
            HostHandler::FilePut => Err(refused(FileRefusal::InvalidInput, "bytes")),
            HostHandler::FileGet => {
                let request: FileRequest = decode(payload)?;
                let principal = principal(caller, "get")?;
                let row = self
                    .record(&principal, &request.application, request.file_id, "get")
                    .await?;
                require_exposure(row.exposure)?;
                Ok(json!({"file_id": row.id, "exposure": row.exposure, "url": row.url}))
            }
            HostHandler::FileDelete => self.delete(caller, decode(payload)?).await,
            HostHandler::FileExpose => self.expose(caller, decode(payload)?).await,
            _ => Err(refused(FileRefusal::InvalidInput, "operation")),
        }
    }

    /// Handle reserved native file URLs after the ingress selects its workload.
    pub fn serves_path(&self, path: &str) -> bool {
        native_route(path).is_some()
    }

    pub async fn http(
        &self,
        routing: &FlowHttpRouting,
        request: hyper::Request<WasiBody>,
    ) -> Option<hyper::Response<WasiBody>> {
        let route = native_route(request.uri().path())?;
        let result = self.http_request(routing, request, route).await;
        Some(match result {
            Ok(response) => response,
            Err(error) => error_response(&error),
        })
    }

    async fn http_request(
        &self,
        routing: &FlowHttpRouting,
        request: hyper::Request<WasiBody>,
        route: NativeRoute,
    ) -> Result<hyper::Response<WasiBody>, FileError> {
        if route == NativeRoute::Invalid {
            return Err(refused(FileRefusal::InvalidInput, "path"));
        }
        let action = match (&route, request.method().as_str()) {
            (NativeRoute::Upload { .. }, "POST") => "put",
            (NativeRoute::Get { .. }, "GET") => "get",
            _ => return Ok(response(405, &json!({"error":"method_not_allowed"}))),
        };
        let id = format!("wamn-control-file-{action}-http");
        let Some(permit) = routing.try_acquire_route(&id) else {
            return Ok(response(429, &json!({"error":"route-capacity-exhausted"})));
        };
        let headers: Result<Vec<_>, _> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                value.to_str().map(|value| Header {
                    name: name.to_string(),
                    value: value.to_owned(),
                })
            })
            .collect();
        let headers = headers.map_err(|_| refused(FileRefusal::InvalidInput, "headers"))?;
        let caller = match routing.authenticate(&id, &headers, &[]).await {
            Ok(Some(caller)) => caller,
            Ok(None) => return Ok(response(401, &json!({"error":"unauthorized"}))),
            Err(error) => return Ok(response(error.status, &json!({"error":error.code}))),
        };
        principal(&caller, action)?;
        let response = match route {
            NativeRoute::Invalid => Err(refused(FileRefusal::InvalidInput, "path")),
            NativeRoute::Get { application, id } => self.get(&caller, &application, id).await,
            NativeRoute::Upload { application } => {
                let query = request
                    .uri()
                    .query()
                    .ok_or_else(|| refused(FileRefusal::InvalidInput, "binding"))?;
                let mut fields = BTreeMap::new();
                for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
                    if fields
                        .insert(key.into_owned(), value.into_owned())
                        .is_some()
                    {
                        return Err(refused(FileRefusal::InvalidInput, "query"));
                    }
                }
                let query: UploadQuery = serde_json::from_value(json!(fields))
                    .map_err(|_| refused(FileRefusal::InvalidInput, "query"))?;
                let media_type = request
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| refused(FileRefusal::InvalidInput, "media_type"))?
                    .to_owned();
                let id = self
                    .put(
                        &caller,
                        &application,
                        &query.binding,
                        &query.name,
                        &media_type,
                        BodyReader {
                            body: request.into_body(),
                            chunk: Bytes::new(),
                        },
                    )
                    .await?;
                Ok(response(201, &json!({"file_id":id})))
            }
        }?;
        let (parts, body) = response.into_parts();
        Ok(hyper::Response::from_parts(
            parts,
            PermitBody {
                body,
                _permit: permit,
            }
            .boxed_unsync(),
        ))
    }
}

#[async_trait::async_trait]
trait FilePublication: Send + Sync {
    async fn publish(&self, id: Uuid) -> Result<String, FileError>;
    async fn unpublish(&self, id: Uuid) -> Result<(), FileError>;
}
#[async_trait::async_trait]
impl FilePublication for FileStore {
    async fn publish(&self, id: Uuid) -> Result<String, FileError> {
        self.expose(id).await.map_err(Into::into)
    }
    async fn unpublish(&self, id: Uuid) -> Result<(), FileError> {
        self.unexpose(id).await.map_err(Into::into)
    }
}
async fn change_exposure(
    store: &impl FilePublication,
    id: Uuid,
    current: Exposure,
    url: Option<String>,
    requested: Exposure,
) -> Result<Option<String>, FileError> {
    if requested == Exposure::Public {
        if current == Exposure::Public {
            return url.map(Some).ok_or_else(|| {
                failed(
                    "exposure",
                    anyhow::anyhow!("a public file must retain its URL"),
                )
            });
        }
        Ok(Some(store.publish(id).await?))
    } else {
        // Repeat deletion after a prior failed metadata update as well.
        store.unpublish(id).await?;
        Ok(None)
    }
}

fn principal(caller: &AuthenticatedCaller, action: &str) -> Result<PrincipalId, FileError> {
    if !caller.permits(&format!("wamn-control:file/{action}")) {
        return Err(refused(FileRefusal::PermissionDenied, "operation"));
    }
    caller
        .principal_id()
        .parse()
        .map_err(|error| failed("principal", error))
}

fn declares_store(
    release: &LoadedRelease,
    app: &wamn_catalog::ApplicationDefinition,
    alias: &str,
) -> bool {
    release.manifest().components.iter().any(|component| {
        app.packages.contains(&component.package_id)
            && component.descriptor.connections.iter().any(|requirement| {
                requirement.store_alias == alias
                    && requirement.requirement_type == ComponentConnectionType::Blobstore
            })
    })
}

fn require_binding(
    release: &LoadedRelease,
    application: &str,
    binding: Option<&str>,
    pinned: Option<(&str, i64)>,
    connection: &FileConnection,
    write: bool,
) -> Result<(), FileError> {
    let app = release
        .scope()
        .applications
        .get(application)
        .ok_or_else(|| refused(FileRefusal::BindingRefused, "application"))?;
    let bound = match binding {
        Some(alias) => {
            declares_store(release, app, alias)
                && app.connections.get(alias) == Some(&connection.name)
        }
        None => app
            .connections
            .iter()
            .any(|(alias, name)| name == &connection.name && declares_store(release, app, alias)),
    };
    if !bound
        || connection.generation <= 0
        || connection.id.parse::<Uuid>().is_err()
        || write && connection.read_only
        || pinned.is_some_and(|(id, generation)| {
            id != connection.id || generation != connection.generation
        })
    {
        return Err(refused(FileRefusal::BindingRefused, "binding"));
    }
    Ok(())
}
fn decode<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, FileError> {
    serde_json::from_str(payload).map_err(|_| refused(FileRefusal::InvalidInput, "input"))
}
fn decode_record(row: &tokio_postgres::Row) -> Result<FileRecord, FileError> {
    let id: String = row.try_get(0)?;
    let exposure: String = row.try_get(5)?;
    let file = FileRecord {
        id: id.parse().map_err(|error| failed("file_id", error))?,
        connection_id: row.try_get(1)?,
        generation: row.try_get(2)?,
        media_type: row.try_get(3)?,
        size: row.try_get(4)?,
        exposure: serde_json::from_value(json!(exposure))
            .map_err(|error| failed("exposure", error))?,
        url: row.try_get(6)?,
        blob_key: row.try_get(7)?,
    };
    if file.generation <= 0
        || file.size < 0
        || !valid_media_type(&file.media_type)
        || (file.exposure == Exposure::Public) != file.url.is_some()
    {
        return Err(failed(
            "catalog",
            anyhow::anyhow!("file catalog contains invalid security metadata"),
        ));
    }
    Ok(file)
}
fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && !value.bytes().any(|byte| byte < 32 || byte == 127)
        && value.contains('/')
}
fn require_exposure(exposure: Exposure) -> Result<(), FileError> {
    if exposure == Exposure::Private {
        Err(refused(FileRefusal::Private, "file_id"))
    } else {
        Ok(())
    }
}
fn signed_lifetime(requested: Duration) -> Result<Duration, FileError> {
    if requested.is_zero() {
        Err(refused(FileRefusal::InvalidInput, "lifetime"))
    } else {
        Ok(requested.min(MAX_URL_LIFETIME))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NativeRoute {
    Invalid,
    Upload { application: String },
    Get { application: String, id: Uuid },
}
fn native_route(path: &str) -> Option<NativeRoute> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "api", application, "files"] if !application.is_empty() => Some(NativeRoute::Upload {
            application: (*application).to_owned(),
        }),
        ["", "api", application, "files", id] if !application.is_empty() => {
            Some(NativeRoute::Get {
                application: (*application).to_owned(),
                id: match id.parse() {
                    Ok(id) => id,
                    Err(_) => return Some(NativeRoute::Invalid),
                },
            })
        }
        ["", "api", _, "files", ..] => Some(NativeRoute::Invalid),
        _ => None,
    }
}
fn response(status: u16, value: &Value) -> hyper::Response<WasiBody> {
    hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(
            Full::new(Bytes::from(value.to_string()))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .expect("fixed file response headers are valid")
}
fn error_response(error: &FileError) -> hyper::Response<WasiBody> {
    if let Some(refusal) = error.refusal {
        response(
            refusal.status(),
            &json!({"error":{"code":refusal.code(),"detail":{"field":error.field}}}),
        )
    } else {
        tracing::warn!(%error,"native file route failed");
        response(503, &json!({"error":"file_unavailable"}))
    }
}

struct PermitBody {
    body: WasiBody,
    _permit: wamn_engine::flow_http_routing::RoutePermit,
}
impl hyper::body::Body for PermitBody {
    type Data = Bytes;
    type Error = wasmtime_wasi_http::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

/// Adapt native request frames without collecting the body.
struct BodyReader {
    body: WasiBody,
    chunk: Bytes,
}
impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if !self.chunk.is_empty() {
                let count = self.chunk.len().min(output.remaining());
                output.put_slice(&self.chunk.split_to(count));
                return Poll::Ready(Ok(()));
            }
            match std::task::ready!(Pin::new(&mut self.body).poll_frame(cx)) {
                None => return Poll::Ready(Ok(())),
                Some(Ok(frame)) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.chunk = bytes;
                    }
                }
                Some(Err(error)) => return Poll::Ready(Err(std::io::Error::other(error))),
            }
        }
    }
}

/// Intercept file transfers on the native HTTP service channel.
/// The ingress still owns TLS, hostname selection, and connection limits.
pub struct FileIngress {
    inner: Arc<dyn wash_runtime::host::http::HostHandler>,
    files: Arc<FileRoutes>,
    routing: Arc<FlowHttpRouting>,
    tasks: tokio::sync::Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
}
impl fmt::Debug for FileIngress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileIngress")
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}
impl FileIngress {
    pub fn new(
        inner: Arc<dyn wash_runtime::host::http::HostHandler>,
        files: Arc<FileRoutes>,
        routing: Arc<FlowHttpRouting>,
    ) -> Self {
        Self {
            inner,
            files,
            routing,
            tasks: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }
    async fn unbind(&self, workload: &str) {
        if let Some(task) = self.tasks.lock().await.remove(workload) {
            task.abort();
        }
    }
}
impl Drop for FileIngress {
    fn drop(&mut self) {
        for task in self.tasks.get_mut().values() {
            task.abort();
        }
    }
}

#[async_trait::async_trait]
impl wash_runtime::host::http::HostHandler for FileIngress {
    async fn inject_meters(&self, meters: &wash_runtime::observability::Meters) {
        self.inner.inject_meters(meters).await;
    }
    async fn start(&self) -> anyhow::Result<()> {
        self.inner.start().await
    }
    async fn stop(&self) -> anyhow::Result<()> {
        for task in self.tasks.lock().await.values() {
            task.abort();
        }
        self.inner.stop().await
    }
    async fn stopped(&self) {
        self.inner.stopped().await;
    }
    fn port(&self) -> u16 {
        self.inner.port()
    }
    async fn on_workload_resolved(
        &self,
        workload: &wash_runtime::engine::workload::ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .on_workload_resolved(workload, component_id)
            .await
    }
    async fn on_workload_unbind(&self, workload: &str) -> anyhow::Result<()> {
        self.inner.on_workload_unbind(workload).await?;
        self.unbind(workload).await;
        Ok(())
    }
    async fn on_service_http_resolved(
        &self,
        workload: &str,
        routes: &[wash_runtime::host::http::IngressRoute],
        next: tokio::sync::mpsc::Sender<wash_runtime::host::http::ServiceHttpJob>,
    ) -> anyhow::Result<()> {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<wash_runtime::host::http::ServiceHttpJob>(1);
        let files = Arc::clone(&self.files);
        let routing = Arc::clone(&self.routing);
        let max_calls = routing.in_flight_limit();
        let task = tokio::spawn(async move {
            let mut calls = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = calls.join_next(), if !calls.is_empty() => {
                        if let Err(error) = result { tracing::warn!(%error, "native file handler task failed"); }
                    }
                    job = receiver.recv(), if calls.len() < max_calls => {
                        let Some(mut job) = job else { break; };
                        if !files.serves_path(job.req.uri().path()) {
                            if next.send(job).await.is_err() { break; }
                            continue;
                        }
                        let files = Arc::clone(&files);
                        let routing = Arc::clone(&routing);
                        calls.spawn(async move {
                            tokio::select! {
                                () = job.resp_tx.closed() => {},
                                response = files.http(&routing, job.req) => {
                                    if let Some(response) = response { let _ = job.resp_tx.send(Ok(response)); }
                                }
                            }
                        });
                    }
                }
            }
        });
        if let Err(error) = self
            .inner
            .on_service_http_resolved(workload, routes, sender)
            .await
        {
            task.abort();
            return Err(error);
        }
        if let Some(previous) = self.tasks.lock().await.insert(workload.to_owned(), task) {
            previous.abort();
        }
        Ok(())
    }
    async fn on_service_http_unbind(&self, workload: &str) -> anyhow::Result<()> {
        self.inner.on_service_http_unbind(workload).await?;
        self.unbind(workload).await;
        Ok(())
    }
    async fn on_trigger_service_messaging_resolved(
        &self,
        workload: &str,
        sender: tokio::sync::mpsc::Sender<wash_runtime::host::trigger_service::MessagingJob>,
    ) -> anyhow::Result<()> {
        self.inner
            .on_trigger_service_messaging_resolved(workload, sender)
            .await
    }
    async fn on_trigger_service_messaging_unbind(&self, workload: &str) -> anyhow::Result<()> {
        self.inner
            .on_trigger_service_messaging_unbind(workload)
            .await
    }
    async fn deliver_trigger_service_message(
        &self,
        workload: &str,
        message: wash_runtime::host::trigger_service::BrokerMessage,
        attributes: Arc<[opentelemetry::KeyValue]>,
    ) -> anyhow::Result<Result<(), String>> {
        self.inner
            .deliver_trigger_service_message(workload, message, attributes)
            .await
    }
    async fn has_trigger_service_messaging(&self, workload: &str) -> bool {
        self.inner.has_trigger_service_messaging(workload).await
    }
    fn outgoing_request(
        &self,
        workload: &str,
        request: hyper::Request<WasiBody>,
        options: Option<wasmtime_wasi_http::RequestOptions>,
        future: wash_runtime::host::http::RequestIoFuture,
        allowed: &[wash_runtime::host::allowed_hosts::AllowedHost],
    ) -> wash_runtime::host::http::SendFuture {
        self.inner
            .outgoing_request(workload, request, options, future, allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};
    use tokio::io::AsyncReadExt as _;
    use wamn_catalog::{
        ApplicationDefinition, ComponentConnection, ComponentDescriptor, HostRouteSet,
        PackageCoordinate, ServingComponent, ServingManifest, ServingRelease,
    };
    use wamn_engine::flow_http_routing::{
        AuthRejection, AuthenticationRequest, RouteAuthenticator, RouteInFlightLimit, unauthorized,
    };
    use wamn_engine::release_manifest::ReleaseScope;
    use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
    use wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig;

    fn release() -> Arc<LoadedRelease> {
        let mut descriptor =
            ComponentDescriptor::named("files", "0.1", format!("sha256:{}", "a".repeat(64)));
        descriptor.connections = vec![ComponentConnection {
            store_alias: "documents".into(),
            requirement_type: ComponentConnectionType::Blobstore,
        }];
        let mut manifest = ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("app", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                package_id: "app".into(),
                runtime_schema: None,
                descriptor,
                operations: BTreeMap::from([(
                    "request".into(),
                    wamn_catalog::ServingComponentOperation {
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        committed_result_schema: None,
                        statements: BTreeMap::new(),
                    },
                )]),
                statement_instances: BTreeMap::new(),
            }]),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap();
        manifest.host_routes.insert(HostRouteSet::Application);
        Arc::new(
            LoadedRelease::load_canonical_bytes(
                &manifest.canonical_bytes(),
                "files-test",
                ReleaseScope::new("acme", "dev").with_applications(BTreeMap::from([(
                    "orders".into(),
                    ApplicationDefinition {
                        packages: BTreeSet::from(["app".into()]),
                        connections: BTreeMap::from([("documents".into(), "store".into())]),
                    },
                )])),
            )
            .unwrap(),
        )
    }
    fn connection() -> FileConnection {
        FileConnection {
            id: "11111111-1111-4111-8111-111111111111".into(),
            name: "store".into(),
            generation: 3,
            definition: json!({}),
            read_only: false,
        }
    }
    fn caller(
        action: Option<&str>,
        admin: bool,
        credential: CredentialType,
    ) -> AuthenticatedCaller {
        AuthenticatedCaller::new(
            "wamn-control-file-get-http",
            "22222222-2222-4222-8222-222222222222",
            credential,
            admin,
            action
                .map(|action| format!("wamn-control:file/{action}"))
                .into_iter()
                .collect::<HashSet<_>>(),
            None,
        )
    }
    #[derive(Debug)]
    struct NoConnections;
    #[async_trait::async_trait]
    impl FileConnections for NoConnections {
        async fn lookup(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<(&str, i64)>,
        ) -> Result<FileConnection, FileError> {
            panic!("a refused request must not reach connection authority")
        }
    }
    fn files() -> FileRoutes {
        let postgres = Arc::new(
            WamnPostgres::new(WamnPostgresConfig {
                credentials: None,
                guest_pool_max_size: 1,
                platform_pool_max_size: 1,
                wait_timeout_ms: 1,
                statement_timeout_ms: 1,
                row_limit: 1,
            })
            .unwrap(),
        );
        let release = release();
        let blobstore = Arc::new(WamnBlobstore::new(
            postgres.clone(),
            Arc::new(WamnCredentials::empty()),
            "acme",
            "project",
            Some(release.clone()),
        ));
        FileRoutes::new(
            release,
            postgres,
            blobstore,
            Arc::new(NoConnections),
            "project".into(),
            None,
        )
    }
    #[derive(Debug)]
    struct SessionWithoutGrant;
    #[async_trait::async_trait]
    impl RouteAuthenticator for SessionWithoutGrant {
        async fn authenticate(
            &self,
            request: AuthenticationRequest<'_>,
        ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
            if request
                .headers
                .iter()
                .any(|header| header.name == "authorization")
            {
                Ok(Some(caller(None, false, CredentialType::Session)))
            } else {
                Err(unauthorized())
            }
        }
    }

    #[test]
    fn file_inputs_refuse_missing_security_fields_and_unknown_members() {
        let id = Uuid::new_v4();
        assert!(
            decode::<FileRequest>(&json!({"application":"orders","file_id":id}).to_string())
                .is_ok()
        );
        for payload in [
            json!({"file_id":id}),
            json!({"application":"orders"}),
            json!({"application":"orders","file_id":id,"signed":true}),
        ] {
            assert_eq!(
                decode::<FileRequest>(&payload.to_string())
                    .unwrap_err()
                    .refusal,
                Some(FileRefusal::InvalidInput)
            );
        }
        for payload in [
            json!({"application":"orders","file_id":id,"binding":"documents"}),
            json!({"application":"orders","file_id":id,"binding":"documents","exposure":"unknown"}),
            json!({"application":"orders","file_id":id,"binding":"documents","exposure":"public","url":"https://attacker.test"}),
        ] {
            assert!(decode::<ExposeRequest>(&payload.to_string()).is_err());
        }
        assert!(
            decode::<DeleteRequest>(&json!({"application":"orders","file_id":id}).to_string())
                .is_err()
        );
    }

    #[test]
    fn writes_require_the_declared_application_binding_and_exact_generation() {
        let release = release();
        let connection = connection();
        assert!(
            require_binding(
                &release,
                "orders",
                Some("documents"),
                None,
                &connection,
                true
            )
            .is_ok()
        );
        for (application, binding, pinned) in [
            ("other", Some("documents"), None),
            ("orders", Some("undeclared"), None),
            (
                "orders",
                Some("documents"),
                Some((connection.id.as_str(), 2)),
            ),
            (
                "orders",
                Some("documents"),
                Some(("33333333-3333-4333-8333-333333333333", 3)),
            ),
        ] {
            assert!(
                require_binding(&release, application, binding, pinned, &connection, true).is_err()
            );
        }
        let mut read = connection.clone();
        read.read_only = true;
        assert!(require_binding(&release, "orders", Some("documents"), None, &read, true).is_err());
        assert!(
            require_binding(
                &release,
                "orders",
                None,
                Some((&read.id, read.generation)),
                &read,
                false
            )
            .is_ok()
        );
        let mut wrong = connection;
        wrong.name = "another-store".into();
        assert!(
            require_binding(&release, "orders", Some("documents"), None, &wrong, false).is_err()
        );
    }

    #[test]
    fn every_file_operation_requires_a_grant() {
        for action in ["put", "get", "delete", "expose"] {
            assert_eq!(
                principal(&caller(None, false, CredentialType::Session), action)
                    .unwrap_err()
                    .refusal,
                Some(FileRefusal::PermissionDenied)
            );
            assert!(
                principal(
                    &caller(Some(action), false, CredentialType::Session),
                    action
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn private_exposure_refuses_even_after_an_administrator_passes_the_acl() {
        assert!(principal(&caller(None, true, CredentialType::Session), "get").is_ok());
        assert_eq!(
            require_exposure(Exposure::Private).unwrap_err().refusal,
            Some(FileRefusal::Private)
        );
        for exposure in [Exposure::Application, Exposure::Public] {
            assert!(require_exposure(exposure).is_ok());
        }
        assert!(serde_json::from_str::<Exposure>("\"Private\"").is_err());
    }

    #[test]
    fn signed_lifetimes_refuse_zero_and_preserve_shorter_requests() {
        assert!(signed_lifetime(Duration::ZERO).is_err());
        assert_eq!(
            signed_lifetime(Duration::from_secs(1)).unwrap(),
            Duration::from_secs(1)
        );
        assert_eq!(
            signed_lifetime(Duration::from_secs(901)).unwrap(),
            Duration::from_mins(15)
        );
    }

    #[tokio::test]
    async fn native_download_refuses_anonymous_and_session_without_get_before_io() {
        let files = files();
        let routing =
            FlowHttpRouting::new(Some(files.release.clone()), RouteInFlightLimit::default())
                .with_authenticator(Arc::new(SessionWithoutGrant));
        let path = format!("/api/orders/files/{}", Uuid::new_v4());
        for (authorization, status) in [(None, 401), (Some("Bearer session"), 403)] {
            let mut request = hyper::Request::builder().uri(&path);
            if let Some(value) = authorization {
                request = request.header("authorization", value);
            }
            let response = files
                .http(&routing, request.body(WasiBody::default()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
        }
        let upload = hyper::Request::builder()
            .method("POST")
            .uri("/api/orders/files")
            .header("authorization", "Bearer session")
            .body(WasiBody::default())
            .unwrap();
        assert_eq!(
            files
                .http(&routing, upload)
                .await
                .unwrap()
                .status()
                .as_u16(),
            403
        );
        assert!(matches!(
            native_route("/api/orders/files/not-a-uuid"),
            Some(NativeRoute::Invalid)
        ));
    }

    #[tokio::test]
    async fn upload_body_streams_frames_and_propagates_body_failure() {
        let frames = futures_util::stream::iter([
            Ok::<_, wasmtime_wasi_http::Error>(Frame::data(Bytes::from_static(b"first"))),
            Ok(Frame::data(Bytes::from_static(b"second"))),
        ]);
        let mut reader = BodyReader {
            body: StreamBody::new(frames).boxed_unsync(),
            chunk: Bytes::new(),
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"firstsecond");
        let frames = futures_util::stream::iter([Err::<Frame<Bytes>, _>(
            wasmtime_wasi_http::Error::InternalError(Some("interrupted".into())),
        )]);
        let mut reader = BodyReader {
            body: StreamBody::new(frames).boxed_unsync(),
            chunk: Bytes::new(),
        };
        assert!(reader.read_to_end(&mut Vec::new()).await.is_err());
    }

    #[tokio::test]
    async fn upload_body_exceeds_the_old_buffer_ceiling_without_collection() {
        let chunk = Bytes::from(vec![b'x'; 64 * 1024]);
        let frames = futures_util::stream::iter(
            (0..1025).map(move |_| Ok::<_, wasmtime_wasi_http::Error>(Frame::data(chunk.clone()))),
        );
        let mut reader = BodyReader {
            body: StreamBody::new(frames).boxed_unsync(),
            chunk: Bytes::new(),
        };
        let count = tokio::io::copy(&mut reader, &mut tokio::io::sink())
            .await
            .unwrap();
        assert_eq!(count, 1025 * 64 * 1024);
    }

    #[derive(Default)]
    struct Publication {
        calls: std::sync::Mutex<Vec<&'static str>>,
    }
    #[async_trait::async_trait]
    impl FilePublication for Publication {
        async fn publish(&self, _: Uuid) -> Result<String, FileError> {
            self.calls.lock().unwrap().push("publish");
            Ok("https://new.test/file".into())
        }
        async fn unpublish(&self, _: Uuid) -> Result<(), FileError> {
            self.calls.lock().unwrap().push("unpublish");
            Ok(())
        }
    }
    #[tokio::test]
    async fn exposure_transitions_retain_public_urls_and_repeat_origin_deletion() {
        let store = Publication::default();
        let id = Uuid::new_v4();
        let url = change_exposure(&store, id, Exposure::Application, None, Exposure::Public)
            .await
            .unwrap();
        assert_eq!(url.as_deref(), Some("https://new.test/file"));
        let url = change_exposure(
            &store,
            id,
            Exposure::Public,
            Some("https://old.test/file".into()),
            Exposure::Public,
        )
        .await
        .unwrap();
        assert_eq!(url.as_deref(), Some("https://old.test/file"));
        assert!(
            change_exposure(&store, id, Exposure::Public, url, Exposure::Private)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            change_exposure(&store, id, Exposure::Private, None, Exposure::Private)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *store.calls.lock().unwrap(),
            ["publish", "unpublish", "unpublish"]
        );
        assert!(
            change_exposure(&store, id, Exposure::Public, None, Exposure::Public)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn signed_urls_refuse_without_a_bound_operation_before_catalog_access() {
        let files = files();
        let error = files
            .signed_url(
                &files.blobstore,
                "unbound",
                &caller(Some("get"), false, CredentialType::Session),
                Uuid::new_v4(),
                Duration::from_mins(1),
            )
            .await
            .unwrap_err();
        assert_eq!(error.refusal, Some(FileRefusal::PermissionDenied));
    }
    #[test]
    fn every_cookie_session_mutation_requires_csrf() {
        use wamn_engine::flow_http_routing::{RouteCredential, check_csrf, route_credential};
        let release = release();
        for action in ["put", "delete", "expose"] {
            let id = format!("wamn-control-file-{action}-http");
            let attachment = release.manifest().served_attachment(&id).unwrap();
            let headers = [Header {
                name: "cookie".into(),
                value: "__Host-wamn-session=token".into(),
            }];
            let request = AuthenticationRequest {
                manifest: release.manifest(),
                scope: release.scope(),
                attachment_id: &id,
                attachment,
                policy: wamn_catalog::parse_attachment_auth_policy(attachment.auth_policy())
                    .unwrap(),
                headers: &headers,
                body: &[],
            };
            assert!(matches!(
                route_credential(&request).unwrap(),
                RouteCredential::Session {
                    csrf: Some(true),
                    ..
                }
            ));
            assert!(check_csrf(true, Some("signed-value"), &headers).is_err());
        }
    }

    #[tokio::test]
    async fn missing_bindings_refuse_before_connection_lookup() {
        let files = files();
        for (application, binding) in [("orders", "missing"), ("other", "documents")] {
            let error = files
                .store(application, Some(binding), None, true)
                .await
                .unwrap_err();
            assert_eq!(error.refusal, Some(FileRefusal::BindingRefused));
        }
    }
    #[test]
    fn native_body_holds_the_route_permit_until_it_is_dropped() {
        let files = files();
        let limit = RouteInFlightLimit::new(std::num::NonZeroUsize::new(1).unwrap());
        let routing = FlowHttpRouting::new(Some(files.release.clone()), limit);
        let route = "wamn-control-file-get-http";
        let permit = routing.try_acquire_route(route).unwrap();
        let response = hyper::Response::new(PermitBody {
            body: WasiBody::default(),
            _permit: permit,
        });
        assert!(routing.try_acquire_route(route).is_none());
        drop(response);
        assert!(routing.try_acquire_route(route).is_some());
    }
    #[tokio::test]
    async fn signed_urls_refuse_an_unregistered_export_before_catalog_access() {
        use wamn_runtime::plugins::connection_http::{
            ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin, InvocationEntry,
        };
        let files = files();
        let digest = format!("sha256:{}", "a".repeat(64));
        files
            .blobstore
            .bind_invocation(
                "palette",
                ConnectionInvocation {
                    origin: ConnectionOrigin {
                        package_id: "app".into(),
                        component_digest: digest.clone(),
                        component: "files".into(),
                        interface_version: "0.1".into(),
                        operation: "request".into(),
                    },
                    entry: InvocationEntry::Route,
                    package_id: "app".into(),
                    component_digest: digest,
                    component: "files".into(),
                    operation: "request".into(),
                    closure: ConnectionExecutionClosure::Released,
                    effects: None,
                },
            )
            .unwrap();
        let error = files
            .signed_url(
                &files.blobstore,
                "palette",
                &caller(None, true, CredentialType::Session),
                Uuid::new_v4(),
                Duration::from_mins(1),
            )
            .await
            .unwrap_err();
        assert_eq!(error.refusal, Some(FileRefusal::PermissionDenied));
    }
}

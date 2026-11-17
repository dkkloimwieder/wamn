//! Native streaming file routes and operation-only signed reads.
//!
//! Uploads finish staging before the metadata transaction. File reads use
//! the active connection generation. No guest receives a body.
//!
//! POST `/api/<application>/files?binding=<alias>&name=<filename>` sends the raw
//! request body. Its Content-Type supplies the media type. GET
//! `/api/<application>/files/<id>` streams the stored bytes after session admission.

use crate::files::{FileStorageError, FileStorageErrorType, FileStore, MAX_URL_LIFETIME};
use crate::plugin::WamnBlobstore;
use bytes::Bytes;
use futures_util::TryStreamExt as _;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Body as _, Frame};
use sqlx::Row as _;
use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, ReadBuf};
use wasmtime_wasi_http::WasiBody;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use wamn_catalog::application_config::ApplicationConfig;
use wamn_catalog::{HostAttachment, HostHandler};
use wamn_engine::flow_http_routing::{
    AuthenticatedCaller, CredentialType, FlowHttpRouting, Header, RoutePolicy,
};
use wamn_identity_protocol::PrincipalId;
use wamn_sql::WamnPostgres;

const FILE_COLUMNS: &str =
    "id::text, connection_id::text, media_type, size, exposure, url, blob_key";

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
    pub fn code(self) -> &'static str {
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
impl From<sqlx::Error> for FileError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
            == Some("23503")
        {
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
        let refusal = match error.type_ {
            FileStorageErrorType::NotFound => Some(FileRefusal::NotFound),
            FileStorageErrorType::Refused => Some(FileRefusal::BindingRefused),
            FileStorageErrorType::Body => Some(FileRefusal::InvalidInput),
            FileStorageErrorType::Backend | FileStorageErrorType::Deadline => None,
        };
        Self {
            refusal,
            field: "binding",
            source: Some(error.into()),
        }
    }
}

/// One generation selected by the targeted registry reader, never by the caller.
#[derive(Debug, Clone)]
pub struct FileConnection {
    pub id: String,
    pub generation: i64,
    pub definition: Value,
    pub read_only: bool,
}

/// The registry authority needed by native file handlers.
#[async_trait::async_trait]
pub trait FileConnections: Send + Sync + fmt::Debug {
    /// Enumerate immutable store generations using the same targeted registry reader.
    async fn generations(&self) -> Result<Vec<FileConnection>, FileError> {
        Err(failed(
            "collector",
            anyhow::anyhow!("store generation listing unavailable"),
        ))
    }

    /// Resolve an application's binding, or an existing file's active generation.
    async fn lookup(
        &self,
        application: &wamn_catalog::ApplicationName,
        binding: Option<&wamn_catalog::canonical_name::CanonicalName>,
        pinned: Option<&str>,
    ) -> Result<FileConnection, FileError>;
}

/// Login-targeted registry lookup of active store generations.
/// Existing files use their connection identity without a binding.
pub struct RegistryFileConnections {
    pub release: Arc<ApplicationConfig>,
    pub facts: Arc<wamn_connection::connection_facts::ConnectionFacts>,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
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
    async fn generations(&self) -> Result<Vec<FileConnection>, FileError> {
        let mut generations = Vec::new();
        for (_name, id) in self
            .facts
            .env_connections()
            .await
            .map_err(|error| failed("collector", error))?
            .connections
        {
            let Some(answer) = self
                .facts
                .answer(&id)
                .await
                .map_err(|error| failed("collector", error))?
            else {
                continue;
            };
            if answer.fact.status != wamn_connection::connection_memory::ConnectionStatus::Active {
                continue;
            }
            let generation = self
                .facts
                .generation(&id, answer.fact.active_generation)
                .await
                .map_err(|error| failed("collector", error))?;
            if !matches!(generation.integration_type.as_str(), "gcs" | "s3" | "disk") {
                continue;
            }
            let mut definition = generation.parameters.clone();
            definition["provider"] = serde_json::json!(generation.integration_type);
            generations.push(FileConnection {
                id: id.to_string(),
                generation: answer.fact.active_generation,
                definition,
                read_only: false,
            });
        }
        Ok(generations)
    }

    async fn lookup(
        &self,
        application: &wamn_catalog::ApplicationName,
        binding: Option<&wamn_catalog::canonical_name::CanonicalName>,
        pinned: Option<&str>,
    ) -> Result<FileConnection, FileError> {
        if application != &self.release.application {
            return Err(refused(FileRefusal::BindingRefused, "application"));
        }
        let (id, read_only) = match binding {
            None => (
                pinned
                    .and_then(|id| id.parse::<Uuid>().ok())
                    .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection"))?,
                false,
            ),
            Some(name) => {
                let (binding, package) = self
                    .release
                    .components
                    .iter()
                    .find_map(|component| {
                        wamn_catalog::application_config::binding(
                            &self.release,
                            &component.name,
                            name.as_str(),
                        )
                        .filter(|binding| {
                            binding.interface == "wasmcloud:blobstore"
                                && pinned.is_none_or(|id| id == binding.connection_id)
                        })
                        .map(|binding| (binding, &component.package))
                    })
                    .ok_or_else(|| refused(FileRefusal::BindingRefused, "binding"))?;
                (
                    binding
                        .connection_id
                        .parse()
                        .map_err(|error| failed("binding", error))?,
                    binding
                        .mode
                        .read_only(package, &binding.name)
                        .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection mode"))?,
                )
            }
        };
        let answer = self
            .facts
            .answer(&id)
            .await
            .map_err(|error| failed("binding", error))?
            .filter(|answer| {
                answer.fact.status == wamn_connection::connection_memory::ConnectionStatus::Active
            })
            .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection"))?;
        let generation = self
            .facts
            .generation(&id, answer.fact.active_generation)
            .await
            .map_err(|error| failed("binding", error))?;
        // These are native storage providers. A stored file remains readable even
        // when this application has no current declaration for its connection.
        if !matches!(generation.integration_type.as_str(), "gcs" | "s3") {
            return Err(refused(FileRefusal::BindingRefused, "connection"));
        }
        Ok(FileConnection {
            id: id.to_string(),
            generation: answer.fact.active_generation,
            definition: generation.parameters.clone(),
            read_only,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRequest {
    application: wamn_catalog::ApplicationName,
    file_id: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    application: wamn_catalog::ApplicationName,
    file_id: Uuid,
    binding: wamn_catalog::canonical_name::CanonicalName,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposeRequest {
    application: wamn_catalog::ApplicationName,
    file_id: Uuid,
    binding: wamn_catalog::canonical_name::CanonicalName,
    exposure: Exposure,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    binding: wamn_catalog::canonical_name::CanonicalName,
    name: String,
}

/// File handlers of one released application environment.
pub struct FileRoutes {
    pub(crate) release: Arc<ApplicationConfig>,
    pub(crate) context: Arc<wamn_connection::invocation::ReleaseContext>,
    pub(crate) postgres: Arc<WamnPostgres>,
    pub(crate) blobstore: Arc<WamnBlobstore>,
    pub(crate) connections: Arc<dyn FileConnections>,
    pub(crate) project: wamn_registry::ProjectId,
}
impl fmt::Debug for FileRoutes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRoutes")
            .field("application", &self.release.application)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// A session advisory lock holds no database transaction during object I/O.
impl FileRoutes {
    pub fn new(
        context: Arc<wamn_connection::invocation::ReleaseContext>,
        postgres: Arc<WamnPostgres>,
        blobstore: Arc<WamnBlobstore>,
        connections: Arc<dyn FileConnections>,
        project: wamn_registry::ProjectId,
    ) -> Self {
        Self {
            release: Arc::clone(&context.release),
            context,
            postgres,
            blobstore,
            connections,
            project,
        }
    }

    async fn lock(&self, id: Uuid) -> Result<sqlx::PgConnection, FileError> {
        self.postgres
            .lock_file(&self.project, id)
            .await
            .map_err(|error| failed("administration", error))
    }

    async fn store(
        &self,
        application: &wamn_catalog::ApplicationName,
        binding: Option<&wamn_catalog::canonical_name::CanonicalName>,
        pinned: Option<&str>,
        write: bool,
        deadline: tokio::time::Instant,
    ) -> Result<(FileConnection, FileStore), FileError> {
        if application != &self.release.application {
            return Err(refused(FileRefusal::BindingRefused, "application"));
        }
        if binding.is_some_and(|alias| !declares_store(&self.release, alias)) {
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
                &self.context,
                connection
                    .id
                    .parse()
                    .map_err(|error| failed("connection", error))?,
                connection.read_only,
                deadline,
            )
            .await?;
        Ok((connection, store))
    }

    async fn record(
        &self,
        principal: &PrincipalId,
        application: &wamn_catalog::ApplicationName,
        id: Uuid,
        action: &str,
    ) -> Result<FileRecord, FileError> {
        if application != &self.release.application {
            return Err(refused(FileRefusal::NotFound, "file_id"));
        }
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM platform.file WHERE id = $1::text::uuid AND application = $2"
        );
        self.postgres
            .administration_transaction(
                &self.project,
                principal,
                &format!(
                    "wamn-control:file/{action}@{}",
                    wamn_catalog::host_route_version()
                )
                .parse()
                .expect("file operation"),
                async |client| {
                    let row = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
                        .bind(id.to_string())
                        .bind(application)
                        .fetch_optional(&mut *client)
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
        application: &wamn_catalog::ApplicationName,
        binding: &wamn_catalog::canonical_name::CanonicalName,
        name: &str,
        media_type: &str,
        body: impl AsyncRead + Unpin + Send,
    ) -> Result<Uuid, FileError> {
        let deadline = tokio::time::Instant::now() + wamn_engine::engine::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "put")?;
        if name.is_empty() || name.contains(['\0', '\r', '\n']) || !valid_media_type(media_type) {
            return Err(refused(FileRefusal::InvalidInput, "metadata"));
        }
        let (connection, store) = self
            .store(application, Some(binding), None, true, deadline)
            .await?;
        let id = Uuid::new_v4();
        let staged = store.stage(id, body).await?;
        let size = i64::try_from(staged.size).map_err(|error| failed("size", error))?;
        self.postgres.administration_transaction(&self.project, &principal,
            &format!("wamn-control:file/put@{}", wamn_catalog::host_route_version()).parse().expect("file operation"), async |client| {
                sqlx::query("INSERT INTO platform.file (id, application, connection_id, name, media_type, size, sha256, blob_key, exposure) VALUES ($1::text::uuid,$2,$3::text::uuid,$4,$5,$6,$7,$8,'application')").bind(id.to_string()).bind(application).bind(&connection.id).bind(name).bind(media_type).bind(size).bind(staged.sha256).bind(&staged.blob_key).execute(&mut *client).await.map(|result| result.rows_affected())?;
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
        application: &wamn_catalog::ApplicationName,
        id: Uuid,
    ) -> Result<hyper::Response<WasiBody>, FileError> {
        let deadline = tokio::time::Instant::now() + wamn_engine::engine::MAX_HOST_CALL_DURATION;
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
                Some(row.connection_id.as_str()),
                false,
                deadline,
            )
            .await?;
        let body = store
            .get(&row.blob_key)
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
        let deadline = tokio::time::Instant::now() + wamn_engine::engine::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "delete")?;
        let _lock = self.lock(request.file_id).await?;
        let row = self
            .record(&principal, &request.application, request.file_id, "delete")
            .await?;
        let (connection, store) = self
            .store(
                &request.application,
                Some(&request.binding),
                Some(row.connection_id.as_str()),
                true,
                deadline,
            )
            .await?;
        self.postgres
            .administration_transaction(
                &self.project,
                &principal,
                &format!(
                    "wamn-control:file/delete@{}",
                    wamn_catalog::host_route_version()
                )
                .parse()
                .expect("file operation"),
                async |client| {
                    let deleted = sqlx::query(
                        "DELETE FROM platform.file WHERE id = $1::text::uuid AND application = $2",
                    )
                    .bind(request.file_id.to_string())
                    .bind(&request.application)
                    .execute(&mut *client)
                    .await
                    .map(|result| result.rows_affected())?;
                    if deleted != 1 {
                        return Err(refused(FileRefusal::NotFound, "file_id"));
                    }
                    Ok::<_, FileError>(())
                },
            )
            .await
            .map_err(|error| failed("catalog", error))??;
        if let Err(error) = store.delete(&row.blob_key).await {
            // Metadata deletion is committed. The collector (wamn-2wdsf.4)
            // owns orphan cleanup, so this path does not retry object deletion.
            tracing::warn!(%error, file_id = %request.file_id, connection_id = %row.connection_id,
                generation = connection.generation, object_key = %row.blob_key,
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
        let deadline = tokio::time::Instant::now() + wamn_engine::engine::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "expose")?;
        let _lock = self.lock(request.file_id).await?;
        let row = self
            .record(&principal, &request.application, request.file_id, "expose")
            .await?;
        let (connection, store) = self
            .store(
                &request.application,
                Some(&request.binding),
                Some(row.connection_id.as_str()),
                true,
                deadline,
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
            &row.blob_key,
            row.exposure,
            row.url,
            request.exposure,
        )
        .await?;
        let written = self.postgres.administration_transaction(&self.project, &principal,
            &format!("wamn-control:file/expose@{}", wamn_catalog::host_route_version()).parse().expect("file operation"), async |client| {
                let updated = sqlx::query("UPDATE platform.file SET exposure = $3, url = $4 WHERE id = $1::text::uuid AND application = $2 AND (exposure IS DISTINCT FROM $3 OR url IS DISTINCT FROM $4)").bind(request.file_id.to_string()).bind(&request.application).bind(request.exposure.literal()).bind(&url).execute(&mut *client).await.map(|result| result.rows_affected())?;
                Ok::<_, FileError>(updated)
            }).await.map_err(|error| failed("catalog", error))?;
        if let Err(error) = written {
            // A confirmed rollback can remove a newly copied public object.
            // A lost COMMIT answer remains uncertain and reaches the caller above.
            if published && let Err(cleanup) = store.unexpose(&row.blob_key).await {
                tracing::warn!(%cleanup, file_id = %request.file_id, "public copy cleanup after catalog rollback failed");
            }
            return Err(error);
        }
        Ok(json!({"file_id": request.file_id, "exposure": request.exposure, "url": url}))
    }

    /// Sign a file under the operation authority retained by SQL after commit or replay.
    pub async fn signed_url(
        &self,
        principal: &PrincipalId,
        id: Uuid,
        lifetime: Duration,
        deadline: tokio::time::Instant,
    ) -> Result<String, FileError> {
        let lifetime = signed_lifetime(lifetime)?;
        let application = &self.release.application;
        let row = self.record(principal, application, id, "get").await?;
        let (_, store) = self
            .store(application, None, Some(&row.connection_id), false, deadline)
            .await?;
        store
            .signed_url(&row.blob_key, lifetime)
            .await
            .map_err(Into::into)
    }

    pub async fn handle(
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
    pub async fn http(
        &self,
        routing: &FlowHttpRouting,
        request: hyper::Request<WasiBody>,
    ) -> std::ops::ControlFlow<hyper::Response<WasiBody>, hyper::Request<WasiBody>> {
        let Some(route) = native_route(request.uri().path()) else {
            return self.host_http(routing, request).await;
        };
        let result = self.http_request(routing, request, route).await;
        std::ops::ControlFlow::Break(match result {
            Ok(response) => response,
            Err(error) => error_response(&error),
        })
    }

    async fn host_http(
        &self,
        routing: &FlowHttpRouting,
        request: hyper::Request<WasiBody>,
    ) -> std::ops::ControlFlow<hyper::Response<WasiBody>, hyper::Request<WasiBody>> {
        let Some((id, attachment)) = wamn_catalog::HostRouteSet::Application
            .attachments()
            .filter(|(_, attachment)| {
                matches!(
                    attachment.route.handler,
                    HostHandler::FilePut
                        | HostHandler::FileGet
                        | HostHandler::FileDelete
                        | HostHandler::FileExpose
                )
            })
            .find(|(_, attachment)| {
                attachment.definition["route"]["path"].as_str() == Some(request.uri().path())
            })
        else {
            return std::ops::ControlFlow::Continue(request);
        };
        let read = attachment.route.type_.is_read();
        if request.method().as_str() != attachment.route.type_.http_method() {
            return std::ops::ControlFlow::Break(response(
                405,
                &json!({"error":{"code":"method-not-allowed"}}),
            ));
        }
        let headers = request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value.to_str().ok().map(|value| Header {
                    name: name.to_string(),
                    value: value.to_owned(),
                })
            })
            .collect::<Vec<_>>();
        let query = request.uri().query().unwrap_or_default().to_owned();
        let mut body = request.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return std::ops::ControlFlow::Break(response(
                    400,
                    &json!({"error":{"code":"invalid-body"}}),
                ));
            };
            if let Ok(data) = frame.into_data() {
                if bytes.len().saturating_add(data.len()) > 1024 * 1024 {
                    return std::ops::ControlFlow::Break(response(
                        413,
                        &json!({"error":{"code":"body-too-large"}}),
                    ));
                }
                bytes.extend_from_slice(&data);
            }
        }
        let target = wamn_registry::name::project_env_key(
            &self.release.org,
            &self.release.project,
            &self.release.environment,
        );
        let caller = match routing
            .authenticate_policy(
                &target,
                id,
                RoutePolicy {
                    policy: &attachment.auth_policy,
                },
                &headers,
                &bytes,
            )
            .await
        {
            Ok(Some(caller)) => caller,
            Ok(None) => {
                return std::ops::ControlFlow::Break(response(
                    401,
                    &json!({"error":{"code":"unauthorized"}}),
                ));
            }
            Err(error) => {
                return std::ops::ControlFlow::Break(response(
                    error.status,
                    &json!({"error":{"code":error.code}}),
                ));
            }
        };
        let authorized = wamn_engine::native_routes::resolve_authorized_host_route(
            wamn_catalog::HostRouteSet::Application,
            id,
            Some(&caller),
        );
        if !matches!(authorized, Some(Ok(_))) {
            return std::ops::ControlFlow::Break(response(
                403,
                &json!({"error":{"code":"permission-denied"}}),
            ));
        }
        let payload = if read {
            match wamn_catalog::read_query::decode_read_query(&query) {
                Ok(input) => json!([input]),
                Err(_) => {
                    return std::ops::ControlFlow::Break(response(
                        400,
                        &json!({"error":{"code":"invalid-query"}}),
                    ));
                }
            }
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(input) => input,
                Err(_) => {
                    return std::ops::ControlFlow::Break(response(
                        400,
                        &json!({"error":{"code":"malformed-json"}}),
                    ));
                }
            }
        };
        if let Some(schema) = attachment.definition.get("input-schema") {
            let mut schemas = boon::Schemas::new();
            let mut compiler = boon::Compiler::new();
            let location = "https://wamn.invalid/platform-input";
            let valid = compiler.add_resource(location, schema.clone()).is_ok()
                && compiler
                    .compile(location, &mut schemas)
                    .is_ok_and(|index| schemas.validate(&payload, index).is_ok());
            if !valid {
                return std::ops::ControlFlow::Break(response(
                    400,
                    &json!({"error":{"code":"invalid-input"}}),
                ));
            }
        }
        let Some(elements) = payload.as_array().filter(|elements| elements.len() == 1) else {
            return std::ops::ControlFlow::Break(response(
                400,
                &json!({"error":{"code":"invalid-input"}}),
            ));
        };
        let element = &elements[0];
        let input = if read { element } else { &element["value"] };
        let mut outcome = serde_json::Map::new();
        if !read {
            outcome.insert("request_id".into(), element["request_id"].clone());
        }
        match self.handle(attachment, &caller, &input.to_string()).await {
            Ok(value) => {
                outcome.insert("value".into(), value);
            }
            Err(error) if error.refusal.is_some() => {
                outcome.insert("error".into(), json!({"code":error.refusal.expect("declared refusal").code(),"detail":
                    if error.refusal == Some(FileRefusal::PermissionDenied) { json!({"operation":attachment.reference}) }
                    else { json!({"field":error.field}) }}));
            }
            Err(error) => return std::ops::ControlFlow::Break(error_response(&error)),
        }
        std::ops::ControlFlow::Break(response(200, &Value::Array(vec![Value::Object(outcome)])))
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
        let id = format!("wamn-control-file-{action}-http")
            .parse()
            .expect("file attachment");
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
        let caller = match routing
            .authenticate_policy(
                &wamn_registry::name::project_env_key(
                    &self.release.org,
                    &self.release.project,
                    &self.release.environment,
                ),
                &id,
                RoutePolicy {
                    policy: &wamn_catalog::HostRouteSet::Application
                        .attachment(&id)
                        .expect("file attachment")
                        .auth_policy,
                },
                &headers,
                &[],
            )
            .await
        {
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
        Ok(response)
    }
}

async fn change_exposure(
    store: &FileStore,
    blob_key: &str,
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
        Ok(Some(store.expose(blob_key).await?))
    } else {
        // Repeat deletion after a prior failed metadata update as well.
        store.unexpose(blob_key).await?;
        Ok(None)
    }
}

fn principal(caller: &AuthenticatedCaller, action: &str) -> Result<PrincipalId, FileError> {
    if !caller.permits(&format!("wamn-control:file/{action}")) {
        return Err(refused(FileRefusal::PermissionDenied, "operation"));
    }
    Ok(*caller.principal_id())
}

fn declares_store(
    application: &ApplicationConfig,
    alias: &wamn_catalog::canonical_name::CanonicalName,
) -> bool {
    application.components.iter().any(|component| {
        wamn_catalog::application_config::binding(application, &component.name, alias.as_str())
            .is_some_and(|binding| {
                binding.interface == "wasmcloud:blobstore" && binding.selector.name() == Some(alias)
            })
    })
}

fn require_binding(
    release: &ApplicationConfig,
    application: &wamn_catalog::ApplicationName,
    binding: Option<&wamn_catalog::canonical_name::CanonicalName>,
    pinned: Option<&str>,
    connection: &FileConnection,
    write: bool,
) -> Result<(), FileError> {
    if application != &release.application {
        return Err(refused(FileRefusal::BindingRefused, "application"));
    }
    if binding.is_some_and(|alias| !declares_store(release, alias))
        || connection.generation <= 0
        || connection.id.parse::<Uuid>().is_err()
        || write && connection.read_only
        || pinned.is_some_and(|id| id != connection.id)
    {
        return Err(refused(FileRefusal::BindingRefused, "binding"));
    }
    Ok(())
}
fn decode<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, FileError> {
    serde_json::from_str(payload).map_err(|_| refused(FileRefusal::InvalidInput, "input"))
}
fn decode_record(row: &sqlx::postgres::PgRow) -> Result<FileRecord, FileError> {
    let id: String = row.try_get(0)?;
    let exposure: String = row.try_get(4)?;
    let file = FileRecord {
        id: id.parse().map_err(|error| failed("file_id", error))?,
        connection_id: row.try_get(1)?,
        media_type: row.try_get(2)?,
        size: row.try_get(3)?,
        exposure: serde_json::from_value(json!(exposure))
            .map_err(|error| failed("exposure", error))?,
        url: row.try_get(5)?,
        blob_key: row.try_get(6)?,
    };
    if file.size < 0
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
    Upload {
        application: wamn_catalog::ApplicationName,
    },
    Get {
        application: wamn_catalog::ApplicationName,
        id: Uuid,
    },
}
fn native_route(path: &str) -> Option<NativeRoute> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "api", application, "files"] => Some(match application.parse() {
            Ok(application) => NativeRoute::Upload { application },
            Err(_) => NativeRoute::Invalid,
        }),
        ["", "api", application, "files", id] => Some(NativeRoute::Get {
            application: match application.parse() {
                Ok(application) => application,
                Err(_) => return Some(NativeRoute::Invalid),
            },
            id: match id.parse() {
                Ok(id) => id,
                Err(_) => return Some(NativeRoute::Invalid),
            },
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use wamn_catalog::HostRouteSet;
    use wamn_engine::flow_http_routing::CredentialType;
    fn caller(
        action: Option<&str>,
        admin: bool,
        credential: CredentialType,
    ) -> AuthenticatedCaller {
        AuthenticatedCaller::new(
            "wamn-control-file-get-http".parse().unwrap(),
            "22222222-2222-4222-8222-222222222222".parse().unwrap(),
            credential,
            admin,
            action
                .map(|action| format!("wamn-control:file/{action}"))
                .into_iter()
                .collect::<HashSet<_>>(),
            None,
        )
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
    fn every_file_operation_requires_a_grant() {
        for (_, attachment) in HostRouteSet::Application.attachments() {
            let Some(action) = attachment.reference.strip_prefix("wamn-control:file/") else {
                continue;
            };
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
}

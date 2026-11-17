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
use sqlx::Row as _;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use wamn_catalog::application_config::ApplicationConfig;
use wamn_catalog::{HostAttachment, HostHandler};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
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
    pub name: String,
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
        for (name, id) in self
            .facts
            .env_connections()
            .await
            .map_err(|error| failed("collector", error))?
            .connections
        {
            let Some(fact) = self
                .facts
                .connection(&id)
                .await
                .map_err(|error| failed("collector", error))?
            else {
                continue;
            };
            if fact.status != wamn_connection::connection_memory::ConnectionStatus::Active {
                continue;
            }
            let generation = self
                .facts
                .generation(&id, fact.active_generation)
                .await
                .map_err(|error| failed("collector", error))?;
            if generation.runtime_type != "host"
                || !matches!(generation.integration_type.as_str(), "gcs" | "s3" | "disk")
            {
                continue;
            }
            let mut definition = generation.parameters.clone();
            definition["provider"] = serde_json::json!(generation.integration_type);
            generations.push(FileConnection {
                id: id.to_string(),
                name,
                generation: fact.active_generation,
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
        let fact = self
            .facts
            .connection(&id)
            .await
            .map_err(|error| failed("binding", error))?
            .filter(|fact| {
                fact.status == wamn_connection::connection_memory::ConnectionStatus::Active
            })
            .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection"))?;
        let generation = self
            .facts
            .generation(&id, fact.active_generation)
            .await
            .map_err(|error| failed("binding", error))?;
        if generation.runtime_type != "host"
            || !matches!(generation.integration_type.as_str(), "gcs" | "s3")
        {
            return Err(refused(FileRefusal::BindingRefused, "connection"));
        }
        Ok(FileConnection {
            id: id.to_string(),
            name: generation.name.clone(),
            generation: fact.active_generation,
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

/// File handlers of one released application environment.
pub struct FileRoutes {
    pub(crate) release: Arc<ApplicationConfig>,
    pub(crate) context: Arc<wamn_connection::invocation::ReleaseContext>,
    pub(crate) postgres: Arc<WamnPostgres>,
    pub(crate) blobstore: Arc<WamnBlobstore>,
    pub(crate) connections: Arc<dyn FileConnections>,
    pub(crate) project: wamn_registry::ProjectId,
    administration_url: Option<String>,
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
        administration_url: Option<String>,
    ) -> Self {
        Self {
            release: Arc::clone(&context.release),
            context,
            postgres,
            blobstore,
            connections,
            project,
            administration_url,
        }
    }

    async fn lock(&self, id: Uuid) -> Result<sqlx::PgConnection, FileError> {
        let url = self.administration_url.as_deref().ok_or_else(|| {
            failed(
                "administration",
                anyhow::anyhow!("file mutation requires the administration login"),
            )
        })?;
        let mut client = wamn_postgres::connect(url)
            .await
            .map_err(|error| failed("administration", error))?;
        let allowed: bool = sqlx::query("SELECT NOT rolsuper AND pg_has_role(current_user, $1, 'MEMBER') FROM pg_roles WHERE rolname = current_user").bind(wamn_identity_protocol::AuthorityClass::Platform.acl_role()).fetch_one(&mut client).await?.try_get(0)?;
        if !allowed {
            return Err(refused(FileRefusal::PermissionDenied, "administration"));
        }
        sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
            .bind(format!("wamn:file:{id}"))
            .fetch_one(&mut client)
            .await?;
        Ok(client)
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
                &connection.name,
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
        let deadline = invocations
            .invocation_deadline(component_id)
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        let invocation = invocations
            .invocation(component_id)
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        if !matches!(
            invocation.closure,
            wamn_connection::connection::ConnectionExecutionClosure::Released
        ) {
            return Err(refused(FileRefusal::PermissionDenied, "operation"));
        }
        let application = &self.release.application;
        let served = self
            .release
            .components
            .iter()
            .find(|component| {
                component.package == invocation.package_id
                    && component.digest == invocation.component_digest
            })
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        let operation = served
            .operations
            .get(&invocation.operation)
            .filter(|operation| operation.public)
            .ok_or_else(|| refused(FileRefusal::PermissionDenied, "operation"))?;
        if !caller.permits(invocation.operation.as_str()) || operation.signed_result.is_none() {
            return Err(refused(FileRefusal::PermissionDenied, "operation"));
        }
        let principal = caller.principal_id();
        let row = self.record(principal, application, id, "get").await?;
        let pin = Some(row.connection_id.as_str());
        let connection = self.connections.lookup(application, None, pin).await?;
        require_binding(&self.release, application, None, pin, &connection, false)?;
        // Signing is the host's declared result work. The operation grant
        // supplies read authority without a guest blobstore import. The
        // registry requires an active connection and uses its active generation.
        // Reads do not require a current application binding.
        let store = self
            .blobstore
            .file_store(
                &self.context,
                &connection.name,
                connection.read_only,
                deadline,
            )
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

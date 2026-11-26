//! Native streaming file routes and operation-only signed reads.
//!
//! Uploads finish staging before the metadata transaction. File reads use
//! the active connection generation. No guest receives a body.
//!
//! POST `/api/<app>/files?binding=<alias>&name=<filename>` sends the raw
//! request body. Its Content-Type supplies the media type. GET
//! `/api/<app>/files/<id>` streams the stored bytes after session admission.

use bytes::Bytes;
use futures_util::TryStreamExt as _;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Body as _, Frame};
use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, ReadBuf};
use wamn_routes::HostAttachment;
use wamn_routes::HostHandler;
use wasmtime_wasi_http::WasiBody;

use serde_json::{Value, json};
use uuid::Uuid;
use wamn_app::app_config::AppConfig;
use wamn_app::ingress::{AuthenticatedCaller, CredentialType, Header, RoutePolicy};
use wamn_blobstore::client::ObjectStorage;
use wamn_blobstore::file_route::{
    DeleteRequest, ExposeRequest, Exposure, FileConnection, FileError, FileExposure, FileInsert,
    FileRecord, FileRefusal, FileRequest, UploadQuery, failed, refused, valid_media_type,
};
use wamn_blobstore::files::{MAX_URL_LIFETIME, StoredFiles};
use wamn_engine::ingress::IngressRouting;
use wamn_identity_protocol::PrincipalId;
use wamn_package::canonical_name::CanonicalName;
use wamn_postgres::WamnPostgres;

use crate::file_catalog;
use crate::file_connections::RegistryFileConnections;

/// File handlers of one released application environment.
pub struct FileRoutes {
    pub(crate) release: Arc<AppConfig>,
    pub(crate) context: Arc<wamn_connection::invocation::ReleaseContext>,
    pub(crate) database: Arc<WamnPostgres>,
    pub(crate) storage: Arc<ObjectStorage>,
    pub(crate) connections: Arc<RegistryFileConnections>,
    pub(crate) project: wamn_registry::ProjectId,
}
impl fmt::Debug for FileRoutes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRoutes")
            .field("app", &self.release.app)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// A session advisory lock holds no database transaction during object I/O.
impl FileRoutes {
    pub fn new(
        context: Arc<wamn_connection::invocation::ReleaseContext>,
        database: Arc<WamnPostgres>,
        storage: Arc<ObjectStorage>,
        connections: Arc<RegistryFileConnections>,
        project: wamn_registry::ProjectId,
    ) -> Self {
        Self {
            release: Arc::clone(&context.release),
            context,
            database,
            storage,
            connections,
            project,
        }
    }

    async fn store(
        &self,
        app: &wamn_registry::AppName,
        binding: Option<&wamn_package::canonical_name::CanonicalName>,
        pinned: Option<&str>,
        write: bool,
        deadline: tokio::time::Instant,
    ) -> Result<(FileConnection, StoredFiles), FileError> {
        if app != &self.release.app {
            return Err(refused(FileRefusal::BindingRefused, "app"));
        }
        if binding.is_some_and(|alias| !declares_store(&self.release, alias)) {
            return Err(refused(FileRefusal::BindingRefused, "binding"));
        }
        let connection = self.connections.lookup(app, binding, pinned).await?;
        require_binding(&self.release, app, binding, pinned, &connection, write)?;
        let store = self
            .storage
            .open_files(
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
        app: &wamn_registry::AppName,
        id: Uuid,
        action: &str,
    ) -> Result<FileRecord, FileError> {
        if app != &self.release.app {
            return Err(refused(FileRefusal::NotFound, "file_id"));
        }
        file_catalog::record(
            &self.database,
            &self.project,
            principal,
            &file_operation(action),
            app,
            id,
        )
        .await
    }

    /// Stage bytes, commit one row, then promote staging to the live key.
    pub async fn put(
        &self,
        caller: &AuthenticatedCaller,
        app: &wamn_registry::AppName,
        binding: &wamn_package::canonical_name::CanonicalName,
        name: &str,
        media_type: &str,
        mut body: impl AsyncRead + Unpin + Send,
    ) -> Result<Uuid, FileError> {
        let deadline = tokio::time::Instant::now() + wamn_app::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "put")?;
        if name.is_empty() || name.contains(['\0', '\r', '\n']) || !valid_media_type(media_type) {
            return Err(refused(FileRefusal::InvalidInput, "metadata"));
        }
        let (connection, store) = self.store(app, Some(binding), None, true, deadline).await?;
        let id = Uuid::new_v4();
        let staged = store.stage(id, &mut body).await?;
        let size = i64::try_from(staged.size).map_err(|error| failed("size", error))?;
        file_catalog::insert(
            &self.database,
            &self.project,
            &principal,
            &file_operation("put"),
            FileInsert {
                id,
                app,
                connection_id: &connection.id,
                name,
                media_type,
                size,
                sha256: &staged.sha256,
                blob_key: &staged.blob_key,
            },
        )
        .await?;
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
        app: &wamn_registry::AppName,
        id: Uuid,
    ) -> Result<hyper::Response<WasiBody>, FileError> {
        let deadline = tokio::time::Instant::now() + wamn_app::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "get")?;
        if caller.credential_type() != CredentialType::Session {
            return Err(refused(FileRefusal::PermissionDenied, "session"));
        }
        let row = self.record(&principal, app, id, "get").await?;
        require_exposure(row.exposure)?;
        let (_, store) = self
            .store(app, None, Some(row.connection_id.as_str()), false, deadline)
            .await?;
        let body = store
            .get(&row.blob_key)
            .await?
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
        let deadline = tokio::time::Instant::now() + wamn_app::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "delete")?;
        let _lock = file_catalog::lock(&self.database, &self.project, request.file_id).await?;
        let row = self
            .record(&principal, &request.app, request.file_id, "delete")
            .await?;
        let (connection, store) = self
            .store(
                &request.app,
                Some(&request.binding),
                Some(row.connection_id.as_str()),
                true,
                deadline,
            )
            .await?;
        file_catalog::delete(
            &self.database,
            &self.project,
            &principal,
            &file_operation("delete"),
            &request.app,
            request.file_id,
        )
        .await?;
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
        let deadline = tokio::time::Instant::now() + wamn_app::MAX_HOST_CALL_DURATION;
        let principal = principal(caller, "expose")?;
        let _lock = file_catalog::lock(&self.database, &self.project, request.file_id).await?;
        let row = self
            .record(&principal, &request.app, request.file_id, "expose")
            .await?;
        let (connection, store) = self
            .store(
                &request.app,
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
        let written = file_catalog::set_exposure(
            &self.database,
            &self.project,
            &principal,
            &file_operation("expose"),
            FileExposure {
                app: &request.app,
                id: request.file_id,
                exposure: request.exposure,
                url: url.as_deref(),
            },
        )
        .await?;
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
        let app = &self.release.app;
        let row = self.record(principal, app, id, "get").await?;
        let (_, store) = self
            .store(app, None, Some(&row.connection_id), false, deadline)
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
                    .record(&principal, &request.app, request.file_id, "get")
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
        routing: &IngressRouting,
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
        routing: &IngressRouting,
        request: hyper::Request<WasiBody>,
    ) -> std::ops::ControlFlow<hyper::Response<WasiBody>, hyper::Request<WasiBody>> {
        let Some((id, attachment)) = wamn_routes::HostRouteSet::App
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
            &self.release.env,
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
        let authorized = wamn_app::native_routes::resolve_authorized_host_route(
            wamn_routes::HostRouteSet::App,
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
            match wamn_routes::read_query::decode_read_query(&query) {
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
        routing: &IngressRouting,
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
                    &self.release.env,
                ),
                &id,
                RoutePolicy {
                    policy: &wamn_routes::HostRouteSet::App
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
            NativeRoute::Get { app, id } => self.get(&caller, &app, id).await,
            NativeRoute::Upload { app } => {
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
                        &app,
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
    store: &StoredFiles,
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

fn file_operation(action: &str) -> CanonicalName {
    format!(
        "wamn-control:file/{action}@{}",
        wamn_routes::host_route_version()
    )
    .parse()
    .expect("file operation")
}

fn principal(caller: &AuthenticatedCaller, action: &str) -> Result<PrincipalId, FileError> {
    if !caller.permits(&format!("wamn-control:file/{action}")) {
        return Err(refused(FileRefusal::PermissionDenied, "operation"));
    }
    Ok(*caller.principal_id())
}

fn declares_store(app: &AppConfig, alias: &wamn_package::canonical_name::CanonicalName) -> bool {
    app.components.iter().any(|component| {
        wamn_app::app_config::binding(app, &component.name, alias.as_str()).is_some_and(|binding| {
            binding.interface == "wasmcloud:blobstore" && binding.selector.name() == Some(alias)
        })
    })
}

fn require_binding(
    release: &AppConfig,
    app: &wamn_registry::AppName,
    binding: Option<&wamn_package::canonical_name::CanonicalName>,
    pinned: Option<&str>,
    connection: &FileConnection,
    write: bool,
) -> Result<(), FileError> {
    if app != &release.app {
        return Err(refused(FileRefusal::BindingRefused, "app"));
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
        app: wamn_registry::AppName,
    },
    Get {
        app: wamn_registry::AppName,
        id: Uuid,
    },
}
fn native_route(path: &str) -> Option<NativeRoute> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "api", app, "files"] => Some(match app.parse() {
            Ok(app) => NativeRoute::Upload { app },
            Err(_) => NativeRoute::Invalid,
        }),
        ["", "api", app, "files", id] => Some(NativeRoute::Get {
            app: match app.parse() {
                Ok(app) => app,
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
    use wamn_routes::HostRouteSet;
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
        assert!(decode::<FileRequest>(&json!({"app":"orders","file_id":id}).to_string()).is_ok());
        for payload in [
            json!({"file_id":id}),
            json!({"app":"orders"}),
            json!({"app":"orders","file_id":id,"signed":true}),
        ] {
            assert_eq!(
                decode::<FileRequest>(&payload.to_string())
                    .unwrap_err()
                    .refusal,
                Some(FileRefusal::InvalidInput)
            );
        }
        for payload in [
            json!({"app":"orders","file_id":id,"binding":"documents"}),
            json!({"app":"orders","file_id":id,"binding":"documents","exposure":"unknown"}),
            json!({"app":"orders","file_id":id,"binding":"documents","exposure":"public","url":"https://attacker.test"}),
        ] {
            assert!(decode::<ExposeRequest>(&payload.to_string()).is_err());
        }
        assert!(
            decode::<DeleteRequest>(&json!({"app":"orders","file_id":id}).to_string()).is_err()
        );
    }
    #[test]
    fn every_file_operation_requires_a_grant() {
        for (_, attachment) in HostRouteSet::App.attachments() {
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
        for exposure in [Exposure::App, Exposure::Public] {
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

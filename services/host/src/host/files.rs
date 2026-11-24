//! Native file handlers and result signing for one resolved application workload.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use wamn_application::application_config::ApplicationConfig;
use wamn_application::result_signing::{ResultSigner, ResultSigning, ResultSigningError};
use wamn_blobstore::client::ObjectStorage;
use wamn_connection::connection_facts::ConnectionCache;
use wamn_connection::invocation::ReleaseContext;
use wamn_package::{SignedFileField, SignedFileResultClass};
use wamn_plugin::OperationHost;
use wamn_plugin::blobstore::file_route::FileRoutes;
use wamn_plugin::file_connections::RegistryFileConnections;
use wamn_postgres::WamnPostgres;
use wamn_postgres::authorization::Authorization;

pub(super) struct Routes {
    pub(super) files: FileRoutes,
    pub(super) application: super::host_route::HostRouteDelivery,
}

struct ApplicationFiles {
    application: Arc<ApplicationConfig>,
    routes: tokio::sync::OnceCell<Arc<Routes>>,
}

pub(super) struct Files {
    postgres: Weak<OperationHost>,
    database: Arc<WamnPostgres>,
    storage: Arc<ObjectStorage>,
    authorization: Arc<Authorization>,
    applications: RwLock<HashMap<String, Arc<ApplicationFiles>>>,
}

impl std::fmt::Debug for Files {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Files").finish_non_exhaustive()
    }
}

impl Files {
    pub(super) fn new(
        postgres: &Arc<OperationHost>,
        database: Arc<WamnPostgres>,
        storage: Arc<ObjectStorage>,
        authorization: Arc<Authorization>,
    ) -> Self {
        Self {
            postgres: Arc::downgrade(postgres),
            database,
            storage,
            authorization,
            applications: RwLock::default(),
        }
    }

    pub(super) fn register(&self, workload: &str, application: Arc<ApplicationConfig>) {
        self.applications
            .write()
            .expect("file applications lock poisoned")
            .insert(
                workload.to_owned(),
                Arc::new(ApplicationFiles {
                    application,
                    routes: tokio::sync::OnceCell::new(),
                }),
            );
    }

    pub(super) fn unregister(&self, workload: &str) {
        self.applications
            .write()
            .expect("file applications lock poisoned")
            .remove(workload);
    }

    pub(super) async fn routes(&self, workload: &str) -> anyhow::Result<Arc<Routes>> {
        let state = self
            .applications
            .read()
            .expect("file applications lock poisoned")
            .get(workload)
            .map(Arc::clone)
            .context("application file routes are unbound")?;
        state
            .routes
            .get_or_try_init(|| async {
                let postgres = self
                    .postgres
                    .upgrade()
                    .context("file database was unloaded")?;
                let application = &state.application;
                let facts = Arc::clone(&self.authorization).facts(Arc::clone(application))?;
                let context = Arc::new(ReleaseContext::new(
                    Arc::clone(application),
                    Arc::clone(&facts),
                )?);
                let connections = Arc::new(RegistryFileConnections {
                    release: Arc::clone(application),
                    facts,
                });
                let application_routes = super::host_route::HostRouteDelivery::new(
                    postgres,
                    Arc::clone(&self.authorization),
                    Arc::clone(application),
                );
                Ok(Arc::new(Routes {
                    files: FileRoutes::new(
                        context,
                        Arc::clone(&self.database),
                        Arc::clone(&self.storage),
                        connections,
                        application.project.clone(),
                    ),
                    application: application_routes,
                }))
            })
            .await
            .map(Arc::clone)
    }
}

#[async_trait::async_trait]
impl ResultSigner for Files {
    async fn sign(&self, request: ResultSigning<'_>) -> Result<String, ResultSigningError> {
        let deadline = tokio::time::Instant::from_std(request.deadline);
        let result = async {
            let caller = request
                .caller
                .filter(|caller| caller.permits(request.operation.as_str()))
                .context("signed result has no operation authority")?;
            let routes = self.routes(request.workload).await?;
            let mut payload: Value = serde_json::from_str(&request.payload)
                .context("decode operation result for file signing")?;
            for field in &request.reads.fields {
                let paths = json_paths(&payload, request.reads.class, field)?;
                let lifetime = Duration::from_secs(field.read.lifetime_seconds()?);
                for path in paths {
                    let target = payload
                        .pointer_mut(&path)
                        .context("signed result field disappeared")?;
                    let id = target
                        .as_str()
                        .context("signed result field is not a file UUID")?
                        .parse()?;
                    *target = Value::String(
                        routes
                            .files
                            .signed_url(caller.principal_id(), id, lifetime, deadline)
                            .await?,
                    );
                }
            }
            serde_json::to_string(&payload).context("encode signed operation result")
        };
        let outcome = tokio::time::timeout_at(deadline, result)
            .await
            .map_err(anyhow::Error::from)
            .and_then(std::convert::identity);
        match outcome {
            Ok(payload) => Ok(payload),
            Err(error) => {
                tracing::warn!(%error, operation = %request.operation, "file result signing failed");
                Err(ResultSigningError {
                    code: Some("result_signing_failed".into()),
                    message: "file result signing failed".into(),
                })
            }
        }
    }
}

fn json_paths(
    value: &Value,
    class: SignedFileResultClass,
    field: &SignedFileField,
) -> anyhow::Result<Vec<String>> {
    let mut paths = Vec::new();
    for (index, element) in value
        .as_array()
        .context("operation result is not an outcome list")?
        .iter()
        .enumerate()
    {
        if let Some(error) = element.get("error") {
            anyhow::ensure!(
                error.is_object() && element.get("value").is_none(),
                "operation result mixes a value with a refusal"
            );
            continue;
        }
        let row = element
            .get("value")
            .context("operation outcome has no value")?;
        let base = format!("/{index}/value");
        match class {
            SignedFileResultClass::One => {
                json_field(row, &base, &field.path, field.nullable, &mut paths)?;
            }
            SignedFileResultClass::OptionalOne => {
                if !row.is_null() {
                    json_field(row, &base, &field.path, field.nullable, &mut paths)?;
                }
            }
            SignedFileResultClass::BoundedList | SignedFileResultClass::Page => {
                for (i, row) in row
                    .get("rows")
                    .and_then(Value::as_array)
                    .context("signed result has no rows")?
                    .iter()
                    .enumerate()
                {
                    json_field(
                        row,
                        &format!("{base}/rows/{i}"),
                        &field.path,
                        field.nullable,
                        &mut paths,
                    )?;
                }
            }
        }
    }
    Ok(paths)
}

fn json_field(
    value: &Value,
    base: &str,
    path: &str,
    nullable: bool,
    paths: &mut Vec<String>,
) -> anyhow::Result<()> {
    if path.is_empty() {
        if !value.is_null() || !nullable {
            paths.push(base.into());
        }
        return Ok(());
    }
    let (member, rest) = path.split_once('.').unwrap_or((path, ""));
    let name = member.strip_suffix("[]").unwrap_or(member);
    let value = value.get(name).context("signed result field is missing")?;
    let base = format!("{base}/{name}");
    if member.ends_with("[]") {
        for (i, element) in value
            .as_array()
            .context("signed result field is not a list")?
            .iter()
            .enumerate()
        {
            json_field(element, &format!("{base}/{i}"), rest, nullable, paths)?;
        }
    } else {
        json_field(value, &base, rest, nullable, paths)?;
    }
    Ok(())
}

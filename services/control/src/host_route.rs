//! Host-run routes (docs/architecture/execution.md#host-run-routes).
//!
//! A host route keeps the route table, credentials, CSRF treatment and
//! operation grant of any route, but the host serves it with a fixed handler
//! instead of a component. [`HostRouteDelivery`] serves the native platform
//! routes of the loaded release.

use std::fmt;
use std::sync::Arc;
use wamn_identity::identity::control::{control_projects_page, is_org_admin};
use wamn_routes::HostAttachment;
use wamn_routes::HostHandler;
use wamn_routes::HostRouteSet;
use wamn_routes::host_wire::{ControlMineResult, ControlMineResultProject};

use serde_json::{Value, json};
use wamn_app::ingress::AuthenticatedCaller;
use wamn_app::native_routes::{
    DeliveryError, DeliveryOutcome, DeliveryReport, DeliveryRequest, Source,
    resolve_authorized_host_route,
};
use wamn_authentication::paging::Order;
use wamn_identity_client::users::UserClient;

use crate::Writer;
use crate::control_route::{ControlRoutes, Refusal, control_page, oauth_authorization};
use crate::execution::Execution;

#[cfg(test)]
mod tests;

/// The control routes of one org, through its `control` login.
pub struct HostRouteHandlers {
    /// The `control` login for reads.
    pub control: sqlx::PgPool,
    /// A second `control` login that holds one write transaction at a
    /// time, so a read never runs inside a write.
    pub writer: Writer,
    /// The identity adapter for user management.
    pub identity: Option<UserClient>,
    /// The configured identity issuer address used for project user rows.
    pub issuer: String,
    /// The `wamn_platform` login of each project environment.
    pub(crate) logins: Arc<crate::project_login::ProjectLogins>,
    pub org: wamn_registry::OrgId,
}

impl fmt::Debug for HostRouteHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Control")
            .field("org", &self.org)
            .finish_non_exhaustive()
    }
}

/// Serves the native Control route set.
pub struct HostRouteDelivery {
    registry_token_metadata: bool,
    operation_jobs: Option<wamn_kubernetes::kubernetes_api::KubernetesApi>,
    worker: Option<wamn_kubernetes::job::Worker>,
    execution: Option<Execution>,
    handlers: HostRouteHandlers,
    connection_health: Option<Arc<wamn_http::connection_health::ConnectionHealth>>,
    oauth: Option<Arc<oauth_authorization::Authorization>>,
    control_authenticator:
        Option<Arc<wamn_authentication::route_authentication::ControlRouteAuthenticator>>,
}

impl fmt::Debug for HostRouteDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRouteDelivery")
            .field("handlers", &self.handlers)
            .finish_non_exhaustive()
    }
}

impl HostRouteDelivery {
    /// Serve the native Control route set.
    pub fn new(handlers: HostRouteHandlers) -> Self {
        Self {
            handlers,
            operation_jobs: None,
            worker: None,
            execution: None,
            registry_token_metadata: false,
            connection_health: None,
            oauth: None,
            control_authenticator: None,
        }
    }

    /// Authenticate the native control endpoint with its org session authority.
    pub fn with_control_authenticator(
        mut self,
        authenticator: Arc<wamn_authentication::route_authentication::ControlRouteAuthenticator>,
    ) -> Self {
        self.control_authenticator = Some(authenticator);
        self
    }

    /// Use the process-owned artifact and transport resources for candidate tests.
    pub fn with_connection_health(
        mut self,
        health: Arc<wamn_http::connection_health::ConnectionHealth>,
    ) -> Self {
        self.connection_health = Some(health);
        self
    }

    /// Share the control callback configuration with the administration routes.
    pub(crate) fn with_oauth(
        mut self,
        oauth: Option<Arc<oauth_authorization::Authorization>>,
    ) -> Self {
        self.oauth = oauth;
        self
    }

    /// Carry the host's registry credential choice into operation Jobs.
    pub fn with_registry_token_metadata(mut self, enabled: bool) -> Self {
        self.registry_token_metadata = enabled;
        self
    }

    /// Hand authenticated service operations to their native owner.
    pub(crate) fn with_control_execution(mut self, execution: Execution) -> Self {
        self.execution = Some(execution);
        self
    }

    /// The installer supplies one pinned worker image and component configuration.
    pub fn with_worker(mut self, worker: wamn_kubernetes::job::Worker) -> Self {
        self.worker = Some(worker);
        self
    }

    /// Use the process-owned Kubernetes client for operation Jobs.
    pub fn with_operation_jobs(
        mut self,
        api: wamn_kubernetes::kubernetes_api::KubernetesApi,
    ) -> Self {
        self.operation_jobs = Some(api);
        self
    }

    /// Serve an authenticated Control operation.
    async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<serde_json::Value, Refusal> {
        let principal = caller.principal_id();
        let HostRouteHandlers {
            control,
            writer,
            identity,
            issuer,
            logins,
            org,
        } = &self.handlers;
        match attachment.route.handler {
            HostHandler::ControlMine => {
                let mut transaction =
                    wamn_identity::identity::transaction::read_pool(control, principal).await?;
                let org_admin = is_org_admin(&mut transaction, principal, org).await?;
                let page = control_page(payload, attachment, principal, org, None, Order::Project)?;
                let projects = control_projects_page(
                    &mut transaction,
                    principal,
                    org,
                    page.after(0)
                        .map(str::parse::<wamn_registry::ProjectId>)
                        .transpose()
                        .map_err(anyhow::Error::from)?
                        .as_ref(),
                    page.read_limit(),
                )
                .await?;
                let mut answer = page.answer("projects", projects, |project| json!([project]));
                let Value::Array(projects) = answer["projects"].take() else {
                    unreachable!("page rows are an array")
                };
                let mut rows = Vec::with_capacity(projects.len());
                for project in projects {
                    let Value::String(project) = project else {
                        unreachable!("project identifiers serialize to strings")
                    };
                    let project_admin = org_admin
                        || wamn_identity::identity::control::is_project_admin(
                            &mut transaction,
                            principal,
                            org,
                            &project.parse().map_err(anyhow::Error::from)?,
                        )
                        .await?;
                    rows.push(ControlMineResultProject {
                        project,
                        project_admin,
                    });
                }
                let result = ControlMineResult {
                    org_admin,
                    projects: rows,
                    next_cursor: serde_json::from_value(answer["next_cursor"].take())
                        .map_err(anyhow::Error::from)?,
                    total: serde_json::from_value(answer["total"].take())
                        .map_err(anyhow::Error::from)?,
                };
                transaction.commit().await?;
                Ok(serde_json::to_value(result).map_err(anyhow::Error::from)?)
            }
            _ => {
                ControlRoutes {
                    control,
                    writer,
                    identity: identity.as_ref(),
                    issuer,
                    logins,
                    operation_jobs: self.operation_jobs.as_ref(),
                    worker: self.worker.as_ref(),
                    execution: self.execution.as_ref(),
                    registry_token_metadata: self.registry_token_metadata,
                    org,
                    connection_health: self.connection_health.as_deref(),
                    oauth: self.oauth.as_deref(),
                }
                .handle(attachment, principal, payload)
                .await
            }
        }
    }
}

/// The one request element of a host route request: its request id and its input. A
/// read carries its input as the request element, and a write carries a request id and
/// its input under `value`, as every generated route takes them. Any other
/// payload is `None`.
fn request_element(payload: &str, read: bool) -> Option<(Option<String>, Value)> {
    let Ok(Value::Array(request_elements)) = serde_json::from_str::<Value>(payload) else {
        return None;
    };
    let [Value::Object(mut request_element)] = <[Value; 1]>::try_from(request_elements).ok()?
    else {
        return None;
    };
    if read {
        return Some((None, Value::Object(request_element)));
    }
    let Some(Value::String(request_id)) = request_element.remove("request_id") else {
        return None;
    };
    let value = request_element
        .remove("value")
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    (request_element.is_empty() && !request_id.is_empty() && value.is_object())
        .then_some((Some(request_id), value))
}

fn report(outcome: Result<DeliveryOutcome, DeliveryError>) -> DeliveryReport {
    DeliveryReport {
        outcome,
        deadline_adjustments: Vec::new(),
        etag: None,
    }
}

impl HostRouteDelivery {
    pub async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let resolved = match &request.source {
            Source::Attachment(id) => {
                resolve_authorized_host_route(HostRouteSet::Control, id, caller.as_ref())
            }
            Source::Registration(_) => None,
        };
        let Some(resolved) = resolved else {
            return report(Err(DeliveryError::SourceNotFound));
        };
        let attachment = match resolved {
            Ok(attachment) => attachment,
            Err(error) => return report(Err(error)),
        };
        if request.delivery_id.is_empty() {
            return report(Err(DeliveryError::InvalidRequest));
        }
        let caller = caller.expect("a resolved host route has its caller");
        let read = attachment.route.type_.is_read();
        let Some((request_id, input)) = request_element(&request.payload, read) else {
            return report(Err(DeliveryError::InvalidPayload));
        };
        // The answer is the one outcome of the one request element, as a generated
        // route answers. A read carries no request id and echoes none.
        let outcome = |member: &str, value: Value| {
            let mut request_element = serde_json::Map::new();
            if let Some(request_id) = &request_id {
                request_element.insert("request_id".to_owned(), json!(request_id));
            }
            request_element.insert(member.to_owned(), value);
            report(Ok(
                Value::Array(vec![Value::Object(request_element)]).to_string()
            ))
        };
        match self.handle(attachment, &caller, &input.to_string()).await {
            Ok(result) => outcome("value", result),
            Err(Refusal::Declared { code, detail }) => {
                let mut error = serde_json::Map::new();
                error.insert("code".to_owned(), json!(code));
                error.insert("detail".to_owned(), detail);
                outcome("error", Value::Object(error))
            }
            Err(Refusal::Delivery(error)) => report(Err(error)),
            Err(Refusal::Failed(error)) => {
                tracing::warn!(
                    error = %error,
                    reference = attachment.reference,
                    "host route failed"
                );
                report(Err(DeliveryError::ExecutionFailed))
            }
        }
    }
}

impl HostRouteDelivery {
    /// Serve a platform endpoint through native ingress.
    pub async fn http(
        &self,
        request: hyper::Request<wamn_http::body::HttpBody>,
    ) -> hyper::Response<wamn_http::body::HttpBody> {
        use wamn_http::body::{ReadError, empty_response, json_response, read_limited};

        if request.uri().path() == wamn_platform_install::provision_org::FIRST_OWNER_PATH {
            let status =
                crate::first_owner::respond(&self.handlers, self.operation_jobs.as_ref(), request)
                    .await;
            return empty_response(status);
        }
        let Some((id, attachment)) = HostRouteSet::Control.attachments().find(|(_, attachment)| {
            attachment.definition["route"]["path"].as_str() == Some(request.uri().path())
        }) else {
            return json_response(404, &json!({"error":{"code":"route-not-found"}}));
        };
        if attachment.route.type_.http_method() != request.method().as_str() {
            return json_response(405, &json!({"error":{"code":"method-not-allowed"}}));
        }
        let headers = request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value.to_str().ok().map(|value| wamn_app::ingress::Header {
                    name: name.to_string(),
                    value: value.to_owned(),
                })
            })
            .collect::<Vec<_>>();
        let query = request.uri().query().unwrap_or_default().to_owned();
        let read = attachment.route.type_.is_read();
        let maximum = attachment.definition["raw-body-bytes"]["maximum"]
            .as_u64()
            .unwrap_or(1024 * 1024);
        let bytes = match read_limited(request.into_body(), maximum).await {
            Ok(bytes) => bytes,
            Err(ReadError::Invalid(_)) => {
                return json_response(400, &json!({"error":{"code":"invalid-body"}}));
            }
            Err(ReadError::TooLarge) => {
                return json_response(413, &json!({"error":{"code":"body-too-large"}}));
            }
        };
        let authentication = match &self.control_authenticator {
            Some(authenticator) => authenticator.authenticate_control(id, &headers).await,
            None => Err(wamn_app::ingress::authentication_unavailable()),
        };
        let caller = match authentication {
            Ok(caller) => Some(caller),
            Err(error) => {
                return json_response(error.status, &json!({"error":{"code":error.code}}));
            }
        };
        let payload = if read {
            let parsed = wamn_routes::read_query::decode_read_query(&query);
            match parsed {
                Ok(input) => json!([input]),
                Err(_) => return json_response(400, &json!({"error":{"code":"invalid-query"}})),
            }
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(input) => input,
                Err(_) => return json_response(400, &json!({"error":{"code":"malformed-json"}})),
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
                return json_response(400, &json!({"error":{"code":"invalid-input"}}));
            }
        }
        let report = self
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(id.to_owned()),
                    caller: None,
                    delivery_id: uuid::Uuid::new_v4().to_string(),
                    payload: payload.to_string(),
                    trace: None,
                    parent_causation: None,
                    if_none_match: None,
                },
                caller,
            )
            .await;
        match report.outcome {
            Ok(body) => match serde_json::from_str(&body) {
                Ok(body) => json_response(200, &body),
                Err(_) => json_response(503, &json!({"error":{"code":"execution-failed"}})),
            },
            Err(
                DeliveryError::PermissionDenied(denial)
                | DeliveryError::FreshCredentialRequired(denial),
            ) => json_response(
                403,
                &json!({"error":{"code":"permission-denied", "operation": denial.operation}}),
            ),
            _ => json_response(503, &json!({"error":{"code":"execution-failed"}})),
        }
    }
}

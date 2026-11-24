//! Application host routes served on the workload's native ingress.

use std::collections::BTreeSet;
use std::sync::Arc;
use wamn_routes::HostAttachment;
use wamn_routes::HostHandler;
use wamn_routes::HostRouteSet;

use serde_json::{Value, json};
use wamn_application::application_config::ApplicationConfig;
use wamn_application::ingress::AuthenticatedCaller;
use wamn_application::native_routes::{
    DeliveryError, DeliveryOutcome, DeliveryReport, DeliveryRequest, Source,
    resolve_authorized_host_route,
};
use wamn_authentication::authorization::Authorization;
use wamn_plugin::OperationHost;

use super::application_route::ApplicationRoutes;
use super::route_refusal::Refusal;
use super::workflow::{self, Workflow};

pub(super) struct HostRouteDelivery {
    postgres: Arc<OperationHost>,
    authorization: Arc<Authorization>,
    config: Arc<ApplicationConfig>,
    project_env_key: wamn_registry::ProjectEnvKey,
}

impl HostRouteDelivery {
    pub(super) fn new(
        postgres: Arc<OperationHost>,
        authorization: Arc<Authorization>,
        config: Arc<ApplicationConfig>,
    ) -> Self {
        let project_env_key =
            wamn_registry::project_env_key(&config.org, &config.project, &config.environment);
        Self {
            postgres,
            authorization,
            config,
            project_env_key,
        }
    }

    async fn handle(
        &self,
        workflow: &Workflow,
        workload: &str,
        attachment: &HostAttachment,
        caller: AuthenticatedCaller,
        request_id: Option<&str>,
        payload: &str,
    ) -> Result<Value, Refusal> {
        match attachment.route.handler {
            HostHandler::PermissionMine => {
                let held = self
                    .authorization
                    .user_operation_permissions(
                        &self.config.project,
                        &self.project_env_key,
                        caller.principal_id(),
                    )
                    .await?;
                let enabled = workflow
                    .database
                    .executor_read(&self.config.project, async |connection| {
                        Ok(workflow::lifecycle::permissions(connection, &self.config).await?)
                    })
                    .await?;
                let permissions = if held.admin {
                    let mut served = served_references(&self.config);
                    served.extend(enabled);
                    served
                } else {
                    let mut references = held.references;
                    let enabled: BTreeSet<_> = enabled.into_iter().collect();
                    references.retain(|reference| {
                        !reference.starts_with("wamn-workflow:") || enabled.contains(reference)
                    });
                    references
                };
                Ok(json!({ "admin": held.admin, "permissions": permissions }))
            }
            HostHandler::WorkflowAuthor
            | HostHandler::WorkflowEnable
            | HostHandler::WorkflowDisable
            | HostHandler::WorkflowList
            | HostHandler::WorkflowRun => {
                workflow
                    .administer(workload, attachment, &caller, payload)
                    .await
            }
            _ => {
                ApplicationRoutes {
                    postgres: &self.postgres,
                    pools: &workflow.database,
                    project: &self.config.project,
                    config: &self.config,
                    project_env_key: &self.project_env_key,
                }
                .handle(attachment, caller, request_id, payload)
                .await
            }
        }
    }
}

/// Public component operations and the Application host route contract.
fn served_references(config: &ApplicationConfig) -> BTreeSet<String> {
    let registered = config
        .components
        .iter()
        .flat_map(|component| &component.operations)
        .filter(|(_, operation)| operation.public)
        .map(|(operation, _)| {
            wamn_package::sealed_operation_reference(operation.as_str()).to_owned()
        });
    let host = HostRouteSet::Application
        .attachments()
        .map(|(_, attachment)| attachment.reference.clone());
    registered.chain(host).collect()
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
    async fn deliver(
        &self,
        workflow: &Workflow,
        workload: &str,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let resolved = match &request.source {
            Source::Attachment(id) => {
                resolve_authorized_host_route(HostRouteSet::Application, id, caller.as_ref())
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
        match self
            .handle(
                workflow,
                workload,
                attachment,
                caller,
                request_id.as_deref(),
                &input.to_string(),
            )
            .await
        {
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
        routing: &wamn_engine::ingress::IngressRouting,
        workflow: &Workflow,
        workload: &str,
        request: hyper::Request<wasmtime_wasi_http::WasiBody>,
    ) -> hyper::Response<wasmtime_wasi_http::WasiBody> {
        use http_body_util::BodyExt as _;
        let answer = |status: u16, body: Value| {
            let body = http_body_util::Full::new(bytes::Bytes::from(body.to_string()))
                .map_err(|never| match never {})
                .boxed_unsync();
            let mut response = hyper::Response::new(body);
            *response.status_mut() =
                hyper::StatusCode::from_u16(status).expect("native HTTP status");
            response.headers_mut().insert(
                "content-type",
                hyper::header::HeaderValue::from_static("application/json"),
            );
            response.headers_mut().insert(
                "cache-control",
                hyper::header::HeaderValue::from_static("no-store"),
            );
            response
        };
        let Some((id, attachment)) =
            HostRouteSet::Application
                .attachments()
                .find(|(_, attachment)| {
                    attachment.definition["route"]["path"].as_str() == Some(request.uri().path())
                })
        else {
            return answer(404, json!({"error":{"code":"route-not-found"}}));
        };
        if attachment.route.type_.http_method() != request.method().as_str() {
            return answer(405, json!({"error":{"code":"method-not-allowed"}}));
        }
        let headers = request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| wamn_application::ingress::Header {
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
        let mut body = request.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return answer(400, json!({"error":{"code":"invalid-body"}}));
            };
            if let Ok(data) = frame.into_data() {
                if bytes.len().saturating_add(data.len()) as u64 > maximum {
                    return answer(413, json!({"error":{"code":"body-too-large"}}));
                }
                bytes.extend_from_slice(&data);
            }
        }
        let authentication = routing
            .authenticate_policy(
                &self.project_env_key,
                id,
                wamn_application::ingress::RoutePolicy {
                    policy: &attachment.auth_policy,
                },
                &headers,
                &bytes,
            )
            .await;
        let caller = match authentication {
            Ok(caller) => caller,
            Err(error) => return answer(error.status, json!({"error":{"code":error.code}})),
        };
        let payload = if read {
            let parsed = wamn_routes::read_query::decode_read_query(&query);
            match parsed {
                Ok(input) => json!([input]),
                Err(_) => return answer(400, json!({"error":{"code":"invalid-query"}})),
            }
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(input) => input,
                Err(_) => return answer(400, json!({"error":{"code":"malformed-json"}})),
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
                return answer(400, json!({"error":{"code":"invalid-input"}}));
            }
        }
        let report = self
            .deliver(
                workflow,
                workload,
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
                Ok(body) => answer(200, body),
                Err(_) => answer(503, json!({"error":{"code":"execution-failed"}})),
            },
            Err(
                DeliveryError::PermissionDenied(denial)
                | DeliveryError::FreshCredentialRequired(denial),
            ) => answer(
                403,
                json!({"error":{"code":"permission-denied", "operation": denial.operation}}),
            ),
            _ => answer(503, json!({"error":{"code":"execution-failed"}})),
        }
    }
}

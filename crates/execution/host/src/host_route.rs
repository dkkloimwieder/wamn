//! Host-run routes (docs/plan/platform-ui.md §4.2).
//!
//! A host route keeps the route table, credentials, CSRF treatment and
//! operation grant of any route, but the host serves it with a fixed handler
//! instead of a component. [`HostRouteDelivery`] serves the host routes of
//! the loaded release and passes every other delivery to the release's
//! [`RouteDelivery`], the router delivery bridge. The control serving root
//! has no component, so a control host has no bridge behind it.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde_json::json;
use wamn_catalog::{HostAttachment, HostHandler};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryReport, DeliveryRequest, FailureType,
    RouteDelivery, Source, SourceRef, StreamedDelivery, resolve_authorized_host_route,
};
use wamn_identity_client::PatIssuerConfig;
use wamn_platform_identity::PrincipalId;
use wamn_platform_identity::control::{control_projects, is_org_admin, org_projects};
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;

use crate::control_route::{ControlRoutes, Refusal};

/// What the handlers of one host read.
pub enum HostRouteHandlers {
    /// The application administration routes of one project environment.
    Application {
        postgres: Arc<WamnPostgres>,
        project: String,
    },
    /// The control routes of one org, through its `control` login.
    Control {
        /// The `control` login for reads.
        control: Arc<tokio_postgres::Client>,
        /// A second `control` login that holds one write transaction at a
        /// time, so a read never runs inside a write.
        writer: Arc<tokio::sync::Mutex<tokio_postgres::Client>>,
        /// The operator client of the identity service, for `user.invite`.
        identity: Option<PatIssuerConfig>,
        org: String,
    },
}

impl fmt::Debug for HostRouteHandlers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Application { project, .. } => formatter
                .debug_struct("Application")
                .field("project", project)
                .finish_non_exhaustive(),
            Self::Control { org, .. } => formatter
                .debug_struct("Control")
                .field("org", org)
                .finish_non_exhaustive(),
        }
    }
}

/// Serves the host routes of a loaded release.
pub struct HostRouteDelivery {
    release: Arc<LoadedRelease>,
    handlers: HostRouteHandlers,
    next: Option<Arc<dyn RouteDelivery>>,
}

impl fmt::Debug for HostRouteDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostRouteDelivery")
            .field("release", &self.release.release())
            .field("next", &self.next.is_some())
            .finish_non_exhaustive()
    }
}

impl HostRouteDelivery {
    /// `next` serves every delivery that is not a host route.
    pub fn new(
        release: Arc<LoadedRelease>,
        handlers: HostRouteHandlers,
        next: Option<Arc<dyn RouteDelivery>>,
    ) -> Self {
        Self {
            release,
            handlers,
            next,
        }
    }

    async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<serde_json::Value, Refusal> {
        let principal: PrincipalId = caller.principal_id().parse().map_err(
            |error: wamn_platform_identity::IdentityError| Refusal::Failed(error.into()),
        )?;
        match (attachment.route.handler, &self.handlers) {
            (HostHandler::PermissionMine, HostRouteHandlers::Application { postgres, project }) => {
                let manifest = self.release.manifest();
                let held = postgres
                    .held_operation_grants(
                        project,
                        &manifest.release.tenant_id,
                        &principal,
                        &attachment.operation,
                    )
                    .await
                    .map_err(Refusal::Failed)?;
                let permissions = if held.admin {
                    served_references(&self.release)
                } else {
                    held.references.into_iter().collect()
                };
                Ok(json!({ "admin": held.admin, "permissions": permissions }))
            }
            (HostHandler::ControlMine, HostRouteHandlers::Control { control, org, .. }) => {
                let org_admin = is_org_admin(control.as_ref(), &principal, org).await?;
                let projects = if org_admin {
                    org_projects(control.as_ref(), org).await?
                } else {
                    control_projects(control.as_ref(), &principal, org).await?
                };
                Ok(json!({
                    "org_admin": org_admin,
                    "projects": projects
                        .iter()
                        .map(|project| json!({ "project": project, "project_admin": true }))
                        .collect::<Vec<_>>(),
                }))
            }
            (
                _,
                HostRouteHandlers::Control {
                    control,
                    writer,
                    identity,
                    org,
                },
            ) => {
                ControlRoutes {
                    control,
                    writer,
                    identity: identity.as_ref(),
                    org,
                }
                .handle(attachment, &principal, payload)
                .await
            }
            (handler, _) => Err(Refusal::Failed(anyhow::anyhow!(
                "this host does not serve the host handler {handler:?}"
            ))),
        }
    }
}

/// Every operation the release serves, without its package version: the
/// registered operations of its components and its host routes.
fn served_references(release: &LoadedRelease) -> BTreeSet<String> {
    let manifest = release.manifest();
    let registered = manifest
        .components
        .iter()
        .flat_map(|component| component.operations.values())
        .filter_map(|operation| operation.registered_operation.as_deref())
        .map(|operation| wamn_catalog::sealed_operation_reference(operation).to_owned());
    let host = manifest
        .host_routes
        .iter()
        .flat_map(|set| set.attachments())
        .map(|(_, attachment)| attachment.reference.clone());
    registered.chain(host).collect()
}

fn report(outcome: Result<DeliveryOutcome, DeliveryError>) -> DeliveryReport {
    DeliveryReport {
        outcome,
        deadline_adjustments: Vec::new(),
        actor_labels: Vec::new(),
        etag: None,
    }
}

#[async_trait::async_trait]
impl RouteDelivery for HostRouteDelivery {
    async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let resolved = match &request.source {
            Source::Attachment(id) => resolve_authorized_host_route(
                self.release.manifest(),
                SourceRef::Attachment(id),
                caller.as_ref(),
            ),
            Source::Registration(_) => None,
        };
        let Some(resolved) = resolved else {
            return match &self.next {
                Some(next) => next.deliver(request, caller).await,
                None => report(Err(DeliveryError::SourceNotFound)),
            };
        };
        let attachment = match resolved {
            Ok(attachment) => attachment,
            Err(error) => return report(Err(error)),
        };
        if request.delivery_id.is_empty() {
            return report(Err(DeliveryError::InvalidRequest));
        }
        let caller = caller.expect("a resolved host route has its caller");
        match self.handle(attachment, &caller, &request.payload).await {
            Ok(result) => report(Ok(DeliveryOutcome::Respond(result.to_string()))),
            Err(Refusal::Delivery(error)) => report(Err(error)),
            Err(Refusal::Invalid(message)) => {
                report(Ok(DeliveryOutcome::Failed(DeliveryFailure {
                    failure_type: FailureType::InvalidInput,
                    code: None,
                    message,
                })))
            }
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

    async fn deliver_stream(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> Result<StreamedDelivery, DeliveryReport> {
        // A host route reads one document and never streams.
        match &self.next {
            Some(next) => next.deliver_stream(request, caller).await,
            None => Err(report(Err(DeliveryError::InvalidRequest))),
        }
    }
}

//! The application administration routes of every application audience
//! (docs/plan/platform-ui.md §4.6).
//!
//! Every route but `permission.mine` needs `admin`, which the router checks
//! before the handler runs. A route runs in one host-owned transaction under
//! the environment's `wamn_administration` login, which binds `app.user_id`
//! to the caller and `app.operation` to the route's sealed operation id. A
//! role or permission write first takes the tenant lock that `wamn-ctl` and
//! release reconciliation take. The writes are the functions of
//! `wamn_platform_identity::application`, which `wamn-ctl` also calls, so no
//! second writer exists. A grant stores one row, and a caller holds exactly
//! its rows, so a grant works while two releases serve during a rollout.
//!
//! `workflow.start` is the one granted route here (docs/plan/workflows.md R3,
//! epic 1): it admits a run of the enabled version of an environment workflow
//! of an application the environment declares, against the release the host
//! serves, as the identity the workflow names.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use wamn_catalog::{
    HostAttachment, HostHandler, HostRouteAuthority, gate_workflow_document,
    is_granted_host_route_operation, is_host_route_operation, resolve_workflow_components,
    sealed_operation_reference,
};
use wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_platform_identity::PrincipalId;
use wamn_platform_identity::application::{
    AdministrationError, AdministrationRefusal, application_users, create_role, delete_role,
    grant_permission, grant_user_role, permission_rows, require_user, revoke_permission,
    revoke_user_role, role_names,
};
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::control::is_project_admin;
use wamn_project_state::{ADMIN_ROLE, workflow_user_id};
use wamn_runtime::plugins::wamn_postgres::{
    CounterSet, CounterSetRefusal, ManualRunAdmission, ManualRunAdmitted, WamnPostgres,
};

use crate::control_route::{Refusal, parse};

/// What the application routes of one host use.
pub(crate) struct ApplicationRoutes<'a> {
    pub(crate) postgres: &'a WamnPostgres,
    pub(crate) project: &'a str,
    /// Publishes each role or permission change.
    pub(crate) changes: &'a AuthChanges,
    pub(crate) release: &'a LoadedRelease,
    /// The identity reader of `wamn_system` and the org, for the covering
    /// fact of `user.list` and the covering check of an `admin` revoke.
    pub(crate) identity: Option<(&'a Client, &'a str)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleRequest {
    role: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionRequest {
    role: String,
    operation: String,
}

/// The input of `workflow.start`. It names no user, role or release: the
/// caller is the authenticated one, and the release is the one the host
/// serves (docs/plan/workflows.md R3). It names the application by the name
/// the environment declares.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStartRequest {
    application: String,
    workflow_id: String,
    input_json: Value,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterSetRequest {
    application: String,
    sequence: String,
    counter: String,
    key: String,
    value: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserRoleRequest {
    user_id: String,
    role: String,
}

impl From<AdministrationError> for Refusal {
    fn from(error: AdministrationError) -> Self {
        match error {
            AdministrationError::Refused(refusal) => declared(&refusal),
            AdministrationError::Failed(error) => Self::Failed(error.into()),
        }
    }
}

/// The code and detail that the contract of the route declares for one
/// refusal. `field` names the input member the refusal is about.
fn declared(refusal: &AdministrationRefusal) -> Refusal {
    let (code, field) = match refusal {
        AdministrationRefusal::AdminTakesNoPermission
        | AdministrationRefusal::AdminCreated
        | AdministrationRefusal::AdminDeleted => ("admin_fixed", "role"),
        AdministrationRefusal::NotRoleName { .. } => ("invalid_input", "role"),
        AdministrationRefusal::RoleNotFound { .. } => ("role_not_found", "role"),
        AdministrationRefusal::NotGrantable { .. } => ("operation_not_grantable", "operation"),
        AdministrationRefusal::NotHeld { .. } => ("permission_not_held", "operation"),
        AdministrationRefusal::UserNotFound { .. } => ("user_not_found", "user_id"),
        AdministrationRefusal::AdminCovered { .. } => ("admin_covered", "user_id"),
    };
    Refusal::Declared {
        code,
        detail: json!({ "field": field }),
    }
}

/// The code and detail that the contract of `number_counter.set` declares
/// for one refused set.
fn counter_set_refusal(refusal: &CounterSetRefusal) -> Refusal {
    let (code, field) = match refusal {
        CounterSetRefusal::InvalidInput { field } => ("invalid_input", *field),
        CounterSetRefusal::SequenceNotFound => ("sequence_not_found", "sequence"),
        CounterSetRefusal::CounterNotFound => ("counter_not_found", "counter"),
        CounterSetRefusal::AssignedPerKey => ("counter_assigned_per_key", "counter"),
        CounterSetRefusal::IdempotencyConflict => ("idempotency_conflict", "request_id"),
    };
    Refusal::Declared {
        code,
        detail: json!({ "field": field }),
    }
}

/// A refusal that the contract of `workflow.start` declares, about `field`.
fn start_refusal(code: &'static str, field: &str) -> Refusal {
    Refusal::Declared {
        code,
        detail: json!({ "field": field }),
    }
}

impl ApplicationRoutes<'_> {
    /// Admit one run of the enabled version of a workflow of an application
    /// the environment declares (docs/plan/workflows.md R3, R8). The stored graph is gated
    /// against the release the host serves, and the run freezes the
    /// component digest of every node. A `role` workflow executes as the
    /// `workflow` user with its role, an `invoker` workflow as the caller
    /// with its active role. The caller is recorded as the invoker.
    pub(crate) async fn start_workflow(
        &self,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: WorkflowStartRequest = parse(payload)?;
        if request.application.is_empty() {
            return Err(start_refusal("invalid_input", "application"));
        }
        if request.workflow_id.is_empty() {
            return Err(start_refusal("invalid_input", "workflow_id"));
        }
        if request.idempotency_key.is_empty() {
            return Err(start_refusal("invalid_input", "idempotency_key"));
        }
        let manifest = self.release.manifest();
        let scope = self.release.scope();
        let tenant = scope.tenant_id.as_str();
        let Some(declared) = scope.applications.get(&request.application) else {
            return Err(start_refusal("application_not_declared", "application"));
        };
        let application = request.application.as_str();
        let packages = &declared.packages;
        let workflow = self
            .postgres
            .enabled_workflow(
                self.project,
                tenant,
                &scope.environment,
                application,
                &request.workflow_id,
            )
            .await?
            .ok_or_else(|| start_refusal("workflow_not_enabled", "workflow_id"))?;
        let not_served = |_| start_refusal("workflow_not_served", "workflow_id");
        let hash = gate_workflow_document(tenant, packages, manifest, &workflow.graph)
            .map_err(not_served)?;
        if hash.as_str() != workflow.wiring_hash {
            return Err(start_refusal("workflow_not_served", "workflow_id"));
        }
        let node_digests: serde_json::Map<String, Value> =
            resolve_workflow_components(tenant, packages, manifest, &workflow.graph)
                .map_err(not_served)?
                .into_iter()
                .map(|(node, component)| (node, Value::String(component.component_digest)))
                .collect();
        let user = caller.principal_id();
        let (principal, role) = match (
            workflow.execution_type.as_str(),
            workflow.execution_role.as_deref(),
        ) {
            ("role", Some(role)) => (workflow_user_id().to_string(), role.to_owned()),
            ("invoker", None) => {
                let role = caller
                    .active_role()
                    .ok_or_else(|| start_refusal("active_role_required", "workflow_id"))?;
                (user.to_owned(), role.to_owned())
            }
            (execution_type, _) => {
                return Err(Refusal::Failed(anyhow::anyhow!(
                    "workflow {:?} stores the execution type {execution_type:?}",
                    request.workflow_id
                )));
            }
        };
        // One key space per caller and workflow, apart from the keys of
        // the other run grains.
        let idempotency_key = format!(
            "manual:{user}:{}:{}",
            request.workflow_id, request.idempotency_key
        );
        let node_digests = Value::Object(node_digests);
        let admitted = self
            .postgres
            .admit_manual_run(
                self.project,
                tenant,
                &ManualRunAdmission {
                    package_id: application,
                    manifest_digest: self.release.release().manifest_digest.as_str(),
                    environment: &scope.environment,
                    workflow_id: &request.workflow_id,
                    workflow_version: workflow.version,
                    wiring_hash: &workflow.wiring_hash,
                    execution_principal_id: &principal,
                    execution_role: &role,
                    invoked_by_user_id: user,
                    node_digests: &node_digests,
                    idempotency_key: &idempotency_key,
                    input: &request.input_json,
                },
            )
            .await
            .map_err(|error| Refusal::Failed(anyhow::anyhow!(error.to_string())))?;
        match admitted {
            ManualRunAdmitted::Queued { run_id } => Ok(json!({ "run_id": run_id })),
            ManualRunAdmitted::Conflict => {
                Err(start_refusal("idempotency_conflict", "idempotency_key"))
            }
        }
    }

    /// Serve one application route. `request_id` is the request id of a
    /// write, which `number_counter.set` logs in the write log.
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        request_id: Option<&str>,
        payload: &str,
    ) -> Result<Value, Refusal> {
        if attachment.route.handler == HostHandler::NumberCounterSet {
            let request: CounterSetRequest = parse(payload)?;
            return self
                .set_counter(attachment, principal, request_id, request)
                .await;
        }
        let tenant = self.release.scope().tenant_id.as_str();
        let mut drops = AuthDrops::default();
        let result = self
            .serve(attachment, principal, payload, tenant, &mut drops)
            .await;
        // The host drops its own memory at once, and the message reaches the
        // other hosts of the tenant.
        if result.is_ok() && !drops.is_empty() {
            self.postgres.auth_cache().clear();
            self.changes.publish(drops).await;
        }
        result
    }

    async fn serve(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
        tenant: &str,
        drops: &mut AuthDrops,
    ) -> Result<Value, Refusal> {
        match attachment.route.handler {
            HostHandler::ApplicationUserList => {
                let Empty {} = parse(payload)?;
                let (identity, org) = self.identity_reader()?;
                self.run(attachment, principal, false, async |client| {
                    let mut listed = Vec::new();
                    for user in application_users(client).await? {
                        let id: PrincipalId = user.id.parse()?;
                        let covered = is_project_admin(identity, &id, org, self.project).await?;
                        listed.push(json!({
                            "id": user.id,
                            "email": user.email,
                            "display_name": user.display_name,
                            "roles": user.roles,
                            "admin_covered": covered,
                        }));
                    }
                    Ok(json!({ "users": listed }))
                })
                .await
            }
            HostHandler::RoleList => {
                let Empty {} = parse(payload)?;
                self.run(attachment, principal, false, async |client| {
                    Ok(json!({ "roles": role_names(client).await? }))
                })
                .await
            }
            HostHandler::RoleCreate => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let created = create_role(client, tenant, &request.role, drops).await?;
                    Ok(json!({ "created": created }))
                })
                .await
            }
            HostHandler::RoleDelete => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let deleted = delete_role(client, tenant, &request.role, drops).await?;
                    Ok(json!({ "deleted": deleted }))
                })
                .await
            }
            HostHandler::PermissionList => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, false, async |client| {
                    let rows = if request.role == ADMIN_ROLE {
                        Vec::new()
                    } else {
                        permission_rows(client, &request.role).await?
                    };
                    Ok(self.permission_list(&request.role, &rows))
                })
                .await
            }
            HostHandler::PermissionGrant => {
                let request: PermissionRequest = parse(payload)?;
                if is_host_route_operation(&request.operation)
                    && !is_granted_host_route_operation(&request.operation)
                {
                    return Err(
                        AdministrationError::from(AdministrationRefusal::NotGrantable {
                            reference: request.operation,
                        })
                        .into(),
                    );
                }
                self.run(attachment, principal, true, async |client| {
                    let outcome =
                        grant_permission(client, tenant, &request.role, &request.operation, drops)
                            .await?;
                    Ok(json!({ "rows_added": outcome.rows_added }))
                })
                .await
            }
            HostHandler::PermissionRevoke => {
                let request: PermissionRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    revoke_permission(client, tenant, &request.role, &request.operation, drops)
                        .await?;
                    Ok(json!({}))
                })
                .await
            }
            HostHandler::UserRoleGrant => {
                let request: UserRoleRequest = parse(payload)?;
                let user: PrincipalId = request.user_id.parse()?;
                let user_id = user.as_str();
                self.run(attachment, principal, false, async |client| {
                    require_user(client, user_id).await?;
                    let granted =
                        grant_user_role(client, tenant, user_id, &request.role, drops).await?;
                    Ok(json!({ "granted": granted }))
                })
                .await
            }
            HostHandler::UserRoleRevoke => {
                let request: UserRoleRequest = parse(payload)?;
                let user: PrincipalId = request.user_id.parse()?;
                if request.role == ADMIN_ROLE {
                    let (identity, org) = self.identity_reader()?;
                    if is_project_admin(identity, &user, org, self.project).await? {
                        return Err(AdministrationError::from(
                            AdministrationRefusal::AdminCovered {
                                user_id: user.as_str().to_owned(),
                            },
                        )
                        .into());
                    }
                }
                let user_id = user.as_str();
                self.run(attachment, principal, false, async |client| {
                    require_user(client, user_id).await?;
                    let revoked =
                        revoke_user_role(client, tenant, user_id, &request.role, drops).await?;
                    Ok(json!({ "revoked": revoked }))
                })
                .await
            }
            handler => Err(Refusal::Failed(anyhow::anyhow!(
                "the application routes do not serve the host handler {handler:?}"
            ))),
        }
    }

    /// Set one counter of a sequence of the requested application in an
    /// operation transaction of the guest authority, logged in the write log
    /// under the route's operation and the request id
    /// (docs/plan/platform-config.md, Numbering, Administration). The
    /// application is a declared application of the release.
    async fn set_counter(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        request_id: Option<&str>,
        request: CounterSetRequest,
    ) -> Result<Value, Refusal> {
        require_counter_application(&self.release.scope().applications, &request.application)
            .map_err(|refusal| counter_set_refusal(&refusal))?;
        let set = CounterSet {
            application: request.application,
            sequence: request.sequence,
            counter: request.counter,
            key: request.key,
            value: request.value,
        };
        let value = self
            .postgres
            .set_number_counter(
                self.project,
                self.release.scope().tenant_id.as_str(),
                principal.as_str(),
                &attachment.operation,
                request_id.unwrap_or_default(),
                &set,
            )
            .await
            .map_err(|error| match error.refusal() {
                Some(refusal) => counter_set_refusal(refusal),
                None => Refusal::Failed(error.into()),
            })?;
        Ok(json!({ "value": value }))
    }

    /// The identity reader of the host. `project-admin`, which `org-admin`
    /// writes in every project of the org, covers a user's `admin`.
    fn identity_reader(&self) -> Result<(&Client, &str), Refusal> {
        self.identity.ok_or_else(|| {
            Refusal::Failed(anyhow::anyhow!(
                "the covering check of admin needs the identity reader of the host"
            ))
        })
    }

    /// Run `work` in one administration transaction of the route. A role or
    /// permission write first takes the tenant lock.
    async fn run(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        lock: bool,
        work: impl AsyncFnOnce(&Client) -> Result<Value, Refusal>,
    ) -> Result<Value, Refusal> {
        let tenant = self.release.scope().tenant_id.as_str();
        self.postgres
            .administration_transaction(
                self.project,
                tenant,
                principal,
                &attachment.operation,
                async |client| {
                    if lock {
                        client
                            .query_one(OPERATION_GRANT_LOCK_SQL, &[&tenant])
                            .await?;
                    }
                    work(client).await
                },
            )
            .await?
    }

    /// Each operation the release serves and each stored permission of the
    /// role: whether the role holds it and whether a role can take it (§4.6).
    fn permission_list(&self, role: &str, rows: &[String]) -> Value {
        let served: BTreeSet<&str> = self
            .release
            .manifest()
            .components
            .iter()
            .flat_map(|component| component.operations.values())
            .filter_map(|operation| operation.registered_operation.as_deref())
            .map(sealed_operation_reference)
            .collect();
        let host: BTreeMap<String, HostRouteAuthority> = self
            .release
            .manifest()
            .host_routes
            .iter()
            .flat_map(|set| set.attachments())
            .map(|(_, attachment)| (attachment.reference.clone(), attachment.route.authority))
            .collect();
        let mut operations = served.clone();
        operations.extend(host.keys().map(String::as_str));
        operations.extend(rows.iter().map(String::as_str));
        let operations: Vec<Value> = operations
            .into_iter()
            .map(|operation| {
                let authority = host.get(operation);
                json!({
                    "operation": operation,
                    "served": served.contains(operation) || authority.is_some(),
                    "grantable": (served.contains(operation) && authority.is_none())
                        || authority == Some(&HostRouteAuthority::Granted),
                    "admin_only": authority == Some(&HostRouteAuthority::Admin),
                    "selected": rows.iter().any(|row| row == operation),
                })
            })
            .collect();
        json!({
            "role": role,
            "admin": role == ADMIN_ROLE,
            "operations": operations,
        })
    }
}

fn require_counter_application(
    applications: &wamn_catalog::EnvironmentApplications,
    application: &str,
) -> Result<(), CounterSetRefusal> {
    if applications.contains_key(application) {
        Ok(())
    } else {
        Err(CounterSetRefusal::InvalidInput {
            field: "application",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_set_accepts_only_a_declared_application_name() {
        let applications = serde_json::from_value(json!({
            "receiving": {"packages": ["base", "overlay"]}
        }))
        .unwrap();
        assert!(require_counter_application(&applications, "receiving").is_ok());
        for name in ["base", "overlay", "unknown", ""] {
            assert!(matches!(
                require_counter_application(&applications, name),
                Err(CounterSetRefusal::InvalidInput {
                    field: "application"
                })
            ));
        }
    }

    /// `workflow.start` takes no user id: the caller is the authenticated one
    /// (docs/plan/workflows.md R3).
    #[test]
    fn a_start_refuses_a_user_id() {
        let start = json!({
            "application": "orders",
            "workflow_id": "nightly",
            "input_json": {"day": 1},
            "idempotency_key": "first",
        });
        let parsed: WorkflowStartRequest =
            parse(&start.to_string()).unwrap_or_else(|_| panic!("the contract input parses"));
        assert_eq!(parsed.application, "orders");
        assert_eq!(parsed.workflow_id, "nightly");
        for (member, value) in [
            ("user_id", json!("11111111-1111-4111-8111-111111111111")),
            ("role", json!("clerk")),
            (
                "manifest_digest",
                json!(format!("sha256:{}", "1".repeat(64))),
            ),
        ] {
            let mut named = start.clone();
            named[member] = value;
            assert!(
                matches!(
                    parse::<WorkflowStartRequest>(&named.to_string()),
                    Err(Refusal::Delivery(
                        wamn_engine::router_delivery::DeliveryError::InvalidPayload
                    ))
                ),
                "a start naming {member} refuses"
            );
        }
    }
}

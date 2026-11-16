//! The application administration routes of every application audience
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Every route but `permission.mine` needs `admin`, which the router tests
//! before the handler runs. A route runs in one host-owned transaction under
//! the environment's `wamn_administration` login, which binds `app.user_id`
//! to the caller and `app.operation` to the route's sealed operation id. A
//! role or permission write first takes the project lock that `wamn-ctl` and
//! release reconciliation take. The writes are the functions of
//! `wamn_platform_identity::application`, which `wamn-ctl` also calls, so no
//! second writer exists. A grant stores one row, and a caller holds exactly
//! its rows, so a grant works while two releases serve during a rollout.
//!
//! `workflow.start` is the one granted route here
//! (docs/architecture/execution.md#environment-workflows): it admits a run of
//! the enabled version of an environment workflow of an application the
//! environment declares, against the release the host serves, as the identity
//! the workflow names.

use std::collections::{BTreeMap, BTreeSet};
use wamn_broker::Publisher;
use wamn_events::admission::{ManualRunAdmission, ManualRunAdmitted};

use crate::number_counter::{CounterSet, CounterSetRefusal};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection as Client;
use wamn_broker::auth_changes::AuthDrops;
use wamn_catalog::{
    HostAttachment, HostHandler, HostRouteAuthority, condition_workflow_document,
    resolve_workflow_components, sealed_operation_reference,
};
use wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::{ADMIN_ROLE, workflow_user_id};
use wamn_platform_identity::application::{
    AdministrationError, AdministrationRefusal, GrantAuthority, GrantChange, application_users,
    create_role, delete_role, mutate_grant, permission_is_grantable, permission_rows, role_names,
};
use wamn_platform_identity::control::is_project_admin;
use wamn_sql::WamnPostgres;

use crate::control_route::{Refusal, parse};

/// What the application routes of one host use.
pub(crate) struct ApplicationRoutes<'a> {
    pub(crate) postgres: &'a WamnPostgres,
    pub(crate) project: &'a wamn_registry::ProjectId,
    /// Publishes each role or permission change.
    pub(crate) changes: &'a Publisher,
    pub(crate) release: &'a LoadedRelease,
    /// The identity reader of `wamn_system` and the org, for the covering
    /// fact of `user.list` and the covering test of an `admin` revoke.
    pub(crate) identity: Option<(&'a sqlx::PgPool, &'a wamn_registry::OrgId)>,
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
    operation: wamn_catalog::canonical_name::CanonicalName,
}

/// The input of `workflow.start`. It names no user, role or release: the caller
/// is the authenticated one, and the release is the one the host serves
/// (docs/architecture/execution.md#environment-workflows). It names the
/// application by the name the environment declares.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowStartRequest {
    application: wamn_catalog::ApplicationName,
    workflow_id: wamn_catalog::canonical_name::CanonicalName,
    input_json: Value,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterSetRequest {
    application: wamn_catalog::ApplicationName,
    sequence: String,
    counter: String,
    key: String,
    value: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserRoleRequest {
    user_id: wamn_identity_protocol::PrincipalId,
    role: String,
}

impl From<wamn_platform_identity::application::GrantError> for Refusal {
    fn from(error: wamn_platform_identity::application::GrantError) -> Self {
        AdministrationError::from(error).into()
    }
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
        AdministrationRefusal::NotProjectRole { .. } => ("invalid_input", "role"),
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
    /// Admit one run of the enabled version of a workflow of an application the
    /// environment declares
    /// (docs/architecture/execution.md#environment-workflows). The stored graph
    /// is subject to the conditions of the release the host serves, and the run freezes the
    /// component digest of every node. A `role` workflow executes as the
    /// `workflow` user with its role, an `invoker` workflow as the caller with
    /// its active role. The caller is recorded as the invoker.
    pub(crate) async fn start_workflow(
        &self,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: WorkflowStartRequest = parse(payload)?;
        if request.idempotency_key.is_empty() {
            return Err(start_refusal("invalid_input", "idempotency_key"));
        }
        let manifest = self.release.manifest();
        let target = self.release.target();
        let project_env_key = &target.project_env_key;
        let Some(declared) = target.applications.get(&request.application) else {
            return Err(start_refusal("application_not_declared", "application"));
        };
        let application = &request.application;
        let packages = &declared.packages;
        let workflow = self
            .postgres
            .executor_read(self.project, async |connection| {
                wamn_events::admission::enabled_workflow(
                    connection,
                    project_env_key,
                    &target.environment,
                    application,
                    &request.workflow_id,
                )
                .await
            })
            .await?
            .ok_or_else(|| start_refusal("workflow_not_enabled", "workflow_id"))?;
        let not_served = |_| start_refusal("workflow_not_served", "workflow_id");
        let hash =
            condition_workflow_document(project_env_key, packages, manifest, &workflow.graph)
                .map_err(not_served)?;
        if hash != workflow.wiring_hash {
            return Err(start_refusal("workflow_not_served", "workflow_id"));
        }
        let node_digests: serde_json::Map<String, Value> =
            resolve_workflow_components(project_env_key, packages, manifest, &workflow.graph)
                .map_err(not_served)?
                .into_iter()
                .map(|(node, component)| {
                    (
                        node.to_string(),
                        Value::String(component.component_digest.to_string()),
                    )
                })
                .collect();
        let user = caller.principal_id();
        let (principal, role) = match (
            workflow.execution_type.as_str(),
            workflow.execution_role.as_deref(),
        ) {
            ("role", Some(role)) => (workflow_user_id(), role.to_owned()),
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
            .executor_write(self.project, async |connection| {
                wamn_events::admission::admit_manual_run(
                    connection,
                    project_env_key,
                    &ManualRunAdmission {
                        application: application,
                        manifest_digest: &self.release.release().manifest_digest,
                        environment: &target.environment,
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
            })
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
        let project_env_key = &self.release.target().project_env_key;
        let mut drops = AuthDrops::default();
        let result = self
            .serve(attachment, principal, payload, project_env_key, &mut drops)
            .await;
        if result.is_ok() && !drops.is_empty() {
            self.changes.publish(drops).await;
        }
        result
    }

    async fn serve(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
        project_env_key: &wamn_registry::ProjectEnvKey,
        drops: &mut AuthDrops,
    ) -> Result<Value, Refusal> {
        match attachment.route.handler {
            HostHandler::ApplicationUserList => {
                let Empty {} = parse(payload)?;
                let (identity, org) = self.identity_reader()?;
                self.run(attachment, principal, false, async |client| {
                    let mut listed = Vec::new();
                    for user in application_users(client).await? {
                        let id: PrincipalId = user.id;
                        let covered = is_project_admin(
                            &mut *identity.acquire().await?,
                            &id,
                            org,
                            self.project,
                            Some(&self.release.target().environment),
                        )
                        .await?;
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
                    let created =
                        create_role(client, project_env_key, &request.role, drops).await?;
                    Ok(json!({ "created": created }))
                })
                .await
            }
            HostHandler::RoleDelete => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let deleted =
                        delete_role(client, project_env_key, &request.role, drops).await?;
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
                self.run(attachment, principal, true, async |client| {
                    let outcome = mutate_grant(
                        client,
                        project_env_key,
                        GrantAuthority::User(principal),
                        GrantChange::Permission {
                            role: &request.role,
                            reference: request.operation.as_str(),
                            grant: true,
                        },
                        drops,
                    )
                    .await?;
                    Ok(json!({ "rows_added": outcome.rows_added }))
                })
                .await
            }
            HostHandler::PermissionRevoke => {
                let request: PermissionRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    mutate_grant(
                        client,
                        project_env_key,
                        GrantAuthority::User(principal),
                        GrantChange::Permission {
                            role: &request.role,
                            reference: request.operation.as_str(),
                            grant: false,
                        },
                        drops,
                    )
                    .await?;
                    Ok(json!({}))
                })
                .await
            }
            HostHandler::UserRoleGrant => {
                let request: UserRoleRequest = parse(payload)?;
                let user: PrincipalId = request.user_id;
                let user_id = &user;
                self.run(attachment, principal, true, async |client| {
                    let granted = mutate_grant(
                        client,
                        project_env_key,
                        GrantAuthority::User(principal),
                        GrantChange::UserRole {
                            user_id,
                            role: &request.role,
                            grant: true,
                        },
                        drops,
                    )
                    .await?;
                    Ok(json!({ "granted": granted.changed }))
                })
                .await
            }
            HostHandler::UserRoleRevoke => {
                let request: UserRoleRequest = parse(payload)?;
                let user: PrincipalId = request.user_id;
                let user_id = &user;
                self.run(attachment, principal, true, async |client| {
                    let revoked = mutate_grant(
                        client,
                        project_env_key,
                        GrantAuthority::User(principal),
                        GrantChange::UserRole {
                            user_id,
                            role: &request.role,
                            grant: false,
                        },
                        drops,
                    )
                    .await?;
                    Ok(json!({ "revoked": revoked.changed }))
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
    /// (docs/architecture/data-access.md#setting-a-counter). The application is
    /// a declared application of the release.
    async fn set_counter(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        request_id: Option<&str>,
        request: CounterSetRequest,
    ) -> Result<Value, Refusal> {
        require_counter_application(&self.release.target().applications, &request.application)
            .map_err(|refusal| counter_set_refusal(&refusal))?;
        let set = CounterSet {
            application: request.application,
            sequence: request.sequence,
            counter: request.counter,
            key: request.key,
            value: request.value,
        };
        let value = crate::number_counter::set_number_counter(
            self.postgres,
            self.project,
            &self.release.target().project_env_key,
            principal,
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
    fn identity_reader(&self) -> Result<(&sqlx::PgPool, &wamn_registry::OrgId), Refusal> {
        self.identity.ok_or_else(|| {
            Refusal::Failed(anyhow::anyhow!(
                "the covering condition of admin needs the identity reader of the host"
            ))
        })
    }

    /// Run `work` in one administration transaction of the route. A role or
    /// permission write first takes the project lock.
    async fn run(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        lock: bool,
        work: impl AsyncFnOnce(&mut Client) -> Result<Value, Refusal>,
    ) -> Result<Value, Refusal> {
        let project_env_key = &self.release.target().project_env_key;
        self.postgres
            .administration_transaction(
                self.project,
                principal,
                &attachment.operation.clone().into(),
                async |client| {
                    if lock {
                        sqlx::query(OPERATION_GRANT_LOCK_SQL)
                            .bind(project_env_key)
                            .fetch_one(&mut *client)
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
            .flat_map(|component| component.descriptor.operations.values())
            .filter_map(|operation| operation.registered_operation.as_ref())
            .map(|operation| sealed_operation_reference(operation.as_str()))
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
                    "grantable": permission_is_grantable(operation),
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
    application: &wamn_catalog::ApplicationName,
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
        assert!(
            require_counter_application(&applications, &"receiving".parse().expect("valid name"))
                .is_ok()
        );
        for name in ["base", "overlay", "unknown"] {
            assert!(matches!(
                require_counter_application(&applications, &name.parse().unwrap()),
                Err(CounterSetRefusal::InvalidInput {
                    field: "application"
                })
            ));
        }
    }

    /// `workflow.start` takes no user id: the caller is the authenticated one
    /// (docs/architecture/execution.md#environment-workflows).
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
        assert_eq!(parsed.application, "orders".parse().unwrap());
        assert_eq!(parsed.workflow_id, "nightly");
        for (member, value) in [
            ("user_id", json!("11111111-1111-4111-8111-111111111111")),
            ("role", json!("clerk")),
            (
                "manifest_digest",
                json!(
                    (format!("sha256:{}", "1".repeat(64)))
                        .parse::<wamn_digest::Digest>()
                        .unwrap()
                ),
            ),
        ] {
            let mut named = start.clone();
            named[member] = value;
            assert!(
                matches!(
                    parse::<WorkflowStartRequest>(&named.to_string()),
                    Err(Refusal::Delivery(
                        wamn_engine::native_routes::DeliveryError::InvalidPayload
                    ))
                ),
                "a start naming {member} refuses"
            );
        }
    }
}

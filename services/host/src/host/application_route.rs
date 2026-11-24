//! The application administration routes of every application audience
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Administration requires `admin`, which the router tests before the handler
//! runs. Counter writes require their operation grant. A route runs in one host-owned transaction under
//! the environment's `wamn_platform` login, which binds `app.user_id`
//! to the caller and `app.operation` to the route's sealed operation id. A
//! role or permission write first takes the project lock that `wamn` and
//! release reconciliation take. The writes are the functions of
//! `wamn_identity::application`, which `wamn` also calls, so no
//! second writer exists. A grant stores one row, and a caller holds exactly
//! its rows, so a grant works while two releases serve during a rollout.
//!
use wamn_routes::HostAttachment;
use wamn_routes::HostHandler;
use wamn_routes::HostRouteAuthority;
use wamn_package::sealed_operation_reference;
use std::collections::{BTreeMap, BTreeSet};
use wamn_identity_protocol::ADMIN_ROLE;
use wamn_identity_protocol::PrincipalId;
use wamn_identity::application::OPERATION_GRANT_LOCK_SQL;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection as Client;
use wamn_application::application_config::ApplicationConfig;
use wamn_application::ingress::AuthenticatedCaller;
use wamn_identity::application::{
    AdministrationError, AdministrationRefusal, GrantAuthority, GrantChange, application_users,
    create_role, delete_role, mutate_grant, permission_is_grantable, permission_rows, role_names,
};
use wamn_plugin::OperationHost;
use wamn_plugin::number_counter::{CounterSet, CounterSetRefusal};

use super::route_refusal::{Refusal, parse};

/// What the application routes of one host use.
pub(crate) struct ApplicationRoutes<'a> {
    pub(crate) postgres: &'a OperationHost,
    pub(crate) project: &'a wamn_registry::ProjectId,
    pub(crate) config: &'a ApplicationConfig,
    pub(crate) project_env_key: &'a wamn_registry::ProjectEnvKey,
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
    operation: wamn_package::canonical_name::CanonicalName,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterSetRequest {
    application: wamn_registry::ApplicationName,
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

impl From<wamn_identity::application::GrantError> for Refusal {
    fn from(error: wamn_identity::application::GrantError) -> Self {
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

impl ApplicationRoutes<'_> {
    /// Serve one application route. `request_id` is the request id of a
    /// write, which `number_counter.set` logs in the write log.
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: AuthenticatedCaller,
        request_id: Option<&str>,
        payload: &str,
    ) -> Result<Value, Refusal> {
        if attachment.route.handler == HostHandler::NumberCounterSet {
            let request: CounterSetRequest = parse(payload)?;
            return self
                .set_counter(attachment, caller, request_id, request)
                .await;
        }
        let project_env_key = self.project_env_key;
        self.serve(attachment, caller.principal_id(), payload, project_env_key)
            .await
    }

    async fn serve(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> Result<Value, Refusal> {
        match attachment.route.handler {
            HostHandler::ApplicationUserList => {
                let Empty {} = parse(payload)?;
                self.run(attachment, principal, false, async |client| {
                    let mut listed = Vec::new();
                    for user in application_users(client).await? {
                        let admin_covered = user.roles.iter().any(|role| role == ADMIN_ROLE);
                        listed.push(json!({
                            "id": user.id,
                            "email": user.email,
                            "display_name": user.display_name,
                            "roles": user.roles,
                            "admin_covered": admin_covered,
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
                    let created = create_role(client, &request.role).await?;
                    Ok(json!({ "created": created }))
                })
                .await
            }
            HostHandler::RoleDelete => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let deleted = delete_role(client, &request.role).await?;
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
                    let workflows =
                        super::workflow::lifecycle::permissions(client, self.config).await?;
                    Ok(self.permission_list(&request.role, &rows, &workflows))
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
        caller: AuthenticatedCaller,
        request_id: Option<&str>,
        request: CounterSetRequest,
    ) -> Result<Value, Refusal> {
        require_counter_application(&self.config.application, &request.application)
            .map_err(|refusal| counter_set_refusal(&refusal))?;
        let set = CounterSet {
            application: request.application,
            sequence: request.sequence,
            counter: request.counter,
            key: request.key,
            value: request.value,
        };
        let value = wamn_plugin::number_counter::set_number_counter(
            self.postgres,
            self.project,
            self.project_env_key,
            caller,
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

    /// Run `work` in one administration transaction of the route. A role or
    /// permission write first takes the project lock.
    async fn run(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        lock: bool,
        work: impl AsyncFnOnce(&mut Client) -> Result<Value, Refusal>,
    ) -> Result<Value, Refusal> {
        let project_env_key = self.project_env_key;
        self.postgres
            .administration_transaction(
                self.project,
                principal,
                &attachment.operation.clone().into(),
                async |client| {
                    if lock {
                        sqlx::query(OPERATION_GRANT_LOCK_SQL)
                            .bind(project_env_key.as_str())
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
    fn permission_list(&self, role: &str, rows: &[String], workflows: &[String]) -> Value {
        let served: BTreeSet<&str> = self
            .config
            .components
            .iter()
            .flat_map(|component| &component.operations)
            .filter(|(_, operation)| operation.public)
            .map(|(operation, _)| sealed_operation_reference(operation.as_str()))
            .chain(workflows.iter().map(String::as_str))
            .collect();
        let host: BTreeMap<String, HostRouteAuthority> = wamn_routes::HostRouteSet::Application
            .attachments()
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
    served: &wamn_registry::ApplicationName,
    application: &wamn_registry::ApplicationName,
) -> Result<(), CounterSetRefusal> {
    if served == application {
        Ok(())
    } else {
        Err(CounterSetRefusal::InvalidInput {
            field: "application",
        })
    }
}

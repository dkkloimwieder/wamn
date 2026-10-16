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

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use wamn_catalog::{
    HostAttachment, HostHandler, HostRouteAuthority, is_host_route_operation,
    sealed_operation_reference,
};
use wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_platform_identity::PrincipalId;
use wamn_platform_identity::application::{
    AdministrationError, AdministrationRefusal, application_users, create_role, delete_role,
    grant_permission, grant_user_role, permission_rows, require_user, revoke_permission,
    revoke_user_role, role_names,
};
use wamn_platform_identity::control::is_project_admin;
use wamn_project_state::ADMIN_ROLE;
use wamn_runtime::auth_cache::AuthChanges;
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;

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

impl ApplicationRoutes<'_> {
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let tenant = self.release.scope().tenant_id.as_str();
        let writes = matches!(
            attachment.route.handler,
            HostHandler::RoleCreate
                | HostHandler::RoleDelete
                | HostHandler::PermissionGrant
                | HostHandler::PermissionRevoke
                | HostHandler::UserRoleGrant
                | HostHandler::UserRoleRevoke
        );
        let result = self.serve(attachment, principal, payload, tenant).await;
        // The host drops its own memory at once, and the message reaches the
        // other hosts of the tenant.
        if writes && result.is_ok() {
            self.postgres.auth_cache().clear();
            self.changes.publish(tenant).await;
        }
        result
    }

    async fn serve(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
        tenant: &str,
    ) -> Result<Value, Refusal> {
        match attachment.route.handler {
            HostHandler::ApplicationUserList => {
                let Empty {} = parse(payload)?;
                let (identity, org) = self.identity_reader()?;
                self.run(attachment, principal, false, async |client| {
                    let mut listed = Vec::new();
                    for user in application_users(client, tenant).await? {
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
                    Ok(json!({ "roles": role_names(client, tenant).await? }))
                })
                .await
            }
            HostHandler::RoleCreate => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let created = create_role(client, tenant, &request.role).await?;
                    Ok(json!({ "created": created }))
                })
                .await
            }
            HostHandler::RoleDelete => {
                let request: RoleRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let deleted = delete_role(client, tenant, &request.role).await?;
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
                        permission_rows(client, tenant, &request.role).await?
                    };
                    Ok(self.permission_list(&request.role, &rows))
                })
                .await
            }
            HostHandler::PermissionGrant => {
                let request: PermissionRequest = parse(payload)?;
                if is_host_route_operation(&request.operation) {
                    return Err(
                        AdministrationError::from(AdministrationRefusal::NotGrantable {
                            reference: request.operation,
                        })
                        .into(),
                    );
                }
                self.run(attachment, principal, true, async |client| {
                    let outcome =
                        grant_permission(client, tenant, &request.role, &request.operation).await?;
                    Ok(json!({ "rows_added": outcome.rows_added }))
                })
                .await
            }
            HostHandler::PermissionRevoke => {
                let request: PermissionRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    revoke_permission(client, tenant, &request.role, &request.operation).await?;
                    Ok(json!({}))
                })
                .await
            }
            HostHandler::UserRoleGrant => {
                let request: UserRoleRequest = parse(payload)?;
                let user: PrincipalId = request.user_id.parse()?;
                let user_id = user.as_str();
                self.run(attachment, principal, false, async |client| {
                    require_user(client, tenant, user_id).await?;
                    let granted = grant_user_role(client, tenant, user_id, &request.role).await?;
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
                    require_user(client, tenant, user_id).await?;
                    let revoked = revoke_user_role(client, tenant, user_id, &request.role).await?;
                    Ok(json!({ "revoked": revoked }))
                })
                .await
            }
            handler => Err(Refusal::Failed(anyhow::anyhow!(
                "the application routes do not serve the host handler {handler:?}"
            ))),
        }
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
                    "grantable": served.contains(operation) && authority.is_none(),
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

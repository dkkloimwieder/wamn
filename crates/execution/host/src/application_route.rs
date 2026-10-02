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
//! second writer exists. A grant writes the closure of the loaded release,
//! and refuses when that release is no longer the head of its environment.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use wamn_catalog::{
    HostAttachment, HostHandler, HostRouteAuthority, ReleaseClosures, is_host_route_operation,
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
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;

use crate::control_route::{Refusal, parse};

/// The release that the environment's head names. Release reconciliation
/// advances it in the transaction that rewrites the closures, under the
/// tenant lock, so a read under that lock is exact.
const RELEASE_HEAD_SQL: &str = "SELECT effective_release_id FROM catalog.effective_release_heads \
    WHERE tenant_id = $1 AND environment = $2";

/// What the application routes of one host use.
pub(crate) struct ApplicationRoutes<'a> {
    pub(crate) postgres: &'a WamnPostgres,
    pub(crate) project: &'a str,
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
            AdministrationError::Refused(refusal) => declared(refusal),
            AdministrationError::Failed(error) => Self::Failed(error.into()),
        }
    }
}

/// The code and detail that the contract of the route declares for one
/// refusal. `field` names the input member the refusal is about.
fn declared(refusal: AdministrationRefusal) -> Refusal {
    let (code, field) = match &refusal {
        AdministrationRefusal::AdminTakesNoPermission
        | AdministrationRefusal::AdminCreated
        | AdministrationRefusal::AdminDeleted => ("admin_fixed", "role"),
        AdministrationRefusal::NotRoleName { .. } => ("invalid_input", "role"),
        AdministrationRefusal::RoleNotFound { .. } => ("role_not_found", "role"),
        AdministrationRefusal::OperationNotServed { .. } => ("operation_not_served", "operation"),
        AdministrationRefusal::NotGrantable { .. } => ("operation_not_grantable", "operation"),
        AdministrationRefusal::NotHeld { .. } => ("permission_not_held", "operation"),
        AdministrationRefusal::NotSelected { .. } => ("permission_not_selected", "operation"),
        AdministrationRefusal::UserNotFound { .. } => ("user_not_found", "user_id"),
        AdministrationRefusal::AdminCovered { .. } => ("admin_covered", "user_id"),
    };
    let mut detail = json!({ "field": field });
    if let AdministrationRefusal::NotSelected { required_by, .. } = refusal {
        detail["required_by"] = json!(required_by);
    }
    Refusal::Declared { code, detail }
}

impl ApplicationRoutes<'_> {
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        principal: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let tenant = self.release.manifest().release.tenant_id.as_str();
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
                let closures = ReleaseClosures::from_manifest(self.release.manifest());
                self.run(attachment, principal, true, async |client| {
                    self.require_head(client).await?;
                    let outcome = grant_permission(
                        client,
                        tenant,
                        &request.role,
                        &request.operation,
                        closures.closure(&request.operation),
                    )
                    .await?;
                    Ok(json!({
                        "rows_added": outcome.rows_added,
                        "closure": outcome.closure,
                    }))
                })
                .await
            }
            HostHandler::PermissionRevoke => {
                let request: PermissionRequest = parse(payload)?;
                self.run(attachment, principal, true, async |client| {
                    let outcome =
                        revoke_permission(client, tenant, &request.role, &request.operation)
                            .await?;
                    Ok(json!({ "still_required_by": outcome.still_required_by }))
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
        let tenant = self.release.manifest().release.tenant_id.as_str();
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

    /// Refuse a grant while the loaded release is not the head, because its
    /// closures are not the ones that reconciliation wrote.
    async fn require_head(&self, client: &Client) -> Result<(), Refusal> {
        let release = &self.release.manifest().release;
        let head: Option<i32> = client
            .query_opt(
                RELEASE_HEAD_SQL,
                &[&release.tenant_id, &release.environment],
            )
            .await?
            .map(|row| row.get(0));
        if head.and_then(|head| u32::try_from(head).ok())
            == Some(release.effective_release_id.get())
        {
            return Ok(());
        }
        Err(Refusal::Declared {
            code: "release_not_current",
            detail: json!({ "field": "operation" }),
        })
    }

    /// Each operation the release serves and each stored row of the role:
    /// whether the role selected it, the roots that require it, and whether
    /// a role can take it (§4.6).
    fn permission_list(&self, role: &str, rows: &[(String, String)]) -> Value {
        let mut required: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut selected: BTreeSet<&str> = BTreeSet::new();
        for (permission, root) in rows {
            if permission == root {
                selected.insert(permission);
            } else {
                required.entry(permission).or_default().insert(root);
            }
        }
        let closures = ReleaseClosures::from_manifest(self.release.manifest());
        let host: BTreeMap<String, HostRouteAuthority> = self
            .release
            .manifest()
            .host_routes
            .iter()
            .flat_map(|set| set.attachments())
            .map(|(_, attachment)| (attachment.reference.clone(), attachment.route.authority))
            .collect();
        let mut operations: BTreeSet<&str> = closures.roots().collect();
        operations.extend(host.keys().map(String::as_str));
        operations.extend(rows.iter().map(|(permission, _)| permission.as_str()));
        let operations: Vec<Value> = operations
            .into_iter()
            .map(|operation| {
                let authority = host.get(operation);
                json!({
                    "operation": operation,
                    "served": closures.closure(operation).is_some() || authority.is_some(),
                    "grantable": closures.closure(operation).is_some() && authority.is_none(),
                    "admin_only": authority == Some(&HostRouteAuthority::Admin),
                    "selected": selected.contains(operation),
                    "required_by": required.get(operation).cloned().unwrap_or_default(),
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

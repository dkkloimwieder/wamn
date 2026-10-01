//! The org routes of the control serving root (docs/plan/platform-ui.md §4.4).
//!
//! Every org route needs a current `org-admin` row of the caller in the
//! token's org. A write runs in one transaction on the org's `control`
//! login, binds `app.user_id` to the caller and `app.operation` to the
//! route's sealed operation id, and checks the role again inside it. The
//! writes are the functions of `wamn_platform_identity::org`, which
//! `wamn-ctl invite` also calls, so no second writer exists.

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_postgres::{Client, Transaction};
use wamn_catalog::{HostAttachment, HostHandler};
use wamn_engine::router_delivery::{DeliveryError, PermissionDenial};
use wamn_identity_client::{PatIssuerConfig, UserRefused, create_user, send_invitation};
use wamn_platform_identity::control::{is_org_admin, org_projects};
use wamn_platform_identity::org::{
    MemberGrants, deactivate_org_membership, grant_org_admin, invite_member, org_users,
    reactivate_org_membership, revoke_org_admin,
};
use wamn_platform_identity::{IdentityError, IdentityErrorType, PrincipalId};

/// Why an org route did not answer.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A delivery refusal, such as a caller without `org-admin`.
    Delivery(DeliveryError),
    /// Invalid input, with a reason the caller can act on.
    Invalid(String),
    /// A failure of the host or of a service it calls.
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<tokio_postgres::Error> for Refusal {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Failed(error.into())
    }
}

impl From<IdentityError> for Refusal {
    fn from(error: IdentityError) -> Self {
        match error.kind() {
            IdentityErrorType::InvalidInput
            | IdentityErrorType::NotFound
            | IdentityErrorType::Conflict => Self::Invalid(error.to_string()),
            _ => Self::Failed(error.into()),
        }
    }
}

/// What the org routes of one control host use.
pub(crate) struct ControlRoutes<'a> {
    /// The `control` login for reads.
    pub(crate) control: &'a Client,
    /// A second `control` login that holds one write transaction at a time.
    pub(crate) writer: &'a Mutex<Client>,
    /// The operator client of the identity service, for `user.invite`.
    pub(crate) identity: Option<&'a PatIssuerConfig>,
    pub(crate) org: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalRequest {
    principal_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteRequest {
    email: String,
    display_name: String,
    #[serde(default)]
    org_admin: bool,
    #[serde(default)]
    project_admins: Vec<String>,
    #[serde(default)]
    memberships: Vec<MembershipRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipRequest {
    project: String,
    env: String,
}

impl ControlRoutes<'_> {
    /// Serve one org route for `caller`.
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        if !is_org_admin(self.control, caller, self.org).await? {
            return Err(denied(attachment));
        }
        match attachment.route.handler {
            HostHandler::UserList => {
                Ok(json!({ "users": org_users(self.control, self.org).await? }))
            }
            HostHandler::ProjectList => {
                Ok(json!({ "projects": org_projects(self.control, self.org).await? }))
            }
            HostHandler::UserInvite => self.invite(attachment, caller, payload).await,
            HostHandler::UserActivate => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                reactivate_org_membership(&transaction, &principal, self.org).await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "status": "active" }))
            }
            HostHandler::UserDeactivate => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                deactivate_org_membership(&transaction, &principal, self.org).await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "status": "inactive" }))
            }
            HostHandler::OrgAdminGrant => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                grant_org_admin(&transaction, &principal, self.org).await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "org_admin": true }))
            }
            HostHandler::OrgAdminRevoke => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                revoke_org_admin(&transaction, &principal, self.org).await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "org_admin": false }))
            }
            handler => Err(Refusal::Failed(anyhow::anyhow!(
                "the control host does not serve the host handler {handler:?}"
            ))),
        }
    }

    /// Identity creates or reuses the user, one transaction writes the org
    /// membership and the grants, and identity mails the invitation only when
    /// the user has no password.
    async fn invite(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: InviteRequest = serde_json::from_str(payload)
            .map_err(|_| Refusal::Delivery(DeliveryError::InvalidPayload))?;
        let identity = self.identity.ok_or_else(|| {
            Refusal::Failed(anyhow::anyhow!(
                "the control host has no identity operator client"
            ))
        })?;
        let user = create_user(identity, &request.email, &request.display_name)
            .await
            .map_err(|error| match error.downcast::<UserRefused>() {
                Ok(refused) => Refusal::Invalid(refused.0),
                Err(error) => Refusal::Failed(error),
            })?;
        let grants = MemberGrants {
            org_admin: request.org_admin,
            project_admins: request.project_admins,
            memberships: request
                .memberships
                .into_iter()
                .map(|membership| (membership.project, membership.env))
                .collect(),
        };
        {
            let mut writer = self.writer.lock().await;
            let transaction = self.begin(&mut writer, attachment, caller).await?;
            invite_member(&transaction, &user.principal_id, self.org, &grants).await?;
            transaction.commit().await?;
        }
        if !user.enrolled {
            let reply = send_invitation(identity, &user.principal_id).await?;
            if reply.status != 201 {
                return Err(Refusal::Failed(anyhow::anyhow!(
                    "identity did not accept the invitation for delivery: {}",
                    reply.status
                )));
            }
        }
        Ok(json!({
            "principal_id": user.principal_id.as_str(),
            "enrolled": user.enrolled,
            "invited": !user.enrolled,
        }))
    }

    /// Open the write transaction of one route: bind the caller and the
    /// sealed operation id, and check `org-admin` again inside it.
    async fn begin<'c>(
        &self,
        writer: &'c mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
    ) -> Result<Transaction<'c>, Refusal> {
        let transaction = writer.transaction().await?;
        transaction
            .execute(
                "SELECT pg_catalog.set_config('app.user_id', $1, true), \
                 pg_catalog.set_config('app.operation', $2, true)",
                &[&caller.as_str(), &attachment.operation],
            )
            .await?;
        if !is_org_admin(&transaction, caller, self.org).await? {
            return Err(denied(attachment));
        }
        Ok(transaction)
    }
}

/// The principal a write route names.
fn principal(payload: &str) -> Result<PrincipalId, Refusal> {
    let request: PrincipalRequest = serde_json::from_str(payload)
        .map_err(|_| Refusal::Delivery(DeliveryError::InvalidPayload))?;
    Ok(request.principal_id.parse()?)
}

fn denied(attachment: &HostAttachment) -> Refusal {
    Refusal::Delivery(DeliveryError::PermissionDenied(PermissionDenial {
        operation: attachment.operation.clone(),
    }))
}

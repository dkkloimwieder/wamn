//! Control user routes and their identity adapter calls.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use wamn_authentication::identity_error::IdentityRefusal;
use wamn_authentication::principal::require_user_contact;
use wamn_identity_client::users::{DisableOutcome, UserClient, UserRefused};
use wamn_identity_protocol::records::MemberGrants;
use wamn_identity_protocol::{
    PrincipalId,
    wire::{UserRequest, UserResponse},
};
use wamn_routes::HostAttachment;
use wamn_postgres::identity::org::invite_member;

use super::{ControlRoutes, Refusal, parse, principal};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteRequest {
    email: String,
    display_name: String,
    #[serde(default)]
    org_admin: bool,
    #[serde(default)]
    project_admins: Vec<wamn_registry::ProjectId>,
    #[serde(default)]
    memberships: Vec<MembershipRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipRequest {
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
}

impl ControlRoutes<'_> {
    fn identity_users(&self) -> Result<&UserClient, Refusal> {
        Ok(self
            .identity
            .context("the control host has no identity user client")?)
    }

    pub(super) async fn create_user(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: UserRequest = parse(payload)?;
        let user = create_identity(
            self.identity_users()?,
            &request.email,
            &request.display_name,
        )
        .await?;
        let mut writer = self.writer.lock().await?;
        self.write_user_membership(
            &mut writer,
            attachment,
            caller,
            &user.principal_id,
            &MemberGrants::default(),
        )
        .await?;
        Ok(json!(user))
    }

    pub(super) async fn disable_user(&self, payload: &str) -> Result<Value, Refusal> {
        let principal = principal(payload)?;
        let code = match self.identity_users()?.disable(&principal).await? {
            DisableOutcome::Disabled => {
                return Ok(json!({"principal_id": principal, "status": "disabled"}));
            }
            DisableOutcome::UserNotFound => "user_not_found",
            DisableOutcome::UserInAnotherOrg => "user_in_another_org",
        };
        Err(Refusal::Declared {
            code,
            detail: json!({"field": "principal_id"}),
        })
    }

    async fn write_user_membership(
        &self,
        writer: &mut PgConnection,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        user: &PrincipalId,
        grants: &MemberGrants,
    ) -> Result<(), Refusal> {
        let mut transaction = self.begin(writer, attachment, caller).await?;
        invite_member(&mut transaction, user, self.org, grants)
            .await
            .map_err(|error| {
                if error.refusal() == Some(IdentityRefusal::UserInAnotherOrg) {
                    Refusal::Declared {
                        code: "user_in_another_org",
                        detail: json!({"field": "email"}),
                    }
                } else {
                    error.into()
                }
            })?;
        // Activation holds the principal lock. Refuse if disable won the race
        // between identity's successful response and this membership write.
        let active = wamn_postgres::control::principal_active(&mut transaction, user).await?;
        if !active {
            return Err(user_refused());
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Identity creates or reuses the user, one transaction writes the org
    /// membership and the grants, and identity mails the invitation only when
    /// the user has no password.
    pub(super) async fn invite(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let request: InviteRequest = parse(payload)?;
        let identity = self.identity_users()?;
        let user = create_identity(identity, &request.email, &request.display_name).await?;
        let grants = MemberGrants {
            org_admin: request.org_admin,
            project_admins: request.project_admins,
            memberships: request
                .memberships
                .into_iter()
                .map(|membership| (membership.project, membership.env))
                .collect(),
        };
        // The lock stays held through the application rows, so no other
        // write of this host runs between the system rows and them.
        let mut writer = self.writer.lock().await?;
        self.write_user_membership(&mut writer, attachment, caller, &user.principal_id, &grants)
            .await?;
        let mut environments: std::collections::BTreeSet<(
            wamn_registry::ProjectId,
            wamn_registry::EnvName,
        )> = grants.memberships.into_iter().collect();
        let admin_targets: Vec<Option<&wamn_registry::ProjectId>> = if grants.org_admin {
            vec![None]
        } else {
            grants.project_admins.iter().map(Some).collect()
        };
        for project in admin_targets {
            environments.extend(self.environments(caller, project).await?);
        }
        let environments: Vec<_> = environments.into_iter().collect();
        self.write_applications(
            &mut writer,
            attachment,
            caller,
            &user.principal_id,
            &environments,
        )
        .await?;
        drop(writer);
        if !user.enrolled {
            identity.invite(&user.principal_id).await?;
        }
        Ok(json!({
            "principal_id": user.principal_id,
            "enrolled": user.enrolled,
            "invited": !user.enrolled,
        }))
    }
}

async fn create_identity(
    identity: &UserClient,
    email: &str,
    display_name: &str,
) -> Result<UserResponse, Refusal> {
    require_user_contact(email, display_name)?;
    identity.create(email, display_name).await.map_err(|error| {
        if error.is::<UserRefused>() {
            user_refused()
        } else {
            Refusal::Failed(error)
        }
    })
}

fn user_refused() -> Refusal {
    Refusal::Declared {
        code: "user_refused",
        detail: json!({"field": "email"}),
    }
}

/// Grant the first owner under the provisioning actor and invite them until enrolled.
pub(crate) async fn first_owner(
    handlers: &crate::host_route::HostRouteHandlers,
    email: &str,
) -> anyhow::Result<hyper::StatusCode> {
    use hyper::StatusCode;
    use wamn_authentication::control_admission::ORG_ADMIN_ROLE;

    let display_name = email.trim().split('@').next().unwrap_or_default();
    let display_name = &display_name[..display_name.floor_char_boundary(200)];
    if require_user_contact(email, display_name).is_err() {
        return Ok(StatusCode::BAD_REQUEST);
    }

    let org = &handlers.org;
    let mut writer = handlers.writer.lock().await?;
    let mut tx = wamn_platform_install::transaction::provisioning_transaction(&mut writer).await?;
    let registered = wamn_postgres::control::login_matches_org(&mut tx, org).await?;
    if !registered {
        return Ok(StatusCode::FORBIDDEN);
    }
    wamn_postgres::control::lock_first_owner(&mut tx, org).await?;
    let (has_owner, same_owner) = wamn_postgres::control::org_role_holders(
        &mut tx,
        org,
        ORG_ADMIN_ROLE,
        &email.trim().to_lowercase(),
    )
    .await?;
    if has_owner && !same_owner {
        return Ok(StatusCode::CONFLICT);
    }

    let identity = handlers
        .identity
        .as_ref()
        .context("control has no identity user client")?;
    let user = match create_identity(identity, email, display_name).await {
        Ok(user) => user,
        Err(Refusal::Declared { .. }) => return Ok(StatusCode::BAD_REQUEST),
        Err(Refusal::Failed(error)) => return Err(error),
        Err(Refusal::Delivery(_)) => unreachable!("identity creation does not route requests"),
    };
    if !has_owner {
        let grants = MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        };
        if let Err(error) = invite_member(&mut tx, &user.principal_id, org, &grants).await {
            if error.refusal() == Some(IdentityRefusal::UserInAnotherOrg) {
                return Ok(StatusCode::CONFLICT);
            }
            return Err(error.into());
        }
        let active = wamn_postgres::control::principal_active(&mut tx, &user.principal_id).await?;
        if !active {
            return Ok(StatusCode::BAD_REQUEST);
        }
    }
    tx.commit().await?;
    drop(writer);
    if !user.enrolled {
        identity.invite(&user.principal_id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

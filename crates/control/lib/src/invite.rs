//! Provision the first admin of an org through SQL
//! (docs/architecture/overview.md#organization-membership-and-administration).
//!
//! An active org admin with another email refuses this path and directs callers
//! to user.invite. provision-org makes the owner the org admin without a
//! password, so the owner's own invitation still passes.
//! Identity creates or reuses the user by email. One system database
//! transaction, as `wamn_system` bound to `wamn:provisioning`, then writes the
//! org membership and the requested grants through the one write of each
//! grant. Identity mails the invitation only when the user has no password
//! credential.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control_provision::validate_project_env;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::{
    PrincipalId,
    org::{MemberGrants, invite_member},
};

use crate::pat_client::{self, InvitationReply, PatIssuerConfig};
use crate::provision_project_env::provisioning_transaction;

/// The user, the org and the grants of one invitation.
#[derive(Debug)]
pub struct InviteRequest {
    /// The user's email.
    pub email: String,
    /// The display name of a new user. A reused user keeps its own.
    pub display_name: String,
    /// The org the user joins.
    pub org: String,
    /// The grants beside the org membership.
    pub grants: MemberGrants,
    /// Provisioning administrator URL for the system database.
    pub system_database_url: String,
    /// The operator client of the identity service.
    pub identity: PatIssuerConfig,
}

/// What one invitation did.
#[derive(Debug)]
pub struct InviteOutcome {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// Identity's reply to the invitation mail, or `None` when the user was
    /// enrolled and no invitation was sent.
    pub invitation: Option<InvitationReply>,
}

/// Provision the first org admin. Later invitations must use user.invite.
pub async fn invite(
    request: &InviteRequest,
    changes: &AuthChanges,
) -> anyhow::Result<InviteOutcome> {
    anyhow::ensure!(
        request.grants.org_admin,
        "invite provisions the first org admin; use user.invite for other invitations"
    );
    for (project, env) in &request.grants.memberships {
        validate_project_env(&request.org, project, env)
            .with_context(|| format!("invalid --membership {project}/{env}"))?;
    }
    let (mut client, connection) = tokio_postgres::connect(&request.system_database_url, NoTls)
        .await
        .context("connect to the system database for the invitation")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for the invitation")?;
        let transaction = provisioning_transaction(&mut client).await?;
        transaction
            .query(
                "SELECT id FROM registry.orgs WHERE id = $1 FOR UPDATE",
                &[&request.org],
            )
            .await
            .context("lock the org for its first admin")?;
        let has_admin: bool = transaction
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM identity.org_roles r \
                 JOIN identity.org_memberships m USING (principal_id, org) \
                 JOIN identity.principals p ON p.id = r.principal_id \
                 WHERE r.org = $1 AND r.role = 'org-admin' AND m.status = 'active' \
                 AND p.status = 'active' AND p.email <> lower(btrim($2)))",
                &[&request.org, &request.email],
            )
            .await
            .context("read the active org admin")?
            .get(0);
        anyhow::ensure!(
            !has_admin,
            "org {} already has an active org admin; use user.invite",
            request.org
        );
        let user =
            pat_client::create_user(&request.identity, &request.email, &request.display_name)
                .await?;
        invite_member(
            &transaction,
            &user.principal_id,
            &request.org,
            &request.grants,
        )
        .await
        .context("write the org membership and the grants")?;
        let mut drops = AuthDrops::default();
        drops.identity(user.principal_id.as_str());
        transaction
            .commit()
            .await
            .context("commit the invitation grants")?;
        changes.publish(drops).await;
        Ok::<_, anyhow::Error>(user)
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    let user = result?;
    let invitation = if user.enrolled {
        None
    } else {
        let reply = pat_client::send_invitation(&request.identity, &user.principal_id).await?;
        anyhow::ensure!(
            reply.status == 201,
            "identity did not accept the invitation for delivery: {} {}",
            reply.status,
            reply.body.trim()
        );
        Some(reply)
    };
    Ok(InviteOutcome {
        principal_id: user.principal_id,
        invitation,
    })
}

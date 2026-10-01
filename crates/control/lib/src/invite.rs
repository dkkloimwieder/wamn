//! Invite one user to an org (docs/plan/platform-ui.md §4.4 `user.invite`).
//!
//! Identity creates or reuses the user by email. One system database
//! transaction, as `wamn_system` bound to `wamn:provisioning`, then writes the
//! org membership and the requested grants through the one write of each
//! grant. Identity mails the invitation only when the user has no password
//! credential.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_control_provision::validate_project_env;
use wamn_platform_identity::{
    PrincipalId, grant_project_env_membership,
    org::{activate_org_membership, grant_org_admin, grant_project_admin},
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
    /// Grant `org-admin` in the org.
    pub org_admin: bool,
    /// Projects of the org where the user gets `project-admin`.
    pub project_admins: Vec<String>,
    /// Environments of the org, as `(project, env)`, where the user gets a
    /// membership.
    pub memberships: Vec<(String, String)>,
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

/// Run the three steps of one invitation.
pub async fn invite(request: &InviteRequest) -> anyhow::Result<InviteOutcome> {
    for (project, env) in &request.memberships {
        validate_project_env(&request.org, project, env)
            .with_context(|| format!("invalid --membership {project}/{env}"))?;
    }
    let user =
        pat_client::create_user(&request.identity, &request.email, &request.display_name).await?;
    write_grants(request, &user.principal_id).await?;
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

/// Write the org membership and the grants in one transaction.
async fn write_grants(request: &InviteRequest, principal_id: &PrincipalId) -> anyhow::Result<()> {
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
        let org = request.org.as_str();
        activate_org_membership(&transaction, principal_id, org)
            .await
            .context("write the org membership")?;
        if request.org_admin {
            grant_org_admin(&transaction, principal_id, org)
                .await
                .context("grant org-admin")?;
        }
        for project in &request.project_admins {
            grant_project_admin(&transaction, principal_id, org, project)
                .await
                .with_context(|| format!("grant project-admin in {project}"))?;
        }
        for (project, env) in &request.memberships {
            grant_project_env_membership(&transaction, principal_id, org, project, env)
                .await
                .with_context(|| format!("grant the membership of {project}/{env}"))?;
        }
        transaction
            .commit()
            .await
            .context("commit the invitation grants")
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

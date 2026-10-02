//! The org and project routes of the control serving root
//! (docs/plan/platform-ui.md §4.4 and §4.5).
//!
//! Every org route needs a current `org-admin` row of the caller in the
//! token's org. A project route needs that row or a current `project-admin`
//! row of the caller in the project that the request names. A write runs in
//! one transaction on the org's `control` login, binds `app.user_id` to the
//! caller and `app.operation` to the route's sealed operation id, and checks
//! the role again inside it. The writes are the functions of
//! `wamn_platform_identity::org`, which `wamn-ctl invite` also calls, so no
//! second writer exists.
//!
//! A write that changes administrative authority or membership also changes
//! the application rows of each environment it covers, with the
//! environment's `wamn_administration` login from the mounted Secret
//! `wamn-control-administration-<org>`, one transaction per environment
//! (§4.4, application writes). A grant commits its system transaction first.
//! A revoke or a deactivation writes the environments first and commits its
//! system transaction last, so its refusals come before any application row
//! changes. The route reports success only when every environment is done.
//!
//! The status routes mirror the status of each environment into its row of
//! `app_system.environment` in the same way (§5.4): an inactivation writes
//! the environments first and commits the system rows last, and an
//! activation commits the system rows first.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{HostAttachment, HostHandler, HostRouteAuthority};
use wamn_control_provision::control_administration_key;
use wamn_engine::router_delivery::{DeliveryError, PermissionDenial};
use wamn_identity_client::{PatIssuerConfig, UserRefused, create_user, send_invitation};
use wamn_platform_identity::application::{
    ApplicationUser, environment_tenant, remove_admin, remove_user, write_admin,
    write_environment_status, write_user,
};
use wamn_platform_identity::control::{
    control_projects, is_org_admin, is_project_admin, org_projects,
};
use wamn_platform_identity::org::{
    EnvironmentStatus, MemberGrants, deactivate_org_membership, grant_member, grant_org_admin,
    grant_project_admin, invite_member, org_environments, org_users, project_envs, project_members,
    reactivate_org_membership, revoke_member, revoke_org_admin, revoke_project_admin,
    set_environment_status, status_environments, user_contact,
};
use wamn_platform_identity::{
    IdentityError, IdentityErrorType, IdentityRefusal, PrincipalId, check_user_contact,
};

/// Why an org route did not answer.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A delivery refusal, such as a caller without `org-admin`.
    Delivery(DeliveryError),
    /// A refusal that the route's contract declares: its code and its
    /// detail, answered as the error of the request item.
    Declared { code: &'static str, detail: Value },
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

/// A refusal of the identity writes is the declared error of the route, with
/// the input member it names in `field`. Any other error is a failure.
impl From<IdentityError> for Refusal {
    fn from(error: IdentityError) -> Self {
        let (code, field) = match error.refusal() {
            Some(IdentityRefusal::Invalid(field)) => ("invalid_input", field),
            Some(IdentityRefusal::ProjectNotFound) => ("project_not_found", "project"),
            Some(IdentityRefusal::EnvironmentNotFound) => ("environment_not_found", "env"),
            Some(IdentityRefusal::UserNotActive) => ("user_not_active", "principal_id"),
            Some(IdentityRefusal::UserNotFound) => ("user_not_found", "principal_id"),
            Some(IdentityRefusal::AdminCovered) => ("admin_covered", "principal_id"),
            None => return Self::Failed(error.into()),
        };
        Self::Declared {
            code,
            detail: json!({ "field": field }),
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
    /// The mounted Secret of the org's administration logins, one file per
    /// environment.
    pub(crate) administration: Option<&'a Path>,
    pub(crate) org: &'a str,
}

/// What a write does to the application rows of one environment. A later
/// variant of a grant covers an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ApplicationWrite {
    User,
    Admin,
    RemoveAdmin,
    RemoveUser,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalRequest {
    principal_id: String,
}

/// The project that a project route names, read before its own request.
#[derive(Deserialize)]
struct ProjectScope {
    project: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    project: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectPrincipalRequest {
    project: String,
    principal_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberRequest {
    project: String,
    env: String,
    principal_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentRequest {
    project: String,
    env: String,
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
    /// Serve one org or project route for `caller`.
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let admitted = match attachment.route.authority {
            HostRouteAuthority::OrgAdmin => self.admits(self.control, caller, None).await?,
            HostRouteAuthority::ProjectAdmin => {
                let project = parse::<ProjectScope>(payload)?.project;
                self.admits(self.control, caller, Some(&project)).await?
            }
            // A read for every control caller: `org-admin`, or `project-admin`
            // of a project of the org.
            HostRouteAuthority::Member => {
                self.admits(self.control, caller, None).await?
                    || !control_projects(self.control, caller, self.org)
                        .await?
                        .is_empty()
            }
            authority @ HostRouteAuthority::Admin => {
                return Err(Refusal::Failed(anyhow::anyhow!(
                    "the control host does not admit the authority {authority:?}"
                )));
            }
        };
        if !admitted {
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
                let environments = self
                    .environments(None, ApplicationWrite::RemoveUser)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "status": "inactive" }))
            }
            HostHandler::OrgAdminGrant => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                grant_org_admin(&transaction, &principal, self.org).await?;
                transaction.commit().await?;
                let environments = self.environments(None, ApplicationWrite::Admin).await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({ "principal_id": principal.as_str(), "org_admin": true }))
            }
            HostHandler::OrgAdminRevoke => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                revoke_org_admin(&transaction, &principal, self.org).await?;
                let environments = self
                    .environments(None, ApplicationWrite::RemoveAdmin)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.as_str(), "org_admin": false }))
            }
            HostHandler::EnvironmentList => {
                let request: ProjectRequest = parse(payload)?;
                let environments = project_envs(self.control, self.org, &request.project).await?;
                Ok(json!({ "environments": environments }))
            }
            HostHandler::MemberList => {
                let request: ProjectRequest = parse(payload)?;
                let members = project_members(self.control, self.org, &request.project).await?;
                Ok(json!({ "members": members }))
            }
            HostHandler::MemberGrant => {
                let request: MemberRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id.parse()?;
                let mut writer = self.writer.lock().await;
                let transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                grant_member(
                    &transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &request.env,
                )
                .await?;
                transaction.commit().await?;
                let environment = [(
                    (request.project.clone(), request.env.clone()),
                    ApplicationWrite::User,
                )];
                self.write_applications(attachment, caller, &principal, &environment)
                    .await?;
                Ok(json!({
                    "principal_id": principal.as_str(),
                    "project": request.project,
                    "env": request.env,
                    "member": true,
                }))
            }
            HostHandler::MemberRevoke => {
                let request: MemberRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id.parse()?;
                let mut writer = self.writer.lock().await;
                let transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                revoke_member(
                    &transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &request.env,
                )
                .await?;
                let environment = [(
                    (request.project.clone(), request.env.clone()),
                    ApplicationWrite::RemoveUser,
                )];
                self.write_applications(attachment, caller, &principal, &environment)
                    .await?;
                transaction.commit().await?;
                Ok(json!({
                    "principal_id": principal.as_str(),
                    "project": request.project,
                    "env": request.env,
                    "member": false,
                }))
            }
            HostHandler::ProjectAdminGrant => {
                let request: ProjectPrincipalRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id.parse()?;
                let mut writer = self.writer.lock().await;
                let transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                grant_project_admin(&transaction, &principal, self.org, &request.project).await?;
                transaction.commit().await?;
                let environments = self
                    .environments(Some(&request.project), ApplicationWrite::Admin)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({
                    "principal_id": principal.as_str(),
                    "project": request.project,
                    "project_admin": true,
                }))
            }
            HostHandler::ProjectAdminRevoke => {
                let request: ProjectPrincipalRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id.parse()?;
                let mut writer = self.writer.lock().await;
                let transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                revoke_project_admin(&transaction, &principal, self.org, &request.project).await?;
                let environments = self
                    .environments(Some(&request.project), ApplicationWrite::RemoveAdmin)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                Ok(json!({
                    "principal_id": principal.as_str(),
                    "project": request.project,
                    "project_admin": false,
                }))
            }
            HostHandler::EnvironmentActivate | HostHandler::EnvironmentInactivate => {
                let request: EnvironmentRequest = parse(payload)?;
                let status = if attachment.route.handler == HostHandler::EnvironmentActivate {
                    EnvironmentStatus::Active
                } else {
                    EnvironmentStatus::Inactive
                };
                self.set_status(
                    attachment,
                    caller,
                    &request.project,
                    Some(&request.env),
                    status,
                )
                .await?;
                Ok(json!({
                    "project": request.project,
                    "env": request.env,
                    "status": status.as_str(),
                }))
            }
            HostHandler::ProjectActivate | HostHandler::ProjectInactivate => {
                let request: ProjectRequest = parse(payload)?;
                let status = if attachment.route.handler == HostHandler::ProjectActivate {
                    EnvironmentStatus::Active
                } else {
                    EnvironmentStatus::Inactive
                };
                self.set_status(attachment, caller, &request.project, None, status)
                    .await?;
                Ok(json!({ "project": request.project, "status": status.as_str() }))
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
        check_user_contact(&request.email, &request.display_name)?;
        let identity = self.identity.ok_or_else(|| {
            Refusal::Failed(anyhow::anyhow!(
                "the control host has no identity operator client"
            ))
        })?;
        let user = create_user(identity, &request.email, &request.display_name)
            .await
            .map_err(|error| match error.downcast::<UserRefused>() {
                // The input passed the identity rules above, so identity
                // refuses the user that holds the email, such as a disabled
                // one.
                Ok(_) => Refusal::Declared {
                    code: "user_refused",
                    detail: json!({ "field": "email" }),
                },
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
        // The lock stays held through the application rows, so no other
        // write of this host runs between the system rows and them.
        let mut writer = self.writer.lock().await;
        let transaction = self.begin(&mut writer, attachment, caller).await?;
        invite_member(&transaction, &user.principal_id, self.org, &grants).await?;
        transaction.commit().await?;
        // The application rows of the grants, each environment once, with
        // `admin` where an administrative grant covers it.
        let mut environments: BTreeMap<(String, String), ApplicationWrite> = BTreeMap::new();
        for environment in grants.memberships {
            environments.insert(environment, ApplicationWrite::User);
        }
        let admin_scopes: Vec<Option<&str>> = if grants.org_admin {
            vec![None]
        } else {
            grants
                .project_admins
                .iter()
                .map(|p| Some(p.as_str()))
                .collect()
        };
        for project in admin_scopes {
            for (environment, _) in self.environments(project, ApplicationWrite::Admin).await? {
                environments.insert(environment, ApplicationWrite::Admin);
            }
        }
        let environments: Vec<_> = environments.into_iter().collect();
        self.write_applications(attachment, caller, &user.principal_id, &environments)
            .await?;
        drop(writer);
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

    /// Write the status of one environment, or of every environment of a
    /// project, in the system rows and in each environment's row. An
    /// inactivation writes the environments first and commits the system
    /// rows last. An activation commits the system rows first.
    async fn set_status(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        project: &str,
        env: Option<&str>,
        status: EnvironmentStatus,
    ) -> Result<(), Refusal> {
        let mut writer = self.writer.lock().await;
        let transaction = self.begin(&mut writer, attachment, caller).await?;
        let envs = status_environments(&transaction, self.org, project, env).await?;
        let environments: Vec<(String, String)> = envs
            .iter()
            .map(|env| (project.to_owned(), env.clone()))
            .collect();
        if status == EnvironmentStatus::Inactive {
            self.write_statuses(attachment, caller, &environments, status)
                .await?;
        }
        set_environment_status(&transaction, self.org, project, &envs, status).await?;
        transaction.commit().await?;
        if status == EnvironmentStatus::Active {
            self.write_statuses(attachment, caller, &environments, status)
                .await?;
        }
        Ok(())
    }

    /// Write the status row of each environment, one transaction each, in
    /// order. When an environment fails, the route refuses and names the
    /// environments that completed.
    async fn write_statuses(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        environments: &[(String, String)],
        status: EnvironmentStatus,
    ) -> Result<(), Refusal> {
        let logins = self.logins(environments)?;
        let mut completed: Vec<String> = Vec::new();
        for ((project, env), url) in environments.iter().zip(logins) {
            if let Err(error) = write_status(&url, attachment, caller, status).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    project,
                    env,
                    reference = attachment.reference,
                    "an environment status write failed"
                );
                return Err(incomplete(project, env, &completed));
            }
            completed.push(format!("{project}/{env}"));
        }
        Ok(())
    }

    /// The administration login of each environment, all read before the
    /// first write.
    fn logins(&self, environments: &[(String, String)]) -> Result<Vec<String>, Refusal> {
        if environments.is_empty() {
            return Ok(Vec::new());
        }
        let directory = self.administration.ok_or_else(|| {
            Refusal::Failed(anyhow::anyhow!(
                "the control host has no administration logins"
            ))
        })?;
        let mut logins = Vec::with_capacity(environments.len());
        for (project, env) in environments {
            let path = directory.join(control_administration_key(project, env));
            match std::fs::read_to_string(&path) {
                Ok(url) => logins.push(url.trim().to_owned()),
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "no administration login");
                    return Err(incomplete(project, env, &[]));
                }
            }
        }
        Ok(logins)
    }

    /// Every environment of the org, or of one project, with one write.
    async fn environments(
        &self,
        project: Option<&str>,
        write: ApplicationWrite,
    ) -> Result<Vec<((String, String), ApplicationWrite)>, Refusal> {
        Ok(org_environments(self.control, self.org, project)
            .await?
            .into_iter()
            .map(|environment| (environment, write))
            .collect())
    }

    /// Write the application rows of `principal` in each environment, one
    /// transaction each, in order. Every login is read before the first
    /// write. When an environment fails, the route refuses and names the
    /// environments that completed.
    async fn write_applications(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        principal: &PrincipalId,
        environments: &[((String, String), ApplicationWrite)],
    ) -> Result<(), Refusal> {
        if environments.is_empty() {
            return Ok(());
        }
        let grants = environments
            .iter()
            .any(|(_, write)| *write <= ApplicationWrite::Admin);
        let contact = if grants {
            Some(user_contact(self.control, principal).await?)
        } else {
            None
        };
        let scopes: Vec<(String, String)> = environments
            .iter()
            .map(|(environment, _)| environment.clone())
            .collect();
        let logins = self.logins(&scopes)?;
        let mut completed: Vec<String> = Vec::new();
        for (((project, env), write), url) in environments.iter().zip(logins) {
            let user = contact
                .as_ref()
                .map(|(email, display_name)| ApplicationUser {
                    principal_id: principal,
                    email,
                    display_name,
                });
            if let Err(error) =
                write_application(&url, attachment, caller, principal, user, *write).await
            {
                tracing::warn!(
                    error = format!("{error:#}"),
                    project,
                    env,
                    reference = attachment.reference,
                    "an application write failed"
                );
                return Err(incomplete(project, env, &completed));
            }
            completed.push(format!("{project}/{env}"));
        }
        Ok(())
    }

    /// Open the write transaction of one org route.
    async fn begin<'c>(
        &self,
        writer: &'c mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
    ) -> Result<Transaction<'c>, Refusal> {
        self.begin_in(writer, attachment, caller, None).await
    }

    /// Open the write transaction of one route: bind the caller and the
    /// sealed operation id, and check the route's role again inside it.
    async fn begin_in<'c>(
        &self,
        writer: &'c mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        project: Option<&str>,
    ) -> Result<Transaction<'c>, Refusal> {
        let transaction = writer.transaction().await?;
        transaction
            .execute(
                "SELECT pg_catalog.set_config('app.user_id', $1, true), \
                 pg_catalog.set_config('app.operation', $2, true)",
                &[&caller.as_str(), &attachment.operation],
            )
            .await?;
        if !self.admits(&transaction, caller, project).await? {
            return Err(denied(attachment));
        }
        Ok(transaction)
    }

    /// Whether `caller` holds `org-admin` in the org, or `project-admin` in
    /// `project` when a project route names one.
    async fn admits(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        caller: &PrincipalId,
        project: Option<&str>,
    ) -> Result<bool, Refusal> {
        if is_org_admin(client, caller, self.org).await? {
            return Ok(true);
        }
        Ok(match project {
            Some(project) => is_project_admin(client, caller, self.org, project).await?,
            None => false,
        })
    }
}

/// One environment's application write, in its own transaction on its
/// administration login.
async fn write_application(
    url: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    principal: &PrincipalId,
    user: Option<ApplicationUser<'_>>,
    write: ApplicationWrite,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect the administration login")?;
    let connection = tokio::spawn(connection);
    let result = async {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "SELECT pg_catalog.set_config('app.user_id', $1, true), \
                 pg_catalog.set_config('app.operation', $2, true)",
                &[&caller.as_str(), &attachment.operation],
            )
            .await?;
        let tenant = match environment_tenant(&transaction).await {
            Ok(tenant) => tenant,
            // A database without platform rows holds no user row to remove.
            Err(error)
                if error.error_type() == IdentityErrorType::NotFound
                    && write >= ApplicationWrite::RemoveAdmin =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        match (write, user) {
            (ApplicationWrite::User, Some(user)) => {
                write_user(&transaction, &tenant, user).await?;
            }
            (ApplicationWrite::Admin, Some(user)) => {
                write_admin(&transaction, &tenant, user).await?;
            }
            (ApplicationWrite::RemoveAdmin, _) => {
                remove_admin(&transaction, &tenant, principal).await?;
            }
            (ApplicationWrite::RemoveUser, _) => {
                remove_user(&transaction, &tenant, principal).await?;
            }
            (write, None) => anyhow::bail!("{write:?} needs the user's email and display name"),
        }
        transaction.commit().await?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// One environment's status row, in its own transaction on its
/// administration login.
async fn write_status(
    url: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    status: EnvironmentStatus,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect the administration login")?;
    let connection = tokio::spawn(connection);
    let result = async {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "SELECT pg_catalog.set_config('app.user_id', $1, true), \
                 pg_catalog.set_config('app.operation', $2, true)",
                &[&caller.as_str(), &attachment.operation],
            )
            .await?;
        let tenant = environment_tenant(&transaction).await?;
        write_environment_status(&transaction, &tenant, status).await?;
        transaction.commit().await?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// The request of a route, or the delivery refusal of a payload that is not
/// one.
pub(crate) fn parse<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, Refusal> {
    serde_json::from_str(payload).map_err(|_| Refusal::Delivery(DeliveryError::InvalidPayload))
}

/// The principal a write route names.
fn principal(payload: &str) -> Result<PrincipalId, Refusal> {
    let request: PrincipalRequest = serde_json::from_str(payload)
        .map_err(|_| Refusal::Delivery(DeliveryError::InvalidPayload))?;
    Ok(request.principal_id.parse()?)
}

/// The refusal of a write whose application rows stopped at `project/env`,
/// with the environments that completed.
fn incomplete(project: &str, env: &str, completed: &[String]) -> Refusal {
    Refusal::Declared {
        code: "application_write_incomplete",
        detail: json!({
            "environment": format!("{project}/{env}"),
            "completed": completed,
        }),
    }
}

fn denied(attachment: &HostAttachment) -> Refusal {
    Refusal::Delivery(DeliveryError::PermissionDenied(PermissionDenial {
        operation: attachment.operation.clone(),
    }))
}

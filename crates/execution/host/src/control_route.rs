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
//! `platform.environment` in the same way (§5.4): an inactivation writes
//! the environments first and commits the system rows last, and an
//! activation commits the system rows first.
//!
//! `project.create` writes a project of the org with no environment, and
//! `project-admin` in it for each `org-admin` of the org (§5.1).
//!
//! `environment.status` reads the runtime through the host-selected
//! [`crate::environment_status::StatusSource`] and writes nothing. Optional
//! counts read the environment through its administration login and the
//! control store through its existing connection. Count failures answer null.
//!
//! `release.profile` reads one installed release of one environment of the
//! project from its `catalog.releases`, and the columns of its application
//! tables from `pg_catalog`, on the environment's administration login, and
//! answers the release's effect profile, with the operations each role of the
//! environment holds (`admin` holds every operation of the release), from its
//! `platform.roles` and `platform.permissions`. It writes nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{HostAttachment, HostHandler, HostRouteAuthority};
use wamn_control_provision::control_administration_key;
use wamn_control_registry::identifiers::valid_project;
use wamn_engine::router_delivery::{DeliveryError, PermissionDenial};
use wamn_identity_client::{PatIssuerConfig, UserRefused, create_user, send_invitation};
use wamn_platform_identity::application::{
    ApplicationUser, environment_tenant, permission_rows, remove_admin, remove_user, role_names,
    write_admin, write_environment_status, write_user,
};
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_platform_identity::control::{
    control_projects_page, is_org_admin, is_project_admin, org_projects_page,
};
use wamn_platform_identity::org::{
    EnvironmentStatus, MemberGrants, create_project, deactivate_org_membership, grant_member,
    grant_org_admin, grant_project_admin, invite_member, org_environments, org_users_page,
    project_envs, project_envs_page, project_members_page, reactivate_org_membership,
    revoke_member, revoke_org_admin, revoke_project_admin, set_environment_status,
    status_environments, user_contact,
};
use wamn_platform_identity::paging::{ListPage, Order};
use wamn_platform_identity::{
    IdentityError, IdentityErrorType, IdentityRefusal, PrincipalId, check_user_contact,
};
use wamn_project_state::ADMIN_ROLE;

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
            Some(IdentityRefusal::ProjectExists) => ("project_exists", "project"),
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
    /// The runtime reader selected by the host mode.
    pub(crate) status: &'a dyn crate::environment_status::StatusSource,
    pub(crate) org: &'a str,
    /// Publishes each users row and status change of an environment.
    pub(crate) changes: &'a AuthChanges,
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
struct ReleaseRequest {
    project: String,
    env: String,
    digest: String,
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
                    || !control_projects_page(self.control, caller, self.org, None, Some(1))
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
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    None,
                    Order::Principal,
                )?;
                let users = org_users_page(
                    self.control,
                    self.org,
                    page.after(0),
                    page.after(1),
                    page.read_limit(),
                )
                .await?;
                Ok(page.answer("users", users, |row| json!([row.email, row.principal_id])))
            }
            HostHandler::ProjectList => {
                let page =
                    control_page(payload, attachment, caller, self.org, None, Order::Project)?;
                let projects =
                    org_projects_page(self.control, self.org, page.after(0), page.read_limit())
                        .await?;
                Ok(page.answer("projects", projects, |row| json!([row.name])))
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
                let mut drops = AuthDrops::default();
                deactivate_org_membership(&transaction, &principal, self.org, &mut drops).await?;
                let environments = self
                    .environments(None, ApplicationWrite::RemoveUser)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                self.changes.publish(drops).await;
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
                let mut drops = AuthDrops::default();
                revoke_org_admin(&transaction, &principal, self.org, &mut drops).await?;
                let environments = self
                    .environments(None, ApplicationWrite::RemoveAdmin)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                self.changes.publish(drops).await;
                Ok(json!({ "principal_id": principal.as_str(), "org_admin": false }))
            }
            HostHandler::EnvironmentList => {
                let request: ProjectScope = parse(payload)?;
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    Some(&request.project),
                    Order::Environment,
                )?;
                let environments = project_envs_page(
                    self.control,
                    self.org,
                    &request.project,
                    page.after(0),
                    page.read_limit(),
                )
                .await?;
                Ok(page.answer("environments", environments, |row| json!([row.name])))
            }
            HostHandler::EnvironmentStatus => {
                let request: EnvironmentRequest = parse(payload)?;
                if !project_envs(self.control, self.org, &request.project)
                    .await?
                    .iter()
                    .any(|entry| entry.name == request.env)
                {
                    return Err(Refusal::Declared {
                        code: "environment_not_found",
                        detail: json!({ "field": "env" }),
                    });
                }
                let status = self
                    .status
                    .read(self.org, &request.project, &request.env)
                    .await?;
                let application = async {
                    let logins = self
                        .logins(&[(request.project.clone(), request.env.clone())])
                        .map_err(|_| {
                            anyhow::anyhow!("no administration login for status counts")
                        })?;
                    application_counts(&logins[0]).await
                };
                let memberships = async {
                    let row = self
                        .control
                        .query_one(
                            "SELECT count(*)::int4 FROM identity.project_env_memberships \
                         WHERE org = $1 AND project = $2 AND env = $3",
                            &[&self.org, &request.project, &request.env],
                        )
                        .await?;
                    anyhow::Ok(row.get::<_, i32>(0))
                };
                Ok(status_with_counts(
                    status,
                    &request.project,
                    &request.env,
                    application,
                    memberships,
                )
                .await)
            }
            HostHandler::ReleaseProfile => {
                let request: ReleaseRequest = parse(payload)?;
                if !project_envs(self.control, self.org, &request.project)
                    .await?
                    .iter()
                    .any(|entry| entry.name == request.env)
                {
                    return Err(Refusal::Declared {
                        code: "environment_not_found",
                        detail: json!({ "field": "env" }),
                    });
                }
                let directory = self.administration.ok_or_else(|| {
                    anyhow::anyhow!("the control host has no administration logins")
                })?;
                let path =
                    directory.join(control_administration_key(&request.project, &request.env));
                let url = std::fs::read_to_string(&path)
                    .with_context(|| format!("read the administration login {}", path.display()))?;
                match release_profile(url.trim(), &request.digest).await? {
                    Some((profile, roles)) => Ok(json!({
                        "digest": request.digest,
                        "operations": profile.operations,
                        "roles": roles
                            .into_iter()
                            .map(|(role, operations)| json!({ "role": role, "operations": operations }))
                            .collect::<Vec<_>>(),
                    })),
                    None => Err(Refusal::Declared {
                        code: "release_not_found",
                        detail: json!({ "field": "digest" }),
                    }),
                }
            }
            HostHandler::MemberList => {
                let request: ProjectScope = parse(payload)?;
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    Some(&request.project),
                    Order::Principal,
                )?;
                let members = project_members_page(
                    self.control,
                    self.org,
                    &request.project,
                    page.after(0),
                    page.after(1),
                    page.read_limit(),
                )
                .await?;
                Ok(page.answer("members", members, |row| {
                    json!([row.email, row.principal_id])
                }))
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
                let mut drops = AuthDrops::default();
                revoke_member(
                    &transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &request.env,
                    &mut drops,
                )
                .await?;
                let environment = [(
                    (request.project.clone(), request.env.clone()),
                    ApplicationWrite::RemoveUser,
                )];
                self.write_applications(attachment, caller, &principal, &environment)
                    .await?;
                transaction.commit().await?;
                self.changes.publish(drops).await;
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
                let mut drops = AuthDrops::default();
                revoke_project_admin(
                    &transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &mut drops,
                )
                .await?;
                let environments = self
                    .environments(Some(&request.project), ApplicationWrite::RemoveAdmin)
                    .await?;
                self.write_applications(attachment, caller, &principal, &environments)
                    .await?;
                transaction.commit().await?;
                self.changes.publish(drops).await;
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
            HostHandler::ProjectCreate => {
                let request: ProjectRequest = parse(payload)?;
                if !valid_project(&request.project) {
                    return Err(Refusal::Declared {
                        code: "invalid_input",
                        detail: json!({ "field": "project" }),
                    });
                }
                let mut writer = self.writer.lock().await;
                let transaction = self.begin(&mut writer, attachment, caller).await?;
                create_project(&transaction, self.org, &request.project).await?;
                transaction.commit().await?;
                Ok(json!({ "project": request.project }))
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
            match write_status(&url, attachment, caller, status).await {
                Ok(drops) => self.changes.publish(drops).await,
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        project,
                        env,
                        reference = attachment.reference,
                        "an environment status write failed"
                    );
                    return Err(incomplete(project, env, &completed));
                }
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
            match write_application(&url, attachment, caller, principal, user, *write).await {
                Ok(drops) => self.changes.publish(drops).await,
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        project,
                        env,
                        reference = attachment.reference,
                        "an application write failed"
                    );
                    return Err(incomplete(project, env, &completed));
                }
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

/// Bound optional count reads to two seconds so unavailable databases do not
/// prevent the runtime status from answering. Each read has its own deadline.
const STATUS_COUNTS_TIMEOUT: Duration = Duration::from_secs(2);

// The platform rows identify the sole tenant, as environment_tenant does.
// query_one refuses missing or multiple tenants. EXISTS counts each human
// user once even when that user holds several roles, and excludes services.
const APPLICATION_COUNTS_SQL: &str = "SELECT \
    (SELECT count(*)::int4 FROM platform.users u \
     WHERE u.tenant_id = t.tenant_id AND u.type = 'user' AND EXISTS \
       (SELECT 1 FROM platform.user_roles r \
        WHERE r.tenant_id = u.tenant_id AND r.user_id = u.id)), \
    (SELECT count(*)::int4 FROM platform.roles r WHERE r.tenant_id = t.tenant_id) \
    FROM (SELECT DISTINCT tenant_id FROM platform.users WHERE type = 'platform') t";

async fn application_counts(url: &str) -> anyhow::Result<(i32, i32)> {
    let (mut client, connection) = tokio_postgres::connect(url, NoTls).await?;
    // Poll the connection here so cancellation drops its socket too. A spawned
    // connection could retain a blocked query after the deadline.
    tokio::select! {
        result = async {
            let transaction = client.build_transaction().read_only(true).start().await?;
            let row = transaction.query_one(APPLICATION_COUNTS_SQL, &[]).await?;
            let counts = (row.get(0), row.get(1));
            transaction.commit().await?;
            anyhow::Ok(counts)
        } => result,
        result = connection => {
            result?;
            anyhow::bail!("administration connection closed before status counts")
        }
    }
}

async fn optional_status_count<T>(
    project: &str,
    env: &str,
    source: &str,
    read: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Option<T> {
    match tokio::time::timeout(STATUS_COUNTS_TIMEOUT, read).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, project, env, source, "environment status count read failed");
            None
        }
        Err(_) => {
            tracing::warn!(
                project,
                env,
                source,
                "environment status count read timed out"
            );
            None
        }
    }
}

async fn status_with_counts(
    mut status: Value,
    project: &str,
    env: &str,
    application: impl std::future::Future<Output = anyhow::Result<(i32, i32)>>,
    memberships: impl std::future::Future<Output = anyhow::Result<i32>>,
) -> Value {
    let (application, memberships) = tokio::join!(
        optional_status_count(project, env, "application", application),
        optional_status_count(project, env, "memberships", memberships),
    );
    status["users_with_role"] = json!(application.map(|counts| counts.0));
    status["roles"] = json!(application.map(|counts| counts.1));
    status["memberships"] = json!(memberships);
    status
}

/// One environment's application write, in its own transaction on its
/// administration login. It returns the drops of the write, none when the
/// database holds no platform rows.
async fn write_application(
    url: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    principal: &PrincipalId,
    user: Option<ApplicationUser<'_>>,
    write: ApplicationWrite,
) -> anyhow::Result<AuthDrops> {
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
                return Ok(AuthDrops::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut drops = AuthDrops::default();
        match (write, user) {
            (ApplicationWrite::User, Some(user)) => {
                write_user(&transaction, &tenant, user, &mut drops).await?;
            }
            (ApplicationWrite::Admin, Some(user)) => {
                write_admin(&transaction, &tenant, user, &mut drops).await?;
            }
            (ApplicationWrite::RemoveAdmin, _) => {
                remove_admin(&transaction, &tenant, principal, &mut drops).await?;
            }
            (ApplicationWrite::RemoveUser, _) => {
                remove_user(&transaction, &tenant, principal, &mut drops).await?;
            }
            (write, None) => anyhow::bail!("{write:?} needs the user's email and display name"),
        }
        transaction.commit().await?;
        anyhow::Ok(drops)
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// The release whose manifest digest is `digest`, read from the environment's
/// `catalog.releases` on its administration login.
const RELEASE_SQL: &str =
    "SELECT canonical_bytes FROM catalog.releases WHERE manifest_digest = $1 LIMIT 1";

/// The columns of each table of the environment's application schemas, by
/// table name, as the release's unqualified SQL names them. The platform
/// schemas hold no application table.
const RELATION_COLUMNS_SQL: &str = "SELECT c.relname::text, a.attname::text \
     FROM pg_catalog.pg_class AS c \
     JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
     JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid \
     WHERE c.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped \
       AND n.nspname NOT IN ('information_schema', 'catalog', 'platform') \
       AND n.nspname NOT LIKE 'pg\\_%' AND n.nspname NOT LIKE 'wamn\\_%'";

/// The effect profile of the installed release `digest`
/// (docs/plan/trx-pkg-plan.md, Spike and acceptance B) and the operations each
/// role of the environment holds, or `None` when the environment has no such
/// release. `admin` holds every operation of the release. It writes nothing.
async fn release_profile(
    url: &str,
    digest: &str,
) -> anyhow::Result<
    Option<(
        wamn_schema_generator::profile::ReleaseProfile,
        BTreeMap<String, Vec<String>>,
    )>,
> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect the administration login")?;
    let connection = tokio::spawn(connection);
    let result = async {
        let Some(row) = client.query_opt(RELEASE_SQL, &[&digest]).await? else {
            return Ok(None);
        };
        let (manifest, _) =
            wamn_catalog::ServingManifest::from_canonical_bytes(&row.get::<_, Vec<u8>>(0))
                .context("decode the release manifest")?;
        let mut relations = BTreeMap::<String, std::collections::BTreeSet<String>>::new();
        for row in client.query(RELATION_COLUMNS_SQL, &[]).await? {
            relations.entry(row.get(0)).or_default().insert(row.get(1));
        }
        let profile =
            wamn_schema_generator::profile::release_profile(&manifest.components, &relations);
        let tenant = environment_tenant(&client).await?;
        let mut roles = BTreeMap::new();
        for role in role_names(&client, &tenant).await? {
            let operations = if role == ADMIN_ROLE {
                profile
                    .operations
                    .iter()
                    .map(|each| each.operation.clone())
                    .collect()
            } else {
                permission_rows(&client, &tenant, &role).await?
            };
            roles.insert(role, operations);
        }
        anyhow::Ok(Some((profile, roles)))
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// One environment's status row, in its own transaction on its
/// administration login. It returns the drops of the write.
async fn write_status(
    url: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    status: EnvironmentStatus,
) -> anyhow::Result<AuthDrops> {
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
        let mut drops = AuthDrops::default();
        write_environment_status(&transaction, &tenant, status, &mut drops).await?;
        transaction.commit().await?;
        anyhow::Ok(drops)
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// Bind a list cursor to this operation and the current authorization scope.
pub(crate) fn control_page(
    payload: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    org: &str,
    project: Option<&str>,
    order: Order,
) -> Result<ListPage, Refusal> {
    let input: Value = parse(payload)?;
    let Some(fields) = input.as_object() else {
        return Err(Refusal::Delivery(DeliveryError::InvalidPayload));
    };
    if fields.keys().any(|key| {
        !(matches!(key.as_str(), "limit" | "cursor") || key == "project" && project.is_some())
    }) {
        return Err(Refusal::Delivery(DeliveryError::InvalidPayload));
    }
    Ok(ListPage::new(
        &input,
        &attachment.operation,
        json!({"principal": caller.as_str(), "org": org, "project": project}),
        order,
    )?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_paging_binds_routes_and_reports_declared_fields() {
        let principal: PrincipalId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        for (handler, order, project, wrapper) in [
            (HostHandler::ProjectList, Order::Project, None, "projects"),
            (HostHandler::UserList, Order::Principal, None, "users"),
            (
                HostHandler::EnvironmentList,
                Order::Environment,
                Some("widgets"),
                "environments",
            ),
            (
                HostHandler::MemberList,
                Order::Principal,
                Some("widgets"),
                "members",
            ),
            (HostHandler::ControlMine, Order::Project, None, "projects"),
        ] {
            let attachment = wamn_catalog::HostRouteSet::Control
                .attachments()
                .find(|(_, attachment)| attachment.route.handler == handler)
                .unwrap()
                .1;
            let page = control_page(
                "{\"limit\":1}",
                attachment,
                &principal,
                "acme",
                project,
                order,
            )
            .unwrap();
            let key = if order == Order::Principal {
                json!(["a@example.invalid", principal.as_str()])
            } else {
                json!(["a"])
            };
            let first = page.answer(wrapper, vec![key.clone(), key], Clone::clone);
            assert!(first["total"].is_null());
            let request = json!({"cursor":first["next_cursor"]}).to_string();
            assert!(
                control_page(&request, attachment, &principal, "other", project, order).is_err()
            );
            if project.is_some() {
                assert!(
                    control_page(
                        &request,
                        attachment,
                        &principal,
                        "acme",
                        Some("other"),
                        order
                    )
                    .is_err()
                );
            }
            let last = control_page(&request, attachment, &principal, "acme", project, order)
                .unwrap()
                .answer(wrapper, Vec::<Value>::new(), Clone::clone);
            assert!(last["next_cursor"].is_null());
            assert!(last["total"].is_null());
            for (input, field) in [
                ("{\"limit\":0}", "limit"),
                ("{\"cursor\":\"bad\"}", "cursor"),
            ] {
                assert!(
                    matches!(control_page(input, attachment, &principal, "acme", project, order),
                    Err(Refusal::Declared {code: "invalid_input", detail}) if detail == json!({"field":field}))
                );
            }
            assert_eq!(
                control_page("{}", attachment, &principal, "acme", project, order)
                    .unwrap()
                    .read_limit(),
                None
            );
        }
    }

    #[tokio::test]
    async fn status_answers_when_count_reads_fail() {
        let status = json!({"digest": "sha256:live", "workloads": [], "live": [], "rollout": {"replicas": 1}});
        let answer = status_with_counts(
            status.clone(),
            "billing",
            "dev",
            async { anyhow::bail!("application unavailable") },
            async { anyhow::bail!("control unavailable") },
        )
        .await;
        for name in ["users_with_role", "roles", "memberships"] {
            assert_eq!(answer[name], Value::Null);
        }
        for name in ["digest", "workloads", "live", "rollout"] {
            assert_eq!(answer[name], status[name]);
        }
    }

    #[tokio::test]
    async fn status_keeps_independent_counts_and_zero() {
        let answer = status_with_counts(json!({}), "billing", "dev", async { Ok((0, 2)) }, async {
            anyhow::bail!("control unavailable")
        })
        .await;
        assert_eq!(
            answer,
            json!({"users_with_role": 0, "roles": 2, "memberships": null})
        );
        let answer = status_with_counts(
            json!({}),
            "billing",
            "dev",
            async { anyhow::bail!("application unavailable") },
            async { Ok(3) },
        )
        .await;
        assert_eq!(
            answer,
            json!({"users_with_role": null, "roles": null, "memberships": 3})
        );
    }

    #[tokio::test]
    async fn status_answers_when_count_reads_time_out() {
        let answer = status_with_counts(
            json!({"digest": "sha256:live"}),
            "billing",
            "dev",
            std::future::pending(),
            std::future::pending(),
        )
        .await;
        assert_eq!(
            answer,
            json!({"digest": "sha256:live", "users_with_role": null, "roles": null, "memberships": null})
        );
    }
}

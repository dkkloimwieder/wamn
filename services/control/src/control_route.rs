//! The org and project routes of the control serving root
//! (docs/architecture/execution.md#host-run-routes).
//!
//! Every org route needs a current `org-admin` row of the caller in the
//! token's org. A project route needs that row or a current `project-admin`
//! row of the caller in the project that the request names. A write runs in
//! one transaction on the org's `control` login, inserts the caller and
//! the route's sealed operation id into its transaction identity, and tests
//! the role again inside it. The writes are the functions of
//! `wamn_identity::org`, which platform installation also calls, so no
//! second writer exists.
//!
//! A write that changes administrative authority or membership also changes the
//! application rows of each environment it covers, with the environment's
//! `wamn_platform` login from the mounted Secret
//! `wamn-control-administration-<org>`, one transaction per environment
//! (docs/architecture/overview.md#organization-membership-and-administration).
//! Every authority mutation commits system state first. Projection then locks
//! the org membership and reads current authority before each application write.
//! The system lock lasts until the application commits. The route reports
//! success only when every environment is done.
//!
//! The status routes mirror the status of each environment into its row of
//! `platform.environment` in the same way
//! (docs/architecture/execution.md#routing-and-availability): an inactivation
//! writes the environments first and commits the system rows last, and an
//! activation commits the system rows first.
//!
//! `project.create` writes a project of the org with no environment and no
//! copied administrator grants (docs/architecture/execution.md#host-run-routes).
//!
//! `environment.status` reads the runtime through the host-selected
//! [`crate::environment_status::StatusSource`] and writes nothing. Optional
//! counts read the environment through its administration login and the
//! control store through its existing connection. Count failures answer null.
//!
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
mod connection_test;
mod integration;
mod operations;

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use wamn_catalog::{HostAttachment, HostHandler, HostRouteAuthority};
use wamn_provision::control_administration_key;
use wamn_engine::native_routes::{DeliveryError, PermissionDenial};
use wamn_identity_client::{PatIssuerConfig, UserRefused, create_user, send_invitation};
use wamn_identity_protocol::PrincipalId;
use wamn_environment::application::{
    ApplicationTarget, project_authority, require_platform_rows, write_environment_status,
};
use wamn_environment::org::{
    create_project, org_environments, set_environment_status, status_environments,
};
use wamn_identity::control::{
    control_projects_page, is_org_admin, is_project_admin, org_projects_page,
};
use wamn_identity::org::{
    EnvironmentStatus, MemberGrants, deactivate_org_membership, grant_member,
    grant_org_admin, grant_project_admin, invite_member, org_users_page,
    project_envs, project_envs_page, project_members_page, reactivate_org_membership,
    revoke_member, revoke_org_admin, revoke_project_admin,
};
use wamn_identity::paging::{ListPage, Order};
use wamn_identity::{IdentityError, IdentityRefusal, require_user_contact};

/// Why an org route did not answer.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A delivery refusal, such as a caller without `org-admin`.
    Delivery(DeliveryError),
    /// A refusal that the route's contract declares: its code and its
    /// detail, answered as the error of the request element.
    Declared { code: &'static str, detail: Value },
    /// A failure of the host or of a service it calls.
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<sqlx::Error> for Refusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Failed(error.into())
    }
}

/// A refusal of the identity writes is the declared error of the route, with
/// the input member it names in `field`. Any other error is a failure.
impl From<wamn_identity_protocol::ParseIdentityError> for Refusal {
    fn from(source: wamn_identity_protocol::ParseIdentityError) -> Self {
        IdentityError::from(source).into()
    }
}

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

fn operation_refusal(error: anyhow::Error) -> Refusal {
    match error.downcast_ref::<wamn_kubernetes::job::ObservationError>() {
        Some(refusal) => Refusal::Declared {
            code: match refusal.type_ {
                wamn_kubernetes::job::ObservationErrorType::InvalidName => "invalid_input",
                wamn_kubernetes::job::ObservationErrorType::NotFound => "not_found",
            },
            detail: json!({"field":"job"}),
        },
        None => Refusal::Failed(error),
    }
}

/// What the org routes of one control host use.
pub(crate) struct ControlRoutes<'a> {
    /// The `control` login for reads.
    pub(crate) control: &'a sqlx::PgPool,
    /// A second `control` login that holds one write transaction at a time.
    pub(crate) writer: &'a Mutex<Client>,
    /// The operator client of the identity service, for `user.invite`.
    pub(crate) identity: Option<&'a PatIssuerConfig>,
    pub(crate) issuer: &'a str,
    /// The mounted Secret of the org's administration logins, one file per
    /// environment.
    pub(crate) administration: Option<&'a Path>,
    /// The runtime reader selected by the host mode.
    pub(crate) status: &'a dyn crate::environment_status::StatusSource,
    pub(crate) registry_token_metadata: bool,
    pub(crate) execution: Option<&'a dyn crate::ControlExecution>,
    pub(crate) worker: Option<&'a wamn_kubernetes::job::Worker>,
    pub(crate) operation_jobs: Option<&'a wamn_kubernetes::kubernetes_api::KubernetesApi>,
    pub(crate) org: &'a wamn_registry::OrgId,
    pub(crate) oauth: Option<&'a wamn_http::oauth_authorization::Authorization>,
    pub(crate) connection_health: Option<&'a crate::connection_health::ConnectionHealth>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalRequest {
    principal_id: wamn_identity_protocol::PrincipalId,
}

/// The project that a project route names, read before its own request.
#[derive(Deserialize)]
struct ProjectTarget {
    project: wamn_registry::ProjectId,
    env: Option<wamn_registry::EnvName>,
    source_env: Option<wamn_registry::EnvName>,
}

/// The request of `member.list`, beside its paging. The page refuses an
/// environment that is not text.
#[derive(Deserialize)]
struct MemberListTarget {
    project: wamn_registry::ProjectId,
    #[serde(default)]
    environment: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    project: wamn_registry::ProjectId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectCreateRequest {
    project: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectPrincipalRequest {
    project: wamn_registry::ProjectId,
    principal_id: wamn_identity_protocol::PrincipalId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberRequest {
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
    principal_id: wamn_identity_protocol::PrincipalId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentRequest {
    project: wamn_registry::ProjectId,
    env: wamn_registry::EnvName,
}

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
    async fn execute(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let input: Value = serde_json::from_str(payload).map_err(anyhow::Error::from)?;
        let mut writer = self.writer.lock().await;
        let transaction = self
            .begin_in(
                &mut writer,
                attachment,
                caller,
                input["project"]
                    .as_str()
                    .map(str::parse::<wamn_registry::ProjectId>)
                    .transpose()
                    .map_err(anyhow::Error::from)?
                    .as_ref(),
            )
            .await?;
        transaction.commit().await?;
        drop(writer);
        self.execution
            .context("native control execution is absent")?
            .execute(
                &attachment.route.operation.parse().expect("host operation"),
                caller,
                payload,
            )
            .await
            .map_err(operation_refusal)
    }

    /// Serve one org or project route for `caller`.
    pub(crate) async fn handle(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let admitted = match attachment.route.authority {
            HostRouteAuthority::OrgAdmin => {
                self.read(caller, async |connection| {
                    Ok(self.admits(connection, caller, None).await?)
                })
                .await?
            }
            HostRouteAuthority::ProjectAdmin => {
                let project = parse::<ProjectTarget>(payload)?.project;
                self.read(caller, async |connection| {
                    Ok(self.admits(connection, caller, Some(&project)).await?)
                })
                .await?
            }
            // A read for every control caller: `org-admin`, or `project-admin`
            // of a project of the org.
            HostRouteAuthority::Member => {
                self.read(caller, async |connection| {
                    Ok(self.admits(connection, caller, None).await?)
                })
                .await?
                    || !self
                        .read(caller, async |connection| {
                            Ok(
                                control_projects_page(connection, caller, self.org, None, Some(1))
                                    .await?,
                            )
                        })
                        .await?
                        .is_empty()
            }
            authority @ (HostRouteAuthority::Admin | HostRouteAuthority::Granted) => {
                return Err(Refusal::Failed(anyhow::anyhow!(
                    "the control host does not admit the authority {authority:?}"
                )));
            }
        };
        if !admitted {
            return Err(denied(attachment));
        }
        match attachment.route.handler {
            HostHandler::ControlExecution => self.execute(attachment, caller, payload).await,
            HostHandler::OperationJob => self.operation_job(attachment, caller, payload).await,
            HostHandler::IntegrationAdministration => {
                self.integration_administration(attachment, caller, payload)
                    .await
            }
            HostHandler::UserList => {
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    None,
                    Order::Principal,
                )?;
                let users = self
                    .read(caller, async |connection| {
                        Ok(org_users_page(
                            connection,
                            self.org,
                            page.after(0),
                            page.after_principal(),
                            page.read_limit(),
                        )
                        .await?)
                    })
                    .await?;
                Ok(page.answer("users", users, |row| json!([row.email, row.principal_id])))
            }
            HostHandler::ProjectList => {
                let page =
                    control_page(payload, attachment, caller, self.org, None, Order::Project)?;
                let projects = self
                    .read(caller, async |connection| {
                        Ok(org_projects_page(
                            connection,
                            self.org,
                            page.after(0),
                            page.read_limit(),
                        )
                        .await?)
                    })
                    .await?;
                Ok(page.answer("projects", projects, |row| json!([row.name])))
            }
            HostHandler::UserInvite => self.invite(attachment, caller, payload).await,
            HostHandler::UserActivate => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let mut transaction = self.begin(&mut writer, attachment, caller).await?;
                reactivate_org_membership(&mut transaction, &principal, self.org).await?;
                transaction.commit().await?;
                Ok(json!({ "principal_id": principal.to_string().as_str(), "status": "active" }))
            }
            HostHandler::UserDeactivate => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let mut transaction = self.begin(&mut writer, attachment, caller).await?;
                deactivate_org_membership(&mut transaction, &principal, self.org).await?;
                transaction.commit().await?;
                let environments = self.environments(caller, None).await?;
                self.write_applications(&mut writer, attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({ "principal_id": principal.to_string().as_str(), "status": "inactive" }))
            }
            HostHandler::OrgAdminGrant => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let mut transaction = self.begin(&mut writer, attachment, caller).await?;
                grant_org_admin(&mut transaction, &principal, self.org).await?;
                transaction.commit().await?;
                let environments = self.environments(caller, None).await?;
                self.write_applications(&mut writer, attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({ "principal_id": principal.to_string().as_str(), "org_admin": true }))
            }
            HostHandler::OrgAdminRevoke => {
                let principal = principal(payload)?;
                let mut writer = self.writer.lock().await;
                let mut transaction = self.begin(&mut writer, attachment, caller).await?;
                revoke_org_admin(&mut transaction, &principal, self.org).await?;
                transaction.commit().await?;
                let environments = self.environments(caller, None).await?;
                self.write_applications(&mut writer, attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({ "principal_id": principal.to_string().as_str(), "org_admin": false }))
            }
            HostHandler::EnvironmentList => {
                let request: ProjectTarget = parse(payload)?;
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    Some(&request.project),
                    Order::Environment,
                )?;
                let environments = self
                    .read(caller, async |connection| {
                        Ok(project_envs_page(
                            connection,
                            self.org,
                            &request.project,
                            page.after(0),
                            page.read_limit(),
                        )
                        .await?)
                    })
                    .await?;
                Ok(page.answer("environments", environments, |row| json!([row.name])))
            }
            HostHandler::EnvironmentStatus => {
                let request: EnvironmentRequest = parse(payload)?;
                if !self
                    .read(caller, async |connection| {
                        Ok(project_envs(connection, self.org, &request.project).await?)
                    })
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
                    application_counts(&logins[0], caller).await
                };
                let memberships = async {
                    let mut transaction =
                        wamn_identity::transaction::read_pool(self.control, caller)
                            .await?;
                    let row = sqlx::query(
                        "SELECT count(*)::int4 FROM identity.project_env_memberships \
                         WHERE org = $1 AND project = $2 AND env = $3",
                    )
                    .bind(self.org)
                    .bind(&request.project)
                    .bind(&request.env)
                    .fetch_one(&mut *transaction)
                    .await?;
                    transaction.commit().await?;
                    anyhow::Ok(row.get::<i32, _>(0))
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
            HostHandler::MemberList => {
                let request: MemberListTarget = parse(payload)?;
                let page = control_page(
                    payload,
                    attachment,
                    caller,
                    self.org,
                    Some(&request.project),
                    Order::Principal,
                )?;
                let environment = request
                    .environment
                    .as_str()
                    .map(str::parse::<wamn_registry::EnvName>)
                    .transpose()
                    .map_err(|_| Refusal::Declared {
                        code: "invalid_input",
                        detail: json!({"field":"environment"}),
                    })?;
                let members = self
                    .read(caller, async |connection| {
                        Ok(project_members_page(
                            connection,
                            self.org,
                            &request.project,
                            environment.as_ref(),
                            page.after(0),
                            page.after_principal(),
                            page.read_limit(),
                        )
                        .await?)
                    })
                    .await?;
                Ok(page.answer("members", members, |row| {
                    json!([row.email, row.principal_id])
                }))
            }
            HostHandler::MemberGrant => {
                let request: MemberRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id;
                let mut writer = self.writer.lock().await;
                let mut transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                grant_member(
                    &mut transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &request.env,
                )
                .await?;
                transaction.commit().await?;
                let environment = [(request.project.clone(), request.env.clone())];
                self.write_applications(&mut writer, attachment, caller, &principal, &environment)
                    .await?;
                Ok(json!({
                    "principal_id": &principal,
                    "project": request.project,
                    "env": request.env,
                    "member": true,
                }))
            }
            HostHandler::MemberRevoke => {
                let request: MemberRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id;
                let mut writer = self.writer.lock().await;
                let mut transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                revoke_member(
                    &mut transaction,
                    &principal,
                    self.org,
                    &request.project,
                    &request.env,
                )
                .await?;
                transaction.commit().await?;
                let environment = [(request.project.clone(), request.env.clone())];
                self.write_applications(&mut writer, attachment, caller, &principal, &environment)
                    .await?;
                Ok(json!({
                    "principal_id": &principal,
                    "project": request.project,
                    "env": request.env,
                    "member": false,
                }))
            }
            HostHandler::ProjectAdminGrant => {
                let request: ProjectPrincipalRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id;
                let mut writer = self.writer.lock().await;
                let mut transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                grant_project_admin(&mut transaction, &principal, self.org, &request.project)
                    .await?;
                transaction.commit().await?;
                let environments = self.environments(caller, Some(&request.project)).await?;
                self.write_applications(&mut writer, attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({
                    "principal_id": &principal,
                    "project": request.project,
                    "project_admin": true,
                }))
            }
            HostHandler::ProjectAdminRevoke => {
                let request: ProjectPrincipalRequest = parse(payload)?;
                let principal: PrincipalId = request.principal_id;
                let mut writer = self.writer.lock().await;
                let mut transaction = self
                    .begin_in(&mut writer, attachment, caller, Some(&request.project))
                    .await?;
                revoke_project_admin(&mut transaction, &principal, self.org, &request.project)
                    .await?;
                transaction.commit().await?;
                let environments = self.environments(caller, Some(&request.project)).await?;
                self.write_applications(&mut writer, attachment, caller, &principal, &environments)
                    .await?;
                Ok(json!({
                    "principal_id": &principal,
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
                let request: ProjectCreateRequest = parse(payload)?;
                let project = request.project.parse().map_err(|_| Refusal::Declared {
                    code: "invalid_input",
                    detail: json!({ "field": "project" }),
                })?;
                let mut writer = self.writer.lock().await;
                let mut transaction = self.begin(&mut writer, attachment, caller).await?;
                create_project(&mut transaction, self.org, &project).await?;
                for row in sqlx::query(
                    "SELECT principal_id FROM identity.project_roles WHERE org=$1 AND project=$2",
                )
                .bind(self.org)
                .bind(&project)
                .fetch_all(&mut *transaction)
                .await?
                {
                    let _principal: PrincipalId = row.get(0);
                }
                transaction.commit().await?;
                Ok(json!({ "project": project }))
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
        require_user_contact(&request.email, &request.display_name)?;
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
        let mut transaction = self.begin(&mut writer, attachment, caller).await?;
        invite_member(&mut transaction, &user.principal_id, self.org, &grants).await?;
        transaction.commit().await?;
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
            let reply = send_invitation(identity, &user.principal_id).await?;
            if reply.status != 201 {
                return Err(Refusal::Failed(anyhow::anyhow!(
                    "identity did not accept the invitation for delivery: {}",
                    reply.status
                )));
            }
        }
        Ok(json!({
            "principal_id": user.principal_id.to_string().as_str(),
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
        project: &wamn_registry::ProjectId,
        env: Option<&wamn_registry::EnvName>,
        status: EnvironmentStatus,
    ) -> Result<(), Refusal> {
        let mut writer = self.writer.lock().await;
        let mut transaction = self.begin(&mut writer, attachment, caller).await?;
        let envs = status_environments(&mut transaction, self.org, project, env).await?;
        let environments: Vec<(wamn_registry::ProjectId, wamn_registry::EnvName)> =
            sqlx::query_as(wamn_registry::sql::ENVIRONMENT_PROJECTS)
                .bind(self.org)
                .bind(&envs)
                .fetch_all(&mut *transaction)
                .await?;
        if status == EnvironmentStatus::Inactive {
            self.write_statuses(attachment, caller, &environments, status)
                .await?;
        }
        set_environment_status(&mut transaction, self.org, project, &envs, status).await?;
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
        environments: &[(wamn_registry::ProjectId, wamn_registry::EnvName)],
        status: EnvironmentStatus,
    ) -> Result<(), Refusal> {
        let logins = self.logins(environments)?;
        let mut completed: Vec<String> = Vec::new();
        for ((project, env), url) in environments.iter().zip(logins) {
            match write_status(&url, attachment, caller, status).await {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        %project,
                        %env,
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
    fn logins(
        &self,
        environments: &[(wamn_registry::ProjectId, wamn_registry::EnvName)],
    ) -> Result<Vec<String>, Refusal> {
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

    async fn read_transaction(
        &self,
        caller: &PrincipalId,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, Refusal> {
        Ok(wamn_identity::transaction::read_pool(self.control, caller).await?)
    }

    async fn read<T>(
        &self,
        caller: &PrincipalId,
        work: impl AsyncFnOnce(&mut Client) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let mut transaction = self.read_transaction(caller).await?;
        let value = work(&mut transaction).await?;
        transaction.commit().await?;
        Ok(value)
    }

    /// Every current environment of the selected target.
    async fn environments(
        &self,
        caller: &PrincipalId,
        project: Option<&wamn_registry::ProjectId>,
    ) -> Result<Vec<(wamn_registry::ProjectId, wamn_registry::EnvName)>, Refusal> {
        self.read(caller, async |connection| {
            Ok(org_environments(connection, self.org, project).await?)
        })
        .await
    }

    /// Project current authority while holding the user's system membership lock.
    async fn write_applications(
        &self,
        writer: &mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        principal: &PrincipalId,
        environments: &[(wamn_registry::ProjectId, wamn_registry::EnvName)],
    ) -> Result<(), Refusal> {
        let logins = self.logins(environments)?;
        let mut system = writer.begin().await?;
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)")
            .bind(caller).bind(&attachment.operation).execute(&mut *system).await?;
        wamn_identity::org::lock_membership(&mut system, principal, self.org).await?;
        let mut completed = Vec::new();
        for ((project, env), url) in environments.iter().zip(logins) {
            let target = ApplicationTarget {
                org: self.org,
                project,
                env,
            };
            match write_application(
                &mut system,
                &url,
                target,
                attachment,
                caller,
                principal,
                self.issuer,
            )
            .await
            {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        %project,
                        %env,
                        "an application projection failed"
                    );
                    return Err(incomplete(project, env, &completed));
                }
            }
            completed.push(format!("{project}/{env}"));
        }
        system.commit().await?;
        Ok(())
    }

    /// Open the write transaction of one org route.
    async fn begin<'c>(
        &self,
        writer: &'c mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
    ) -> Result<sqlx::Transaction<'c, sqlx::Postgres>, Refusal> {
        self.begin_in(writer, attachment, caller, None).await
    }

    /// Open the write transaction of one route: bind the caller and the
    /// sealed operation id, and test the route's role again inside it.
    async fn begin_in<'c>(
        &self,
        writer: &'c mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        project: Option<&wamn_registry::ProjectId>,
    ) -> Result<sqlx::Transaction<'c, sqlx::Postgres>, Refusal> {
        let mut transaction = writer.begin().await?;
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)",
        )
        .bind(caller)
        .bind(&attachment.operation)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())?;
        if !self.admits(&mut transaction, caller, project).await? {
            return Err(denied(attachment));
        }
        Ok(transaction)
    }

    /// Whether `caller` holds `org-admin` in the org, or `project-admin` in
    /// `project` when a project route names one.
    async fn admits(
        &self,
        client: &mut sqlx::PgConnection,
        caller: &PrincipalId,
        project: Option<&wamn_registry::ProjectId>,
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

// The role rows of the selected environment database. EXISTS counts each
// human user once even when that user holds several roles, and excludes
// services.
const APPLICATION_COUNTS_SQL: &str = "SELECT \
    (SELECT count(*)::int4 FROM platform.\"user\" u \
     WHERE u.type = 'user' AND EXISTS \
       (SELECT 1 FROM platform.user_role r \
        WHERE r.user_id = u.id)), \
    (SELECT count(*)::int4 FROM platform.role r)";

async fn application_counts(url: &str, caller: &PrincipalId) -> anyhow::Result<(i32, i32)> {
    let mut client = wamn_postgres::connect(url).await?;
    let mut transaction =
        wamn_identity::transaction::read_connection(&mut client, caller).await?;
    let row = sqlx::query(APPLICATION_COUNTS_SQL)
        .fetch_one(&mut *transaction)
        .await?;
    let counts = (row.get(0), row.get(1));
    transaction.commit().await?;
    Ok(counts)
}

async fn optional_status_count<T>(
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    source: &str,
    read: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Option<T> {
    match tokio::time::timeout(STATUS_COUNTS_TIMEOUT, read).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, %project, %env, source, "environment status count read failed");
            None
        }
        Err(_) => {
            tracing::warn!(
                %project,
                %env,
                source,
                "environment status count read timed out"
            );
            None
        }
    }
}

async fn status_with_counts(
    mut status: Value,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
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
/// administration login.
async fn write_application(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    url: &str,
    target: ApplicationTarget<'_>,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    principal: &PrincipalId,
    issuer: &str,
) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(url).await?;

    let result = async {
        let mut transaction = client.begin().await?;
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)").bind(caller).bind(&attachment.operation).execute(&mut *transaction).await.map(|result| result.rows_affected())?;
        require_platform_rows(&mut transaction).await?;
        project_authority(system, &mut transaction, target, principal, issuer).await?;
        transaction.commit().await?;
        anyhow::Ok(())
    }.await;
    drop(client);
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
    let mut client = wamn_postgres::connect(url)
        .await
        .context("connect the administration login")?;

    let result = async {
        let mut transaction = client.begin().await?;
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)",
        )
        .bind(caller)
        .bind(&attachment.operation)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())?;
        require_platform_rows(&mut transaction).await?;
        write_environment_status(&mut transaction, status).await?;
        transaction.commit().await?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    result
}

/// Bind a list cursor to this operation and the current authorization target.
pub(crate) fn control_page(
    payload: &str,
    attachment: &HostAttachment,
    caller: &PrincipalId,
    org: &wamn_registry::OrgId,
    project: Option<&wamn_registry::ProjectId>,
    order: Order,
) -> Result<ListPage, Refusal> {
    let input: Value = parse(payload)?;
    let Some(fields) = input.as_object() else {
        return Err(Refusal::Delivery(DeliveryError::InvalidPayload));
    };
    // Only `member.list` filters by environment, and only its cursor binds it.
    let filtered = attachment.route.handler == HostHandler::MemberList;
    if fields.keys().any(|key| {
        !(matches!(key.as_str(), "limit" | "cursor")
            || key == "project" && project.is_some()
            || key == "environment" && filtered)
    }) {
        return Err(Refusal::Delivery(DeliveryError::InvalidPayload));
    }
    let mut target =
        json!({"principal": caller.to_string().as_str(), "org": org, "project": project});
    if filtered {
        let environment = match fields.get("environment") {
            None => Value::Null,
            Some(Value::String(environment)) => json!(environment),
            Some(_) => {
                return Err(Refusal::Declared {
                    code: "invalid_input",
                    detail: json!({ "field": "environment" }),
                });
            }
        };
        target["environment"] = environment;
    }
    Ok(ListPage::new(
        &input,
        attachment.operation.as_str(),
        target,
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
    Ok(request.principal_id)
}

/// The refusal of a write whose application rows stopped at `project/env`,
/// with the environments that completed.
fn incomplete(
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    completed: &[String],
) -> Refusal {
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
        operation: attachment.operation.to_string(),
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
            let project = project
                .map(str::parse::<wamn_registry::ProjectId>)
                .transpose()
                .unwrap();
            let project = project.as_ref();
            let attachment = wamn_catalog::HostRouteSet::Control
                .attachments()
                .find(|(_, attachment)| attachment.route.handler == handler)
                .unwrap()
                .1;
            let page = control_page(
                "{\"limit\":1}",
                attachment,
                &principal,
                &"acme".parse().unwrap(),
                project,
                order,
            )
            .unwrap();
            let key = if order == Order::Principal {
                json!(["a@example.invalid", principal.to_string().as_str()])
            } else {
                json!(["a"])
            };
            let first = page.answer(wrapper, vec![key.clone(), key], Clone::clone);
            assert!(first["total"].is_null());
            let request = json!({"cursor":first["next_cursor"]}).to_string();
            assert!(
                control_page(
                    &request,
                    attachment,
                    &principal,
                    &"other".parse().unwrap(),
                    project,
                    order
                )
                .is_err()
            );
            if project.is_some() {
                assert!(
                    control_page(
                        &request,
                        attachment,
                        &principal,
                        &"acme".parse().unwrap(),
                        Some(&"other".parse().unwrap()),
                        order
                    )
                    .is_err()
                );
            }
            let last = control_page(
                &request,
                attachment,
                &principal,
                &"acme".parse().unwrap(),
                project,
                order,
            )
            .unwrap()
            .answer(wrapper, Vec::<Value>::new(), Clone::clone);
            assert!(last["next_cursor"].is_null());
            assert!(last["total"].is_null());
            for (input, field) in [
                ("{\"limit\":0}", "limit"),
                ("{\"cursor\":\"bad\"}", "cursor"),
            ] {
                assert!(
                    matches!(control_page(input, attachment, &principal, &"acme".parse().unwrap(), project, order),
                    Err(Refusal::Declared {code: "invalid_input", detail}) if ({ let observed = &(detail); observed["field"] == serde_json::json!(field) }))
                );
            }
            assert_eq!(
                control_page(
                    "{}",
                    attachment,
                    &principal,
                    &"acme".parse().unwrap(),
                    project,
                    order
                )
                .unwrap()
                .read_limit(),
                None
            );
        }
    }

    #[test]
    fn member_list_cursor_binds_the_environment() {
        let principal: PrincipalId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let attachment = |handler| {
            wamn_catalog::HostRouteSet::Control
                .attachments()
                .find(|(_, attachment)| attachment.route.handler == handler)
                .unwrap()
                .1
        };
        let members = attachment(HostHandler::MemberList);
        let page = |payload: &str| {
            control_page(
                payload,
                members,
                &principal,
                &"acme".parse().unwrap(),
                Some(&"widgets".parse().unwrap()),
                Order::Principal,
            )
        };
        let key = json!(["a@example.invalid", principal.to_string().as_str()]);
        let cursor = page("{\"limit\":1,\"environment\":\"dev\"}")
            .unwrap()
            .answer("members", vec![key.clone(), key], Clone::clone)["next_cursor"]
            .clone();
        assert!(page(&json!({"cursor": cursor, "environment": "dev"}).to_string()).is_ok());
        for request in [
            json!({"cursor": cursor, "environment": "prod"}),
            json!({"cursor": cursor}),
        ] {
            assert!(matches!(
                page(&request.to_string()),
                Err(Refusal::Declared {code: "invalid_input", detail}) if ({ let observed = &(detail); observed["field"] == serde_json::json!("cursor") })
            ));
        }
        for input in ["{\"environment\":null}", "{\"environment\":1}"] {
            assert!(matches!(
                page(input),
                Err(Refusal::Declared {code: "invalid_input", detail}) if ({ let observed = &(detail); observed["field"] == serde_json::json!("environment") })
            ));
        }
        assert!(matches!(
            control_page(
                "{\"environment\":\"dev\"}",
                attachment(HostHandler::EnvironmentList),
                &principal,
                &"acme".parse().unwrap(),
                Some(&"widgets".parse().unwrap()),
                Order::Environment,
            ),
            Err(Refusal::Delivery(DeliveryError::InvalidPayload))
        ));
    }

    #[tokio::test]
    async fn status_answers_when_count_reads_fail() {
        let status = json!({"digest": "sha256:live", "workloads": [], "live": [], "rollout": {"replicas": 1}});
        let answer = status_with_counts(
            status.clone(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
            async { anyhow::bail!("application unavailable") },
            async { anyhow::bail!("control unavailable") },
        )
        .await;
        for name in ["users_with_role", "roles", "memberships"] {
            assert!(answer[name].is_null());
        }
        for (name, value) in status.as_object().unwrap() {
            assert_eq!(answer[name], *value, "{name}");
        }
    }

    #[tokio::test]
    async fn status_answers_when_count_reads_time_out() {
        let status = json!({"digest": "sha256:live"});
        let answer = status_with_counts(
            status.clone(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
            std::future::pending(),
            std::future::pending(),
        )
        .await;
        for name in ["users_with_role", "roles", "memberships"] {
            assert!(answer[name].is_null());
        }
        for (name, value) in status.as_object().unwrap() {
            assert_eq!(answer[name], *value, "{name}");
        }
    }
}

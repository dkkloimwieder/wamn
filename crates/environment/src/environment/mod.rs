//! Independent environment provisioning, initialization, and targeted application activation.

pub mod analyse;
pub mod bindings;
pub mod capture_gap;
pub mod connections;
pub mod delete;
pub mod document;
pub mod drain;
pub mod ensure;
pub mod lifecycle;
pub mod runtime;
pub mod show;
pub mod sql;
pub mod stage;
pub mod write;

use crate::application::{self, ApplicationTarget, ApplicationUser};
use anyhow::Context as _;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;

use std::path::PathBuf;

use wamn_identity_client::PatIssuerConfig;
use wamn_kubernetes::Target;

/// The platform inputs of the lifecycle verbs.
#[derive(Debug, Clone)]
pub struct Platform {
    /// The system database URL selected for this verb.
    pub system_database_url: String,
    /// The cluster, its context and the operator release's namespace (R6).
    pub target: Target,
    /// A stamped release chart directory.
    pub chart: PathBuf,
    /// Keep fetched chart files alive for this operation.
    pub chart_download: Option<std::sync::Arc<crate::platform::artifact_plan::SelectedChart>>,
    pub registry_auth_file: PathBuf,
    pub registry_token_metadata: bool,
    pub oci_ca_paths: Vec<PathBuf>,
    /// Registry transport selected by the installed platform host.
    pub insecure_registry: bool,
    /// `<registry>/<repository>` of package artifacts. Needed only when a
    /// release names a package the environment does not have yet, or the
    /// document pins a web artifact.
    pub package_artifact_base: Option<String>,
    /// The namespace of the CloudNativePG `Cluster`, which the environment's
    /// `Database` CR shares.
    pub database_namespace: String,
    /// The host and port every credential URL names. Absent host: the target
    /// Service `<cluster>-rw.<database_namespace>.svc.cluster.local`.
    pub database_host: Option<String>,
    pub database_port: u16,
    /// A YAML mapping: the platform's part of the host group body (replicas,
    /// resources, registry mounts). The verb adds the environment's
    /// credential Secret references.
    pub host_group: Option<PathBuf>,
    /// The identity service that issues the management-author PAT. Without
    /// it, `apply` issues no PAT.
    pub pat_issuer: PatIssuerConfig,
    /// The identity issuer address used for project user rows.
    pub identity_issuer: String,
    /// The event broker for application capture and entity consumers.
    pub events: Option<wamn_broker::EventBroker>,
}

impl Platform {
    pub(crate) async fn registry(
        &self,
        base: &str,
    ) -> anyhow::Result<wamn_oci::package_artifact::PackageRegistry> {
        let credentials = if self.registry_token_metadata {
            let base = wamn_catalog::parse_component_artifact_base(base)?;
            Some(wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
                .credentials(base.registry()).await?)
        } else {
            None
        };
        Ok(wamn_oci::package_artifact::PackageRegistry {
            artifact_base: base.to_owned(),
            credentials,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        })
    }
}

/// Fill service, member and administrator rows from system authority.
pub async fn fill(
    system_url: &str,
    project_url: &str,
    issuer: &str,
    target: ApplicationTarget<'_>,
) -> anyhow::Result<()> {
    let mut system = wamn_postgres::connect(system_url).await?;
    let mut system = system.begin().await?;
    wamn_identity::org::lock_org_memberships(&mut system, target.org).await?;
    let domain: Option<wamn_registry::platform_domain::PlatformDomain> =
        sqlx::query_scalar(wamn_registry::sql::IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY)
            .fetch_one(&mut *system)
            .await?;
    let domain = domain.context("platform domain is absent")?;
    let mut project = wamn_postgres::connect(project_url).await?;
    let mut project = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::bind_platform_principal_sql(
            wamn_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *project)
    .await?;
    let mut principals = BTreeSet::new();
    for row in sqlx::query("SELECT DISTINCT p.id, p.subject, p.display_name FROM identity.project_roles r JOIN identity.principals p ON p.id=r.principal_id WHERE r.org=$1 AND r.project=$2 AND p.type='service' AND p.status='active'")
        .bind(target.org).bind(target.project).fetch_all(&mut *system).await? {
        let principal_id: wamn_identity_protocol::PrincipalId = row.get(0);
        let subject: String = row.get(1);
        let display_name: String = row.get(2);
        let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM platform.\"user\" WHERE id=$1)")
            .bind(principal_id).fetch_one(&mut *project).await?;
        if !present {
            application::write_service(
                &mut project,
                issuer,
                ApplicationUser {
                    principal_id: &principal_id,
                    email: &format!("{subject}@{domain}"),
                    display_name: &display_name,
                },
            )
            .await?;
        }
        principals.insert(principal_id);
    }
    principals.extend(
        sqlx::query_scalar::<_, wamn_identity_protocol::PrincipalId>(
            "SELECT id FROM identity.principals WHERE identity.has_project_access(id,$1,$2,$3)",
        )
        .bind(target.org)
        .bind(target.env)
        .bind(target.project)
        .fetch_all(&mut *system)
        .await?,
    );
    principals.extend(
        sqlx::query_scalar::<_, wamn_identity_protocol::PrincipalId>(
            "SELECT id FROM platform.\"user\" WHERE type IN ('user','service')",
        )
        .fetch_all(&mut *project)
        .await?,
    );
    for id in principals {
        application::project_authority(&mut system, &mut project, target, &id, issuer).await?;
    }
    project.commit().await?;
    system.commit().await?;
    Ok(())
}

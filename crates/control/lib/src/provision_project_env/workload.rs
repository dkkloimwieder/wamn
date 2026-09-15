//! Workload credential preparation, authentication, retirement, and abort.

use anyhow::Context as _;
use ring::rand::SecureRandom as _;

use super::grants::{
    RoleAclExpectation, stable_grant_set, verify_public_access_floor, verify_role_grants,
};
use super::{
    CredentialGeneration, DateTime, EffectWriterCredentialScope, EffectWriterCredentialValidity,
    GenericClient, PLATFORM_GROUP_ROLE, PgConfig, ProvisionProjectEnvArgs, SecondsFormat,
    SessionTarget, SystemRandom, Triple, Utc, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind, WorkloadSecretBody,
    WorkloadSecretBodyKind, connect_config, effect_writer_credential, emit_text, ensure_secret_path,
    exact_project_database_config, json, legacy_effect_writer_generation_role, named_database_config,
    project_env_database_name, read_project_env_instance, render_workload_secret_manifest, role_sql,
    sql, tenant_key, validate_project_env, validate_session_tenant_id, workload_action_flag,
    workload_config, workload_generation_role, workload_secret_flag, workload_url, write_secret_json,
};

const WORKLOAD_CREDENTIAL_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkloadRoleState {
    login: bool,
    superuser: bool,
    inherit: bool,
    create_role: bool,
    create_db: bool,
    replication: bool,
    bypass_rls: bool,
    password_set: bool,
    valid_until: Option<String>,
    valid_until_finite: bool,
    memberships: Vec<String>,
    membership_options_exact: bool,
    membership_options_migratable: bool,
    member_roles: Vec<String>,
    member_options_exact: bool,
    generation_children_exact: bool,
    connect_databases: Vec<String>,
    sessions: i64,
    owned_objects: i64,
}

impl WorkloadRoleState {
    fn is_active_for(&self, family: WorkloadRoleFamily, database: &str) -> bool {
        self.has_active_shape_for(database)
            && self.memberships == [family.acl_role()]
            && self.membership_options_exact
    }

    fn is_migratable_active_for(&self, family: WorkloadRoleFamily, database: &str) -> bool {
        let memberships_are_known = self.memberships == [family.acl_role()]
            || (family == WorkloadRoleFamily::EffectWriter
                && self.memberships
                    == [
                        family.acl_role(),
                        wamn_run_state::RUN_PROJECTION_WRITER_ROLE,
                    ]);
        self.has_active_shape_for(database)
            && memberships_are_known
            && self.membership_options_migratable
    }

    fn has_active_shape_for(&self, database: &str) -> bool {
        self.login
            && self.restrictive_attributes()
            && self.inherit
            && self.password_set
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.connect_databases == [database]
            && self.owned_objects == 0
    }

    fn is_inactive(&self) -> bool {
        self.has_inactive_shape() && self.memberships.is_empty() && self.membership_options_exact
    }

    fn is_migratable_inactive_for(&self, family: WorkloadRoleFamily) -> bool {
        let memberships_are_known = self.memberships.is_empty()
            || (family == WorkloadRoleFamily::EffectWriter
                && self.memberships == [wamn_run_state::RUN_PROJECTION_WRITER_ROLE]);
        self.has_inactive_shape() && memberships_are_known && self.membership_options_migratable
    }

    fn has_inactive_shape(&self) -> bool {
        !self.login
            && self.restrictive_attributes()
            && self.inherit
            && !self.password_set
            && self.valid_until.as_deref() == Some("1970-01-01T00:00:00Z")
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    fn is_acl_role(&self, family: WorkloadRoleFamily) -> bool {
        self.has_acl_role_shape(family)
            && self.member_options_exact
            && self.generation_children_exact
    }

    /// THE ONE PARENT EDGE A STABLE ACL ROLE MAY CARRY (`wamn-0h0g.22.17`).
    ///
    /// A platform-grain family's ACL role is a member of
    /// [`PLATFORM_GROUP_ROLE`], and it has to be: the tenant floor is narrowed
    /// `TO wamn_app`, PostgreSQL default-denies when no policy matches the
    /// connected role, and the permissive arm names the group. Nothing else may
    /// appear here — an extra parent is authority this provisioner did not
    /// confer.
    fn expected_acl_parents(family: WorkloadRoleFamily) -> &'static [&'static str] {
        if family.is_platform_grain() {
            &[PLATFORM_GROUP_ROLE]
        } else {
            &[]
        }
    }

    fn has_acl_role_shape(&self, family: WorkloadRoleFamily) -> bool {
        !self.login
            && self.restrictive_attributes()
            && !self.inherit
            && !self.password_set
            && self.valid_until.is_none()
            && !self.valid_until_finite
            && self.memberships == Self::expected_acl_parents(family)
            && self.membership_options_exact
            && self
                .member_roles
                .iter()
                .all(|role| is_workload_generation_role(family, role))
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    fn restrictive_attributes(&self) -> bool {
        !self.superuser
            && !self.create_role
            && !self.create_db
            && !self.replication
            && !self.bypass_rls
    }
}

pub(super) fn is_workload_generation_role(family: WorkloadRoleFamily, role: &str) -> bool {
    let prefix = format!("{}_", family.generation_prefix());
    let Some(scoped) = role.strip_prefix(&prefix) else {
        return false;
    };
    let Some((hash, generation)) = scoped.split_once('_') else {
        return false;
    };
    hash.len() == 40
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && matches!(generation, "a" | "b")
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WorkloadLifecycle<'a> {
    pub(super) family: WorkloadRoleFamily,
    pub(super) scope: WorkloadRoleScope<'a>,
    pub(super) control_tenant: Option<&'a str>,
}

impl<'a> WorkloadLifecycle<'a> {
    pub(super) fn database(self) -> &'a str {
        self.scope.database()
    }

    pub(super) fn role(self, generation: CredentialGeneration) -> String {
        workload_generation_role(self.family, self.scope, generation)
            .expect("the lifecycle constructor pairs each family with its exact scope")
    }

    pub(super) fn family_lock_key(self) -> String {
        format!("wamn.workload-family.v1:{}", self.family.acl_role())
    }

    pub(super) fn label(self) -> String {
        self.family.label()
    }
}

/// ONE lifecycle constructor, for any family (`wamn-0h0g.22.16`).
///
/// Replaces four copy-pasted constructors that differed only in the scope arm
/// they filled in. The scope GRAIN is the family's own declaration, so pairing a
/// family with the wrong grain is not something a caller can get wrong here.
///
/// The control tenant follows the same derivation: only a control-scoped family
/// records a login-to-tenant mapping row, because that row is the control
/// plane's and a project-environment credential never reaches the control
/// database.
pub(super) fn workload_lifecycle<'a>(
    family: WorkloadRoleFamily,
    identity: WorkloadActionIdentity<'a>,
    database: &'a str,
) -> WorkloadLifecycle<'a> {
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    } = identity;
    let scope = match family.scope_kind() {
        // Tenant scope for the effect writer and the guest credential alike:
        // the digest in the role name IS the tenant key, so the login the mint
        // issues and the key `wamn_authority.tenant_key` computes are the same
        // string (`wamn-0h0g.22.6.4`).
        WorkloadRoleScopeKind::Tenant => WorkloadRoleScope::Tenant { tenant, database },
        WorkloadRoleScopeKind::ProjectEnvironment => WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        },
        WorkloadRoleScopeKind::Control => WorkloadRoleScope::Control {
            org,
            project,
            environment,
            database,
        },
    };
    WorkloadLifecycle {
        family,
        scope,
        control_tenant: (family.scope_kind() == WorkloadRoleScopeKind::Control).then_some(tenant),
    }
}

/// The retired project-environment effect-writer identities, migration input
/// only.
///
/// `None` for every other family, including any admitted later: a legacy
/// identity is a fact about one family's history, not a generic property.
fn legacy_generation_roles(
    family: WorkloadRoleFamily,
    identity: WorkloadActionIdentity<'_>,
    database: &str,
    generation: CredentialGeneration,
) -> (Option<String>, Option<String>) {
    if family != WorkloadRoleFamily::EffectWriter {
        return (None, None);
    }
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        ..
    } = identity;
    (
        Some(legacy_effect_writer_generation_role(
            org,
            project,
            environment,
            database,
            generation,
        )),
        Some(legacy_effect_writer_generation_role(
            org,
            project,
            environment,
            database,
            generation.other(),
        )),
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WorkloadActionIdentity<'a> {
    pub(super) org: &'a str,
    pub(super) project: &'a str,
    pub(super) environment: &'a str,
    pub(super) tenant: &'a str,
}

fn workload_action_identity<'a>(
    args: &'a ProvisionProjectEnvArgs,
    label: &str,
) -> anyhow::Result<WorkloadActionIdentity<'a>> {
    let org = args
        .org
        .as_deref()
        .expect("clap parser invariant: --org is required unless --revoke-pat-prefix is present");
    let project = args.project.as_deref().expect(
        "clap parser invariant: --project is required unless --revoke-pat-prefix is present",
    );
    let environment = args
        .env
        .as_deref()
        .expect("clap parser invariant: --env is required unless --revoke-pat-prefix is present");
    let tenant = args
        .tenant
        .as_deref()
        .with_context(|| format!("{label} generation actions require --tenant"))?;
    anyhow::ensure!(!tenant.is_empty(), "--tenant must not be empty");
    validate_project_env(org, project, environment)
        .map_err(|error| anyhow::anyhow!("project-env names: {error}"))?;
    Ok(WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    })
}

async fn converge_workload_generation_state(
    client: &(impl GenericClient + Sync),
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<Option<WorkloadRoleState>> {
    let state = read_workload_role_state(client, role, &lifecycle.label()).await?;
    let Some(found) = state.as_ref() else {
        return Ok(None);
    };
    let active =
        if found.is_active_for(lifecycle.family, lifecycle.database()) || found.is_inactive() {
            return Ok(state);
        } else if found.is_migratable_active_for(lifecycle.family, lifecycle.database()) {
            true
        } else if found.is_migratable_inactive_for(lifecycle.family) {
            false
        } else {
            return Ok(state);
        };
    client
        .batch_execute(&sql::normalize_workload_generation_membership_sql(
            lifecycle.family,
            role,
            active,
        ))
        .await
        .with_context(|| {
            format!(
                "normalize legacy {} generation membership",
                lifecycle.label()
            )
        })?;
    read_workload_role_state(client, role, &lifecycle.label()).await
}

async fn converge_stable_workload_memberships(
    client: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let Some(stable) =
        read_workload_role_state(client, lifecycle.family.acl_role(), &lifecycle.label()).await?
    else {
        return Ok(());
    };
    for role in stable.member_roles {
        anyhow::ensure!(
            is_workload_generation_role(lifecycle.family, &role),
            "stable {} ACL role has a member outside its generation family",
            lifecycle.label()
        );
        client
            .batch_execute(&sql::normalize_workload_generation_membership_sql(
                lifecycle.family,
                &role,
                true,
            ))
            .await
            .with_context(|| {
                format!(
                    "normalize {} stable-role generation member",
                    lifecycle.label()
                )
            })?;
        let child = read_workload_role_state(client, &role, &lifecycle.label())
            .await?
            .with_context(|| {
                format!(
                    "{} stable-role generation member disappeared",
                    lifecycle.label()
                )
            })?;
        let [database] = child.connect_databases.as_slice() else {
            anyhow::bail!(
                "{} stable-role generation member does not carry exactly one direct database CONNECT grant",
                lifecycle.label()
            );
        };
        anyhow::ensure!(
            child.is_active_for(lifecycle.family, database),
            "{} stable-role generation member is not an exact active credential",
            lifecycle.label()
        );
        verify_role_grants(
            admin_config,
            &role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
    }
    Ok(())
}

/// ONE workload generation action, for any family (`wamn-0h0g.22.16`).
///
/// Replaces four copy-pasted `run_*_action` functions and the dispatch chain
/// that chose between them. Everything this needs is DERIVED from the family:
/// its scope grain picks the identity inputs, the admin database and the
/// lifecycle scope; its label names the action in every message; its declared
/// Secret body shape picks what the published Secret carries. What is
/// deliberately NOT derived is the GRANT SET, which is where a family's
/// authority actually lives.
pub(super) async fn run_workload_action(
    args: &ProvisionProjectEnvArgs,
    action: WorkloadGenerationAction,
) -> anyhow::Result<()> {
    let WorkloadGenerationAction {
        family,
        verb,
        generation,
    } = action;
    let label = family.label();
    let emits_app_retirement_sql =
        family == WorkloadRoleFamily::App && verb == WorkloadActionVerb::Prepare;
    anyhow::ensure!(
        args.cluster.is_none()
            && args.connection_limit.is_none()
            && args.app_host.is_none()
            && args.emit_database.is_none()
            && (args.emit_role_sql.is_none() || emits_app_retirement_sql)
            && args.emit_privilege_sql.is_none()
            && args.emit_secret.is_none()
            && args.emit_management_author_pat_secret.is_none()
            && args.emit_route_caller_pat_secret.is_none(),
        "{label} generation actions cannot render ordinary provisioning or PAT artifacts; only \
         App prepare may emit the canonical shared-login retirement role SQL"
    );
    let identity = workload_action_identity(args, &label)?;
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    } = identity;
    if family == WorkloadRoleFamily::SessionRoleReader {
        validate_session_tenant_id(tenant)?;
    }
    let triple = Triple::new(org, project, environment);
    let system_url = args
        .system_database_url
        .as_deref()
        .with_context(|| format!("{label} generation actions require --system-database-url"))?;

    // A CONTROL-scoped family addresses the control database the system URL
    // already names; every other family addresses the project environment's own
    // database, whose instance suffix is READ from the registry rather than
    // typed. One derivation over the scope grain, not a branch per family.
    let (admin_url, database, admin_config, instance) = if family.scope_kind()
        == WorkloadRoleScopeKind::Control
    {
        anyhow::ensure!(
            args.target_admin_database_url.is_none(),
            "--target-admin-database-url is not a {label} input: this family addresses the control database"
        );
        let config = named_database_config(system_url, &format!("{label} admin"))?;
        let database = config
            .get_dbname()
            .expect("named_database_config requires a database name")
            .to_string();
        (system_url, database, config, None)
    } else {
        let instance = read_project_env_instance(system_url, &triple).await?;
        let database = project_env_database_name(org, project, environment, &instance);
        let admin_url = args.target_admin_database_url.as_deref().with_context(|| {
            format!("{label} generation actions require --target-admin-database-url")
        })?;
        let config = exact_project_database_config(admin_url, &database)?;
        (admin_url, database, config, Some(instance))
    };
    let lifecycle = workload_lifecycle(family, identity, &database);

    match verb {
        WorkloadActionVerb::Prepare => {
            let secret_path = args.workload_secret_path(family).with_context(|| {
                format!(
                    "--{} requires --{} PATH",
                    workload_action_flag(family, verb),
                    workload_secret_flag(family)
                )
            })?;
            ensure_secret_path(secret_path, &format!("--{}", workload_secret_flag(family)))?;
            let validity = workload_validity(Utc::now());
            // The key the RLS predicate computes, taken from the ONE Rust
            // definition rather than re-derived here — the Secret's label must
            // name the same tenant the role name's digest does.
            let key = tenant_key(tenant, &database);
            let scope = EffectWriterCredentialScope {
                tenant: tenant.to_string(),
                org: org.to_string(),
                project: project.to_string(),
                environment: environment.to_string(),
                database: database.clone(),
            };
            let (legacy_desired, legacy_other) =
                legacy_generation_roles(family, identity, &database, generation);
            prepare_workload_generation(
                &admin_config,
                lifecycle,
                legacy_desired.as_deref(),
                legacy_other.as_deref(),
                generation,
                &validity.expires_at,
                |role, password, predecessor_role| {
                    let credential_url = workload_url(admin_url, role, password, &database)?;
                    let secret = match family.secret_body_kind() {
                        WorkloadSecretBodyKind::Url => render_workload_secret_manifest(
                            family,
                            &triple,
                            &args.namespace,
                            WorkloadSecretBody::Url(&credential_url),
                        ),
                        WorkloadSecretBodyKind::TenantUrl => {
                            anyhow::ensure!(
                                role.contains(&key),
                                "the minted {label} login does not carry the tenant key the RLS \
                                 predicate computes, so every guest read would refuse"
                            );
                            render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::TenantUrl {
                                    tenant,
                                    tenant_key: &key,
                                    url: &credential_url,
                                },
                            )
                        }
                        WorkloadSecretBodyKind::EffectWriterCredential => {
                            let credential_id = random_lower_hex(16)?;
                            let credential = effect_writer_credential(
                                &scope,
                                &credential_id,
                                generation,
                                &validity,
                                &credential_url,
                            );
                            let mut secret = render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::EffectWriterCredential(&credential),
                            );
                            if let Some(predecessor_role) = predecessor_role {
                                secret["metadata"]["annotations"]
                                    ["wamn.io/predecessor-database-role"] = json!(predecessor_role);
                            }
                            secret
                        }
                        WorkloadSecretBodyKind::SessionTarget => {
                            let target = SessionTarget::new(
                                &triple,
                                instance.as_deref().expect("session readers use project-environment scope"),
                                tenant,
                                &credential_url,
                            )?;
                            render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::SessionTarget(&target),
                            )
                        }
                    };
                    write_secret_json(secret_path, &secret)
                        .with_context(|| format!("write authenticated {label} Secret"))
                },
            )
            .await?;
            println!(
                "prepared and authenticated {label} credential generation {} for {org}/{project}/{environment}; wrote {}",
                generation.as_str(),
                secret_path.display()
            );
            if family == WorkloadRoleFamily::App && args.emit_role_sql.is_some() {
                emit_text(
                    &args.emit_role_sql,
                    "shared App-login retirement role SQL (apply once after every replacement carrier is verified)",
                    &role_sql(""),
                )?;
            }
        }
        WorkloadActionVerb::Retire => {
            let (legacy_old_role, _) =
                legacy_generation_roles(family, identity, &database, generation);
            retire_workload_generation(
                &admin_config,
                lifecycle,
                legacy_old_role.as_deref(),
                generation,
            )
            .await?;
            println!(
                "retired {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
        WorkloadActionVerb::Abort => {
            abort_workload_generation(&admin_config, lifecycle, generation).await?;
            println!(
                "aborted unpublished {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
    }
    Ok(())
}

/// Prepare one generation, then verify it and publish its Secret.
///
/// **A REFUSED PREPARE IS NOT ATOMIC, deliberately (`wamn-0h0g.12.179`).** The
/// prepare transaction COMMITS before the post-commit checks run, because the
/// generation must be authenticated over a real connection — which no
/// uncommitted role can accept. A refusal after that point therefore leaves,
/// and is contracted to leave, exactly two things behind:
///
/// * the stable ACL role converged to its NOLOGIN, password-free shape by
///   `ensure_workload_acl_role_sql`, which is idempotent and is the shape every
///   subsequent prepare wants anyway; and
/// * the target generation role, rolled back by
///   [`rollback_prepared_workload_generation`] to the INACTIVE shape — no
///   `LOGIN`, no password, no membership, no `CONNECT`, `VALID UNTIL 'epoch'`.
///
/// Nothing else survives, and no Secret is published. A retry meets precisely
/// the inactive target a prepare requires, so the partial state is recoverable
/// rather than wedging; what it is NOT is a clean cluster, and a live arm that
/// assumes a refusal left no role behind will find a healthy object sitting
/// inside `prepare_workload_generation_sql`'s `IF NOT EXISTS` guard. Live arms
/// must drop the roles themselves, not rely on a failed run to have done it.
async fn prepare_workload_generation<F>(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    legacy_desired_role: Option<&str>,
    legacy_other_role: Option<&str>,
    generation: CredentialGeneration,
    expires_at: &str,
    publish: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&str, &str, Option<&str>) -> anyhow::Result<()>,
{
    let database = lifecycle.database();
    let role = lifecycle.role(generation);
    let mut other_role = lifecycle.role(generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation prepare", lifecycle.label()))?;
    transaction
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .context("converge cluster PUBLIC CONNECT floor")?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let desired = converge_workload_generation_state(&transaction, lifecycle, &role).await?;
    if let Some(legacy_role) = legacy_desired_role {
        if let Some(legacy) =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?
        {
            anyhow::ensure!(
                legacy.is_inactive(),
                "legacy effect-writer migration must prepare the opposite generation"
            );
        }
    }
    let mut other =
        converge_workload_generation_state(&transaction, lifecycle, &other_role).await?;
    if other.as_ref().is_none_or(WorkloadRoleState::is_inactive)
        && let Some(legacy_role) = legacy_other_role
    {
        let legacy =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?;
        if legacy.as_ref().is_some_and(|state| !state.is_inactive()) {
            other_role = legacy_role.to_string();
            other = legacy;
        }
    }
    let recovering_active = match (generation, desired.as_ref(), other.as_ref()) {
        (CredentialGeneration::A, desired, None)
            if desired.is_none_or(WorkloadRoleState::is_inactive) =>
        {
            false
        }
        (CredentialGeneration::A, Some(desired), None)
            if desired.is_active_for(lifecycle.family, database) && desired.sessions == 0 =>
        {
            true
        }
        (_, desired, Some(other))
            if desired.is_none_or(WorkloadRoleState::is_inactive)
                && other.is_active_for(lifecycle.family, database) =>
        {
            false
        }
        (_, Some(desired), Some(other))
            if desired.is_active_for(lifecycle.family, database)
                && desired.sessions == 0
                && other.is_active_for(lifecycle.family, database) =>
        {
            true
        }
        (CredentialGeneration::B, None, None) => {
            anyhow::bail!(
                "initial {} credential generation must be a",
                lifecycle.label()
            )
        }
        _ => anyhow::bail!(
            "{} generation prepare requires an inactive target, or an exact zero-session active target recovered after failed Secret publication",
            lifecycle.label()
        ),
    };
    let desired_acl = if recovering_active {
        RoleAclExpectation::Generation { database }
    } else {
        RoleAclExpectation::None
    };
    verify_role_grants(admin_config, &role, desired_acl).await?;
    if other.is_some() {
        verify_role_grants(
            admin_config,
            &other_role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
    }
    let predecessor_role = other.as_ref().map(|_| other_role.as_str());
    // Pre-checked ONLY for a family whose stable grant set is converged
    // elsewhere (schema control owns the effect writer's, because its grants
    // exist only once the effect tables do). A family whose grant set
    // THIS batch applies has nothing to assert yet on a first prepare, so the
    // condition is the absence of a stable surface, not a family name.
    if sql::stable_surface_sql(lifecycle.family).is_none()
        && let Some(grant_set) = stable_grant_set(lifecycle.family)
        && read_workload_role_state(
            &transaction,
            lifecycle.family.acl_role(),
            &lifecycle.label(),
        )
        .await?
        .is_some()
    {
        verify_role_grants(
            admin_config,
            lifecycle.family.acl_role(),
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database: database,
            },
        )
        .await?;
    }
    let password = random_lower_hex(32)?;
    transaction
        .batch_execute(&sql::prepare_workload_generation_sql(
            lifecycle.family,
            database,
            &role,
            &password,
            expires_at,
        ))
        .await
        .with_context(|| format!("prepare {} credential generation", lifecycle.label()))?;
    if let (
        Some(tenant),
        WorkloadRoleScope::Control {
            org,
            project,
            environment,
            ..
        },
    ) = (lifecycle.control_tenant, lifecycle.scope)
    {
        let mapped: Option<String> = transaction
            .query_opt(
                sql::upsert_control_author_tenant_mapping_sql(),
                &[&role, &tenant, &org, &project, &environment],
            )
            .await
            .context("record control-author login tenant mapping")?
            .map(|row| row.get("tenant_id"));
        anyhow::ensure!(
            mapped.as_deref() == Some(tenant),
            "control-author login identity already maps to a different tenant"
        );
    }
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation prepare", lifecycle.label()))?;

    // App prepare has now made the stable role NOLOGIN, so old credentials
    // cannot reconnect. Do not drain its existing sessions here: they bridge
    // the interval in which the authenticated generation Secret is published
    // and workloads roll. The explicit retirement step owns the final bounded
    // native drain after that cutover.

    let publish_result = async {
        let credential_config = workload_config(admin_config, &role, &password, database);
        authenticate_workload_generation(&credential_config, lifecycle, &role)
            .await
            .with_context(|| format!("authenticate prepared {} generation", lifecycle.label()))?;

        let prepared = read_workload_role_state(&admin, &role, &lifecycle.label())
            .await?
            .with_context(|| format!("prepared {} generation disappeared", lifecycle.label()))?;
        anyhow::ensure!(
            prepared.is_active_for(lifecycle.family, database),
            "prepared {} generation did not have the exact active ACL",
            lifecycle.label()
        );
        anyhow::ensure!(
            prepared.valid_until.as_deref() == Some(expires_at),
            "prepared {} generation VALID UNTIL does not match credential expires-at",
            lifecycle.label()
        );
        verify_role_grants(
            admin_config,
            &role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
        verify_stable_workload_role(&admin, admin_config, lifecycle).await?;
        publish(&role, &password, predecessor_role)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = publish_result {
        let rollback =
            rollback_prepared_workload_generation(&admin, admin_config, lifecycle, &role).await;
        drop(admin);
        let _ = admin_task.await;
        if let Err(rollback_error) = rollback {
            anyhow::bail!(
                "{} prepare failed after LOGIN was enabled: {error:#}; rollback also failed: {rollback_error:#}",
                lifecycle.label()
            );
        }
        return Err(error);
    }
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

/// Undo the authority a committed prepare granted, back to the INACTIVE shape.
///
/// It does NOT drop the role, and does not undo the stable ACL role's
/// convergence — see [`prepare_workload_generation`] for the contract on what a
/// refused prepare leaves behind.
async fn rollback_prepared_workload_generation(
    admin: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<()> {
    admin
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            lifecycle.database(),
            role,
        ))
        .await
        .with_context(|| format!("revoke prepared {} generation authority", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(role))
        .await
        .with_context(|| {
            format!(
                "terminate prepared {} generation sessions",
                lifecycle.label()
            )
        })?;
    let state = read_workload_role_state(admin, role, &lifecycle.label())
        .await?
        .with_context(|| format!("rolled-back {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        state.is_inactive(),
        "rolled-back {} generation did not converge to inactive",
        lifecycle.label()
    );
    verify_role_grants(admin_config, role, RoleAclExpectation::None).await
}

async fn retire_workload_generation(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    legacy_old_role: Option<&str>,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let database = lifecycle.database();
    let mut old_role = lifecycle.role(generation);
    let replacement_role = lifecycle.role(generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation retirement", lifecycle.label()))?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let mut old = converge_workload_generation_state(&transaction, lifecycle, &old_role).await?;
    if old.as_ref().is_none_or(WorkloadRoleState::is_inactive)
        && let Some(legacy_role) = legacy_old_role
    {
        let legacy =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?;
        if legacy.as_ref().is_some_and(|state| !state.is_inactive()) {
            old_role = legacy_role.to_string();
            old = legacy;
        }
    }
    let old =
        old.with_context(|| format!("old {} generation does not exist", lifecycle.label()))?;
    let replacement =
        converge_workload_generation_state(&transaction, lifecycle, &replacement_role)
            .await?
            .with_context(|| {
                format!(
                    "replacement {} generation does not exist",
                    lifecycle.label()
                )
            })?;
    anyhow::ensure!(
        old.is_active_for(lifecycle.family, database),
        "old {} generation is not the exact active credential",
        lifecycle.label()
    );
    anyhow::ensure!(
        replacement.is_active_for(lifecycle.family, database),
        "replacement {} generation is not LOGIN-capable with exact ACL",
        lifecycle.label()
    );
    anyhow::ensure!(
        replacement.sessions > 0,
        "replacement {} generation has no verified live private-pool session",
        lifecycle.label()
    );
    verify_role_grants(
        admin_config,
        &old_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    verify_role_grants(
        admin_config,
        &replacement_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    transaction
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            database,
            &old_role,
        ))
        .await
        .with_context(|| format!("retire old {} credential generation", lifecycle.label()))?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation retirement", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(&old_role))
        .await
        .with_context(|| {
            format!(
                "terminate retired {} generation sessions",
                lifecycle.label()
            )
        })?;
    let retired = read_workload_role_state(&admin, &old_role, &lifecycle.label())
        .await?
        .with_context(|| format!("retired {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        retired.is_inactive(),
        "old {} generation did not converge to inactive",
        lifecycle.label()
    );
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

async fn abort_workload_generation(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let database = lifecycle.database();
    let role = lifecycle.role(generation);
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation abort", lifecycle.label()))?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let prepared = converge_workload_generation_state(&transaction, lifecycle, &role)
        .await?
        .with_context(|| format!("prepared {} generation does not exist", lifecycle.label()))?;
    let other_role = lifecycle.role(generation.other());
    let _ = converge_workload_generation_state(&transaction, lifecycle, &other_role).await?;
    anyhow::ensure!(
        prepared.is_active_for(lifecycle.family, database),
        "prepared {} generation is not the exact active credential",
        lifecycle.label()
    );
    anyhow::ensure!(
        prepared.sessions == 0,
        "published or in-use {} generation cannot be aborted",
        lifecycle.label()
    );
    verify_role_grants(
        admin_config,
        &role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    verify_stable_workload_role(&transaction, admin_config, lifecycle).await?;
    transaction
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            database,
            &role,
        ))
        .await
        .with_context(|| {
            format!(
                "abort unpublished {} credential generation",
                lifecycle.label()
            )
        })?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation abort", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(&role))
        .await
        .with_context(|| {
            format!(
                "terminate aborted {} generation sessions",
                lifecycle.label()
            )
        })?;
    let aborted = read_workload_role_state(&admin, &role, &lifecycle.label())
        .await?
        .with_context(|| format!("aborted {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        aborted.is_inactive(),
        "aborted {} generation did not converge to inactive",
        lifecycle.label()
    );
    verify_role_grants(admin_config, &role, RoleAclExpectation::None).await?;
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

fn workload_validity(now: DateTime<Utc>) -> EffectWriterCredentialValidity {
    let expires_at = now + chrono::Duration::days(WORKLOAD_CREDENTIAL_TTL_DAYS);
    EffectWriterCredentialValidity {
        issued_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        not_before: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        revoked_at: None,
    }
}

fn random_lower_hex(bytes: usize) -> anyhow::Result<String> {
    let mut material = vec![0_u8; bytes];
    SystemRandom::new()
        .fill(&mut material)
        .map_err(|_| anyhow::anyhow!("operating system could not supply credential entropy"))?;
    Ok(hex::encode(material))
}

async fn authenticate_workload_generation(
    config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<()> {
    let (client, task) = connect_config(config, &lifecycle.label()).await?;
    let row = client
        .query_one(
            "SELECT current_user::text, current_database()::text, \
                    has_database_privilege(current_user, current_database(), 'TEMPORARY')",
            &[],
        )
        .await
        .with_context(|| format!("probe prepared {} generation", lifecycle.label()))?;
    let current_user: String = row.get(0);
    let current_database: String = row.get(1);
    let can_create_temporary: bool = row.get(2);
    anyhow::ensure!(
        current_user == role,
        "prepared generation authenticated as wrong role"
    );
    anyhow::ensure!(
        current_database == lifecycle.database(),
        "prepared generation authenticated to wrong database"
    );
    anyhow::ensure!(
        !can_create_temporary,
        "prepared generation inherited TEMPORARY on its database"
    );
    drop(client);
    task.await
        .with_context(|| format!("join {} authentication connection", lifecycle.label()))??;
    Ok(())
}

async fn lock_workload_family(
    client: &(impl GenericClient + Sync),
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let family_key = lifecycle.family_lock_key();
    client
        .query_one(sql::workload_scope_lock_sql(), &[&family_key])
        .await
        .with_context(|| format!("acquire {} family rotation lock", lifecycle.label()))?;
    Ok(())
}

async fn verify_stable_workload_role(
    client: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let role = lifecycle.family.acl_role();
    let state = read_workload_role_state(client, role, &lifecycle.label())
        .await?
        .with_context(|| format!("stable {} ACL role does not exist", lifecycle.label()))?;
    anyhow::ensure!(
        state.is_acl_role(lifecycle.family),
        "stable {} ACL role is not a connection-free NOLOGIN role with exact generation members",
        lifecycle.label()
    );
    if let Some(grant_set) = stable_grant_set(lifecycle.family) {
        verify_role_grants(
            admin_config,
            role,
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database: lifecycle.database(),
            },
        )
        .await?;
    }
    Ok(())
}

async fn read_workload_role_state(
    client: &(impl GenericClient + Sync),
    role: &str,
    label: &str,
) -> anyhow::Result<Option<WorkloadRoleState>> {
    let row = client
        .query_opt(sql::workload_generation_state_sql(), &[&role])
        .await
        .with_context(|| format!("read {label} generation state"))?;
    Ok(row.map(|row| WorkloadRoleState {
        login: row.get("rolcanlogin"),
        superuser: row.get("rolsuper"),
        inherit: row.get("rolinherit"),
        create_role: row.get("rolcreaterole"),
        create_db: row.get("rolcreatedb"),
        replication: row.get("rolreplication"),
        bypass_rls: row.get("rolbypassrls"),
        password_set: row.get("password_set"),
        valid_until: row.get("valid_until"),
        valid_until_finite: row.get("valid_until_finite"),
        memberships: row.get("memberships"),
        membership_options_exact: row.get("membership_options_exact"),
        membership_options_migratable: row.get("membership_options_migratable"),
        member_roles: row.get("member_roles"),
        member_options_exact: row.get("member_options_exact"),
        generation_children_exact: row.get("generation_children_exact"),
        connect_databases: row.get("connect_databases"),
        sessions: row.get("sessions"),
        owned_objects: row.get("owned_objects"),
    }))
}

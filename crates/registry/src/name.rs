//! Resource names derived from registry coordinates.

use crate::instance_suffix::InstanceSuffix;
use crate::platform_domain::PlatformDomain;
use wamn_digest::sha256;

/// Check whether a resource name can be used as a single path segment.
pub fn valid_resource_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// The public host name of an application with a validated label.
pub fn app_host(label: &str, org: &crate::OrgId, base: &PlatformDomain) -> String {
    format!("{label}--{org}.{base}")
}

/// The key of a project environment: `<org>--<env>--<project>` (owner
/// ruling of 2026-10-09 on `wamn-snz0`). The first apply, the analysis of
/// `env apply` and the host target all derive it here, so neither the
/// environment document nor the serving manifest repeats the composed key.
pub fn project_env_key(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> crate::ProjectEnvKey {
    format!("{org}--{env}--{project}")
        .parse()
        .expect("validated registry coordinates fit a deployment key")
}

/// The JetStream account shared by the projects of an environment.
pub fn env_nats_account(org: &crate::OrgId, env: &crate::EnvName) -> String {
    format!("ENV_{org}_{env}")
}

/// Inbox prefix for a user NKey issued by identity.
pub fn nats_user_inbox(public_key: &str) -> String {
    format!("_INBOX_wamn.{public_key}")
}

/// The org Secret holding an environment-scoped NATS user.
pub fn env_nats_secret(org: &crate::OrgId, env: &crate::EnvName, kind: &str) -> String {
    format!("wamn-nats-{kind}-{}", env_host_name(org, env))
}

/// The org Secret holding a project-scoped NATS user.
pub fn project_nats_secret(target: &crate::Triple, kind: &str) -> String {
    format!(
        "wamn-nats-{kind}-{}",
        project_env_resource_name(&target.org, &target.project, &target.env)
    )
}

/// The ServiceAccount of the org control service.
pub fn control_service_account(org: &crate::OrgId) -> String {
    format!("wamn-control-{org}")
}

/// Coordinates of one shared environment, without a project database.
pub fn env_key(org: &crate::OrgId, env: &crate::EnvName) -> String {
    format!("{org}--{env}")
}

/// The Secret that stores a project deployment's credential values.
pub fn credential_secret_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("wamn-credential-{}", project_env_key(org, project, env))
}

/// Name budget or prefix.
pub const DB_PREFIX: &str = "wamn-db-";

/// Name budget or prefix.
pub const MAX_DB_NAME_LEN: usize = 63;

/// Name budget or prefix.
pub const NAMESPACE_PREFIX: &str = "wamn-";

/// Name budget or prefix.
pub const MAX_NAMESPACE_LEN: usize = 63;

/// Name budget or prefix.
pub const INSTANCE_SUFFIX_LEN: usize = 8;

/// Name budget or prefix.
pub const INSTANCE_SUFFIX_SEPARATOR: &str = "--";

/// Name budget or prefix.
pub const MAX_NAMESPACE_STEM_LEN: usize =
    MAX_NAMESPACE_LEN - INSTANCE_SUFFIX_SEPARATOR.len() - INSTANCE_SUFFIX_LEN;

/// Name budget or prefix.
pub const CDC_SECRET_PREFIX: &str = "wamn-cdc-";

/// Name budget or prefix.
pub const CDC_OBJECT_PREFIX: &str = "wamn_cdc_";

/// The per-project-env database name:
/// `wamn-db-<org>--<env>--<project>--<instance>`
/// (wamn-q3n.7).
///
/// The **org** is encoded — unlike the retired 2.3 project-only name — because the shared
/// T3 trials pool hosts many orgs (two orgs' identically-named projects would
/// otherwise collide on one cluster), and because every cluster's CNPG `Database`
/// resources share the one K8s namespace, so the resource name must be unique
/// there. `--` separates the identity components (the coordinate
/// convention). Validate the assembled length with the provisioning coordinate validator
/// before use; quote it in DDL (it contains hyphens) with the provisioning SQL builder.
pub fn project_env_database_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    instance: &InstanceSuffix,
) -> String {
    format!(
        "{}{INSTANCE_SUFFIX_SEPARATOR}{instance}",
        project_env_database_stem(org, project, env)
    )
}

/// The project's CloudNativePG Cluster. The database name encodes its
/// coordinate and generation. Keep space for the recovery suffix.
pub fn project_cluster(database: &str) -> String {
    format!(
        "wamn-pg-{}",
        &hex::encode(sha256(database.as_bytes()))[..32]
    )
}

/// The project primary service in its environment namespace.
pub fn project_database_host(database: &str, namespace: &str) -> String {
    format!(
        "{}-rw.{namespace}.svc.cluster.local",
        project_cluster(database)
    )
}

/// A login certificate Secret for this coordinate, generation, and role. Its 43 bytes
/// fit the runtime operator's volume name with the host-plugin-secret- prefix.
pub fn project_login_certificate(database: &str, login: &str) -> String {
    let identity = format!("{database}/{login}");
    format!(
        "wamn-pgtls-{}",
        &hex::encode(sha256(identity.as_bytes()))[..32]
    )
}

/// Read a project coordinate only from a valid, generation-qualified database name.
pub fn project_database_key(database: &str) -> Option<crate::ProjectEnvKey> {
    let (key, generation) = database.strip_prefix(DB_PREFIX)?.rsplit_once("--")?;
    generation.parse::<InstanceSuffix>().ok()?;
    key.parse().ok()
}

pub fn project_env_database_stem(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{DB_PREFIX}{org}--{env}--{project}")
}

/// The per-project-env credential Secret name:
/// `wamn-db-<org>--<env>--<project>`. It stays triple-only because its namespace
/// carries the instance identity; its URL names the suffixed Postgres database.
pub fn project_env_secret_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    project_env_database_stem(org, project, env)
}

/// The ConfigMap that holds only the `ca.crt` of a project cluster's server CA.
pub fn project_server_ca_copy(cluster: &str) -> String {
    format!("{cluster}-ca-crt")
}

/// The namespace stem `wamn-<org>--<env>--<project>` used in coordinate validation.
/// The validator applies [`MAX_NAMESPACE_STEM_LEN`].
pub fn project_env_namespace_stem(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{NAMESPACE_PREFIX}{org}--{env}--{project}")
}

/// The CDC reader Deployment and ServiceAccount name.
pub fn cdc_reader_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{CDC_SECRET_PREFIX}{org}--{env}--{project}")
}

/// The shared name of the per-project-env CDC Postgres objects — publication,
/// and failover replication slot:
/// `wamn_cdc_<org>__<env>__<project>__<instance>`, slug hyphens mapped to `_`
/// and `__` as the separator (a slot name admits only `[a-z0-9_]`). Since slugs cannot
/// contain `_`, the mapping keeps distinct triples distinct except for the same
/// consecutive-hyphen ambiguity the `--` database-name separator already
/// carries; identity always travels in the registration row / Secret labels,
/// never parsed back out of a name. Validate the assembled length with
/// the provisioning CDC validator before use.
pub fn cdc_object_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    instance: &InstanceSuffix,
) -> String {
    format!("{}__{instance}", cdc_object_name_stem(org, project, env))
}

pub fn cdc_object_name_stem(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    let flat = |s: &str| s.replace('-', "_");
    format!(
        "{CDC_OBJECT_PREFIX}{}__{}__{}",
        flat(org.as_str()),
        flat(env.as_str()),
        flat(project.as_str())
    )
}

/// The subject ceiling shared by the projects of an environment.
pub fn env_event_subjects(org: &crate::OrgId, env: &crate::EnvName) -> String {
    format!("evt.{org}.{env}.>")
}

/// The prefix shared by a project deployment's event subjects.
pub fn event_prefix(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("evt.{org}.{env}.{project}.")
}
/// A subject within a project deployment's event stream.
pub fn event_subject(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    tail: &str,
) -> String {
    format!("{}{tail}", event_prefix(org, project, env))
}
/// The host group for the local development loop.
pub fn dev_host_group(key: &crate::ProjectEnvKey) -> String {
    format!("wamn-dev-{key}")
}
/// The Job and account of an org's control operation.
pub fn operation(org: &crate::OrgId) -> String {
    format!("wamn-operation-{org}")
}
/// The installed OCI pull credential Secret in kind org and environment namespaces.
pub const REGISTRY_PULL: &str = "wamn-registry-pull";

/// The longest slug in a release name: the env name cut to this length.
pub const SLUG_LEN: usize = 24;
/// The hex characters of the coordinate hash in a release name (80 bits).
pub const HASH_LEN: usize = 20;

/// The name of one project deployment's resources: `e-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/env/project`. The name is at most 47
/// characters, inside Helm's bound of 53.
///
pub fn project_env_resource_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    let hash = hex::encode(sha256(format!("{org}/{env}/{project}").as_bytes()));
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("e-{slug}-{}", &hash[..HASH_LEN])
}

/// The Helm release and host group shared by an org and environment.
pub fn env_host_name(org: &crate::OrgId, env: &crate::EnvName) -> String {
    let hash = hex::encode(sha256(format!("{org}/{env}").as_bytes()));
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("h-{slug}-{}", &hash[..HASH_LEN])
}

/// The Deployment that the wasmCloud runtime chart creates for a host group.
pub fn host_deployment_name(host_group: &str) -> String {
    format!("hostgroup-{host_group}")
}

/// The ServiceAccount shared by an environment's project resources.
pub fn env_service_account(org: &crate::OrgId, env: &crate::EnvName) -> String {
    format!("wamn-host-{}", env_key(org, env))
}

/// A project deployment's complete event subscription.
pub fn stream_subjects(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    event_subject(org, project, env, ">")
}
/// The event stream of one project deployment. Length framing separates every coordinate.
pub fn stream_name(org: &crate::OrgId, project: &crate::ProjectId, env: &crate::EnvName) -> String {
    format!(
        "EVT_{}_{org}_{}_{env}_{}_{project}",
        org.as_str().len(),
        env.as_str().len(),
        project.as_str().len()
    )
}
/// The coordinate label in a host status response.
pub fn host_status(org: &crate::OrgId, project: &crate::ProjectId, env: &crate::EnvName) -> String {
    format!("{org}/{env}/{project}")
}

/// A resource derived from an existing resource and a role or purpose.
pub fn resource_suffix(resource: &str, purpose: &str) -> String {
    format!("{resource}-{purpose}")
}
/// A service resource derived from its project deployment.
pub fn service_resource(
    stem: &str,
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{stem}-{}", project_env_key(org, project, env))
}
/// A login name derived from its credential identity.
///
/// # Panics
/// Panics if the prefix and hash cannot form a PostgreSQL login name.
pub fn credential_login(prefix: &str, identity_hash: &str) -> wamn_role::LoginName {
    format!("{prefix}_{identity_hash}")
        .parse()
        .expect("derived login fits PostgreSQL identifiers")
}
/// The audience of one project database instance.
pub fn project_audience(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    instance: &InstanceSuffix,
) -> String {
    format!("urn:wamn:project-env:{org}:{env}:{project}:{instance}")
}

/// The prefix of every org audience.
pub const ORG_AUDIENCE_PREFIX: &str = "urn:wamn:org:";

/// The access token audience shared by an org's environments and control routes.
pub fn org_audience(org: &crate::OrgId) -> String {
    format!("{ORG_AUDIENCE_PREFIX}{org}")
}
/// The release discovery filter for a validated environment slug.
pub fn app_release_filter(env: &crate::EnvName) -> String {
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("^r-{slug}-")
}
/// The memory key of one connection's current facts.
pub fn connection_key(project: &crate::ProjectEnvKey, id: impl std::fmt::Display) -> String {
    format!("{project}.connection.{id}")
}

/// Select the host pods belonging to one group.
pub fn host_selector(group: &str) -> String {
    format!("wasmcloud.com/hostgroup={group}")
}

/// The temporary template used to copy a local project database.
pub fn database_template(database: &str) -> String {
    format!("{database}--template")
}

/// The control host release and group of one org.
pub fn control_resource(org: &crate::OrgId) -> String {
    format!("c-{org}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_coordinates_keep_project_databases_separate() {
        let first = crate::Triple::new(
            "acme".parse().unwrap(),
            "dev".parse().unwrap(),
            "billing".parse().unwrap(),
        );
        let key = project_env_key(&first.org, &first.project, &first.env);
        assert_ne!(
            key,
            project_env_key(
                &"acme".parse().unwrap(),
                &"shipping".parse().unwrap(),
                &"dev".parse().unwrap()
            )
        );
        assert_ne!(
            project_env_database_name(
                &"acme".parse().unwrap(),
                &"billing".parse().unwrap(),
                &"dev".parse().unwrap(),
                &"abcdefgh"
                    .parse::<crate::instance_suffix::InstanceSuffix>()
                    .unwrap()
            ),
            project_env_database_name(
                &"acme".parse().unwrap(),
                &"shipping".parse().unwrap(),
                &"dev".parse().unwrap(),
                &"abcdefgh"
                    .parse::<crate::instance_suffix::InstanceSuffix>()
                    .unwrap()
            )
        );
        let resource = project_env_resource_name(
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
        );
        assert!(resource.len() <= 47);
        assert_ne!(
            resource,
            project_env_resource_name(
                &"acme".parse().unwrap(),
                &"shipping".parse().unwrap(),
                &"dev".parse().unwrap()
            )
        );
    }
}

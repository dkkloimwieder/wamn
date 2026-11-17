//! Resource names derived from registry coordinates.

use crate::instance_suffix::InstanceSuffix;
use wamn_digest::sha256;

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

/// The NATS user of the environment `project_env_key`. Its client certificate
/// carries this DNS name, and its replies use the inbox `_INBOX_<user>`
/// (docs/plan/alignment.md R5).
pub fn environment_nats_user(project_env_key: &crate::ProjectEnvKey) -> String {
    format!("wamn-env-{project_env_key}")
}

/// The NATS user of the control host of `org`, which covers every
/// environment of the org (docs/plan/alignment.md R7).
pub fn control_nats_user(org: &crate::OrgId) -> String {
    format!("wamn-control-{org}")
}

/// Coordinates of one shared environment, without a project database.
pub fn environment_key(org: &crate::OrgId, env: &crate::EnvName) -> String {
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
pub const GUEST_SECRET_PREFIX: &str = "wamn-guest-";

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

/// The Secret of an org's control host that holds the administration login of
/// every environment of the org (docs/plan/platform-ui.md §4.4):
/// `wamn-control-administration-<org>`.
pub fn control_administration_secret_name(org: &crate::OrgId) -> String {
    format!("wamn-control-administration-{org}")
}

/// The key of one environment in [`control_administration_secret_name`]:
/// `<env>--<project>`, the file name the mounted Secret shows.
pub fn control_administration_key(project: &crate::ProjectId, env: &crate::EnvName) -> String {
    format!("{env}--{project}")
}

/// The per-project-env Kubernetes namespace:
/// `wamn-<org>--<env>--<project>--<instance>`.
///
/// One wamn environment `(org, project, env)` is one namespace (wamn-0h0g.15.3),
/// so every environment-targeted resource — the credential Secrets, the release
/// manifest, the environment's hosts — is addressed by DERIVING this name from
/// the triple and the environment's minted instance suffix. It is never
/// configured alongside the triple and never parsed back out of a resource.
///
/// `instance` is the environment's stored `registry.environment_projects.instance_suffix`,
/// NOT a fresh draw: minting is the effect driver's job, and re-deriving under a
/// new suffix would orphan every resource the old one named. The suffix is what
/// makes delete-and-recreate safe — the recreated environment lands in a
/// different namespace and cannot inherit the deleted one's contents.
///
/// Validate the triple's assembled length with the provisioning coordinate validator and the
/// suffix through `InstanceSuffix`; a triple that does not fit is refused.
pub fn project_env_namespace(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
    instance: &InstanceSuffix,
) -> String {
    format!(
        "{}{INSTANCE_SUFFIX_SEPARATOR}{instance}",
        project_env_namespace_stem(org, project, env)
    )
}

/// The namespace's identity stem `wamn-<org>--<env>--<project>` — the part the
/// triple contributes, bounded by [`MAX_NAMESPACE_STEM_LEN`].
pub fn project_env_namespace_stem(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{NAMESPACE_PREFIX}{org}--{env}--{project}")
}

/// The per-project-env CDC credential Secret name:
/// `wamn-cdc-<org>--<env>--<project>` (wamn-l5i9.9). Distinct from the
/// `wamn-db-…` query-credential Secret — the replication credential is a
/// separate, higher-privilege tier (R8b), so a leaked query credential never
/// implies the WAL and vice versa.
pub fn project_env_cdc_secret_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{CDC_SECRET_PREFIX}{org}--{env}--{project}")
}

/// The shared name of the per-project-env CDC Postgres objects — publication,
/// failover replication slot, and replication role:
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

/// A workload credential Secret for one project deployment.
pub fn workload_secret_name(
    prefix: &str,
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("{prefix}{}", project_env_key(org, project, env))
}
/// A workload credential Secret shared by an org.
pub fn org_workload_secret_name(prefix: &str, org: &crate::OrgId) -> String {
    format!("{prefix}{org}")
}
/// A credential family's resource prefix.
pub fn workload_secret_prefix(stem: &str) -> String {
    format!("wamn-{stem}-")
}
/// A broker certificate Secret.
pub fn nats_certificate(key: &crate::ProjectEnvKey) -> String {
    format!("wamn-nats-{key}")
}
/// A control broker certificate Secret.
pub fn control_nats_certificate(org: &crate::OrgId) -> String {
    format!("wamn-nats-control-{org}")
}
/// The operator presenter Secret.
pub fn operator_secret(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    format!("wamn-pat-operator-{}", project_env_key(org, project, env))
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
/// The installed OCI pull credential Secret in each org namespace.
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
pub fn environment_resource_name(
    org: &crate::OrgId,
    project: &crate::ProjectId,
    env: &crate::EnvName,
) -> String {
    let hash = hex::encode(sha256(format!("{org}/{env}/{project}").as_bytes()));
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("e-{slug}-{}", &hash[..HASH_LEN])
}

/// The Helm release and host group shared by an org and environment.
pub fn environment_host_name(org: &crate::OrgId, env: &crate::EnvName) -> String {
    let hash = hex::encode(sha256(format!("{org}/{env}").as_bytes()));
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("h-{slug}-{}", &hash[..HASH_LEN])
}

/// The ServiceAccount shared by an environment's project resources.
pub fn environment_service_account(org: &crate::OrgId, env: &crate::EnvName) -> String {
    format!("wamn-host-{}", environment_key(org, env))
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

/// The database cluster of a dedicated org and recovery owner.
pub fn cluster(org: &crate::OrgId, owner: &crate::EnvName) -> String {
    format!("{org}-{owner}")
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
pub fn credential_login(prefix: &str, identity_hash: &str) -> wamn_identity_protocol::LoginName {
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
/// The audience of one org's control service.
pub fn org_audience(org: &crate::OrgId) -> String {
    format!("urn:wamn:control:{org}")
}
/// The reply subject prefix of one authenticated broker user.
pub fn inbox(user: &str) -> String {
    format!("_INBOX_{user}")
}

/// The release discovery filter for a validated environment slug.
pub fn application_release_filter(env: &crate::EnvName) -> String {
    let slug: String = env.as_str().chars().take(SLUG_LEN).collect();
    format!("^r-{slug}-")
}
/// Join the parts of a broker subject without reordering its protocol tokens.
pub fn nats_subject(parts: &[&str]) -> String {
    parts.join(".")
}
/// A broker token that prefixes an identity.
pub fn nats_token(prefix: &str, identity: &str, suffix: &str) -> String {
    format!("{prefix}{identity}{suffix}")
}

/// The memory key of one connection's current facts.
pub fn connection_key(project: &crate::ProjectEnvKey, id: impl std::fmt::Display) -> String {
    format!("{project}.connection.{id}")
}

/// Select the host pods belonging to one group.
pub fn host_selector(group: &str) -> String {
    format!("wasmcloud.com/hostgroup={group}")
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
        let resource = environment_resource_name(
            &"acme".parse().unwrap(),
            &"billing".parse().unwrap(),
            &"dev".parse().unwrap(),
        );
        assert!(resource.len() <= 47);
        assert_ne!(
            resource,
            environment_resource_name(
                &"acme".parse().unwrap(),
                &"shipping".parse().unwrap(),
                &"dev".parse().unwrap()
            )
        );
    }
}

/// The temporary template used to copy a local project database.
pub fn database_template(database: &str) -> String {
    format!("{database}--template")
}

/// The control host release and group of one org.
pub fn control_resource(org: &crate::OrgId) -> String {
    format!("c-{org}")
}

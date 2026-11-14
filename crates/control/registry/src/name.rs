//! Resource names derived from registry coordinates.

use wamn_digest::sha256;

/// The key of a project environment: `<org>--<env>--<project>` (owner
/// ruling of 2026-10-09 on `wamn-snz0`). The first apply, the analysis of
/// `env apply` and the host target all derive it here, so neither the
/// environment document nor the serving manifest repeats the composed key.
pub fn project_env_key(org: &str, project: &str, env: &str) -> String {
    format!("{org}--{env}--{project}")
}

/// The NATS user of the environment `project_env_key`. Its client certificate
/// carries this DNS name, and its replies use the inbox `_INBOX_<user>`
/// (docs/plan/alignment.md R5).
pub fn environment_nats_user(project_env_key: &str) -> String {
    format!("wamn-env-{project_env_key}")
}

/// The NATS user of the control host of `org`, which covers every
/// environment of the org (docs/plan/alignment.md R7).
pub fn control_nats_user(org: &str) -> String {
    format!("wamn-control-{org}")
}

/// Coordinates of one shared environment, without a project database.
pub fn environment_key(org: &str, env: &str) -> String {
    format!("{org}--{env}")
}

/// The Secret that stores a project deployment's credential values.
pub fn credential_secret_name(org: &str, project: &str, env: &str) -> String {
    format!("wamn-credential-{}", project_env_key(org, project, env))
}

/// The account used by a project deployment's hosts.
pub fn service_account_name(org: &str, project: &str, env: &str) -> String {
    format!("wamn-host-{}", project_env_key(org, project, env))
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
pub fn project_env_database_name(org: &str, project: &str, env: &str, instance: &str) -> String {
    format!(
        "{}{INSTANCE_SUFFIX_SEPARATOR}{instance}",
        project_env_database_stem(org, project, env)
    )
}

pub fn project_env_database_stem(org: &str, project: &str, env: &str) -> String {
    format!("{DB_PREFIX}{org}--{env}--{project}")
}

/// The per-project-env credential Secret name:
/// `wamn-db-<org>--<env>--<project>`. It stays triple-only because its namespace
/// carries the instance identity; its URL names the suffixed Postgres database.
pub fn project_env_secret_name(org: &str, project: &str, env: &str) -> String {
    project_env_database_stem(org, project, env)
}

/// The Secret of an org's control host that holds the administration login of
/// every environment of the org (docs/plan/platform-ui.md §4.4):
/// `wamn-control-administration-<org>`.
pub fn control_administration_secret_name(org: &str) -> String {
    format!("wamn-control-administration-{org}")
}

/// The key of one environment in [`control_administration_secret_name`]:
/// `<env>--<project>`, the file name the mounted Secret shows.
pub fn control_administration_key(project: &str, env: &str) -> String {
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
/// suffix with the provisioning instance validator before use; a triple that does not
/// fit is refused, not shortened.
pub fn project_env_namespace(org: &str, project: &str, env: &str, instance: &str) -> String {
    format!(
        "{}{INSTANCE_SUFFIX_SEPARATOR}{instance}",
        project_env_namespace_stem(org, project, env)
    )
}

/// The namespace's identity stem `wamn-<org>--<env>--<project>` — the part the
/// triple contributes, bounded by [`MAX_NAMESPACE_STEM_LEN`].
pub fn project_env_namespace_stem(org: &str, project: &str, env: &str) -> String {
    format!("{NAMESPACE_PREFIX}{org}--{env}--{project}")
}

/// The per-project-env CDC credential Secret name:
/// `wamn-cdc-<org>--<env>--<project>` (wamn-l5i9.9). Distinct from the
/// `wamn-db-…` query-credential Secret — the replication credential is a
/// separate, higher-privilege tier (R8b), so a leaked query credential never
/// implies the WAL and vice versa.
pub fn project_env_cdc_secret_name(org: &str, project: &str, env: &str) -> String {
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
pub fn cdc_object_name(org: &str, project: &str, env: &str, instance: &str) -> String {
    format!("{}__{instance}", cdc_object_name_stem(org, project, env))
}

pub fn cdc_object_name_stem(org: &str, project: &str, env: &str) -> String {
    let flat = |s: &str| s.replace('-', "_");
    format!(
        "{CDC_OBJECT_PREFIX}{}__{}__{}",
        flat(org),
        flat(env),
        flat(project)
    )
}

/// A workload credential Secret for one project deployment.
pub fn workload_secret_name(prefix: &str, org: &str, project: &str, env: &str) -> String {
    format!("{prefix}{}", project_env_key(org, project, env))
}
/// A workload credential Secret shared by an org.
pub fn org_workload_secret_name(prefix: &str, org: &str) -> String {
    format!("{prefix}{org}")
}
/// A credential family's resource prefix.
pub fn workload_secret_prefix(stem: &str) -> String {
    format!("wamn-{stem}-")
}
/// A broker certificate Secret.
pub fn nats_certificate(key: &str) -> String {
    format!("wamn-nats-{key}")
}
/// A control broker certificate Secret.
pub fn control_nats_certificate(org: &str) -> String {
    format!("wamn-nats-control-{org}")
}
/// The operator service principal of a project deployment.
pub fn operator_subject(org: &str, project: &str, env: &str) -> String {
    format!("wamn-operator-{}", project_env_key(org, project, env))
}
/// A scheduled application's service principal.
pub fn application_subject(key: &str, package: &str) -> String {
    format!("wamn-app-{key}--{package}")
}
/// The operator presenter Secret.
pub fn operator_secret(org: &str, project: &str, env: &str) -> String {
    format!("wamn-pat-operator-{}", project_env_key(org, project, env))
}
/// The prefix shared by a project deployment's event subjects.
pub fn event_prefix(org: &str, project: &str, env: &str) -> String {
    format!("evt.{org}.{env}.{project}.")
}
/// A subject within a project deployment's event stream.
pub fn event_subject(org: &str, project: &str, env: &str, tail: &str) -> String {
    format!("{}{tail}", event_prefix(org, project, env))
}
/// The host group for the local development loop.
pub fn dev_host_group(key: &str) -> String {
    format!("wamn-dev-{key}")
}
/// The Job and account of an org's control operation.
pub fn operation(org: &str) -> String {
    format!("wamn-operation-{org}")
}
/// The input Secret for a control operation.
pub fn operation_inputs(org: &str, digest: &str) -> String {
    format!("wamn-operation-{org}-{digest}")
}
/// The OCI credential Secret for a control operation.
pub fn operation_registry(org: &str) -> String {
    format!("wamn-operation-registry-{org}")
}

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
/// # Errors
///
/// When the coordinate is not valid.
pub fn environment_resource_name(org: &str, project: &str, env: &str) -> Result<String, NameError> {
    validate_coordinate("org", org, true)?;
    validate_coordinate("environment", env, false)?;
    validate_coordinate("project", project, true)?;
    let hash = hex::encode(sha256(format!("{org}/{env}/{project}").as_bytes()));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("e-{slug}-{}", &hash[..HASH_LEN]))
}

/// The Helm release and host group of one application.
pub fn application_release_name(
    org: &str,
    project: &str,
    env: &str,
    application: &str,
) -> Result<String, NameError> {
    validate_coordinate("org", org, true)?;
    validate_coordinate("environment", env, false)?;
    validate_coordinate("project", project, true)?;
    let hash = hex::encode(sha256(
        format!("{org}/{env}/{project}/{application}").as_bytes(),
    ));
    let slug: String = env.chars().take(SLUG_LEN).collect();
    Ok(format!("r-{slug}-{}", &hash[..HASH_LEN]))
}

/// Read the registry coordinate encoded in a canonical project deployment key.
pub fn parse_project_env_key(key: &str) -> Option<crate::Triple> {
    if !crate::identifiers::valid_project_env_key(key) {
        return None;
    }
    let mut parts = key.split("--");
    let org = parts.next()?;
    let env = parts.next()?;
    let project = parts.next()?;
    Some(crate::Triple::new(org, env, project))
}
/// A project deployment's complete event subscription.
pub fn stream_subjects(org: &str, project: &str, env: &str) -> String {
    event_subject(org, project, env, ">")
}
/// The event stream of one project deployment. Length framing separates every coordinate.
pub fn stream_name(org: &str, project: &str, env: &str) -> String {
    format!(
        "EVT_{}_{org}_{}_{env}_{}_{project}",
        org.len(),
        env.len(),
        project.len()
    )
}
/// The coordinate label in a host status response.
pub fn host_status(org: &str, project: &str, env: &str) -> String {
    format!("{org}/{env}/{project}")
}

/// The database cluster of a dedicated org and recovery owner.
pub fn cluster(org: &str, owner: &str) -> String {
    format!("{org}-{owner}")
}

/// A resource coordinate refused before its name can reach an external system.
#[derive(Debug)]
pub struct NameError {
    coordinate: &'static str,
    value: Box<str>,
}
impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid {} coordinate {:?}", self.coordinate, self.value)
    }
}
impl std::error::Error for NameError {}
fn validate_coordinate(
    coordinate: &'static str,
    value: &str,
    reserved: bool,
) -> Result<(), NameError> {
    if crate::identifiers::project_id_reason(value).is_none()
        && (!reserved || !crate::identifiers::project_id_is_reserved(value))
    {
        return Ok(());
    }
    Err(NameError {
        coordinate,
        value: value.into(),
    })
}

/// A resource derived from an existing resource and a role or purpose.
pub fn resource_suffix(resource: &str, purpose: &str) -> String {
    format!("{resource}-{purpose}")
}
/// A service resource derived from its project deployment.
pub fn service_resource(stem: &str, org: &str, project: &str, env: &str) -> String {
    format!("{stem}-{}", project_env_key(org, project, env))
}
/// A login name derived from its credential identity.
pub fn credential_login(prefix: &str, digest: &str) -> String {
    format!("{prefix}_{digest}")
}
/// The audience of one project database instance.
pub fn project_audience(org: &str, project: &str, env: &str, instance: &str) -> String {
    format!("urn:wamn:project-env:{org}:{env}:{project}:{instance}")
}
/// The audience of one org's control service.
pub fn control_audience(org: &str) -> String {
    format!("urn:wamn:control:{org}")
}
/// The reply subject prefix of one authenticated broker user.
pub fn inbox(user: &str) -> String {
    format!("_INBOX_{user}")
}

/// The release discovery filter for a validated environment slug.
pub fn application_release_filter(env: &str) -> String {
    let slug: String = env.chars().take(SLUG_LEN).collect();
    format!("^r-{slug}-")
}
/// The advisory stream of one event source.
pub fn delivery_advisory_stream(source: &str) -> String {
    format!("WAMN_EVENT_ADVISORIES_{source}")
}
/// The advisory subscriptions of one event source.
pub fn delivery_advisory_subjects(source: &str) -> [String; 2] {
    ["MAX_DELIVERIES", "MSG_TERMINATED"]
        .map(|type_| format!("$JS.EVENT.ADVISORY.CONSUMER.{type_}.{source}.*"))
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
pub fn connection_key(project: &str, id: impl std::fmt::Display) -> String {
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
    fn resource_coordinates_round_trip_and_keep_project_databases_separate() {
        let first = crate::Triple::new("acme", "dev", "billing");
        let key = project_env_key(&first.org, &first.project, first.env.as_str());
        assert_eq!(parse_project_env_key(&key), Some(first));
        assert_ne!(key, project_env_key("acme", "shipping", "dev"));
        assert_ne!(
            project_env_database_name("acme", "billing", "dev", "abcdefgh"),
            project_env_database_name("acme", "shipping", "dev", "abcdefgh")
        );
        let resource = environment_resource_name("acme", "billing", "dev").unwrap();
        assert!(resource.len() <= 47);
        assert_ne!(
            resource,
            environment_resource_name("acme", "shipping", "dev").unwrap()
        );
        assert!(environment_resource_name("acme", "billing", "Bad Env").is_err());
        assert!(parse_project_env_key("acme--project").is_none());
    }
}

/// The temporary template used to copy a local project database.
pub fn database_template(database: &str) -> String {
    format!("{database}--template")
}

/// The control host release and group of one org.
pub fn control_resource(org: &str) -> String {
    format!("c-{org}")
}

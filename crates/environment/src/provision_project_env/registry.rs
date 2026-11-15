//! Environment instance creation, registry reads, and control-store projection.

use anyhow::Context as _;
use ring::rand::SecureRandom as _;

use super::{
    INSTANCE_SUFFIX_LEN, Org, Placement, SystemRandom, Triple, cluster_of, read_env_policy,
    validate_instance_suffix,
};

/// Alphabet of the provision-minted instance suffix: `[a-z0-9]`, 36 symbols, so
/// eight of them carry ~41 bits. Narrower than an identity slug's on purpose —
/// the suffix is the LAST bytes of a DNS-1123 label, which must end alphanumeric.
pub(super) const INSTANCE_SUFFIX_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Largest multiple of the alphabet size that fits in a byte (252). A draw at or
/// above it is redrawn rather than folded: a plain `% 36` would over-weight the
/// first four symbols, and the suffix's uniform randomness IS the non-reuse
/// mechanism (wamn-0h0g.13.57) — nothing else keeps a recreated environment off
/// a deleted one's names.
const INSTANCE_SUFFIX_REJECT_AT: usize = 256 - 256 % INSTANCE_SUFFIX_ALPHABET.len();

/// Mint one environment's instance suffix. The randomness is the whole
/// uniqueness mechanism: no naming registry, no collision-retry loop, no
/// derivation from the triple (an owner ruling of wamn-0h0g.13.57).
///
/// The mint lives HERE and not in `wamn-control-provision` because that crate is
/// deliberately pure — no DB, no K8s client, no clock, and no entropy. It takes
/// the suffix as a parameter and derives names from it.
pub(super) fn mint_instance_suffix() -> anyhow::Result<String> {
    let random = SystemRandom::new();
    let mut suffix = String::with_capacity(INSTANCE_SUFFIX_LEN);
    let mut draw = [0_u8; INSTANCE_SUFFIX_LEN];
    while suffix.len() < INSTANCE_SUFFIX_LEN {
        random.fill(&mut draw).map_err(|_| {
            anyhow::anyhow!("operating system could not supply instance-suffix entropy")
        })?;
        for byte in draw {
            if usize::from(byte) >= INSTANCE_SUFFIX_REJECT_AT {
                continue;
            }
            let index = usize::from(byte) % INSTANCE_SUFFIX_ALPHABET.len();
            suffix.push(char::from(INSTANCE_SUFFIX_ALPHABET[index]));
            if suffix.len() == INSTANCE_SUFFIX_LEN {
                break;
            }
        }
    }
    Ok(suffix)
}

/// Derive the target cluster from the org's placement and the named policy.
/// The `env` argument names a policy, which can differ from the environment slug.
/// A pooled org uses its pool. A dedicated org uses the policy's recovery domain
/// through [`cluster_of`]. The caller supplies a registry reader credential.
pub async fn resolve_cluster(system_url: &str, org: &str, env: &str) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = resolve_cluster_on(&mut client, org, env).await;
    drop(client);
    result
}

/// Resolve placement through the supplied system database connection.
pub(crate) async fn resolve_cluster_on(
    client: &mut sqlx::PgConnection,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    let mut transaction = wamn_platform_identity::transaction::read_connection(
        client,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    let row = wamn_registry::deployment::org_placement(&mut transaction, org)
        .await
        .context("read org placement")?
        .with_context(|| {
            format!(
                "org {org:?} is not registered: run provision-org before provisioning a project-env"
            )
        })?;
    let placement_type = row.placement_type;
    let pool = row.pool_cluster;
    let placement = match placement_type.as_str() {
        "pooled" => Placement::Pooled {
            pool: pool.context("pooled org row is missing its pool_cluster")?,
        },
        "dedicated" => Placement::Dedicated,
        other => anyhow::bail!("unknown placement_type {other:?} for org {org:?}"),
    };
    let org_obj = Org {
        id: org.to_string(),
        placement,
    };
    // The env must name a policy in the ORG's own set (8df.4 — its recovery
    // domain drives the derivation); a pooled org ignores the policy but the env
    // must still resolve.
    let policy = read_env_policy(&mut transaction, org, env)
        .await?
        .with_context(|| {
            format!(
                "env {env:?} names none of org {org:?}'s env policies — provision-org stamps them \
             from a template; customize/add rows in registry.env_policies"
            )
        })?;
    transaction.commit().await?;
    Ok(cluster_of(&org_obj, &policy).name)
}

/// Read one project-env's stored instance suffix from the registry.
pub async fn read_project_env_instance(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let mut transaction = wamn_platform_identity::transaction::read_connection(
        &mut client,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    let result = async {
        let stored = wamn_registry::deployment::instance_suffix(&mut transaction, triple)
            .await
            .context("read project deployment")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        validate_instance_suffix(&stored)
            .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
        Ok(stored)
    }
    .await;
    transaction.commit().await?;
    drop(client);
    result
}

/// Read the policy name of one project-env's registry row. The environment
/// document owns it (docs/architecture/overview.md#environment-lifecycle).
pub async fn read_project_env_policy(system_url: &str, triple: &Triple) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let mut transaction = wamn_platform_identity::transaction::read_connection(
        &mut client,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    let result = async {
        wamn_registry::deployment::project_policy(&mut transaction, triple)
            .await
            .context("read the project deployment policy")?
            .with_context(|| format!("project-env {triple} is not recorded"))
    }
    .await;
    transaction.commit().await?;
    drop(client);
    result
}

/// Record the project and the provisioned project-env in the registry (idempotent).
/// The supplied credential holds the grants used by the registry builders.
///
/// Returns the environment's STORED instance suffix, which is `minted` only on a
/// first provision — see [`do_record_project_env`]. The project derives from
/// the coordinate (`project_env_key`).
#[expect(clippy::too_many_arguments)]
pub(super) async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    policy: Option<&str>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: Option<bool>,
) -> anyhow::Result<String> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = do_record_project_env(
        &mut client,
        triple,
        policy,
        target,
        secret_name,
        secret_namespace,
        minted,
        disposable,
    )
    .await;
    drop(client);
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn do_record_project_env(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
    policy: Option<&str>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: Option<bool>,
) -> anyhow::Result<String> {
    let mut transaction = super::provisioning_transaction(client).await?;
    wamn_registry::deployment::record_project(&mut transaction, &triple.org, &triple.project)
        .await
        .context("record project")?;
    let target_id = wamn_registry::deployment::target_id(&mut transaction, target)
        .await
        .context("read deployment target")?
        .with_context(|| format!("target {target:?} is not registered"))?;
    let stored = wamn_registry::deployment::record(
        &mut transaction,
        &wamn_registry::deployment::Registration {
            triple,
            secret_name,
            secret_namespace,
            instance_suffix: minted,
            disposable,
            policy: policy.unwrap_or(triple.env.as_str()),
            target_id: &target_id,
        },
    )
    .await
    .context("record project deployment")?;
    transaction
        .commit()
        .await
        .context("commit registry coordinates")?;
    // Read-or-mint: the upsert RETURNS the STORED suffix, which is the freshly
    // minted one on a first provision and the EXISTING one when this triple was
    // already provisioned — the upsert deliberately never refreshes it, because
    // re-minting would orphan every resource the old suffix named. The registry
    // is a trust boundary, so the value is re-validated before any name derives
    // from it.
    validate_instance_suffix(&stored)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    Ok(stored)
}

/// Require the environment's registry coordinate to exist.
pub async fn require_connected(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<()> {
    let present = wamn_registry::deployment::exists(client, triple).await?;
    anyhow::ensure!(present, "project environment is not recorded");
    Ok(())
}

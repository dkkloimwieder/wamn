//! Environment instance creation, registry reads, and control-store projection.

use anyhow::Context as _;
use ring::rand::SecureRandom as _;
use wamn_registry::instance_suffix::InstanceSuffix;

use ring::rand::SystemRandom;
use wamn_registry::Triple;

use wamn_provision::INSTANCE_SUFFIX_LEN;

/// Alphabet of the provision-minted instance suffix: `[a-z0-9]`, 36 symbols, so
/// eight of them carry ~41 bits. Narrower than an identity slug's on purpose —
/// the suffix is the LAST bytes of a DNS-1123 label, which must end alphanumeric.
const INSTANCE_SUFFIX_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

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
/// The mint lives HERE and not in `wamn-provision` because that crate is
/// deliberately pure — no DB, no K8s client, no clock, and no entropy. It takes
/// the suffix as a parameter and derives names from it.
pub(crate) fn mint_instance_suffix() -> anyhow::Result<InstanceSuffix> {
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
    Ok(suffix.try_into()?)
}

/// Read one project-env's stored instance suffix from the registry.
pub async fn read_project_env_instance(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<InstanceSuffix> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let mut transaction = wamn_identity::transaction::read_connection(
        &mut client,
        &wamn_provision::PlatformComponent::Provisioning.principal_id(),
    )
    .await?;
    let result = async {
        let stored = wamn_registry::deployment::instance_suffix(&mut transaction, triple)
            .await
            .context("read project deployment")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        Ok(stored)
    }
    .await;
    transaction.commit().await?;
    drop(client);
    result
}

/// Read the policy name of one project-env's registry row. The environment
/// document owns it (docs/architecture/overview.md#environment-lifecycle).
pub async fn read_project_env_policy(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<wamn_registry::EnvName> {
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let mut transaction = wamn_identity::transaction::read_connection(
        &mut client,
        &wamn_provision::PlatformComponent::Provisioning.principal_id(),
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
pub(crate) async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    policy: Option<&wamn_registry::EnvName>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &InstanceSuffix,
    disposable: Option<bool>,
) -> anyhow::Result<InstanceSuffix> {
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
    policy: Option<&wamn_registry::EnvName>,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &InstanceSuffix,
    disposable: Option<bool>,
) -> anyhow::Result<InstanceSuffix> {
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
            policy: policy.unwrap_or(&triple.env),
            target_id: &target_id,
        },
    )
    .await
    .context("record project deployment")?;
    transaction
        .commit()
        .await
        .context("commit registry coordinates")?;
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

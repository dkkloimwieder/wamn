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
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<InstanceSuffix> {
    let mut transaction = wamn_identity::transaction::read_connection(
        client,
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
    result
}

/// Record the project and its deployment in the caller's reservation transaction.
///
/// Returns the stored instance suffix. A new deployment uses `minted`.
#[expect(clippy::too_many_arguments)]
pub(crate) async fn record_project_env(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    triple: &Triple,
    readiness_budget_seconds: i32,
    drain_bound_seconds: i32,
    target: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &InstanceSuffix,
    disposable: Option<bool>,
) -> anyhow::Result<InstanceSuffix> {
    wamn_registry::deployment::record_project(transaction, &triple.org, &triple.project)
        .await
        .context("record project")?;
    let target_id = wamn_registry::deployment::target_id(transaction, target)
        .await
        .context("read deployment target")?
        .with_context(|| format!("target {target:?} is not registered"))?;
    wamn_registry::deployment::record(
        transaction,
        &wamn_registry::deployment::Registration {
            triple,
            secret_name,
            secret_namespace,
            instance_suffix: minted,
            disposable,
            readiness_budget_seconds,
            drain_bound_seconds,
            target_id: &target_id,
        },
    )
    .await
    .context("record project deployment")
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

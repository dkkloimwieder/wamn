//! Environment instance creation, registry reads, and control-store projection.

use anyhow::Context as _;
use ring::rand::SecureRandom as _;

use super::{
    INSTANCE_SUFFIX_LEN, NoTls, Org, Placement, SystemRandom, Triple, cluster_of,
    ensure_env_policy_durability_schema, read_env_policy, sql, validate_instance_suffix,
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

/// Read the org's placement + the env's policy from the registry and **derive**
/// the target cluster via [`cluster_of`] (D18): a pooled org collapses onto its
/// pool; a dedicated org owns `<org>-<owner(env)>`. Connects as the `wamn_system`
/// owner (`SET ROLE`). Shared with the `enable-cdc-project-env` overlay
/// (wamn-l5i9.9), which targets the same derived cluster.
pub(crate) async fn resolve_cluster(
    system_url: &str,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_resolve_cluster(&client, org, env).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_resolve_cluster(
    client: &tokio_postgres::Client,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    ensure_env_policy_durability_schema(client).await?;
    let row = client
        .query_opt(
            wamn_control_registry::sql::select_org_placement_sql(),
            &[&org],
        )
        .await
        .context("read org placement")?
        .with_context(|| {
            format!(
                "org {org:?} is not registered: run provision-org before provisioning a project-env"
            )
        })?;
    let placement_kind: String = row.get("placement_kind");
    let pool: Option<String> = row.get("pool_cluster");
    let placement = match placement_kind.as_str() {
        "pooled" => Placement::Pooled {
            pool: pool.context("pooled org row is missing its pool_cluster")?,
        },
        "dedicated" => Placement::Dedicated,
        other => anyhow::bail!("unknown placement_kind {other:?} for org {org:?}"),
    };
    let org_obj = Org {
        id: org.to_string(),
        placement,
    };
    // The env must name a policy in the ORG's own set (8df.4 — its recovery
    // domain drives the derivation); a pooled org ignores the policy but the env
    // must still resolve.
    let policy = read_env_policy(client, org, env).await?.with_context(|| {
        format!(
            "env {env:?} names none of org {org:?}'s env policies — provision-org stamps them \
             from a template; customize/add rows in registry.env_policies"
        )
    })?;
    Ok(cluster_of(&org_obj, &policy).name)
}

/// Read one project-env's stored instance suffix from the registry.
pub(crate) async fn read_project_env_instance(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        let env = triple.env.as_str();
        let row = client
            .query_opt(
                &wamn_control_registry::sql::select_project_env_sql(),
                &[&triple.org, &triple.project, &env],
            )
            .await
            .context("read registry.project_envs row")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        let stored: String = row.get("instance_suffix");
        validate_instance_suffix(&stored)
            .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
        Ok(stored)
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

/// Record the project and the provisioned project-env in the registry (idempotent).
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner — the
/// wamn-q3n.3 apply pattern), then runs the pure `wamn-control-registry` builders.
///
/// Returns the environment's STORED instance suffix, which is `minted` only on a
/// first provision — see [`do_record_project_env`].
pub(super) async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    tenant: Option<&str>,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: bool,
) -> anyhow::Result<String> {
    let (mut client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_record_project_env(
        &mut client,
        triple,
        tenant,
        secret_name,
        secret_namespace,
        minted,
        disposable,
    )
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

pub(super) async fn do_record_project_env(
    client: &mut tokio_postgres::Client,
    triple: &Triple,
    tenant: Option<&str>,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: bool,
) -> anyhow::Result<String> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    client
        .execute(
            wamn_control_registry::sql::upsert_project_sql(),
            &[&triple.org, &triple.project],
        )
        .await
        .context("upsert registry.projects row")?;
    let env = triple.env.as_str();
    let row = client
        .query_one(
            wamn_control_registry::sql::upsert_project_env_sql(),
            &[
                &triple.org,
                &triple.project,
                &env,
                &secret_name,
                &secret_namespace,
                &minted,
                &disposable,
            ],
        )
        .await
        .context("upsert registry.project_envs row")?;
    // Read-or-mint: the upsert RETURNS the STORED suffix, which is the freshly
    // minted one on a first provision and the EXISTING one when this triple was
    // already provisioned — the upsert deliberately never refreshes it, because
    // re-minting would orphan every resource the old suffix named. The registry
    // is a trust boundary, so the value is re-checked before any name derives
    // from it.
    let stored: String = row.get(0);
    let stored_disposable: bool = row.get(1);
    validate_instance_suffix(&stored)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    project_tenant_environment(client, triple, tenant, &stored, stored_disposable).await?;
    Ok(stored)
}

/// Probe for the control store's environment projection without assuming it.
const CONTROL_PROJECTION_INSTALLED_SQL: &str =
    "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL";

/// Claim the projected tenant for the transaction's RLS policy.
const CLAIM_PROJECTED_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Project the recorded project-env into the control store, beside the facts its
/// `disposable` marker governs (wamn-10yt.38).
///
/// `registry.project_envs` stays the AUTHORITY. This copy carries that row's
/// identity — the triple plus the STORED instance suffix, read back from the
/// upsert rather than assumed — so the admit path resolves the marker locally
/// and a disagreement between the two planes refuses instead of passing.
///
/// ABSENCE MEANS DURABLE, so a system database with no control store, or a
/// provisioning that names no tenant, records nothing here and admits exactly as
/// it always did. Only a DISPOSABLE environment insists, because for it the
/// missing projection would be the difference between an author's edit landing
/// and an author's edit being refused.
pub(crate) async fn project_tenant_environment(
    client: &mut tokio_postgres::Client,
    triple: &Triple,
    tenant: Option<&str>,
    instance_suffix: &str,
    disposable: bool,
) -> anyhow::Result<()> {
    let installed: bool = client
        .query_one(CONTROL_PROJECTION_INSTALLED_SQL, &[])
        .await
        .context("probe the control store's environment projection")?
        .get(0);
    if !installed {
        anyhow::ensure!(
            !disposable,
            "a disposable project-env needs catalog.tenant_environments: apply \
             wamn_control_provision::CONTROL_PORTABLE_STORE_SQL to the system database first"
        );
        return Ok(());
    }
    let Some(tenant) = tenant else {
        anyhow::ensure!(
            !disposable,
            "a disposable project-env needs --tenant: the admit path resolves the \
             marker by the tenant its component facts are keyed on"
        );
        return Ok(());
    };
    let env = triple.env.as_str();
    let transaction = client
        .transaction()
        .await
        .context("begin the project-env control projection")?;
    transaction
        .query_one(CLAIM_PROJECTED_TENANT_SQL, &[&tenant])
        .await
        .context("claim the projected tenant")?;
    transaction
        .execute(
            sql::insert_tenant_environment_sql(),
            &[
                &tenant,
                &triple.org,
                &triple.project,
                &env,
                &instance_suffix,
                &disposable,
            ],
        )
        .await
        .context("project the project-env into the control store")?;
    // A losing insert waits, then this read locks the committed winner.
    let row = transaction
        .query_one(sql::read_tenant_environment_sql(), &[&tenant])
        .await
        .context("read the projected environment identity")?;
    let recorded = Triple::new(
        row.try_get::<_, String>(0)?,
        row.try_get::<_, String>(1)?,
        row.try_get::<_, String>(2)?,
    );
    wamn_control_provision::check_tenant_environment_identity(tenant, &recorded, triple)?;
    transaction
        .execute(
            sql::refresh_tenant_environment_sql(),
            &[&tenant, &instance_suffix, &disposable],
        )
        .await
        .context("refresh the projected environment")?;

    transaction
        .commit()
        .await
        .context("commit the project-env control projection")
}

//! Internal bootstrap steps and authenticated capture-gap clients.

use std::fmt;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use serde_json::Value;
use wamn_control_provision::Template;
use wamn_environment::provision_org::{self, OrgRecord, ProvisionOrgRequest, ProvisionedOrg};
use wamn_environment::provision_system::{self, ProvisionSystemRequest};
use wamn_identity_client::PatIssuerConfig;

#[derive(Debug, Args)]
pub struct ProvisionSystemArgs {
    /// Superuser Postgres URL to the empty system database (`wamn_system`),
    /// whose owner role `wamn_system` already exists.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_url: String,
    /// The restricted platform migration connection for explicit PostgreSQL access.
    #[arg(long, hide_env_values = true)]
    pub migration_database_url: Option<String>,

    /// Email domain of the platform principal rows, written to
    /// `registry.meta.platform_domain`.
    #[arg(long)]
    pub platform_domain: Option<String>,
}

/// Install the control store into an empty system database.
pub async fn provision_system(args: ProvisionSystemArgs) -> anyhow::Result<()> {
    let request = ProvisionSystemRequest {
        migration_database_url: args.migration_database_url,
        system_database_url: args.system_url,
        platform_domain: args
            .platform_domain
            .context("--platform-domain is required")?,
    };
    provision_system::provision_system(&request).await?;
    provision_system::provision_apply(&request.system_database_url).await?;
    println!("provision-system: control store installed");
    Ok(())
}

#[derive(Debug, Args)]
pub struct RecoverCaptureGapArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub env: String,
}

#[derive(Debug, Args)]
pub struct CloseCaptureGapArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub env: String,
}

/// TLS credentials and the identity service endpoint for operator PAT issuance.
#[derive(Clone, Default, Args)]
pub struct PatIssuerArgs {
    /// HTTPS base URL of the identity service. Its path is preserved.
    #[arg(long = "pat-issuer", env = "WAMN_PAT_ISSUER", hide_env_values = true)]
    pub endpoint: Option<String>,

    /// PEM certificate chain for the provisioning operator.
    #[arg(
        long = "pat-client-cert",
        env = "WAMN_PAT_CLIENT_CERT",
        hide_env_values = true
    )]
    pub client_cert: Option<PathBuf>,

    /// PEM private key for the provisioning operator.
    #[arg(
        long = "pat-client-key",
        env = "WAMN_PAT_CLIENT_KEY",
        hide_env_values = true
    )]
    pub client_key: Option<PathBuf>,

    /// PEM roots that replace the default trust roots for this identity service.
    #[arg(
        long = "pat-server-ca",
        env = "WAMN_PAT_SERVER_CA",
        hide_env_values = true
    )]
    pub server_ca: Option<PathBuf>,
}

impl fmt::Debug for PatIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PatIssuerArgs")
            .field("endpoint", &self.endpoint.as_ref().map(|_| "<redacted>"))
            .field(
                "client_cert",
                &self.client_cert.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "client_key",
                &self.client_key.as_ref().map(|_| "<redacted>"),
            )
            .field("server_ca", &self.server_ca.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl From<PatIssuerArgs> for PatIssuerConfig {
    fn from(args: PatIssuerArgs) -> Self {
        Self {
            endpoint: args.endpoint,
            client_cert: args.client_cert,
            client_key: args.client_key,
            server_ca: args.server_ca,
        }
    }
}

/// Recover one capture gap and print the row it wrote.
pub async fn recover_capture_gap(args: RecoverCaptureGapArgs) -> anyhow::Result<()> {
    let value = crate::operation_verbs::ControlHost::from_env()?
        .call(
            "capture_gap.recover",
            &serde_json::json!({"project":args.project,"env":args.env}),
        )
        .await?;
    println!("{value}");
    Ok(())
}

/// Close the newest recorded gap through authenticated control.
pub async fn close_capture_gap(args: CloseCaptureGapArgs) -> anyhow::Result<()> {
    let value = crate::operation_verbs::ControlHost::from_env()?
        .call(
            "capture_gap.close",
            &serde_json::json!({"project":args.project,"env":args.env}),
        )
        .await?;
    println!("{value}");
    Ok(())
}

/// The named org preset `provision-org` stamps (the `Tier` successor —
/// [`wamn_control_provision::Template`]).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TemplateArg {
    /// Pre-contract: placed on the shared `--pool` cluster (owns no clusters;
    /// the RLS base grants are load-bearing there); stamps `dev` + `prod`.
    Trials,
    /// Standard paying tier: owns per-recovery-domain clusters; stamps `dev` /
    /// `prod` (own) + `canary` sharing prod's recovery domain (T2).
    Standard,
    /// Regulated tier: like standard, but `canary` owns its recovery domain — a
    /// third cluster (T4).
    Dedicated,
}

impl TemplateArg {
    pub(crate) fn template(self) -> Template {
        match self {
            TemplateArg::Trials => Template::trials(),
            TemplateArg::Standard => Template::standard(),
            TemplateArg::Dedicated => Template::dedicated(),
        }
    }
}

#[derive(Debug, Args)]
pub struct ProvisionOrgArgs {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub org: String,

    /// The template preset to stamp: `trials` (pooled, record-only), `standard`
    /// (dedicated, canary shared-with prod), or `dedicated` (canary own).
    #[arg(long, value_enum)]
    pub template: TemplateArg,

    /// The shared pool cluster a `trials` org is placed on. Ignored for
    /// dedicated templates. Default: the shipped `wamn-pg` pool.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// and its policy rows are recorded and read back (for cluster sizing). Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Omit to render/plan only (with template policies).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: Option<String>,

    /// The email of the org's admin. The run writes that user's active org
    /// membership and `org-admin` row. When the identity store has no user,
    /// the run creates this user, the platform's first; otherwise the email
    /// must name an existing user. It is required with
    /// `--system-database-url` and refused without it, because a render-only
    /// run cannot look the email up or write a row.
    #[arg(long, value_name = "EMAIL")]
    pub admin: Option<String>,

    /// Namespace of the CNPG `Cluster`. Everything that lives beside it (the
    /// `Database`, the `ObjectStore`, the `ScheduledBackup`) shares it.
    #[arg(long, env = "WAMN_CLUSTER_NAMESPACE", default_value = "wamn-system")]
    pub cluster_namespace: String,

    /// Write the rendered `Cluster` CRs (a JSON `List`) here; `-` = stdout
    /// (default). Empty for a pooled org (no owned clusters).
    #[arg(long)]
    pub emit_clusters: Option<PathBuf>,
}

/// Stamp one org, then print what was recorded and write the CRs it owns.
pub async fn provision_org(args: ProvisionOrgArgs) -> anyhow::Result<()> {
    let record = match (args.system_database_url, args.admin) {
        (Some(system_database_url), Some(admin)) => Some(OrgRecord {
            system_database_url,
            admin,
        }),
        (None, None) => None,
        (None, Some(_)) => anyhow::bail!("--admin needs --system-database-url"),
        (Some(_), None) => anyhow::bail!("--system-database-url needs --admin"),
    };
    // The platform install provisions the org before auth NATS and any host
    // exist, so it sets no WAMN_NATS_URL and there is no cache to drop.
    let changes = if record.is_some() && std::env::var_os("WAMN_NATS_URL").is_some() {
        wamn_broker::Publisher::connect(&wamn_broker::nats::PlatformNats::from_env()?).await?
    } else {
        if record.is_some() {
            eprintln!(
                "WAMN_NATS_URL is unset: a host keeps a revoked login for its auth memory TTL"
            );
        }
        wamn_broker::Publisher::default()
    };
    let control = record
        .as_ref()
        .map(|record| (record.system_database_url.clone(), args.org.clone()));
    let provisioned = provision_org::provision_org_with_changes(
        ProvisionOrgRequest {
            org: args.org,
            template: args.template.template(),
            pool: args.pool,
            record,
            cluster_namespace: args.cluster_namespace,
        },
        &changes,
    )
    .await?;
    if let Some((input, org)) = control {
        provision_org::provision_control_role(
            &input,
            &org,
            wamn_control_provision::WorkloadRoleFamily::Control,
        )
        .await?;
    }
    print_provisioned_org(&provisioned);
    let Some(set) = &provisioned.clusters else {
        return Ok(());
    };
    let emit_clusters = args.emit_clusters.unwrap_or_else(|| PathBuf::from("-"));
    write_json(&emit_clusters, &k8s_list(&set.clusters)).context("emit Cluster CRs")?;
    Ok(())
}

/// Print the lines that report one org provisioning run.
fn print_provisioned_org(provisioned: &ProvisionedOrg) {
    let id = &provisioned.org.id;
    let tpl = provisioned.template_name;
    match provisioned.stamped_policies {
        Some(n) => println!(
            "recorded org {id:?} (template {tpl:?}) in registry.orgs + {n} env \
             policy row(s) stamped insert-if-absent (wamn_system)"
        ),
        None => println!("(no --system-database-url: org not recorded; template policies used)"),
    }
    if let Some(set) = &provisioned.clusters {
        let names: Vec<String> = set
            .clusters
            .iter()
            .map(|c| c["metadata"]["name"].as_str().unwrap_or("?").to_string())
            .collect();
        println!(
            "org {id:?} (template {tpl:?}, dedicated): {n} cluster(s) [{names}], \
             sized by the org's env policies",
            n = set.clusters.len(),
            names = names.join(", "),
        );
    } else if let wamn_control_provision::Placement::Pooled { pool } = &provisioned.org.placement {
        println!(
            "org {id:?} (template {tpl:?}, pooled): placed on the shared pool {pool:?} \
             (owns no clusters)"
        );
    }
}

/// Wrap CRs in a Kubernetes `v1` `List` so `kubectl apply -f` accepts the whole
/// set from one file/stream. An empty items list is a valid, harmless no-op apply.
fn k8s_list(items: &[Value]) -> Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": items,
    })
}

fn write_json(path: &PathBuf, doc: &Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(doc)?;
    if path.as_os_str() == "-" {
        println!("{text}");
    } else {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

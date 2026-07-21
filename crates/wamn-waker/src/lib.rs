//! wamn-waker: the scale-to-zero / parked-project wake actuator (wamn-fqg.12, POC-F3).
//!
//! A tiny always-on service that turns a doorbell hint into a Kubernetes scale
//! action: when the dispatcher publishes `wamn.doorbell.<tenant>` for a project
//! whose runner Deployment sits at 0 replicas, the waker scales it `0 -> 1` so
//! the runner comes up, subscribes to the same doorbell, and drains the enqueued
//! run. It scales UP only — idle `-> 0` (scale-down) automation is out of scope.
//!
//! ## Topology (decided; do not relitigate)
//! The dispatcher deliberately never talks to Kubernetes
//! (`automountServiceAccountToken: false`) and KEDA is not installed, so this
//! waker is the ONE component granted the k8s `deployments/scale` privilege
//! (`deploy/platform/waker.yaml`). It has **no polling loop of its own**: a doorbell
//! published while the runner is parked at 0 is lost, but the dispatcher
//! re-hints every currently-due, unleased queue row on every sweep
//! (`wamn_run_queue::parked_due_sql`) — so a lost first hint self-heals on the
//! next sweep, and that dispatcher re-hint IS the waker's retry path. The waker
//! only ever reacts to a hint; async-nats reconnects the subscription itself.
//!
//! ## Decisions vs. effects
//! The load-bearing decision ([`decide`]) is pure over `(tenant, mappings,
//! current_replicas)` and unit-tested; the k8s call ([`KubeScale`]) and the NATS
//! doorbell are the effect shell. The scale client is shared with the
//! `wakeproof` gate (which parks + restores the runner around the proof).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use futures_util::StreamExt as _;

/// The in-cluster service-account directory Kubernetes mounts into every pod.
const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
/// The in-cluster Kubernetes API server (its serving cert is signed by `ca.crt`).
const API_BASE: &str = "https://kubernetes.default.svc";
/// The doorbell subject prefix the dispatcher publishes to (`+ <tenant>`).
const DOORBELL_PREFIX: &str = "wamn.doorbell.";

// ---------------------------------------------------------------------------
// Pure decision logic (no I/O — unit-tested; the mutation loop's assert target).
// ---------------------------------------------------------------------------

/// A parsed `--wake <tenant>=<deployment>` mapping: the doorbell tenant to watch
/// and the Deployment to scale up when it fires while parked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeMapping {
    pub tenant: String,
    pub deployment: String,
}

/// Parse one `<tenant>=<deployment>` mapping (the clap value parser for
/// `--wake`). Returns a `String` error so clap can surface it directly.
pub fn parse_wake_mapping(s: &str) -> Result<WakeMapping, String> {
    let (tenant, deployment) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid --wake {s:?}: expected <tenant>=<deployment>"))?;
    if tenant.is_empty() || deployment.is_empty() {
        return Err(format!(
            "invalid --wake {s:?}: tenant and deployment must both be non-empty"
        ));
    }
    Ok(WakeMapping {
        tenant: tenant.to_string(),
        deployment: deployment.to_string(),
    })
}

/// The Deployment a doorbell tenant maps to, if any.
pub fn mapping_for<'a>(tenant: &str, mappings: &'a [WakeMapping]) -> Option<&'a WakeMapping> {
    mappings.iter().find(|m| m.tenant == tenant)
}

/// What a doorbell hint should actuate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeAction {
    /// Do nothing: the tenant is unmapped, or its Deployment already has replicas.
    None,
    /// The mapped Deployment is parked (`replicas == 0`); scale it to `to`.
    Scale { deployment: String, to: i32 },
}

/// Decide the wake action from a hint's tenant, the configured mappings, and the
/// mapped Deployment's CURRENT (desired) replica count.
///
/// A wake fires only for a MAPPED tenant whose Deployment sits at exactly 0
/// replicas (parked). A Deployment already running (`replicas > 0`) is a no-op —
/// the runner is up and drains the hint itself. This is the load-bearing
/// decision the `wakeproof` gate and the fqg.12 mutation loop pin.
pub fn decide(tenant: &str, mappings: &[WakeMapping], current_replicas: i32) -> WakeAction {
    match mapping_for(tenant, mappings) {
        None => WakeAction::None,
        Some(m) => {
            if current_replicas == 0 {
                WakeAction::Scale {
                    deployment: m.deployment.clone(),
                    to: 1,
                }
            } else {
                WakeAction::None
            }
        }
    }
}

/// Extract the tenant from a doorbell subject `wamn.doorbell.<tenant>`.
fn tenant_of_subject(subject: &str) -> Option<&str> {
    subject.strip_prefix(DOORBELL_PREFIX)
}

/// Parse the Kubernetes `Scale` subresource JSON — the desired
/// (`.spec.replicas`) and observed (`.status.replicas`) counts. Absent fields
/// read as 0 (a freshly-created Deployment reports no status replicas yet).
fn parse_scale(body: &str) -> anyhow::Result<Scale> {
    let v: serde_json::Value = serde_json::from_str(body).context("parse Scale JSON")?;
    Ok(Scale {
        spec_replicas: v["spec"]["replicas"].as_i64().unwrap_or(0) as i32,
        status_replicas: v["status"]["replicas"].as_i64().unwrap_or(0) as i32,
    })
}

// ---------------------------------------------------------------------------
// The in-cluster Kubernetes scale client (effect shell; shared with the gate).
// ---------------------------------------------------------------------------

/// A Deployment's `Scale` subresource counts.
#[derive(Debug, Clone, Copy)]
pub struct Scale {
    /// The DESIRED replica count (`.spec.replicas`) — what the waker actuates.
    pub spec_replicas: i32,
    /// The OBSERVED replica count (`.status.replicas`) — running pods.
    pub status_replicas: i32,
}

/// A minimal in-cluster client for the `apps/v1` Deployment `scale` subresource:
/// GET the current counts, PATCH a new desired count. Trusts ONLY the
/// service-account CA (`ca.crt`) and authenticates with the mounted bearer token.
pub struct KubeScale {
    http: reqwest::Client,
    base: String,
    namespace: String,
    token: String,
}

impl KubeScale {
    /// Build from the in-cluster service account: the bearer token, the CA
    /// bundle, and the namespace, all mounted under [`SA_DIR`].
    pub fn in_cluster() -> anyhow::Result<Self> {
        let dir = std::path::Path::new(SA_DIR);
        let token = std::fs::read_to_string(dir.join("token"))
            .context("read service-account token")?
            .trim()
            .to_string();
        let ca_pem = std::fs::read(dir.join("ca.crt")).context("read service-account CA")?;
        let namespace = std::fs::read_to_string(dir.join("namespace"))
            .context("read service-account namespace")?
            .trim()
            .to_string();
        Self::new(API_BASE, &namespace, &token, &ca_pem)
    }

    /// Build against an explicit API base + namespace + bearer token, trusting
    /// only the given CA bundle (PEM, one or more certs).
    pub fn new(base: &str, namespace: &str, token: &str, ca_pem: &[u8]) -> anyhow::Result<Self> {
        // Trust ONLY the cluster CA: `tls_certs_only` disables the native/built-in
        // roots and verifies the API server against exactly the certs in ca.crt.
        let certs = reqwest::Certificate::from_pem_bundle(ca_pem)
            .context("parse service-account CA bundle")?;
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_certs_only(certs)
            .build()
            .context("build Kubernetes HTTPS client")?;
        Ok(Self {
            http,
            base: base.to_string(),
            namespace: namespace.to_string(),
            token: token.to_string(),
        })
    }

    fn scale_url(&self, deployment: &str) -> String {
        format!(
            "{}/apis/apps/v1/namespaces/{}/deployments/{}/scale",
            self.base, self.namespace, deployment
        )
    }

    /// GET the Deployment's current `Scale` (desired + observed replicas).
    pub async fn get_scale(&self, deployment: &str) -> anyhow::Result<Scale> {
        let resp = self
            .http
            .get(self.scale_url(deployment))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET scale")?;
        let status = resp.status();
        let body = resp.text().await.context("read GET scale body")?;
        if !status.is_success() {
            bail!("GET scale {deployment}: {status}: {body}");
        }
        parse_scale(&body)
    }

    /// PATCH the Deployment's desired replica count (a merge-patch on `.spec`).
    pub async fn set_replicas(&self, deployment: &str, replicas: i32) -> anyhow::Result<()> {
        let body = serde_json::json!({ "spec": { "replicas": replicas } }).to_string();
        let resp = self
            .http
            .patch(self.scale_url(deployment))
            .bearer_auth(&self.token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/merge-patch+json",
            )
            .body(body)
            .send()
            .await
            .context("PATCH scale")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("PATCH scale {deployment}={replicas}: {status}: {body}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The service (arg parsing + doorbell subscribe + hint handling).
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct WakeArgs {
    /// Doorbell-tenant -> Deployment mappings (repeatable): `--wake demo-tenant=runner`.
    /// The waker subscribes to `wamn.doorbell.<tenant>` for each and scales the
    /// named Deployment `0 -> 1` on a hint that arrives while it is parked.
    #[arg(long = "wake", value_parser = parse_wake_mapping, required = true)]
    pub wake: Vec<WakeMapping>,

    /// NATS URL for doorbell hints — the SAME control-plane `nats` the dispatcher
    /// publishes to and the runner subscribes to (NOT evt-nats).
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// mTLS material for the doorbell NATS connection (mount the
    /// wasmcloud-runtime-tls secret in-cluster). Omit for plain NATS.
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,
}

/// Handle one doorbell hint: read the mapped Deployment's scale, decide, and (if
/// parked) scale it up. A read/patch error is logged, never fatal — the
/// dispatcher's next sweep re-hints, which is the retry path.
async fn handle_hint(scale: &KubeScale, mappings: &[WakeMapping], subject: &str) {
    let Some(tenant) = tenant_of_subject(subject) else {
        tracing::warn!(subject, "waker: hint on an unexpected subject; ignored");
        return;
    };
    // Only mapped tenants are subscribed, but re-checking keeps the GET off any
    // stray subject a wildcard/broker quirk could deliver.
    let Some(mapping) = mapping_for(tenant, mappings) else {
        return;
    };
    let current = match scale.get_scale(&mapping.deployment).await {
        Ok(s) => s.spec_replicas,
        Err(e) => {
            tracing::warn!(deployment = %mapping.deployment, error = %e,
                "waker: read scale failed; the dispatcher will re-hint");
            return;
        }
    };
    match decide(tenant, mappings, current) {
        WakeAction::None => {
            tracing::debug!(tenant, deployment = %mapping.deployment, current,
                "waker: runner already up; no-op");
        }
        WakeAction::Scale { deployment, to } => match scale.set_replicas(&deployment, to).await {
            Ok(()) => tracing::info!(tenant, %deployment, from = current, to,
                "waker: scaled parked runner up"),
            Err(e) => tracing::warn!(%deployment, error = %e,
                "waker: scale patch failed; the dispatcher will re-hint"),
        },
    }
}

pub async fn run(args: WakeArgs) -> anyhow::Result<()> {
    init_crypto();

    let mappings = args.wake.clone();
    let scale = KubeScale::in_cluster().context("build in-cluster Kubernetes scale client")?;

    // The doorbell is the waker's ONLY input — connect NATS or fail (the pod
    // restarts). Unlike the dispatcher/runner (which have a poll backstop), a
    // waker without NATS has nothing to react to.
    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = connect_nats(args.nats_url.clone(), nats_opts)
        .await
        .with_context(|| format!("connect NATS {}", args.nats_url))?;

    // One subscription per configured tenant's doorbell subject, merged into one
    // stream. Subscribing per configured tenant scopes the watch to mapped work.
    let mut subs = Vec::with_capacity(mappings.len());
    for m in &mappings {
        let subject = format!("{DOORBELL_PREFIX}{}", m.tenant);
        subs.push(
            nats.subscribe(subject.clone())
                .await
                .with_context(|| format!("subscribe {subject}"))?,
        );
        tracing::info!(tenant = %m.tenant, deployment = %m.deployment, subject,
            "waker: watching doorbell");
    }
    let mut hints = futures_util::stream::select_all(subs);

    // SIGTERM handled explicitly (PID 1 in-container gets no default disposition).
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "waker: no SIGTERM handler; Ctrl-C only");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = tx.send(true);
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        let _ = tx.send(true);
    });

    tracing::info!(
        mappings = mappings.len(),
        "wamn-waker up (doorbell -> scale 0->1; the dispatcher re-hint is the retry)"
    );

    loop {
        tokio::select! {
            hint = hints.next() => match hint {
                Some(msg) => handle_hint(&scale, &mappings, &msg.subject).await,
                None => {
                    tracing::warn!("waker: all doorbell subscriptions closed; exiting");
                    return Ok(());
                }
            },
            _ = rx.changed() => {
                if *rx.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// NATS glue — local copies of the fork's washlet helpers (SR9), exactly as the
// dispatcher carries them: this crate's only NATS use, no runtime linked.
// ---------------------------------------------------------------------------

/// TLS material for the doorbell connection. Local copy of the fork's
/// `wash_runtime::washlet::NatsConnectionOptions`.
struct NatsConnectionOptions {
    request_timeout: Option<Duration>,
    tls_ca: Option<PathBuf>,
    tls_first: bool,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
}

/// Local copy of the fork's `wash_runtime::washlet::connect_nats`.
async fn connect_nats(
    addr: impl async_nats::ToServerAddrs,
    options: NatsConnectionOptions,
) -> anyhow::Result<async_nats::Client> {
    let mut opts = async_nats::ConnectOptions::new();
    if let Some(timeout) = options.request_timeout {
        opts = opts.request_timeout(Some(timeout));
    }
    if let Some(ca_path) = options.tls_ca {
        opts = opts.add_root_certificates(ca_path)
    }
    if options.tls_first {
        opts = opts.tls_first();
    }
    if let (Some(cert_path), Some(key_path)) = (options.tls_cert, options.tls_key) {
        opts = opts.add_client_certificate(cert_path, key_path)
    }
    opts.connect(addr)
        .await
        .context("failed to connect to NATS")
}

/// Local copy of the fork's `wash_runtime::init_crypto`: standardize on
/// aws-lc-rs so the rustls provider is deterministic regardless of which backends
/// the dep graph enables.
fn init_crypto() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            tracing::warn!(
                "a rustls CryptoProvider was already installed; \
                 the waker standardizes on aws-lc-rs — check dependencies if this is unexpected"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wake_mapping_splits_tenant_and_deployment() {
        assert_eq!(
            parse_wake_mapping("demo-tenant=runner"),
            Ok(WakeMapping {
                tenant: "demo-tenant".into(),
                deployment: "runner".into(),
            })
        );
        // A deployment value may itself be any non-empty token.
        assert_eq!(
            parse_wake_mapping("t=dep").unwrap().deployment,
            "dep".to_string()
        );
        assert!(parse_wake_mapping("no-separator").is_err());
        assert!(parse_wake_mapping("=runner").is_err());
        assert!(parse_wake_mapping("demo-tenant=").is_err());
    }

    fn mappings() -> Vec<WakeMapping> {
        vec![WakeMapping {
            tenant: "demo-tenant".into(),
            deployment: "runner".into(),
        }]
    }

    #[test]
    fn decide_wakes_a_parked_mapped_deployment() {
        // A mapped tenant whose deployment sits at 0 replicas is woken to 1.
        assert_eq!(
            decide("demo-tenant", &mappings(), 0),
            WakeAction::Scale {
                deployment: "runner".into(),
                to: 1,
            }
        );
    }

    #[test]
    fn decide_skips_an_already_awake_deployment() {
        // Replicas > 0 => the runner is up; do NOT re-scale. This is the mutation
        // target: flip `current_replicas == 0` and this run-1/run-2 case fails.
        assert_eq!(decide("demo-tenant", &mappings(), 1), WakeAction::None);
        assert_eq!(decide("demo-tenant", &mappings(), 2), WakeAction::None);
    }

    #[test]
    fn decide_ignores_an_unmapped_tenant() {
        // An unmapped tenant is never woken, even parked at 0.
        assert_eq!(decide("other-tenant", &mappings(), 0), WakeAction::None);
    }

    #[test]
    fn tenant_of_subject_strips_the_doorbell_prefix() {
        assert_eq!(
            tenant_of_subject("wamn.doorbell.demo-tenant"),
            Some("demo-tenant")
        );
        assert_eq!(tenant_of_subject("wamn.other.demo-tenant"), None);
    }

    #[test]
    fn parse_scale_reads_spec_and_status_replicas() {
        let body = r#"{"kind":"Scale","spec":{"replicas":2},"status":{"replicas":1,"selector":"app=runner"}}"#;
        let s = parse_scale(body).expect("scale parses");
        assert_eq!((s.spec_replicas, s.status_replicas), (2, 1));
        // A freshly-parked deployment with no status replicas reads as 0.
        let parked = parse_scale(r#"{"spec":{"replicas":0},"status":{}}"#).unwrap();
        assert_eq!((parked.spec_replicas, parked.status_replicas), (0, 0));
    }
}

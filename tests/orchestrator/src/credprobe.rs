//! credprobe — the direct-import credential-grant gate (cjv.3).
//!
//! Prove the host enforces the frozen `wamn:node/credentials` per-execution
//! grant + fail-closed project at the REAL WIT boundary — the boundary the SDK
//! `CapsCtx` facade cannot vouch for, because a custom node (wamn-bd5) imports
//! `wamn:node/credentials` DIRECTLY and bypasses the facade entirely (C3-1).
//!
//! The gate instantiates the `cred-probe` threat fixture (which imports `get`
//! directly and can NOT grant itself) in several stores with distinct
//! component identities against ONE shared vault, registers a NARROW grant
//! host-side for one of them, and drives `probe(name)`:
//!
//! * a GRANTED name resolves (delivery);
//! * a name that EXISTS in the project but was NOT granted is `not-granted`
//!   (the exact C3-1 sibling-credential read, now closed host-side);
//! * an UNGRANTED unknown name is `not-granted` before any lookup;
//! * a component with NO registered project is `not-granted` even for a granted
//!   name (fail-closed identity, cjv.3 — never fail-open to a default project);
//! * a component with a project but NO grant is `not-granted` (fail-closed
//!   grant).
//!
//! Pure in-proc (no DB, no network): the enforcement is plugin logic, so the
//! gate is topology-independent — it runs identically locally and in-cluster.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, InstancePre, Linker};

use wamn_gate_harness::check;
use wamn_host::engine::build_engine;
use wamn_host::plugins::wamn_credentials::{self, WAMN_CREDENTIALS_ID, WamnCredentials};

/// The project the fixture components are routed to.
const PROJECT: &str = "cred-probe-project";
/// A distinctive secret so `ok:<secret>` is unambiguous.
const SECRET: &str = "cred-probe-secret-3f9a2e";
/// A sibling flow's secret in the SAME project — present in the vault but never
/// granted to the probe. The C3-1 read the host must now refuse.
const SIBLING_SECRET: &str = "sibling-secret-do-not-leak";

#[derive(Debug, Args)]
pub struct CredProbeArgs {
    /// Path to the cred-probe threat fixture component.
    #[arg(long, default_value = "/bench/cred-probe.wasm")]
    pub cred_probe: PathBuf,
}

/// A fixture instance bound to a component identity, ready to call `probe`.
struct Probe {
    store: Store<SharedCtx>,
    func: wash_runtime::wasmtime::component::TypedFunc<(String,), (String,)>,
}

impl Probe {
    async fn probe(&mut self, name: &str) -> anyhow::Result<String> {
        let (out,) = self
            .func
            .call_async(&mut self.store, (name.to_string(),))
            .await?;
        Ok(out)
    }
}

/// Compiled + linked fixture + the shared vault; mints `Probe`s per identity.
struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: InstancePre<SharedCtx>,
    vault: Arc<WamnCredentials>,
}

impl Harness {
    fn new(guest: &[u8], vault: Arc<WamnCredentials>) -> anyhow::Result<Self> {
        let engine = build_engine(&[])?;
        let raw = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile cred-probe: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // Link ONLY `get` — the fixture models an untrusted direct-import node,
        // so it deliberately does NOT get the trusted `set-granted` channel.
        wamn_credentials::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;
        Ok(Self { engine, pre, vault })
    }

    async fn probe_for(&self, component_id: &str) -> anyhow::Result<Probe> {
        let mut m: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        m.insert(
            WAMN_CREDENTIALS_ID,
            self.vault.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        let ctx = Ctx::builder(component_id.to_string(), component_id.to_string())
            .with_plugins(m)
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = self.pre.instantiate_async(&mut store).await?;
        let func = instance.get_typed_func::<(String,), (String,)>(&mut store, "probe")?;
        Ok(Probe { store, func })
    }
}

pub async fn run(args: CredProbeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.cred_probe)
        .with_context(|| format!("read cred-probe fixture {}", args.cred_probe.display()))?;

    // One project with a granted secret + a sibling secret it must never reach.
    let vault = Arc::new(WamnCredentials::from_projects(HashMap::from([(
        PROJECT.to_string(),
        HashMap::from([
            ("granted".to_string(), SECRET.to_string()),
            ("sibling".to_string(), SIBLING_SECRET.to_string()),
        ]),
    )])));

    // A properly registered node: project + a grant for exactly "granted".
    vault.set_project("granted-node", PROJECT)?;
    vault.set_granted_credentials("granted-node", ["granted".to_string()]);
    // Granted the name, but NO project registered — fail-closed identity.
    vault.set_granted_credentials("no-project-node", ["granted".to_string()]);
    // Project registered, but NO grant — fail-closed grant.
    vault.set_project("no-grant-node", PROJECT)?;

    println!("# wamn-gates credprobe — direct-import credential grant enforcement (cjv.3)");

    let harness = Harness::new(&guest, vault)?;
    let mut ok = true;

    // ---- the granted node ---------------------------------------------------
    let mut granted = harness.probe_for("granted-node").await?;
    check(
        &mut ok,
        "DELIVERY: a granted name resolves the secret",
        granted.probe("granted").await? == format!("ok:{SECRET}"),
    );
    check(
        &mut ok,
        "GRANT: a sibling credential in the project (not granted) is not-granted",
        granted.probe("sibling").await? == "err:not-granted",
    );
    check(
        &mut ok,
        "GRANT: an ungranted unknown name is not-granted (checked before existence)",
        granted.probe("absent").await? == "err:not-granted",
    );

    // ---- fail-closed identity ----------------------------------------------
    let mut no_project = harness.probe_for("no-project-node").await?;
    check(
        &mut ok,
        "FAIL-CLOSED PROJECT: no registered project is not-granted even for a granted name",
        no_project.probe("granted").await? == "err:not-granted",
    );

    // ---- fail-closed grant --------------------------------------------------
    let mut no_grant = harness.probe_for("no-grant-node").await?;
    check(
        &mut ok,
        "FAIL-CLOSED GRANT: a project with no granted set is not-granted",
        no_grant.probe("granted").await? == "err:not-granted",
    );

    // ---- an entirely unregistered component --------------------------------
    let mut unknown = harness.probe_for("unregistered-node").await?;
    check(
        &mut ok,
        "FAIL-CLOSED: an unregistered component grants nothing",
        unknown.probe("granted").await? == "err:not-granted",
    );

    println!("\ncredprobe complete — overall PASS: {ok}");
    if !ok {
        bail!("credprobe failed");
    }
    Ok(())
}

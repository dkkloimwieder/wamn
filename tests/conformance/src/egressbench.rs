//! 2.6 egress-review gate: assert that the shipped workload components expose no
//! raw-socket surface, so the `wamn:postgres` plugin (and the `allowed_hosts`-
//! gated, egress-spied `wasi:http` chokepoint from S6) are the only egress paths
//! a component can reach.
//!
//! STATIC IMPORT REVIEW (the default). This gate compiles each shipped component
//! and walks its import list; it never opens a socket or touches Postgres. That
//! result is a pure function of the wasm bytes, identical in-cluster and locally.
//!
//! RUNTIME RAW-SOCKET PHASE (`--sockprobe`, riders 1 + 8). The static review
//! shows the SHIPPED components carry no socket surface, but not that the runtime
//! *refuses* a component that DOES attempt raw egress. That is the fork's job now
//! (below), and this optional phase proves it: the sockprobe fixture attempts raw
//! TCP + UDP egress through the production host store path, and the gate asserts
//! the fork denies it by default and permits it only under the
//! `wamn.allow-raw-sockets` opt-in (see `assert_runtime_sockets`).
//!
//! HOST-LOOPBACK GRANT SURFACE (wamn-0h0g.15.52). `allowedHostLoopbackPorts` is
//! a second, independent door: it governs the fork's `host.wasmcloud.internal`
//! sentinel, which reaches the *machine's* real loopback rather than the guest's
//! virtual network. It is not raw egress — `shape_socket_policy` leaves it
//! untouched — so neither the raw-socket opt-in nor its absence decides it. The
//! runtime phase dials the sentinel in all three runs and asserts every dial is
//! refused, INCLUDING the run whose workload listed the port it dials: a
//! workload grant is inert unless the host itself enabled the door, and wamn's
//! host never does (nothing in this repository installs a host socket policy, so
//! the engine carries the fork's `host_loopback_enabled: false` default). The
//! unit gate isolates the two halves against the linked fork's own
//! `SocketPolicy::decide`, which is the only place they can be separated.
//!
//! WHAT IT PROVES. The "plugin is the only DB path" guarantee (docs 2.6) rests
//! on WIT-world composition: the runtime registers `wasi:sockets` on every
//! workload linker unconditionally (wash-runtime `engine/mod.rs`), and the fork's
//! socket policy (`linked_call.rs` `build_ctx_from_template`: `socket_addr_check`
//! / `socket_addr_permitted`, pins 8b76869 E13 / eef76cd E15/E16) now DENIES raw
//! `TcpConnect`/`UdpConnect`/`UdpOutgoingDatagram` unless the workload opts in via
//! `wamn.allow-raw-sockets`, consulting `allowed_hosts` for `wasi:http` only. So a
//! shipped component's world still must not *import* `wasi:sockets` at all — this
//! gate asserts it does not, that the DB-touching runner imports `wamn:postgres`,
//! and (with `--sockprobe`) that the runtime deny actually fires independently
//! for every P2 arm. The unit gate additionally pins the same shared decision
//! and every P3 mirror call site on the exact linked fork revision. See
//! docs/archive/data-path/security-db-path.md.
//!
//! The verdict comes from `wamn_component_policy` — the same classifier the
//! host publish-gate uses — not a forked local rule. The retained scope is the
//! first-party flow-runner: it legitimately imports `wamn:postgres` (the DB
//! path), is screened by the socket denylist
//! (`egress_guard::denied_imports`, E13a), and must import the plugin.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, UdpSocket as StdUdpSocket};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::host::allowed_loopback::AllowedLoopbackPort;
use wash_runtime::host::{HostApi, HostBuilder};
use wash_runtime::sockets::internal_names::HOST_SENTINEL;
use wash_runtime::types::{
    HostPathVolume, LocalResources, Service, Volume, VolumeMount, VolumeType, Workload,
    WorkloadStartRequest,
};
use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

use wamn_component_policy::denied_imports;
use wamn_runtime::engine::build_engine;
#[cfg(test)]
use wamn_runtime::engine::build_engine_with_socket_policy;

#[derive(Args)]
pub struct EgressBenchArgs {
    /// The standard flow-runner — the first-party DB-touching shipped workload.
    /// Must import `wamn:postgres` (the DB path) and must NOT import `wasi:sockets`.
    #[arg(long)]
    flowrunner: PathBuf,

    /// The sockprobe fixture (`components/fixtures/sockprobe`). When set, the
    /// RUNTIME raw-socket phase runs: sockprobe is instantiated as a service
    /// through the production host store path — where the fork's `linked_call`
    /// `socket_addr_check` governs raw TCP/UDP egress — with the raw-socket
    /// opt-in OFF (deny-by-default, E13/E15 negative), then ON (opted-in
    /// positive), then a third run carrying a non-empty
    /// `allowedHostLoopbackPorts` (the grant surface, wamn-0h0g.15.52).
    /// Optional: the static import review runs without it.
    #[arg(long)]
    sockprobe: Option<PathBuf>,
}

/// Other host-plugin egress `namespace:package`s not expected in the
/// first-party flow-runner; if one appears it is flagged as a new egress path
/// to justify.
const OTHER_EGRESS_PKGS: &[&str] = &[
    "wasi:blobstore",
    "wasi:keyvalue",
    "wasi:messaging",
    "wamn:messaging",
];

/// The host-loopback port the runtime phase's third run names in
/// `allowedHostLoopbackPorts`, and dials through the sentinel.
///
/// 5432 deliberately: if this door ever opened it would open onto the local
/// Postgres this whole gate exists to keep behind the `wamn:postgres` plugin, so
/// a regression reports `connected` rather than something academic.
const HOST_LOOPBACK_LISTED_PORT: u16 = 5432;

/// A host-loopback port NO run ever lists. Dialed alongside the listed one so
/// the report distinguishes "this port was refused" from "the arm never ran".
const HOST_LOOPBACK_UNLISTED_PORT: u16 = 6379;

/// The `namespace:package` of an instance import. Imports look like
/// `wasi:sockets/tcp@0.2.3`; we key policy on the `ns:pkg` prefix.
fn ns_pkg(import_name: &str) -> &str {
    import_name.split('/').next().unwrap_or(import_name)
}

/// Compile `path` and return its full import NAMES (e.g. `wasi:sockets/tcp@0.2.3`)
/// — names, not just `ns:pkg`, so the shared `egress_guard` classifiers can name
/// the exact offending interface.
fn import_names(engine: &RawEngine, path: &Path) -> anyhow::Result<Vec<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let component = WasmtimeComponent::new(engine, &bytes)
        .map_err(|e| anyhow::anyhow!("compile {}: {e}", path.display()))?;
    let ty = component.component_type();
    let eng = component.engine();
    Ok(ty
        .imports(eng)
        .map(|(name, _item)| name.to_string())
        .collect())
}

/// The distinct `namespace:package`s of `names`, for the human-readable summary.
fn ns_pkgs(names: &[String]) -> BTreeSet<&str> {
    names.iter().map(|n| ns_pkg(n)).collect()
}

/// First-party flow-runner profile: it legitimately imports `wamn:postgres` (the
/// DB path) and the `allowed_hosts`-gated `wasi:http` chokepoint, and must NOT
/// import raw sockets. The raw-socket verdict is the shared E13a guard
/// (`egress_guard::denied_imports`) — not a forked local rule. Returns whether
/// it passed.
fn assert_flowrunner(label: &str, names: &[String]) -> bool {
    let pkgs = ns_pkgs(names);
    let denied = denied_imports(names.iter().map(String::as_str));
    let other: Vec<&str> = pkgs
        .iter()
        .copied()
        .filter(|p| OTHER_EGRESS_PKGS.contains(p))
        .collect();

    println!("  {label}");
    println!("    packages: {pkgs:?}");

    let mut ok = true;
    if !denied.is_empty() {
        println!(
            "    FAIL: imports raw-socket interface(s) {denied:?} — can reach Postgres directly, \
             bypassing the plugin"
        );
        ok = false;
    }
    if !other.is_empty() {
        println!(
            "    FAIL: imports unexpected egress interface(s) {other:?} — new egress path, must be \
             justified / allowlisted"
        );
        ok = false;
    }
    if !names.iter().any(|n| ns_pkg(n) == "wamn:postgres") {
        println!(
            "    FAIL: does not import wamn:postgres — expected the DB-touching workload to use the \
             plugin"
        );
        ok = false;
    }
    if ok {
        println!("    PASS: no raw-socket surface; wamn:postgres is the DB path");
    }
    ok
}

/// A sockprobe per-arm verdict that means the raw-egress op was PERMITTED:
/// the policy let the socket op proceed (it then either connected or failed for
/// an unrelated reason). Anything else — `denied`, `bind-failed`, or a missing
/// report — is NOT permitted. Keyed on sockprobe's stable tokens, not error
/// text, so the positive assertion never guesses the exact non-deny error.
fn sock_permitted(verdict: &str) -> bool {
    matches!(verdict, "connected" | "allowed-failed" | "sent")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SocketVerdicts {
    tcp_connect: String,
    udp_connect: String,
    udp_outgoing_datagram: String,
    udp_bind_loopback: String,
    udp_bind_non_loopback: String,
    host_loopback_listed: String,
    host_loopback_unlisted: String,
}

/// Parse sockprobe's seven arm-specific verdicts.
fn read_verdicts(report_dir: &Path) -> Option<SocketVerdicts> {
    let contents = std::fs::read_to_string(report_dir.join("outcome")).ok()?;
    let mut tcp_connect = None;
    let mut udp_connect = None;
    let mut udp_outgoing_datagram = None;
    let mut udp_bind_loopback = None;
    let mut udp_bind_non_loopback = None;
    let mut host_loopback_listed = None;
    let mut host_loopback_unlisted = None;
    for line in contents.lines() {
        if let Some(v) = line.strip_prefix("tcp-connect=") {
            tcp_connect = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("udp-connect=") {
            udp_connect = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("udp-outgoing-datagram=") {
            udp_outgoing_datagram = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("udp-bind-loopback=") {
            udp_bind_loopback = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("udp-bind-non-loopback=") {
            udp_bind_non_loopback = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("host-loopback-listed=") {
            host_loopback_listed = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("host-loopback-unlisted=") {
            host_loopback_unlisted = Some(v.trim().to_string());
        }
    }
    Some(SocketVerdicts {
        tcp_connect: tcp_connect?,
        udp_connect: udp_connect?,
        udp_outgoing_datagram: udp_outgoing_datagram?,
        udp_bind_loopback: udp_bind_loopback?,
        udp_bind_non_loopback: udp_bind_non_loopback?,
        host_loopback_listed: host_loopback_listed?,
        host_loopback_unlisted: host_loopback_unlisted?,
    })
}

fn runtime_verdicts_pass(deny: &SocketVerdicts, optin: &SocketVerdicts) -> bool {
    deny.tcp_connect == "denied"
        && deny.udp_connect == "denied"
        && deny.udp_outgoing_datagram == "denied"
        && deny.udp_bind_loopback == "bound"
        && deny.udp_bind_non_loopback == "denied"
        && sock_permitted(&optin.tcp_connect)
        && sock_permitted(&optin.udp_connect)
        && sock_permitted(&optin.udp_outgoing_datagram)
        && optin.udp_bind_loopback == "bound"
        && optin.udp_bind_non_loopback == "denied"
}

/// The host-loopback door, across all three runs.
///
/// Every sentinel dial must be refused — including `host_loopback_listed` in the
/// run whose workload DID list that port, because a workload-level
/// `allowedHostLoopbackPorts` grant is inert unless the host itself enabled the
/// door. That is the property the third run exists to show, and it is the only
/// half of the two-part grant this phase can isolate: the fork answers
/// `HostLoopbackNotPermitted` for either missing half, so separating them needs
/// a host that enabled the door, which nothing in this repository configures
/// (see `host_loopback_needs_the_host_enable_and_the_listed_port_together`).
///
/// `denied` is required verbatim rather than "not permitted", so sockprobe's
/// `no-target` (the gate named no sentinel) can never pass as a refusal.
fn host_loopback_verdicts_pass(
    deny: &SocketVerdicts,
    optin: &SocketVerdicts,
    loopback: &SocketVerdicts,
) -> bool {
    [deny, optin, loopback].into_iter().all(|verdicts| {
        verdicts.host_loopback_listed == "denied" && verdicts.host_loopback_unlisted == "denied"
    })
}

/// Resolve an address on this host's non-loopback interface. Port 9 is
/// intentionally expected to be closed: opted-in TCP therefore returns quickly
/// with connection-refused, while the policy check still sees a real
/// non-loopback address before the syscall.
fn non_loopback_target() -> anyhow::Result<SocketAddr> {
    let socket = StdUdpSocket::bind("0.0.0.0:0")?;
    socket.connect("192.0.2.1:9")?;
    let ip = socket.local_addr()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        bail!("could not resolve a non-loopback address for sockprobe: {ip}");
    }
    Ok(SocketAddr::new(ip, 9))
}

/// The `host.wasmcloud.internal` address for `port`, built from the LINKED
/// fork's own `HOST_SENTINEL`.
///
/// sockprobe is a wasm guest with no access to the fork's constants, so the gate
/// resolves the sentinel here and passes the whole address through the
/// environment. Reading the constant rather than spelling `127.255.255.254`
/// means the fixture cannot drift onto an address the policy no longer gates —
/// and `the_sentinel_target_handed_to_sockprobe_reaches_the_gated_branch` proves
/// what this returns still lands on the host-loopback branch.
fn sentinel_target(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(HOST_SENTINEL), port)
}

/// Start sockprobe as a SERVICE (so `is_service` is true and its loopback UDP
/// bind is permitted — the raw-egress connect is the gated op) with a mounted
/// host-path report volume, optionally opting into raw sockets and optionally
/// granting host-loopback ports.
async fn run_sockprobe(
    host: &Arc<wash_runtime::host::Host>,
    bytes: &[u8],
    id: &str,
    allow_raw_sockets: bool,
    host_loopback: &[AllowedLoopbackPort],
    report_dir: &Path,
) -> anyhow::Result<()> {
    let mut resources = LocalResources {
        memory_limit_mb: 0,
        cpu_limit: 0,
        config: Default::default(),
        environment: Default::default(),
        volume_mounts: vec![VolumeMount {
            name: "report".to_string(),
            mount_path: "/report".to_string(),
            read_only: false,
        }],
        allowed_hosts: Arc::from(vec![]),
        allowed_ip_name_lookups: Default::default(),
        // [wamn-0h0g.15.20 / .15.52] New at the wamn/2.7.0 pin. EMPTY = deny
        // every host-loopback connection. The third run supplies a NON-empty
        // list naming the port it dials, which must change nothing: the grant is
        // inert unless the host was started with `--allow-host-loopback`, and
        // nothing here installs a host socket policy that could enable it.
        allowed_host_loopback_ports: Arc::from(host_loopback),
    };
    resources.environment.insert(
        "SOCKPROBE_REPORT_PATH".to_string(),
        "/report/outcome".to_string(),
    );
    resources.environment.insert(
        "SOCKPROBE_NON_LOOPBACK_TARGET".to_string(),
        non_loopback_target()?.to_string(),
    );
    resources.environment.insert(
        "SOCKPROBE_HOST_LOOPBACK_LISTED".to_string(),
        sentinel_target(HOST_LOOPBACK_LISTED_PORT).to_string(),
    );
    resources.environment.insert(
        "SOCKPROBE_HOST_LOOPBACK_UNLISTED".to_string(),
        sentinel_target(HOST_LOOPBACK_UNLISTED_PORT).to_string(),
    );
    if allow_raw_sockets {
        // The fork reads this per-component config in build_ctx_from_template
        // (docs/archive/platform/wash-runtime-fork.md); it is the ONLY opt-in that flips the
        // raw-egress verdict from deny to allow.
        resources
            .config
            .insert("wamn.allow-raw-sockets".to_string(), "true".to_string());
    }
    host.workload_start(WorkloadStartRequest {
        workload_id: id.to_string(),
        workload: Workload {
            namespace: "egress".to_string(),
            name: id.to_string(),
            annotations: Default::default(),
            service: Some(Service {
                bytes: bytes.to_vec().into(),
                digest: Some(format!("egress-{id}")),
                local_resources: resources,
                max_restarts: 0,
            }),
            components: vec![],
            host_interfaces: vec![],
            volumes: vec![Volume {
                name: "report".to_string(),
                volume_type: VolumeType::HostPath(HostPathVolume {
                    local_path: report_dir.to_string_lossy().into_owned(),
                }),
            }],
        },
    })
    .await
    .with_context(|| format!("failed to start sockprobe service {id}"))?;
    Ok(())
}

/// RUNTIME raw-socket phase (riders 1 + 8): the sockprobe fixture attempts raw
/// TCP + UDP egress through the production host store path, so the fork's
/// `socket_addr_check` (pins 8b76869 E13 / eef76cd E15/E16) is the policy under
/// test — not a re-implementation. Deny-by-default (no opt-in) must refuse both
/// protocols; the opt-in (`wamn.allow-raw-sockets=true`) must permit both. The
/// verdict comes from sockprobe's report file (`denied` vs NOT-`denied`), so the
/// assertion is text-independent.
///
/// A THIRD run (wamn-0h0g.15.52) carries a non-empty `allowedHostLoopbackPorts`
/// naming the port its sentinel arm dials. Its raw-egress verdicts are not
/// asserted — the grant governs a different door — but every run's host-loopback
/// arms must be refused, so the listed port shows the workload half of the grant
/// is inert on its own. Returns whether the phase passed.
async fn assert_runtime_sockets(sockprobe: &[u8]) -> anyhow::Result<bool> {
    println!("\n## runtime raw-socket policy (E13 TCP / E15 UDP) — sockprobe as a service");
    let engine = build_engine(&[])?;
    let host = HostBuilder::new().with_engine(engine).build()?;
    let host = host.start().await?;

    let base = std::env::temp_dir().join(format!("wamn-egress-sock-{}", std::process::id()));
    let deny_dir = base.join("deny");
    let optin_dir = base.join("optin");
    let loopback_dir = base.join("loopback");
    std::fs::create_dir_all(&deny_dir)?;
    std::fs::create_dir_all(&optin_dir)?;
    std::fs::create_dir_all(&loopback_dir)?;

    let listed = [AllowedLoopbackPort::tcp(HOST_LOOPBACK_LISTED_PORT)];
    run_sockprobe(&host, sockprobe, "sock-deny", false, &[], &deny_dir).await?;
    run_sockprobe(&host, sockprobe, "sock-optin", true, &[], &optin_dir).await?;
    run_sockprobe(
        &host,
        sockprobe,
        "sock-loopback",
        false,
        &listed,
        &loopback_dir,
    )
    .await?;

    // sockprobe writes its verdicts and exits within milliseconds; give all
    // three services time to run before reading the reports.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let deny = read_verdicts(&deny_dir);
    let optin = read_verdicts(&optin_dir);
    let loopback = read_verdicts(&loopback_dir);
    let _ = std::fs::remove_dir_all(&base);

    println!("  deny-by-default (no wamn.allow-raw-sockets): {deny:?}");
    println!("  opted-in        (wamn.allow-raw-sockets=true): {optin:?}");
    println!(
        "  host-loopback   (allowedHostLoopbackPorts=[{HOST_LOOPBACK_LISTED_PORT}]): {loopback:?}"
    );

    let pass = matches!(
        (&deny, &optin, &loopback),
        (Some(deny), Some(optin), Some(loopback))
            if runtime_verdicts_pass(deny, optin)
                && host_loopback_verdicts_pass(deny, optin, loopback)
    );
    if pass {
        println!(
            "    PASS: TcpConnect, UdpConnect, and UdpOutgoingDatagram deny by default and \
             permit only on opt-in; UdpBind permits loopback but denies a concrete \
             non-loopback address; every \
             host.wasmcloud.internal dial is refused, including the port the workload listed"
        );
    } else {
        println!(
            "    FAIL: expected each raw-egress arm denied/default + permitted/opt-in, a \
             concrete non-loopback UdpBind denied, and every host-loopback arm denied in all three runs; \
             deny={deny:?}, optin={optin:?}, loopback={loopback:?}"
        );
    }
    Ok(pass)
}

pub async fn run(args: EgressBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-gates 2.6 egressbench — DB-path egress review");
    println!("# claim: the wamn:postgres plugin is the only DB path. The first-party");
    println!("#        flow-runner imports it and no raw sockets.");
    println!("#        With --sockprobe, the runtime raw-socket policy (E13/E15) is also");
    println!("#        exercised: raw TCP/UDP egress denied by default, allowed only on opt-in,");
    println!("#        and every host.wasmcloud.internal dial refused — a workload's");
    println!("#        allowedHostLoopbackPorts grant is inert without the host's own enable.");

    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let mut pass = true;

    println!("\n## first-party flow-runner (DB path)");
    let fr = import_names(raw, &args.flowrunner)?;
    pass &= assert_flowrunner(&format!("flow-runner  {}", args.flowrunner.display()), &fr);

    if let Some(sockprobe) = &args.sockprobe {
        let bytes = std::fs::read(sockprobe)
            .with_context(|| format!("read sockprobe {}", sockprobe.display()))?;
        pass &= assert_runtime_sockets(&bytes).await?;
    }

    println!("\negressbench complete — overall PASS: {pass}");
    if !pass {
        bail!("2.6 egress gate failed: a shipped workload exposes a raw-socket egress path");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::Ipv4Addr;

    use wash_runtime::sockets::internal_names::is_host_sentinel;
    use wash_runtime::sockets::policy::{EgressMode, GuestKind, SocketPolicy};
    use wash_runtime::sockets::{AddrDecision, DenyReason, Plane, SocketAddrUse};

    fn canonical_verdicts() -> (SocketVerdicts, SocketVerdicts) {
        (
            SocketVerdicts {
                tcp_connect: "denied".to_string(),
                udp_connect: "denied".to_string(),
                udp_outgoing_datagram: "denied".to_string(),
                udp_bind_loopback: "bound".to_string(),
                udp_bind_non_loopback: "denied".to_string(),
                host_loopback_listed: "denied".to_string(),
                host_loopback_unlisted: "denied".to_string(),
            },
            SocketVerdicts {
                tcp_connect: "allowed-failed".to_string(),
                udp_connect: "connected".to_string(),
                udp_outgoing_datagram: "sent".to_string(),
                udp_bind_loopback: "bound".to_string(),
                udp_bind_non_loopback: "denied".to_string(),
                host_loopback_listed: "denied".to_string(),
                host_loopback_unlisted: "denied".to_string(),
            },
        )
    }

    /// The third run's canonical report is the deny-by-default shape exactly:
    /// listing a host-loopback port must change nothing the report can see.
    fn canonical_loopback_verdicts() -> SocketVerdicts {
        canonical_verdicts().0
    }

    /// A guest policy shaped only around the host-loopback door.
    ///
    /// `SocketPolicy::for_kind` supplies the fork's own defaults (permissive
    /// address ranges, no quota, no host-owned port table), so nothing but the
    /// two halves under test can decide a sentinel dial.
    fn loopback_policy(
        host_enabled: bool,
        listed: &[AllowedLoopbackPort],
        mode: EgressMode,
    ) -> SocketPolicy {
        SocketPolicy {
            host_loopback_enabled: host_enabled,
            host_loopback: Arc::from(listed),
            egress_mode: mode,
            ..SocketPolicy::for_kind(GuestKind::Component)
        }
    }

    /// The test-only socket-policy seam must feed the same builder that carries
    /// the production engine's pooling, epoch, and proposal configuration.
    #[test]
    fn explicit_socket_policy_reaches_the_production_engine_builder() {
        let socket_policy = SocketPolicy {
            host_loopback_enabled: true,
            egress_mode: EgressMode::Enforce,
            ..SocketPolicy::default()
        };
        assert!(socket_policy.host_loopback_enabled);
        assert!(matches!(socket_policy.egress_mode, EgressMode::Enforce));

        build_engine_with_socket_policy(&[], socket_policy)
            .expect("the production engine accepts the explicit socket policy");

        let engine_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/platform/runtime/src/engine.rs"
        ));
        assert_eq!(
            engine_source.matches("Engine::builder()").count(),
            1,
            "the explicit policy must not create a second engine-builder path"
        );
        assert_eq!(
            engine_source
                .matches(".with_socket_policy(Arc::new(socket_policy))")
                .count(),
            1,
            "the explicit policy must reach the canonical production EngineBuilder"
        );
    }

    fn deny_reason(decision: &AddrDecision) -> Option<DenyReason> {
        match decision {
            AddrDecision::Deny(reason) => Some(*reason),
            AddrDecision::Allow(_) => None,
        }
    }

    /// The address and plane a permitted decision resolved to — the evidence a
    /// bare "it was allowed" cannot give.
    fn permitted(decision: &AddrDecision) -> Option<(SocketAddr, Plane)> {
        match decision {
            AddrDecision::Allow(a) => Some((a.addr, a.plane)),
            AddrDecision::Deny(_) => None,
        }
    }

    fn wash_runtime_source() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("conformance package must live at tests/conformance");
        let output = Command::new(env!("CARGO"))
            .current_dir(root)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()
            .expect("run cargo metadata for linked wash-runtime source");
        assert!(
            output.status.success(),
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("cargo metadata must be JSON");
        let package = metadata["packages"]
            .as_array()
            .expect("metadata packages must be an array")
            .iter()
            .find(|package| package["name"] == "wash-runtime")
            .expect("linked graph must contain wash-runtime");
        assert_eq!(
            package["version"], "2.7.0",
            "socket gate must inspect wash-runtime 2.7.0"
        );
        let source = package["source"]
            .as_str()
            .expect("wash-runtime must retain its git source");
        assert!(
            source.ends_with("#daba602901507338e99f277e07a8e923c61dc557"),
            "socket gate must inspect the exact linked wash-runtime 2.7.0 fork revision, got {source}"
        );
        Path::new(
            package["manifest_path"]
                .as_str()
                .expect("wash-runtime manifest path must be text"),
        )
        .parent()
        .expect("wash-runtime manifest must have a parent")
        .to_path_buf()
    }

    fn fork_source(path: &str) -> String {
        let path = wash_runtime_source().join(path);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read linked fork source {}: {error}", path.display()))
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ns_pkg_keeps_namespace_package() {
        assert_eq!(ns_pkg("wasi:sockets/tcp@0.2.3"), "wasi:sockets");
        assert_eq!(ns_pkg("wamn:postgres/client@0.1.0"), "wamn:postgres");
        assert_eq!(ns_pkg("wasi:http/outgoing-handler@0.2.3"), "wasi:http");
        // Degenerate: no interface segment.
        assert_eq!(ns_pkg("wasi:clocks"), "wasi:clocks");
    }

    #[test]
    fn flowrunner_shape_passes() {
        // flow-runner: DB plugin + chokepointed http, no raw sockets.
        let n = names(&[
            "wamn:postgres/client@0.1.0",
            "wasi:http/outgoing-handler@0.2.3",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:io/streams@0.2.3",
        ]);
        assert!(assert_flowrunner("runner", &n));
    }

    #[test]
    fn flowrunner_importing_sockets_fails() {
        // The boundary 2.6/E13a defends: a wasi:sockets import is a DB-path bypass.
        let n = names(&["wamn:postgres/client@0.1.0", "wasi:sockets/tcp@0.2.3"]);
        assert!(!assert_flowrunner("socket-runner", &n));
    }

    #[test]
    fn flowrunner_without_postgres_fails() {
        // The DB-touching workload must actually use the plugin.
        let n = names(&["wasi:cli/run@0.2.3"]);
        assert!(!assert_flowrunner("no-db", &n));
    }

    /// The runtime-phase verdict classifier: only sockprobe's two "the op was
    /// permitted" tokens count as permitted. `denied` (policy refusal) and
    /// `bind-failed` (a harness misconfiguration, not a policy pass) do not — so
    /// a stuck/failed run can never masquerade as the opted-in positive.
    #[test]
    fn sock_permitted_only_accepts_permitted_tokens() {
        assert!(sock_permitted("connected"));
        assert!(sock_permitted("allowed-failed"));
        assert!(sock_permitted("sent"));
        assert!(!sock_permitted("denied"));
        assert!(!sock_permitted("bind-failed"));
        assert!(!sock_permitted(""));
    }

    /// The report parser requires every independently asserted arm; a missing
    /// line yields None (treated as a phase failure).
    #[test]
    fn read_verdicts_requires_every_socket_arm() {
        let dir = std::env::temp_dir().join(format!("wamn-egress-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("outcome"),
            "tcp-connect=denied\nudp-connect=connected\nudp-outgoing-datagram=sent\n\
             udp-bind-loopback=bound\nudp-bind-non-loopback=denied\n\
             host-loopback-listed=denied\nhost-loopback-unlisted=denied\n",
        )
        .unwrap();
        assert_eq!(
            read_verdicts(&dir),
            Some(SocketVerdicts {
                tcp_connect: "denied".to_string(),
                udp_connect: "connected".to_string(),
                udp_outgoing_datagram: "sent".to_string(),
                udp_bind_loopback: "bound".to_string(),
                udp_bind_non_loopback: "denied".to_string(),
                host_loopback_listed: "denied".to_string(),
                host_loopback_unlisted: "denied".to_string(),
            })
        );
        std::fs::write(dir.join("outcome"), "tcp-connect=denied\n").unwrap();
        assert_eq!(read_verdicts(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tcp_connect_arm_rejects_unconditional_allow_mutation() {
        let (deny, optin) = canonical_verdicts();
        assert!(runtime_verdicts_pass(&deny, &optin));
        let mutant = SocketVerdicts {
            tcp_connect: "connected".to_string(),
            ..deny
        };
        assert!(
            !runtime_verdicts_pass(&mutant, &optin),
            "TcpConnect unconditional-allow mutation survived"
        );
    }

    #[test]
    fn udp_connect_arm_rejects_unconditional_allow_mutation() {
        let (deny, optin) = canonical_verdicts();
        assert!(runtime_verdicts_pass(&deny, &optin));
        let mutant = SocketVerdicts {
            udp_connect: "connected".to_string(),
            ..deny
        };
        assert!(
            !runtime_verdicts_pass(&mutant, &optin),
            "UdpConnect unconditional-allow mutation survived"
        );
    }

    #[test]
    fn udp_outgoing_datagram_arm_rejects_unconditional_allow_mutation() {
        let (deny, optin) = canonical_verdicts();
        assert!(runtime_verdicts_pass(&deny, &optin));
        let mutant = SocketVerdicts {
            udp_outgoing_datagram: "sent".to_string(),
            ..deny
        };
        assert!(
            !runtime_verdicts_pass(&mutant, &optin),
            "UdpOutgoingDatagram unconditional-allow mutation survived"
        );
    }

    #[test]
    fn udp_bind_arm_rejects_concrete_non_loopback_address() {
        let (deny, optin) = canonical_verdicts();
        assert!(runtime_verdicts_pass(&deny, &optin));
        let mutant = SocketVerdicts {
            udp_bind_non_loopback: "bound".to_string(),
            ..deny
        };
        assert!(
            !runtime_verdicts_pass(&mutant, &optin),
            "UdpBind concrete-non-loopback widening mutation survived"
        );
    }

    /// The host-loopback classifier, against the three mutations that would make
    /// the new arms vacuous.
    #[test]
    fn host_loopback_arm_rejects_a_workload_only_grant_and_an_unrun_arm() {
        let (deny, optin) = canonical_verdicts();
        let loopback = canonical_loopback_verdicts();
        assert!(host_loopback_verdicts_pass(&deny, &optin, &loopback));

        // A policy that honoured the workload's listed port without the host's
        // own enable — the inertness the third run exists to assert.
        let mutant = SocketVerdicts {
            host_loopback_listed: "connected".to_string(),
            ..loopback.clone()
        };
        assert!(
            !host_loopback_verdicts_pass(&deny, &optin, &mutant),
            "a workload-only host-loopback grant survived"
        );

        // `wamn.allow-raw-sockets` governs raw egress, not the machine's own
        // loopback; it must not become a second door onto it.
        let mutant = SocketVerdicts {
            host_loopback_unlisted: "allowed-failed".to_string(),
            ..optin.clone()
        };
        assert!(
            !host_loopback_verdicts_pass(&deny, &mutant, &loopback),
            "wamn.allow-raw-sockets opened the host-loopback door"
        );

        // `no-target` means the gate never named a sentinel, so the arm never
        // reached the policy. It must not read as a refusal.
        let mutant = SocketVerdicts {
            host_loopback_listed: "no-target".to_string(),
            ..deny.clone()
        };
        assert!(
            !host_loopback_verdicts_pass(&mutant, &optin, &loopback),
            "an unconfigured sentinel arm passed as a refusal"
        );
    }

    /// Opting out of raw sockets must produce an EMPTY allowlist under
    /// `Enforce`, and both halves must be pinned together.
    ///
    /// Re-pinned at the `wamn/2.7.0` fork bump (wamn-0h0g.15.20). Upstream
    /// `82e06949` moved every socket decision into one place, so the fork no
    /// longer answers a per-operation boolean (`SocketAddrUse::TcpConnect =>
    /// allow_raw_sockets`, the shape these guards used to pin) — it SHAPES the
    /// guest's policy instead. `EgressMode::Count` is upstream's `#[default]`
    /// and merely *logs and counts* what it would refuse while allowing it, so
    /// an empty allowlist ALONE denies nothing. Losing either half reopens raw
    /// egress silently, which is precisely the plugin-tier hole `wamn-g2br.15`
    /// found and closed.
    #[test]
    fn raw_socket_opt_out_shapes_an_empty_allowlist_under_enforce() {
        let shaper = fork_source("src/engine/linked_call.rs");
        assert!(
            shaper.contains("pub(crate) fn shape_socket_policy("),
            "the raw-socket shaping entry point must stay crate-visible so the plugin tier \
             reuses the same gate as workload stores (wamn-g2br.15)"
        );
        assert!(
            shaper.contains("    if allow_raw_sockets {\n        return policy;\n    }\n"),
            "an opted-in guest must get its policy back untouched"
        );
        assert!(
            shaper.contains(concat!(
                "        allowed_hosts: Arc::from([]),\n",
                "        egress_mode: sockets::policy::EgressMode::Enforce,"
            )),
            "opting out must empty the allowlist AND force Enforce — the empty list alone is \
             inert under upstream's Count default"
        );
        assert!(
            shaper.contains(
                "SocketAddrUse::TcpConnect | SocketAddrUse::UdpConnect | SocketAddrUse::UdpOutgoingDatagram"
            ),
            "the three raw-egress operations must stay named together for the warn-once event"
        );

        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy.contains("EgressMode::Enforce => Err(reason),"),
            "Enforce must actually refuse what the policy refuses"
        );
        assert!(
            policy.contains("if !check_allowed_addr(&self.allowed_hosts, addr) {"),
            "real egress must consult the declared allowlist that shaping emptied"
        );
        assert!(
            policy.contains("if addr.ip().to_canonical().is_loopback() {"),
            "the guest's own virtual network must stay outside the egress policy — that is why \
             this denial is scoped to NON-loopback destinations"
        );
    }

    #[test]
    fn tcp_connect_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy
                .contains("SocketAddrUse::TcpConnect => self.decide_connect(addr, Protocol::Tcp),"),
            "TcpConnect must route to the connect decision, which the shaped policy denies for \
             non-loopback without opt-in"
        );
        let p2 = fork_source("src/sockets/host_tcp.rs");
        assert!(
            p2.contains(".check_socket_addr(remote_address, SocketAddrUse::TcpConnect)"),
            "P2 TcpConnect must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_tcp_p3.rs");
        assert!(
            p3.contains("let allowed = check(remote_address, SocketAddrUse::TcpConnect)")
                && p3.contains(".into_allowed()"),
            "P3 TcpConnect mirror must consult the shared socket policy AND consume the decision \
             fallibly, not discard it"
        );
    }

    #[test]
    fn udp_connect_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy
                .contains("SocketAddrUse::UdpConnect => self.decide_connect(addr, Protocol::Udp),"),
            "UdpConnect must route to the connect decision, which the shaped policy denies for \
             non-loopback without opt-in"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(connect_addr, SocketAddrUse::UdpConnect)"),
            "P2 UdpConnect must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains(
                "let allowed = (self.ctx.socket_addr_check)(remote_address, SocketAddrUse::UdpConnect)"
            ) && p3.contains(".into_allowed()"),
            "P3 UdpConnect mirror must consult the shared socket policy AND consume the decision \
             fallibly"
        );
    }

    #[test]
    fn udp_outgoing_datagram_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy.contains(
                "SocketAddrUse::UdpOutgoingDatagram => match self.resolve_connect(addr, Protocol::Udp) {"
            ),
            "an outgoing datagram must run the same connect resolution as a connect — spending no \
             quota slot, but taking the same denial"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(addr, SocketAddrUse::UdpOutgoingDatagram)"),
            "P2 UdpOutgoingDatagram must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains("let allowed = check(remote_address, SocketAddrUse::UdpOutgoingDatagram)")
                && p3.contains(".into_allowed()"),
            "P3 UdpOutgoingDatagram mirror must consult the shared socket policy AND consume the \
             decision fallibly"
        );
    }

    /// `UdpBind` authority as it stands at `wamn/2.7.0` — WIDER than the shape
    /// this guard pinned at `wamn/2.6.1`, deliberately recorded rather than
    /// papered over (wamn-0h0g.15.20).
    ///
    /// At `09b1132f` the fork answered `TcpBind | UdpBind => is_service &&
    /// ip_is_loopback`: a component could not bind at all, a service only on
    /// loopback, and an unspecified address was never permitted (the old
    /// predicate took `_ip_is_unspecified` and deliberately ignored it). At
    /// `daba6029` a COMPONENT may `UdpBind` on loopback *or unspecified*, and so
    /// may a service. Upstream's rationale is that a UDP bind is not listening —
    /// p2 refuses `stream()` on an unbound socket and std binds explicitly on
    /// p3, so refusing the bind would refuse outbound datagrams instead of
    /// refusing a listener. TcpBind stays denied for components.
    ///
    /// The unspecified case is a real widening, and upstream's comment in
    /// `policy.rs` understates it: the OS socket IS bound to the wildcard
    /// (`sockets/udp.rs`), not rewritten to loopback as the TCP path does —
    /// binding loopback would make the kernel refuse `send_to` off-box with
    /// `EADDRNOTAVAIL`. Inbound confinement moves to the RECEIVE path
    /// (`egress_peers`). That is a different mechanism with a different failure
    /// mode than a bind refusal, so it is pinned here and owned separately.
    #[test]
    fn udp_bind_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy.contains(concat!(
                "            GuestKind::Component => match reason {\n",
                "                SocketAddrUse::UdpBind => ip.is_loopback() || ip.is_unspecified(),\n",
                "                _ => false,\n",
                "            },"
            )),
            "a component may bind UDP on loopback or unspecified, and NOTHING else — TcpBind in \
             particular must stay denied"
        );
        assert!(
            policy.contains(concat!(
                "            GuestKind::Service => match reason {\n",
                "                SocketAddrUse::UdpBind => ip.is_loopback() || ip.is_unspecified(),\n",
                "                _ => ip.is_loopback(),\n",
                "            },"
            )),
            "a service may bind UDP on loopback or unspecified, and anything else on loopback only"
        );
        assert!(
            policy.contains("AddrDecision::Deny(DenyReason::BindNotPermitted)"),
            "a bind outside that authority must be denied, not silently allowed"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(local_address, SocketAddrUse::UdpBind)"),
            "P2 UdpBind must consult the shared socket policy"
        );
        let p2_policy = p2
            .find(".check(addr, SocketAddrUse::UdpOutgoingDatagram)")
            .expect("P2 network send must consult the outgoing-datagram policy");
        let p2_peer_insert = p2
            .find("peers.insert(addr);")
            .expect("P2 network send must record an admitted egress peer");
        assert!(
            p2_policy < p2_peer_insert,
            "P2 must admit the destination before it can populate egress_peers"
        );
        assert!(
            p2.contains(concat!(
                "            if let Some(peers) = stream.egress_peers.as_ref()\n",
                "                && !peers\n",
                "                    .lock()\n",
                "                    .is_ok_and(|peers| peers.contains(&received_addr))\n",
                "            {\n",
                "                return Ok(None);\n",
                "            }"
            )),
            "P2 must discard an off-box datagram whose sender is absent from egress_peers"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains("(self.ctx.socket_addr_check)(local_address, SocketAddrUse::UdpBind)")
                && p3.contains(".into_allowed()"),
            "P3 UdpBind mirror must consult the shared socket policy AND consume the decision \
             fallibly"
        );
        let p3_policy = p3
            .find("let allowed = check(remote_address, SocketAddrUse::UdpOutgoingDatagram)")
            .expect("P3 network send must consult the outgoing-datagram policy");
        let p3_peer_insert = p3
            .find("peers.insert(a);")
            .expect("P3 network send must record an admitted egress peer");
        assert!(
            p3_policy < p3_peer_insert,
            "P3 must admit the destination before it can populate egress_peers"
        );
        assert!(
            p3.contains(concat!(
                "                        if let Some(peers) = egress_peers.as_ref()\n",
                "                            && !peers.lock().is_ok_and(|peers| peers.contains(&addr))\n",
                "                        {\n",
                "                            continue;\n",
                "                        }"
            )),
            "P3 must keep waiting rather than deliver an off-box datagram whose sender is absent \
             from egress_peers"
        );
    }

    /// The `allowedHostLoopbackPorts` grant needs BOTH halves — asserted against
    /// the LINKED fork's own `SocketPolicy::decide`, the only place they can be
    /// separated (wamn-0h0g.15.52).
    ///
    /// `resolve_host_loopback` answers `HostLoopbackNotPermitted` for either
    /// missing half, so "it was refused" alone cannot say which half refused it:
    /// each negative case here removes exactly one half and leaves the other in
    /// place. The runtime phase can only reach the two cases where the host half
    /// is absent, because nothing in this repository installs a host socket
    /// policy — so this is where the per-port grant itself is exercised.
    ///
    /// The permitted case asserts the EVIDENCE that the grant was consulted, not
    /// merely that the dial was allowed: the sentinel is rewritten to real
    /// loopback and forced onto `Plane::Host`. A bare "allowed" would be vacuous,
    /// because the sentinel lives inside `127.0.0.0/8` and falling through to the
    /// guest's own virtual network also allows it.
    #[test]
    fn host_loopback_needs_the_host_enable_and_the_listed_port_together() {
        let listed = [AllowedLoopbackPort::tcp(HOST_LOOPBACK_LISTED_PORT)];
        let target = sentinel_target(HOST_LOOPBACK_LISTED_PORT);

        // Neither half.
        assert_eq!(
            deny_reason(
                &loopback_policy(false, &[], EgressMode::Enforce)
                    .decide(SocketAddrUse::TcpConnect, target)
            ),
            Some(DenyReason::HostLoopbackNotPermitted)
        );
        // Host enabled, port NOT listed — isolates the per-port grant, the half
        // nothing asserted before this bead.
        assert_eq!(
            deny_reason(
                &loopback_policy(true, &[], EgressMode::Enforce)
                    .decide(SocketAddrUse::TcpConnect, target)
            ),
            Some(DenyReason::HostLoopbackNotPermitted),
            "an unlisted loopback port must be refused even where the host enabled the door"
        );
        // Port listed, host NOT enabled — isolates the host-level enable, the
        // inertness the runtime phase exercises end to end.
        assert_eq!(
            deny_reason(
                &loopback_policy(false, &listed, EgressMode::Enforce)
                    .decide(SocketAddrUse::TcpConnect, target)
            ),
            Some(DenyReason::HostLoopbackNotPermitted),
            "a workload grant alone must not open the machine's own loopback"
        );
        // Both halves.
        assert_eq!(
            permitted(
                &loopback_policy(true, &listed, EgressMode::Enforce)
                    .decide(SocketAddrUse::TcpConnect, target)
            ),
            Some((
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), HOST_LOOPBACK_LISTED_PORT),
                Plane::Host
            )),
            "both halves must reach the machine's REAL loopback, not the guest's virtual network"
        );
    }

    /// Drift protection for a linked-fork invariant, not a claim that wamn
    /// currently installs a host-owned port table. The refusal must remain
    /// after the explicit sentinel grant so listing a port cannot override host
    /// ownership, and it must return directly rather than pass through the
    /// count-mode softening used for ordinary egress policy.
    #[test]
    fn host_owned_port_refusal_stays_after_the_allowlist_and_before_success() {
        let policy = fork_source("src/sockets/policy.rs");
        let host_enable = policy
            .find("if !self.host_loopback_enabled {")
            .expect("sentinel resolution must require the host-level enable");
        let listed_port = policy
            .find("if !check_allowed_loopback(&self.host_loopback, addr, protocol) {")
            .expect("sentinel resolution must require the workload's per-port grant");
        let rewrite = policy
            .find("let target = internal_names::rewrite_sentinel(addr);")
            .expect("the sentinel must resolve to the machine's real loopback");
        let host_owned = policy
            .find("&& table.is_published(protocol, target)")
            .expect("sentinel resolution must consult the host-owned port table");
        let direct_refusal = policy
            .find("return Err(DenyReason::HostOwnedPort);")
            .expect("a host-owned port must refuse directly, even in Count mode");
        let success = policy
            .find("Ok((target, Plane::Host))")
            .expect("the granted non-host-owned sentinel path must reach the host plane");

        assert!(
            host_enable < listed_port
                && listed_port < rewrite
                && rewrite < host_owned
                && host_owned < direct_refusal
                && direct_refusal < success,
            "sentinel resolution must check host enable, per-port grant, host ownership, and only then succeed"
        );
    }

    /// A listed entry grants one port over one transport — not loopback.
    ///
    /// Without this, `allowedHostLoopbackPorts` could degrade into a boolean and
    /// the gate above would still pass: the port it dials is the one it listed.
    #[test]
    fn a_listed_loopback_port_grants_only_that_port_and_transport() {
        let policy = loopback_policy(
            true,
            &[AllowedLoopbackPort::tcp(HOST_LOOPBACK_LISTED_PORT)],
            EgressMode::Enforce,
        );
        let listed = sentinel_target(HOST_LOOPBACK_LISTED_PORT);

        assert!(
            permitted(&policy.decide(SocketAddrUse::TcpConnect, listed)).is_some(),
            "the listed port over the listed transport must be reachable"
        );
        assert_eq!(
            deny_reason(&policy.decide(SocketAddrUse::UdpConnect, listed)),
            Some(DenyReason::HostLoopbackNotPermitted),
            "a TCP grant must not carry UDP on the same port"
        );
        assert_eq!(
            deny_reason(&policy.decide(
                SocketAddrUse::TcpConnect,
                sentinel_target(HOST_LOOPBACK_UNLISTED_PORT)
            )),
            Some(DenyReason::HostLoopbackNotPermitted),
            "granting one local port must not grant the machine's other local ports"
        );
    }

    /// Count mode must not soften the host-loopback refusal.
    ///
    /// `EgressMode::Count` is upstream's `#[default]`: it logs what it would
    /// refuse and allows it anyway. If the host-loopback refusal ever routed
    /// through that gate instead of returning directly, the machine's own
    /// loopback would stand open on a default-configured host — and this gate's
    /// opted-in run is exactly such a host, since `shape_socket_policy` returns
    /// an opted-in guest's policy unshaped.
    ///
    /// The second half is the control that makes the first half mean something:
    /// under the SAME policy an ordinary destination IS allowed, so the refusal
    /// above is the loopback rule and not a mode that was never in effect.
    #[test]
    fn count_mode_cannot_open_the_host_loopback_door() {
        let policy = loopback_policy(
            false,
            &[AllowedLoopbackPort::tcp(HOST_LOOPBACK_LISTED_PORT)],
            EgressMode::Count,
        );
        assert_eq!(
            deny_reason(&policy.decide(
                SocketAddrUse::TcpConnect,
                sentinel_target(HOST_LOOPBACK_LISTED_PORT)
            )),
            Some(DenyReason::HostLoopbackNotPermitted),
            "count mode must keep the machine's own loopback shut"
        );
        assert!(
            permitted(&policy.decide(
                SocketAddrUse::TcpConnect,
                "93.184.216.34:443".parse().expect("test address")
            ))
            .is_some(),
            "count mode must allow the ordinary egress it merely counts, or the refusal above \
             proves nothing about the loopback rule"
        );
    }

    /// The address the runtime phase hands sockprobe must land on the
    /// host-loopback branch.
    ///
    /// sockprobe cannot read the fork's constants, so the gate resolves the
    /// sentinel and passes it through the environment. If that address ever
    /// stopped being recognised as the sentinel, every `host-loopback-*` arm
    /// would quietly become an ordinary virtual-loopback connect — permitted, and
    /// proving nothing. `SocketPolicy::default()` on purpose: production
    /// `build_engine` installs that policy, and no production caller selects
    /// another one.
    #[test]
    fn the_sentinel_target_handed_to_sockprobe_reaches_the_gated_branch() {
        for port in [HOST_LOOPBACK_LISTED_PORT, HOST_LOOPBACK_UNLISTED_PORT] {
            let target = sentinel_target(port);
            assert!(
                is_host_sentinel(target.ip()),
                "{target} must be the host sentinel, or the arm dials the guest's own network"
            );
            assert_eq!(
                deny_reason(&SocketPolicy::default().decide(SocketAddrUse::TcpConnect, target)),
                Some(DenyReason::HostLoopbackNotPermitted),
                "{target} must be refused by the host-loopback rule under the policy this \
                 repository's engine installs, not allowed onto the virtual plane"
            );
        }
    }

    /// The host-loopback grant surface, pinned on the exact linked fork revision.
    ///
    /// Three things must hold for the arms above to mean what they claim, and
    /// none of them is observable from the guest side:
    ///
    /// 1. The sentinel is resolved BEFORE the virtual-loopback shortcut. It lives
    ///    inside `127.0.0.0/8`, so were the two ever reordered, every dial would
    ///    match `is_loopback()` first and return `Plane::Virtual` — reaching the
    ///    guest's own network instead of taking the gate.
    /// 2. BOTH halves are checked, and each refuses DIRECTLY rather than through
    ///    `Self::gate`, which `EgressMode::Count` softens.
    /// 3. The workload's `allowedHostLoopbackPorts` is what becomes
    ///    `SocketPolicy::host_loopback`. Were that plumbing dropped, the list the
    ///    third run supplies would not be the list the policy consults.
    #[test]
    fn host_loopback_grant_surface_is_pinned() {
        let policy = fork_source("src/sockets/policy.rs");
        assert!(
            policy.contains(concat!(
                "        if internal_names::is_host_sentinel(addr.ip()) {\n",
                "            return self.resolve_host_loopback(addr, protocol);\n",
                "        }\n"
            )),
            "the host sentinel must resolve before the virtual-loopback shortcut, or an address \
             inside 127.0.0.0/8 bypasses the host-loopback gate entirely"
        );
        assert!(
            policy.contains(concat!(
                "        if !self.host_loopback_enabled {\n",
                "            return Err(DenyReason::HostLoopbackNotPermitted);\n",
                "        }\n",
                "        if !check_allowed_loopback(&self.host_loopback, addr, protocol) {\n",
                "            return Err(DenyReason::HostLoopbackNotPermitted);\n",
                "        }\n"
            )),
            "both halves of the host-loopback grant must be checked, and each must refuse \
             directly rather than through the EgressMode gate that count mode softens"
        );

        let shaper = fork_source("src/engine/linked_call.rs");
        assert!(
            shaper.contains(concat!(
                "            host_loopback: Arc::clone",
                "(&template.local_resources.allowed_host_loopback_ports),"
            )),
            "the workload's allowedHostLoopbackPorts must be what the guest's policy carries, or \
             the grant surface this gate configures is not the one the policy consults"
        );
    }
}

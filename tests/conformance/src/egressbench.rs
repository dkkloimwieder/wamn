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
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::host::{HostApi, HostBuilder};
use wash_runtime::types::{
    HostPathVolume, LocalResources, Service, Volume, VolumeMount, VolumeType, Workload,
    WorkloadStartRequest,
};
use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

use wamn_component_policy::denied_imports;
use wamn_runtime::engine::build_engine;

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
    /// opt-in OFF (deny-by-default, E13/E15 negative) then ON (opted-in
    /// positive). Optional: the static import review runs without it.
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
}

/// Parse sockprobe's five arm-specific verdicts.
fn read_verdicts(report_dir: &Path) -> Option<SocketVerdicts> {
    let contents = std::fs::read_to_string(report_dir.join("outcome")).ok()?;
    let mut tcp_connect = None;
    let mut udp_connect = None;
    let mut udp_outgoing_datagram = None;
    let mut udp_bind_loopback = None;
    let mut udp_bind_non_loopback = None;
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
        }
    }
    Some(SocketVerdicts {
        tcp_connect: tcp_connect?,
        udp_connect: udp_connect?,
        udp_outgoing_datagram: udp_outgoing_datagram?,
        udp_bind_loopback: udp_bind_loopback?,
        udp_bind_non_loopback: udp_bind_non_loopback?,
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

/// Start sockprobe as a SERVICE (so `is_service` is true and its loopback UDP
/// bind is permitted — the raw-egress connect is the gated op) with a mounted
/// host-path report volume, optionally opting into raw sockets.
async fn run_sockprobe(
    host: &Arc<wash_runtime::host::Host>,
    bytes: &[u8],
    id: &str,
    allow_raw_sockets: bool,
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
    };
    resources.environment.insert(
        "SOCKPROBE_REPORT_PATH".to_string(),
        "/report/outcome".to_string(),
    );
    resources.environment.insert(
        "SOCKPROBE_NON_LOOPBACK_TARGET".to_string(),
        non_loopback_target()?.to_string(),
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
/// assertion is text-independent. Returns whether the phase passed.
async fn assert_runtime_sockets(sockprobe: &[u8]) -> anyhow::Result<bool> {
    println!("\n## runtime raw-socket policy (E13 TCP / E15 UDP) — sockprobe as a service");
    let engine = build_engine(&[])?;
    let host = HostBuilder::new().with_engine(engine).build()?;
    let host = host.start().await?;

    let base = std::env::temp_dir().join(format!("wamn-egress-sock-{}", std::process::id()));
    let deny_dir = base.join("deny");
    let optin_dir = base.join("optin");
    std::fs::create_dir_all(&deny_dir)?;
    std::fs::create_dir_all(&optin_dir)?;

    run_sockprobe(&host, sockprobe, "sock-deny", false, &deny_dir).await?;
    run_sockprobe(&host, sockprobe, "sock-optin", true, &optin_dir).await?;

    // sockprobe writes its verdicts and exits within milliseconds; give both
    // services time to run before reading the reports.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let deny = read_verdicts(&deny_dir);
    let optin = read_verdicts(&optin_dir);
    let _ = std::fs::remove_dir_all(&base);

    println!("  deny-by-default (no wamn.allow-raw-sockets): {deny:?}");
    println!("  opted-in        (wamn.allow-raw-sockets=true): {optin:?}");

    let pass =
        matches!((&deny, &optin), (Some(deny), Some(optin)) if runtime_verdicts_pass(deny, optin));
    if pass {
        println!(
            "    PASS: TcpConnect, UdpConnect, and UdpOutgoingDatagram deny by default and \
             permit only on opt-in; UdpBind remains service-loopback-only"
        );
    } else {
        println!(
            "    FAIL: expected each raw-egress arm denied/default + permitted/opt-in and \
             UdpBind service-loopback-only; deny={deny:?}, optin={optin:?}"
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
    println!("#        exercised: raw TCP/UDP egress denied by default, allowed only on opt-in.");

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

    fn canonical_verdicts() -> (SocketVerdicts, SocketVerdicts) {
        (
            SocketVerdicts {
                tcp_connect: "denied".to_string(),
                udp_connect: "denied".to_string(),
                udp_outgoing_datagram: "denied".to_string(),
                udp_bind_loopback: "bound".to_string(),
                udp_bind_non_loopback: "denied".to_string(),
            },
            SocketVerdicts {
                tcp_connect: "allowed-failed".to_string(),
                udp_connect: "connected".to_string(),
                udp_outgoing_datagram: "sent".to_string(),
                udp_bind_loopback: "bound".to_string(),
                udp_bind_non_loopback: "denied".to_string(),
            },
        )
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
            package["version"], "2.6.1",
            "socket gate must inspect wash-runtime 2.6.1"
        );
        let source = package["source"]
            .as_str()
            .expect("wash-runtime must retain its git source");
        assert!(
            source.ends_with("#09b1132f2bab36e6e71f4637bd0e4755e359dd43"),
            "socket gate must inspect the exact linked wash-runtime 2.6.1 fork revision, got {source}"
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
             udp-bind-loopback=bound\nudp-bind-non-loopback=denied\n",
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
    fn udp_bind_arm_rejects_service_scope_or_address_widening() {
        let (deny, optin) = canonical_verdicts();
        assert!(runtime_verdicts_pass(&deny, &optin));
        let mutant = SocketVerdicts {
            udp_bind_non_loopback: "bound".to_string(),
            ..deny
        };
        assert!(
            !runtime_verdicts_pass(&mutant, &optin),
            "UdpBind service-non-loopback widening mutation survived"
        );
    }

    #[test]
    fn tcp_connect_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/engine/linked_call.rs");
        assert!(
            policy.contains("SocketAddrUse::TcpConnect => allow_raw_sockets,"),
            "TcpConnect must deny non-loopback P2/P3 egress without opt-in and permit opted-in work"
        );
        let p2 = fork_source("src/sockets/host_tcp.rs");
        assert!(
            p2.contains(".check_socket_addr(remote_address, SocketAddrUse::TcpConnect)"),
            "P2 TcpConnect must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_tcp_p3.rs");
        assert!(
            p3.contains("if !check(remote_address, SocketAddrUse::TcpConnect).await"),
            "P3 TcpConnect mirror must consult the shared socket policy"
        );
    }

    #[test]
    fn udp_connect_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/engine/linked_call.rs");
        assert!(
            policy.contains("SocketAddrUse::UdpConnect => allow_raw_sockets,"),
            "UdpConnect must deny non-loopback P2/P3 egress without opt-in and permit opted-in work"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(connect_addr, SocketAddrUse::UdpConnect)"),
            "P2 UdpConnect must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains(
                "(self.ctx.socket_addr_check)(remote_address, SocketAddrUse::UdpConnect).await"
            ),
            "P3 UdpConnect mirror must consult the shared socket policy"
        );
    }

    #[test]
    fn udp_outgoing_datagram_policy_and_p2_p3_call_sites_are_pinned() {
        let policy = fork_source("src/engine/linked_call.rs");
        assert!(
            policy.contains("SocketAddrUse::UdpOutgoingDatagram => allow_raw_sockets,"),
            "UdpOutgoingDatagram must deny non-loopback P2/P3 sends without opt-in and permit opt-in"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(addr, SocketAddrUse::UdpOutgoingDatagram)"),
            "P2 UdpOutgoingDatagram must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains("check(remote_address, SocketAddrUse::UdpOutgoingDatagram).await"),
            "P3 UdpOutgoingDatagram mirror must consult the shared socket policy"
        );
    }

    #[test]
    fn udp_bind_policy_and_p2_p3_call_sites_are_service_loopback_only() {
        let policy = fork_source("src/engine/linked_call.rs");
        assert!(
            policy.contains(
                "SocketAddrUse::TcpBind | SocketAddrUse::UdpBind => is_service && ip_is_loopback,"
            ),
            "UdpBind must allow service-loopback, deny service-non-loopback, and deny \
             non-service-loopback on P2/P3"
        );
        let p2 = fork_source("src/sockets/host_udp.rs");
        assert!(
            p2.contains(".check(local_address, SocketAddrUse::UdpBind)"),
            "P2 UdpBind must consult the shared socket policy"
        );
        let p3 = fork_source("src/sockets/host_udp_p3.rs");
        assert!(
            p3.contains(
                "(self.ctx.socket_addr_check)(local_address, SocketAddrUse::UdpBind).await"
            ),
            "P3 UdpBind mirror must consult the shared socket policy"
        );
    }
}

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
//! `wamn.allow-raw-sockets` opt-in (see [`assert_runtime_sockets`]).
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
//! and (with `--sockprobe`) that the runtime deny actually fires. See
//! docs/security-db-path.md.
//!
//! TWO PROFILES (E17). The verdict comes from `wamn_host::egress_guard` — the
//! same classifier the host publish-gate uses — not a forked local rule:
//! - **first-party flow-runner** legitimately imports `wamn:postgres` (the DB
//!   path); it is screened by the socket denylist (`egress_guard::denied_imports`,
//!   E13a) and must import the plugin.
//! - **tenant / custom-node** artifacts are held to the POSITIVE allowlist v1
//!   (`egress_guard::disallowed_tenant_imports`, docs/wamn-node-design-notes.md
//!   §9): any non-allowlisted package is refused — `wamn:postgres` most of all,
//!   since importing the plugin hands a tenant node the raw DB surface + the
//!   `DO`/`EXECUTE` claim-mutation bypass (docs/findings.md §3 E17). A denylist
//!   cannot express this: `wamn:postgres` is the *intended* path for the runner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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

use wamn_host::egress_guard::{denied_imports, disallowed_tenant_imports};
use wamn_host::engine::build_engine;

#[derive(Args)]
pub struct EgressBenchArgs {
    /// The standard flow-runner — the first-party DB-touching shipped workload.
    /// Must import `wamn:postgres` (the DB path) and must NOT import `wasi:sockets`.
    #[arg(long)]
    flowrunner: PathBuf,

    /// Tenant / custom-node artifacts expected to CLEAR the positive allowlist
    /// v1 — a legitimate custom node (no `wamn:postgres`, no `wasi:sockets`).
    /// The gate fails if one is refused. Repeatable.
    #[arg(long = "component")]
    components: Vec<PathBuf>,

    /// Tenant / custom-node artifacts that MUST be REFUSED by the allowlist v1
    /// — the E17 negative. Each imports a non-allowlisted package (e.g. pgprobe
    /// imports `wamn:postgres`, the raw DB surface + `DO`/`EXECUTE`
    /// claim-mutation bypass); the gate PASSES when the classifier refuses it,
    /// FAILS if it is admitted. This is the polarity the `--component` sweep
    /// cannot express. Repeatable.
    #[arg(long = "reject-tenant")]
    reject_tenants: Vec<PathBuf>,

    /// The sockprobe fixture (`components/fixtures/sockprobe`). When set, the
    /// RUNTIME raw-socket phase runs: sockprobe is instantiated as a service
    /// through the production host store path — where the fork's `linked_call`
    /// `socket_addr_check` governs raw TCP/UDP egress — with the raw-socket
    /// opt-in OFF (deny-by-default, E13/E15 negative) then ON (opted-in
    /// positive). Optional: the static import review runs without it.
    #[arg(long)]
    sockprobe: Option<PathBuf>,
}

/// Other host-plugin egress `namespace:package`s not expected in the first-party
/// flow-runner; if one appears it is flagged as a new egress path to justify.
/// (Tenant artifacts are held to the positive allowlist instead, which rejects
/// these by construction.)
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

/// Tenant / custom-node profile: held to the positive allowlist v1
/// (`egress_guard::disallowed_tenant_imports` — the shared classifier the host
/// publish-gate uses). Any non-allowlisted package is refused — most of all
/// `wamn:postgres` (raw DB surface + `DO`/`EXECUTE` claim-mutation bypass) and
/// `wasi:sockets`. Returns whether it passed.
fn assert_tenant(label: &str, names: &[String]) -> bool {
    let pkgs = ns_pkgs(names);
    let disallowed = disallowed_tenant_imports(names.iter().map(String::as_str));

    println!("  {label}");
    println!("    packages: {pkgs:?}");

    if disallowed.is_empty() {
        println!(
            "    PASS: imports only allowlisted packages (allowlist v1; no wamn:postgres, no sockets)"
        );
        true
    } else {
        println!(
            "    FAIL: imports non-allowlisted package(s) {disallowed:?} — tenant artifacts may \
             import only the allowlist v1 (excludes wamn:postgres + wasi:sockets)"
        );
        false
    }
}

/// E17 negative: a tenant / custom-node artifact that MUST be refused because it
/// imports a non-allowlisted package. Passes iff the shared classifier
/// (`egress_guard::disallowed_tenant_imports`) refuses it, naming the offense —
/// the polarity the positive `--component` sweep cannot show, that the allowlist
/// REJECTS (most load-bearingly) a `wamn:postgres`-importing tenant. Returns
/// whether it passed.
fn assert_reject_tenant(label: &str, names: &[String]) -> bool {
    let pkgs = ns_pkgs(names);
    let disallowed = disallowed_tenant_imports(names.iter().map(String::as_str));

    println!("  {label}");
    println!("    packages: {pkgs:?}");

    if disallowed.is_empty() {
        println!(
            "    FAIL: ADMITTED — a tenant artifact importing {pkgs:?} cleared the allowlist v1; \
             the wamn:postgres / raw-socket exclusion did not hold"
        );
        false
    } else {
        println!("    PASS: refused — imports non-allowlisted package(s) {disallowed:?}");
        true
    }
}

/// A sockprobe per-protocol verdict that means the raw-egress op was PERMITTED:
/// the policy let the socket op proceed (it then either connected or failed for
/// an unrelated reason). Anything else — `denied`, `bind-failed`, or a missing
/// report — is NOT permitted. Keyed on sockprobe's stable tokens, not error
/// text, so the positive assertion never guesses the exact non-deny error.
fn sock_permitted(verdict: &str) -> bool {
    matches!(verdict, "connected" | "allowed-failed")
}

/// Parse sockprobe's `tcp=<v>` / `udp=<v>` report file into `(tcp, udp)`.
fn read_verdicts(report_dir: &Path) -> Option<(String, String)> {
    let contents = std::fs::read_to_string(report_dir.join("outcome")).ok()?;
    let mut tcp = None;
    let mut udp = None;
    for line in contents.lines() {
        if let Some(v) = line.strip_prefix("tcp=") {
            tcp = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("udp=") {
            udp = Some(v.trim().to_string());
        }
    }
    Some((tcp?, udp?))
}

/// Start sockprobe as a SERVICE (so `is_service` is true and its loopback UDP
/// bind is permitted — the raw-egress connect is the gated op) with a mounted
/// host-path report volume, optionally opting into raw sockets. Mirrors bench's
/// memhog service pattern.
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
    };
    resources.environment.insert(
        "SOCKPROBE_REPORT_PATH".to_string(),
        "/report/outcome".to_string(),
    );
    if allow_raw_sockets {
        // The fork reads this per-component config in build_ctx_from_template
        // (docs/wash-runtime-fork.md); it is the ONLY opt-in that flips the
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

    // Negative (E13/E15): both protocols denied when NOT opted in.
    let neg = matches!(&deny, Some((tcp, udp)) if tcp == "denied" && udp == "denied");
    // Positive (opted-in): both protocols permitted once opted in.
    let pos = matches!(&optin, Some((tcp, udp)) if sock_permitted(tcp) && sock_permitted(udp));

    if neg {
        println!("    PASS(negative): raw TCP + UDP egress DENIED by default (no opt-in)");
    } else {
        println!(
            "    FAIL(negative): expected tcp=denied,udp=denied without opt-in, got {deny:?} — \
             the fork's socket_addr_check deny-by-default did not hold"
        );
    }
    if pos {
        println!("    PASS(positive): raw TCP + UDP egress PERMITTED under wamn.allow-raw-sockets");
    } else {
        println!(
            "    FAIL(positive): expected both permitted under wamn.allow-raw-sockets=true, got \
             {optin:?} — the opt-in did not flip the verdict"
        );
    }
    Ok(neg && pos)
}

pub async fn run(args: EgressBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-host 2.6/E17 egressbench — DB-path egress review");
    println!("# claim: the wamn:postgres plugin is the only DB path. The first-party");
    println!("#        flow-runner imports it and no raw sockets; tenant / custom-node");
    println!("#        artifacts are held to the positive allowlist v1 (no wamn:postgres).");
    println!("#        With --sockprobe, the runtime raw-socket policy (E13/E15) is also");
    println!("#        exercised: raw TCP/UDP egress denied by default, allowed only on opt-in.");

    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let mut pass = true;

    println!("\n## first-party flow-runner (DB path)");
    let fr = import_names(raw, &args.flowrunner)?;
    pass &= assert_flowrunner(&format!("flow-runner  {}", args.flowrunner.display()), &fr);

    if !args.components.is_empty() {
        println!("\n## tenant / custom-node artifacts (allowlist v1)");
    }
    for path in &args.components {
        let names = import_names(raw, path)?;
        pass &= assert_tenant(&format!("component    {}", path.display()), &names);
    }

    if !args.reject_tenants.is_empty() {
        println!("\n## E17 negative — tenant artifacts that MUST be refused (allowlist v1)");
    }
    for path in &args.reject_tenants {
        let names = import_names(raw, path)?;
        pass &= assert_reject_tenant(&format!("reject-tenant {}", path.display()), &names);
    }

    if let Some(sockprobe) = &args.sockprobe {
        let bytes = std::fs::read(sockprobe)
            .with_context(|| format!("read sockprobe {}", sockprobe.display()))?;
        pass &= assert_runtime_sockets(&bytes).await?;
    }

    println!("\negressbench complete — overall PASS: {pass}");
    if !pass {
        bail!(
            "2.6/E17 egress gate failed: a shipped workload exposes a raw-socket / non-allowlisted egress path"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn tenant_node_shape_passes() {
        // A standard custom node: SDK imports + determinism plumbing, no DB.
        let n = names(&[
            "wamn:node/credentials@0.1.0",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:io/streams@0.2.3",
            "wasi:cli/run@0.2.3",
        ]);
        assert!(assert_tenant("node", &n));
    }

    /// E17 — the fix of record. A TENANT artifact importing `wamn:postgres` (the
    /// raw DB surface + the DO/EXECUTE claim-mutation bypass) is REJECTED by the
    /// custom-node profile. The bug was that this shape PASSED (require_postgres
    /// was false and the socket-only classifier let `wamn:postgres` through).
    /// Mutation (a): re-adding `wamn:postgres` to `TENANT_ALLOWED_PKGS` flips
    /// this to a pass. Mutation (b): removing the `disallowed_tenant_imports`
    /// call from `assert_tenant` (bypassing the chokepoint) flips it too.
    #[test]
    fn tenant_importing_postgres_is_rejected() {
        let n = names(&["wamn:postgres/client@0.1.0", "wasi:io/streams@0.2.3"]);
        assert!(!assert_tenant("evil-node", &n));
    }

    /// E13a no-regression at the tenant layer: a socket importer is still refused
    /// (wasi:sockets is not on the allowlist).
    #[test]
    fn tenant_importing_sockets_is_rejected() {
        let n = names(&["wasi:sockets/tcp@0.2.3", "wasi:io/streams@0.2.3"]);
        assert!(!assert_tenant("socket-node", &n));
    }

    /// E17 negative — the fixture of record. A tenant importing `wamn:postgres`
    /// (the raw DB surface + the DO/EXECUTE claim-mutation bypass) is REFUSED, so
    /// the negative assertion PASSES. This is the polarity `--component` cannot
    /// express: there, a postgres import is a gate FAILURE, not a proven refusal.
    #[test]
    fn reject_tenant_passes_when_postgres_importer_refused() {
        let n = names(&["wamn:postgres/client@0.1.0", "wasi:io/streams@0.2.3"]);
        assert!(assert_reject_tenant("pgprobe", &n));
    }

    /// A legitimate node has nothing to refuse, so the E17 negative assertion
    /// FAILS — it asserts a rejection that did not happen (no false green).
    #[test]
    fn reject_tenant_fails_when_admitted() {
        let n = names(&["wamn:node/credentials@0.1.0", "wasi:io/streams@0.2.3"]);
        assert!(!assert_reject_tenant("legit-node", &n));
    }

    /// The runtime-phase verdict classifier: only sockprobe's two "the op was
    /// permitted" tokens count as permitted. `denied` (policy refusal) and
    /// `bind-failed` (a harness misconfiguration, not a policy pass) do not — so
    /// a stuck/failed run can never masquerade as the opted-in positive.
    #[test]
    fn sock_permitted_only_accepts_permitted_tokens() {
        assert!(sock_permitted("connected"));
        assert!(sock_permitted("allowed-failed"));
        assert!(!sock_permitted("denied"));
        assert!(!sock_permitted("bind-failed"));
        assert!(!sock_permitted(""));
    }

    /// The report parser pulls the tcp/udp verdicts out of sockprobe's file; a
    /// report missing either line yields None (treated as a phase failure).
    #[test]
    fn read_verdicts_parses_both_lines() {
        let dir = std::env::temp_dir().join(format!("wamn-egress-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("outcome"), "tcp=denied\nudp=connected\n").unwrap();
        assert_eq!(
            read_verdicts(&dir),
            Some(("denied".to_string(), "connected".to_string()))
        );
        std::fs::write(dir.join("outcome"), "tcp=denied\n").unwrap();
        assert_eq!(read_verdicts(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

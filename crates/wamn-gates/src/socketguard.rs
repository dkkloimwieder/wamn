//! socketguard — the E13a publish-time egress-guard refusal gate.
//!
//! The runtime links `wasi:sockets` on every workload linker unconditionally
//! and its `TcpConnect` policy is allow-all (never consulting `allowed_hosts`,
//! which governs `wasi:http` ONLY), so a component that *imports* `wasi:sockets`
//! opens arbitrary outbound TCP with DNS, bypassing the `wamn:postgres`
//! tenant-claim / RLS path (docs/findings.md §3 E13, docs/security-db-path.md).
//! `wamn_host::egress_guard` is the build/publish-side enforcement: one
//! structural rule that refuses any component importing the `wasi:sockets`
//! package.
//!
//! This gate proves that enforcement HERMETICALLY — it synthesizes its two
//! fixtures in-process (no external guest build, no OCI registry, so the local
//! mode is the whole gate): a NEGATIVE case (a world importing `wasi:sockets`
//! must be refused at publish) and a POSITIVE control (a standard world —
//! clocks/io — must still publish). The in-cluster registry run rides
//! wamn-2jkm.41.
//!
//! Unlike `egressbench` — which walks the REAL shipped components and asserts
//! they carry no socket surface — this gate proves the guard *rejects* an
//! adversarial component, the property the shipped-component sweep cannot show.

use anyhow::bail;
use clap::Args;
use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

use wamn_host::egress_guard::{EgressGuardError, screen_compiled};
use wamn_host::engine::build_engine;

#[derive(Args)]
pub struct SocketGuardArgs {}

/// A socket interface the runtime links unconditionally — the world an
/// attacker component would import to reach Postgres directly.
const ATTACKER_IMPORTS: &[&str] = &[
    "wasi:sockets/tcp@0.2.3",
    "wasi:sockets/ip-name-lookup@0.2.3",
    "wasi:clocks/monotonic-clock@0.2.3",
];

/// A standard workload world — the `allowed_hosts`-gated egress and plumbing a
/// generic node imports, no raw sockets. The positive control.
const STANDARD_IMPORTS: &[&str] = &[
    "wasi:clocks/monotonic-clock@0.2.3",
    "wasi:io/streams@0.2.3",
    "wasi:http/outgoing-handler@0.2.3",
];

/// Synthesize a minimal, valid component whose world imports exactly
/// `import_names` (each as an empty instance). Enough for the guard to walk the
/// import list — the guard keys on the import NAMES, not their shapes.
fn synth_component(import_names: &[&str]) -> Vec<u8> {
    use wasm_encoder::{
        Component, ComponentImportSection, ComponentTypeRef, ComponentTypeSection, InstanceType,
    };

    let mut types = ComponentTypeSection::new();
    for _ in import_names {
        types.instance(&InstanceType::new());
    }
    let mut imports = ComponentImportSection::new();
    for (i, name) in import_names.iter().enumerate() {
        imports.import(*name, ComponentTypeRef::Instance(i as u32));
    }

    let mut component = Component::new();
    component.section(&types);
    component.section(&imports);
    component.finish()
}

/// Compile the synthesized bytes on the production engine, then screen them.
/// A compile failure is a hard gate error (bad synthesis), NOT a refusal — so
/// the negative assertion below tests the guard, never a malformed fixture.
fn screen(
    engine: &RawEngine,
    imports: &[&str],
    label: &str,
) -> anyhow::Result<Result<(), EgressGuardError>> {
    let bytes = synth_component(imports);
    let component = WasmtimeComponent::new(engine, &bytes)
        .map_err(|e| anyhow::anyhow!("compile synthesized component {label}: {e}"))?;
    Ok(screen_compiled(&component, label))
}

pub async fn run(_args: SocketGuardArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-gates socketguard — E13a publish-time egress guard (hermetic)");
    println!("# claim: a component importing wasi:sockets is REFUSED at publish; a");
    println!("#        standard component still publishes. Fixtures synthesized in-process.");

    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let mut pass = true;

    // NEGATIVE — the socket-importing world must be refused, naming the offense.
    println!("\n## negative — a wasi:sockets importer is refused at publish");
    match screen(raw, ATTACKER_IMPORTS, "socket-importer.wasm")? {
        Err(e) => println!("    PASS: refused — {e}"),
        Ok(()) => {
            println!("    FAIL: a wasi:sockets importer was ADMITTED — the DB-path bypass is open");
            pass = false;
        }
    }

    // POSITIVE control — a standard world must still publish.
    println!("\n## positive control — a standard workload still publishes");
    match screen(raw, STANDARD_IMPORTS, "standard.wasm")? {
        Ok(()) => println!("    PASS: admitted — no raw-socket surface"),
        Err(e) => {
            println!("    FAIL: a standard workload was REFUSED — {e}");
            pass = false;
        }
    }

    println!("\nsocketguard complete — overall PASS: {pass}");
    if !pass {
        bail!("E13a socketguard failed: the publish-time egress guard did not hold");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> RawEngine {
        build_engine(&[]).expect("engine").inner().clone()
    }

    /// End-to-end over a REAL compiled component: the guard walks the synthesized
    /// world's import list and refuses it, naming every socket interface (and no
    /// non-socket import). This is what egressbench's shipped-component sweep
    /// cannot show — that the guard *rejects* an adversarial world.
    #[test]
    fn socket_importer_is_refused_naming_the_offense() {
        let e = engine();
        let verdict = screen(&e, ATTACKER_IMPORTS, "socket-importer.wasm").expect("compiles");
        match verdict {
            Err(EgressGuardError::RawSocketImport { component, imports }) => {
                assert_eq!(component, "socket-importer.wasm");
                assert_eq!(
                    imports,
                    vec![
                        "wasi:sockets/tcp@0.2.3".to_string(),
                        "wasi:sockets/ip-name-lookup@0.2.3".to_string(),
                    ],
                    "names every socket import, no others"
                );
            }
            Err(EgressGuardError::DisallowedTenantImport { .. }) => {
                panic!("socket denylist produced a tenant-allowlist refusal — wrong classifier")
            }
            Err(EgressGuardError::DisallowedNodeInterface { .. }) => {
                panic!("socket denylist produced an interface-lint refusal — wrong classifier")
            }
            Ok(()) => panic!("guard ADMITTED a wasi:sockets importer — the bypass is open"),
        }
    }

    /// The positive control: a standard world (clocks/io/http) clears the guard.
    #[test]
    fn standard_workload_publishes() {
        let e = engine();
        let verdict = screen(&e, STANDARD_IMPORTS, "standard.wasm").expect("compiles");
        assert!(
            verdict.is_ok(),
            "a standard workload must publish: {verdict:?}"
        );
    }
}

//! socketguard — the E13a publish-time egress-guard refusal gate.
//!
//! The runtime links `wasi:sockets` on every workload linker unconditionally.
//! The fork independently denies raw TCP/UDP by default, but publish must still
//! reject components that import the package: runtime denial is not a
//! substitute for keeping the raw DB-bypass surface out of published worlds.
//! `wamn_component_policy` is that build/publish-side enforcement.
//!
//! This gate proves that enforcement HERMETICALLY — it synthesizes its fixtures
//! in-process (no external guest build, no OCI registry, so the local mode is
//! the whole gate): P2 and P3 NEGATIVE cases (worlds importing `wasi:sockets`
//! must be refused at publish) and a POSITIVE control (a standard world —
//! clocks/io — must still publish).
//!
//! Unlike `egressbench` — which walks the REAL shipped components and asserts
//! they carry no socket surface — this gate proves the guard *rejects* an
//! adversarial component, the property the shipped-component sweep cannot show.

use anyhow::bail;
use clap::Args;
use wamn_component_policy::{EgressGuardError, PolicyProfile, analyze};
use wamn_runtime::engine::build_engine;

#[derive(Args)]
pub struct SocketGuardArgs {}

/// A socket interface the runtime links unconditionally — the world an
/// attacker component would import to reach Postgres directly.
const P2_ATTACKER_IMPORTS: &[&str] = &[
    "wasi:sockets/tcp@0.2.3",
    "wasi:sockets/ip-name-lookup@0.2.3",
    "wasi:clocks/monotonic-clock@0.2.3",
];

const P3_ATTACKER_IMPORTS: &[&str] = &[
    "wasi:sockets/types@0.3.0",
    "wasi:sockets/ip-name-lookup@0.3.0",
    "wasi:clocks/monotonic-clock@0.3.0",
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
    engine: &wash_runtime::engine::Engine,
    imports: &[&str],
    label: &str,
) -> anyhow::Result<Result<(), EgressGuardError>> {
    let bytes = synth_component(imports);
    let imports = wamn_runtime::component_imports(engine, &bytes, label)?;
    Ok(analyze(&imports, PolicyProfile::FirstParty, label).map(|_| ()))
}

pub async fn run(_args: SocketGuardArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-gates socketguard — E13a publish-time egress guard (hermetic)");
    println!("# claim: P2 and P3 components importing wasi:sockets are REFUSED at publish;");
    println!("#        a standard component still publishes. Fixtures synthesized in-process.");

    let engine = build_engine(&[])?;

    let mut pass = true;

    // NEGATIVE — the socket-importing world must be refused, naming the offense.
    for (abi, imports) in [("P2", P2_ATTACKER_IMPORTS), ("P3", P3_ATTACKER_IMPORTS)] {
        println!("\n## negative — a {abi} wasi:sockets importer is refused at publish");
        match screen(&engine, imports, &format!("socket-importer-{abi}.wasm"))? {
            Err(e) => println!("    PASS: refused — {e}"),
            Ok(()) => {
                println!(
                    "    FAIL: a {abi} wasi:sockets importer was ADMITTED — the DB-path bypass is open"
                );
                pass = false;
            }
        }
    }

    // POSITIVE control — a standard world must still publish.
    println!("\n## positive control — a standard workload still publishes");
    match screen(&engine, STANDARD_IMPORTS, "standard.wasm")? {
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

    fn engine() -> wash_runtime::engine::Engine {
        build_engine(&[]).expect("engine")
    }

    /// End-to-end over a REAL compiled component: the guard walks the synthesized
    /// world's import list and refuses it, naming every socket interface (and no
    /// non-socket import). This is what egressbench's shipped-component sweep
    /// cannot show — that the guard *rejects* an adversarial world.
    #[test]
    fn p2_socket_importer_is_refused_naming_the_offense() {
        let e = engine();
        let verdict = screen(&e, P2_ATTACKER_IMPORTS, "socket-importer-P2.wasm").expect("compiles");
        match verdict {
            Err(EgressGuardError::RawSocketImport { component, imports }) => {
                assert_eq!(component, "socket-importer-P2.wasm");
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

    #[test]
    fn p3_socket_importer_is_refused_naming_the_offense() {
        let e = engine();
        let verdict = screen(&e, P3_ATTACKER_IMPORTS, "socket-importer-P3.wasm").expect("compiles");
        match verdict {
            Err(EgressGuardError::RawSocketImport { component, imports }) => {
                assert_eq!(component, "socket-importer-P3.wasm");
                assert_eq!(
                    imports,
                    vec![
                        "wasi:sockets/types@0.3.0".to_string(),
                        "wasi:sockets/ip-name-lookup@0.3.0".to_string(),
                    ],
                    "names every P3 socket import, no others"
                );
            }
            other => panic!("P3 socket importer was not refused by the socket guard: {other:?}"),
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

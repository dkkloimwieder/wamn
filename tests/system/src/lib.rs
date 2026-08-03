//! Black-box proofs that drive deployed or public repository surfaces.

pub use wamn_test_fixtures::{apifixture, f1fixture};

pub(crate) fn standard_implementation(
    node_type: &str,
) -> anyhow::Result<wamn_catalog::NodeImplementation> {
    let descriptor = wamn_standard_nodes::describe(node_type)
        .ok_or_else(|| anyhow::anyhow!("unknown standard node type {node_type:?}"))?;
    let contract =
        wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
    wamn_catalog::NodeImplementation::from_resolved_platform_contract(contract)
        .map_err(anyhow::Error::new)
}

pub mod apiproof;
pub mod callable_f0;
pub mod callable_f1;
pub mod callable_f2;
pub mod callable_f3;
pub mod callable_f4;
pub mod callable_wave1;
pub mod callable_wave2;
pub mod childproof;
pub mod credproof;
pub mod deadlineproof;
pub mod f1proof;
pub mod invocationproof;
pub mod pocsuiteproof;
pub mod traceproof;

//! Results and execution identity for a run-store transaction.

use crate::run_store::ProductionLeaseError;

/// Result of one host-only production queue turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionLeaseResult {
    /// No eligible row was visible to this project.
    Empty,
    /// A fresh lease for router execution after the caller commits.
    Ready {
        run_id: String,
        input_json: String,
        checkpoint_json: Option<String>,
        lease_generation: i64,
        graph_id: wamn_package::canonical_name::CanonicalName,
        graph_version: i32,
        workflow: Box<ProductionWorkflow>,
    },
}

/// The workflow and execution identity fixed at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionWorkflow {
    pub graph_hash: wamn_digest::Digest,
    pub execution_principal_id: wamn_identity_protocol::PrincipalId,
    pub execution_role: String,
    pub causation: Option<wamn_event::Causation>,
}

/// The composed outcome of one lease transaction, decided before COMMIT.
#[derive(Debug)]
pub enum LeaseTurn {
    /// The transaction may commit and report this result.
    Leased(ProductionLeaseResult),
    /// The lease grant was refused inside its own subtransaction, which the
    /// composer already rolled back. The transaction is still live and MUST
    /// commit so the crash-evidence attempt advance taken before the savepoint
    /// survives — a refusal that rolls its own attempt counter back can never
    /// reach `max_attempts`, so the janitor can never reap the run and it stays
    /// the project's FIFO head forever (wamn-0h0g.15.69). The lease itself still
    /// fails with this error.
    GrantRefused(ProductionLeaseError),
}

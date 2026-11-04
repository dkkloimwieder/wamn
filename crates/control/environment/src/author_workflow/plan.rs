use super::{AuthorWorkflowError, AuthorWorkflowErrorType, DefinitionHash, WiringDocument};

/// Require a GREEN verdict from the report read under this document's own hash.
///
/// The hash is the whole binding. This decides only what the control store
/// answered for that exact key, so a green report belonging to another document
/// cannot reach here as `Some(true)` — it is not under this key at all, and
/// arrives as the same `None` a never-gated document does.
pub(crate) fn require_green_report(
    verdict: Option<bool>,
    document: &WiringDocument,
    wiring_hash: &DefinitionHash,
) -> Result<(), AuthorWorkflowError> {
    match verdict {
        Some(true) => Ok(()),
        Some(false) => Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Report,
            format!(
                "wiring {:?} version {} was gated and REFUSED at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
        None => Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Report,
            format!(
                "wiring {:?} version {} has no gate report at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
    }
}

/// The identity a run of the workflow executes as
/// (docs/architecture/execution.md#environment-workflows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowExecution<'a> {
    /// The `workflow` user, with this role active.
    Role(&'a str),
    /// The caller, with its active role.
    Invoker,
}

/// What starts a run of the workflow
/// (docs/architecture/execution.md#workflows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowTrigger<'a> {
    /// The host route `workflow.start`.
    Manual,
    /// The schedule task, at each due moment of this five-field UTC cron.
    Schedule(&'a str),
    /// A captured model change, filtered with the registration vocabulary.
    Event {
        source_package: &'a str,
        entity: &'a str,
        ops: &'a [wamn_event_reg::Op],
        condition: Option<&'a str>,
    },
}

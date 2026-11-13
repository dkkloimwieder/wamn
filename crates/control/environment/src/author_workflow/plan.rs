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

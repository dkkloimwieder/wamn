pub(super) fn recovery(status: &str) -> Option<Recovery> {
    match status {
        "pending-install" => Some(Recovery::Uninstall),
        "pending-upgrade" | "pending-rollback" => Some(Recovery::Rollback),
        _ => None,
    }
}

/// What clears an operation that a killed writer left pending.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// A first install never finished: no revision ever served.
    Uninstall,
    /// An upgrade or rollback never finished: the previous revision serves.
    Rollback,
}

//! Mapping [`StoreError`] onto the contract's `error` variant.
//!
//! Upstream exercises only four of the eight cases and has no precedent for
//! `access-denied`, `timeout`, `store-unavailable` or `quota-exceeded`, so
//! WAMN decides this mapping. The choices below are deliberate, because an
//! error variant is what a guest branches on:
//!
//! * A **confinement breach** is `access-denied`, not `other`. The component
//!   genuinely does not have access to the location it named — that is the
//!   variant's exact meaning, and reporting it as `other` would hide a
//!   containment refusal inside a catch-all a guest cannot branch on.
//! * **Over the ceiling** is `quota-exceeded`: a storage limit was exceeded,
//!   which is what the variant says.
//! * A body with no established end is `other`, carrying the
//!   reason. No named case means "I will not commit what I cannot verify",
//!   and inventing a closer-sounding one would misreport it.
//! * **A refused verb** is `other`, naming the verb and why. It is NOT
//!   `access-denied`: nothing about this component's access is at issue — WAMN
//!   does not implement the verb in that shape for anyone.
//! * **A lost response body** is `other`, saying the object was found and
//!   telling the guest to re-read. It is NOT `no-such-object`, which says the
//!   object is gone when the store just found it, and it is NOT
//!   `timeout`, which names a deadline that never elapsed (`wamn-b2m6.3`).

use super::bindings::wasmcloud::blobstore::types::Error as WitError;
use wamn_blobstore::intake::IntakeError;
use wamn_blobstore::store::{BackendError, BackendErrorType, StoreError};

/// Translate a store failure into the contract's error variant.
///
/// This is the ONE translation point; nothing else constructs a [`WitError`].
#[must_use]
pub fn to_wit(error: &StoreError) -> WitError {
    match error {
        StoreError::Deadline => WitError::Timeout,
        StoreError::Confinement(_) => WitError::AccessDenied,
        StoreError::NoSuchObject => WitError::NoSuchObject,
        StoreError::Intake(intake) => match intake {
            IntakeError::TooLarge { .. } => WitError::QuotaExceeded,
            IntakeError::Incomplete { .. } => WitError::Other(intake.to_string()),
        },
        StoreError::Refused { verb, reason } => {
            WitError::Other(format!("{verb} is refused by this platform: {reason}"))
        }
        StoreError::Backend(backend) => backend_to_wit(backend),
        StoreError::ResponseLost(_) => WitError::Other(error.to_string()),
    }
}

fn backend_to_wit(error: &BackendError) -> WitError {
    match error.type_ {
        BackendErrorType::NotFound => WitError::NoSuchObject,
        BackendErrorType::AccessDenied => WitError::AccessDenied,
        BackendErrorType::AlreadyExists => WitError::ContainerAlreadyExists,
        BackendErrorType::Timeout => WitError::Timeout,
        BackendErrorType::Other => WitError::Other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_blobstore::confinement::KeyRefusal;
    use wamn_blobstore::intake::MAX_OBJECT_BYTES;

    /// A containment refusal must be visible to the guest AS a containment
    /// refusal. Collapsing it into `other` would bury the one refusal a
    /// well-behaved guest most needs to see.
    #[test]
    fn a_confinement_breach_is_access_denied() {
        for refusal in [
            KeyRefusal::Absolute,
            KeyRefusal::ParentTraversal,
            KeyRefusal::Empty,
        ] {
            assert!(matches!(
                to_wit(&StoreError::Confinement(refusal)),
                WitError::AccessDenied
            ));
        }
    }

    #[test]
    fn over_the_ceiling_is_quota_exceeded() {
        assert!(matches!(
            to_wit(&StoreError::Intake(IntakeError::TooLarge {
                limit: MAX_OBJECT_BYTES,
                observed: MAX_OBJECT_BYTES + 1,
            })),
            WitError::QuotaExceeded
        ));
    }

    /// An incomplete body returns the contract's catch-all variant.
    #[test]
    fn an_incomplete_body_is_other() {
        let error = to_wit(&StoreError::Intake(IntakeError::Incomplete {
            observed: 12,
            detail: "peer reset".to_owned(),
        }));
        assert!(matches!(error, WitError::Other(_)));
    }

    /// A refused verb is not an access decision about this component.
    #[test]
    fn a_refused_verb_is_other() {
        let error = to_wit(&StoreError::Refused {
            verb: "copy-object",
            reason: "object ids carry no binding discriminator",
        });
        assert!(matches!(error, WitError::Other(_)));
        assert!(
            !matches!(
                to_wit(&StoreError::Refused {
                    verb: "move-object",
                    reason: "x"
                }),
                WitError::AccessDenied
            ),
            "a refused verb must not masquerade as an access decision"
        );
    }

    /// The store found the object and then stopped sending it. The
    /// guest must be told to read again, and must not be told the object is
    /// missing.
    #[test]
    fn a_lost_body_is_other() {
        let error = to_wit(&StoreError::ResponseLost("stream closed".into()));
        assert!(matches!(error, WitError::Other(_)));
    }

    #[test]
    fn a_missing_object_is_no_such_object_from_either_route() {
        assert!(matches!(
            to_wit(&StoreError::NoSuchObject),
            WitError::NoSuchObject
        ));
        assert!(matches!(
            to_wit(&StoreError::Backend(BackendError::new(
                BackendErrorType::NotFound,
                "gone"
            ))),
            WitError::NoSuchObject
        ));
    }

    #[test]
    fn a_denied_backend_is_access_denied() {
        assert!(matches!(
            to_wit(&StoreError::Backend(BackendError::new(
                BackendErrorType::AccessDenied,
                "no"
            ))),
            WitError::AccessDenied
        ));
    }
}

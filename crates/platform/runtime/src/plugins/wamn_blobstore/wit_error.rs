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
//! * **A body that cannot be proven complete** is `other`, carrying the
//!   reason. No named case means "I will not commit what I cannot verify",
//!   and inventing a closer-sounding one would misreport it.
//! * **A refused verb** is `other`, naming the verb and why. It is NOT
//!   `access-denied`: nothing about this component's access is at issue — WAMN
//!   does not implement the verb in that shape for anyone.

use super::bindings::wasmcloud::blobstore::types::Error as WitError;
use super::store::StoreError;

/// Translate a store failure into the contract's error variant.
///
/// This is the ONE translation point; nothing else constructs a [`WitError`].
#[must_use]
pub fn to_wit(error: &StoreError) -> WitError {
    match error {
        StoreError::Confinement(_) => WitError::AccessDenied,
        StoreError::NoSuchObject => WitError::NoSuchObject,
        StoreError::Intake(intake) => match intake {
            super::intake::IntakeError::TooLarge { .. } => WitError::QuotaExceeded,
            super::intake::IntakeError::Incomplete { .. } => WitError::Other(intake.to_string()),
        },
        StoreError::Refused { verb, reason } => {
            WitError::Other(format!("{verb} is refused by this platform: {reason}"))
        }
        StoreError::Backend(backend) => backend_to_wit(backend),
    }
}

fn backend_to_wit(error: &object_store::Error) -> WitError {
    match error {
        object_store::Error::NotFound { .. } => WitError::NoSuchObject,
        object_store::Error::PermissionDenied { .. }
        | object_store::Error::Unauthenticated { .. } => WitError::AccessDenied,
        object_store::Error::AlreadyExists { .. } => WitError::ContainerAlreadyExists,
        // A backend that says "retry me" is `store-unavailable`, which is the
        // variant documented as "may succeed if retried later" — the guest can
        // act on that, where `other` would leave it guessing.
        object_store::Error::Generic { source, .. } if is_timeout(source.as_ref()) => {
            WitError::Timeout
        }
        other => WitError::Other(other.to_string()),
    }
}

/// Whether a boxed backend error reads as a timeout.
///
/// `object_store` funnels transport failures through `Generic`, so the only
/// discriminator available is the rendered message. Matching text is
/// unattractive; reporting every transport failure as `other` is worse,
/// because `timeout` is the one a guest can sensibly retry.
fn is_timeout(source: &(dyn std::error::Error + 'static)) -> bool {
    let rendered = source.to_string().to_ascii_lowercase();
    rendered.contains("timed out") || rendered.contains("timeout")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::wamn_blobstore::confinement::KeyRefusal;
    use crate::plugins::wamn_blobstore::intake::{IntakeError, MAX_OBJECT_BYTES};

    /// A containment refusal must be visible to the guest AS a containment
    /// refusal. Collapsing it into `other` would bury the one refusal a
    /// well-behaved guest most needs to see.
    #[test]
    fn a_confinement_breach_is_access_denied() {
        for refusal in [
            KeyRefusal::Absolute,
            KeyRefusal::ParentTraversal,
            KeyRefusal::EscapedPrefix,
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

    /// An unprovable body carries its reason across the boundary — including
    /// that nothing was written, which is the fact the guest must act on.
    #[test]
    fn an_incomplete_body_is_other_and_says_nothing_was_written() {
        let error = to_wit(&StoreError::Intake(IntakeError::Incomplete {
            observed: 12,
            detail: "peer reset".to_owned(),
        }));
        match error {
            WitError::Other(message) => {
                assert!(message.contains("nothing was written"), "{message}");
                assert!(message.contains("peer reset"), "{message}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// A refused verb is not an access decision about this component.
    #[test]
    fn a_refused_verb_is_other_and_names_the_verb() {
        let error = to_wit(&StoreError::Refused {
            verb: "copy-object",
            reason: "object ids carry no binding discriminator",
        });
        match error {
            WitError::Other(message) => {
                assert!(message.contains("copy-object"), "{message}");
                assert!(message.contains("binding discriminator"), "{message}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
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

    #[test]
    fn a_missing_object_is_no_such_object_from_either_route() {
        assert!(matches!(
            to_wit(&StoreError::NoSuchObject),
            WitError::NoSuchObject
        ));
        assert!(matches!(
            to_wit(&StoreError::Backend(object_store::Error::NotFound {
                path: "p".to_owned(),
                source: "gone".into(),
            })),
            WitError::NoSuchObject
        ));
    }

    #[test]
    fn a_denied_backend_is_access_denied() {
        assert!(matches!(
            to_wit(&StoreError::Backend(object_store::Error::PermissionDenied {
                path: "p".to_owned(),
                source: "no".into(),
            })),
            WitError::AccessDenied
        ));
    }
}

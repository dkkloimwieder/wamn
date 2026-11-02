//! Shared fixture and its local checks.

include!("model_fixture.rs");
#[cfg(test)]
mod tests {
    use super::{Expected, Step, apply, model, receipt};

    #[test]
    fn replay_preserves_the_result_before_completion_and_later_updates() {
        let mut model = model([1, 1]);
        apply(
            &mut model,
            Step::Receive {
                key: 0,
                line: 0,
                quantity: 1,
            },
        );
        apply(
            &mut model,
            Step::Receive {
                key: 1,
                line: 1,
                quantity: 1,
            },
        );
        apply(&mut model, Step::Update { stale: false });
        let completed = model.clone();
        assert_eq!(model.status(), "complete");
        assert_eq!(model.revision, 4);
        assert_eq!(
            apply(&mut model, Step::Replay { key: 0 }),
            Expected::Committed {
                status: "open",
                revision: 2,
                replay: true
            },
        );
        assert_eq!(model, completed);
    }

    #[test]
    fn refused_attempt_does_not_claim_the_key_or_change_business_state() {
        let mut model = model([2, 1]);
        let empty = model.clone();
        assert_eq!(
            apply(
                &mut model,
                Step::Receive {
                    key: 0,
                    line: 0,
                    quantity: 3
                }
            ),
            Expected::Refused("quantity_exceeds_remaining"),
        );
        assert_eq!(model, empty);
        assert_eq!(
            apply(
                &mut model,
                Step::Receive {
                    key: 0,
                    line: 0,
                    quantity: 1
                }
            ),
            Expected::Committed {
                status: "open",
                revision: 2,
                replay: false
            },
        );
        let committed = model.clone();
        assert_eq!(
            apply(
                &mut model,
                Step::Receive {
                    key: 0,
                    line: 1,
                    quantity: 1
                }
            ),
            Expected::Refused("idempotency_conflict"),
        );
        assert_eq!(
            apply(
                &mut model,
                Step::Receive {
                    key: 0,
                    line: 0,
                    quantity: 0
                }
            ),
            Expected::Refused("idempotency_conflict"),
        );
        assert_eq!(
            apply(
                &mut model,
                Step::Receive {
                    key: 1,
                    line: 0,
                    quantity: 0
                }
            ),
            Expected::Refused("invalid_input"),
        );
        assert_eq!(model, committed);
        assert_eq!(model.receipt_count(), 1);
    }

    #[test]
    fn stale_updates_and_replays_do_not_advance_the_revision() {
        let mut model = model([2, 1]);
        apply(&mut model, Step::Update { stale: false });
        assert_eq!(
            apply(&mut model, Step::Update { stale: true }),
            Expected::Refused("concurrency_conflict"),
        );
        let step = Step::Replay { key: 3 };
        assert_eq!(receipt(&model, step).expect("replay body").quantity, 1);
        assert_eq!(
            apply(&mut model, step),
            Expected::Committed {
                status: "open",
                revision: 3,
                replay: false
            },
        );
        apply(&mut model, step);
        assert_eq!(model.revision, 3);
        assert_eq!(model.received, [1, 0]);
        assert_eq!(model.receipt_count(), 1);
    }
}

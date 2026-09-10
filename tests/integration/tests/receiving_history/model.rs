//! Independent business expectations for bounded Receiving command histories.

use std::collections::BTreeMap;

use proptest::collection;
use proptest::prop_oneof;
use proptest::strategy::Strategy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct History {
    pub(super) ordered: [u16; 2],
    pub(super) steps: Vec<Step>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum Step {
    Receive { key: u8, line: usize, quantity: u16 },
    Replay { key: u8 },
    Update { stale: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Receipt {
    pub(super) key: u8,
    pub(super) line: usize,
    pub(super) quantity: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Expected {
    Committed {
        status: &'static str,
        revision: i64,
        replay: bool,
    },
    Refused(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Model {
    ordered: [u16; 2],
    pub(super) received: [u16; 2],
    pub(super) revision: i64,
    committed: BTreeMap<u8, (Receipt, &'static str, i64)>,
}

pub(super) fn model(ordered: [u16; 2]) -> Model {
    assert!(ordered.into_iter().all(|quantity| quantity > 0));
    Model {
        ordered,
        received: [0; 2],
        revision: 1,
        committed: BTreeMap::new(),
    }
}

impl Model {
    pub(super) fn status(&self) -> &'static str {
        if self.received == self.ordered {
            "complete"
        } else {
            "open"
        }
    }

    pub(super) fn receipt_count(&self) -> usize {
        self.committed.len()
    }
}

/// Resolve replay steps to their original body, or a fresh unit receipt.
pub(super) fn receipt(model: &Model, step: Step) -> Option<Receipt> {
    match step {
        Step::Receive {
            key,
            line,
            quantity,
        } => Some(Receipt {
            key,
            line,
            quantity,
        }),
        Step::Replay { key } => Some(model.committed.get(&key).map_or(
            Receipt {
                key,
                line: 0,
                quantity: 1,
            },
            |(receipt, _, _)| *receipt,
        )),
        Step::Update { .. } => None,
    }
}

/// Apply one confirmed command result to the independent business state.
pub(super) fn apply(model: &mut Model, step: Step) -> Expected {
    let Some(receipt) = receipt(model, step) else {
        if let Step::Update { stale: true } = step {
            return Expected::Refused("concurrency_conflict");
        }
        model.revision += 1;
        return Expected::Committed {
            status: model.status(),
            revision: model.revision,
            replay: false,
        };
    };
    assert!(
        receipt.line < model.ordered.len(),
        "history names an absent line"
    );
    if receipt.quantity == 0 {
        return Expected::Refused("invalid_input");
    }
    if let Some((original, status, revision)) = model.committed.get(&receipt.key) {
        return if receipt == *original {
            Expected::Committed {
                status,
                revision: *revision,
                replay: true,
            }
        } else {
            Expected::Refused("idempotency_conflict")
        };
    }
    if model.status() != "open" {
        return Expected::Refused("purchase_order_not_open");
    }
    if receipt.quantity > model.ordered[receipt.line] - model.received[receipt.line] {
        return Expected::Refused("quantity_exceeds_remaining");
    }
    model.received[receipt.line] += receipt.quantity;
    model.revision += 1;
    let status = model.status();
    model
        .committed
        .insert(receipt.key, (receipt, status, model.revision));
    Expected::Committed {
        status,
        revision: model.revision,
        replay: false,
    }
}

pub(super) fn histories() -> impl Strategy<Value = History> {
    // Small keys force collisions; quantities cover zero, exact fits, and excess.
    // Every history retains at least one command while the vector shrinks.
    let step = prop_oneof![
        5 => (0u8..4, 0usize..2, 0u16..=16)
            .prop_map(|(key, line, quantity)| Step::Receive { key, line, quantity }),
        2 => (0u8..4).prop_map(|key| Step::Replay { key }),
        2 => proptest::bool::ANY.prop_map(|stale| Step::Update { stale }),
    ];
    (1u16..=12, 1u16..=12, collection::vec(step, 1..=12)).prop_map(|(first, second, steps)| {
        History {
            ordered: [first, second],
            steps,
        }
    })
}

pub(super) fn examples() -> Vec<History> {
    use Step::{Receive, Replay, Update};

    vec![
        History {
            ordered: [3, 2],
            steps: vec![
                Receive {
                    key: 0,
                    line: 0,
                    quantity: 3,
                },
                Receive {
                    key: 1,
                    line: 0,
                    quantity: 1,
                },
                Receive {
                    key: 2,
                    line: 1,
                    quantity: 2,
                },
                Receive {
                    key: 3,
                    line: 1,
                    quantity: 1,
                },
                Replay { key: 0 },
                Update { stale: false },
                Replay { key: 2 },
                Update { stale: true },
                Receive {
                    key: 0,
                    line: 1,
                    quantity: 1,
                },
            ],
        },
        History {
            ordered: [3, 3],
            steps: vec![
                Receive {
                    key: 0,
                    line: 0,
                    quantity: 4,
                },
                Receive {
                    key: 0,
                    line: 0,
                    quantity: 2,
                },
                Receive {
                    key: 0,
                    line: 0,
                    quantity: 0,
                },
                Replay { key: 0 },
                Update { stale: false },
                Receive {
                    key: 1,
                    line: 0,
                    quantity: 1,
                },
                Receive {
                    key: 2,
                    line: 1,
                    quantity: 3,
                },
                Update { stale: true },
            ],
        },
        History {
            ordered: [1, 1],
            steps: vec![
                Replay { key: 0 },
                Replay { key: 0 },
                Replay { key: 1 },
                Receive {
                    key: 1,
                    line: 1,
                    quantity: 1,
                },
                Replay { key: 1 },
            ],
        },
    ]
}

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

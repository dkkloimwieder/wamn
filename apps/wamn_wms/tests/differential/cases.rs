//! Shrinkable histories use the executable model for admission to its finite bounds.

use super::oracle;
use oracle::{Action, Command, Disposition, Inventory, Lifecycle, Packaging, PackagingType, State};
use proptest::prelude::{Strategy, any};
use proptest::{collection, option, prop_oneof};

#[derive(Clone, Debug)]
pub(super) struct History {
    pub initial: State,
    pub commands: Vec<Command>,
}

fn packaging(id: bool) -> impl Strategy<Value = Packaging> {
    (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(move |(is_tote, location_id, closed)| {
        Packaging {
            id,
            r#type: if is_tote {
                PackagingType::Tote
            } else {
                PackagingType::Pallet
            },
            code: id,
            location_id,
            lifecycle: if closed {
                Lifecycle::Closed
            } else {
                Lifecycle::Open
            },
        }
    })
}

fn inventory(id: bool) -> impl Strategy<Value = Option<Inventory>> {
    option::of(
        (any::<bool>(), any::<bool>(), 0_u8..=6, any::<bool>()).prop_map(
            move |(packaging_id, location_id, quantity, held)| Inventory {
                id,
                product_id: false,
                packaging_id,
                location_id,
                quantity,
                disposition: if held {
                    Disposition::Held
                } else {
                    Disposition::Available
                },
                lifecycle: if quantity == 0 {
                    Lifecycle::Closed
                } else {
                    Lifecycle::Open
                },
            },
        ),
    )
}

fn state() -> impl Strategy<Value = State> {
    (
        inventory(false),
        inventory(true),
        packaging(false),
        packaging(true),
        any::<bool>(),
    )
        .prop_map(|(mut first, mut second, left, right, product_id)| {
            for item in [&mut first, &mut second].into_iter().flatten() {
                item.product_id = product_id;
            }
            oracle::initial([first, second], [left, right])
        })
        .prop_filter(
            "the executable model admits the initial business state",
            |state| oracle::valid(*state),
        )
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
            |(inventory_id, to_packaging_id, to_location_id)| Action::Move {
                inventory_id,
                to_packaging_id,
                to_location_id
            }
        ),
        (any::<bool>(), 0_u8..=6).prop_map(|(inventory_id, to_quantity)| Action::Adjust {
            inventory_id,
            to_quantity
        }),
        (any::<bool>(), 0_u8..=7, any::<bool>(), any::<bool>()).prop_map(
            |(from_inventory_id, quantity, to_packaging_id, to_location_id)| Action::Split {
                from_inventory_id,
                quantity,
                to_packaging_id,
                to_location_id
            }
        ),
        (any::<bool>(), any::<bool>()).prop_map(|(from_inventory_id, to_inventory_id)| {
            Action::Merge {
                from_inventory_id,
                to_inventory_id,
            }
        }),
        (any::<bool>(), any::<bool>()).prop_map(|(packaging_id, to_location_id)| {
            Action::RelocatePackaging {
                packaging_id,
                to_location_id,
            }
        }),
        any::<bool>().prop_map(|packaging_id| Action::ClosePackaging { packaging_id }),
    ]
}

fn command() -> impl Strategy<Value = Command> {
    (
        any::<bool>(),
        action(),
        any::<bool>(),
        option::of(any::<bool>()),
    )
        .prop_map(|(key, action, occurred_at, reason)| Command {
            key,
            action,
            occurred_at: !matches!(action, Action::ClosePackaging { .. }) && occurred_at,
            reason: if matches!(action, Action::Adjust { .. }) {
                reason
            } else {
                None
            },
        })
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Command(Command),
    Replay(bool),
    ChangedIntent(bool),
}

fn admitted(initial: State, steps: Vec<Step>) -> History {
    let mut state = initial;
    let mut commands = Vec::new();
    for step in steps {
        let command = match step {
            Step::Command(command) => command,
            Step::Replay(key) | Step::ChangedIntent(key) => {
                let Some(operation) = state
                    .operations
                    .iter()
                    .flatten()
                    .find(|op| op.command.key == key)
                else {
                    continue;
                };
                let mut command = operation.command;
                if matches!(step, Step::ChangedIntent(_)) {
                    if let Action::ClosePackaging { packaging_id } = &mut command.action {
                        *packaging_id = !*packaging_id;
                    } else {
                        command.occurred_at = !command.occurred_at;
                    }
                }
                command
            }
        };
        let claim = state
            .operations
            .iter()
            .flatten()
            .find(|op| op.command.key == command.key);
        if let Some(claim) = claim {
            // Production uses one claim namespace per operation. A small model key
            // therefore stays bound to its first accepted command type.
            if std::mem::discriminant(&claim.command.action)
                != std::mem::discriminant(&command.action)
            {
                continue;
            }
        } else if !oracle::command_domain(state, command) {
            // Only model capacity excludes commands: total quantity and identity slots.
            // Existing claims cannot mutate state, so they need no capacity restriction.
            continue;
        }
        oracle::execute(&mut state, command);
        commands.push(command);
    }
    History { initial, commands }
}

pub(super) fn strategy() -> impl Strategy<Value = History> {
    let step = prop_oneof![6 => command().prop_map(Step::Command), 2 => any::<bool>().prop_map(Step::Replay), 2 => any::<bool>().prop_map(Step::ChangedIntent)];
    (state(), collection::vec(step, 1..=10))
        .prop_map(|(initial, steps)| admitted(initial, steps))
        .prop_filter("at least one command fits the model bounds", |history| {
            !history.commands.is_empty()
        })
}

fn fixture() -> State {
    oracle::initial(
        [
            Some(Inventory {
                id: false,
                product_id: false,
                packaging_id: false,
                location_id: false,
                quantity: 3,
                disposition: Disposition::Held,
                lifecycle: Lifecycle::Open,
            }),
            None,
        ],
        [
            Packaging {
                id: false,
                r#type: PackagingType::Pallet,
                code: false,
                location_id: false,
                lifecycle: Lifecycle::Open,
            },
            Packaging {
                id: true,
                r#type: PackagingType::Tote,
                code: true,
                location_id: true,
                lifecycle: Lifecycle::Open,
            },
        ],
    )
}

fn cmd(key: bool, action: Action) -> Command {
    Command {
        key,
        action,
        occurred_at: false,
        reason: matches!(action, Action::Adjust { .. }).then_some(false),
    }
}

/// Fixed examples supplement generated histories with repeatable business paths.
pub(super) fn examples() -> Vec<History> {
    let initial = fixture();
    let split = cmd(
        false,
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: true,
        },
    );
    let merge = cmd(
        true,
        Action::Merge {
            from_inventory_id: false,
            to_inventory_id: true,
        },
    );
    let movement = cmd(
        false,
        Action::Move {
            inventory_id: false,
            to_packaging_id: true,
            to_location_id: true,
        },
    );
    let adjust = cmd(
        true,
        Action::Adjust {
            inventory_id: false,
            to_quantity: 4,
        },
    );
    let relocate = cmd(
        false,
        Action::RelocatePackaging {
            packaging_id: false,
            to_location_id: true,
        },
    );
    let close = cmd(false, Action::ClosePackaging { packaging_id: true });
    let mut mixed = initial;
    mixed.inventory[1] = Some(Inventory {
        id: true,
        quantity: 2,
        disposition: Disposition::Available,
        ..initial.inventory[0].unwrap()
    });
    let mut historical = initial;
    historical.inventory[0].as_mut().unwrap().quantity = 0;
    historical.inventory[0].as_mut().unwrap().lifecycle = Lifecycle::Closed;
    let mut closed = initial;
    closed.packaging[1].lifecycle = Lifecycle::Closed;
    let mut histories = vec![
        History {
            initial,
            commands: vec![split, merge, split],
        },
        History {
            initial,
            commands: vec![
                movement,
                adjust,
                movement,
                Command {
                    occurred_at: true,
                    ..movement
                },
            ],
        },
        History {
            initial: mixed,
            commands: vec![
                cmd(
                    false,
                    Action::Merge {
                        from_inventory_id: false,
                        to_inventory_id: true,
                    },
                ),
                relocate,
                cmd(
                    true,
                    Action::RelocatePackaging {
                        packaging_id: false,
                        to_location_id: false,
                    },
                ),
                relocate,
            ],
        },
        History {
            initial: historical,
            commands: vec![
                relocate,
                cmd(
                    true,
                    Action::ClosePackaging {
                        packaging_id: false,
                    },
                ),
                relocate,
            ],
        },
        History {
            initial,
            commands: vec![
                close,
                close,
                cmd(true, Action::ClosePackaging { packaging_id: true }),
            ],
        },
        History {
            initial: closed,
            commands: vec![
                cmd(
                    false,
                    Action::RelocatePackaging {
                        packaging_id: true,
                        to_location_id: false,
                    },
                ),
                cmd(
                    false,
                    Action::Move {
                        inventory_id: false,
                        to_packaging_id: true,
                        to_location_id: true,
                    },
                ),
                cmd(
                    false,
                    Action::Split {
                        from_inventory_id: false,
                        quantity: 1,
                        to_packaging_id: true,
                        to_location_id: true,
                    },
                ),
                close,
            ],
        },
        History {
            initial,
            commands: vec![
                cmd(
                    false,
                    Action::ClosePackaging {
                        packaging_id: false,
                    },
                ),
                cmd(
                    false,
                    Action::RelocatePackaging {
                        packaging_id: false,
                        to_location_id: false,
                    },
                ),
                cmd(
                    false,
                    Action::Split {
                        from_inventory_id: false,
                        quantity: 0,
                        to_packaging_id: true,
                        to_location_id: true,
                    },
                ),
                cmd(
                    false,
                    Action::Split {
                        from_inventory_id: false,
                        quantity: 3,
                        to_packaging_id: true,
                        to_location_id: true,
                    },
                ),
                cmd(
                    false,
                    Action::Move {
                        inventory_id: false,
                        to_packaging_id: true,
                        to_location_id: false,
                    },
                ),
                cmd(
                    false,
                    Action::Adjust {
                        inventory_id: false,
                        to_quantity: 0,
                    },
                ),
            ],
        },
    ];
    for action in [
        Action::Move {
            inventory_id: false,
            to_packaging_id: true,
            to_location_id: true,
        },
        Action::Adjust {
            inventory_id: false,
            to_quantity: 1,
        },
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: true,
        },
        Action::Merge {
            from_inventory_id: false,
            to_inventory_id: true,
        },
    ] {
        histories.push(History {
            initial: historical,
            commands: vec![cmd(false, action)],
        });
    }
    // Keep this accepted-then-fresh-close case visible. The old model accepts
    // closing closed packaging, while production refuses it.
    histories
}

#[test]
fn generated_histories_fit_model_capacity() {
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&strategy(), |history| {
            let mut state = history.initial;
            proptest::prop_assert!(oracle::valid(state));
            for command in history.commands {
                let claimed = state
                    .operations
                    .iter()
                    .flatten()
                    .any(|operation| operation.command.key == command.key);
                proptest::prop_assert!(claimed || oracle::command_domain(state, command));
                oracle::execute(&mut state, command);
                proptest::prop_assert!(oracle::valid(state));
            }
            Ok(())
        })
        .expect("generated histories stay inside the executable model bounds");
    for history in examples() {
        let mut state = history.initial;
        assert!(oracle::valid(state));
        for command in history.commands {
            oracle::execute(&mut state, command);
            assert!(oracle::valid(state));
        }
    }
}

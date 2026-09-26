//! Local transition obligations and two-operation histories.
use super::*;

fn arbitrary_state() -> State {
    let state = kani::any();
    kani::assume(valid(state));
    state
}
fn arbitrary_command(state: State) -> Command {
    let command = kani::any();
    kani::assume(command_domain(state, command));
    command
}

#[kani::proof]
#[kani::unwind(4)]
fn fresh_close_checks_lifecycle_before_contents() {
    let mut state = initial(kani::any(), kani::any());
    let packaging_id: bool = kani::any();
    state.packaging[usize::from(packaging_id)].lifecycle = Lifecycle::Closed;
    let from = state;
    // Arbitrary contents include inconsistent open stock to distinguish refusal precedence.
    let close = command(false, Action::ClosePackaging { packaging_id });
    assert_eq!(
        execute(&mut state, close),
        Outcome::Refused(Refusal::ClosedPackaging)
    );
    assert_eq!(state, from);
    kani::cover!(empty(from.inventory, packaging_id), "closed and empty");
    kani::cover!(
        !empty(from.inventory, packaging_id),
        "closed with open inventory"
    );
}

// Read rows by their explicit inventory identity and reconstruct the result.
fn explains(from: State, operation: Operation) -> bool {
    let mut reconstructed = from.inventory;
    let mut seen = [false; 2];
    let expected_type = match operation.command.action {
        Action::Move { .. } => Some(Type::Move),
        Action::Adjust { .. } => Some(Type::Adjust),
        Action::Split { .. } => Some(Type::Split),
        Action::Merge { .. } => Some(Type::Merge),
        Action::RelocatePackaging { .. } => Some(Type::RelocatePackaging),
        Action::ClosePackaging { .. } => None,
    };
    for row in operation.transactions.into_iter().flatten() {
        let id = row.inventory_id;
        let index = usize::from(id);
        if seen[index] {
            return false;
        }
        seen[index] = true;
        let item = from.inventory[index];
        let source = match operation.command.action {
            Action::Split {
                from_inventory_id, ..
            }
            | Action::Merge {
                from_inventory_id, ..
            } => from_inventory_id,
            _ => id,
        };
        let target = match operation.command.action {
            Action::Merge {
                to_inventory_id, ..
            } => to_inventory_id,
            _ => id,
        };
        if row.id != u8::from(operation.result.operation_id) * 2 + u8::from(id)
            || row.operation_id != operation.result.operation_id
            || Some(row.r#type) != expected_type
            || row.occurred_at != operation.command.occurred_at
            || row.reason != operation.command.reason
            || row.from_inventory_id != source
            || row.to_inventory_id != target
            || row.from_product_id != item.map(|x| x.product_id)
            || row.from_packaging_id != item.map(|x| x.packaging_id)
            || row.from_location_id != item.map(|x| x.location_id)
            || row.from_quantity != item.map_or(0, |x| x.quantity)
            || row.from_disposition != item.map(|x| x.disposition)
            || row.from_lifecycle != item.map(|x| x.lifecycle)
        {
            return false;
        }
        reconstructed[index] = Some(Inventory {
            id,
            product_id: row.to_product_id,
            packaging_id: row.to_packaging_id,
            location_id: row.to_location_id,
            quantity: row.to_quantity,
            disposition: row.to_disposition,
            lifecycle: row.to_lifecycle,
        });
    }
    for id in [false, true] {
        let needed = match operation.command.action {
            Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
                id == inventory_id
            }
            Action::Split { .. } | Action::Merge { .. } => true,
            Action::RelocatePackaging { packaging_id, .. } => from.inventory[usize::from(id)]
                .is_some_and(|item| {
                    item.lifecycle == Lifecycle::Open && item.packaging_id == packaging_id
                }),
            Action::ClosePackaging { .. } => false,
        };
        if seen[usize::from(id)] != needed {
            return false;
        }
    }
    reconstructed == operation.result.inventory
}

#[kani::proof]
#[kani::unwind(4)]
fn initialization() {
    let state = initial(kani::any(), kani::any());
    kani::assume(valid_business(state));
    assert!(valid(state));
    assert!(state.operations == [None; 2]);
    kani::cover!(state.inventory[0].is_some(), "existing inventory");
}

#[kani::proof]
#[kani::unwind(4)]
fn state_and_history_preservation() {
    let from = arbitrary_state();
    let mut to = from;
    let command = arbitrary_command(from);
    let outcome = execute(&mut to, command);
    assert!(valid(to));
    for index in 0..2 {
        if from.operations[index].is_some() {
            assert!(to.operations[index] == from.operations[index]);
        }
    }
    if let Outcome::Accepted(result) = outcome {
        let index = usize::from(result.operation_id);
        assert!(from.operations[index].is_none());
        let operation = to.operations[index].unwrap();
        assert!(operation.command == command && operation.result == result);
        assert!(result.inventory == to.inventory && result.packaging == to.packaging);
        assert!(to.operations[1 - index] == from.operations[1 - index]);
    } else {
        assert!(to == from);
    }
    kani::cover!(matches!(outcome, Outcome::Accepted(_)), "new operation");
    kani::cover!(matches!(outcome, Outcome::Refused(_)), "refusal");
    kani::cover!(matches!(outcome, Outcome::Replayed(_)), "replay");
}

#[kani::proof]
#[kani::unwind(4)]
fn complete_transactions() {
    let from = arbitrary_state();
    let command = arbitrary_command(from);
    let mut to = from;
    if let Outcome::Accepted(result) = execute(&mut to, command) {
        assert!(explains(
            from,
            to.operations[usize::from(result.operation_id)].unwrap()
        ));
        kani::cover!(
            matches!(command.action, Action::Move { .. }),
            "move transaction"
        );
        kani::cover!(
            matches!(command.action, Action::Adjust { .. }),
            "adjustment transaction"
        );
        kani::cover!(
            matches!(command.action, Action::Split { .. }),
            "two split transactions"
        );
        kani::cover!(
            matches!(command.action, Action::Merge { .. }),
            "two merge transactions"
        );
        kani::cover!(
            matches!(command.action, Action::ClosePackaging { .. }),
            "packaging closure without inventory mutation"
        );
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn quantities_and_closed_inventory() {
    let from = arbitrary_state();
    let command = arbitrary_command(from);
    let mut to = from;
    let outcome = execute(&mut to, command);
    for id in [false, true] {
        let touched = match command.action {
            Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
                id == inventory_id
            }
            Action::Split {
                from_inventory_id, ..
            } => id == from_inventory_id,
            Action::Merge { .. } => true,
            Action::RelocatePackaging { .. } | Action::ClosePackaging { .. } => false,
        };
        if touched
            && from.inventory[usize::from(id)].is_some_and(|x| x.lifecycle == Lifecycle::Closed)
        {
            assert!(!matches!(outcome, Outcome::Accepted(_)));
        }
    }
    if let Outcome::Accepted(_) = outcome {
        if !matches!(command.action, Action::Adjust { .. }) {
            assert!(total(to.inventory) == total(from.inventory));
        }
        if !matches!(
            command.action,
            Action::ClosePackaging { .. } | Action::RelocatePackaging { .. }
        ) {
            assert!(to.packaging == from.packaging);
        }
        match command.action {
            Action::Move {
                inventory_id,
                to_packaging_id,
                to_location_id,
            } => {
                let mut expected = from.inventory;
                expected[usize::from(inventory_id)]
                    .as_mut()
                    .unwrap()
                    .packaging_id = to_packaging_id;
                expected[usize::from(inventory_id)]
                    .as_mut()
                    .unwrap()
                    .location_id = to_location_id;
                assert!(to.inventory == expected);
                assert!(to.packaging[usize::from(to_packaging_id)].lifecycle == Lifecycle::Open);
                let old = from.inventory[usize::from(inventory_id)].unwrap();
                kani::cover!(
                    old.location_id != to_location_id,
                    "move changes packaging and location"
                );
                kani::cover!(
                    old.packaging_id != to_packaging_id && old.location_id == to_location_id,
                    "move changes packaging at same location"
                );
            }
            Action::Adjust {
                inventory_id,
                to_quantity,
            } => {
                assert!(to_quantity > 0 && command.reason.is_some());
                let mut expected = from.inventory;
                expected[usize::from(inventory_id)]
                    .as_mut()
                    .unwrap()
                    .quantity = to_quantity;
                assert!(to.inventory == expected);
                assert!(
                    total(to.inventory)
                        == total(from.inventory)
                            - u16::from(
                                from.inventory[usize::from(inventory_id)].unwrap().quantity
                            )
                            + u16::from(to_quantity)
                );
            }
            Action::Split {
                from_inventory_id,
                quantity,
                to_packaging_id,
                to_location_id,
            } => {
                let source = from.inventory[usize::from(from_inventory_id)].unwrap();
                assert!(quantity > 0 && quantity < source.quantity);
                assert!(
                    to.inventory[usize::from(from_inventory_id)]
                        == Some(Inventory {
                            quantity: source.quantity - quantity,
                            ..source
                        })
                );
                assert!(
                    to.inventory[usize::from(!from_inventory_id)]
                        == Some(Inventory {
                            id: !from_inventory_id,
                            packaging_id: to_packaging_id,
                            location_id: to_location_id,
                            quantity,
                            ..source
                        })
                );
                assert!(to.packaging[usize::from(to_packaging_id)].lifecycle == Lifecycle::Open);
            }
            Action::Merge {
                from_inventory_id,
                to_inventory_id,
            } => {
                let source = from.inventory[usize::from(from_inventory_id)].unwrap();
                let target = from.inventory[usize::from(to_inventory_id)].unwrap();
                assert!(source.disposition == target.disposition);
                assert!(
                    to.inventory[usize::from(from_inventory_id)]
                        == Some(Inventory {
                            quantity: 0,
                            lifecycle: Lifecycle::Closed,
                            ..source
                        })
                );
                assert!(
                    to.inventory[usize::from(to_inventory_id)]
                        == Some(Inventory {
                            quantity: source.quantity + target.quantity,
                            ..target
                        })
                );
            }
            Action::RelocatePackaging {
                packaging_id,
                to_location_id,
            } => {
                assert!(from.packaging[usize::from(packaging_id)].lifecycle == Lifecycle::Open);
                assert!(from.packaging[usize::from(packaging_id)].location_id != to_location_id);
                let mut expected_packaging = from.packaging;
                expected_packaging[usize::from(packaging_id)].location_id = to_location_id;
                assert!(to.packaging == expected_packaging);
                for id in [false, true] {
                    let expected = from.inventory[usize::from(id)].map(|mut item| {
                        if item.lifecycle == Lifecycle::Open && item.packaging_id == packaging_id {
                            item.location_id = to_location_id;
                        }
                        item
                    });
                    assert!(to.inventory[usize::from(id)] == expected);
                }
            }
            Action::ClosePackaging { packaging_id } => {
                assert!(empty(from.inventory, packaging_id));
                assert!(to.inventory == from.inventory);
                let mut expected = from.packaging;
                expected[usize::from(packaging_id)].lifecycle = Lifecycle::Closed;
                assert!(to.packaging == expected);
            }
        }
    }
    kani::cover!(
        matches!(outcome, Outcome::Refused(Refusal::ClosedInventory)),
        "closed inventory refuses"
    );
    kani::cover!(
        matches!(outcome, Outcome::Refused(Refusal::ClosedPackaging)),
        "closed packaging refuses receipt"
    );
    kani::cover!(
        matches!(outcome, Outcome::Refused(Refusal::PackagingNotEmpty)),
        "occupied packaging refuses closure"
    );
    kani::cover!(
        matches!(outcome, Outcome::Refused(Refusal::DispositionMismatch)),
        "different dispositions refuse merge"
    );
    kani::cover!(
        total(to.inventory) > total(from.inventory),
        "positive adjustment"
    );
    kani::cover!(
        total(to.inventory) < total(from.inventory),
        "negative adjustment delta"
    );
}

#[kani::proof]
#[kani::unwind(4)]
fn original_result_and_changed_intent() {
    let from = arbitrary_state();
    let index: bool = kani::any();
    kani::assume(from.operations[usize::from(index)].is_some());
    let operation = from.operations[usize::from(index)].unwrap();
    let mut to = from;
    assert!(execute(&mut to, operation.command) == Outcome::Replayed(operation.result));
    assert!(to == from);
    let changed: Command = kani::any();
    kani::assume(changed.key == operation.command.key && changed != operation.command);
    let outcome = execute(&mut to, changed);
    assert!(
        outcome
            == Outcome::Refused(if prepared(changed) {
                Refusal::IntentConflict
            } else {
                Refusal::InvalidInput
            })
    );
    assert!(to == from);
    kani::cover!(
        from.operations[1].is_some(),
        "replay with two stored operations"
    );
}

fn fixture(quantity: u8, disposition: Disposition) -> State {
    initial(
        [
            Some(Inventory {
                id: false,
                product_id: false,
                packaging_id: false,
                location_id: false,
                quantity,
                disposition,
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
fn command(key: bool, action: Action) -> Command {
    Command {
        key,
        action,
        occurred_at: key,
        reason: None,
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn split_history_is_complete() {
    let quantity: u8 = kani::any();
    kani::assume((2..=3).contains(&quantity));
    let from = fixture(quantity, Disposition::Available);
    let mut to = from;
    assert!(matches!(
        execute(
            &mut to,
            command(
                false,
                Action::Split {
                    from_inventory_id: false,
                    quantity: 1,
                    to_packaging_id: true,
                    to_location_id: true
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    let operation = to.operations[0].unwrap();
    assert!(
        operation.transactions[1].is_some(),
        "created inventory lacks its transaction"
    );
    assert!(explains(from, operation));
}

#[kani::proof]
#[kani::unwind(4)]
fn later_merge_cannot_change_split_history_or_replay() {
    let mut state = fixture(3, kani::any());
    let split = command(
        false,
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: true,
        },
    );
    let Outcome::Accepted(result) = execute(&mut state, split) else {
        panic!("split refused")
    };
    let operation = state.operations[0];
    assert!(matches!(
        execute(
            &mut state,
            command(
                true,
                Action::Merge {
                    from_inventory_id: false,
                    to_inventory_id: true
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    assert!(state.inventory[0].unwrap().lifecycle == Lifecycle::Closed);
    assert!(state.operations[0] == operation);
    let from = state;
    assert!(execute(&mut state, split) == Outcome::Replayed(result));
    assert!(state == from && valid(state));
    assert!(result.inventory[0].unwrap().lifecycle == Lifecycle::Open);
}

#[kani::proof]
#[kani::unwind(4)]
fn packaging_closure_preserves_history_and_replay() {
    let mut state = fixture(3, kani::any());
    let movement = command(
        false,
        Action::Move {
            inventory_id: false,
            to_packaging_id: true,
            to_location_id: true,
        },
    );
    let Outcome::Accepted(result) = execute(&mut state, movement) else {
        panic!("move refused")
    };
    let operation = state.operations[0];
    assert!(matches!(
        execute(
            &mut state,
            command(
                true,
                Action::ClosePackaging {
                    packaging_id: false
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    assert!(state.packaging[0].lifecycle == Lifecycle::Closed);
    assert!(state.operations[0] == operation);
    let from = state;
    assert!(execute(&mut state, movement) == Outcome::Replayed(result));
    assert!(state == from && valid(state));
    assert!(result.packaging[0].lifecycle == Lifecycle::Open);
}

#[kani::proof]
#[kani::unwind(4)]
fn relocation_atomicity_and_refusals() {
    let from = arbitrary_state();
    let command = command(
        kani::any(),
        Action::RelocatePackaging {
            packaging_id: kani::any(),
            to_location_id: kani::any(),
        },
    );
    let failure: Failure = kani::any();
    let mut to = from;
    let outcome = execute_with_failure(&mut to, command, failure);
    match outcome {
        Outcome::Accepted(result) => {
            assert!(failure == Failure::None);
            assert!(valid(to));
            assert!(total(to.inventory) == total(from.inventory));
            assert!(explains(
                from,
                to.operations[usize::from(result.operation_id)].unwrap()
            ));
            for index in 0..2 {
                if from.operations[index].is_some() {
                    assert!(to.operations[index] == from.operations[index]);
                }
            }
            let rows = to.operations[usize::from(result.operation_id)]
                .unwrap()
                .transactions;
            kani::cover!(rows == [None; 2], "empty packaging relocation");
            kani::cover!(
                rows[0].is_some() && rows[1].is_some(),
                "two inventory relocation rows"
            );
            kani::cover!(
                from.inventory
                    .iter()
                    .flatten()
                    .any(|item| item.lifecycle == Lifecycle::Closed),
                "historical inventory stays put"
            );
        }
        _ => assert!(to == from),
    }
    kani::cover!(
        outcome == Outcome::Aborted && failure == Failure::BusinessState,
        "abort staged business state"
    );
    kani::cover!(
        outcome == Outcome::Aborted && failure == Failure::History,
        "abort staged history"
    );
    kani::cover!(
        outcome == Outcome::Aborted && failure == Failure::StoredResult,
        "abort staged result"
    );
    kani::cover!(
        outcome == Outcome::Refused(Refusal::NoOp),
        "fresh same-location relocation refuses"
    );
    kani::cover!(
        outcome == Outcome::Refused(Refusal::ClosedPackaging),
        "closed packaging relocation refuses"
    );
}

#[kani::proof]
#[kani::unwind(4)]
fn relocation_history_is_complete() {
    let mut from = fixture(3, Disposition::Available);
    from.inventory[1] = Some(Inventory {
        id: true,
        quantity: 2,
        disposition: Disposition::Held,
        ..from.inventory[0].unwrap()
    });
    let mut to = from;
    assert!(matches!(
        execute(
            &mut to,
            command(
                false,
                Action::RelocatePackaging {
                    packaging_id: false,
                    to_location_id: true,
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    assert!(valid(to));
    assert!(total(to.inventory) == total(from.inventory));
    let operation = to.operations[0].unwrap();
    assert!(
        operation.transactions[1].is_some(),
        "relocated inventory lacks its transaction"
    );
    assert!(explains(from, operation));
}

#[kani::proof]
#[kani::unwind(4)]
fn later_relocation_preserves_history_and_replay() {
    let mut state = fixture(3, kani::any());
    let relocation = command(
        false,
        Action::RelocatePackaging {
            packaging_id: false,
            to_location_id: true,
        },
    );
    let Outcome::Accepted(result) = execute(&mut state, relocation) else {
        panic!("relocation refused")
    };
    let operation = state.operations[0];
    let accepted = state;
    assert!(execute(&mut state, relocation) == Outcome::Replayed(result));
    assert!(state == accepted);
    assert!(
        execute(
            &mut state,
            Command {
                key: true,
                ..relocation
            }
        ) == Outcome::Refused(Refusal::NoOp)
    );
    assert!(state == accepted);
    assert!(matches!(
        execute(
            &mut state,
            command(
                true,
                Action::RelocatePackaging {
                    packaging_id: false,
                    to_location_id: false,
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    assert!(state.operations[0] == operation);
    assert!(!state.inventory[0].unwrap().location_id);
    assert!(!state.packaging[0].location_id);
    let from = state;
    assert!(execute(&mut state, relocation) == Outcome::Replayed(result));
    assert!(state == from && valid(state));
    assert!(result.inventory[0].unwrap().location_id && result.packaging[0].location_id);
}

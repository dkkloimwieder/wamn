//! Inductive local obligations over finite inventory, claims, and transaction prefixes.
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

// Independent observation of every row, including reconstruction of the full state.
fn explains(from_inventory: Inventories, operation: Operation) -> bool {
    let mut reconstructed = from_inventory;
    let expected_type = match operation.command.action {
        Action::Move { .. } => Type::Move,
        Action::Adjust { .. } => Type::Adjust,
        Action::Split { .. } => Type::Split,
        Action::Merge { .. } => Type::Merge,
    };
    for id in [false, true] {
        let index = usize::from(id);
        let needed = match operation.command.action {
            Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
                id == inventory_id
            }
            _ => true,
        };
        let row = operation.transactions[index];
        if row.is_some() != needed {
            return false;
        }
        let Some(row) = row else { continue };
        let from_item = from_inventory[index];
        let expected_from_id = match operation.command.action {
            Action::Split {
                from_inventory_id, ..
            } => from_inventory_id,
            _ => id,
        };
        let expected_to_id = match operation.command.action {
            Action::Merge {
                to_inventory_id, ..
            } => to_inventory_id,
            _ => id,
        };
        if row.id != u8::from(operation.result.operation_id) * 2 + u8::from(id)
            || row.operation_id != operation.result.operation_id
            || row.r#type != expected_type
            || row.occurred_at != operation.command.occurred_at
            || row.from_inventory_id != expected_from_id
            || row.to_inventory_id != expected_to_id
            || row.from_product_id != from_item.map(|item| item.product_id)
            || row.from_location_id != from_item.map(|item| item.location_id)
            || row.from_quantities
                != from_item.map_or(Quantities::default(), |item| item.quantities)
            || row.from_pallet_status != from_item.map(|item| item.pallet_status)
        {
            return false;
        }
        reconstructed[index] = Some(Inventory {
            id,
            product_id: row.to_product_id,
            location_id: row.to_location_id,
            quantities: row.to_quantities,
            pallet_status: row.to_pallet_status,
        });
    }
    reconstructed == operation.result.inventory
}

#[kani::proof]
#[kani::unwind(4)]
fn initialization() {
    let inventory = kani::any();
    kani::assume(valid_inventory(inventory));
    let state = initial(inventory);
    assert!(valid(state));
    assert!(state.operations == [None; 2]);
    kani::cover!(state.inventory[0].is_some(), "existing inventory");
}

#[kani::proof]
#[kani::unwind(4)]
fn state_and_history_preservation() {
    let from_state = arbitrary_state();
    let mut to_state = from_state;
    let command = arbitrary_command(from_state);
    let outcome = execute(&mut to_state, command);
    assert!(valid(to_state));
    for id in 0..2 {
        if from_state.operations[id].is_some() {
            assert!(to_state.operations[id] == from_state.operations[id]);
        }
    }
    if let Outcome::Accepted(result) = outcome {
        let index = usize::from(result.operation_id);
        assert!(from_state.operations[index].is_none());
        let operation = to_state.operations[index].unwrap();
        assert!(operation.command == command);
        assert!(operation.result == result);
        assert!(result.inventory == to_state.inventory);
        assert!(to_state.operations[1 - index] == from_state.operations[1 - index]);
    } else {
        assert!(to_state == from_state);
    }
    kani::cover!(matches!(outcome, Outcome::Accepted(_)), "new operation");
    kani::cover!(matches!(outcome, Outcome::Refused(_)), "refusal");
    kani::cover!(matches!(outcome, Outcome::Replayed(_)), "replay");
}

#[kani::proof]
#[kani::unwind(4)]
fn complete_transactions() {
    let from_state = arbitrary_state();
    let command = arbitrary_command(from_state);
    let mut to_state = from_state;
    if let Outcome::Accepted(result) = execute(&mut to_state, command) {
        let operation = to_state.operations[usize::from(result.operation_id)].unwrap();
        assert!(explains(from_state.inventory, operation));
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
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn quantities_and_closed_inventory() {
    let from_state = arbitrary_state();
    let command = arbitrary_command(from_state);
    let mut to_state = from_state;
    let outcome = execute(&mut to_state, command);
    for id in [false, true] {
        let item = from_state.inventory[usize::from(id)];
        let touched = match command.action {
            Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
                id == inventory_id
            }
            Action::Split {
                from_inventory_id, ..
            } => id == from_inventory_id,
            Action::Merge { .. } => true,
        };
        if item.is_some_and(|item| item.pallet_status == PalletStatus::Closed) && touched {
            assert!(!matches!(outcome, Outcome::Accepted(_)));
        }
    }
    if let Outcome::Accepted(_) = outcome {
        if !matches!(command.action, Action::Adjust { .. }) {
            assert!(total(to_state.inventory) == total(from_state.inventory));
            for status in [QuantityStatus::Available, QuantityStatus::Held] {
                assert!(
                    total_for_status(to_state.inventory, status)
                        == total_for_status(from_state.inventory, status)
                );
            }
        }
        match command.action {
            Action::Adjust {
                inventory_id,
                quantity_status,
                to_quantity,
                reason_present,
            } => {
                assert!(reason_present && to_quantity > 0);
                let id = usize::from(inventory_id);
                let from_item = from_state.inventory[id].unwrap();
                assert!(quantity(from_item.quantities, quantity_status) > 0);
                let mut expected = from_item;
                match quantity_status {
                    QuantityStatus::Available => expected.quantities.available = to_quantity,
                    QuantityStatus::Held => expected.quantities.held = to_quantity,
                }
                assert!(to_state.inventory[id] == Some(expected));
                assert!(to_state.inventory[1 - id] == from_state.inventory[1 - id]);
                assert!(
                    total(to_state.inventory)
                        == total(from_state.inventory)
                            - u16::from(quantity(from_item.quantities, quantity_status))
                            + u16::from(to_quantity)
                );
            }
            Action::Move {
                inventory_id,
                to_location_id,
            } => {
                let mut expected = from_state.inventory;
                expected[usize::from(inventory_id)]
                    .as_mut()
                    .unwrap()
                    .location_id = to_location_id;
                assert!(to_state.inventory == expected);
            }
            Action::Split {
                from_inventory_id,
                quantity_status,
                quantity: requested,
                to_location_id,
            } => {
                let from_item = from_state.inventory[usize::from(from_inventory_id)].unwrap();
                let retained = to_state.inventory[usize::from(from_inventory_id)].unwrap();
                let created = to_state.inventory[usize::from(!from_inventory_id)].unwrap();
                assert!(
                    requested > 0 && requested < quantity(from_item.quantities, quantity_status)
                );
                for status in [QuantityStatus::Available, QuantityStatus::Held] {
                    let transferred = if status == quantity_status {
                        requested
                    } else {
                        0
                    };
                    assert!(quantity(created.quantities, status) == transferred);
                    assert!(
                        quantity(retained.quantities, status)
                            == quantity(from_item.quantities, status) - transferred
                    );
                }
                assert!(
                    retained
                        == Inventory {
                            quantities: retained.quantities,
                            ..from_item
                        }
                );
                assert!(created.id == !from_inventory_id);
                assert!(created.product_id == from_item.product_id);
                assert!(created.location_id == to_location_id);
                assert!(created.pallet_status == from_item.pallet_status);
            }
            Action::Merge {
                from_inventory_id,
                to_inventory_id,
            } => {
                let from_id = usize::from(from_inventory_id);
                let to_id = usize::from(to_inventory_id);
                let source = from_state.inventory[from_id].unwrap();
                let target = from_state.inventory[to_id].unwrap();
                let merged = to_state.inventory[to_id].unwrap();
                assert!(
                    to_state.inventory[from_id]
                        == Some(Inventory {
                            quantities: Quantities::default(),
                            pallet_status: PalletStatus::Closed,
                            ..source
                        })
                );
                assert!(
                    merged
                        == Inventory {
                            quantities: Quantities {
                                available: source.quantities.available
                                    + target.quantities.available,
                                held: source.quantities.held + target.quantities.held,
                            },
                            ..target
                        }
                );
                kani::cover!(
                    source.pallet_status == PalletStatus::Held
                        && target.pallet_status == PalletStatus::Available
                        && source.quantities.held > 0
                        && target.quantities.available > 0,
                    "held source merges into available target with both stock statuses"
                );
                kani::cover!(
                    source.pallet_status == PalletStatus::Available
                        && target.pallet_status == PalletStatus::Held
                        && source.quantities.available > 0
                        && target.quantities.held > 0,
                    "available source merges into held target with both stock statuses"
                );
            }
        }
    }
    kani::cover!(
        matches!(outcome, Outcome::Refused(Refusal::Closed)),
        "closed inventory refuses"
    );
    kani::cover!(
        total(to_state.inventory) > total(from_state.inventory),
        "positive adjustment"
    );
    kani::cover!(
        total(to_state.inventory) < total(from_state.inventory),
        "negative adjustment delta"
    );
}

#[kani::proof]
#[kani::unwind(4)]
fn original_result_and_changed_intent() {
    let from_state = arbitrary_state();
    let index: bool = kani::any();
    kani::assume(from_state.operations[usize::from(index)].is_some());
    let operation = from_state.operations[usize::from(index)].unwrap();
    let mut to_state = from_state;
    assert!(execute(&mut to_state, operation.command) == Outcome::Replayed(operation.result));
    assert!(to_state == from_state);
    let changed: Command = kani::any();
    kani::assume(changed.key == operation.command.key);
    kani::assume(changed != operation.command);
    let outcome = execute(&mut to_state, changed);
    if prepared(changed) {
        assert!(outcome == Outcome::Refused(Refusal::IntentConflict));
    } else {
        assert!(outcome == Outcome::Refused(Refusal::InvalidInput));
    }
    assert!(to_state == from_state);
    kani::cover!(
        from_state.operations[1].is_some(),
        "replay with two stored operations"
    );
}

#[kani::proof]
#[kani::unwind(4)]
fn split_history_is_complete() {
    let quantity: u8 = kani::any();
    kani::assume((2..=3).contains(&quantity));
    let inventory = [
        Some(Inventory {
            id: false,
            product_id: false,
            location_id: false,
            quantities: Quantities {
                available: quantity,
                held: 0,
            },
            pallet_status: PalletStatus::Available,
        }),
        None,
    ];
    let mut state = initial(inventory);
    let command = Command {
        key: false,
        occurred_at: false,
        action: Action::Split {
            from_inventory_id: false,
            quantity_status: QuantityStatus::Available,
            quantity: 1,
            to_location_id: true,
        },
    };
    assert!(matches!(execute(&mut state, command), Outcome::Accepted(_)));
    let operation = state.operations[0].unwrap();
    assert!(
        operation.transactions[1].is_some(),
        "created inventory lacks its transaction"
    );
    assert!(explains(inventory, operation));
}

#[kani::proof]
#[kani::unwind(4)]
fn later_merge_cannot_change_split_history_or_replay() {
    let status = if kani::any() {
        PalletStatus::Held
    } else {
        PalletStatus::Available
    };
    let mut state = initial([
        Some(Inventory {
            id: false,
            product_id: false,
            location_id: false,
            quantities: Quantities {
                available: 2,
                held: 1,
            },
            pallet_status: status,
        }),
        None,
    ]);
    let split = Command {
        key: false,
        occurred_at: false,
        action: Action::Split {
            from_inventory_id: false,
            quantity_status: QuantityStatus::Available,
            quantity: 1,
            to_location_id: true,
        },
    };
    let Outcome::Accepted(result) = execute(&mut state, split) else {
        panic!("split refused")
    };
    let first_operation = state.operations[0];
    let merge = Command {
        key: true,
        occurred_at: true,
        action: Action::Merge {
            from_inventory_id: false,
            to_inventory_id: true,
        },
    };
    assert!(matches!(execute(&mut state, merge), Outcome::Accepted(_)));
    assert!(state.inventory[0].unwrap().pallet_status == PalletStatus::Closed);
    assert!(state.operations[0] == first_operation);
    let from_state = state;
    assert!(execute(&mut state, split) == Outcome::Replayed(result));
    assert!(state == from_state);
    assert!(result.inventory[0].unwrap().pallet_status == status);
    assert!(valid(state));
}

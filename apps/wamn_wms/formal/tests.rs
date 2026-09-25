use super::*;

fn fixture() -> State {
    initial([
        Some(Inventory {
            id: false,
            product_id: false,
            location_id: false,
            quantities: Quantities {
                available: 1,
                held: 2,
            },
            pallet_status: PalletStatus::Held,
        }),
        None,
    ])
}

#[test]
fn split_then_merge_keeps_original_history_and_result() {
    let mut state = fixture();
    let split = Command {
        key: false,
        occurred_at: false,
        action: Action::Split {
            from_inventory_id: false,
            quantity_status: QuantityStatus::Held,
            quantity: 1,
            to_location_id: true,
        },
    };
    assert!(command_domain(state, split));
    let Outcome::Accepted(original) = execute(&mut state, split) else {
        panic!("split refused")
    };
    assert_eq!(
        state.inventory[0].unwrap().quantities,
        Quantities {
            available: 1,
            held: 1
        }
    );
    assert_eq!(
        state.inventory[1].unwrap().quantities,
        Quantities {
            available: 0,
            held: 1
        }
    );
    assert_eq!(
        state.inventory[1].unwrap().pallet_status,
        PalletStatus::Held
    );
    let original_operation = state.operations[0];
    let merge = Command {
        key: true,
        occurred_at: true,
        action: Action::Merge {
            from_inventory_id: false,
            to_inventory_id: true,
        },
    };
    assert!(matches!(execute(&mut state, merge), Outcome::Accepted(_)));
    assert_eq!(
        state.inventory[0].unwrap().pallet_status,
        PalletStatus::Closed
    );
    assert_eq!(
        state.inventory[0].unwrap().quantities,
        Quantities::default()
    );
    assert_eq!(total(state.inventory), 3);
    assert_eq!(state.operations[0], original_operation);
    let from_state = state;
    assert_eq!(execute(&mut state, split), Outcome::Replayed(original));
    assert_eq!(state, from_state);
    assert_eq!(
        original.inventory[0].unwrap().pallet_status,
        PalletStatus::Held
    );
    assert!(valid(state));
}

#[test]
fn adjustment_records_both_stock_statuses_and_requires_reason() {
    let mut state = fixture();
    let command = Command {
        key: false,
        occurred_at: true,
        action: Action::Adjust {
            inventory_id: false,
            quantity_status: QuantityStatus::Held,
            to_quantity: 1,
            reason_present: true,
        },
    };
    assert!(matches!(execute(&mut state, command), Outcome::Accepted(_)));
    let transaction = state.operations[0].unwrap().transactions[0].unwrap();
    assert_eq!(
        transaction.from_quantities,
        Quantities {
            available: 1,
            held: 2
        }
    );
    assert_eq!(
        transaction.to_quantities,
        Quantities {
            available: 1,
            held: 1
        }
    );
    assert_eq!(transaction.r#type, Type::Adjust);
    let from_state = state;
    let changed = Command {
        action: Action::Adjust {
            inventory_id: false,
            quantity_status: QuantityStatus::Available,
            to_quantity: 1,
            reason_present: true,
        },
        ..command
    };
    assert_eq!(
        execute(&mut state, changed),
        Outcome::Refused(Refusal::IntentConflict)
    );
    let refused = Command {
        key: true,
        action: Action::Adjust {
            inventory_id: false,
            quantity_status: QuantityStatus::Held,
            to_quantity: 1,
            reason_present: false,
        },
        ..command
    };
    assert_eq!(
        execute(&mut state, refused),
        Outcome::Refused(Refusal::InvalidInput)
    );
    assert_eq!(state, from_state);
}

#[test]
fn move_and_changed_intent_do_not_duplicate_an_operation() {
    let mut state = fixture();
    let command = Command {
        key: false,
        occurred_at: true,
        action: Action::Move {
            inventory_id: false,
            to_location_id: true,
        },
    };
    assert!(matches!(execute(&mut state, command), Outcome::Accepted(_)));
    let from_state = state;
    let changed = Command {
        occurred_at: false,
        ..command
    };
    assert!(matches!(execute(&mut state, changed), Outcome::Refused(_)));
    assert_eq!(state, from_state);
    assert_eq!(total(state.inventory), 3);
}

#[test]
fn mixed_status_merge_keeps_target_status_stock_history_and_replay() {
    let mut state = fixture();
    state.inventory[1] = Some(Inventory {
        id: true,
        product_id: false,
        location_id: true,
        quantities: Quantities {
            available: 2,
            held: 0,
        },
        pallet_status: PalletStatus::Available,
    });
    assert!(valid(state));
    let merge = Command {
        key: false,
        occurred_at: false,
        action: Action::Merge {
            from_inventory_id: false,
            to_inventory_id: true,
        },
    };
    let Outcome::Accepted(original) = execute(&mut state, merge) else {
        panic!("mixed-status merge refused")
    };
    let operation = state.operations[0].unwrap();
    assert_eq!(
        state.inventory[0].unwrap().pallet_status,
        PalletStatus::Closed
    );
    assert_eq!(
        state.inventory[1].unwrap().pallet_status,
        PalletStatus::Available
    );
    assert_eq!(
        state.inventory[1].unwrap().quantities,
        Quantities {
            available: 3,
            held: 2
        }
    );
    let source_row = operation.transactions[0].unwrap();
    let target_row = operation.transactions[1].unwrap();
    assert_eq!(source_row.from_pallet_status, Some(PalletStatus::Held));
    assert_eq!(source_row.to_pallet_status, PalletStatus::Closed);
    assert_eq!(
        source_row.from_quantities,
        Quantities {
            available: 1,
            held: 2
        }
    );
    assert_eq!(source_row.to_quantities, Quantities::default());
    assert_eq!(target_row.from_pallet_status, Some(PalletStatus::Available));
    assert_eq!(target_row.to_pallet_status, PalletStatus::Available);
    assert_eq!(
        target_row.from_quantities,
        Quantities {
            available: 2,
            held: 0
        }
    );
    assert_eq!(
        target_row.to_quantities,
        Quantities {
            available: 3,
            held: 2
        }
    );
    assert_eq!(source_row.operation_id, target_row.operation_id);
    let adjust = Command {
        key: true,
        occurred_at: true,
        action: Action::Adjust {
            inventory_id: true,
            quantity_status: QuantityStatus::Held,
            to_quantity: 1,
            reason_present: true,
        },
    };
    assert!(matches!(execute(&mut state, adjust), Outcome::Accepted(_)));
    assert_eq!(
        state.inventory[1].unwrap().quantities,
        Quantities {
            available: 3,
            held: 1
        }
    );
    assert_eq!(state.operations[0], Some(operation));
    let from_state = state;
    assert_eq!(execute(&mut state, merge), Outcome::Replayed(original));
    assert_eq!(state, from_state);
    assert!(valid(state));
}

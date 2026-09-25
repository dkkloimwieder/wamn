use super::*;

fn fixture() -> State {
    initial([
        Some(Inventory {
            id: false,
            product_id: false,
            location_id: false,
            quantity: 3,
            status: Status::Held,
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
            quantity: 1,
            to_location_id: true,
        },
    };
    assert!(command_domain(state, split));
    let Outcome::Accepted(original) = execute(&mut state, split) else {
        panic!("split refused")
    };
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
    assert_eq!(state.inventory[0].unwrap().status, Status::Closed);
    assert_eq!(state.inventory[0].unwrap().quantity, 0);
    assert_eq!(total(state.inventory), 3);
    assert_eq!(state.operations[0], original_operation);
    let from_state = state;
    assert_eq!(execute(&mut state, split), Outcome::Replayed(original));
    assert_eq!(state, from_state);
    assert_eq!(original.inventory[0].unwrap().status, Status::Held);
    assert!(valid(state));
}

#[test]
fn adjustment_records_both_quantities_and_requires_reason() {
    let mut state = fixture();
    let command = Command {
        key: false,
        occurred_at: true,
        action: Action::Adjust {
            inventory_id: false,
            to_quantity: 2,
            reason_present: true,
        },
    };
    assert!(matches!(execute(&mut state, command), Outcome::Accepted(_)));
    let transaction = state.operations[0].unwrap().transactions[0].unwrap();
    assert_eq!((transaction.from_quantity, transaction.to_quantity), (3, 2));
    assert_eq!(transaction.r#type, Type::Adjust);
    let from_state = state;
    let refused = Command {
        key: true,
        action: Action::Adjust {
            inventory_id: false,
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
    let available = Inventory {
        status: Status::Available,
        ..state.inventory[0].unwrap()
    };
    assert!(valid_inventory([Some(available), None]));
}

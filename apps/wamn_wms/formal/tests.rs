use super::*;
fn fixture() -> State {
    initial(
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
fn command(key: bool, action: Action) -> Command {
    Command {
        key,
        action,
        occurred_at: key,
        reason: None,
    }
}
#[test]
fn split_merge_retains_lineage_history_and_replay() {
    let mut state = fixture();
    let split = command(
        false,
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: true,
        },
    );
    assert!(command_domain(state, split));
    let Outcome::Accepted(result) = execute(&mut state, split) else {
        panic!("split refused")
    };
    let original = state.operations[0];
    let created = original.unwrap().transactions[1].unwrap();
    assert_eq!(
        (
            created.inventory_id,
            created.from_inventory_id,
            created.to_inventory_id
        ),
        (true, false, true)
    );
    assert_eq!((created.from_quantity, created.to_quantity), (0, 1));
    assert_eq!(created.to_disposition, Disposition::Held);
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
    for (id, row) in state.operations[1]
        .unwrap()
        .transactions
        .into_iter()
        .enumerate()
    {
        let row = row.unwrap();
        assert_eq!(usize::from(row.inventory_id), id);
        assert_eq!((row.from_inventory_id, row.to_inventory_id), (false, true));
    }
    assert_eq!(total(state.inventory), 3);
    assert_eq!(state.inventory[0].unwrap().lifecycle, Lifecycle::Closed);
    assert_eq!(state.packaging[0].lifecycle, Lifecycle::Open);
    assert_eq!(state.operations[0], original);
    let from = state;
    assert_eq!(execute(&mut state, split), Outcome::Replayed(result));
    assert_eq!(state, from);
    assert!(valid(state));
}
#[test]
fn different_dispositions_refuse_merge_without_mutation() {
    let mut state = fixture();
    state.inventory[1] = Some(Inventory {
        id: true,
        packaging_id: false,
        location_id: false,
        quantity: 2,
        disposition: Disposition::Available,
        ..state.inventory[0].unwrap()
    });
    assert!(valid(state));
    let from = state;
    assert_eq!(
        execute(
            &mut state,
            command(
                false,
                Action::Merge {
                    from_inventory_id: false,
                    to_inventory_id: true
                }
            )
        ),
        Outcome::Refused(Refusal::DispositionMismatch)
    );
    assert_eq!(state, from);
}
#[test]
fn move_records_packaging_location_and_original_result() {
    let mut state = fixture();
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
    let original = state.operations[0];
    let row = original.unwrap().transactions[0].unwrap();
    assert_eq!(
        (row.from_packaging_id, row.to_packaging_id),
        (Some(false), true)
    );
    assert_eq!(
        (row.from_location_id, row.to_location_id),
        (Some(false), true)
    );
    assert_eq!((row.from_quantity, row.to_quantity), (3, 3));
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
    assert_eq!(state.operations[1].unwrap().transactions, [None; 2]);
    assert_eq!(state.operations[0], original);
    let from = state;
    assert_eq!(execute(&mut state, movement), Outcome::Replayed(result));
    assert_eq!(state, from);
    assert_eq!(result.packaging[0].lifecycle, Lifecycle::Open);
    assert_eq!(state.packaging[0].lifecycle, Lifecycle::Closed);
}
#[test]
fn packaging_closure_requires_empty_and_refuses_receipt() {
    let mut state = fixture();
    let from = state;
    assert_eq!(
        execute(
            &mut state,
            command(
                false,
                Action::ClosePackaging {
                    packaging_id: false
                }
            )
        ),
        Outcome::Refused(Refusal::PackagingNotEmpty)
    );
    assert_eq!(state, from);
    assert!(matches!(
        execute(
            &mut state,
            command(false, Action::ClosePackaging { packaging_id: true })
        ),
        Outcome::Accepted(_)
    ));
    let closed = state;
    let close = command(false, Action::ClosePackaging { packaging_id: true });
    let original = closed.operations[0].unwrap().result;
    assert_eq!(execute(&mut state, close), Outcome::Replayed(original));
    assert_eq!(state, closed);
    assert_eq!(
        execute(&mut state, Command { key: true, ..close }),
        Outcome::Refused(Refusal::ClosedPackaging)
    );
    assert_eq!(state, closed);
    for action in [
        Action::Move {
            inventory_id: false,
            to_packaging_id: true,
            to_location_id: true,
        },
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: true,
        },
    ] {
        assert_eq!(
            execute(&mut state, command(true, action)),
            Outcome::Refused(Refusal::ClosedPackaging)
        );
        assert_eq!(state, closed);
    }
    assert!(valid(state));
}
#[test]
fn adjustment_records_reason_and_refuses_changed_intent() {
    let mut state = fixture();
    let adjust = Command {
        reason: Some(false),
        ..command(
            false,
            Action::Adjust {
                inventory_id: false,
                to_quantity: 5,
            },
        )
    };
    assert!(matches!(execute(&mut state, adjust), Outcome::Accepted(_)));
    let row = state.operations[0].unwrap().transactions[0].unwrap();
    assert_eq!(
        (row.from_quantity, row.to_quantity, row.reason),
        (3, 5, Some(false))
    );
    let from = state;
    assert_eq!(
        execute(
            &mut state,
            Command {
                reason: Some(true),
                ..adjust
            }
        ),
        Outcome::Refused(Refusal::IntentConflict)
    );
    assert_eq!(
        execute(
            &mut state,
            Command {
                key: true,
                reason: None,
                ..adjust
            }
        ),
        Outcome::Refused(Refusal::InvalidInput)
    );
    assert_eq!(state, from);
}

#[test]
fn inventory_location_is_independent_and_destination_must_be_colocated() {
    let mut metadata_only = fixture();
    let inventory = metadata_only.inventory;
    metadata_only.packaging[0].location_id = true;
    assert_eq!(metadata_only.inventory, inventory);
    assert!(!metadata_only.inventory[0].unwrap().location_id);
    assert!(!valid_business(metadata_only));

    let mut state = fixture();
    let original = state;
    for action in [
        Action::Move {
            inventory_id: false,
            to_packaging_id: true,
            to_location_id: false,
        },
        Action::Split {
            from_inventory_id: false,
            quantity: 1,
            to_packaging_id: true,
            to_location_id: false,
        },
    ] {
        assert_eq!(
            execute(&mut state, command(false, action)),
            Outcome::Refused(Refusal::InvalidInput)
        );
        assert_eq!(state, original);
    }
    assert!(matches!(
        execute(
            &mut state,
            command(
                false,
                Action::Move {
                    inventory_id: false,
                    to_packaging_id: true,
                    to_location_id: true,
                }
            )
        ),
        Outcome::Accepted(_)
    ));
    assert!(state.inventory[0].unwrap().location_id);
    assert_eq!(state.packaging, original.packaging);
    assert!(valid_business(state));
}

#[test]
fn relocation_records_all_open_stock_and_preserves_prior_results() {
    let mut state = fixture();
    state.inventory[1] = Some(Inventory {
        id: true,
        disposition: Disposition::Available,
        quantity: 2,
        ..state.inventory[0].unwrap()
    });
    let from = state;
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
    assert!(valid(state));
    assert_eq!(total(state.inventory), total(from.inventory));
    for row in state.operations[0].unwrap().transactions {
        let row = row.unwrap();
        assert_eq!(row.r#type, Type::RelocatePackaging);
        assert_eq!(row.from_location_id, Some(false));
        assert!(row.to_location_id);
        assert_eq!(row.from_quantity, row.to_quantity);
        assert_eq!(row.from_inventory_id, row.to_inventory_id);
        assert_eq!(row.from_packaging_id, Some(row.to_packaging_id));
    }
    let original = state.operations[0];
    let accepted = state;
    assert_eq!(execute(&mut state, relocation), Outcome::Replayed(result));
    assert_eq!(
        execute(
            &mut state,
            Command {
                key: true,
                ..relocation
            }
        ),
        Outcome::Refused(Refusal::NoOp)
    );
    assert_eq!(state, accepted);
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
    assert_eq!(state.operations[0], original);
    let later = state;
    assert_eq!(execute(&mut state, relocation), Outcome::Replayed(result));
    assert_eq!(state, later);
    assert!(result.inventory[0].unwrap().location_id);
    assert!(!state.inventory[0].unwrap().location_id);
}

#[test]
fn empty_packaging_relocation_leaves_closed_inventory_historical() {
    let mut state = fixture();
    state.inventory[0].as_mut().unwrap().lifecycle = Lifecycle::Closed;
    state.inventory[0].as_mut().unwrap().quantity = 0;
    let inventory = state.inventory;
    let relocation = command(
        false,
        Action::RelocatePackaging {
            packaging_id: false,
            to_location_id: true,
        },
    );
    let Outcome::Accepted(result) = execute(&mut state, relocation) else {
        panic!("empty relocation refused")
    };
    assert!(state.packaging[0].location_id);
    assert_eq!(state.inventory, inventory);
    assert_eq!(state.operations[0].unwrap().transactions, [None; 2]);
    assert!(valid(state));
    let accepted = state;
    assert_eq!(execute(&mut state, relocation), Outcome::Replayed(result));
    assert_eq!(state, accepted);
}

#[test]
fn failed_relocation_preserves_all_state_and_allows_retry() {
    let mut state = fixture();
    let adjust = Command {
        reason: Some(false),
        ..command(
            false,
            Action::Adjust {
                inventory_id: false,
                to_quantity: 4,
            },
        )
    };
    assert!(matches!(execute(&mut state, adjust), Outcome::Accepted(_)));
    let from = state;
    let relocation = command(
        true,
        Action::RelocatePackaging {
            packaging_id: false,
            to_location_id: true,
        },
    );
    for failure in [
        Failure::BusinessState,
        Failure::History,
        Failure::StoredResult,
    ] {
        assert_eq!(
            execute_with_failure(&mut state, relocation, failure),
            Outcome::Aborted
        );
        assert_eq!(state, from);
    }
    assert!(matches!(
        execute(&mut state, relocation),
        Outcome::Accepted(_)
    ));
    assert_eq!(state.operations[0], from.operations[0]);
    assert!(valid(state));
}

#[test]
fn relocation_refuses_closed_packaging_and_changed_intent() {
    let mut state = fixture();
    state.packaging[1].lifecycle = Lifecycle::Closed;
    let from = state;
    assert_eq!(
        execute(
            &mut state,
            command(
                false,
                Action::RelocatePackaging {
                    packaging_id: true,
                    to_location_id: false,
                }
            )
        ),
        Outcome::Refused(Refusal::ClosedPackaging)
    );
    assert_eq!(state, from);
    let relocation = command(
        false,
        Action::RelocatePackaging {
            packaging_id: false,
            to_location_id: true,
        },
    );
    assert!(matches!(
        execute(&mut state, relocation),
        Outcome::Accepted(_)
    ));
    let accepted = state;
    assert_eq!(
        execute(
            &mut state,
            Command {
                occurred_at: true,
                ..relocation
            }
        ),
        Outcome::Refused(Refusal::IntentConflict)
    );
    assert_eq!(state, accepted);
}

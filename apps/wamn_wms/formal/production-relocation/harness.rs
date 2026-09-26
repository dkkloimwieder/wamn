//! Bounded obligations over the production relocation decision function.

#[path = "../../data/src/packaging_relocate/decision.rs"]
mod decision;

use decision::{Command, Inventory, Packaging, RefusalType, State, Transition};

// Equal immutable string pointers and lengths imply equal bytes. This is a
// stronger preservation assertion, not a substitute for business comparisons.
fn assert_same_text(actual: &str, expected: &str) {
    assert!(std::ptr::eq(actual, expected));
}

fn assert_inventory(actual: Inventory<'_>, original: Inventory<'_>, location: &str) {
    assert_same_text(actual.id, original.id);
    assert_same_text(actual.product_id, original.product_id);
    assert_same_text(actual.packaging_id, original.packaging_id);
    assert_same_text(actual.location_id, location);
    assert_same_text(actual.quantity, original.quantity);
    assert_same_text(actual.disposition, original.disposition);
    assert_same_text(actual.lifecycle, original.lifecycle);
}

fn assert_transition(state: State<'_>, destination: &str, transition: &Transition<'_>) {
    assert_same_text(transition.from_packaging.id, state.packaging.id);
    assert_same_text(
        transition.from_packaging.location_id,
        state.packaging.location_id,
    );
    assert_same_text(
        transition.from_packaging.lifecycle,
        state.packaging.lifecycle,
    );
    assert_same_text(transition.to_packaging.id, state.packaging.id);
    assert_same_text(transition.to_packaging.lifecycle, state.packaging.lifecycle);
    assert_same_text(transition.to_packaging.location_id, destination);
    let mut affected = 0;
    for original in state.inventory {
        let expected = original.lifecycle == "open" && original.packaging_id == state.packaging.id;
        let mut updates = 0;
        let mut rows = 0;
        for update in &transition.inventory {
            if std::ptr::eq(update.id, original.id) {
                updates += 1;
                assert_inventory(*update, *original, destination);
                assert_same_text(update.location_id, transition.to_packaging.location_id);
            }
        }
        for row in &transition.transactions {
            if std::ptr::eq(row.from_inventory.id, original.id) {
                rows += 1;
                assert_inventory(row.from_inventory, *original, original.location_id);
                assert_inventory(row.to_inventory, *original, destination);
            }
        }
        if expected {
            affected += 1;
            assert_eq!(updates, 1, "each affected identity has exactly one update");
            assert_eq!(
                rows, 1,
                "each affected identity has exactly one history row"
            );
        } else {
            assert_eq!(updates, 0, "closed and unrelated inventory does not move");
            assert_eq!(rows, 0, "closed and unrelated inventory has no new history");
        }
    }
    assert_eq!(transition.inventory.len(), affected);
    assert_eq!(transition.transactions.len(), affected);
}

fn item(id: &'static str) -> Inventory<'static> {
    Inventory {
        id,
        product_id: "product-a",
        packaging_id: "package-a",
        location_id: "location-a",
        quantity: "4.50",
        disposition: "held",
        lifecycle: "open",
    }
}

fn packaging() -> Packaging<'static> {
    Packaging {
        id: "package-a",
        location_id: "location-a",
        lifecycle: "open",
    }
}

#[cfg(kani)]
fn arbitrary_inventory(id: &'static str) -> Inventory<'static> {
    Inventory {
        id,
        product_id: if kani::any() {
            "product-a"
        } else {
            "product-b"
        },
        packaging_id: if kani::any() {
            "package-a"
        } else {
            "package-b"
        },
        location_id: if kani::any() {
            "location-a"
        } else {
            "location-b"
        },
        quantity: if kani::any() { "4.50" } else { "0" },
        disposition: if kani::any() { "available" } else { "held" },
        lifecycle: if kani::any() { "open" } else { "closed" },
    }
}

#[cfg(kani)]
fn complete_relocation_case(
    length: usize,
    first: (&'static str, &'static str),
    second: (&'static str, &'static str),
) -> Option<usize> {
    let inventory = [
        Inventory {
            packaging_id: first.0,
            lifecycle: first.1,
            ..arbitrary_inventory("inventory-a")
        },
        Inventory {
            packaging_id: second.0,
            lifecycle: second.1,
            ..arbitrary_inventory("inventory-b")
        },
    ];
    let state = State {
        packaging: packaging(),
        inventory: &inventory[..length],
        destination_exists: true,
    };
    let original = inventory;
    let command = Command {
        to_location_id: "location-b",
    };
    let outcome = decision::decide(state, command);
    let affected = if let Ok(transition) = outcome {
        assert_transition(state, command.to_location_id, &transition);
        Some(transition.inventory.len())
    } else {
        None
    };
    for (actual, from) in inventory.iter().zip(original.iter()) {
        assert_inventory(*actual, *from, from.location_id);
    }
    affected
}

#[cfg(kani)]
const AO: (&str, &str) = ("package-a", "open");
#[cfg(kani)]
const UO: (&str, &str) = ("package-b", "open");
#[cfg(kani)]
const AC: (&str, &str) = ("package-a", "closed");
#[cfg(kani)]
const UC: (&str, &str) = ("package-b", "closed");

#[cfg(kani)]
macro_rules! relocation_case {
    ($name:ident, $length:expr, $first:expr, $second:expr, $affected:expr) => {
        #[kani::proof]
        #[kani::unwind(3)]
        fn $name() {
            let affected = complete_relocation_case($length, $first, $second);
            kani::cover!(
                affected == Some($affected),
                "this membership partition accepts a relocation"
            );
        }
    };
}

#[cfg(kani)]
relocation_case!(relocation_empty, 0, AO, AO, 0);
#[cfg(kani)]
relocation_case!(relocation_one_ao, 1, AO, AO, 1);
#[cfg(kani)]
relocation_case!(relocation_one_uo, 1, UO, AO, 0);
#[cfg(kani)]
relocation_case!(relocation_one_ac, 1, AC, AO, 0);
#[cfg(kani)]
relocation_case!(relocation_one_uc, 1, UC, AO, 0);
#[cfg(kani)]
relocation_case!(relocation_two_ao_ao, 2, AO, AO, 2);
#[cfg(kani)]
relocation_case!(relocation_two_ao_uo, 2, AO, UO, 1);
#[cfg(kani)]
relocation_case!(relocation_two_ao_ac, 2, AO, AC, 1);
#[cfg(kani)]
relocation_case!(relocation_two_ao_uc, 2, AO, UC, 1);
#[cfg(kani)]
relocation_case!(relocation_two_uo_ao, 2, UO, AO, 1);
#[cfg(kani)]
relocation_case!(relocation_two_uo_uo, 2, UO, UO, 0);
#[cfg(kani)]
relocation_case!(relocation_two_uo_ac, 2, UO, AC, 0);
#[cfg(kani)]
relocation_case!(relocation_two_uo_uc, 2, UO, UC, 0);
#[cfg(kani)]
relocation_case!(relocation_two_ac_ao, 2, AC, AO, 1);
#[cfg(kani)]
relocation_case!(relocation_two_ac_uo, 2, AC, UO, 0);
#[cfg(kani)]
relocation_case!(relocation_two_ac_ac, 2, AC, AC, 0);
#[cfg(kani)]
relocation_case!(relocation_two_ac_uc, 2, AC, UC, 0);
#[cfg(kani)]
relocation_case!(relocation_two_uc_ao, 2, UC, AO, 1);
#[cfg(kani)]
relocation_case!(relocation_two_uc_uo, 2, UC, UO, 0);
#[cfg(kani)]
relocation_case!(relocation_two_uc_ac, 2, UC, AC, 0);
#[cfg(kani)]
relocation_case!(relocation_two_uc_uc, 2, UC, UC, 0);

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(12)]
fn refusals_preserve_input_and_precedence() {
    let inventory = [
        arbitrary_inventory("inventory-a"),
        arbitrary_inventory("inventory-b"),
    ];
    let package = Packaging {
        lifecycle: if kani::any() { "open" } else { "closed" },
        ..packaging()
    };
    let destination = if kani::any() {
        "location-a"
    } else {
        "location-b"
    };
    let exists: bool = kani::any();
    let original = inventory;
    let state = State {
        packaging: package,
        inventory: &inventory,
        destination_exists: exists,
    };
    let outcome = decision::decide(
        state,
        Command {
            to_location_id: destination,
        },
    );
    let displaced = inventory.iter().any(|row| {
        row.lifecycle == "open"
            && row.packaging_id == "package-a"
            && row.location_id != "location-a"
    });
    let expected = if package.lifecycle == "closed" {
        Some(RefusalType::ClosedPackaging)
    } else if destination == "location-a" {
        Some(RefusalType::NoOp)
    } else if displaced {
        Some(RefusalType::NotColocated)
    } else if !exists {
        Some(RefusalType::MissingDestination)
    } else {
        None
    };
    assert_eq!(
        outcome.as_ref().err().map(|refusal| refusal.r#type),
        expected
    );
    assert_eq!(inventory, original);
    assert_eq!(state.packaging, package);
    kani::cover!(expected == Some(RefusalType::ClosedPackaging));
    kani::cover!(expected == Some(RefusalType::NoOp));
    kani::cover!(expected == Some(RefusalType::NotColocated));
    kani::cover!(expected == Some(RefusalType::MissingDestination));
    kani::cover!(expected.is_none());
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(12)]
fn two_inventory_history_is_complete() {
    let inventory = [
        item("inventory-a"),
        Inventory {
            quantity: "2",
            disposition: "available",
            ..item("inventory-b")
        },
    ];
    let state = State {
        packaging: packaging(),
        inventory: &inventory,
        destination_exists: true,
    };
    let command = Command {
        to_location_id: "location-b",
    };
    let transition = decision::decide(state, command).unwrap();
    assert_transition(state, command.to_location_id, &transition);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocation_keeps_decimal_scale_and_closed_history() {
        let inventory = [
            item("inventory-a"),
            Inventory {
                quantity: "0",
                lifecycle: "closed",
                ..item("inventory-b")
            },
        ];
        let state = State {
            packaging: packaging(),
            inventory: &inventory,
            destination_exists: true,
        };
        let command = Command {
            to_location_id: "location-b",
        };
        let transition = decision::decide(state, command).unwrap();
        assert_transition(state, command.to_location_id, &transition);
        assert_eq!(transition.transactions[0].to_inventory.quantity, "4.50");
        assert_eq!(inventory[1].location_id, "location-a");
    }

    #[test]
    fn empty_packaging_relocates_but_fresh_noop_refuses() {
        let state = State {
            packaging: packaging(),
            inventory: &[],
            destination_exists: true,
        };
        let command = Command {
            to_location_id: "location-b",
        };
        let transition = decision::decide(state, command).unwrap();
        assert_transition(state, command.to_location_id, &transition);
        assert!(transition.transactions.is_empty());
        let result = decision::decide(
            state,
            Command {
                to_location_id: "location-a",
            },
        );
        assert_eq!(result.unwrap_err().r#type, RefusalType::NoOp);
    }
}

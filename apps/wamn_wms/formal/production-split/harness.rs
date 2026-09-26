//! Independent bounded obligations over the actual production split decision.

#[path = "../../data/src/inventory_split/decision.rs"]
mod decision;

use decision::{Command, Inventory, Packaging, RefusalType, State, Transition};

fn same_text(actual: &str, expected: &str) {
    assert!(std::ptr::eq(actual, expected));
}

// The observer reads base-ten units independently. It does not call the
// production decimal subtraction routine or repeat its subtraction algorithm.
fn hundredths(text: &str) -> u32 {
    let mut units = 0;
    let mut fractional_digits = None;
    for byte in text.bytes() {
        if byte == b'.' {
            assert!(fractional_digits.is_none());
            fractional_digits = Some(0);
        } else {
            assert!(byte.is_ascii_digit());
            units = units * 10 + u32::from(byte - b'0');
            if let Some(digits) = &mut fractional_digits {
                *digits += 1;
            }
        }
    }
    match fractional_digits {
        None | Some(0) => units * 100,
        Some(1) => units * 10,
        Some(2) => units,
        _ => panic!("the observation domain has at most two fractional digits"),
    }
}

fn unchanged_fields(actual: &Inventory<'_>, source: &Inventory<'_>) {
    same_text(actual.product_id, source.product_id);
    same_text(actual.disposition, source.disposition);
    same_text(actual.lifecycle, source.lifecycle);
}

fn exact_inventory(actual: &Inventory<'_>, expected: &Inventory<'_>) {
    unchanged_fields(actual, expected);
    same_text(actual.id, expected.id);
    same_text(actual.packaging_id, expected.packaging_id);
    same_text(actual.location_id, expected.location_id);
    assert_eq!(actual.quantity, expected.quantity);
}

fn complete_transition(source: &Inventory<'_>, command: Command<'_>, transition: &Transition<'_>) {
    let mut retained = 0;
    let mut created = 0;
    let mut total = 0;
    for item in &transition.inventory {
        unchanged_fields(item, source);
        let quantity = hundredths(&item.quantity);
        assert!(quantity > 0);
        total += quantity;
        if std::ptr::eq(item.id, source.id) {
            retained += 1;
            same_text(item.packaging_id, source.packaging_id);
            same_text(item.location_id, source.location_id);
        } else {
            created += 1;
            same_text(item.id, command.new_inventory_id);
            same_text(item.packaging_id, command.to_packaging_id);
            same_text(item.location_id, command.to_location_id);
            assert_eq!(quantity, hundredths(command.quantity));
        }
        let mut history_count = 0;
        for row in &transition.transactions {
            if std::ptr::eq(row.inventory_id, item.id) {
                history_count += 1;
                // Both rows permanently identify the source of the split.
                same_text(row.from_inventory_id, source.id);
                same_text(row.to_inventory_id, item.id);
                exact_inventory(&row.to_inventory, item);
                if std::ptr::eq(item.id, source.id) {
                    exact_inventory(row.from_inventory.as_ref().unwrap(), source);
                } else {
                    assert!(row.from_inventory.is_none());
                }
            }
        }
        assert_eq!(
            history_count, 1,
            "each identity has one complete history row"
        );
    }
    assert_eq!(retained, 1);
    assert_eq!(created, 1);
    assert_eq!(
        total,
        hundredths(&source.quantity),
        "split conserves quantity"
    );
}

fn source(quantity: &str) -> Inventory<'static> {
    Inventory {
        id: "inventory-a",
        product_id: "product-a",
        packaging_id: "package-a",
        location_id: "location-a",
        quantity: quantity.to_owned(),
        disposition: "held",
        lifecycle: "open",
    }
}

fn package() -> Packaging<'static> {
    Packaging {
        id: "package-a",
        location_id: "location-a",
        lifecycle: "open",
    }
}

fn command(quantity: &str, same_package: bool) -> Command<'_> {
    Command {
        new_inventory_id: "inventory-b",
        quantity,
        to_packaging_id: if same_package {
            "package-a"
        } else {
            "package-b"
        },
        to_location_id: if same_package {
            "location-a"
        } else {
            "location-b"
        },
    }
}

fn run_quantity_case(source: &Inventory<'_>, requested: &str, same_package: bool) -> bool {
    let command = command(requested, same_package);
    let state = State {
        source,
        source_packaging: Some(package()),
        destination: Some(Packaging {
            id: command.to_packaging_id,
            location_id: command.to_location_id,
            lifecycle: "open",
        }),
    };
    let original = source.clone();
    let result = decision::decide(state, command);
    let enough = hundredths(&source.quantity) > hundredths(requested);
    match result {
        Ok(transition) => {
            assert!(enough);
            complete_transition(source, command, &transition);
        }
        Err(refusal) => {
            assert!(!enough);
            assert_eq!(refusal.r#type, RefusalType::InsufficientQuantity);
        }
    }
    exact_inventory(source, &original);
    enough
}

#[cfg(kani)]
fn bounded_quantity_case(loaded: &str, requested: &str) -> bool {
    let source = Inventory {
        product_id: if kani::any() {
            "product-a"
        } else {
            "product-b"
        },
        disposition: if kani::any() { "held" } else { "available" },
        ..source(loaded)
    };
    run_quantity_case(&source, requested, kani::any())
}

#[cfg(kani)]
macro_rules! quantity_case {
    ($name:ident, $loaded:expr, $requested:expr, $accepted:expr) => {
        #[kani::proof]
        #[kani::unwind(6)]
        fn $name() {
            let accepted = bounded_quantity_case($loaded, $requested);
            kani::cover!(
                accepted == $accepted,
                "this operand pair reaches its business outcome"
            );
        }
    };
}

#[cfg(kani)]
quantity_case!(split_half_by_quarter, "0.50", "0.25", true);
#[cfg(kani)]
quantity_case!(split_half_by_half, "0.50", "0.50", false);
#[cfg(kani)]
quantity_case!(split_half_by_one, "0.50", "1", false);
#[cfg(kani)]
quantity_case!(split_half_by_two_and_half, "0.50", "2.5", false);
#[cfg(kani)]
quantity_case!(split_half_by_three, "0.50", "3.00", false);
#[cfg(kani)]
quantity_case!(split_one_by_quarter, "1.00", "0.25", true);
#[cfg(kani)]
quantity_case!(split_one_by_half, "1.00", "0.50", true);
#[cfg(kani)]
quantity_case!(split_one_by_one, "1.00", "1", false);
#[cfg(kani)]
quantity_case!(split_one_by_two_and_half, "1.00", "2.5", false);
#[cfg(kani)]
quantity_case!(split_one_by_three, "1.00", "3.00", false);
#[cfg(kani)]
quantity_case!(split_two_and_half_by_quarter, "2.50", "0.25", true);
#[cfg(kani)]
quantity_case!(split_two_and_half_by_half, "2.50", "0.50", true);
#[cfg(kani)]
quantity_case!(split_two_and_half_by_one, "2.50", "1", true);
#[cfg(kani)]
quantity_case!(split_two_and_half_by_two_and_half, "2.50", "2.5", false);
#[cfg(kani)]
quantity_case!(split_two_and_half_by_three, "2.50", "3.00", false);

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(6)]
fn refusal_precedence_preserves_input() {
    let closed: bool = kani::any();
    let source_exists: bool = kani::any();
    let source_open: bool = kani::any();
    let source_colocated: bool = kani::any();
    let destination_exists: bool = kani::any();
    let destination_open: bool = kani::any();
    let destination_colocated: bool = kani::any();
    kani::assume(
        closed
            || !source_exists
            || !source_open
            || !source_colocated
            || !destination_exists
            || !destination_open
            || !destination_colocated,
    );
    let source = Inventory {
        lifecycle: if closed { "closed" } else { "open" },
        ..source("1.50")
    };
    let original = source.clone();
    let source_packaging = source_exists.then_some(Packaging {
        lifecycle: if source_open { "open" } else { "closed" },
        location_id: if source_colocated {
            "location-a"
        } else {
            "location-b"
        },
        ..package()
    });
    let destination = destination_exists.then_some(Packaging {
        id: "package-b",
        lifecycle: if destination_open { "open" } else { "closed" },
        location_id: if destination_colocated {
            "location-b"
        } else {
            "location-a"
        },
    });
    let state = State {
        source: &source,
        source_packaging,
        destination,
    };
    let refusal = decision::decide(state, command("0.50", false)).unwrap_err();
    let expected = if closed {
        RefusalType::ClosedInventory
    } else if !source_exists {
        RefusalType::MissingSourcePackaging
    } else if !source_open || !source_colocated {
        RefusalType::InvalidSourcePackaging
    } else if !destination_exists {
        RefusalType::MissingDestination
    } else if !destination_open {
        RefusalType::ClosedDestination
    } else {
        RefusalType::DestinationLocationMismatch
    };
    assert_eq!(refusal.r#type, expected);
    exact_inventory(&source, &original);
    assert_eq!(state.source_packaging, source_packaging);
    assert_eq!(state.destination, destination);
    kani::cover!(expected == RefusalType::ClosedInventory);
    kani::cover!(expected == RefusalType::MissingSourcePackaging);
    kani::cover!(expected == RefusalType::InvalidSourcePackaging);
    kani::cover!(expected == RefusalType::MissingDestination);
    kani::cover!(expected == RefusalType::ClosedDestination);
    kani::cover!(expected == RefusalType::DestinationLocationMismatch);
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(6)]
fn split_history_retains_source_lineage() {
    assert!(run_quantity_case(&source("2.50"), "0.50", false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_split_conserves_quantity_and_retains_lineage() {
        assert!(run_quantity_case(&source("2.50"), "0.25", false));
        assert!(run_quantity_case(&source("1.00"), "0.50", true));
    }

    #[test]
    fn equal_or_excess_quantity_refuses_without_changes() {
        assert!(!run_quantity_case(&source("1.00"), "1", false));
        assert!(!run_quantity_case(&source("0.50"), "2.5", true));
    }
}

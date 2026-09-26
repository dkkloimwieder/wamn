//! Pure split decisions over loaded inventory and packaging.
//!
//! The caller allocates the new identity and supplies canonical positive decimal
//! command quantities. Loaded quantities use PostgreSQL's numeric text spelling.
//! The caller matches packaging identities and guarantees that the allocated
//! identity is unused. Revisions, claims, locks, and persistence remain outside
//! this module.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packaging<'a> {
    pub id: &'a str,
    pub location_id: &'a str,
    pub lifecycle: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory<'a> {
    pub id: &'a str,
    pub product_id: &'a str,
    pub packaging_id: &'a str,
    pub location_id: &'a str,
    pub quantity: String,
    pub disposition: &'a str,
    pub lifecycle: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct State<'a> {
    pub source: &'a Inventory<'a>,
    pub source_packaging: Option<Packaging<'a>>,
    pub destination: Option<Packaging<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Command<'a> {
    pub new_inventory_id: &'a str,
    pub quantity: &'a str,
    pub to_packaging_id: &'a str,
    pub to_location_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalType {
    ClosedInventory,
    MissingSourcePackaging,
    InvalidSourcePackaging,
    MissingDestination,
    ClosedDestination,
    DestinationLocationMismatch,
    InsufficientQuantity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub r#type: RefusalType,
}

/// A new identity has no prior attributes and a prior quantity of zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryTransaction<'a> {
    pub inventory_id: &'a str,
    pub from_inventory_id: &'a str,
    pub to_inventory_id: &'a str,
    pub from_inventory: Option<Inventory<'a>>,
    pub to_inventory: Inventory<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Transition<'a> {
    pub inventory: [Inventory<'a>; 2],
    pub transactions: [InventoryTransaction<'a>; 2],
}

/// Preserve lifecycle refusal precedence across the caller's revision check.
pub fn require_open(source: &Inventory<'_>) -> Result<(), Refusal> {
    if source.lifecycle != "open" {
        return Err(Refusal {
            r#type: RefusalType::ClosedInventory,
        });
    }
    Ok(())
}

/// Decide a split without changing loaded state.
pub fn decide<'a>(state: State<'a>, command: Command<'a>) -> Result<Transition<'a>, Refusal> {
    require_open(state.source)?;
    let source_packaging = state.source_packaging.ok_or(Refusal {
        r#type: RefusalType::MissingSourcePackaging,
    })?;
    if source_packaging.lifecycle != "open"
        || source_packaging.location_id != state.source.location_id
    {
        return Err(Refusal {
            r#type: RefusalType::InvalidSourcePackaging,
        });
    }
    let destination = state.destination.ok_or(Refusal {
        r#type: RefusalType::MissingDestination,
    })?;
    if destination.lifecycle != "open" {
        return Err(Refusal {
            r#type: RefusalType::ClosedDestination,
        });
    }
    if destination.location_id != command.to_location_id {
        return Err(Refusal {
            r#type: RefusalType::DestinationLocationMismatch,
        });
    }
    let quantity = subtract(&state.source.quantity, command.quantity).ok_or(Refusal {
        r#type: RefusalType::InsufficientQuantity,
    })?;
    let source = Inventory {
        quantity,
        ..state.source.clone()
    };
    let created = Inventory {
        id: command.new_inventory_id,
        packaging_id: command.to_packaging_id,
        location_id: command.to_location_id,
        quantity: command.quantity.to_owned(),
        ..state.source.clone()
    };
    Ok(Transition {
        transactions: [
            InventoryTransaction {
                inventory_id: source.id,
                from_inventory_id: source.id,
                to_inventory_id: source.id,
                from_inventory: Some(state.source.clone()),
                to_inventory: source.clone(),
            },
            InventoryTransaction {
                inventory_id: created.id,
                from_inventory_id: source.id,
                to_inventory_id: created.id,
                from_inventory: None,
                to_inventory: created.clone(),
            },
        ],
        inventory: [source, created],
    })
}

/// Subtract canonical positive decimals, preserving PostgreSQL's result scale.
fn subtract(source: &str, requested: &str) -> Option<String> {
    // PostgreSQL admits these stored values and orders both above finite values.
    // Finite command input cannot create them, but existing loaded state can.
    if source == "NaN" || source == "Infinity" {
        return Some(source.to_owned());
    }
    let (source_whole, source_fraction) = source.split_once('.').unwrap_or((source, ""));
    let (request_whole, request_fraction) = requested.split_once('.').unwrap_or((requested, ""));
    let whole = source_whole.len().max(request_whole.len());
    let scale = source_fraction.len().max(request_fraction.len());
    let digit = |integer: &str, fraction: &str, index: usize| {
        if index < whole {
            if index < whole - integer.len() {
                0
            } else {
                integer.as_bytes()[index - (whole - integer.len())] - b'0'
            }
        } else {
            fraction
                .as_bytes()
                .get(index - whole)
                .map_or(0, |b| b - b'0')
        }
    };
    let mut result = vec![b'0'; whole + scale];
    let mut borrow = 0;
    let mut positive = false;
    for index in (0..result.len()).rev() {
        let left = digit(source_whole, source_fraction, index);
        let right = digit(request_whole, request_fraction, index) + borrow;
        let value = if left < right {
            borrow = 1;
            left + 10 - right
        } else {
            borrow = 0;
            left - right
        };
        positive |= value != 0;
        result[index] = b'0' + value;
    }
    if borrow != 0 || !positive {
        return None;
    }
    let start = result[..whole]
        .iter()
        .position(|b| *b != b'0')
        .unwrap_or(whole - 1);
    let mut text = String::with_capacity(result.len() - start + usize::from(scale != 0));
    for byte in &result[start..whole] {
        text.push(char::from(*byte));
    }
    if scale != 0 {
        text.push('.');
        for byte in &result[whole..] {
            text.push(char::from(*byte));
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::{Command, Inventory, Packaging, RefusalType, State, decide, subtract};

    #[test]
    fn decimal_subtraction_preserves_scale_without_a_machine_integer_bound() {
        for (source, requested, expected) in [
            ("10", "4", "6"),
            ("1.000", "0.001", "0.999"),
            ("10.0", "0.0001", "9.9999"),
            ("12.3400", "2.34", "10.0000"),
            ("0.02", "0.010", "0.010"),
            (
                "10000000000000000000000000000000000000000",
                "1",
                "9999999999999999999999999999999999999999",
            ),
            ("NaN", "1.0", "NaN"),
            ("Infinity", "1", "Infinity"),
        ] {
            assert_eq!(subtract(source, requested).as_deref(), Some(expected));
        }
        for (source, requested) in [("1", "1.00"), ("0.01", "0.0101"), ("9", "10")] {
            assert_eq!(subtract(source, requested), None);
        }
    }

    #[test]
    fn decimal_subtraction_matches_independent_hundredths_arithmetic() {
        for source in 1..250 {
            for requested in 1..250 {
                let source_text = format!("{}.{:02}", source / 100, source % 100);
                let request_text = format!("{}.{:02}", requested / 100, requested % 100);
                let expected = (source > requested).then(|| {
                    let remainder = source - requested;
                    format!("{}.{:02}", remainder / 100, remainder % 100)
                });
                assert_eq!(subtract(&source_text, &request_text), expected);
            }
        }
    }

    #[test]
    fn split_retains_held_disposition_and_records_both_identities() {
        let source = Inventory {
            id: "source",
            product_id: "product",
            packaging_id: "packaging-a",
            location_id: "location-a",
            quantity: "12.3400".into(),
            disposition: "held",
            lifecycle: "open",
        };
        let original = source.clone();
        let source_packaging = Packaging {
            id: "packaging-a",
            location_id: "location-a",
            lifecycle: "open",
        };
        let destination = Packaging {
            id: "packaging-b",
            location_id: "location-b",
            lifecycle: "open",
        };
        let command = Command {
            new_inventory_id: "new",
            quantity: "2.34",
            to_packaging_id: "packaging-b",
            to_location_id: "location-b",
        };
        let transition = decide(
            State {
                source: &source,
                source_packaging: Some(source_packaging),
                destination: Some(destination),
            },
            command,
        )
        .unwrap();
        assert_eq!(source, original);
        assert_eq!(transition.inventory[0].quantity, "10.0000");
        assert_eq!(transition.inventory[1].quantity, "2.34");
        assert_eq!(transition.inventory[1].disposition, "held");
        assert_eq!(transition.inventory[1].lifecycle, "open");
        assert_eq!(transition.transactions[0].from_inventory, Some(original));
        assert_eq!(transition.transactions[1].from_inventory, None);
        assert_eq!(transition.transactions[1].from_inventory_id, "source");
        assert_eq!(transition.transactions[1].to_inventory_id, "new");
        assert_eq!(
            transition.transactions[1].to_inventory.location_id,
            "location-b"
        );

        let closed = Inventory {
            lifecycle: "closed",
            ..source
        };
        let unchanged = closed.clone();
        let refusal = decide(
            State {
                source: &closed,
                source_packaging: None,
                destination: None,
            },
            command,
        )
        .unwrap_err();
        assert_eq!(refusal.r#type, RefusalType::ClosedInventory);
        assert_eq!(closed, unchanged);
    }
}

//! Pure packaging relocation decisions over loaded business state.
//!
//! Identities and quantities are opaque: relocation compares identities and
//! preserves quantities without parsing them. Revisions and persistence are
//! outside this module. The caller supplies a stable inventory membership set.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packaging<'a> {
    pub id: &'a str,
    pub location_id: &'a str,
    pub lifecycle: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inventory<'a> {
    pub id: &'a str,
    pub product_id: &'a str,
    pub packaging_id: &'a str,
    pub location_id: &'a str,
    pub quantity: &'a str,
    pub disposition: &'a str,
    pub lifecycle: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct State<'a> {
    pub packaging: Packaging<'a>,
    pub inventory: &'a [Inventory<'a>],
    pub destination_exists: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Command<'a> {
    pub to_location_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalType {
    ClosedPackaging,
    NoOp,
    NotColocated,
    MissingDestination,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub r#type: RefusalType,
}

/// One required immutable history row for an affected inventory identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryTransaction<'a> {
    pub from_inventory: Inventory<'a>,
    pub to_inventory: Inventory<'a>,
}

/// Required state updates and history, without persistence or command claims.
#[derive(Debug, PartialEq, Eq)]
pub struct Transition<'a> {
    pub from_packaging: Packaging<'a>,
    pub to_packaging: Packaging<'a>,
    pub inventory: Vec<Inventory<'a>>,
    pub transactions: Vec<InventoryTransaction<'a>>,
}

/// Preserve lifecycle refusal precedence across the caller's concurrency checks.
pub fn require_open(packaging: Packaging<'_>) -> Result<(), Refusal> {
    if packaging.lifecycle != "open" {
        return Err(Refusal {
            r#type: RefusalType::ClosedPackaging,
        });
    }
    Ok(())
}

/// Decide all business changes without mutating the loaded state.
pub fn decide<'a>(state: State<'a>, command: Command<'a>) -> Result<Transition<'a>, Refusal> {
    require_open(state.packaging)?;
    if state.packaging.location_id == command.to_location_id {
        return Err(Refusal {
            r#type: RefusalType::NoOp,
        });
    }
    for item in state.inventory {
        if item.packaging_id == state.packaging.id
            && item.lifecycle == "open"
            && item.location_id != state.packaging.location_id
        {
            return Err(Refusal {
                r#type: RefusalType::NotColocated,
            });
        }
    }
    if !state.destination_exists {
        return Err(Refusal {
            r#type: RefusalType::MissingDestination,
        });
    }
    let mut transition = Transition {
        from_packaging: state.packaging,
        to_packaging: Packaging {
            location_id: command.to_location_id,
            ..state.packaging
        },
        inventory: Vec::new(),
        transactions: Vec::new(),
    };
    for item in state.inventory {
        if item.packaging_id == state.packaging.id && item.lifecycle == "open" {
            let relocated = Inventory {
                location_id: command.to_location_id,
                ..*item
            };
            transition.inventory.push(relocated);
            transition.transactions.push(InventoryTransaction {
                from_inventory: *item,
                to_inventory: relocated,
            });
        }
    }
    Ok(transition)
}

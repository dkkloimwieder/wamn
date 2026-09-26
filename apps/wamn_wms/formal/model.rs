//! Finite inventory identities, packaging context, and immutable transactions.
#![crate_type = "lib"]

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    Available,
    Held,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Open,
    Closed,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackagingType {
    Pallet,
    Tote,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Packaging {
    id: bool,
    r#type: PackagingType,
    code: bool,
    location_id: bool,
    lifecycle: Lifecycle,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Inventory {
    id: bool,
    product_id: bool,
    packaging_id: bool,
    location_id: bool,
    quantity: u8,
    disposition: Disposition,
    lifecycle: Lifecycle,
}

type Inventories = [Option<Inventory>; 2];

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Type {
    Move,
    Adjust,
    Split,
    Merge,
    RelocatePackaging,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Move {
        inventory_id: bool,
        to_packaging_id: bool,
        to_location_id: bool,
    },
    Adjust {
        inventory_id: bool,
        to_quantity: u8,
    },
    Split {
        from_inventory_id: bool,
        quantity: u8,
        to_packaging_id: bool,
        to_location_id: bool,
    },
    Merge {
        from_inventory_id: bool,
        to_inventory_id: bool,
    },
    RelocatePackaging {
        packaging_id: bool,
        to_location_id: bool,
    },
    ClosePackaging {
        packaging_id: bool,
    },
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Command {
    key: bool,
    action: Action,
    occurred_at: bool,
    // Two opaque nonempty reason values, or no supplied reason.
    reason: Option<bool>,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InventoryTransaction {
    id: u8,
    operation_id: bool,
    r#type: Type,
    inventory_id: bool,
    from_inventory_id: bool,
    to_inventory_id: bool,
    from_product_id: Option<bool>,
    to_product_id: bool,
    from_packaging_id: Option<bool>,
    to_packaging_id: bool,
    from_location_id: Option<bool>,
    to_location_id: bool,
    from_quantity: u8,
    to_quantity: u8,
    from_disposition: Option<Disposition>,
    to_disposition: Disposition,
    from_lifecycle: Option<Lifecycle>,
    to_lifecycle: Lifecycle,
    occurred_at: bool,
    reason: Option<bool>,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandResult {
    operation_id: bool,
    inventory: Inventories,
    packaging: [Packaging; 2],
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Operation {
    command: Command,
    result: CommandResult,
    transactions: [Option<InventoryTransaction>; 2],
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct State {
    inventory: Inventories,
    packaging: [Packaging; 2],
    operations: [Option<Operation>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Refusal {
    InvalidInput,
    IntentConflict,
    Missing,
    ClosedInventory,
    ClosedPackaging,
    PackagingNotEmpty,
    DispositionMismatch,
    Quantity,
    NoOp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Accepted(CommandResult),
    Replayed(CommandResult),
    Refused(Refusal),
    Aborted,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    None,
    BusinessState,
    History,
    StoredResult,
}

fn total(inventory: Inventories) -> u16 {
    inventory
        .iter()
        .flatten()
        .map(|item| u16::from(item.quantity))
        .sum()
}

fn empty(inventory: Inventories, packaging_id: bool) -> bool {
    !inventory
        .iter()
        .flatten()
        .any(|item| item.lifecycle == Lifecycle::Open && item.packaging_id == packaging_id)
}

fn valid_business(state: State) -> bool {
    for id in [false, true] {
        let packaging = state.packaging[usize::from(id)];
        if packaging.id != id
            || (packaging.lifecycle == Lifecycle::Closed && !empty(state.inventory, id))
        {
            return false;
        }
        if state.inventory[usize::from(id)].is_some_and(|item| {
            item.id != id
                || item.quantity > 6
                || (item.lifecycle == Lifecycle::Open
                    && item.location_id
                        != state.packaging[usize::from(item.packaging_id)].location_id)
                || (item.lifecycle == Lifecycle::Closed) != (item.quantity == 0)
        }) {
            return false;
        }
    }
    if matches!(state.inventory, [Some(first), Some(second)] if first.product_id != second.product_id)
    {
        return false;
    }
    total(state.inventory) <= 6
}

// History correctness is a separate induction over complete appends and unchanged prefixes.
fn valid(state: State) -> bool {
    if !valid_business(state)
        || state
            .operations
            .iter()
            .flatten()
            .any(|operation| !prepared(operation.command))
    {
        return false;
    }
    match state.operations {
        [None, None] => true,
        [Some(first), None] => !first.result.operation_id,
        [Some(first), Some(second)] => {
            !first.result.operation_id
                && second.result.operation_id
                && first.command.key != second.command.key
        }
        [None, Some(_)] => false,
    }
}

fn initial(inventory: Inventories, packaging: [Packaging; 2]) -> State {
    State {
        inventory,
        packaging,
        operations: [None; 2],
    }
}

// Capacity restrictions bound this experiment, not the target business rules.
fn command_domain(state: State, command: Command) -> bool {
    match command.action {
        Action::Adjust {
            inventory_id,
            to_quantity,
        } => {
            let current =
                state.inventory[usize::from(inventory_id)].map_or(0, |item| item.quantity);
            to_quantity <= 6
                && total(state.inventory) - u16::from(current) + u16::from(to_quantity) <= 6
        }
        Action::Split {
            from_inventory_id,
            quantity,
            ..
        } => {
            quantity <= 7
                && (state.inventory[usize::from(!from_inventory_id)].is_none()
                    || state
                        .operations
                        .iter()
                        .flatten()
                        .any(|operation| operation.command.key == command.key))
        }
        _ => true,
    }
}

fn operation_type(action: Action) -> Option<Type> {
    match action {
        Action::Move { .. } => Some(Type::Move),
        Action::Adjust { .. } => Some(Type::Adjust),
        Action::Split { .. } => Some(Type::Split),
        Action::Merge { .. } => Some(Type::Merge),
        Action::RelocatePackaging { .. } => Some(Type::RelocatePackaging),
        Action::ClosePackaging { .. } => None,
    }
}

fn prepared(command: Command) -> bool {
    match command.action {
        Action::Adjust { to_quantity, .. } => to_quantity > 0 && command.reason.is_some(),
        Action::Split { quantity, .. } => quantity > 0,
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => from_inventory_id != to_inventory_id,
        _ => true,
    }
}

fn active(inventory: Inventories, id: bool) -> Result<Inventory, Refusal> {
    let item = inventory[usize::from(id)].ok_or(Refusal::Missing)?;
    if item.lifecycle == Lifecycle::Closed {
        return Err(Refusal::ClosedInventory);
    }
    Ok(item)
}

fn open_packaging(packaging: [Packaging; 2], id: bool) -> Result<(), Refusal> {
    if packaging[usize::from(id)].lifecycle == Lifecycle::Closed {
        return Err(Refusal::ClosedPackaging);
    }
    Ok(())
}

fn change(state: &mut State, action: Action) -> Result<(), Refusal> {
    match action {
        Action::Move {
            inventory_id,
            to_packaging_id,
            to_location_id,
        } => {
            let mut item = active(state.inventory, inventory_id)?;
            open_packaging(state.packaging, to_packaging_id)?;
            if to_location_id != state.packaging[usize::from(to_packaging_id)].location_id {
                return Err(Refusal::InvalidInput);
            }
            item.packaging_id = to_packaging_id;
            item.location_id = to_location_id;
            state.inventory[usize::from(inventory_id)] = Some(item);
        }
        Action::Adjust {
            inventory_id,
            to_quantity,
        } => {
            let mut item = active(state.inventory, inventory_id)?;
            item.quantity = to_quantity;
            state.inventory[usize::from(inventory_id)] = Some(item);
        }
        Action::Split {
            from_inventory_id,
            quantity,
            to_packaging_id,
            to_location_id,
        } => {
            let mut item = active(state.inventory, from_inventory_id)?;
            open_packaging(state.packaging, to_packaging_id)?;
            if to_location_id != state.packaging[usize::from(to_packaging_id)].location_id {
                return Err(Refusal::InvalidInput);
            }
            if quantity >= item.quantity {
                return Err(Refusal::Quantity);
            }
            item.quantity -= quantity;
            state.inventory[usize::from(from_inventory_id)] = Some(item);
            state.inventory[usize::from(!from_inventory_id)] = Some(Inventory {
                id: !from_inventory_id,
                packaging_id: to_packaging_id,
                location_id: to_location_id,
                quantity,
                ..item
            });
        }
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => {
            let mut source = active(state.inventory, from_inventory_id)?;
            let mut target = active(state.inventory, to_inventory_id)?;
            if source.disposition != target.disposition {
                return Err(Refusal::DispositionMismatch);
            }
            target.quantity += source.quantity;
            source.quantity = 0;
            source.lifecycle = Lifecycle::Closed;
            state.inventory[usize::from(from_inventory_id)] = Some(source);
            state.inventory[usize::from(to_inventory_id)] = Some(target);
        }
        Action::RelocatePackaging {
            packaging_id,
            to_location_id,
        } => {
            open_packaging(state.packaging, packaging_id)?;
            if state.packaging[usize::from(packaging_id)].location_id == to_location_id {
                return Err(Refusal::NoOp);
            }
            state.packaging[usize::from(packaging_id)].location_id = to_location_id;
            for item in state.inventory.iter_mut().flatten() {
                if item.lifecycle == Lifecycle::Open && item.packaging_id == packaging_id {
                    item.location_id = to_location_id;
                }
            }
        }
        Action::ClosePackaging { packaging_id } => {
            if !empty(state.inventory, packaging_id) {
                return Err(Refusal::PackagingNotEmpty);
            }
            state.packaging[usize::from(packaging_id)].lifecycle = Lifecycle::Closed;
        }
    }
    Ok(())
}

fn affected(inventory: Inventories, action: Action, id: bool) -> bool {
    match action {
        Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
            id == inventory_id
        }
        Action::Split { .. } | Action::Merge { .. } => true,
        Action::RelocatePackaging { packaging_id, .. } => {
            inventory[usize::from(id)].is_some_and(|item| {
                item.lifecycle == Lifecycle::Open && item.packaging_id == packaging_id
            })
        }
        Action::ClosePackaging { .. } => false,
    }
}

fn transactions(
    from_state: State,
    to_state: State,
    command: Command,
    operation_id: bool,
) -> [Option<InventoryTransaction>; 2] {
    let mut rows = [None; 2];
    let Some(r#type) = operation_type(command.action) else {
        return rows;
    };
    for id in [false, true] {
        if !affected(from_state.inventory, command.action, id) {
            continue;
        }
        let from_item = from_state.inventory[usize::from(id)];
        let to_item = to_state.inventory[usize::from(id)].unwrap();
        let (from_inventory_id, to_inventory_id) = match command.action {
            Action::Split {
                from_inventory_id, ..
            } => (from_inventory_id, id),
            Action::Merge {
                from_inventory_id,
                to_inventory_id,
            } => (from_inventory_id, to_inventory_id),
            _ => (id, id),
        };
        rows[usize::from(id)] = Some(InventoryTransaction {
            id: u8::from(operation_id) * 2 + u8::from(id),
            operation_id,
            r#type,
            inventory_id: id,
            from_inventory_id,
            to_inventory_id,
            from_product_id: from_item.map(|item| item.product_id),
            to_product_id: to_item.product_id,
            from_packaging_id: from_item.map(|item| item.packaging_id),
            to_packaging_id: to_item.packaging_id,
            from_location_id: from_item.map(|item| item.location_id),
            to_location_id: to_item.location_id,
            from_quantity: from_item.map_or(0, |item| item.quantity),
            to_quantity: to_item.quantity,
            from_disposition: from_item.map(|item| item.disposition),
            to_disposition: to_item.disposition,
            from_lifecycle: from_item.map(|item| item.lifecycle),
            to_lifecycle: to_item.lifecycle,
            occurred_at: command.occurred_at,
            reason: command.reason,
        });
    }
    rows
}

fn execute(state: &mut State, command: Command) -> Outcome {
    execute_with_failure(state, command, Failure::None)
}

// Staged work is invisible until the final business commit. No persistence mechanism is modeled.
fn execute_with_failure(state: &mut State, command: Command, failure: Failure) -> Outcome {
    if !prepared(command) {
        return Outcome::Refused(Refusal::InvalidInput);
    }
    for operation in state.operations.iter().flatten() {
        if operation.command.key == command.key {
            return if operation.command == command {
                Outcome::Replayed(operation.result)
            } else {
                Outcome::Refused(Refusal::IntentConflict)
            };
        }
    }
    let from_state = *state;
    let mut to_state = from_state;
    if let Err(refusal) = change(&mut to_state, command.action) {
        return Outcome::Refused(refusal);
    }
    if failure == Failure::BusinessState {
        return Outcome::Aborted;
    }
    let operation_id = from_state.operations[0].is_some();
    assert!(from_state.operations[usize::from(operation_id)].is_none());
    let transactions = transactions(from_state, to_state, command, operation_id);
    if failure == Failure::History {
        return Outcome::Aborted;
    }
    let result = CommandResult {
        operation_id,
        inventory: to_state.inventory,
        packaging: to_state.packaging,
    };
    to_state.operations[usize::from(operation_id)] = Some(Operation {
        command,
        result,
        transactions,
    });
    if failure == Failure::StoredResult {
        return Outcome::Aborted;
    }
    *state = to_state;
    Outcome::Accepted(result)
}

#[cfg(kani)]
mod proofs;
#[cfg(test)]
mod tests;

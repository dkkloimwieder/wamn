//! Finite WMS inventory model with immutable operation transactions.
#![crate_type = "lib"]

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Available,
    Held,
    Closed,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Inventory {
    id: bool,
    product_id: bool,
    location_id: bool,
    quantity: u8,
    status: Status,
}

type Inventories = [Option<Inventory>; 2];

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Type {
    Move,
    Adjust,
    Split,
    Merge,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Move {
        inventory_id: bool,
        to_location_id: bool,
    },
    Adjust {
        inventory_id: bool,
        to_quantity: u8,
        reason_present: bool,
    },
    Split {
        from_inventory_id: bool,
        quantity: u8,
        to_location_id: bool,
    },
    Merge {
        from_inventory_id: bool,
        to_inventory_id: bool,
    },
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Command {
    key: bool,
    action: Action,
    occurred_at: bool,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InventoryTransaction {
    id: u8,
    operation_id: bool,
    r#type: Type,
    from_inventory_id: bool,
    to_inventory_id: bool,
    from_product_id: Option<bool>,
    to_product_id: bool,
    from_location_id: Option<bool>,
    to_location_id: bool,
    from_quantity: u8,
    to_quantity: u8,
    from_status: Option<Status>,
    to_status: Status,
    occurred_at: bool,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandResult {
    operation_id: bool,
    inventory: Inventories,
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
    operations: [Option<Operation>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Refusal {
    InvalidInput,
    IntentConflict,
    Missing,
    Closed,
    Quantity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Accepted(CommandResult),
    Replayed(CommandResult),
    Refused(Refusal),
}

fn total(inventory: Inventories) -> u16 {
    inventory
        .iter()
        .flatten()
        .map(|item| u16::from(item.quantity))
        .sum()
}

fn valid_inventory(inventory: Inventories) -> bool {
    for (id, item) in inventory.iter().enumerate() {
        if item.is_some_and(|item| {
            usize::from(item.id) != id
                || item.quantity > 6
                || (item.status == Status::Closed) != (item.quantity == 0)
        }) {
            return false;
        }
    }
    if matches!(inventory, [Some(first), Some(second)] if first.product_id != second.product_id
        || (first.status != Status::Closed && second.status != Status::Closed && first.status != second.status))
    {
        return false;
    }
    total(inventory) <= 6
}

// Log content correctness is inductive: preserve every prefix and explain each append.
fn valid(state: State) -> bool {
    if !valid_inventory(state.inventory) {
        return false;
    }
    if state
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

fn initial(inventory: Inventories) -> State {
    State {
        inventory,
        operations: [None; 2],
    }
}

// Capacity restrictions describe this experiment, not new production refusals.
fn command_domain(state: State, command: Command) -> bool {
    match command.action {
        Action::Adjust {
            inventory_id,
            to_quantity,
            ..
        } => {
            to_quantity <= 6
                && u16::from(to_quantity)
                    + u16::from(
                        state.inventory[usize::from(!inventory_id)].map_or(0, |item| item.quantity),
                    )
                    <= 6
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

fn operation_type(action: Action) -> Type {
    match action {
        Action::Move { .. } => Type::Move,
        Action::Adjust { .. } => Type::Adjust,
        Action::Split { .. } => Type::Split,
        Action::Merge { .. } => Type::Merge,
    }
}

fn prepared(command: Command) -> bool {
    match command.action {
        Action::Adjust {
            to_quantity,
            reason_present,
            ..
        } => to_quantity > 0 && reason_present,
        Action::Split { quantity, .. } => quantity > 0,
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => from_inventory_id != to_inventory_id,
        Action::Move { .. } => true,
    }
}

fn active(inventory: Inventories, id: bool) -> Result<Inventory, Refusal> {
    let item = inventory[usize::from(id)].ok_or(Refusal::Missing)?;
    if item.status == Status::Closed {
        return Err(Refusal::Closed);
    }
    Ok(item)
}

fn change(from_inventory: Inventories, action: Action) -> Result<Inventories, Refusal> {
    let mut to_inventory = from_inventory;
    match action {
        Action::Move {
            inventory_id,
            to_location_id,
        } => {
            let mut item = active(from_inventory, inventory_id)?;
            if item.location_id == to_location_id {
                return Err(Refusal::InvalidInput);
            }
            item.location_id = to_location_id;
            to_inventory[usize::from(inventory_id)] = Some(item);
        }
        Action::Adjust {
            inventory_id,
            to_quantity,
            ..
        } => {
            let mut item = active(from_inventory, inventory_id)?;
            item.quantity = to_quantity;
            to_inventory[usize::from(inventory_id)] = Some(item);
        }
        Action::Split {
            from_inventory_id,
            quantity,
            to_location_id,
        } => {
            let mut item = active(from_inventory, from_inventory_id)?;
            if quantity >= item.quantity {
                return Err(Refusal::Quantity);
            }
            item.quantity -= quantity;
            to_inventory[usize::from(from_inventory_id)] = Some(item);
            to_inventory[usize::from(!from_inventory_id)] = Some(Inventory {
                id: !from_inventory_id,
                quantity,
                location_id: to_location_id,
                ..item
            });
        }
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => {
            let mut from_item = active(from_inventory, from_inventory_id)?;
            let mut to_item = active(from_inventory, to_inventory_id)?;
            to_item.quantity += from_item.quantity;
            from_item.quantity = 0;
            from_item.status = Status::Closed;
            to_inventory[usize::from(from_inventory_id)] = Some(from_item);
            to_inventory[usize::from(to_inventory_id)] = Some(to_item);
        }
    }
    Ok(to_inventory)
}

fn affected(action: Action, id: bool) -> bool {
    match action {
        Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => {
            id == inventory_id
        }
        Action::Split { .. } | Action::Merge { .. } => true,
    }
}

fn transactions(
    from_inventory: Inventories,
    to_inventory: Inventories,
    command: Command,
    operation_id: bool,
) -> [Option<InventoryTransaction>; 2] {
    let mut rows = [None; 2];
    for id in [false, true] {
        if !affected(command.action, id) {
            continue;
        }
        let from_item = from_inventory[usize::from(id)];
        let to_item = to_inventory[usize::from(id)].unwrap();
        let (from_inventory_id, to_inventory_id) = match command.action {
            Action::Split {
                from_inventory_id, ..
            } => (from_inventory_id, id),
            Action::Merge {
                to_inventory_id, ..
            } => (id, to_inventory_id),
            _ => (id, id),
        };
        rows[usize::from(id)] = Some(InventoryTransaction {
            id: u8::from(operation_id) * 2 + u8::from(id),
            operation_id,
            r#type: operation_type(command.action),
            from_inventory_id,
            to_inventory_id,
            from_product_id: from_item.map(|item| item.product_id),
            to_product_id: to_item.product_id,
            from_location_id: from_item.map(|item| item.location_id),
            to_location_id: to_item.location_id,
            from_quantity: from_item.map_or(0, |item| item.quantity),
            to_quantity: to_item.quantity,
            from_status: from_item.map(|item| item.status),
            to_status: to_item.status,
            occurred_at: command.occurred_at,
        });
    }
    rows
}

fn execute(state: &mut State, command: Command) -> Outcome {
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
    let to_inventory = match change(state.inventory, command.action) {
        Ok(inventory) => inventory,
        Err(refusal) => return Outcome::Refused(refusal),
    };
    let operation_id = state.operations[0].is_some();
    assert!(state.operations[usize::from(operation_id)].is_none());
    let result = CommandResult {
        operation_id,
        inventory: to_inventory,
    };
    let operation = Operation {
        command,
        result,
        transactions: transactions(state.inventory, to_inventory, command, operation_id),
    };
    state.inventory = to_inventory;
    state.operations[usize::from(operation_id)] = Some(operation);
    Outcome::Accepted(result)
}

#[cfg(kani)]
mod proofs;
#[cfg(test)]
mod tests;

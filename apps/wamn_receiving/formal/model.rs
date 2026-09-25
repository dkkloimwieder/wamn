//! Independent finite business model for one Receiving receipt item.
#![crate_type = "lib"]

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Open,
    Complete,
    Cancelled,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Command {
    key: bool,
    lines: [Option<u8>; 2],
    intent: bool,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResultSnapshot {
    receipt: bool,
    status: Status,
    revision: u8,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Claim {
    command: Command,
    result: ResultSnapshot,
}

#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct State {
    ordered: [u8; 2],
    received: [u8; 2],
    status: Status,
    revision: u8,
    claims: [Option<Claim>; 2],
    receipts: [Option<[u8; 2]>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Accepted(ResultSnapshot),
    Replayed(ResultSnapshot),
    InvalidInput,
    IntentConflict,
    OrderNotOpen,
    ExcessQuantity,
}

fn initial(ordered: [u8; 2], cancelled: bool) -> State {
    State {
        ordered,
        received: [0; 2],
        status: if cancelled {
            Status::Cancelled
        } else {
            Status::Open
        },
        revision: 1,
        claims: [None; 2],
        receipts: [None; 2],
    }
}

fn quantities(command: Command) -> [u8; 2] {
    [command.lines[0].unwrap_or(0), command.lines[1].unwrap_or(0)]
}

fn prepared(command: Command) -> bool {
    command.lines != [None; 2] && command.lines.iter().all(|quantity| *quantity != Some(0))
}

// A finite input domain, not a new business validation rule.
fn command_domain(command: Command) -> bool {
    command
        .lines
        .iter()
        .all(|quantity| quantity.unwrap_or(0) <= 4)
}

fn valid(state: State) -> bool {
    if !(1..=3).contains(&state.ordered[0]) || !(1..=3).contains(&state.ordered[1]) {
        return false;
    }
    if state.received[0] > state.ordered[0] || state.received[1] > state.ordered[1] {
        return false;
    }
    let complete = state.received == state.ordered;
    if (state.status == Status::Complete) != complete {
        return false;
    }
    let mut totals = [0u16; 2];
    let mut count = 0;
    for key in 0..2 {
        match (state.claims[key], state.receipts[key]) {
            (None, None) => (),
            (Some(claim), Some(effect)) => {
                if usize::from(claim.command.key) != key
                    || claim.result.receipt != claim.command.key
                    || !prepared(claim.command)
                    || !command_domain(claim.command)
                    || effect != quantities(claim.command)
                    || !(2..=state.revision).contains(&claim.result.revision)
                    || claim.result.status == Status::Cancelled
                {
                    return false;
                }
                let final_receipt = claim.result.revision == state.revision && complete;
                if (claim.result.status == Status::Complete) != final_receipt {
                    return false;
                }
                totals[0] += u16::from(effect[0]);
                totals[1] += u16::from(effect[1]);
                count += 1;
            }
            _ => return false,
        }
    }
    if matches!((state.claims[0], state.claims[1]),
        (Some(first), Some(second)) if first.result.revision == second.result.revision)
    {
        return false;
    }
    totals == [u16::from(state.received[0]), u16::from(state.received[1])]
        && state.revision == 1 + count
        && (state.status != Status::Cancelled || count == 0)
}

fn record_receipt(state: &mut State, command: Command) -> Outcome {
    if !prepared(command) {
        return Outcome::InvalidInput;
    }
    let key = usize::from(command.key);
    if let Some(claim) = state.claims[key] {
        return if claim.command == command {
            Outcome::Replayed(claim.result)
        } else {
            Outcome::IntentConflict
        };
    }
    if state.status != Status::Open {
        return Outcome::OrderNotOpen;
    }
    let effect = quantities(command);
    for (line, quantity) in effect.iter().enumerate() {
        if *quantity > state.ordered[line] - state.received[line] {
            return Outcome::ExcessQuantity;
        }
    }
    for (line, quantity) in effect.iter().enumerate() {
        state.received[line] += quantity;
    }
    state.status = if state.received == state.ordered {
        Status::Complete
    } else {
        Status::Open
    };
    state.revision += 1;
    let result = ResultSnapshot {
        receipt: command.key,
        status: state.status,
        revision: state.revision,
    };
    state.receipts[key] = Some(effect);
    state.claims[key] = Some(Claim { command, result });
    Outcome::Accepted(result)
}

#[cfg(kani)]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_keeps_original_result_after_completion() {
        let mut state = initial([2, 1], false);
        let first = Command {
            key: false,
            lines: [Some(1), Some(1)],
            intent: false,
        };
        let Outcome::Accepted(original) = record_receipt(&mut state, first) else {
            panic!("first receipt refused")
        };
        assert_eq!(original.status, Status::Open);
        let last = Command {
            key: true,
            lines: [Some(1), None],
            intent: false,
        };
        assert!(matches!(
            record_receipt(&mut state, last),
            Outcome::Accepted(_)
        ));
        assert_eq!(state.status, Status::Complete);
        let before = state;
        assert_eq!(
            record_receipt(&mut state, first),
            Outcome::Replayed(original)
        );
        assert_eq!(state, before);
        assert!(valid(state));
    }

    #[test]
    fn invalid_second_line_preserves_the_entire_item() {
        let mut state = initial([2, 1], false);
        let before = state;
        let command = Command {
            key: false,
            lines: [Some(1), Some(2)],
            intent: false,
        };
        assert_eq!(record_receipt(&mut state, command), Outcome::ExcessQuantity);
        assert_eq!(state, before);
    }

    #[test]
    fn changed_valid_intent_refuses_but_invalid_input_precedes_conflict() {
        let mut state = initial([2, 1], false);
        let command = Command {
            key: false,
            lines: [Some(1), None],
            intent: false,
        };
        assert!(matches!(
            record_receipt(&mut state, command),
            Outcome::Accepted(_)
        ));
        let before = state;
        assert_eq!(
            record_receipt(
                &mut state,
                Command {
                    intent: true,
                    ..command
                }
            ),
            Outcome::IntentConflict
        );
        assert_eq!(
            record_receipt(
                &mut state,
                Command {
                    lines: [Some(0), None],
                    ..command
                }
            ),
            Outcome::InvalidInput
        );
        assert_eq!(state, before);
    }
}

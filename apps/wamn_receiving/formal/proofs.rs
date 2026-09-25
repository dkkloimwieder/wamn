//! Proofs over the explicitly bounded Receiving model, not production code.
use super::*;

fn arbitrary_state() -> State {
    let state = kani::any();
    kani::assume(valid(state));
    state
}

fn arbitrary_command() -> Command {
    let command = kani::any();
    kani::assume(command_domain(command));
    command
}

#[kani::proof]
#[kani::unwind(64)]
fn initialization() {
    let ordered: [u8; 2] = kani::any();
    kani::assume((1..=3).contains(&ordered[0]) && (1..=3).contains(&ordered[1]));
    let state = initial(ordered, kani::any());
    assert!(valid(state));
    kani::cover!(state.status == Status::Open, "open initialization");
    kani::cover!(
        state.status == Status::Cancelled,
        "cancelled fixture initialization"
    );
}

// REC-INV-01, REC-INV-02, REC-LIFE-02, including all auxiliary validity facts.
#[kani::proof]
#[kani::unwind(64)]
fn transition_preserves_validity() {
    let mut state = arbitrary_state();
    let outcome = record_receipt(&mut state, arbitrary_command());
    assert!(valid(state));
    kani::cover!(matches!(outcome, Outcome::Accepted(_)), "new receipt");
    kani::cover!(matches!(outcome, Outcome::Replayed(_)), "exact replay");
    kani::cover!(outcome == Outcome::ExcessQuantity, "excess refusal");
    kani::cover!(state.status == Status::Complete, "complete order");
}

// REC-CMD-01 and REC-LIFE-01 distinguish new effects from replay.
#[kani::proof]
#[kani::unwind(64)]
fn new_receipt_effect() {
    let before = arbitrary_state();
    let mut after = before;
    let command = arbitrary_command();
    let outcome = record_receipt(&mut after, command);
    if let Outcome::Accepted(result) = outcome {
        assert_eq!(before.status, Status::Open);
        assert!(before.claims[usize::from(command.key)].is_none());
        for line in 0..2 {
            assert_eq!(
                after.received[line],
                before.received[line] + command.lines[line].unwrap_or(0)
            );
        }
        assert_eq!(after.ordered, before.ordered);
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(
            after.status == Status::Complete,
            after.received == after.ordered
        );
        assert_eq!(result.status, after.status);
        assert_eq!(result.revision, after.revision);
        assert_eq!(result.receipt, command.key);
        let other = usize::from(!command.key);
        assert_eq!(after.claims[other], before.claims[other]);
        assert_eq!(after.receipts[other], before.receipts[other]);
    }
    kani::cover!(
        matches!(outcome, Outcome::Accepted(_)) && after.status == Status::Open,
        "partial receipt"
    );
    kani::cover!(
        matches!(outcome, Outcome::Accepted(_)) && after.status == Status::Complete,
        "completing receipt"
    );
}

// REC-CMD-02, REC-CMD-03, REC-CMD-04 and REC-IDEM-02.
#[kani::proof]
#[kani::unwind(64)]
fn refusal_preserves_state() {
    let before = arbitrary_state();
    let mut after = before;
    let command = arbitrary_command();
    let outcome = record_receipt(&mut after, command);
    if !matches!(outcome, Outcome::Accepted(_) | Outcome::Replayed(_)) {
        assert_eq!(after, before);
    }
    if !prepared(command) {
        assert_eq!(outcome, Outcome::InvalidInput);
    } else if let Some(claim) = before.claims[usize::from(command.key)] {
        if claim.command != command {
            assert_eq!(outcome, Outcome::IntentConflict);
        }
    } else if before.status != Status::Open {
        assert_eq!(outcome, Outcome::OrderNotOpen);
    } else {
        let effect = quantities(command);
        if effect[0] > before.ordered[0] - before.received[0]
            || effect[1] > before.ordered[1] - before.received[1]
        {
            assert_eq!(outcome, Outcome::ExcessQuantity);
        } else {
            assert!(matches!(outcome, Outcome::Accepted(_)));
        }
    }
    kani::cover!(outcome == Outcome::InvalidInput, "invalid input");
    kani::cover!(outcome == Outcome::IntentConflict, "changed valid intent");
    kani::cover!(outcome == Outcome::OrderNotOpen, "order not open");
    kani::cover!(
        outcome == Outcome::ExcessQuantity
            && command.lines[0].is_some()
            && command.lines[1].is_some(),
        "atomic multi-line refusal"
    );
}

// REC-IDEM-01 covers claims in any valid state, including later receipts.
#[kani::proof]
#[kani::unwind(64)]
fn replay_returns_original_result() {
    let before = arbitrary_state();
    let key: bool = kani::any();
    kani::assume(before.claims[usize::from(key)].is_some());
    let claim = before.claims[usize::from(key)].unwrap();
    let mut after = before;
    assert_eq!(
        record_receipt(&mut after, claim.command),
        Outcome::Replayed(claim.result)
    );
    assert_eq!(after, before);
    kani::cover!(
        before.status == Status::Complete && claim.result.status == Status::Open,
        "old open result after completion"
    );
}

// REC-INV-01 from an initial state, with two distinct claims on one line.
#[kani::proof]
#[kani::unwind(64)]
fn two_receipts_preserve_quantity() {
    let ordered: u8 = kani::any();
    let first: u8 = kani::any();
    let second: u8 = kani::any();
    kani::assume((1..=3).contains(&ordered) && first <= 4 && second <= 4);
    let mut state = initial([ordered, 1], false);
    record_receipt(
        &mut state,
        Command {
            key: false,
            lines: [Some(first), None],
            intent: false,
        },
    );
    record_receipt(
        &mut state,
        Command {
            key: true,
            lines: [Some(second), None],
            intent: false,
        },
    );
    assert!(state.received[0] <= ordered);
    assert!(valid(state));
    kani::cover!(
        state.claims.iter().all(Option::is_some),
        "two distinct accepted receipts"
    );
}

//! Guards D23's current wash-runtime fork-governance posture.

const PLATFORM_PLAN: &str = include_str!("../../../docs/platform-plan.md");
const AUTHORITATIVE_LEDGER_STATEMENT: &str =
    "`docs/wash-runtime-fork.md` is the authoritative branch, revision, and carried-policy ledger";

#[test]
fn d23_current_state_rejects_wamn_2_5_2_fork_restoration() {
    let d23_rows = PLATFORM_PLAN
        .lines()
        .filter(|line| line.starts_with("| D23 |"))
        .collect::<Vec<_>>();

    assert_eq!(
        d23_rows.len(),
        1,
        "platform plan must contain exactly one D23 decision row"
    );

    let d23 = d23_rows[0];
    assert!(
        d23.contains("`wamn/2.6.0`"),
        "D23 current state must name the v2.6.0 fork branch"
    );
    assert!(
        !d23.contains("`wamn/2.5.2`"),
        "D23 current state must not restore the superseded wamn/2.5.2 fork branch"
    );
    assert!(
        d23.contains(AUTHORITATIVE_LEDGER_STATEMENT),
        "D23 must delegate branch, revision, and carried-policy details to the authoritative ledger"
    );
}

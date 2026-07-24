//! The catalog-version lifecycle state machine (3.4).
//!
//! A catalog version moves through four states:
//!
//! ```text
//!   Draft ──stage──▶ Staged ──apply──▶ Applied ──(superseded on next apply)──▶ Superseded
//!     ▲                 │
//!     └─────unstage─────┘
//!   Draft / Staged ──discard──▶ (removed)
//! ```
//!
//! - **Draft** — editable; the only state the designer (3.3) may mutate content in.
//! - **Staged** — a frozen candidate, awaiting apply (impact-analyzed by 11.8).
//! - **Applied** — the live schema. Exactly one per (environment, catalog) — the
//!   single-applied invariant enforced by [`super::Environment`].
//! - **Superseded** — a previously-applied version, kept as history.
//!
//! This module is the **pure transition table** (legal state changes, no
//! cross-version context). The cross-version guards — single-applied and the
//! stale-base rebase guard — live in [`super::Environment`], which owns the set
//! of versions and their current states.
//!
//! The four state names are also the values of the `state` column in
//! `deploy/sql/catalog-schema.sql`; [`State::as_sql`] is the authoritative mapping,
//! kept in lockstep with the DDL `CHECK` by a test.

/// The lifecycle state of a single catalog version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Editable; not yet applied.
    Draft,
    /// A frozen candidate awaiting apply.
    Staged,
    /// The live schema (exactly one per environment + catalog).
    Applied,
    /// A previously-applied version, kept as history.
    Superseded,
}

impl State {
    /// The storage literal for this state — the value written to the `state`
    /// column in `deploy/sql/catalog-schema.sql` (its `CHECK` lists exactly these).
    pub fn as_sql(self) -> &'static str {
        match self {
            State::Draft => "draft",
            State::Staged => "staged",
            State::Applied => "applied",
            State::Superseded => "superseded",
        }
    }

    /// Parse a state from its storage literal (see [`State::as_sql`]).
    pub fn from_sql(s: &str) -> Option<State> {
        match s {
            "draft" => Some(State::Draft),
            "staged" => Some(State::Staged),
            "applied" => Some(State::Applied),
            "superseded" => Some(State::Superseded),
            _ => None,
        }
    }

    /// The four states, in lifecycle order — the exact set the DDL `CHECK` allows.
    pub const ALL: [State; 4] = [
        State::Draft,
        State::Staged,
        State::Applied,
        State::Superseded,
    ];

    /// `true` only for [`State::Draft`] — the sole state whose catalog content
    /// the designer (3.3) may edit. Staged/Applied/Superseded are immutable.
    pub fn is_editable(self) -> bool {
        matches!(self, State::Draft)
    }

    /// `true` only for [`State::Applied`] — the live schema.
    pub fn is_live(self) -> bool {
        matches!(self, State::Applied)
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// A lifecycle action requested on a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Freeze a Draft into a Staged candidate.
    Stage,
    /// Return a Staged candidate to Draft.
    Unstage,
    /// Make a Staged candidate the live schema (demotes the prior Applied).
    Apply,
    /// Remove a Draft or Staged version.
    Discard,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Stage => "stage",
            Action::Unstage => "unstage",
            Action::Apply => "apply",
            Action::Discard => "discard",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The outcome of a legal transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The version transitions to a new state.
    State(State),
    /// The version is removed (a discarded Draft / Staged).
    Removed,
}

/// The **pure** transition table: the new outcome of applying `action` in state
/// `from`, or `None` if the transition is illegal. This ignores cross-version
/// guards (single-applied, stale-base) — [`super::Environment`] layers those on.
///
/// Note that Applied → Superseded is not a directly requestable action: a
/// version becomes Superseded only as a side effect of another version being
/// applied (handled in [`super::Environment::apply`]).
pub fn transition(from: State, action: Action) -> Option<Outcome> {
    use Action::*;
    use State::*;
    match (from, action) {
        (Draft, Stage) => Some(Outcome::State(Staged)),
        (Staged, Unstage) => Some(Outcome::State(Draft)),
        (Staged, Apply) => Some(Outcome::State(Applied)),
        (Draft | Staged, Discard) => Some(Outcome::Removed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_transitions() {
        assert_eq!(
            transition(State::Draft, Action::Stage),
            Some(Outcome::State(State::Staged))
        );
        assert_eq!(
            transition(State::Staged, Action::Unstage),
            Some(Outcome::State(State::Draft))
        );
        assert_eq!(
            transition(State::Staged, Action::Apply),
            Some(Outcome::State(State::Applied))
        );
        assert_eq!(
            transition(State::Draft, Action::Discard),
            Some(Outcome::Removed)
        );
        assert_eq!(
            transition(State::Staged, Action::Discard),
            Some(Outcome::Removed)
        );
    }

    #[test]
    fn illegal_transitions() {
        // Can't apply or unstage a Draft.
        assert_eq!(transition(State::Draft, Action::Apply), None);
        assert_eq!(transition(State::Draft, Action::Unstage), None);
        // Can't stage a Staged.
        assert_eq!(transition(State::Staged, Action::Stage), None);
        // Applied / Superseded accept no action (Superseded is terminal history;
        // Applied is only left when another version is applied).
        for a in [
            Action::Stage,
            Action::Unstage,
            Action::Apply,
            Action::Discard,
        ] {
            assert_eq!(transition(State::Applied, a), None, "applied + {a}");
            assert_eq!(transition(State::Superseded, a), None, "superseded + {a}");
        }
    }

    #[test]
    fn sql_literals_round_trip() {
        for s in State::ALL {
            assert_eq!(State::from_sql(s.as_sql()), Some(s));
        }
        assert_eq!(State::from_sql("bogus"), None);
    }

    #[test]
    fn only_draft_is_editable_only_applied_is_live() {
        assert!(State::Draft.is_editable());
        assert!(!State::Staged.is_editable());
        assert!(State::Applied.is_live());
        assert!(!State::Staged.is_live());
    }
}

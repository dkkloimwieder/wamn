//! The migration plan — an ordered list of typed DDL operations, each classified
//! by data safety, plus the confirmation / backup-checkpoint gate.
//!
//! A plan is *reviewed* then *applied* (3.2). This crate produces and classifies
//! it; the live transactional apply, versioned migration history, and rollback
//! belong to the migration engine (2.5), and the real backup mechanism to
//! hosting (2.3 / 10.3). What lives here is the **policy**: destructive DDL is
//! refused unless the caller confirms it *and* asserts a backup checkpoint was
//! taken.

/// Data-safety classification of a single operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Data-preserving and (barring an empty-table / existing-null edge case)
    /// safe to apply unattended — creating a table, adding a column or index,
    /// relaxing NOT NULL.
    Additive,
    /// Data-losing or downstream-breaking — dropping a table/column, retyping or
    /// renaming a column, tightening NOT NULL. Requires explicit confirmation
    /// and a backup checkpoint.
    Destructive,
}

impl Safety {
    pub fn is_destructive(self) -> bool {
        self == Safety::Destructive
    }
}

/// One DDL step. `sql` is the statement to run; `entity` / `field` name the
/// affected catalog objects so schema-impact analysis (11.8) can attribute the
/// change without re-parsing the SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Human-readable one-line summary, e.g. `add column receipts.received_at`.
    pub summary: String,
    /// The DDL statement (no trailing semicolon).
    pub sql: String,
    pub safety: Safety,
    /// The affected entity id.
    pub entity: String,
    /// The affected field id, if the operation is field-scoped.
    pub field: Option<String>,
    /// Optional caveat surfaced in the review (e.g. an `ADD COLUMN NOT NULL`
    /// with no default fails on a non-empty table).
    pub note: Option<String>,
}

/// An ordered, classified migration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationPlan {
    pub operations: Vec<Operation>,
}

/// The caller's acknowledgement for a plan containing destructive operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// No special acknowledgement — only an additive plan may be emitted.
    None,
    /// The caller has reviewed the destructive operations *and* asserts a backup
    /// checkpoint has been taken (the mechanism is hosting's, 2.3 / 10.3).
    ConfirmedWithBackup,
}

/// Refused because the plan is destructive and was not confirmed with a backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiresConfirmation {
    /// Summaries of the destructive operations that triggered the refusal.
    pub destructive: Vec<String>,
}

impl std::fmt::Display for RequiresConfirmation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "migration has {} destructive operation(s) requiring explicit confirmation + a backup checkpoint: {}",
            self.destructive.len(),
            self.destructive.join("; ")
        )
    }
}

impl std::error::Error for RequiresConfirmation {}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// `true` if every operation is additive.
    pub fn is_additive(&self) -> bool {
        !self.operations.iter().any(|o| o.safety.is_destructive())
    }

    /// `true` if any operation is destructive (so applying it needs confirmation
    /// + a backup checkpoint).
    pub fn requires_confirmation(&self) -> bool {
        !self.is_additive()
    }

    /// The destructive operations, in plan order.
    pub fn destructive(&self) -> impl Iterator<Item = &Operation> {
        self.operations.iter().filter(|o| o.safety.is_destructive())
    }

    /// The full DDL script, **without** the safety gate — for preview / review /
    /// impact analysis. Use [`MigrationPlan::sql`] to apply.
    pub fn preview_sql(&self) -> String {
        let mut out = String::new();
        for op in &self.operations {
            out.push_str(&op.sql);
            out.push_str(";\n");
        }
        out
    }

    /// The DDL script to apply, honoring the safety gate. An additive plan needs
    /// no confirmation; a destructive plan is refused unless `confirm` is
    /// [`Confirmation::ConfirmedWithBackup`], in which case the script is
    /// prefixed with a backup-checkpoint marker the executor (2.5) must honor.
    pub fn sql(&self, confirm: Confirmation) -> Result<String, RequiresConfirmation> {
        if self.requires_confirmation() && confirm != Confirmation::ConfirmedWithBackup {
            return Err(RequiresConfirmation {
                destructive: self.destructive().map(|o| o.summary.clone()).collect(),
            });
        }
        let mut out = String::new();
        if self.requires_confirmation() {
            out.push_str(
                "-- BACKUP CHECKPOINT REQUIRED: this migration is destructive; a backup/PITR\n\
                 -- checkpoint must be taken before these statements run (2.3 / 10.3).\n",
            );
        }
        out.push_str(&self.preview_sql());
        Ok(out)
    }

    /// A human-readable review of the plan — each operation with its safety tag
    /// and any caveat. This is the "reviewed" surface of "reviewed/applied DDL".
    pub fn report(&self) -> String {
        if self.is_empty() {
            return "no changes\n".to_string();
        }
        let mut out = String::new();
        for op in &self.operations {
            let tag = match op.safety {
                Safety::Additive => "additive   ",
                Safety::Destructive => "DESTRUCTIVE",
            };
            out.push_str(&format!("[{tag}] {}\n", op.summary));
            if let Some(note) = &op.note {
                out.push_str(&format!("             note: {note}\n"));
            }
        }
        out
    }

    pub(crate) fn push(&mut self, op: Operation) {
        self.operations.push(op);
    }
}

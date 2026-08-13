//! The migration plan — an ordered list of typed DDL operations, each classified
//! by data safety, plus an additive-only default emission boundary.
//!
//! A plan is *reviewed* then *applied* (3.2). This crate produces and classifies
//! it. The live transactional apply and versioned migration history belong to
//! the migration engine. Destructive SQL emission exists only when the crate's
//! `ops` feature is selected; the caller owns durable authorization.

/// Data-safety classification of a single operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    /// Data-preserving and (barring an empty-table / existing-null edge case)
    /// safe to apply unattended — creating a table, adding a column or index,
    /// relaxing NOT NULL.
    Additive,
    /// Data-losing or downstream-breaking — dropping a table/column, retyping or
    /// renaming a column, tightening NOT NULL.
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
#[derive(Clone, PartialEq, Eq)]
pub struct Operation {
    /// Human-readable one-line summary, e.g. `add column receipts.received_at`.
    pub summary: String,
    /// The DDL statement (no trailing semicolon).
    pub(crate) sql: String,
    pub(crate) safety: Safety,
    /// The affected entity id.
    pub entity: String,
    /// The affected field id, if the operation is field-scoped.
    pub field: Option<String>,
    /// Optional caveat surfaced in the review (e.g. an `ADD COLUMN NOT NULL`
    /// with no default fails on a non-empty table).
    pub note: Option<String>,
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operation")
            .field("summary", &self.summary)
            .field("safety", &self.safety)
            .field("entity", &self.entity)
            .field("field", &self.field)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

/// An ordered, classified migration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationPlan {
    pub operations: Vec<Operation>,
}

/// Refused because the default boundary cannot emit destructive SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructivePlan {
    /// Summaries of the destructive operations that triggered the refusal.
    pub destructive: Vec<String>,
}

impl std::fmt::Display for DestructivePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "migration has {} destructive operation(s); default SQL emission is additive-only: {}",
            self.destructive.len(),
            self.destructive.join("; ")
        )
    }
}

impl std::error::Error for DestructivePlan {}

impl Operation {
    /// Return this operation's immutable safety classification.
    pub fn safety(&self) -> Safety {
        self.safety
    }

    /// Construct an operation for ops-only classification analysis without
    /// publishing or carrying executable SQL.
    #[cfg(feature = "ops")]
    pub fn classified(summary: String, safety: Safety, entity: String) -> Self {
        Self {
            summary,
            sql: String::new(),
            safety,
            entity,
            field: None,
            note: None,
        }
    }

    /// Return this statement only when it is additive.
    pub fn additive_sql(&self) -> Option<&str> {
        (!self.safety.is_destructive()).then_some(self.sql.as_str())
    }

    /// Return this statement inside an operations-enabled build.
    #[cfg(feature = "ops")]
    pub fn ops_sql(&self) -> &str {
        &self.sql
    }
}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// `true` if every operation is additive.
    pub fn is_additive(&self) -> bool {
        !self.operations.iter().any(|o| o.safety.is_destructive())
    }

    /// `true` if any operation is destructive.
    pub fn is_destructive(&self) -> bool {
        !self.is_additive()
    }

    /// The destructive operations, in plan order.
    pub fn destructive(&self) -> impl Iterator<Item = &Operation> {
        self.operations.iter().filter(|o| o.safety.is_destructive())
    }

    fn render_sql(&self) -> String {
        let mut out = String::new();
        for op in &self.operations {
            out.push_str(&op.sql);
            out.push_str(";\n");
        }
        out
    }

    /// Emit the DDL only when every operation is additive.
    pub fn sql(&self) -> Result<String, DestructivePlan> {
        if self.is_destructive() {
            return Err(DestructivePlan {
                destructive: self.destructive().map(|o| o.summary.clone()).collect(),
            });
        }
        Ok(self.render_sql())
    }

    /// Emit the classified DDL inside an operations-enabled build.
    ///
    /// The consuming operations verb must verify its durable authorization
    /// before executing the returned statements.
    #[cfg(feature = "ops")]
    pub fn ops_sql(&self) -> String {
        self.render_sql()
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

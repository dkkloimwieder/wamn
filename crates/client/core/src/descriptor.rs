//! Field descriptors, as generated code carries them.
//!
//! The RUNTIME half of the projection. `FieldIr` in the generator is what the
//! contract projects to; this is what an emitted binding holds and what a
//! control reads at runtime. Neither is hand-authored: the projection is read
//! from the contract, and every value of this type is emitted from that
//! projection, so a field that changes shape changes both by regeneration.
//!
//! It lives here rather than in the generator because a terminal, a form or a
//! table has no business depending on a code generator — which would drag a
//! Postgres client and an async runtime into a UI binary to read four strings.

/// One field, as its contract declares it.
///
/// Borrowed rather than owned: every value is a constant in generated code,
/// so nothing allocates to describe a schema that cannot change at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldDescriptor {
    /// Dotted path within its carrier, e.g. `value.purchase_order_id`.
    pub path: &'static str,
    /// Contract type name, e.g. `uuid`, `text`, `timestamptz`, `int64`.
    pub type_name: &'static str,
    /// Whether the contract admits the field being absent or null.
    pub nullable: bool,
    /// Closed value domain, when the contract declares one.
    ///
    /// EMPTY MEANS OPEN, and the difference is what a control renders: a
    /// closed domain is a selector over known values, an open one is a free
    /// input. A control that treated empty as "no values allowed" would offer
    /// a caller nothing to pick for every ordinary text field.
    pub values: &'static [&'static str],
}

impl FieldDescriptor {
    /// Whether the contract closes this field to a known set of values.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        !self.values.is_empty()
    }

    /// The field's own name — the last segment of its path.
    ///
    /// Array segments keep their marker in the path (`value.line[].quantity`)
    /// but a label wants the leaf, not the route to it.
    #[must_use]
    pub fn leaf(&self) -> &'static str {
        self.path.rsplit('.').next().unwrap_or(self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: FieldDescriptor = FieldDescriptor {
        path: "value.status",
        type_name: "text",
        nullable: false,
        values: &["open", "closed"],
    };
    const QUANTITY: FieldDescriptor = FieldDescriptor {
        path: "value.line[].quantity",
        type_name: "numeric",
        nullable: true,
        values: &[],
    };

    /// Empty means OPEN, not "nothing allowed" — the distinction decides
    /// whether a control is a selector or a free input.
    #[test]
    fn an_empty_domain_is_open_and_a_populated_one_is_closed() {
        assert!(STATUS.is_closed());
        assert!(!QUANTITY.is_closed());
    }

    #[test]
    fn a_leaf_is_the_last_path_segment() {
        assert_eq!(STATUS.leaf(), "status");
        assert_eq!(QUANTITY.leaf(), "quantity");
        assert_eq!(
            FieldDescriptor {
                path: "request_id",
                ..STATUS
            }
            .leaf(),
            "request_id",
            "a bare name is its own leaf"
        );
    }

    /// Descriptors are constants, so they must be usable in a const context —
    /// that is the whole reason the type borrows rather than owns.
    #[test]
    fn descriptors_are_const_constructible() {
        const FIELDS: &[FieldDescriptor] = &[STATUS, QUANTITY];
        assert_eq!(FIELDS.len(), 2);
    }
}

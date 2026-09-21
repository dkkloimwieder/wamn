//! The UI-neutral screen plan that every client emitter reads.
//!
//! The plan states interaction meaning only: which operations get a screen,
//! which inputs the platform supplies, which record a screen reads or writes,
//! and which read operation supplies a command's revision. No layout, styling,
//! or framework concept enters it, so a terminal, a browser, and a native
//! emitter apply the same rules.
//!
//! The plan borrows its contract facts from [`ClientContractIr`]. It is a plain
//! Rust value: generation does not serialize it, and it carries no version.

use crate::client_ir::{ClientContractIr, ModelIr, OperationIr, leaf_fields, revision_inputs};

/// Contract input paths that carry a platform value instead of operator input.
const SUPPLIED_PATHS: [(&str, SuppliedKind); 5] = [
    ("request_id", SuppliedKind::RequestId),
    ("idempotency_key", SuppliedKind::IdempotencyKey),
    ("value.idempotency_key", SuppliedKind::IdempotencyKey),
    ("occurred_at", SuppliedKind::OccurredAt),
    ("value.occurred_at", SuppliedKind::OccurredAt),
];

/// The operation kind that no operator calls.
const PRIVATE_KIND: &str = "event_handler";

/// What an operator does on one screen.
///
/// The map from contract shape to role is a starting point. An emitter writes
/// one component per supported role. A shape with no role gets no component,
/// and [`ClientPlan::unsupported`] names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Many records, one row for each.
    Table,
    /// The fields of one record.
    Detail,
    /// Typed input that the operator sends.
    Form,
    /// A removal that the operator confirms first.
    Delete,
    /// No supported shape, with the reason.
    Unsupported(NoRole),
}

impl Role {
    /// Whether an emitter writes a component for this screen.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

/// Why one operation's shape has no supported role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRole {
    /// The operation kind is outside the client vocabulary.
    UnknownKind,
    /// A read kind and its result class do not agree on what comes back.
    UnsupportedResult,
}

impl NoRole {
    /// One line that a generator report prints beside the operation name.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnknownKind => "the operation kind has no screen role",
            Self::UnsupportedResult => "the result class does not fit the operation kind",
        }
    }
}

impl core::fmt::Display for NoRole {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.reason())
    }
}

/// The platform value that a session driver writes into a reserved input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppliedKind {
    /// One submission attempt's identity.
    RequestId,
    /// The replay key of one submission intent.
    IdempotencyKey,
    /// The time at which the operator started the intent.
    OccurredAt,
}

/// A declared input path that the operator never types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuppliedField<'a> {
    /// Exact input path declared by the contract.
    pub path: &'a str,
    /// The platform value bound to that path.
    pub kind: SuppliedKind,
}

/// The stored record that one screen reads or changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLink<'a> {
    /// Qualified relation that holds the record.
    pub relation: &'a str,
    /// Result field that carries the record key.
    pub key_field: &'a str,
    /// Input path that carries the record key, when the operation takes one.
    pub key_input: Option<&'a str>,
}

/// The read operation that supplies a command's key and current revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionBinding<'a> {
    /// Operation name that reads the current record.
    pub read_operation: &'a str,
    /// Input path of the read operation that takes the key.
    pub read_key_input: &'a str,
    /// Result field of the read operation that carries the key.
    pub key_field: &'a str,
    /// Result field of the read operation that carries the revision.
    pub revision_field: &'a str,
    /// Input path of the command that takes the key.
    pub command_key_input: &'a str,
    /// Input path of the command that takes the revision.
    pub command_revision_input: &'a str,
}

/// One callable operation's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenPlan<'a> {
    /// Model that owns the operation.
    pub model: &'a str,
    /// Operation name inside its model.
    pub name: &'a str,
    /// Contract facts that an emitter copies without applying a rule.
    pub contract: &'a OperationIr,
    /// What the operator does on the screen.
    pub role: Role,
    /// The result class that the release serves.
    ///
    /// A served route states its own class. An operation with no route keeps
    /// the class it declared. A route whose terminal contract is absent states
    /// no class at all.
    pub result_class: Option<&'a str>,
    /// Reserved input paths, in contract order.
    pub supplied: Vec<SuppliedField<'a>>,
    /// Input paths that carry a declared or platform revision, sorted by path.
    pub revision_inputs: Vec<&'a str>,
    /// The record that the screen reads or changes.
    pub record: Option<RecordLink<'a>>,
    /// The read operation that supplies the revision this screen sends.
    pub revision: Option<RevisionBinding<'a>>,
}

/// One model's screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPlan<'a> {
    /// Contract facts for the model, including its display fields.
    pub model: &'a ModelIr,
    /// Callable screens, sorted by operation name.
    pub screens: Vec<ScreenPlan<'a>>,
}

/// The screen plan for one package's client contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPlan<'a> {
    /// Package that declared the contract.
    pub package: &'a str,
    /// Every model in the contract, sorted by name.
    ///
    /// A model whose operations are all private keeps an empty screen list, so
    /// an emitter still writes that model's module.
    pub models: Vec<ModelPlan<'a>>,
}

impl<'a> ClientPlan<'a> {
    /// Build the plan for one client contract.
    #[must_use]
    pub fn from_ir(ir: &'a ClientContractIr) -> Self {
        let mut models: Vec<_> = ir.models.iter().map(ModelPlan::from_ir).collect();
        models.sort_by(|left, right| left.model.name.cmp(&right.model.name));
        Self {
            package: &ir.package,
            models,
        }
    }

    /// Every screen in the plan, in model and then operation order.
    pub fn screens(&self) -> impl Iterator<Item = &ScreenPlan<'a>> {
        self.models.iter().flat_map(|model| model.screens.iter())
    }

    /// Every operation with no supported role, by canonical identity.
    ///
    /// A generator reports this list. The named operations get no component.
    #[must_use]
    pub fn unsupported(&self) -> Vec<(&'a str, NoRole)> {
        self.screens()
            .filter_map(|screen| match screen.role {
                Role::Unsupported(reason) => Some((screen.contract.operation.as_str(), reason)),
                _ => None,
            })
            .collect()
    }
}

impl<'a> ModelPlan<'a> {
    fn from_ir(model: &'a ModelIr) -> Self {
        let mut screens: Vec<_> = model
            .operations
            .iter()
            .filter(|operation| operation.kind != PRIVATE_KIND)
            .map(|operation| ScreenPlan::from_ir(&model.name, operation))
            .collect();
        screens.sort_by(|left, right| left.name.cmp(right.name));
        Self { model, screens }
    }
}

impl<'a> ScreenPlan<'a> {
    fn from_ir(model: &'a str, operation: &'a OperationIr) -> Self {
        let supplied = leaf_fields(&operation.input_fields)
            .into_iter()
            .filter_map(|field| {
                SUPPLIED_PATHS
                    .iter()
                    .find(|(path, _)| *path == field.path)
                    .map(|(_, kind)| SuppliedField {
                        path: field.path.as_str(),
                        kind: *kind,
                    })
            })
            .collect();
        let record = operation.record.as_ref().map(|record| RecordLink {
            relation: &record.relation,
            key_field: &record.key_field,
            key_input: record.key_input.as_deref(),
        });
        let revision = operation
            .revision_binding
            .as_ref()
            .map(|binding| RevisionBinding {
                read_operation: &binding.read_operation,
                read_key_input: &binding.read_key_input,
                key_field: &binding.key_field,
                revision_field: &binding.revision_field,
                command_key_input: &binding.command_key_input,
                command_revision_input: &binding.command_revision_input,
            });
        let result_class = operation.route.as_ref().map_or_else(
            || Some(operation.result_class.as_str()),
            |route| route.response.result_class.as_deref(),
        );
        Self {
            model,
            name: &operation.name,
            contract: operation,
            role: role(&operation.kind, result_class),
            result_class,
            supplied,
            revision_inputs: revision_inputs(operation),
            record,
            revision,
        }
    }
}

/// Select the screen role for one contract shape.
fn role(kind: &str, result_class: Option<&str>) -> Role {
    match (kind, result_class) {
        ("query" | "projection", Some("bounded_list" | "page")) => Role::Table,
        ("get", Some("one")) => Role::Detail,
        ("create" | "update" | "command", _) => Role::Form,
        ("delete", _) => Role::Delete,
        ("get" | "query" | "projection", _) => Role::Unsupported(NoRole::UnsupportedResult),
        _ => Role::Unsupported(NoRole::UnknownKind),
    }
}

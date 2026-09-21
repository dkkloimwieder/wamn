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
        Self {
            model,
            name: &operation.name,
            contract: operation,
            supplied,
            revision_inputs: revision_inputs(operation),
            record,
            revision,
        }
    }
}

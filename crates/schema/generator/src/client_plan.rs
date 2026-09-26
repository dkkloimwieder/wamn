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

use serde::Deserialize as _;
use serde::de::IntoDeserializer as _;
use wamn_catalog::OperationKind;

use crate::client_ir::{
    CURSOR_INPUT, ClientContractIr, FILTER_PREFIX, FieldIr, FilterIr, LIMIT_INPUT, LimitIr,
    ModelIr, OperationIr, SORT_DIRECTION_INPUT, SORT_FIELD_INPUT, SortIr, leaf_fields,
    revision_inputs,
};

/// The contract type a display field falls back to when nobody states one.
const DISPLAY_TYPE: &str = "text";

/// Contract input paths that carry a platform value instead of operator input.
const SUPPLIED_PATHS: [(&str, SuppliedKind); 5] = [
    ("request_id", SuppliedKind::RequestId),
    ("idempotency_key", SuppliedKind::IdempotencyKey),
    ("value.idempotency_key", SuppliedKind::IdempotencyKey),
    ("occurred_at", SuppliedKind::OccurredAt),
    ("value.occurred_at", SuppliedKind::OccurredAt),
];

/// The transaction of a command that runs each outer input on its own.
const PER_INPUT_TRANSACTION: &str = "explicit_per_input";

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

/// The item field that carries the idempotency key of one operation, from its
/// generated input contract, or `None` when the contract declares no key.
///
/// Publish writes it into the operation's route, so the route host and a
/// session driver read the same reserved path.
#[must_use]
pub fn idempotency_field(input_contract: &serde_json::Value) -> Option<String> {
    let fields = crate::client_fields::input_fields_of(input_contract);
    leaf_fields(&fields)
        .into_iter()
        .find(|field| {
            SUPPLIED_PATHS
                .iter()
                .any(|(path, kind)| *kind == SuppliedKind::IdempotencyKey && *path == field.path)
        })
        .map(|field| field.path.clone())
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

/// Where one screen's result rows come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rows {
    /// The whole result value is the one row.
    Single,
    /// Each element of the named result array is one row.
    List {
        /// Result key that holds the array.
        key: &'static str,
    },
}

/// How a screen asks the release for the next page of rows.
///
/// The page controls are input, and they are not the operator's own fields, so
/// each one states the exact input path that carries it and
/// [`ScreenPlan::inputs`] leaves that path out. An emitter that renders a page
/// control reads it here. An emitter that renders the operator's fields reads
/// `inputs` and gets no page control by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paging<'a> {
    /// Declared filters, ordered by field.
    pub filters: &'a [FilterIr],
    /// Input paths that carry the declared filters, in contract order.
    pub filter_inputs: Vec<&'a str>,
    /// Sortable fields and the permitted directions.
    pub sort: Option<&'a SortIr>,
    /// Input path that carries the sort field.
    pub sort_field_input: Option<&'a str>,
    /// Input path that carries the sort direction.
    pub sort_direction_input: Option<&'a str>,
    /// Row limit bounds and default.
    pub limit: Option<&'a LimitIr>,
    /// Input path that carries the page size.
    pub limit_input: Option<&'a str>,
    /// Input path that carries the cursor, when the release serves pages.
    pub cursor_input: Option<&'a str>,
}

impl<'a> Paging<'a> {
    /// Every input path that carries a page control, in one list.
    #[must_use]
    pub fn inputs(&self) -> Vec<&'a str> {
        self.filter_inputs
            .iter()
            .copied()
            .chain(self.sort_field_input)
            .chain(self.sort_direction_input)
            .chain(self.limit_input)
            .chain(self.cursor_input)
            .collect()
    }
}

/// Why a result row can open another screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkReason {
    /// The target reads the same record by its key.
    Record,
    /// The target sends a revision that this screen reads.
    Revision,
}

/// Another screen that one result row opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowLink<'a> {
    /// Canonical identity of the operation that the row opens.
    pub operation: &'a str,
    /// Why the row can open it.
    pub reason: LinkReason,
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

/// One input the operator chooses from a list instead of typing.
///
/// The reference is a contract fact, and choosing WHICH list serves it is a
/// rule about two operations, so it is resolved here beside the row links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopulatedInput<'a> {
    /// Input path this selector fills.
    pub input: &'a str,
    /// Canonical identity of the list operation the selector reads.
    pub list_operation: &'a str,
    /// Model that owns the list operation, which is the module an emitter
    /// imports it from. It is not always the model whose records it lists.
    pub list_model: &'a str,
    /// Operation name of the list inside its own model.
    pub list_name: &'a str,
    /// Where that list's rows come from, so a caller reads the right member.
    pub list_rows: Rows,
    /// Result field of that list which carries the value to send.
    pub key_field: &'a str,
    /// Result field of that list which carries the text a person reads.
    pub display_field: &'a str,
    /// Input path of the list's declared filter on that display field, when
    /// the list declares one. A selector searches by it and by nothing else.
    pub search_input: Option<&'a str>,
    /// Input path of the list's page cursor, when the release serves pages.
    pub cursor_input: Option<&'a str>,
    /// How one selector narrows another, when the reference states it.
    pub narrowed_by: Option<Narrowing<'a>>,
    /// The revision that the chosen row supplies, when a revision input
    /// names this input in its `revision`.
    pub revision: Option<ChosenRevision<'a>>,
    /// The read that loads one held record the list did not return, so the
    /// selector shows its text. It is the read the list's rows open, and it
    /// returns the key field and the display field.
    pub read: Option<RecordRead<'a>>,
}

/// The read that returns one record by its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordRead<'a> {
    /// Canonical identity of the read.
    pub operation: &'a str,
    /// Model that owns the read, which is the module an emitter imports it
    /// from.
    pub model: &'a str,
    /// Operation name of the read inside its own model.
    pub name: &'a str,
    /// Input path of the read that takes the key.
    pub key_input: &'a str,
}

/// A revision that the operator's choice in one selector supplies.
///
/// The row the operator chooses carries its record's revision, so the form
/// sends the revision of the record it names and no page prop supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChosenRevision<'a> {
    /// Input path of the command that takes the revision.
    pub input: &'a str,
    /// Result field of the list that carries the revision.
    pub field: &'a str,
}

/// One selector whose list declares no filter on its display field.
///
/// The operator reads the first page and cannot search it. The gap is in the
/// contract, so a generator names the selector and an author closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsearchableSelector<'a> {
    /// Canonical identity of the screen that renders the selector.
    pub operation: &'a str,
    /// Input path the selector fills.
    pub input: &'a str,
    /// Canonical identity of the list it reads.
    pub list_operation: &'a str,
}

/// One table column that names a record, and the read that shows its text.
///
/// The column carries the record key. The model's served list states the
/// field a person reads, and the record read that list's rows open returns
/// it, so a cell shows the same text a selector offers for that record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedColumn<'a> {
    /// Result field of this screen that carries the key.
    pub column: &'a str,
    /// Canonical identity of the read that returns one record.
    pub read_operation: &'a str,
    /// Model that owns the read, which is the module an emitter imports it
    /// from.
    pub read_model: &'a str,
    /// Operation name of the read inside its own model.
    pub read_name: &'a str,
    /// Input path of the read that takes the key.
    pub key_input: &'a str,
    /// Result field of the read that carries the text a person reads.
    pub display_field: &'a str,
}

/// One table column that names a record no served read can show.
///
/// The cell shows the key. The gap is in the release, so a generator names
/// the column and an author closes it by serving the model's list and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnresolvedColumn<'a> {
    /// Canonical identity of the screen that renders the column.
    pub operation: &'a str,
    /// Result field the column shows.
    pub column: &'a str,
    /// Model whose record the column names.
    pub model: &'a str,
}

/// One selector narrowing another: this screen's value fills that list input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Narrowing<'a> {
    /// Input path of THIS screen whose value narrows the list.
    pub input: &'a str,
    /// Input path of the LIST operation that takes it.
    pub list_input: &'a str,
}

/// A form that one result row opens with values it already knows.
///
/// The pairs come from two declared facts: this screen's rows are records of
/// one model, and that form states exactly one input which names the same
/// model. No name is compared, so a form and a table that merely share a
/// spelling stay unrelated. A form with two inputs of that model gets no pair
/// from the row, because no declared path says which one the row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowForm<'a> {
    /// Canonical identity of the form the row opens.
    pub operation: &'a str,
    /// Model that owns the form, which is the module an emitter reads it
    /// from.
    pub model: &'a str,
    /// Operation name of the form inside its own model.
    pub name: &'a str,
    /// Result field of this screen, and the input path of that form.
    pub pairs: Vec<(&'a str, &'a str)>,
    /// The revision the row carries for the record it fills, when a revision
    /// input of the form names the filled input in its `revision` and this
    /// screen shows the field that carries it: the result field of this
    /// screen, and the revision input of that form. A form that sends one
    /// item for each of many rows sends each row's own revision this way.
    pub revision: Option<(&'a str, &'a str)>,
}

/// The update that edits one table's rows in place.
///
/// It is the served update of the table's own relation, keyed by the table's
/// row id. Its record states the key input, the revision input and the row
/// field that carries the revision, so a cell edit sends the row's own values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableUpdate<'a> {
    /// Canonical identity of the update.
    pub operation: &'a str,
    /// Model that owns the update, which is the module an emitter imports it
    /// from.
    pub model: &'a str,
    /// Operation name of the update inside its own model.
    pub name: &'a str,
    /// Input path of the update that takes the row id.
    pub key_input: &'a str,
    /// Input path of the update that takes the expected revision, when the
    /// update guards one.
    pub revision_input: Option<&'a str>,
    /// Table column that carries that revision.
    pub revision_field: Option<&'a str>,
    /// The table columns a cell edits, in the update's input order.
    pub fields: Vec<EditableField<'a>>,
}

/// One table column that the update writes.
///
/// The update states the column each writable input writes, so no name is
/// compared. The plan supplies none of these inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditableField<'a> {
    /// Result field of the table, which is the column the input writes.
    pub column: &'a str,
    /// Input path of the update that carries the new value.
    pub input: &'a str,
}

/// One served operation that a table row opens, from the row links and the
/// row forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableAction<'a> {
    /// Canonical identity of the operation.
    pub operation: &'a str,
    /// Whether one call takes many rows: the operation accepts more than one
    /// outer input and runs each in its own transaction.
    pub many: bool,
}

/// Another table that shows the records of one row, scoped by that row.
///
/// The child declares a filter on a column that names a record of this
/// table's model, so the parent row's id is the child's scope value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildTable<'a> {
    /// Canonical identity of the child's read.
    pub operation: &'a str,
    /// Model that owns the child's read.
    pub model: &'a str,
    /// Operation name of the child's read inside its own model.
    pub name: &'a str,
    /// Result field of the child that its scope filter narrows.
    pub field: &'a str,
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
    /// Result leaf fields in contract order, one column for each.
    ///
    /// A served route states its own result fields. An operation with no route
    /// keeps the fields it declared.
    pub columns: Vec<&'a FieldIr>,
    /// Input leaf fields that the operator fills, in contract order.
    ///
    /// The reserved paths are absent: the supplied fields, the revision
    /// inputs, and the page cursor.
    pub inputs: Vec<&'a FieldIr>,
    /// Where the result rows come from.
    pub rows: Rows,
    /// Filters, sort, limit, and the cursor input.
    pub paging: Option<Paging<'a>>,
    /// Screens that one result row opens, in plan order.
    pub row_links: Vec<RowLink<'a>>,
    /// Reserved input paths, in contract order.
    pub supplied: Vec<SuppliedField<'a>>,
    /// Input paths that carry a declared or platform revision, sorted by path.
    pub revision_inputs: Vec<&'a str>,
    /// The record that the screen reads or changes.
    pub record: Option<RecordLink<'a>>,
    /// The read operation that supplies the revision this screen sends.
    pub revision: Option<RevisionBinding<'a>>,
    /// Inputs the operator chooses from a list, in contract order.
    pub population: Vec<PopulatedInput<'a>>,
    /// Forms that one result row opens prefilled, in plan order.
    pub row_forms: Vec<RowForm<'a>>,
    /// Columns that name a record, each with the read that shows its text,
    /// in contract order. Only a table states any.
    pub resolved_columns: Vec<ResolvedColumn<'a>>,
    /// The update a table cell edits through. Only a table states one.
    pub update: Option<TableUpdate<'a>>,
    /// Served operations one table row opens, row links first, then row
    /// forms, each with whether it takes many rows.
    pub actions: Vec<TableAction<'a>>,
    /// Tables scoped by one row of this table, in plan order.
    pub child_tables: Vec<ChildTable<'a>>,
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
        let mut plan = Self {
            package: &ir.package,
            models,
        };
        plan.link_rows();
        plan.populate_inputs();
        plan.link_forms();
        plan.resolve_columns();
        plan.bind_tables();
        plan
    }

    /// Bind each table to its update, its actions and its child tables.
    ///
    /// It runs last, because the actions read the row links and row forms.
    fn bind_tables(&mut self) {
        let bound: Vec<_> = self
            .screens()
            .map(|screen| {
                (
                    table_update(screen, self),
                    table_actions(screen, self),
                    child_tables(screen, self),
                )
            })
            .collect();
        let mut bound = bound.into_iter();
        for model in &mut self.models {
            for screen in &mut model.screens {
                (screen.update, screen.actions, screen.child_tables) =
                    bound.next().expect("one binding for each screen");
            }
        }
    }

    /// Bind each screen's row links once every screen exists.
    fn link_rows(&mut self) {
        let targets: Vec<_> = self.screens().map(Target::of).collect();
        let links: Vec<_> = targets
            .iter()
            .map(|source| row_links(source, &targets))
            .collect();
        let mut links = links.into_iter();
        for model in &mut self.models {
            for screen in &mut model.screens {
                screen.row_links = links.next().expect("one link list for each screen");
            }
        }
    }

    /// Bind each input that names a record to the list that offers it.
    ///
    /// This runs after every screen exists, for the reason `link_rows` does:
    /// the list that serves an input belongs to another operation, and one
    /// screen's IR never states another's. It runs after `link_rows` too,
    /// because the read that loads a held record is the one the list's rows
    /// open by record.
    fn populate_inputs(&mut self) {
        let lists: Vec<Lister<'a>> = self.screens().filter_map(Lister::of).collect();
        let populated: Vec<_> = self
            .screens()
            .map(|screen| populated_inputs(screen, &lists, self))
            .collect();
        let mut populated = populated.into_iter();
        for model in &mut self.models {
            for screen in &mut model.screens {
                screen.population = populated
                    .next()
                    .expect("one population list for each screen");
            }
        }
    }

    /// Bind each table to the forms its rows can open prefilled.
    ///
    /// It runs after `populate_inputs`, because a form states which of its
    /// inputs names a record there, and this pass reads that answer instead
    /// of asking the contract again.
    fn link_forms(&mut self) {
        let forms: Vec<FormTarget<'a>> = self
            .screens()
            .filter(|screen| matches!(screen.role, Role::Form))
            .map(|screen| FormTarget {
                target: Target::of(screen),
                references: screen
                    .inputs
                    .iter()
                    .filter_map(|input| {
                        input
                            .references
                            .as_ref()
                            .map(|reference| (reference.model.as_str(), input.path.as_str()))
                    })
                    .collect(),
                revisions: screen
                    .population
                    .iter()
                    .filter_map(|populated| {
                        populated
                            .revision
                            .map(|revision| (populated.input, revision))
                    })
                    .collect(),
            })
            .collect();
        let linked: Vec<_> = self
            .screens()
            .map(|screen| row_forms(screen, &forms))
            .collect();
        let mut linked = linked.into_iter();
        for model in &mut self.models {
            for screen in &mut model.screens {
                screen.row_forms = linked.next().expect("one form list for each screen");
            }
        }
    }

    /// Bind each table column that names a record to the read that shows it.
    ///
    /// It runs after `link_rows`, because the read is the one a list's rows
    /// open by record, and this pass reads that link instead of matching the
    /// relation again.
    fn resolve_columns(&mut self) {
        let lists: Vec<Lister<'a>> = self.screens().filter_map(Lister::of).collect();
        let resolved: Vec<_> = self
            .screens()
            .map(|screen| resolved_columns(screen, &lists, self))
            .collect();
        let mut resolved = resolved.into_iter();
        for model in &mut self.models {
            for screen in &mut model.screens {
                screen.resolved_columns = resolved.next().expect("one column list for each screen");
            }
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

    /// Every operation with a role that the release does not serve.
    ///
    /// A generator reports this list beside [`Self::unsupported`]. The named
    /// operations get no component, because the bindings write no invoke
    /// function for an operation with no route.
    #[must_use]
    pub fn unserved(&self) -> Vec<&'a str> {
        self.screens()
            .filter(|screen| screen.role.is_supported() && screen.contract.route.is_none())
            .map(|screen| screen.contract.operation.as_str())
            .collect()
    }

    /// Every table column that names a record no served read can show.
    ///
    /// A generator reports this list beside [`Self::unsearchable`]. The named
    /// columns show the record key.
    #[must_use]
    pub fn unresolved(&self) -> Vec<UnresolvedColumn<'a>> {
        self.screens()
            .filter(|screen| screen.role == Role::Table)
            .flat_map(|screen| {
                screen.columns.iter().filter_map(|column| {
                    let reference = column.references.as_ref()?;
                    let resolved = screen
                        .resolved_columns
                        .iter()
                        .any(|resolved| resolved.column == column.path);
                    (!resolved).then_some(UnresolvedColumn {
                        operation: screen.contract.operation.as_str(),
                        column: column.path.as_str(),
                        model: reference.model.as_str(),
                    })
                })
            })
            .collect()
    }

    /// Every selector whose list declares no filter on its display field.
    ///
    /// A generator reports this list beside [`Self::unsupported`]. The named
    /// selectors read the first page and render no search control.
    #[must_use]
    pub fn unsearchable(&self) -> Vec<UnsearchableSelector<'a>> {
        self.screens()
            .flat_map(|screen| {
                screen
                    .population
                    .iter()
                    .filter(|populated| populated.search_input.is_none())
                    .map(|populated| UnsearchableSelector {
                        operation: screen.contract.operation.as_str(),
                        input: populated.input,
                        list_operation: populated.list_operation,
                    })
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
        let supplied: Vec<SuppliedField<'a>> = leaf_fields(&operation.input_fields)
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
        let result_fields = effective_result_fields(operation);
        let revision_inputs = revision_inputs(operation);
        let input_leaves = leaf_fields(&operation.input_fields);
        let declared = |path: &str| input_leaves.iter().any(|field| field.path == path);
        let cursor_input =
            (result_class == Some("page") && declared(CURSOR_INPUT)).then_some(CURSOR_INPUT);
        let rows = match result_class {
            Some("bounded_list") => Rows::List { key: "rows" },
            Some("page") => Rows::List { key: "item" },
            _ => Rows::Single,
        };
        let declared_paging = operation.paging.as_ref();
        let sort = declared_paging.and_then(|paging| paging.sort.as_ref());
        let limit = declared_paging.and_then(|paging| paging.limit.as_ref());
        let filters = declared_paging.map_or(&[][..], |paging| paging.filters.as_slice());
        let paging = (declared_paging.is_some() || cursor_input.is_some()).then(|| Paging {
            filters,
            filter_inputs: input_leaves
                .iter()
                .filter(|field| {
                    filters
                        .iter()
                        .any(|filter| filter_input(&field.path, &filter.field))
                })
                .map(|field| field.path.as_str())
                .collect(),
            sort,
            sort_field_input: (sort.is_some() && declared(SORT_FIELD_INPUT))
                .then_some(SORT_FIELD_INPUT),
            sort_direction_input: (sort.is_some() && declared(SORT_DIRECTION_INPUT))
                .then_some(SORT_DIRECTION_INPUT),
            limit,
            limit_input: (limit.is_some() && declared(LIMIT_INPUT)).then_some(LIMIT_INPUT),
            cursor_input,
        });
        let page_controls = paging.as_ref().map(Paging::inputs).unwrap_or_default();
        let inputs = input_leaves
            .iter()
            .filter(|field| {
                let path = field.path.as_str();
                !supplied.iter().any(|field| field.path == path)
                    && !revision_inputs.contains(&path)
                    && !page_controls.contains(&path)
            })
            .copied()
            .collect();
        Self {
            model,
            name: &operation.name,
            contract: operation,
            role: role(&operation.kind, result_class),
            result_class,
            columns: leaf_fields(result_fields),
            inputs,
            rows,
            paging,
            row_links: Vec::new(),
            supplied,
            revision_inputs,
            record,
            revision,
            population: Vec::new(),
            row_forms: Vec::new(),
            resolved_columns: Vec::new(),
            update: None,
            actions: Vec::new(),
            child_tables: Vec::new(),
        }
    }

    /// Whether the screen reads without changing a record.
    ///
    /// A read clears its rows when the operator changes an input.
    #[must_use]
    pub fn is_read(&self) -> bool {
        let kind: Result<OperationKind, serde::de::value::Error> =
            OperationKind::deserialize(self.contract.kind.as_str().into_deserializer());
        kind.is_ok_and(OperationKind::is_read)
    }

    /// Whether the operator confirms before the screen submits.
    #[must_use]
    pub const fn confirms(&self) -> bool {
        matches!(self.role, Role::Delete)
    }
}

/// The facts that one screen shows to another when rows link.
struct Target<'a> {
    operation: &'a str,
    model: &'a str,
    name: &'a str,
    kind: &'a str,
    served: bool,
    record: Option<RecordLink<'a>>,
    revision_read: Option<&'a str>,
}

impl<'a> Target<'a> {
    fn of(screen: &ScreenPlan<'a>) -> Self {
        Self {
            operation: screen.contract.operation.as_str(),
            model: screen.model,
            name: screen.name,
            kind: screen.contract.kind.as_str(),
            served: screen.contract.route.is_some(),
            record: screen.record,
            revision_read: screen.revision.map(|revision| revision.read_operation),
        }
    }
}

/// Select the screens that one screen's result row opens.
fn row_links<'a>(from: &Target<'a>, targets: &[Target<'a>]) -> Vec<RowLink<'a>> {
    targets
        .iter()
        .filter(|to| to.served && to.operation != from.operation)
        .filter_map(|to| {
            let reason = if to.revision_read == Some(from.operation) {
                LinkReason::Revision
            } else if to.kind == "get"
                && from.record.zip(to.record).is_some_and(|(from, to)| {
                    from.relation == to.relation
                        && from.key_field == to.key_field
                        && to.key_input.is_some()
                })
            {
                LinkReason::Record
            } else {
                return None;
            };
            Some(RowLink {
                operation: to.operation,
                reason,
            })
        })
        .collect()
}

/// One form, as a row that fills it sees it.
struct FormTarget<'a> {
    target: Target<'a>,
    /// Input paths that name a record, each with the model it names.
    references: Vec<(&'a str, &'a str)>,
    /// Each input whose chosen record supplies a revision, with that revision.
    revisions: Vec<(&'a str, ChosenRevision<'a>)>,
}

/// Select the forms that one screen's result row opens prefilled.
fn row_forms<'a>(screen: &ScreenPlan<'a>, forms: &[FormTarget<'a>]) -> Vec<RowForm<'a>> {
    let Some(list) = Lister::of(screen) else {
        return Vec::new();
    };
    forms
        .iter()
        .filter(|form| form.target.operation != screen.contract.operation)
        .filter_map(|form| {
            let pairs: Vec<_> = form
                .references
                .iter()
                .filter(|(model, _)| *model == list.model)
                .map(|(_, input)| (list.key_field, *input))
                .collect();
            // The row fills the one input that names its model. When two
            // inputs name it, no declared path says which one the row is, so
            // the row fills neither and the operator chooses both.
            let [(_, filled)] = pairs.as_slice() else {
                return None;
            };
            let revision = form
                .revisions
                .iter()
                .find(|(input, _)| input == filled)
                .filter(|(_, revision)| {
                    screen
                        .columns
                        .iter()
                        .any(|column| column.path == revision.field)
                })
                .map(|(_, revision)| (revision.field, revision.input));
            Some(RowForm {
                operation: form.target.operation,
                model: form.target.model,
                name: form.target.name,
                pairs,
                revision,
            })
        })
        .collect()
}

/// One served list, as an input that names a record sees it.
struct Lister<'a> {
    operation: &'a str,
    owner: &'a str,
    name: &'a str,
    rows: Rows,
    model: &'a str,
    key_field: &'a str,
    display_field: Option<&'a str>,
    /// Input paths of the list, each with the model it names.
    references: Vec<(&'a str, &'a str)>,
    /// Result leaves, which the default display field reads.
    columns: Vec<&'a FieldIr>,
    /// Input paths that carry the list's declared filters, in contract order.
    filter_inputs: Vec<&'a str>,
    /// Input path of the list's page cursor, when it serves pages.
    cursor_input: Option<&'a str>,
}

impl<'a> Lister<'a> {
    /// The list a selector can call, or nothing.
    ///
    /// A selector reads rows over HTTP, so an unserved read offers none. A
    /// generated `query` states its model through the record it declares, and
    /// an authored read states it in `lists`.
    fn of(screen: &ScreenPlan<'a>) -> Option<Self> {
        if screen.role != Role::Table || screen.contract.route.is_none() {
            return None;
        }
        // One member, one rule. Every read that serves rows states `lists`,
        // whether an author wrote it or generation derived it, so nothing
        // here asks where the fact came from. A selector offers a record by
        // one key, so rows of no model, or rows that several fields name,
        // offer none.
        let lists = screen.contract.lists.as_ref()?;
        let model = lists.model.as_deref()?;
        let [key_field] = lists.key_field.as_slice() else {
            return None;
        };
        Some(Self {
            operation: screen.contract.operation.as_str(),
            owner: screen.model,
            name: screen.name,
            rows: screen.rows,
            model,
            key_field: key_field.as_str(),
            display_field: lists.display_field.as_deref(),
            references: leaf_fields(&screen.contract.input_fields)
                .into_iter()
                .filter_map(|field| {
                    field
                        .references
                        .as_ref()
                        .map(|reference| (field.path.as_str(), reference.model.as_str()))
                })
                .collect(),
            columns: screen.columns.clone(),
            filter_inputs: screen
                .paging
                .as_ref()
                .map(|paging| paging.filter_inputs.clone())
                .unwrap_or_default(),
            cursor_input: screen
                .paging
                .as_ref()
                .and_then(|paging| paging.cursor_input),
        })
    }

    /// The result field a person reads, authored or defaulted.
    ///
    /// The default is the first text field in contract order. A list whose
    /// rows carry no text falls back to the key, so a selector always shows
    /// something an operator can tell apart.
    fn display(&self) -> &'a str {
        self.display_field.unwrap_or_else(|| {
            self.columns
                .iter()
                .find(|field| field.type_name == DISPLAY_TYPE)
                .map_or(self.key_field, |field| field.path.as_str())
        })
    }

    /// The input a selector searches by, which is the declared filter on the
    /// display field. A list that declares no such filter offers no search,
    /// and the plan reports it.
    fn search(&self) -> Option<&'a str> {
        let display = self.display();
        self.filter_inputs
            .iter()
            .copied()
            .find(|path| filter_input(path, display))
    }
}

/// Bind one table's columns that name a record to the reads that show them.
///
/// The model's served list states the display field, and the record read its
/// rows open returns that field for one key. A column whose model offers no
/// such pair stays unresolved, and the plan reports it.
fn resolved_columns<'a>(
    screen: &ScreenPlan<'a>,
    lists: &[Lister<'a>],
    plan: &ClientPlan<'a>,
) -> Vec<ResolvedColumn<'a>> {
    if screen.role != Role::Table {
        return Vec::new();
    }
    screen
        .columns
        .iter()
        .filter_map(|column| {
            let reference = column.references.as_ref()?;
            lists
                .iter()
                .filter(|list| list.model == reference.model)
                .find_map(|list| {
                    // The read must return the text the list shows, or the
                    // cell would show a different field than the selector.
                    let display = list.display();
                    let read = record_read(list, plan, &[display])?;
                    Some(ResolvedColumn {
                        column: column.path.as_str(),
                        read_operation: read.operation,
                        read_model: read.model,
                        read_name: read.name,
                        key_input: read.key_input,
                        display_field: display,
                    })
                })
        })
        .collect()
}

/// The read that the rows of one list open by record, when it returns every
/// field in `fields`.
fn record_read<'a>(
    list: &Lister<'a>,
    plan: &ClientPlan<'a>,
    fields: &[&str],
) -> Option<RecordRead<'a>> {
    let read = plan
        .screens()
        .find(|candidate| candidate.contract.operation == list.operation)?
        .row_links
        .iter()
        .filter(|link| link.reason == LinkReason::Record)
        .find_map(|link| {
            plan.screens()
                .find(|candidate| candidate.contract.operation == link.operation)
        })?;
    let key_input = read.record?.key_input?;
    fields
        .iter()
        .all(|wanted| read.columns.iter().any(|field| field.path == *wanted))
        .then_some(RecordRead {
            operation: read.contract.operation.as_str(),
            model: read.model,
            name: read.name,
            key_input,
        })
}

/// The served update of one table's relation, keyed by the table's row id,
/// with the columns it writes that the table shows.
fn table_update<'a>(screen: &ScreenPlan<'a>, plan: &ClientPlan<'a>) -> Option<TableUpdate<'a>> {
    let list = Lister::of(screen)?;
    let relation = screen.record?.relation;
    let shows = |path: &str| screen.columns.iter().any(|column| column.path == path);
    plan.screens().find_map(|update| {
        let record = update.contract.record.as_ref()?;
        if update.contract.kind != "update"
            || update.contract.route.is_none()
            || record.relation != relation
            || record.key_field != list.key_field
        {
            return None;
        }
        let key_input = record.key_input.as_deref()?;
        // A guarded update needs the revision the row shows.
        let revision_field = record.revision_field.as_deref();
        let revision_input = record.revision_input.as_deref();
        if revision_input.is_some() && !revision_field.is_some_and(shows) {
            return None;
        }
        let fields: Vec<_> = update
            .inputs
            .iter()
            .filter_map(|input| {
                let column = input.column.as_deref()?;
                shows(column).then_some(EditableField {
                    column,
                    input: input.path.as_str(),
                })
            })
            .collect();
        (!fields.is_empty()).then_some(TableUpdate {
            operation: update.contract.operation.as_str(),
            model: update.model,
            name: update.name,
            key_input,
            revision_input,
            revision_field: revision_input.and(revision_field),
            fields,
        })
    })
}

/// The served operations one table row opens, each with whether one call
/// takes many rows.
fn table_actions<'a>(screen: &ScreenPlan<'a>, plan: &ClientPlan<'a>) -> Vec<TableAction<'a>> {
    screen
        .row_links
        .iter()
        .map(|link| link.operation)
        .chain(screen.row_forms.iter().map(|form| form.operation))
        .filter_map(|operation| {
            let target = plan
                .screens()
                .find(|candidate| candidate.contract.operation == operation)?;
            target.contract.route.as_ref()?;
            Some(TableAction {
                operation,
                many: takes_many(target.contract),
            })
        })
        .collect()
}

/// Whether one call of an operation takes many outer inputs and runs each
/// in its own transaction, so one call can carry one input for each of many
/// rows.
pub fn takes_many(contract: &OperationIr) -> bool {
    let maximum = contract
        .envelope
        .as_ref()
        .and_then(|envelope| envelope.get("maximum"))
        .and_then(serde_json::Value::as_u64);
    maximum.is_some_and(|maximum| maximum > 1)
        && contract.transaction.as_deref() == Some(PER_INPUT_TRANSACTION)
}

/// The served tables whose declared filter narrows a column that names a
/// record of this table's model.
fn child_tables<'a>(screen: &ScreenPlan<'a>, plan: &ClientPlan<'a>) -> Vec<ChildTable<'a>> {
    let Some(parent) = Lister::of(screen) else {
        return Vec::new();
    };
    plan.screens()
        .filter(|child| {
            child.role == Role::Table
                && child.contract.route.is_some()
                && child.contract.operation != screen.contract.operation
        })
        .flat_map(|child| {
            let filters = child
                .paging
                .as_ref()
                .map_or(&[][..], |paging| paging.filters);
            filters.iter().filter_map(move |filter| {
                let column = child.columns.iter().find(|column| {
                    column.path == filter.field
                        && column
                            .references
                            .as_ref()
                            .is_some_and(|reference| reference.model == parent.model)
                })?;
                Some(ChildTable {
                    operation: child.contract.operation.as_str(),
                    model: child.model,
                    name: child.name,
                    field: column.path.as_str(),
                })
            })
        })
        .collect()
}

/// Bind one screen's inputs to the lists that offer their records.
fn populated_inputs<'a>(
    screen: &ScreenPlan<'a>,
    lists: &[Lister<'a>],
    plan: &ClientPlan<'a>,
) -> Vec<PopulatedInput<'a>> {
    let contract: &'a OperationIr = screen.contract;
    let input_leaves = leaf_fields(&contract.input_fields);
    screen
        .inputs
        .iter()
        .filter_map(|input| {
            let reference = input.references.as_ref()?;
            // An input whose model has no served list stays a plain control.
            // That is a fact about the release, not a failure.
            let list = lists.iter().find(|list| {
                list.model == reference.model && list.operation != screen.contract.operation
            })?;
            let narrowed_by = reference.narrowed_by.as_deref().and_then(|path| {
                let sibling = screen
                    .inputs
                    .iter()
                    .find(|field| field.path == path)?
                    .references
                    .as_ref()?;
                let list_input = list
                    .references
                    .iter()
                    .find(|(_, model)| *model == sibling.model)
                    .map(|(path, _)| *path)?;
                Some(Narrowing {
                    input: screen
                        .inputs
                        .iter()
                        .find(|field| field.path == path)
                        .map(|field| field.path.as_str())?,
                    list_input,
                })
            });
            // The list's rows carry the revision of the record they name, in
            // the one result field the contract marks as a revision.
            let revision = input_leaves
                .iter()
                .find(|field| field.revision.guards() == Some(input.path.as_str()))
                .and_then(|revision| {
                    let field = list.columns.iter().find(|column| {
                        column.revision.is_revision() && column.type_name == revision.type_name
                    })?;
                    Some(ChosenRevision {
                        input: revision.path.as_str(),
                        field: field.path.as_str(),
                    })
                });
            Some(PopulatedInput {
                input: input.path.as_str(),
                list_operation: list.operation,
                list_model: list.owner,
                list_name: list.name,
                list_rows: list.rows,
                key_field: list.key_field,
                display_field: list.display(),
                search_input: list.search(),
                cursor_input: list.cursor_input,
                narrowed_by,
                revision,
                // The read returns the value the form stores and the text the
                // selector shows, so a loaded record reads as a listed row.
                read: record_read(list, plan, &[list.key_field, list.display()]),
            })
        })
        .collect()
}

/// Whether one input path carries the named filter.
///
/// The query input contract writes a filter under `filter.<field>`, and an
/// array filter's leaf keeps the `[]` the IR adds.
pub(crate) fn filter_input(path: &str, field: &str) -> bool {
    path.strip_prefix(FILTER_PREFIX)
        .is_some_and(|rest| rest == field || rest.trim_end_matches("[]") == field)
}

/// The result fields one operation serves.
///
/// A served route states its own fields, because the release publishes them.
/// An operation with no route keeps the fields it declared. Every emitter
/// reads this one rule, including the emitters that run for an operation the
/// plan holds no screen for.
#[must_use]
pub fn effective_result_fields(operation: &OperationIr) -> &[FieldIr] {
    operation
        .route
        .as_ref()
        .map_or(operation.result_fields.as_slice(), |route| {
            route.response.fields.as_slice()
        })
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

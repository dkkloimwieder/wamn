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

/// The operation kind that no operator calls.
const PRIVATE_KIND: &str = "event_handler";

/// Operation kinds that read without changing a record.
const READ_KINDS: [&str; 3] = ["get", "query", "projection"];

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
    /// Result field of that list which carries the value to send.
    pub key_field: &'a str,
    /// Result field of that list which carries the text a person reads.
    pub display_field: &'a str,
    /// How one selector narrows another, when the reference states it.
    pub narrowed_by: Option<Narrowing<'a>>,
}

/// One selector narrowing another: this screen's value fills that list input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Narrowing<'a> {
    /// Input path of THIS screen whose value narrows the list.
    pub input: &'a str,
    /// Input path of the LIST operation that takes it.
    pub list_input: &'a str,
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
        plan
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
    /// screen's IR never states another's.
    fn populate_inputs(&mut self) {
        let lists: Vec<Lister<'a>> = self.screens().filter_map(Lister::of).collect();
        let populated: Vec<_> = self
            .screens()
            .map(|screen| populated_inputs(screen, &lists))
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
        let result_fields = operation
            .route
            .as_ref()
            .map_or(operation.result_fields.as_slice(), |route| {
                route.response.fields.as_slice()
            });
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
        }
    }

    /// Whether the screen reads without changing a record.
    ///
    /// A read clears its rows when the operator changes an input.
    #[must_use]
    pub fn is_read(&self) -> bool {
        READ_KINDS.contains(&self.contract.kind.as_str())
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
    kind: &'a str,
    served: bool,
    record: Option<RecordLink<'a>>,
    revision_read: Option<&'a str>,
}

impl<'a> Target<'a> {
    fn of(screen: &ScreenPlan<'a>) -> Self {
        Self {
            operation: screen.contract.operation.as_str(),
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

/// One served list, as an input that names a record sees it.
struct Lister<'a> {
    operation: &'a str,
    model: &'a str,
    key_field: &'a str,
    display_field: Option<&'a str>,
    /// Input paths of the list, each with the model it names.
    references: Vec<(&'a str, &'a str)>,
    /// Result leaves, which the default display field reads.
    columns: Vec<&'a FieldIr>,
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
        let (model, key_field, display_field) = match (&screen.contract.lists, screen.record) {
            (Some(lists), _) => (
                lists.model.as_str(),
                lists.key_field.as_str(),
                lists.display_field.as_deref(),
            ),
            (None, Some(record)) => (screen.model, record.key_field, None),
            (None, None) => return None,
        };
        Some(Self {
            operation: screen.contract.operation.as_str(),
            model,
            key_field,
            display_field,
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
}

/// Bind one screen's inputs to the lists that offer their records.
fn populated_inputs<'a>(screen: &ScreenPlan<'a>, lists: &[Lister<'a>]) -> Vec<PopulatedInput<'a>> {
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
            Some(PopulatedInput {
                input: input.path.as_str(),
                list_operation: list.operation,
                key_field: list.key_field,
                display_field: list.display(),
                narrowed_by,
            })
        })
        .collect()
}

/// Whether one input path carries the named filter.
///
/// The query input contract writes a filter under `filter.<field>`, and an
/// array filter's leaf keeps the `[]` the IR adds.
fn filter_input(path: &str, field: &str) -> bool {
    path.strip_prefix(FILTER_PREFIX)
        .is_some_and(|rest| rest == field || rest.trim_end_matches("[]") == field)
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

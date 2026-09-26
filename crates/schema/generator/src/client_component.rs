//! The SolidJS component emitter.
//!
//! It writes one component for each screen the [screen plan](crate::client_plan)
//! gives a role. The input is the plan plus the bindings that
//! [`client_ts`](crate::client_ts) emits, and nothing else: no manifest, and no
//! application file.
//!
//! # What is NOT emitted, and why
//!
//! No route, no navigation, no login, and no application shell. A component
//! reports what happened through a callback, and the application decides what
//! to open. No host and no base URL, for the reason
//! [`client_ts`](crate::client_ts) states.
//!
//! No layout system and no styling. The markup is plain, because the visual
//! format is not decided yet and a generated file is not edited.
//!
//! No component for a shape with no role. [`ClientPlan::unsupported`] names
//! those operations, and the emitted index lists them with the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::client_ir::FieldIr;
use crate::client_plan::{
    ClientPlan, ModelPlan, PopulatedInput, Role, Rows, ScreenPlan, SuppliedKind,
};
use crate::client_ts::{RUNTIME_PACKAGE, UI_PACKAGE, ts_type};
use crate::generate::GeneratedFile;

/// Why a component could not be emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientComponentError {
    kind: ClientComponentErrorKind,
    detail: String,
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientComponentErrorKind {
    /// A screen states a role whose component this emitter does not write yet.
    UnwrittenRole,
    /// A table column that a table definition cannot state: it is not a member
    /// of its row, or its type is not one of the frozen `sql-value` names.
    UnwrittenColumn,
}

impl ClientComponentErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnwrittenRole => "unwritten_role",
            Self::UnwrittenColumn => "unwritten_column",
        }
    }
}

impl ClientComponentError {
    fn new(kind: ClientComponentErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> ClientComponentErrorKind {
        self.kind
    }
}

impl core::fmt::Display for ClientComponentError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.detail)
    }
}

impl std::error::Error for ClientComponentError {}

/// The directory that holds the emitted components.
pub const COMPONENT_DIRECTORY: &str = "generated/client-ts/components";

/// Emit one component module per model, plus an index.
///
/// # Errors
///
/// [`ClientComponentError`] names a screen whose role has no component yet.
pub fn emit_ts_components(
    plan: &ClientPlan<'_>,
) -> Result<Vec<GeneratedFile>, ClientComponentError> {
    let mut files = Vec::new();
    let mut index = String::from("// @generated from the client-contract IR; do not edit.\n//\n");
    writeln!(
        index,
        "// Components of package `{}`, one for each operation that has a role\n// and a route.",
        plan.package
    )
    .expect("writing to a String cannot fail");
    index.push('\n');
    for model in &plan.models {
        let source = emit_model(model, plan)?;
        if source.is_empty() {
            continue;
        }
        writeln!(index, "export * from \"./{}.js\";", model.model.name)
            .expect("writing to a String cannot fail");
        files.push(GeneratedFile::new(
            format!("{COMPONENT_DIRECTORY}/{}.tsx", model.model.name).into_boxed_str(),
            source.into_bytes().into_boxed_slice(),
        ));
    }
    let unsupported = plan.unsupported();
    if unsupported.is_empty() {
        index.push_str("\n// Every operation of this release has a screen role.\n");
    } else {
        index.push_str(
            "\n// These operations get no component, and the bindings keep their types:\n",
        );
        for (operation, reason) in unsupported {
            writeln!(index, "// {operation}: {reason}").expect("writing to a String cannot fail");
        }
    }
    let unserved = plan.unserved();
    if !unserved.is_empty() {
        index.push_str(
            "\n// These operations get no component, because this release does not serve\n// them over HTTP and the bindings write no invoke function for them:\n",
        );
        for operation in unserved {
            writeln!(index, "// {operation}").expect("writing to a String cannot fail");
        }
    }
    // The contract gap an author closes: a list that declares no filter on its
    // display field gives a selector nothing to search by.
    let unsearchable = plan.unsearchable();
    if !unsearchable.is_empty() {
        index.push_str(
            "\n// These selectors read the first page and render no search, because the\n// list they read declares no filter on its display field:\n",
        );
        for selector in unsearchable {
            writeln!(
                index,
                "// {} {}: {}",
                selector.operation, selector.input, selector.list_operation
            )
            .expect("writing to a String cannot fail");
        }
    }
    // The release gap an author closes: a column that names a record whose
    // model serves no list with a record read beside it shows the key.
    let unresolved = plan.unresolved();
    if !unresolved.is_empty() {
        index.push_str(
            "\n// These table columns show the record key, because the model they name\n// serves no list whose rows open a record read that returns its text:\n",
        );
        for column in unresolved {
            writeln!(
                index,
                "// {} {}: {}",
                column.operation, column.column, column.model
            )
            .expect("writing to a String cannot fail");
        }
    }
    files.push(GeneratedFile::new(
        format!("{COMPONENT_DIRECTORY}/index.ts").into_boxed_str(),
        index.into_bytes().into_boxed_slice(),
    ));
    Ok(files)
}

/// The screens this emitter writes today.
///
/// An operation with no route gets no component. `client_ts.rs` writes no
/// invoke function for one, so the component would import a name that the
/// module does not export, and the package would not type-check.
fn written(screen: &ScreenPlan<'_>) -> bool {
    screen.role.is_supported() && screen.contract.route.is_some()
}

fn emit_model(
    model: &ModelPlan<'_>,
    plan: &ClientPlan<'_>,
) -> Result<String, ClientComponentError> {
    let screens: Vec<&ScreenPlan<'_>> = model
        .screens
        .iter()
        .filter(|screen| written(screen))
        .collect();
    if screens.is_empty() {
        return Ok(String::new());
    }
    // A command that binds a revision names the record its read states, and
    // that read is the detail screen of this model.
    let records: BTreeMap<String, String> = screens
        .iter()
        .filter(|screen| screen.role == Role::Detail)
        .map(|screen| {
            (
                screen.contract.operation.clone(),
                format!(
                    "{}DetailInput",
                    crate::client_ts::type_stem(screen.model, screen.name)
                ),
            )
        })
        .collect();

    let mut body = String::new();
    let mut runtime = BTreeSet::new();
    let mut bindings = BTreeSet::new();
    // Bindings of another model, which a selector calls. Each one is imported
    // under an alias, because two models can both declare a `list`.
    let mut foreign: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Types of another model's components, which a prefill hands over.
    let mut sibling: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut solid = BTreeSet::new();
    // Exports of `@wamn/ui` this module renders through.
    let mut ui = BTreeSet::new();
    let mut form = false;
    let mut narrowed = false;
    for screen in &screens {
        match screen.role {
            Role::Table => {
                emit_definition_table(&mut body, screen, &mut runtime, &mut ui, &mut bindings);
                write_table_definition(
                    &mut body,
                    screen,
                    plan,
                    &mut bindings,
                    &mut foreign,
                    &mut sibling,
                )?;
            }
            Role::Detail => {
                solid.extend(["createResource", "onCleanup"]);
                emit_detail(&mut body, screen, &mut runtime, &mut ui, &mut bindings)?;
            }
            Role::Form => {
                form = true;
                solid.insert("createSignal");
                if screen
                    .inputs
                    .iter()
                    .any(|input| repeated_ancestor(&input.path).is_some())
                {
                    solid.insert("For");
                }
                if screen
                    .population
                    .iter()
                    .any(|populated| populated.narrowed_by.is_some())
                {
                    solid.insert("createEffect");
                    narrowed = true;
                }
                if screen.revision.is_some() {
                    solid.insert("createResource");
                }
                if !screen.population.is_empty() {
                    solid.insert("onCleanup");
                }
                emit_form(
                    &mut body,
                    screen,
                    &mut runtime,
                    &mut ui,
                    &mut bindings,
                    &mut foreign,
                    &records,
                )?;
            }
            Role::Delete => {
                solid.insert("createSignal");
                emit_delete(&mut body, screen, &mut runtime, &mut ui, &mut bindings)?;
            }
            role @ Role::Unsupported(_) => {
                return Err(ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    format!(
                        "{} states the role {role:?}, which gets no component",
                        screen.contract.operation
                    ),
                ));
            }
        }
    }

    // A table shows a badge, a row link or a refusal only when its plan names
    // one, so the import follows the markup the screens wrote.
    if body.contains("<Show") {
        solid.insert("Show");
    }

    let mut source = String::from("// @generated from the client-contract IR; do not edit.\n//\n");
    writeln!(
        source,
        "// `{}` components. Each one calls the bindings and the runtime, and\n// nothing else.",
        model.model.name
    )
    .expect("writing to a String cannot fail");
    // A module of tables alone renders no Solid primitive.
    if !solid.is_empty() {
        writeln!(
            source,
            "\nimport {{ {} }} from \"solid-js\";",
            solid.iter().copied().collect::<Vec<_>>().join(", ")
        )
        .expect("writing to a String cannot fail");
    }
    if form {
        // A narrowed selector follows the form's own values, so it reads the
        // store the form owns rather than a second copy of the value.
        if narrowed {
            source.push_str("import { createForm, useStore } from \"@tanstack/solid-form\";\n");
        } else {
            source.push_str("import { createForm } from \"@tanstack/solid-form\";\n");
        }
        source.push_str("import { z } from \"zod\";\n");
    }
    writeln!(
        source,
        "import {{\n{}\n}} from \"{RUNTIME_PACKAGE}\";",
        runtime
            .iter()
            .map(|name| format!("  {name},"))
            .collect::<Vec<_>>()
            .join("\n")
    )
    .expect("writing to a String cannot fail");
    // The UI a component renders through. The package owns how each one looks.
    if !ui.is_empty() {
        writeln!(
            source,
            "import {{\n{}\n}} from \"{UI_PACKAGE}\";",
            ui.iter()
                .map(|name| format!("  {name},"))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        "import {{\n{}\n}} from \"../{}.js\";",
        bindings
            .iter()
            .map(|name| format!("  {name},"))
            .collect::<Vec<_>>()
            .join("\n"),
        model.model.name
    )
    .expect("writing to a String cannot fail");
    for (module, names) in &sibling {
        writeln!(
            source,
            "import {{\n{}\n}} from \"./{module}.js\";",
            names
                .iter()
                .map(|name| format!("  {name},"))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .expect("writing to a String cannot fail");
    }
    for (module, names) in &foreign {
        writeln!(
            source,
            "import {{\n{}\n}} from \"../{module}.js\";",
            names
                .iter()
                .map(|name| format!("  {name},"))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .expect("writing to a String cannot fail");
    }
    // The spellings this module's schemas name. Only the ones it uses are
    // written, so a module carries no rule it does not apply.
    for (name, pattern, meaning) in WIRE_SPELLINGS {
        if body.contains(&format!("regex({name}")) {
            writeln!(source, "\n/** What the release accepts: {meaning}. */")
                .expect("writing to a String cannot fail");
            writeln!(source, "const {name} = {pattern};").expect("writing to a String cannot fail");
        }
    }
    source.push_str(&body);
    Ok(source)
}

/// The TypeScript member path of one contract path, as a literal list.
fn member_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(|part| crate::client_ts::to_camel(part.trim_end_matches("[]")))
        .collect()
}

/// The prop that carries one revision input no read supplies.
///
/// A nested revision such as `value.expected_row_version` is one prop,
/// `valueExpectedRowVersion`, because a prop name holds no dot.
fn revision_prop(path: &str) -> String {
    crate::client_ts::to_camel(&path.replace('.', "_"))
}

/// The repeated ancestor of one input path, when the contract declares one.
///
/// A contract marks a repeated member with `[]`, and every member below it
/// belongs to one element of that list.
fn repeated_ancestor(path: &str) -> Option<&str> {
    let end = path.find("[]")?;
    Some(&path[..end])
}

/// The member path as a TypeScript literal, for a runtime helper.
fn member_literal(path: &str) -> String {
    format!(
        "[{}]",
        member_path(path)
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The label of one field when the author states none: its name with spaces.
fn derived_label(path: &str) -> String {
    path.rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches("[]")
        .replace('_', " ")
}

/// The label of one field, authored if the manifest states one.
fn label(field: &FieldIr) -> String {
    field
        .label
        .clone()
        .unwrap_or_else(|| derived_label(&field.path))
}

/// The label of one screen, authored if the manifest states one.
///
/// The default applies the field rule to the operation's own name. No
/// component renders this: each module exports it, and the page that places
/// the component decides where the text goes. Owner ruling of 2026-09-22.
fn screen_label(screen: &ScreenPlan<'_>) -> String {
    screen
        .contract
        .label
        .clone()
        .unwrap_or_else(|| screen.name.replace('_', " "))
}

/// The button text of one row link, which is the target operation's own name.
fn link_label(identity: &str) -> String {
    let after_package = identity.split_once(':').map_or(identity, |(_, rest)| rest);
    let without_version = after_package
        .split_once('@')
        .map_or(after_package, |(name, _)| name);
    without_version
        .rsplit('/')
        .next()
        .unwrap_or(without_version)
        .replace('_', " ")
}

/// The cell type of one field, as `cellText` names it.
fn cell_type(field: &FieldIr) -> &str {
    match field.type_name.as_str() {
        "object" | "array" | "json" => field.type_name.as_str(),
        other if ts_type(other).is_ok() => other,
        _ => "json",
    }
}

/// One table screen: the QueryTable over its table definition.
///
/// The component holds no table code. The definition states the read, its
/// inputs and every operation the table calls, and `QueryTable` in the UI
/// package wires them.
fn emit_definition_table(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    ui: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let definition = format!(
        "{}_{}_TABLE",
        screen.model.to_uppercase(),
        screen.name.to_uppercase()
    );
    ui.extend(["QueryTable", "TableScreen"]);
    runtime.extend(["type Outcome", "type Transport"]);
    bindings.insert(format!("type {stem}Request"));
    bindings.insert(format!("type {stem}Result"));
    bindings.insert(format!("type {stem}Row"));

    writeln!(
        source,
        "\n/** What the table for `{}` takes. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}TableProps {{").expect("write");
    source.push_str("  /** The transport the application supplies. */\n");
    source.push_str("  readonly transport: Transport;\n");
    source.push_str("  /** Input the parent fixes, which the operator does not edit. */\n");
    writeln!(source, "  readonly fixed?: Partial<{stem}Request>;").expect("write");
    source.push_str(
        "  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */\n",
    );
    writeln!(
        source,
        "  readonly onOpen?: {{ readonly [operation: string]: (row: {stem}Row) => void }};"
    )
    .expect("write");
    source.push_str(
        "  /** For each operation whose form a row fills, what the filled values do. */\n",
    );
    source.push_str(
        "  readonly onFill?: { readonly [operation: string]: (initial: object) => void };\n",
    );
    source.push_str("  /** Called with every outcome this screen reads. */\n");
    writeln!(
        source,
        "  readonly onOutcome?: (outcome: Outcome<{stem}Result>) => void;\n}}"
    )
    .expect("write");
    writeln!(
        source,
        "\n/** What an operator calls this screen. The page decides where it goes. */\nexport const {stem}TableLabel = {:?};",
        screen_label(screen)
    )
    .expect("write");
    writeln!(
        source,
        "\n/** The table for `{}`: the QueryTable over `{definition}`, in the table screen. */\nexport function {stem}Table(props: {stem}TableProps) {{\n  return (\n    <TableScreen>\n      <QueryTable<{stem}Row, {stem}Result> definition={{{definition}}} label={{{stem}TableLabel}} {{...props}} />\n    </TableScreen>\n  );\n}}",
        screen.contract.operation
    )
    .expect("write");
}

/// The binding of one operation: the route and field maps its bindings
/// module exports. Another model's module is imported by name, because the
/// constants carry the model.
fn binding(
    model: &str,
    name: &str,
    own: &str,
    bindings: &mut BTreeSet<String>,
    foreign: &mut BTreeMap<String, BTreeSet<String>>,
) -> String {
    let stem = format!("{}_{}", model.to_uppercase(), name.to_uppercase());
    let names = [
        format!("{stem}_ROUTE"),
        format!("{stem}_REQUEST_FIELDS"),
        format!("{stem}_RESULT_FIELDS"),
    ];
    if model == own {
        bindings.extend(names.iter().cloned());
    } else {
        foreign
            .entry(model.to_owned())
            .or_default()
            .extend(names.iter().cloned());
    }
    format!(
        "{{ route: {}, request: {}, result: {} }}",
        names[0], names[1], names[2]
    )
}

/// The member path of one input a row fills, with `"[]"` after each
/// repeated member, as `fillMember` of the runtime reads it.
fn fill_literal(path: &str) -> String {
    let mut segments = Vec::new();
    for part in path.split('.') {
        segments.push(format!(
            "{:?}",
            crate::client_ts::to_camel(part.trim_end_matches("[]"))
        ));
        if part.ends_with("[]") {
            segments.push("\"[]\"".to_owned());
        }
    }
    format!("[{}]", segments.join(", "))
}

/// The table definition of one table screen, as data, beside its component.
///
/// It names the read as a binding, whether it answers one page (`item`) or
/// every row (`rows`), the input paths of the limit, the sort and each scope
/// filter, its scope filters, its sort, its row id, its page maximum and its
/// columns. A column names its row member, its label, the type the contract
/// states, and its role: the row id is the key, a column that names a record
/// is a reference, a revision is a revision, and any other column is a value.
/// A column that names a record also names the field its record read shows,
/// and that read. The definition states no mode and no cap, because no
/// manifest declares either.
///
/// The row id is the list of fields that `lists` keys the rows on. A read that
/// declares no page limit has a null page maximum, and a load reads what the
/// release answers.
///
/// It also names the update a cell edits through, with the columns it writes
/// and the inputs the runtime supplies, each operation a row opens with
/// whether it takes many rows and the inputs the row fills, and each child
/// table by its definition with the column its scope filter narrows.
fn write_table_definition(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    plan: &ClientPlan<'_>,
    bindings: &mut BTreeSet<String>,
    foreign: &mut BTreeMap<String, BTreeSet<String>>,
    sibling: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<(), ClientComponentError> {
    let operation = screen.contract;
    // Generation refuses a table read that states no row key.
    let lists = operation.lists.as_ref().ok_or_else(|| {
        ClientComponentError::new(
            ClientComponentErrorKind::UnwrittenRole,
            format!(
                "{} is a table read with no `lists` key",
                operation.operation
            ),
        )
    })?;
    let paging = screen.paging.as_ref();
    // The load state reads the rows of a page as `item` and its cursor as
    // `nextCursor`. A bounded list holds every row under `rows`, as one page
    // with no cursor.
    let rows = match screen.rows {
        crate::client_plan::Rows::List { key: "item" }
            if paging.is_some_and(|paging| paging.cursor_input.is_some()) =>
        {
            "item"
        }
        crate::client_plan::Rows::List { key: "rows" } => "rows",
        _ => {
            return Err(ClientComponentError::new(
                ClientComponentErrorKind::UnwrittenRole,
                format!(
                    "{} states the table role, but neither `item` rows with a cursor nor bounded `rows`",
                    operation.operation
                ),
            ));
        }
    };
    let quote = |text: &str| serde_json::Value::String(text.to_owned()).to_string();
    let path = |path: Option<&str>| path.map_or_else(|| "null".to_owned(), member_literal);

    writeln!(
        source,
        "\n/** The table definition of `{}`. */\nexport const {}_{}_TABLE = {{",
        operation.operation,
        screen.model.to_uppercase(),
        screen.name.to_uppercase()
    )
    .expect("write");
    // The file name of a CSV export starts with the model.
    writeln!(
        source,
        "  name: {},",
        quote(&screen.model.replace('_', "-"))
    )
    .expect("write");
    writeln!(
        source,
        "  read: {},",
        binding(screen.model, screen.name, screen.model, bindings, foreign)
    )
    .expect("write");
    writeln!(source, "  rows: {},", quote(rows)).expect("write");
    let row_id = lists
        .key_field
        .iter()
        .map(|field| column_member(field, &operation.operation).map(|member| quote(&member)))
        .collect::<Result<Vec<_>, _>>()?;
    writeln!(source, "  rowId: [{}],", row_id.join(", ")).expect("write");
    let maximum = paging
        .and_then(|paging| paging.limit)
        .map_or_else(|| "null".to_owned(), |limit| limit.maximum.to_string());
    writeln!(source, "  pageMaximum: {maximum},").expect("write");
    let sort = paging.and_then(|paging| paging.sort);
    writeln!(
        source,
        "  limitInput: {},",
        path(paging.and_then(|paging| paging.limit_input))
    )
    .expect("write");
    writeln!(
        source,
        "  sortFieldInput: {},",
        path(paging.and_then(|paging| sort.and(paging.sort_field_input)))
    )
    .expect("write");
    writeln!(
        source,
        "  sortDirectionInput: {},",
        path(paging.and_then(|paging| sort.and(paging.sort_direction_input)))
    )
    .expect("write");
    // A filter whose input the contract does not declare is no scope filter a
    // read can send.
    let mut filters = Vec::new();
    if let Some(paging) = paging {
        for filter in paging.filters {
            let Some(input) = paging
                .filter_inputs
                .iter()
                .find(|path| crate::client_plan::filter_input(path, &filter.field))
            else {
                continue;
            };
            filters.push(format!(
                "{{ field: {}, input: {}, list: {} }}",
                quote(&column_member(&filter.field, &operation.operation)?),
                member_literal(input),
                input.ends_with("[]")
            ));
        }
    }
    writeln!(source, "  filters: [{}],", filters.join(", ")).expect("write");
    let scope = paging
        .map_or(&[][..], |paging| paging.filters)
        .iter()
        .map(|filter| {
            column_member(&filter.field, &operation.operation).map(|member| quote(&member))
        })
        .collect::<Result<Vec<_>, _>>()?;
    writeln!(source, "  scopeFilters: [{}],", scope.join(", ")).expect("write");
    // A sort field names its row member, which the table matches, and its wire
    // name, which the request sends.
    let fields = sort
        .map_or(&[][..], |sort| &sort.fields[..])
        .iter()
        .map(|field| {
            column_member(field, &operation.operation)
                .map(|member| format!("{{ field: {}, wire: {} }}", quote(&member), quote(field)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let directions = sort
        .map_or(&[][..], |sort| &sort.directions[..])
        .iter()
        .map(|direction| quote(direction))
        .collect::<Vec<_>>();
    // A contract that states no bound sorts by one field.
    let max_fields = sort.and_then(|sort| sort.max_fields).unwrap_or(1);
    writeln!(source, "  sortFields: [{}],", fields.join(", ")).expect("write");
    writeln!(source, "  sortDirections: [{}],", directions.join(", ")).expect("write");
    writeln!(source, "  sortMaxFields: {max_fields},").expect("write");
    source.push_str("  columns: [\n");
    for column in &screen.columns {
        column_type(column, &operation.operation)?;
        write!(
            source,
            "    {{ field: {}, label: {}, type: {}",
            quote(&column_member(&column.path, &operation.operation)?),
            quote(&label(column)),
            quote(&column.type_name)
        )
        .expect("write");
        let resolved = screen
            .resolved_columns
            .iter()
            .find(|resolved| resolved.column == column.path);
        // The role decides the column's default aggregate.
        let role = if lists.key_field.contains(&column.path) {
            "key"
        } else if resolved.is_some() || column.references.is_some() {
            "reference"
        } else if column.revision.is_revision() {
            "revision"
        } else {
            "value"
        };
        write!(source, ", role: {}", quote(role)).expect("write");
        if let Some(resolved) = resolved {
            write!(
                source,
                ", displayField: {}, recordRead: {{ read: {}, keyInput: {} }}",
                quote(&crate::client_ts::to_camel(resolved.display_field)),
                binding(
                    resolved.read_model,
                    resolved.read_name,
                    screen.model,
                    bindings,
                    foreign
                ),
                member_literal(resolved.key_input)
            )
            .expect("write");
        }
        source.push_str(" },\n");
    }
    source.push_str("  ],\n");
    if let Some(update) = &screen.update {
        write!(
            source,
            "  update: {{ operation: {}, binding: {}, keyInput: {}",
            quote(update.operation),
            binding(update.model, update.name, screen.model, bindings, foreign),
            member_literal(update.key_input)
        )
        .expect("write");
        if let (Some(input), Some(field)) = (update.revision_input, update.revision_field) {
            write!(
                source,
                ", revisionInput: {}, revisionField: {}",
                member_literal(input),
                quote(&column_member(field, &operation.operation)?)
            )
            .expect("write");
        }
        let supplied = plan
            .screens()
            .find(|candidate| candidate.contract.operation == update.operation)
            .map_or(&[][..], |candidate| &candidate.supplied[..])
            .iter()
            .map(|field| {
                let kind = match field.kind {
                    SuppliedKind::RequestId => "requestId",
                    SuppliedKind::IdempotencyKey => "idempotencyKey",
                    SuppliedKind::OccurredAt => "occurredAt",
                };
                format!(
                    "{{ input: {}, kind: {} }}",
                    member_literal(field.path),
                    quote(kind)
                )
            })
            .collect::<Vec<_>>();
        write!(source, ", supplied: [{}]", supplied.join(", ")).expect("write");
        source.push_str(", fields: [\n");
        for field in &update.fields {
            writeln!(
                source,
                "    {{ field: {}, input: {} }},",
                quote(&column_member(field.column, &operation.operation)?),
                member_literal(field.input)
            )
            .expect("write");
        }
        source.push_str("  ] },\n");
    }
    // A row opens a record by itself, or fills the inputs of a form.
    let actions = screen
        .actions
        .iter()
        .map(|action| {
            let row_form = screen
                .row_forms
                .iter()
                .find(|form| form.operation == action.operation);
            let mut tail = String::new();
            if let Some(form) = row_form {
                if let Some((field, input)) = form.revision {
                    write!(
                        tail,
                        ", revision: {{ field: {}, input: {} }}",
                        quote(&column_member(field, &operation.operation)?),
                        member_literal(input)
                    )
                    .expect("write");
                }
                // A form that sends one input for each of many rows runs a
                // bulk action. Its component is read when the action runs, so
                // modules that import each other load.
                let target = plan
                    .screens()
                    .find(|candidate| candidate.contract.operation == form.operation);
                if action.many && target.is_some_and(many_rows) {
                    let component =
                        format!("{}Form", crate::client_ts::type_stem(form.model, form.name));
                    if form.model != screen.model {
                        sibling
                            .entry(form.model.to_owned())
                            .or_default()
                            .insert(component.clone());
                    }
                    write!(tail, ", form: () => {component}").expect("write");
                }
            }
            let (opens, fill) = match row_form {
                Some(form) => (
                    "form",
                    form.pairs
                        .iter()
                        .map(|(field, input)| {
                            column_member(field, &operation.operation).map(|member| {
                                format!(
                                    "{{ field: {}, input: {} }}",
                                    quote(&member),
                                    fill_literal(input)
                                )
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                None => ("record", Vec::new()),
            };
            Ok(format!(
                "{{ operation: {}, label: {}, many: {}, opens: {}, fill: [{}]{tail} }}",
                quote(action.operation),
                quote(&link_label(action.operation)),
                action.many,
                quote(opens),
                fill.join(", ")
            ))
        })
        .collect::<Result<Vec<_>, ClientComponentError>>()?;
    // A child that gets no component of its own is no child table. Its
    // definition is read when a row expands, so modules that import each
    // other load.
    let children = screen
        .child_tables
        .iter()
        .filter_map(|child| {
            plan.screens().find(|candidate| {
                candidate.contract.operation == child.operation && written(candidate)
            })?;
            Some(column_member(child.field, child.operation).map(|field| {
                let name = format!(
                    "{}_{}_TABLE",
                    child.model.to_uppercase(),
                    child.name.to_uppercase()
                );
                if child.model != screen.model {
                    sibling
                        .entry(child.model.to_owned())
                        .or_default()
                        .insert(name.clone());
                }
                format!(
                    "{{ label: {}, table: () => {name}, scopeFilter: {} }}",
                    // The expanded row names the child by the model its rows are.
                    quote(&child.model.replace('_', " ")),
                    quote(&field)
                )
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (name, items) in [("actions", actions), ("childTables", children)] {
        if items.is_empty() {
            writeln!(source, "  {name}: [],").expect("write");
        } else {
            writeln!(source, "  {name}: [\n    {},\n  ],", items.join(",\n    ")).expect("write");
        }
    }
    source.push_str("} as const;\n");
    Ok(())
}

/// The row member of one result field, which is a leaf of the row.
fn column_member(path: &str, operation: &str) -> Result<String, ClientComponentError> {
    if path.contains('.') {
        return Err(ClientComponentError::new(
            ClientComponentErrorKind::UnwrittenColumn,
            format!("{operation} table column {path:?} is not a member of its row"),
        ));
    }
    Ok(crate::client_ts::to_camel(path))
}

/// Refuse a table column whose type is not one of the frozen `sql-value` names.
fn column_type(field: &FieldIr, operation: &str) -> Result<(), ClientComponentError> {
    use wamn_schema_introspection::ir::ColumnType;

    serde_json::from_value::<ColumnType>(serde_json::Value::String(field.type_name.clone()))
        .map(|_| ())
        .map_err(|_| {
            ClientComponentError::new(
                ClientComponentErrorKind::UnwrittenColumn,
                format!(
                    "{operation} table column {:?} has type {:?}, which is not a sql-value type",
                    field.path, field.type_name
                ),
            )
        })
}

fn emit_detail(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    ui: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) -> Result<(), ClientComponentError> {
    ui.extend(["DetailItem", "DetailList", "FieldError", "announceOutcome"]);
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "afterWrites",
        "cellText",
        "readMember",
        "type Outcome",
        "type Transport",
    ]);
    bindings.insert(function.clone());
    bindings.insert(format!("type {stem}Request"));
    bindings.insert(format!("type {stem}Result"));

    writeln!(
        source,
        "\n/** The record that the detail for `{}` reads. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}DetailInput {{").expect("write");
    alias_imports(&operator_inputs(screen), false, runtime);
    write_record_members(source, &operator_inputs(screen), &[], 1);
    source.push_str("}\n");
    writeln!(
        source,
        "\n/** What the detail screen for `{}` takes. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}DetailProps {{").expect("write");
    source.push_str("  /** The transport the application supplies. */\n");
    source.push_str("  readonly transport: Transport;\n");
    source.push_str("  /** The input that names the record. */\n");
    writeln!(source, "  readonly input: {stem}DetailInput;").expect("write");
    source.push_str("  /** Called with every outcome this screen reads. */\n");
    writeln!(
        source,
        "  readonly onOutcome?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
    source.push_str("}\n");

    writeln!(
        source,
        "\n/** What an operator calls this screen. The page decides where it goes. */\nexport const {stem}DetailLabel = {:?};",
        screen_label(screen)
    )
    .expect("write");
    writeln!(
        source,
        "\n/**\n * The detail screen for `{}`.\n *\n * It reads when it mounts and again whenever its input changes, because the\n * input names the record it shows.\n */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "export function {stem}Detail(props: {stem}DetailProps) {{"
    )
    .expect("write");
    writeln!(
        source,
        "  const [outcome, {{ refetch: readAgain }}] = createResource(\n    () => props.input,\n    async (input: {stem}DetailInput) => {{\n      const read = await {function}(props.transport, [\n        input as {stem}Request,\n      ]);\n      props.onOutcome?.(read);\n      if (read.status !== \"completed\") {{\n        announceOutcome(read, {stem}DetailLabel);\n      }}\n      return read;\n    }},\n  );"
    )
    .expect("write");
    // A write can change the record a detail shows, so it reads it again.
    source.push_str("  onCleanup(afterWrites(props.transport, () => void readAgain()));\n");
    writeln!(
        source,
        "  const record = (): {stem}Result | undefined => {{\n    const read = outcome();\n    return read?.status === \"completed\" ? read.value : undefined;\n  }};"
    )
    .expect("write");
    source.push_str("  const state = () => outcome()?.status;\n");
    source.push_str("\n  return (\n    <section>\n      <Show when={state() !== undefined && state() !== \"completed\"}>\n        <FieldError>{state()}</FieldError>\n      </Show>\n      <DetailList loading={outcome.loading}>\n");
    for column in &screen.columns {
        writeln!(
            source,
            "        <DetailItem term={:?}>{{cellText(readMember(record(), {}), {:?})}}</DetailItem>",
            label(column),
            member_literal(&column.path),
            cell_type(column)
        )
        .expect("write");
    }
    source.push_str("      </DetailList>\n    </section>\n  );\n}\n");
    Ok(())
}

/// The spellings a value that travels as text must match, each with the
/// meaning the emitted comment states.
///
/// Each one states what `crates/client/core/src/request.rs` canonicalizes, so
/// a form refuses exactly what the release would refuse.
const WIRE_SPELLINGS: [(&str, &str, &str); 4] = [
    (
        "UUID_TEXT",
        "/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/",
        "one UUID, hyphenated",
    ),
    (
        "TIMESTAMP_TEXT",
        "/^\\d{4}-\\d{2}-\\d{2}[Tt ]\\d{2}:\\d{2}:\\d{2}(\\.\\d+)?([Zz]|[+-]\\d{2}:\\d{2})$/",
        "one RFC3339 timestamp",
    ),
    (
        "NUMERIC_TEXT",
        "/^[+-]?(\\d+(\\.\\d*)?|\\.\\d+)$/",
        "decimal text without an exponent",
    ),
    ("INTEGER_TEXT", "/^[+-]?\\d+$/", "one whole number"),
];

/// The zod expression for one input field.
///
/// It states what the release declared: the type, the closed value domain, the
/// admitted null, and whether the property must be present. Nothing here
/// checks a reply, because the platform is the authority on its own values.
fn zod_type(field: &FieldIr) -> String {
    let base = match field.type_name.as_str() {
        "boolean" => "z.boolean()".to_owned(),
        "int32" | "float64" => "z.number()".to_owned(),
        "json" | "object" | "array" => "z.unknown()".to_owned(),
        _ if !field.values.is_empty() => format!(
            "z.enum([{}])",
            field
                .values
                .iter()
                .map(|value| format!("{value:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        // A type that travels as text states the spelling the request
        // canonicalizer accepts, so the form names the field before the
        // request goes out instead of the release refusing it later.
        "uuid" => "z.string().regex(UUID_TEXT, \"expected a UUID\")".to_owned(),
        "timestamptz" => {
            "z.string().regex(TIMESTAMP_TEXT, \"expected an RFC3339 timestamp\")".to_owned()
        }
        "numeric" => "z.string().regex(NUMERIC_TEXT, \"expected decimal text\")".to_owned(),
        "int64" => "z.string().regex(INTEGER_TEXT, \"expected a whole number\")".to_owned(),
        _ => "z.string()".to_owned(),
    };
    let mut spelling = base;
    if field.nullable {
        spelling.push_str(".nullable()");
    }
    if !field.required {
        spelling.push_str(".optional()");
    }
    spelling
}

/// The inputs an operator fills, which excludes a key the binding supplies.
///
/// A bound command takes the record as a prop and writes its key from the read,
/// so a control for that key would show a value the submission overwrites.
fn operator_inputs<'a>(screen: &'a ScreenPlan<'a>) -> Vec<&'a FieldIr> {
    let bound = screen.revision.map(|binding| binding.command_key_input);
    screen
        .inputs
        .iter()
        .filter(|input| Some(input.path.as_str()) != bound)
        .copied()
        .collect()
}

/// The zod object of one level of the operator's input.
///
/// The members mirror the request type exactly, so the check runs over the
/// value the form holds. A member that carries members of its own is optional,
/// because a form fills it one field at a time.
fn write_input_schema(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    inputs: &[&FieldIr],
    prefix: &[String],
    depth: usize,
) {
    let indent = "  ".repeat(depth);
    let mut written: Vec<String> = Vec::new();
    for input in inputs {
        let path = member_path(&input.path);
        if path.len() <= prefix.len() || !path.starts_with(prefix) {
            continue;
        }
        let name = path[prefix.len()].clone();
        if written.contains(&name) {
            continue;
        }
        written.push(name.clone());
        if path.len() == prefix.len() + 1 {
            writeln!(source, "{indent}{name}: {},", zod_type(input)).expect("write");
            continue;
        }
        let mut deeper = prefix.to_vec();
        deeper.push(name.clone());
        let group = inputs.iter().find(|input| {
            repeated_ancestor(&input.path).is_some_and(|ancestor| member_path(ancestor) == deeper)
        });
        writeln!(source, "{indent}{name}: z").expect("write");
        writeln!(source, "{indent}  .object({{").expect("write");
        write_input_schema(source, screen, inputs, &deeper, depth + 2);
        writeln!(source, "{indent}  }})").expect("write");
        if let Some(group) = group {
            // The declared bounds of the group, so the form refuses a list
            // that the release would refuse.
            let declared =
                repeated_ancestor(&group.path).and_then(|ancestor| leaf_group(screen, ancestor));
            writeln!(source, "{indent}  .array()").expect("write");
            if let Some(minimum) = declared.and_then(|group| group.minimum) {
                writeln!(source, "{indent}  .min({minimum})").expect("write");
            }
            if let Some(maximum) = declared.and_then(|group| group.maximum) {
                writeln!(source, "{indent}  .max({maximum})").expect("write");
            }
        }
        writeln!(source, "{indent}  .optional(),").expect("write");
    }
}

/// The members that name one record, as a caller states them.
///
/// The members are the operator inputs alone, so a caller never states a value
/// that the platform writes, such as the request identity. Each member keeps
/// what the contract declares: a required member stays required.
fn write_record_members(source: &mut String, inputs: &[&FieldIr], prefix: &[String], depth: usize) {
    let indent = "  ".repeat(depth);
    let mut written: Vec<String> = Vec::new();
    for input in inputs {
        let path = member_path(&input.path);
        if path.len() <= prefix.len() || !path.starts_with(prefix) {
            continue;
        }
        let name = path[prefix.len()].clone();
        if written.contains(&name) {
            continue;
        }
        let mut deeper = prefix.to_vec();
        deeper.push(name.clone());
        let repeated = inputs.iter().any(|input| {
            repeated_ancestor(&input.path).is_some_and(|ancestor| member_path(ancestor) == deeper)
        });
        if repeated {
            continue;
        }
        written.push(name.clone());
        let optional = if input.required { "" } else { "?" };
        if path.len() == prefix.len() + 1 {
            let spelling = crate::client_ts::ts_type(&input.type_name).unwrap_or("string");
            let nullable = if input.nullable { " | null" } else { "" };
            writeln!(
                source,
                "{indent}readonly {name}{optional}: {spelling}{nullable};"
            )
            .expect("write");
            continue;
        }
        writeln!(source, "{indent}readonly {name}{optional}: {{").expect("write");
        write_record_members(source, inputs, &deeper, depth + 1);
        writeln!(source, "{indent}}};").expect("write");
    }
}

/// The wire aliases that one set of inputs names.
///
/// A record type holds no repeated group, and the initial values of a form do,
/// so `lists` says whether an input inside a repeated group counts.
fn alias_imports(inputs: &[&FieldIr], lists: bool, runtime: &mut BTreeSet<&'static str>) {
    for input in inputs
        .iter()
        .filter(|input| lists || repeated_ancestor(&input.path).is_none())
    {
        match crate::client_ts::ts_type(&input.type_name).unwrap_or("string") {
            "Int64" => runtime.insert("type Int64"),
            "Numeric" => runtime.insert("type Numeric"),
            "Timestamptz" => runtime.insert("type Timestamptz"),
            "Uuid" => runtime.insert("type Uuid"),
            _ => false,
        };
    }
}

/// The values one form can start with, as a type of its own.
///
/// The members are the operator inputs alone, and every level is optional, so a
/// caller can prefill one member of a nested value. A repeated group is a list
/// of elements whose members are optional too, so a row can fill one element.
fn write_initial_members(
    source: &mut String,
    inputs: &[&FieldIr],
    prefix: &[String],
    depth: usize,
) {
    let indent = "  ".repeat(depth);
    let mut written: Vec<String> = Vec::new();
    for input in inputs {
        let path = member_path(&input.path);
        if path.len() <= prefix.len() || !path.starts_with(prefix) {
            continue;
        }
        let name = path[prefix.len()].clone();
        if written.contains(&name) {
            continue;
        }
        let mut deeper = prefix.to_vec();
        deeper.push(name.clone());
        let repeated = inputs.iter().any(|input| {
            repeated_ancestor(&input.path).is_some_and(|ancestor| member_path(ancestor) == deeper)
        });
        written.push(name.clone());
        let list = if repeated { "[]" } else { "" };
        if path.len() == prefix.len() + 1 {
            // A declared domain types as its union, as the bindings do.
            let spelling = if input.values.is_empty() {
                crate::client_ts::ts_type(&input.type_name)
                    .unwrap_or("string")
                    .to_owned()
            } else {
                input
                    .values
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            };
            let nullable = if input.nullable { " | null" } else { "" };
            let spelling = if repeated {
                format!("({spelling}{nullable})[]")
            } else {
                format!("{spelling}{nullable}")
            };
            writeln!(source, "{indent}{name}?: {spelling};").expect("write");
            continue;
        }
        writeln!(source, "{indent}{name}?: {{").expect("write");
        write_initial_members(source, inputs, &deeper, depth + 1);
        writeln!(source, "{indent}}}{list};").expect("write");
    }
}

/// One form screen: what the operator types, and what the platform supplies.
fn emit_form(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    ui: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
    foreign: &mut BTreeMap<String, BTreeSet<String>>,
    records: &BTreeMap<String, String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "checkedMember",
        "newRequestId",
        "refusalMarks",
        "refusalSentence",
        "refusedMember",
        "writeMember",
        "type Outcome",
        "type Transport",
    ]);
    if !screen.supplied.is_empty() {
        for supplied in &screen.supplied {
            match supplied.kind {
                SuppliedKind::RequestId => runtime.insert("newRequestId"),
                SuppliedKind::IdempotencyKey => runtime.insert("newIdempotencyKey"),
                SuppliedKind::OccurredAt => runtime.insert("occurredAt"),
            };
        }
    }
    bindings.insert(function.clone());
    bindings.insert(format!("type {stem}Request"));
    bindings.insert(format!("type {stem}Result"));

    let fields = format!(
        "{}_{}_REQUEST_FIELDS",
        screen.model.to_uppercase(),
        screen.name.to_uppercase()
    );
    bindings.insert(fields.clone());

    // What the operator types, checked before the request goes out.
    writeln!(
        source,
        "\n/** What an operator types for `{}`. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "const {}_INPUT = z.object({{",
        screen.name.to_uppercase()
    )
    .expect("write");
    write_input_schema(source, screen, &operator_inputs(screen), &[], 1);
    source.push_str("});\n");

    // What a caller can prefill, and then the props.
    writeln!(
        source,
        "\n/** What the form for `{}` can start with. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}FormInitial {{").expect("write");
    // The initial values name the wire aliases their leaves carry.
    alias_imports(&operator_inputs(screen), true, runtime);
    write_initial_members(source, &operator_inputs(screen), &[], 1);
    source.push_str("}\n");
    writeln!(
        source,
        "\n/** What the form for `{}` takes. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}FormProps {{").expect("write");
    source.push_str("  /** The transport the application supplies. */\n");
    source.push_str("  readonly transport: Transport;\n");
    source.push_str("  /** Values the form starts with. */\n");
    writeln!(source, "  readonly initial?: {stem}FormInitial;").expect("write");
    if let Some(binding) = screen.revision {
        let read = crate::client_ts::operation_stem(binding.read_operation);
        let key = records
            .get(binding.read_operation)
            .cloned()
            .unwrap_or_else(|| format!("{read}Request"));
        writeln!(
            source,
            "  /** The record this command changes. The form reads it when it opens, and\n   * sends the revision it read, because `{}` states that binding. */",
            binding.read_operation
        )
        .expect("write");
        writeln!(source, "  readonly key: {key};").expect("write");
    } else {
        for revision in &screen.revision_inputs {
            // The row the operator chooses supplies this one, so no prop does.
            if chosen(screen, revision).is_some() {
                continue;
            }
            source.push_str(
                "  /** The revision this command sends. The release binds no read that supplies it. */\n",
            );
            write!(
                source,
                "  readonly {}: {stem}Request",
                revision_prop(revision)
            )
            .expect("write");
            for name in member_path(revision) {
                write!(source, "[{name:?}]").expect("write");
            }
            source.push_str(";\n");
        }
    }
    source.push_str("  /** Called with the outcome of every submission. */\n");
    writeln!(
        source,
        "  readonly onSubmitted?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
    let many = many_rows(screen);
    if many {
        source.push_str("  /**\n   * The values each selected row fills. One submission then sends one input\n   * for each row, in one call, and the form hides the inputs the rows fill.\n   */\n");
        source.push_str("  readonly rows?: readonly object[];\n");
        writeln!(
            source,
            "  /** Called with the outcome of each row's input, in row order. */\n  readonly onEach?: (outcomes: readonly Outcome<{stem}Result>[]) => void;"
        )
        .expect("write");
    }
    source.push_str("}\n");

    // The component.
    writeln!(
        source,
        "\n/** What an operator calls this screen. The page decides where it goes. */\nexport const {stem}FormLabel = {:?};",
        screen_label(screen)
    )
    .expect("write");
    writeln!(
        source,
        "\n/**\n * The form for `{}`.\n *\n * It renders what the operator fills and nothing else. The reserved inputs\n * come from the runtime at submit time, and the operator never sees them.\n */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "export function {stem}Form(props: {stem}FormProps) {{"
    )
    .expect("write");
    source.push_str(
        "  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);\n",
    );
    // True after the last submission completed, so the form shows its result.
    source.push_str("  const [done, setDone] = createSignal(false);\n");
    if let Some(binding) = screen.revision {
        let read = crate::client_ts::function_name(local_name(binding.read_operation)).map_err(
            |error| {
                ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    error.to_string(),
                )
            },
        )?;
        let read_stem = crate::client_ts::operation_stem(binding.read_operation);
        let key = records
            .get(binding.read_operation)
            .cloned()
            .unwrap_or_else(|| format!("{read_stem}Request"));
        bindings.insert(read.clone());
        bindings.insert(format!("type {read_stem}Request"));
        runtime.insert("readMember");
        source.push_str("  // The record this command changes, read when the form opens and again\n  // when its key changes. The form sends the revision of this read, so a\n  // change another writer makes after it refuses as a conflict.\n");
        writeln!(
            source,
            "  const [record, {{ refetch: readAgain }}] = createResource(\n    () => props.key,\n    (key: {key}) => {read}(props.transport, [key]),\n  );"
        )
        .expect("write");
    }
    source.push_str("\n  const form = createForm(() => ({\n");
    // The form holds the whole request, including the repeated group it owns.
    // The caller states the operator inputs alone, which `initial` names.
    writeln!(
        source,
        "    defaultValues: {{ ...props.initial }} as Partial<{stem}Request>,"
    )
    .expect("write");
    source.push_str("    onSubmit: async ({ value }: { value: Partial<");
    writeln!(source, "{stem}Request> }}) => {{").expect("write");
    source.push_str("      setDone(false);\n");
    if many {
        emit_many_submit(source, screen, &stem, &fields, runtime, bindings);
    }
    writeln!(
        source,
        "      const checked = {}_INPUT.safeParse(value);",
        screen.name.to_uppercase()
    )
    .expect("write");
    writeln!(
        source,
        "      if (!checked.success) {{\n        const issue = checked.error.issues[0];\n        setRefusal({{\n          text: issue?.message ?? \"A value is not valid.\",\n          member: checkedMember(\n            issue?.path as (string | number)[] | undefined,\n            {fields},\n          ),\n        }});\n        return;\n      }}"
    )
    .expect("write");
    writeln!(source, "      let item = {{ ...value }} as {stem}Request;").expect("write");
    write_supplied(source, screen, "      ");
    if let Some(binding) = screen.revision {
        source.push_str("      // The revision is the one the form read when it opened, never one read\n      // now, because a change made since then is what the conflict outcome names.\n");
        source.push_str(
            "      const read = record();\n      if (read?.status !== \"completed\") {\n        setRefusal({ text: \"The record this form changes is not read yet.\", member: null });\n        return;\n      }\n",
        );
        writeln!(
            source,
            "      item = writeMember(item, {}, readMember(read.value, {}) ?? null);",
            member_literal(binding.command_key_input),
            member_literal(binding.key_field)
        )
        .expect("write");
        writeln!(
            source,
            "      item = writeMember(item, {}, readMember(read.value, {}) ?? null);",
            member_literal(binding.command_revision_input),
            member_literal(binding.revision_field)
        )
        .expect("write");
    } else {
        for revision in &screen.revision_inputs {
            write_revision(source, screen, revision, "      ");
        }
    }
    writeln!(
        source,
        "      const outcome = await {function}(props.transport, [item]);"
    )
    .expect("write");
    source.push_str("      props.onSubmitted?.(outcome);\n");
    if screen.revision.is_some() {
        // Its own write moved the revision, so the next submission needs it.
        source.push_str(
            "      if (outcome.status === \"completed\") {\n        void readAgain();\n      }\n",
        );
    }
    // Every outcome of a submission is announced. A refusal that names a
    // member still marks that member in place.
    writeln!(source, "      announceOutcome(outcome, {stem}FormLabel);").expect("write");
    source.push_str(
        "      setRefusal(\n        outcome.status === \"refused\"\n          ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }\n          : null,\n      );\n      setDone(outcome.status === \"completed\");\n",
    );
    source.push_str("    },\n  }));\n");

    // One accessor over the form's values, when a selector narrows by one of
    // them. `getFieldValue` reads a value without following it, so the effect
    // below follows this accessor and then reads the member the plan named.
    if screen
        .population
        .iter()
        .any(|populated| populated.narrowed_by.is_some())
    {
        source.push_str("  const formValues = useStore(form.store, (state) => state.values);\n");
    }

    // One selector state for each input the operator chooses from a list.
    for populated in &screen.population {
        let list_stem = crate::client_ts::type_stem(populated.list_model, populated.list_name);
        let list_function =
            crate::client_ts::function_name(populated.list_name).map_err(|error| {
                ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    error.to_string(),
                )
            })?;
        let alias = foreign_alias(populated.list_model, populated.list_name);
        let names = if populated.list_model == screen.model {
            &mut *bindings
        } else {
            foreign.entry(populated.list_model.to_owned()).or_default()
        };
        names.insert(format!("{list_function} as {alias}"));
        names.insert(format!("type {list_stem}Request"));
        names.insert(format!("type {list_stem}Row"));
        if let Some(read) = populated.read {
            let read_function = crate::client_ts::function_name(read.name).map_err(|error| {
                ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    error.to_string(),
                )
            })?;
            let read_stem = crate::client_ts::type_stem(read.model, read.name);
            let names = if read.model == screen.model {
                &mut *bindings
            } else {
                foreign.entry(read.model.to_owned()).or_default()
            };
            names.insert(format!(
                "{read_function} as {}",
                foreign_alias(read.model, read.name)
            ));
            names.insert(format!("type {read_stem}Request"));
        }
        emit_selector_state(source, populated, runtime);
    }

    if many {
        // Whether the rows of a bulk action fill one input, which the form
        // then hides.
        source.push_str("  const rowsFill = (path: readonly string[]) =>\n    props.rows?.some((row) => readMember(row, path) !== undefined) ?? false;\n");
    }

    // The markup. A refusal that names no member reads above the controls.
    ui.extend([
        "Button",
        "FieldError",
        "FormActions",
        "FormDone",
        "announceOutcome",
    ]);
    source.push_str(
        "\n  return (\n    <form\n      onSubmit={(event) => {\n        event.preventDefault();\n        void form.handleSubmit();\n      }}\n    >\n      <Show when={refusal()?.member === null ? refusal() : undefined}>\n        <FieldError>{refusal()?.text}</FieldError>\n      </Show>\n",
    );
    let inputs = operator_inputs(screen);
    let mut repeated_written: Vec<String> = Vec::new();
    if inputs
        .iter()
        .any(|input| repeated_ancestor(&input.path).is_some())
    {
        runtime.extend(["canAdd", "canRemove"]);
    }
    let mut fields = String::new();
    for input in &inputs {
        match repeated_ancestor(&input.path) {
            None if many => {
                let mut field = String::new();
                emit_field(
                    &mut field,
                    input,
                    &member_path(&input.path).join("."),
                    6,
                    None,
                    populated(screen, &input.path),
                    ui,
                );
                writeln!(
                    fields,
                    "      <Show when={{!rowsFill({})}}>",
                    member_literal(&input.path)
                )
                .expect("write");
                fields.push_str(&deepen(&field));
                fields.push_str("      </Show>\n");
            }
            None => emit_field(
                &mut fields,
                input,
                &member_path(&input.path).join("."),
                6,
                None,
                populated(screen, &input.path),
                ui,
            ),
            Some(ancestor) => {
                let ancestor = ancestor.to_owned();
                if repeated_written.contains(&ancestor) {
                    continue;
                }
                repeated_written.push(ancestor.clone());
                emit_repeated_group(&mut fields, screen, &inputs, &ancestor, &stem, ui);
            }
        }
    }
    // The group spaces the fields, so no field states a margin.
    if !fields.is_empty() {
        ui.insert("FieldGroup");
        source.push_str("      <FieldGroup>\n");
        source.push_str(&deepen(&fields));
        source.push_str("      </FieldGroup>\n");
    }
    source.push_str(
        "      <FormActions>\n        <FormDone when={done()} />\n        <Button type=\"submit\">submit</Button>\n      </FormActions>\n    </form>\n  );\n}\n",
    );
    Ok(())
}

/// Whether a form sends one input for each of many rows: its operation
/// takes many outer inputs, and no read it binds names one record.
fn many_rows(screen: &ScreenPlan<'_>) -> bool {
    crate::client_plan::takes_many(screen.contract) && screen.revision.is_none()
}

/// The writes of the inputs the runtime supplies, at `pad`.
fn write_supplied(source: &mut String, screen: &ScreenPlan<'_>, pad: &str) {
    for supplied in &screen.supplied {
        let value = match supplied.kind {
            SuppliedKind::RequestId => "newRequestId()",
            SuppliedKind::IdempotencyKey => "newIdempotencyKey()",
            SuppliedKind::OccurredAt => "occurredAt()",
        };
        writeln!(
            source,
            "{pad}item = writeMember(item, {}, {value});",
            member_literal(supplied.path)
        )
        .expect("write");
    }
}

/// The write of one revision input that no bound read supplies, at `pad`.
fn write_revision(source: &mut String, screen: &ScreenPlan<'_>, revision: &str, pad: &str) {
    if let Some(populated) = chosen(screen, revision) {
        // The revision is the one the chosen row carried when the operator
        // chose it, never one read now.
        let (state, _) = selector_state(populated.input);
        writeln!(
            source,
            "{pad}const {state}Chosen = {state}Revision();\n{pad}if ({state}Chosen === null) {{\n{pad}  setRefusal({{ text: \"Choose the record from its list.\", member: {:?} }});\n{pad}  return;\n{pad}}}\n{pad}item = writeMember(item, {}, {state}Chosen);",
            populated.input,
            member_literal(revision)
        )
        .expect("write");
        return;
    }
    writeln!(
        source,
        "{pad}item = writeMember(item, {}, props.{});",
        member_literal(revision),
        revision_prop(revision)
    )
    .expect("write");
}

/// The submission of a bulk action: one input for each row, in one call.
///
/// Each input is the typed values with the row's values written over them.
/// A revision the row carries is the row's own, and any other revision comes
/// as it does for one input. The outcomes go to the caller, which shows each
/// on its row.
fn emit_many_submit(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    stem: &str,
    fields: &str,
    runtime: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) {
    runtime.extend(["callEach", "mergeMembers", "readMember"]);
    let constant = format!(
        "{}_{}",
        screen.model.to_uppercase(),
        screen.name.to_uppercase()
    );
    bindings.insert(format!("{constant}_ROUTE"));
    bindings.insert(format!("{constant}_RESULT_FIELDS"));
    source.push_str("      if (props.rows !== undefined) {\n");
    writeln!(source, "        const items: {stem}Request[] = [];").expect("write");
    source.push_str("        for (const row of props.rows) {\n");
    source.push_str("          const each = mergeMembers(value, row);\n");
    writeln!(
        source,
        "          const checked = {}_INPUT.safeParse(each);",
        screen.name.to_uppercase()
    )
    .expect("write");
    writeln!(
        source,
        "          if (!checked.success) {{\n            const issue = checked.error.issues[0];\n            setRefusal({{\n              text: issue?.message ?? \"A value is not valid.\",\n              member: checkedMember(\n                issue?.path as (string | number)[] | undefined,\n                {fields},\n              ),\n            }});\n            return;\n          }}"
    )
    .expect("write");
    writeln!(
        source,
        "          let item = {{ ...each }} as {stem}Request;"
    )
    .expect("write");
    write_supplied(source, screen, "          ");
    for revision in &screen.revision_inputs {
        writeln!(
            source,
            "          if (readMember(item, {}) === undefined) {{",
            member_literal(revision)
        )
        .expect("write");
        write_revision(source, screen, revision, "            ");
        source.push_str("          }\n");
    }
    source.push_str("          items.push(item);\n        }\n");
    writeln!(
        source,
        "        props.onEach?.(\n          await callEach<{stem}Result>(\n            props.transport,\n            {{ route: {constant}_ROUTE, request: {fields}, result: {constant}_RESULT_FIELDS }},\n            items,\n          ),\n        );\n        return;\n      }}"
    )
    .expect("write");
}

/// The element type of one repeated group, read out of the request type.
///
/// A member can admit null or be absent, so each step drops those before it
/// reaches the next member, and the last step takes one element of the list.
fn element_type(stem: &str, ancestor: &[String]) -> String {
    let mut expression = format!("{stem}Request");
    for name in ancestor {
        expression = format!("NonNullable<{expression}>[{name:?}]");
    }
    format!("NonNullable<{expression}>[number]")
}

/// The repeated group itself, which carries its text and its bounds.
fn leaf_group<'a>(screen: &'a ScreenPlan<'_>, ancestor: &str) -> Option<&'a FieldIr> {
    fn find<'a>(fields: &'a [FieldIr], path: &str) -> Option<&'a FieldIr> {
        for field in fields {
            if field.path == path {
                return Some(field);
            }
            if let Some(found) = find(&field.children, path) {
                return Some(found);
            }
        }
        None
    }
    // `repeated_ancestor` drops the `[]`, and the contract keeps it: the
    // group node's own path is `value.line[]`.
    find(&screen.contract.input_fields, &format!("{ancestor}[]"))
}

/// The plan's answer for one input, or nothing when the operator types it.
fn populated<'a>(screen: &'a ScreenPlan<'_>, path: &str) -> Option<&'a PopulatedInput<'a>> {
    screen
        .population
        .iter()
        .find(|populated| populated.input == path)
}

/// The selector whose chosen row supplies one revision input, if any does.
fn chosen<'a>(screen: &'a ScreenPlan<'_>, revision: &str) -> Option<&'a PopulatedInput<'a>> {
    screen.population.iter().find(|populated| {
        populated
            .revision
            .is_some_and(|chosen| chosen.input == revision)
    })
}

/// The alias one module imports another model's binding under.
///
/// Two models can each declare a `list`, so a foreign binding always carries
/// the model in its alias and the import is never ambiguous.
fn foreign_alias(model: &str, name: &str) -> String {
    crate::client_ts::to_camel(&format!("{model}_{name}"))
}

/// The names of one selector's state, from the input it fills.
///
/// Two inputs can read the same list, so the state is named after the input,
/// never after the list: `value.maker_id` gives `valueMakerIdOptions` and
/// `readValueMakerIdOptions`.
fn selector_state(input: &str) -> (String, String) {
    let state = crate::client_ts::to_camel(&input.replace("[]", "").replace('.', "_"));
    let mut state_upper = state.clone();
    state_upper[..1].make_ascii_uppercase();
    (state, state_upper)
}

/// The state and the read that back one selector.
///
/// Each selector owns its options. Two inputs that name the same model read
/// that list twice, which is one request each and no shared cache to get
/// stale.
fn emit_selector_state(
    source: &mut String,
    populated: &PopulatedInput<'_>,
    runtime: &mut BTreeSet<&'static str>,
) {
    let alias = foreign_alias(populated.list_model, populated.list_name);
    let (state, state_upper) = selector_state(populated.input);
    let stem = crate::client_ts::type_stem(populated.list_model, populated.list_name);
    let rows = match populated.list_rows {
        Rows::List { key } => key,
        Rows::Single => "rows",
    };
    // A page states where the next one starts. Every other list answers once,
    // and its page has no cursor to follow.
    let next_cursor = if populated.cursor_input.is_some() {
        "outcome.value.nextCursor"
    } else {
        "null"
    };
    runtime.extend([
        "appendPage",
        "emptyPage",
        "firstPage",
        "hasNextPage",
        "type PageState",
    ]);
    if populated.narrowed_by.is_some()
        || populated.search_input.is_some()
        || populated.cursor_input.is_some()
    {
        runtime.insert("writeMember");
    }
    writeln!(
        source,
        "  const [{state}Options, set{state_upper}Options] = createSignal<PageState<{stem}Row>>(emptyPage<{stem}Row>());"
    )
    .expect("write");
    // A narrowed selector holds the value it narrows by, so the next page of
    // its list asks the same question the first page asked.
    if populated.narrowed_by.is_some() {
        writeln!(
            source,
            "  const [{state}Narrowed, set{state_upper}Narrowed] = createSignal<string | null>(null);"
        )
        .expect("write");
    }
    if populated.search_input.is_some() {
        writeln!(
            source,
            "  const [{state}Search, set{state_upper}Search] = createSignal(\"\");"
        )
        .expect("write");
    }
    // The chosen row's revision, which the command sends for that record.
    if let Some(revision) = populated.revision {
        writeln!(
            source,
            "  const [{state}Revision, set{state_upper}Revision] = createSignal<{stem}Row[{:?}] | null>(null);",
            crate::client_ts::to_camel(revision.field)
        )
        .expect("write");
    }

    // The request gains a member for each control the selector renders, so it
    // is rebound only where one exists.
    let binding = if populated.search_input.is_some() || populated.cursor_input.is_some() {
        "let"
    } else {
        "const"
    };
    writeln!(
        source,
        "  const read{state_upper}Options = async (cursor: string | null) => {{"
    )
    .expect("write");
    if let Some(narrowing) = populated.narrowed_by {
        // Until the operator chooses the record this list narrows by, the list
        // has no input to read, and the release refuses a call that states
        // none. The selector offers nothing instead of asking.
        writeln!(
            source,
            "    const narrowed = {state}Narrowed();\n    if (narrowed === null || narrowed === \"\") {{\n      set{state_upper}Options(emptyPage<{stem}Row>());\n      return;\n    }}"
        )
        .expect("write");
        let member = crate::client_ts::to_camel(
            narrowing
                .list_input
                .rsplit('.')
                .next()
                .unwrap_or(narrowing.list_input),
        );
        writeln!(
            source,
            "    {binding} request = {{ {member}: narrowed }} as {stem}Request;"
        )
        .expect("write");
    } else {
        writeln!(source, "    {binding} request = {{}} as {stem}Request;").expect("write");
    }
    // The search sends the declared filter, which takes a list of values and
    // matches each one in full.
    if let Some(path) = populated.search_input {
        writeln!(
            source,
            "    if ({state}Search() !== \"\") {{\n      request = writeMember(request, {}, [{state}Search()]) as {stem}Request;\n    }}",
            member_literal(path)
        )
        .expect("write");
    }
    if let Some(path) = populated.cursor_input {
        writeln!(
            source,
            "    if (cursor !== null) {{\n      request = writeMember(request, {}, cursor) as {stem}Request;\n    }}",
            member_literal(path)
        )
        .expect("write");
    }
    writeln!(
        source,
        "    const outcome = await {alias}(props.transport, [request]);\n    if (outcome.status !== \"completed\") {{\n      return;\n    }}\n    const rows = outcome.value.{rows} as {stem}Row[];\n    set{state_upper}Options(\n      cursor === null\n        ? firstPage(rows, {next_cursor})\n        : appendPage({state}Options(), rows, {next_cursor}),\n    );\n  }};"
    )
    .expect("write");

    // A held record the list did not return, such as one a row action filled
    // from off the first page, is read by itself. The read returns the key
    // and the display field, which are all the selector reads of a row.
    if let Some(read) = populated.read {
        let read_stem = crate::client_ts::type_stem(read.model, read.name);
        writeln!(
            source,
            "  const read{state_upper}Record = async (key: string): Promise<{stem}Row | null> => {{\n    const request = writeMember({{}}, {}, key) as {read_stem}Request;\n    const outcome = await {}(props.transport, [request]);\n    return outcome.status === \"completed\" ? ((outcome.value ?? null) as unknown as {stem}Row | null) : null;\n  }};",
            member_literal(read.key_input),
            foreign_alias(read.model, read.name)
        )
        .expect("write");
        runtime.insert("writeMember");
    }

    if let Some(narrowing) = populated.narrowed_by {
        let source_member = member_path(narrowing.input).join(".");
        writeln!(
            source,
            "  createEffect(() => {{\n    formValues();\n    set{state_upper}Narrowed((form.getFieldValue(`{source_member}`) as string | null) ?? null);\n    void read{state_upper}Options(null);\n  }});"
        )
        .expect("write");
    } else {
        writeln!(source, "  void read{state_upper}Options(null);").expect("write");
    }
    // A write can add or change a record the list offers, so the list reads
    // its first page again. The chosen row stays chosen, because the selector
    // keeps it by key, so its revision is still the one the operator chose.
    runtime.insert("afterWrites");
    writeln!(
        source,
        "  onCleanup(afterWrites(props.transport, () => void read{state_upper}Options(null)));"
    )
    .expect("write");
}

/// One selector: the options are the rows the list returned.
///
/// It renders `RecordSelect` from `@wamn/ui`. A list that declares a filter on
/// its display field also gets the search, and a list that serves pages also
/// gets the next page. The list's other filters stay on the list's own
/// screen: a selector asks one question, which is the text the operator
/// already knows.
fn emit_selector_control(
    source: &mut String,
    populated: &PopulatedInput<'_>,
    indent: usize,
    label: &str,
    marks: &str,
    ui: &mut BTreeSet<&'static str>,
) {
    ui.insert("RecordSelect");
    let pad = " ".repeat(indent);
    let (state, state_upper) = selector_state(populated.input);
    let key = crate::client_ts::to_camel(populated.key_field);
    let display = crate::client_ts::to_camel(populated.display_field);
    writeln!(
        source,
        "{pad}<RecordSelect\n{pad}  label={label:?}\n{pad}  options={{{state}Options().rows}}\n{pad}  optionValue={{(row) => String(row.{key})}}\n{pad}  optionLabel={{(row) => String(row.{display})}}\n{pad}  value={{field().state.value == null ? null : String(field().state.value)}}"
    )
    .expect("write");
    writeln!(
        source,
        "{pad}  onChange={{(value) => field().handleChange(value ?? \"\")}}"
    )
    .expect("write");
    // A selector that supplies a revision keeps the revision of the row that
    // carries the held key: the row the operator picked, a listed row that
    // carries a filled key, or the record read for a key off the list.
    if let Some(revision) = populated.revision {
        writeln!(
            source,
            "{pad}  onRow={{(row) => set{state_upper}Revision(row?.{} ?? null)}}",
            crate::client_ts::to_camel(revision.field)
        )
        .expect("write");
    }
    // The search asks the list again from its first page, because a cursor
    // names a position in the answer the old value produced.
    if populated.search_input.is_some() {
        writeln!(
            source,
            "{pad}  onSearch={{(text) => {{\n{pad}    set{state_upper}Search(text);\n{pad}    void read{state_upper}Options(null);\n{pad}  }}}}"
        )
        .expect("write");
    }
    if populated.cursor_input.is_some() {
        writeln!(
            source,
            "{pad}  hasNextPage={{hasNextPage({state}Options())}}\n{pad}  onNextPage={{() => void read{state_upper}Options({state}Options().cursor)}}"
        )
        .expect("write");
    }
    if populated.read.is_some() {
        writeln!(source, "{pad}  readRow={{read{state_upper}Record}}").expect("write");
    }
    writeln!(
        source,
        "{pad}  error={{{marks} ? (refusal()?.text ?? null) : null}}\n{pad}/>"
    )
    .expect("write");
}

/// One control, its label, and the refusal that names it.
///
/// The control states the path that the contract declares, and the runtime
/// decides whether the refused path reaches it. A control inside a repeated
/// group also states its own index, so a refusal that names one line marks
/// that line alone.
fn emit_field(
    source: &mut String,
    input: &FieldIr,
    name: &str,
    indent: usize,
    index: Option<&str>,
    populated: Option<&PopulatedInput<'_>>,
    ui: &mut BTreeSet<&'static str>,
) {
    let pad = " ".repeat(indent);
    let declared = input.path.as_str();
    let element = index.map_or_else(String::new, |index| format!(", {index}"));
    let marks = format!("refusalMarks(refusal()?.member ?? null, {declared:?}{element})");
    writeln!(source, "{pad}<form.Field name={{`{name}`}}>").expect("write");
    writeln!(source, "{pad}  {{(field) => (").expect("write");
    match populated {
        // An input that names a record is chosen from the list that offers
        // it, never typed. The options come from one read of that list.
        Some(populated) => {
            emit_selector_control(source, populated, indent + 4, &label(input), &marks, ui);
        }
        None => emit_input_control(source, input, indent + 4, &marks, ui),
    }
    writeln!(source, "{pad}  )}}").expect("write");
    writeln!(source, "{pad}</form.Field>").expect("write");
}

/// One repeated input group: a list of elements the operator adds and removes.
///
/// The contract marks the group with `[]` and states its bounds. Each element
/// carries the same members, and the field name takes the element's index.
fn emit_repeated_group(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    inputs: &[&FieldIr],
    ancestor: &str,
    stem: &str,
    ui: &mut BTreeSet<&'static str>,
) {
    ui.extend(["Button", "FieldGroup", "FieldLegend", "FieldSet"]);
    let member = member_path(ancestor).join(".");
    let element = element_type(stem, &member_path(ancestor));
    let members: Vec<&&FieldIr> = inputs
        .iter()
        .filter(|input| repeated_ancestor(&input.path) == Some(ancestor))
        .collect();
    // The group itself is one field of the contract, which carries its text
    // and its declared bounds.
    let group = leaf_group(screen, ancestor);
    let minimum = group.and_then(|group| group.minimum).unwrap_or(0);
    let maximum = group.and_then(|group| group.maximum);
    writeln!(
        source,
        "      <form.Field name={{\"{member}\"}} mode=\"array\">"
    )
    .expect("write");
    source.push_str("        {(group) => (\n          <FieldSet>\n");
    writeln!(
        source,
        "            <FieldLegend>{}</FieldLegend>",
        group.map_or_else(|| derived_label(ancestor), label)
    )
    .expect("write");
    source.push_str("            <For each={group().state.value ?? []}>\n              {(_, index) => (\n                <FieldGroup>\n");
    for input in &members {
        let leaf = input
            .path
            .rsplit('.')
            .next()
            .unwrap_or(&input.path)
            .trim_end_matches("[]");
        let name = format!(
            "{member}[${{index()}}].{}",
            crate::client_ts::to_camel(leaf)
        );
        emit_field(
            source,
            input,
            &name,
            18,
            Some("index()"),
            populated(screen, &input.path),
            ui,
        );
    }
    writeln!(
        source,
        "                  <Button\n                    type=\"button\"\n                    variant=\"outline\"\n                    size=\"sm\"\n                    disabled={{!canRemove(group().state.value ?? [], {minimum})}}\n                    onClick={{() => group().removeValue(index())}}\n                  >\n                    remove\n                  </Button>\n                </FieldGroup>\n              )}}\n            </For>"
    )
    .expect("write");
    let bound = maximum.map_or_else(|| "null".to_owned(), |maximum| maximum.to_string());
    writeln!(
        source,
        "            <Button\n              type=\"button\"\n              variant=\"outline\"\n              disabled={{!canAdd(group().state.value ?? [], {bound})}}\n              onClick={{() => group().pushValue({{}} as {element})}}\n            >\n              add\n            </Button>\n          </FieldSet>\n        )}}\n      </form.Field>"
    )
    .expect("write");
}

/// One control for one operator field, chosen by what the contract declares.
///
/// Each one is a field from `@wamn/ui`, which owns the label, the control, the
/// refusal mark and how they look. `marks` is the expression that is true when
/// the last refusal names this control.
fn emit_input_control(
    source: &mut String,
    input: &FieldIr,
    indent: usize,
    marks: &str,
    ui: &mut BTreeSet<&'static str>,
) {
    let pad = " ".repeat(indent);
    let text = label(input);
    let error = format!("error={{{marks} ? (refusal()?.text ?? null) : null}}");
    if input.type_name == "boolean" {
        ui.insert("CheckField");
        writeln!(
            source,
            "{pad}<CheckField\n{pad}  label={text:?}\n{pad}  checked={{field().state.value === true}}\n{pad}  onChange={{(checked) => field().handleChange(checked)}}\n{pad}  {error}\n{pad}/>"
        )
        .expect("write");
        return;
    }
    if !input.values.is_empty() {
        ui.insert("ChoiceField");
        writeln!(
            source,
            "{pad}<ChoiceField\n{pad}  label={text:?}\n{pad}  allowEmpty={{{}}}\n{pad}  choices={{[",
            !input.required || input.nullable
        )
        .expect("write");
        for value in &input.values {
            writeln!(
                source,
                "{pad}    {{ value: {value:?}, text: {:?} }},",
                value.replace('_', " ")
            )
            .expect("write");
        }
        // The control hands back text. The field's type is the union of the
        // declared values, and the control offers only those values.
        let union = input
            .values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(
            source,
            "{pad}  ]}}\n{pad}  value={{String(field().state.value ?? \"\")}}\n{pad}  onChange={{(value) => field().handleChange(value as {union})}}\n{pad}  {error}\n{pad}/>"
        )
        .expect("write");
        return;
    }
    let kind = if matches!(input.type_name.as_str(), "int32" | "float64") {
        "number"
    } else {
        "text"
    };
    ui.insert("TextField");
    writeln!(
        source,
        "{pad}<TextField\n{pad}  label={text:?}\n{pad}  type=\"{kind}\"\n{pad}  value={{String(field().state.value ?? \"\")}}\n{pad}  onInput={{(value) => field().handleChange(value)}}\n{pad}  {error}\n{pad}/>"
    )
    .expect("write");
}

/// One delete screen: a removal that the operator confirms first.
fn emit_delete(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    ui: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "newRequestId",
        "refusalSentence",
        "refusedMember",
        "writeMember",
        "type Outcome",
        "type Transport",
    ]);
    ui.extend(["ConfirmAction", "FieldError", "announceOutcome"]);
    for supplied in &screen.supplied {
        match supplied.kind {
            SuppliedKind::RequestId => runtime.insert("newRequestId"),
            SuppliedKind::IdempotencyKey => runtime.insert("newIdempotencyKey"),
            SuppliedKind::OccurredAt => runtime.insert("occurredAt"),
        };
    }
    bindings.insert(function.clone());
    bindings.insert(format!("type {stem}Request"));
    bindings.insert(format!("type {stem}Result"));

    // A delete that binds no read states the record itself, so it carries its
    // own input type.
    if screen.revision.is_none() {
        writeln!(
            source,
            "\n/** The record that the delete for `{}` removes. */",
            screen.contract.operation
        )
        .expect("write");
        writeln!(source, "export interface {stem}DeleteInput {{").expect("write");
        alias_imports(&operator_inputs(screen), false, runtime);
        write_record_members(source, &operator_inputs(screen), &[], 1);
        source.push_str("}\n");
    }
    writeln!(
        source,
        "\n/** What the delete for `{}` takes. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}DeleteProps {{").expect("write");
    source.push_str("  /** The transport the application supplies. */\n");
    source.push_str("  readonly transport: Transport;\n");
    if let Some(binding) = screen.revision {
        let read = crate::client_ts::operation_stem(binding.read_operation);
        bindings.insert(format!("type {read}Result"));
        let mut fields = [
            member_path(binding.key_field)[0].clone(),
            member_path(binding.revision_field)[0].clone(),
        ];
        fields.sort();
        writeln!(
            source,
            "  /** The record to remove, as the page displayed it. The component sends\n   * its key and its revision and reads nothing, so a change another writer\n   * made after the page read it refuses as a conflict. `{}` states the fields. */",
            binding.read_operation
        )
        .expect("write");
        writeln!(
            source,
            "  readonly record: Pick<{read}Result, {:?} | {:?}>;",
            fields[0], fields[1]
        )
        .expect("write");
    } else {
        writeln!(
            source,
            "  /** The input that names the record to remove. */\n  readonly input: {stem}DeleteInput;"
        )
        .expect("write");
    }
    source.push_str("  /** Called with the outcome of the removal. */\n");
    writeln!(
        source,
        "  readonly onSubmitted?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
    source.push_str("}\n");

    writeln!(
        source,
        "\n/** What an operator calls this screen. The page decides where it goes. */\nexport const {stem}DeleteLabel = {:?};",
        screen_label(screen)
    )
    .expect("write");
    writeln!(
        source,
        "\n/**\n * The delete for `{}`.\n *\n * It asks for a confirmation first, because a removal is not an edit that an\n * operator undoes.\n */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "export function {stem}Delete(props: {stem}DeleteProps) {{"
    )
    .expect("write");
    source.push_str(
        "  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);\n",
    );
    source.push_str("\n  const remove = async () => {\n");
    if let Some(binding) = screen.revision {
        runtime.insert("readMember");
        writeln!(source, "    let item = {{}} as {stem}Request;").expect("write");
        writeln!(
            source,
            "    item = writeMember(item, {}, readMember(props.record, {}) ?? null);",
            member_literal(binding.command_key_input),
            member_literal(binding.key_field)
        )
        .expect("write");
        writeln!(
            source,
            "    item = writeMember(item, {}, readMember(props.record, {}) ?? null);",
            member_literal(binding.command_revision_input),
            member_literal(binding.revision_field)
        )
        .expect("write");
    } else {
        writeln!(
            source,
            "    let item = {{ ...props.input }} as {stem}Request;"
        )
        .expect("write");
    }
    for supplied in &screen.supplied {
        let value = match supplied.kind {
            SuppliedKind::RequestId => "newRequestId()",
            SuppliedKind::IdempotencyKey => "newIdempotencyKey()",
            SuppliedKind::OccurredAt => "occurredAt()",
        };
        writeln!(
            source,
            "    item = writeMember(item, {}, {value});",
            member_literal(supplied.path)
        )
        .expect("write");
    }
    writeln!(
        source,
        "    const outcome = await {function}(props.transport, [item]);"
    )
    .expect("write");
    source.push_str("    props.onSubmitted?.(outcome);\n");
    writeln!(source, "    announceOutcome(outcome, {stem}DeleteLabel);").expect("write");
    source.push_str(
        "    setRefusal(\n      outcome.status === \"refused\"\n        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }\n        : null,\n    );\n  };\n",
    );
    source.push_str(
        "\n  return (\n    <section>\n      <Show when={refusal()}>\n        <FieldError>{refusal()?.text}</FieldError>\n      </Show>\n      <ConfirmAction\n        trigger=\"delete\"\n        question=\"remove this record?\"\n        confirm=\"confirm\"\n        cancel=\"cancel\"\n        onConfirm={() => void remove()}\n      />\n    </section>\n  );\n}\n",
    );
    Ok(())
}

/// The local name of one operation, from its canonical identity.
fn local_name(identity: &str) -> &str {
    let without_version = identity.split('@').next().unwrap_or(identity);
    without_version
        .rsplit('/')
        .next()
        .unwrap_or(without_version)
}

/// Moves every line of `block` one level deeper, for the group that wraps it.
fn deepen(block: &str) -> String {
    block
        .lines()
        .map(|line| {
            if line.is_empty() {
                "\n".to_owned()
            } else {
                format!("  {line}\n")
            }
        })
        .collect()
}

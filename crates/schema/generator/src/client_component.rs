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

use crate::client_ir::{FieldIr, leaf_fields};
use crate::client_plan::{
    ClientPlan, ModelPlan, PopulatedInput, Role, Rows, ScreenPlan, SuppliedKind,
};
use crate::client_ts::{RUNTIME_PACKAGE, ts_type};
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
}

impl ClientComponentErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnwrittenRole => "unwritten_role",
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
        "// Components of package `{}`, one for each operation the plan gives a role.",
        plan.package
    )
    .expect("writing to a String cannot fail");
    index.push('\n');
    for model in &plan.models {
        let source = emit_model(model)?;
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
    files.push(GeneratedFile::new(
        format!("{COMPONENT_DIRECTORY}/index.ts").into_boxed_str(),
        index.into_bytes().into_boxed_slice(),
    ));
    Ok(files)
}

/// The screens this emitter writes today.
fn written(screen: &ScreenPlan<'_>) -> bool {
    screen.role.is_supported()
}

fn emit_model(model: &ModelPlan<'_>) -> Result<String, ClientComponentError> {
    let screens: Vec<&ScreenPlan<'_>> = model
        .screens
        .iter()
        .filter(|screen| screen.role.is_supported() && written(screen))
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
    let mut solid = BTreeSet::new();
    let mut table = false;
    let mut form = false;
    for screen in &screens {
        match screen.role {
            Role::Table => {
                table = true;
                solid.extend(["createSignal", "For", "Show"]);
                emit_table(&mut body, screen, &mut runtime, &mut bindings)?;
            }
            Role::Detail => {
                solid.extend(["createResource", "Show"]);
                emit_detail(&mut body, screen, &mut runtime, &mut bindings)?;
            }
            Role::Form => {
                form = true;
                solid.extend(["createSignal", "Show"]);
                if !screen.population.is_empty()
                    || screen
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
                }
                emit_form(
                    &mut body,
                    screen,
                    &mut runtime,
                    &mut bindings,
                    &mut foreign,
                    &records,
                )?;
            }
            Role::Delete => {
                solid.extend(["createSignal", "Show"]);
                emit_delete(&mut body, screen, &mut runtime, &mut bindings, &records)?;
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

    let mut source = String::from("// @generated from the client-contract IR; do not edit.\n//\n");
    writeln!(
        source,
        "// `{}` components. Each one calls the bindings and the runtime, and\n// nothing else.",
        model.model.name
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "\nimport {{ {} }} from \"solid-js\";",
        solid.iter().copied().collect::<Vec<_>>().join(", ")
    )
    .expect("writing to a String cannot fail");
    if table {
        source.push_str(
            "import {\n  createSolidTable,\n  flexRender,\n  getCoreRowModel,\n  type ColumnDef,\n} from \"@tanstack/solid-table\";\n",
        );
    }
    if form {
        source.push_str("import { createForm } from \"@tanstack/solid-form\";\n");
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
    source.push_str(&body);
    Ok(source)
}

/// The TypeScript member path of one contract path, as a literal list.
fn member_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(|part| crate::client_ts::to_camel(part.trim_end_matches("[]")))
        .collect()
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

/// The member path as an accessor, for a table column.
fn accessor(path: &str) -> String {
    member_path(path).join(".")
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

/// The label of one page control, which names an input the plan reserves.
fn control_label(screen: &ScreenPlan<'_>, path: &str) -> String {
    leaf_fields(&screen.contract.input_fields)
        .into_iter()
        .find(|field| field.path == path)
        .map_or_else(|| derived_label(path), label)
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

fn emit_table(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    // A bounded list states no paging at all, so it renders no control and
    // asks for no next page.
    let paging = screen.paging.as_ref();
    let controls = paging.map_or(0, |paging| {
        paging.filter_inputs.len()
            + usize::from(paging.sort_field_input.is_some())
            + usize::from(paging.sort_direction_input.is_some())
            + usize::from(paging.limit_input.is_some())
    });
    let cursor_input = paging.and_then(|paging| paging.cursor_input);
    runtime.extend([
        "appendPage",
        "cellText",
        "emptyPage",
        "firstPage",
        "hasNextPage",
        "newRequestId",
        "startRead",
        "type JsonValue",
        "type Outcome",
        "type PageState",
        "type Transport",
    ]);
    if controls > 0 || cursor_input.is_some() {
        runtime.insert("writeMember");
    }
    bindings.insert(function.clone());
    bindings.insert(format!("type {stem}Request"));
    bindings.insert(format!("type {stem}Result"));
    bindings.insert(format!("type {stem}Row"));

    let rows_key = match screen.rows {
        crate::client_plan::Rows::List { key } => key,
        crate::client_plan::Rows::Single => {
            return Err(ClientComponentError::new(
                ClientComponentErrorKind::UnwrittenRole,
                format!(
                    "{} states the table role and one row",
                    screen.contract.operation
                ),
            ));
        }
    };

    // The columns, in contract order.
    writeln!(
        source,
        "\n/** Columns of `{}`, in contract order. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "const {}_COLUMNS: ColumnDef<{stem}Row, unknown>[] = [",
        screen.name.to_uppercase()
    )
    .expect("write");
    for column in &screen.columns {
        writeln!(source, "  {{").expect("write");
        writeln!(source, "    accessorKey: {:?},", accessor(&column.path)).expect("write");
        writeln!(source, "    header: {:?},", label(column)).expect("write");
        writeln!(
            source,
            "    cell: (cell) => cellText(cell.getValue() as JsonValue, {:?}),",
            cell_type(column)
        )
        .expect("write");
        writeln!(source, "  }},").expect("write");
    }
    source.push_str("];\n");

    // The props.
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
    source.push_str("  /** Called when the operator picks one row. */\n");
    writeln!(source, "  readonly onRowSelect?: (row: {stem}Row) => void;").expect("write");
    for link in &screen.row_links {
        let target = crate::client_ts::operation_stem(link.operation);
        writeln!(
            source,
            "  /** Called when the operator opens `{}` from one row. */",
            link.operation
        )
        .expect("write");
        writeln!(
            source,
            "  readonly onOpen{target}?: (row: {stem}Row) => void;"
        )
        .expect("write");
    }
    source.push_str("  /** Called with every outcome this screen reads. */\n");
    writeln!(
        source,
        "  readonly onOutcome?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
    source.push_str("}\n");

    // The component.
    writeln!(
        source,
        "\n/** What an operator calls this screen. The page decides where it goes. */\nexport const {stem}TableLabel = {:?};",
        screen_label(screen)
    )
    .expect("write");
    writeln!(
        source,
        "\n/**\n * The table for `{}`.\n *\n * It owns its page controls and its rows. A change to a control clears the\n * rows, because a cursor names a position in the list the old input produced.\n *\n * It reads when the operator asks, and not when it mounts, because a read is\n * a request that the operator did not send yet.\n */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(
        source,
        "export function {stem}Table(props: {stem}TableProps) {{"
    )
    .expect("write");
    if controls > 0 {
        writeln!(
            source,
            "  const [controls, setControls] = createSignal<Partial<{stem}Request>>({{}});"
        )
        .expect("write");
    } else {
        writeln!(
            source,
            "  const controls = (): Partial<{stem}Request> => ({{}});"
        )
        .expect("write");
    }
    writeln!(
        source,
        "  const [page, setPage] = createSignal<PageState<{stem}Row>>(emptyPage<{stem}Row>());"
    )
    .expect("write");
    source.push_str("\n  const read = async (cursor: string | null) => {\n");
    source.push_str("    setPage(startRead(page()));\n");
    writeln!(
        source,
        "    const request = {{\n      ...controls(),\n      ...props.fixed,\n      requestId: newRequestId(),\n    }} as {stem}Request;"
    )
    .expect("write");
    if let Some(path) = cursor_input {
        writeln!(
            source,
            "    const sent = cursor === null ? request : (writeMember(request, {}, cursor) as {stem}Request);",
            member_literal(path)
        )
        .expect("write");
    } else {
        source.push_str("    const sent = request;\n");
    }
    writeln!(
        source,
        "    const outcome = await {function}(props.transport, [sent]);"
    )
    .expect("write");
    source.push_str("    props.onOutcome?.(outcome);\n");
    source.push_str("    if (outcome.status !== \"completed\") {\n");
    source.push_str("      setPage({ ...page(), busy: false });\n      return;\n    }\n");
    let cursor_of = if rows_key == "item" {
        "outcome.value.nextCursor"
    } else {
        "null"
    };
    writeln!(
        source,
        "    const rows = outcome.value.{};",
        crate::client_ts::to_camel(rows_key)
    )
    .expect("write");
    writeln!(
        source,
        "    setPage(cursor === null ? firstPage(rows, {cursor_of}) : appendPage(page(), rows, {cursor_of}));"
    )
    .expect("write");
    source.push_str("  };\n");
    source.push_str("\n  const restart = () => {\n");
    writeln!(source, "    setPage(emptyPage<{stem}Row>());").expect("write");
    source.push_str("    void read(null);\n  };\n");
    if controls > 0 {
        source.push_str("\n  const change = (path: readonly string[], value: JsonValue) => {\n");
        source.push_str("    setControls((current) => writeMember(current, path, value));\n");
        source.push_str("    restart();\n  };\n");
    }
    source.push_str("\n  const table = createSolidTable({\n");
    source.push_str("    get data() {\n      return page().rows as ");
    writeln!(source, "{stem}Row[];\n    }},").expect("write");
    writeln!(
        source,
        "    columns: {}_COLUMNS,",
        screen.name.to_uppercase()
    )
    .expect("write");
    source.push_str("    getCoreRowModel: getCoreRowModel(),\n  });\n");

    // The markup.
    source.push_str("\n  return (\n    <section>\n      <form\n        onSubmit={(event) => {\n          event.preventDefault();\n          restart();\n        }}\n      >\n");
    emit_controls(source, screen);
    source.push_str("        <button type=\"submit\">read</button>\n      </form>\n");
    source.push_str("      <table>\n        <thead>\n          <For each={table.getHeaderGroups()}>\n            {(group) => (\n              <tr>\n                <For each={group.headers}>\n                  {(header) => (\n                    <th>{flexRender(header.column.columnDef.header, header.getContext())}</th>\n                  )}\n                </For>\n              </tr>\n            )}\n          </For>\n        </thead>\n        <tbody>\n          <For each={table.getRowModel().rows}>\n            {(row) => (\n              <tr onClick={() => props.onRowSelect?.(row.original)}>\n                <For each={row.getVisibleCells()}>\n                  {(cell) => <td>{flexRender(cell.column.columnDef.cell, cell.getContext())}</td>}\n                </For>\n");
    for link in &screen.row_links {
        let target = crate::client_ts::operation_stem(link.operation);
        writeln!(
            source,
            "                <td>\n                  <Show when={{props.onOpen{target}}}>\n                    <button type=\"button\" onClick={{() => props.onOpen{target}?.(row.original)}}>\n                      {}\n                    </button>\n                  </Show>\n                </td>",
            link_label(link.operation)
        )
        .expect("write");
    }
    source.push_str(
        "              </tr>\n            )}\n          </For>\n        </tbody>\n      </table>\n",
    );
    source.push_str("      <Show when={hasNextPage(page())}>\n        <button type=\"button\" onClick={() => void read(page().cursor)}>\n          next page\n        </button>\n      </Show>\n    </section>\n  );\n}\n");
    Ok(())
}

/// One detail screen: the fields of one record that the release reads.
fn emit_detail(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "cellText",
        "newRequestId",
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
    alias_imports(&operator_inputs(screen), runtime);
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
        "  const [outcome] = createResource(\n    () => props.input,\n    async (input: {stem}DetailInput) => {{\n      const read = await {function}(props.transport, [\n        {{ ...input, requestId: newRequestId() }} as {stem}Request,\n      ]);\n      props.onOutcome?.(read);\n      return read;\n    }},\n  );"
    )
    .expect("write");
    writeln!(
        source,
        "  const record = (): {stem}Result | undefined => {{\n    const read = outcome();\n    return read?.status === \"completed\" ? read.value : undefined;\n  }};"
    )
    .expect("write");
    source.push_str("  const state = () => outcome()?.status;\n");
    source.push_str("\n  return (\n    <section>\n      <Show when={state() !== undefined && state() !== \"completed\"}>\n        <p>{state()}</p>\n      </Show>\n      <dl>\n");
    for column in &screen.columns {
        writeln!(source, "        <dt>{}</dt>", label(column)).expect("write");
        writeln!(
            source,
            "        <dd>{{cellText(readMember(record(), {}), {:?})}}</dd>",
            member_literal(&column.path),
            cell_type(column)
        )
        .expect("write");
    }
    source.push_str("      </dl>\n    </section>\n  );\n}\n");
    Ok(())
}

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
fn write_input_schema(source: &mut String, inputs: &[&FieldIr], prefix: &[String], depth: usize) {
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
        let repeated = inputs.iter().any(|input| {
            repeated_ancestor(&input.path).is_some_and(|ancestor| member_path(ancestor) == deeper)
        });
        writeln!(source, "{indent}{name}: z").expect("write");
        writeln!(source, "{indent}  .object({{").expect("write");
        write_input_schema(source, inputs, &deeper, depth + 2);
        writeln!(source, "{indent}  }})").expect("write");
        if repeated {
            writeln!(source, "{indent}  .array()").expect("write");
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
fn alias_imports(inputs: &[&FieldIr], runtime: &mut BTreeSet<&'static str>) {
    for input in inputs
        .iter()
        .filter(|input| repeated_ancestor(&input.path).is_none())
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
/// caller can prefill one member of a nested value. A repeated group is not a
/// member: the form owns the list that the operator adds to.
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
        if repeated {
            continue;
        }
        written.push(name.clone());
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
            writeln!(source, "{indent}{name}?: {spelling}{nullable};").expect("write");
            continue;
        }
        writeln!(source, "{indent}{name}?: {{").expect("write");
        write_initial_members(source, inputs, &deeper, depth + 1);
        writeln!(source, "{indent}}};").expect("write");
    }
}

/// One form screen: what the operator types, and what the platform supplies.
fn emit_form(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
    foreign: &mut BTreeMap<String, BTreeSet<String>>,
    records: &BTreeMap<String, String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "newRequestId",
        "refusalMarks",
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
    write_input_schema(source, &operator_inputs(screen), &[], 1);
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
    alias_imports(&operator_inputs(screen), runtime);
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
            "  /** The record this command changes. The form reads it, and sends the\n   * revision it read, because `{}` states that binding. */",
            binding.read_operation
        )
        .expect("write");
        writeln!(source, "  readonly key: {key};").expect("write");
    } else {
        for revision in &screen.revision_inputs {
            source.push_str(
                "  /** The revision this command sends. The release binds no read that supplies it. */\n",
            );
            writeln!(
                source,
                "  readonly {}: {stem}Request[{:?}];",
                crate::client_ts::to_camel(revision),
                crate::client_ts::to_camel(revision)
            )
            .expect("write");
        }
    }
    source.push_str("  /** Called with the outcome of every submission. */\n");
    writeln!(
        source,
        "  readonly onSubmitted?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
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
        "  const [refusal, setRefusal] = createSignal<{ code: string | null; member: string | null } | null>(\n    null,\n  );\n",
    );
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
    writeln!(
        source,
        "      const checked = {}_INPUT.safeParse(value);",
        screen.name.to_uppercase()
    )
    .expect("write");
    source.push_str(
        "      if (!checked.success) {\n        const issue = checked.error.issues[0];\n        setRefusal({\n          code: issue?.message ?? \"the input is not valid\",\n          member: typeof issue?.path.at(-1) === \"string\" ? String(issue.path.at(-1)) : null,\n        });\n        return;\n      }\n",
    );
    writeln!(source, "      let item = {{ ...value }} as {stem}Request;").expect("write");
    for supplied in &screen.supplied {
        let value = match supplied.kind {
            SuppliedKind::RequestId => "newRequestId()",
            SuppliedKind::IdempotencyKey => "newIdempotencyKey()",
            SuppliedKind::OccurredAt => "occurredAt()",
        };
        writeln!(
            source,
            "      item = writeMember(item, {}, {value});",
            member_literal(supplied.path)
        )
        .expect("write");
    }
    if let Some(binding) = screen.revision {
        let read = crate::client_ts::function_name(local_name(binding.read_operation)).map_err(
            |error| {
                ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    error.to_string(),
                )
            },
        )?;
        bindings.insert(read.clone());
        bindings.insert(format!(
            "type {}Request",
            crate::client_ts::operation_stem(binding.read_operation)
        ));
        runtime.insert("readMember");
        source.push_str("      // The revision comes from the record this command changes, read\n      // now, because a stale revision is what the conflict outcome names.\n");
        writeln!(
            source,
            "      const record = await {read}(props.transport, [\n        {{ ...props.key, requestId: newRequestId() }},\n      ]);"
        )
        .expect("write");
        source.push_str(
            "      if (record.status !== \"completed\") {\n        setRefusal({ code: record.status, member: null });\n        return;\n      }\n",
        );
        writeln!(
            source,
            "      item = writeMember(item, {}, readMember(record.value, {}) ?? null);",
            member_literal(binding.command_key_input),
            member_literal(binding.key_field)
        )
        .expect("write");
        writeln!(
            source,
            "      item = writeMember(item, {}, readMember(record.value, {}) ?? null);",
            member_literal(binding.command_revision_input),
            member_literal(binding.revision_field)
        )
        .expect("write");
    } else {
        for revision in &screen.revision_inputs {
            writeln!(
                source,
                "      item = writeMember(item, {}, props.{});",
                member_literal(revision),
                crate::client_ts::to_camel(revision)
            )
            .expect("write");
        }
    }
    writeln!(
        source,
        "      const outcome = await {function}(props.transport, [item]);"
    )
    .expect("write");
    source.push_str("      props.onSubmitted?.(outcome);\n");
    source.push_str(
        "      setRefusal(\n        outcome.status === \"refused\"\n          ? { code: outcome.code, member: refusedMember(outcome.detail) }\n          : null,\n      );\n",
    );
    source.push_str("    },\n  }));\n");

    // One selector state for each input the operator chooses from a list.
    for populated in &screen.population {
        runtime.insert("newRequestId");
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
        emit_selector_state(source, populated);
    }

    // The markup.
    source.push_str(
        "\n  return (\n    <form\n      onSubmit={(event) => {\n        event.preventDefault();\n        void form.handleSubmit();\n      }}\n    >\n      <Show when={refusal()?.member === null ? refusal() : undefined}>\n        <p>{refusal()?.code}</p>\n      </Show>\n",
    );
    let inputs = operator_inputs(screen);
    let mut repeated_written: Vec<String> = Vec::new();
    for input in &inputs {
        match repeated_ancestor(&input.path) {
            None => emit_field(
                source,
                input,
                &member_path(&input.path).join("."),
                6,
                None,
                populated(screen, &input.path),
            ),
            Some(ancestor) => {
                let ancestor = ancestor.to_owned();
                if repeated_written.contains(&ancestor) {
                    continue;
                }
                repeated_written.push(ancestor.clone());
                emit_repeated_group(source, screen, &inputs, &ancestor, &stem);
            }
        }
    }
    source.push_str("      <button type=\"submit\">submit</button>\n    </form>\n  );\n}\n");
    Ok(())
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

/// The plan's answer for one input, or nothing when the operator types it.
fn populated<'a>(screen: &'a ScreenPlan<'_>, path: &str) -> Option<&'a PopulatedInput<'a>> {
    screen
        .population
        .iter()
        .find(|populated| populated.input == path)
}

/// The alias one module imports another model's binding under.
///
/// Two models can each declare a `list`, so a foreign binding always carries
/// the model in its alias and the import is never ambiguous.
fn foreign_alias(model: &str, name: &str) -> String {
    crate::client_ts::to_camel(&format!("{model}_{name}"))
}

/// The state and the read that back one selector.
///
/// Each selector owns its options. Two inputs that name the same model read
/// that list twice, which is one request each and no shared cache to get
/// stale.
fn emit_selector_state(source: &mut String, populated: &PopulatedInput<'_>) {
    let alias = foreign_alias(populated.list_model, populated.list_name);
    let stem = crate::client_ts::type_stem(populated.list_model, populated.list_name);
    let rows = match populated.list_rows {
        Rows::List { key } => key,
        Rows::Single => "rows",
    };
    writeln!(
        source,
        "  const [{alias}Options, set{stem}Options] = createSignal<{stem}Row[]>([]);"
    )
    .expect("write");
    // A narrowed selector states the value it narrows by, so the list returns
    // the rows of the record the operator already chose. Every other selector
    // reads its list once.
    if let Some(narrowing) = populated.narrowed_by {
        {
            let member = crate::client_ts::to_camel(
                narrowing
                    .list_input
                    .rsplit('.')
                    .next()
                    .unwrap_or(narrowing.list_input),
            );
            let source_member = member_path(narrowing.input).join(".");
            writeln!(
                source,
                "  const read{stem}Options = async (narrowed: string | null) => {{\n    const outcome = await {alias}(props.transport, [\n      {{ requestId: newRequestId(), {member}: narrowed }} as {stem}Request,\n    ]);\n    if (outcome.status === \"completed\") {{\n      set{stem}Options(outcome.value.{rows} as {stem}Row[]);\n    }}\n  }};"
            )
            .expect("write");
            writeln!(
                source,
                "  createEffect(() => {{\n    const narrowed = form.getFieldValue(`{source_member}`) as string | null;\n    void read{stem}Options(narrowed ?? null);\n  }});"
            )
            .expect("write");
        }
    } else {
        writeln!(
            source,
            "  const read{stem}Options = async () => {{\n    const outcome = await {alias}(props.transport, [\n      {{ requestId: newRequestId() }} as {stem}Request,\n    ]);\n    if (outcome.status === \"completed\") {{\n      set{stem}Options(outcome.value.{rows} as {stem}Row[]);\n    }}\n  }};"
        )
        .expect("write");
        writeln!(source, "  void read{stem}Options();").expect("write");
    }
}

/// One selector: the options are the rows the list returned.
fn emit_selector_control(source: &mut String, populated: &PopulatedInput<'_>, indent: usize) {
    let pad = " ".repeat(indent);
    let alias = foreign_alias(populated.list_model, populated.list_name);
    let key = crate::client_ts::to_camel(populated.key_field);
    let display = crate::client_ts::to_camel(populated.display_field);
    writeln!(
        source,
        "{pad}<select\n{pad}  value={{String(field().state.value ?? \"\")}}\n{pad}  onChange={{(event) => field().handleChange(event.currentTarget.value)}}\n{pad}>"
    )
    .expect("write");
    writeln!(source, "{pad}  <option value=\"\"></option>").expect("write");
    writeln!(
        source,
        "{pad}  <For each={{{alias}Options()}}>\n{pad}    {{(row) => (\n{pad}      <option value={{String(row.{key})}}>{{String(row.{display})}}</option>\n{pad}    )}}\n{pad}  </For>"
    )
    .expect("write");
    writeln!(source, "{pad}</select>").expect("write");
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
) {
    let pad = " ".repeat(indent);
    let declared = input.path.as_str();
    let element = index.map_or_else(String::new, |index| format!(", {index}"));
    writeln!(source, "{pad}<form.Field name={{`{name}`}}>").expect("write");
    writeln!(source, "{pad}  {{(field) => (").expect("write");
    writeln!(source, "{pad}    <label>").expect("write");
    writeln!(source, "{pad}      {}", label(input)).expect("write");
    match populated {
        // An input that names a record is chosen from the list that offers
        // it, never typed. The options come from one read of that list.
        Some(populated) => emit_selector_control(source, populated, indent + 6),
        None => emit_input_control(source, input, indent + 6),
    }
    writeln!(
        source,
        "{pad}      <Show when={{refusalMarks(refusal()?.member ?? null, {declared:?}{element})}}>\n{pad}        <em>{{refusal()?.code}}</em>\n{pad}      </Show>"
    )
    .expect("write");
    writeln!(source, "{pad}    </label>").expect("write");
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
) {
    let member = member_path(ancestor).join(".");
    let element = element_type(stem, &member_path(ancestor));
    let members: Vec<&&FieldIr> = inputs
        .iter()
        .filter(|input| repeated_ancestor(&input.path) == Some(ancestor))
        .collect();
    writeln!(
        source,
        "      <form.Field name={{\"{member}\"}} mode=\"array\">"
    )
    .expect("write");
    source.push_str("        {(group) => (\n          <fieldset>\n");
    // A repeated group is synthesized from the leaf paths, so no author
    // declares it and its legend stays derived. `wamn-j3yr` holds that gap.
    writeln!(
        source,
        "            <legend>{}</legend>",
        derived_label(ancestor)
    )
    .expect("write");
    source.push_str("            <For each={group().state.value ?? []}>\n              {(_, index) => (\n                <fieldset>\n");
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
        );
    }
    source.push_str("                  <button type=\"button\" onClick={() => group().removeValue(index())}>\n                    remove\n                  </button>\n                </fieldset>\n              )}\n            </For>\n");
    writeln!(
        source,
        "            <button type=\"button\" onClick={{() => group().pushValue({{}} as {element})}}>\n              add\n            </button>\n          </fieldset>\n        )}}\n      </form.Field>"
    )
    .expect("write");
}

/// One control for one operator field, chosen by what the contract declares.
fn emit_input_control(source: &mut String, input: &FieldIr, indent: usize) {
    let pad = " ".repeat(indent);
    if input.type_name == "boolean" {
        writeln!(
            source,
            "{pad}<input\n{pad}  type=\"checkbox\"\n{pad}  checked={{field().state.value === true}}\n{pad}  onChange={{(event) => field().handleChange(event.currentTarget.checked)}}\n{pad}/>"
        )
        .expect("write");
        return;
    }
    if !input.values.is_empty() {
        writeln!(
            source,
            "{pad}<select\n{pad}  value={{String(field().state.value ?? \"\")}}\n{pad}  onChange={{(event) => field().handleChange(event.currentTarget.value)}}\n{pad}>"
        )
        .expect("write");
        if !input.required || input.nullable {
            writeln!(source, "{pad}  <option value=\"\"></option>").expect("write");
        }
        for value in &input.values {
            writeln!(
                source,
                "{pad}  <option value={value:?}>{}</option>",
                value.replace('_', " ")
            )
            .expect("write");
        }
        writeln!(source, "{pad}</select>").expect("write");
        return;
    }
    let kind = if matches!(input.type_name.as_str(), "int32" | "float64") {
        "number"
    } else {
        "text"
    };
    writeln!(
        source,
        "{pad}<input\n{pad}  type=\"{kind}\"\n{pad}  value={{String(field().state.value ?? \"\")}}\n{pad}  onInput={{(event) => field().handleChange(event.currentTarget.value)}}\n{pad}/>"
    )
    .expect("write");
}

/// One delete screen: a removal that the operator confirms first.
fn emit_delete(
    source: &mut String,
    screen: &ScreenPlan<'_>,
    runtime: &mut BTreeSet<&'static str>,
    bindings: &mut BTreeSet<String>,
    records: &BTreeMap<String, String>,
) -> Result<(), ClientComponentError> {
    let stem = crate::client_ts::type_stem(screen.model, screen.name);
    let function = crate::client_ts::function_name(screen.name).map_err(|error| {
        ClientComponentError::new(ClientComponentErrorKind::UnwrittenRole, error.to_string())
    })?;
    runtime.extend([
        "newRequestId",
        "refusedMember",
        "writeMember",
        "type Outcome",
        "type Transport",
    ]);
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
        alias_imports(&operator_inputs(screen), runtime);
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
        let key = records
            .get(binding.read_operation)
            .cloned()
            .unwrap_or_else(|| format!("{read}Request"));
        bindings.insert(format!("type {read}Request"));
        writeln!(
            source,
            "  /** The record to remove. The component reads it, and sends the\n   * revision it read, because `{}` states that binding. */",
            binding.read_operation
        )
        .expect("write");
        writeln!(source, "  readonly key: {key};").expect("write");
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
    source.push_str("  const [confirming, setConfirming] = createSignal(false);\n");
    source.push_str(
        "  const [refusal, setRefusal] = createSignal<{ code: string | null; member: string | null } | null>(\n    null,\n  );\n",
    );
    source.push_str("\n  const remove = async () => {\n");
    if let Some(binding) = screen.revision {
        let read = crate::client_ts::function_name(local_name(binding.read_operation)).map_err(
            |error| {
                ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    error.to_string(),
                )
            },
        )?;
        bindings.insert(read.clone());
        runtime.insert("readMember");
        writeln!(
            source,
            "    const record = await {read}(props.transport, [\n      {{ ...props.key, requestId: newRequestId() }},\n    ]);"
        )
        .expect("write");
        source.push_str(
            "    if (record.status !== \"completed\") {\n      setRefusal({ code: record.status, member: null });\n      return;\n    }\n",
        );
        writeln!(source, "    let item = {{}} as {stem}Request;").expect("write");
        writeln!(
            source,
            "    item = writeMember(item, {}, readMember(record.value, {}) ?? null);",
            member_literal(binding.command_key_input),
            member_literal(binding.key_field)
        )
        .expect("write");
        writeln!(
            source,
            "    item = writeMember(item, {}, readMember(record.value, {}) ?? null);",
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
    source.push_str(
        "    setRefusal(\n      outcome.status === \"refused\"\n        ? { code: outcome.code, member: refusedMember(outcome.detail) }\n        : null,\n    );\n  };\n",
    );
    source.push_str(
        "\n  return (\n    <section>\n      <Show when={refusal()}>\n        <p>{refusal()?.code}</p>\n      </Show>\n      <Show\n        when={confirming()}\n        fallback={\n          <button type=\"button\" onClick={() => setConfirming(true)}>\n            delete\n          </button>\n        }\n      >\n        <p>remove this record?</p>\n        <button\n          type=\"button\"\n          onClick={() => {\n            setConfirming(false);\n            void remove();\n          }}\n        >\n          confirm\n        </button>\n        <button type=\"button\" onClick={() => setConfirming(false)}>\n          cancel\n        </button>\n      </Show>\n    </section>\n  );\n}\n",
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

/// One control for each page control the plan names.
fn emit_controls(source: &mut String, screen: &ScreenPlan<'_>) {
    let Some(paging) = screen.paging.as_ref() else {
        return;
    };
    for path in &paging.filter_inputs {
        let repeated = path.ends_with("[]");
        writeln!(source, "        <label>").expect("write");
        writeln!(source, "          {}", control_label(screen, path)).expect("write");
        let value = if repeated {
            format!(
                "change({}, event.currentTarget.value.split(\",\").filter((part) => part !== \"\"))",
                member_literal(path)
            )
        } else {
            format!(
                "change({}, event.currentTarget.value)",
                member_literal(path)
            )
        };
        writeln!(
            source,
            "          <input type=\"text\" onChange={{(event) => {value}}} />"
        )
        .expect("write");
        writeln!(source, "        </label>").expect("write");
    }
    if let (Some(path), Some(sort)) = (paging.sort_field_input, paging.sort) {
        emit_select(source, path, &sort.fields, &control_label(screen, path));
    }
    if let (Some(path), Some(sort)) = (paging.sort_direction_input, paging.sort) {
        emit_select(source, path, &sort.directions, &control_label(screen, path));
    }
    if let (Some(path), Some(limit)) = (paging.limit_input, paging.limit) {
        writeln!(source, "        <label>").expect("write");
        writeln!(source, "          {}", control_label(screen, path)).expect("write");
        writeln!(
            source,
            "          <input\n            type=\"number\"\n            min={{{}}}\n            max={{{}}}\n            value={{{}}}\n            onChange={{(event) => change({}, event.currentTarget.value)}}\n          />",
            limit.minimum,
            limit.maximum,
            limit.default,
            member_literal(path)
        )
        .expect("write");
        writeln!(source, "        </label>").expect("write");
    }
}

/// One select whose options are exactly what the contract permits.
fn emit_select(source: &mut String, path: &str, values: &[String], text: &str) {
    writeln!(source, "        <label>").expect("write");
    writeln!(source, "          {text}").expect("write");
    writeln!(
        source,
        "          <select onChange={{(event) => change({}, event.currentTarget.value)}}>",
        member_literal(path)
    )
    .expect("write");
    source.push_str("            <option value=\"\"></option>\n");
    for value in values {
        writeln!(
            source,
            "            <option value={value:?}>{}</option>",
            value.replace('_', " ")
        )
        .expect("write");
    }
    source.push_str("          </select>\n");
    writeln!(source, "        </label>").expect("write");
}

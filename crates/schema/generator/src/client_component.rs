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

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::client_ir::FieldIr;
use crate::client_plan::{ClientPlan, ModelPlan, Role, ScreenPlan};
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
    matches!(screen.role, Role::Table | Role::Detail)
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

    let mut body = String::new();
    let mut runtime = BTreeSet::new();
    let mut bindings = BTreeSet::new();
    let mut solid = BTreeSet::new();
    let mut table = false;
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
            role => {
                return Err(ClientComponentError::new(
                    ClientComponentErrorKind::UnwrittenRole,
                    format!(
                        "{} states the role {role:?}, which has no component yet",
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
    source.push_str(&body);
    Ok(source)
}

/// The TypeScript member path of one contract path, as a literal list.
fn member_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(|part| crate::client_ts::to_camel(part.trim_end_matches("[]")))
        .collect()
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

/// The label of one field, which is its name with spaces.
///
/// An authored label replaces this when the label epic lands, and the plan
/// carries none today.
fn label(path: &str) -> String {
    path.rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches("[]")
        .replace('_', " ")
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
        writeln!(source, "    header: {:?},", label(&column.path)).expect("write");
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
        "\n/** What the detail screen for `{}` takes. */",
        screen.contract.operation
    )
    .expect("write");
    writeln!(source, "export interface {stem}DetailProps {{").expect("write");
    source.push_str("  /** The transport the application supplies. */\n");
    source.push_str("  readonly transport: Transport;\n");
    source.push_str("  /** The input that names the record. */\n");
    writeln!(source, "  readonly input: {stem}Request;").expect("write");
    source.push_str("  /** Called with every outcome this screen reads. */\n");
    writeln!(
        source,
        "  readonly onOutcome?: (outcome: Outcome<{stem}Result>) => void;"
    )
    .expect("write");
    source.push_str("}\n");

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
        "  const [outcome] = createResource(\n    () => props.input,\n    async (input: {stem}Request) => {{\n      const read = await {function}(props.transport, [\n        {{ ...input, requestId: newRequestId() }},\n      ]);\n      props.onOutcome?.(read);\n      return read;\n    }},\n  );"
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
        writeln!(source, "        <dt>{}</dt>", label(&column.path)).expect("write");
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

/// One control for each page control the plan names.
fn emit_controls(source: &mut String, screen: &ScreenPlan<'_>) {
    let Some(paging) = screen.paging.as_ref() else {
        return;
    };
    for path in &paging.filter_inputs {
        let repeated = path.ends_with("[]");
        writeln!(source, "        <label>").expect("write");
        writeln!(source, "          {}", label(path)).expect("write");
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
        emit_select(source, path, &sort.fields);
    }
    if let (Some(path), Some(sort)) = (paging.sort_direction_input, paging.sort) {
        emit_select(source, path, &sort.directions);
    }
    if let (Some(path), Some(limit)) = (paging.limit_input, paging.limit) {
        writeln!(source, "        <label>").expect("write");
        writeln!(source, "          {}", label(path)).expect("write");
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
fn emit_select(source: &mut String, path: &str, values: &[String]) {
    writeln!(source, "        <label>").expect("write");
    writeln!(source, "          {}", label(path)).expect("write");
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

/**
 * The app-shaped table with its actions (wamn-8iul.7).
 *
 * The fixture's widget query table, through `createTableLoad`, over one stub
 * transport that answers every call:
 *
 * - The row buttons are the definition's actions that take one row.
 * - The bulk action is record-batch, which takes many rows. One call carries
 *   one outer input for each selected row, and the stub refuses a held
 *   widget, so a refusal marks only its row. The gallery fills the other
 *   inputs of each item with fixed values, which a form would ask for.
 * - The editable columns are the definition's update fields. The stub
 *   refuses a code outside its domain, which marks the cell, and a stale
 *   revision, which marks the row. The button above the table changes the
 *   first widget elsewhere, so the next edit of it meets a conflict.
 * - The fixture declares no child table, so the gallery states one: the
 *   events of each widget, scoped by its id, answered by the same stub. A
 *   record-batch adds one event to each widget it recorded.
 */

import { createSignal, type JSX, Show } from "solid-js";

import {
  createTableLoad,
  DataTable,
  type DataTableColumn,
  type DataTableEditResult,
  type DataTableRowResult,
  type DataTableScopeFilter,
} from "@wamn/ui";
import {
  newIdempotencyKey,
  newRequestId,
  type JsonValue,
  type LoadPage,
  type OperationRoute,
  type Outcome,
  toWire,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import {
  query,
  update,
  WIDGET_RECORD_BATCH_REQUEST_FIELDS,
  WIDGET_RECORD_BATCH_ROUTE,
  type WidgetQueryRequest,
  type WidgetQueryRow,
  type WidgetUpdateRequest,
} from "../fixture/widget.js";

/** One widget as the stub stores it, in wire spelling. */
interface StoredWidget {
  id: string;
  code: "priority" | "standard";
  maker_id: string | null;
  note: string | null;
  edit_version: string;
  created_at: string;
}

/** One event of a widget, the row of the child table. */
interface EventRow {
  readonly id: string;
  readonly widgetId: string;
  readonly grade: string;
  readonly amount: string;
  readonly recordedAt: string;
}

/**
 * The child table the gallery states, shaped as the emitter writes one. Its
 * one scope filter is the widget it belongs to.
 */
const WIDGET_EVENT_TABLE = {
  rowId: "id",
  pageMaximum: 100,
  scopeFilters: ["widgetId"],
  sortFields: [],
  sortMaxFields: 1,
  columns: [
    { field: "grade", label: "grade", type: "text", role: "value" },
    { field: "amount", label: "amount", type: "numeric", role: "value" },
    { field: "recordedAt", label: "recorded at", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
  ],
} as const;

/** The widget table with the child the gallery states for it. */
const WIDGETS = {
  ...WIDGET_QUERY_TABLE,
  childTables: [{ definition: "WIDGET_EVENT_TABLE", scopeFilter: "widgetId" }],
} as const;

/** The route of the gallery's child read, which only the stub answers. */
const EVENT_QUERY_ROUTE: OperationRoute = {
  operation: "gallery:widget-event/query@1.0.0",
  method: "GET",
  template: "/widget_event/query",
  freshOnly: false,
  contract: {
    resultClass: "page",
    partialSchema: null,
    errors: [],
    replay: null,
    direct: true,
    kind: "query",
    transaction: "implicit",
  },
};

const uuid = (prefix: number, index: number) =>
  `0000000${prefix}-0000-4000-8000-${String(index).padStart(12, "0")}`;

/** Every seventh widget is held, and record-batch refuses it. */
const HELD = "held";

/** A stub of the widget release, with `size` widgets and two events for each. */
export function actionStub(size: number) {
  const widgets: StoredWidget[] = Array.from({ length: size }, (_, index) => ({
    id: uuid(0, index),
    code: index % 3 === 0 ? "priority" : "standard",
    maker_id: null,
    note: index % 7 === 3 ? HELD : null,
    edit_version: "1",
    created_at: `2026-09-21T${String(index % 24).padStart(2, "0")}:00:00.000000Z`,
  }));
  const events: EventRow[] = widgets.flatMap((widget, index) =>
    [0, 1].map((number) => ({
      id: uuid(1, index * 2 + number),
      widgetId: widget.id,
      grade: number === 0 ? "first" : "second",
      amount: `${index + number}.50`,
      recordedAt: `2026-09-2${number + 2}T08:00:00.000000Z`,
    })),
  );
  const writeListeners = new Set<() => void>();

  const completed = (value: JsonValue): Outcome<JsonValue> => ({ status: "completed", value });
  const refused = (code: string, detail: JsonValue): Outcome<JsonValue> => ({
    status: "refused",
    code,
    detail,
  });
  const find = (id: unknown) => widgets.find((widget) => widget.id === id);

  function recordOne(item: JsonValue): Outcome<JsonValue> {
    const value = (item as { value: { expected_edit_version: string; line: { widget_id: string; amount: string }[] } })
      .value;
    const widget = find(value.line[0]?.widget_id);
    if (widget === undefined) {
      return refused("invalid_input", { field: "value.line[].widget_id" });
    }
    if (widget.note === HELD) {
      return refused("invalid_input", { field: "value.line[].widget_id", reason: "held" });
    }
    events.push({
      id: uuid(2, events.length),
      widgetId: widget.id,
      grade: "first",
      amount: value.line[0]!.amount,
      recordedAt: new Date().toISOString(),
    });
    widget.edit_version = String(Number(widget.edit_version) + 1);
    return completed({ widget_id: widget.id });
  }

  const transport: Transport = {
    invoke: async (request) => {
      const item = request.items[0] as { [name: string]: JsonValue } | undefined;
      switch (request.operation) {
        case "platform-fixture:widget/query@1.0.0": {
          const limit = (item?.["limit"] as number | undefined) ?? 100;
          const codes = (item?.["filter"] as { code?: string[] } | undefined)?.code;
          const kept = codes === undefined ? widgets : widgets.filter((widget) => codes.includes(widget.code));
          return completed({
            item: kept.slice(0, limit).map((widget) => ({ ...widget })),
            next_cursor: kept.length > limit ? "more" : null,
          });
        }
        case "platform-fixture:widget/update@1.0.0": {
          const widget = find(item?.["id"]);
          if (widget === undefined) {
            return refused("not_found", { field: "id", id: item?.["id"] ?? null });
          }
          if (item?.["expected_edit_version"] !== widget.edit_version) {
            return refused("concurrency_conflict", {
              expected_row_version: item?.["expected_edit_version"] ?? null,
              observed_row_version: widget.edit_version,
            });
          }
          const change = (item?.["change"] ?? {}) as Partial<StoredWidget>;
          if (change.code !== undefined && change.code !== "priority" && change.code !== "standard") {
            return refused("invalid_input", { field: "change.code" });
          }
          Object.assign(widget, change, { edit_version: String(Number(widget.edit_version) + 1) });
          for (const listener of writeListeners) {
            listener();
          }
          return completed({ ...widget });
        }
        case EVENT_QUERY_ROUTE.operation: {
          const scope = (item?.["filter"] as { widget_id?: string[] } | undefined)?.widget_id ?? [];
          const kept = events.filter((event) => scope.includes(event.widgetId));
          return completed({ item: kept as unknown as JsonValue, next_cursor: null });
        }
        default:
          return refused("permission_denied", { operation: request.operation });
      }
    },
    invokeEach: async (request) => {
      if (request.operation !== WIDGET_RECORD_BATCH_ROUTE.operation) {
        return request.items.map(() => refused("permission_denied", { operation: request.operation }));
      }
      return request.items.map(recordOne);
    },
    onWrite: (listener) => {
      writeListeners.add(listener);
      return () => writeListeners.delete(listener);
    },
  };

  /** Changes the first widget as another session would, so its revision moves. */
  const changeElsewhere = () => {
    const widget = widgets[0]!;
    widget.edit_version = String(Number(widget.edit_version) + 1);
  };

  return { transport, changeElsewhere };
}

/** The events of one widget: the child table, scoped by the widget's id alone. */
function EventTable(props: { transport: Transport; widgetId: string }): JSX.Element {
  const load = createTableLoad<EventRow>(WIDGET_EVENT_TABLE, async (limit) => {
    const outcome = await props.transport.invoke({
      ...EVENT_QUERY_ROUTE,
      items: [{ limit, filter: { widget_id: [props.widgetId] } }],
    });
    return outcome as Outcome<LoadPage<EventRow>>;
  });
  void load.load();
  return (
    <DataTable
      name="widget-event"
      columns={WIDGET_EVENT_TABLE.columns as readonly DataTableColumn<EventRow>[]}
      rowId={WIDGET_EVENT_TABLE.rowId}
      rows={load.state().rows}
      fullyRead={load.state().fullyRead}
      busy={load.state().busy}
      refusal={load.state().refusal}
      cap={load.state().cap}
      onCapChange={(cap) => void load.load(cap)}
      onRefresh={() => void load.load()}
      startedAt={load.state().startedAt}
      endedAt={load.state().endedAt}
      sortFields={[]}
      sortMaxFields={1}
      onSortChange={() => {}}
      // The parent fixes the one scope filter, so the child offers none to change.
      scopeFilters={[]}
      onScopeChange={() => {}}
    />
  );
}

/** What one outcome of a many-item write means to its row. */
function rowResult(outcome: Outcome<JsonValue>): DataTableRowResult {
  switch (outcome.status) {
    case "completed":
      return { status: "completed" };
    case "refused":
      return { status: "refused", message: outcome.code ?? "refused" };
    case "uncertain":
      return { status: "uncertain", message: outcome.reason };
    default:
      return { status: "uncertain", message: "partially completed" };
  }
}

/** The widget table with its row buttons, its bulk action, its editable cells and its child. */
export function ActionTable(props: { size: number }): JSX.Element {
  const stub = actionStub(props.size);
  const definition = WIDGETS;
  const [scope, setScope] = createSignal<Partial<WidgetQueryRequest>>({});
  const [opened, setOpened] = createSignal<string | null>(null);

  const load = createTableLoad<WidgetQueryRow>(definition, async (limit) => {
    const request = writeMember({ ...scope() }, ["limit"], limit) as WidgetQueryRequest;
    return query(stub.transport, [request]);
  });
  void load.load();

  const changeScope = (filters: readonly DataTableScopeFilter[]) => {
    const codes = filters.find((filter) => filter.field === "code")?.values;
    setScope(codes === undefined ? {} : { filter: { code: [...codes] as ("priority" | "standard")[] } });
    void load.load();
  };

  async function edit(
    row: WidgetQueryRow,
    field: string,
    value: unknown,
  ): Promise<DataTableEditResult<WidgetQueryRow>> {
    const target = definition.update.fields.find((editable) => editable.field === field)!;
    let request = {
      requestId: newRequestId(),
      id: row.id,
      expectedEditVersion: row[definition.update.revisionField],
    } as WidgetUpdateRequest;
    request = writeMember(request, target.input, value as JsonValue);
    const outcome = await update(stub.transport, [request]);
    switch (outcome.status) {
      case "completed":
        return { status: "completed", row: outcome.value };
      case "refused":
        return outcome.code === "concurrency_conflict"
          ? { status: "conflict", message: "The widget changed since it loaded." }
          : { status: "refused", message: outcome.code ?? "refused" };
      case "uncertain":
        return { status: "uncertain", message: outcome.reason };
      default:
        return { status: "uncertain", message: "partially completed" };
    }
  }

  async function bulk(_operation: string, rows: readonly WidgetQueryRow[]): Promise<readonly DataTableRowResult[]> {
    const items = rows.map((row) =>
      toWire(
        {
          requestId: newRequestId(),
          value: {
            expectedEditVersion: row.editVersion,
            grade: "first",
            idempotencyKey: newIdempotencyKey(),
            line: [{ widgetId: row.id, amount: "1.00" }],
          },
        },
        WIDGET_RECORD_BATCH_REQUEST_FIELDS,
      ),
    );
    const outcomes = await stub.transport.invokeEach!({ ...WIDGET_RECORD_BATCH_ROUTE, items });
    return outcomes.map(rowResult);
  }

  return (
    <div class="flex h-full min-h-0 flex-col gap-2">
      <div class="flex shrink-0 items-center gap-4 text-sm">
        <button type="button" class="underline" onClick={() => stub.changeElsewhere()}>
          change the first widget elsewhere
        </button>
        <Show when={opened()}>{(text) => <p role="status">{text()}</p>}</Show>
      </div>
      <div class="min-h-0 flex-1">
        <DataTable
          name="widget"
          columns={definition.columns as readonly DataTableColumn<WidgetQueryRow>[]}
          rowId={definition.rowId}
          rows={load.state().rows}
          fullyRead={load.state().fullyRead}
          busy={load.state().busy}
          refusal={load.state().refusal}
          cap={load.state().cap}
          onCapChange={(cap) => void load.load(cap)}
          onRefresh={() => void load.load()}
          startedAt={load.state().startedAt}
          endedAt={load.state().endedAt}
          sortFields={definition.sortFields}
          sortMaxFields={definition.sortMaxFields}
          onSortChange={load.sortBy}
          scopeFilters={definition.scopeFilters}
          onScopeChange={changeScope}
          rowActions={(row) => (
            <>
              {definition.actions
                .filter((action) => !action.many)
                .map((action) => (
                  <button
                    type="button"
                    class="underline"
                    onClick={() => setOpened(`${action.label} opened for ${row.id}`)}
                  >
                    {action.label}
                  </button>
                ))}
            </>
          )}
          bulkActions={definition.actions.filter((action) => action.many)}
          onBulk={bulk}
          editableFields={definition.update.fields.map((editable) => editable.field)}
          onEdit={edit}
          onEditing={load.hold}
          onRowChange={load.replaceRow}
          onReload={() => void load.load()}
          childTables={definition.childTables.map(() => ({
            label: "events",
            render: (widgetId: string) => <EventTable transport={stub.transport} widgetId={widgetId} />,
          }))}
        />
      </div>
    </div>
  );
}

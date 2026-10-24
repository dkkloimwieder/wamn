import { For, createMemo, untrack } from "solid-js";
import { Table, TableRow, createTable } from "@wfe/components/table";
import { VirtualRows, createTableVirtualizer } from "@wfe/components/virtual";
import { PackagingQueryTable } from "@wamn/wms-client/components/packaging.js";
import { PACKAGING_QUERY_RESULT_FIELDS } from "@wamn/wms-client/packaging.js";
import type { NestedFields } from "@wamn/web-runtime";
import type { PackagingRow } from "./packaging-feed";
import { roundHalfAwayFromZero } from "./totals";

/** One group of the screen: packaging by status and type. */
export interface GroupRow {
  readonly status: string;
  readonly type: string;
  readonly count: number;
  /** The sum of `row_version`, a stand-in: packaging has no other numeric column. */
  readonly total: number | string;
}

/** The rows a shape hands to the view, in display order. */
export interface ViewSource {
  readonly ids: () => readonly string[];
  readonly get: (id: string) => PackagingRow | undefined;
  readonly groups: () => readonly GroupRow[];
  readonly totals: () => { readonly count: number; readonly total: number | string };
}

const ROW_HEIGHT = 32;
const members = (PACKAGING_QUERY_RESULT_FIELDS.item as NestedFields).fields;

/** The generated columns, read through the binding's own member names. */
const columns = Object.entries(PackagingQueryTable.columns).map(([field, column]) => {
  const member = members[field] as keyof PackagingRow;
  return { id: field, header: column.label, accessorFn: (row: PackagingRow) => row[member] };
});

/** The total as the screen shows it: rounded half away from zero to whole units. */
export const shown = (value: number | string) => roundHalfAwayFromZero(value, 0);

/**
 * The packaging table of one shape: the generated columns over virtual rows,
 * then the groups and the totals. Both shapes render through this component.
 */
export function PackagingView(props: { label: string; source: ViewSource }) {
  const source = untrack(() => props.source);
  const table = createTable<PackagingRow>({
    source: { ids: source.ids, get: source.get },
    columns,
    manualProcessing: true,
  });
  let viewport: HTMLDivElement | undefined;
  const virtualizer = createTableVirtualizer<string>({
    mode: "fixed",
    get count() {
      return source.ids().length;
    },
    rowHeight: ROW_HEIGHT,
    getItemKey: (index) => untrack(() => source.ids()[index]!),
    getIndexForKey: (key) => untrack(() => source.ids().indexOf(key)),
    getScrollElement: () => viewport ?? null,
    get scrollMargin() {
      return viewport?.querySelector("thead")?.offsetHeight ?? 0;
    },
    observeElements: () => Array.from(viewport?.querySelectorAll("thead") ?? []),
  });
  const totals = createMemo(() => source.totals());
  return (
    <section class="bench-shape" aria-label={props.label}>
      <p class="bench-count">
        <output data-bench-rows={source.ids().length}>{source.ids().length.toLocaleString("en-US")}</output> rows
      </p>
      <div
        class="bench-scroll"
        ref={(node) => {
          viewport = node;
        }}
      >
        <Table
          table={table}
          accessibleLabel={`${props.label} rows`}
          renderBody={() => (
            <VirtualRows
              window={virtualizer}
              layout="table"
              columns={() => columns.length}
              renderRow={(id) => <TableRow table={table} id={id} />}
            />
          )}
        />
      </div>
      <table class="bench-groups" aria-label={`${props.label} groups`}>
        <thead>
          <tr>
            <th>status</th>
            <th>type</th>
            <th>count</th>
            <th>row_version sum</th>
          </tr>
        </thead>
        <tbody>
          <For each={source.groups()} keyed={(group) => `${group.status}|${group.type}`}>
            {(group) => (
              <tr>
                <td>{group().status}</td>
                <td>{group().type}</td>
                <td>{group().count}</td>
                <td>{shown(group().total)}</td>
              </tr>
            )}
          </For>
        </tbody>
        <tfoot>
          <tr>
            <th colspan={2}>total</th>
            <td data-bench-total-count={totals().count}>{totals().count}</td>
            <td>{shown(totals().total)}</td>
          </tr>
        </tfoot>
      </table>
    </section>
  );
}

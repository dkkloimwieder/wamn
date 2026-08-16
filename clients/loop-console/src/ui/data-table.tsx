import type { JSX } from "@solidjs/web";
import { For, Show, createMemo, createSignal, createUniqueId, untrack } from "solid-js";

import type { Tone } from "./status";
import "./data-table.css";
import "./tone.css";

/**
 * The semantic table §2.2's execution table and §2.3's case list are both built
 * from: a caption, a header row, a body of rows, and — where a row has more to
 * say than its cells hold — a disclosure that opens into a row of its own.
 */

export type Column<Row> = {
  readonly header: string;
  readonly cell: (row: Row) => JSX.Element;
};

/** Disclosure's contract, row-shaped: uncontrolled until `open` is supplied. */
export type RowDisclosure = {
  /** What the toggle opens, so the button's name says what it acts on. */
  readonly label: string;
  /**
   * A thunk, not an element: step 4's three-lane inspector over a 200-row run
   * would otherwise build 200 closed subtrees the reader never asks for.
   */
  readonly content: () => JSX.Element;
  /** §2.3's failed cases arrive open — the author came for them — and still close. */
  readonly startOpen?: boolean;
  /**
   * Supplied means the owner drives the row: step 4's expand-all/collapse-all.
   * It outranks `startOpen`, which a controlled row never consults again. The
   * owner's value is a loan and not a transfer — released, the row returns to
   * what the reader last asked for, so leaving trace mode gives the reader back
   * the table they had rather than freezing the owner's last sweep.
   */
  readonly open?: boolean;
  /**
   * The state the reader ASKED for, not one the row entered: a controlled row
   * reports every click and moves only when its owner answers, so an owner that
   * ignores two clicks hears the same value twice. An owner that supplies `open`
   * and no `onToggle` leaves the row nothing to answer with — §2.6's `Enter on
   * any row toggles its disclosure` is that owner's to keep, not the row's.
   */
  readonly onToggle?: (next: boolean) => void;
};

export type DataTableProps<Row> = {
  caption: string;
  columns: readonly Column<Row>[];
  rows: readonly Row[];
  /** Row identity, so a re-read moves rows rather than rebuilding the body. */
  rowKey: (row: Row) => string;
  /**
   * §2.2's `error rows tint their left edge`. The tint only ever restates what
   * the row's own cells already say in words, so no row tells its status in
   * colour alone.
   */
  tone?: (row: Row) => Tone | null;
  disclosure?: (row: Row) => RowDisclosure | null;
  /** Required: a table with no rows still owes the reader a reason. */
  empty: JSX.Element;
  /** §2.2's `showing 200 of 3,412 · truncated`. */
  footer?: JSX.Element;
};

function DataRow<Row>(props: {
  row: Row;
  columns: readonly Column<Row>[];
  tone: Tone | null;
  disclosure: RowDisclosure | null;
}): JSX.Element {
  const panelId = createUniqueId();
  /** The caller's factory, run once per row rather than once per reader of it. */
  const rowDisclosure = createMemo(() => props.disclosure);
  // Seeded, not derived: the reader's own open/closed state outlives a re-read,
  // so `startOpen` is read once here and never again.
  const [ownOpen, setOwnOpen] = createSignal(untrack(() => rowDisclosure()?.startOpen ?? false));
  const open = () => rowDisclosure()?.open ?? ownOpen();

  function toggle(): void {
    const next = !open();
    setOwnOpen(next);
    rowDisclosure()?.onToggle?.(next);
  }

  return (
    /*
     * A row group per row, so opening one row inserts a node inside its own
     * group instead of lengthening the body's row list. Expand-all moves every
     * row at once, and a body that reorders its rows re-inserts the toggle the
     * reader is standing on — which drops the focus §2.6's keyboard depends on.
     */
    <tbody>
      <tr class="data-table-row" data-tone={props.tone ?? undefined}>
        <For each={props.columns}>
          {(column, index) => (
            <td class="data-table-cell">
              {/*
               * §2.6's `Enter on any row toggles its disclosure`: a native button
               * earns Enter and Space, so the row itself carries no handler.
               */}
              <Show when={index() === 0 ? rowDisclosure() : null}>
                {(disclosure) => (
                  <button
                    type="button"
                    class="data-table-toggle"
                    // spelled out: @solidjs/web drops a `false` aria value
                    aria-expanded={open() ? "true" : "false"}
                    // only while the disclosed row exists; see disclosure.tsx
                    aria-controls={open() ? panelId : undefined}
                    aria-label={`${open() ? "collapse" : "expand"} ${disclosure().label}`}
                    onClick={toggle}
                  >
                    {open() ? "▾" : "▸"}
                  </button>
                )}
              </Show>
              {column.cell(props.row)}
            </td>
          )}
        </For>
      </tr>
      {/* The thunk runs only here, so closed content is never built at all. */}
      <Show when={open()}>
        <tr class="data-table-disclosed" id={panelId}>
          <td class="data-table-cell" colspan={props.columns.length}>
            {/*
             * Read once, on the flush that opens the row: step 4's inspector
             * holds disclosures and JSON views of its own, and rebuilding it
             * because the caller's factory returned a fresh descriptor would
             * throw away expansions the reader opened by hand.
             */}
            {untrack(() => rowDisclosure()?.content())}
          </td>
        </tr>
      </Show>
    </tbody>
  );
}

export function DataTable<Row>(props: DataTableProps<Row>): JSX.Element {
  return (
    <table class="data-table">
      {/*
       * §3's floor: every table carries a caption. It is hidden because §1.4's
       * SectionLabel is already the visible heading above the table, and a
       * second visible one would compete with the section rule it sits on.
       */}
      <caption class="visually-hidden">{props.caption}</caption>
      <thead>
        <tr>
          <For each={props.columns}>
            {(column) => (
              <th class="data-table-head frame" scope="col">
                {column.header}
              </th>
            )}
          </For>
        </tr>
      </thead>
      <Show
        when={props.rows.length > 0}
        fallback={
          <tbody>
            <tr>
              <td class="data-table-cell" colspan={props.columns.length}>
                {props.empty}
              </td>
            </tr>
          </tbody>
        }
      >
        <For each={props.rows} keyed={props.rowKey}>
          {(row) => (
            <DataRow
              row={row()}
              columns={props.columns}
              tone={props.tone?.(row()) ?? null}
              disclosure={props.disclosure?.(row()) ?? null}
            />
          )}
        </For>
      </Show>
      <Show when={props.footer}>
        <tfoot>
          <tr>
            <td class="data-table-footer" colspan={props.columns.length}>
              {props.footer}
            </td>
          </tr>
        </tfoot>
      </Show>
    </table>
  );
}

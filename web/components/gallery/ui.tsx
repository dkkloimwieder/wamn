/**
 * One section for each web/ui export, with its main states.
 *
 * Every value here is sample data. The one control that reads a list,
 * RecordSelect, reads it from the shared stub transport through the fixture's
 * binding, so it searches and pages exactly as a generated form does.
 */

import { createSignal, For, onMount, type JSX } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";

import {
  announceOutcome,
  Badge,
  Button,
  CheckField,
  ChoiceField,
  ConfirmAction,
  DataGrid,
  DataGridContainer,
  DataGridTable,
  DetailItem,
  DetailList,
  FieldGroup,
  FormActions,
  gridFeatures,
  RecordSelect,
  TableScreen,
  TextField,
  type Choice,
  type GridFeatures,
} from "@wamn/ui";
import type { Outcome } from "@wamn/web-runtime";

import { query, type WidgetMakerQueryRow } from "../fixture/widget_maker.js";
import { paged, WIDGET } from "../stubs/index.js";
import { Section, State } from "./section.js";

const BUTTON_VARIANTS = [
  "default",
  "outline",
  "secondary",
  "ghost",
  "destructive",
  "link",
] as const;

const BADGE_VARIANTS = [
  "default",
  "secondary",
  "destructive",
  "outline",
  "ghost",
  "link",
  "primary-light",
  "destructive-light",
  "success-light",
  "warning-light",
  "info-light",
  "primary-outline",
  "destructive-outline",
  "success-outline",
  "warning-outline",
  "info-outline",
] as const;

const CODES: readonly Choice[] = [
  { value: "standard", text: "standard" },
  { value: "priority", text: "priority" },
];

function Fields(): JSX.Element {
  const [text, setText] = createSignal("standard");
  const [choice, setChoice] = createSignal("priority");
  const [checked, setChecked] = createSignal(true);
  return (
    <div class="grid gap-6 md:grid-cols-3">
      <State name="empty">
        <FieldGroup>
          <TextField label="code" type="text" />
          <ChoiceField label="code" choices={CODES} allowEmpty value="" onChange={() => {}} />
          <CheckField label="archived" checked={false} onChange={() => {}} />
        </FieldGroup>
      </State>
      <State name="filled">
        <FieldGroup>
          <TextField label="code" type="text" value={text()} onInput={setText} />
          <ChoiceField
            label="code"
            choices={CODES}
            allowEmpty={false}
            value={choice()}
            onChange={setChoice}
          />
          <CheckField label="archived" checked={checked()} onChange={setChecked} />
        </FieldGroup>
      </State>
      <State name="refused">
        <FieldGroup>
          <TextField label="code" type="text" value="x" error="invalid_value" />
          <ChoiceField
            label="code"
            choices={CODES}
            allowEmpty
            value=""
            onChange={() => {}}
            error="required"
          />
          <CheckField label="archived" checked={false} onChange={() => {}} error="required" />
        </FieldGroup>
      </State>
    </div>
  );
}

/** A selector over the shared stub: a first page, a search and a next page. */
function LiveSelect(): JSX.Element {
  const { transport } = paged();
  const [rows, setRows] = createSignal<readonly WidgetMakerQueryRow[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [value, setValue] = createSignal<string | null>(null);

  async function read(search: string | null, after: string | null) {
    const outcome = await query(transport, [
      {
        requestId: crypto.randomUUID(),
        ...(search === null || search === "" ? {} : { filter: { name: [search] } }),
        ...(after === null ? {} : { cursor: after }),
      },
    ]);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, "RecordSelect");
      return;
    }
    setRows(after === null ? outcome.value.item : [...rows(), ...outcome.value.item]);
    setCursor(outcome.value.nextCursor);
  }
  onMount(() => void read(null, null));

  return (
    <RecordSelect
      label="maker"
      options={rows()}
      optionValue={(row) => row.id}
      optionLabel={(row) => row.name}
      value={value()}
      onChange={setValue}
      onSearch={(text) => void read(text, null)}
      hasNextPage={cursor() !== null}
      onNextPage={() => void read(null, cursor())}
    />
  );
}

/** One sample widget row of the grid. */
interface SampleRow {
  readonly id: string;
  readonly code: string;
  readonly note: string | null;
  readonly editVersion: string;
  readonly createdAt: string;
}

const COLUMNS: ColumnDef<GridFeatures, SampleRow>[] = [
  { accessorKey: "code", header: "code" },
  { accessorKey: "createdAt", header: "created at" },
  { accessorKey: "editVersion", header: "edit version" },
  { accessorKey: "id", header: "id" },
  { accessorKey: "note", header: "note" },
];

const HUNDRED: readonly SampleRow[] = Array.from({ length: 100 }, (_, index) => ({
  id: `widget-${String(index + 1).padStart(3, "0")}`,
  code: index % 3 === 0 ? "priority" : "standard",
  note: index % 4 === 0 ? "checked at the dock" : null,
  editVersion: String(1 + (index % 5)),
  createdAt: `2026-09-${String(1 + (index % 28)).padStart(2, "0")}T12:00:00.000000Z`,
}));

function Grid(props: { rows: readonly SampleRow[]; loading: boolean }): JSX.Element {
  const table = createTable({
    features: gridFeatures,
    get data() {
      return props.rows as SampleRow[];
    },
    columns: COLUMNS,
    manualPagination: true,
  });
  return (
    <DataGrid table={table} recordCount={props.rows.length} isLoading={props.loading}>
      <DataGridContainer>
        <DataGridTable />
      </DataGridContainer>
    </DataGrid>
  );
}

const OUTCOMES: readonly Outcome<null>[] = [
  { status: "completed", value: null },
  { status: "partiallyCompleted", committedResult: null, failedOutcome: null },
  { status: "refused", code: "permission_denied", detail: null },
  { status: "uncertain", reason: "the connection closed", retryRefusal: null },
];

export function UiSections(): JSX.Element {
  return (
    <>
      <Section title="Button" name="Button">
        <div class="flex flex-wrap gap-2">
          <For each={BUTTON_VARIANTS}>
            {(variant) => <Button variant={variant}>{variant}</Button>}
          </For>
          <Button disabled>disabled</Button>
        </div>
      </Section>

      <Section title="Badge" name="Badge">
        <div class="flex flex-wrap gap-2">
          <For each={BADGE_VARIANTS}>{(variant) => <Badge variant={variant}>{variant}</Badge>}</For>
        </div>
      </Section>

      <Section title="Fields" name="TextField, ChoiceField, CheckField">
        <Fields />
      </Section>

      <Section title="Record select" name="RecordSelect">
        <div class="grid gap-6 md:grid-cols-2">
          <State name="first page, search and next page, over the stub transport">
            <LiveSelect />
          </State>
          <State name="refused">
            <RecordSelect
              label="maker"
              options={[]}
              optionValue={(row: { id: string }) => row.id}
              optionLabel={(row: { id: string }) => row.id}
              value={null}
              onChange={() => {}}
              error="permission_denied"
            />
          </State>
        </div>
      </Section>

      <Section title="Detail list" name="DetailList, DetailItem">
        <div class="grid gap-6 md:grid-cols-2">
          <State name="loading">
            <DetailList loading>
              <DetailItem term="id">{WIDGET}</DetailItem>
              <DetailItem term="code">standard</DetailItem>
              <DetailItem term="edit version">7</DetailItem>
            </DetailList>
          </State>
          <State name="loaded">
            <DetailList loading={false}>
              <DetailItem term="id">{WIDGET}</DetailItem>
              <DetailItem term="code">standard</DetailItem>
              <DetailItem term="edit version">7</DetailItem>
            </DetailList>
          </State>
        </div>
      </Section>

      <Section title="Data grid" name="DataGrid, DataGridContainer, DataGridTable">
        <div class="flex flex-col gap-6">
          <State name="no rows">
            <Grid rows={[]} loading={false} />
          </State>
          <State name="loading">
            <Grid rows={[]} loading />
          </State>
          <State name="100 rows">
            <Grid rows={HUNDRED} loading={false} />
          </State>
        </div>
      </Section>

      <Section title="Actions row and table screen" name="FormActions, TableScreen">
        <TableScreen>
          <form onSubmit={(event) => event.preventDefault()}>
            <FieldGroup>
              <TextField label="code" type="text" value="standard" />
            </FieldGroup>
            <FormActions>
              <Button type="submit">read</Button>
            </FormActions>
          </form>
          <Grid rows={HUNDRED.slice(0, 3)} loading={false} />
          <FormActions>
            <Button type="button" variant="outline" disabled>
              next page
            </Button>
          </FormActions>
        </TableScreen>
      </Section>

      <Section title="Confirm action" name="ConfirmAction">
        <State name="closed, and opens when you select it">
          <div>
            <ConfirmAction
              trigger="delete"
              question="Delete this widget?"
              confirm="delete"
              cancel="cancel"
              onConfirm={() => announceOutcome({ status: "completed", value: null }, "delete")}
            />
          </div>
        </State>
      </Section>

      <Section title="Outcome toast" name="announceOutcome">
        <div class="flex flex-wrap gap-2">
          <For each={OUTCOMES}>
            {(outcome) => (
              <Button variant="outline" onClick={() => announceOutcome(outcome, "widget.query")}>
                {outcome.status}
              </Button>
            )}
          </For>
        </div>
      </Section>
    </>
  );
}

/**
 * The role grid of one application (docs/plan/platform-ui.md §4.7).
 *
 * It reads `role.list` for its role choice and `permission.list` for the
 * chosen role, and shows one row per operation, grouped by interface. A row
 * shows the direct selection, the effective state and the roots that require
 * it, so the screen answers why a role has a permission.
 *
 * A served, grantable row has one toggle, which calls `permission.grant` or
 * `permission.revoke`. A row that is effective only through other roots has
 * no direct selection to remove, so it has no toggle. The role `admin` holds
 * every operation the release serves, and it has no toggle. Each write
 * announces its outcome, a refusal shows the text of its contract, and the
 * grid reads again after each write. It writes no database directly.
 */

import { permission, role } from "@wamn/control-client";
import { newRequestId, type Outcome, refusalSentence, type Transport } from "@wamn/web-runtime";
import { createResource, createSignal, createUniqueId, type JSX, Show } from "solid-js";

import { Switch } from "../components/ui/switch";
import { ChoiceField } from "../fields";
import { announceOutcome } from "../outcome";
import { builtColumns, type BuiltColumn, type TableColumn, type TableSort } from "../table/columns";
import { defaultGridView } from "../table/grid-view";
import { SetTable } from "../table/set-table";
import { defaultSetView } from "../table/set-view";

export interface RoleGridProps {
  readonly transport: Transport;
  /** The role chosen first. The default is no role. */
  readonly role?: string | undefined;
}

/** One operation of the role, as the grid shows it. */
interface PermissionRow {
  readonly operation: string;
  readonly interface: string;
  readonly selected: boolean;
  readonly effective: boolean;
  readonly requiredBy: string;
  /** How the row changes: a toggle, or why it has none. */
  readonly control: "toggle" | "admin" | "admin only" | "fixed" | "required";
}

/** The part of an operation reference between `:` and the last `/`. */
export function operationInterface(operation: string): string {
  const start = operation.indexOf(":") + 1;
  const end = operation.lastIndexOf("/");
  return end > start ? operation.slice(start, end) : "";
}

const COLUMNS: readonly TableColumn<PermissionRow>[] = [
  { field: "interface", label: "interface", type: "text" },
  { field: "operation", label: "operation", type: "text", role: "key" },
  { field: "selected", label: "selected", type: "boolean" },
  { field: "effective", label: "effective", type: "boolean" },
  { field: "requiredBy", label: "required by", type: "text" },
];

function rows(listed: permission.PermissionListResult): PermissionRow[] {
  return listed.operations.map((operation) => {
    const required = operation.requiredBy.length > 0;
    const control: PermissionRow["control"] = listed.admin
      ? "admin"
      : operation.adminOnly
        ? "admin only"
        : !operation.served || !operation.grantable
          ? "fixed"
          : required && !operation.selected
            ? "required"
            : "toggle";
    return {
      operation: operation.operation,
      interface: operationInterface(operation.operation),
      selected: operation.selected,
      effective: listed.admin ? operation.served : operation.selected || required,
      requiredBy: operation.requiredBy.join(", "),
      control,
    };
  });
}

/** The toggle of one row, named for its operation. */
function ToggleCell(props: { row: PermissionRow; onChange: (on: boolean) => void }): JSX.Element {
  const id = createUniqueId();
  return (
    <span class="flex items-center">
      <Switch id={id} size="sm" checked={props.row.selected} onChange={props.onChange} />
      <label for={id} class="sr-only">
        select {props.row.operation}
      </label>
    </span>
  );
}

export function RoleGrid(props: RoleGridProps): JSX.Element {
  // eslint-disable-next-line solid/reactivity -- the prop is the first choice only.
  const [chosen, setChosen] = createSignal<string | null>(props.role ?? null);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [roles] = createResource(async () => {
    const outcome = await role.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.roles : [];
  });
  const [listed, { refetch }] = createResource(chosen, async (name) => {
    const outcome = await permission.list(props.transport, [{ role: name }]);
    return outcome.status === "completed" ? outcome.value : null;
  });
  const [view, setView] = createSignal(defaultSetView([{ field: "interface", time: false }]));
  const [grid, setGrid] = createSignal(defaultGridView([...COLUMNS.map((column) => column.field), "control"]));
  const [sort, setSort] = createSignal<readonly TableSort[]>([{ field: "operation", direction: "ascending" }]);

  /** Announces one write, shows a refusal's text, and reads the role again. */
  const settle = async <T,>(outcome: Outcome<T>, screen: string) => {
    announceOutcome(outcome, screen);
    setRefusal(outcome.status === "refused" ? refusalSentence(outcome.code, outcome.text) : null);
    await refetch();
  };

  const toggle = async (row: PermissionRow, on: boolean) => {
    const name = chosen();
    if (name === null) {
      return;
    }
    const value = { role: name, operation: row.operation };
    if (on) {
      await settle(await permission.grant(props.transport, [{ requestId: newRequestId(), value }]), "grant");
    } else {
      await settle(await permission.revoke(props.transport, [{ requestId: newRequestId(), value }]), "revoke");
    }
  };

  const columns = (): BuiltColumn<PermissionRow>[] => [
    ...builtColumns(COLUMNS),
    {
      id: "control",
      size: 140,
      header: () => "change",
      cell: (row) =>
        row.control === "toggle" ? (
          <ToggleCell row={row} onChange={(on) => void toggle(row, on)} />
        ) : (
          <span class="text-muted-foreground text-xs">{row.control}</span>
        ),
    },
  ];

  return (
    <div class="flex flex-col gap-4">
      <ChoiceField
        label="role"
        choices={(roles() ?? []).map((name) => ({ value: name, text: name }))}
        allowEmpty={false}
        value={chosen()}
        onChange={(name) => {
          setRefusal(null);
          setChosen(name === "" ? null : name);
        }}
      />
      <Show when={refusal()}>{(text) => <p role="alert">{text()}</p>}</Show>
      <Show when={listed()}>
        {(current) => (
          <>
            <Show when={current().admin}>
              <p>The role admin holds every operation that the release serves.</p>
            </Show>
            <SetTable
              name={`role-${current().role}`}
              rows={rows(current())}
              rowId={(row) => row.operation}
              columns={columns()}
              view={view()}
              onView={setView}
              grid={grid()}
              onGrid={setGrid}
              sort={sort()}
              onSort={setSort}
              sortMaxFields={1}
            />
          </>
        )}
      </Show>
    </div>
  );
}

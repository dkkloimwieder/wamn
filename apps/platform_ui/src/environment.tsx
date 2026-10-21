import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { environment, release } from "@wamn/control-org-client";
import { MissingPage, Page, RecordLink } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { Field, FieldValue, FormGrid, Label } from "@wfe/components";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export function Environment(
  props: ScreenProps & {
    table?: "workloads" | "live-releases" | "operations" | "roles";
    reachOperations?: boolean;
    fields?: "reads" | "writes";
  },
): JSX.Element {
  const scope = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
  const [state, setState] = createSignal<"active" | "inactive">();
  const [status, setStatus] = createSignal<environment.EnvironmentStatusResult>();
  const [profile, setProfile] = createSignal<release.ReleaseProfileResult>();
  const [trouble, setTrouble] = createSignal<string>();
  const [missing, setMissing] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const [listed, observed] = await Promise.all([
      environment.list(props.transport, [{ project: scope.project }]),
      environment.status(props.transport, [scope]),
    ]);
    if (observed.status === "refused" && observed.code === "environment_not_found") {
      if (active) setMissing(true);
      return;
    }
    const entry = value(listed).environments.find((entry) => entry.name === scope.env);
    if (active) setMissing(!entry);
    if (!entry) return;
    if (active) setState(entry.state);
    const next = value(observed);
    if (active) {
      setStatus(next);
      if (!next.digest) setProfile(undefined);
    }
    if (next.digest) {
      const nextProfile = value(await release.profile(props.transport, [{ ...scope, digest: next.digest }]));
      if (active) setProfile(nextProfile);
    }
    if (active) setLoaded(true);
  };
  void untrack(refresh).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const toggle = async () => {
    if (pending() || !state()) return;
    setPending(true);
    setTrouble(undefined);
    try {
      value(
        await (state() === "active" ? environment.inactivate : environment.activate)(props.transport, [
          { requestId: crypto.randomUUID(), value: scope },
        ]),
      );
      await refresh();
    } catch (error) {
      if (active) setTrouble(String(error));
    } finally {
      if (active) setPending(false);
    }
  };
  const base = `projects/${encodeURIComponent(scope.project)}/${encodeURIComponent(scope.env)}`;
  const operation = () => profile()?.operations.find((entry) => entry.operation === props.params.operation);
  const reach = () => operation()?.reaches.find((entry) => entry.package === props.params.reach);
  const statement = () => reach()?.statements.find((entry) => entry.path === props.params.statement);
  const workload = () => status()?.workloads.find((entry) => entry.name === props.params.workload);
  const role = () => profile()?.roles.find((entry) => entry.role === props.params.role);
  const relation = () => statement()?.relations.find((entry) => entry.table === props.params.relation);
  const operationPath = `${base}/operations/${encodeURIComponent(props.params.operation ?? "")}`;
  const reachPath = `${operationPath}/reaches/${encodeURIComponent(props.params.reach ?? "")}`;
  const statementPath = `${reachPath}/statements/${encodeURIComponent(props.params.statement ?? "")}`;
  const relationPath = `${statementPath}/relations/${encodeURIComponent(props.params.relation ?? "")}`;
  const detail =
    props.params.workload ??
    props.params.role ??
    props.params.relation ??
    props.params.statement ??
    props.params.reach ??
    props.params.operation;
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show
        when={detail}
        fallback={
          <>
            <Show when={!props.table}>
              <Page
                title={`${scope.project}/${scope.env}`}
                actions={
                  <Show
                    when={
                      state() &&
                      props.holds(
                        state() === "active"
                          ? environment.ENVIRONMENT_INACTIVATE_ROUTE.operation
                          : environment.ENVIRONMENT_ACTIVATE_ROUTE.operation,
                      )
                    }
                  >
                    <Button disabled={pending()} onClick={() => void toggle()}>
                      {state() === "active" ? "Inactivate environment" : "Activate environment"}
                    </Button>
                  </Show>
                }
              >
                <Show when={state()}>
                  <p>State: {state()}</p>
                </Show>
                <Show when={status()}>
                  {(current) => (
                    <DashboardPanel role="region" aria-label="Environment status">
                      <h2>Status</h2>
                      <p>Release: {current().digest ?? "No release"}</p>
                      <FormGrid columns={2}>
                        <Field>
                          <Label>Replicas</Label>
                          <FieldValue value={current().rollout.replicas} />
                        </Field>
                        <Field>
                          <Label>Updated replicas</Label>
                          <FieldValue value={current().rollout.updatedReplicas} />
                        </Field>
                        <Field>
                          <Label>Ready replicas</Label>
                          <FieldValue value={current().rollout.readyReplicas} />
                        </Field>
                        <Field>
                          <Label>Available replicas</Label>
                          <FieldValue value={current().rollout.availableReplicas} />
                        </Field>
                      </FormGrid>
                    </DashboardPanel>
                  )}
                </Show>
                <Show when={profile()}>
                  {(current) => (
                    <DashboardPanel role="region" aria-label="Release profile">
                      <h2>Release profile</h2>
                      <p>{current().digest}</p>
                    </DashboardPanel>
                  )}
                </Show>
                <nav aria-label="Environment tables" class="ui-form-actions">
                  <For
                    each={[
                      { path: "workloads", label: "Workloads" },
                      { path: "live-releases", label: "Live releases" },
                      { path: "operations", label: "Operations" },
                      { path: "roles", label: "Roles" },
                    ]}
                  >
                    {(entry) => (
                      <RecordLink screen={props} path={`${base}/${entry.path}`}>
                        {entry.label}
                      </RecordLink>
                    )}
                  </For>
                </nav>
              </Page>
            </Show>
            <Show when={props.table === "workloads" && status()}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="workloads"
                  title="Workloads"
                  rows={status()?.workloads ?? []}
                  rowKey={(entry) => entry.name}
                  columns={[
                    { id: "name", header: "Workload", accessorKey: "name" },
                    {
                      id: "conditions",
                      header: "Conditions",
                      accessorFn: (entry) => entry.conditions.length,
                      size: 350,
                    },
                  ]}
                  cells={{
                    name: (cell) => (
                      <RecordLink screen={props} path={`${base}/workloads/${encodeURIComponent(cell.row.name)}`}>
                        {cell.row.name}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.table === "live-releases" && status()}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="live-releases"
                  title="Live releases"
                  rows={status()?.live ?? []}
                  rowKey={(entry) => entry.digest}
                  columns={[
                    { id: "digest", header: "Release", accessorKey: "digest", size: 150 },
                    { id: "pods", header: "Pods", accessorKey: "pods" },
                  ]}
                  formats={{ digest: "short-hash" }}
                />
              </div>
            </Show>
            <Show when={props.table === "operations" && (profile() || (status() && !status()?.digest))}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="operations"
                  title="Operations"
                  rows={profile()?.operations ?? []}
                  rowKey={(entry) => entry.operation}
                  columns={[
                    { id: "operation", header: "Operation", size: 400 },
                    { id: "package", header: "Package", accessorKey: "package", size: 250 },
                    { id: "component", header: "Component", accessorKey: "component", size: 250 },
                  ]}
                  cells={{
                    operation: (cell) => (
                      <RecordLink screen={props} path={`${base}/operations/${encodeURIComponent(cell.row.operation)}`}>
                        {cell.row.operation}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.table === "roles" && (profile() || (status() && !status()?.digest))}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="roles"
                  title="Roles"
                  rows={profile()?.roles ?? []}
                  rowKey={(entry) => entry.role}
                  columns={[
                    { id: "role", header: "Role" },
                    { id: "operations", header: "Operations", accessorFn: (entry) => entry.operations.length },
                  ]}
                  cells={{
                    role: (cell) => (
                      <RecordLink screen={props} path={`${base}/roles/${encodeURIComponent(cell.row.role)}`}>
                        {cell.row.role}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
          </>
        }
      >
        <Show when={loaded()}>
          <Show
            when={
              (!props.params.workload || workload()) &&
              (!props.params.role || role()) &&
              (!props.params.operation || operation()) &&
              (!props.params.reach || reach()) &&
              (!props.params.statement || statement()) &&
              (!props.params.relation || relation())
            }
            fallback={<MissingPage />}
          >
            <Page title={detail} />
            <Show when={!!props.params.workload && workload()}>
              {(current) => (
                <div class="platform-table-screen">
                  <Records
                    refresh={refresh}
                    id="conditions"
                    title={`Conditions for ${current().name}`}
                    rows={current().conditions}
                    rowKey={(entry) => entry.type}
                    columns={[
                      { id: "type", header: "Condition", accessorKey: "type" },
                      { id: "status", header: "State", accessorKey: "status" },
                    ]}
                  />
                </div>
              )}
            </Show>
            <Show when={!!props.params.role && role()}>
              {(current) => (
                <div class="platform-table-screen">
                  <Records
                    refresh={refresh}
                    id="role-operations"
                    title="Role operations"
                    rows={current().operations}
                    rowKey={(entry) => entry}
                    columns={[{ id: "operation", header: "Operation" }]}
                    cells={{
                      operation: (cell) => (
                        <RecordLink screen={props} path={`${base}/operations/${encodeURIComponent(cell.row)}`}>
                          {cell.row}
                        </RecordLink>
                      ),
                    }}
                  />
                </div>
              )}
            </Show>
            <Show when={props.params.operation && !props.params.reach}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="reaches"
                  title="Reaches"
                  rows={operation()!.reaches}
                  rowKey={(entry) => entry.package}
                  columns={[
                    { id: "package", header: "Package" },
                    {
                      id: "precision",
                      header: "Precision",
                      accessorFn: (entry) => (entry.imprecise ? "Imprecise" : "Precise"),
                    },
                    { id: "operations", header: "Operations" },
                    { id: "statements", header: "Statements" },
                  ]}
                  cells={{
                    package: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${operationPath}/reaches/${encodeURIComponent(cell.row.package)}`}
                      >
                        {cell.row.package}
                      </RecordLink>
                    ),
                    operations: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${operationPath}/reaches/${encodeURIComponent(cell.row.package)}/operations`}
                      >
                        {cell.row.operations.length}
                      </RecordLink>
                    ),
                    statements: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${operationPath}/reaches/${encodeURIComponent(cell.row.package)}`}
                      >
                        {cell.row.statements.length}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.params.reach && !props.params.statement && props.reachOperations}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="reach-operations"
                  title="Reach operations"
                  rows={reach()!.operations}
                  rowKey={(entry) => entry}
                  columns={[{ id: "operation", header: "Operation" }]}
                  cells={{
                    operation: (cell) => (
                      <RecordLink screen={props} path={`${base}/operations/${encodeURIComponent(cell.row)}`}>
                        {cell.row}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.params.reach && !props.params.statement && !props.reachOperations}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="statements"
                  title="Statements"
                  rows={reach()!.statements}
                  rowKey={(entry) => entry.path}
                  columns={[
                    { id: "path", header: "Statement", size: 300 },
                    { id: "refused", header: "Refusal", accessorKey: "refused", size: 300 },
                    { id: "relations", header: "Relations", accessorFn: (entry) => entry.relations.length },
                  ]}
                  cells={{
                    path: (cell) => (
                      <RecordLink screen={props} path={`${reachPath}/statements/${encodeURIComponent(cell.row.path)}`}>
                        {cell.row.path}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.params.statement && !props.params.relation}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="relations"
                  title="Relations"
                  rows={statement()!.relations}
                  rowKey={(entry) => entry.table}
                  columns={[
                    { id: "relation", header: "Relation" },
                    { id: "reads", header: "Read fields" },
                    { id: "writes", header: "Write fields" },
                    { id: "locks", header: "Locks", accessorKey: "locks" },
                    { id: "deletes", header: "Deletes", accessorKey: "deletes" },
                  ]}
                  formats={{ locks: "boolean-symbol", deletes: "boolean-symbol" }}
                  cells={{
                    relation: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${statementPath}/relations/${encodeURIComponent(cell.row.table)}`}
                      >
                        {cell.row.table}
                      </RecordLink>
                    ),
                    reads: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${statementPath}/relations/${encodeURIComponent(cell.row.table)}/reads`}
                      >
                        {cell.row.reads.length}
                      </RecordLink>
                    ),
                    writes: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`${statementPath}/relations/${encodeURIComponent(cell.row.table)}/writes`}
                      >
                        {cell.row.writes.length}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.params.relation && !props.fields}>
              <Page>
                <nav aria-label="Relation tables" class="ui-form-actions">
                  <RecordLink screen={props} path={`${relationPath}/reads`}>
                    Read fields
                  </RecordLink>
                  <RecordLink screen={props} path={`${relationPath}/writes`}>
                    Write fields
                  </RecordLink>
                </nav>
              </Page>
            </Show>
            <Show when={props.fields}>
              {(fields) => (
                <div class="platform-table-screen">
                  <Records
                    refresh={refresh}
                    id="relation-fields"
                    title={fields() === "reads" ? "Read fields" : "Write fields"}
                    rows={relation()![fields()]}
                    rowKey={(entry) => entry}
                    columns={[{ id: "field", header: "Field", accessorFn: (entry) => entry }]}
                  />
                </div>
              )}
            </Show>
          </Show>
        </Show>
      </Show>
    </Show>
  );
}

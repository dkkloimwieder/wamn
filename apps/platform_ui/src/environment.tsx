import { createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { environment, release } from "@wamn/control-org-client";
import { NotFound, Page } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { ButtonLink, Field, FieldValue, FormGrid, Label } from "@wfe/components";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export function Environment(
  props: ScreenProps & { table?: "workloads" | "live-releases" | "operations" | "roles" },
): JSX.Element {
  const scope = { project: props.params.project!, env: props.params.env! };
  const [state, setState] = createSignal<"active" | "inactive">();
  const [status, setStatus] = createSignal<environment.EnvironmentStatusResult>();
  const [profile, setProfile] = createSignal<release.ReleaseProfileResult>();
  const [trouble, setTrouble] = createSignal<string>();
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
    const entry = value(listed).environments.find((entry) => entry.name === scope.env);
    if (!entry) throw new Error("This environment is not an environment of the project.");
    if (active) setState(entry.state);
    const next = value(observed);
    if (active) {
      setStatus(next);
      setProfile(undefined);
    }
    if (next.digest) {
      const nextProfile = value(await release.profile(props.transport, [{ ...scope, digest: next.digest }]));
      if (active) setProfile(nextProfile);
    }
  };
  void refresh().catch((error: unknown) => {
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
  const detail = props.params.workload ?? props.params.statement ?? props.params.reach ?? props.params.operation;
  return (
    <>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show
        when={detail}
        fallback={
          <>
            <Show when={!props.table}>
              <Page title={`${scope.project}/${scope.env}`}>
                <Show when={state()}>
                  <p>Environment state: {state()}</p>
                  <Show
                    when={props.holds(
                      state() === "active"
                        ? environment.ENVIRONMENT_INACTIVATE_ROUTE.operation
                        : environment.ENVIRONMENT_ACTIVATE_ROUTE.operation,
                    )}
                  >
                    <Button disabled={pending()} onClick={() => void toggle()}>
                      {state() === "active" ? "Inactivate environment" : "Activate environment"}
                    </Button>
                  </Show>
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
                <nav aria-label="Environment tables">
                  <For
                    each={[
                      { path: "workloads", label: "Workloads" },
                      { path: "live-releases", label: "Live releases" },
                      { path: "operations", label: "Operations" },
                      { path: "roles", label: "Roles" },
                    ]}
                  >
                    {(entry) => (
                      <ButtonLink
                        href={`/${props.params.aud}/${base}/${entry.path}`}
                        onClick={(event) => {
                          if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
                            return;
                          event.preventDefault();
                          props.open(`${base}/${entry.path}`);
                        }}
                      >
                        {entry.label}
                      </ButtonLink>
                    )}
                  </For>
                </nav>
              </Page>
            </Show>
            <Show when={props.table === "workloads" && status()}>
              <div class="platform-table-screen">
                <Records
                  id="workloads"
                  title="Workloads"
                  rows={status()?.workloads ?? []}
                  rowKey={(entry) => entry.name}
                  columns={[
                    { id: "name", header: "Workload", accessorKey: "name" },
                    {
                      id: "conditions",
                      header: "Conditions",
                      accessorFn: (entry) => entry.conditions.map((condition) => condition.type).join(", "),
                      size: 350,
                    },
                  ]}
                  cells={{
                    name: (cell) => (
                      <Button onClick={() => props.open(`${base}/workloads/${encodeURIComponent(cell.row.name)}`)}>
                        {cell.row.name}
                      </Button>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.table === "live-releases" && status()}>
              <div class="platform-table-screen">
                <Records
                  id="live-releases"
                  title="Live releases"
                  rows={status()?.live ?? []}
                  rowKey={(entry) => entry.digest}
                  columns={[
                    { id: "digest", header: "Release", accessorKey: "digest", size: 150 },
                    { id: "pods", header: "Pods" },
                  ]}
                  formats={{ digest: "short-hash" }}
                  cells={{ pods: (cell) => <>{cell.row.pods} pods</> }}
                />
              </div>
            </Show>
            <Show when={props.table === "operations" && (profile() || (status() && !status()?.digest))}>
              <div class="platform-table-screen">
                <Records
                  id="operations"
                  title="Operations"
                  rows={profile()?.operations ?? []}
                  rowKey={(entry) => entry.operation}
                  columns={[{ id: "operation", header: "Operation", size: 950 }]}
                  cells={{
                    operation: (cell) => (
                      <Button
                        onClick={() => props.open(`${base}/operations/${encodeURIComponent(cell.row.operation)}`)}
                      >
                        {cell.row.operation} ({cell.row.package}/{cell.row.component})
                      </Button>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.table === "roles" && (profile() || (status() && !status()?.digest))}>
              <div class="platform-table-screen">
                <Records
                  id="roles"
                  title="Roles"
                  rows={profile()?.roles ?? []}
                  rowKey={(entry) => entry.role}
                  columns={[{ id: "role", header: "Role and operations", size: 950 }]}
                  cells={{
                    role: (cell) => (
                      <>
                        {cell.row.role}: {cell.row.operations.join(", ") || "No operations"}
                      </>
                    ),
                  }}
                />
              </div>
            </Show>
          </>
        }
      >
        <Page title={detail} />
        <Show when={props.params.workload}>
          <Show when={status()}>
            <Show when={workload()} fallback={<NotFound />}>
              {(current) => (
                <div class="platform-table-screen">
                  <Records
                    id="conditions"
                    title={`Conditions for ${current().name}`}
                    rows={current().conditions}
                    rowKey={(entry) => entry.type}
                    columns={[
                      { id: "type", header: "Condition", accessorKey: "type" },
                      { id: "status", header: "Status", accessorKey: "status" },
                    ]}
                  />
                </div>
              )}
            </Show>
          </Show>
        </Show>
        <Show when={props.params.operation}>
          <Show when={profile() || (status() && !status()?.digest)}>
            <Show
              when={operation() && (!props.params.reach || reach()) && (!props.params.statement || statement())}
              fallback={<NotFound />}
            >
              <div class="platform-table-screen">
                <Show
                  when={props.params.reach}
                  fallback={
                    <Records
                      id="reaches"
                      title="Reaches"
                      rows={operation()!.reaches}
                      rowKey={(entry) => entry.package}
                      columns={[
                        { id: "reach", header: "Reach", size: 250 },
                        {
                          id: "statements",
                          header: "Statements",
                          accessorFn: (entry) => entry.statements.map((statement) => statement.path).join(", "),
                          size: 650,
                        },
                      ]}
                      cells={{
                        reach: (cell) => (
                          <Button
                            onClick={() =>
                              props.open(
                                `${base}/operations/${encodeURIComponent(props.params.operation!)}/reaches/${encodeURIComponent(cell.row.package)}`,
                              )
                            }
                          >
                            {cell.row.package}: {cell.row.operations.join(", ")}
                            {cell.row.imprecise ? " (imprecise)" : ""}
                          </Button>
                        ),
                      }}
                    />
                  }
                >
                  <Show
                    when={props.params.statement}
                    fallback={
                      <Records
                        id="statements"
                        title="Statements"
                        rows={reach()!.statements}
                        rowKey={(entry) => entry.path}
                        columns={[
                          { id: "path", header: "Statement", size: 200 },
                          {
                            id: "relations",
                            header: "Relations",
                            accessorFn: (entry) => entry.relations.map((relation) => relation.table).join(", "),
                            size: 450,
                          },
                        ]}
                        cells={{
                          path: (cell) => (
                            <Button
                              onClick={() =>
                                props.open(
                                  `${base}/operations/${encodeURIComponent(props.params.operation!)}/reaches/${encodeURIComponent(props.params.reach!)}/statements/${encodeURIComponent(cell.row.path)}`,
                                )
                              }
                            >
                              {cell.row.path}
                              {cell.row.refused ? `: ${cell.row.refused}` : ""}
                            </Button>
                          ),
                        }}
                      />
                    }
                  >
                    <Records
                      id="relations"
                      title="Relations"
                      rows={statement()!.relations}
                      rowKey={(entry) => entry.table}
                      columns={[{ id: "relation", header: "Relation", size: 430 }]}
                      cells={{
                        relation: (cell) => (
                          <>
                            {cell.row.table}: reads {cell.row.reads.join(", ") || "none"}; writes{" "}
                            {cell.row.writes.join(", ") || "none"}; locks {cell.row.locks ? "yes" : "no"}; deletes{" "}
                            {cell.row.deletes ? "yes" : "no"}
                          </>
                        ),
                      }}
                    />
                  </Show>
                </Show>
              </div>
            </Show>
          </Show>
        </Show>
      </Show>
    </>
  );
}

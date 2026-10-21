import { createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { environment, release } from "@wamn/control-org-client";
import { Page } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { Field, FieldValue, FormGrid, Label } from "@wfe/components";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export function Environment(props: ScreenProps): JSX.Element {
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
  return (
    <Page title={`${scope.project}/${scope.env}`}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
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
            <h3>Workloads</h3>
            <Records
              id="workloads"
              title="Workloads"
              rows={current().workloads}
              rowKey={(workload) => workload.name}
              columns={[
                { id: "name", header: "Workload", accessorKey: "name" },
                {
                  id: "conditions",
                  header: "Conditions",
                  accessorFn: (workload) => workload.conditions.map((condition) => condition.type).join(", "),
                  size: 350,
                },
              ]}
              details={(cell) => (
                <Records
                  id="conditions"
                  title={`Conditions for ${cell.row.name}`}
                  rows={cell.row.conditions}
                  rowKey={(condition) => condition.type}
                  columns={[
                    { id: "type", header: "Condition", accessorKey: "type" },
                    { id: "status", header: "Status", accessorKey: "status" },
                  ]}
                />
              )}
            />
            <h3>Live releases</h3>
            <Records
              id="live-releases"
              title="Live releases"
              rows={current().live}
              rowKey={(live) => live.digest}
              columns={[
                { id: "digest", header: "Release", accessorKey: "digest", size: 150 },
                { id: "pods", header: "Pods" },
              ]}
              formats={{ digest: "short-hash" }}
              cells={{ pods: (cell) => <>{cell.row.pods} pods</> }}
            />
          </DashboardPanel>
        )}
      </Show>
      <Show when={profile()}>
        {(current) => (
          <DashboardPanel role="region" aria-label="Release profile">
            <h2>Release profile</h2>
            <p>{current().digest}</p>
            <h3>Operations</h3>
            <Records
              id="operations"
              title="Operations"
              rows={current().operations}
              rowKey={(operation) => operation.operation}
              columns={[{ id: "operation", header: "Operation", size: 950 }]}
              cells={{
                operation: (cell) => (
                  <>
                    {cell.row.operation} ({cell.row.package}/{cell.row.component})
                  </>
                ),
              }}
              details={(cell) => (
                <Records
                  id="reaches"
                  title="Reaches"
                  rows={cell.row.reaches}
                  rowKey={(reach) => reach.package}
                  columns={[
                    { id: "reach", header: "Reach", size: 250 },
                    {
                      id: "statements",
                      header: "Statements",
                      accessorFn: (reach) => reach.statements.map((statement) => statement.path).join(", "),
                      size: 650,
                    },
                  ]}
                  cells={{
                    reach: (reach) => (
                      <>
                        {reach.row.package}: {reach.row.operations.join(", ")}
                        {reach.row.imprecise ? " (imprecise)" : ""}
                      </>
                    ),
                  }}
                  details={(reach) => (
                    <Records
                      id="statements"
                      title="Statements"
                      rows={reach.row.statements}
                      rowKey={(statement) => statement.path}
                      columns={[
                        { id: "path", header: "Statement", size: 200 },
                        {
                          id: "relations",
                          header: "Relations",
                          accessorFn: (statement) => statement.relations.map((relation) => relation.table).join(", "),
                          size: 450,
                        },
                      ]}
                      cells={{
                        path: (statement) => (
                          <>
                            {statement.row.path}
                            {statement.row.refused ? `: ${statement.row.refused}` : ""}
                          </>
                        ),
                      }}
                      details={(statement) => (
                        <Records
                          id="relations"
                          title="Relations"
                          rows={statement.row.relations}
                          rowKey={(relation) => relation.table}
                          columns={[{ id: "relation", header: "Relation", size: 430 }]}
                          cells={{
                            relation: (relation) => (
                              <>
                                {relation.row.table}: reads {relation.row.reads.join(", ") || "none"}; writes{" "}
                                {relation.row.writes.join(", ") || "none"}; locks {relation.row.locks ? "yes" : "no"};
                                deletes {relation.row.deletes ? "yes" : "no"}
                              </>
                            ),
                          }}
                        />
                      )}
                    />
                  )}
                />
              )}
            />
            <h3>Roles</h3>
            <Records
              id="roles"
              title="Roles"
              rows={current().roles}
              rowKey={(role) => role.role}
              columns={[{ id: "role", header: "Role and operations", size: 950 }]}
              cells={{
                role: (cell) => (
                  <>
                    {cell.row.role}: {cell.row.operations.join(", ") || "No operations"}
                  </>
                ),
              }}
            />
          </DashboardPanel>
        )}
      </Show>
    </Page>
  );
}

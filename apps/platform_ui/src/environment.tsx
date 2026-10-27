import { createSignal, onCleanup, Show, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { environment, release } from "./generated/wamn_control_org/index.js";
import { MissingPage, Page } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { EnvironmentTiles } from "./status-tiles";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export function Environment(
  props: ScreenProps & {
    table?: "workloads" | "live-releases" | "operations" | "roles";
  },
): JSX.Element {
  const scope = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
  const [state, setState] = createSignal<"active" | "inactive">();
  const [status, setStatus] = createSignal<environment.EnvironmentStatusResult>();
  const [profile, setProfile] = createSignal<release.ReleaseProfileResult>();
  const [trouble, setTrouble] = createSignal<string>();
  const [missing, setMissing] = createSignal(false);
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
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
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
          <EnvironmentTiles status={status()} />
          <Show when={profile()}>
            {(current) => (
              <DashboardPanel role="region" aria-label="Release profile">
                <h2>Release profile</h2>
                <p>{current().digest}</p>
              </DashboardPanel>
            )}
          </Show>
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
            details={(cell) => (
              <Records
                refresh={refresh}
                id="conditions"
                title={`Conditions for ${cell.row.name}`}
                rows={cell.row.conditions}
                rowKey={(entry) => entry.type}
                columns={[
                  { id: "type", header: "Condition", accessorKey: "type" },
                  { id: "status", header: "State", accessorKey: "status" },
                ]}
              />
            )}
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
              { id: "operation", header: "Operation", accessorKey: "operation", size: 400 },
              { id: "package", header: "Package", accessorKey: "package", size: 250 },
              { id: "component", header: "Component", accessorKey: "component", size: 250 },
            ]}
            details={(cell) => (
              <Records
                refresh={refresh}
                id="reaches"
                title="Reaches"
                rows={cell.row.reaches}
                rowKey={(entry) => entry.package}
                columns={[
                  { id: "package", header: "Package", accessorKey: "package" },
                  {
                    id: "precision",
                    header: "Precision",
                    accessorFn: (entry) => (entry.imprecise ? "Imprecise" : "Precise"),
                  },
                  { id: "operations", header: "Operations", accessorFn: (entry) => entry.operations.length },
                  { id: "statements", header: "Statements", accessorFn: (entry) => entry.statements.length },
                ]}
                details={(reach) => (
                  <>
                    <Records
                      refresh={refresh}
                      id="reach-operations"
                      title="Reach operations"
                      rows={reach.row.operations}
                      rowKey={(entry) => entry}
                      columns={[{ id: "operation", header: "Operation", accessorFn: (entry) => entry }]}
                    />
                    <Records
                      refresh={refresh}
                      id="statements"
                      title="Statements"
                      rows={reach.row.statements}
                      rowKey={(entry) => entry.path}
                      columns={[
                        { id: "path", header: "Statement", accessorKey: "path", size: 300 },
                        { id: "refused", header: "Refusal", accessorKey: "refused", size: 300 },
                        { id: "relations", header: "Relations", accessorFn: (entry) => entry.relations.length },
                      ]}
                      details={(statement) => (
                        <Records
                          refresh={refresh}
                          id="relations"
                          title="Relations"
                          rows={statement.row.relations}
                          rowKey={(entry) => entry.table}
                          columns={[
                            { id: "table", header: "Relation", accessorKey: "table" },
                            { id: "reads", header: "Read fields", accessorFn: (entry) => entry.reads.length },
                            { id: "writes", header: "Write fields", accessorFn: (entry) => entry.writes.length },
                            { id: "locks", header: "Locks", accessorKey: "locks" },
                            { id: "deletes", header: "Deletes", accessorKey: "deletes" },
                          ]}
                          formats={{ locks: "boolean-symbol", deletes: "boolean-symbol" }}
                          details={(relation) => (
                            <>
                              <Records
                                refresh={refresh}
                                id="read-fields"
                                title="Read fields"
                                rows={relation.row.reads}
                                rowKey={(entry) => entry}
                                columns={[{ id: "field", header: "Field", accessorFn: (entry) => entry }]}
                              />
                              <Records
                                refresh={refresh}
                                id="write-fields"
                                title="Write fields"
                                rows={relation.row.writes}
                                rowKey={(entry) => entry}
                                columns={[{ id: "field", header: "Field", accessorFn: (entry) => entry }]}
                              />
                            </>
                          )}
                        />
                      )}
                    />
                  </>
                )}
              />
            )}
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
              { id: "role", header: "Role", accessorKey: "role" },
              { id: "operations", header: "Operations", accessorFn: (entry) => entry.operations.length },
            ]}
            details={(cell) => (
              <Records
                refresh={refresh}
                id="role-operations"
                title="Role operations"
                rows={cell.row.operations}
                rowKey={(entry) => entry}
                columns={[{ id: "operation", header: "Operation", accessorFn: (entry) => entry }]}
              />
            )}
          />
        </div>
      </Show>
    </Show>
  );
}

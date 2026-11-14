import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { AlertDialog } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView, type FormDescription } from "@wfe/generator";
import { application, environment, operation, release, schema } from "./generated/wamn_control_org/index.js";
import { MissingPage, Page } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { EnvironmentTiles } from "./status-tiles";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

const operationActions = {
  deploy: { label: "Deploy release", route: application.APPLICATION_DEPLOY_ROUTE, effect: "" },
  rollback: {
    label: "Rollback application",
    route: application.APPLICATION_ROLLBACK_ROUTE,
    effect: "Replace the running application code with the selected or previous release. SQL is not reversed.",
  },
  diff: { label: "Diff schema", route: schema.SCHEMA_DIFF_ROUTE, effect: "" },
  apply: {
    label: "Apply schema",
    route: schema.SCHEMA_APPLY_ROUTE,
    effect: "Apply SQL changes to this application's database tables. Changes may remove data.",
  },
  reset: {
    label: "Reset schema",
    route: schema.SCHEMA_RESET_ROUTE,
    effect:
      "Stop all applications, clear their data, runs, queued events and authored workflows, then reapply migrations.",
  },
};

function OperationControls(props: ScreenProps) {
  const config = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
  const [action, setAction] = createSignal<keyof typeof operationActions>();
  const [observed, setObserved] = createSignal<operation.OperationStatusResult>();
  const [busy, setBusy] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => {
    controller.abort();
    clearTimeout(timer);
  });
  const poll = async (job: string) => {
    try {
      const next = value(await operation.status(props.transport, [{ ...config, job }], { signal: controller.signal }));
      if (controller.signal.aborted) return;
      setObserved(next);
      if (next.state === "pending" || next.state === "running") {
        timer = setTimeout(() => void poll(job), 1000);
      } else {
        setBusy(false);
      }
    } catch (error) {
      if (!controller.signal.aborted) {
        setTrouble(String(error));
        setBusy(false);
      }
    }
  };
  return (
    <Show when={Object.values(operationActions).some((entry) => props.holds(entry.route.operation, config.project))}>
      <DashboardPanel role="region" aria-label="Deployment controls">
        <h2>Application and schema operations</h2>
        <For each={Object.entries(operationActions)}>
          {([key, entry]) => (
            <Show when={props.holds(entry.route.operation, config.project)}>
              <Button disabled={busy()} onClick={() => setAction(key as keyof typeof operationActions)}>
                {entry.label}
              </Button>
            </Show>
          )}
        </For>
        <Show when={action()} keyed>
          {(selected) => {
            const entry = operationActions[selected];
            const defaults = { application: "", release: "" };
            const [values, setValues] = createSignal(defaults);
            const [confirmation, setConfirmation] = createSignal<typeof defaults>();
            const needsApplication = selected === "rollback" || selected === "diff" || selected === "apply";
            const needsRelease = selected === "deploy" || needsApplication;
            const start = async (snapshot: typeof defaults) => {
              if (busy() || controller.signal.aborted || !props.holds(entry.route.operation, config.project)) return;
              setBusy(true);
              setTrouble(undefined);
              setObserved(undefined);
              setConfirmation(undefined);
              const requestId = crypto.randomUUID();
              const options = { signal: controller.signal };
              const target = {
                ...config,
                application: snapshot.application.trim(),
                release: snapshot.release.trim() || null,
              };
              try {
                const result = value(
                  await (selected === "deploy"
                    ? application.deploy(
                        props.transport,
                        [{ requestId, value: { ...config, release: snapshot.release.trim() } }],
                        options,
                      )
                    : selected === "rollback"
                      ? application.rollback(props.transport, [{ requestId, value: target }], options)
                      : selected === "diff"
                        ? schema.diff(props.transport, [{ requestId, value: target }], options)
                        : selected === "apply"
                          ? schema.apply(props.transport, [{ requestId, value: target }], options)
                          : schema.reset(props.transport, [{ requestId, value: config }], options)),
                );
                if (controller.signal.aborted) return;
                setObserved({ job: result.job, state: "pending", log: "" });
                void poll(result.job);
              } catch (error) {
                if (!controller.signal.aborted) {
                  setTrouble(String(error));
                  setBusy(false);
                }
              }
            };
            const form = createForm({
              values,
              defaults: () => defaults,
              onValuesChange: setValues,
              validate: (snapshot) =>
                needsApplication && !snapshot.application.trim()
                  ? { form: "Enter an application." }
                  : selected === "deploy" && !snapshot.release.trim()
                    ? { form: "Enter a release digest." }
                    : {},
              onSubmit: async (snapshot) => {
                if (entry.effect) setConfirmation(snapshot);
                else await start(snapshot);
                return { ok: true };
              },
            });
            return (
              <>
                <p>
                  Project: {config.project}; environment: {config.env}
                </p>
                <FormView
                  description={
                    {
                      id: "environment-operation",
                      title: entry.label,
                      fields: [
                        ...(needsApplication
                          ? [{ field: "application", label: "Application", control: "text" } as const]
                          : []),
                        ...(needsRelease
                          ? [
                              {
                                field: "release",
                                label: selected === "deploy" ? "Release digest" : "Release digest (optional)",
                                control: "text",
                              } as const,
                            ]
                          : []),
                      ],
                      submit: { id: "start", label: `Start ${entry.label.toLowerCase()}` },
                    } satisfies FormDescription<typeof defaults>
                  }
                  bindings={{ form, submitDisabled: busy }}
                />
                <AlertDialog
                  open={!!confirmation()}
                  onOpenChange={(open) => {
                    if (!open) setConfirmation(undefined);
                  }}
                >
                  <AlertDialog.Portal>
                    <AlertDialog.Overlay />
                    <AlertDialog.Content>
                      <AlertDialog.Title>Confirm {entry.label.toLowerCase()}?</AlertDialog.Title>
                      <AlertDialog.Description>
                        Project: {config.project}; environment: {config.env}; application:{" "}
                        {needsApplication ? confirmation()?.application : "all applications"}.
                        {needsRelease && ` Release: ${confirmation()?.release || "configured or previous release"}. `}
                        {entry.effect}
                      </AlertDialog.Description>
                      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
                      <AlertDialog.Action
                        onClick={() => {
                          const snapshot = confirmation();
                          if (snapshot) void start(snapshot);
                        }}
                      >
                        Confirm {entry.label.toLowerCase()}
                      </AlertDialog.Action>
                    </AlertDialog.Content>
                  </AlertDialog.Portal>
                </AlertDialog>
              </>
            );
          }}
        </Show>
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Show when={observed()}>
          {(current) => (
            <div role="region" aria-label="Operation status">
              <p>Job: {current().job}</p>
              <p role="status">State: {current().state}</p>
              <pre aria-label="Log tail">{current().log}</pre>
            </div>
          )}
        </Show>
      </DashboardPanel>
    </Show>
  );
}

export function Environment(
  props: ScreenProps & {
    table?: "workloads" | "live-releases" | "operations" | "roles";
  },
): JSX.Element {
  const config = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
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
      environment.list(props.transport, [{ project: config.project }]),
      environment.status(props.transport, [config]),
    ]);
    if (observed.status === "refused" && observed.code === "environment_not_found") {
      if (active) setMissing(true);
      return;
    }
    const entry = value(listed).environments.find((entry) => entry.name === config.env);
    if (active) setMissing(!entry);
    if (!entry) return;
    if (active) setState(entry.state);
    const next = value(observed);
    if (active) {
      setStatus(next);
      if (!next.digest) setProfile(undefined);
    }
    if (next.digest) {
      const nextProfile = value(await release.profile(props.transport, [{ ...config, digest: next.digest }]));
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
          { requestId: crypto.randomUUID(), value: config },
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
          title={`${config.project}/${config.env}`}
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
          <OperationControls {...props} />
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

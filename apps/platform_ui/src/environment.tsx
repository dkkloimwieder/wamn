import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { AlertDialog } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView, type FormDescription } from "@wfe/generator";
import {
  application,
  environment,
  operation,
  schema,
  type EnvironmentStatusResult,
  type OperationStatusResult,
} from "./control-client";
import { MissingPage, Page } from "./page";
import { DashboardPanel } from "@wfe/components/layout";
import { EnvironmentTiles } from "./status-tiles";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

const operationActions = {
  deploy: { label: "Deploy release", route: application.deploy.route, effect: "" },
  stop: { label: "Stop application", route: application.stop.route, effect: "" },
  rollback: {
    label: "Rollback application",
    route: application.rollback.route,
    effect: "Replace the running application code with the selected or previous release. SQL is not reversed.",
  },
  diff: { label: "Diff schema", route: schema.diff.route, effect: "" },
  apply: {
    label: "Apply schema",
    route: schema.apply.route,
    effect: "Apply SQL changes to this application's database tables. Changes may remove data.",
  },
  reset: {
    label: "Reset schema",
    route: schema.reset.route,
    effect:
      "Stop all applications, clear their data, runs, queued events and authored workflows, then reapply migrations.",
  },
};

function OperationControls(props: ScreenProps) {
  const config = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
  const [action, setAction] = createSignal<keyof typeof operationActions>();
  const [observed, setObserved] = createSignal<OperationStatusResult>();
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
            const defaults = { application: "", release: "", revision: "" };
            const [values, setValues] = createSignal(defaults);
            const [confirmation, setConfirmation] = createSignal<typeof defaults>();
            const needsApplication = selected !== "reset";
            const needsRevision = selected === "rollback";
            const needsRelease = selected !== "stop" && selected !== "reset" && !needsRevision;
            const requiresRelease = selected === "deploy" || selected === "apply";
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
                        [{ request_id: requestId, value: { ...target, release: snapshot.release.trim() } }],
                        options,
                      )
                    : selected === "stop"
                      ? application.stop(
                          props.transport,
                          [{ request_id: requestId, value: { ...config, application: target.application } }],
                          options,
                        )
                      : selected === "rollback"
                        ? application.rollback(
                            props.transport,
                            [
                              {
                                request_id: requestId,
                                value: {
                                  ...config,
                                  application: target.application,
                                  revision: snapshot.revision.trim() ? Number(snapshot.revision) : null,
                                },
                              },
                            ],
                            options,
                          )
                        : selected === "diff"
                          ? schema.diff(props.transport, [{ request_id: requestId, value: target }], options)
                          : selected === "apply"
                            ? schema.apply(
                                props.transport,
                                [{ request_id: requestId, value: { ...target, release: snapshot.release.trim() } }],
                                options,
                              )
                            : schema.reset(props.transport, [{ request_id: requestId, value: config }], options)),
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
              validate: (snapshot) => {
                if (needsApplication && !snapshot.application.trim()) return { form: "Enter an application." };
                if (requiresRelease && !snapshot.release.trim()) return { form: "Enter a release digest." };
                const revision = snapshot.revision.trim();
                if (
                  needsRevision &&
                  revision &&
                  (!/^\d+$/.test(revision) || Number(revision) < 1 || Number(revision) > 2_147_483_647)
                ) {
                  return { form: "Enter a positive revision up to 2147483647." };
                }
                return {};
              },
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
                                label: requiresRelease ? "Release digest" : "Release digest (optional)",
                                control: "text",
                              } as const,
                            ]
                          : []),
                        ...(needsRevision
                          ? [{ field: "revision", label: "Revision (optional)", control: "text" } as const]
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
                        {needsRelease && ` Release: ${confirmation()?.release || "configured release"}. `}
                        {needsRevision && ` Revision: ${confirmation()?.revision || "previous revision"}. `}
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
    table?: "workloads" | "live-releases";
  },
): JSX.Element {
  const config = untrack(() => ({ project: props.params.project!, env: props.params.env! }));
  const [state, setState] = createSignal<"active" | "inactive">();
  const [status, setStatus] = createSignal<EnvironmentStatusResult>();
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
    if (!active) return;
    setStatus(next);
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
          { request_id: crypto.randomUUID(), value: config },
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
                  state() === "active" ? environment.inactivate.route.operation : environment.activate.route.operation,
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
            rows={status()?.workloads ?? []}
            rowKey={(entry) => entry.name}
            columns={[
              { id: "application", header: "Application", accessorKey: "application" },
              { id: "package_digest", header: "Package digest", accessorKey: "package_digest", size: 150 },
              { id: "revision", header: "Revision", accessorKey: "revision" },
            ]}
            formats={{ package_digest: "short-hash" }}
          />
        </div>
      </Show>
    </Show>
  );
}

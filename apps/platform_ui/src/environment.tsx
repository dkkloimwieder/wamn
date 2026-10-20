import { createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { environment, release } from "@wamn/control-org-client";
import { Page } from "./page";
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
          <section aria-label="Environment status">
            <h2>Status</h2>
            <p>Release: {current().digest ?? "No release"}</p>
            <dl>
              <dt>Replicas</dt>
              <dd>{current().rollout.replicas}</dd>
              <dt>Updated replicas</dt>
              <dd>{current().rollout.updatedReplicas}</dd>
              <dt>Ready replicas</dt>
              <dd>{current().rollout.readyReplicas}</dd>
              <dt>Available replicas</dt>
              <dd>{current().rollout.availableReplicas}</dd>
            </dl>
            <h3>Workloads</h3>
            <ul aria-label="Workloads">
              <For each={current().workloads}>
                {(workload) => (
                  <li>
                    {workload.name}
                    <ul>
                      <For each={workload.conditions}>
                        {(condition) => (
                          <li>
                            {condition.type}: {condition.status}
                          </li>
                        )}
                      </For>
                    </ul>
                  </li>
                )}
              </For>
            </ul>
            <h3>Live releases</h3>
            <ul aria-label="Live releases">
              <For each={current().live}>
                {(live) => (
                  <li>
                    {live.digest}: {live.pods} pods
                  </li>
                )}
              </For>
            </ul>
          </section>
        )}
      </Show>
      <Show when={profile()}>
        {(current) => (
          <section aria-label="Release profile">
            <h2>Release profile</h2>
            <p>{current().digest}</p>
            <h3>Operations</h3>
            <ul aria-label="Operations">
              <For each={current().operations}>
                {(operation) => (
                  <li>
                    <details>
                      <summary>
                        {operation.operation} ({operation.package}/{operation.component})
                      </summary>
                      <For each={operation.reaches}>
                        {(reach) => (
                          <div>
                            <p>
                              {reach.package}: {reach.operations.join(", ")}
                              {reach.imprecise ? " (imprecise)" : ""}
                            </p>
                            <For each={reach.statements}>
                              {(statement) => (
                                <div>
                                  <p>
                                    {statement.path}
                                    {statement.refused ? `: ${statement.refused}` : ""}
                                  </p>
                                  <ul>
                                    <For each={statement.relations}>
                                      {(relation) => (
                                        <li>
                                          {relation.table}: reads {relation.reads.join(", ") || "none"}; writes{" "}
                                          {relation.writes.join(", ") || "none"}; locks {relation.locks ? "yes" : "no"};
                                          deletes {relation.deletes ? "yes" : "no"}
                                        </li>
                                      )}
                                    </For>
                                  </ul>
                                </div>
                              )}
                            </For>
                          </div>
                        )}
                      </For>
                    </details>
                  </li>
                )}
              </For>
            </ul>
            <h3>Roles</h3>
            <ul aria-label="Roles">
              <For each={current().roles}>
                {(role) => (
                  <li>
                    {role.role}: {role.operations.join(", ") || "No operations"}
                  </li>
                )}
              </For>
            </ul>
          </section>
        )}
      </Show>
    </Page>
  );
}

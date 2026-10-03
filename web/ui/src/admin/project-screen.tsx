/**
 * The project screen of Control (docs/plan/platform-ui.md §4.5, issue 8).
 *
 * It reads `member.list` and `environment.list` of one project. Each member
 * has one membership toggle per environment, which calls `member.grant` or
 * `member.revoke`, and one `project-admin` toggle, which calls
 * `project_admin.grant` or `project_admin.revoke`. A membership that
 * `org-admin` or `project-admin` covers, and a `project-admin` that
 * `org-admin` covers, is hierarchy-controlled, with no toggle. A new member is
 * chosen from org `user.list`, which a `project-admin` may read.
 *
 * Each write announces its outcome, a refusal shows the text of its contract
 * and the environments a partial write completed, and the screen reads the
 * members again after each write. It writes no database directly.
 *
 * Below the members, a table shows each create-environment saga of the
 * project from `environment.list`, with its steps and the commands that its
 * `awaiting operator` step records (§5.2, issue 10). A failed saga has a
 * resume button, which calls `environment.resume`. A failed or pending saga
 * has an abandon button, which the operator confirms first and which calls
 * `environment.abandon` (issue 11).
 */

import { environment, member, projectAdmin, user } from "@wamn/control-org-client";
import { type JsonValue, newRequestId, type Outcome, type Transport } from "@wamn/web-runtime";
import { createResource, createSignal, createUniqueId, For, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { Switch } from "../components/ui/switch";
import { ConfirmAction } from "../confirm";
import { ChoiceField } from "../fields";
import { announceOutcome } from "../outcome";
import { controlRefusal } from "./refusal";

export interface ProjectScreenProps {
  readonly transport: Transport;
  readonly project: string;
}

/** The text of a control that a higher grant decides. */
function Covered(): JSX.Element {
  return <span class="text-muted-foreground text-xs">hierarchy-controlled</span>;
}

/** One group of operator commands in the detail of the `awaiting operator` step. */
interface OperatorCommands {
  readonly purpose: string;
  readonly runbook: string;
  readonly commands: readonly string[];
}

/** The command groups of a step detail, or none. */
function operatorCommands(detail: JsonValue | null): readonly OperatorCommands[] {
  if (detail === null || typeof detail !== "object" || Array.isArray(detail)) {
    return [];
  }
  const groups = (detail as { readonly [key: string]: JsonValue })["commands"];
  return Array.isArray(groups) ? (groups as unknown as OperatorCommands[]) : [];
}

/** One switch with a label that only a screen reader reads. */
function Toggle(props: { label: string; checked: boolean; onChange: (on: boolean) => void }): JSX.Element {
  const id = createUniqueId();
  return (
    <>
      <Switch id={id} size="sm" checked={props.checked} onChange={(on: boolean) => props.onChange(on)} />
      <label for={id} class="sr-only">
        {props.label}
      </label>
    </>
  );
}

export function ProjectScreen(props: ProjectScreenProps): JSX.Element {
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [members, { refetch }] = createResource(
    () => props.project,
    async (project) => {
      const outcome = await member.list(props.transport, [{ project }]);
      return outcome.status === "completed" ? outcome.value.members : [];
    },
  );
  const [listed, { refetch: refetchListed }] = createResource(
    () => props.project,
    async (project) => {
      const outcome = await environment.list(props.transport, [{ project }]);
      return outcome.status === "completed" ? outcome.value : { environments: [], sagas: [] };
    },
  );
  const environments = () => listed()?.environments;
  const [users] = createResource(async () => {
    const outcome = await user.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.users : [];
  });
  /** The active members of the org that the project does not list yet. */
  const candidates = () =>
    (users() ?? []).filter(
      (each) =>
        each.status === "active" && !(members() ?? []).some((listed) => listed.principalId === each.principalId),
    );
  const [newMember, setNewMember] = createSignal<string | null>(null);
  const [newEnvironment, setNewEnvironment] = createSignal<string | null>(null);

  /** Announces one write, shows a refusal's text, and reads the members again. */
  const settle = async <T,>(outcome: Outcome<T>, screen: string) => {
    announceOutcome(outcome, screen);
    setRefusal(outcome.status === "refused" ? controlRefusal(outcome) : null);
    await refetch();
  };

  const setMembership = async (principalId: string, env: string, on: boolean) => {
    const item = { requestId: newRequestId(), value: { project: props.project, env, principalId } };
    if (on) {
      await settle(await member.grant(props.transport, [item]), "membership grant");
    } else {
      await settle(await member.revoke(props.transport, [item]), "membership revoke");
    }
  };

  const setProjectAdmin = async (principalId: string, on: boolean) => {
    const item = { requestId: newRequestId(), value: { project: props.project, principalId } };
    if (on) {
      await settle(await projectAdmin.grant(props.transport, [item]), "project-admin grant");
    } else {
      await settle(await projectAdmin.revoke(props.transport, [item]), "project-admin revoke");
    }
  };

  /** Resumes a failed saga, or abandons a failed or pending one, and reads the sagas again. */
  const endFailure = async (sagaId: string, resume: boolean) => {
    const item = [{ requestId: newRequestId(), value: { sagaId } }];
    const outcome = resume
      ? await environment.resume(props.transport, item)
      : await environment.abandon(props.transport, item);
    announceOutcome(outcome, resume ? "saga resume" : "saga abandon");
    setRefusal(outcome.status === "refused" ? controlRefusal(outcome) : null);
    await refetchListed();
  };

  const add = async () => {
    const principalId = newMember();
    const env = newEnvironment();
    if (principalId === null || env === null) {
      return;
    }
    await setMembership(principalId, env, true);
    setNewMember(null);
  };

  return (
    <div class="flex flex-col gap-6">
      <Show when={refusal()}>{(text) => <p role="alert">{text()}</p>}</Show>
      <section class="flex flex-col gap-2">
        <p class="text-sm font-semibold uppercase">members of {props.project}</p>
        <table class="w-full text-sm" data-slot="project-members">
          <thead>
            <tr class="text-left">
              <th>email</th>
              <th>name</th>
              <For each={environments() ?? []}>{(env) => <th>{env}</th>}</For>
              <th>project-admin</th>
            </tr>
          </thead>
          <tbody>
            <For each={members() ?? []}>
              {(listed) => (
                <tr data-member={listed.email}>
                  <td>{listed.email}</td>
                  <td>{listed.displayName}</td>
                  <For each={environments() ?? []}>
                    {(env) => (
                      <td>
                        <Show when={!listed.orgAdmin && !listed.projectAdmin} fallback={<Covered />}>
                          <Toggle
                            label={`${env} ${listed.email}`}
                            checked={listed.environments.includes(env)}
                            onChange={(on) => void setMembership(listed.principalId, env, on)}
                          />
                        </Show>
                      </td>
                    )}
                  </For>
                  <td>
                    <Show when={!listed.orgAdmin} fallback={<Covered />}>
                      <Toggle
                        label={`project-admin ${listed.email}`}
                        checked={listed.projectAdmin}
                        onChange={(on) => void setProjectAdmin(listed.principalId, on)}
                      />
                    </Show>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </section>
      <section class="flex max-w-md flex-col gap-3" data-slot="project-add">
        <p class="text-sm font-semibold uppercase">add a member</p>
        <ChoiceField
          label="user"
          choices={candidates().map((each) => ({ value: each.principalId, text: each.email }))}
          allowEmpty={false}
          value={newMember()}
          onChange={(id) => setNewMember(id === "" ? null : id)}
        />
        <ChoiceField
          label="environment"
          choices={(environments() ?? []).map((env) => ({ value: env, text: env }))}
          allowEmpty={false}
          value={newEnvironment()}
          onChange={(env) => setNewEnvironment(env === "" ? null : env)}
        />
        <Button class="self-start" onClick={() => void add()}>
          add
        </Button>
      </section>
      <section class="flex flex-col gap-2" data-slot="project-sagas">
        <p class="text-sm font-semibold uppercase">environment creation</p>
        <Show
          when={(listed()?.sagas ?? []).length > 0}
          fallback={<p class="text-muted-foreground text-sm">No environment creation.</p>}
        >
          <For each={listed()?.sagas ?? []}>
            {(saga) => (
              <div class="flex flex-col gap-1" data-saga={saga.sagaId}>
                <p class="text-sm">
                  {saga.env}: {saga.status}
                  <Show when={saga.lastError}>{(error) => <span>, {error()}</span>}</Show>
                </p>
                <div class="flex gap-2">
                  <Show when={saga.status === "failed"}>
                    <Button class="self-start" onClick={() => void endFailure(saga.sagaId, true)}>
                      resume
                    </Button>
                  </Show>
                  <Show when={saga.status === "failed" || saga.status === "pending"}>
                    <ConfirmAction
                      trigger="abandon"
                      question={`Abandon the creation of ${saga.env ?? "this environment"}?`}
                      confirm="abandon"
                      cancel="cancel"
                      onConfirm={() => void endFailure(saga.sagaId, false)}
                    />
                  </Show>
                </div>
                <table class="w-full text-sm">
                  <thead>
                    <tr class="text-left">
                      <th>step</th>
                      <th>name</th>
                      <th>status</th>
                      <th>error</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={saga.steps}>
                      {(step) => (
                        <tr data-step={step.name}>
                          <td>{step.step}</td>
                          <td>{step.name}</td>
                          <td>{step.status}</td>
                          <td>{step.error ?? ""}</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
                <For each={saga.steps.flatMap((step) => operatorCommands(step.detail))}>
                  {(group) => (
                    <div class="flex flex-col gap-1" data-slot="operator-commands">
                      <p class="text-sm">
                        {group.purpose} ({group.runbook})
                      </p>
                      <pre class="overflow-x-auto text-xs">{group.commands.join("\n")}</pre>
                    </div>
                  )}
                </For>
              </div>
            )}
          </For>
        </Show>
      </section>
    </div>
  );
}

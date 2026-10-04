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
 *
 * For an org-admin, as `control.mine` answers it, a form above the sagas
 * calls `environment.create`. It offers one version per package from
 * `package.list`, and takes each connection as typed fields and one JSON
 * object for its definition (issue 11).
 */

import { control, environment, member, package_, projectAdmin, user } from "@wamn/control-org-client";
import { type JsonValue, newRequestId, type Outcome, type Transport } from "@wamn/web-runtime";
import { createResource, createSignal, createUniqueId, For, Index, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { Switch } from "../components/ui/switch";
import { ConfirmAction } from "../confirm";
import { Field, FieldLabel } from "../components/ui/field";
import { ChoiceField, TextField } from "../fields";
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

/** One connection of the create form, as the operator types it. */
interface ConnectionDraft {
  readonly key: number;
  instanceId: string;
  alias: string;
  requirementType: string;
  /** The definition as JSON text. */
  definition: string;
}

/** The connection types that `bind-connection` binds. */
const REQUIREMENT_TYPES = [{ value: "blobstore", text: "blobstore" }];

/**
 * The create form of one environment, for an org-admin. It offers only the
 * package versions that `package.list` answers, one version per package, and
 * sends one JSON object per connection definition to `environment.create`,
 * which checks each definition before it writes the saga.
 */
function CreateEnvironmentForm(props: {
  transport: Transport;
  project: string;
  settle: (outcome: Outcome<unknown>, screen: string) => Promise<void>;
  refuse: (text: string) => void;
}): JSX.Element {
  const [packages] = createResource(async () => {
    const outcome = await package_.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.packages : [];
  });
  /** Each package id with its versions, in the order `package.list` answers them. */
  const versions = () => {
    const byPackage = new Map<string, string[]>();
    for (const each of packages() ?? []) {
      byPackage.set(each.packageId, [...(byPackage.get(each.packageId) ?? []), each.version]);
    }
    return [...byPackage.entries()];
  };
  const [env, setEnv] = createSignal("");
  const [tenant, setTenant] = createSignal("");
  const [routeHost, setRouteHost] = createSignal("");
  const [chosen, setChosen] = createSignal<Record<string, string>>({});
  const [connections, setConnections] = createSignal<ConnectionDraft[]>([]);
  let nextKey = 0;

  const edit = (key: number, change: Partial<ConnectionDraft>) =>
    setConnections((all) => all.map((each) => (each.key === key ? { ...each, ...change } : each)));

  const create = async () => {
    const definitions: JsonValue[] = [];
    for (const each of connections()) {
      let definition: unknown;
      try {
        definition = JSON.parse(each.definition);
      } catch {
        definition = null;
      }
      if (definition === null || typeof definition !== "object" || Array.isArray(definition)) {
        props.refuse(`The definition of ${each.instanceId} is not a JSON object.`);
        return;
      }
      definitions.push(definition as JsonValue);
    }
    const value = {
      project: props.project,
      env: env(),
      tenant: tenant(),
      routeHost: routeHost(),
      packages: Object.entries(chosen())
        .filter(([, version]) => version !== "")
        .map(([packageId, version]) => ({ packageId, version })),
      connections: connections().map((each, index) => ({
        instanceId: each.instanceId,
        alias: each.alias,
        requirementType: each.requirementType as "blobstore",
        definition: definitions[index] as JsonValue,
      })),
    };
    const outcome = await environment.create(props.transport, [{ requestId: newRequestId(), value }]);
    await props.settle(outcome, "environment create");
    if (outcome.status === "completed") {
      setEnv("");
      setTenant("");
      setRouteHost("");
      setChosen({});
      setConnections([]);
    }
  };

  return (
    <section class="flex max-w-md flex-col gap-3" data-slot="project-create-environment">
      <p class="text-sm font-semibold uppercase">create an environment</p>
      <TextField label="environment name" type="text" value={env()} onInput={setEnv} />
      <TextField label="tenant" type="text" value={tenant()} onInput={setTenant} />
      <TextField label="route host" type="text" value={routeHost()} onInput={setRouteHost} />
      <For each={versions()}>
        {([packageId, list]) => (
          <ChoiceField
            label={`version of ${packageId}`}
            choices={list.map((version) => ({ value: version, text: version }))}
            allowEmpty={true}
            value={chosen()[packageId] ?? ""}
            onChange={(version) => setChosen((all) => ({ ...all, [packageId]: version }))}
          />
        )}
      </For>
      <Index each={connections()}>
        {(draft) => {
          const id = createUniqueId();
          return (
            <div class="flex flex-col gap-2" data-connection={draft().key}>
              <TextField
                label="instance id"
                type="text"
                value={draft().instanceId}
                onInput={(instanceId) => edit(draft().key, { instanceId })}
              />
              <TextField
                label="alias"
                type="text"
                value={draft().alias}
                onInput={(alias) => edit(draft().key, { alias })}
              />
              <ChoiceField
                label="requirement type"
                choices={REQUIREMENT_TYPES}
                allowEmpty={false}
                value={draft().requirementType}
                onChange={(requirementType) => edit(draft().key, { requirementType })}
              />
              <Field>
                <FieldLabel for={id}>definition</FieldLabel>
                <textarea
                  id={id}
                  class="border-input min-h-24 rounded-md border bg-transparent p-2 font-mono text-xs"
                  value={draft().definition}
                  onInput={(event) => edit(draft().key, { definition: event.currentTarget.value })}
                />
              </Field>
              <Button
                class="self-start"
                variant="outline"
                onClick={() => setConnections((all) => all.filter((each) => each.key !== draft().key))}
              >
                remove connection
              </Button>
            </div>
          );
        }}
      </Index>
      <Button
        class="self-start"
        variant="outline"
        onClick={() =>
          setConnections((all) => [
            ...all,
            { key: nextKey++, instanceId: "", alias: "", requirementType: "blobstore", definition: "" },
          ])
        }
      >
        add connection
      </Button>
      <Button class="self-start" onClick={() => void create()}>
        create
      </Button>
    </section>
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

  const [mine] = createResource(async () => {
    const outcome = await control.mine(props.transport, [{}]);
    return outcome.status === "completed" && outcome.value.orgAdmin;
  });

  /** Announces one saga write, shows a refusal's text, and reads the sagas again. */
  const settleSaga = async (outcome: Outcome<unknown>, screen: string) => {
    announceOutcome(outcome, screen);
    setRefusal(outcome.status === "refused" ? controlRefusal(outcome) : null);
    await refetchListed();
  };

  /** Resumes a failed saga, or abandons a failed or pending one, and reads the sagas again. */
  const endFailure = async (sagaId: string, resume: boolean) => {
    const item = [{ requestId: newRequestId(), value: { sagaId } }];
    const outcome = resume
      ? await environment.resume(props.transport, item)
      : await environment.abandon(props.transport, item);
    await settleSaga(outcome, resume ? "saga resume" : "saga abandon");
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
      <Show when={mine()}>
        <CreateEnvironmentForm
          transport={props.transport}
          project={props.project}
          settle={settleSaga}
          refuse={(text) => setRefusal(text)}
        />
      </Show>
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

/**
 * The user grid of one application (docs/plan/platform-ui.md §4.7).
 *
 * It reads `user.list` for its user choice and `role.list` for its rows, with
 * one toggle per role that calls `user_role.grant` or `user_role.revoke`. The
 * effective permission set is the union of `permission.list` of each held
 * role, or every served operation when the user holds `admin`.
 *
 * When `project-admin` or `org-admin` covers the user's `admin`, `user.list`
 * says so in `admin_covered`, and the `admin` row is hierarchy-controlled,
 * with no toggle. Each write announces its outcome, a refusal shows the text
 * of its contract, and the grid reads again after each write. It writes no
 * database directly.
 */

import { permission, role, user, userRole } from "@wamn/control-client";
import { newRequestId, type Outcome, refusalSentence, type Transport } from "@wamn/web-runtime";
import { createResource, createSignal, createUniqueId, For, type JSX, Show } from "solid-js";

import { Switch } from "../components/ui/switch";
import { ChoiceField } from "../fields";
import { announceOutcome } from "../outcome";

/** The role that holds every operation. */
const ADMIN = "admin";

export interface UserGridProps {
  readonly transport: Transport;
  /** The id of the user chosen first. The default is no user. */
  readonly user?: string | undefined;
}

/** The operations that the roles `held` give, in reference order. */
async function effective(transport: Transport, held: readonly string[]): Promise<string[]> {
  const operations = new Set<string>();
  for (const name of held.includes(ADMIN) ? [ADMIN] : held) {
    const outcome = await permission.list(transport, [{ role: name }]);
    if (outcome.status !== "completed") {
      continue;
    }
    for (const row of outcome.value.operations) {
      const holds = outcome.value.admin ? row.served : row.selected || row.requiredBy.length > 0;
      if (holds) {
        operations.add(row.operation);
      }
    }
  }
  return [...operations].sort();
}

export function UserGrid(props: UserGridProps): JSX.Element {
  // eslint-disable-next-line solid/reactivity -- the prop is the first choice only.
  const [chosen, setChosen] = createSignal<string | null>(props.user ?? null);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [users, { refetch }] = createResource(async () => {
    const outcome = await user.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.users : [];
  });
  const [roles] = createResource(async () => {
    const outcome = await role.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.roles : [];
  });
  const current = () => (users() ?? []).find((candidate) => candidate.id === chosen()) ?? null;
  const [operations] = createResource(
    () => current()?.roles,
    (held) => effective(props.transport, held),
  );

  /** Announces one write, shows a refusal's text, and reads the users again. */
  const settle = async <T,>(outcome: Outcome<T>, screen: string) => {
    announceOutcome(outcome, screen);
    setRefusal(outcome.status === "refused" ? refusalSentence(outcome.code, outcome.text) : null);
    await refetch();
  };

  const toggle = async (name: string, on: boolean) => {
    const userId = chosen();
    if (userId === null) {
      return;
    }
    const value = { userId, role: name };
    if (on) {
      await settle(await userRole.grant(props.transport, [{ requestId: newRequestId(), value }]), "grant");
    } else {
      await settle(await userRole.revoke(props.transport, [{ requestId: newRequestId(), value }]), "revoke");
    }
  };

  return (
    <div class="flex flex-col gap-4">
      <ChoiceField
        label="user"
        choices={(users() ?? []).map((candidate) => ({ value: candidate.id, text: candidate.email }))}
        allowEmpty={false}
        value={chosen()}
        onChange={(id) => {
          setRefusal(null);
          setChosen(id === "" ? null : id);
        }}
      />
      <Show when={refusal()}>{(text) => <p role="alert">{text()}</p>}</Show>
      <Show when={current()}>
        {(shown) => (
          <div class="grid gap-6 md:grid-cols-2">
            <section class="flex flex-col gap-2">
              <p class="text-sm font-semibold uppercase">roles</p>
              <ul class="flex flex-col gap-2">
                <For each={roles() ?? []}>
                  {(name) => {
                    const id = createUniqueId();
                    const held = () => shown().roles.includes(name);
                    const covered = () => name === ADMIN && shown().adminCovered;
                    return (
                      <li class="flex items-center gap-3" data-role={name}>
                        <Show
                          when={!covered()}
                          fallback={<span class="text-muted-foreground text-xs">hierarchy-controlled</span>}
                        >
                          <Switch
                            id={id}
                            size="sm"
                            checked={held()}
                            onChange={(on: boolean) => void toggle(name, on)}
                          />
                        </Show>
                        <label for={id} class="text-sm">
                          {name}
                        </label>
                      </li>
                    );
                  }}
                </For>
              </ul>
            </section>
            <section class="flex flex-col gap-2">
              <p class="text-sm font-semibold uppercase">effective permissions</p>
              <ul class="flex flex-col gap-1 text-sm" data-slot="effective-permissions">
                <For each={operations() ?? []}>{(operation) => <li>{operation}</li>}</For>
              </ul>
            </section>
          </div>
        )}
      </Show>
    </div>
  );
}
